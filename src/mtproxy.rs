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
/// `config.json` бота `mtproxy.tgws` пуст и порт отличается от стандартного.
pub fn tgws_local() -> Option<(String, u16, [u8; 16])> {
    let base = std::env::var_os("APPDATA")?;
    let path = std::path::PathBuf::from(base)
        .join("TgWsProxy")
        .join("config.json");
    let raw = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let host = value
        .get("host")
        .and_then(|host| host.as_str())
        .unwrap_or("127.0.0.1")
        .to_string();
    let port = value
        .get("port")
        .and_then(|port| port.as_u64())
        .and_then(|port| u16::try_from(port).ok())
        .unwrap_or_else(crate::config::default_tgws_port);
    let secret = parse_secret(value.get("secret")?.as_str()?).ok()?;
    Some((host, port, secret))
}

/// Адрес локального TgWsProxy из настроек бота (`mtproxy.tgws`), иначе —
/// найденный автоматически, если это разрешено (`mtproxy.tgws_local`).
/// Пусто и `off`/`none` — плагин выключен.
pub fn tgws_target(cfg: &crate::config::Config) -> Option<std::net::SocketAddr> {
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
    Some(std::net::SocketAddr::new(host.parse().ok()?, port))
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
                    let _ = stream.set_nodelay(true);
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
    // Живой сокет отвечает за доли секунды; 10 с молчания — это заведомо
    // дохлое подключение (пул прокси выдаёт полумёртвые сокеты).
    const ATTEMPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);
    const MAX_ATTEMPTS: u32 = 60;

    let _media_idx = if dc_idx > 0 { -dc_idx } else { dc_idx };
    let started = tokio::time::Instant::now();
    let overall = started + std::time::Duration::from_secs(120);
    // Сколько раз прокси принимал подключение, но не сказал ни байта:
    // если такое повторяется, дело в его собственном апстриме, а не в нас.
    let mut silent_rounds = 0u32;
    // grammers (Intermediate) ставит служебный тег `ee ee ee ee` ПЕРЕД первым
    // кадром, и ровно один раз. У обфусцированного потока этот тег живёт
    // в рукопожатии (init[56..60]), поэтому из клиентского потока его надо
    // вычистить: иначе MsgSplitter апстрима прочитает `0xEEEEEEEE & 0x7FFFFFFF`
    // как длину кадра, не дождётся её и не отправит в WS ни байта.
    let mut tag_stripped = false;

    for attempt in 1..=MAX_ATTEMPTS {
        if tokio::time::Instant::now() >= overall {
            break;
        }
        // Основное кольцо — всегда. Медиа (DC2m) наш собственный TgWsProxy
        // на unauth req_pq не отвечает, поэтому в него не ходим вовсе.
        let route_idx = dc_idx;
        let route_name = format!(
            "DC{}{}",
            route_idx.abs(),
            if route_idx < 0 { " media" } else { "" }
        );

        let mut upstream = match TcpStream::connect(target.addr).await {
            Ok(s) => {
                // Мелкие MTProto-пакеты: без этого Nagle даёт +40 мс на обмен.
                let _ = s.set_nodelay(true);
                s
            }
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
        // Некоторые маршруты Flowseal предваряют поток 64-байтовым эхом
        // relay-init, наш собственный TgWsProxy — нет. Смещение первого кадра
        // определяем по правдоподобной длине, а не по константе.
        let (mut latched, mut failed, mut client_gone) = (false, false, false);
        let mut frame_offset: Option<usize> = None;
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
                        up_plain.extend_from_slice(data);

                        if frame_offset.is_none() {
                            frame_offset = first_frame_offset(&up_plain, FRAME_LEN_CAP);
                        }
                        // Первый полный кадр получен — фиксируемся.
                        if let Some(off) = frame_offset {
                            let body = &up_plain[off..];
                            if body.len() >= 4 {
                                let flen = u32::from_le_bytes([
                                    body[0], body[1], body[2], body[3],
                                ]) as usize;
                                if flen < 4 || flen > FRAME_LEN_CAP {
                                    bad_frame = flen;
                                    failed = true;
                                } else if body.len() >= 4 + flen {
                                    latched = true;
                                    break;
                                }
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
                        strip_transport_tag(pending, &mut tag_stripped);
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
                // Отдаём клиенту только настоящие кадры: возможное эхо
                // relay-init (64 Б) у Flowseal клиенту не нужно.
                backlog: up_plain[frame_offset.unwrap_or(0)..].to_vec(),
                sent,
            });
        }
        let reason = if bad_frame != 0 {
            format!("мусорный кадр len={bad_frame}")
        } else if up_plain.is_empty() {
            format!(
                "прокси молчит {:.0} c: подключение принято, но апстрим прокси не ответил",
                ATTEMPT_TIMEOUT.as_secs_f32()
            )
        } else if frame_offset.is_none() {
            format!("нет начала кадра в {} Б", up_plain.len())
        } else {
            format!("обрыв на {} Б кадра", up_plain.len())
        };
        println!("[мост] попытка {attempt} ({route_name}): апстрим «{reason}» — редайал");
        if frame_offset.is_none() && bad_frame == 0 {
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
                    let _ = stream.set_nodelay(true);
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
    match connect_ws_once(url, timeout_msg, true, None, None).await {
        // Часть воркеров (в т.ч. собственный воркер TgWsProxy) не подтверждает
        // subprotocol «binary» — им достаточно обычного WSS-рукопожатия.
        Err(e) if e.to_lowercase().contains("subprotocol") => {
            connect_ws_once(url, timeout_msg, false, None, None).await
        }
        other => other,
    }
}

/// Рукопожатие WSS. `dial` — куда идти по TCP (если имя из URL не резолвится),
/// `host_header` — что поставить в заголовок `Host` (SNI при этом берётся из
/// URL, как в SNI-фронтинге апстрима).
async fn connect_ws_once(
    url: &str,
    timeout_msg: &str,
    want_subprotocol: bool,
    dial: Option<&str>,
    host_header: Option<&str>,
) -> Result<WsStream, String> {
    let mut request = url
        .into_client_request()
        .map_err(|e| format!("адрес {url}: {e}"))?;
    if want_subprotocol {
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", HeaderValue::from_static("binary"));
    }
    if let Some(host) = host_header
        && let Ok(value) = HeaderValue::from_str(host)
    {
        request.headers_mut().insert("Host", value);
    }

    // Свой TLS-коннект вместо connect_async: только так сокет доступен до
    // рукопожатия и получает TCP_NODELAY. Для мелких MTProto-пакетов это
    // десятки миллисекунд на каждый обмен.
    let host = url
        .trim_start_matches("wss://")
        .split(['/', ':'])
        .next()
        .unwrap_or_default()
        .to_string();
    let dial = dial.unwrap_or(host.as_str());
    let tcp = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        TcpStream::connect((dial, 443)),
    )
    .await
    .map_err(|_| timeout_msg.to_string())?
    .map_err(|e| format!("TCP {dial}:443: {e}"))?;
    let _ = tcp.set_nodelay(true);

    // При фронтинге SNI (sprinthost.ru) не совпадает с реальным именем точки,
    // поэтому сертификат не проверяем — ровно как `_ssl_ctx_fronting` апстрима.
    let config = if host_header.is_some() {
        tls_client_config()
    } else {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let mut config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Arc::new(config)
    };
    let connector = tokio_tungstenite::Connector::Rustls(config);

    let (ws, response) = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        tokio_tungstenite::client_async_tls_with_config(
            request,
            tcp,
            None,
            Some(connector),
        ),
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

/// Пул тёплых WSS к собственному воркеру: TLS+WS-рукопожатие стоит сотни
/// миллисекунд, а приложение переподключается к DC часто. Ключ пула — (DC, адрес
/// целевого DC), потому что адрес попадает в запрос `?dst=`.
#[derive(Default)]
struct WorkerPool {
    idle: Mutex<HashMap<(i16, IpAddr), VecDeque<PooledWs>>>,
}

static WORKER_POOL: OnceLock<WorkerPool> = OnceLock::new();

fn worker_pool() -> &'static WorkerPool {
    WORKER_POOL.get_or_init(WorkerPool::default)
}

