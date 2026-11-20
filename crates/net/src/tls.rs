//! TLS and the socket a request travels over.
//!
//! Both transports expose the same blocking `Read`/`Write` surface, so the HTTP
//! layer above never branches on the scheme. rustls is driven directly rather
//! than through an adapter crate: the workspace already pays for `rustls`,
//! `ring` and `webpki-roots`, and a non-blocking adapter would drag in `tokio`.

use std::io::{self, ErrorKind as IoErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hearth_core::{Error, Result};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};

use crate::url::Url;

/// The message a closed connection reports. Callers that have their own
/// vocabulary (the provider) replace it at their boundary.
pub const ERR_ABORTED: &str = "net: connection aborted";

/// How often a blocking read wakes up to check the deadline and the cancel
/// flag. Small enough to abort promptly, large enough not to spin.
const READ_POLL: Duration = Duration::from_millis(200);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// Certificate roots: the pinned public bundle, optionally extended.
#[derive(Clone)]
pub struct Roots(rustls::RootCertStore);

impl Roots {
    /// Public roots only.
    pub fn public() -> Self {
        let mut store = rustls::RootCertStore::empty();
        store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        Self(store)
    }

    /// Public roots plus every certificate in `ca_file`.
    ///
    /// A private CA *extends* the bundle; it never replaces it, so adding one
    /// cannot silently disable verification for public endpoints.
    pub fn with_ca_file(ca_file: &Path) -> Result<Self> {
        let mut roots = Self::public();
        let bytes = std::fs::read(ca_file).map_err(|e| {
            Error::new(
                hearth_core::ErrorKind::Io,
                format!("net: cannot read CA file {}: {e}", ca_file.display()),
            )
        })?;
        let certs = parse_pem_certificates(&bytes).map_err(|e| {
            Error::new(
                e.kind(),
                format!("net: CA file {}: {}", ca_file.display(), e.message()),
            )
        })?;
        let (valid, rejected) = roots.0.add_parsable_certificates(certs);
        if valid == 0 {
            return Err(Error::new(
                hearth_core::ErrorKind::Invalid,
                format!(
                    "net: no usable certificates in CA file {} ({rejected} rejected)",
                    ca_file.display()
                ),
            ));
        }
        Ok(roots)
    }

    /// Roots from an optional file, the common shape of a client option.
    pub fn from_option(ca_file: Option<&Path>) -> Result<Self> {
        match ca_file {
            Some(path) => Self::with_ca_file(path),
            None => Ok(Self::public()),
        }
    }

    fn store(&self) -> rustls::RootCertStore {
        self.0.clone()
    }
}

/// Decode every `CERTIFICATE` section of a PEM blob.
///
/// The error never echoes the body: a CA bundle can sit next to a private key.
pub fn parse_pem_certificates(bytes: &[u8]) -> Result<Vec<CertificateDer<'static>>> {
    let mut certs = Vec::new();
    for item in CertificateDer::pem_slice_iter(bytes) {
        match item {
            Ok(cert) => certs.push(cert),
            Err(e) => {
                return Err(Error::new(
                    hearth_core::ErrorKind::Invalid,
                    format!("invalid CA PEM: {e}"),
                ))
            }
        }
    }
    if certs.is_empty() {
        return Err(Error::new(
            hearth_core::ErrorKind::Invalid,
            "no certificates found in CA PEM".to_string(),
        ));
    }
    Ok(certs)
}

/// A connected socket. `Read`/`Write` work for both transports.
pub struct Connection {
    inner: Inner,
    closed: Arc<AtomicBool>,
    deadline: Option<Instant>,
}

enum Inner {
    Plain(TcpStream),
    Tls(Box<TlsStream>),
}

impl Connection {
    /// A handle that can break a blocked read from another thread.
    pub fn abort_handle(&self) -> Option<TcpStream> {
        match &self.inner {
            Inner::Plain(stream) => stream.try_clone().ok(),
            Inner::Tls(stream) => stream.tcp.try_clone().ok(),
        }
    }
}

/// Connect to `url`, performing the TLS handshake when the scheme is https.
///
/// `deadline` bounds the whole exchange: when it passes, reads fail with
/// `TimedOut` instead of blocking forever. `None` means "no deadline, only the
/// cancel flag stops me", which is what a long-lived stream needs.
pub fn connect(
    url: &Url,
    roots: &Roots,
    closed: Arc<AtomicBool>,
    deadline: Option<Instant>,
) -> Result<Connection> {
    let address = format!("{}:{}", url.host, url.port);
    let socket_addr = address
        .to_socket_addrs()
        .map_err(|e| transport(&format!("cannot resolve {address}: {e}")))?
        .next()
        .ok_or_else(|| transport(&format!("no address for {address}")))?;

    let stream = TcpStream::connect(socket_addr)
        .map_err(|e| transport(&format!("cannot connect to {address}: {e}")))?;
    let _ = stream.set_read_timeout(Some(READ_POLL));
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));

    let inner = if url.scheme.eq_ignore_ascii_case("https") {
        Inner::Tls(Box::new(TlsStream::new(
            stream,
            &url.host,
            closed.clone(),
            roots.store(),
            deadline,
        )?))
    } else {
        Inner::Plain(stream)
    };

    Ok(Connection {
        inner,
        closed,
        deadline,
    })
}

