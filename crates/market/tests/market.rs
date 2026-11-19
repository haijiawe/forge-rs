//! End-to-end market tests over a real loopback HTTP server.
//!
//! The unit tests check each layer alone: an index in memory, a tar built by the
//! test writer, a download from a canned `Response`. These check the thing the
//! acceptance sentence is about — *a market install verifies a checksum and
//! refuses a tampered tarball* — as one run: index fetched over HTTP, resolved by
//! name, tarball fetched and digested, unpacked, recorded, and undone. A mock
//! response would prove none of the wire behaviour, so the server is a real
//! `TcpListener` on 127.0.0.1, the same shape `crates/net/tests/net_test.rs` uses.
//!
//! The tarball is the embedded real one: a `tar -czf` produced on this machine,
//! whose sha256 is pinned below. Both of its forms are used — the gzip member as
//! served, and the same bytes gunzipped for the plain-`.tar` case — so the two
//! decompression paths are exercised against bytes no test writer made up.

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

use forge_market::{
    download, gunzip, install, is_sha256_hex, list, sha256_hex, uninstall, Bundle, Index, Store,
    ERR_CHECKSUM_MISMATCH, ERR_EXTRA_FILES, ERR_FILE_MODIFIED, ERR_TRUNCATED,
};
use forge_net::{Client, ClientOptions};

/// The real `tar -czf` blob, produced by `tar.exe` on this machine.
const TARBALL_GZ: &[u8] = include_bytes!("fixtures/fixture.tar.gz");

/// Its digest, measured with `Get-FileHash` when it was made and pinned here so a
/// re-generated fixture cannot silently change what these tests prove.
const TARBALL_GZ_SHA256: &str = "fc52df773ef9be8b2ad0bacf6a4908324117cbc64001f91637a2eb4224957683";

/// The first entry of that blob, byte for byte, so "it unpacked" is a claim about
/// content and not only about a file count.
const PLUGIN_JSON: &str = "{\n  \"name\": \"fixture-plugin\",\n  \"version\": \"0.3.1\",\n  \"description\": \"a fixture plugin for the forge market tests\",\n  \"runtime\": \"node\",\n  \"entry\": \"index.js\"\n}";

/// Its digest, pinned separately: the file's *content* is what an install
/// records, and a test that only compared a count would pass on a tarball that
/// unpacked the wrong bytes.
const PLUGIN_JSON_SHA256: &str = "9f3cadf08c99a994a279cdb26656aeb0382938ab1bec13f7b7a7acea1efa91a3";

/// How many *files* the fixture holds. It lists six entries, two of which are
/// directories, and a directory is not a file the record keeps.
const FIXTURE_FILE_COUNT: usize = 5;

/// The long name the pax extended header carries: 122 characters, which is why
/// the fixture needs a pax entry at all. Written as a length plus a suffix so a
/// miscount here shows up as a failed assertion rather than a typo that happens
/// to be the same length.
fn long_name() -> String {
    format!("{}.txt", "l".repeat(118))
}

// ---- the fixture server ---------------------------------------------------

/// Serve `responses` connections, answering each with `handler`'s bytes.
///
/// Returns the base URL and a counter of connections actually served, so a test
/// can assert that a refusal happened *without* a second request — that is the
/// difference between refusing early and refusing after doing the work.
fn serve<F>(responses: usize, handler: F) -> (String, Arc<AtomicUsize>)
where
    F: Fn(usize, &str) -> Vec<u8> + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("addr");
    let served = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&served);
    let handler = Arc::new(handler);
    thread::spawn(move || {
        for index in 0..responses {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            counter.fetch_add(1, Ordering::SeqCst);
            let head = read_head(&mut stream);
            let bytes = handler(index, &head);
            // The client may already be gone; that ends one connection, not the
            // test.
            let _ = stream.write_all(&bytes);
            let _ = stream.flush();
        }
    });
    (format!("http://{address}"), served)
}

fn read_head(stream: &mut TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                head.push(byte[0]);
                if head.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&head).into_owned()
}

/// A response with a `Content-Length` that matches the body.
fn ok(body: &[u8], content_type: &str) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

/// A response whose `Content-Length` deliberately disagrees with the body, which
/// is how a proxy that mangles a download looks on the wire.
fn ok_with_length(body: &[u8], declared: usize, content_type: &str) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {declared}\r\n\r\n"
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