// --- Прямой TCP к DC: копия `_tcp_fallback` из tg-ws-proxy ---------------------
// Когда CF-пул лежит, апстрим льёт обфусцированный поток прямо на IP дата-центра
// (без WSS и без Cloudflare). Пауза после отказа — как `TCP_BACKOFF_*`.

const TCP_BACKOFF_INITIAL: f64 = 30.0;
const TCP_BACKOFF_MAX: f64 = 3600.0;

static TCP_BACKOFF: OnceLock<Mutex<HashMap<i16, (u32, Instant)>>> = OnceLock::new();

fn tcp_backoff() -> &'static Mutex<HashMap<i16, (u32, Instant)>> {
    TCP_BACKOFF.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Адрес DC для прямого TCP (`DC_DEFAULT_IPS` апстрима, не `dc_ip` профиля).
fn dc_default_ip(dc: i16) -> Option<&'static str> {
    Some(match dc.abs() {
        1 => "149.154.175.50",
        2 => "149.154.167.51",
        3 => "149.154.175.100",
        4 => "149.154.167.91",
        5 => "149.154.171.5",
        203 => "91.105.192.100",
        _ => return None,
    })
}

/// `dc_ip` из профиля TgWsProxy (`parse_dc_ip_list`, формат `DC:IP`).
/// Апстрим ходит на эти адреса (`dc_redirects` по умолчанию 149.154.167.220),
/// а `DC_DEFAULT_IPS` — только запасной вариант.
fn tgws_dc_ips() -> HashMap<i16, String> {
    let mut map = HashMap::new();
    let Some(base) = std::env::var_os("APPDATA") else {
        return map;
    };
    let path = std::path::PathBuf::from(base)
        .join("TgWsProxy")
        .join("config.json");
    let Ok(raw) = std::fs::read_to_string(path) else {
        return map;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return map;
    };
    let Some(list) = value.get("dc_ip").and_then(|list| list.as_array()) else {
        return map;
    };
    for entry in list.iter().filter_map(|entry| entry.as_str()) {
        let Some((dc, ip)) = entry.split_once(':') else {
            continue;
        };
        let (Ok(dc), Ok(ip)) = (dc.trim().parse::<i16>(), ip.trim().parse::<IpAddr>()) else {
            continue;
        };
        map.insert(dc, ip.to_string());
    }
    map
}

