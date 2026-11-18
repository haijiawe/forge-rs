//! OpenAI-compatible streaming provider over HTTP/1.1.
//!
//! No async runtime is used: the provider performs a blocking request and
//! decodes the response as a [`text/event-stream`](crate::sse::SseReader) body.
//! TLS is provided by the workspace rustls stack; plain HTTP uses TCP only.

use std::collections::VecDeque;
use std::io::{self, BufReader, ErrorKind as IoErrorKind, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use forge_core::{Error, ErrorKind, Result};
use rustls_pki_types::{pem::PemObject, CertificateDer, ServerName};
use serde::Serialize;
use serde_json::{json, Value};

use crate::sse::{
    api_error_code, clip, decode_api_error, decode_event, payloads, pe, provider_error_with,
    SseReader, DONE_TOKEN, ERR_CLOSED, ERR_MISSING_DONE,
};
use crate::{ContentPart, Delta, Message, Provider, ProviderError, Request, Stream};

#[allow(unused_imports)]
pub use crate::sse::{
    provider_error, CODE_BAD_EVENT, CODE_CANCELED, CODE_CLOSED, CODE_INVALID_REQUEST, CODE_STREAM,
    CODE_TIMEOUT, CODE_TRANSPORT, MAX_ERROR_BODY, PROVIDER_NAME,
};

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const ENV_BASE_URL: &str = "FORGE_BASE_URL";
const ENV_API_KEY: &str = "FORGE_API_KEY";
const READ_TIMEOUT: Duration = Duration::from_millis(200);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// Which wire transport a base URL selects. Kept separate from the socket code
/// so the choice can be tested without opening a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Transport {
    Plain,
    Tls,
}

pub(crate) fn transport_for_scheme(scheme: &str) -> Transport {
    if scheme.eq_ignore_ascii_case("https") {
        Transport::Tls
    } else {
        Transport::Plain
    }
}

/// Build the root store used to verify TLS peers.
///
/// The store always starts from the pinned `webpki-roots` bundle, so public
/// endpoints keep verifying exactly as before. When `ca_file` is set, the
/// certificates in that PEM file are **added** to the bundle: a private CA
/// extends the public roots, it never replaces them.
pub(crate) fn build_root_store(ca_file: Option<&Path>) -> Result<rustls::RootCertStore> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let Some(path) = ca_file else {
        return Ok(roots);
    };
    let bytes = std::fs::read(path).map_err(|e| {
        ca_error(
            ErrorKind::Io,
            format!("llm openai: cannot read CA file {}: {e}", path.display()),
        )
    })?;
    let certs = parse_ca_pem(&bytes, path)?;
    let (valid, invalid) = roots.add_parsable_certificates(certs);
    if valid == 0 {
        return Err(ca_error(
            ErrorKind::Invalid,
            format!(
                "llm openai: no usable certificates in CA file {} ({invalid} rejected)",
                path.display()
            ),
        ));
    }
    Ok(roots)
}

/// Decode every `CERTIFICATE` section of a PEM blob.
///
/// The error names the file but never echoes its body: a CA bundle can sit
/// next to a private key, and a message must not leak it.
fn parse_ca_pem(bytes: &[u8], path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let mut certs = Vec::new();
    for item in CertificateDer::pem_slice_iter(bytes) {
        match item {
            Ok(cert) => certs.push(cert),
            Err(e) => {
                return Err(ca_error(
                    ErrorKind::Invalid,
                    format!("llm openai: invalid CA PEM in {}: {e}", path.display()),
                ))
            }
        }
    }
    if certs.is_empty() {
        return Err(ca_error(
            ErrorKind::Invalid,
            format!(
                "llm openai: no certificates found in CA file {}",
                path.display()
            ),
        ));
    }
    Ok(certs)
}

/// A typed provider failure for CA configuration, with no file contents in it.
fn ca_error(kind: ErrorKind, message: String) -> Error {
    provider_error_with(
        kind,
        pe(
            CODE_TRANSPORT,
            0,
            false,
            Duration::ZERO,
            String::new(),
            message,
        ),
    )
}

