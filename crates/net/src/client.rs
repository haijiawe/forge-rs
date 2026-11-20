//! A one-shot HTTP client: request in, bounded response out.
//!
//! Two deliberate choices:
//!
//! * A non-2xx status is **data**, not an error. The caller decides what a 404
//!   means; a transport failure is the only thing that returns `Err`.
//! * The body is always bounded. An unbounded fetch is how a tool turns a
//!   misconfigured URL into an out-of-memory kill.

use std::io::{BufReader, Read, Write};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hearth_core::{Error, ErrorKind, Result};

use crate::http::{parse_retry_after, read_head, status_slug, Body};
use crate::tls::{connect, Roots};
use crate::url::Url;

/// Default for the whole exchange: connect, request, response.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);
/// Default body cap. Large enough for an article, small enough to stay a tool
/// result rather than a memory problem.
pub const DEFAULT_MAX_BYTES: usize = 2 << 20;
/// Redirects are followed a bounded number of times; a loop must terminate.
pub const MAX_REDIRECTS: usize = 5;

/// Client configuration.
#[derive(Clone, Debug)]
pub struct ClientOptions {
    pub timeout: Duration,
    pub max_bytes: usize,
    pub user_agent: String,
    pub max_redirects: usize,
    /// Extra PEM certificates, additively — see [`Roots::with_ca_file`].
    pub ca_file: Option<std::path::PathBuf>,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
            max_bytes: DEFAULT_MAX_BYTES,
            user_agent: concat!("hearth/", env!("CARGO_PKG_VERSION")).to_string(),
            max_redirects: MAX_REDIRECTS,
            ca_file: None,
        }
    }
}

/// A complete response. `body` is already capped; `truncated` says whether the
/// cap cut it.
#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    /// The URL that produced this response, after redirects.
    pub url: String,
    pub body: Vec<u8>,
    pub truncated: bool,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        let wanted = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(key, _)| *key == wanted)
            .map(|(_, value)| value.as_str())
    }

    pub fn header_values(&self, name: &str) -> Vec<&str> {
        let wanted = name.to_ascii_lowercase();
        self.headers
            .iter()
            .filter(|(key, _)| *key == wanted)
            .map(|(_, value)| value.as_str())
            .collect()
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// The body as text, lossily decoded. Byte counts stay available on the
    /// `truncated` flag and `body.len()`.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// A one-line description of a non-2xx response, safe to show a model.
    pub fn error_summary(&self) -> String {
        let mut text = format!("HTTP {} {}", self.status, status_slug(self.status));
        if let Some(retry) = self.header("retry-after") {
            let wait = parse_retry_after(retry);
            if !wait.is_zero() {
                text.push_str(&format!(" (retry after {}s)", wait.as_secs()));
            }
        }
        text
    }
}

/// The client. Cheap to clone-share through an `Arc`.
pub struct Client {
    roots: Roots,
    options: ClientOptions,
}

impl Client {
    pub fn new(options: ClientOptions) -> Result<Self> {
        let roots = Roots::from_option(options.ca_file.as_deref())?;
        Ok(Self { roots, options })
    }

    pub fn options(&self) -> &ClientOptions {
        &self.options
    }

    /// `GET url`, following redirects.
    pub fn get(&self, url: &str) -> Result<Response> {
        let parsed = Url::parse(url)?;
        self.execute("GET", parsed, None, &[])
    }

    /// `POST url` with a JSON body.
    pub fn post_json(&self, url: &str, payload: &[u8]) -> Result<Response> {
        self.post_json_with_headers(url, &[], payload)
    }

    /// `POST url` with a JSON body and extra request headers.
    ///
    /// The headers are for things the server cannot be asked to work without —
    /// an `Authorization` bearer token, for instance. They are only sent to the
    /// configured endpoint and are never carried across a redirect to a
    /// different origin (see [`Client::execute`]).
    pub fn post_json_with_headers(
        &self,
        url: &str,
        headers: &[(String, String)],
        payload: &[u8],
    ) -> Result<Response> {
        let parsed = Url::parse(url)?;
        self.execute(
            "POST",
            parsed,
            Some(("application/json".to_string(), payload.to_vec())),
            headers,
        )
    }

    /// `POST url` with an arbitrary content type and body.
    pub fn post(&self, url: &str, content_type: &str, payload: &[u8]) -> Result<Response> {
        let parsed = Url::parse(url)?;
        self.execute(
            "POST",
            parsed,
            Some((content_type.to_string(), payload.to_vec())),
            &[],
        )
    }

