//! Локальный SOCKS5-мост к MTProto-прокси (стандартный порт 8444).
//!
//! grammers умеет ходить только через SOCKS5, а TgWsProxy принимает входные
//! MTProto-подключения с dd-секретом (obfuscated2, AES-256-CTR). Этот модуль
//! поднимает собственный SOCKS5-слушатель внутри процесса: grammers подключается
//! к нему, а мост сам устанавливает obfuscated2-соединение с настоящим
//! MTProto-прокси, выбирая DC по адресу назначения из SOCKS-запроса.
//!
//! Клиентская сторона рукопожатия перенесена с адаптацией из crypto.rs
//! проекта tg-ws-proxy-rs (MIT, Copyright valnesfjord/tg-ws-proxy-rs authors):
//! https://github.com/valnesfjord/tg-ws-proxy-rs

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use aes::Aes256;
use cipher::{KeyIvInit, StreamCipher};
use ctr::Ctr128BE;
use futures_util::{SinkExt as _, StreamExt as _};
use rand::RngCore;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

/// Открытый WSS-туннель (воркер или CF-прокси).
type WsStream = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;


/// Параметры пула из `config.json` (`mtproxy.pool_size` и соседи).
static POOL_SIZE: AtomicUsize = AtomicUsize::new(2);
// По умолчанию 20: стандартный пул tg-ws-proxy — 20 доменов, и весь он
// пробуется одним забегом, т.е. время до ошибки — один таймаут, а не 20.
static PROBE_PARALLEL: AtomicUsize = AtomicUsize::new(20);
static SCORE_TTL_SECS: AtomicU64 = AtomicU64::new(600);

/// Применить настройки балансировщика и пула из конфига.
pub fn configure(mt: &crate::config::MtProxyConfig) {
    POOL_SIZE.store(mt.pool_size.min(8), Ordering::Relaxed);
    PROBE_PARALLEL.store(mt.probe_parallel.clamp(1, 32), Ordering::Relaxed);
    SCORE_TTL_SECS.store(mt.score_ttl.max(30), Ordering::Relaxed);
}

/// Сколько готовых туннелей держать на DC (0 — пул выключен).
fn pool_size() -> usize {
    POOL_SIZE.load(Ordering::Relaxed)
}

/// Сколько доменов пробовать одновременно в одном забеге.
fn probe_parallel() -> usize {
    PROBE_PARALLEL.load(Ordering::Relaxed).max(1)
}

/// Сколько секунд живёт замер (для таблицы и `--check`).
pub fn score_ttl_secs() -> u64 {
    SCORE_TTL_SECS.load(Ordering::Relaxed)
}

/// Сколько живёт замер задержки домена.
fn score_ttl() -> Duration {
    Duration::from_secs(SCORE_TTL_SECS.load(Ordering::Relaxed))
}

/// Записать в балансировщик исход рукопожатия с доменом.
fn record_score(dc: i16, domain: &str, handshake: Option<Duration>) {
    let Ok(mut scores) = crate::balancer::global().lock() else {
        return;
    };
    let now = crate::balancer::now();
    let ttl = score_ttl();
    match handshake {
        Some(elapsed) => scores.record_success(dc, domain, elapsed, now, ttl),
        None => scores.record_failure(dc, domain, now, ttl),
    }
}

/// Домен-лидер по счёту для DC, если замер уже есть.
fn best_candidate(dc: i16) -> Option<String> {
    crate::balancer::global()
        .lock()
        .ok()
        .and_then(|scores| scores.best(dc, crate::balancer::now(), score_ttl()))
        .map(|(domain, _)| domain)
}

/// План попытки: сначала лидер по счёту (если он есть в списке и на него
/// есть свежий замер), остальные домены — для забега.
fn attempt_plan(dc: i16, mut order: Vec<String>) -> (Option<String>, Vec<String>) {
    let Some(leader) = best_candidate(dc) else {
        return (None, order);
    };
    match order.iter().position(|domain| domain == &leader) {
        Some(idx) => {
            order.remove(idx);
            (Some(leader), order)
        }
        None => (None, order),
    }
}

/// Проверить, что пулу есть куда идти: пустой список — понятная ошибка.
fn domains_for(dc: i16, domains: Vec<String>) -> Result<Vec<String>, String> {
    if domains.is_empty() {
        return Err(format!("список CF-прокси пуст, DC{dc} некуда слать"));
    }
    Ok(domains)
}

/// Прогреть счёт перед показом таблицы (использует `--check`): делает по
/// одному забегу на DC и отпускает туннели.
pub async fn score_pool(dcs: &[i16]) {
    let attempts = dcs.iter().map(|dc| async move {
        let _ = pool().connect_one(*dc).await;
    });
    futures_util::future::join_all(attempts).await;
}
/// Стандартные CF-прокси из Flowseal/tg-ws-proxy (`proxy/config.py`,
/// список `_CFPROXY_ENC`) — те же домены после `_dd()`: буквы каждой метки
/// сдвинуты назад на их количество, TLD заменён на `.co.uk`.
/// Свой Cloudflare-worker для них не нужен.
pub const CF_PROXY_DOMAINS: &[&str] = &[
    "pclead.co.uk",
    "offshor.co.uk",
    "cakeisalie.co.uk",
    "noskomnadzor.co.uk",
    "lovetrue.co.uk",
    "sorokdva.co.uk",
    "pyatdesyatdva.co.uk",
    "kartoshka.co.uk",
    "sorokodin.co.uk",
    "pyatdesyatodin.co.uk",
    "notelega.co.uk",
    "ebally.co.uk",
    "nebally.co.uk",
    "havegreatday.co.uk",
    "pomogite.co.uk",
    "fixtelega.co.uk",
    "sadnews.co.uk",
    "onedaychamp.co.uk",
    "stopblocking.co.uk",
    "nothingthere.co.uk",
];

/// Список доменов, по которому ходит пул: стартуем с зашитых, потом
/// подменяем свежими из репозитория tg-ws-proxy.
static CF_DOMAINS: OnceLock<std::sync::Mutex<Vec<String>>> = OnceLock::new();

fn cf_domains_store() -> &'static std::sync::Mutex<Vec<String>> {
    CF_DOMAINS.get_or_init(|| {
        std::sync::Mutex::new(CF_PROXY_DOMAINS.iter().map(|d| d.to_string()).collect())
    })
}

/// Текущий список доменов пула.
pub fn cf_domains() -> Vec<String> {
    cf_domains_store().lock().unwrap().clone()
}

/// Принять свежий список (как ежечасный refresh в tg-ws-proxy).
/// Возвращает число принятых доменов; при мусоре список не меняется.
pub fn set_cf_domains(list: Vec<String>) -> usize {
    let mut cleaned: Vec<String> = Vec::new();
    for item in list {
        let d = item.trim().to_ascii_lowercase();
        let ok = d.len() > 3
            && d.len() < 254
            && d.contains('.')
            && !d.starts_with('.')
            && !d.ends_with('.')
            && d.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
        if !ok || cleaned.iter().any(|x| *x == d) {
            continue;
        }
        cleaned.push(d);
    }
    // Апстрим тоже держится правила «минимум три живых домена».
    if cleaned.len() < 3 {
        return 0;
    }
    let count = cleaned.len();
    *cf_domains_store().lock().unwrap() = cleaned;
    count
}

/// Разобрать список доменов: по строке на домен, `#` — комментарий.
/// Принимаем и «закодированный» вид апстрима (`xxx.com`), и готовый `.co.uk`.
pub fn parse_cf_domains(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(decode_cf_domain_line)
        .collect()
}

/// `.github/cfproxy-domains.txt` апстрима содержит кодированные имена.
fn decode_cf_domain_line(line: &str) -> String {
    if !line.ends_with(".com") {
        return line.to_string();
    }
    let label = &line[..line.len() - 4];
    let shift = (label.chars().filter(|c| c.is_ascii_alphabetic()).count() % 26) as u32;
    let mut out = String::with_capacity(line.len());
    for ch in label.chars() {
        if ch.is_ascii_alphabetic() {
            let base = if ch.is_ascii_lowercase() { b'a' } else { b'A' } as u32;
            let idx = (ch as u32 - base + 26 - shift) % 26;
            out.push(char::from_u32(base + idx).unwrap_or(ch));
        } else {
            out.push(ch);
        }
    }
    out.push_str(".co.uk");
    out
}

