//! Маршрут подключения к Telegram через локальный MTProto-прокси.
//!
//! grammers умеет ходить только через SOCKS5, а MTProto-прокси (в том числе
//! локальный TgWsProxy, <https://github.com/Flowseal/tg-ws-proxy>) принимает
//! входные подключения с obfuscated2-секретом. Мост живёт в отдельном крейте
//! [`tgws_bridge`] (<https://crates.io/crates/tgws-bridge>): сам поднимает
//! SOCKS5-слушатель, по адресу назначения из SOCKS-запроса выбирает DC,
//! делает рукопожатие и перекладывает кадры в WSS.
//!
//! Здесь остались только функции, нужные боту: разбор секрета, поиск
//! запущенного TgWsProxy, проба порта, диагностический зонд и запуск моста.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use std::net::SocketAddr;

use tokio::net::TcpStream;

/// Чем оказался локальный порт из `config.json`.
pub enum LocalKind {
    /// Порт отвечает SOCKS5-приветствием без пароля — можно отдать адрес grammers как есть.
    Socks5,
    /// Порт открыт, но SOCKS5 не здоровается: похоже на MTProto-прокси (нужен секрет).
    Open,
    /// Порт закрыт или не отвечает — локального прокси нет.
    Closed,
}

impl LocalKind {
    /// Человеческое описание для лога и `--check`.
    pub fn describe(self, host: &str, port: u16) -> String {
        match self {
            Self::Socks5 => {
                format!("{host}:{port} отвечает как SOCKS5 — ходим через него напрямую")
            }
            Self::Open => format!(
                "{host}:{port} открыт, но SOCKS5 не отвечает: это MTProto-прокси, ему нужен mtproxy.secret"
            ),
            Self::Closed => format!("{host}:{port} закрыт — локального прокси нет"),
        }
    }
}

/// Живые мосты: `tgws_bridge::Bridge` останавливается на `Drop`, поэтому
/// каждый поднятый мост живёт здесь до конца процесса.
static BRIDGES: std::sync::Mutex<Vec<tgws_bridge::Bridge>> = std::sync::Mutex::new(Vec::new());

/// Последняя причина отказа моста (для сообщения в `--check` и в GUI).
pub fn last_reason() -> Option<String> {
    let bridges = BRIDGES.lock().ok()?;
    bridges.iter().rev().find_map(|bridge| bridge.last_reason())
}

/// Настройки локального TgWsProxy (Flowseal/tg-ws-proxy): адрес, порт и
/// секрет из его собственного `config.json` (`%APPDATA%\TgWsProxy\config.json`).
/// Нужно, чтобы бот подключался к уже запущенному прокси, даже если в
/// `config.json` бота `mtproxy.tgws` пуст и порт отличается от стандартного.
pub fn tgws_local() -> Option<(String, u16, [u8; 16])> {
    let proxy = tgws_bridge::autodetect()?;
    Some((proxy.host, proxy.port, proxy.secret?))
}

/// Адрес локального TgWsProxy из настроек бота (`mtproxy.tgws`), иначе —
/// найденный автоматически, если это разрешено (`mtproxy.tgws_local`).
/// Пусто и `off`/`none` — плагин выключен.
pub fn tgws_target(cfg: &crate::config::Config) -> Option<SocketAddr> {
    let direct = cfg.mtproxy.tgws.trim().to_string();
    if !direct.is_empty() {
        let off = direct.eq_ignore_ascii_case("off") || direct.eq_ignore_ascii_case("none");
        if off {
            return None;
        }
        let text = if direct.contains(':') {
            direct
        } else {
            format!("{direct}:{}", crate::config::default_tgws_port())
        };
        if let Ok(addr) = text.parse() {
            return Some(addr);
        }
        eprintln!("[tgws] адрес mtproxy.tgws «{text}» не разобран, беру автоопределение");
    }
    if !cfg.mtproxy.tgws_local {
        return None;
    }
    let (host, port, _) = tgws_local()?;
    Some(SocketAddr::new(host.parse().ok()?, port))
}

/// Секрет локального TgWsProxy: явный из `mtproxy.tgws_secret`, иначе из
/// его собственного `config.json`.
pub fn tgws_secret(cfg: &crate::config::Config) -> Option<[u8; 16]> {
    let direct = cfg.mtproxy.tgws_secret.trim();
    if !direct.is_empty() {
        return parse_secret(direct).ok();
    }
    if !cfg.mtproxy.tgws_local {
        return None;
    }
    tgws_local().map(|(_, _, secret)| secret)
}

/// Разбор секрета MTProto-прокси (hex, с префиксом `dd`/`ee` или без).
pub fn parse_secret(raw: &str) -> Result<[u8; 16], String> {
    tgws_bridge::parse_secret(raw).map_err(|e| e.to_string())
}

/// Проба локального прокси: сначала TCP-коннект, затем SOCKS5-приветствие
/// (`05 01 00` → ждём `05 00`). Ничего не логирует, зовётся и из `--check`,
/// и при выборе маршрута подключения.
pub async fn probe_local(host: &str, port: u16) -> LocalKind {
    let addr = format!("{host}:{port}");
    let connect = tokio::time::timeout(Duration::from_millis(1500), TcpStream::connect(&addr)).await;
    let mut stream = match connect {
        Ok(Ok(stream)) => stream,
        _ => return LocalKind::Closed,
    };

    // Метод «без авторизации»; сервер обязан ответить версией и выбранным методом.
    if stream.write_all(&[0x05, 0x01, 0x00]).await.is_err() {
        return LocalKind::Open;
    }
    let mut reply = [0u8; 2];
    let read = tokio::time::timeout(Duration::from_millis(1200), stream.read_exact(&mut reply)).await;
    match read {
        Ok(Ok(_)) if reply == [0x05, 0x00] => LocalKind::Socks5,
        _ => LocalKind::Open,
    }
}

/// Диагностический зонд: отправить прокси obfuscated2-рукопожатие с заданным
/// секретом и замерить, как быстро он отвечает.
pub async fn probe(target: SocketAddr, secret: [u8; 16]) -> String {
    tgws_bridge::probe(target, secret)
        .await
        .unwrap_or_else(|e| format!("проба {target} не удалась: {e}"))
}

/// Запускает локальный SOCKS5-мост и возвращает его адрес (`socks5://...`).
pub async fn spawn_socks_bridge(
    target_addr: SocketAddr,
    secret: [u8; 16],
) -> Result<SocketAddr, String> {
    let bridge = tgws_bridge::Bridge::spawn(
        tgws_bridge::BridgeConfig::new(target_addr, secret).with_log(|line| eprintln!("{line}")),
    )
    .await
    .map_err(|e| format!("не удалось поднять мост к {target_addr}: {e}"))?;
    let local = bridge.local_addr();
    if let Ok(mut bridges) = BRIDGES.lock() {
        bridges.push(bridge);
    }
    Ok(local)
}