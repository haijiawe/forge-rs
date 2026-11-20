//! The append-only event log.
//!
//! A session is a file. Every reader reconstructs state by replaying it, so
//! this module owns the two things replay cannot recover on its own: sequence
//! assignment (dense, one writer at a time) and crash repair (a torn final
//! line is truncated away before the file is opened for append again).

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use hearth_core::event::FORMAT_VERSION;
use hearth_core::{Error, ErrorKind, Event, Result};

/// How often the store calls `fsync`.
///
/// `sync_every <= 1` means every append, which is the default because losing
/// the last record of a session is what the log exists to prevent. A larger
/// value trades a window of loss for throughput in bulk imports.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    pub sync_every: usize,
    pub no_fsync: bool,
}

struct Inner {
    file: Option<File>,
    path: PathBuf,
    seq: u64,
    pending: usize,
    options: Options,
}

/// An open session log. All methods take `&self` and serialise on one mutex:
/// two writers appending at once is the case where "dense seq" is decided.
pub struct Store {
    inner: Mutex<Inner>,
}

impl Store {
    /// Open (or create) `path`, repairing a torn final line first.
    ///
    /// The version check happens before any truncation. A v2 log is someone
    /// else's truth; rewriting even its torn tail would destroy data this
    /// build cannot interpret, so refusal must be complete.
    pub fn open(path: impl AsRef<Path>, options: Options) -> Result<Store> {
        let path = path.as_ref().to_path_buf();
        let (seq, good) = scan_log(&path)?;
        if let Some(good) = good {
            let size = std::fs::metadata(&path)?.len();
            if size > good {
                // Windows returns "Access is denied" when `set_len` is called
                // on a handle opened with O_APPEND. Truncate through a plain
                // write handle, then drop it before opening the append handle.
                let truncate = OpenOptions::new().write(true).open(&path)?;
                truncate.set_len(good)?;
            }
        }
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&path)?;
        Ok(Store {
            inner: Mutex::new(Inner {
                file: Some(file),
                path,
                seq,
                pending: 0,
                options,
            }),
        })
    }

    /// Append one record and return the sequence number assigned to it.
    ///
    /// The caller's `v`, `seq`, `ts` (when zero) and `id` (when empty) are
    /// overwritten: the log is the only place identity and order exist, and a
    /// caller that supplies them could supply them wrongly.
    pub fn append(&self, event: &Event) -> Result<u64> {
        let mut inner = self.lock();
        let Inner {
            file,
            seq,
            pending,
            options,
            ..
        } = &mut *inner;
        let file = file
            .as_mut()
            .ok_or_else(|| Error::new(ErrorKind::Conflict, "store: append after close"))?;

        let mut record = event.clone();
        record.v = FORMAT_VERSION;
        record.seq = *seq + 1;
        if record.ts == 0 {
            record.ts = now_millis()?;
        }
        if record.id.is_empty() {
            record.id = hearth_core::new_id()?;
        }
        record.validate()?;

        let line = record.canonical()?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        // Advance the counter only after the bytes are in the file: a failed
        // write that still bumped `seq` would leave a hole in the log.
        *seq = record.seq;
        *pending += 1;

        if !options.no_fsync && (options.sync_every <= 1 || *pending >= options.sync_every) {
            file.sync_data()?;
            *pending = 0;
        }
        Ok(record.seq)
    }

    /// Call `f` for every record whose `seq` is at least `after_seq`.
    ///
    /// Reading and appending share the mutex, so a reader never observes the
    /// half-written line an appender is in the middle of emitting.
    pub fn read_from<F>(&self, after_seq: u64, mut f: F) -> Result<()>
    where
        F: FnMut(&Event) -> Result<()>,
    {
        let mut inner = self.lock();
        let file = inner
            .file
            .as_mut()
            .ok_or_else(|| Error::new(ErrorKind::Conflict, "store: read after close"))?;
        file.seek(SeekFrom::Start(0))?;
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;

        for chunk in data.split(|&b| b == b'\n') {
            if chunk.iter().all(|b| b.is_ascii_whitespace()) {
                continue;
            }
            let text = std::str::from_utf8(chunk).map_err(|e| {
                Error::new(ErrorKind::Json, format!("store: log is not UTF-8: {e}"))
            })?;
            let event = Event::parse_line(text.trim())?;
            if event.seq >= after_seq {
                f(&event)?;
            }
        }
        Ok(())
    }

    /// Call `f` for every record stored at or after byte `offset`, and return
    /// the offset just past the last complete line consumed.
    ///
    /// `read_from` re-reads and re-parses the whole file. A resident process
    /// that folds the log once per turn needs only the tail, and at 100k events
    /// the difference is 1.8 s of work before the first byte reaches the model.
    ///
    /// A trailing partial line is **not** consumed: either another process is
    /// midway through its own append, or the file was torn and the next `open`
    /// will repair it. Returning the position before it is what lets the next
    /// call pick the line up whole instead of failing on half a record.
    pub fn read_after<F>(&self, offset: u64, mut f: F) -> Result<u64>
    where
        F: FnMut(&Event) -> Result<()>,
    {
        let mut inner = self.lock();
        let file = inner
            .file
            .as_mut()
            .ok_or_else(|| Error::new(ErrorKind::Conflict, "store: read after close"))?;
        let end = file.metadata()?.len();
        if end <= offset {
            return Ok(offset);
        }
        // Reads seek; writes do not care where the cursor is, because the file
        // is opened `append(true)` and every write goes to the end regardless.
        file.seek(SeekFrom::Start(offset))?;
        let mut data = Vec::new();
        std::io::Read::by_ref(file)
            .take(end - offset)
            .read_to_end(&mut data)?;

        let mut position = offset;
        let mut start = 0usize;
        while let Some(rel) = data[start..].iter().position(|&b| b == b'\n') {
            let stop = start + rel;
            let text = &data[start..stop];
            start = stop + 1;
            position = offset + start as u64;
            if text.iter().all(|b| b.is_ascii_whitespace()) {
                continue;
            }
            let line = std::str::from_utf8(text).map_err(|e| {
                Error::new(ErrorKind::Json, format!("store: log is not UTF-8: {e}"))
            })?;
            let event = Event::parse_line(line.trim())?;
            f(&event)?;
        }
        Ok(position)
    }

    pub fn last_seq(&self) -> u64 {
        self.lock().seq
    }

    pub fn path(&self) -> PathBuf {
        self.lock().path.clone()
    }

    /// Force the pending batch to disk. Safe to call when nothing is pending.
    pub fn sync(&self) -> Result<()> {
        let mut inner = self.lock();
        let Inner {
            file,
            pending,
            options,
            ..
        } = &mut *inner;
        let file = match file.as_mut() {
            // Closing is a flush point; a later sync has nothing to do and
            // should not turn ordinary teardown into an error.
            None => return Ok(()),
            Some(file) => file,
        };
        if *pending == 0 && !options.no_fsync {
            return Ok(());
        }
        file.sync_data()?;
        *pending = 0;
        Ok(())
    }

    /// Flush and close. Idempotent: a second close is a no-op, so teardown code
    /// may close defensively without carrying a flag.
    pub fn close(&self) -> Result<()> {
        let mut inner = self.lock();
        let Inner {
            file,
            pending,
            options,
            ..
        } = &mut *inner;
        let Some(handle) = file.take() else {
            return Ok(());
        };
        if !options.no_fsync {
            handle.sync_data()?;
        }
        *pending = 0;
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A poisoned mutex means another thread panicked mid-operation. The
        // on-disk format is still the truth, so recover the guard rather than
        // turning every later call into a panic of its own.
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

/// Result of the open-time scan: `(last_seq, byte offset after the last
/// complete record)`. A missing file is `(0, None)` and is created by the
/// caller; a file that exists but has no complete record is `(0, Some(0))`.
fn scan_log(path: &Path) -> Result<(u64, Option<u64>)> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((0, None)),
        Err(e) => return Err(e.into()),
    };
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;

    let mut last_seq = 0u64;
    let mut good = 0u64;
    let mut pos = 0usize;
    while pos < data.len() {
        // A record without a terminating newline is a torn write: the writer
        // died before it could commit the line, so it must not be parsed.
        let Some(relative) = data[pos..].iter().position(|&b| b == b'\n') else {
            break;
        };
        let end = pos + relative + 1;
        let line = &data[pos..end - 1];
        pos = end;

        let Ok(text) = std::str::from_utf8(line) else {
            break;
        };
        let text = text.trim();
        if text.is_empty() {
            // Blank lines are harmless; count them as part of the valid
            // prefix so a file ending in "\n\n" does not look torn.
            good = pos as u64;
            continue;
        }

        let tree: serde_json::Value = match serde_json::from_str(text) {
            Ok(tree) => tree,
            Err(_) => break,
        };
        match tree.get("v").and_then(serde_json::Value::as_u64) {
            Some(v) if v == FORMAT_VERSION as u64 => {}
            Some(v) => {
                return Err(Error::new(
                    ErrorKind::UnsupportedVersion,
                    format!(
                        "store: unsupported event version v{v} in {}; this build reads v{FORMAT_VERSION}",
                        path.display()
                    ),
                ))
            }
            None => {
                return Err(Error::new(
                    ErrorKind::UnsupportedVersion,
                    format!(
                        "store: record without a version in {}; this build reads v{FORMAT_VERSION}",
                        path.display()
                    ),
                ))
            }
        }

        // Cheap envelope check: enough to know whether this line is a
        // committed event before trusting its seq. Payload members are
        // deliberately not inspected here.
        let seq = tree
            .get("seq")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let id = tree
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let session = tree
            .get("session")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let typ = tree
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let ts = tree
            .get("ts")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        if seq == 0 || id.is_empty() || session.is_empty() || typ.is_empty() || ts <= 0 {
            break;
        }

        last_seq = seq;
        good = pos as u64;
    }
    Ok((last_seq, Some(good)))
}

pub(crate) fn now_millis() -> Result<i64> {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|e| {
        Error::new(
            ErrorKind::Io,
            format!("store: system clock is before the Unix epoch: {e}"),
        )
    })?;
    i64::try_from(elapsed.as_millis())
        .map_err(|_| Error::new(ErrorKind::Io, "store: system clock is out of range"))
}