/// Результат проверки пула: живой домен (если есть) и причины отказов.
pub struct PoolProbe {
    pub live: Option<String>,
    pub reasons: Vec<String>,
}

/// Как апстрим: список доменов берём из `.github/cfproxy-domains.txt`
/// репозитория tg-ws-proxy — он обновляется там, домены протухают.
const CF_DOMAINS_URL_PATH: &str = "/Flowseal/tg-ws-proxy/main/.github/cfproxy-domains.txt";
const CF_DOMAINS_URL_HOST: &str = "raw.githubusercontent.com";

/// Один HTTPS GET (без сторонних HTTP-клиентов: TLS есть под рукой).
async fn https_get(host: &str, path: &str, timeout: Duration) -> Result<String, String> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|e| format!("имя {host}: {e}"))?;
    let tcp = TcpStream::connect((host, 443))
        .await
        .map_err(|e| format!("TCP {host}:443: {e}"))?;
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let mut tls = connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| format!("TLS {host}: {e}"))?;
    let req = format!(
        "GET {path} HTTP/1.1
Host: {host}
User-Agent: gorilla-bot
Connection: close

"
    );
    tls.write_all(req.as_bytes())
        .await
        .map_err(|e| format!("запрос {host}: {e}"))?;
    let mut raw = Vec::new();
    let _ = tokio::time::timeout(timeout, tls.read_to_end(&mut raw)).await;
    let text = String::from_utf8_lossy(&raw).to_string();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| format!("{host}: пустой ответ"))?;
    let status = head.lines().next().unwrap_or_default().to_string();
    if !status.contains(" 200 ") {
        return Err(format!("{host}: {status}"));
    }
    // `Connection: close` + chunked бывает; для списка доменов это текст.
    Ok(body.to_string())
}

/// Обновить список доменов пула из репозитория tg-ws-proxy.
/// Возвращает число принятых доменов; при сбое список остаётся прежним.
pub async fn refresh_cf_domains() -> Result<usize, String> {
    let text = https_get(
        CF_DOMAINS_URL_HOST,
        CF_DOMAINS_URL_PATH,
        Duration::from_secs(8),
    )
    .await?;
    let list = parse_cf_domains(&text);
    let accepted = set_cf_domains(list);
    if accepted == 0 {
        return Err(format!(
            "список из репозитория не принят (доменов в ответе: {})",
            text.lines().filter(|l| !l.trim().is_empty()).count()
        ));
    }
    Ok(accepted)
}

/// Жив ли стандартный пул из этой сети. Cloudflare маршрутизирует по SNI,
/// поэтому честный тест один: попробовать WSS-рукопожатие к нескольким
/// доменам. Причины отказов возвращаем наружу — их печатает `--check`.
pub async fn cf_pool_probe() -> PoolProbe {
    let domains = cf_domains();
    let mut reasons = Vec::new();
    if domains.is_empty() {
        reasons.push("список доменов пула пуст".to_string());
        return PoolProbe {
            live: None,
            reasons,
        };
    }
    let step = (domains.len() / 4).max(1);
    let picks: Vec<String> = domains.iter().step_by(step).take(4).cloned().collect();
    let mut tasks = Vec::new();
    for domain in picks {
        tasks.push(async move {
            let host = cf_proxy_host(&domain, 2);
            let url = format!("wss://{host}/apiws");
            match tokio::time::timeout(
                Duration::from_secs(6),
                connect_ws(&url, "таймаут WSS-подключения"),
            )
            .await
            {
                Ok(Ok(_ws)) => Ok(host),
                Ok(Err(e)) => Err(format!("{host}: {e}")),
                Err(_) => Err(format!("{host}: нет ответа за 6 c (TLS/WS)")),
            }
        });
    }
    for found in futures_util::future::join_all(tasks).await {
        match found {
            Ok(host) if !host.is_empty() => {
                return PoolProbe {
                    live: Some(host),
                    reasons,
                }
            }
            Ok(_) => {}
            Err(reason) => reasons.push(reason),
        }
    }
    PoolProbe {
        live: None,
        reasons,
    }
}

pub type AesCtr256 = Ctr128BE<Aes256>;

const HANDSHAKE_LEN: usize = 64;
const SKIP_LEN: usize = 8;
const PREKEY_LEN: usize = 32;
const IV_LEN: usize = 16;
const PROTO_TAG_POS: usize = 56;
const DC_IDX_POS: usize = 60;

/// Внутренний транспорт внутри обфускации: intermediate (0xee).
/// grmers работает с ним, и MTProto-прокси его поддерживает.
const PROTO_TAG_INTERMEDIATE: [u8; 4] = [0xee, 0xee, 0xee, 0xee];

const PROTO_TAG_ABRIDGED: [u8; 4] = [0xef, 0xef, 0xef, 0xef];
const RESERVED_FIRST_BYTES: &[u8] = &[0xef];
const RESERVED_STARTS: &[[u8; 4]] = &[
    *b"HEAD",
    *b"POST",
    *b"GET ",
    [0xee, 0xee, 0xee, 0xee],
    [0xdd, 0xdd, 0xdd, 0xdd],
    [0x16, 0x03, 0x01, 0x02], // TLS ClientHello
];

/// IPv4-адреса, которые grammers может запросить, → знаковый индекс DC
/// (отрицательный — медиа-DC). Объединение встроенных адресов grammers
/// (src/dc_options.rs) и карты из tg-ws-proxy-rs (src/inbound/socks.rs).
const DC_IPS: &[(i16, Ipv4Addr)] = &[
    (1, ipv4(149, 154, 175, 50)),
    (1, ipv4(149, 154, 175, 51)),
    (1, ipv4(149, 154, 175, 53)),
    (1, ipv4(149, 154, 175, 54)),
    (-1, ipv4(149, 154, 175, 52)),
    (2, ipv4(149, 154, 167, 41)),
    (2, ipv4(149, 154, 167, 50)),
    (2, ipv4(149, 154, 167, 51)),
    (2, ipv4(149, 154, 167, 220)),
    (2, ipv4(95, 161, 76, 100)),
    (-2, ipv4(149, 154, 167, 151)),
    (-2, ipv4(149, 154, 167, 222)),
    (-2, ipv4(149, 154, 167, 223)),
    (-2, ipv4(149, 154, 162, 123)),
    (3, ipv4(149, 154, 175, 100)),
    (3, ipv4(149, 154, 175, 101)),
    (-3, ipv4(149, 154, 175, 102)),
    (4, ipv4(149, 154, 167, 91)),
    (4, ipv4(149, 154, 167, 92)),
    (-4, ipv4(149, 154, 164, 250)),
    (-4, ipv4(149, 154, 166, 120)),
    (-4, ipv4(149, 154, 166, 121)),
    (-4, ipv4(149, 154, 167, 118)),
    (-4, ipv4(149, 154, 165, 111)),
    (-4, ipv4(149, 154, 165, 136)),
    (5, ipv4(91, 108, 56, 100)),
    (5, ipv4(91, 108, 56, 101)),
    (5, ipv4(91, 108, 56, 104)),
    (5, ipv4(91, 108, 56, 116)),
    (5, ipv4(91, 108, 56, 126)),
    (5, ipv4(149, 154, 171, 5)),
    (-5, ipv4(91, 108, 56, 102)),
    (-5, ipv4(91, 108, 56, 128)),
    (-5, ipv4(91, 108, 56, 147)),
    (-5, ipv4(91, 108, 56, 151)),
];

const fn ipv4(a: u8, b: u8, c: u8, d: u8) -> Ipv4Addr {
    Ipv4Addr::new(a, b, c, d)
}

/// Настоящий MTProto-прокси, к которому мост пробивается.
#[derive(Clone)]
struct Target {
    addr: SocketAddr,
    secret: Arc<[u8; 16]>,
}

/// Разбор секрета: «dd<32hex>» или чистые 32 hex-символа.
pub fn parse_secret(raw: &str) -> Result<[u8; 16], String> {
    let raw = raw.trim();
    let hex = if raw.len() == 34 && (raw.starts_with("dd") || raw.starts_with("ee")) {
        &raw[2..]
    } else {
        raw
    };
    let bytes: Result<Vec<u8>, _> = (0..16)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16))
        .collect();
    let bytes = bytes.map_err(|_| format!("не удалось разобрать секрет «{raw}»"))?;
    bytes
        .try_into()
        .map_err(|_| "секрет должен быть 16 байт".into())
}

