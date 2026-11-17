//! Error type shared by the crate's entry points.

use std::fmt;
use std::io;
use std::net::IpAddr;

/// Things that can go wrong while bridging.
#[derive(Debug)]
pub enum Error {
    /// Socket I/O failed.
    Io(io::Error),
    /// The proxy secret could not be parsed.
    Secret(String),
    /// The SOCKS5 client (or the upstream stream) broke the framing rules.
    Protocol(String),
    /// A SOCKS5 request used a feature this bridge does not implement.
    Unsupported(String),
    /// The SOCKS5 destination is not one of the known Telegram data centres.
    UnknownDc(IpAddr),
    /// The upstream proxy answered nothing, or nothing usable.
    Upstream(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "i/o error: {e}"),
            Self::Secret(msg) => write!(f, "bad secret: {msg}"),
            Self::Protocol(msg) => write!(f, "protocol error: {msg}"),
            Self::Unsupported(msg) => write!(f, "unsupported: {msg}"),
            Self::UnknownDc(ip) => write!(f, "{ip} is not a known Telegram DC address"),
            Self::Upstream(msg) => write!(f, "upstream proxy: {msg}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

pub(crate) fn protocol(msg: impl fmt::Display) -> Error {
    Error::Protocol(msg.to_string())
}

pub(crate) fn unsupported(msg: impl fmt::Display) -> Error {
    Error::Unsupported(msg.to_string())
}