/// Provider construction options. Non-empty fields win over environment
/// variables; keys are trimmed before use.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub base_url: String,
    pub api_key: String,
    /// Optional PEM file holding extra root certificates, such as a private CA.
    /// The certificates are added to the pinned `webpki-roots` bundle, never
    /// substituted for it.
    pub ca_file: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct ParsedUrl {
    scheme: String,
    authority: String,
    path: String,
    default_port: u16,
}

/// OpenAI-compatible provider.
pub struct Client {
    base_url: String,
    api_key: String,
    endpoint: ParsedUrl,
    tls_roots: rustls::RootCertStore,
}

impl Client {
    /// Build a client. Precedence is `options > env > default`.
    pub fn new(options: Options) -> Result<Self> {
        let base = {
            let candidate = options.base_url.trim();
            if !candidate.is_empty() {
                candidate.to_string()
            } else {
                let from_env = std::env::var(ENV_BASE_URL).unwrap_or_default();
                let from_env = from_env.trim();
                if from_env.is_empty() {
                    DEFAULT_BASE_URL.to_string()
                } else {
                    from_env.to_string()
                }
            }
        };
        let api_key = {
            let candidate = options.api_key.trim();
            if !candidate.is_empty() {
                candidate.to_string()
            } else {
                std::env::var(ENV_API_KEY)
                    .unwrap_or_default()
                    .trim()
                    .to_string()
            }
        };
        let endpoint = parse_base_url(&base)?;
        let tls_roots = build_root_store(options.ca_file.as_deref())?;
        Ok(Self {
            base_url: base.trim_end_matches('/').to_string(),
            api_key,
            endpoint,
            tls_roots,
        })
    }

    /// The configured base URL (without a trailing slash).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn request_path(&self) -> String {
        let prefix = self.endpoint.path.trim_end_matches('/');
        format!("{prefix}/chat/completions")
    }

    fn send(&self, payload: &[u8], closed: Arc<AtomicBool>) -> Result<Connected> {
        let transport = transport_for_scheme(&self.endpoint.scheme);
        let host = host_without_port(&self.endpoint.authority);
        let port =
            port_from_authority(&self.endpoint.authority).unwrap_or(self.endpoint.default_port);
        let address = format!("{host}:{port}");
        let mut addresses = match address.to_socket_addrs() {
            Ok(iter) => iter,
            Err(e) => return Err(transport_error(&format!("{e}"))),
        };
        let socket_addr = match addresses.next() {
            Some(addr) => addr,
            None => return Err(transport_error("no address for host")),
        };

        let stream = match TcpStream::connect(socket_addr) {
            Ok(stream) => stream,
            Err(e) => return Err(transport_error(&format!("{e}"))),
        };
        let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
        let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
        let shutdown = stream.try_clone().ok();
        let mut stream = match transport {
            Transport::Plain => ConnectionStream::Plain(stream),
            Transport::Tls => ConnectionStream::Tls(Box::new(TlsStream::new(
                stream,
                &host,
                closed.clone(),
                self.tls_roots.clone(),
            )?)),
        };

        let mut request = Vec::with_capacity(payload.len() + 512);
        let header = format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nAccept: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n",
            self.request_path(),
            self.endpoint.authority,
            payload.len()
        );
        request.extend_from_slice(header.as_bytes());
        if !self.api_key.is_empty() {
            let auth = format!("Authorization: Bearer {}\r\n", self.api_key);
            request.extend_from_slice(auth.as_bytes());
        }
        request.extend_from_slice(b"\r\n");
        request.extend_from_slice(payload);

        if let Err(e) = stream.write_all(&request) {
            return Err(transport_error(&format!("{e}")));
        }

        let mut reader = BufReader::new(stream);
        let status_line = match read_header_line(&mut reader) {
            Ok(Some(line)) => line,
            Ok(None) => return Err(transport_error("no response from server")),
            Err(e) => return Err(transport_error(&format!("{e}"))),
        };
        let status = parse_status(&status_line).unwrap_or(0);
        let mut headers = Vec::new();
        loop {
            let line = match read_header_line(&mut reader) {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(e) => return Err(transport_error(&format!("{e}"))),
            };
            if line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
            }
        }

        let mode = if headers.iter().any(|(name, value)| {
            name == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked")
        }) {
            BodyMode::Chunked {
                remaining: 0,
                need_crlf: false,
            }
        } else if let Some(length) = headers.iter().find_map(|(name, value)| {
            if name == "content-length" {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        }) {
            BodyMode::Length(length)
        } else {
            BodyMode::UntilEof
        };

        let retry_after = headers
            .iter()
            .find(|(name, _)| name == "retry-after")
            .map(|(_, value)| parse_retry_after(value))
            .unwrap_or(Duration::ZERO);

        Ok(Connected {
            status,
            retry_after,
            body: HttpBody::new(reader, mode, closed),
            shutdown,
        })
    }
}

