//! gorilla-bot — автоигра в казино Bot Gorilla в Telegram (мартингейл).
//!
//! Режимы (передаются первым аргументом):
//! * `login`   — вход в Telegram-аккаунт (интерактивный, делается один раз);
//! * `scan`    — выгрузить последние сообщения чата с ботом в `gorilla_scan.txt`;
//! * `balance` — спросить у бота баланс и показать ответ;
//! * `casino`  — автоигра в казино с удвоением ставки после проигрыша.

mod casino;
mod config;
mod engine;
mod gui;
mod mtproxy;

use std::io::{self, BufRead as _, Write as _};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use casino::{Martingale, Outcome};
use config::{CasinoConfig, Config};
use grammers_client::client::UpdateStream;
use grammers_client::peer::Peer;
use grammers_client::sender::{ConnectionParams, SenderPool, SenderPoolFatHandle, UpdatesConfiguration};
use grammers_mtproto::transport::Intermediate;
use grammers_mtsender::ServerAddr;
use grammers_client::session::storages::SqliteSession;
use grammers_client::session::types::{PeerId, PeerRef};
use grammers_client::session::updates::UpdatesLike;
use grammers_client::update::Update;
use grammers_client::{Client, SignInError};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub(crate) type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Отмена не запрошена — для CLI-режимов, где остановка по Ctrl+C.
static NO_CANCEL: AtomicBool = AtomicBool::new(false);

const SCAN_FILE: &str = "gorilla_scan.txt";
const LOG_FILE: &str = "gorilla_log.txt";

/// Файлы данных (сессия, логи, скан) лежат рядом с config.json, а не в текущей папке.
pub(crate) static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

pub(crate) fn data_path(name: &str) -> PathBuf {
    match DATA_DIR.get() {
        Some(dir) => dir.join(name),
        None => PathBuf::from(name),
    }
}

fn main() {
    // Явно выбираем крипто-провайдер TLS (ring), иначе rustls не может
    // решить между ring и aws-lc-rs и паникует при WSS-подключении.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let mode = std::env::args().nth(1).unwrap_or_default();
    if mode.is_empty() || mode == "gui" {
        // GUI живёт на главном потоке (winit), движок — в своём.
        gui::start();
        return;
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("не удалось создать tokio runtime");
    rt.block_on(run_cli(mode));
}

async fn run_cli(mode: String) {
    let cfg = match Config::load() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("Ошибка конфигурации: {e}");
            std::process::exit(1);
        }
    };
    let _ = DATA_DIR.set(cfg.base_dir.clone());

    let result = match mode.as_str() {
        "login" => login(&cfg).await,
        "scan" => scan(&cfg).await,
        "balance" => balance(&cfg).await,
        "pet" => pet_mode(&cfg).await,
        "bonus" => bonus_mode(&cfg).await,
        "casino" => casino_mode(&cfg).await,
        "check" => check(&cfg).await,
        "probe" => probe_mode(&cfg).await,
        other => {
            eprintln!("Неизвестный режим «{other}».");
            print_usage();
            std::process::exit(2);
        }
    };

    if let Err(e) = result {
        eprintln!("Ошибка: {e}");
        std::process::exit(1);
    }
}

fn print_usage() {
    println!(
        "Использование: gorilla-bot <режим>\n\
         \x20 login    — вход в Telegram (один раз, интерактивно)\n\
         \x20 scan [N] — последние N сообщений с ботом → {SCAN_FILE} (по умолчанию 60)\n\
         \x20 balance  — запросить у бота баланс и показать ответ\n\
         \x20 pet      — «Питомец поход», при просьбе покормить — покормить и повторить\n\
         \x20 bonus    — отправить «Бонус» в чат из bonus.target (проверка)\n\
         \x20 casino   — авто-казино: ставка растёт после проигрыша, сброс после выигрыша\n\
         \x20 check    — проверить канал связи (MTProto-прокси/SOCKS5), без логина"
    );
}