/// Адрес DC: сначала переопределение из профиля TgWsProxy, затем стандартный.
fn dc_address(dc: i16) -> Option<String> {
    let dc = dc.abs();
    tgws_dc_ips()
        .get(&dc)
        .cloned()
        .or_else(|| dc_default_ip(dc).map(str::to_string))
}

/// Живо ли прямое TCP-соединение с DC (проверка для выбора маршрута).
/// Транспорт до дата-центра.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direct {
    /// Голый обфусцированный TCP — как tcp-fallback в tg-ws-proxy.
    Tcp,
    /// Обфусцированный поток внутри TLS (транспорт официальных клиентов).
    Tls,
}

/// Домены для SNI при TLS-транспорте: Telegram не проверяет сертификат,
/// поэтому годится любой «обычный» домен.
const TLS_SNIS: [&str; 4] = [
    "web.telegram.org",
    "telegram.org",
    "www.google.com",
    "www.microsoft.com",
];

fn tls_domain() -> &'static str {
    let index = rand::random::<u8>() as usize % TLS_SNIS.len();
    TLS_SNIS[index]
}

/// Проверяет, каким транспортом DC реально отвечает: сначала TLS
/// (официальный клиент ходит так), затем голый обфусцированный поток.
pub async fn dc_probe(dc: i16) -> Option<Direct> {
    let ip = dc_address(dc)?;
    if probe_tls(dc, &ip).await {
        return Some(Direct::Tls);
    }
    let address = format!("{ip}:443").parse::<std::net::SocketAddr>().ok()?;
    let server = grammers_mtsender::ServerAddr::Tcp { address };
    let handshake =
        grammers_mtsender::connect(grammers_mtproto::transport::Intermediate::new(), server);
    if tokio::time::timeout(Duration::from_secs(8), handshake)
        .await
        .is_ok_and(|result| result.is_ok())
    {
        return Some(Direct::Tcp);
    }
    None
}

/// TLS-рукопожатие + первый обмен: DC должен ответить MTProto-кадром,
/// а не страницей HTTP (на `149.154.167.220` приходит `HTTP/1.1 302`).
async fn probe_tls(dc: i16, ip: &str) -> bool {
    let Ok(mut stream) = tls_upstream(ip).await else {
        return false;
    };
    let (init, mut enc, _dec) = generate_relay_handshake(dc);
    if stream.write_all(&init).await.is_err() {
        return false;
    }
    let mut payload = vec![0u8; 8];
    payload.extend_from_slice(&0i64.to_le_bytes());
    payload.extend_from_slice(&20u32.to_le_bytes());
    payload.extend_from_slice(&0xbe7e_8ef1u32.to_le_bytes());
    payload.extend_from_slice(&[0u8; 16]);
    let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
    frame.extend_from_slice(&payload);
    enc.apply_keystream(&mut frame);
    if stream.write_all(&frame).await.is_err() {
        return false;
    }
    let mut first = [0u8; 8];
    let read = tokio::time::timeout(Duration::from_secs(6), stream.read_exact(&mut first)).await;
    matches!(read, Ok(Ok(_))) && !first.starts_with(b"HTTP")
}

/// TLS-конфиг без проверки сертификата: у Telegram на 443 свой сертификат,
/// официальные клиенты его не валидируют, а при SNI-фронтинге имя вообще чужое.
fn tls_client_config() -> Arc<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    match rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
    {
        Ok(builder) => Arc::new(
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(provider)))
                .with_no_client_auth(),
        ),
        Err(_) => Arc::new(
            rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .expect("ring-провайдер поддерживает TLS1.2/1.3")
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(Arc::new(
                    rustls::crypto::ring::default_provider(),
                ))))
                .with_no_client_auth(),
        ),
    }
}

/// TLS до DC без проверки сертификата: у Telegram на 443 стоит свой
/// сертификат, официальные клиенты его не валидируют.
async fn tls_upstream(ip: &str) -> Result<tokio_rustls::client::TlsStream<TcpStream>, String> {
    let connector = tokio_rustls::TlsConnector::from(tls_client_config());

    let stream = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect((ip, 443)))
        .await
        .map_err(|_| format!("TLS: таймаут TCP {ip}:443"))?
        .map_err(|e| format!("TLS: TCP {ip}:443: {e}"))?;
    let _ = stream.set_nodelay(true);

    let name = tls_domain();
    let server_name = rustls::pki_types::ServerName::try_from(name.to_string())
        .map_err(|e| format!("TLS: имя {name}: {e}"))?;
    tokio::time::timeout(Duration::from_secs(10), connector.connect(server_name, stream))
        .await
        .map_err(|_| "TLS: таймаут рукопожатия".to_string())?
        .map_err(|e| format!("TLS: рукопожатие: {e}"))
}