fn make_cipher(key: &[u8], iv: &[u8]) -> AesCtr256 {
    AesCtr256::new_from_slices(key, iv).expect("ключ 32 байта и IV 16 байт")
}

/// Генерация 64-байтового обфусцированного рукопожатия для вышестоящего
/// MTProto-прокси плюс два шифра сессии (отправка/приём).
/// Перенесено с адаптацией из tg-ws-proxy-rs/src/crypto.rs (MIT).
fn generate_client_handshake(
    secret: &[u8; 16],
    dc_idx: i16,
) -> ([u8; HANDSHAKE_LEN], AesCtr256, AesCtr256) {
    let dc_bytes = dc_idx.to_le_bytes();

    loop {
        let mut raw = [0u8; HANDSHAKE_LEN];
        rand::rng().fill_bytes(&mut raw);

        if RESERVED_FIRST_BYTES.contains(&raw[0]) {
            continue;
        }
        if RESERVED_STARTS.iter().any(|s| *s == raw[..4]) {
            continue;
        }
        if raw[4..8] == [0; 4] {
            continue;
        }

        // Ключ выводится из «сырых» байтов [8..56] и секрета.
        let mut key = Sha256::new();
        key.update(&raw[SKIP_LEN..SKIP_LEN + PREKEY_LEN]);
        key.update(secret);
        let key: [u8; 32] = key.finalize().into();
        let iv = &raw[SKIP_LEN + PREKEY_LEN..SKIP_LEN + PREKEY_LEN + IV_LEN];

        // Полный keystream (шифрование нулей).
        let mut keystream = [0u8; HANDSHAKE_LEN];
        make_cipher(&key, iv).apply_keystream(&mut keystream);

        let mut handshake = raw;
        for i in 0..4 {
            handshake[PROTO_TAG_POS + i] = PROTO_TAG_INTERMEDIATE[i] ^ keystream[PROTO_TAG_POS + i];
        }
        for i in 0..2 {
            handshake[DC_IDX_POS + i] = dc_bytes[i] ^ keystream[DC_IDX_POS + i];
        }
        // handshake[62..64] — случайные, прокси их не трактует.

        // Шифр отправки: те же ключ и IV, промотанные на 64 байта рукопожатия.
        let mut enc_key = Sha256::new();
        enc_key.update(&handshake[SKIP_LEN..SKIP_LEN + PREKEY_LEN]);
        enc_key.update(secret);
        let enc_key: [u8; 32] = enc_key.finalize().into();
        let mut enc = make_cipher(
            &enc_key,
            &handshake[SKIP_LEN + PREKEY_LEN..SKIP_LEN + PREKEY_LEN + IV_LEN],
        );
        let mut dummy = [0u8; HANDSHAKE_LEN];
        enc.apply_keystream(&mut dummy);

        // Шифр приёма: перевёрнутые prekey+IV, без промотки.
        let mut reversed = [0u8; PREKEY_LEN + IV_LEN];
        reversed.copy_from_slice(&handshake[SKIP_LEN..SKIP_LEN + PREKEY_LEN + IV_LEN]);
        reversed.reverse();
        let mut dec_key = Sha256::new();
        dec_key.update(&reversed[..PREKEY_LEN]);
        dec_key.update(secret);
        let dec_key: [u8; 32] = dec_key.finalize().into();
        let dec = make_cipher(&dec_key, &reversed[PREKEY_LEN..]);

        return (handshake, enc, dec);
    }
}

/// Транспорт MTProto, которым клиент общается с DC.
#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Abridged — запасной вариант, grammers ходит intermediate
enum Proto {
    Abridged,
    Intermediate,
}

/// Обфусцированное рукопожатие для релея через WSS: ключ берётся из самого
/// заголовка, без хеша секрета (так делает tg-ws-proxy: `relay_init`).
fn generate_relay_handshake(dc_idx: i16) -> ([u8; HANDSHAKE_LEN], AesCtr256, AesCtr256) {
    let dc_bytes = dc_idx.to_le_bytes();
    let proto_tag = PROTO_TAG_INTERMEDIATE;

    loop {
        let mut handshake = [0u8; HANDSHAKE_LEN];
        rand::rng().fill_bytes(&mut handshake);

        if RESERVED_FIRST_BYTES.contains(&handshake[0]) {
            continue;
        }
        if RESERVED_STARTS
            .iter()
            .any(|start| handshake[..4] == start[..])
        {
            continue;
        }

        let mut enc = make_cipher(
            &handshake[SKIP_LEN..SKIP_LEN + PREKEY_LEN],
            &handshake[SKIP_LEN + PREKEY_LEN..SKIP_LEN + PREKEY_LEN + IV_LEN],
        );
        // Промотка на длину заголовка: полезная нагрузка начинается с 64-го байта.
        let mut keystream = [0u8; HANDSHAKE_LEN];
        enc.apply_keystream(&mut keystream);

        // Шифр от DC: перевёрнутые prekey+IV, без промотки и без хеша.
        let mut reversed = [0u8; PREKEY_LEN + IV_LEN];
        reversed.copy_from_slice(&handshake[SKIP_LEN..SKIP_LEN + PREKEY_LEN + IV_LEN]);
        reversed.reverse();
        let dec = make_cipher(&reversed[..PREKEY_LEN], &reversed[PREKEY_LEN..]);

        // Тег (4 байта) и DC (i16 LE) в конце заголовка, остальное — мусор.
        let mut tail = [0u8; 6];
        tail[..4].copy_from_slice(&proto_tag);
        tail[4..].copy_from_slice(&dc_bytes);
        for (i, byte) in tail.iter().enumerate() {
            handshake[PROTO_TAG_POS + i] = byte ^ keystream[PROTO_TAG_POS + i];
        }

        return (handshake, enc, dec);
    }
}

/// Что отвечает на `host:port` локального прокси.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
            Self::Socks5 => format!("{host}:{port} отвечает как SOCKS5 — ходим через него напрямую"),
            Self::Open => format!(
                "{host}:{port} открыт, но SOCKS5 не отвечает: это MTProto-прокси, ему нужен mtproxy.secret"
            ),
            Self::Closed => format!("{host}:{port} закрыт — локального прокси нет"),
        }
    }
}

/// Последняя причина, по которой мост не смог получить живой ответ.
static LAST_REASON: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub fn last_reason() -> Option<String> {
    LAST_REASON.lock().ok().and_then(|slot| slot.clone())
}

fn remember_reason(text: String) {
    if let Ok(mut slot) = LAST_REASON.lock() {
        *slot = Some(text);
    }
}

