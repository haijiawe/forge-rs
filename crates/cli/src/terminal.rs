//! The browser terminal's process runner.
//!
//! Layer L3. `hearth-server` owns the socket, the frames and the two log lines;
//! this module owns the process, because starting one needs `hearth-exec` and a
//! sandbox, and the HTTP layer must not be able to do that on its own.
//!
//! Two decisions live here rather than in the server:
//!
//! - **No approval per command.** A person at the keyboard is who the approval
//!   prompt protects; asking them to approve their own keystroke is theatre. The
//!   line is still recorded (`terminal/run` / `terminal/end`, written by the
//!   server), and the sandbox is installed on the child, so the terminal is
//!   strictly less capable than the `shell` tool it sits beside.
//! - **No deadline.** A build takes as long as it takes and the person watching
//!   decides when to stop it; the cancel frame is the only way a run ends early,
//!   and a socket that closes cancels too.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use hearth_core::Result;
use hearth_exec::{Command, Pipe, Stream as ExecStream};
use hearth_server::api::{TerminalOutcome, TerminalRequest, TerminalRunner, TerminalSink};

/// How much of each stream `terminal/end` keeps.
///
/// The live socket carries the whole stream; this is what survives for a reader
/// who arrives after the fact, and 4 KiB is about what a scrollback line-up of
/// a build failure needs. The ADR's original 8 KiB was cut because two tails ride
/// in one event and the log is append-only.
const TAIL_BYTES: usize = 4 << 10;

/// How often the cancel token is checked. Ten milliseconds matches exec's own
/// wait loop: any slower and a `Ctrl+C` on a fast command loses the race.
const CANCEL_POLL: Duration = Duration::from_millis(10);

/// Runs terminal commands for one session.
pub struct Terminal {
    enforcer: Arc<dyn hearth_sandbox::Enforcer>,
}

impl Terminal {
    pub fn new(enforcer: Arc<dyn hearth_sandbox::Enforcer>) -> Self {
        Self { enforcer }
    }
}

impl TerminalRunner for Terminal {
    fn run(
        &self,
        request: &TerminalRequest,
        sink: Arc<dyn TerminalSink>,
    ) -> Result<TerminalOutcome> {
        let started = Instant::now();
        let line = request.command.trim().to_string();
        if line.is_empty() {
            return Ok(refused("the command is empty"));
        }
        // The same list the `shell` tool refuses. A person can still run these
        // by hand in their own terminal; what this surface must not do is run
        // them with the workspace's blessing and no second thought.
        if let Some(reason) = hearth_tool::builtin::dangerous(&line) {
            return Ok(refused(&reason));
        }
        let (path, argv, cmd_line) = hearth_tool::builtin::shell_invocation(&line);
        // One decoder per stream: a program can write UTF-8 on stdout and hand
        // stderr to a library that still speaks the console's code page.
        let decoders = Arc::new(Mutex::new([Decoder::new(), Decoder::new()]));
        let observer = {
            let sink = Arc::clone(&sink);
            let decoders = Arc::clone(&decoders);
            Arc::new(move |stream: ExecStream, bytes: &[u8]| {
                let index = if matches!(stream, ExecStream::Err) {
                    1
                } else {
                    0
                };
                let text = match decoders.lock() {
                    Ok(mut decoders) => decoders[index].push(bytes),
                    // A poisoned lock means a panic already ended this run; the
                    // server still reports the exit, and dropping the rest of the
                    // output beats panicking inside the reader thread.
                    Err(_) => return,
                };
                if !text.is_empty() {
                    sink.output(stream.as_str(), &text);
                }
            })
        };
        let mut command = Command::new(path)
            .args(argv)
            .dir(request.cwd.to_string_lossy().to_string())
            .sandbox(crate::exec_sandbox::tool_sandbox(Arc::clone(
                &self.enforcer,
            )))
            .observe(observer);
        if let Some(line) = cmd_line {
            command = command.cmd_line(line);
        }
        // A watcher, because `run_with_cancel` wants a flag and the server
        // hands over a token: the bridge is this thread, not a second mechanism.
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::new(AtomicBool::new(false));
        let watcher = {
            let stop = Arc::clone(&stop);
            let cancelled = Arc::clone(&cancelled);
            let token = request.cancel.clone();
            thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    if token.is_cancelled() {
                        cancelled.store(true, Ordering::Release);
                        return;
                    }
                    thread::sleep(CANCEL_POLL);
                }
            })
        };
        let outcome = Pipe.run_with_cancel_capture(command, &cancelled);
        stop.store(true, Ordering::Release);
        let _ = watcher.join();
        let duration_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
        let was_cancelled = cancelled.load(Ordering::Acquire);
        match outcome {
            Ok(result) => Ok(TerminalOutcome {
                // A cancelled run has no exit status to report; saying -1 and
                // why is more honest than passing through whatever the killed
                // child happened to return.
                exit_code: if result.cancelled {
                    -1
                } else {
                    result.exit_code
                },
                duration_ms,
                cancelled: result.cancelled || was_cancelled,
                stdout_tail: tail(&result.stdout, result.truncated),
                stderr_tail: tail(&result.stderr, result.truncated),
                message: if result.cancelled {
                    "exec: cancelled".to_string()
                } else {
                    String::new()
                },
            }),
            Err(error) => Ok(TerminalOutcome {
                exit_code: -1,
                duration_ms,
                cancelled: was_cancelled,
                message: error.message().to_string(),
                ..TerminalOutcome::default()
            }),
        }
    }
}

