//! Движок автоигры: общее состояние, казино и «Питомец поход» по расписанию.
//!
//! GUI на egui только показывает состояние и шлёт команды через канал,
//! вся сеть и таймеры живут здесь, в отдельном tokio-рантайме.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use grammers_client::client::UpdateStream;
use grammers_client::sender::SenderPoolFatHandle;
use grammers_client::session::types::{PeerId, PeerRef};
use grammers_client::session::updates::UpdatesLike;
use grammers_client::Client;
use rand::Rng as _;
use tokio::sync::{mpsc, Mutex as AsyncMutex, Notify};
use tokio::task::JoinHandle;

use crate::casino::{self, Martingale, Outcome};
use crate::config::Config;
use crate::{append_log, data_path, one_line, resolve_chat, wait_reply, Wait, BoxError};

/// Файл с временем последней отправки питомца.
const STATE_FILE: &str = "gorilla_state.json";
/// Сколько строк держим в окне лога.
const MAX_LOG_LINES: usize = 800;
/// Сколько обрывов подряд без прогресса терпим.
const MAX_FAILS: u32 = 7;

type Conn = (
    Client,
    mpsc::Receiver<UpdatesLike>,
    SenderPoolFatHandle,
    JoinHandle<()>,
);

/// Что показать в окне (обновляет движок).
#[derive(Debug, Clone, Default)]
pub struct Status {
    pub connected: bool,
    pub casino_running: bool,
    pub rounds: u64,
    pub wins: u64,
    pub losses_in_row: u32,
    pub bet: u64,
    pub message: String,
    pub last_bot_reply: String,
}

/// Команды из GUI в движок.
pub enum Command {
    StartCasino,
    StopCasino,
    /// Отправить «Питомец поход» прямо сейчас.
    PetNow,
    /// Отправить «Бонус» в настроенный чат прямо сейчас.
    BonusNow,
    /// Отправить «Бонус» в личку бота прямо сейчас.
    BonusDmNow,
    /// Сохранить новые настройки в рантайме.
    SetConfig(Config),
    Shutdown,
}

/// Общее состояние GUI и движка.
pub struct Shared {
    cfg: Mutex<Config>,
    status: Mutex<Status>,
    log: Mutex<Vec<String>>,
    chat_conn: AsyncMutex<Option<Session>>,
    casino_on: AtomicBool,
    casino_gen: AtomicU64,
    casino_cancel: Mutex<Arc<AtomicBool>>,
    pet_wake: Notify,
    pet_forced: AtomicBool,
    last_pet: Mutex<Option<i64>>,
    next_pet: Mutex<Option<i64>>,
    /// Бонус в канал (t.me/gorillabot_double).
    bonus: BonusSlot,
    /// Бонус в личку бота (цикл ~12 часов).
    bonus_dm: BonusSlot,
}

/// Какой из авто-бонусов: канал или личка бота.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BonusKind {
    Channel,
    Dm,
}

impl BonusKind {
    fn cfg<'a>(self, cfg: &'a Config) -> &'a crate::config::BonusConfig {
        match self {
            BonusKind::Channel => &cfg.bonus,
            BonusKind::Dm => &cfg.bonus_dm,
        }
    }

    fn tag(self) -> &'static str {
        match self {
            BonusKind::Channel => "[бонус]",
            BonusKind::Dm => "[бонус ЛС]",
        }
    }
}

/// Состояние одного таймера бонуса.
pub struct BonusSlot {
    wake: Notify,
    lock: AsyncMutex<()>,
    forced: AtomicBool,
    last: Mutex<Option<i64>>,
    next: Mutex<Option<i64>>,
    /// Кэш найденного чата: (цель, peer).
    peer: AsyncMutex<Option<(String, PeerRef)>>,
}

impl BonusSlot {
    fn new(last: Option<i64>, next: Option<i64>) -> Self {
        Self {
            wake: Notify::new(),
            lock: AsyncMutex::new(()),
            forced: AtomicBool::new(false),
            last: Mutex::new(last),
            next: Mutex::new(next),
            peer: AsyncMutex::new(None),
        }
    }
}

struct Session {
    chat: Arc<Chat>,
    _conn: Conn,
}

