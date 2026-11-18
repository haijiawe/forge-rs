//! The roster: what the tree says, checked, and turned into a prompt.
//!
//! Three jobs live here, in this order.
//!
//! **Load.** A roster is a directory tree the user owns, so the walker has to
//! treat three states as normal: the root is absent (a machine that never had an
//! experts directory), the root is deep and nested, and some entries are not
//! definitions at all. Absent root is an **empty set**, not an error, because a
//! fresh checkout that has never run `forge expert new` must still start.
//!
//! **Refuse.** Every check below exists because the alternative is a persona
//! that quietly changed. A duplicate slug means "which file wins" is decided by
//! directory-listing order, so it is refused with both paths. A team member that
//! does not resolve means the team silently lost a member, so the *slug* and the
//! team path are named and load fails. A disabled expert referenced by a team
//! gets its own error, because the fix ("enable it") is different from the fix
//! for a typo ("correct the slug") and one message cannot serve both.
//!
//! **Assemble.** [`prompt_for`] and [`team_prompt`] are pure functions of the
//! loaded set, so two calls give byte-identical strings; nothing sorts, samples
//! or timestamps. Member order is the order the team file lists, because a
//! reviewer approved that order and a shuffed prompt is a prompt nobody can
//! diff.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::error::{Error, ErrorKind, Result};
use crate::frontmatter::{self, Frontmatter, Value};
use crate::fsio::{self, Fs, RealFs};
use crate::util::{clean_path, describe_io, display, quote};

/// How deep the walker will descend before calling the tree a mistake.
///
/// A roster is a flat list of individuals with a little grouping; thirty-two
/// levels means somebody pointed the loader at `$HOME`. Refusing beats walking a
/// whole home directory looking for Markdown.
const MAX_DEPTH: usize = 32;

/// Longest legal slug in bytes, from the grammar `^[a-z0-9][a-z0-9-]{0,63}$`.
const SLUG_MAX: usize = 64;

/// Fields an expert file may carry. Anything else is refused, not ignored.
const EXPERT_KEYS: [&str; 6] = [
    "name",
    "slug",
    "description",
    "division",
    "enabled",
    "tools",
];

/// Fields a team file may carry.
const TEAM_KEYS: [&str; 5] = ["name", "slug", "description", "coordinator", "members"];

/// One expert definition, loaded and validated.
///
/// The fields are public because the CLI prints them and a caller assembling its
/// own prompt needs them; a `Set` can only be built by [`scan`], so a value that
/// exists here has already passed every check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expert {
    /// The stable name used on a command line and in a team's `members:` list.
    pub slug: String,
    /// The human-facing name.
    pub name: String,
    /// One line saying what this expert is for.
    pub description: String,
    /// The `division:` line, when the file carries one.
    pub division: Option<String>,
    /// False when the file says `enabled: false`.
    pub enabled: bool,
    /// `None` when the file has no `tools:` line, meaning "no restriction is
    /// stated"; `Some(vec![])` when it says `tools: []`, meaning "this expert
    /// may call no tool at all". The two are different instructions and the
    /// assembled prompt says so.
    pub tools: Option<Vec<String>>,
    /// The file body, byte for byte, exactly as `frontmatter::parse` sliced it.
    pub prompt: String,
    /// Where the definition came from, for messages.
    pub path: PathBuf,
}

/// One `members:` entry of a team, with the expert's description already
/// resolved.
///
/// Why the description is copied here instead of looked up when the prompt is
/// built: the prompt function then has no second lookup that could fail or
/// diverge, so byte-identical output needs no reasoning about lookups.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamMember {
    /// The expert's slug, as the team file spelled it (already checked).
    pub expert: String,
    /// What this member is responsible for in this team.
    pub duty: String,
    /// The `description:` of the expert this slug resolves to.
    pub description: String,
}

/// One team definition, loaded and validated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Team {
    /// The team's stable name.
    pub slug: String,
    /// The human-facing name.
    pub name: String,
    /// The optional `description:` line.
    pub description: Option<String>,
    /// The expert that directs the team. Resolved: it exists and is enabled.
    pub coordinator: String,
    /// Members in file order, never sorted.
    pub members: Vec<TeamMember>,
    /// The file body, byte for byte.
    pub prompt: String,
    /// Where the definition came from, for messages.
    pub path: PathBuf,
}

/// A validated roster.
///
/// The fields are private on purpose: the only way to obtain a `Set` is [`scan`],
/// so "a `Set` exists" and "the tree was checked" are the same fact. A caller
/// cannot hand-build one whose team names a missing expert.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Set {
    experts: Vec<Expert>,
    teams: Vec<Team>,
}

impl Set {
    /// An empty roster, for a root that does not exist.
    pub fn empty() -> Self {
        Set {
            experts: Vec::new(),
            teams: Vec::new(),
        }
    }

    /// Every expert in discovery order (sorted directory walk, so stable).
    pub fn experts(&self) -> &[Expert] {
        &self.experts
    }

    /// Every team in discovery order.
    pub fn teams(&self) -> &[Team] {
        &self.teams
    }

    /// Look up one expert by slug, **including disabled ones**, for listings.
    ///
    /// Summoning goes through [`Set::summonable`] instead; a listing wants to
    /// show the retired persona with its state, which is the whole point of
    /// keeping `enabled: false` files.
    pub fn expert(&self, slug: &str) -> Option<&Expert> {
        self.experts.iter().find(|expert| expert.slug == slug)
    }

    /// Look up one team by slug.
    pub fn team(&self, slug: &str) -> Option<&Team> {
        self.teams.iter().find(|team| team.slug == slug)
    }

    /// Look up one expert that may actually be summoned.
    ///
    /// A disabled expert is absent from this set, so a caller that forgets to
    /// check `enabled` cannot summon it by accident: the type makes the check
    /// unavoidable rather than remembered.
    pub fn summonable(&self, slug: &str) -> Option<&Expert> {
        self.expert(slug).filter(|expert| expert.enabled)
    }

    /// True when the tree held no definition at all.
    pub fn is_empty(&self) -> bool {
        self.experts.is_empty() && self.teams.is_empty()
    }

    /// Total number of definitions in the set.
    pub fn len(&self) -> usize {
        self.experts.len() + self.teams.len()
    }
}

/// Scan `root` for definitions and validate them together.
///
/// # Errors
///
/// One refusal for the first problem found, in walk order, so running it twice
/// on the same tree reports the same file. A missing root is `Ok(Set::empty())`;
/// an unreadable entry, a file over the size cap, invalid frontmatter, a bad
/// slug, a missing required field, a duplicate slug, an unresolved or disabled
/// team reference, and a team with fewer than two members are all refusals.
pub fn scan(root: &Path) -> Result<Set> {
    scan_with(&RealFs, root)
}