/// Проверялка сертификата «принимаем любой».
#[derive(Debug)]
struct AcceptAnyCert(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}


/// Свежее подключение к воркеру через тот же рукопожатный конвейер.
async fn dial_worker(dc: i16, dst: IpAddr, worker: &str) -> Result<WsStream, String> {
    let url = format!("wss://{worker}/apiws?dst={dst}&dc={}", dc.abs());
    connect_ws(&url, &format!("таймаут WSS-подключения к {worker}")).await
}

impl WorkerPool {
    /// Готовый туннель или живое подключение; запас на следующий раз
    /// догревается в фоне, чтобы переподключение приложения не ждало TLS.
    async fn acquire(&self, dc: i16, dst: IpAddr, worker: &str) -> Result<WsStream, String> {
        let key = (dc, dst);
        let warmed = {
            let mut idle = self.idle.lock().await;
            let queue = idle.entry(key).or_default();
            let mut found = None;
            while let Some(item) = queue.pop_front() {
                if item.created.elapsed() < POOL_MAX_AGE {
                    found = Some(item.ws);
                    break;
                }
            }
            found
        };

        let worker = worker.to_string();
        let refill_worker = worker.clone();
        tokio::spawn(async move {
            let idle = worker_pool().idle.lock().await;
            let busy = idle.get(&key).is_some_and(|queue| !queue.is_empty());
            drop(idle);
            if busy {
                return;
            }
            match dial_worker(dc, dst, &refill_worker).await {
                Ok(ws) => {
                    let mut idle = worker_pool().idle.lock().await;
                    let queue = idle.entry(key).or_default();
                    if queue.len() < 2 {
                        queue.push_back(PooledWs {
                            ws,
                            created: Instant::now(),
                        });
                    }
                }
                Err(e) => {
                    if std::env::var_os("GORILLA_WS_DEBUG").is_some() {
                        eprintln!("[dbg пул воркера] DC{dc} {dst}: {e}");
                    }
                }
            }
        });

        match warmed {
            Some(ws) => Ok(ws),
            None => dial_worker(dc, dst, worker.as_str()).await,
        }
    }
}

/// Одно клиентское подключение через worker: SOCKS5 → WSS → DC.
async fn serve_worker_client(mut client: TcpStream, worker: &str) -> Result<(), String> {
    let (dst_ip, dc_idx) = socks_accept(&mut client).await?;
    let mut ws = worker_pool().acquire(dc_idx.abs(), dst_ip, worker).await?;
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
                    let _ = stream.set_nodelay(true);
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

/// Мост «прямой TCP к DC» — аналог `_tcp_fallback` из tg-ws-proxy: когда CF
/// недоступен, обфусцированный поток идёт прямо на IP дата-центра.
pub async fn spawn_dc_bridge() -> Result<SocketAddr, String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("не удалось занять локальный порт под мост: {e}"))?;
    let local = listener
        .local_addr()
        .map_err(|e| format!("адрес моста: {e}"))?;

    println!("[dc-tcp] локальный SOCKS5: {local} → DC Telegram напрямую");

    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let _ = stream.set_nodelay(true);
                    tokio::spawn(async move {
                        if let Err(e) = serve_dc_client(stream).await {
                            eprintln!("[dc-tcp] соединение закрыто: {e}");
                        }
                    });
                }
                Err(e) => {
                    eprintln!("[dc-tcp] приём подключений: {e}");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    });

    Ok(local)
}

/// Одно клиентское подключение напрямую в DC: SOCKS5 → TCP:443 → обфускация.
async fn serve_dc_client(mut client: TcpStream) -> Result<(), String> {
    let (_, dc_idx) = socks_accept(&mut client).await?;
    let dc = dc_idx.abs();
    let ip = dc_address(dc).ok_or_else(|| format!("DC{dc}: нет адреса для прямого TCP"))?;

    let pause = {
        let map = tcp_backoff().lock().await;
        match map.get(&dc) {
            Some((failures, until)) if Instant::now() < *until => {
                Some(format!("DC{dc}: прямой TCP на паузе после {failures} отказов"))
            }
            _ => None,
        }
    };
    if let Some(reason) = pause {
        return Err(reason);
    }

    let (init, enc, dec) = generate_relay_handshake(dc_idx);
    let transport = dc_probe(dc).await;
    if transport.is_none() {
        record_tcp_failure(dc).await;
        return Err(format!("DC{dc}: не отвечает ни по TLS, ни обфусцированным TCP"));
    }
    tcp_backoff().lock().await.remove(&dc);

    match transport {
        Some(Direct::Tls) => {
            let mut upstream = tls_upstream(&ip).await?;
            upstream
                .write_all(&init)
                .await
                .map_err(|e| format!("DC{dc}: отправка рукопожатия в TLS: {e}"))?;
            relay(client, upstream, enc, dec).await
        }
        _ => {
            let mut upstream =
                tokio::time::timeout(Duration::from_secs(10), TcpStream::connect((ip.as_str(), 443)))
                    .await
                    .map_err(|_| format!("DC{dc}: таймаут TCP {ip}:443"))?
                    .map_err(|e| format!("DC{dc}: TCP {ip}:443: {e}"))?;
            let _ = upstream.set_nodelay(true);
            upstream
                .write_all(&init)
                .await
                .map_err(|e| format!("DC{dc}: отправка рукопожатия: {e}"))?;
            relay(client, upstream, enc, dec).await
        }
    }
}