/// Подключение: открывает сессию, поднимает пул, проверяет авторизацию.
pub(crate) async fn connect(
    cfg: &Config,
    allow_unauthorized: bool,
) -> Result<(Client, mpsc::Receiver<UpdatesLike>, SenderPoolFatHandle, JoinHandle<()>), BoxError>
{
    let session_path = data_path(&cfg.session_file);
    let session = Arc::new(
        SqliteSession::open(&session_path)
            .await
            .map_err(|e| format!("не удалось открыть {}: {e}", session_path.display()))?,
    );

    let proxy_url = if !cfg.mtproxy.worker.trim().is_empty() {
        // Cloudflare-worker: WSS-труба прямо к DC, DPI её не видит.
        let local = mtproxy::spawn_worker_bridge(cfg.mtproxy.worker.trim().to_string()).await?;
        Some(format!("socks5://{local}"))
    } else if !cfg.mtproxy.secret.trim().is_empty() {
        // Встроенный мост: локальный SOCKS5 → настоящий MTProto-прокси.
        let secret = mtproxy::parse_secret(&cfg.mtproxy.secret)?;
        let target = std::net::SocketAddr::new(
            cfg.mtproxy
                .host
                .trim()
                .parse()
                .map_err(|e| format!("mtproxy.host «{}»: {e}", cfg.mtproxy.host))?,
            cfg.mtproxy.port,
        );
        let local = mtproxy::spawn_socks_bridge(target, secret).await?;
        Some(format!("socks5://{local}"))
    } else if cfg
        .proxy
        .as_deref()
        .is_some_and(|p| !p.trim().is_empty())
    {
        cfg.proxy.clone()
    } else {
        None
    };
    let params = ConnectionParams {
        proxy_url,
        transport: if cfg.mtproxy.worker.trim().is_empty()
            && cfg.mtproxy.secret.trim().is_empty()
        {
            // Прямое подключение/внешний SOCKS: обычный «Full»-транспорт.
            Box::new(|| Box::new(grammers_mtproto::transport::Full::new()))
        } else {
            // Мост рассчитан на intermediate-кадровку (0xee).
            Box::new(|| Box::new(Intermediate::new()))
        },
        ..Default::default()
    };

    let SenderPool {
        runner,
        handle,
        updates,
    } = SenderPool::with_configuration(session, cfg.api_id, params);

    let client = Client::new(handle.clone());
    let pool_task = tokio::spawn(runner.run());

    if !allow_unauthorized && !client.is_authorized().await? {
        return Err(format!(
            "нет активной сессии в {} — сначала сделай `gorilla-bot login`",
            session_path.display()
        )
        .into());
    }

    Ok((client, updates, handle, pool_task))
}

/// Режим `probe`: замер реакции MTProto-прокси на handshake (диагностика секрета).
async fn probe_mode(cfg: &Config) -> Result<(), BoxError> {
    if cfg.mtproxy.secret.trim().is_empty() {
        return Err("в config.json не задан mtproxy.secret".into());
    }
    let secret = mtproxy::parse_secret(&cfg.mtproxy.secret)?;
    let target = std::net::SocketAddr::new(
        cfg.mtproxy
            .host
            .trim()
            .parse()
            .map_err(|e| format!("mtproxy.host «{}»: {e}", cfg.mtproxy.host))?,
        cfg.mtproxy.port,
    );
    println!("Зонд {target} с секретом из конфига…");
    println!("{}", mtproxy::probe(target, secret).await);
    Ok(())
}

