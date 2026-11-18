//! The mailbox's contract: delivery before the reader exists, a torn tail that
//! does not poison the file, and two writers that do not corrupt each other.
//!
//! These are file-level facts, so the tests read the bytes rather than trusting
//! the API's own summary of them.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use forge_core::ErrorKind;
use forge_team::{Mailbox, Message};

fn temp_dir(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    // The nanosecond stamp matters: a process id is recycled by the OS, so a
    // name built from it alone can land on an inbox an earlier run left behind —
    // and then a "first drain" test sees mail it never sent.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "forge-team-mailbox-{}-{}-{}-{}",
        std::process::id(),
        stamp,
        NEXT.fetch_add(1, Ordering::SeqCst),
        label
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn mailbox(label: &str) -> Mailbox {
    Mailbox::open(temp_dir(label)).expect("open mailbox")
}

#[test]
fn a_message_sent_before_the_worker_starts_is_delivered_on_its_first_drain() {
    let mailbox = mailbox("before-start");
    // Nothing named "alpha" exists anywhere: no thread, no file, no cursor.
    mailbox
        .send("lead", "alpha", "start with the board")
        .expect("send");
    mailbox
        .send("lead", "alpha", "then the mailbox")
        .expect("send");

    let delivered = mailbox.drain("alpha").expect("drain");
    assert_eq!(delivered.len(), 2);
    assert_eq!(delivered[0].text, "start with the board");
    assert_eq!(delivered[1].text, "then the mailbox");
    assert_eq!(delivered[0].from, "lead");
    assert_eq!(delivered[0].to, "alpha");
}

#[test]
fn a_second_drain_does_not_re_deliver() {
    let mailbox = mailbox("once");
    mailbox.send("lead", "alpha", "only once").expect("send");
    assert_eq!(mailbox.drain("alpha").expect("drain").len(), 1);
    assert!(mailbox.drain("alpha").expect("drain").is_empty());
}

#[test]
fn a_drain_from_an_old_cursor_replays() {
    let mailbox = mailbox("replay");
    mailbox.send("lead", "alpha", "one").expect("send");
    let first = mailbox.drain_from("alpha", 0).expect("drain");
    assert_eq!(first.messages.len(), 1);
    assert!(first.cursor > 0);
    // A worker that died before using the message re-reads from the old cursor.
    let again = mailbox.drain_from("alpha", 0).expect("drain again");
    assert_eq!(again.messages.len(), 1);
    let after = mailbox
        .drain_from("alpha", first.cursor)
        .expect("drain after");
    assert!(after.messages.is_empty());
}

#[test]
fn a_torn_last_line_is_skipped_and_does_not_hide_the_whole_lines() {
    let mailbox = mailbox("torn");
    mailbox.send("lead", "alpha", "committed").expect("send");

    // Simulate a writer that died mid-line: no terminating newline. The payload
    // is a complete message (including `ts`) up to the cut, so completing it
    // below yields something the crate actually accepts.
    let path = mailbox.path("alpha");
    let mut text = std::fs::read_to_string(&path).expect("read inbox");
    text.push_str("{\"from\":\"lead\",\"to\":\"alpha\",\"ts\":0,\"text\":\"half-writ");
    std::fs::write(&path, &text).expect("write torn inbox");

    let drained = mailbox.drain_from("alpha", 0).expect("drain");
    assert_eq!(drained.messages.len(), 1, "only the committed line arrives");
    assert_eq!(drained.messages[0].text, "committed");
    assert!(drained.torn_tail, "the torn tail must be reported");
    assert_eq!(drained.skipped, 0, "a torn tail is not a corrupt record");

    // Completing the line makes it readable on the next drain from that cursor.
    let mut fixed = text;
    fixed.push_str("ten\"}\n");
    std::fs::write(&path, &fixed).expect("complete the line");
    let drained = mailbox.drain_from("alpha", drained.cursor).expect("drain");
    assert_eq!(drained.messages.len(), 1);
    assert_eq!(drained.messages[0].text, "half-written");
    assert!(!drained.torn_tail);
}

#[test]
fn an_empty_file_is_an_empty_drain_not_an_error() {
    let mailbox = mailbox("empty");
    std::fs::create_dir_all(mailbox.root().join("inbox")).expect("inbox dir");
    std::fs::write(mailbox.path("alpha"), b"").expect("empty inbox");
    let drained = mailbox.drain_from("alpha", 0).expect("drain");
    assert!(drained.messages.is_empty());
    assert_eq!(drained.cursor, 0);
    assert!(!drained.torn_tail);
}

#[test]
fn a_missing_inbox_is_an_empty_drain() {
    let mailbox = mailbox("missing");
    let drained = mailbox.drain_from("nobody", 0).expect("drain");
    assert!(drained.messages.is_empty());
    assert!(!drained.torn_tail);
}

#[test]
fn a_corrupt_committed_line_is_counted_and_the_rest_still_arrives() {
    let mailbox = mailbox("corrupt");
    let path = mailbox.path("alpha");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("inbox dir");
    std::fs::write(&path, b"{\"not\":\"a message\"}\nnot json at all\n").expect("write inbox");
    mailbox
        .send("lead", "alpha", "after the garbage")
        .expect("send");

    let drained = mailbox.drain_from("alpha", 0).expect("drain");
    assert_eq!(drained.skipped, 2, "two committed lines were unreadable");
    assert_eq!(drained.messages.len(), 1, "the good line still arrives");
    assert_eq!(drained.messages[0].text, "after the garbage");
    assert!(!drained.torn_tail);
}

#[test]
fn two_writers_to_different_inboxes_do_not_corrupt_each_other() {
    let mailbox = mailbox("two-writers");
    let first = mailbox.clone();
    let second = mailbox.clone();
    let a = std::thread::spawn(move || {
        for index in 0..50 {
            first
                .send("lead", "alpha", &format!("alpha {index}"))
                .expect("send to alpha");
        }
    });
    let b = std::thread::spawn(move || {
        for index in 0..50 {
            second
                .send("lead", "beta", &format!("beta {index}"))
                .expect("send to beta");
        }
    });
    a.join().expect("join alpha writer");
    b.join().expect("join beta writer");

    let alpha: Vec<Message> = mailbox.drain_from("alpha", 0).expect("drain").messages;
    let beta: Vec<Message> = mailbox.drain_from("beta", 0).expect("drain").messages;
    assert_eq!(alpha.len(), 50);
    assert_eq!(beta.len(), 50);
    for (index, message) in alpha.iter().enumerate() {
        assert_eq!(message.text, format!("alpha {index}"), "order and content");
    }
    for (index, message) in beta.iter().enumerate() {
        assert_eq!(message.text, format!("beta {index}"), "order and content");
    }
}

#[test]
fn concurrent_writers_to_one_inbox_produce_whole_lines() {
    let mailbox = mailbox("one-inbox");
    let mut handles = Vec::new();
    for writer in 0..4 {
        let mailbox = mailbox.clone();
        handles.push(std::thread::spawn(move || {
            for index in 0..25 {
                mailbox
                    .send("lead", "alpha", &format!("w{writer}-{index}"))
                    .expect("send");
            }
        }));
    }
    for handle in handles {
        handle.join().expect("join writer");
    }
    let drained = mailbox.drain_from("alpha", 0).expect("drain");
    assert_eq!(drained.messages.len(), 100, "no line lost, none split");
    assert_eq!(drained.skipped, 0);
    assert!(!drained.torn_tail);
}

#[test]
fn an_empty_message_is_refused() {
    let mailbox = mailbox("empty-message");
    assert_eq!(
        mailbox
            .send("lead", "alpha", "   ")
            .expect_err("empty text")
            .kind(),
        ErrorKind::Invalid
    );
}

#[test]
fn an_unusable_recipient_name_is_refused() {
    let mailbox = mailbox("bad-name");
    for bad in ["", "../escape", "a/b", "con"] {
        assert_eq!(
            mailbox
                .send("lead", bad, "hello")
                .expect_err("unusable name")
                .kind(),
            ErrorKind::Invalid,
            "recipient {bad:?}"
        );
    }
}