/// Пауза после отказа (30 c, дальше вдвое, максимум 3600), как в апстриме.
async fn record_tcp_failure(dc: i16) {
    let mut map = tcp_backoff().lock().await;
    let entry = map.entry(dc).or_insert((0, Instant::now()));
    entry.0 += 1;
    let delay = (TCP_BACKOFF_INITIAL * 2f64.powi(entry.0.saturating_sub(1) as i32))
        .min(TCP_BACKOFF_MAX);
    entry.1 = Instant::now() + Duration::from_secs_f64(delay);
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
async fn relay<U>(mut client: TcpStream, upstream: U, enc: AesCtr256, dec: AesCtr256) -> Result<(), String>
where
    U: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
{
    let (mut from_client, mut to_client) = client.split();
    let (mut from_upstream, mut to_upstream) = tokio::io::split(upstream);

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

#[cfg(test)]
mod dc_direct_tests {
    /// Прямой TCP к DC без прокси: проверка, что дата-центр вообще принимает
    /// обфусцированный транспорт с этого адреса. Запускать вручную:
    /// `cargo test --bin gorilla-bot dc_direct -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "живая сеть"]
    async fn dc_direct_handshake() {
        let addr = super::dc_address(2).expect("адрес DC2");
        let target: std::net::SocketAddr = format!("{addr}:443").parse().expect("адрес");
        let server = grammers_mtsender::ServerAddr::Tcp { address: target };
        let plain: Result<_, _> = grammers_mtsender::connect(
            grammers_mtproto::transport::Intermediate::new(),
            server.clone(),
        )
        .await;
        println!("plain intermediate: {}", plain.as_ref().map(|_| "ok").unwrap_or("fail"));
        let obf = grammers_mtsender::connect(
            grammers_mtproto::transport::Obfuscated::new(
                grammers_mtproto::transport::Intermediate::new(),
            ),
            server,
        )
        .await;
        println!("obfuscated intermediate: {}", obf.as_ref().map(|_| "ok").unwrap_or("fail"));
        assert!(plain.is_ok() || obf.is_ok());
    }
}

// --- WS-пул прямо к web.telegram.org: то, чем реально ходит tg-ws-proxy -------
// Апстрим (proxy/pool.py) поднимает WSS на IP из `dc_ip` профиля и подставляет
// заголовок `Host: kws{dc}.web.telegram.org`; точка `/apiws` выбирается именно
// заголовком, а SNI при фронтинге — чужое имя (`sprinthost.ru`). Имена
// kws*.web.telegram.org в DNS у нашего провайдера отравлены, поэтому идём на IP.

/// Донор SNI: провайдер режет TLS по SNI наших доменов, а этот проходит.
const FRONTING_SNI: &str = "sprinthost.ru";

/// Хост-заголовок для DC: как у апстрима, без query-строки.
fn kws_host(dc: i16) -> String {
    format!("kws{}.web.telegram.org", dc.abs())
}

/// Варианты SNI по очереди: фронтинг, затем честное имя домена.
fn sni_candidates(host: &str) -> Vec<String> {
    vec![FRONTING_SNI.to_string(), host.to_string()]
}

/// WSS-туннель к Telegram: TCP на IP дата-центра + `Host: kws{dc}...`, без query.
async fn ws_to_dc(dc: i16) -> Result<WsStream, String> {
    let ip = dc_address(dc).ok_or_else(|| format!("DC{dc}: нет адреса для WSS"))?;
    let host = kws_host(dc);
    let mut errors = Vec::new();
    for sni in sni_candidates(&host) {
        let url = format!("wss://{ip}/apiws");
        match connect_ws_once(&url, &format!("DC{dc}: таймаут WSS на {ip}"), true, None, Some(&host)).await {
            Ok(ws) => {
                if std::env::var_os("GORILLA_WS_DEBUG").is_some() {
                    eprintln!("[dbg kws] DC{dc} {ip} SNI {sni}: рукопожатие прошло");
                }
                return Ok(ws);
            }
            Err(e) => errors.push(format!("SNI {sni}: {e}")),
        }
    }
    Err(format!("DC{dc}: WSS не поднялся ({})", errors.join("; ")))
}

/// Тёплый туннель к web.telegram.org (ключ — DC).
#[derive(Default)]
struct KwsPool {
    idle: Mutex<HashMap<i16, VecDeque<PooledWs>>>,
}

static KWS_POOL: OnceLock<KwsPool> = OnceLock::new();

fn pool_kws() -> &'static KwsPool {
    KWS_POOL.get_or_init(KwsPool::default)
}

impl KwsPool {
    /// Готовый туннель или свежий; запас догревается фоном.
    async fn acquire(&self, dc: i16) -> Result<WsStream, String> {
        let warmed = {
            let mut idle = self.idle.lock().await;
            let queue = idle.entry(dc).or_default();
            let mut found = None;
            while let Some(item) = queue.pop_front() {
                if item.created.elapsed() < POOL_MAX_AGE {
                    found = Some(item.ws);
                    break;
                }
            }
            found
        };

        tokio::spawn(async move {
            {
                let idle = pool_kws().idle.lock().await;
                let busy = idle.get(&dc).is_some_and(|queue| !queue.is_empty());
                drop(idle);
                if busy {
                    return;
                }
            }
            match ws_to_dc(dc).await {
                Ok(ws) => {
                    let mut idle = pool_kws().idle.lock().await;
                    let queue = idle.entry(dc).or_default();
                    if queue.len() < 2 {
                        queue.push_back(PooledWs { ws, created: Instant::now() });
                    }
                }
                Err(e) => {
                    if std::env::var_os("GORILLA_WS_DEBUG").is_some() {
                        eprintln!("[dbg пул kws] DC{dc}: {e}");
                    }
                }
            }
        });

        match warmed {
            Some(ws) => Ok(ws),
            None => ws_to_dc(dc).await,
        }
    }
}

/// Жива ли точка `/apiws` на IP дата-центра: WS-рукопожатие и ответ на
/// `req_pq_multi` (resPQ), а не HTTP-страница.
pub async fn probe_kws(dc: i16) -> bool {
    let Ok(mut ws) = ws_to_dc(dc).await else {
        return false;
    };
    let (init, mut enc, mut dec) = generate_relay_handshake(dc);
    if ws.send(Message::Binary(init.to_vec().into())).await.is_err() {
        return false;
    }
    let mut frame = kws_packet(&mut enc);
    if ws.send(Message::Binary(frame.drain(..).collect::<Vec<u8>>().into())).await.is_err() {
        return false;
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        let Ok(Some(Ok(message))) = tokio::time::timeout(left, ws.next()).await else {
            return false;
        };
        let Message::Binary(bytes) = message else {
            continue;
        };
        let mut bytes = bytes.to_vec();
        dec.apply_keystream(&mut bytes);
        if looks_like_respq(&bytes) {
            return true;
        }
    }
}

/// Убирает служебный тег транспорта `ee ee ee ee` из начала клиентского
/// потока. Тег идёт первым и единственным (grammers `Intermediate::pack`
/// выставляет `init` после первой вставки), поэтому достаточно одного раза;
/// дальше в потоке идут обычные 4-байтовые длины кадров.
fn strip_transport_tag(pending: &mut Vec<u8>, stripped: &mut bool) {
    if *stripped || pending.len() < 4 {
        return;
    }
    if pending[..4] == [0xee; 4] {
        pending.drain(..4);
    }
    *stripped = true;
}

/// Ищет смещение первого правдоподобного intermediate-кадра в начале потока:
/// длина кадра лежит в первых 4 байтах, лежит в пределах разумного, и сам
/// кадр уже уместился. Так мы пропускаем возможное 64-байтовое эхо relay-init,
/// не отрезая настоящие данные.
fn first_frame_offset(stream: &[u8], cap: usize) -> Option<usize> {
    let mut off = 0usize;
    while off + 4 <= stream.len() && off <= 256 {
        let len = u32::from_le_bytes([
            stream[off],
            stream[off + 1],
            stream[off + 2],
            stream[off + 3],
        ]) as usize;
        if len >= 4 && len <= cap && stream.len() >= off + 4 + len {
            return Some(off);
        }
        off += 4;
    }
    None
}


/// Пакет `req_pq_multi` в intermediate-транспорте (без тега — его несёт init).
fn kws_packet(enc: &mut AesCtr256) -> Vec<u8> {
    let msg_id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or_default()
        * 4294967296.0;
    let msg_id = (msg_id as i64) & !3;
    let mut payload = Vec::new();
    payload.extend_from_slice(&[0u8; 8]);
    payload.extend_from_slice(&msg_id.to_le_bytes());
    payload.extend_from_slice(&20u32.to_le_bytes());
    payload.extend_from_slice(&0xbe7e_8ef1u32.to_le_bytes());
    payload.extend_from_slice(&[7u8; 16]);
    let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
    frame.extend_from_slice(&payload);
    enc.apply_keystream(&mut frame);
    frame
}

/// `0x7f` + `auth_key_id = 0` + конструктор `0x05162463` (resPQ) — доказательство
/// живого канала к дата-центру.
fn looks_like_respq(bytes: &[u8]) -> bool {
    if bytes.len() < 21 || bytes[0] != 0x7f {
        return false;
    }
    let payload = &bytes[1..];
    let auth_key_id = i64::from_le_bytes(payload[..8].try_into().unwrap_or([0; 8]));
    if auth_key_id != 0 {
        return false;
    }
    let constructor = u32::from_le_bytes(payload[8..12].try_into().unwrap_or([0; 4]));
    constructor == 0x0516_2463
}

/// Мост «WS-пул к web.telegram.org»: SOCKS5 → WSS → DC.
pub async fn spawn_kws_bridge() -> Result<SocketAddr, String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("не удалось занять локальный порт под мост: {e}"))?;
    let local = listener
        .local_addr()
        .map_err(|e| format!("адрес моста: {e}"))?;

    println!("[kws] локальный SOCKS5: {local} → WSS kws{{DC}}.web.telegram.org");

    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let _ = stream.set_nodelay(true);
                    tokio::spawn(async move {
                        if let Err(e) = serve_kws_client(stream).await {
                            eprintln!("[kws] соединение закрыто: {e}");
                        }
                    });
                }
                Err(e) => {
                    eprintln!("[kws] приём подключений: {e}");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    });

    Ok(local)
}

