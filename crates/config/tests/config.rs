//! Behaviour tests for the config contract. They use only `std` temp files so
//! the crate stays dependency-free; each test owns a unique directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use forge_config::{
    Config, MCP_TRANSPORT_HTTP, MCP_TRANSPORT_STDIO, SANDBOX_FULL_ACCESS, SANDBOX_WORKSPACE_WRITE,
};
use forge_core::ErrorKind;

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "forge-config-test-{}-{tag}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir { path }
    }

    fn file(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.path.join(name);
        fs::write(&path, contents).expect("write fixture");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn load(path: &Path) -> Config {
    Config::load(path).expect("config loads")
}

#[test]
fn missing_file_is_defaults_not_an_error() {
    let dir = TempDir::new("missing");
    let config = load(&dir.path.join("does-not-exist.json"));
    assert_eq!(config.profile, "default");
    assert_eq!(config.sandbox, SANDBOX_WORKSPACE_WRITE);
    assert!(!config.data_dir.is_empty(), "default dataDir is populated");
    assert!(config
        .skills
        .roots
        .first()
        .is_some_and(|root| root.ends_with("skills")));
}

#[test]
fn a_null_document_keeps_the_defaults_like_go() {
    // Go's json.Unmarshal("null", &cfg) succeeds without touching cfg; a
    // "null" config is odd but must not be the one file that fails to load.
    let dir = TempDir::new("null");
    let path = dir.file("config.json", "null");
    let config = load(&path);
    assert_eq!(config.profile, "default");
    assert_eq!(config.sandbox, SANDBOX_WORKSPACE_WRITE);
}

#[test]
fn unknown_members_are_ignored_and_absent_nested_fields_keep_defaults() {
    let dir = TempDir::new("forward-compat");
    let path = dir.file(
        "config.json",
        r#"{
            "model": "gpt-test",
            "futureBlock": {"anything": [1, 2, 3]},
            "skills": {}
        }"#,
    );
    let config = load(&path);
    assert_eq!(config.model, "gpt-test");
    assert_eq!(config.sandbox, SANDBOX_WORKSPACE_WRITE);
    assert!(
        !config.skills.roots.is_empty(),
        "an empty skills object must keep the default roots, not erase them"
    );
}

#[test]
fn invalid_sandbox_is_a_typed_error_with_the_go_message() {
    let dir = TempDir::new("bad-sandbox");
    let path = dir.file("config.json", r#"{"sandbox": "yolo"}"#);
    let config = load(&path);
    let err = config.validate().expect_err("yolo must be refused");
    assert_eq!(err.kind(), ErrorKind::Invalid);
    assert_eq!(
        err.message(),
        "config: invalid sandbox mode \"yolo\" (want read-only|workspace-write|full-access)"
    );
}

#[test]
fn every_sandbox_mode_constant_validates() {
    let dir = TempDir::new("sandbox-modes");
    for mode in [
        forge_config::SANDBOX_READ_ONLY,
        SANDBOX_WORKSPACE_WRITE,
        SANDBOX_FULL_ACCESS,
    ] {
        let path = dir.file(
            &format!("{mode}.json"),
            &format!(r#"{{"sandbox":"{mode}"}}"#),
        );
        load(&path).validate().expect("documented mode is valid");
    }
}

#[test]
fn broken_mcp_entry_is_skipped_with_a_warning_and_the_rest_still_loads() {
    let dir = TempDir::new("mcp-broken");
    let path = dir.file(
        "config.json",
        r#"{
            "mcp": {
                "servers": [
                    {"name": "good", "command": ["npx", "server"]},
                    {"name": "typo", "env": ["A=B"]}
                ]
            }
        }"#,
    );
    let config = load(&path);
    let err = config
        .validate()
        .expect_err("a bad MCP entry is reported by Validate, not by Load");
    assert_eq!(err.kind(), ErrorKind::Invalid);
    let mcp_err = std::error::Error::source(&err)
        .and_then(|source| source.downcast_ref::<forge_config::MCPConfigError>())
        .expect("the error carries the typed MCPConfigError");
    assert_eq!(mcp_err.problems.len(), 1);
    assert!(mcp_err.problems[0].contains("mcp.servers[1] (\"typo\")"));
    let usable = config.usable_mcp_servers();
    assert_eq!(usable.len(), 1);
    assert_eq!(usable[0].name, "good");
    let warnings = config.warnings();
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0].contains("mcp.servers[1] (\"typo\")"),
        "warning names the entry: {}",
        warnings[0]
    );
    assert!(
        warnings[0].contains("needs \"command\" (stdio) or \"url\" (http)"),
        "warning states the rule: {}",
        warnings[0]
    );
}

#[test]
fn duplicate_mcp_names_keep_the_first_declaration() {
    let dir = TempDir::new("mcp-duplicate");
    let path = dir.file(
        "config.json",
        r#"{
            "mcp": {"servers": [
                {"name": "same", "command": ["one"]},
                {"name": "same", "command": ["two"]}
            ]}
        }"#,
    );
    let config = load(&path);
    let usable = config.usable_mcp_servers();
    assert_eq!(usable.len(), 1);
    assert_eq!(usable[0].command, vec!["one".to_string()]);
    assert!(config.mcp_warnings()[0].contains("duplicate server name"));
}

