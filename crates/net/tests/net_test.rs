//! End-to-end client tests against a real loopback server.
//!
//! The unit tests check parsing in isolation; these check that the request the
//! client builds is one a server will answer, that a redirect is followed with
//! the right method, that the byte cap actually stops the read, and that a
//! chunked body arrives reassembled. A mock that returns a canned `Response`
//! would prove none of that.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

use forge_net::{Client, ClientOptions};

/// A one-shot HTTP server: each accepted connection is answered by `handler`,
/// which receives the request head and returns the raw bytes to write.
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
            // Drain the request body before answering. A server that closes a
            // socket while unread bytes are still in flight makes Windows send
            // an RST, and an RST may discard the response the client has not
            // read yet — the POST test failed with os error 10054 exactly that
            // way on a hosted runner, which is faster than this laptop.
            drain_body(&mut stream, &head);
            let bytes = handler(index, &head);
            // The client may have closed already; a failed write ends this one
            // connection, not the test.
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

/// Consume exactly `Content-Length` bytes of request body, if the head names a
/// length. Without this a POST fixture answers and closes while the body sits
/// unread in its receive buffer, and the resulting RST can swallow the answer.
fn drain_body(stream: &mut TcpStream, head: &str) {
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    let mut left = length;
    let mut scratch = [0u8; 4096];
    while left > 0 {
        let want = left.min(scratch.len());
        match stream.read(&mut scratch[..want]) {
            Ok(0) => break,
            Ok(read) => left -= read,
            Err(_) => break,
        }
    }
}

fn response(status: &str, headers: &[(&str, &str)], body: &str) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {status}\r\n");
    for (name, value) in headers {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    out.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    out.push_str(body);
    out.into_bytes()
}

#[test]
fn a_get_returns_the_body_and_the_status() {
    let (base, served) = serve(1, |_, head| {
        assert!(head.starts_with("GET /path?q=1 HTTP/1.1"), "got {head:?}");
        response("200 OK", &[("Content-Type", "text/plain")], "hello")
    });
    let client = Client::new(ClientOptions::default()).expect("client");
    let answer = client
        .get(&format!("{base}/path?q=1"))
        .expect("get should succeed");
    assert_eq!(answer.status, 200);
    assert!(answer.is_success());
    assert_eq!(answer.text(), "hello");
    assert_eq!(answer.header("content-type"), Some("text/plain"));
    assert_eq!(served.load(Ordering::SeqCst), 1);
}

/// A 404 is data: the client returns it instead of failing, because only the
/// tool layer knows whether a missing page is an error for this turn.
#[test]
fn a_non_success_status_is_returned_not_raised() {
    let (base, _) = serve(1, |_, _| {
        response("404 Not Found", &[("Content-Type", "text/plain")], "nope")
    });
    let client = Client::new(ClientOptions::default()).expect("client");
    let answer = client.get(&base).expect("a 404 is not a transport error");
    assert_eq!(answer.status, 404);
    assert!(!answer.is_success());
    assert_eq!(answer.text(), "nope");
    assert!(answer.error_summary().contains("404"));
}

#[test]
fn a_redirect_is_followed_and_the_final_url_is_reported() {
    let (base, served) = serve(2, |index, head| {
        if index == 0 {
            assert!(head.starts_with("GET /start"), "got {head:?}");
            response("302 Found", &[("Location", "/end")], "")
        } else {
            assert!(head.starts_with("GET /end"), "got {head:?}");
            response("200 OK", &[], "arrived")
        }
    });
    let client = Client::new(ClientOptions::default()).expect("client");
    let answer = client.get(&format!("{base}/start")).expect("follow");
    assert_eq!(answer.status, 200);
    assert_eq!(answer.text(), "arrived");
    assert!(
        answer.url.ends_with("/end"),
        "the reported url must be the one that answered: {}",
        answer.url
    );
    assert_eq!(served.load(Ordering::SeqCst), 2);
}