/// Одно клиентское подключение: SOCKS5 → тёплый WSS → DC.
async fn serve_kws_client(mut client: TcpStream) -> Result<(), String> {
    let (_, dc_idx) = socks_accept(&mut client).await?;
    let mut ws = pool_kws().acquire(dc_idx.abs()).await?;
    let (init, enc, dec) = generate_relay_handshake(dc_idx);
    ws.send(Message::Binary(init.to_vec().into()))
        .await
        .map_err(|e| format!("kws: рукопожатие: {e}"))?;
    pump(client, ws, enc, dec, Proto::Intermediate, "kws").await
}

#[cfg(test)]
mod kws_tests {
    /// Живой канал «WS-пул к web.telegram.org»: рукопожатие, init, `req_pq_multi`
    /// и разбор ответа. Признак успеха — resPQ от дата-центра.
    #[tokio::test]
    #[ignore = "живая сеть"]
    async fn kws_relay_answers_req_pq() {
        use aes::cipher::StreamCipher as _;
        use futures_util::{SinkExt as _, StreamExt as _};

        let mut ws = super::ws_to_dc(2).await.expect("WSS до DC2");
        let (init, mut enc, mut dec) = super::generate_relay_handshake(2);
        ws.send(tokio_tungstenite::tungstenite::Message::Binary(
            init.to_vec().into(),
        ))
        .await
        .expect("init");

        let frame = super::kws_packet(&mut enc);
        ws.send(tokio_tungstenite::tungstenite::Message::Binary(
            frame.into(),
        ))
        .await
        .expect("req_pq_multi");

        let raw = loop {
            let next = tokio::time::timeout(std::time::Duration::from_secs(12), ws.next())
                .await
                .expect("таймаут ответа")
                .expect("поток закрыт")
                .expect("ошибка ws");
            let tokio_tungstenite::tungstenite::Message::Binary(bytes) = next else {
                println!("пропущен не бинарный кадр: {next:?}");
                continue;
            };
            let mut bytes = bytes.to_vec();
            dec.apply_keystream(&mut bytes);
            println!("кадр {} байт: {:02x?}", bytes.len(), &bytes[..bytes.len().min(16)]);
            if bytes.len() > 4 {
                break bytes;
            }
        };
        assert!(
            super::looks_like_respq(&raw),
            "не resPQ: {:02x?}",
            &raw[..raw.len().min(24)]
        );
    }
}