/// A plain or TLS-wrapped socket. Both variants expose the same blocking
/// `Read`/`Write` surface so the HTTP parser above does not branch on scheme.
enum ConnectionStream {
    Plain(TcpStream),
    Tls(Box<TlsStream>),
}

struct TlsStream {
    tcp: TcpStream,
    tls: rustls::ClientConnection,
    closed: Arc<AtomicBool>,
}

impl TlsStream {
    fn new(
        tcp: TcpStream,
        host: &str,
        closed: Arc<AtomicBool>,
        roots: rustls::RootCertStore,
    ) -> Result<Self> {
        let name = ServerName::try_from(host.to_string())
            .map_err(|_| transport_error("invalid TLS server name"))?;
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let tls = rustls::ClientConnection::new(Arc::new(config), name)
            .map_err(|e| transport_error(&format!("TLS setup: {e}")))?;
        let mut stream = Self { tcp, tls, closed };
        while stream.tls.is_handshaking() {
            stream.pump()?;
        }
        Ok(stream)
    }

    /// Flush pending TLS records, read what is available, and feed it to rustls.
    /// A socket timeout is not a failure: `read()` retries until data arrives or
    /// the caller's `Canceller` is closed.
    fn pump(&mut self) -> io::Result<()> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(io::Error::new(IoErrorKind::ConnectionAborted, ERR_CLOSED));
        }
        self.tls.write_tls(&mut self.tcp)?;
        match self.tls.read_tls(&mut self.tcp) {
            Ok(0) => {
                return Err(io::Error::new(
                    IoErrorKind::UnexpectedEof,
                    "TLS peer closed",
                ))
            }
            Ok(_) => {}
            Err(e) if matches!(e.kind(), IoErrorKind::WouldBlock | IoErrorKind::TimedOut) => {
                return Ok(())
            }
            Err(e) => return Err(e),
        }
        self.tls
            .process_new_packets()
            .map_err(|e| io::Error::new(IoErrorKind::InvalidData, e))?;
        Ok(())
    }
}

impl Read for TlsStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.closed.load(Ordering::Relaxed) {
                return Err(io::Error::new(IoErrorKind::ConnectionAborted, ERR_CLOSED));
            }
            match self.tls.reader().read(buf) {
                Ok(n) if n > 0 => return Ok(n),
                Ok(_) => {}
                Err(e) if e.kind() == IoErrorKind::WouldBlock => {}
                Err(e) => return Err(e),
            }
            self.pump()?;
        }
    }
}

impl Write for TlsStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.tls.writer().write(buf)?;
        self.tls.write_tls(&mut self.tcp)?;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.tls.write_tls(&mut self.tcp)?;
        self.tcp.flush()
    }
}

impl Read for ConnectionStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.read(buf),
            Self::Tls(stream) => stream.read(buf),
        }
    }
}

impl Write for ConnectionStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.write(buf),
            Self::Tls(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(stream) => stream.flush(),
            Self::Tls(stream) => stream.flush(),
        }
    }
}

struct Connected {
    status: u16,
    retry_after: Duration,
    body: HttpBody<BufReader<ConnectionStream>>,
    shutdown: Option<TcpStream>,
}

