//! config file, home directory, defaults.
//!
//! Layer L0. The shape here is the on-disk contract: a Go build wrote these
//! files, and a newer config must still load in an older binary. That is why
//! unknown members are ignored rather than rejected — refusing a file with one
//! extra key would break forward compatibility for no safety gain.
//!
//! MCP entries are the one deliberate exception to "validate everything": a
//! typo in one server drops that server, not the harness. [`Config::warnings`]
//! reports what was dropped and [`Config::usable_mcp_servers`] returns exactly
//! the rest, so the two lists can never disagree.

#![forbid(unsafe_code)]
#![forbid(unsafe_op_in_unsafe_fn)]

use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use forge_core::{Error, ErrorKind, Result};
use serde::{Deserialize, Serialize};

/// Sandbox modes, ordered from most to least restrictive.
pub const SANDBOX_READ_ONLY: &str = "read-only";
/// The default: writes are confined to the workspace.
pub const SANDBOX_WORKSPACE_WRITE: &str = "workspace-write";
/// No containment at all; the operator asked for it out loud.
pub const SANDBOX_FULL_ACCESS: &str = "full-access";

/// MCP transport names, spelled as the clients dial them.
pub const MCP_TRANSPORT_STDIO: &str = "stdio";
/// HTTP MCP transport.
pub const MCP_TRANSPORT_HTTP: &str = "http";

/// Unusable `mcp.servers` entries, reported by [`Config::validate`] as a
/// distinct type.
///
/// The distinction is load-bearing: `forge doctor` must fail on a typo in an
/// MCP server so the operator sees it, while the runtime starts anyway and
/// drops exactly those servers. A caller that cannot tell this error apart
/// would turn one typo into "forge does not start".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MCPConfigError {
    /// One operator-facing line per bad entry, in declaration order.
    pub problems: Vec<String>,
}

impl fmt::Display for MCPConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "config: {}", self.problems.join("; "))
    }
}

impl std::error::Error for MCPConfigError {}

/// One entry of `"mcp": {"servers": [...]}`.
///
/// It is a neutral record: which fields matter depends on the transport, and
/// the MCP layer owns that decision. Keeping it neutral here avoids a config
/// dependency on a layer above L0.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MCPServerConfig {
    /// Middle segment of `mcp__<server>__<tool>`, so it must be unique.
    pub name: String,
    /// `stdio` or `http`; empty means "infer from what is set".
    #[serde(skip_serializing_if = "String::is_empty")]
    pub transport: String,
    /// argv of a stdio server; element 0 is the program.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// Extra `K=V` pairs for a stdio server.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    /// Endpoint of an http server.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub url: String,
    /// Extra request headers for an http server.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub headers: std::collections::BTreeMap<String, String>,
    /// One-call timeout; zero means the MCP package default.
    #[serde(skip_serializing_if = "is_zero_i32")]
    pub timeout_ms: i32,
}

fn is_zero_i32(v: &i32) -> bool {
    *v == 0
}

/// Serde helper: an absent number and a zero mean the same thing here, so a
/// zero is not written back out.
fn is_zero_i64(v: &i64) -> bool {
    *v == 0
}

/// The `"mcp"` object.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MCPConfig {
    /// Servers to expose as tools, in declaration order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<MCPServerConfig>,
}

/// The `"skills"` object: directories the skill scanner walks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SkillsConfig {
    /// Roots, most trusted first. A leading `~` is expanded by
    /// [`Config::skill_roots`], never stored expanded, so the file stays
    /// inspectable.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub roots: Vec<String>,
}

/// One named subagent backend: a program that speaks the child protocol on
/// stdin/stdout (one request line in, one response line out).
///
/// The point of naming them is that "which backend ran my child" must never be a
/// guess. A default of none means the built-in in-process path runs the child;
/// registering a provider is the operator saying "this one leaves the process".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SubagentProviderConfig {
    /// The name the model and the CLI use to refer to this backend.
    pub name: String,
    /// The program to spawn.
    pub program: String,
    /// Arguments passed before the request is written.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// How long to wait for the answer, in seconds. Zero means the package
    /// default.
    #[serde(skip_serializing_if = "is_zero_i64")]
    pub timeout_secs: i64,
}

