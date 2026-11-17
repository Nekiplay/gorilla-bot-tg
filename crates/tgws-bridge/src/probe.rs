//! Connectivity probes: what is listening, and does the proxy answer?

use std::fmt;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use ctr::cipher::StreamCipher as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::Error;
use crate::handshake::{generate_client_handshake, AesCtr256};

/// How a local address behaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalKind {
    /// Accepts connections and speaks SOCKS5 without authentication.
    Socks5,
    /// Accepts connections but does not speak SOCKS5 (some other proxy).
    Open,
    /// Refuses connections.
    Closed,
}

impl LocalKind {
    /// Human-readable description used in logs and UI notes.
    ///
    /// # Examples
    ///
    /// ```
    /// use tgws_bridge::LocalKind;
    ///
    /// assert_eq!(
    ///     LocalKind::Socks5.describe("127.0.0.1", 1443),
    ///     "127.0.0.1:1443 — SOCKS5 без пароля (отвечает)"
    /// );
    /// ```
    pub fn describe(&self, host: &str, port: u16) -> String {
        match self {
            Self::Socks5 => format!("{host}:{port} — SOCKS5 без пароля (отвечает)"),
            Self::Open => format!("{host}:{port} — порт открыт, но это не SOCKS5"),
            Self::Closed => format!("{host}:{port} — порт закрыт"),
        }
    }
}

impl fmt::Display for LocalKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Socks5 => "socks5",
            Self::Open => "open",
            Self::Closed => "closed",
        })
    }
}

/// Checks whether `host:port` is a working SOCKS5 endpoint.
///
/// Times out after 1.5 s for the connection and 1.2 s for the SOCKS5 greeting,
/// so it is safe to call on startup for several candidates.
pub async fn probe_local(host: &str, port: u16) -> LocalKind {
    let addr = match tokio::time::timeout(
        Duration::from_millis(1500),
        TcpStream::connect((host, port)),
    )
    .await
    {
        Ok(Ok(stream)) => stream,
        _ => return LocalKind::Closed,
    };

    let mut stream = addr;
    if stream.write_all(&[0x05, 0x01, 0x00]).await.is_err() {
        return LocalKind::Open;
    }
    let mut reply = [0u8; 2];
    match tokio::time::timeout(Duration::from_millis(1200), stream.read_exact(&mut reply)).await {
        Ok(Ok(_)) if reply == [0x05, 0x00] => LocalKind::Socks5,
        _ => LocalKind::Open,
    }
}

/// What a live probe saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The proxy answered bytes: the secret is right and the route works.
    Answered {
        /// Number of bytes received.
        bytes: usize,
        /// Time from connect to the first byte.
        elapsed: Duration,
    },
    /// The proxy accepted the connection and closed it without a byte.
    Closed {
        /// Time from connect to EOF.
        elapsed: Duration,
    },
    /// The proxy accepted the connection and said nothing at all.
    Silent {
        /// How long the probe waited.
        waited: Duration,
    },
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Answered { bytes, elapsed } => write!(
                f,
                "answered {bytes} bytes in {:.1}s — the proxy is alive and the secret is correct",
                elapsed.as_secs_f64()
            ),
            Self::Closed { elapsed } => write!(
                f,
                "closed after {:.1}s without a byte — the proxy accepted the connection \
                 and dropped it; the problem is further upstream",
                elapsed.as_secs_f64()
            ),
            Self::Silent { waited } => write!(
                f,
                "silent for {:.0}s — the secret is probably wrong: the proxy drains the \
                 garbage and never answers",
                waited.as_secs_f64()
            ),
        }
    }
}

/// The encrypted first frame of every MTProto client: `req_pq_multi`.
///
/// A proxy stays silent until it sees this frame, so a probe has to send it.
fn req_pq_frame(sender: &mut AesCtr256) -> Vec<u8> {
    let mut payload = Vec::with_capacity(40);
    payload.extend_from_slice(&[0u8; 8]); // auth_key_id
    payload.extend_from_slice(&[0x11; 8]); // msg_id
    payload.extend_from_slice(&20u32.to_le_bytes()); // body length
    payload.extend_from_slice(&0xbe7e_8ef1u32.to_le_bytes()); // req_pq_multi
    payload.extend_from_slice(&[7u8; 16]); // nonce
    // `Intermediate` framing: a four byte length prefixed to the payload.
    let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
    frame.extend_from_slice(&payload);
    sender.apply_keystream(&mut frame);
    frame
}

/// A single live probe against a proxy.
#[derive(Debug, Clone)]
pub struct Probe {
    /// Proxy address.
    pub target: SocketAddr,
    /// Proxy secret.
    pub secret: [u8; 16],
    /// Data centre index to announce, `2` by default.
    pub dc: i16,
    /// How long to wait for the first byte.
    pub timeout: Duration,
}

impl Probe {
    /// Creates a probe for `target` with `secret`, DC 2 and a 12 s timeout.
    pub fn new(target: SocketAddr, secret: [u8; 16]) -> Self {
        Self {
            target,
            secret,
            dc: 2,
            timeout: Duration::from_secs(12),
        }
    }

    /// Overrides the announced data centre index.
    pub fn with_dc(mut self, dc: i16) -> Self {
        self.dc = dc;
        self
    }

    /// Overrides how long the probe waits for the first byte.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Runs the probe.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the TCP connection cannot be established.
    pub async fn run(&self) -> Result<Outcome, Error> {
        let mut stream = TcpStream::connect(self.target).await?;
        stream.set_nodelay(true)?;
        let started = Instant::now();

        let (handshake, mut sender, mut receiver) = generate_client_handshake(&self.secret, self.dc);
        stream.write_all(&handshake).await?;
        // A bare handshake is not enough: `TgWsProxy` only learns which DC to
        // dial once the first frame arrives, so send a real `req_pq_multi`.
        stream.write_all(&req_pq_frame(&mut sender)).await?;

        let mut buf = vec![0u8; 4096];
        let read = tokio::time::timeout(self.timeout, stream.read(&mut buf)).await;
        match read {
            Ok(Ok(0)) => Ok(Outcome::Closed {
                elapsed: started.elapsed(),
            }),
            Ok(Ok(n)) => {
                receiver.apply_keystream(&mut buf[..n]);
                Ok(Outcome::Answered {
                    bytes: n,
                    elapsed: started.elapsed(),
                })
            }
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(Outcome::Closed {
                elapsed: started.elapsed(),
            }),
            Ok(Err(e)) => Err(Error::Io(e)),
            Err(_) => Ok(Outcome::Silent {
                waited: self.timeout,
            }),
        }
    }
}

/// Shortcut for [`Probe::new`] + [`Probe::run`] that returns a sentence.
///
/// # Errors
///
/// Returns [`Error::Io`] when the TCP connection cannot be established.
pub async fn probe(target: SocketAddr, secret: [u8; 16]) -> Result<String, Error> {
    Ok(Probe::new(target, secret).run().await?.to_string())
}