/// A 303 turns a POST into a GET with no body, which is what browsers and curl
/// do; a POST that kept its body would be sent twice.
#[test]
fn a_303_after_a_post_becomes_a_get_without_a_body() {
    let (base, _) = serve(2, |index, head| {
        if index == 0 {
            assert!(head.starts_with("POST /submit"), "got {head:?}");
            response("303 See Other", &[("Location", "/done")], "")
        } else {
            assert!(head.starts_with("GET /done"), "got {head:?}");
            assert!(
                !head.to_ascii_lowercase().contains("content-length: 4"),
                "the body must not be resent"
            );
            response("200 OK", &[], "ok")
        }
    });
    let client = Client::new(ClientOptions::default()).expect("client");
    let answer = client
        .post_json(&format!("{base}/submit"), b"{\"a\":1}")
        .expect("post");
    assert_eq!(answer.status, 200);
    assert_eq!(answer.text(), "ok");
}

#[test]
fn the_byte_cap_stops_the_read_and_says_so() {
    let body = "x".repeat(64 * 1024);
    let (base, _) = serve(1, move |_, _| {
        response("200 OK", &[("Content-Type", "text/plain")], &body)
    });
    let client = Client::new(ClientOptions {
        max_bytes: 1024,
        ..ClientOptions::default()
    })
    .expect("client");
    let answer = client.get(&base).expect("get");
    assert_eq!(answer.body.len(), 1024);
    assert!(answer.truncated, "the caller must be able to tell");
}

#[test]
fn a_chunked_body_is_reassembled() {
    let (base, _) = serve(1, |_, _| {
        let mut out = Vec::new();
        out.extend_from_slice(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\n\r\n",
        );
        out.extend_from_slice(b"5\r\nhello\r\n");
        out.extend_from_slice(b"6\r\n world\r\n");
        out.extend_from_slice(b"0\r\n\r\n");
        out
    });
    let client = Client::new(ClientOptions::default()).expect("client");
    let answer = client.get(&base).expect("get");
    assert_eq!(answer.text(), "hello world");
}

#[test]
fn a_chunked_body_with_a_trailer_is_finished_cleanly() {
    let (base, _) = serve(1, |_, _| {
        let mut out = Vec::new();
        out.extend_from_slice(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n");
        out.extend_from_slice(b"3\r\nabc\r\n0\r\nX-Checksum: 1\r\n\r\n");
        out
    });
    let client = Client::new(ClientOptions::default()).expect("client");
    let answer = client.get(&base).expect("get");
    assert_eq!(answer.text(), "abc");
}

/// The credential must not follow a redirect to a different origin: that is the
/// whole reason it travels in a header instead of the URL.
#[test]
fn a_cross_origin_redirect_drops_the_authorization_header() {
    let other = TcpListener::bind("127.0.0.1:0").expect("bind other");
    let other_address = other.local_addr().expect("addr");
    let saw_authorization = Arc::new(AtomicUsize::new(0));
    let flag = Arc::clone(&saw_authorization);
    thread::spawn(move || {
        let Ok((mut stream, _)) = other.accept() else {
            return;
        };
        let head = read_head(&mut stream);
        if head.to_ascii_lowercase().contains("authorization:") {
            flag.fetch_add(1, Ordering::SeqCst);
        }
        let _ = stream.write_all(&response("200 OK", &[], "other"));
    });

    let (base, _) = serve(1, move |_, _| {
        let location = format!("http://{other_address}/landing");
        let mut out = format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\n");
        out.push_str("Content-Length: 0\r\n\r\n");
        out.into_bytes()
    });

    let client = Client::new(ClientOptions::default()).expect("client");
    let answer = client
        .post_json_with_headers(
            &format!("{base}/start"),
            &[("Authorization".to_string(), "Bearer secret".to_string())],
            b"{}",
        )
        .expect("post");
    assert_eq!(answer.status, 200);
    assert_eq!(
        saw_authorization.load(Ordering::SeqCst),
        0,
        "the bearer token reached a host it was not meant for"
    );
}