impl SubagentProviderConfig {
    /// Whether this entry is usable: both halves have to be there.
    pub fn is_complete(&self) -> bool {
        !self.name.trim().is_empty() && !self.program.trim().is_empty()
    }
}

/// The `"subagent"` object.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SubagentConfig {
    /// Nesting depth. Zero means the subagent package's own default, so the
    /// number is not restated here.
    #[serde(skip_serializing_if = "is_zero_i32")]
    pub max_depth: i32,
    /// How many children may run at once. Zero means the subagent package's own
    /// default (one, the historical serial behaviour); a higher number lets the
    /// model fan out. Excess children queue rather than fail.
    #[serde(skip_serializing_if = "is_zero_i32")]
    pub max_active: i32,
    /// Named out-of-process backends. Empty means every child runs in this
    /// process, which is the historical behaviour.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<SubagentProviderConfig>,
    /// Which provider to use when the caller does not name one. Empty means the
    /// in-process path, so an unset value cannot silently route children
    /// somewhere else.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub backend: String,
}

/// The `"sandboxAccess"` object: what the sandboxed child may touch outside its
/// workspace, and whether it may reach the network.
///
/// Absent members mean "use the built-in behaviour", which for the path lists is
/// what makes a contained `git` or `npm` work at all and for the network is "no
/// outbound sockets"; an empty list is the explicit way to say "grant nothing
/// extra". The distinction matters: a caller who found the defaults too wide must
/// be able to narrow them without editing code, a casing or key typo must not
/// silently turn into "everything allowed", and an explicit `false` must stay
/// false even if a future default ever says otherwise — which is why `network` is
/// an `Option<bool>` here and an `Option<bool>` in the raw form.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SandboxAccessConfig {
    /// Read-only grants, for the platforms that deny reads by default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read: Option<Vec<String>>,
    /// Read-write grants.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub write: Option<Vec<String>>,
    /// Whether the sandboxed child may use the network. `Some(false)` is a
    /// statement the operator made out loud; `None` is "nobody said", which is
    /// the same answer today but not the same answer as a written default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<bool>,
}

/// The `"plugins"` object: which plugins in `<dataDir>/plugins` a session may
/// start.
///
/// Off by default. A plugin is a process this harness spawns, so its existence
/// is a decision the operator makes in the file rather than something discovered
/// by walking a directory: a stray `plugin.json` in a data directory must not be
/// able to put a new tool in front of the model.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PluginsConfig {
    /// Plugin names to load. Empty means none, which is the default.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub enabled: Vec<String>,
    /// Overrides `<dataDir>/plugins`; `~` is expanded by [`Config::plugin_root`].
    #[serde(skip_serializing_if = "String::is_empty")]
    pub dir: String,
}

/// The `"experts"` object: directories the expert scanner walks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ExpertsConfig {
    /// Roots, most trusted first. A leading `~` is expanded by
    /// [`Config::expert_roots`], never stored expanded.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub roots: Vec<String>,
}

/// The `"team"` object: parallel workers over a shared board.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TeamConfig {
    /// Workers allowed to run at once. Zero means the `team` tool is not
    /// registered at all, because a team that cannot overlap is a subagent with
    /// extra steps.
    #[serde(skip_serializing_if = "is_zero_i32")]
    pub max_parallel: i32,
}