/// Проверка канала связи: поднимает мост/прокси из конфига и делает
/// настоящий MTProto-обмен (генерация ключа авторизации) с DC2 —
/// без входа и без отправки кода на телефон.
async fn check(cfg: &Config) -> Result<(), BoxError> {
    let proxy_url = if !cfg.mtproxy.worker.trim().is_empty() {
        let local = mtproxy::spawn_worker_bridge(cfg.mtproxy.worker.trim().to_string()).await?;
        format!("socks5://{local}")
    } else if !cfg.mtproxy.secret.trim().is_empty() {
        let secret = mtproxy::parse_secret(&cfg.mtproxy.secret)?;
        let target = std::net::SocketAddr::new(
            cfg.mtproxy
                .host
                .trim()
                .parse()
                .map_err(|e| format!("mtproxy.host «{}»: {e}", cfg.mtproxy.host))?,
            cfg.mtproxy.port,
        );
        let local = mtproxy::spawn_socks_bridge(target, secret).await?;
        format!("socks5://{local}")
    } else if let Some(p) = cfg
        .proxy
        .as_deref()
        .filter(|p| !p.trim().is_empty())
    {
        p.to_string()
    } else {
        println!("Прямое подключение (прокси не настроен).");
        String::new()
    };

    // Main-адрес DC2: мост сначала пробует main-маршрут, потом медиа.
    // Через worker — IP передаётся воркеру как dst (там он же и открывается).
    let dc2: std::net::SocketAddr = if cfg.mtproxy.worker.trim().is_empty() {
        "149.154.167.50:443".parse().unwrap()
    } else {
        "149.154.167.51:443".parse().unwrap()
    };
    let addr = if proxy_url.is_empty() {
        ServerAddr::Tcp { address: dc2 }
    } else {
        ServerAddr::Proxied {
            address: dc2,
            proxy: proxy_url,
        }
    };

    // Живой сокет — лотерея (мост сам делает редайалы до первого кадра,
    // но обрывы возможны и середине DH), поэтому несколько попыток.
    let use_intermediate =
        !cfg.mtproxy.worker.trim().is_empty() || !cfg.mtproxy.secret.trim().is_empty();
    let mut last_err = String::new();
    let mut ok = false;
    for attempt in 1..=3u32 {
        println!("Попытка {attempt} из 3: генерация ключа авторизации с DC2…");
        let result = if use_intermediate {
            grammers_mtsender::connect(Intermediate::new(), addr.clone())
                .await
                .map(|_| ())
        } else {
            grammers_mtsender::connect(grammers_mtproto::transport::Full::new(), addr.clone())
                .await
                .map(|_| ())
        };
        match result {
            Ok(()) => {
                ok = true;
                break;
            }
            Err(e) => {
                last_err = e.to_string();
                eprintln!("  не вышло: {e}");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }

    if ok {
        println!("✔️ Канал работает: ключ авторизации сгенерирован через DC2.");
        Ok(())
    } else {
        eprintln!("❌ Канал не работает: {last_err}");
        Err("проверка канала не прошла".into())
    }
}

/// Читает строку с клавиатуры (для интерактивного логина).
fn prompt(message: &str) -> io::Result<String> {
    let mut out = io::stdout().lock();
    out.write_all(message.as_bytes())?;
    out.flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(line)
}

/// Находит бота по username и возвращает его (Peer, PeerRef).
pub(crate) async fn resolve_bot(client: &Client, username: &str) -> Result<(Peer, PeerRef), BoxError> {
    let username = username.trim().trim_start_matches('@');
    let peer = client
        .resolve_username(username)
        .await?
        .ok_or(format!("@{username} не найден"))?;
    let peer_ref = peer.to_ref().await?.ok_or(format!(
        "не удалось получить доступ к @{username}: пусть бот появится в диалогах \
         (напиши ему /start из официального клиента) и запусти снова"
    ))?;
    Ok((peer, peer_ref))
}

/// Ищет чат/канал по @username или по названию среди диалогов аккаунта.
pub(crate) async fn resolve_chat(client: &Client, target: &str) -> Result<PeerRef, BoxError> {
    let want = target.trim();
    if want.is_empty() {
        return Err("не задан чат (bonus.target в config.json)".into());
    }

    // 1) Похоже на @username — спрашиваем Telegram напрямую.
    let username = want.trim_start_matches('@');
    if !username.is_empty() && !username.contains(char::is_whitespace) {
        match client.resolve_username(username).await {
            Ok(Some(peer)) => {
                if let Some(peer_ref) = peer.to_ref().await? {
                    return Ok(peer_ref);
                }
            }
            Ok(None) => {}
            // Не критично: цель может быть названием чата, а не username.
            Err(e) => eprintln!("@{username}: {e}"),
        }
    }

    // 2) Ищем по названию среди диалогов: точное совпадение важнее частичного.
    let needle = want.to_lowercase();
    let mut partial: Option<PeerRef> = None;
    let mut dialogs = client.iter_dialogs();
    while let Some(dialog) = dialogs.next().await? {
        let Some(title) = dialog.peer().name() else {
            continue;
        };
        let title = title.to_lowercase();
        if title == needle {
            return Ok(dialog.peer_ref());
        }
        if partial.is_none() && (title.contains(&needle) || needle.contains(&title)) {
            partial = Some(dialog.peer_ref());
        }
    }

    partial.ok_or_else(|| {
        format!(
            "чат «{want}» не найден ни по username, ни среди диалогов — он должен быть в списке чатов аккаунта"
        )
        .into()
    })
}

/// `run bonus`: одноразовая отправка «Бонус» в настроенный чат.
async fn bonus_mode(cfg: &Config) -> Result<(), BoxError> {
    let (client, _updates, handle, pool_task) = connect(cfg, false).await?;
    let peer = resolve_chat(&client, &cfg.bonus.target).await?;
    println!(
        "Чат «{}» найден (id {}), отправляю «{}»...",
        cfg.bonus.target, peer.id, cfg.bonus.command
    );
    let sent = client.send_message(peer, cfg.bonus.command.as_str()).await?;
    println!("Отправлено (id {}).", sent.id());

    // Бот отвечает сроком до следующего бонуса — его и запланируем.
    let mut none: Option<UpdateStream> = None;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    match wait_reply(
        &client,
        &mut none,
        peer,
        peer.id,
        sent.id(),
        deadline,
        &NO_CANCEL,
        false,
    )
    .await
    {
        Ok(Wait::Reply(_, text)) => {
            println!("Бот: {}", one_line(&text));
            match crate::engine::parse_bonus_delay(&text) {
                Some(secs) => println!(
                    "Следующий бонус по ответу бота: {} мин (разброс ±{} мин)",
                    secs / 60,
                    cfg.bonus.jitter_secs / 60
                ),
                None => println!(
                    "Срок в ответе не нашёлся — в GUI будет запасной {} мин",
                    cfg.bonus.interval_secs / 60
                ),
            }
        }
        Ok(Wait::Timeout) => println!("Бот не ответил за 60 с."),
        Ok(Wait::Cancelled) => {}
        Err(e) => println!("Ошибка ожидания ответа: {e}"),
    }

    handle.quit();
    let _ = pool_task.await;
    Ok(())
}

/// `run login`: вход по телефону и коду (или паролю 2FA), сессия сохраняется в файл.
async fn login(cfg: &Config) -> Result<(), BoxError> {
    let (client, _updates, handle, pool_task) = connect(cfg, true).await?;

    if client.is_authorized().await? {
        println!("Сессия уже активна, повторный вход не нужен.");
    } else {
        let phone = if cfg.phone.trim().is_empty() || cfg.phone.trim() == "+" {
            prompt("Телефон в формате +79991234567: ")?.trim().to_string()
        } else {
            cfg.phone.trim().to_string()
        };

        let token = client
            .request_login_code(&phone, &cfg.api_hash)
            .await
            .map_err(|e| format!("не удалось отправить код на {phone}: {e}"))?;
        println!("Код отправлен, проверь Telegram (или SMS).");

        let code = prompt("Код из Telegram: ")?;
        match client.sign_in(&token, code.trim()).await {
            Ok(_) => {}
            Err(SignInError::PasswordRequired(password_token)) => {
                let hint = password_token.hint().unwrap_or("нет подсказки");
                let password = prompt(&format!("Пароль 2FA (подсказка: {hint}): "))?;
                client
                    .check_password(password_token, password.trim())
                    .await
                    .map_err(|e| format!("неверный пароль 2FA: {e}"))?;
            }
            Err(e) => return Err(format!("вход не удался: {e}").into()),
        }
    }

    let me = client.get_me().await?;
    println!(
        "Готово, {}! Сессия сохранена в {}.",
        me.first_name().unwrap_or("?"),
        data_path(&cfg.session_file).display()
    );

    handle.quit();
    let _ = pool_task.await;
    Ok(())
}

/// `run scan [N]`: выгружает историю чата с ботом в gorilla_scan.txt (старые сверху).
async fn scan(cfg: &Config) -> Result<(), BoxError> {
    let count: usize = std::env::args()
        .nth(2)
        .and_then(|a| a.parse().ok())
        .unwrap_or(60);

    let (client, _updates, handle, pool_task) = connect(cfg, false).await?;
    let (bot, bot_ref) = resolve_bot(&client, &cfg.bot_username).await?;
    println!(
        "Бот: @{} (id {}), забираю до {count} последних сообщений...",
        cfg.bot_username,
        bot.id()
    );

    let mut messages = Vec::new();
    let mut iter = client.iter_messages(bot_ref).limit(count);
    while let Some(msg) = iter.next().await? {
        messages.push((
            msg.id(),
            msg.date(),
            msg.outgoing(),
            msg.text().trim().to_string(),
        ));
    }
    drop(iter);

    // newest-to-oldest переворачиваем в хронологический порядок.
    messages.reverse();

    let mut out = String::new();
    use std::fmt::Write as _;
    for (id, date, outgoing, text) in &messages {
        let who = if *outgoing { "Я" } else { "Бот" };
        let _ = writeln!(out, "{date} | id={id} | {who} | {text}");
    }
    std::fs::write(data_path(SCAN_FILE), &out)?;
    println!(
        "Сохранено {} сообщений в {}.",
        messages.len(),
        data_path(SCAN_FILE).display()
    );

    handle.quit();
    let _ = pool_task.await;
    Ok(())
}

/// `run pet`: поход питомца; если бот просит покормить — кормим и повторяем.
async fn pet_mode(cfg: &Config) -> Result<(), BoxError> {
    let (client, _updates, handle, pool_task) = connect(cfg, false).await?;
    let (bot, bot_ref) = resolve_bot(&client, &cfg.bot_username).await?;
    let bot_id = bot.id();

    let reply = pet_cmd(
        &client,
        bot_ref,
        bot_id,
        &cfg.pet.command,
        cfg.pet.reply_timeout_secs,
    )
    .await?;
    let prep = reply.as_deref().and_then(engine::pet_prep_command);
    if let Some(prep) = prep {
        if cfg.pet.feed_when_hungry {
            println!("Питомец просит подготовку, отправляю: «{prep}»");
            pet_cmd(
                &client,
                bot_ref,
                bot_id,
                &prep,
                cfg.pet.reply_timeout_secs,
            )
            .await?;
            pet_cmd(
                &client,
                bot_ref,
                bot_id,
                &cfg.pet.command,
                cfg.pet.reply_timeout_secs,
            )
            .await?;
        }
    }

    handle.quit();
    let _ = pool_task.await;
    Ok(())
}

/// Отправляет команду боту и печатает ответ. `None` — бот молчит.
async fn pet_cmd(
    client: &Client,
    bot_ref: PeerRef,
    bot_id: PeerId,
    cmd: &str,
    timeout_secs: u64,
) -> Result<Option<String>, BoxError> {
    let sent = client.send_message(bot_ref, cmd).await?;
    println!("Отправлено: «{cmd}»");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    let mut none: Option<UpdateStream> = None;
    match wait_reply(client, &mut none, bot_ref, bot_id, sent.id(), deadline, &NO_CANCEL, false)
        .await?
    {
        Wait::Reply(_, text) => {
            println!("Ответ бота:\n{text}");
            Ok(Some(text))
        }
        Wait::Timeout => {
            println!("Бот не ответил за {timeout_secs} с.");
            Ok(None)
        }
        Wait::Cancelled => Ok(None),
    }
}

/// `run balance`: отправляет команду баланса и печатает первый ответ бота.
async fn balance(cfg: &Config) -> Result<(), BoxError> {
    let (client, _updates, handle, pool_task) = connect(cfg, false).await?;
    let (bot, bot_ref) = resolve_bot(&client, &cfg.bot_username).await?;
    let bot_id = bot.id();

    let sent = client
        .send_message(bot_ref, cfg.casino.balance_command.as_str())
        .await?;
    println!("Отправлено: «{}»", cfg.casino.balance_command);

    let deadline =
        tokio::time::Instant::now() + Duration::from_secs(cfg.casino.reply_timeout_secs);
    let mut no_stream: Option<UpdateStream> = None;
    match wait_reply(
        &client,
        &mut no_stream,
        bot_ref,
        bot_id,
        sent.id(),
        deadline,
        &NO_CANCEL,
        false,
    )
    .await?
    {
        Wait::Reply(_, text) => println!("Ответ бота:\n{text}"),
        Wait::Timeout => println!("Бот не ответил за {} с.", cfg.casino.reply_timeout_secs),
        Wait::Cancelled => println!("Отменено."),
    }

    handle.quit();
    let _ = pool_task.await;
    Ok(())
}

/// Сколько раз подряд переподключаться без прогресса, прежде чем сдаться.
const MAX_RECONNECTS: u32 = 7;

/// Как часто опрашивать историю чата в ожидании ответа.
const POLL_MS: u64 = 900;

/// Чем закончился игровой цикл.
enum Flow {
    /// Остановка по правилам игры (лимит, таймаут, нераспознанный ответ).
    Done,
    /// Обрыв связи — нужно переподключиться и продолжить.
    Reconnect,
}

/// Отправленная ставка, результат которой ещё не получен
/// (переживает переподключение, чтобы не ставить дважды вслепую).
struct Pending {
    sent_id: i32,
}

/// Результат ожидания ответа бота.
pub(crate) enum Wait {
    #[allow(dead_code)]
    Reply(i32, String),
    Timeout,
    /// Ожидание прервано: в GUI нажали «стоп».
    Cancelled,
}

/// `run casino`: автоигра. Останавливается по Ctrl+C, лимиту проигрышей,
/// нераспознанному ответу, отсутствию средств или таймауту.
/// При обрыве связи (WSS-воркер рвёт соединение) переподключается и
/// восстанавливает результат последней ставки по истории чата.
async fn casino_mode(cfg: &Config) -> Result<(), BoxError> {
    println!(
        "Бот: @{} | ставка {}/x{:.2}±{:.0}%/округление {}/до {}, лимит проигрышей подряд {}",
        cfg.bot_username,
        cfg.casino.start_bet,
        cfg.casino.multiplier,
        cfg.casino.multiplier_jitter * 100.0,
        cfg.casino.bet_round_to,
        cfg.casino.max_bet,
        cfg.casino.max_losses_in_row
    );
    println!("Ctrl+C — остановка.");

    let mut mg = Martingale::new(&cfg.casino);
    let mut pending: Option<Pending> = None;
    let mut attempts: u32 = 0;
    let mut rounds_seen: u64 = 0;
    let mut aborted = false;
    let mut last_err: Option<BoxError> = None;

    loop {
        // Подключение + бот (обе операции могут упасть при обрыве).
        let connected = async {
            let (client, updates, handle, pool_task) = connect(cfg, false).await?;
            let (bot, bot_ref) = resolve_bot(&client, &cfg.bot_username).await?;
            Ok::<_, BoxError>((client, updates, handle, pool_task, bot.id(), bot_ref))
        }
        .await;

        let (client, updates, handle, pool_task, bot_id, bot_ref) = match connected {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[связь] не удалось подключиться: {e}");
                append_log(&format!("[connect] {e}"))?;
                last_err = Some(e);
                attempts += 1;
                if attempts >= MAX_RECONNECTS {
                    break;
                }
                println!("[связь] повтор через 5 с ({attempts}/{MAX_RECONNECTS})…");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        println!("[связь] бот @{} (id {bot_id})", cfg.bot_username);

        // Поток обновлений — быстрый путь; если он не поднялся, остаётся опрос истории.
        let mut stream = match client
            .stream_updates(updates, UpdatesConfiguration { catch_up: true })
            .await
        {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("[updates] поток не поднялся: {e} — работаю опросом истории");
                append_log(&format!("[updates] {e}"))?;
                None
            }
        };

        let flow = tokio::select! {
            _ = tokio::signal::ctrl_c() => { aborted = true; Flow::Done }
            r = casino_loop(&client, &mut stream, bot_ref, bot_id, &cfg.casino, &mut mg, &mut pending) => {
                match r {
                    Ok(flow) => flow,
                    Err(e) => {
                        eprintln!("[связь] обрыв во время раунда: {e}");
                        append_log(&format!("[net] {e}"))?;
                        last_err = Some(e);
                        Flow::Reconnect
                    }
                }
            }
        };

        if let Some(s) = stream {
            let _ = tokio::time::timeout(Duration::from_secs(5), s.sync_update_state()).await;
        }
        handle.quit();
        let _ = pool_task.await;

        match flow {
            Flow::Done => break,
            Flow::Reconnect => {
                // Прогресс был (сыграли раунд) — счётчик обрывов сбрасываем.
                if mg.rounds() > rounds_seen {
                    rounds_seen = mg.rounds();
                    attempts = 0;
                }
                attempts += 1;
                if attempts >= MAX_RECONNECTS {
                    eprintln!(
                        "[связь] {MAX_RECONNECTS} обрывов подряд без прогресса — останавливаюсь."
                    );
                    break;
                }
                println!("[связь] переподключаюсь ({attempts}/{MAX_RECONNECTS}) через 3 с…");
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    }

    if aborted {
        println!("\nОстановлено вручную.");
    }
    println!("{}", mg.stats());
    append_log(&format!("[stop] aborted={aborted} {}", mg.stats()))?;

    match last_err {
        Some(e) if !aborted => Err(e),
        _ => Ok(()),
    }
}

/// Главный игровой цикл. Один вызов = одно подключение.
async fn casino_loop(
    client: &Client,
    stream: &mut Option<UpdateStream>,
    bot_ref: PeerRef,
    bot_id: PeerId,
    cfg: &CasinoConfig,
    mg: &mut Martingale,
    pending: &mut Option<Pending>,
) -> Result<Flow, BoxError> {
    // После обрыва ответ на прошлую ставку мог прийти, пока связи не было.
    if let Some(p) = pending.take() {
        println!("[восстановление] проверяю, чем закончилась ставка до обрыва…");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        match wait_reply(
            client,
            stream,
            bot_ref,
            bot_id,
            p.sent_id,
            deadline,
            &NO_CANCEL,
            true,
        )
        .await
        {
            Ok(Wait::Reply(_, text)) => {
                println!("  Бот: {}", one_line(&text));
                append_log(&format!("[bot-restored] {text}"))?;
                if !apply_outcome(&text, mg, cfg).await {
                    return Ok(Flow::Done);
                }
            }
            Ok(Wait::Cancelled) => return Ok(Flow::Done),
            Ok(Wait::Timeout) => {
                println!("[восстановление] ответа нет — повторяю ставку.");
                append_log("[retry] повтор ставки после обрыва")?;
            }
            Err(e) => {
                *pending = Some(p);
                return Err(e);
            }
        }
    }

    let reply_timeout = Duration::from_secs(cfg.reply_timeout_secs);

    loop {
        let bet = mg.current_bet();
        let command = cfg.command.replace("{bet}", &bet.to_string());
        println!("[ставка {bet}] → {command}");
        append_log(&format!("[cmd] {command}"))?;

        let sent = client.send_message(bot_ref, command.as_str()).await?;
        let sent_id = sent.id();
        mg.note_round();
        *pending = Some(Pending { sent_id });

        let deadline = tokio::time::Instant::now() + reply_timeout;
        match wait_reply(
            client,
            stream,
            bot_ref,
            bot_id,
            sent_id,
            deadline,
            &NO_CANCEL,
            true,
        )
        .await
        {
            Ok(Wait::Reply(_, text)) => {
                *pending = None;
                println!("  Бот: {}", one_line(&text));
                append_log(&format!("[bot] {text}"))?;
                if !apply_outcome(&text, mg, cfg).await {
                    return Ok(Flow::Done);
                }
            }
            Ok(Wait::Cancelled) => return Ok(Flow::Done),
            Ok(Wait::Timeout) => {
                *pending = None;
                println!("Бот не ответил за {} с — останавливаюсь.", cfg.reply_timeout_secs);
                append_log("[timeout] бот не ответил")?;
                return Ok(Flow::Done);
            }
            Err(e) => return Err(e),
        }
    }
}

/// Разбирает ответ бота и двигает мартингейл. `false` — пора остановиться.
async fn apply_outcome(text: &str, mg: &mut Martingale, cfg: &CasinoConfig) -> bool {
    match casino::parse_reply(text) {
        Outcome::Win => {
            mg.on_win();
            println!("  Выигрыш! Ставка сброшена до {}.", mg.current_bet());
            tokio::time::sleep(casino::round_gap(cfg)).await;
            true
        }
        Outcome::Lose => {
            if !mg.on_lose() {
                println!(
                    "  Проигрыш. Достигнут лимит {} подряд — останавливаюсь.",
                    cfg.max_losses_in_row
                );
                return false;
            }
            println!("  Проигрыш. Следующая ставка: {}.", mg.current_bet());
            tokio::time::sleep(casino::round_gap(cfg)).await;
            true
        }
        Outcome::Cooldown(secs) => {
            let wait = secs + cfg.cooldown_margin_secs;
            println!("  Кулдаун от бота: жду {wait} с и повторяю.");
            tokio::time::sleep(Duration::from_secs(wait)).await;
            true
        }
        Outcome::NoFunds => {
            println!("  Средств на ставку не хватает — останавливаюсь.");
            false
        }
        Outcome::Unknown(text) => {
            println!("  Ответ бота не распознан: {text}");
            println!("  Останавливаюсь (данные — в {LOG_FILE} и {SCAN_FILE}).");
            false
        }
    }
}

/// Ждёт ответ бота: мгновенно из потока обновлений, а если он потерялся
/// или поток умер — опросом истории чата (обрывы WSS не страшны).
pub(crate) async fn wait_reply(
    client: &Client,
    stream: &mut Option<UpdateStream>,
    bot_ref: PeerRef,
    bot_id: PeerId,
    after_id: i32,
    deadline: tokio::time::Instant,
    cancel: &AtomicBool,
    skip_noise: bool,
) -> Result<Wait, BoxError> {
    let mut poll = tokio::time::interval(Duration::from_millis(POLL_MS));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(Wait::Cancelled);
        }
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => return Ok(Wait::Timeout),
            _ = poll.tick() => {
                if let Some((id, text)) = latest_reply(client, bot_ref, after_id).await? {
                    if skip_noise && is_chat_noise(&text) {
                        continue;
                    }
                    return Ok(Wait::Reply(id, text));
                }
            }
            update = next_update(stream) => match update {
                Ok(Update::NewMessage(msg))
                    if !msg.outgoing() && msg.id() > after_id && msg.peer_id() == bot_id =>
                {
                    let text = msg.text().trim().to_string();
                    if skip_noise && is_chat_noise(&text) {
                        continue;
                    }
                    return Ok(Wait::Reply(msg.id(), text));
                }
                Ok(_) => {}
                Err(e) => {
                    eprintln!("[updates] поток обновлений умер: {e} — дальше опрос истории");
                    let _ = append_log(&format!("[updates] {e}"));
                    *stream = None;
                }
            },
        }
    }
}

/// Сообщения бота, которые не являются ответом на ставку (питомец, реклама).
/// Их игнорируем, чтобы не принять за результат раунда.
pub(crate) fn is_chat_noise(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("питомец")
        || lower.contains("питомца")
        || lower.contains("поход")
        || lower.contains("приглашай друзей")
}

/// Следующее обновление; если поток уже снят — ждём вечно (select игнорирует ветку).
async fn next_update(
    stream: &mut Option<UpdateStream>,
) -> Result<Update, grammers_client::InvocationError> {
    match stream.as_mut() {
        Some(s) => s.next().await,
        None => std::future::pending().await,
    }
}

/// Последнее сообщение бота с id > after_id, найденное в истории чата.
pub(crate) async fn latest_reply(
    client: &Client,
    bot_ref: PeerRef,
    after_id: i32,
) -> Result<Option<(i32, String)>, BoxError> {
    let mut best: Option<(i32, String)> = None;
    {
        let mut iter = client.iter_messages(bot_ref).limit(8);
        while let Some(msg) = iter.next().await? {
            if msg.outgoing() || msg.id() <= after_id {
                continue;
            }
            let text = msg.text().trim().to_string();
            if text.is_empty() {
                continue;
            }
            if best.as_ref().map_or(true, |(id, _)| msg.id() > *id) {
                best = Some((msg.id(), text));
            }
        }
    }
    Ok(best)
}

/// Первая непустая строка ответа бота — для консоли (полный текст уходит в лог).
pub(crate) fn one_line(text: &str) -> String {
    text.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Дописывает строку в gorilla_log.txt с unix-временем.
pub(crate) fn append_log(line: &str) -> Result<(), BoxError> {
    use std::fs::OpenOptions;
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(data_path(LOG_FILE))?;
    writeln!(file, "{secs} | {line}")?;
    Ok(())
}
