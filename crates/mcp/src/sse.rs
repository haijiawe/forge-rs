//! Incremental `text/event-stream` decoding for Streamable HTTP responses.

use std::io::BufRead;

use crate::Error;

pub(crate) const MAX_EVENT_BYTES: usize = 8 << 20;

pub(crate) struct SseReader<R: BufRead> {
    inner: R,
}

impl<R: BufRead> SseReader<R> {
    pub(crate) fn new(inner: R) -> Self {
        Self { inner }
    }

    /// Return the next complete event's joined `data` lines. `Ok(None)` means
    /// a clean end of stream after the last event.
    pub(crate) fn next_event(&mut self) -> Result<Option<String>, Error> {
        let mut data = String::new();
        let mut line: Vec<u8> = Vec::new();
        loop {
            line.clear();
            let read = self.inner.read_until(b'\n', &mut line).map_err(Error::Io)?;
            if read == 0 {
                return if data.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(data))
                };
            }
            if line.len() > MAX_EVENT_BYTES {
                return Err(Error::protocol(format!(
                    "SSE event exceeds {MAX_EVENT_BYTES} bytes"
                )));
            }
            while matches!(line.last(), Some(b'\n' | b'\r')) {
                line.pop();
            }
            if line.is_empty() {
                return if data.is_empty() {
                    continue;
                } else {
                    Ok(Some(data))
                };
            }
            if line[0] == b':' {
                continue;
            }
            let (field, value) = match line.iter().position(|b| *b == b':') {
                Some(at) => {
                    let mut value = &line[at + 1..];
                    if value.first() == Some(&b' ') {
                        value = &value[1..];
                    }
                    (&line[..at], value)
                }
                None => (&line[..], &[][..]),
            };
            if field == b"data" {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(&String::from_utf8_lossy(value));
            }
        }
    }
}