/// Настройки локального TgWsProxy (Flowseal/tg-ws-proxy): адрес, порт и
/// секрет из его собственного `config.json` (`%APPDATA%\TgWsProxy\config.json`).
/// Нужно, чтобы бот подключался к уже запущенному прокси, даже если в
/// `config.json` бота `mtproxy.secret` пуст и порт отличается от стандартного.
///
/// Домен собственного Cloudflare-worker из настроек TgWsProxy
/// (`cfproxy_worker_domain[0]`): публичный пул `kws*.*` часто не отвечает,
/// а свой воркер у того же пользователя работает.
///
/// Воркер из настроек TgWsProxy: `(домен, включён ли он в самом TgWsProxy)`.
///
/// Второй элемент важен: если пользователь выключил свой воркер
/// (`cfproxy_worker_enabled: false`), молча ходить через него нельзя —
/// решение принимает вызывающий и говорит об этом в логе.
pub fn tgws_worker_domain() -> Option<(String, bool)> {
    let base = std::env::var_os("APPDATA")?;
    let path = std::path::PathBuf::from(base)
        .join("TgWsProxy")
        .join("config.json");
    let raw = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let domains = value.get("cfproxy_worker_domain")?;
    let first = match domains {
        serde_json::Value::Array(items) => items.first()?.as_str()?.to_string(),
        serde_json::Value::String(text) => text.clone(),
        _ => return None,
    };
    let trimmed = first
        .trim()
        .trim_start_matches("https://")
        .trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    let enabled = value
        .get("cfproxy_worker_enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    Some((trimmed.to_string(), enabled))
}

pub fn tgws_local() -> Option<(String, u16, [u8; 16])> {
    let base = std::env::var_os("APPDATA")?;
    let path = std::path::PathBuf::from(base)
        .join("TgWsProxy")
        .join("config.json");
    let raw = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let port = u16::try_from(value.get("port")?.as_u64()?).ok()?;
    let host = value
        .get("host")
        .and_then(|host| host.as_str())
        .unwrap_or("127.0.0.1")
        .to_string();
    let secret = parse_secret(value.get("secret")?.as_str()?).ok()?;
    Some((host, port, secret))
}

/// Проба локального прокси: сначала TCP-коннект, затем SOCKS5-приветствие
/// (`05 01 00` → ждём `05 00`). Ничего не логирует, зовётся и из `--check`,
/// и при выборе маршрута подключения.
pub async fn probe_local(host: &str, port: u16) -> LocalKind {
    let addr = format!("{host}:{port}");
    let connect = tokio::time::timeout(
        std::time::Duration::from_millis(1500),
        TcpStream::connect(&addr),
    )
    .await;
    let mut stream = match connect {
        Ok(Ok(stream)) => stream,
        _ => return LocalKind::Closed,
    };

    // Метод «без авторизации»; сервер обязан ответить версией и выбранным методом.
    if stream.write_all(&[0x05, 0x01, 0x00]).await.is_err() {
        return LocalKind::Open;
    }
    let mut reply = [0u8; 2];
    let read = tokio::time::timeout(
        std::time::Duration::from_millis(1200),
        stream.read_exact(&mut reply),
    )
    .await;
    match read {
        Ok(Ok(_)) if reply == [0x05, 0x00] => LocalKind::Socks5,
        _ => LocalKind::Open,
    }
}

/// Диагностический зонд: отправить прокси obfuscated2-рукопожатие с заданным
/// секретом и замерить, как быстро он закрывает соединение.
/// Мгновенный EOF — режим/секрет не приняты (Close); долгое молчание —
/// неверный секрет (Drain: прокси вычитывает мусор, не отвечая).
pub async fn probe(target: SocketAddr, secret: [u8; 16]) -> String {
    let (init, _enc, _dec) = generate_client_handshake(&secret, 2);

    let mut stream = match TcpStream::connect(target).await {
        Ok(s) => s,
        Err(e) => return format!("не удалось подключиться к {target}: {e}"),
    };
    let start = tokio::time::Instant::now();
    if let Err(e) = stream.write_all(&init).await {
        return format!("не удалось отправить рукопожатие: {e}");
    }

    let mut buf = [0u8; 512];
    tokio::select! {
        r = stream.read(&mut buf) => match r {
            Ok(0) => format!(
                "EOF через {:?} — прокси ПРИНЯЛ подключение и закрыл его (Close, вероятно проблема выше по цепочке)",
                start.elapsed()
            ),
            Ok(n) => format!(
                "ОТВЕТ: {n} байт через {:?} — прокси жив и отвечает (секрет верен)",
                start.elapsed()
            ),
            Err(e) => format!("ошибка чтения через {:?}: {e}", start.elapsed()),
        },
        _ = tokio::time::sleep(std::time::Duration::from_secs(12)) => {
            "МОЛЧИТ >12 c (Drain) — секрет неверен: прокси вычитывает мусор, не отвечая".to_string()
        }
    }
}

/// Запускает локальный SOCKS5-мост и возвращает его адрес (`socks5://...`).
pub async fn spawn_socks_bridge(
    target_addr: SocketAddr,
    secret: [u8; 16],
) -> Result<SocketAddr, String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("не удалось занять локальный порт под мост: {e}"))?;
    let local = listener
        .local_addr()
        .map_err(|e| format!("адрес моста: {e}"))?;

    let target = Arc::new(Target {
        addr: target_addr,
        secret: Arc::new(secret),
    });

    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let target = Arc::clone(&target);
                    tokio::spawn(async move {
                        if let Err(e) = serve_client(stream, &target).await {
                            eprintln!("[мост] соединение закрыто: {e}");
                        }
                    });
                }
                Err(e) => {
                    eprintln!("[мост] приём подключений: {e}");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }
    });

    println!("[мост] локальный SOCKS5: {local} → MTProto {target_addr}");
    Ok(local)
}

fn dc_by_ip(ip: IpAddr) -> Option<i16> {
    match ip {
        IpAddr::V4(v4) => DC_IPS
            .iter()
            .find(|(_, known)| *known == v4)
            .map(|(dc, _)| *dc),
        IpAddr::V6(_) => None,
    }
}

async fn socks_reply(stream: &mut TcpStream, code: u8) -> Result<(), String> {
    stream
        .write_all(&[5, code, 0, 1, 0, 0, 0, 0, 0, 0])
        .await
        .map_err(|e| format!("socks-ответ: {e}"))
}

/// Принимает SOCKS5-рукопожатие и CONNECT-запрос, отвечает клиенту успехом.
/// Возвращает IP назначения и знаковый индекс DC (отрицательный — медиа).
async fn socks_accept(client: &mut TcpStream) -> Result<(IpAddr, i16), String> {
    // --- SOCKS5 greeting ---
    let mut greeting = [0; 2];
    client
        .read_exact(&mut greeting)
        .await
        .map_err(|e| format!("greeting: {e}"))?;
    if greeting[0] != 5 {
        return Err("не SOCKS5-клиент".into());
    }
    let mut methods = vec![0u8; usize::from(greeting[1])];
    client
        .read_exact(&mut methods)
        .await
        .map_err(|e| format!("methods: {e}"))?;
    if !methods.contains(&0) {
        client
            .write_all(&[5, 0xff])
            .await
            .map_err(|e| e.to_string())?;
        return Err("клиент требует SOCKS5-авторизацию".into());
    }
    client
        .write_all(&[5, 0])
        .await
        .map_err(|e| format!("auth-ответ: {e}"))?;

    // --- SOCKS5 CONNECT request ---
    let mut request = [0; 4];
    client
        .read_exact(&mut request)
        .await
        .map_err(|e| format!("запрос: {e}"))?;
    if request[0] != 5 || request[2] != 0 {
        socks_reply(client, 1).await?;
        return Err("некорректный SOCKS5-запрос".into());
    }
    if request[1] != 1 {
        socks_reply(client, 7).await?;
        return Err("поддержан только CONNECT".into());
    }
    let dst_ip: IpAddr = match request[3] {
        1 => {
            let mut b = [0; 4];
            client.read_exact(&mut b).await.map_err(|e| e.to_string())?;
            b.into()
        }
        4 => {
            let mut b = [0; 16];
            client.read_exact(&mut b).await.map_err(|e| e.to_string())?;
            b.into()
        }
        3 => {
            let len = client.read_u8().await.map_err(|e| e.to_string())? as usize;
            let mut b = vec![0u8; len];
            client.read_exact(&mut b).await.map_err(|e| e.to_string())?;
            std::str::from_utf8(&b)
                .ok()
                .and_then(|s| s.parse().ok())
                .ok_or("SOCKS-домен вместо IP не поддержан")?
        }
        _ => {
            socks_reply(client, 8).await?;
            return Err("неизвестный тип адреса SOCKS".into());
        }
    };
    let _dst_port = client.read_u16().await.map_err(|e| e.to_string())?;

    let Some(dc_idx) = dc_by_ip(dst_ip) else {
        socks_reply(client, 2).await?;
        return Err(format!(
            "адрес {dst_ip} не опознан как известный DC Telegram"
        ));
    };

    socks_reply(client, 0).await?;
    Ok((dst_ip, dc_idx))
}

/// Результат успешной «фиксации» на живом сокете прокси.
struct Latched {
    upstream: TcpStream,
    enc: AesCtr256,
    dec: AesCtr256,
    /// Расшифрованные байты апстрима: не меньше первого полного кадра.
    backlog: Vec<u8>,
    /// Сколько байт из `pending` уже ушло в этот апстрим.
    sent: usize,
}