#[test]
fn transport_is_inferred_from_command_or_url() {
    let mut server = forge_config::MCPServerConfig {
        name: "local".to_string(),
        command: vec!["server".to_string()],
        ..Default::default()
    };
    assert_eq!(server.effective_transport(), MCP_TRANSPORT_STDIO);
    server.command.clear();
    server.url = "https://example.invalid/mcp".to_string();
    assert_eq!(server.effective_transport(), MCP_TRANSPORT_HTTP);
    server.transport = MCP_TRANSPORT_STDIO.to_string();
    assert_eq!(
        server.effective_transport(),
        MCP_TRANSPORT_STDIO,
        "an explicit transport wins"
    );
}

#[test]
fn empty_data_dir_falls_back_to_the_default() {
    let dir = TempDir::new("empty-data");
    let path = dir.file("config.json", r#"{"dataDir": ""}"#);
    let config = load(&path);
    assert!(!config.data_dir.is_empty());
}

#[test]
fn ensure_data_dir_creates_it_and_leaves_no_probe_behind() {
    let dir = TempDir::new("ensure-data");
    let data = dir.path.join("nested").join("data");
    let config = Config {
        data_dir: data.to_string_lossy().into_owned(),
        ..Config::defaults()
    };
    config.ensure_data_dir().expect("writable temp dir");
    assert!(data.is_dir());
    assert!(
        !data.join(".write-probe").exists(),
        "the write probe must be removed"
    );
}

#[test]
fn serialization_uses_the_go_json_names() {
    let config = Config::defaults();
    let value = serde_json::to_value(&config).expect("config serializes");
    let object = value.as_object().expect("object");
    for key in [
        "profile", "model", "dataDir", "sandbox", "debug", "mcp", "skills", "subagent",
    ] {
        assert!(object.contains_key(key), "missing key {key}");
    }
    assert!(!object.contains_key("data_dir"));
}

#[test]
fn ca_file_parses_from_config_json() {
    let dir = TempDir::new("ca-file");
    let path = dir.file("config.json", r#"{"caFile": "/etc/forge/private-ca.pem"}"#);
    let config = load(&path);
    assert_eq!(config.ca_file.as_deref(), Some("/etc/forge/private-ca.pem"));

    // The key round-trips under its Go/JSON name.
    let value = serde_json::to_value(&config).expect("config serializes");
    assert_eq!(value["caFile"], "/etc/forge/private-ca.pem");
}

#[test]
fn ca_file_is_omitted_from_json_when_absent() {
    let config = Config::defaults();
    assert!(config.ca_file.is_none());
    let value = serde_json::to_value(&config).expect("config serializes");
    let object = value.as_object().expect("object");
    assert!(
        !object.contains_key("caFile"),
        "an unset caFile must not serialize"
    );
}

// ---------------------------------------------------------------------------
// sandboxAccess.network
// ---------------------------------------------------------------------------

#[test]
fn sandbox_network_is_absent_by_default_and_reads_as_false() {
    // Absent must mean "no outbound sockets": the sandbox's own default is
    // `allow_net = false`, and a config file nobody edited must not widen it.
    let dir = TempDir::new("net-absent");
    let config = load(&dir.file("config.json", r#"{"profile": "default"}"#));
    assert!(
        config.sandbox_access.network.is_none(),
        "absent stays absent"
    );
    assert!(!config.sandbox_allow_net(), "absent reads as false");

    // And an untouched defaults() object is the same answer.
    assert!(!Config::defaults().sandbox_allow_net());
}

#[test]
fn explicit_false_is_kept_distinct_from_absent() {
    // The whole reason `network` is an Option in both the struct and the raw
    // form: an operator who wrote `false` must still be reported as having
    // written it, so the day a permissive default appears it does not win.
    let dir = TempDir::new("net-explicit-false");
    let path = dir.file(
        "config.json",
        r#"{"sandboxAccess": {"network": false, "read": ["/tmp"]}}"#,
    );
    let config = load(&path);
    assert_eq!(config.sandbox_access.network, Some(false));
    assert!(!config.sandbox_allow_net());
    // The sibling members are unaffected: one member's presence must not reset
    // the others.
    assert_eq!(config.sandbox_access.read, Some(vec!["/tmp".to_string()]));
    assert!(
        config.sandbox_access.write.is_none(),
        "write was not mentioned"
    );
}

#[test]
fn explicit_true_turns_the_network_on_and_round_trips() {
    let dir = TempDir::new("net-explicit-true");
    let path = dir.file("config.json", r#"{"sandboxAccess": {"network": true}}"#);
    let config = load(&path);
    assert_eq!(config.sandbox_access.network, Some(true));
    assert!(config.sandbox_allow_net());
    let value = serde_json::to_value(&config).expect("config serializes");
    assert_eq!(
        value["sandboxAccess"]["network"], true,
        "network must round-trip under its camelCase name"
    );
}

#[test]
fn an_absent_network_is_omitted_from_json() {
    let config = Config::defaults();
    let value = serde_json::to_value(&config).expect("config serializes");
    let object = value
        .get("sandboxAccess")
        .and_then(|access| access.as_object())
        .expect("sandboxAccess object");
    assert!(
        !object.contains_key("network"),
        "an unstated network must not serialize as a value: {value}"
    );
}

#[test]
fn an_explicitly_empty_sandbox_access_object_keeps_the_other_members_alone() {
    // Forward compatibility: `{"sandboxAccess": {}}` is what a fresh file has,
    // and it must not erase the path defaults or silently enable the network.
    let dir = TempDir::new("net-empty-object");
    let config = load(&dir.file("config.json", r#"{"sandboxAccess": {}}"#));
    assert!(config.sandbox_access.read.is_none());
    assert!(config.sandbox_access.write.is_none());
    assert!(config.sandbox_access.network.is_none());
    assert!(!config.sandbox_allow_net());
}
