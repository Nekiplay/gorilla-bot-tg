//! The bridge itself: a local SOCKS5 listener backed by a proxy session.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ctr::cipher::StreamCipher as _;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;
use tokio::time::Instant as TokioInstant;

use crate::error::{protocol, Error};
use crate::framing::{first_frame_offset, strip_transport_tag};
use crate::handshake::{generate_client_handshake, AesCtr256};
use crate::socks::{reply_code, socks_accept};

/// Callback used for progress messages, e.g. `|line| eprintln!("{line}")`.
pub type LogFn = Arc<dyn Fn(&str) + Send + Sync>;

/// Everything needed to run a bridge.
#[derive(Clone)]
pub struct BridgeConfig {
    /// The `TgWsProxy` endpoint to dial (usually `127.0.0.1:1443`).
    pub target: SocketAddr,
    /// The proxy secret.
    pub secret: [u8; 16],
    /// Where to report progress; see [`BridgeConfig::with_log`].
    pub log: Option<LogFn>,
    /// How long a single redial may stay silent before the next one starts.
    pub attempt_timeout: Duration,
    /// Upper bound on redials for one client connection.
    pub max_attempts: usize,
    /// Upper bound on the whole redial loop.
    pub session_budget: Duration,
    /// Upper bound on bytes buffered from the client before the proxy answers.
    pub pending_cap: usize,
    /// Largest plausible upstream frame, used to recognise a live socket.
    pub frame_len_cap: usize,
    /// How many silent rounds in a row end the redial loop early.
    pub silent_limit: usize,
}

impl BridgeConfig {
    /// Creates a configuration with the tuned defaults.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::net::SocketAddr;
    /// use tgws_bridge::BridgeConfig;
    ///
    /// let target: SocketAddr = "127.0.0.1:1443".parse().unwrap();
    /// let cfg = BridgeConfig::new(target, [0u8; 16]);
    /// assert_eq!(cfg.attempt_timeout.as_secs(), 4);
    /// ```
    pub fn new(target: SocketAddr, secret: [u8; 16]) -> Self {
        Self {
            target,
            secret,
            log: None,
            attempt_timeout: Duration::from_secs(4),
            max_attempts: 60,
            session_budget: Duration::from_secs(120),
            pending_cap: 256 * 1024,
            frame_len_cap: 8 * 1024 * 1024,
            silent_limit: 3,
        }
    }

    /// Installs a progress callback.
    pub fn with_log(mut self, log: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.log = Some(Arc::new(log));
        self
    }
}

struct Inner {
    target: SocketAddr,
    secret: [u8; 16],
    log: Option<LogFn>,
    attempt_timeout: Duration,
    max_attempts: usize,
    session_budget: Duration,
    pending_cap: usize,
    frame_len_cap: usize,
    silent_limit: usize,
    sessions: AtomicUsize,
}

impl Inner {
    fn say(&self, line: impl FnOnce() -> String) {
        if let Some(log) = &self.log {
            log(&line());
        }
    }
}

/// A running bridge.
pub struct Bridge {
    local: SocketAddr,
    handle: JoinHandle<()>,
    last_reason: Arc<Mutex<Option<String>>>,
}

impl Bridge {
    /// Binds a local SOCKS5 listener and returns immediately; connections are
    /// served on background tasks.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the listener cannot be bound.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
    /// use tgws_bridge::{Bridge, BridgeConfig};
    /// let target = "127.0.0.1:1443".parse()?;
    /// let bridge = Bridge::spawn(BridgeConfig::new(target, [0x11; 16])).await?;
    /// println!("point your client at {}", bridge.socks_url());
    /// # Ok(())
    /// # }
    /// ```
    pub async fn spawn(cfg: BridgeConfig) -> Result<Self, Error> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let local = listener.local_addr()?;
        let last_reason = Arc::new(Mutex::new(None));

        let inner = Arc::new(Inner {
            target: cfg.target,
            secret: cfg.secret,
            log: cfg.log.clone(),
            attempt_timeout: cfg.attempt_timeout,
            max_attempts: cfg.max_attempts,
            session_budget: cfg.session_budget,
            pending_cap: cfg.pending_cap,
            frame_len_cap: cfg.frame_len_cap,
            silent_limit: cfg.silent_limit,
            sessions: AtomicUsize::new(0),
        });
        let reason_slot = Arc::clone(&last_reason);

        inner.say(|| format!("[bridge] local SOCKS5: {local} -> {}", cfg.target));

