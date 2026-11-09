//! gorilla-bot — автоигра в казино Bot Gorilla в Telegram (мартингейл).
//!
//! Режимы (передаются первым аргументом):
//! * `login`   — вход в Telegram-аккаунт (интерактивный, делается один раз);
//! * `scan`    — выгрузить последние сообщения чата с ботом в `gorilla_scan.txt`;
//! * `balance` — спросить у бота баланс и показать ответ;
//! * `casino`  — автоигра в казино с удвоением ставки после проигрыша.

mod casino;
mod config;

use std::io::{self, BufRead as _, Write as _};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use casino::{Martingale, Outcome};
use config::{CasinoConfig, Config};
use grammers_client::client::UpdateStream;
use grammers_client::peer::Peer;
use grammers_client::sender::{SenderPool, SenderPoolFatHandle, UpdatesConfiguration};
use grammers_client::session::storages::SqliteSession;
use grammers_client::session::types::{PeerId, PeerRef};
use grammers_client::session::updates::UpdatesLike;
use grammers_client::update::Update;
use grammers_client::{Client, SignInError};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const SCAN_FILE: &str = "gorilla_scan.txt";
const LOG_FILE: &str = "gorilla_log.txt";

/// Файлы данных (сессия, логи, скан) лежат рядом с config.json, а не в текущей папке.
static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

fn data_path(name: &str) -> PathBuf {
    match DATA_DIR.get() {
        Some(dir) => dir.join(name),
        None => PathBuf::from(name),
    }
}

#[tokio::main]
async fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
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
        "casino" => casino_mode(&cfg).await,
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
         \x20 casino   — авто-казино: ставка растёт после проигрыша, сброс после выигрыша"
    );
}