/// Живучий подбор апстрима. Пул прокси выдаёт наполовину мёртвые сокеты
/// (DPI рвёт апстрим, а локально сокет выглядит живым: _is_stale у Flowseal
/// проверяет только локальное состояние), поэтому до первого ПОЛНОГО кадра
/// ответа делаем редайал с реплеем: клиентские байты копятся в `pending`,
/// каждый новый сокет получает их заново (req_pq/DH stateless со стороны
/// сервера). Живой сокет отвечает за доли секунды, таймаут попытки 4 с.
async fn latch_upstream(
    client: &mut TcpStream,
    target: &Target,
    dc_idx: i16,
    pending: &mut Vec<u8>,
) -> Result<Latched, String> {
    const PENDING_CAP: usize = 256 * 1024;
    const FRAME_LEN_CAP: usize = 8 * 1024 * 1024;
    const ATTEMPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
    const MAX_ATTEMPTS: u32 = 60;

    let media_idx = if dc_idx > 0 { -dc_idx } else { dc_idx };
    let started = tokio::time::Instant::now();
    let overall = started + std::time::Duration::from_secs(120);
    // Сколько раз прокси принимал подключение, но не сказал ни байта:
    // если такое повторяется, дело в его собственном апстриме, а не в нас.
    let mut silent_rounds = 0u32;

    for attempt in 1..=MAX_ATTEMPTS {
        if tokio::time::Instant::now() >= overall {
            break;
        }
        // Чередуем маршруты: main, media, main, media… main важнее:
        // медиа-кольцо может не отвечать на unauth req_pq, но труба живая.
        let route_idx = if attempt % 2 == 0 { media_idx } else { dc_idx };
        let route_name = format!(
            "DC{}{}",
            route_idx.abs(),
            if route_idx < 0 { " media" } else { "" }
        );

        let mut upstream = match TcpStream::connect(target.addr).await {
            Ok(s) => s,
            Err(e) => {
                println!("[мост] попытка {attempt}: прокси недоступен ({e}), пауза");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
        };
        let (init, mut enc, mut dec) = generate_client_handshake(&target.secret, route_idx);
        if let Err(e) = upstream.write_all(&init).await {
            println!("[мост] попытка {attempt} ({route_name}): рукопожатие не ушло ({e})");
            continue;
        }
        let mut sent = 0usize;
        if !pending.is_empty() {
            let mut blob = pending.clone();
            enc.apply_keystream(&mut blob);
            if let Err(e) = upstream.write_all(&blob).await {
                println!("[мост] попытка {attempt} ({route_name}): реплей не ушёл ({e})");
                continue;
            }
            sent = pending.len();
        }

        // Гонка: первый кадр апстрима / новые байты клиента / таймаут попытки.
        // ВСЕ маршруты Flowseal (pool/CF/worker/TCP) начинают поток клиента
        // 64-байтовым эхом relay-init (их bridge_ws_reencrypt не вычитает его),
        // и только потом идут кадры MTProto. Эхо обязано быть пропущено,
        // иначе первые 4 байта случайности читаются как «длина кадра» и живой
        // ответ выбрасывается как мусор.
        let attempt_deadline = tokio::time::Instant::now() + ATTEMPT_TIMEOUT;
        let mut up_plain: Vec<u8> = Vec::new();
        let mut echo_skipped = 0usize;
        let (mut latched, mut failed, mut client_gone) = (false, false, false);
        let mut bad_frame = 0usize;
        let mut up_buf = [0u8; 16 * 1024];
        let mut cli_buf = [0u8; 16 * 1024];
        loop {
            tokio::select! {
                biased;
                r = upstream.read(&mut up_buf) => match r {
                    Ok(0) => failed = true,
                    Ok(n) => {
                        let data = &mut up_buf[..n];
                        dec.apply_keystream(data);
                        let plain = if echo_skipped < 64 {
                            let take = std::cmp::min(64 - echo_skipped, n);
                            echo_skipped += take;
                            &data[take..]
                        } else {
                            data
                        };
                        up_plain.extend_from_slice(plain);
                        if echo_skipped == 64 && !up_plain.is_empty() && up_plain.len() >= 4 {
                            let flen = u32::from_le_bytes([
                                up_plain[0], up_plain[1], up_plain[2], up_plain[3],
                            ]) as usize;
                            if flen == 0 || flen > FRAME_LEN_CAP {
                                bad_frame = flen;
                                failed = true;
                            } else if up_plain.len() >= 4 + flen {
                                // Первый полный кадр получен — фиксируемся.
                                latched = true;
                                break;
                            }
                        }
                    }
                    Err(_) => failed = true,
                },
                r = client.read(&mut cli_buf) => match r {
                    Ok(0) => client_gone = true,
                    Ok(n) => {
                        if pending.len() + n > PENDING_CAP {
                            return Err("слишком много байтов клиента до первого ответа апстрима".into());
                        }
                        pending.extend_from_slice(&cli_buf[..n]);
                    }
                    Err(_) => client_gone = true,
                },
                _ = tokio::time::sleep_until(attempt_deadline) => failed = true,
            }
            if latched || failed || client_gone {
                break;
            }
        }

        if client_gone {
            let _ = upstream.shutdown().await;
            return Err("клиент закрыл соединение до ответа апстрима".into());
        }
        if latched {
            println!(
                "[мост] ЖИВОЙ сокет: попытка {attempt} ({route_name}), первый кадр {} Б ({:.1} c от старта)",
                up_plain.len(),
                started.elapsed().as_secs_f32()
            );
            return Ok(Latched {
                upstream,
                enc,
                dec,
                backlog: up_plain,
                sent,
            });
        }
        let reason = if bad_frame != 0 {
            format!("мусорный кадр len={bad_frame}")
        } else if echo_skipped < 64 {
            if echo_skipped == 0 {
                format!(
                    "прокси молчит {:.0} c: подключение принято, но апстрим прокси не ответил",
                    ATTEMPT_TIMEOUT.as_secs_f32()
                )
            } else {
                format!("эхо {echo_skipped}/64")
            }
        } else if up_plain.is_empty() {
            "эхо есть, кадра нет".to_string()
        } else {
            format!("обрыв на {} Б кадра", up_plain.len())
        };
        println!("[мост] попытка {attempt} ({route_name}): апстрим «{reason}» — редайал");
        if echo_skipped == 0 && bad_frame == 0 {
            silent_rounds += 1;
        } else {
            silent_rounds = 0;
        }
        let _ = upstream.shutdown().await;
        // Не_DDoSим прокси и CF: сначала быстро ловим окно, дальше всё реже.
        let gap = if attempt < 5 {
            std::time::Duration::from_secs(1)
        } else if attempt < 15 {
            std::time::Duration::from_secs(10)
        } else {
            std::time::Duration::from_secs(20)
        };
        tokio::time::sleep(gap).await;
        if silent_rounds >= 3 {
            break;
        }
    }

    let verdict = if silent_rounds >= 3 {
        format!(
            "прокси {target_addr} принимает подключение, но {silent_rounds} раза подряд молчал по {:.0} c — не отвечает его собственный апстрим; проверь лог TgWsProxy (proxy.log)",
            ATTEMPT_TIMEOUT.as_secs_f32(),
            target_addr = target.addr,
        )
    } else {
        format!(
            "ни один сокет прокси не отдал живой ответ за {:.0} c — канал мёртв",
            started.elapsed().as_secs()
        )
    };
    remember_reason(verdict.clone());
    Err(verdict)
}

/// Полный цикл одного клиента: SOCKS5-переговоры → obfuscated2-рукопожатие
/// с настоящим прокси (с редайалом до первого кадра) → двунаправленная
/// пересылка с фиксацией на живом сокете.
/// Запускает локальный SOCKS5-мост через Cloudflare-Worker пользователя:
/// `wss://<worker>/apiws?dst=<IP DC>&dc=<N>` — воркер сам открывает TCP к DC.
/// Внутри туннеля идёт чистый MTProto (intermediate): DPI видит только TLS
/// к `workers.dev`, а до DC пакеты идут уже из сети Cloudflare.
pub async fn spawn_worker_bridge(worker: String) -> Result<SocketAddr, String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("не удалось занять локальный порт под мост: {e}"))?;
    let local = listener
        .local_addr()
        .map_err(|e| format!("адрес моста: {e}"))?;

    let worker = Arc::new(worker);
    println!("[воркер] локальный SOCKS5: {local} → WSS {worker}");

    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let worker = Arc::clone(&worker);
                    tokio::spawn(async move {
                        if let Err(e) = serve_worker_client(stream, &worker).await {
                            eprintln!("[воркер] соединение закрыто: {e}");
                        }
                    });
                }
                Err(e) => {
                    eprintln!("[воркер] приём подключений: {e}");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }
    });

    Ok(local)
}

/// WSS-подключение к воркеру/CF-прокси с заголовком, который они ожидают.
async fn connect_ws(url: &str, timeout_msg: &str) -> Result<WsStream, String> {
    match connect_ws_once(url, timeout_msg, true).await {
        // Часть воркеров (в т.ч. собственный воркер TgWsProxy) не подтверждает
        // subprotocol «binary» — им достаточно обычного WSS-рукопожатия.
        Err(e) if e.to_lowercase().contains("subprotocol") => {
            connect_ws_once(url, timeout_msg, false).await
        }
        other => other,
    }
}

