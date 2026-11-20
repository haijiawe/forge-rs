//! Reading a parent's children back out of its log.
//!
//! A read, never a start: `forge subagent list` must not spawn a child, resume a
//! child, or open a child log — it answers from the parent's own rows. That is
//! the whole reason the catalog is on the parent log rather than only in memory.

use std::path::Path;

use forge_core::event::{parse_log, Event, EventType};
use forge_core::{Error, ErrorKind, Result as CoreResult};
use forge_session::Store;

use crate::catalog::{self, CatalogEntry};
use crate::child::{child_log_path, is_child_id};
use crate::descriptor;
use crate::names::{agent_path_segments, is_agent_name, join_agent_path, ROOT_AGENT_NAME};

/// Every event on `path`, or an empty list if the file does not exist yet (a
/// session that never ran a turn has no catalog and that is not an error).
pub fn read_events(path: &Path) -> CoreResult<Vec<Event>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let store = Store::open(path, forge_session::Options::default())?;
    let mut events = Vec::new();
    store.read_from(0, |event| {
        events.push(event.clone());
        Ok(())
    })?;
    Ok(events)
}

/// The direct children recorded on a parent log at `path`.
pub fn children_in_log(path: &Path) -> CoreResult<Vec<CatalogEntry>> {
    Ok(catalog::fold(&read_events(path)?))
}

/// The direct children recorded in an open store.
pub fn children(store: &Store) -> CoreResult<Vec<CatalogEntry>> {
    let mut events = Vec::new();
    store.read_from(0, |event| {
        events.push(event.clone());
        Ok(())
    })?;
    Ok(catalog::fold(&events))
}

/// A one-line-per-child rendering for `forge subagent list`.
pub fn render(entries: &[CatalogEntry]) -> String {
    if entries.is_empty() {
        return "no subagents in this session\n".to_string();
    }
    let mut text = String::from("name\tchild\tsession\tmode\tlabel\n");
    for entry in entries {
        text.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\n",
            entry.name.as_deref().unwrap_or("-"),
            entry.child_id,
            entry.child_id,
            entry.mode.as_str(),
            entry.label.as_deref().unwrap_or("-"),
        ));
    }
    text
}

/// Why one child of the tree could not be read as a session.
///
/// A tree is a *report*, not a transaction: one unreadable child says something
/// about that child, never about its siblings, so a diagnostic is attached to
/// the node and the walk continues.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Diagnostic {
    /// The log exists but is not a log this build can read: bad JSON, an
    /// envelope that does not parse, a non-dense sequence, a duplicate id.
    Corrupt(String),
    /// The log parses, but holds no `subagent/descriptor` at a version this
    /// build reads — so the child cannot even be classified.
    Unsupported(String),
    /// There is no log file for this child at all.
    Unavailable(String),
}

impl Diagnostic {
    /// The short tag `render_tree` prints, without the detail.
    pub fn as_str(&self) -> &'static str {
        match self {
            Diagnostic::Corrupt(_) => "corrupt",
            Diagnostic::Unsupported(_) => "unsupported",
            Diagnostic::Unavailable(_) => "unavailable",
        }
    }

    /// The sentence behind the tag.
    pub fn detail(&self) -> &str {
        match self {
            Diagnostic::Corrupt(detail)
            | Diagnostic::Unsupported(detail)
            | Diagnostic::Unavailable(detail) => detail,
        }
    }
}

/// One node of a subagent tree: the parent's catalog row, plus the walk's own
/// verdict on the child's log.
///
/// `path` is relative to the root and always starts with [`ROOT_AGENT_NAME`]
/// (`lead/agent-1/agent-2`) — the same shape [`resolve_name`] accepts. `depth`
/// counts from the root's direct children, which are depth 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub entry: CatalogEntry,
    pub path: String,
    pub depth: usize,
    pub children: Vec<Node>,
    pub diagnostic: Option<Diagnostic>,
}

/// Every descendant of `root_session_id`, depth first, in sibling order.
///
/// The subtree is rebuilt from catalog rows alone — one file read per node, no
/// child is started and no child is resumed. A child whose log is missing,
/// corrupt, or at an unknown descriptor version becomes a node with a
/// [`Diagnostic`] and no children; its siblings are still walked.
pub fn descendants(data_dir: &Path, root_session_id: &str) -> CoreResult<Vec<Node>> {
    let root_path = child_log_path(data_dir, root_session_id);
    let events = read_log(&root_path)?.unwrap_or_default();
    Ok(walk(data_dir, catalog::fold(&events), ROOT_AGENT_NAME, 1))
}

