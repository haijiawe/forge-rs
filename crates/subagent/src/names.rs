//! Who a child is, inside its parent: the `lead` root, `agent-N` siblings, and
//! slash paths.
//!
//! These are the naming rules DSH uses (`packages/subagent/subagent/src/names.ts`),
//! ported without the remote RPC face. A name is what a parent can say to reach a
//! child, so the rules are deliberately narrow: an ASCII lowercase word, at most
//! 32 bytes, never the reserved root name. A name is chosen once, when the child
//! is created, and recorded in the parent's `subagent/catalog`; it is not derived
//! again later, because a derived name would move if a sibling were renamed.

use std::collections::BTreeSet;

/// The name of the root session every other name hangs off.
pub const ROOT_AGENT_NAME: &str = "lead";

/// The longest name, in bytes.
const MAX_NAME_LEN: usize = 32;

/// Is `name` a legal agent name on its own? (`lead` matches the pattern; refusing
/// to *assign* it to a child is [`reserve`]'s job, not this predicate's.)
pub fn is_agent_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_NAME_LEN {
        return false;
    }
    if !bytes[0].is_ascii_lowercase() {
        return false;
    }
    name.bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Why a requested name could not be granted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameError {
    /// The name does not match the pattern.
    Invalid(String),
    /// The name is already used by a sibling (or is the reserved root name).
    Duplicate(String),
}

impl NameError {
    pub fn message(&self) -> String {
        match self {
            NameError::Invalid(name) => format!(
                "subagent: {name:?} is not a valid agent name (1-32 lowercase letters, digits \
                 and hyphens, starting with a letter)"
            ),
            NameError::Duplicate(name) => {
                format!("subagent: a sibling is already named {name:?}")
            }
        }
    }
}

/// Pick the name for a new child among its siblings.
///
/// `committed` is the set of names already recorded in the parent catalog. With
/// no `requested` name this yields the first free `agent-N`; a requested name is
/// validated and checked for collision. The result is deterministic given the
/// same `committed` set, which is what makes two parents that see the same log
/// agree on the name without talking to each other.
pub fn reserve(committed: &BTreeSet<String>, requested: Option<&str>) -> Result<String, NameError> {
    match requested {
        Some(name) => {
            if !is_agent_name(name) {
                return Err(NameError::Invalid(name.to_string()));
            }
            if name == ROOT_AGENT_NAME || committed.contains(name) {
                return Err(NameError::Duplicate(name.to_string()));
            }
            Ok(name.to_string())
        }
        None => {
            let mut index = 1usize;
            loop {
                let candidate = format!("agent-{index}");
                if !committed.contains(&candidate) {
                    return Ok(candidate);
                }
                index += 1;
            }
        }
    }
}

/// The path of `name` under `parent_path`. The root's path is empty, so its
/// children are bare names.
pub fn join_agent_path(parent_path: &str, name: &str) -> String {
    if parent_path.is_empty() {
        name.to_string()
    } else {
        format!("{parent_path}/{name}")
    }
}

/// The parent path of a slash path, or an empty string when `path` is a root.
pub fn parent_agent_path(path: &str) -> &str {
    match path.rfind('/') {
        Some(index) => &path[..index],
        None => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pattern_accepts_what_the_cli_documents() {
        for ok in ["agent-1", "a", "rust-reviewer", "x0", "lead"] {
            assert!(is_agent_name(ok), "{ok:?} should be legal");
        }
        for bad in ["", "Agent", "1agent", "agent 1", "agent_1", "-x"] {
            assert!(!is_agent_name(bad), "{bad:?} should be illegal");
        }
        // A trailing hyphen survives the pattern (`[a-z0-9-]{0,31}`), the same
        // way DSH's regex allows it; the pattern constrains the alphabet, not
        // aesthetics.
        assert!(is_agent_name("agent-"));
        assert!(!is_agent_name(&"a".repeat(33)), "33 bytes is too long");
        assert!(is_agent_name(&"a".repeat(32)), "32 bytes is the limit");
    }

    #[test]
    fn the_default_name_is_the_first_free_sibling_slot() {
        let mut committed = BTreeSet::new();
        assert_eq!(reserve(&committed, None), Ok("agent-1".to_string()));
        committed.insert("agent-1".to_string());
        assert_eq!(reserve(&committed, None), Ok("agent-2".to_string()));
        committed.insert("agent-3".to_string());
        assert_eq!(reserve(&committed, None), Ok("agent-2".to_string()));
    }

    #[test]
    fn a_requested_name_is_validated_and_checked_for_collision() {
        let mut committed = BTreeSet::new();
        assert_eq!(
            reserve(&committed, Some("research")),
            Ok("research".to_string())
        );
        committed.insert("research".to_string());
        assert_eq!(
            reserve(&committed, Some("research")),
            Err(NameError::Duplicate("research".to_string()))
        );
        assert_eq!(
            reserve(&committed, Some("lead")),
            Err(NameError::Duplicate("lead".to_string())),
            "the root name is reserved"
        );
        assert_eq!(
            reserve(&committed, Some("Bad Name")),
            Err(NameError::Invalid("Bad Name".to_string()))
        );
    }

    #[test]
    fn paths_join_under_the_root_and_split_back() {
        assert_eq!(join_agent_path("", "agent-1"), "agent-1");
        assert_eq!(join_agent_path("agent-1", "agent-2"), "agent-1/agent-2");
        assert_eq!(parent_agent_path("agent-1"), "");
        assert_eq!(parent_agent_path("agent-1/agent-2"), "agent-1");
    }
}