    fn execute(
        &self,
        method: &str,
        url: Url,
        body: Option<(String, Vec<u8>)>,
        headers: &[(String, String)],
    ) -> Result<Response> {
        let deadline = Instant::now() + self.options.timeout;
        let origin = url.origin();
        let mut current = url;
        let mut current_method = method.to_string();
        let mut current_body = body;
        let mut current_headers = headers.to_vec();
        let mut redirects = 0usize;

        loop {
            let response = self.once(
                &current_method,
                &current,
                current_body.as_ref(),
                &current_headers,
                deadline,
            )?;
            let is_redirect = matches!(response.status, 301 | 302 | 303 | 307 | 308);
            if !is_redirect || self.options.max_redirects == 0 {
                return Ok(response);
            }
            let Some(location) = response.header("location") else {
                return Ok(response);
            };
            redirects += 1;
            if redirects > self.options.max_redirects {
                return Err(Error::new(
                    ErrorKind::Invalid,
                    format!(
                        "net: gave up after {} redirects at {}",
                        self.options.max_redirects, current
                    ),
                ));
            }
            let next = current.join(location)?;
            // A credential that was meant for one host must not be handed to
            // whatever host the redirect names: that is the whole point of
            // keeping it out of the URL.
            if next.origin() != origin {
                current_headers.clear();
            }
            // 303 always becomes a bodyless GET. 301/302 are treated the same
            // way for POST, which is what every browser and curl do in practice.
            if response.status == 303
                || (current_method == "POST" && matches!(response.status, 301 | 302))
            {
                current_method = "GET".to_string();
                current_body = None;
            }
            current = next;
        }
    }

    fn once(
        &self,
        method: &str,
        url: &Url,
        body: Option<&(String, Vec<u8>)>,
        headers: &[(String, String)],
        deadline: Instant,
    ) -> Result<Response> {
        let closed = Arc::new(AtomicBool::new(false));
        let mut connection = connect(url, &self.roots, closed.clone(), Some(deadline))?;

        let mut request = Vec::with_capacity(512);
        request.extend_from_slice(
            format!(
                "{method} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {}\r\nAccept: */*\r\nConnection: close\r\n",
                url.request_target(),
                url.authority,
                self.options.user_agent
            )
            .as_bytes(),
        );
        if let Some((content_type, payload)) = body {
            request.extend_from_slice(
                format!(
                    "Content-Type: {content_type}\r\nContent-Length: {}\r\n",
                    payload.len()
                )
                .as_bytes(),
            );
        }
        for (name, value) in headers {
            request.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        }
        request.extend_from_slice(b"\r\n");
        if let Some((_, payload)) = body {
            request.extend_from_slice(payload);
        }

        connection
            .write_all(&request)
            .map_err(|e| Error::new(ErrorKind::Io, format!("net: cannot send request: {e}")))?;

        let mut reader = BufReader::new(connection);
        let head = read_head(&mut reader)?;
        let mode = head.body_mode();
        let mut body = Body::new(reader, mode, closed);

        let mut collected = Vec::new();
        let mut truncated = false;
        let cap = self.options.max_bytes.max(1);
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let wanted = (cap - collected.len()).min(chunk.len());
            if wanted == 0 {
                truncated = true;
                break;
            }
            match body.read(&mut chunk[..wanted]) {
                Ok(0) => break,
                Ok(n) => collected.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                    return Err(Error::new(
                        ErrorKind::Io,
                        format!("net: {} timed out after {:?}", url, self.options.timeout),
                    ))
                }
                Err(e) => {
                    // A body cut short is still a usable answer: report what was
                    // read and let the caller decide. Losing the head because the
                    // tail broke would hide a 200 that mostly worked.
                    if !collected.is_empty() {
                        truncated = true;
                        break;
                    }
                    return Err(Error::new(
                        ErrorKind::Io,
                        format!("net: cannot read response body: {e}"),
                    ));
                }
            }
        }

        Ok(Response {
            status: head.status,
            headers: head.headers,
            url: url.origin() + &url.request_target(),
            body: collected,
            truncated,
        })
    }
}

impl std::fmt::Display for Url {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}{}", self.origin(), self.request_target())
    }
}

/// Read at most `limit` bytes from `reader`, reporting whether it was cut.
pub fn read_bounded<R: Read>(reader: &mut R, limit: usize) -> Result<(Vec<u8>, bool)> {
    let mut out = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let wanted = (limit.saturating_sub(out.len())).min(chunk.len());
        if wanted == 0 {
            return Ok((out, true));
        }
        match reader.read(&mut chunk[..wanted]) {
            Ok(0) => return Ok((out, false)),
            Ok(n) => out.extend_from_slice(&chunk[..n]),
            Err(e) => {
                return Err(Error::new(
                    ErrorKind::Io,
                    format!("net: cannot read stream: {e}"),
                ))
            }
        }
    }
}