/// Resolve a name — `lead`, `agent-1`, `lead/agent-1/agent-2`, `agent-1/agent-2`
/// — to the session id it names, by walking catalog rows from the root.
///
/// A bare name that is not a child of the root is treated as a session id when a
/// log for it exists in this data dir (DSH also accepts an id where a name would
/// go); when no such log exists there is nothing to resolve, and the error says
/// which level was searched and what names it offered.
pub fn resolve_name(data_dir: &Path, root_session_id: &str, name: &str) -> CoreResult<String> {
    let trimmed = name.trim();
    let segments = agent_path_segments(trimmed);
    if segments.is_empty() {
        return Err(Error::new(
            ErrorKind::Invalid,
            "subagent: an empty name resolves to nothing",
        ));
    }
    if let Some(bad) = segments.iter().find(|segment| !is_agent_name(segment)) {
        return Err(Error::new(
            ErrorKind::Invalid,
            format!(
                "subagent: {bad:?} is not a valid agent name in {trimmed:?} (1-32 lowercase \
                 letters, digits and hyphens, starting with a letter)"
            ),
        ));
    }

    // `lead` is the root itself; any other path is relative to the root.
    let mut walk = segments.as_slice();
    if walk[0] == ROOT_AGENT_NAME {
        walk = &walk[1..];
        if walk.is_empty() {
            return Ok(root_session_id.to_string());
        }
    }

    let root_path = child_log_path(data_dir, root_session_id);
    let root_entries = catalog::fold(&read_log(&root_path)?.unwrap_or_default());

    if walk.len() == 1 {
        let segment = walk[0];
        if let Some(entry) = named(&root_entries, segment) {
            return Ok(entry.child_id.clone());
        }
        let direct = child_log_path(data_dir, segment);
        if is_child_id(segment) && direct.exists() {
            return Ok(segment.to_string());
        }
        return Err(not_found(segment, ROOT_AGENT_NAME, &root_entries));
    }

    let mut current = root_session_id.to_string();
    let mut walked = ROOT_AGENT_NAME.to_string();
    let mut entries = root_entries;
    for segment in walk {
        let Some(entry) = named(&entries, segment) else {
            return Err(not_found(segment, &walked, &entries));
        };
        current.clone_from(&entry.child_id);
        walked = join_agent_path(&walked, segment);
        let path = child_log_path(data_dir, &current);
        entries = catalog::fold(&read_log(&path)?.unwrap_or_default());
    }
    Ok(current)
}

/// A one-line-per-node rendering of a tree, indented two spaces per depth.
///
/// The line is `<indent><name or child id>  <mode>  <session id>`, with
/// `  [<tag>: <detail>]` appended when the node carries a diagnostic. The tree
/// is already sorted, so this is stable output for a given log set.
pub fn render_tree(nodes: &[Node]) -> String {
    if nodes.is_empty() {
        return "no subagents in this session\n".to_string();
    }
    let mut text = String::new();
    for node in nodes {
        render_node(node, &mut text);
    }
    text
}

fn render_node(node: &Node, text: &mut String) {
    for _ in 0..node.depth {
        text.push_str("  ");
    }
    text.push_str(label(&node.entry));
    text.push_str("  ");
    text.push_str(node.entry.mode.as_str());
    text.push_str("  ");
    text.push_str(&node.entry.child_id);
    if let Some(diagnostic) = &node.diagnostic {
        text.push_str("  [");
        text.push_str(diagnostic.as_str());
        text.push_str(": ");
        text.push_str(diagnostic.detail());
        text.push(']');
    }
    text.push('\n');
    for child in &node.children {
        render_node(child, text);
    }
}

/// Read a log without touching the file, or `None` when there is no file.
///
/// Deliberately not [`Store::open`]: opening repairs a torn tail by truncating
/// it, and a read command may not write to a session log. `forge_core`'s
/// `parse_log` is the same parser with the two checks one line cannot make
/// (dense `seq`, unique ids), so a damaged child log is reported, not repaired.
fn read_log(path: &Path) -> CoreResult<Option<Vec<Event>>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let text = std::str::from_utf8(&bytes).map_err(|error| {
        Error::new(
            ErrorKind::Json,
            format!("{}: the log is not UTF-8: {error}", path.display()),
        )
    })?;
    // A final chunk without its newline is a write that never committed: the
    // writer died mid-record. `forge_session`'s own recovery drops it too
    // (`scan_log`: "A record without a terminating newline is a torn write"),
    // but recovery truncates the file, and a read command may not. So the
    // unfinished line is dropped here, in memory, and only it.
    let committed = match text.rfind('\n') {
        Some(end) => &text[..=end],
        None => "",
    };
    Ok(Some(parse_log(committed)?))
}

