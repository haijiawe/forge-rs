//! Streamable HTTP transport: one POST per JSON-RPC message, response either a
//! JSON document or an SSE stream. TLS is real rustls with webpki roots; the
//! socket layer is a small blocking HTTP/1.1 implementation over `TcpStream`.

use std::io::{self, BufRead, BufReader, ErrorKind as IoErrorKind, Read, Write};
use std::net::{IpAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use rustls_pki_types::ServerName;

use crate::jsonrpc::{self, WireMessage};
use crate::{Conn, Error, Logger, Server};

pub(crate) const MAX_RESPONSE_BYTES: usize = 8 << 20;
const MAX_ERROR_BODY: usize = 4 << 10;
const MAX_HEADER_BYTES: usize = 64 << 10;
const SESSION_HEADER: &str = "mcp-session-id";
const PROTOCOL_HEADER: &str = "mcp-protocol-version";

#[derive(Debug, Clone)]
struct Url {
    tls: bool,
    authority: String,
    host: String,
    port: u16,
    path: String,
}

impl Url {
    fn parse(raw: &str) -> Result<Self, Error> {
        let (scheme, rest) = raw
            .split_once("://")
            .ok_or_else(|| Error::Config(format!("invalid MCP URL {raw:?}: missing scheme")))?;
        let tls = match scheme.to_ascii_lowercase().as_str() {
            "http" => false,
            "https" => true,
            other => {
                return Err(Error::Config(format!(
                    "invalid MCP URL {raw:?}: unsupported scheme {other:?}"
                )))
            }
        };
        let (authority, path) = match rest.find('/') {
            Some(at) => (&rest[..at], &rest[at..]),
            None => (rest, "/"),
        };
        if authority.is_empty() {
            return Err(Error::Config(format!(
                "invalid MCP URL {raw:?}: empty host"
            )));
        }
        let (host, port) = if authority.starts_with('[') {
            let end = authority.find(']').ok_or_else(|| {
                Error::Config(format!("invalid MCP URL {raw:?}: unclosed IPv6 host"))
            })?;
            let host = &authority[1..end];
            let port = if authority.len() > end + 1 {
                let tail = &authority[end + 1..];
                let port = tail.strip_prefix(':').ok_or_else(|| {
                    Error::Config(format!("invalid MCP URL {raw:?}: bad authority"))
                })?;
                parse_port(port, raw)?
            } else if tls {
                443
            } else {
                80
            };
            (host.to_string(), port)
        } else {
            match authority.rsplit_once(':') {
                Some((host, port)) if !port.contains(']') => {
                    (host.to_string(), parse_port(port, raw)?)
                }
                _ => (authority.to_string(), if tls { 443 } else { 80 }),
            }
        };
        if host.is_empty() {
            return Err(Error::Config(format!(
                "invalid MCP URL {raw:?}: empty host"
            )));
        }
        Ok(Url {
            tls,
            authority: authority.to_string(),
            host,
            port,
            path: path.to_string(),
        })
    }
}

fn parse_port(port: &str, raw: &str) -> Result<u16, Error> {
    port.parse::<u16>()
        .map_err(|_| Error::Config(format!("invalid MCP URL {raw:?}: invalid port {port:?}")))
}

#[derive(Default)]
struct Headers(Vec<(String, String)>);

impl Headers {
    fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn push(&mut self, name: &str, value: &str) {
        self.0.push((name.to_ascii_lowercase(), value.to_string()));
    }
}

enum NetStream {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
}

impl Read for NetStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            NetStream::Plain(stream) => stream.read(buf),
            NetStream::Tls(stream) => stream.read(buf),
        }
    }
}

impl Write for NetStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            NetStream::Plain(stream) => stream.write(buf),
            NetStream::Tls(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            NetStream::Plain(stream) => stream.flush(),
            NetStream::Tls(stream) => stream.flush(),
        }
    }
}

enum BodyMode {
    Chunked(usize),
    Length(u64),
    UntilEof,
}

/// Reads an HTTP body with a hard byte cap. Chunk framing is consumed here so
/// callers see only payload bytes.
struct BodyReader<R: BufRead> {
    inner: R,
    mode: BodyMode,
    total: usize,
    limit: usize,
    finished: bool,
}