#[cfg(test)]
mod dc_ws_tests {
    /// WSS прямо в дата-центр: 149.154.167.220:443 отвечает на TLS
    /// `HTTP/1.1 400 Bad Request`, значит это не голый TCP-транспорт,
    /// а WebSocket-точка `/apiws` (как kws*.web.telegram.org).
    #[tokio::test]
    #[ignore = "живая сеть"]
    async fn dc_ws_handshake() {
        let ip = super::dc_address(2).expect("адрес DC2");
        let url = format!("wss://{ip}/apiws?dst={ip}&dc=2");
        let ws = super::connect_ws(&url, "DC ws").await.expect("ws");
        use futures_util::{SinkExt as _, StreamExt as _};
        let (mut ws_send, mut ws_recv) = ws.split();
        let (init, mut enc, mut dec) = super::generate_relay_handshake(2);
        ws_send.send(tokio_tungstenite::tungstenite::Message::Binary(
            init.to_vec().into(),
        ))
        .await
        .expect("init frame");
        let msg_id = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("время")
            .as_secs_f64()
            * 4294967296.0) as i64
            & !3;
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0u8; 8]);
        payload.extend_from_slice(&msg_id.to_le_bytes());
        payload.extend_from_slice(&20u32.to_le_bytes());
        payload.extend_from_slice(&0xbe7e_8ef1u32.to_le_bytes());
        payload.extend_from_slice(&[7u8; 16]);
        let mut frame = payload;
        use aes::cipher::StreamCipher as _;
        enc.apply_keystream(&mut frame);
        ws_send.send(tokio_tungstenite::tungstenite::Message::Binary(
            frame.into(),
        ))
        .await
        .expect("data frame");
        let answer = tokio::time::timeout(std::time::Duration::from_secs(10), ws_recv.next())
            .await
            .expect("таймаут")
            .expect("кадр")
            .expect("ошибка ws");
        let mut raw = match answer {
            tokio_tungstenite::tungstenite::Message::Binary(b) => b.to_vec(),
            other => panic!("не бинарный кадр: {other:?}"),
        };
        println!("кадров {len}, первые байты {hex:02x?}", len = 1, hex = &raw[..raw.len().min(16)]);
        use aes::cipher::StreamCipher as _;
        dec.apply_keystream(&mut raw);
        println!("расшифровано: {:02x?}", &raw[..raw.len().min(16)]);
        let len = i32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
        assert!(len > 0 && len < 1_000_000, "плохая длина {len}");
    }
}