/// Build the prompt for one summonable expert.
///
/// The body comes back as a prefix, unchanged; when the file restricts tools, one
/// line is appended saying which tools are allowed. A disabled expert returns
/// `None`: the flag exists to make exactly this call fail.
pub fn prompt_for(set: &Set, slug: &str) -> Option<String> {
    let expert = set.summonable(slug)?;
    let mut prompt = expert.prompt.clone();
    let Some(tools) = &expert.tools else {
        return Some(prompt);
    };
    // The body is never rewritten, so the restriction is appended after it. The
    // leading newline terminates the body's last line when the file did not end
    // with one, which keeps the appended line a line in every case.
    prompt.push('\n');
    if tools.is_empty() {
        prompt.push_str(
            "[forge-experts] Tool restriction: this expert may not call any tool. Answer from what you are given.\n",
        );
    } else {
        prompt.push_str("[forge-experts] Tool restriction: ");
        prompt.push_str(&tools.join(", "));
        prompt.push_str(". This expert must not call any other tool.\n");
    }
    Some(prompt)
}

/// Build the prompt for one team: the coordinator's instruction, then one line
/// per member in file order.
///
/// Determinism is the requirement, so there is no sorting, no dedup and no
/// lookup that can vary: `members` is already resolved and in file order.
pub fn team_prompt(set: &Set, slug: &str) -> Option<String> {
    let team = set.team(slug)?;
    let mut prompt = team.prompt.clone();
    prompt.push('\n');
    prompt.push_str("[forge-experts] Coordinate this team as expert ");
    prompt.push_str(&team.coordinator);
    prompt.push_str(
        ". You direct the members listed below, in this order, and you answer for the combined result. Each line gives a member's slug, its duty in this team, and what that expert is for.\n",
    );
    for (index, member) in team.members.iter().enumerate() {
        prompt.push_str(&format!(
            "{}. {}: duty = {}; description = {}\n",
            index + 1,
            member.expert,
            member.duty,
            member.description
        ));
    }
    Some(prompt)
}

/// Whether a slug is legal, using the grammar `^[a-z0-9][a-z0-9-]{0,63}$`.
///
/// Why so strict, and why this shape: the slug is a **file name** (`<slug>.md`
/// beside its neighbours, and the stem of an exported prompt) and a **command
/// line argument** (`forge expert show <slug>`). Lowercase letters, digits and
/// dashes survive both without quoting rules, without a case-insensitive
/// filesystem collapse (`Review` and `review` are one file on Windows) and
/// without the ambiguity a leading dash would create for an argument parser.
pub(crate) fn valid_slug(slug: &str) -> bool {
    let bytes = slug.as_bytes();
    if bytes.is_empty() || bytes.len() > SLUG_MAX {
        return false;
    }
    let first = bytes[0];
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

/// What one file turned out to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Definition {
    /// An expert persona.
    Expert,
    /// A team of experts.
    Team,
}

impl Definition {
    fn label(self) -> &'static str {
        match self {
            Definition::Expert => "expert",
            Definition::Team => "team",
        }
    }
}

/// The scan with the filesystem handed in, so the refusal branches are testable.
pub(crate) fn scan_with(fs: &dyn Fs, root: &Path) -> Result<Set> {
    // An absent roster directory is a normal machine, not a failure: the next
    // `forge expert new` creates it. Only a *real* I/O error is an error here.
    match fs.metadata(root) {
        Ok(meta) if meta.is_dir => {}
        Ok(_) => {
            return Err(Error::new(
                ErrorKind::Unreadable,
                format!(
                    "{}: is a file, not a directory; point the roster at a directory of Markdown definitions, or delete this file",
                    display(root)
                ),
            ))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Set::empty()),
        Err(err) => {
            return Err(Error::new(
                ErrorKind::Unreadable,
                format!(
                    "cannot stat roster root {}: {}; check that the path exists and is readable",
                    display(root),
                    describe_io(&err)
                ),
            ))
        }
    }

    let mut paths = Vec::new();
    let mut visited: Vec<PathBuf> = Vec::new();
    walk(fs, root, 0, &mut visited, &mut paths)?;
    // Sorted by the display form so two runs over the same tree report the same
    // file first. `walk` already sorts each directory listing; this only makes
    // the order independent of how the tree is nested.
    paths.sort_by_key(|path| display(path));

    let mut parsed: Vec<(PathBuf, Frontmatter)> = Vec::with_capacity(paths.len());
    for path in paths {
        let (data, truncated) = fsio::read_capped(fs, &path)?;
        if truncated {
            return Err(Error::new(
                ErrorKind::Unreadable,
                format!(
                    "{}: file is larger than the {} KiB limit for a definition; a prompt is content, so it is refused rather than truncated. Split the file or move the long text into the references it points at",
                    display(&path),
                    fsio::MAX_FILE_BYTES / 1024
                ),
            ));
        }
        let fm = frontmatter::parse(&data, &display(&path))?;
        parsed.push((path, fm));
    }

    // Pass one: every file on its own, no cross-file knowledge yet.
    let mut experts: Vec<Expert> = Vec::new();
    let mut raw_teams: Vec<RawTeam> = Vec::new();
    for (path, fm) in &parsed {
        let definition = classify(fm);
        match definition {
            Definition::Expert => experts.push(read_expert(fm, path)?),
            Definition::Team => raw_teams.push(read_team(fm, path)?),
        }
    }

    // Pass two: one slug namespace for the whole tree. A team and an expert that
    // answer to the same name is the same ambiguity as two experts that do.
    let mut seen: BTreeMap<&str, (&PathBuf, Definition)> = BTreeMap::new();
    for expert in &experts {
        let first = seen.insert(expert.slug.as_str(), (&expert.path, Definition::Expert));
        if let Some((other_path, other_kind)) = first {
            return Err(duplicate_slug(
                &expert.slug,
                (other_path, other_kind),
                (&expert.path, Definition::Expert),
            ));
        }
    }
    for team in &raw_teams {
        let first = seen.insert(team.slug.as_str(), (&team.path, Definition::Team));
        if let Some((other_path, other_kind)) = first {
            return Err(duplicate_slug(
                &team.slug,
                (other_path, other_kind),
                (&team.path, Definition::Team),
            ));
        }
    }

    // Pass three: resolve team references now that every expert is known.
    let mut teams: Vec<Team> = Vec::with_capacity(raw_teams.len());
    for raw in raw_teams {
        teams.push(resolve_team(raw, &experts)?);
    }

    Ok(Set { experts, teams })
}

