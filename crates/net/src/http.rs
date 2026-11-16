//! Response parsing: the status line, the headers, and the body framing.
//!
//! Kept separate from the client so a long-lived stream (the provider) and a
//! one-shot fetch share one framing implementation. Framing is where HTTP
//! clients quietly disagree, so it has its own tests rather than being proven by
//! whoever happens to use it.

use std::io::{self, BufRead, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use forge_core::{Error, ErrorKind, Result};

use crate::tls::ERR_ABORTED;

/// The response head, before any body bytes.
#[derive(Clone, Debug, Default)]
pub struct Head {
    pub status: u16,
    /// Header names lowercased, values trimmed, in the order received.
    pub headers: Vec<(String, String)>,
}

impl Head {
    /// First value for `name` (case-insensitive on the name as stored).
    pub fn header(&self, name: &str) -> Option<&str> {
        let wanted = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(key, _)| *key == wanted)
            .map(|(_, value)| value.as_str())
    }

    /// Every value for `name`, since `Set-Cookie` repeats.
    pub fn headers_named(&self, name: &str) -> Vec<&str> {
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

    /// The framing the body uses, from `Transfer-Encoding` then `Content-Length`.
    pub fn body_mode(&self) -> BodyMode {
        let chunked = self.headers.iter().any(|(name, value)| {
            name == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked")
        });
        if chunked {
            return BodyMode::Chunked {
                remaining: 0,
                need_crlf: false,
            };
        }
        match self
            .header("content-length")
            .and_then(|v| v.trim().parse::<usize>().ok())
        {
            Some(length) => BodyMode::Length(length),
            None => BodyMode::UntilEof,
        }
    }
}

/// Read the status line and headers from `reader`.
pub fn read_head<R: BufRead>(reader: &mut R) -> Result<Head> {
    let status_line = match read_line(reader)? {
        Some(line) => line,
        None => return Err(protocol("no response from server")),
    };
    let status = parse_status(&status_line)
        .ok_or_else(|| protocol(&format!("malformed status line: {status_line:?}")))?;
    let mut headers = Vec::new();
    loop {
        let Some(line) = read_line(reader)? else {
            break;
        };
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    Ok(Head { status, headers })
}

fn protocol(message: &str) -> Error {
    Error::new(ErrorKind::Io, format!("net: {message}"))
}

/// Read one CRLF-terminated line without its terminator.
pub(crate) fn read_line<R: BufRead>(reader: &mut R) -> Result<Option<String>> {
    let mut line = String::new();
    let read = reader
        .read_line(&mut line)
        .map_err(|e| protocol(&format!("cannot read response: {e}")))?;
    if read == 0 && line.is_empty() {
        return Ok(None);
    }
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
    Ok(Some(line))
}

/// Parse `HTTP/1.1 200 OK` into `200`.
pub fn parse_status(line: &str) -> Option<u16> {
    let mut parts = line.split_whitespace();
    let version = parts.next()?;
    if !version.starts_with("HTTP/") {
        return None;
    }
    parts.next()?.parse().ok()
}

/// How the response body is delimited.
#[derive(Clone, Copy, Debug)]
pub enum BodyMode {
    Chunked { remaining: usize, need_crlf: bool },
    Length(usize),
    UntilEof,
}

/// A `Read` adapter that honours the response framing, the deadline and the
/// cancel flag of the connection beneath it.
pub struct Body<R: Read> {
    inner: R,
    mode: BodyMode,
    closed: Arc<AtomicBool>,
    finished: bool,
}

impl<R: Read> Body<R> {
    pub fn new(inner: R, mode: BodyMode, closed: Arc<AtomicBool>) -> Self {
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
                        return Err(io::Error::new(
                            io::ErrorKind::ConnectionAborted,
                            ERR_ABORTED,
                        ));
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

impl<R: Read> Read for Body<R> {
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

/// A short slug for a status, used in error codes and messages.
pub fn status_slug(status: u16) -> String {
    match status {
        400 => "bad_request",
        401 => "unauthorized",
        403 => "forbidden",
        404 => "not_found",
        405 => "method_not_allowed",
        408 => "request_timeout",
        409 => "conflict",
        410 => "gone",
        413 => "payload_too_large",
        415 => "unsupported_media_type",
        422 => "unprocessable",
        429 => "rate_limited",
        500 => "server_error",
        501 => "not_implemented",
        502 => "bad_gateway",
        503 => "unavailable",
        504 => "gateway_timeout",
        _ => "http_error",
    }
    .to_string()
}

/// Retry-After as a duration: seconds or an HTTP date.
pub fn parse_retry_after(value: &str) -> Duration {
    let value = value.trim();
    if value.is_empty() {
        return Duration::ZERO;
    }
    if let Ok(seconds) = value.parse::<u64>() {
        return Duration::from_secs(seconds);
    }
    parse_http_date(value)
        .map(|instant| Duration::from_secs(instant.saturating_sub(now_unix())))
        .unwrap_or(Duration::ZERO)
}

/// An HTTP date as seconds since the Unix epoch. `None` when it does not parse.
pub fn parse_http_date(value: &str) -> Option<u64> {
    // IMF-fixdate: `Sun, 06 Nov 1994 08:49:37 GMT`
    let mut parts = value.split_whitespace();
    let _day_name = parts.next()?;
    let day: u32 = parts.next()?.trim_end_matches(',').parse().ok()?;
    let month = month_number(parts.next()?)?;
    let year: i64 = parts.next()?.parse().ok()?;
    let time = parts.next()?;
    let mut clock = time.split(':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let second: i64 = clock.next()?.parse().ok()?;
    let days = days_from_civil(year, month, day);
    Some((days * 86_400 + hour * 3_600 + minute * 60 + second).max(0) as u64)
}

fn month_number(token: &str) -> Option<u32> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    MONTHS
        .iter()
        .position(|name| name.eq_ignore_ascii_case(token))
        .map(|index| index as u32 + 1)
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = month as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn head_of(raw: &str) -> Head {
        let mut reader = Cursor::new(raw.as_bytes().to_vec());
        read_head(&mut reader).expect("head")
    }

    #[test]
    fn parses_status_and_headers() {
        let head = head_of("HTTP/1.1 301 Moved Permanently\r\nLocation: /next\r\nX-A:  b \r\n\r\n");
        assert_eq!(head.status, 301);
        assert_eq!(head.header("location"), Some("/next"));
        assert_eq!(head.header("x-a"), Some("b"));
        assert!(!head.is_success());
    }

    #[test]
    fn chooses_the_framing_from_the_headers() {
        let chunked = head_of("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n");
        assert!(matches!(chunked.body_mode(), BodyMode::Chunked { .. }));

        let sized = head_of("HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\n");
        assert!(matches!(sized.body_mode(), BodyMode::Length(12)));

        let until_eof = head_of("HTTP/1.1 200 OK\r\n\r\n");
        assert!(matches!(until_eof.body_mode(), BodyMode::UntilEof));
    }

    #[test]
    fn reads_a_length_delimited_body_once() {
        let mode = BodyMode::Length(5);
        let mut body = Body::new(
            Cursor::new(b"helloextra".to_vec()),
            mode,
            Arc::new(AtomicBool::new(false)),
        );
        let mut out = String::new();
        body.read_to_string(&mut out).expect("read");
        assert_eq!(out, "hello");
    }

    #[test]
    fn reads_a_chunked_body_including_the_trailer() {
        let raw = b"4\r\nWiki\r\n5\r\npedia\r\n0\r\nX-Trailer: 1\r\n\r\n";
        let mode = BodyMode::Chunked {
            remaining: 0,
            need_crlf: false,
        };
        let mut body = Body::new(
            Cursor::new(raw.to_vec()),
            mode,
            Arc::new(AtomicBool::new(false)),
        );
        let mut out = String::new();
        body.read_to_string(&mut out).expect("read");
        assert_eq!(out, "Wikipedia");
    }

    #[test]
    fn reports_a_truncated_chunked_body() {
        let raw = b"10\r\nshort";
        let mode = BodyMode::Chunked {
            remaining: 0,
            need_crlf: false,
        };
        let mut body = Body::new(
            Cursor::new(raw.to_vec()),
            mode,
            Arc::new(AtomicBool::new(false)),
        );
        let mut out = Vec::new();
        assert!(body.read_to_end(&mut out).is_err());
    }

    #[test]
    fn parses_retry_after_seconds_and_dates() {
        assert_eq!(parse_retry_after("7"), Duration::from_secs(7));
        assert_eq!(parse_retry_after(""), Duration::ZERO);
        // A date in the past saturates at zero rather than going negative.
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT"),
            Duration::ZERO
        );
        assert!(parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT").is_some());
        assert!(parse_http_date("not a date").is_none());
    }

    #[test]
    fn status_slugs_are_stable_names() {
        assert_eq!(status_slug(404), "not_found");
        assert_eq!(status_slug(429), "rate_limited");
        assert_eq!(status_slug(599), "http_error");
    }
}
