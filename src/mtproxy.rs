//! Локальный SOCKS5-мост к MTProto-прокси (например, TgWsProxy на 127.0.0.1:1443).
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

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use aes::Aes256;
use cipher::{KeyIvInit, StreamCipher};
use ctr::Ctr128BE;
use rand::RngCore;
use sha2::{Digest, Sha256};
use futures_util::{SinkExt as _, StreamExt as _};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;

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
    let hex = if raw.len() == 34
        && (raw.starts_with("dd") || raw.starts_with("ee"))
    {
        &raw[2..]
    } else {
        raw
    };
    let bytes: Result<Vec<u8>, _> = (0..16)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16))
        .collect();
    let bytes = bytes.map_err(|_| format!("не удалось разобрать секрет «{raw}»"))?;
    bytes.try_into().map_err(|_| "секрет должен быть 16 байт".into())
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
        let mut enc = make_cipher(&enc_key, &handshake[SKIP_LEN + PREKEY_LEN..SKIP_LEN + PREKEY_LEN + IV_LEN]);
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
        client.write_all(&[5, 0xff]).await.map_err(|e| e.to_string())?;
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
        return Err(format!("адрес {dst_ip} не опознан как известный DC Telegram"));
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
    let overall = started + std::time::Duration::from_secs(300);

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
            return Ok(Latched { upstream, enc, dec, backlog: up_plain, sent });
        }
        let reason = if bad_frame != 0 {
            format!("мусорный кадр len={bad_frame}")
        } else if echo_skipped < 64 {
            format!("эхо {echo_skipped}/64")
        } else if up_plain.is_empty() {
            "эхо есть, кадра нет".to_string()
        } else {
            format!("обрыв на {} Б кадра", up_plain.len())
        };
        println!("[мост] попытка {attempt} ({route_name}): апстрим «{reason}» — редайал");
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
    }

    Err(format!(
        "ни один сокет прокси не отдал живой ответ за {:.0} c — канал мёртв",
        started.elapsed().as_secs()
    ))
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

/// Одно клиентское подключение через worker: SOCKS5 → WSS → DC.
async fn serve_worker_client(mut client: TcpStream, worker: &str) -> Result<(), String> {
    let (dst_ip, dc_idx) = socks_accept(&mut client).await?;
    let url = format!(
        "wss://{worker}/apiws?dst={dst_ip}&dc={}",
        dc_idx.abs()
    );

    let (ws, _response) = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        tokio_tungstenite::connect_async(&url),
    )
    .await
    .map_err(|_| format!("таймаут WSS-подключения к {worker}"))?
    .map_err(|e| format!("WSS {worker}: {e}"))?;

    let (mut ws_send, mut ws_recv) = ws.split();
    let (mut from_client, mut to_client) = client.split();

    let up = async {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let n = from_client
                .read(&mut buf)
                .await
                .map_err(|e| format!("клиент→воркер: {e}"))?;
            if n == 0 {
                return Ok::<(), String>(());
            }
            ws_send
                .send(Message::Binary(buf[..n].to_vec().into()))
                .await
                .map_err(|e| format!("отправка в воркер: {e}"))?;
        }
    };
    let down = async {
        loop {
            match ws_recv.next().await {
                Some(Ok(Message::Binary(bytes))) => {
                    to_client
                        .write_all(&bytes)
                        .await
                        .map_err(|e| format!("воркер→клиент: {e}"))?;
                }
                Some(Ok(Message::Text(text))) => {
                    to_client
                        .write_all(text.as_str().as_bytes())
                        .await
                        .map_err(|e| format!("воркер→клиент: {e}"))?;
                }
                Some(Ok(Message::Close(_))) | None => return Ok::<(), String>(()),
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(format!("WSS-поток: {e}")),
            }
        }
    };

    tokio::select! {
        result = up => result,
        result = down => result,
    }
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
    let Latched { upstream, enc, dec, .. } = latched;
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
                let n = from_client.read(&mut buf).await.map_err(|e| e.to_string())?;
                if n == 0 {
                    to_upstream.shutdown().await.map_err(|e| e.to_string())?;
                    return Ok::<(), String>(());
                }
                let data = &mut buf[..n];
                enc.lock().await.apply_keystream(data);
                to_upstream.write_all(data).await.map_err(|e| e.to_string())?;
            }
        }
    };
    let down = {
        let dec = Arc::clone(&dec);
        async move {
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                let n = from_upstream.read(&mut buf).await.map_err(|e| e.to_string())?;
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
