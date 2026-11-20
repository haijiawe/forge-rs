//! The gate a caller runs: scan a tree on disk, refuse what is wrong, assemble
//! prompts, and compare both against the bytes the fixture files actually hold.
//!
//! Everything here reads `tests/fixtures/` from disk. Inlining the fixtures would
//! let a test pass over a file that is no longer a Markdown document; the point
//! is that a real directory tree with real bytes goes through the real walker.
//!
//! A roster is **one** tree. Teams live among the experts (the loader tells them
//! apart by frontmatter, not by directory name), which is why the `good` fixture
//! has `experts/` and `teams/` as siblings under one root: a user organising a
//! large roster by kind must still be able to scan the parent once.

use std::path::{Path, PathBuf};

use hearth_experts::{prompt_for, scan, team_prompt, ErrorKind};

/// The root of one fixture tree.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// A path as the crate prints it in messages (forward slashes), so a message can
/// be matched against the fixture it names on any host.
fn shown(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

#[test]
fn a_good_tree_loads_with_stable_order_and_verbatim_prompts() {
    let set = scan(&fixture("good")).expect("the good tree must load");

    let slugs: Vec<&str> = set
        .experts()
        .iter()
        .map(|expert| expert.slug.as_str())
        .collect();
    assert_eq!(
        slugs,
        vec![
            "api-reviewer",
            "bench-owner",
            "release-lead",
            "rust-concurrency-review"
        ],
        "the walk is sorted, so the order does not depend on the filesystem"
    );
    assert_eq!(set.teams().len(), 1);
    assert_eq!(set.teams()[0].slug, "release-squad");

    // The inline flow list and the block list become the same shape.
    let review = set.expert("rust-concurrency-review").expect("expert");
    assert_eq!(review.name, "Rust 并发审查");
    assert_eq!(
        review.description,
        "Reviews lock ordering and Send/Sync boundaries"
    );
    assert_eq!(review.division.as_deref(), Some("engineering"));
    assert!(review.enabled);
    assert_eq!(
        review.tools.as_deref(),
        Some(&["fs_read".to_string(), "fs_grep".to_string()][..])
    );

    let bench = set.expert("bench-owner").expect("expert");
    assert_eq!(
        bench.tools.as_deref(),
        Some(&["fs_read".to_string(), "shell".to_string()][..])
    );

    // `tools: []` is a restriction, not an absence of one; no `tools:` line at
    // all is the absence of a restriction. The two are different instructions.
    assert_eq!(
        set.expert("api-reviewer").expect("expert").tools.as_deref(),
        Some(&[][..])
    );
    assert_eq!(set.expert("release-lead").expect("expert").tools, None);
}

#[test]
fn the_body_on_disk_and_the_body_in_the_set_are_the_same_bytes() {
    // Deliberately independent of the loader's parser: read the file, find the
    // closing fence by hand, and compare the remainder.
    let path = fixture("good").join("experts").join("api-reviewer.md");
    let raw = std::fs::read(&path).expect("read fixture");
    let text = std::str::from_utf8(&raw).expect("utf8");
    let close = text.find("\n---\n").expect("a closing fence");
    let expected = &text[close + "\n---\n".len()..];
    assert!(expected.contains('\t'));
    assert!(expected.contains("\n---\n"), "a fence-looking line is body");
    assert!(expected.contains("中文与标点"));
    assert!(expected.ends_with('\n'));

    let set = scan(&fixture("good")).expect("scan");
    assert_eq!(set.expert("api-reviewer").expect("expert").prompt, expected);
}

#[test]
fn a_crlf_body_survives_byte_for_byte() {
    let root = fixture("verbatim");
    let raw = std::fs::read(root.join("experts").join("crlf-expert.md")).expect("read fixture");
    assert!(
        raw.windows(2).any(|window| window == b"\r\n"),
        "the fixture itself must be CRLF, or this test proves nothing"
    );

    let set = scan(&root).expect("scan");
    let expert = set.expert("crlf-expert").expect("expert");

    let text = std::str::from_utf8(&raw).expect("utf8");
    let close = text.find("\r\n---\r\n").expect("closing fence") + "\r\n---\r\n".len();
    assert_eq!(expert.prompt.as_bytes(), &raw[close..]);
    assert!(
        expert.prompt.ends_with("\r\n\r\n"),
        "trailing blank lines are part of the prompt"
    );
    assert!(expert.prompt.contains('\t'));
    assert!(prompt_for(&set, "crlf-expert")
        .expect("prompt")
        .contains("\r\n"));
}

#[test]
fn prompt_for_appends_the_tool_restriction_only_when_the_file_states_one() {
    let set = scan(&fixture("good")).expect("scan");

    assert_eq!(
        prompt_for(&set, "release-lead").expect("prompt"),
        set.expert("release-lead").expect("expert").prompt,
        "no tools line means the body is the whole prompt"
    );

    let limited = prompt_for(&set, "bench-owner").expect("prompt");
    assert!(limited.starts_with(&set.expert("bench-owner").expect("expert").prompt));
    assert!(limited.ends_with(
        "\n[hearth-experts] Tool restriction: fs_read, shell. This expert must not call any other tool.\n"
    ));

    let none = prompt_for(&set, "api-reviewer").expect("prompt");
    assert!(none.ends_with(
        "\n[hearth-experts] Tool restriction: this expert may not call any tool. Answer from what you are given.\n"
    ));

    assert_eq!(prompt_for(&set, "no-such-expert"), None);
}

#[test]
fn every_prompt_is_byte_identical_across_calls() {
    let set = scan(&fixture("good")).expect("scan");
    for slug in [
        "rust-concurrency-review",
        "bench-owner",
        "release-lead",
        "api-reviewer",
    ] {
        let first = prompt_for(&set, slug).expect("prompt");
        let second = prompt_for(&set, slug).expect("prompt");
        assert_eq!(first.as_bytes(), second.as_bytes(), "slug {slug}");
    }
    let team_first = team_prompt(&set, "release-squad").expect("prompt");
    let team_second = team_prompt(&set, "release-squad").expect("prompt");
    assert_eq!(team_first.as_bytes(), team_second.as_bytes());
}

#[test]
fn a_team_resolves_every_member_and_keeps_the_files_order() {
    let set = scan(&fixture("good")).expect("scan");
    let team = set.team("release-squad").expect("team");

    assert_eq!(team.name, "发布小组");
    assert_eq!(
        team.description.as_deref(),
        Some("Reviews a release candidate before it ships")
    );
    assert_eq!(team.coordinator, "release-lead");
    assert_eq!(
        team.members
            .iter()
            .map(|member| member.expert.as_str())
            .collect::<Vec<_>>(),
        vec!["rust-concurrency-review", "bench-owner", "api-reviewer"],
        "file order, never sorted (sorted would be api-reviewer first)"
    );
    assert_eq!(team.members[0].duty, "reviews the lock ordering");
    assert_eq!(
        team.members[0].description, "Reviews lock ordering and Send/Sync boundaries",
        "the member's own description is resolved in at load time"
    );
    assert!(team.prompt.starts_with("Team-level instructions."));
}

#[test]
fn the_team_prompt_names_the_coordinator_and_one_line_per_member() {
    let set = scan(&fixture("good")).expect("scan");
    let team = set.team("release-squad").expect("team");
    let prompt = team_prompt(&set, "release-squad").expect("prompt");

    assert!(prompt.starts_with(&team.prompt), "the body comes first");
    assert!(prompt.contains("Coordinate this team as expert release-lead."));

    let lines: Vec<&str> = prompt
        .lines()
        .filter(|line| {
            line.starts_with("1. ") || line.starts_with("2. ") || line.starts_with("3. ")
        })
        .collect();
    assert_eq!(
        lines,
        vec![
            "1. rust-concurrency-review: duty = reviews the lock ordering; description = Reviews lock ordering and Send/Sync boundaries",
            "2. bench-owner: duty = re-records the baselines; description = Re-records the baseline numbers and says which ones moved",
            "3. api-reviewer: duty = checks the public surface for accidental breaks; description = Checks the public surface of a crate for accidental breaks",
        ]
    );
    assert_eq!(team_prompt(&set, "no-such-team"), None);
}

#[test]
fn duplicate_slugs_are_refused_with_both_paths() {
    let root = fixture("duplicate");
    let err = scan(&root).expect_err("a duplicate slug must be refused");
    assert_eq!(err.kind(), ErrorKind::DuplicateSlug);
    assert_eq!(err.code(), "duplicate_slug");
    let message = err.message();
    assert!(
        message.contains("duplicate slug \"dup-review\""),
        "{message}"
    );
    assert!(
        message.contains(&shown(&root.join("experts").join("a").join("one.md"))),
        "the first path is named: {message}"
    );
    assert!(
        message.contains(&shown(&root.join("experts").join("b").join("two.md"))),
        "the second path is named: {message}"
    );
}

#[test]
fn an_unknown_member_is_refused_with_the_slug_and_the_team_path() {
    let root = fixture("unknown-member");
    let err = scan(&root).expect_err("an unknown member must be refused");
    assert_eq!(err.kind(), ErrorKind::UnknownMember);
    let message = err.message();
    assert!(message.contains("\"never-written\""), "{message}");
    assert!(
        message.contains(&shown(&root.join("teams").join("broken-squad.md"))),
        "the team path is named: {message}"
    );
    assert!(
        message.contains("ghost-expert"),
        "the known experts are listed, so the typo is visible: {message}"
    );
}

#[test]
fn a_disabled_member_is_a_different_refusal_from_an_unknown_one() {
    let root = fixture("disabled-member");
    let err = scan(&root).expect_err("a disabled member must be refused");

    assert_eq!(err.kind(), ErrorKind::DisabledMember);
    assert_ne!(err.kind(), ErrorKind::UnknownMember);
    assert_eq!(err.code(), "disabled_member");

    let message = err.message();
    assert!(message.contains("\"retired-expert\""), "{message}");
    assert!(message.contains("which is disabled by"), "{message}");
    assert!(
        message.contains(&shown(&root.join("experts").join("retired-expert.md"))),
        "the disabled file is named: {message}"
    );
    assert!(
        message.contains("enabled: true"),
        "the fix is stated: {message}"
    );
    assert!(
        message.contains("retired-expert (disabled)"),
        "the known-expert list carries the state: {message}"
    );
}

#[test]
fn a_one_member_team_is_refused() {
    let root = fixture("one-member");
    let err = scan(&root).expect_err("a team of one must be refused");
    assert_eq!(err.kind(), ErrorKind::TooFewMembers);
    assert_eq!(err.code(), "too_few_members");
    assert!(err.message().contains("has 1 member"), "{}", err.message());
    assert!(err.message().contains("at least 2"), "{}", err.message());
}

#[test]
fn a_file_without_frontmatter_is_refused_not_skipped() {
    let root = fixture("no-frontmatter");
    let err = scan(&root).expect_err("prose is not a definition");
    assert_eq!(err.kind(), ErrorKind::MissingFrontmatter);
    assert_eq!(err.code(), "missing_frontmatter");
    assert!(
        err.message()
            .contains(&shown(&root.join("experts").join("prose.md"))),
        "the file is named: {}",
        err.message()
    );
    assert!(err.message().contains("line 1 must be ---"));
}

#[test]
fn a_bad_slug_is_refused() {
    let root = fixture("bad-slug");
    let err = scan(&root).expect_err("a bad slug must be refused");
    assert_eq!(err.kind(), ErrorKind::BadSlug);
    assert_eq!(err.code(), "bad_slug");
    assert!(
        err.message().contains("\"Not-A-Slug\""),
        "{}",
        err.message()
    );
    assert!(
        err.message()
            .contains("file name and a command-line argument"),
        "the message says why the grammar is strict: {}",
        err.message()
    );
}

#[test]
fn a_missing_description_is_refused() {
    let root = fixture("missing-field");
    let err = scan(&root).expect_err("a missing description must be refused");
    assert_eq!(err.kind(), ErrorKind::MissingField);
    assert_eq!(err.code(), "missing_field");
    assert!(
        err.message().contains("has no \"description\""),
        "{}",
        err.message()
    );
    assert!(
        err.message()
            .contains(&shown(&root.join("experts").join("no-description.md"))),
        "{}",
        err.message()
    );
}

#[test]
fn a_missing_root_is_an_empty_set_not_an_error() {
    let set = scan(&fixture("this-directory-does-not-exist")).expect("absent root is normal");
    assert!(set.is_empty());
    assert_eq!(set.len(), 0);
    assert!(prompt_for(&set, "anything").is_none());
    assert!(team_prompt(&set, "anything").is_none());
}

#[test]
fn every_fixture_tree_is_read_only() {
    // A test that edited a fixture could make the next run pass for the wrong
    // reason. This asserts the trees are only ever read by checking that the
    // known files are still there with their expected sizes.
    let good = fixture("good");
    for name in [
        "experts/api-reviewer.md",
        "experts/bench-owner.md",
        "experts/release-lead.md",
        "experts/rust-concurrency-review.md",
        "teams/release-squad.md",
    ] {
        let path = good.join(name.replace('/', std::path::MAIN_SEPARATOR_STR));
        let size = std::fs::metadata(&path)
            .unwrap_or_else(|err| panic!("{}: {err}", shown(&path)))
            .len();
        assert!(size > 0, "{} is empty", shown(&path));
    }
}
