//! Local SOCKS5 bridge to a [`TgWsProxy`] instance.
//!
//! [`TgWsProxy`] (Flowseal's `tg-ws-proxy`) is a small program that keeps
//! WebSocket tunnels to the Telegram data centres and exposes them as an
//! MTProto proxy: it accepts an *obfuscated2* TCP handshake and then forwards
//! intermediate-framed MTProto traffic. Telegram Desktop talks to it directly,
//! but most MTProto clients (and the `grammers` stack) need a SOCKS5 endpoint.
//!
//! This crate fills that gap. It binds a local SOCKS5 listener and, for every
//! client connection, dials the proxy with a generated obfuscated2 handshake,
//! replays the client's first frame once the upstream answers, and then relays
//! both directions with the matching AES-CTR state.
//!
//! ```no_run
//! use std::net::{IpAddr, Ipv4Addr, SocketAddr};
//! use tgws_bridge::{Bridge, BridgeConfig, parse_secret};
//!
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! let secret = parse_secret("dd00000000000000000000000000000000")?;
//! let target: SocketAddr = "127.0.0.1:1443".parse()?;
//! let bridge = Bridge::spawn(BridgeConfig::new(target, secret)).await?;
//!
//! // Hand `bridge.socks_url()` to the MTProto client.
//! println!("socks5://{}", bridge.local_addr());
//! # Ok(())
//! # }
//! ```
//!
//! # What the bridge works around
//!
//! * **Transport tag.** `grammers` prepends a one-off transport tag
//!   (`ee ee ee ee` for intermediate) to the first frame after obfuscation.
//!   The proxy reads those bytes as a frame length and silently drops the
//!   stream, so [`strip_transport_tag`] removes them.
//! * **Relay-init echo.** Some routes prepend a 64-byte echo of the generated
//!   handshake to the upstream stream, others do not. The first frame offset is
//!   detected with [`first_frame_offset`] instead of assuming a constant.
//! * **Half-dead sockets.** A proxy pool hands out sockets that accept a
//!   connection and never answer. The bridge redials with replay, so the client
//!   only ever sees a live socket.
//!
//! [`TgWsProxy`]: https://github.com/Flowseal/tg-ws-proxy

#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod autodetect;
mod bridge;
mod dc;
mod error;
mod framing;
mod handshake;
mod probe;
mod secret;
mod socks;

pub use autodetect::{autodetect, autodetect_in, config_path, read_config, LocalProxy};
pub use bridge::{reject, Bridge, BridgeConfig, LogFn};
pub use dc::{dc_for_ip, DC_ADDRESSES};
pub use error::Error;
pub use framing::{first_frame_offset, strip_transport_tag};
pub use handshake::{
    generate_client_handshake, AesCtr256, HANDSHAKE_LEN, PROTO_TAG_ABRIDGED, PROTO_TAG_INTERMEDIATE,
};
pub use probe::{probe, probe_local, LocalKind, Outcome, Probe};
pub use secret::parse_secret;
pub use socks::{socks_accept, reply};

/// Default port of a local `TgWsProxy` process.
pub const DEFAULT_PORT: u16 = 1443;

/// Default loopback host of a local `TgWsProxy` process.
pub const DEFAULT_HOST: &str = "127.0.0.1";