/// User-level configuration stored in `$FORGE_HOME/config.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    /// Named preset of model/tool settings.
    pub profile: String,
    /// Default model id; empty means the profile decides.
    pub model: String,
    /// Optional PEM file holding extra CA roots (for example a private CA) for
    /// the LLM HTTPS client. Absent means the pinned public bundle only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ca_file: Option<String>,
    /// Where sessions, indexes and logs live.
    pub data_dir: String,
    /// One of the `SANDBOX_*` constants.
    pub sandbox: String,
    /// Verbose logging.
    pub debug: bool,
    /// MCP servers.
    pub mcp: MCPConfig,
    /// Skill roots.
    pub skills: SkillsConfig,
    /// Subagent depth.
    pub subagent: SubagentConfig,
    /// Extra paths the sandboxed child may read or write.
    pub sandbox_access: SandboxAccessConfig,
    /// Web search endpoint. Absent means `web_search` is not registered at all.
    pub search: SearchConfig,
    /// Plugins this session may start.
    pub plugins: PluginsConfig,
    /// Expert roster roots.
    pub experts: ExpertsConfig,
    /// Team concurrency.
    pub team: TeamConfig,
}

/// The search endpoint `web_search` posts to.
///
/// The harness deliberately has no favourite search vendor: an empty `endpoint`
/// means the tool does not exist for this session, which is more honest than a
/// built-in default that would silently mail the operator's queries somewhere.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SearchConfig {
    /// Full URL of the endpoint; the request is a JSON `POST`.
    pub endpoint: String,
    /// Bearer token sent with the request. Empty means no `Authorization`.
    pub key: String,
}

impl Default for Config {
    fn default() -> Self {
        Config::defaults()
    }
}

impl Config {
    /// The populated default configuration used when no file exists.
    ///
    /// The home directory is allowed to fail: `DataDir` then stays empty and
    /// [`Config::validate`] reports it, which is more useful than panicking in
    /// a constructor whose signature has no place to return the error.
    pub fn defaults() -> Self {
        let home = home_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let skills_root = Path::new(&home)
            .join("skills")
            .to_string_lossy()
            .into_owned();
        Config {
            profile: "default".to_string(),
            model: String::new(),
            ca_file: None,
            data_dir: home,
            sandbox: SANDBOX_WORKSPACE_WRITE.to_string(),
            debug: false,
            mcp: MCPConfig::default(),
            skills: SkillsConfig {
                roots: vec![skills_root],
            },
            subagent: SubagentConfig::default(),
            sandbox_access: SandboxAccessConfig::default(),
            search: SearchConfig::default(),
            plugins: PluginsConfig::default(),
            experts: ExpertsConfig::default(),
            team: TeamConfig::default(),
        }
    }

