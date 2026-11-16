//! A dependency-free MCP client: JSON-RPC 2.0 over stdio or Streamable HTTP.
//!
//! This crate is an external-protocol client. It deliberately imports no other
//! `forge-*` crate: MCP is a wire protocol, not an internal session concept.
//! `Dial` only connects; `Client::initialize` performs the handshake.

use std::error::Error as StdError;
use std::fmt;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

mod http;
mod jsonrpc;
mod process;
#[cfg(not(windows))]
mod process_unix;
#[cfg(windows)]
mod process_windows;
mod sse;
mod stdio;

/// The revision this client implements.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Sentinel text for a server that can no longer answer (sticky).
#[allow(non_upper_case_globals)]
pub const ERR_SERVER_GONE: &str = "mcp: server gone";
/// Sentinel text for a negotiated revision this client does not implement.
#[allow(non_upper_case_globals)]
pub const ERR_PROTOCOL_VERSION: &str = "mcp: unsupported protocol version";

/// Go-compatible sentinel name for [`ERR_SERVER_GONE`].
#[allow(non_upper_case_globals)]
pub const ErrServerGone: &str = ERR_SERVER_GONE;
/// Go-compatible sentinel name for [`ERR_PROTOCOL_VERSION`].
#[allow(non_upper_case_globals)]
pub const ErrProtocolVersion: &str = ERR_PROTOCOL_VERSION;

const DEFAULT_TIMEOUT_MS: u64 = 60_000;
const MAX_TOOL_PAGES: usize = 100;
const CLIENT_NAME: &str = "forge";
const CLIENT_VERSION: &str = "0.0.0-dev";

/// Sink for child stderr and protocol diagnostics. Defined here so this crate
/// has no dependency on any forge-internal logger type.
pub trait Logger: Send + Sync {
    fn debug(&self, message: &str);
    fn warn(&self, message: &str);
}

/// A logger that drops every message.
pub struct NoopLogger;

impl Logger for NoopLogger {
    fn debug(&self, _message: &str) {}
    fn warn(&self, _message: &str) {}
}

/// All errors returned by this crate. `Display` text always carries the server
/// name where one is known, so errors from several servers stay attributable.
#[derive(Debug)]
pub enum Error {
    Config(String),
    Io(io::Error),
    Protocol(String),
    HttpStatus { status: u16, detail: String },
    ServerGone(String),
    ProtocolVersion { expected: String, actual: String },
    Timeout(String),
}

impl Error {
    pub fn gone(message: impl Into<String>) -> Self {
        Error::ServerGone(message.into())
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Error::Protocol(message.into())
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Error::Timeout(message.into())
    }

    /// True once the server is unusable; later calls fail immediately.
    pub fn is_server_gone(&self) -> bool {
        matches!(self, Error::ServerGone(_))
    }

    /// True when the server negotiated an unsupported MCP revision.
    pub fn is_protocol_version(&self) -> bool {
        matches!(self, Error::ProtocolVersion { .. })
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Config(s) => write!(f, "mcp: config: {s}"),
            Error::Io(e) => write!(f, "mcp: io: {e}"),
            Error::Protocol(s) => write!(f, "mcp: protocol: {s}"),
            Error::HttpStatus { status, detail } => {
                write!(f, "mcp: http status {status}: {detail}")
            }
            Error::ServerGone(s) => write!(f, "{ERR_SERVER_GONE}: {s}"),
            Error::ProtocolVersion { expected, actual } => write!(
                f,
                "{ERR_PROTOCOL_VERSION}: server negotiated {actual:?}, want {expected:?}"
            ),
            Error::Timeout(s) => write!(f, "mcp: timeout: {s}"),
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        if matches!(
            value.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ) {
            Error::timeout(format!("io: {value}"))
        } else {
            Error::Io(value)
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Error::Protocol(format!("invalid JSON: {value}"))
    }
}

/// Which wire transport a [`Server`] uses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    #[default]
    Stdio,
    Http,
}

impl fmt::Display for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Transport::Stdio => write!(f, "stdio"),
            Transport::Http => write!(f, "http"),
        }
    }
}

/// One MCP server configuration. No logger field: pass the logger to [`Dial`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Server {
    pub name: String,
    pub transport: Transport,
    /// `[program, arg, ...]` for stdio.
    #[serde(default)]
    pub command: Vec<String>,
    /// Explicit environment. Empty means a minimal baseline; see `process`.
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub url: String,
    /// Additional `Name: value` HTTP request headers.
    #[serde(default)]
    pub headers: Vec<String>,
    /// Per-call timeout in milliseconds; 0 selects 60000.
    #[serde(default)]
    pub timeout_ms: u64,
}

impl Server {
    fn label(&self) -> String {
        if !self.name.is_empty() {
            self.name.clone()
        } else {
            self.transport.to_string()
        }
    }
}