/// Подключение: открывает сессию, поднимает пул, проверяет авторизацию.
async fn connect(
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

    let SenderPool {
        runner,
        handle,
        updates,
    } = SenderPool::new(session, cfg.api_id);

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
async fn resolve_bot(client: &Client, username: &str) -> Result<(Peer, PeerRef), BoxError> {
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

/// `run balance`: отправляет команду баланса и печатает первый ответ бота.
async fn balance(cfg: &Config) -> Result<(), BoxError> {
    let (client, updates, handle, pool_task) = connect(cfg, false).await?;
    let (bot, bot_ref) = resolve_bot(&client, &cfg.bot_username).await?;
    let bot_id = bot.id();
    let mut stream = client
        .stream_updates(updates, UpdatesConfiguration { catch_up: true })
        .await?;

    let sent = client
        .send_message(bot_ref, cfg.casino.balance_command.as_str())
        .await?;
    println!("Отправлено: «{}»", cfg.casino.balance_command);

    match wait_reply(
        &mut stream,
        bot_id,
        sent.id(),
        Duration::from_secs(cfg.casino.reply_timeout_secs),
    )
    .await?
    {
        Some(text) => println!("Ответ бота:\n{text}"),
        None => println!("Бот не ответил за {} с.", cfg.casino.reply_timeout_secs),
    }

    stream.sync_update_state().await?;
    handle.quit();
    let _ = pool_task.await;
    Ok(())
}

/// `run casino`: автоигра. Останавливается по Ctrl+C, лимиту проигрышей,
/// нераспознанному ответу, отсутствию средств или таймауту.
async fn casino_mode(cfg: &Config) -> Result<(), BoxError> {
    let (client, updates, handle, pool_task) = connect(cfg, false).await?;
    let (bot, bot_ref) = resolve_bot(&client, &cfg.bot_username).await?;
    let bot_id = bot.id();

    println!(
        "Бот: @{} (id {}) | ставка {}/x{}/до {}, лимит проигрышей подряд {}",
        cfg.bot_username,
        bot_id,
        cfg.casino.start_bet,
        cfg.casino.multiplier,
        cfg.casino.max_bet,
        cfg.casino.max_losses_in_row
    );
    println!("Ctrl+C — остановка.");

    let mut stream = client
        .stream_updates(updates, UpdatesConfiguration { catch_up: true })
        .await?;
    let mut mg = Martingale::new(&cfg.casino);

    let mut result: Result<(), BoxError> = Ok(());
    let mut aborted = false;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => { aborted = true; }
        r = casino_loop(&client, &mut stream, &bot_ref, bot_id, &cfg.casino, &mut mg) => {
            result = r;
        }
    }

    if aborted {
        println!("\nОстановлено вручную.");
    }
    println!("{}", mg.stats());
    append_log(&format!("[stop] aborted={aborted} {}", mg.stats()))?;

    stream.sync_update_state().await?;
    handle.quit();
    let _ = pool_task.await;
    result
}

/// Главный игровой цикл.
async fn casino_loop(
    client: &Client,
    stream: &mut UpdateStream,
    bot_ref: &PeerRef,
    bot_id: PeerId,
    cfg: &CasinoConfig,
    mg: &mut Martingale,
) -> Result<(), BoxError> {
    let reply_timeout = Duration::from_secs(cfg.reply_timeout_secs);
    let round_gap = Duration::from_secs(cfg.round_gap_secs);

    loop {
        let bet = mg.current_bet();
        let command = cfg.command.replace("{bet}", &bet.to_string());
        println!("[ставка {bet}] → {command}");
        append_log(&format!("[cmd] {command}"))?;

        let sent = client.send_message(bot_ref.clone(), command.as_str()).await?;
        mg.note_round();

        let Some(reply) = wait_reply(stream, bot_id, sent.id(), reply_timeout).await? else {
            println!("Бот не ответил за {} с — останавливаюсь.", cfg.reply_timeout_secs);
            append_log("[timeout] бот не ответил")?;
            return Ok(());
        };
        append_log(&format!("[bot] {reply}"))?;

        match casino::parse_reply(&reply) {
            Outcome::Win => {
                mg.on_win();
                println!("  Выигрыш! Ставка сброшена до {}.", mg.current_bet());
                tokio::time::sleep(round_gap).await;
            }
            Outcome::Lose => {
                if mg.on_lose() {
                    println!("  Проигрыш. Следующая ставка: {}.", mg.current_bet());
                } else {
                    println!(
                        "  Проигрыш. Достигнут лимит {} подряд — останавливаюсь.",
                        cfg.max_losses_in_row
                    );
                    return Ok(());
                }
                tokio::time::sleep(round_gap).await;
            }
            Outcome::Cooldown(secs) => {
                let wait = secs + cfg.cooldown_margin_secs;
                println!("  Кулдаун от бота: жду {wait} с и повторяю.");
                tokio::time::sleep(Duration::from_secs(wait)).await;
            }
            Outcome::NoFunds => {
                println!("  Средств на ставку не хватает — останавливаюсь.");
                return Ok(());
            }
            Outcome::Unknown(text) => {
                println!("  Ответ бота не распознан: {text}");
                println!("  Останавливаюсь (данные — в {LOG_FILE} и {SCAN_FILE}).");
                return Ok(());
            }
        }
    }
}

/// Ждёт нового сообщения от бота (не наше, id больше after_id) до дедлайна.
async fn wait_reply(
    stream: &mut UpdateStream,
    bot_id: PeerId,
    after_id: i32,
    timeout: Duration,
) -> Result<Option<String>, BoxError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let update = match tokio::time::timeout_at(deadline, stream.next()).await {
            Ok(Ok(update)) => update,
            Ok(Err(e)) => return Err(e.to_string().into()),
            Err(_elapsed) => return Ok(None),
        };
        let Update::NewMessage(msg) = update else {
            continue;
        };
        if msg.outgoing() || msg.id() <= after_id || msg.peer_id() != bot_id {
            continue;
        }
        return Ok(Some(msg.text().trim().to_string()));
    }
}

/// Дописывает строку в gorilla_log.txt с unix-временем.
fn append_log(line: &str) -> Result<(), BoxError> {
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
