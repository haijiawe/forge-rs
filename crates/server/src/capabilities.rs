//! What this process can do, as a read-only snapshot for the browser.
//!
//! Layer L3. The server is deliberately the wrong place to *discover* these
//! things: a plugin root needs `hearth-plugin`, a roster needs `hearth-experts`, a
//! bundle list needs `hearth-market`, and pulling those in would turn the HTTP
//! layer into the assembly layer (and would put process-spawning code on the
//! request path). So the CLI computes the report once at startup, and this crate
//! only carries and serialises it.
//!
//! Two rules for what belongs here:
//!
//! 1. **Describe, do not start.** A plugin that is installed but not enabled is
//!    the single most useful row in this table — it is what the operator forgot
//!    to turn on — so the snapshot names it with `active: false` rather than
//!    omitting it. Nothing here may launch a child process.
//! 2. **An empty group means "none configured", never "not supported".** Every
//!    group is always present in the JSON, even when empty, because a missing
//!    key and an empty list read differently to a UI and the second one is true.
//!
//! A third rule governs *who fills* each section. Configuration — plugins,
//! experts, teams, bundles, skills, MCP servers, and the tool list — can only be
//! discovered by the CLI, because discovering it means reading roots that belong
//! to crates above this one. Build vocabulary cannot: `events` comes from
//! `hearth_core::EventType::ALL` and `routes` from [`crate::ROUTES`], both of
//! which this crate already has. [`crate::Server::capability_report`] therefore
//! fills those two on every request, so no caller can forget them and no
//! snapshot can claim a build knows zero event types.

use serde::{Deserialize, Serialize};

/// One capability row: a name, something to show under it, and whether the
/// model can actually reach it in this session.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// The stable name: a plugin name, an expert slug, a server name, a path.
    pub name: String,
    /// One line of context a human needs to act on the row.
    pub detail: String,
    /// True when this build exposes it to the model right now.
    pub active: bool,
}

impl Entry {
    pub fn new(name: impl Into<String>, detail: impl Into<String>, active: bool) -> Self {
        Self {
            name: name.into(),
            detail: detail.into(),
            active,
        }
    }

    /// A row for something configured but deliberately not in use.
    pub fn inactive(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(name, detail, false)
    }
}

/// The whole snapshot. Field names are the wire contract; adding a group is
/// additive and safe, renaming one is not.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// The sandbox mode this process enforces (`read-only`, ...).
    pub mode: String,
    /// The workspace the tools are confined to.
    pub workspace: String,
    /// The model name the session will ask for.
    pub model: String,
    /// Installed plugins, whether enabled or not.
    pub plugins: Vec<Entry>,
    /// Experts found on the roster, whether summonable or not.
    pub experts: Vec<Entry>,
    /// Teams defined on the roster.
    pub teams: Vec<Entry>,
    /// Bundles installed from a market.
    pub bundles: Vec<Entry>,
    /// Skills found on the configured roots.
    pub skills: Vec<Entry>,
    /// MCP servers named in the configuration.
    pub mcp: Vec<Entry>,
    /// The tools this build registers when nothing extra is configured. These
    /// are vocabulary, not configuration: they exist whether or not an operator
    /// turned anything on. Tools that only appear after a plugin or an MCP
    /// server loads are *not* listed here -- see the module note on why a
    /// snapshot cannot answer that, and read the session's `llm/call` events for
    /// what actually reached the model.
    pub tools: Vec<Entry>,
    /// Every event type this build writes, in the order `EventType::ALL` states.
    /// A reader that meets one it does not know may skip it; this list is how a
    /// reader finds out it is not looking at something newer than itself.
    pub events: Vec<String>,
    /// The HTTP surface, from [`crate::routes::ROUTES`].
    pub routes: Vec<Entry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_report_still_names_every_group() {
        let json = serde_json::to_string(&Report::default()).expect("encode");
        for key in [
            "mode",
            "workspace",
            "model",
            "plugins",
            "experts",
            "teams",
            "bundles",
            "skills",
            "mcp",
            "tools",
            "events",
            "routes",
        ] {
            assert!(
                json.contains(&format!("\"{key}\"")),
                "missing {key}: {json}"
            );
        }
    }

    #[test]
    fn an_inactive_entry_says_so_and_keeps_its_detail() {
        let entry = Entry::inactive("counter", "installed, not enabled");
        assert!(!entry.active);
        assert_eq!(entry.name, "counter");
        assert_eq!(entry.detail, "installed, not enabled");
    }
}