    /// Read `path`, falling back to defaults when the file is absent.
    ///
    /// A missing file is not an error: first-run must work without asking the
    /// operator to create one. Every other read or parse failure is an error,
    /// because silently ignoring a corrupt file hides the operator's mistake.
    pub fn load(path: &Path) -> Result<Config> {
        let mut config = Config::defaults();
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(config),
            Err(err) => {
                return Err(Error::new(
                    ErrorKind::Io,
                    format!("config: read {}: {err}", path.display()),
                )
                .with_source(err))
            }
        };
        // Go's `json.Unmarshal("null", &cfg)` leaves the populated struct
        // untouched. A file containing `null` is still a legal (if pointless)
        // config document, so keep the defaults instead of failing the load.
        if text.trim() == "null" {
            return Ok(config);
        }
        let raw: RawConfig = serde_json::from_str(&text).map_err(|err| {
            Error::new(
                ErrorKind::Json,
                format!("config: parse {}: {err}", path.display()),
            )
            .with_source(err)
        })?;
        config.apply_raw(raw);
        if config.data_dir.is_empty() {
            config.data_dir = Config::defaults().data_dir;
        }
        Ok(config)
    }

    /// Overlay only the members the file actually contained.
    ///
    /// Go's `json.Unmarshal` into a pre-populated struct keeps a default when
    /// the member is absent (`{"skills":{}}` keeps the default roots). Serde's
    /// `#[serde(default)]` cannot express "keep the caller's value", so the
    /// raw form makes presence explicit and this function applies it field by
    /// field.
    fn apply_raw(&mut self, raw: RawConfig) {
        if let Some(v) = raw.profile {
            self.profile = v;
        }
        if let Some(v) = raw.model {
            self.model = v;
        }
        if let Some(v) = raw.ca_file {
            self.ca_file = Some(v);
        }
        if let Some(v) = raw.data_dir {
            self.data_dir = v;
        }
        if let Some(v) = raw.sandbox {
            self.sandbox = v;
        }
        if let Some(v) = raw.debug {
            self.debug = v;
        }
        if let Some(mcp) = raw.mcp {
            if let Some(servers) = mcp.servers {
                self.mcp.servers = servers;
            }
        }
        if let Some(skills) = raw.skills {
            if let Some(roots) = skills.roots {
                self.skills.roots = roots;
            }
        }
        if let Some(subagent) = raw.subagent {
            if let Some(max_depth) = subagent.max_depth {
                self.subagent.max_depth = max_depth;
            }
            if let Some(max_active) = subagent.max_active {
                self.subagent.max_active = max_active;
            }
            if let Some(providers) = subagent.providers {
                self.subagent.providers = providers;
            }
            if let Some(backend) = subagent.backend {
                self.subagent.backend = backend;
            }
        }
        if let Some(access) = raw.sandbox_access {
            if access.read.is_some() {
                self.sandbox_access.read = access.read;
            }
            if access.write.is_some() {
                self.sandbox_access.write = access.write;
            }
            if access.network.is_some() {
                self.sandbox_access.network = access.network;
            }
        }
        if let Some(search) = raw.search {
            self.search = search;
        }
        if let Some(plugins) = raw.plugins {
            if let Some(enabled) = plugins.enabled {
                self.plugins.enabled = enabled;
            }
            if let Some(dir) = plugins.dir {
                self.plugins.dir = dir;
            }
        }
        if let Some(experts) = raw.experts {
            if let Some(roots) = experts.roots {
                self.experts.roots = roots;
            }
        }
        if let Some(team) = raw.team {
            if let Some(max_parallel) = team.max_parallel {
                self.team.max_parallel = max_parallel;
            }
        }
    }

    /// Check the values other packages assume are sound.
    ///
    /// MCP problems are returned as [`MCPConfigError`] rather than a generic
    /// [`Error`]: `Loader` never rejects a broken server entry, and a runtime
    /// caller can match the type, warn, and keep going with
    /// [`Config::usable_mcp_servers`]. A bad sandbox mode or negative depth is
    /// still an ordinary validation failure and must stop the start.
    pub fn validate(&self) -> Result<()> {
        match self.sandbox.as_str() {
            SANDBOX_READ_ONLY | SANDBOX_WORKSPACE_WRITE | SANDBOX_FULL_ACCESS => {}
            "" => {
                return Err(Error::new(
                    ErrorKind::Invalid,
                    "config: sandbox mode is empty (want read-only|workspace-write|full-access)",
                ))
            }
            other => {
                return Err(Error::new(
                    ErrorKind::Invalid,
                    format!(
                        "config: invalid sandbox mode {other:?} (want read-only|workspace-write|full-access)"
                    ),
                ))
            }
        }
        if self.data_dir.is_empty() {
            return Err(Error::new(ErrorKind::Invalid, "config: dataDir is empty"));
        }
        if self.subagent.max_depth < 0 {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!(
                    "config: subagent.maxDepth {} is negative (0 means the subagent package default)",
                    self.subagent.max_depth
                ),
            ));
        }
        if self.subagent.max_active < 0 {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!(
                    "config: subagent.maxActive {} is negative (0 means the subagent package default)",
                    self.subagent.max_active
                ),
            ));
        }
        // A provider entry that names nothing is a typo that would otherwise
        // read as "no backends configured" and silently run in-process.
        for provider in &self.subagent.providers {
            if provider.name.trim().is_empty() || provider.program.trim().is_empty() {
                return Err(Error::new(
                    ErrorKind::Invalid,
                    "config: subagent.providers entries need both name and program",
                ));
            }
            if provider.timeout_secs < 0 {
                return Err(Error::new(
                    ErrorKind::Invalid,
                    format!(
                        "config: subagent.providers[{}].timeoutSecs {} is negative",
                        provider.name, provider.timeout_secs
                    ),
                ));
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for provider in &self.subagent.providers {
            if !seen.insert(provider.name.clone()) {
                return Err(Error::new(
                    ErrorKind::Invalid,
                    format!(
                        "config: subagent.providers names {} twice; which one ran a child would depend on order",
                        provider.name
                    ),
                ));
            }
        }
        // A default backend that is not registered would route every child to a
        // name nothing answers to. Refusing here means the operator finds out at
        // load time rather than the first delegation.
        if !self.subagent.backend.trim().is_empty() && !seen.contains(&self.subagent.backend) {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!(
                    "config: subagent.backend names {:?}, which is not in subagent.providers",
                    self.subagent.backend
                ),
            ));
        }
        if self.team.max_parallel < 0 {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!(
                    "config: team.maxParallel {} is negative (0 means the team tool is not registered)",
                    self.team.max_parallel
                ),
            ));
        }
        let problems = self.mcp_warnings();
        if !problems.is_empty() {
            let message = format!("config: {}", problems.join("; "));
            return Err(
                Error::new(ErrorKind::Invalid, message).with_source(MCPConfigError { problems })
            );
        }
        Ok(())
    }

    /// The MCP entries that cannot be dialled, in declaration order.
    ///
    /// This is the "warning" half of validation: names are unique, a transport
    /// is either stated or inferable, and each effective transport has the
    /// field it needs.
    pub fn mcp_warnings(&self) -> Vec<String> {
        self.mcp_problems()
            .into_iter()
            .map(|(_, reason)| reason)
            .collect()
    }

    /// Alias kept short for callers that treat every config warning the same.
    pub fn warnings(&self) -> Vec<String> {
        self.mcp_warnings()
    }

    /// Entries that can be dialled, in declaration order. Exactly the entries
    /// [`Config::mcp_warnings`] does not name; a repeated name keeps its first
    /// definition.
    pub fn usable_mcp_servers(&self) -> Vec<MCPServerConfig> {
        let bad: Vec<usize> = self
            .mcp_problems()
            .into_iter()
            .map(|(index, _)| index)
            .collect();
        self.mcp
            .servers
            .iter()
            .enumerate()
            .filter(|(index, _)| !bad.contains(index))
            .map(|(_, server)| server.clone())
            .collect()
    }

    /// Configured skill roots with a leading `~` expanded against the user
    /// home. `~user` is left alone: guessing another account's home is worse
    /// than reporting a missing root.
    pub fn skill_roots(&self) -> Vec<String> {
        self.skills
            .roots
            .iter()
            .map(|root| expand_tilde(root))
            .collect()
    }

    /// Where plugins live: `plugins.dir` when set, else `<dataDir>/plugins`.
    ///
    /// A relative `dir` is resolved against nothing — it is returned as written,
    /// because turning it into an absolute path here would make the value depend
    /// on the process's working directory, and a plugin root that moves when you
    /// `cd` is a plugin root that loads different code in different terminals.
    pub fn plugin_root(&self) -> String {
        if !self.plugins.dir.trim().is_empty() {
            return expand_tilde(self.plugins.dir.trim());
        }
        Path::new(&self.data_dir)
            .join("plugins")
            .to_string_lossy()
            .into_owned()
    }

    /// Configured expert roots with a leading `~` expanded, same rule as
    /// [`Config::skill_roots`].
    pub fn expert_roots(&self) -> Vec<String> {
        self.experts
            .roots
            .iter()
            .map(|root| expand_tilde(root))
            .collect()
    }

    /// Whether a sandboxed child may use the network.
    ///
    /// The absent-and-explicit-false distinction lives in the file so a future
    /// default can tell "the operator wrote false" from "the operator wrote
    /// nothing"; every reader wants the effective answer, which is this method.
    /// The default is `false`, matching the sandbox's own `allow_net` default:
    /// a containment switch nobody asked for must not be the permissive one.
    pub fn sandbox_allow_net(&self) -> bool {
        self.sandbox_access.network.unwrap_or(false)
    }

    /// Create the data directory and prove it is writable with a probe file.
    ///
    /// The probe is removed in every success path; a leftover `.write-probe`
    /// is evidence the process died between write and remove.
    pub fn ensure_data_dir(&self) -> Result<()> {
        let dir = Path::new(&self.data_dir);
        fs::create_dir_all(dir).map_err(|err| {
            Error::new(
                ErrorKind::Io,
                format!("config: create data dir {}: {err}", dir.display()),
            )
            .with_source(err)
        })?;
        let probe = dir.join(".write-probe");
        fs::write(&probe, b"ok").map_err(|err| {
            Error::new(
                ErrorKind::Io,
                format!("config: data dir {} is not writable: {err}", dir.display()),
            )
            .with_source(err)
        })?;
        fs::remove_file(&probe).map_err(|err| {
            Error::new(
                ErrorKind::Io,
                format!("config: remove probe {}: {err}", probe.display()),
            )
            .with_source(err)
        })?;
        Ok(())
    }

    fn mcp_problems(&self) -> Vec<(usize, String)> {
        let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        let mut problems = Vec::new();
        for (index, server) in self.mcp.servers.iter().enumerate() {
            let mut at = format!("mcp.servers[{index}]");
            let name = server.name.trim();
            if name.is_empty() {
                problems.push((
                    index,
                    format!(
                        "{at}: name is empty (it is the middle segment of mcp__<server>__<tool>)"
                    ),
                ));
                continue;
            }
            if let Some(first) = seen.get(name) {
                problems.push((
                    index,
                    format!(
                        "{at} ({name:?}): duplicate server name (first declared at mcp.servers[{first}])"
                    ),
                ));
                continue;
            }
            seen.insert(name, index);
            at = format!("{at} ({:?})", server.name);

            if server.command.is_empty() && server.url.trim().is_empty() {
                problems.push((
                    index,
                    format!("{at}: needs \"command\" (stdio) or \"url\" (http)"),
                ));
                continue;
            }
            if !server.transport.is_empty()
                && server.transport != MCP_TRANSPORT_STDIO
                && server.transport != MCP_TRANSPORT_HTTP
            {
                problems.push((
                    index,
                    format!(
                        "{at}: transport {:?} is not {:?} or {:?}",
                        server.transport, MCP_TRANSPORT_STDIO, MCP_TRANSPORT_HTTP
                    ),
                ));
                continue;
            }
            match server.effective_transport() {
                MCP_TRANSPORT_STDIO => {
                    if server.command.is_empty() {
                        problems.push((
                            index,
                            format!("{at}: transport \"stdio\" needs a non-empty \"command\""),
                        ));
                        continue;
                    }
                }
                MCP_TRANSPORT_HTTP => {
                    if server.url.trim().is_empty() {
                        problems.push((
                            index,
                            format!("{at}: transport \"http\" needs a non-empty \"url\""),
                        ));
                        continue;
                    }
                }
                _ => {
                    problems.push((
                        index,
                        format!("{at}: both \"command\" and \"url\" are set; add \"transport\" to choose"),
                    ));
                    continue;
                }
            }
            if server.timeout_ms < 0 {
                problems.push((
                    index,
                    format!("{at}: timeoutMs {} is negative", server.timeout_ms),
                ));
            }
        }
        problems
    }
}