// ---- the index document ---------------------------------------------------

/// The index a server would publish, with the tarball URL pointing at `base`.
fn index_document(base: &str, name: &str, version: &str, bytes: &[u8]) -> String {
    format!(
        concat!(
            r#"{{"version":1,"bundles":[{{"name":"{name}","version":"{version}","#,
            r#""description":"a fixture plugin","author":"nobody","#,
            r#""keywords":["fixture"],"homepage":"https://example.invalid/","#,
            r#""tarball":"{base}/tarballs/{name}-{version}.tar.gz","#,
            r#""sha256":"{sha}","size":{size},"#,
            r#""engines":{{"forge":">=0.1.0","runtime":"node"}}}}]}}"#
        ),
        name = name,
        version = version,
        base = base,
        sha = sha256_hex(bytes),
        size = bytes.len(),
    )
}

/// A bundle record matching what `index_document` publishes — used by the tests
/// that skip the network and go straight to the install.
fn bundle_for(base: &str, name: &str, version: &str, bytes: &[u8]) -> Bundle {
    Bundle {
        name: name.to_string(),
        version: version.to_string(),
        description: "a fixture plugin".to_string(),
        author: Some("nobody".to_string()),
        keywords: Some(vec!["fixture".to_string()]),
        homepage: Some("https://example.invalid/".to_string()),
        tarball: format!("{base}/tarballs/{name}-{version}.tar.gz"),
        sha256: sha256_hex(bytes),
        size: bytes.len() as u64,
        engines: Some(forge_market::Engines {
            forge: Some(">=0.1.0".to_string()),
            runtime: Some("node".to_string()),
        }),
        // The fixture is a source plugin: one tarball, every platform.
        os: None,
        arch: None,
    }
}

fn client() -> Client {
    // Default options: 20s timeout, 2 MiB body cap. A fixture tarball is 1 KiB,
    // so the cap is never the reason a test passes or fails.
    Client::new(ClientOptions::default()).expect("client")
}

fn scratch(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("forge-market-it-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create scratch");
    path
}

/// Every file under `root` with its digest. A claim that a failure changed
/// nothing has to be about content, not only about names.
fn snapshot(root: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if !root.exists() {
        return out;
    }
    fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, String)>) {
        let mut entries: Vec<_> = fs::read_dir(dir)
            .expect("read dir")
            .map(|entry| entry.expect("entry"))
            .collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let relative = path
                .strip_prefix(base)
                .expect("under base")
                .to_string_lossy()
                .replace('\\', "/");
            if path.is_dir() {
                out.push((format!("{relative}/"), String::new()));
                walk(&path, base, out);
            } else {
                out.push((relative, sha256_hex(&fs::read(&path).expect("read"))));
            }
        }
    }
    walk(root, root, &mut out);
    out
}

// ---- the tests ------------------------------------------------------------