impl Client {
    /// Open a stream as its concrete type, so callers can obtain a [`Canceller`].
    pub fn stream_open(&self, request: &Request) -> Result<OpenAiStream> {
        if let Err(validation) = request.validate() {
            let error = pe(
                CODE_INVALID_REQUEST,
                0,
                false,
                Duration::ZERO,
                String::new(),
                validation.to_string(),
            );
            return Err(provider_error_with(ErrorKind::Invalid, error));
        }

        let payload = match encode_request(request) {
            Ok(payload) => payload,
            Err(e) => {
                let error = pe(
                    CODE_INVALID_REQUEST,
                    0,
                    false,
                    Duration::ZERO,
                    String::new(),
                    e.to_string(),
                );
                return Err(provider_error_with(ErrorKind::Invalid, error));
            }
        };

        let closed = Arc::new(AtomicBool::new(false));
        let mut connected = self.send(&payload, closed.clone())?;
        if !(200..=299).contains(&connected.status) {
            let mut body = vec![0u8; MAX_ERROR_BODY + 1];
            let read = connected.body.read(&mut body).unwrap_or(0);
            body.truncate(read);
            return Err(status_error(connected.status, &body, connected.retry_after));
        }

        let canceller = Canceller {
            closed,
            shutdown: Arc::new(Mutex::new(connected.shutdown)),
        };
        Ok(OpenAiStream {
            reader: SseReader::new(connected.body),
            canceller: canceller.clone(),
            queue: VecDeque::new(),
            terminal: None,
            done: false,
        })
    }
}

impl Provider for Client {
    fn stream(&self, request: &Request) -> Result<Box<dyn Stream>> {
        self.stream_open(request)
            .map(|s| Box::new(s) as Box<dyn Stream>)
    }
}

/// A shareable handle used to close a stream from another thread.
#[derive(Clone)]
pub struct Canceller {
    closed: Arc<AtomicBool>,
    shutdown: Arc<Mutex<Option<TcpStream>>>,
}

impl Canceller {
    /// Idempotently close the underlying connection and unblock any reader.
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        if let Ok(mut guard) = self.shutdown.lock() {
            if let Some(stream) = guard.take() {
                let _ = stream.shutdown(Shutdown::Both);
            }
        }
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
enum Terminal {
    CleanEnd,
    Failed(ErrorKind, ProviderError),
}

/// A blocking OpenAI-compatible response stream.
pub struct OpenAiStream {
    reader: SseReader<HttpBody<BufReader<ConnectionStream>>>,
    canceller: Canceller,
    queue: VecDeque<Delta>,
    terminal: Option<Terminal>,
    done: bool,
}

impl OpenAiStream {
    /// Obtain a handle that can close this stream from another thread.
    ///
    /// The contract exposes `Stream::close(&mut self)`; this method exists
    /// because a blocked `recv(&mut self)` cannot be closed through `&mut self`
    /// from a second thread.
    pub fn canceller(&self) -> Canceller {
        self.canceller.clone()
    }

    fn fail(&mut self, err: Error) -> Error {
        let kind = err.kind();
        let provider_error = match provider_error(&err) {
            Some(existing) => existing.clone(),
            None if self.canceller.is_closed() => pe(
                CODE_CLOSED,
                0,
                false,
                Duration::ZERO,
                String::new(),
                ERR_CLOSED.to_string(),
            ),
            None => pe(
                CODE_STREAM,
                0,
                false,
                Duration::ZERO,
                String::new(),
                err.to_string(),
            ),
        };
        self.terminal = Some(Terminal::Failed(kind, provider_error.clone()));
        provider_error_with(kind, provider_error)
    }
}

impl Stream for OpenAiStream {
    fn recv(&mut self) -> Result<Option<Delta>> {
        loop {
            if let Some(delta) = self.queue.pop_front() {
                return Ok(Some(delta));
            }
            match self.terminal.as_ref() {
                Some(Terminal::CleanEnd) => return Ok(None),
                Some(Terminal::Failed(kind, error)) => {
                    return Err(provider_error_with(*kind, error.clone()));
                }
                None => {}
            }
            if self.canceller.is_closed() {
                return Err(self.fail(Error::new(ErrorKind::Io, ERR_CLOSED)));
            }

            let data = match self.reader.next() {
                Ok(Some(data)) => data,
                Ok(None) => {
                    if !self.done {
                        return Err(self.fail(Error::new(ErrorKind::Io, ERR_MISSING_DONE)));
                    }
                    self.terminal = Some(Terminal::CleanEnd);
                    return Ok(None);
                }
                Err(e) => return Err(self.fail(Error::from(e))),
            };

            let mut finished = false;
            for payload in payloads(&data) {
                let payload = payload.trim();
                if payload.is_empty() {
                    continue;
                }
                if payload == DONE_TOKEN {
                    finished = true;
                    self.done = true;
                    break;
                }
                match decode_event(payload) {
                    Ok(deltas) => self.queue.extend(deltas),
                    Err(e) => return Err(self.fail(e)),
                }
            }
            if !self.queue.is_empty() {
                if finished {
                    self.terminal = Some(Terminal::CleanEnd);
                }
                continue;
            }
            if finished {
                self.terminal = Some(Terminal::CleanEnd);
                return Ok(None);
            }
        }
    }