/// A command that never started, in the same shape as one that did.
fn refused(reason: &str) -> TerminalOutcome {
    TerminalOutcome {
        exit_code: -1,
        message: reason.to_string(),
        ..TerminalOutcome::default()
    }
}

/// The end of one stream, as text, for the `terminal/end` line.
///
/// Cutting on a byte boundary can split a character, so the cut is moved
/// forward to the next boundary rather than emitting a replacement character at
/// the start of the kept tail: the text is a record, and a record that begins
/// with damage looks like the command did that. The same decoder as the live
/// stream runs over the result, so a GBK tail is readable in the log too.
fn tail(bytes: &[u8], truncated: bool) -> String {
    tail_with_page(bytes, truncated, 0)
}

/// The tail, decoded with a pinned page instead of the console's own.
fn tail_with_page(bytes: &[u8], truncated: bool, code_page: u32) -> String {
    let mut start = bytes.len().saturating_sub(TAIL_BYTES);
    while start < bytes.len() && (bytes[start] & 0xC0) == 0x80 {
        start += 1;
    }
    let mut decoder = Decoder::with_code_page(code_page);
    let mut decoded = decoder.push(&bytes[start..]);
    decoded.extend_from_slice(&decoder.finish());
    let mut text = String::from_utf8_lossy(&decoded).to_string();
    if truncated || start > 0 {
        text.insert_str(0, "(truncated)\n");
    }
    text
}

/// What a stream's bytes have turned out to be.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Only ASCII so far, which is the same byte in every encoding handled here.
    Unknown,
    /// Confirmed UTF-8 by a non-ASCII character that decoded cleanly.
    Utf8,
    /// Not UTF-8: decoded with the console's code page.
    Legacy,
}

/// Turns the bytes a command wrote into the UTF-8 bytes the browser shows.
///
/// stdout is a pipe of bytes, not text. A console program on Chinese Windows
/// writes GBK (ping, ipconfig, dir over a folder with Chinese names), and
/// handing those bytes to the wire as if they were UTF-8 turns every Chinese
/// line into a row of U+FFFD. The runner is the only layer that knows which
/// console it started the child on, so the code page lives here: frames stay
/// plain UTF-8 text and the web client needs no code-page tables.
///
/// ASCII is the same byte in every encoding handled here, so it is passed
/// through before the mode is settled. Otherwise `echo hi` followed by a GBK
/// line would fix the stream to UTF-8 on the first line and lose the second.
struct Decoder {
    mode: Mode,
    /// Bytes held back: half a character, or a byte that may still be the first
    /// half of one.
    pending: Vec<u8>,
    /// Pages to try when the bytes are not UTF-8, best first. Resolved once:
    /// the console's page does not change under a running command.
    pages: Vec<u32>,
}

impl Decoder {
    fn new() -> Self {
        Self::with_code_page(0)
    }