/// The children of one node, sorted by the name they are known by.
fn walk(
    data_dir: &Path,
    mut entries: Vec<CatalogEntry>,
    parent_path: &str,
    depth: usize,
) -> Vec<Node> {
    entries.sort_by(|a, b| label(a).cmp(label(b)));
    let mut nodes = Vec::with_capacity(entries.len());
    for entry in entries {
        let path = join_agent_path(parent_path, label(&entry));
        let log_path = child_log_path(data_dir, &entry.child_id);
        let (diagnostic, children) = match read_log(&log_path) {
            Ok(None) => (
                Some(Diagnostic::Unavailable(format!(
                    "{}: no session log for this child",
                    log_path.display()
                ))),
                Vec::new(),
            ),
            Err(error) => (
                Some(Diagnostic::Corrupt(error.message().to_string())),
                Vec::new(),
            ),
            Ok(Some(events)) => match descriptor::fold(&events) {
                None => (
                    Some(Diagnostic::Unsupported(unsupported_detail(
                        &events, &log_path,
                    ))),
                    Vec::new(),
                ),
                Some(_) => (
                    None,
                    walk(data_dir, catalog::fold(&events), &path, depth + 1),
                ),
            },
        };
        nodes.push(Node {
            entry,
            path,
            depth,
            children,
            diagnostic,
        });
    }
    nodes
}

/// The name a row is known by inside its parent, or its session id when the row
/// was written without one.
fn label(entry: &CatalogEntry) -> &str {
    entry.name.as_deref().unwrap_or(entry.child_id.as_str())
}

fn named<'a>(entries: &'a [CatalogEntry], name: &str) -> Option<&'a CatalogEntry> {
    entries
        .iter()
        .find(|entry| entry.name.as_deref() == Some(name))
}

/// The names a level offered, for the "have: ..." half of a miss.
fn candidates(entries: &[CatalogEntry]) -> String {
    let mut names: Vec<&str> = entries
        .iter()
        .filter_map(|entry| entry.name.as_deref())
        .collect();
    names.sort_unstable();
    if names.is_empty() {
        "none".to_string()
    } else {
        names.join(", ")
    }
}

fn not_found(segment: &str, walked: &str, entries: &[CatalogEntry]) -> Error {
    Error::new(
        ErrorKind::NotFound,
        format!(
            "subagent: no child named {segment:?} under {walked} (have: {})",
            candidates(entries)
        ),
    )
}

/// Why a parsed log yielded no descriptor: name the version it did write, if the
/// payload still says.
fn unsupported_detail(events: &[Event], log_path: &Path) -> String {
    let version = events
        .iter()
        .find(|event| event.typ == EventType::SUBAGENT_DESCRIPTOR.as_str())
        .and_then(|event| event.payload.as_ref())
        .and_then(|payload| payload.get("version"))
        .and_then(serde_json::Value::as_u64);
    match version {
        Some(version) => format!(
            "{}: subagent/descriptor version {version} is not supported (this build reads {})",
            log_path.display(),
            descriptor::SUBAGENT_DESCRIPTOR_VERSION
        ),
        None => format!(
            "{}: no subagent/descriptor at version {}",
            log_path.display(),
            descriptor::SUBAGENT_DESCRIPTOR_VERSION
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::Mode;

    fn scratch(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "forge-subagent-list-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        dir.join("parent.jsonl")
    }

    #[test]
    fn a_missing_log_reads_as_no_children() {
        let path = scratch("missing").with_file_name("never.jsonl");
        assert!(children_in_log(&path).expect("read").is_empty());
    }

    #[test]
    fn rows_written_to_a_log_read_back() {
        let path = scratch("rows");
        let store = Store::open(&path, forge_session::Options::default()).expect("open");
        store
            .append(
                &CatalogEntry::new("s_a", Mode::Continuable)
                    .with_name("agent-1")
                    .event("s_p"),
            )
            .expect("append a");
        store
            .append(
                &CatalogEntry::new("s_b", Mode::OneShot)
                    .with_name("agent-2")
                    .event("s_p"),
            )
            .expect("append b");
        let rows = children_in_log(&path).expect("read");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name.as_deref(), Some("agent-1"));
        assert_eq!(rows[1].name.as_deref(), Some("agent-2"));
        let text = render(&rows);
        assert!(text.contains("agent-1"), "{text}");
        assert!(text.contains("continuable"), "{text}");
    }

    #[test]
    fn an_empty_session_renders_a_plain_sentence() {
        assert_eq!(
            render(&[]),
            "no subagents in this session\n",
            "an empty list is an answer, not a blank table"
        );
    }
}