    fn close(&mut self) {
        self.canceller.close();
    }
}

/// How the response body is delimited.
#[derive(Clone, Copy)]
pub(crate) enum BodyMode {
    Chunked { remaining: usize, need_crlf: bool },
    Length(usize),
    UntilEof,
}

/// A `Read` adapter that honours the response framing and cancellation flag.
///
/// This is transport machinery, not part of the provider boundary: keeping it
/// crate-private lets `lib.rs` re-export `Client` without also publishing a
/// constructor that takes the private `BodyMode`.
pub(crate) struct HttpBody<R: Read> {
    inner: R,
    mode: BodyMode,
    closed: Arc<AtomicBool>,
    finished: bool,
}

impl<R: Read> HttpBody<R> {
    pub(crate) fn new(inner: R, mode: BodyMode, closed: Arc<AtomicBool>) -> Self {
        Self {
            inner,
            mode,
            closed,
            finished: false,
        }
    }

    fn read_some(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.inner.read(buf) {
                Ok(n) => return Ok(n),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    if self.closed.load(Ordering::Acquire) {
                        return Err(io::Error::new(io::ErrorKind::ConnectionAborted, ERR_CLOSED));
                    }
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn read_line_raw(&mut self) -> io::Result<String> {
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let read = self.read_some(&mut byte)?;
            if read == 0 {
                break;
            }
            if byte[0] == b'\n' {
                break;
            }
            line.push(byte[0]);
        }
        let end = usize::from(line.last() == Some(&b'\r'));
        line.truncate(line.len().saturating_sub(end));
        Ok(String::from_utf8_lossy(&line).into_owned())
    }

    fn read_exact_two(&mut self) -> io::Result<[u8; 2]> {
        let mut bytes = [0u8; 2];
        let mut filled = 0;
        while filled < 2 {
            let n = self.read_some(&mut bytes[filled..])?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "chunked body truncated",
                ));
            }
            filled += n;
        }
        Ok(bytes)
    }
}

impl<R: Read> Read for HttpBody<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.finished {
            return Ok(0);
        }
        match self.mode {
            BodyMode::Length(mut remaining) => {
                if remaining == 0 {
                    self.finished = true;
                    return Ok(0);
                }
                let take = remaining.min(buf.len());
                let got = self.read_some(&mut buf[..take])?;
                remaining -= got;
                self.mode = BodyMode::Length(remaining);
                if got == 0 || remaining == 0 {
                    self.finished = true;
                }
                Ok(got)
            }
            BodyMode::UntilEof => {
                let got = self.read_some(buf)?;
                if got == 0 {
                    self.finished = true;
                }
                Ok(got)
            }
            BodyMode::Chunked {
                mut remaining,
                mut need_crlf,
            } => {
                if need_crlf {
                    self.read_exact_two()?;
                    need_crlf = false;
                }
                if remaining == 0 {
                    let line = self.read_line_raw()?;
                    let size_text = line.split(';').next().unwrap_or("").trim();
                    let size = usize::from_str_radix(size_text, 16).map_err(|_| {
                        io::Error::new(io::ErrorKind::InvalidData, "bad chunk size")
                    })?;
                    if size == 0 {
                        loop {
                            let trailer = self.read_line_raw()?;
                            if trailer.is_empty() {
                                break;
                            }
                        }
                        self.mode = BodyMode::Chunked {
                            remaining: 0,
                            need_crlf,
                        };
                        self.finished = true;
                        return Ok(0);
                    }
                    remaining = size;
                }
                let take = remaining.min(buf.len());
                let got = self.read_some(&mut buf[..take])?;
                if got == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "chunked body truncated",
                    ));
                }
                remaining -= got;
                if remaining == 0 {
                    need_crlf = true;
                }
                self.mode = BodyMode::Chunked {
                    remaining,
                    need_crlf,
                };
                Ok(got)
            }
        }
    }
}