/// A team as read from one file, before its references are resolved.
struct RawTeam {
    slug: String,
    name: String,
    description: Option<String>,
    coordinator: String,
    members: Vec<RawMember>,
    prompt: String,
    path: PathBuf,
}

/// One unresolved `members:` entry.
struct RawMember {
    expert: String,
    duty: String,
}

/// Walk one directory, appending every `.md` file found.
fn walk(
    fs: &dyn Fs,
    dir: &Path,
    depth: usize,
    visited: &mut Vec<PathBuf>,
    out: &mut Vec<PathBuf>,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::new(
            ErrorKind::Unreadable,
            format!(
                "{}: directory nesting is deeper than {MAX_DEPTH} levels; a roster is a short tree, so this is almost certainly the wrong root. Point the loader at the experts directory",
                display(dir)
            ),
        ));
    }
    // Symlinked directories can form a cycle, and a cycle is an infinite walk.
    // The canonical path is the identity of a directory, so a repeat means the
    // subtree was already read. When the filesystem will not canonicalise (the
    // entry vanished, or the volume does not support it) the lexical form is the
    // best available identity, so `a/./b` and `a/b` still count as one directory.
    let key = fs
        .canonicalize(dir)
        .unwrap_or_else(|_| PathBuf::from(clean_path(&display(dir))));
    if visited.contains(&key) {
        return Ok(());
    }
    visited.push(key);

    let mut names = fs.read_dir(dir).map_err(|err| {
        Error::new(
            ErrorKind::Unreadable,
            format!(
                "cannot list {}: {}; check that the directory is readable and not locked",
                display(dir),
                describe_io(&err)
            ),
        )
    })?;
    // Sorted so the load order (and therefore which refusal is reported first)
    // does not depend on the filesystem's listing order.
    names.sort();

    for name in names {
        // A dot entry is not part of a roster: `.git`, `.obsidian` and the like
        // hold Markdown that is prose, and scanning them would refuse a tree that
        // is perfectly usable. `.` and `..` never appear in a listing.
        if name.starts_with('.') {
            continue;
        }
        let path = dir.join(&name);
        let meta = fs.metadata(&path).map_err(|err| {
            Error::new(
                ErrorKind::Unreadable,
                format!(
                    "cannot stat {}: {}; remove the broken entry or fix its target",
                    display(&path),
                    describe_io(&err)
                ),
            )
        })?;
        if meta.is_dir {
            walk(fs, &path, depth + 1, visited, out)?;
        } else if is_markdown(&name) {
            out.push(path);
        }
    }
    Ok(())
}

/// True for the file names this loader reads.
fn is_markdown(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".md")
}

/// Decide whether a parsed file is an expert or a team.
///
/// A team is a file that talks about a team: it has `members:` or `coordinator:`.
/// Nothing else could mean either, since both keys are refused on an expert, so a
/// file with `coordinator:` and no `members:` is read as a team and then refused
/// for the missing `members:` rather than being reported as an expert with an
/// unknown field.
fn classify(fm: &Frontmatter) -> Definition {
    if fm.get("members").is_some() || fm.get("coordinator").is_some() {
        Definition::Team
    } else {
        Definition::Expert
    }
}

/// Build an expert from one file, refusing anything doubtful.
fn read_expert(fm: &Frontmatter, path: &Path) -> Result<Expert> {
    let where_ = display(path);
    refuse_unknown_keys(fm, &EXPERT_KEYS, path, "expert")?;

    let name = required_scalar(fm, "name", path, "expert")?;
    let slug = required_slug(fm, path, "expert")?;
    let description = required_scalar(fm, "description", path, "expert")?;
    let division = optional_scalar(fm, "division", path, "expert")?;
    let enabled = read_enabled(fm, path)?;
    let tools = match fm.get("tools") {
        None => None,
        Some(value) => Some(read_string_list(value, path, "tools")?),
    };

    // A division named "all" or an empty one is a typo, not a statement.
    if let Some(division) = &division {
        if division.trim().is_empty() {
            return Err(Error::new(
                ErrorKind::MissingField,
                format!(
                    "{where_}: expert {slug:?} has an empty \"division\"; delete the line or name the division this expert belongs to"
                ),
            ));
        }
    }

    Ok(Expert {
        slug,
        name,
        description,
        division,
        enabled,
        tools,
        prompt: fm.body.clone(),
        path: path.to_path_buf(),
    })
}

/// Build a team from one file, checking everything that does not need the rest
/// of the tree.
fn read_team(fm: &Frontmatter, path: &Path) -> Result<RawTeam> {
    let where_ = display(path);
    refuse_unknown_keys(fm, &TEAM_KEYS, path, "team")?;

    let name = required_scalar(fm, "name", path, "team")?;
    let slug = required_slug(fm, path, "team")?;
    let description = optional_scalar(fm, "description", path, "team")?;
    let coordinator = required_scalar(fm, "coordinator", path, "team")?;
    if !valid_slug(&coordinator) {
        return Err(Error::new(
            ErrorKind::BadSlug,
            format!(
                "{where_}: team {slug:?} has coordinator {} which is not a legal slug; a coordinator is an expert slug, so use lowercase letters, digits and dashes, e.g. release-lead",
                quote(&coordinator)
            ),
        ));
    }

    let raw_members = fm.get("members").ok_or_else(|| {
        Error::new(
            ErrorKind::MissingField,
            format!(
                "{where_}: team {slug:?} has a coordinator but no \"members:\"; list the experts it contains as \"- expert: <slug>\" entries, or delete the file if it is not a team"
            ),
        )
    })?;
    let items = raw_members.as_list().ok_or_else(|| {
        Error::new(
            ErrorKind::BadFrontmatter,
            format!(
                "{where_}: team {slug:?} has \"members: {}\"; a team needs a list of \"- expert: <slug>\" entries",
                raw_members.shape()
            ),
        )
    })?;

    let mut members = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let position = index + 1;
        let entries = item.as_map().ok_or_else(|| {
            Error::new(
                ErrorKind::BadFrontmatter,
                format!(
                    "{where_}: member {position} of team {slug:?} is {}; every member is a map with \"expert: <slug>\" and \"duty: <what it does>\", written as \"- expert: <slug>\" followed by an indented \"duty:\" line",
                    item.shape()
                ),
            )
        })?;
        for (key, _) in entries {
            if key != "expert" && key != "duty" {
                return Err(Error::new(
                    ErrorKind::BadFrontmatter,
                    format!(
                        "{where_}: member {position} of team {slug:?} has the field {}; a member carries exactly \"expert\" and \"duty\", so remove it or move the detail into the duty text",
                        quote(key)
                    ),
                ));
            }
        }
        let expert = entry_scalar(entries, "expert", &where_, position, &slug)?;
        let duty = entry_scalar(entries, "duty", &where_, position, &slug)?;
        if !valid_slug(&expert) {
            return Err(Error::new(
                ErrorKind::BadSlug,
                format!(
                    "{where_}: member {position} of team {slug:?} names expert {} which is not a legal slug; use lowercase letters, digits and dashes",
                    quote(&expert)
                ),
            ));
        }
        members.push(RawMember { expert, duty });
    }

    if members.len() < 2 {
        return Err(Error::new(
            ErrorKind::TooFewMembers,
            format!(
                "{where_}: team {slug:?} has {} member; a team needs at least 2, because a team of one is a subagent call with extra steps. Add the second member, or summon the one expert instead",
                members.len()
            ),
        ));
    }

    Ok(RawTeam {
        slug,
        name,
        description,
        coordinator,
        members,
        prompt: fm.body.clone(),
        path: path.to_path_buf(),
    })
}