impl MCPServerConfig {
    /// Declared transport wins; otherwise infer from the fields that are set.
    /// Empty means neither form was declared, so the entry is unusable.
    pub fn effective_transport(&self) -> &str {
        if !self.transport.is_empty() {
            return &self.transport;
        }
        if !self.command.is_empty() && self.url.is_empty() {
            MCP_TRANSPORT_STDIO
        } else if !self.url.is_empty() && self.command.is_empty() {
            MCP_TRANSPORT_HTTP
        } else {
            ""
        }
    }
}

/// Package-level spelling of [`Config::load`], matching the Go `config.Load`.
pub fn load(path: &Path) -> Result<Config> {
    Config::load(path)
}

/// `$FORGE_HOME` when set, else `~/.forge`.
pub fn home_dir() -> Result<PathBuf> {
    if let Some(value) = nonempty_var("FORGE_HOME") {
        return Ok(PathBuf::from(value));
    }
    match user_home_dir() {
        Some(home) => Ok(home.join(".forge")),
        None => Err(Error::new(
            ErrorKind::NotFound,
            "config: locate home directory: no home directory in the environment",
        )),
    }
}

/// The operating system's user home, independent of `FORGE_HOME`.
///
/// `~` in a skill root means the user's home as the shell means it; using
/// `home_dir()` there would expand it to `$FORGE_HOME`, which is a different
/// directory when an operator points Forge at a custom home.
fn user_home_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        nonempty_var("USERPROFILE").map(PathBuf::from).or_else(|| {
            let drive = nonempty_var("HOMEDRIVE")?;
            let path = nonempty_var("HOMEPATH")?;
            Some(PathBuf::from(drive).join(path))
        })
    } else {
        nonempty_var("HOME").map(PathBuf::from)
    }
}