impl<R: BufRead> BodyReader<R> {
    fn new(inner: R, headers: &Headers, limit: usize) -> Self {
        let mode = if headers
            .get("transfer-encoding")
            .map(|v| v.to_ascii_lowercase().contains("chunked"))
            .unwrap_or(false)
        {
            BodyMode::Chunked(0)
        } else if let Some(length) = headers
            .get("content-length")
            .and_then(|v| v.trim().parse::<u64>().ok())
        {
            BodyMode::Length(length)
        } else {
            BodyMode::UntilEof
        };
        BodyReader {
            inner,
            mode,
            total: 0,
            limit,
            finished: false,
        }
    }

    fn allowance(&self, wanted: usize) -> Result<usize, io::Error> {
        let left = self.limit + 1;
        if self.total >= left {
            return Err(io::Error::new(
                IoErrorKind::InvalidData,
                format!("response body exceeds {limit} bytes", limit = self.limit),
            ));
        }
        Ok(wanted.min(left - self.total))
    }

    fn next_chunk(&mut self) -> io::Result<()> {
        let mut line = String::new();
        let read = self.inner.read_line(&mut line)?;
        if read == 0 {
            return Err(io::Error::new(
                IoErrorKind::UnexpectedEof,
                "missing chunk size",
            ));
        }
        if line.len() > MAX_HEADER_BYTES {
            return Err(io::Error::new(
                IoErrorKind::InvalidData,
                "chunk header too long",
            ));
        }
        let size = line.trim().split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size, 16)
            .map_err(|_| io::Error::new(IoErrorKind::InvalidData, "invalid chunk size"))?;
        if size == 0 {
            loop {
                let mut trailer = String::new();
                if self.inner.read_line(&mut trailer)? == 0 {
                    break;
                }
                if trailer.trim().is_empty() {
                    break;
                }
            }
            self.finished = true;
            return Ok(());
        }
        self.mode = BodyMode::Chunked(size);
        Ok(())
    }
}

impl<R: BufRead> Read for BodyReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.finished || buf.is_empty() {
            return Ok(0);
        }
        match self.mode {
            BodyMode::Chunked(remaining) => {
                if remaining == 0 {
                    self.next_chunk()?;
                    if self.finished {
                        return Ok(0);
                    }
                }
                let allowed = self.allowance(buf.len())?;
                let wanted = match self.mode {
                    BodyMode::Chunked(remaining) => remaining.min(allowed),
                    _ => unreachable!(),
                };
                let n = self.inner.read(&mut buf[..wanted])?;
                if n == 0 {
                    return Err(io::Error::new(IoErrorKind::UnexpectedEof, "short chunk"));
                }
                self.total += n;
                if let BodyMode::Chunked(remaining) = &mut self.mode {
                    *remaining -= n;
                    if *remaining == 0 {
                        let mut ending = [0u8; 2];
                        let mut count = 0;
                        while count < 2 {
                            let got = self.inner.read(&mut ending[count..])?;
                            if got == 0 {
                                break;
                            }
                            count += got;
                        }
                        if ending[0] == b'\r' && ending[1] == b'\n' {
                            // normal chunk terminator
                        } else if ending[0] != b'\n' {
                            return Err(io::Error::new(
                                IoErrorKind::InvalidData,
                                "invalid chunk terminator",
                            ));
                        }
                    }
                }
                if self.total > self.limit {
                    return Err(io::Error::new(
                        IoErrorKind::InvalidData,
                        format!("response body exceeds {limit} bytes", limit = self.limit),
                    ));
                }
                Ok(n)
            }
            BodyMode::Length(remaining) => {
                if remaining == 0 {
                    self.finished = true;
                    return Ok(0);
                }
                let allowed = self.allowance(buf.len())?;
                let wanted = (remaining as usize).min(allowed);
                let n = self.inner.read(&mut buf[..wanted])?;
                if n == 0 {
                    return Err(io::Error::new(
                        IoErrorKind::UnexpectedEof,
                        "short response body",
                    ));
                }
                self.total += n;
                self.mode = BodyMode::Length(remaining - n as u64);
                if self.total > self.limit {
                    return Err(io::Error::new(
                        IoErrorKind::InvalidData,
                        format!("response body exceeds {limit} bytes", limit = self.limit),
                    ));
                }
                Ok(n)
            }
            BodyMode::UntilEof => {
                let allowed = self.allowance(buf.len())?;
                let n = self.inner.read(&mut buf[..allowed])?;
                if n == 0 {
                    self.finished = true;
                    return Ok(0);
                }
                self.total += n;
                if self.total > self.limit {
                    return Err(io::Error::new(
                        IoErrorKind::InvalidData,
                        format!("response body exceeds {limit} bytes", limit = self.limit),
                    ));
                }
                Ok(n)
            }
        }
    }
}