/// A tool advertised by `tools/list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, rename = "inputSchema", alias = "input_schema")]
    pub input_schema: Value,
}

/// One content block of a tool result. `data`/`mime_type` are used by images;
/// `text` is used by text blocks. Unknown fields are ignored on the wire.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Content {
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(
        default,
        rename = "mimeType",
        alias = "mime_type",
        skip_serializing_if = "Option::is_none"
    )]
    pub mime_type: Option<String>,
}

/// A `tools/call` result. A tool-level failure is data (`is_error == true`), not
/// a Rust `Err`; protocol and transport failures are `Err`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CallResult {
    #[serde(default)]
    pub content: Vec<Content>,
    #[serde(default, rename = "isError", alias = "is_error")]
    pub is_error: bool,
}

/// The identity a server reports during `initialize`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
}

/// One initialized-or-initializing connection to an MCP server.
///
/// Methods take `&mut self` as the public contract requires; `clone_handle`
/// returns another owner of the same underlying connection, which is how a
/// caller may close (or call) while another call is in flight.
pub trait Client: Send {
    fn initialize(&mut self) -> Result<ServerInfo, Error>;
    fn tools(&mut self) -> Result<Vec<ToolDef>, Error>;
    fn call(&mut self, name: &str, args: Value) -> Result<CallResult, Error>;
    fn close(&mut self) -> Result<(), Error>;
    /// Another handle onto the same session and process.
    fn clone_handle(&self) -> Box<dyn Client>;
}

pub(crate) trait Conn: Send + Sync {
    fn round_trip(
        &self,
        deadline: std::time::Instant,
        method: &str,
        payload: &[u8],
        id: u64,
    ) -> Result<jsonrpc::WireMessage, Error>;
    fn notify(
        &self,
        deadline: std::time::Instant,
        method: &str,
        payload: &[u8],
    ) -> Result<(), Error>;
    fn set_protocol(&self, version: &str);
    fn close(&self) -> Result<(), Error>;
}

/// Connect to `srv`. For stdio this spawns the child; for HTTP it validates the
/// URL and builds the TLS configuration. No JSON-RPC is exchanged here.
pub fn dial(srv: &Server, logger: Arc<dyn Logger>) -> Result<Box<dyn Client>, Error> {
    let timeout_ms = if srv.timeout_ms == 0 {
        DEFAULT_TIMEOUT_MS
    } else {
        srv.timeout_ms
    };
    let conn: Arc<dyn Conn> = match srv.transport {
        Transport::Stdio => stdio::dial_stdio(srv, &logger)?,
        Transport::Http => http::dial_http(srv, &logger)?,
    };
    Ok(Box::new(ClientImpl {
        name: srv.label(),
        timeout: Duration::from_millis(timeout_ms),
        conn,
        next_id: Arc::new(AtomicU64::new(0)),
        state: Arc::new(Mutex::new(ClientState::default())),
        closed_result: Arc::new(Mutex::new(false)),
    }))
}

/// Non-snake-case spelling of [`dial`], matching the Go entry point.
#[allow(non_snake_case)]
pub fn Dial(srv: &Server, logger: Arc<dyn Logger>) -> Result<Box<dyn Client>, Error> {
    dial(srv, logger)
}

#[derive(Default)]
struct ClientState {
    closed: bool,
    initializing: bool,
    initialized: bool,
    info: Option<ServerInfo>,
}

struct ClientImpl {
    name: String,
    timeout: Duration,
    conn: Arc<dyn Conn>,
    next_id: Arc<AtomicU64>,
    state: Arc<Mutex<ClientState>>,
    closed_result: Arc<Mutex<bool>>,
}

impl ClientImpl {
    fn alive(&self) -> Result<(), Error> {
        if self
            .state
            .lock()
            .map_err(|_| Error::gone("state lock poisoned"))?
            .closed
        {
            return Err(Error::gone(format!(
                "server {:?}: client is closed",
                self.name
            )));
        }
        Ok(())
    }

    fn require_initialized(&self, op: &str) -> Result<(), Error> {
        self.alive()?;
        if !self
            .state
            .lock()
            .map_err(|_| Error::gone("state lock poisoned"))?
            .initialized
        {
            return Err(Error::protocol(format!(
                "server {:?}: {op}: client is not initialized (call initialize first)",
                self.name
            )));
        }
        Ok(())
    }

    fn exchange<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<T, Error> {
        self.alive()?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let payload = jsonrpc::encode_request(id, method, params)?;
        let deadline = std::time::Instant::now() + self.timeout;
        let message = self.conn.round_trip(deadline, method, &payload, id)?;
        if let Some(failure) = message.error {
            return Err(Error::protocol(format!(
                "server {:?}: {method}: JSON-RPC error {}: {}",
                self.name, failure.code, failure.message
            )));
        }
        let result = message.result.unwrap_or(Value::Null);
        serde_json::from_value(result).map_err(|e| {
            Error::protocol(format!(
                "server {:?}: decode {method} result: {e}",
                self.name
            ))
        })
    }
}