        let handle = tokio::spawn(async move {
            loop {
                let (client, _) = match listener.accept().await {
                    Ok(pair) => pair,
                    Err(e) => {
                        inner.say(|| format!("[bridge] accept failed: {e}"));
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let _ = client.set_nodelay(true);
                inner.sessions.fetch_add(1, Ordering::Relaxed);
                let inner = Arc::clone(&inner);
                let slot = Arc::clone(&reason_slot);
                tokio::spawn(async move {
                    if let Err(e) = serve_client(client, &inner).await {
                        let text = e.to_string();
                        inner.say(|| format!("[bridge] session closed: {text}"));
                        if let Ok(mut guard) = slot.lock() {
                            *guard = Some(text);
                        }
                    }
                });
            }
        });

        Ok(Self {
            local,
            handle,
            last_reason,
        })
    }

    /// The local SOCKS5 address.
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// The local endpoint as a `socks5://host:port` URL.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
    /// # use tgws_bridge::{Bridge, BridgeConfig};
    /// # let bridge = Bridge::spawn(BridgeConfig::new("127.0.0.1:1443".parse()?, [0; 16])).await?;
    /// assert!(bridge.socks_url().starts_with("socks5://127.0.0.1:"));
    /// # Ok(())
    /// # }
    /// ```
    pub fn socks_url(&self) -> String {
        format!("socks5://{}", self.local)
    }

    /// The reason the last failed session gave up, if any.
    pub fn last_reason(&self) -> Option<String> {
        self.last_reason.lock().ok().and_then(|g| g.clone())
    }

    /// Stops the listener and every session it started.
    pub fn shutdown(&self) {
        self.handle.abort();
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// One live upstream connection plus the cipher state it needs.
struct Session {
    upstream: TcpStream,
    sender: AesCtr256,
    receiver: AesCtr256,
    backlog: Vec<u8>,
}

async fn serve_client(mut client: TcpStream, inner: &Arc<Inner>) -> Result<(), Error> {
    let dc_idx = match socks_accept(&mut client).await {
        Ok(dc) => dc,
        Err(e) => return Err(e),
    };

    let mut pending = Vec::new();
    // The SOCKS5 success reply is already on the wire, so a failure can only be
    // reported by closing the connection: a second reply would be read as data.
    let mut session = latch_upstream(&mut client, inner, dc_idx, &mut pending).await?;

    client.write_all(&session.backlog).await?;
    if !pending.is_empty() {
        session.sender.apply_keystream(&mut pending);
        session.upstream.write_all(&pending).await?;
    }

    relay(client, session.upstream, session.sender, session.receiver).await
}

/// Dials the proxy, replays whatever the client already sent, and waits for the
/// first complete upstream frame. Half-dead sockets are retried.
async fn latch_upstream(
    client: &mut TcpStream,
    inner: &Inner,
    dc_idx: i16,
    pending: &mut Vec<u8>,
) -> Result<Session, Error> {
    let started = Instant::now();
    let deadline = TokioInstant::now() + inner.session_budget;
    let mut attempt = 0usize;
    let mut silent_rounds = 0usize;
    let mut last_reason = String::from("no attempt was made");
    let mut tag_stripped = false;
    let mut cli_buf = vec![0u8; 16 * 1024];

    loop {
        if attempt >= inner.max_attempts || TokioInstant::now() >= deadline {
            break;
        }
        attempt += 1;

        let mut upstream = match TcpStream::connect(inner.target).await {
            Ok(stream) => stream,
            Err(e) => {
                last_reason = format!("connect failed: {e}");
                inner.say(|| format!("[bridge] attempt {attempt}: {last_reason}"));
                silent_rounds = 0;
                if !sleep_until_allowed(attempt, deadline).await {
                    break;
                }
                continue;
            }
        };
        upstream.set_nodelay(true)?;

        let (handshake, mut sender, mut receiver) = generate_client_handshake(&inner.secret, dc_idx);
        if let Err(e) = upstream.write_all(&handshake).await {
            last_reason = format!("handshake write failed: {e}");
            continue;
        }

        // Replay everything the client has sent so far under the new cipher.
        let mut replay = pending.clone();
        sender.apply_keystream(&mut replay);
        if let Err(e) = upstream.write_all(&replay).await {
            last_reason = format!("replay write failed: {e}");
            continue;
        }
        pending.clear();

        let attempt_deadline = TokioInstant::now() + inner.attempt_timeout;
        let mut up_plain: Vec<u8> = Vec::with_capacity(4096);
        let mut up_buf = vec![0u8; 16 * 1024];

        loop {
            tokio::select! {
                biased;
                read = upstream.read(&mut up_buf) => {
                    match read {
                        Ok(0) => {
                            last_reason = format!(
                                "closed after {:.1}s without a complete frame",
                                started.elapsed().as_secs_f64()
                            );
                            silent_rounds = 0;
                            break;
                        }
                        Ok(n) => {
                            receiver.apply_keystream(&mut up_buf[..n]);
                            up_plain.extend_from_slice(&up_buf[..n]);
                        }
                        Err(e) => {
                            last_reason = format!("upstream read failed: {e}");
                            silent_rounds = 0;
                            break;
                        }
                    }
                }
                read = client.read(&mut cli_buf) => {
                    match read {
                        Ok(0) => {
                            return Err(Error::Upstream(
                                "client closed the connection before the proxy answered".into(),
                            ));
                        }
                        Ok(n) => {
                            pending.extend_from_slice(&cli_buf[..n]);
                            strip_transport_tag(pending, &mut tag_stripped);
                            if pending.len() > inner.pending_cap {
                                return Err(protocol(format!(
                                    "client sent more than {} KiB before the proxy answered",
                                    inner.pending_cap / 1024
                                )));
                            }
                        }
                        Err(e) => return Err(Error::Io(e)),
                    }
                }
                _ = tokio::time::sleep_until(attempt_deadline) => {
                    last_reason = format!(
                        "silent for {:.0}s: accepted the connection but answered nothing",
                        inner.attempt_timeout.as_secs_f64()
                    );
                    silent_rounds += 1;
                    break;
                }
            }

            if let Some(off) = first_frame_offset(&up_plain, inner.frame_len_cap) {
                let head = u32::from_le_bytes([up_plain[off], up_plain[off + 1], up_plain[off + 2], up_plain[off + 3]]);
                inner.say(|| {
                    format!(
                        "[bridge] live socket: attempt {attempt}, first frame {} B ({:.1}s from start)",
                        head,
                        started.elapsed().as_secs_f64()
                    )
                });
                return Ok(Session {
                    upstream,
                    sender,
                    receiver,
                    backlog: up_plain[off..].to_vec(),
                });
            }

            // Nothing complete yet. Once enough bytes have arrived with no
            // plausible frame anywhere, the socket is talking nonsense.
            if !up_plain.is_empty() && up_plain.len() >= 260 {
                last_reason = format!("no frame header in {} B of response", up_plain.len());
                silent_rounds = 0;
                break;
            }
        }

        inner.say(|| format!("[bridge] attempt {attempt}: upstream «{last_reason}» — redial"));
        if silent_rounds >= inner.silent_limit {
            break;
        }
        if !sleep_until_allowed(attempt, deadline).await {
            break;
        }
    }

    let verdict = if silent_rounds >= inner.silent_limit {
        format!(
            "proxy {} accepted the connection but stayed silent {} times in a row — \
             its own upstream is dead; check the proxy log",
            inner.target, silent_rounds
        )
    } else {
        format!(
            "no socket of proxy {} returned a live answer in {:.0}s ({last_reason})",
            inner.target,
            started.elapsed().as_secs_f64()
        )
    };
    Err(Error::Upstream(verdict))
}

/// Waits before the next redial. Returns `false` when the budget is spent.
async fn sleep_until_allowed(attempt: usize, deadline: TokioInstant) -> bool {
    let pause = match attempt {
        0..=4 => Duration::from_secs(1),
        5..=14 => Duration::from_secs(10),
        _ => Duration::from_secs(20),
    };
    let now = TokioInstant::now();
    if now + pause >= deadline {
        return false;
    }
    tokio::time::sleep(pause).await;
    true
}

/// Pumps bytes in both directions, encrypting and decrypting as it goes.
async fn relay<U>(
    client: TcpStream,
    upstream: U,
    sender: AesCtr256,
    receiver: AesCtr256,
) -> Result<(), Error>
where
    U: AsyncRead + AsyncWrite + Send + 'static,
{
    let (mut client_read, mut client_write) = client.into_split();
    let (mut up_read, mut up_write) = tokio::io::split(upstream);

    let sender = Arc::new(AsyncMutex::new(sender));
    let receiver = Arc::new(AsyncMutex::new(receiver));

    let to_upstream = {
        let sender = Arc::clone(&sender);
        async move {
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                let n = client_read.read(&mut buf).await?;
                if n == 0 {
                    up_write.shutdown().await?;
                    return Ok::<(), std::io::Error>(());
                }
                sender.lock().await.apply_keystream(&mut buf[..n]);
                up_write.write_all(&buf[..n]).await?;
                up_write.flush().await?;
            }
        }
    };

    let to_client = {
        let receiver = Arc::clone(&receiver);
        async move {
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                let n = up_read.read(&mut buf).await?;
                if n == 0 {
                    client_write.shutdown().await?;
                    return Ok::<(), std::io::Error>(());
                }
                receiver.lock().await.apply_keystream(&mut buf[..n]);
                client_write.write_all(&buf[..n]).await?;
                client_write.flush().await?;
            }
        }
    };

    let (up, down) = tokio::join!(to_upstream, to_client);
    up?;
    down?;
    Ok(())
}

/// Sends a SOCKS5 "host unreachable" reply; exposed for integrators that
/// reject a destination before calling [`socks_accept`].
pub async fn reject(client: &mut TcpStream, code: u8) -> Result<(), Error> {
    reply_code(client, code).await
}