/// Подключённый чат с ботом. `peer_lock` сериализует обмен, чтобы казино
/// и питомец не отправляли команды одновременно.
pub(crate) struct Chat {
    pub client: Client,
    pub bot_ref: PeerRef,
    pub bot_id: PeerId,
    pub peer_lock: AsyncMutex<()>,
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct PersistedState {
    #[serde(default)]
    last_pet_unix: Option<i64>,
    #[serde(default)]
    next_pet_unix: Option<i64>,
    #[serde(default)]
    last_bonus_unix: Option<i64>,
    #[serde(default)]
    next_bonus_unix: Option<i64>,
    #[serde(default)]
    last_bonus_dm_unix: Option<i64>,
    #[serde(default)]
    next_bonus_dm_unix: Option<i64>,
}

/// Интервал до следующего похода с разбросом: interval_secs ± interval_jitter_secs.
fn pet_interval(cfg: &crate::config::PetConfig) -> i64 {
    let base = cfg.interval_secs as i64;
    let jitter = (cfg.interval_jitter_secs as i64).min(base.max(1));
    let secs = if jitter > 0 {
        rand::rng().random_range(base - jitter..=base + jitter)
    } else {
        base
    };
    secs.max(1)
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn clock() -> String {
    use time::OffsetDateTime;
    let t = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    format!("{:02}:{:02}:{:02}", t.hour(), t.minute(), t.second())
}

fn state_path() -> PathBuf {
    data_path(STATE_FILE)
}

fn load_state() -> PersistedState {
    std::fs::read_to_string(state_path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_state(state: &PersistedState) -> Result<(), BoxError> {
    std::fs::write(state_path(), serde_json::to_string_pretty(state)?)?;
    Ok(())
}

impl Shared {
    pub fn new(cfg: Config) -> Arc<Self> {
        let state = load_state();
        Arc::new(Self {
            cfg: Mutex::new(cfg),
            status: Mutex::new(Status::default()),
            log: Mutex::new(Vec::new()),
            chat_conn: AsyncMutex::new(None),
            casino_on: AtomicBool::new(false),
            casino_gen: AtomicU64::new(0),
            casino_cancel: Mutex::new(Arc::new(AtomicBool::new(false))),
            pet_wake: Notify::new(),
            pet_forced: AtomicBool::new(false),
            last_pet: Mutex::new(state.last_pet_unix),
            next_pet: Mutex::new(state.next_pet_unix),
            bonus: BonusSlot::new(state.last_bonus_unix, state.next_bonus_unix),
            bonus_dm: BonusSlot::new(state.last_bonus_dm_unix, state.next_bonus_dm_unix),
        })
    }

    pub fn cfg(&self) -> Config {
        self.cfg.lock().unwrap().clone()
    }

    pub fn set_cfg(&self, cfg: Config) {
        let mut guard = self.cfg.lock().unwrap();
        *guard = cfg;
    }

    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    pub fn set_status(&self, f: impl FnOnce(&mut Status)) {
        let mut guard = self.status.lock().unwrap();
        f(&mut guard);
    }

    pub fn logs(&self) -> std::sync::MutexGuard<'_, Vec<String>> {
        self.log.lock().unwrap()
    }

    /// Строка в окно лога + в gorilla_log.txt.
    pub fn log(&self, line: impl Into<String>) {
        let line = line.into();
        {
            let mut log = self.log.lock().unwrap();
            log.push(format!("[{}] {line}", clock()));
            if log.len() > MAX_LOG_LINES {
                let extra = log.len() - MAX_LOG_LINES;
                log.drain(..extra);
            }
        }
        let _ = append_log(&line);
    }

    pub fn last_pet(&self) -> Option<i64> {
        *self.last_pet.lock().unwrap()
    }

    fn set_last_pet(&self, ts: i64) {
        let cfg = self.cfg();
        let next = ts + pet_interval(&cfg.pet);
        *self.last_pet.lock().unwrap() = Some(ts);
        *self.next_pet.lock().unwrap() = Some(next);
        self.persist();
    }

    /// Пишет gorilla_state.json (время питомца и бонуса).
    fn persist(&self) {
        let state = PersistedState {
            last_pet_unix: *self.last_pet.lock().unwrap(),
            next_pet_unix: *self.next_pet.lock().unwrap(),
            last_bonus_unix: *self.bonus.last.lock().unwrap(),
            next_bonus_unix: *self.bonus.next.lock().unwrap(),
            last_bonus_dm_unix: *self.bonus_dm.last.lock().unwrap(),
            next_bonus_dm_unix: *self.bonus_dm.next.lock().unwrap(),
        };
        if let Err(e) = save_state(&state) {
            self.log(format!("[состояние] не удалось сохранить время: {e}"));
        }
    }

    fn bonus_slot(&self, kind: BonusKind) -> &BonusSlot {
        match kind {
            BonusKind::Channel => &self.bonus,
            BonusKind::Dm => &self.bonus_dm,
        }
    }

    pub fn last_bonus(&self, kind: BonusKind) -> Option<i64> {
        *self.bonus_slot(kind).last.lock().unwrap()
    }

    fn set_last_bonus(&self, kind: BonusKind, ts: i64) {
        *self.bonus_slot(kind).last.lock().unwrap() = Some(ts);
        self.persist();
    }

    /// Срок следующего похода (например, скорый повтор, если бот молчит).
    fn set_next_pet(&self, ts: i64) {
        *self.next_pet.lock().unwrap() = Some(ts);
        self.persist();
    }

    fn next_bonus_at(&self, kind: BonusKind) -> Option<i64> {
        *self.bonus_slot(kind).next.lock().unwrap()
    }

    /// Время следующей отправки берём из ответа бота, а сюда кладём
    /// результат с разбросом.
    fn set_next_bonus(&self, kind: BonusKind, ts: i64) {
        *self.bonus_slot(kind).next.lock().unwrap() = Some(ts);
        self.persist();
    }

    /// Пересчитать срок бонуса (после смены настроек).
    fn reschedule_bonus(&self, kind: BonusKind) {
        let cfg = self.cfg();
        let bonus = kind.cfg(&cfg);
        let base = bonus.interval_secs as i64;
        let jitter = (bonus.jitter_secs as i64).min((base / 4).max(1));
        let last = self.last_bonus(kind).unwrap_or_else(now_unix);
        let delta = if jitter > 0 {
            rand::rng().random_range(base - jitter..=base + jitter).max(60)
        } else {
            base.max(60)
        };
        self.set_next_bonus(kind, last + delta);
    }

    /// Через сколько отправлять «Бонус». `None` — автоотправка выключена.
    pub fn next_bonus_in(&self, kind: BonusKind) -> Option<Duration> {
        let cfg = self.cfg();
        let bonus = kind.cfg(&cfg);
        if !bonus.enabled {
            return None;
        }
        let due = match self.next_bonus_at(kind) {
            Some(ts) => ts,
            None => match self.last_bonus(kind) {
                Some(ts) => ts + bonus.interval_secs as i64,
                None => now_unix(),
            },
        };
        Some(Duration::from_secs((due - now_unix()).max(0) as u64))
    }

    /// «Отправить сейчас»: будим таймер сразу.
    fn trigger_bonus(&self, kind: BonusKind) {
        let slot = self.bonus_slot(kind);
        slot.forced.store(true, Ordering::SeqCst);
        slot.wake.notify_waiters();
    }

    /// Когда поход по расписанию (unix-секунды).
    pub fn next_pet_at(&self) -> Option<i64> {
        *self.next_pet.lock().unwrap()
    }

    /// Пересчитать срок похода (после смены интервала в настройках).
    fn reschedule_pet(&self) {
        let Some(last) = self.last_pet() else {
            return;
        };
        let cfg = self.cfg();
        *self.next_pet.lock().unwrap() = Some(last + pet_interval(&cfg.pet));
    }

    /// Через сколько отправлять питомца. `None` — авто-поход выключен.
    pub fn next_pet_in(&self) -> Option<Duration> {
        let cfg = self.cfg();
        if !cfg.pet.enabled {
            return None;
        }
        let due = match self.next_pet_at() {
            Some(ts) => ts,
            None => match self.last_pet() {
                Some(ts) => ts + cfg.pet.interval_secs as i64,
                None => now_unix(),
            },
        };
        Some(Duration::from_secs((due - now_unix()).max(0) as u64))
    }

    /// Подключение (или переиспользование) чата с ботом.
    pub(crate) async fn chat(&self) -> Result<Arc<Chat>, BoxError> {
        let mut guard = self.chat_conn.lock().await;
        if let Some(session) = guard.as_ref() {
            return Ok(session.chat.clone());
        }

        let cfg = self.cfg();
        let conn = crate::connect(&cfg, false).await?;
        let (bot, bot_ref) = match crate::resolve_bot(&conn.0, &cfg.bot_username).await {
            Ok(v) => v,
            Err(e) => {
                conn.2.quit();
                return Err(e);
            }
        };
        let chat = Arc::new(Chat {
            client: conn.0.clone(),
            bot_ref,
            bot_id: bot.id(),
            peer_lock: AsyncMutex::new(()),
        });
        *guard = Some(Session {
            chat: chat.clone(),
            _conn: conn,
        });
        self.set_status(|s| {
            s.connected = true;
            s.message = format!("подключено, @{}", cfg.bot_username);
        });
        self.log(format!("[связь] подключено к @{}", cfg.bot_username));
        Ok(chat)
    }

    /// Закрыть соединение (после обрыва или по требованию).
    pub(crate) async fn reset_chat(&self) {
        let mut guard = self.chat_conn.lock().await;
        if let Some(Session {
            _conn: (_, _, handle, pool),
            ..
        }) = guard.take()
        {
            handle.quit();
            drop(pool);
        }
        self.set_status(|s| s.connected = false);
    }
}

/// Запуск движка в отдельном потоке со своим tokio-рантаймом.
pub fn spawn(shared: Arc<Shared>, mut rx: mpsc::UnboundedReceiver<Command>) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("не удалось создать tokio runtime для движка");
        rt.block_on(async move {
            spawn_pet(shared.clone());
            spawn_bonus(shared.clone(), BonusKind::Channel);
            spawn_bonus(shared.clone(), BonusKind::Dm);

            while let Some(cmd) = rx.recv().await {
                match cmd {
                    Command::StartCasino => {
                        if shared.casino_on.swap(true, Ordering::SeqCst) {
                            continue;
                        }
                        let gen = shared.casino_gen.fetch_add(1, Ordering::SeqCst) + 1;
                        let cancel = Arc::new(AtomicBool::new(false));
                        *shared.casino_cancel.lock().unwrap() = cancel.clone();
                        shared.set_status(|s| {
                            s.casino_running = true;
                            s.message = "казино запущено".into();
                        });
                        shared.log("[казино] старт");
                        tokio::spawn(casino_task(shared.clone(), gen, cancel));
                    }
                    Command::StopCasino => stop_casino(&shared, "остановлено пользователем"),
                    Command::PetNow => {
                        shared.pet_forced.store(true, Ordering::SeqCst);
                        shared.pet_wake.notify_waiters();
                    }
                    Command::BonusNow => shared.trigger_bonus(BonusKind::Channel),
                    Command::BonusDmNow => shared.trigger_bonus(BonusKind::Dm),
                    Command::SetConfig(cfg) => {
                        let pet_changed = {
                            let old = shared.cfg.lock().unwrap();
                            old.pet.enabled != cfg.pet.enabled
                                || old.pet.interval_secs != cfg.pet.interval_secs
                                || old.pet.interval_jitter_secs != cfg.pet.interval_jitter_secs
                        };
                        let bonus_changed = {
                            let old = shared.cfg.lock().unwrap();
                            bonus_differs(&old.bonus, &cfg.bonus)
                                || bonus_differs(&old.bonus_dm, &cfg.bonus_dm)
                        };
                        shared.set_cfg(cfg);
                        if pet_changed {
                            shared.reschedule_pet();
                            shared.pet_wake.notify_waiters();
                        }
                        if bonus_changed {
                            // Срок берётся из ответа бота; пересчитываем только
                            // если сохранённого срока ещё нет.
                            for kind in [BonusKind::Channel, BonusKind::Dm] {
                                if shared.next_bonus_at(kind).is_none() {
                                    shared.reschedule_bonus(kind);
                                }
                                shared.bonus_slot(kind).wake.notify_waiters();
                            }
                        }
                        shared.log("[настройки] сохранены");
                    }
                    Command::Shutdown => {
                        stop_casino(&shared, "приложение закрыто");
                        break;
                    }
                }
            }
        });
    });
}