impl Client for ClientImpl {
    fn initialize(&mut self) -> Result<ServerInfo, Error> {
        self.alive()?;
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| Error::gone("state lock poisoned"))?;
            if state.initialized {
                return Err(Error::protocol(format!(
                    "server {:?}: already initialized",
                    self.name
                )));
            }
            if state.initializing {
                return Err(Error::protocol(format!(
                    "server {:?}: initialize already in progress",
                    self.name
                )));
            }
            state.initializing = true;
        }
        let finish = |result: Result<ServerInfo, Error>| {
            let mut state = self
                .state
                .lock()
                .map_err(|_| Error::gone("state lock poisoned"))?;
            state.initializing = false;
            if let Ok(info) = &result {
                state.initialized = true;
                state.info = Some(info.clone());
            }
            result
        };

        #[derive(Serialize)]
        struct InitializeParams {
            #[serde(rename = "protocolVersion")]
            protocol_version: String,
            capabilities: Value,
            #[serde(rename = "clientInfo")]
            client_info: ClientInfo,
        }
        #[derive(Serialize)]
        struct ClientInfo {
            name: String,
            version: String,
        }
        #[derive(Deserialize)]
        struct InitializeResult {
            #[serde(rename = "protocolVersion")]
            protocol_version: String,
            #[serde(rename = "serverInfo")]
            server_info: ServerInfo,
        }

        let params = InitializeParams {
            protocol_version: PROTOCOL_VERSION.to_string(),
            capabilities: serde_json::json!({}),
            client_info: ClientInfo {
                name: CLIENT_NAME.to_string(),
                version: CLIENT_VERSION.to_string(),
            },
        };
        let res: Result<InitializeResult, Error> = self.exchange(
            "initialize",
            Some(serde_json::to_value(params).map_err(Error::from)?),
        );
        let res = match res {
            Ok(res) => res,
            Err(e) => return finish(Err(e)),
        };
        if res.protocol_version != PROTOCOL_VERSION {
            return finish(Err(Error::ProtocolVersion {
                expected: PROTOCOL_VERSION.to_string(),
                actual: res.protocol_version,
            }));
        }
        self.conn.set_protocol(PROTOCOL_VERSION);
        let notification = match jsonrpc::encode_notification("notifications/initialized", None) {
            Ok(n) => n,
            Err(e) => return finish(Err(e)),
        };
        let deadline = std::time::Instant::now() + self.timeout;
        if let Err(e) = self
            .conn
            .notify(deadline, "notifications/initialized", &notification)
        {
            return finish(Err(e));
        }
        finish(Ok(res.server_info))
    }

    fn tools(&mut self) -> Result<Vec<ToolDef>, Error> {
        self.require_initialized("tools/list")?;
        #[derive(Deserialize)]
        struct ToolsListResult {
            #[serde(default)]
            tools: Vec<ToolDef>,
            #[serde(default, rename = "nextCursor")]
            next_cursor: String,
        }
        let mut all = Vec::new();
        let mut cursor = String::new();
        for _ in 0..MAX_TOOL_PAGES {
            let params = if cursor.is_empty() {
                None
            } else {
                Some(serde_json::json!({ "cursor": cursor }))
            };
            let page: ToolsListResult = self.exchange("tools/list", params)?;
            all.extend(page.tools);
            if page.next_cursor.is_empty() {
                return Ok(all);
            }
            cursor = page.next_cursor;
        }
        Err(Error::protocol(format!(
            "server {:?}: tools/list did not finish within {MAX_TOOL_PAGES} pages",
            self.name
        )))
    }

    fn call(&mut self, name: &str, args: Value) -> Result<CallResult, Error> {
        if name.trim().is_empty() {
            return Err(Error::protocol("tools/call: empty tool name"));
        }
        self.require_initialized("tools/call")?;
        let arguments = if args.is_null() {
            serde_json::json!({})
        } else {
            args
        };
        let params = serde_json::json!({ "name": name, "arguments": arguments });
        self.exchange("tools/call", Some(params))
    }

    fn close(&mut self) -> Result<(), Error> {
        let mut guard = self
            .closed_result
            .lock()
            .map_err(|_| Error::gone("close lock poisoned"))?;
        if *guard {
            return Ok(());
        }
        if let Ok(mut state) = self.state.lock() {
            state.closed = true;
            state.initializing = false;
        }
        let result = self.conn.close();
        *guard = true;
        result
    }

    fn clone_handle(&self) -> Box<dyn Client> {
        Box::new(ClientImpl {
            name: self.name.clone(),
            timeout: self.timeout,
            conn: self.conn.clone(),
            next_id: self.next_id.clone(),
            state: self.state.clone(),
            closed_result: self.closed_result.clone(),
        })
    }
}