    /// A decoder for one page; 0 asks the console which pages to try.
    fn with_code_page(code_page: u32) -> Self {
        Self {
            mode: Mode::Unknown,
            pending: Vec::new(),
            pages: if code_page == 0 {
                console_code_pages()
            } else {
                vec![code_page]
            },
        }
    }

    /// Convert everything whole in `bytes`, holding a split tail for next time.
    fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.pending.extend_from_slice(bytes);
        match self.mode {
            // Already UTF-8: pass it through untouched. The server already
            // carries a character split across two chunks, and one decoder for
            // that job is enough.
            Mode::Utf8 => return std::mem::take(&mut self.pending),
            Mode::Legacy => return self.decode_legacy_pending(),
            Mode::Unknown => {}
        }
        let mut out = Vec::new();
        let ascii = self
            .pending
            .iter()
            .position(|byte| *byte >= 0x80)
            .unwrap_or(self.pending.len());
        out.extend_from_slice(&self.pending[..ascii]);
        self.pending.drain(..ascii);
        if self.pending.is_empty() {
            return out;
        }
        match std::str::from_utf8(&self.pending) {
            Ok(_) => {
                self.mode = Mode::Utf8;
                out.extend_from_slice(&self.pending);
                self.pending.clear();
            }
            // A character cut in half by the pipe: pass the whole prefix and
            // wait, rather than deciding the encoding on a truncated sequence.
            Err(error) if error.error_len().is_none() => {
                let whole = error.valid_up_to();
                out.extend_from_slice(&self.pending[..whole]);
                self.pending.drain(..whole);
            }
            // Bytes that will never be UTF-8: this stream is the console's
            // code page, from here to the end.
            Err(_) => {
                self.mode = Mode::Legacy;
                out.extend_from_slice(&self.decode_legacy_pending());
            }
        }
        out
    }

    /// Decode what is held back, once the stream has ended.
    ///
    /// A half character at the end is a process that died mid-write; it becomes
    /// a replacement character rather than vanishing, so the reader sees that
    /// something was there.
    fn finish(&mut self) -> Vec<u8> {
        let pending = std::mem::take(&mut self.pending);
        if pending.is_empty() {
            return Vec::new();
        }
        match std::str::from_utf8(&pending) {
            Ok(_) => pending,
            Err(_) => decode_pages(&pending, &self.pages),
        }
    }

    /// Decode the pending bytes with the console's code page.
    ///
    /// In a double-byte page the last byte can be the first half of a character
    /// whose second half has not been read yet, so it is held back.
    fn decode_legacy_pending(&mut self) -> Vec<u8> {
        let hold = match (self.pages.first(), self.pending.last()) {
            (Some(page), Some(last)) if is_lead_byte(*page, *last) => 1,
            _ => 0,
        };
        let split = self.pending.len() - hold;
        if split == 0 {
            return Vec::new();
        }
        let whole: Vec<u8> = self.pending.drain(..split).collect();
        decode_pages(&whole, &self.pages)
    }
}

/// The code pages a child's output could be in, best first.
///
/// A console program writes with the console's output code page and a C runtime
/// program with the ANSI one; on a Chinese Windows both are 936, so the question
/// is settled. They part company where the console was switched to UTF-8
/// (`chcp 65001`) or where the "beta: use UTF-8" option moved the ANSI page: the
/// console then claims 65001 while the program still wrote GBK, which is the
/// state that made the first version of this decoder print replacement
/// characters. So UTF-8 is not a candidate here — it already failed — and the
/// double-byte pages are tried before single-byte ones, because a single-byte
/// page decodes any sequence of bytes without complaint.
#[cfg(windows)]
fn console_code_pages() -> Vec<u32> {
    use windows_sys::Win32::Globalization::{GetACP, GetOEMCP};
    use windows_sys::Win32::System::Console::GetConsoleOutputCP;

    // SAFETY: three argument-less kernel32 queries; nothing is borrowed, nothing
    // owned by the caller is touched, and none of them can fail.
    #[allow(unsafe_code)]
    let (console, ansi, oem) = unsafe { (GetConsoleOutputCP(), GetACP(), GetOEMCP()) };

    let mut pages: Vec<u32> = Vec::new();
    for page in [console, ansi, oem] {
        if page != 0 && page != 65001 && !pages.contains(&page) {
            pages.push(page);
        }
    }
    let (double, single): (Vec<u32>, Vec<u32>) = pages.into_iter().partition(|page| is_dbcs(*page));
    double.into_iter().chain(single).collect()
}