async fn connect_ws_once(
    url: &str,
    timeout_msg: &str,
    want_subprotocol: bool,
) -> Result<WsStream, String> {
    let mut request = url
        .into_client_request()
        .map_err(|e| format!("адрес {url}: {e}"))?;
    if want_subprotocol {
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", HeaderValue::from_static("binary"));
    }

    let (ws, response) = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        tokio_tungstenite::connect_async(request),
    )
    .await
    .map_err(|_| timeout_msg.to_string())?
    .map_err(|e| format!("WSS {url}: {e}"))?;
    if std::env::var_os("GORILLA_WS_DEBUG").is_some() {
        eprintln!("[dbg ws] {url} → {}", response.status());
    }
    Ok(ws)
}

/// Режет исходящий поток на отдельные MTProto-пакеты: CF-прокси и воркер
/// пересылают каждый WS-фрейм как есть, а DC разбирает поток пакетами.
struct MsgSplitter {
    proto: Proto,
    buf: Vec<u8>,
    disabled: bool,
    /// grammers один раз присылает тег 0xeeeeeeee перед первым пакетом —
    /// он уже объявлен в нашем relay-заголовке, в DC его слать не нужно.
    drop_tag: bool,
}

impl MsgSplitter {
    fn new(proto: Proto) -> Self {
        Self {
            proto,
            buf: Vec::with_capacity(16 * 1024),
            disabled: false,
            drop_tag: true,
        }
    }

    fn split(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        if self.disabled {
            return vec![chunk.to_vec()];
        }
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        if self.drop_tag && self.buf.len() >= 4 {
            if self.buf[..4] == PROTO_TAG_INTERMEDIATE[..]
                || self.buf[..4] == PROTO_TAG_ABRIDGED[..]
            {
                self.buf.drain(..4);
            }
            self.drop_tag = false;
        }
        loop {
            let Some((header, payload)) = self.peek_len() else {
                break;
            };
            if payload == 0 {
                self.disabled = true;
                out.push(self.buf.drain(..).collect::<Vec<u8>>());
                break;
            }
            if self.buf.len() < header + payload {
                break;
            }
            out.push(self.buf.drain(..header + payload).collect::<Vec<u8>>());
        }
        out
    }

    fn peek_len(&self) -> Option<(usize, usize)> {
        match self.proto {
            Proto::Intermediate => {
                if self.buf.len() < 4 {
                    return None;
                }
                let len = u32::from_le_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]])
                    & 0x7FFF_FFFF;
                Some((4, len as usize))
            }
            Proto::Abridged => {
                if self.buf.is_empty() {
                    return None;
                }
                if self.buf[0] < 0x7f {
                    Some((1, self.buf[0] as usize * 4))
                } else {
                    if self.buf.len() < 4 {
                        return None;
                    }
                    let word =
                        u32::from_le_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]);
                    Some((4, (word >> 8) as usize * 4))
                }
            }
        }
    }

    fn flush(&mut self) -> Option<Vec<u8>> {
        if self.buf.is_empty() {
            None
        } else {
            Some(self.buf.drain(..).collect())
        }
    }
}

/// Двусторонняя пересылка: клиентский сокет ↔ WSS-туннель.
async fn pump(
    mut client: TcpStream,
    ws: WsStream,
    enc: AesCtr256,
    dec: AesCtr256,
    proto: Proto,
    tag: &str,
) -> Result<(), String> {
    let (mut ws_send, mut ws_recv) = ws.split();
    let (mut from_client, mut to_client) = client.split();

    let up = async {
        let mut enc = enc;
        let mut splitter = MsgSplitter::new(proto);
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let n = from_client
                .read(&mut buf)
                .await
                .map_err(|e| format!("клиент→{tag}: {e}"))?;
            if n == 0 {
                if let Some(tail) = splitter.flush() {
                    ws_send
                        .send(Message::Binary(tail.into()))
                        .await
                        .map_err(|e| format!("отправка в {tag}: {e}"))?;
                }
                return Ok::<(), String>(());
            }
            for part in splitter.split(&buf[..n]) {
                let mut cipher = part;
                enc.apply_keystream(&mut cipher);
                ws_send
                    .send(Message::Binary(cipher.into()))
                    .await
                    .map_err(|e| format!("отправка в {tag}: {e}"))?;
            }
        }
    };
    let down = async {
        let mut dec = dec;
        loop {
            match ws_recv.next().await {
                Some(Ok(Message::Binary(bytes))) => {
                    let mut data = bytes.to_vec();
                    dec.apply_keystream(&mut data);
                    to_client
                        .write_all(&data)
                        .await
                        .map_err(|e| format!("{tag}→клиент: {e}"))?;
                }
                Some(Ok(Message::Text(text))) => {
                    to_client
                        .write_all(text.as_str().as_bytes())
                        .await
                        .map_err(|e| format!("{tag}→клиент: {e}"))?;
                }
                other => {
                    if std::env::var_os("GORILLA_WS_DEBUG").is_some() {
                        eprintln!("[dbg {tag}] вниз: {other:?}");
                    }
                    match other {
                        Some(Ok(Message::Close(_))) | None => return Ok::<(), String>(()),
                        Some(Ok(_)) => {}
                        Some(Err(e)) => return Err(format!("WSS-поток {tag}: {e}")),
                    }
                }
            }
        }
    };

    tokio::select! {
        result = up => result,
        result = down => result,
    }
}

/// Одно клиентское подключение через worker: SOCKS5 → WSS → DC.
async fn serve_worker_client(mut client: TcpStream, worker: &str) -> Result<(), String> {
    let (dst_ip, dc_idx) = socks_accept(&mut client).await?;
    let url = format!("wss://{worker}/apiws?dst={dst_ip}&dc={}", dc_idx.abs());

    let mut ws = connect_ws(&url, &format!("таймаут WSS-подключения к {worker}")).await?;
    let (init, enc, dec) = generate_relay_handshake(dc_idx);
    ws.send(Message::Binary(init.to_vec().into()))
        .await
        .map_err(|e| format!("воркер: рукопожатие: {e}"))?;
    pump(client, ws, enc, dec, Proto::Intermediate, "воркер").await
}

/// Хост CF-прокси для нужного DC: `kws<dc>.<домен>` (так же в tg-ws-proxy).
fn cf_proxy_host(domain: &str, dc_idx: i16) -> String {
    format!("kws{}.{domain}", dc_idx.abs())
}

/// Прогретый WSS-туннель в пуле.
struct PooledWs {
    ws: WsStream,
    created: Instant,
}

/// Пул готовых туннелей к CF-прокси — как `_CfWorkerPool` в tg-ws-proxy.
///
/// Для каждого DC держим заранее поднятый WSS и отдаём его клиенту сразу,
/// протухший (старше 100 c) меняем в фоне. Так обрыв DPI не превращается
/// в таймаут TLS-рукопожатия, а grammers переподключается в ту же секунду.
struct CfProxyPool {
    idle: Mutex<HashMap<i16, VecDeque<PooledWs>>>,
    refilling: Mutex<HashSet<i16>>,
    rotating: Mutex<HashSet<i16>>,
    failures: Mutex<HashMap<i16, u32>>,
    retry_after: Mutex<HashMap<i16, Instant>>,
}

/// Сколько живёт прогретый туннель (в tg-ws-proxy — 100 c).
const POOL_MAX_AGE: Duration = Duration::from_secs(100);
/// Как часто проверять живость пула.
const POOL_CHECK_INTERVAL: Duration = Duration::from_secs(5);

static CF_POOL: OnceLock<CfProxyPool> = OnceLock::new();

fn pool() -> &'static CfProxyPool {
    CF_POOL.get_or_init(CfProxyPool::new)
}

impl CfProxyPool {
    fn new() -> Self {
        Self {
            idle: Mutex::new(HashMap::new()),
            refilling: Mutex::new(HashSet::new()),
            rotating: Mutex::new(HashSet::new()),
            failures: Mutex::new(HashMap::new()),
            retry_after: Mutex::new(HashMap::new()),
        }
    }