/// The acceptance sentence, end to end: a market install fetches the index over
/// HTTP, resolves a name, downloads the tarball, proves its digest, unpacks it,
/// records it, and can undo it.
#[test]
fn a_market_install_verifies_a_checksum_and_installs() {
    assert_eq!(
        sha256_hex(TARBALL_GZ),
        TARBALL_GZ_SHA256,
        "the embedded fixture is not the blob the digest below was pinned from"
    );
    assert!(is_sha256_hex(TARBALL_GZ_SHA256));

    let (base, served) = serve(2, |index, head| match index {
        0 => {
            assert!(head.starts_with("GET /index.json HTTP/1.1"), "got {head:?}");
            // The handler cannot see `base`, so the URL is filled in by the
            // second pass below; here it only answers the index request.
            assert!(head.contains("Host: 127.0.0.1:"), "got {head:?}");
            ok(
                index_document(&base_for(head), "fixture", "0.3.1", TARBALL_GZ).as_bytes(),
                "application/json",
            )
        }
        _ => {
            assert!(
                head.starts_with("GET /tarballs/fixture-0.3.1.tar.gz HTTP/1.1"),
                "got {head:?}"
            );
            ok(TARBALL_GZ, "application/gzip")
        }
    });

    let fetched = client().get(&format!("{base}/index.json")).expect("index");
    assert_eq!(fetched.status, 200);
    let index = Index::parse(&fetched.body).expect("the published index parses");
    let bundle = index
        .resolve("fixture", "0.3.1")
        .expect("resolve by exact version");
    assert_eq!(bundle.size, TARBALL_GZ.len() as u64);

    let root = scratch("install");
    let download_dir = root.join("downloads");
    let path = download(&client(), bundle, &download_dir).expect("download and verify");
    assert_eq!(
        sha256_hex(&fs::read(&path).expect("read")),
        TARBALL_GZ_SHA256
    );

    let store = Store::new(&root);
    let installed = install(&store, bundle, &path).expect("install");
    assert_eq!(installed.name, "fixture");
    assert_eq!(installed.version, "0.3.1");
    // Five files, two of which the fixture's tar lists as directories.
    assert_eq!(installed.files.len(), FIXTURE_FILE_COUNT);
    assert!(installed.files.contains(&"plugin.json".to_string()));
    assert!(installed.files.contains(&long_name()));

    let bundle_dir = store.bundle_dir("fixture");
    assert_eq!(
        fs::read_to_string(bundle_dir.join("plugin.json")).expect("read"),
        PLUGIN_JSON
    );
    // The digest of what landed is the digest pinned from the real blob: the
    // bytes that were extracted are the bytes that were published.
    assert_eq!(
        sha256_hex(&fs::read(bundle_dir.join("plugin.json")).expect("read")),
        PLUGIN_JSON_SHA256
    );
    // The pax long name landed as one file name, not as a `PaxHeader` file and
    // not truncated at 100 characters.
    assert!(
        bundle_dir.join(long_name()).exists(),
        "the pax name was lost"
    );

    let records = list(&store).expect("list");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].sha256, TARBALL_GZ_SHA256);
    assert!(records[0].installed_at.ends_with('Z'));

    let removed = uninstall(&store, "fixture").expect("uninstall");
    assert_eq!(removed.files.len(), FIXTURE_FILE_COUNT);
    assert!(!bundle_dir.exists());
    assert!(list(&store).expect("list").is_empty());
    assert_eq!(served.load(Ordering::SeqCst), 2);
    let _ = fs::remove_dir_all(&root);
}

/// Derive the `base` URL the handler should publish from the request's Host
/// header. A closure cannot capture `base` because `base` is what `serve`
/// returns; the Host header carries the same authority.
fn base_for(head: &str) -> String {
    for line in head.lines() {
        if let Some(value) = line.strip_prefix("Host: ") {
            return format!("http://{}", value.trim());
        }
    }
    panic!("the request has no Host header: {head:?}");
}

/// **The acceptance criterion.** One byte of the tarball is flipped in flight;
/// the install must refuse it, name both digests, delete the unverified file, and
/// leave the root untouched.
#[test]
fn a_tampered_tarball_is_refused_over_the_wire() {
    let mut tampered = TARBALL_GZ.to_vec();
    let position = tampered.len() - 8 - 1;
    tampered[position] ^= 0x01;
    let tampered_sha = sha256_hex(&tampered);
    assert_ne!(
        tampered_sha, TARBALL_GZ_SHA256,
        "the flip must change bytes"
    );

    let served_body = tampered.clone();
    let (base, _served) = serve(1, move |_, _| ok(&served_body, "application/gzip"));
    let root = scratch("tampered");
    let store = Store::new(&root);
    // The index describes the *original*: the same size (a byte was changed, not
    // added), the digest the publisher signed.
    let bundle = bundle_for(&base, "fixture", "0.3.1", TARBALL_GZ);

    let download_dir = root.join("downloads");
    let err = download(&client(), &bundle, &download_dir).unwrap_err();
    let text = err.to_string();
    assert!(text.starts_with(ERR_CHECKSUM_MISMATCH), "{text}");
    // Both digests are in the message: the operator has to be able to see which
    // side is which without re-downloading.
    assert!(text.contains(TARBALL_GZ_SHA256), "{text}");
    assert!(text.contains(&tampered_sha), "{text}");
    // The unverified file does not survive: a tarball in a directory is one
    // `install` away from being trusted.
    let left: Vec<_> = fs::read_dir(&download_dir)
        .map(|entries| {
            entries
                .map(|entry| entry.expect("entry").file_name())
                .collect()
        })
        .unwrap_or_default();
    assert!(left.is_empty(), "a refused download left {left:?}");

    // And an install of a file that never passed verification is refused too,
    // whether or not something wrote it there.
    let root_before = snapshot(&root);
    fs::create_dir_all(&download_dir).expect("mkdir");
    let bad = download_dir.join("fixture-0.3.1.tar.gz");
    fs::write(&bad, &tampered).expect("write the tampered bytes");
    let before_with_bad = snapshot(&root);
    assert_ne!(root_before, before_with_bad);
    let err = install(&store, &bundle, &bad).unwrap_err();
    assert!(err.to_string().starts_with(ERR_CHECKSUM_MISMATCH), "{err}");
    assert_eq!(
        snapshot(&root),
        before_with_bad,
        "a refused install changed the root"
    );
    assert!(
        fs::read_dir(store.staging_dir())
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(true),
        "a refused install left staging files"
    );
    assert!(list(&store).expect("list").is_empty());
    let _ = fs::remove_dir_all(&root);
}