fn read_capped<R: Read>(mut reader: R, limit: usize) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    let mut buffer = [0u8; 8 << 10];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            return Ok(out);
        }
        out.extend_from_slice(&buffer[..n]);
        if out.len() > limit {
            return Err(Error::protocol(format!(
                "response body exceeds {limit} bytes"
            )));
        }
    }
}

struct HttpResponse {
    status: u16,
    headers: Headers,
    reader: BufReader<NetStream>,
}

struct HttpConn {
    name: String,
    url: Url,
    headers: Vec<(String, String)>,
    session: Mutex<String>,
    protocol: Mutex<String>,
    gone: Mutex<Option<String>>,
    tls: Option<Arc<ClientConfig>>,
    logger: Arc<dyn Logger>,
    closed: AtomicBool,
}

pub(crate) fn dial_http(server: &Server, logger: &Arc<dyn Logger>) -> Result<Arc<dyn Conn>, Error> {
    let url = Url::parse(&server.url)?;
    let mut headers = Vec::new();
    for header in &server.headers {
        let Some((name, value)) = header.split_once(':') else {
            return Err(Error::Config(format!(
                "server {:?}: invalid header {header:?} (want Name: value)",
                server.label()
            )));
        };
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::Config(format!(
                "server {:?}: invalid header {header:?} (empty name)",
                server.label()
            )));
        }
        headers.push((name.to_ascii_lowercase(), value.trim().to_string()));
    }
    let tls = if url.tls {
        let mut roots = RootCertStore::empty();
        for anchor in webpki_roots::TLS_SERVER_ROOTS.iter() {
            roots.roots.push(anchor.clone());
        }
        let config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        Some(Arc::new(config))
    } else {
        None
    };
    Ok(Arc::new(HttpConn {
        name: server.label(),
        url,
        headers,
        session: Mutex::new(String::new()),
        protocol: Mutex::new(String::new()),
        gone: Mutex::new(None),
        tls,
        logger: logger.clone(),
        closed: AtomicBool::new(false),
    }))
}

fn remaining_ms(deadline: Instant) -> u64 {
    deadline
        .checked_duration_since(Instant::now())
        .unwrap_or(Duration::ZERO)
        .as_millis()
        .min(u128::from(u32::MAX)) as u64
}

impl HttpConn {
    fn mark_gone(&self, reason: &str) {
        if let Ok(mut gone) = self.gone.lock() {
            if gone.is_none() {
                *gone = Some(reason.to_string());
            }
        }
    }

    fn gone_reason(&self) -> Option<String> {
        self.gone.lock().ok().and_then(|g| g.clone()).or_else(|| {
            if self.closed.load(Ordering::Acquire) {
                Some("client closed".to_string())
            } else {
                None
            }
        })
    }

    fn connect(&self, deadline: Instant) -> Result<NetStream, Error> {
        let address = (self.url.host.as_str(), self.url.port)
            .to_socket_addrs()
            .map_err(|e| Error::Config(format!("server {:?}: resolve host: {e}", self.name)))?
            .next()
            .ok_or_else(|| Error::Config(format!("server {:?}: no address for host", self.name)))?;
        let timeout = Duration::from_millis(remaining_ms(deadline).max(1));
        let tcp = TcpStream::connect_timeout(&address, timeout).map_err(|e| {
            if e.kind() == IoErrorKind::TimedOut {
                Error::timeout(format!("server {:?}: TCP connect timed out", self.name))
            } else {
                Error::gone(format!("server {:?}: connect: {e}", self.name))
            }
        })?;
        let _ = tcp.set_read_timeout(Some(timeout));
        let _ = tcp.set_write_timeout(Some(timeout));
        let _ = tcp.set_nodelay(true);
        let Some(config) = self.tls.clone() else {
            return Ok(NetStream::Plain(tcp));
        };
        let server_name = if let Ok(ip) = self.url.host.parse::<IpAddr>() {
            ServerName::IpAddress(ip.into())
        } else {
            ServerName::try_from(self.url.host.clone()).map_err(|e| {
                Error::Config(format!("server {:?}: invalid TLS host: {e}", self.name))
            })?
        };
        let connection = ClientConnection::new(config, server_name)
            .map_err(|e| Error::Config(format!("server {:?}: TLS client: {e}", self.name)))?;
        let mut stream = StreamOwned::new(connection, tcp);
        while stream.conn.is_handshaking() {
            if remaining_ms(deadline) == 0 {
                return Err(Error::timeout(format!(
                    "server {:?}: TLS handshake timed out",
                    self.name
                )));
            }
            match stream.conn.complete_io(&mut stream.sock) {
                Ok(_) => {}
                Err(e)
                    if e.kind() == IoErrorKind::TimedOut || e.kind() == IoErrorKind::WouldBlock =>
                {
                    if remaining_ms(deadline) == 0 {
                        return Err(Error::timeout(format!(
                            "server {:?}: TLS handshake timed out",
                            self.name
                        )));
                    }
                }
                Err(e) => return Err(Error::gone(format!("server {:?}: TLS: {e}", self.name))),
            }
        }
        Ok(NetStream::Tls(Box::new(stream)))
    }

