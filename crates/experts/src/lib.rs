//! Expert and expert-team definitions, read from Markdown plus frontmatter.
//!
//! An *expert* is a persona that can be summoned as a subagent: a Markdown file
//! whose frontmatter carries `name`, `slug`, `description` and optionally
//! `division`, `enabled` and `tools`, and whose body is the system prompt. A
//! *team* is a Markdown file that names experts and their duties, plus one
//! coordinator. [`scan`] reads a directory tree of them and hands back a [`Set`]
//! that has already been checked, and [`prompt_for`] / [`team_prompt`] turn one
//! entry of that set into the exact string a caller sends to a model.
//!
//! Three decisions are worth knowing before reading further.
//!
//! **Refuse, do not warn.** `hearth-skill` reads its own Markdown with a parser
//! that warns and keeps going, which is right for a skill: a skill that lost an
//! optional key still helps. A roster is different in kind. Losing a `tools:`
//! line turns a read-only reviewer into a persona that may run commands; losing a
//! `members:` line turns a four-person team into a three-person team without a
//! word. So this crate refuses a file it does not fully understand, and every
//! refusal names the file and the fix.
//!
//! **The body is bytes.** [`Expert::prompt`] and [`Team::prompt`] are the file
//! body exactly as written, including `\r\n`, tabs and trailing newlines. A
//! loader that normalised anything would make the prompt differ from the file a
//! reviewer approved, and then no diff could tell anyone which one the model saw.
//!
//! **Assembly is a pure function.** [`prompt_for`] and [`team_prompt`] take the
//! loaded set and a slug and nothing else: no clock, no filesystem, no hash map
//! iteration. Two calls produce byte-identical strings, and a team's members
//! appear in the order its file lists them, not in sorted order, because the file
//! is what was reviewed.
//!
//! Why this crate does not reuse `hearth-skill`'s reader: that parser is
//! `pub(crate)`, so it is not a surface at all; it scans exactly `root/*/SKILL.md`
//! and never recurses; and its policies are the opposite of the two above (it
//! drops keys it cannot read and normalises the body). Reuse would have meant
//! widening another crate's privacy and then fighting its policy on every field.
//! See `README.md` for the same note in the place a maintainer looks first.
//!
//! This crate is L1 and depends on nothing. The roster is text; it needs the
//! standard library and the filesystem, and a dependency would only add a version
//! to argue about.

#![forbid(unsafe_code)]

mod error;
mod frontmatter;
mod fsio;
mod roster;
mod util;

pub use crate::error::{Error, ErrorKind, Result};
pub use crate::roster::{prompt_for, scan, team_prompt, Expert, Set, Team, TeamMember};

/// The stable code strings this crate can report, for a caller that wants to
/// document or test the whole set at once.
///
/// Kept next to the re-exports so `doctor` can print the list without reaching
/// into the error module, and so a new variant cannot be added without a test
/// noticing that this array changed length.
pub const ERROR_CODES: [&str; 9] = [
    "missing_frontmatter",
    "bad_frontmatter",
    "missing_field",
    "bad_slug",
    "duplicate_slug",
    "unknown_member",
    "disabled_member",
    "too_few_members",
    "unreadable",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_match_the_enum() {
        let kinds = [
            ErrorKind::MissingFrontmatter,
            ErrorKind::BadFrontmatter,
            ErrorKind::MissingField,
            ErrorKind::BadSlug,
            ErrorKind::DuplicateSlug,
            ErrorKind::UnknownMember,
            ErrorKind::DisabledMember,
            ErrorKind::TooFewMembers,
            ErrorKind::Unreadable,
        ];
        assert_eq!(kinds.len(), ERROR_CODES.len());
        for (kind, code) in kinds.iter().zip(ERROR_CODES) {
            assert_eq!(kind.as_str(), code);
        }
    }

    #[test]
    fn an_empty_set_is_empty_and_answers_none() {
        let set = Set::empty();
        assert!(set.is_empty());
        assert_eq!(set.len(), 0);
        assert!(set.experts().is_empty());
        assert!(set.teams().is_empty());
        assert!(set.expert("anything").is_none());
        assert!(set.team("anything").is_none());
        assert_eq!(prompt_for(&set, "anything"), None);
        assert_eq!(team_prompt(&set, "anything"), None);
    }
}