fn stop_casino(shared: &Shared, reason: &str) {
    shared.casino_on.store(false, Ordering::SeqCst);
    shared.casino_gen.fetch_add(1, Ordering::SeqCst);
    shared.casino_cancel.lock().unwrap().store(true, Ordering::SeqCst);
    shared.set_status(|s| {
        s.casino_running = false;
        s.message = reason.to_string();
    });
    shared.log(format!("[казино] {reason}"));
}

fn alive(shared: &Shared, gen: u64, cancel: &AtomicBool) -> bool {
    shared.casino_on.load(Ordering::SeqCst)
        && shared.casino_gen.load(Ordering::SeqCst) == gen
        && !cancel.load(Ordering::Relaxed)
}

/// Пауза, которую можно прервать кнопкой «стоп».
async fn pause(d: Duration, cancel: &AtomicBool) -> bool {
    let deadline = tokio::time::Instant::now() + d;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return true;
        }
        tokio::time::sleep((deadline - now).min(Duration::from_millis(400))).await;
    }
}

/// Игровой цикл казино до остановки/лимита/обрыва связи.
async fn casino_task(shared: Arc<Shared>, gen: u64, cancel: Arc<AtomicBool>) {
    let mut mg = Martingale::new(&shared.cfg().casino);
    let mut pending: Option<i32> = None;
    let mut fails: u32 = 0;

    'outer: while alive(&shared, gen, &cancel) {
        mg.apply_config(&shared.cfg().casino);
        let chat = match shared.chat().await {
            Ok(chat) => {
                fails = 0;
                chat
            }
            Err(e) => {
                fails += 1;
                shared.log(format!("[связь] {e}"));
                shared.set_status(|s| {
                    s.connected = false;
                    s.message = format!("нет связи: {e}");
                });
                if fails >= MAX_FAILS {
                    stop_casino(&shared, "не удалось подключиться");
                    break;
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        loop {
            if !alive(&shared, gen, &cancel) {
                break 'outer;
            }
            let cfg = shared.cfg();

            // Ответ на прошлую ставку мог прийти во время обрыва.
            if let Some(sent_id) = pending {
                shared.log("[казино] проверяю, чем закончилась ставка до обрыва");
                let mut none: Option<UpdateStream> = None;
                let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
                let waited = {
                    let _guard = chat.peer_lock.lock().await;
                    wait_reply(
                        &chat.client,
                        &mut none,
                        chat.bot_ref,
                        chat.bot_id,
                        sent_id,
                        deadline,
                        &cancel,
                        true,
                    )
                    .await
                };
                match waited {
                    Ok(Wait::Reply(_, text)) => {
                        pending = None;
                        match apply_reply(&shared, &cfg, &mut mg, &text, &cancel).await {
                            Step::Continue => {}
                            Step::Stop(reason) => {
                                stop_casino(&shared, &reason);
                                break 'outer;
                            }
                        }
                    }
                    Ok(Wait::Timeout) => {
                        pending = None;
                        shared.log("[казино] ответа не было — ставлю заново");
                    }
                    Ok(Wait::Cancelled) => break 'outer,
                    Err(e) => {
                        shared.log(format!("[связь] {e}"));
                        shared.reset_chat().await;
                        continue 'outer;
                    }
                }
            }

            let bet = mg.current_bet();
            let command = cfg.casino.command.replace("{bet}", &bet.to_string());

            if !alive(&shared, gen, &cancel) {
                break 'outer;
            }
            let guard = chat.peer_lock.lock().await;
            shared.log(format!("[ставка {bet}] → {command}"));
            let sent = match chat.client.send_message(chat.bot_ref, command.as_str()).await {
                Ok(msg) => msg,
                Err(e) => {
                    drop(guard);
                    fails += 1;
                    shared.log(format!("[связь] {e}"));
                    shared.reset_chat().await;
                    if fails >= MAX_FAILS {
                        stop_casino(&shared, "обрыв связи");
                        break 'outer;
                    }
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    continue 'outer;
                }
            };
            mg.note_round();
            pending = Some(sent.id());
            shared.set_status(|s| {
                s.rounds = mg.rounds();
                s.wins = mg.wins();
                s.losses_in_row = mg.losses_in_row();
                s.bet = mg.current_bet();
                s.last_bot_reply.clear();
            });

            let mut waited = wait_round(&chat, sent.id(), cfg.casino.reply_timeout_secs, &cancel).await;
            // Бот может ответить позже таймаута (очередь на его стороне):
            // ждём ещё пару таких же окон, прежде чем сдаться.
            let mut attempt = 1;
            while matches!(waited, Ok(Wait::Timeout)) && attempt < MAX_REPLY_ATTEMPTS {
                attempt += 1;
                shared.log(format!(
                    "[казино] бот молчит {} с — жду ещё",
                    cfg.casino.reply_timeout_secs
                ));
                waited = wait_round(&chat, sent.id(), cfg.casino.reply_timeout_secs, &cancel).await;
            }
            drop(guard);

            match waited {
                Ok(Wait::Reply(_, text)) => {
                    fails = 0;
                    let step = apply_reply(&shared, &cfg, &mut mg, &text, &cancel).await;
                    // Обновляем плитки сразу после результата, а не к следующей
                    // ставке: иначе победа появляется в статусе только спустя раунд.
                    shared.set_status(|s| {
                        s.rounds = mg.rounds();
                        s.wins = mg.wins();
                        s.losses_in_row = mg.losses_in_row();
                        s.bet = mg.current_bet();
                    });
                    match step {
                        Step::Continue => {}
                        Step::Stop(reason) => {
                            stop_casino(&shared, &reason);
                            break 'outer;
                        }
                    }
                }
                Ok(Wait::Timeout) => {
                    pending = None;
                    stop_casino(&shared, "бот не ответил на ставку");
                    break 'outer;
                }
                Ok(Wait::Cancelled) => {
                    // Ставка уже отправлена: дождёмся её результата, чтобы
                    // не потерять выигрыш, но новые ставки не делаем.
                    if let Some(sent_id) = pending.take() {
                        settled_after_stop(&shared, &chat, sent_id, &mut mg).await;
                    }
                    break 'outer;
                }
                Err(e) => {
                    shared.log(format!("[связь] {e}"));
                    shared.reset_chat().await;
                    continue 'outer;
                }
            }
        }
    }

    shared.set_status(|s| {
        s.rounds = mg.rounds();
        s.wins = mg.wins();
        s.losses_in_row = mg.losses_in_row();
        s.bet = mg.current_bet();
    });
    shared.log(format!("[казино] итог: {}", mg.stats()));
}

/// Сколько окон ожидания даём боту на одну ставку.
const MAX_REPLY_ATTEMPTS: u32 = 3;

/// Ожидание ответа бота без учёта флага отмены: поздний ответ на уже
/// отправленную ставку важнее, чем немедленная остановка.
async fn wait_round(
    chat: &Chat,
    sent_id: i32,
    timeout_secs: u64,
    cancel: &AtomicBool,
) -> Result<Wait, BoxError> {
    let mut none: Option<UpdateStream> = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    wait_reply(
        &chat.client,
        &mut none,
        chat.bot_ref,
        chat.bot_id,
        sent_id,
        deadline,
        cancel,
        true,
    )
    .await
}

/// Дожидается результата ставки, отправленной перед остановкой казино,
/// и обновляет статистику (флаг отмены здесь намеренно игнорируется).
async fn settled_after_stop(
    shared: &Arc<Shared>,
    chat: &Chat,
    sent_id: i32,
    mg: &mut Martingale,
) {
    shared.log("[казино] жду результат последней ставки перед остановкой");
    let never = AtomicBool::new(false);
    let cfg = shared.cfg();
    let mut left = MAX_REPLY_ATTEMPTS;
    while left > 0 {
        left -= 1;
        match wait_round(chat, sent_id, cfg.casino.reply_timeout_secs, &never).await {
            Ok(Wait::Reply(_, text)) => {
                let _ = apply_reply(shared, &cfg, mg, &text, &never).await;
                return;
            }
            Ok(Wait::Timeout) => continue,
            Ok(Wait::Cancelled) | Err(_) => break,
        }
    }
    shared.log("[казино] результат последней ставки не пришёл");
}

enum Step {
    Continue,
    Stop(String),
}

/// Разбирает ответ бота, двигает мартингейл и выдерживает паузы.
async fn apply_reply(
    shared: &Shared,
    cfg: &Config,
    mg: &mut Martingale,
    text: &str,
    cancel: &AtomicBool,
) -> Step {
    let line = one_line(text);
    shared.set_status(|s| s.last_bot_reply = line.clone());
    shared.log(format!("[бот] {text}"));

    match casino::parse_reply(text) {
        Outcome::Win => {
            mg.on_win();
            shared.log(format!("[казино] выигрыш, ставка сброшена до {}", mg.current_bet()));
            if !pause(casino::round_gap(&cfg.casino), cancel).await {
                return Step::Stop("остановлено пользователем".into());
            }
            Step::Continue
        }
        Outcome::Lose => {
            if !mg.on_lose() {
                return Step::Stop(format!(
                    "лимит {} проигрышей подряд",
                    cfg.casino.max_losses_in_row
                ));
            }
            shared.log(format!(
                "[казино] проигрыш, следующая ставка {}",
                mg.current_bet()
            ));
            if !pause(casino::round_gap(&cfg.casino), cancel).await {
                return Step::Stop("остановлено пользователем".into());
            }
            Step::Continue
        }
        Outcome::Cooldown(secs) => {
            let wait = secs + cfg.casino.cooldown_margin_secs;
            shared.log(format!("[казино] кулдаун от бота: жду {wait} с"));
            if !pause(Duration::from_secs(wait), cancel).await {
                return Step::Stop("остановлено пользователем".into());
            }
            Step::Continue
        }
        Outcome::NoFunds => Step::Stop("не хватает средств на ставку".into()),
        Outcome::Unknown(_) => Step::Stop(format!("ответ бота не распознан: {line}")),
    }
}

/// Таймер питомца: спит до ближайшего срока и отправляет команду.
fn spawn_pet(shared: Arc<Shared>) {
    tokio::spawn(async move {
        loop {
            let Some(wait) = shared.next_pet_in() else {
                shared.pet_wake.notified().await;
                continue;
            };
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = shared.pet_wake.notified() => {}
            }

            let forced = shared.pet_forced.swap(false, Ordering::SeqCst);
            let cfg = shared.cfg();
            if !cfg.pet.enabled && !forced {
                continue;
            }
            if !forced {
                match shared.next_pet_in() {
                    Some(left) if left > Duration::from_millis(0) => continue,
                    _ => {}
                }
            }

            if let Err(e) = pet_once(&shared, &cfg).await {
                shared.log(format!("[питомец] ошибка: {e}"));
                shared.reset_chat().await;
                // Не долбим бота каждую секунду при проблемах со связью.
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        }
    });
}

/// Команда подготовки из ответа бота: он пишет её в кавычках после
/// «команда:» — «Питомец покормить», «Питомец поиграть» и т.п.
/// Список частных случаев не нужен: берём то, что назвал бот.
pub(crate) fn pet_prep_command(reply: &str) -> Option<String> {
    let after = reply
        .find("команда:")
        .or_else(|| reply.find("Команда:"))? 
        + "команда:".len();
    let tail = &reply[after..];
    let open = tail.find('«')?;
    let rest = &tail[open..];
    let close = rest.find('»')?;
    let cmd = rest['«'.len_utf8()..close].trim();
    if cmd.to_lowercase().starts_with("питомец") && cmd.len() > "питомец".len() {
        Some(cmd.to_string())
    } else {
        None
    }
}

async fn pet_once(shared: &Shared, cfg: &Config) -> Result<(), BoxError> {
    let chat = shared.chat().await?;

    let mut reply =
        pet_send_confirmed(shared, &chat, &cfg.pet.command, cfg.pet.reply_timeout_secs).await?;

    // Бот перед походом может потребовать подготовку — то «питомец покормить»,
    // то «питомец поиграть». Команду он называет сам в кавычках после
    // «команда:», поэтому просто выполняем её и повторяем поход.
    let mut steps = 0;
    while steps < 2 {
        let Some(prep) = reply.as_deref().and_then(pet_prep_command) else {
            break;
        };
        if !cfg.pet.feed_when_hungry {
            break;
        }
        shared.log(format!("[питомец] бот просит подготовку → {prep}"));
        pet_send_confirmed(shared, &chat, &prep, cfg.pet.reply_timeout_secs).await?;
        reply = pet_send_confirmed(shared, &chat, &cfg.pet.command, cfg.pet.reply_timeout_secs)
            .await?;
        steps += 1;
    }

    match reply {
        // Бот ответил — поход состоялся, следующий через интервал.
        Some(_) => {
            shared.set_last_pet(now_unix());
            Ok(())
        }
        // Бот промолчал: «последний поход» не двигаем, повторяем скоро —
        // иначе таймер уйдёт на полный интервал из-за потерянной команды.
        None => {
            shared.log(format!(
                "[питомец] бот молчит после {PET_ATTEMPTS} попыток — повторю через {}",
                fmt_secs(PET_RETRY_SOON as i64)
            ));
            shared.set_next_pet(now_unix() + PET_RETRY_SOON as i64);
            Ok(())
        }
    }
}
/// Сравниваем настройки таймера бонуса (для пересчёта срока).
fn bonus_differs(a: &crate::config::BonusConfig, b: &crate::config::BonusConfig) -> bool {
    a.enabled != b.enabled
        || a.target != b.target
        || a.command != b.command
        || a.interval_secs != b.interval_secs
        || a.jitter_secs != b.jitter_secs
}

/// Таймер «Бонуса»: спит до ближайшего срока и отправляет команду в чат.
fn spawn_bonus(shared: Arc<Shared>, kind: BonusKind) {
    tokio::spawn(async move {
        loop {
            let slot = shared.bonus_slot(kind);
            let Some(wait) = shared.next_bonus_in(kind) else {
                slot.wake.notified().await;
                continue;
            };
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = slot.wake.notified() => {}
            }

            let forced = slot.forced.swap(false, Ordering::SeqCst);
            let cfg = shared.cfg();
            if !kind.cfg(&cfg).enabled && !forced {
                continue;
            }
            if !forced {
                match shared.next_bonus_in(kind) {
                    Some(left) if left > Duration::from_millis(0) => continue,
                    _ => {}
                }
            }

            if let Err(e) = bonus_once(&shared, &cfg, kind).await {
                shared.log(format!("{} ошибка: {e}", kind.tag()));
                shared.reset_chat().await;
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        }
    });
}

/// Сколько ждём ответ бота на «Бонус» (там же и время до следующего).
const BONUS_REPLY_TIMEOUT: u64 = 60;
/// Сколько раз пробуем отправить «Бонус», если бот молчит.
const BONUS_ATTEMPTS: u32 = 3;
/// Пауза перед повторной отправкой (сообщение могло не дойти).
const BONUS_RETRY_GAP: u64 = 25;
/// Если бот так и не ответил — повторяем скоро, а не через полный интервал.
const BONUS_RETRY_SOON: u64 = 5 * 60;

/// Разбирает срок из ответа бота: «⏳ Следующий бонус через 12ч»,
/// «до получения бонуса 10 мин. 9 сек.», «1ч 30мин», «2 дня».
/// Возвращает секунды.
/// Достаёт срок из ответа бота — где угодно в тексте.
///
/// Ответы у бота разные: «⏳ Следующий бонус через 12ч.», «Роман, до получения
/// бонуса 6 мин. 29 сек. ⏳», «бонус будет доступен через 3 часа», у VIP-игроков
/// текст другой. Поэтому парсер не цепляется за конкретные слова и статус, а
/// просто собирает все пары «число + единица времени». Суммы денег, GB, стрики
/// и множители отбрасываются: у них нет единиц времени.
pub(crate) fn parse_bonus_delay(text: &str) -> Option<u64> {
    // Слишком маленькие значения — это не срок, а мусор вроде «1 раз».
    parse_duration(&text.to_lowercase()).filter(|secs| *secs >= 5)
}

fn parse_duration(text: &str) -> Option<u64> {
    let mut total = 0u64;
    let mut found = 0u32;
    let mut num = String::new();
    let mut word = String::new();

    for ch in text.chars() {
        if ch.is_ascii_digit() {
            if !word.is_empty() {
                if let Some(v) = accumulate(&mut num, &word) {
                    total += v;
                    found += 1;
                }
                word.clear();
            }
            // Длинные числа — это про деньги, а не про время.
            if num.len() < 9 {
                num.push(ch);
            }
        } else if ch.is_alphabetic() {
            word.push(ch);
        } else if !word.is_empty() {
            if let Some(v) = accumulate(&mut num, &word) {
                total += v;
                found += 1;
            }
            word.clear();
        }
    }
    if !word.is_empty() {
        if let Some(v) = accumulate(&mut num, &word) {
            total += v;
            found += 1;
        }
    }

    if found > 0 && total > 0 {
        Some(total)
    } else {
        None
    }
}

fn accumulate(num: &mut String, word: &str) -> Option<u64> {
    let raw = std::mem::take(num);
    let n: u64 = raw.parse().ok()?;
    unit_secs(word).map(|unit| n.saturating_mul(unit))
}

fn unit_secs(word: &str) -> Option<u64> {
    if word == "ч" || word.starts_with("час") {
        return Some(3_600);
    }
    if word.starts_with("мин") || word == "м" {
        return Some(60);
    }
    if word.starts_with("дн") || word.starts_with("ден") {
        return Some(86_400);
    }
    if word.starts_with("нед") {
        return Some(604_800);
    }
    if word.starts_with("сек") || word == "с" {
        return Some(1);
    }
    None
}

fn fmt_secs(secs: i64) -> String {
    let s = secs.max(0);
    if s >= 3_600 {
        format!("{}ч {}мин", s / 3_600, (s % 3_600) / 60)
    } else if s >= 60 {
        format!("{}мин", s / 60)
    } else {
        format!("{s}с")
    }
}

async fn bonus_once(shared: &Shared, cfg: &Config, kind: BonusKind) -> Result<(), BoxError> {
    let tag = kind.tag();
    let bonus = kind.cfg(cfg);
    let chat = shared.chat().await?;
    let peer = match bonus_peer(shared, &chat.client, &bonus.target, kind).await {
        Ok(peer) => peer,
        Err(e) => {
            // Название канала могло не найтись (или запрос сорвался) —
            // шлём в чат основного бота: для Bot Gorilla это тот же диалог.
            shared.log(format!("{tag} не нашёл «{}»: {e}", bonus.target));
            let alt = cfg.bot_username.trim().trim_start_matches('@');
            if alt.is_empty() || alt == bonus.target.trim().trim_start_matches('@') {
                return Err(e);
            }
            shared.log(format!("{tag} пробую чат бота @{alt}"));
            bonus_peer(shared, &chat.client, alt, kind).await?
        }
    };

    // Чат бота общий с казино и питомцем — там нужен peer_lock.
    // Канал живёт отдельно, ему хватает собственного лока слота.
    let mut none: Option<UpdateStream> = None;
    let cancel = AtomicBool::new(false);
    let mut after_id: Option<i32> = None;

    shared.log(format!(
        "{tag} → «{}» в «{}»",
        bonus.command, bonus.target
    ));
    for attempt in 1..=BONUS_ATTEMPTS {
        // Лок держим только на свою попытку: паузы между повторами не
        // должны блокировать казино и питомца в том же чате.
        let waited = {
            let _guard = if kind == BonusKind::Dm {
                // Чат бота общий с казино и питомцем — там нужен peer_lock.
                chat.peer_lock.lock().await
            } else {
                // Канал живёт отдельно, ему хватает лока слота.
                shared.bonus_slot(kind).lock.lock().await
            };

            let sent = chat
                .client
                .send_message(peer, bonus.command.as_str())
                .await?;
            shared.log(format!("{tag} отправлено (id {})", sent.id()));

            // Ответ на любую из попыток считаем ответом на команду.
            let after = *after_id.get_or_insert(sent.id());
            let deadline =
                tokio::time::Instant::now() + Duration::from_secs(BONUS_REPLY_TIMEOUT);
            wait_reply(
                &chat.client,
                &mut none,
                peer,
                peer.id,
                after,
                deadline,
                &cancel,
                false,
            )
            .await
        };

        // Интервал берём из ответа бота («⏳ Следующий бонус через 12ч»),
        // а из настроек — только разброс и запасное значение.
        let base = match waited {
            Ok(Wait::Reply(_, text)) => {
                let line = one_line(&text);
                shared.set_status(|s| s.last_bot_reply = line.clone());
                shared.log(format!("{tag} бот: {line}"));
                match parse_bonus_delay(&text) {
                    Some(secs) => {
                        shared.log(format!("{tag} бот назвал срок: {}", fmt_secs(secs as i64)));
                        secs as i64
                    }
                    None => {
                        shared.log(format!(
                            "{tag} в ответе нет срока — беру запасные {}",
                            fmt_secs(bonus.interval_secs as i64)
                        ));
                        bonus.interval_secs as i64
                    }
                }
            }
            Ok(Wait::Timeout) => {
                if attempt < BONUS_ATTEMPTS {
                    shared.log(format!(
                        "{tag} бот не ответил за {BONUS_REPLY_TIMEOUT}с — повтор {attempt}/{BONUS_ATTEMPTS}"
                    ));
                    tokio::time::sleep(Duration::from_secs(BONUS_RETRY_GAP)).await;
                    continue;
                }
                // Бот так и не отреагировал: «последнюю отправку» не двигаем,
                // а повторяем скоро — таймер не должен уходить на полный
                // интервал из-за потерянного сообщения.
                shared.log(format!(
                    "{tag} бот молчит после {BONUS_ATTEMPTS} попыток — повторю через {}",
                    fmt_secs(BONUS_RETRY_SOON as i64)
                ));
                shared.set_next_bonus(kind, now_unix() + BONUS_RETRY_SOON as i64);
                return Ok(());
            }
            Ok(Wait::Cancelled) => {
                shared.log(format!("{tag} отправка прервана — повторю позже"));
                shared.set_next_bonus(kind, now_unix() + BONUS_RETRY_SOON as i64);
                return Ok(());
            }
            Err(e) => {
                shared.log(format!("{tag} {e}"));
                shared.reset_chat().await;
                return Err(e);
            }
        };

        // Дальше — только если бот ответил: теперь таймер можно двигать.
        let now = now_unix();
        shared.set_last_bonus(kind, now);

        let jitter = (bonus.jitter_secs as i64).min((base / 4).max(1));
        let delta = if jitter > 0 {
            rand::rng().random_range(base - jitter..=base + jitter).max(60)
        } else {
            base.max(60)
        };
        shared.set_next_bonus(kind, now + delta);
        shared.log(format!(
            "{tag} следующая отправка через {} (±{} мин)",
            fmt_secs(delta),
            jitter / 60
        ));
        return Ok(());
    }

    Ok(())
}

/// Находит чат для бонуса и кэширует его, пока цель не поменялась в настройках.
async fn bonus_peer(
    shared: &Shared,
    client: &Client,
    target: &str,
    kind: BonusKind,
) -> Result<PeerRef, BoxError> {
    let mut guard = shared.bonus_slot(kind).peer.lock().await;
    if let Some((cached, peer)) = guard.as_ref() {
        if cached == target {
            return Ok(*peer);
        }
    }
    let peer = match resolve_chat(client, target).await {
        Ok(peer) => peer,
        Err(first) => {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            resolve_chat(client, target).await.map_err(|second| {
                BoxError::from(format!("{first}; повтор: {second}"))
            })?
        }
    };
    shared.log(format!("{} чат найден: «{target}» (id {})", kind.tag(), peer.id));
    *guard = Some((target.to_string(), peer));
    Ok(peer)
}

/// Отправляет команду питомцу и ждёт ответ. `None` — бот молчит.
async fn pet_send(
    shared: &Shared,
    chat: &Chat,
    cmd: &str,
    timeout_secs: u64,
) -> Result<Option<String>, BoxError> {
    // Лок на одну отправку: паузы между повторами не держат казино и бонус.
    let _guard = chat.peer_lock.lock().await;
    shared.log(format!("[питомец] → {cmd}"));
    let sent = chat.client.send_message(chat.bot_ref, cmd).await?;

    let mut none: Option<UpdateStream> = None;
    let cancel = AtomicBool::new(false);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    let waited = wait_reply(
        &chat.client,
        &mut none,
        chat.bot_ref,
        chat.bot_id,
        sent.id(),
        deadline,
        &cancel,
        false,
    )
    .await;

    match waited {
        Ok(Wait::Reply(_, text)) => {
            shared.set_status(|s| s.last_bot_reply = one_line(&text));
            shared.log(format!("[питомец] {}", one_line(&text)));
            Ok(Some(text))
        }
        Ok(Wait::Timeout) => {
            shared.log("[питомец] ответ не пришёл за отведённое время");
            Ok(None)
        }
        Ok(Wait::Cancelled) => Ok(None),
        Err(e) => {
            shared.log(format!("[питомец] {e}"));
            shared.reset_chat().await;
            Err(e)
        }
    }
}
/// Сколько раз повторяем команду питомцу, если бот молчит.
const PET_ATTEMPTS: u32 = 3;
/// Пауза перед повтором команды питомцу.
const PET_RETRY_GAP: u64 = 25;
/// Бот так и не ответил — повторяем скоро, а не через полный интервал.
const PET_RETRY_SOON: u64 = 5 * 60;

/// Шлёт команду питомцу, пока бот не ответит. `None` — так и не ответил.
async fn pet_send_confirmed(
    shared: &Shared,
    chat: &Chat,
    cmd: &str,
    timeout_secs: u64,
) -> Result<Option<String>, BoxError> {
    for attempt in 1..=PET_ATTEMPTS {
        if let Some(text) = pet_send(shared, chat, cmd, timeout_secs).await? {
            return Ok(Some(text));
        }
        if attempt < PET_ATTEMPTS {
            shared.log(format!(
                "[питомец] бот не ответил на «{cmd}» — повтор {attempt}/{PET_ATTEMPTS}"
            ));
            tokio::time::sleep(Duration::from_secs(PET_RETRY_GAP)).await;
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::{parse_bonus_delay, pet_prep_command};

    #[test]
    fn parses_any_bot_variant() {
        // VIP-ответ с готовым бонусом.
        assert_eq!(parse_bonus_delay("Nekiplay, бонус 26.000.000$ и 400 GB 🙂\n🔥 Стрик: 1/7\n⏳ Следующий бонус через 12ч."), Some(12 * 3600));
        // Кулдаун в канале.
        assert_eq!(parse_bonus_delay("Роман, до получения бонуса 6 мин. 29 сек. ⏳"), Some(389));
        assert_eq!(parse_bonus_delay("Роман, до получения бонуса 10 мин. 9 сек. ⏳"), Some(609));
        // Без слова «бонус» рядом.
        assert_eq!(parse_bonus_delay("Попробуйте через 3 часа 😉"), Some(3 * 3600));
        assert_eq!(parse_bonus_delay("Приходите через 2 дня"), Some(2 * 86_400));
        // Уже получен, без срока — таймер возьмёт запасной интервал.
        assert_eq!(parse_bonus_delay("Роман, VIP бонус +200 GB 💵"), None);
        assert_eq!(parse_bonus_delay("Nekiplay, бонус 26.000.000$ и 400 GB 🙂"), None);
        // Баланс и ставки не должны превращаться в срок.
        assert_eq!(parse_bonus_delay("Баланс: 2.046.868.646.673$"), None);
    }

    #[test]
    fn takes_prep_command_from_bot_reply() {
        let feed = "Nekiplay (http://t.me/nekiplay), покормите своего питомца чтобы отправить его в поход, команда: «питомец покормить» 🍗";
        let play = "Nekiplay, поиграйте со своим питомцем чтобы отправить его в поход, команда: «питомец поиграть» 💖";
        assert_eq!(pet_prep_command(feed).as_deref(), Some("питомец покормить"));
        assert_eq!(pet_prep_command(play).as_deref(), Some("питомец поиграть"));
        assert_eq!(
            pet_prep_command("Nekiplay, ваш питомец вернулся из похода, зачислено 124.331.091$ 🎁"),
            None
        );
    }
}