fn parse_base_url(base: &str) -> Result<ParsedUrl> {
    let Some((scheme, rest)) = base.split_once("://") else {
        return Err(Error::new(
            ErrorKind::Invalid,
            format!("llm openai: invalid base URL {base:?}"),
        ));
    };
    if scheme != "http" && scheme != "https" {
        return Err(Error::new(
            ErrorKind::Invalid,
            format!("llm openai: unsupported scheme {scheme:?}"),
        ));
    }
    let default_port = if scheme == "https" { 443 } else { 80 };
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, format!("/{path}")),
        None => (rest, String::new()),
    };
    if authority.is_empty() {
        return Err(Error::new(
            ErrorKind::Invalid,
            format!("llm openai: missing host in base URL {base:?}"),
        ));
    }
    Ok(ParsedUrl {
        scheme: scheme.to_string(),
        authority: authority.to_string(),
        path,
        default_port,
    })
}

fn host_without_port(authority: &str) -> String {
    if let Some(stripped) = authority.strip_prefix('[') {
        if let Some(end) = stripped.find(']') {
            return stripped[..end].to_string();
        }
    }
    authority
        .rsplit_once(':')
        .map(|(host, _)| host.to_string())
        .unwrap_or_else(|| authority.to_string())
}

fn port_from_authority(authority: &str) -> Option<u16> {
    let port = if authority.starts_with('[') {
        authority.split_once(']')?.1.strip_prefix(':')?
    } else {
        authority.rsplit_once(':')?.1
    };
    port.parse().ok()
}

fn read_header_line<R: Read>(reader: &mut R) -> io::Result<Option<String>> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => {
                if line.is_empty() {
                    return Ok(None);
                }
                break;
            }
            Ok(_) => {
                if byte[0] == b'\n' {
                    break;
                }
                line.push(byte[0]);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == io::ErrorKind::TimedOut => continue,
            Err(e) => return Err(e),
        }
    }
    let end = usize::from(line.last() == Some(&b'\r'));
    line.truncate(line.len().saturating_sub(end));
    Ok(Some(String::from_utf8_lossy(&line).into_owned()))
}

fn parse_status(line: &str) -> Option<u16> {
    line.split_whitespace().nth(1)?.parse().ok()
}

fn transport_error(message: &str) -> Error {
    provider_error_with(
        ErrorKind::Io,
        pe(
            CODE_TRANSPORT,
            0,
            true,
            Duration::ZERO,
            String::new(),
            message.to_string(),
        ),
    )
}

fn retryable_status(status: u16) -> bool {
    status == 408 || status == 429 || (500..=599).contains(&status)
}

fn status_slug(status: u16) -> String {
    match status {
        400 => "bad_request".to_string(),
        401 => "unauthorized".to_string(),
        403 => "forbidden".to_string(),
        404 => "not_found".to_string(),
        408 => "timeout".to_string(),
        409 => "conflict".to_string(),
        429 => "rate_limited".to_string(),
        500..=599 => "server_error".to_string(),
        other => format!("http_{other}"),
    }
}

fn error_code(status: u16, body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    if let Ok(value) = serde_json::from_str::<Value>(text.trim()) {
        if let Some(raw) = value.get("error") {
            if !raw.is_null() {
                if let Some(api_error) = decode_api_error(raw) {
                    let code = api_error_code(&api_error);
                    if !code.is_empty() {
                        return code;
                    }
                }
            }
        }
    }
    status_slug(status)
}

fn status_error(status: u16, body: &[u8], retry_after: Duration) -> Error {
    let error = pe(
        &error_code(status, body),
        status as i32,
        retryable_status(status),
        retry_after,
        clip(body),
        String::new(),
    );
    provider_error_with(ErrorKind::Io, error)
}