#[cfg(not(windows))]
fn console_code_pages() -> Vec<u32> {
    Vec::new()
}

/// Whether `page` is a double-byte page (Chinese, Japanese, Korean).
#[cfg(windows)]
fn is_dbcs(page: u32) -> bool {
    use windows_sys::Win32::Globalization::IsDBCSLeadByteEx;

    // SAFETY: a table lookup on two integers; it reads nothing the caller owns.
    #[allow(unsafe_code)]
    unsafe {
        IsDBCSLeadByteEx(page, 0xA1) != 0
    }
}

/// Whether `byte` can start a two-byte character in `page`.
#[cfg(windows)]
fn is_lead_byte(page: u32, byte: u8) -> bool {
    use windows_sys::Win32::Globalization::IsDBCSLeadByteEx;

    // SAFETY: a table lookup on two integers; it reads nothing the caller owns.
    #[allow(unsafe_code)]
    unsafe {
        IsDBCSLeadByteEx(page, byte) != 0
    }
}

#[cfg(not(windows))]
fn is_lead_byte(_page: u32, _byte: u8) -> bool {
    false
}

/// Decode with the first page that yields text without damage.
///
/// A page that produces U+FFFD did not write this stream, so the next candidate
/// gets a turn — that is what saves the `chcp 65001` console whose child still
/// wrote GBK. A single-byte page is never refuted this way (it has a letter for
/// every byte), so the order of `pages` carries the rest; see
/// [`console_code_pages`]. Damage still beats silence, so the first attempt is
/// kept as the answer when every page fails.
#[cfg(windows)]
fn decode_pages(bytes: &[u8], pages: &[u32]) -> Vec<u8> {
    let mut first: Option<Vec<u8>> = None;
    for page in pages {
        let text = decode_code_page(bytes, *page);
        if !text.is_empty() && !text.windows(3).any(|window| window == b"\xEF\xBF\xBD") {
            return text;
        }
        if first.is_none() {
            first = Some(text);
        }
    }
    first.unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned().into_bytes())
}

#[cfg(not(windows))]
fn decode_pages(bytes: &[u8], _pages: &[u32]) -> Vec<u8> {
    String::from_utf8_lossy(bytes).into_owned().into_bytes()
}

/// Decode bytes written in one `page` into UTF-8.
///
/// A page of 0 and every failed conversion fall back to lossy UTF-8: damaged
/// text still beats a stream that stops.
#[cfg(windows)]
fn decode_code_page(bytes: &[u8], page: u32) -> Vec<u8> {
    use windows_sys::Win32::Globalization::MultiByteToWideChar;

    if bytes.is_empty() {
        return Vec::new();
    }
    let Ok(len) = i32::try_from(bytes.len()) else {
        return String::from_utf8_lossy(bytes).into_owned().into_bytes();
    };
    // SAFETY: the source pointer is valid for `len` bytes for both calls; the
    // first call passes a null destination and only asks how much room is
    // needed; the second writes into a buffer sized by exactly that answer, and
    // `written` bounds what is handed to from_utf16_lossy.
    #[allow(unsafe_code)]
    let needed =
        unsafe { MultiByteToWideChar(page, 0, bytes.as_ptr(), len, std::ptr::null_mut(), 0) };
    if needed <= 0 {
        return String::from_utf8_lossy(bytes).into_owned().into_bytes();
    }
    let mut wide = vec![0u16; needed as usize];
    #[allow(unsafe_code)]
    let written =
        unsafe { MultiByteToWideChar(page, 0, bytes.as_ptr(), len, wide.as_mut_ptr(), needed) };
    if written <= 0 {
        return String::from_utf8_lossy(bytes).into_owned().into_bytes();
    }
    wide.truncate(written as usize);
    String::from_utf16_lossy(&wide).into_bytes()
}