    /// Туннель для DC: готовый из пула, иначе — свежий (и сразу пополнить).
    async fn acquire(self: &'static Self, dc: i16) -> Result<WsStream, String> {
        if pool_size() == 0 {
            let (ws, host) = self.connect_one(dc).await?;
            println!("[cf-прокси] DC{dc} через {host} (пул выключен)");
            return Ok(ws);
        }

        let mut taken = None;
        {
            let mut idle = self.idle.lock().await;
            let bucket = idle.entry(dc).or_default();
            while let Some(entry) = bucket.pop_front() {
                if entry.created.elapsed() < POOL_MAX_AGE {
                    taken = Some(entry.ws);
                    break;
                }
            }
        }
        self.spawn_refill(dc).await;

        match taken {
            Some(ws) => {
                println!("[cf-прокси] DC{dc}: готовый туннель из пула");
                Ok(ws)
            }
            None => {
                let (ws, host) = self.connect_one(dc).await?;
                println!("[cf-прокси] DC{dc} через {host}");
                Ok(ws)
            }
        }
    }

    /// Фоновое пополнение: начинается с первой же потребности в DC.
    async fn spawn_refill(self: &'static Self, dc: i16) {
        if let Some(at) = self.retry_after.lock().await.get(&dc).copied() {
            if at > Instant::now() {
                return;
            }
        }
        if !self.refilling.lock().await.insert(dc) {
            return;
        }
        self.spawn_rotation(dc).await;
        tokio::spawn(async move {
            pool().refill(dc).await;
        });
    }

    /// Держит пул живым: раз в 5 c выбрасывает протухшие и дозаправляет.
    async fn spawn_rotation(self: &'static Self, dc: i16) {
        if !self.rotating.lock().await.insert(dc) {
            return;
        }
        tokio::spawn(async move {
            loop {
                let ready = {
                    let mut idle = pool().idle.lock().await;
                    let bucket = idle.entry(dc).or_default();
                    bucket.retain(|entry| entry.created.elapsed() < POOL_MAX_AGE);
                    bucket.len()
                };
                if ready < pool_size() {
                    pool().kick_refill(dc).await;
                }
                tokio::time::sleep(POOL_CHECK_INTERVAL).await;
            }
        });
    }

    /// Одна попытка дозаправки без запуска ротации (её уже крутит цикл).
    async fn kick_refill(self: &'static Self, dc: i16) {
        if let Some(at) = self.retry_after.lock().await.get(&dc).copied() {
            if at > Instant::now() {
                return;
            }
        }
        if !self.refilling.lock().await.insert(dc) {
            return;
        }
        tokio::spawn(async move {
            pool().refill(dc).await;
        });
    }

    /// Пополнить пул до `POOL_PER_DC`; при отказе — пауза с удвоением.
    async fn refill(&self, dc: i16) {
        let mut connected = 0u32;
        loop {
            let have = self.idle.lock().await.get(&dc).map_or(0, |b| b.len());
            if have >= pool_size() {
                break;
            }
            match self.connect_one(dc).await {
                Ok((ws, _host)) => {
                    self.idle
                        .lock()
                        .await
                        .entry(dc)
                        .or_default()
                        .push_back(PooledWs {
                            ws,
                            created: Instant::now(),
                        });
                    connected += 1;
                }
                Err(e) => {
                    eprintln!("[cf-прокси] пул DC{dc}: {e}");
                    break;
                }
            }
        }

        self.refilling.lock().await.remove(&dc);
        let mut failures = self.failures.lock().await;
        let mut retry = self.retry_after.lock().await;
        if connected > 0 {
            failures.remove(&dc);
            retry.remove(&dc);
            println!("[cf-прокси] пул DC{dc}: {connected} готовых туннелей");
        } else {
            let n = {
                let n = failures.entry(dc).or_insert(0);
                *n += 1;
                *n
            };
            let delay = 2f64.powi((n - 1).min(12) as i32).min(3600.0);
            retry.insert(dc, Instant::now() + Duration::from_secs_f64(delay));
            println!("[cf-прокси] пул DC{dc} пуст, следующая попытка через {delay:.0} c");
        }
    }

    /// Подключение к CF-прокси для DC: параллельный забег по доменам.
    ///
    /// Порядок берётся у балансировщика (лучший счёт первым), первые
    /// `probe_parallel` доменов пробуются одновременно — побеждает тот, кто
    /// первым закончил рукопожатие. Исход каждого идёт в счёт.
    async fn connect_one(&self, dc: i16) -> Result<(WsStream, String), String> {
        let domains = domains_for(dc, cf_domains())?;
        let order = {
            let Ok(scores) = crate::balancer::global().lock() else {
                return Err("счёт балансировщика отравлен".to_string());
            };
            scores.order(dc, &domains, crate::balancer::now(), score_ttl())
        };
        let batch = probe_parallel();
        let rest = order;
        let mut last_err = String::new();

        // Сначала один домен — лидер по счёту. Забег включается только
        // при промахе, поэтому измеренный лидер реально определяет выбор,
        // а не гоняется вслепую вместе со всем пулом.
        let (leader, mut rest) = attempt_plan(dc, rest);
        if let Some(best) = leader {
            let host = cf_proxy_host(&best, dc);
            let url = format!("wss://{host}/apiws");
            eprintln!("[cf-прокси] DC{dc}: пробую лучший по счёту {best}");
            let started = Instant::now();
            match connect_ws(&url, &format!("таймаут WSS-подключения к {host}")).await {
                Ok(ws) => {
                    let elapsed = started.elapsed();
                    record_score(dc, &best, Some(elapsed));
                    println!("[cf-прокси] DC{dc}: лучший сервер — {best}");
                    eprintln!(
                        "[cf-прокси] {best}: рукопожатие за {} мс, счёт {:.2}",
                        elapsed.as_millis(),
                        elapsed.as_secs_f64()
                    );
                    return Ok((ws, host));
                }
                Err(e) => {
                    record_score(dc, &best, None);
                    eprintln!("[cf-прокси] {best}: {e} — перехожу к забегу");
                    last_err = e;
                }
            }
        }

        while !rest.is_empty() {
            let take = batch.min(rest.len());
            let racers: Vec<String> = rest.drain(..take).collect();
            let mut set: tokio::task::JoinSet<(String, Result<WsStream, String>, Duration)> =
                tokio::task::JoinSet::new();
            for domain in racers {
                set.spawn(async move {
                    let host = cf_proxy_host(&domain, dc);
                    let url = format!("wss://{host}/apiws");
                    let started = Instant::now();
                    let res = connect_ws(&url, &format!("таймаут WSS-подключения к {host}")).await;
                    (domain, res, started.elapsed())
                });
            }

            let mut winner: Option<(String, WsStream)> = None;
            // После победы даём остальным полсекунды, чтобы их задержки тоже
            // попали в счёт, и отпускаем ожидание.
            let mut grace: Option<Instant> = None;
            loop {
                let next = match grace {
                    Some(deadline) => {
                        match tokio::time::timeout_at(deadline.into(), set.join_next()).await {
                            Ok(next) => next,
                            Err(_) => break,
                        }
                    }
                    None => set.join_next().await,
                };
                let Some(joined) = next else { break };
                match joined {
                    Ok((domain, Ok(ws), elapsed)) => {
                        record_score(dc, &domain, Some(elapsed));
                        let score = {
                            let now = crate::balancer::now();
                            let ttl = score_ttl();
                            crate::balancer::global()
                                .lock()
                                .ok()
                                .and_then(|scores| scores.lookup(dc, &domain, now, ttl))
                        }
                        .unwrap_or(elapsed.as_secs_f64());
                        eprintln!(
                            "[cf-прокси] {domain}: рукопожатие за {} мс, счёт {score:.2}",
                            elapsed.as_millis()
                        );
                        if winner.is_none() {
                            winner = Some((domain, ws));
                            grace = Some(Instant::now() + Duration::from_millis(500));
                        }
                    }
                    Ok((domain, Err(e), _)) => {
                        record_score(dc, &domain, None);
                        eprintln!("[cf-прокси] {domain}: {e}");
                        last_err = e;
                    }
                    Err(e) => last_err = format!("проба оборвалась: {e}"),
                }
            }
            drop(set);

            if let Some((domain, ws)) = winner {
                println!("[cf-прокси] DC{dc}: лучший сервер — {domain}");
                return Ok((ws, cf_proxy_host(&domain, dc)));
            }
        }

        Err(format!(
            "ни один стандартный CF-прокси не ответил для DC{dc}: {last_err}"
        ))
    }
}