#[cfg(test)]
mod tgws_relay_tests {
    /// Живой релей до локального TgWsProxy: настоящий сценарий — init, затем
    /// `req_pq_multi` в intermediate-кадре (init объявляет тег `\xee\xee\xee\xee`).
    /// `cargo test --bin gorilla-bot tgws_relay -- --ignored --nocapture`

    /// Проверка гипотезы: апстрим ждёт служебный тег `\xee\xee\xee\xee`
    /// перед первым пакетом (так его шлёт grammers напрямую).
    #[tokio::test]
    #[ignore = "живая сеть"]
    async fn tgws_relay_preamble() {
        let addr: std::net::SocketAddr = "127.0.0.1:1443".parse().expect("адрес");
        let secret = super::parse_secret("e00a3136e590b94046bb639c875ec14a").expect("секрет");
        let (init, mut enc, mut dec) = super::generate_client_handshake(&secret, 2);
        let mut stream = tokio::net::TcpStream::connect(addr).await.expect("TCP до TgWsProxy");
        let _ = stream.set_nodelay(true);
        println!("[tgws] порт {}", stream.local_addr().expect("порт"));
        use aes::cipher::StreamCipher as _;
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        stream.write_all(&init).await.expect("отправка init");

        let mut frame = vec![0xeeu8, 0xee, 0xee, 0xee];
        frame.extend_from_slice(&super::kws_packet(&mut enc));
        enc.apply_keystream(&mut frame);
        println!("[tgws] кадр с тегом {} байт", frame.len());
        stream.write_all(&frame).await.expect("отправка req_pq");

        let mut buf = vec![0u8; 1024];
        match tokio::time::timeout(std::time::Duration::from_secs(12), stream.read(&mut buf)).await {
            Ok(Ok(0)) => println!("[tgws] EOF — апстрим закрыл сессию"),
            Ok(Ok(n)) => {
                let mut body = buf[..n].to_vec();
                dec.apply_keystream(&mut body);
                println!("[tgws] ответ {} байт: {:02x?}", n, &body[..body.len().min(20)]);
            }
            Ok(Err(e)) => println!("[tgws] ошибка чтения: {e}"),
            Err(_) => println!("[tgws] МОЛЧИТ 12 c"),
        }
    }

    #[tokio::test]
    #[ignore = "живая сеть"]
    async fn tgws_relay_echo() {
        let addr: std::net::SocketAddr = "127.0.0.1:1443".parse().expect("адрес");
        let secret = super::parse_secret("e00a3136e590b94046bb639c875ec14a").expect("секрет");
        let (init, mut enc, mut dec) = super::generate_client_handshake(&secret, 2);
        let mut stream = tokio::net::TcpStream::connect(addr).await.expect("TCP до TgWsProxy");
        let _ = stream.set_nodelay(true);
        println!("[tgws] порт {}", stream.local_addr().expect("порт"));
        use aes::cipher::StreamCipher as _;
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        stream.write_all(&init).await.expect("отправка init");

        let frame = super::kws_packet(&mut enc);
        println!("[tgws] кадр {} байт, заголовок {:02x?}", frame.len(), &frame[..4]);
        stream.write_all(&frame).await.expect("отправка req_pq");

        let mut buf = vec![0u8; 1024];
        match tokio::time::timeout(std::time::Duration::from_secs(15), stream.read(&mut buf)).await {
            Ok(Ok(0)) => println!("[tgws] EOF — апстрим закрыл сессию"),
            Ok(Ok(n)) => {
                let mut body = buf[..n].to_vec();
                dec.apply_keystream(&mut body);
                println!(
                    "[tgws] ответ {} байт, первые байты: {:02x?}",
                    n,
                    &body[..body.len().min(24)]
                );
            }
            Ok(Err(e)) => println!("[tgws] ошибка чтения: {e}"),
            Err(_) => println!("[tgws] МОЛЧИТ 15 c"),
        }
    }
}