/// Parse a `Retry-After` header as delta-seconds or an HTTP date.
pub fn parse_retry_after(value: &str) -> Duration {
    let value = value.trim();
    if value.is_empty() {
        return Duration::ZERO;
    }
    if let Ok(seconds) = value.parse::<u64>() {
        return if seconds == 0 {
            Duration::ZERO
        } else {
            Duration::from_secs(seconds)
        };
    }
    if let Some(when) = parse_http_date(value) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        if when > now {
            return when - now;
        }
    }
    Duration::ZERO
}

fn month_number(token: &str) -> Option<u32> {
    let lower = token.to_ascii_lowercase();
    match lower.as_str() {
        "jan" => Some(1),
        "feb" => Some(2),
        "mar" => Some(3),
        "apr" => Some(4),
        "may" => Some(5),
        "jun" => Some(6),
        "jul" => Some(7),
        "aug" => Some(8),
        "sep" => Some(9),
        "oct" => Some(10),
        "nov" => Some(11),
        "dec" => Some(12),
        _ => None,
    }
}

fn parse_http_date(value: &str) -> Option<Duration> {
    let normalized = value.replace([',', '-'], " ");
    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    let month_index = tokens
        .iter()
        .position(|token| month_number(token).is_some())?;
    let month = month_number(tokens[month_index])?;
    let mut day: Option<u32> = None;
    let mut year: Option<i64> = None;
    let mut time: Option<(u32, u32, u32)> = None;
    for (index, token) in tokens.iter().enumerate() {
        if token.contains(':') {
            let mut pieces = token.split(':');
            let hour = pieces.next()?.parse().ok()?;
            let minute = pieces.next()?.parse().ok()?;
            let second = pieces.next()?.parse().ok()?;
            time = Some((hour, minute, second));
        } else if !token.is_empty() && token.bytes().all(|b| b.is_ascii_digit()) {
            let adjacent = index + 1 == month_index || index == month_index + 1;
            if adjacent && token.len() <= 2 {
                day = Some(token.parse().ok()?);
            } else if token.len() == 4 {
                year = Some(token.parse().ok()?);
            } else if token.len() == 2 {
                let short: i64 = token.parse().ok()?;
                year = Some(if short >= 70 {
                    1900 + short
                } else {
                    2000 + short
                });
            } else if day.is_none() {
                day = Some(token.parse().ok()?);
            }
        }
    }
    let day = day?;
    let year = year?;
    let (hour, minute, second) = time?;
    if !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour as i64 * 3_600 + minute as i64 * 60 + second as i64;
    if seconds < 0 {
        return None;
    }
    Some(Duration::from_secs(seconds as u64))
}

/// Days since 1970-01-01 (Howard Hinnant's civil-date algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = month as i64;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ChatTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "is_zero")]
    max_tokens: i64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    stop: Vec<String>,
    stream: bool,
    stream_options: StreamOptions,
}

fn is_zero(value: &i64) -> bool {
    *value == 0
}

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    /// Either a bare string (the ordinary case) or an array of content parts.
    /// `encode_message` decides which; the field is `Value` so the plain path
    /// keeps producing exactly the JSON it produced before.
    #[serde(skip_serializing_if = "content_is_empty")]
    content: Value,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<ChatToolCall>,
    #[serde(skip_serializing_if = "String::is_empty")]
    tool_call_id: String,
}

/// The old `skip_serializing_if = "String::is_empty"` rule, now applied to a
/// `Value`: skip only when it is the empty string (never when it is an array of
/// parts, which is how an image-only message stays on the wire).
fn content_is_empty(value: &Value) -> bool {
    matches!(value, Value::String(text) if text.is_empty())
}

