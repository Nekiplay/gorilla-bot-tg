//! Finding a locally installed `TgWsProxy` and reading its configuration.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use crate::bridge::BridgeConfig;
use crate::error::Error;
use crate::probe::{probe_local, LocalKind};
use crate::secret::parse_secret;
use crate::{DEFAULT_HOST, DEFAULT_PORT};

/// A `TgWsProxy` instance described by its own configuration file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalProxy {
    /// Host the proxy listens on (`127.0.0.1` by default).
    pub host: String,
    /// Port the proxy listens on (`1443` by default).
    pub port: u16,
    /// The secret found in the configuration, if it was readable.
    pub secret: Option<[u8; 16]>,
    /// Where the configuration came from, if any.
    pub source: Option<PathBuf>,
}

impl LocalProxy {
    /// A guess that assumes the default install with no configuration file.
    pub fn defaults(secret: Option<[u8; 16]>) -> Self {
        Self {
            host: DEFAULT_HOST.to_string(),
            port: DEFAULT_PORT,
            secret,
            source: None,
        }
    }

    /// Resolves the listening address.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Protocol`] when the host cannot be parsed or the port
    /// is `0`.
    pub fn socks_addr(&self) -> Result<SocketAddr, Error> {
        if self.port == 0 {
            return Err(crate::error::protocol("proxy port is 0"));
        }
        let text = format!("{}:{}", self.host, self.port);
        text.parse()
            .map_err(|e| crate::error::protocol(format!("bad proxy address «{text}»: {e}")))
    }

    /// A `socks5://host:port` URL for clients that want a URL.
    pub fn socks_url(&self) -> String {
        format!("socks5://{}:{}", self.host, self.port)
    }

    /// The secret, or an error explaining that it is missing.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Secret`] when the configuration had no usable secret.
    pub fn secret(&self) -> Result<[u8; 16], Error> {
        self.secret.ok_or_else(|| {
            Error::Secret(match &self.source {
                Some(path) => format!("no usable secret in {}", path.display()),
                None => "no secret configured".to_string(),
            })
        })
    }

    /// A ready-to-use [`BridgeConfig`] for this proxy.
    ///
    /// # Errors
    ///
    /// Propagates the errors of [`LocalProxy::socks_addr`] and
    /// [`LocalProxy::secret`].
    pub fn bridge_config(&self) -> Result<BridgeConfig, Error> {
        Ok(BridgeConfig::new(self.socks_addr()?, self.secret()?))
    }

    /// Checks whether something SOCKS5-shaped is actually listening here.
    pub async fn probe(&self) -> LocalKind {
        probe_local(&self.host, self.port).await
    }

    /// One-line description for logs and UI notes.
    pub fn describe(&self) -> String {
        let where_from = match &self.source {
            Some(path) => format!(" (from {})", path.display()),
            None => String::new(),
        };
        let secret = match self.secret {
            Some(_) => "secret found",
            None => "no secret",
        };
        format!("{} — {secret}{where_from}", self.socks_url())
    }
}

/// The conventional configuration path of `TgWsProxy` on Windows.
///
/// Honours `TGWSPROXY_CONFIG` first, then `%APPDATA%\TgWsProxy\config.json`.
pub fn config_path() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("TGWSPROXY_CONFIG") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
    }
    let base = std::env::var_os("APPDATA")?;
    let path = PathBuf::from(base).join("TgWsProxy").join("config.json");
    path.is_file().then_some(path)
}

/// Reads a `TgWsProxy` configuration file.
///
/// Unknown fields are ignored, so a newer proxy version still works here.
/// Returns `None` when the file is missing or is not valid JSON.
///
/// # Examples
///
/// ```
/// use std::fs;
/// use tgws_bridge::autodetect::read_config;
///
/// let dir = std::env::temp_dir().join("tgws-bridge-doc-config");
/// fs::create_dir_all(&dir).unwrap();
/// let path = dir.join("config.json");
/// fs::write(&path, r#"{"host":"127.0.0.1","port":1443,"secret":"dd00112233445566778899aabbccddeeff"}"#).unwrap();
/// let proxy = read_config(&path).unwrap();
/// assert_eq!(proxy.port, 1443);
/// assert!(proxy.secret.is_some());
/// ```
pub fn read_config(path: &Path) -> Option<LocalProxy> {
    let raw = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;

    let host = value
        .get("host")
        .and_then(|host| host.as_str())
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .unwrap_or(DEFAULT_HOST)
        .to_string();

    // Some builds store the port as a string.
    let port = value
        .get("port")
        .and_then(|port| {
            port.as_u64()
                .and_then(|port| u16::try_from(port).ok())
                .or_else(|| port.as_str().and_then(|port| port.trim().parse().ok()))
        })
        .unwrap_or(DEFAULT_PORT);

    let secret = value
        .get("secret")
        .and_then(|secret| secret.as_str())
        .and_then(|secret| parse_secret(secret).ok());

    Some(LocalProxy {
        host,
        port,
        secret,
        source: Some(path.to_path_buf()),
    })
}

/// Looks for a `TgWsProxy` configuration in the usual place.
///
/// Returns `None` when no configuration file exists; see
/// [`LocalProxy::defaults`] if you want to try the default address anyway.
pub fn autodetect() -> Option<LocalProxy> {
    let path = config_path()?;
    read_config(&path)
}

/// Looks at one directory instead of the platform default, e.g.
/// `autodetect_in("C:/Tools/TgWsProxy")` after an unpacked install.
pub fn autodetect_in(dir: &Path) -> Option<LocalProxy> {
    read_config(&dir.join("config.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, body: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, body).unwrap();
        path
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tgws-bridge-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn reads_host_port_and_secret() {
        let dir = temp("config-full");
        let path = write(
            &dir,
            r#"{"host":"127.0.0.1","port":1443,"secret":"dd00112233445566778899aabbccddeeff"}"#,
        );
        let proxy = read_config(&path).unwrap();
        assert_eq!(proxy.socks_url(), "socks5://127.0.0.1:1443");
        assert_eq!(proxy.secret.unwrap()[0], 0x00);
        assert_eq!(proxy.secret.unwrap()[15], 0xff);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn falls_back_to_defaults() {
        let dir = temp("config-minimal");
        let path = write(&dir, r#"{"secret":"00112233445566778899aabbccddeeff"}"#);
        let proxy = read_config(&path).unwrap();
        assert_eq!(proxy.host, "127.0.0.1");
        assert_eq!(proxy.port, 1443);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accepts_a_stringy_port_and_broken_secret() {
        let dir = temp("config-stringy");
        let path = write(&dir, r#"{"port":"1444","secret":"nonsense"}"#);
        let proxy = read_config(&path).unwrap();
        assert_eq!(proxy.port, 1444);
        assert!(proxy.secret.is_none());
        assert!(proxy.secret().is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_none() {
        assert!(read_config(&temp("config-absent").join("config.json")).is_none());
    }

    #[test]
    fn describe_mentions_the_source() {
        let proxy = LocalProxy::defaults(Some([0; 16]));
        assert!(proxy.describe().starts_with("socks5://127.0.0.1:1443"));
        assert!(proxy.describe().contains("secret found"));
    }
}