fn transport(message: &str) -> Error {
    Error::new(hearth_core::ErrorKind::Io, format!("net: {message}"))
}

/// The TLS half of a connection.
struct TlsStream {
    tcp: TcpStream,
    tls: rustls::ClientConnection,
    closed: Arc<AtomicBool>,
    deadline: Option<Instant>,
}

impl TlsStream {
    fn new(
        tcp: TcpStream,
        host: &str,
        closed: Arc<AtomicBool>,
        roots: rustls::RootCertStore,
        deadline: Option<Instant>,
    ) -> Result<Self> {
        let name = ServerName::try_from(host.to_string())
            .map_err(|_| transport("invalid TLS server name"))?;
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let tls = rustls::ClientConnection::new(Arc::new(config), name)
            .map_err(|e| transport(&format!("TLS setup: {e}")))?;
        let mut stream = Self {
            tcp,
            tls,
            closed,
            deadline,
        };
        while stream.tls.is_handshaking() {
            stream.pump()?;
        }
        Ok(stream)
    }

    /// Flush pending TLS records, read what is available, feed it to rustls.
    ///
    /// A socket timeout is not a failure — it is how the loop wakes up to check
    /// the deadline and the cancel flag.
    fn pump(&mut self) -> io::Result<()> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(io::Error::new(IoErrorKind::ConnectionAborted, ERR_ABORTED));
        }
        if let Some(deadline) = self.deadline {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    IoErrorKind::TimedOut,
                    "net: deadline passed",
                ));
            }
        }
        self.tls.write_tls(&mut self.tcp)?;
        match self.tls.read_tls(&mut self.tcp) {
            Ok(0) => {
                return Err(io::Error::new(
                    IoErrorKind::UnexpectedEof,
                    "TLS peer closed",
                ))
            }
            Ok(_) => {}
            Err(e) if matches!(e.kind(), IoErrorKind::WouldBlock | IoErrorKind::TimedOut) => {
                return Ok(())
            }
            Err(e) => return Err(e),
        }
        self.tls
            .process_new_packets()
            .map_err(|e| io::Error::new(IoErrorKind::InvalidData, e))?;
        Ok(())
    }
}

impl Read for TlsStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.closed.load(Ordering::Relaxed) {
                return Err(io::Error::new(IoErrorKind::ConnectionAborted, ERR_ABORTED));
            }
            match self.tls.reader().read(buf) {
                Ok(n) if n > 0 => return Ok(n),
                Ok(_) => {}
                Err(e) if e.kind() == IoErrorKind::WouldBlock => {}
                Err(e) => return Err(e),
            }
            self.pump()?;
        }
    }
}

impl Write for TlsStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.tls.writer().write(buf)?;
        self.tls.write_tls(&mut self.tcp)?;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.tls.write_tls(&mut self.tcp)?;
        self.tcp.flush()
    }
}

impl Read for Connection {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &mut self.inner {
            Inner::Plain(stream) => read_with_deadline(stream, buf, &self.closed, self.deadline),
            Inner::Tls(stream) => stream.read(buf),
        }
    }
}

impl Write for Connection {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match &mut self.inner {
            Inner::Plain(stream) => stream.write(buf),
            Inner::Tls(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.inner {
            Inner::Plain(stream) => stream.flush(),
            Inner::Tls(stream) => stream.flush(),
        }
    }
}

/// A plain socket read that honours the deadline and the cancel flag, using the
/// same wake-up-and-retry shape as the TLS path.
fn read_with_deadline(
    stream: &mut TcpStream,
    buf: &mut [u8],
    closed: &AtomicBool,
    deadline: Option<Instant>,
) -> io::Result<usize> {
    loop {
        if closed.load(Ordering::Relaxed) {
            return Err(io::Error::new(IoErrorKind::ConnectionAborted, ERR_ABORTED));
        }
        match stream.read(buf) {
            Ok(n) => return Ok(n),
            Err(e) if e.kind() == IoErrorKind::Interrupted => continue,
            Err(e) if matches!(e.kind(), IoErrorKind::WouldBlock | IoErrorKind::TimedOut) => {
                if let Some(deadline) = deadline {
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(
                            IoErrorKind::TimedOut,
                            "net: deadline passed",
                        ));
                    }
                }
                continue;
            }
            Err(e) => return Err(e),
        }
    }
}