/// The config file path inside [`home_dir`].
pub fn default_path() -> Result<PathBuf> {
    Ok(home_dir()?.join("config.json"))
}

fn nonempty_var(name: &str) -> Option<std::ffi::OsString> {
    env::var_os(name).filter(|value| !value.is_empty())
}

fn expand_tilde(path: &str) -> String {
    if path != "~" && !path.starts_with("~/") && !path.starts_with("~\\") {
        return path.to_string();
    }
    let Some(user_home) = user_home_dir() else {
        return path.to_string();
    };
    if path == "~" {
        return user_home.to_string_lossy().into_owned();
    }
    user_home.join(&path[2..]).to_string_lossy().into_owned()
}

/// The subset of the file that was actually present. `Option` is what keeps
/// "absent" distinct from "present and empty".
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawConfig {
    profile: Option<String>,
    model: Option<String>,
    ca_file: Option<String>,
    data_dir: Option<String>,
    sandbox: Option<String>,
    debug: Option<bool>,
    mcp: Option<RawMCPConfig>,
    skills: Option<RawSkillsConfig>,
    subagent: Option<RawSubagentConfig>,
    sandbox_access: Option<RawSandboxAccessConfig>,
    search: Option<SearchConfig>,
    plugins: Option<RawPluginsConfig>,
    experts: Option<RawExpertsConfig>,
    team: Option<RawTeamConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawMCPConfig {
    servers: Option<Vec<MCPServerConfig>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawSkillsConfig {
    roots: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawSubagentConfig {
    max_depth: Option<i32>,
    max_active: Option<i32>,
    /// Absent means "no out-of-process backends", which is the in-process path.
    providers: Option<Vec<SubagentProviderConfig>>,
    /// Absent means "use the in-process path". The distinction between absent
    /// and empty matters: an empty name is a typo, not a request to fall back.
    backend: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawSandboxAccessConfig {
    read: Option<Vec<String>>,
    write: Option<Vec<String>>,
    /// Absent is `None`, an explicit `false` is `Some(false)`. The whole point of
    /// this type is that the two are distinguishable, so this must not be a bare
    /// `bool` with `#[serde(default)]`.
    network: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawPluginsConfig {
    enabled: Option<Vec<String>>,
    dir: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawExpertsConfig {
    roots: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawTeamConfig {
    max_parallel: Option<i32>,
}
