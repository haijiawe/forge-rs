//! A byte sink that keeps only the first `limit` bytes.
//!
//! The runner reads pipes itself, so this type never has to report a short
//! write: every chunk is consumed completely and excess is discarded. That is
//! load-bearing. A writer that returns `Ok(room)` instead of `Ok(chunk.len())`
//! makes `os/exec`-style copiers stop early, the pipe fills, and the child
//! blocks forever in `write(2)` while the parent waits for it to exit.

pub(crate) struct CappedBuffer {
    limit: usize,
    pub(crate) data: Vec<u8>,
    pub(crate) truncated: bool,
}

impl CappedBuffer {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            limit,
            // A 1 MiB default is common; reserving it once avoids repeated
            // growth for the shell output that dominates real runs.
            data: Vec::with_capacity(limit.min(64 * 1024)),
            truncated: false,
        }
    }

    pub(crate) fn push(&mut self, chunk: &[u8]) {
        let room = self.limit.saturating_sub(self.data.len());
        if room == 0 {
            if !chunk.is_empty() {
                self.truncated = true;
            }
            return;
        }
        if chunk.len() > room {
            self.data.extend_from_slice(&chunk[..room]);
            self.truncated = true;
        } else {
            self.data.extend_from_slice(chunk);
        }
    }
}