    fn post(&self, deadline: Instant, payload: &[u8]) -> Result<HttpResponse, Error> {
        if let Some(reason) = self.gone_reason() {
            return Err(Error::gone(reason.to_string()));
        }
        let mut stream = self.connect(deadline)?;
        let session = self.session.lock().map(|s| s.clone()).unwrap_or_default();
        let protocol = self.protocol.lock().map(|s| s.clone()).unwrap_or_default();
        let mut request = Vec::with_capacity(payload.len() + 512);
        request.extend_from_slice(format!("POST {} HTTP/1.1\r\n", self.url.path).as_bytes());
        request.extend_from_slice(format!("Host: {}\r\n", self.url.authority).as_bytes());
        request.extend_from_slice(b"Connection: close\r\n");
        request.extend_from_slice(b"Content-Type: application/json\r\n");
        request.extend_from_slice(b"Accept: application/json, text/event-stream\r\n");
        request.extend_from_slice(format!("Content-Length: {}\r\n", payload.len()).as_bytes());
        for (name, value) in &self.headers {
            if name == SESSION_HEADER && !session.is_empty() {
                continue;
            }
            if name == PROTOCOL_HEADER && !protocol.is_empty() {
                continue;
            }
            request.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        }
        if !session.is_empty() {
            request.extend_from_slice(format!("Mcp-Session-Id: {session}\r\n").as_bytes());
        }
        if !protocol.is_empty() {
            request.extend_from_slice(format!("MCP-Protocol-Version: {protocol}\r\n").as_bytes());
        }
        request.extend_from_slice(b"\r\n");
        request.extend_from_slice(payload);
        stream.write_all(&request).map_err(|e| {
            if e.kind() == IoErrorKind::TimedOut {
                Error::timeout(format!("server {:?}: write request timed out", self.name))
            } else {
                Error::gone(format!("server {:?}: write request: {e}", self.name))
            }
        })?;
        stream.flush().map_err(Error::from)?;

        let mut reader = BufReader::with_capacity(8 << 10, stream);
        let mut status_line = String::new();
        let read = reader.read_line(&mut status_line)?;
        if read == 0 {
            return Err(Error::gone(format!(
                "server {:?}: connection closed before status line",
                self.name
            )));
        }
        let status = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse::<u16>().ok())
            .ok_or_else(|| {
                Error::protocol(format!(
                    "server {:?}: malformed HTTP status line {:?}",
                    self.name,
                    status_line.trim()
                ))
            })?;
        let mut headers = Headers::default();
        let mut header_bytes = status_line.len();
        loop {
            let mut line = String::new();
            let read = reader.read_line(&mut line)?;
            if read == 0 {
                break;
            }
            header_bytes += line.len();
            if header_bytes > MAX_HEADER_BYTES {
                return Err(Error::protocol(format!(
                    "server {:?}: headers too large",
                    self.name
                )));
            }
            if line.trim().is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                headers.push(name.trim(), value.trim());
            }
        }
        if let Some(id) = headers.get(SESSION_HEADER) {
            if !id.is_empty() {
                if let Ok(mut session) = self.session.lock() {
                    *session = id.to_string();
                }
            }
        }
        Ok(HttpResponse {
            status,
            headers,
            reader,
        })
    }

    fn status_error(&self, response: &mut HttpResponse, method: &str) -> Result<(), Error> {
        if response.status == 404 || response.status == 410 {
            let reason = format!(
                "server {:?}: {method}: session ended (HTTP {})",
                self.name, response.status
            );
            self.mark_gone(&reason);
            return Err(Error::gone(reason));
        }
        if !(200..300).contains(&response.status) {
            let body = read_capped(
                BodyReader::new(&mut response.reader, &response.headers, MAX_ERROR_BODY),
                MAX_ERROR_BODY,
            )
            .unwrap_or_default();
            let detail = String::from_utf8_lossy(&body);
            let detail = detail.trim();
            return Err(Error::HttpStatus {
                status: response.status,
                detail: if detail.is_empty() {
                    method.to_string()
                } else {
                    format!("{method}: {detail}")
                },
            });
        }
        Ok(())
    }

    fn read_document(
        &self,
        method: &str,
        response: &mut HttpResponse,
        id: u64,
    ) -> Result<WireMessage, Error> {
        let body = read_capped(
            BodyReader::new(&mut response.reader, &response.headers, MAX_RESPONSE_BYTES),
            MAX_RESPONSE_BYTES,
        )?;
        let message = jsonrpc::parse_message(&body)?;
        match jsonrpc::parse_id(message.id.as_ref()) {
            Some(got) if got == id => Ok(message),
            other => Err(Error::protocol(format!(
                "server {:?}: {method}: response id {:?} does not match request id {id}",
                self.name, other
            ))),
        }
    }

    fn read_stream(
        &self,
        method: &str,
        response: &mut HttpResponse,
        id: u64,
    ) -> Result<WireMessage, Error> {
        let mut sse = crate::sse::SseReader::new(&mut response.reader);
        loop {
            let event = sse.next_event()?.ok_or_else(|| {
                Error::gone(format!(
                    "server {:?}: {method}: stream ended before response id {id}",
                    self.name
                ))
            })?;
            let message = match jsonrpc::parse_message(event.as_bytes()) {
                Ok(message) => message,
                Err(e) => {
                    self.logger.warn(&format!(
                        "mcp: server {:?}: ignoring malformed SSE event: {e}",
                        self.name
                    ));
                    continue;
                }
            };
            if message.method.is_some() {
                self.logger.debug(&format!(
                    "mcp: server {:?}: ignoring stream notification",
                    self.name
                ));
                continue;
            }
            match jsonrpc::parse_id(message.id.as_ref()) {
                Some(got) if got == id => return Ok(message),
                other => self.logger.debug(&format!(
                    "mcp: server {:?}: ignoring stream response id {:?}",
                    self.name, other
                )),
            }
        }
    }
}