/// Resolve a team's coordinator and members against the loaded experts.
fn resolve_team(raw: RawTeam, experts: &[Expert]) -> Result<Team> {
    let where_ = display(&raw.path);
    resolve_name(&raw.coordinator, experts, &raw, "coordinator", &where_)?;

    let mut members = Vec::with_capacity(raw.members.len());
    for member in &raw.members {
        let expert = resolve_name(&member.expert, experts, &raw, "member", &where_)?;
        members.push(TeamMember {
            expert: expert.slug.clone(),
            duty: member.duty.clone(),
            description: expert.description.clone(),
        });
    }

    Ok(Team {
        slug: raw.slug,
        name: raw.name,
        description: raw.description,
        coordinator: raw.coordinator,
        members,
        prompt: raw.prompt,
        path: raw.path,
    })
}

/// Resolve one expert slug for a team, refusing unknown and disabled separately.
///
/// The two refusals are different on purpose: "unknown" means the slug is wrong
/// or the file was never written, and the fix is to add or correct it; "disabled"
/// means the file exists and says `enabled: false`, and the fix is to enable it
/// or drop it from the team. A single message would send half the readers to the
/// wrong file.
fn resolve_name<'a>(
    slug: &str,
    experts: &'a [Expert],
    raw: &RawTeam,
    role: &str,
    where_: &str,
) -> Result<&'a Expert> {
    let known = || {
        experts
            .iter()
            .map(|expert| {
                if expert.enabled {
                    expert.slug.clone()
                } else {
                    format!("{} (disabled)", expert.slug)
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let Some(expert) = experts.iter().find(|expert| expert.slug == slug) else {
        return Err(Error::new(
            ErrorKind::UnknownMember,
            format!(
                "{where_}: team {} names {} expert {slug:?}, which no file in this roster defines; add a file with \"slug: {slug}\" or correct the name. Known experts: {}",
                quote(&raw.slug),
                role,
                known()
            ),
        ));
    };
    if !expert.enabled {
        return Err(Error::new(
            ErrorKind::DisabledMember,
            format!(
                "{where_}: team {} names {} expert {slug:?}, which is disabled by {}; set \"enabled: true\" in that file, or remove it from this team. Known experts: {}",
                quote(&raw.slug),
                role,
                display(&expert.path),
                known()
            ),
        ));
    }
    Ok(expert)
}

/// Refuse a field the definition kind does not know.
///
/// Why refuse instead of ignore: `tools:` mistyped as `tool:` would silently drop
/// a tool restriction, turning a read-only reviewer into a persona that can do
/// anything. A typo must be an error, not a quiet downgrade of what the user
/// approved.
fn refuse_unknown_keys(fm: &Frontmatter, allowed: &[&str], path: &Path, kind: &str) -> Result<()> {
    for (key, _) in &fm.fields {
        if !allowed.contains(&key.as_str()) {
            return Err(Error::new(
                ErrorKind::BadFrontmatter,
                format!(
                    "{}: unknown field {} in a {kind} definition; an unrecognised field is refused because a mistyped one would silently drop an instruction. Allowed fields: {}",
                    display(path),
                    quote(key),
                    allowed.join(", ")
                ),
            ));
        }
    }
    Ok(())
}

/// One required, non-empty scalar.
fn required_scalar(fm: &Frontmatter, key: &str, path: &Path, kind: &str) -> Result<String> {
    match fm.get(key) {
        None => Err(Error::new(
            ErrorKind::MissingField,
            format!(
                "{}: {kind} has no {key:?}; add a \"{key}: <text>\" line. {detail}",
                display(path),
                detail = required_hint(key)
            ),
        )),
        Some(value) => {
            let text = value.as_str().ok_or_else(|| {
                Error::new(
                    ErrorKind::BadFrontmatter,
                    format!(
                        "{}: {kind} has {key:?} as {}; write it as \"{key}: <text>\" on one line",
                        display(path),
                        value.shape()
                    ),
                )
            })?;
            if text.trim().is_empty() {
                return Err(Error::new(
                    ErrorKind::MissingField,
                    format!(
                        "{}: {kind} has an empty {key:?}; fill it in. {detail}",
                        display(path),
                        detail = required_hint(key)
                    ),
                ));
            }
            Ok(text.trim().to_string())
        }
    }
}

/// Why the two required text fields are required, for the refusal message.
fn required_hint(key: &str) -> &'static str {
    match key {
        "name" => "The name is what a person reads in a roster listing",
        "description" => {
            "The description is how a coordinator decides which expert to summon, and it is quoted in every team prompt that names this expert"
        }
        "coordinator" => "The coordinator is the expert that directs the team",
        _ => "Every field this crate requires is used to identify or assemble a prompt",
    }
}

/// One optional scalar: absent is `None`, present-but-empty is refused.
fn optional_scalar(fm: &Frontmatter, key: &str, path: &Path, kind: &str) -> Result<Option<String>> {
    match fm.get(key) {
        None => Ok(None),
        Some(value) => {
            let text = value.as_str().ok_or_else(|| {
                Error::new(
                    ErrorKind::BadFrontmatter,
                    format!(
                        "{}: {kind} has {key:?} as {}; write it as \"{key}: <text>\" on one line, or delete the line",
                        display(path),
                        value.shape()
                    ),
                )
            })?;
            if text.trim().is_empty() {
                return Err(Error::new(
                    ErrorKind::MissingField,
                    format!(
                        "{}: {kind} has an empty {key:?}; fill it in or delete the line — an empty line says nothing and hides that a value was meant",
                        display(path)
                    ),
                ));
            }
            Ok(Some(text.trim().to_string()))
        }
    }
}

/// The required `slug`, checked against the grammar.
fn required_slug(fm: &Frontmatter, path: &Path, kind: &str) -> Result<String> {
    let slug = required_scalar(fm, "slug", path, kind)?;
    if !valid_slug(&slug) {
        return Err(Error::new(
            ErrorKind::BadSlug,
            format!(
                "{}: {kind} has slug {} which is not a legal slug; use 1 to {SLUG_MAX} characters of lowercase letters, digits and dashes, starting with a letter or digit (the slug is a file name and a command-line argument, so it cannot contain spaces, uppercase or slashes)",
                display(path),
                quote(&slug)
            ),
        ));
    }
    Ok(slug)
}

/// Read `enabled:`, defaulting to `true`.
///
/// Why default to enabled: a file that says nothing about being retired is a
/// roster entry the author intends to use, and a new expert is written by writing
/// a persona, not by flipping a flag. The flag exists to withdraw one, so off is
/// the value that must be spelled out.
fn read_enabled(fm: &Frontmatter, path: &Path) -> Result<bool> {
    let Some(value) = fm.get("enabled") else {
        return Ok(true);
    };
    let text = value.as_str().ok_or_else(|| {
        Error::new(
            ErrorKind::BadFrontmatter,
            format!(
                "{}: \"enabled\" is {}; write \"enabled: true\" or \"enabled: false\"",
                display(path),
                value.shape()
            ),
        )
    })?;
    match text.trim().to_ascii_lowercase().as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(Error::new(
            ErrorKind::BadFrontmatter,
            format!(
                "{}: \"enabled: {other}\" is not a yes/no value; write \"enabled: true\" or \"enabled: false\", because an expert whose state is unclear must not be summonable by accident",
                display(path)
            ),
        )),
    }
}

/// A list of single-token strings, like `tools: [fs_read, fs_grep]`.
fn read_string_list(value: &Value, path: &Path, key: &str) -> Result<Vec<String>> {
    let items = value.as_list().ok_or_else(|| {
        Error::new(
            ErrorKind::BadFrontmatter,
            format!(
                "{}: {key:?} is {}; write a list of names, e.g. \"{key}: [fs_read, fs_grep]\" or a \"- name\" block",
                display(path),
                value.shape()
            ),
        )
    })?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let text = item.as_str().ok_or_else(|| {
            Error::new(
                ErrorKind::BadFrontmatter,
                format!(
                    "{}: every {key:?} item must be one name, but one is {}; remove the nesting",
                    display(path),
                    item.shape()
                ),
            )
        })?;
        let text = text.trim();
        if text.is_empty() {
            return Err(Error::new(
                ErrorKind::BadFrontmatter,
                format!(
                    "{}: {key:?} contains an empty item; remove the stray comma",
                    display(path)
                ),
            ));
        }
        if text.split_whitespace().count() != 1 {
            return Err(Error::new(
                ErrorKind::BadFrontmatter,
                format!(
                    "{}: {key:?} item {} contains a space; a tool name is one token, so quote nothing and fix the name",
                    display(path),
                    quote(text)
                ),
            ));
        }
        out.push(text.to_string());
    }
    Ok(out)
}