/// Base64, standard alphabet, padded.
///
/// A twenty-line copy rather than a dependency: the budget is nine crates, and
/// the server crate already has one of these (`crates/server/src/server.rs`);
/// importing it would make the provider layer depend on the HTTP layer for a
/// function with no HTTP in it.
fn base64_padded(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let word = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((word >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((word >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((word >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(word & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// A `data:` URI for one image, the form an OpenAI-compatible server accepts.
///
/// Separated from `encode_parts` so the base64 itself can be checked against
/// values computed outside this crate: padding is where hand-written encoders go
/// wrong, and an off-by-one there is invisible in a round-trip test.
pub(crate) fn encode_message_image_uri(media_type: &str, data: &[u8]) -> String {
    format!("data:{media_type};base64,{}", base64_padded(data))
}

/// The OpenAI-compatible array form: a text part followed by one `image_url`
/// part per image, each carrying the bytes as a `data:` URI.
///
/// The text part is emitted first even when it is empty: a message carrying only
/// an image still needs the array shape, and providers that accept an image-only
/// message expect the text part to come first.
fn encode_parts(message: &Message) -> Value {
    let mut parts: Vec<Value> = vec![json!({"type": "text", "text": message.content})];
    for part in &message.parts {
        match part {
            ContentPart::Text(text) => parts.push(json!({"type": "text", "text": text})),
            ContentPart::Image { media_type, data } => {
                parts.push(json!({
                    "type": "image_url",
                    "image_url": {"url": encode_message_image_uri(media_type, data)},
                }));
            }
        }
    }
    Value::Array(parts)
}

#[derive(Serialize)]
struct ChatToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: ChatFunction,
}

#[derive(Serialize)]
struct ChatFunction {
    name: String,
    arguments: String,
}

#[derive(Serialize)]
struct ChatTool {
    #[serde(rename = "type")]
    kind: String,
    function: ChatToolFunction,
}

#[derive(Serialize)]
struct ChatToolFunction {
    name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    description: String,
    parameters: Value,
}

fn encode_message(message: &Message) -> Result<ChatMessage> {
    let mut tool_calls = Vec::new();
    for (index, call) in message.tool_calls.iter().enumerate() {
        let arguments = if call.args.is_null() {
            "{}".to_string()
        } else {
            call.args.to_string()
        };
        if serde_json::from_str::<Value>(&arguments).is_err() {
            return Err(Error::new(
                ErrorKind::Json,
                format!("message {index}: invalid tool arguments"),
            ));
        }
        tool_calls.push(ChatToolCall {
            id: call.id.clone(),
            kind: "function".to_string(),
            function: ChatFunction {
                name: call.name.clone(),
                arguments,
            },
        });
    }
    Ok(ChatMessage {
        role: message.role.as_str().to_string(),
        // A message with parts gets the array form; everything else keeps the
        // bare string it always had, so no frozen fixture moves.
        content: if message.has_parts() {
            encode_parts(message)
        } else {
            Value::String(message.content.clone())
        },
        tool_calls,
        tool_call_id: message.tool_call_id.clone().unwrap_or_default(),
    })
}

fn encode_tool(spec: &crate::ToolSpec) -> Result<ChatTool> {
    let schema = match &spec.schema {
        Value::Null => json!({"type": "object", "properties": {}}),
        other => other.clone(),
    };
    if !schema.is_object() {
        return Err(Error::new(
            ErrorKind::Invalid,
            "tool schema must be an object",
        ));
    }
    Ok(ChatTool {
        kind: "function".to_string(),
        function: ChatToolFunction {
            name: spec.name.clone(),
            description: spec.description.clone(),
            parameters: schema,
        },
    })
}

fn encode_request(request: &Request) -> Result<Vec<u8>> {
    let mut messages = Vec::with_capacity(request.messages.len());
    for (index, message) in request.messages.iter().enumerate() {
        messages.push(encode_message(message).map_err(|e| {
            Error::new(
                ErrorKind::Invalid,
                format!("message {index}: {}", e.message()),
            )
        })?);
    }
    let mut tools = Vec::with_capacity(request.tools.len());
    for (index, tool) in request.tools.iter().enumerate() {
        tools.push(encode_tool(tool).map_err(|e| {
            Error::new(ErrorKind::Invalid, format!("tool {index}: {}", e.message()))
        })?);
    }
    let wire = ChatRequest {
        model: request.model.clone(),
        messages,
        tools,
        temperature: request.temperature,
        max_tokens: request.max_tokens.unwrap_or(0),
        stop: request.stop.clone(),
        stream: true,
        stream_options: StreamOptions {
            include_usage: true,
        },
    };
    serde_json::to_vec(&wire).map_err(Error::from)
}