impl Conn for HttpConn {
    fn round_trip(
        &self,
        deadline: Instant,
        method: &str,
        payload: &[u8],
        id: u64,
    ) -> Result<WireMessage, Error> {
        let mut response = self.post(deadline, payload)?;
        self.status_error(&mut response, method)?;
        let is_sse = response
            .headers
            .get("content-type")
            .map(|value| {
                value
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .eq_ignore_ascii_case("text/event-stream")
            })
            .unwrap_or(false);
        if is_sse {
            self.read_stream(method, &mut response, id)
        } else {
            self.read_document(method, &mut response, id)
        }
    }

    fn notify(&self, deadline: Instant, method: &str, payload: &[u8]) -> Result<(), Error> {
        let mut response = self.post(deadline, payload)?;
        self.status_error(&mut response, method)
    }

    fn set_protocol(&self, version: &str) {
        if let Ok(mut protocol) = self.protocol.lock() {
            *protocol = version.to_string();
        }
    }

    fn close(&self) -> Result<(), Error> {
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.mark_gone("client closed");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_stack_builds_with_bundled_roots() {
        let mut roots = RootCertStore::empty();
        for anchor in webpki_roots::TLS_SERVER_ROOTS.iter() {
            roots.roots.push(anchor.clone());
        }
        assert!(!roots.roots.is_empty());
        let config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let name = ServerName::try_from("localhost".to_string()).expect("valid server name");
        assert!(ClientConnection::new(Arc::new(config), name).is_ok());
    }

    #[test]
    fn url_parse_rejects_bad_targets() {
        assert!(Url::parse("ftp://host/mcp").is_err());
        assert!(Url::parse("http:///mcp").is_err());
        assert!(Url::parse("http://host:bad/mcp").is_err());
        let url = Url::parse("https://example.test:8443/mcp").expect("valid URL");
        assert_eq!(url.port, 8443);
        assert_eq!(url.path, "/mcp");
        assert!(url.tls);
    }
}