/// A body longer than the index declares is refused, not truncated-to-fit: a
/// bundle that grew is a different bundle.
#[test]
fn a_body_longer_than_declared_is_refused() {
    let mut longer = TARBALL_GZ.to_vec();
    longer.extend_from_slice(b"extra bytes the index never declared");
    let served_body = longer.clone();
    let (base, _served) = serve(1, move |_, _| ok(&served_body, "application/gzip"));
    let bundle = bundle_for(&base, "fixture", "0.3.1", TARBALL_GZ);

    let root = scratch("too-long");
    let err = download(&client(), &bundle, &root.join("downloads")).unwrap_err();
    let text = err.to_string();
    assert!(text.starts_with(forge_market::ERR_BODY_TOO_LARGE), "{text}");
    // Both numbers are in the message, so the operator sees which side lied.
    assert!(text.contains(&TARBALL_GZ.len().to_string()), "{text}");
    assert!(text.contains(&longer.len().to_string()), "{text}");
    let _ = fs::remove_dir_all(&root);
}

/// A short body is `truncated`, with both sizes, and leaves nothing behind.
#[test]
fn a_short_body_is_refused_as_truncated() {
    let mut short = TARBALL_GZ.to_vec();
    short.truncate(600);
    let short_len = short.len();
    let (base, _served) = serve(1, move |_, _| ok(&short, "application/gzip"));
    let bundle = bundle_for(&base, "fixture", "0.3.1", TARBALL_GZ);

    let root = scratch("short");
    let download_dir = root.join("downloads");
    let err = download(&client(), &bundle, &download_dir).unwrap_err();
    let text = err.to_string();
    assert!(text.starts_with(ERR_TRUNCATED), "{text}");
    assert!(text.contains(&short_len.to_string()), "{text}");
    assert!(text.contains(&TARBALL_GZ.len().to_string()), "{text}");
    assert!(
        fs::read_dir(&download_dir)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(true),
        "a truncated download left a file"
    );
    let _ = fs::remove_dir_all(&root);
}

/// A `Content-Length` that contradicts the body is caught before the digest, and
/// reported as truncation because that is what it is.
#[test]
fn a_content_length_that_contradicts_the_body_is_refused() {
    let body = TARBALL_GZ.to_vec();
    let declared = body.len() + 32;
    let (base, _served) = serve(1, move |_, _| {
        ok_with_length(&body, declared, "application/gzip")
    });
    let bundle = bundle_for(&base, "fixture", "0.3.1", TARBALL_GZ);
    let root = scratch("content-length");
    let err = download(&client(), &bundle, &root.join("downloads")).unwrap_err();
    assert!(err.to_string().starts_with(ERR_TRUNCATED), "{err}");
    let _ = fs::remove_dir_all(&root);
}

/// A 404 is data, not a transport failure: the report names the status.
#[test]
fn a_failed_tarball_request_names_its_status() {
    let (base, _served) = serve(1, |_, _| {
        b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\n\r\nnot here!".to_vec()
    });
    let bundle = bundle_for(&base, "fixture", "0.3.1", TARBALL_GZ);
    let root = scratch("status");
    let err = download(&client(), &bundle, &root.join("downloads")).unwrap_err();
    let text = err.to_string();
    assert!(text.starts_with(forge_market::ERR_HTTP_STATUS), "{text}");
    assert!(text.contains("404"), "{text}");
    let _ = fs::remove_dir_all(&root);
}