/// One required key inside a `members:` entry.
fn entry_scalar(
    entries: &[(String, Value)],
    key: &str,
    where_: &str,
    position: usize,
    team_slug: &str,
) -> Result<String> {
    let value = entries
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value);
    let Some(value) = value else {
        return Err(Error::new(
            ErrorKind::MissingField,
            format!(
                "{where_}: member {position} of team {team_slug:?} has no {key:?}; every member says which expert it is (\"expert: <slug>\") and what it does here (\"duty: <text>\")"
            ),
        ));
    };
    let text = value.as_str().ok_or_else(|| {
        Error::new(
            ErrorKind::BadFrontmatter,
            format!(
                "{where_}: member {position} of team {team_slug:?} has {key:?} as {}; write it as \"{key}: <text>\"",
                value.shape()
            ),
        )
    })?;
    if text.trim().is_empty() {
        return Err(Error::new(
            ErrorKind::MissingField,
            format!(
                "{where_}: member {position} of team {team_slug:?} has an empty {key:?}; fill it in"
            ),
        ));
    }
    Ok(text.trim().to_string())
}

/// The refusal for a slug that appears twice, naming both paths.
///
/// Both kinds are named because the cross-kind collision (an expert and a team
/// answering to one slug) is the one a reader will not guess from the paths
/// alone: the files look unrelated until you know one of them is a team.
fn duplicate_slug(slug: &str, first: (&Path, Definition), second: (&Path, Definition)) -> Error {
    Error::new(
        ErrorKind::DuplicateSlug,
        format!(
            "duplicate slug {slug:?}: defined as {} in {} and again as {} in {}; \"which one wins\" is exactly the ambiguity a roster must not have, so rename one of them (the slug is the definition's identity on the command line)",
            first.1.label(),
            display(first.0),
            second.1.label(),
            display(second.0)
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    /// A filesystem that is the real one except where a test injects a failure.
    struct FakeFs {
        /// Path suffixes whose `metadata` call fails with `NotFound`.
        missing: Vec<String>,
        /// Path suffixes whose `read_dir` call fails with `PermissionDenied`.
        unlistable: Vec<String>,
        /// Path suffixes whose `open` call fails with `PermissionDenied`.
        unopenable: Vec<String>,
    }

    impl FakeFs {
        fn new() -> Self {
            FakeFs {
                missing: Vec::new(),
                unlistable: Vec::new(),
                unopenable: Vec::new(),
            }
        }
    }

    impl Fs for FakeFs {
        fn metadata(&self, path: &Path) -> io::Result<fsio::Meta> {
            let text = display(path);
            if self.missing.iter().any(|suffix| text.ends_with(suffix)) {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            }
            RealFs.metadata(path)
        }

        fn read_dir(&self, path: &Path) -> io::Result<Vec<String>> {
            let text = display(path);
            if self.unlistable.iter().any(|suffix| text.ends_with(suffix)) {
                return Err(io::Error::from(io::ErrorKind::PermissionDenied));
            }
            RealFs.read_dir(path)
        }

        fn open(&self, path: &Path) -> io::Result<fs::File> {
            let text = display(path);
            if self.unopenable.iter().any(|suffix| text.ends_with(suffix)) {
                return Err(io::Error::from(io::ErrorKind::PermissionDenied));
            }
            RealFs.open(path)
        }

        fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
            RealFs.canonicalize(path)
        }
    }

    /// One scratch directory that removes itself.
    struct Scratch {
        path: PathBuf,
    }

    impl Scratch {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "forge-experts-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("create scratch dir");
            Scratch { path }
        }

        fn write(&self, name: &str, text: &str) -> PathBuf {
            let path = self.path.join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create parent");
            }
            fs::write(&path, text).expect("write fixture");
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    const GOOD_EXPERT: &str =
        "---\nname: Reviewer\nslug: reviewer\ndescription: reviews things\n---\nYou review.\n";

    /// One legal expert file with the given slug, plus optional extra lines in
    /// the frontmatter and an optional body.
    fn expert_file(slug: &str, extra: &str, body: &str) -> String {
        format!("---\nname: {slug}\nslug: {slug}\ndescription: reviews {slug}\n{extra}---\n{body}")
    }

    #[test]
    fn a_missing_root_is_an_empty_set() {
        let scratch = Scratch::new("missing-root");
        let set = scan(&scratch.path.join("no-such-dir")).expect("absent root is not an error");
        assert!(set.is_empty());
        assert_eq!(set.len(), 0);
    }

    #[test]
    fn a_root_that_is_a_file_is_refused() {
        let scratch = Scratch::new("root-file");
        let path = scratch.write("roster.md", GOOD_EXPERT);
        let err = scan(&path).expect_err("a file root is a mistake");
        assert_eq!(err.kind(), ErrorKind::Unreadable);
        assert!(err.message().contains("is a file, not a directory"));
    }

    #[test]
    fn an_unlistable_root_is_refused() {
        let scratch = Scratch::new("stat-fail");
        scratch.write("a.md", GOOD_EXPERT);
        let bad = FakeFs {
            unlistable: vec![display(&scratch.path)],
            ..FakeFs::new()
        };
        let err = scan_with(&bad, &scratch.path).expect_err("unlistable root is refused");
        assert_eq!(err.kind(), ErrorKind::Unreadable);
        assert!(err.message().contains("cannot list"));
    }

    #[test]
    fn an_unreadable_definition_is_refused() {
        let scratch = Scratch::new("open-fail");
        scratch.write("a.md", GOOD_EXPERT);
        let bad = FakeFs {
            unopenable: vec!["a.md".to_string()],
            ..FakeFs::new()
        };
        let err = scan_with(&bad, &scratch.path).expect_err("unopenable file is refused");
        assert_eq!(err.kind(), ErrorKind::Unreadable);
        assert!(err.message().contains("permission denied"));
    }

    #[test]
    fn a_broken_entry_is_refused() {
        let scratch = Scratch::new("dangling");
        scratch.write("a.md", GOOD_EXPERT);
        // The file exists so the walker lists it; the fake then reports its stat
        // as missing, which is what a dangling symlink looks like to `metadata`.
        scratch.write("broken.md", GOOD_EXPERT);
        let bad = FakeFs {
            missing: vec!["broken.md".to_string()],
            ..FakeFs::new()
        };
        let err = scan_with(&bad, &scratch.path).expect_err("dangling entry is refused");
        assert_eq!(err.kind(), ErrorKind::Unreadable);
        assert!(err.message().contains("cannot stat"));
    }

    #[test]
    fn a_file_over_the_cap_is_refused_not_truncated() {
        let scratch = Scratch::new("cap");
        let path = scratch.path.join("big.md");
        let mut file = fs::File::create(&path).expect("create");
        file.write_all(&vec![b'x'; fsio::MAX_FILE_BYTES + 1])
            .expect("write");
        let err = scan(&scratch.path).expect_err("oversized file is refused");
        assert_eq!(err.kind(), ErrorKind::Unreadable);
        assert!(err.message().contains("larger than the 1024 KiB limit"));
    }

    #[test]
    fn dot_entries_and_non_markdown_files_are_skipped() {
        let scratch = Scratch::new("skip");
        scratch.write("a.md", GOOD_EXPERT);
        scratch.write("notes.txt", "not a definition\n");
        scratch.write(".cache/prose.md", "no frontmatter at all\n");
        scratch.write(
            "nested/deeper/b.md",
            &GOOD_EXPERT.replace("reviewer", "second"),
        );
        let set = scan(&scratch.path).expect("a usable tree");
        assert_eq!(
            set.experts()
                .iter()
                .map(|expert| expert.slug.as_str())
                .collect::<Vec<_>>(),
            vec!["reviewer", "second"]
        );
    }

    #[test]
    fn a_tree_deeper_than_the_depth_cap_is_refused() {
        let scratch = Scratch::new("depth");
        let mut relative = String::new();
        for _ in 0..(MAX_DEPTH + 2) {
            relative.push_str("d/");
        }
        scratch.write(&format!("{relative}a.md"), GOOD_EXPERT);
        let err = scan(&scratch.path).expect_err("a deep tree is a wrong root");
        assert_eq!(err.kind(), ErrorKind::Unreadable);
        assert!(err.message().contains("deeper than"));
    }

    #[test]
    fn a_directory_reached_twice_is_walked_once() {
        // A symlink loop is the real case; a repeated root is the same shape and
        // needs no privilege on Windows to set up.
        let scratch = Scratch::new("cycles");
        scratch.write("a.md", GOOD_EXPERT);
        let mut visited = Vec::new();
        let mut out = Vec::new();
        walk(&RealFs, &scratch.path, 0, &mut visited, &mut out).expect("walk once");
        walk(&RealFs, &scratch.path, 0, &mut visited, &mut out).expect("walk twice");
        assert_eq!(out.len(), 1, "the second walk must add nothing");
    }

    #[test]
    fn enabled_defaults_to_true_and_accepts_only_booleans() {
        let scratch = Scratch::new("enabled");
        scratch.write(
            "a.md",
            "---\nname: A\nslug: a\ndescription: d\nenabled: FALSE\n---\nbody\n",
        );
        let set = scan(&scratch.path).expect("scan");
        assert!(!set.expert("a").expect("expert").enabled);

        let scratch = Scratch::new("enabled-bad");
        scratch.write(
            "a.md",
            "---\nname: A\nslug: a\ndescription: d\nenabled: yes\n---\nbody\n",
        );
        let err = scan(&scratch.path).expect_err("a vague flag is refused");
        assert_eq!(err.kind(), ErrorKind::BadFrontmatter);
        assert!(err.message().contains("is not a yes/no value"));
    }

    #[test]
    fn an_unknown_field_is_refused() {
        let scratch = Scratch::new("unknown-key");
        scratch.write(
            "a.md",
            "---\nname: A\nslug: a\ndescription: d\ntool: [fs_read]\n---\nbody\n",
        );
        let err = scan(&scratch.path).expect_err("a typo must not silently drop a restriction");
        assert_eq!(err.kind(), ErrorKind::BadFrontmatter);
        assert!(err.message().contains("unknown field \"tool\""));
        assert!(err
            .message()
            .contains("Allowed fields: name, slug, description"));
    }

    #[test]
    fn a_missing_name_is_refused_with_the_fix() {
        let scratch = Scratch::new("missing-name");
        scratch.write("a.md", "---\nslug: a\ndescription: d\n---\nbody\n");
        let err = scan(&scratch.path).expect_err("missing name is refused");
        assert_eq!(err.kind(), ErrorKind::MissingField);
        assert!(err.message().contains("has no \"name\""));
    }

    #[test]
    fn a_missing_description_is_refused() {
        let scratch = Scratch::new("missing-desc");
        scratch.write("a.md", "---\nname: A\nslug: a\n---\nbody\n");
        let err = scan(&scratch.path).expect_err("missing description is refused");
        assert_eq!(err.kind(), ErrorKind::MissingField);
        assert!(err.message().contains("has no \"description\""));
    }

    #[test]
    fn a_bad_slug_is_refused() {
        let too_long = "a".repeat(SLUG_MAX + 1);
        let cases = [
            "Review",
            "with space",
            "-leading",
            "a/b",
            "under_score",
            too_long.as_str(),
        ];
        for slug in cases {
            let scratch = Scratch::new("bad-slug");
            scratch.write(
                "a.md",
                &format!("---\nname: A\nslug: {slug}\ndescription: d\n---\nbody\n"),
            );
            let result = scan(&scratch.path);
            assert!(result.is_err(), "slug {slug:?} must be refused");
            let err = result.expect_err("refused");
            assert_eq!(err.kind(), ErrorKind::BadSlug, "slug {slug:?}");
            assert!(
                err.message()
                    .contains("file name and a command-line argument"),
                "{}",
                err.message()
            );
        }

        // An empty slug is a *missing* field, not a malformed one: there is
        // nothing to fix by renaming, so the code says so. A bare `slug:` with
        // nothing after it is refused earlier, by the frontmatter reader, as a
        // key that has no value at all.
        let scratch = Scratch::new("empty-slug");
        scratch.write(
            "a.md",
            "---\nname: A\nslug: \"\"\ndescription: d\n---\nbody\n",
        );
        let err = scan(&scratch.path).expect_err("empty slug is refused");
        assert_eq!(err.kind(), ErrorKind::MissingField);
    }

    #[test]
    fn a_team_with_one_member_is_refused() {
        let scratch = Scratch::new("one-member");
        scratch.write("a.md", GOOD_EXPERT);
        scratch.write(
            "team.md",
            "---\nname: T\nslug: t\ncoordinator: reviewer\nmembers:\n  - expert: reviewer\n    duty: everything\n---\nbody\n",
        );
        let err = scan(&scratch.path).expect_err("a team of one is refused");
        assert_eq!(err.kind(), ErrorKind::TooFewMembers);
        assert!(err.message().contains("has 1 member"));
        assert!(err.message().contains("at least 2"));
    }

    #[test]
    fn a_team_without_members_is_refused_as_a_field_problem() {
        let scratch = Scratch::new("no-members");
        scratch.write(
            "team.md",
            "---\nname: T\nslug: t\ncoordinator: reviewer\n---\nbody\n",
        );
        let err = scan(&scratch.path).expect_err("a coordinator without members is a team");
        assert_eq!(err.kind(), ErrorKind::MissingField);
        assert!(err.message().contains("no \"members:\""));
    }

    #[test]
    fn a_member_without_a_duty_is_refused() {
        let scratch = Scratch::new("no-duty");
        scratch.write("a.md", GOOD_EXPERT);
        scratch.write("b.md", &GOOD_EXPERT.replace("reviewer", "second"));
        scratch.write(
            "team.md",
            "---\nname: T\nslug: t\ncoordinator: reviewer\nmembers:\n  - expert: reviewer\n  - expert: second\n    duty: helps\n---\nbody\n",
        );
        let err = scan(&scratch.path).expect_err("a member with no duty is refused");
        assert_eq!(err.kind(), ErrorKind::MissingField);
        assert!(err.message().contains("has no \"duty\""));
    }

    #[test]
    fn a_disabled_expert_is_kept_but_not_summonable() {
        let scratch = Scratch::new("disabled-kept");
        scratch.write(
            "a.md",
            "---\nname: A\nslug: retired\ndescription: d\nenabled: false\n---\nold prompt\n",
        );
        let set = scan(&scratch.path).expect("scan");
        assert_eq!(set.experts().len(), 1, "the definition is kept");
        assert!(!set.expert("retired").expect("kept").enabled);
        assert!(set.summonable("retired").is_none());
        assert_eq!(prompt_for(&set, "retired"), None);
    }

    #[test]
    fn a_duplicate_slug_names_both_paths() {
        let scratch = Scratch::new("dup");
        let first = scratch.write("a/one.md", GOOD_EXPERT);
        let second = scratch.write("b/two.md", GOOD_EXPERT);
        let err = scan(&scratch.path).expect_err("duplicate slug is refused");
        assert_eq!(err.kind(), ErrorKind::DuplicateSlug);
        let message = err.message();
        assert!(message.contains("duplicate slug \"reviewer\""));
        assert!(
            message.contains(&display(&first)) && message.contains(&display(&second)),
            "{message}"
        );
    }

    #[test]
    fn a_team_slug_colliding_with_an_expert_slug_is_a_duplicate() {
        let scratch = Scratch::new("cross-dup");
        scratch.write("a.md", GOOD_EXPERT);
        scratch.write("b.md", &GOOD_EXPERT.replace("reviewer", "second"));
        scratch.write(
            "team.md",
            "---\nname: T\nslug: reviewer\ncoordinator: reviewer\nmembers:\n  - expert: reviewer\n    duty: x\n  - expert: second\n    duty: y\n---\nbody\n",
        );
        let err = scan(&scratch.path).expect_err("one slug namespace");
        assert_eq!(err.kind(), ErrorKind::DuplicateSlug);
    }

    #[test]
    fn an_unknown_coordinator_is_refused() {
        let scratch = Scratch::new("unknown-coord");
        scratch.write("a.md", GOOD_EXPERT);
        scratch.write("b.md", &GOOD_EXPERT.replace("reviewer", "second"));
        scratch.write(
            "team.md",
            "---\nname: T\nslug: t\ncoordinator: ghost\nmembers:\n  - expert: reviewer\n    duty: x\n  - expert: second\n    duty: y\n---\nbody\n",
        );
        let err = scan(&scratch.path).expect_err("unknown coordinator is refused");
        assert_eq!(err.kind(), ErrorKind::UnknownMember);
        assert!(err.message().contains("coordinator expert \"ghost\""));
        assert!(err.message().contains("team.md"));
    }

    #[test]
    fn a_disabled_member_is_a_different_refusal_from_an_unknown_one() {
        let scratch = Scratch::new("disabled-member");
        scratch.write("a.md", GOOD_EXPERT);
        scratch.write(
            "b.md",
            "---\nname: B\nslug: retired\ndescription: d\nenabled: false\n---\nbody\n",
        );
        scratch.write(
            "team.md",
            "---\nname: T\nslug: t\ncoordinator: reviewer\nmembers:\n  - expert: reviewer\n    duty: x\n  - expert: retired\n    duty: y\n---\nbody\n",
        );
        let err = scan(&scratch.path).expect_err("disabled member is refused");
        assert_eq!(err.kind(), ErrorKind::DisabledMember);
        assert!(err.message().contains("which is disabled by"));
        assert!(err.message().contains("b.md"));
        assert!(err.message().contains("enabled: true"));
    }

    #[test]
    fn prompts_are_byte_identical_across_calls_and_keep_file_order() {
        let scratch = Scratch::new("prompt");
        scratch.write("a.md", GOOD_EXPERT);
        scratch.write("b.md", &GOOD_EXPERT.replace("reviewer", "second"));
        scratch.write(
            "team.md",
            "---\nname: T\nslug: t\ncoordinator: reviewer\nmembers:\n  - expert: second\n    duty: re-records\n  - expert: reviewer\n    duty: reviews\n---\nbody\n",
        );
        let set = scan(&scratch.path).expect("scan");
        let first = team_prompt(&set, "t").expect("prompt");
        let second = team_prompt(&set, "t").expect("prompt");
        assert_eq!(first, second);
        let second_line = first.lines().find(|line| line.starts_with("1. "));
        assert_eq!(
            second_line,
            Some("1. second: duty = re-records; description = reviews things")
        );
        assert!(first.starts_with("body\n"));
        assert_eq!(team_prompt(&set, "nope"), None);
    }

    #[test]
    fn prompt_for_appends_the_tool_line_only_when_tools_are_present() {
        let scratch = Scratch::new("tools");
        scratch.write("a.md", GOOD_EXPERT);
        scratch.write(
            "b.md",
            "---\nname: B\nslug: limited\ndescription: d\ntools: [fs_read, fs_grep]\n---\nBe careful.\n",
        );
        scratch.write(
            "c.md",
            "---\nname: C\nslug: toothless\ndescription: d\ntools: []\n---\nAnswer only.\n",
        );
        let set = scan(&scratch.path).expect("scan");
        assert_eq!(
            prompt_for(&set, "reviewer").as_deref(),
            Some("You review.\n")
        );
        assert_eq!(
            prompt_for(&set, "limited").as_deref(),
            Some(
                "Be careful.\n\n[forge-experts] Tool restriction: fs_read, fs_grep. This expert must not call any other tool.\n"
            )
        );
        assert_eq!(
            prompt_for(&set, "toothless").as_deref(),
            Some(
                "Answer only.\n\n[forge-experts] Tool restriction: this expert may not call any tool. Answer from what you are given.\n"
            )
        );
        assert_eq!(prompt_for(&set, "ghost"), None);
    }

    #[test]
    fn a_tool_name_with_a_space_is_refused() {
        let scratch = Scratch::new("tool-space");
        scratch.write(
            "a.md",
            "---\nname: A\nslug: a\ndescription: d\ntools: [fs_read, fs grep]\n---\nbody\n",
        );
        let err = scan(&scratch.path).expect_err("a two-word tool name is refused");
        assert_eq!(err.kind(), ErrorKind::BadFrontmatter);
        assert!(err.message().contains("contains a space"));
    }

    #[test]
    fn a_team_body_is_kept_verbatim_including_crlf() {
        let scratch = Scratch::new("verbatim");
        let raw = b"---\r\nname: T\r\nslug: t\r\ncoordinator: reviewer\r\nmembers:\r\n  - expert: reviewer\r\n    duty: x\r\n  - expert: second\r\n    duty: y\r\n---\r\nteam body\r\n";
        let path = scratch.path.join("team.md");
        fs::write(&path, raw).expect("write");
        scratch.write("a.md", GOOD_EXPERT);
        scratch.write("b.md", &GOOD_EXPERT.replace("reviewer", "second"));
        let set = scan(&scratch.path).expect("scan");
        let team = set.team("t").expect("team");
        assert_eq!(team.prompt.as_bytes(), b"team body\r\n");
        let prompt = team_prompt(&set, "t").expect("prompt");
        assert!(prompt.starts_with("team body\r\n"));
    }

    #[test]
    fn a_division_may_be_absent_or_present() {
        let scratch = Scratch::new("division");
        scratch.write("a.md", &expert_file("plain", "", "You are plain.\n"));
        scratch.write(
            "b.md",
            &expert_file("grouped", "division: engineering\n", "You are grouped.\n"),
        );
        let set = scan(&scratch.path).expect("scan");
        assert_eq!(set.expert("plain").expect("a").division, None);
        assert_eq!(
            set.expert("grouped").expect("b").division.as_deref(),
            Some("engineering")
        );

        let scratch = Scratch::new("division-empty");
        scratch.write("a.md", &expert_file("blank", "division: \"\"\n", "body\n"));
        let err = scan(&scratch.path).expect_err("an empty division is a typo");
        assert_eq!(err.kind(), ErrorKind::MissingField);
        assert!(err.message().contains("empty \"division\""));
    }

    #[test]
    fn slugs_of_the_maximum_length_are_legal_and_one_more_is_not() {
        assert!(valid_slug(&"a".repeat(SLUG_MAX)));
        assert!(!valid_slug(&"a".repeat(SLUG_MAX + 1)));
        assert!(valid_slug("0"));
        assert!(!valid_slug(""));
        assert!(!valid_slug("_leading"));
        assert!(valid_slug("a-b-9"));
    }
}