#[cfg(not(windows))]
fn decode_code_page(bytes: &[u8], _page: u32) -> Vec<u8> {
    String::from_utf8_lossy(bytes).into_owned().into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }

    #[test]
    fn ascii_before_a_non_ascii_line_does_not_settle_the_encoding() {
        // The first line of a GBK program is usually ASCII. If that fixed the
        // stream to UTF-8, the line after it would arrive as replacement
        // characters, which is exactly the bug this decoder exists for.
        let mut decoder = Decoder::new();
        assert_eq!(text(&decoder.push(b"stdout\n")), "stdout\n");
        let bytes = "世界".as_bytes();
        assert_eq!(decoder.push(&bytes[..2]), Vec::<u8>::new());
        assert_eq!(text(&decoder.push(&bytes[2..])), "世界");
    }

    #[test]
    fn a_character_split_across_writes_is_carried() {
        let mut decoder = Decoder::new();
        assert_eq!(decoder.push(b"caf\xc3"), b"caf");
        assert_eq!(text(&decoder.push(b"\xa9 au lait")), "é au lait");
    }

    #[test]
    fn a_utf8_stream_keeps_passing_through() {
        let mut decoder = Decoder::new();
        let mut out = decoder.push("héllo ".as_bytes());
        out.extend(decoder.push("世界\n".as_bytes()));
        assert_eq!(text(&out), "héllo 世界\n");
    }

    #[cfg(not(windows))]
    #[test]
    fn bytes_that_are_never_utf8_become_replacement_characters() {
        let mut decoder = Decoder::new();
        let mut out = decoder.push(b"\xc4\xe3\xba\xc3\n");
        out.extend(decoder.finish());
        assert!(text(&out).contains('\u{FFFD}'), "{}", text(&out));
        assert!(text(&out).ends_with('\n'));
    }

    // A Chinese console writes GBK, and 936 is the page it writes with whether
    // or not this machine is set to it: Windows ships the table either way.
    #[cfg(windows)]
    #[test]
    fn gbk_from_a_chinese_console_comes_out_as_chinese() {
        let mut decoder = Decoder::with_code_page(936);
        let out = decoder.push(b"\xc4\xe3\xba\xc3\n");
        assert_eq!(text(&out), "你好\n");
    }

    #[cfg(windows)]
    #[test]
    fn a_gbk_character_split_across_writes_is_not_torn() {
        // C4 E3 is 你, BA C3 is 好; the pipe can hand over the break between
        // them, and a lead byte whose trail has not arrived must not become a
        // replacement character.
        let mut decoder = Decoder::with_code_page(936);
        assert_eq!(text(&decoder.push(b"\xc4\xe3\xba")), "你");
        assert_eq!(text(&decoder.push(b"\xc3\n")), "好\n");
    }

    #[cfg(windows)]
    #[test]
    fn a_page_that_cannot_decode_the_bytes_gives_way_to_the_next() {
        // This is the state the first version got wrong: the console says UTF-8
        // (chcp 65001) but the program wrote GBK, so 65001 has to be dropped and
        // the answer comes from the pages behind it.
        assert_eq!(
            decode_pages(b"\xc4\xe3\xba\xc3", &[65001, 936]),
            "你好".as_bytes()
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_single_byte_page_cannot_be_refuted_so_the_order_decides() {
        // Windows-1252 has a letter for every byte and 936 agrees with these
        // bytes, so both decode "cleanly" and nothing inside the bytes can
        // referee. That is why console_code_pages puts the double-byte pages
        // first: the order is the only honest lever there is.
        assert_eq!(
            text(&decode_pages(b"\xc4\xe3\xba\xc3", &[936, 1252])),
            "你好"
        );
    }

    #[cfg(windows)]
    #[test]
    fn the_logged_tail_is_decoded_with_the_same_page() {
        // Pinned to 936: the process's own console page is whatever it happens
        // to be, and this test is about the tail path, not about the host.
        assert_eq!(tail_with_page(b"\xc4\xe3\xba\xc3\n", false, 936), "你好\n");
    }

    #[test]
    fn an_ascii_tail_is_unchanged() {
        assert_eq!(tail(b"done\n", false), "done\n");
    }
}