/// The same bundle in a plain `.tar` installs identically: the reader must not
/// care whether the publisher compressed it.
#[test]
fn a_plain_tar_installs_the_same_files() {
    let tar = gunzip(TARBALL_GZ).expect("the fixture is a gzip member");
    let served_body = tar.clone();
    let (base, _served) = serve(1, move |_, _| ok(&served_body, "application/x-tar"));
    let bundle = bundle_for(&base, "fixture", "0.3.1", &tar);
    let root = scratch("plain-tar");
    let store = Store::new(&root);
    let path = download(&client(), &bundle, &root.join("downloads")).expect("download");
    let installed = install(&store, &bundle, &path).expect("install a plain tar");
    assert_eq!(installed.files.len(), FIXTURE_FILE_COUNT);
    assert!(store.bundle_dir("fixture").join(long_name()).exists());
    let _ = fs::remove_dir_all(&root);
}

/// An index fetched over HTTP is parsed, and a version the document does not
/// publish is refused rather than resolved to something close.
#[test]
fn the_fetched_index_refuses_a_version_it_does_not_publish() {
    let (base, _served) = serve(1, move |_, head| {
        ok(
            index_document(&base_for(head), "fixture", "0.3.1", TARBALL_GZ).as_bytes(),
            "application/json",
        )
    });
    let fetched = client().get(&format!("{base}/index.json")).expect("index");
    let index = Index::parse(&fetched.body).expect("parse");
    // `^0.3` is a range: refused by name, never approximated.
    let err = index.resolve("fixture", "^0.3").unwrap_err();
    assert!(
        err.to_string()
            .starts_with(forge_market::ERR_RANGE_REQUIREMENT),
        "{err}"
    );
    let err = index.resolve("fixture", "9.9.9").unwrap_err();
    assert!(
        err.to_string()
            .starts_with(forge_market::ERR_UNKNOWN_VERSION),
        "{err}"
    );
    assert!(err.to_string().contains("0.3.1"), "{err}");
}

/// A user's edit is not the market's to delete: uninstall refuses and names the
/// file, and nothing is removed.
#[test]
fn an_uninstall_refuses_a_file_the_user_edited() {
    let (base, _served) = serve(1, move |_, _| ok(TARBALL_GZ, "application/gzip"));
    let bundle = bundle_for(&base, "fixture", "0.3.1", TARBALL_GZ);
    let root = scratch("edited");
    let store = Store::new(&root);
    let path = download(&client(), &bundle, &root.join("downloads")).expect("download");
    install(&store, &bundle, &path).expect("install");

    let edited = store.bundle_dir("fixture").join("plugin.json");
    fs::write(&edited, b"{\"edited\":true}").expect("edit");
    let before = snapshot(&root);

    let err = uninstall(&store, "fixture").unwrap_err();
    let text = err.to_string();
    assert!(text.starts_with(ERR_FILE_MODIFIED), "{text}");
    assert!(text.contains("plugin.json"), "{text}");
    assert_eq!(snapshot(&root), before, "a refused uninstall deleted files");
    assert_eq!(list(&store).expect("list").len(), 1);
    let _ = fs::remove_dir_all(&root);
}

/// A file nobody installed stops the uninstall too: the record does not own it.
#[test]
fn an_uninstall_refuses_a_file_the_record_never_listed() {
    let (base, _served) = serve(1, move |_, _| ok(TARBALL_GZ, "application/gzip"));
    let bundle = bundle_for(&base, "fixture", "0.3.1", TARBALL_GZ);
    let root = scratch("extra");
    let store = Store::new(&root);
    let path = download(&client(), &bundle, &root.join("downloads")).expect("download");
    install(&store, &bundle, &path).expect("install");

    let stray = store.bundle_dir("fixture").join("user-notes.txt");
    fs::write(&stray, b"mine").expect("write a file nobody installed");
    let before = snapshot(&root);
    let err = uninstall(&store, "fixture").unwrap_err();
    assert!(err.to_string().starts_with(ERR_EXTRA_FILES), "{err}");
    assert_eq!(snapshot(&root), before, "a refused uninstall deleted files");
    let _ = fs::remove_dir_all(&root);
}