/// Локальный SOCKS5-мост через стандартные CF-прокси tg-ws-proxy:
/// `wss://kws<dc>.<домен>/apiws` — домен сам знает DC, свой воркер не нужен.
/// Используется, когда `mtproxy.worker` не задан.
pub async fn spawn_cfproxy_bridge() -> Result<SocketAddr, String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("не удалось занять локальный порт под мост: {e}"))?;
    let local = listener
        .local_addr()
        .map_err(|e| format!("адрес моста: {e}"))?;

    println!(
        "[cf-прокси] локальный SOCKS5: {local} → {} стандартных доменов tg-ws-proxy, пул прогретых туннелей",
        cf_domains().len()
    );

    // Прогреваем туннель к первому DC: к первому подключению grammers он уже готов.
    tokio::spawn(async { pool().spawn_refill(2).await });

    // Как апстрим: список доменов обновляем раз в час — домены протухают.
    tokio::spawn(async {
        loop {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            match refresh_cf_domains().await {
                Ok(count) => println!("[cf-прокси] домены пула обновлены из репозитория: {count}"),
                Err(e) => eprintln!("[cf-прокси] обновление доменов не удалось: {e}"),
            }
        }
    });

    // Балансировщик: раз в 5 минут гоняет забег по доменам для основных DC,
    // чтобы счёт не вырождался в «кто один раз ответил первым».
    tokio::spawn(async {
        let mut ticker = tokio::time::interval(Duration::from_secs(300));
        ticker.tick().await;
        loop {
            ticker.tick().await;
            if pool_size() == 0 {
                continue;
            }
            for dc in [2i16, 4] {
                match pool().connect_one(dc).await {
                    Ok((ws, host)) => {
                        drop(ws);
                        println!("[балансировщик] DC{dc}: лучший сервер — {host}");
                    }
                    Err(e) => eprintln!("[балансировщик] DC{dc}: {e}"),
                }
            }
        }
    });

    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    tokio::spawn(async move {
                        if let Err(e) = serve_cfproxy_client(stream).await {
                            eprintln!("[cf-прокси] соединение закрыто: {e}");
                        }
                    });
                }
                Err(e) => {
                    eprintln!("[cf-прокси] приём подключений: {e}");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    });

    Ok(local)
}

/// Одно клиентское подключение через стандартный CF-прокси: SOCKS5 → WSS → DC.
async fn serve_cfproxy_client(mut client: TcpStream) -> Result<(), String> {
    let (_dst_ip, dc_idx) = socks_accept(&mut client).await?;
    let mut ws = pool().acquire(dc_idx.abs()).await?;
    let (init, enc, dec) = generate_relay_handshake(dc_idx);
    ws.send(Message::Binary(init.to_vec().into()))
        .await
        .map_err(|e| format!("cf-прокси: рукопожатие: {e}"))?;
    pump(client, ws, enc, dec, Proto::Intermediate, "cf-прокси").await
}

async fn serve_client(mut client: TcpStream, target: &Target) -> Result<(), String> {
    let (_, dc_idx) = socks_accept(&mut client).await?;

    // --- obfuscated2 к настоящему прокси, с редайалом до живого сокета ---
    let mut pending: Vec<u8> = Vec::new();
    let mut latched = latch_upstream(&mut client, target, dc_idx, &mut pending).await?;

    // Отдать клиенту уже расшифрованный первый кадр (и всё, что пришло с ним).
    client
        .write_all(&latched.backlog)
        .await
        .map_err(|e| format!("первый кадр клиенту: {e}"))?;
    // Дослать клиентские байты, накопившиеся ПОСЛЕ реплея на живой сокет.
    if pending.len() > latched.sent {
        let delta = &pending[latched.sent..];
        let mut blob = delta.to_vec();
        latched.enc.apply_keystream(&mut blob);
        latched
            .upstream
            .write_all(&blob)
            .await
            .map_err(|e| format!("досыл клиентских байтов: {e}"))?;
    }
    let Latched {
        upstream, enc, dec, ..
    } = latched;
    relay(client, upstream, enc, dec).await
}

/// Двунаправленная пересылка с AES-CTR поверх обоих направлений.
async fn relay(
    mut client: TcpStream,
    mut upstream: TcpStream,
    enc: AesCtr256,
    dec: AesCtr256,
) -> Result<(), String> {
    let (mut from_client, mut to_client) = client.split();
    let (mut from_upstream, mut to_upstream) = upstream.split();

    let enc = Arc::new(Mutex::new(enc));
    let dec = Arc::new(Mutex::new(dec));

    let up = {
        let enc = Arc::clone(&enc);
        async move {
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                let n = from_client
                    .read(&mut buf)
                    .await
                    .map_err(|e| e.to_string())?;
                if n == 0 {
                    to_upstream.shutdown().await.map_err(|e| e.to_string())?;
                    return Ok::<(), String>(());
                }
                let data = &mut buf[..n];
                enc.lock().await.apply_keystream(data);
                to_upstream
                    .write_all(data)
                    .await
                    .map_err(|e| e.to_string())?;
            }
        }
    };
    let down = {
        let dec = Arc::clone(&dec);
        async move {
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                let n = from_upstream
                    .read(&mut buf)
                    .await
                    .map_err(|e| e.to_string())?;
                if n == 0 {
                    to_client.shutdown().await.map_err(|e| e.to_string())?;
                    return Ok::<(), String>(());
                }
                let data = &mut buf[..n];
                dec.lock().await.apply_keystream(data);
                to_client.write_all(data).await.map_err(|e| e.to_string())?;
            }
        }
    };

    tokio::try_join!(up, down).map(|_| ())
}

#[cfg(test)]
mod local_probe_tests {
    use super::{probe_local, LocalKind};

    /// Стандартный пул — 20 доменов; забег должен покрывать его целиком,
    /// иначе время до ошибки растёт кратно числу пачек по 6 секунд.
    #[test]
    fn default_probe_parallel_covers_standard_pool() {
        let batch = super::probe_parallel();
        assert!(batch >= 20, "параллельных проб {batch}, а доменов 20");
    }

    /// Измеренный лидер идёт первым, остальные — на забег.
    #[test]
    fn plan_takes_scored_leader_first() {
        super::record_score(91, "kws2.fast", Some(std::time::Duration::from_millis(150)));
        super::record_score(91, "kws2.slow", Some(std::time::Duration::from_millis(1500)));
        let order = vec!["kws2.slow".to_string(), "kws2.fast".to_string()];
        let (leader, rest) = super::attempt_plan(91, order);
        assert_eq!(leader.as_deref(), Some("kws2.fast"));
        assert_eq!(rest, vec!["kws2.slow".to_string()]);
    }

    /// Без замеров лидера нет — весь список уходит на забег.
    #[test]
    fn plan_without_scores_races_everything() {
        let order = vec!["kws2.a".to_string(), "kws2.b".to_string()];
        let (leader, rest) = super::attempt_plan(92, order.clone());
        assert!(leader.is_none());
        assert_eq!(rest, order);
    }

    /// Пустой список доменов — понятная ошибка без единого сетевого запроса.
    #[test]
    fn pool_without_domains_reports_error() {
        let err = super::domains_for(2, Vec::new()).unwrap_err();
        assert!(err.contains("список CF-прокси пуст"), "err: {err}");
        let list = vec!["kws2.x.co.uk".to_string()];
        assert_eq!(super::domains_for(2, list.clone()).unwrap(), list);
    }
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Закрытый порт: проба не должна висеть и обязана сказать Closed.
    #[tokio::test]
    async fn closed_port() {
        assert_eq!(probe_local("127.0.0.1", 1).await, LocalKind::Closed);
    }

    /// Мини-SOCKS5 отвечает `05 00` — значит через него можно ходить напрямую.
    #[tokio::test]
    async fn socks5_greeting() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut greeting = [0u8; 3];
            socket.read_exact(&mut greeting).await.unwrap();
            socket.write_all(&[0x05, 0x00]).await.unwrap();
        });
        assert_eq!(probe_local("127.0.0.1", port).await, LocalKind::Socks5);
        server.await.unwrap();
    }

    /// Молчащий порт — это MTProto-прокси без SOCKS5, ему нужен секрет.
    #[tokio::test]
    async fn silent_port_is_open() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        });
        assert_eq!(probe_local("127.0.0.1", port).await, LocalKind::Open);
        server.abort();
    }
}
