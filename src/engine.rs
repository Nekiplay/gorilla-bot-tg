//! Движок автоигры: общее состояние, казино и «Питомец поход» по расписанию.
//!
//! GUI на egui только показывает состояние и шлёт команды через канал,
//! вся сеть и таймеры живут здесь, в отдельном tokio-рантайме.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use grammers_client::client::{LoginToken, PasswordToken, SignInError, UpdateStream};
use grammers_client::sender::SenderPoolFatHandle;
use grammers_client::session::types::{PeerId, PeerRef};
use grammers_client::session::updates::UpdatesLike;
use grammers_client::Client;
use rand::Rng as _;
use tokio::sync::{mpsc, Mutex as AsyncMutex, Notify};
use tokio::task::JoinHandle;

use crate::casino::{self, Martingale, Outcome};
use crate::captcha::{self, Captcha};
use crate::config::Config;
use crate::profile::{self, Profile};
use crate::race::{self, RaceReply};
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

/// Этап входа в Telegram. Вход живёт в GUI: консоль у релизной сборки
/// недоступна, поэтому код и пароль вводятся в окне.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum LoginStage {
    /// Ничего не происходит: показываем кнопку «Отправить код».
    #[default]
    Idle,
    /// Код уже отправлен, ждём его ввода.
    AwaitingCode,
    /// Аккаунт с 2FA: нужен пароль (в строке — подсказка от Telegram).
    AwaitingPassword(String),
    /// Вошли, в строке — имя аккаунта.
    Done(String),
    /// Не вышло, в строке — человеческая причина.
    Failed(String),
}

/// Открытое соединение на время входа: держим его, пока пользователь вводит
/// код, иначе `phone_code_hash` станет недействительным.
struct LoginConn {
    client: Client,
    conn: Conn,
    token: Option<LoginToken>,
    password: Option<PasswordToken>,
}

/// Что показать в окне (обновляет движок).
#[derive(Debug, Clone, Default)]
pub struct Status {
    pub connected: bool,
    /// Сессия в tdata авторизована (можно работать без входа).
    pub authorized: bool,
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
    /// Обслужить принтер прямо сейчас («Принтер» + заправить + снять).
    PrinterNow,
    /// Отправить «Принтер заправить» прямо сейчас.
    PrinterRefillNow,
    /// Отправить «Принтер снять» прямо сейчас.
    PrinterWithdrawNow,
    /// Отправить «Бонус» в настроенный чат прямо сейчас.
    BonusNow,
    /// Отправить «Бонус» в личку бота прямо сейчас.
    BonusDmNow,
    /// Отправить «Гонку» прямо сейчас.
    RaceNow,
    /// Снять доход с бизнеса прямо сейчас.
    BusinessNow,
    /// Запросить профиль игрока у бота.
    ProfileNow,
    /// Отправить код входа на телефон.
    LoginRequest { phone: String },
    /// Проверить введённый код из Telegram.
    LoginCode { code: String },
    /// Проверить пароль 2FA.
    LoginPassword { password: String },
    /// Прервать вход и закрыть соединение.
    LoginCancel,
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
    printer_wake: Notify,
    printer_forced: AtomicBool,
    printer_refill_forced: AtomicBool,
    printer_withdraw_forced: AtomicBool,
    last_printer: Mutex<Option<i64>>,
    next_printer: Mutex<Option<i64>>,
    /// Бонус в канал (t.me/gorillabot_double).
    bonus: BonusSlot,
    /// Бонус в личку бота (цикл ~12 часов).
    bonus_dm: BonusSlot,
    /// Автоматическая «Гонка» (заезд).
    race_wake: Notify,
    race_forced: AtomicBool,
    last_race: Mutex<Option<i64>>,
    next_race: Mutex<Option<i64>>,
    /// Автоснятие дохода с бизнеса.
    business_wake: Notify,
    business_forced: AtomicBool,
    last_business: Mutex<Option<i64>>,
    next_business: Mutex<Option<i64>>,
    /// Последний полученный профиль игрока.
    profile: Mutex<Option<Profile>>,
    last_profile: Mutex<Option<i64>>,
    /// Этап входа в Telegram.
    login: Mutex<LoginStage>,
    /// Соединение, открытое на время входа.
    login_conn: AsyncMutex<Option<LoginConn>>,
    /// Капча от бота, которую надо разметить в окне.
    captcha: Mutex<Option<Arc<Captcha>>>,
    /// Выбор плиток из окна: (id сообщения, отмеченные плитки).
    captcha_choice: Mutex<Option<(i32, Vec<bool>)>>,
    /// Что сейчас происходит с капчей (подсказка для окна).
    captcha_note: Mutex<String>,
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

#[derive(Debug, PartialEq, Eq)]
enum BonusReply {
    Claimed(Option<u64>),
    Cooldown(u64),
    Unconfirmed,
}

/// Accept reward only from a recognizable success response, never arbitrary channel posts.
fn classify_bonus_reply(text: &str) -> BonusReply {
    let lower = text.to_lowercase();
    let delay = parse_bonus_delay(&lower);
    let has_bonus = lower.contains("бонус");
    let has_reward = ["$", "₽", "руб", "gb", "💵", "💰", "зачислен", "начислен", "получен", "выдан"]
        .iter()
        .any(|marker| lower.contains(marker));
    let refusal = ["не удалось", "не получ", "уже получ", "до получения", "получи через", "попробуйте через", "приходите через", "недоступ", "ошибка", "не выполн"]
        .iter()
        .any(|marker| lower.contains(marker));

    if has_bonus && has_reward && !refusal {
        return BonusReply::Claimed(delay);
    }
    if has_bonus && refusal {
        if let Some(secs) = delay {
            return BonusReply::Cooldown(secs);
        }
    }
    BonusReply::Unconfirmed
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
    last_printer_unix: Option<i64>,
    #[serde(default)]
    next_printer_unix: Option<i64>,
    #[serde(default)]
    last_bonus_unix: Option<i64>,
    #[serde(default)]
    next_bonus_unix: Option<i64>,
    #[serde(default)]
    last_bonus_dm_unix: Option<i64>,
    #[serde(default)]
    next_bonus_dm_unix: Option<i64>,
    #[serde(default)]
    last_race_unix: Option<i64>,
    #[serde(default)]
    next_race_unix: Option<i64>,
    #[serde(default)]
    last_business_unix: Option<i64>,
    #[serde(default)]
    next_business_unix: Option<i64>,
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


/// Интервал до следующей проверки принтера с разбросом: interval_secs ± interval_jitter_secs.
fn printer_interval(cfg: &crate::config::PrinterConfig) -> i64 {
    let base = cfg.interval_secs as i64;
    let jitter = (cfg.interval_jitter_secs as i64).min(base.max(1));
    let secs = if jitter > 0 {
        rand::rng().random_range(base - jitter..=base + jitter)
    } else {
        base
    };
    secs.max(1)
}

/// Интервал до следующей гонки с разбросом: interval_secs ± interval_jitter_secs.
fn race_interval(cfg: &crate::config::RaceConfig) -> i64 {
    race::interval(cfg)
}

/// Интервал до следующего «Бизнес снять» с разбросом.
fn business_interval(cfg: &crate::config::BusinessConfig) -> i64 {
    let base = cfg.interval_secs as i64;
    let jitter = (cfg.interval_jitter_secs as i64).min(base.max(1));
    let secs = if jitter > 0 {
        rand::rng().random_range((base - jitter).max(1)..=(base + jitter))
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
            printer_wake: Notify::new(),
            printer_forced: AtomicBool::new(false),
            printer_refill_forced: AtomicBool::new(false),
            printer_withdraw_forced: AtomicBool::new(false),
            last_printer: Mutex::new(state.last_printer_unix),
            next_printer: Mutex::new(state.next_printer_unix),
            bonus: BonusSlot::new(state.last_bonus_unix, state.next_bonus_unix),
            bonus_dm: BonusSlot::new(state.last_bonus_dm_unix, state.next_bonus_dm_unix),
            race_wake: Notify::new(),
            race_forced: AtomicBool::new(false),
            last_race: Mutex::new(state.last_race_unix),
            next_race: Mutex::new(state.next_race_unix),
            business_wake: Notify::new(),
            business_forced: AtomicBool::new(false),
            last_business: Mutex::new(state.last_business_unix),
            next_business: Mutex::new(state.next_business_unix),
            profile: Mutex::new(None),
            last_profile: Mutex::new(None),
            login: Mutex::new(LoginStage::Idle),
            login_conn: AsyncMutex::new(None),
            captcha: Mutex::new(None),
            captcha_choice: Mutex::new(None),
            captcha_note: Mutex::new(String::new()),
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

    /// Капча, которую надо пройти прямо сейчас.
    pub fn captcha(&self) -> Option<Arc<Captcha>> {
        self.captcha.lock().unwrap().clone()
    }

    pub fn set_captcha(&self, captcha: Option<Arc<Captcha>>) {
        *self.captcha.lock().unwrap() = captcha;
        *self.captcha_choice.lock().unwrap() = None;
    }

    /// Подсказка/статус капчи для окна.
    pub fn captcha_note(&self) -> String {
        self.captcha_note.lock().unwrap().clone()
    }

    pub fn set_captcha_note(&self, note: impl Into<String>) {
        *self.captcha_note.lock().unwrap() = note.into();
    }

    /// Забирает выбор пользователя из окна (одноразово).
    pub fn take_captcha_choice(&self) -> Option<(i32, Vec<bool>)> {
        self.captcha_choice.lock().unwrap().take()
    }

    /// Выбор плиток пользователем: кнопки нажмёт движок.
    pub fn submit_captcha(&self, message_id: i32, tiles: Vec<bool>) {
        *self.captcha_choice.lock().unwrap() = Some((message_id, tiles));
    }

    /// Текущий этап входа в Telegram.
    pub fn login_stage(&self) -> LoginStage {
        self.login.lock().unwrap().clone()
    }

    fn set_login(&self, stage: LoginStage) {
        {
            let mut guard = self.login.lock().unwrap();
            *guard = stage.clone();
        }
        match stage {
            LoginStage::AwaitingCode => self.log("[вход] код отправлен, введи его в окне"),
            LoginStage::AwaitingPassword(_) => self.log("[вход] нужен пароль 2FA"),
            LoginStage::Done(_) => {
                self.set_status(|s| s.authorized = true);
            }
            _ => {}
        }
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
            last_printer_unix: *self.last_printer.lock().unwrap(),
            next_printer_unix: *self.next_printer.lock().unwrap(),
            last_bonus_unix: *self.bonus.last.lock().unwrap(),
            next_bonus_unix: *self.bonus.next.lock().unwrap(),
            last_bonus_dm_unix: *self.bonus_dm.last.lock().unwrap(),
            next_bonus_dm_unix: *self.bonus_dm.next.lock().unwrap(),
            last_race_unix: *self.last_race.lock().unwrap(),
            next_race_unix: *self.next_race.lock().unwrap(),
            last_business_unix: *self.last_business.lock().unwrap(),
            next_business_unix: *self.next_business.lock().unwrap(),
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

    pub fn last_printer(&self) -> Option<i64> {
        *self.last_printer.lock().unwrap()
    }

    fn set_last_printer(&self, ts: i64) {
        let cfg = self.cfg();
        let next = ts + printer_interval(&cfg.printer);
        *self.last_printer.lock().unwrap() = Some(ts);
        *self.next_printer.lock().unwrap() = Some(next);
        self.persist();
    }

    fn set_next_printer(&self, ts: i64) {
        *self.next_printer.lock().unwrap() = Some(ts);
        self.persist();
    }

    pub fn next_printer_at(&self) -> Option<i64> {
        *self.next_printer.lock().unwrap()
    }

    fn reschedule_printer(&self) {
        let Some(last) = self.last_printer() else {
            return;
        };
        let cfg = self.cfg();
        *self.next_printer.lock().unwrap() = Some(last + printer_interval(&cfg.printer));
    }

    /// Через сколько проверять принтер. `None` — авто-принтер выключен.
    pub fn next_printer_in(&self) -> Option<Duration> {
        let cfg = self.cfg();
        if !cfg.printer.enabled {
            return None;
        }
        let due = match self.next_printer_at() {
            Some(ts) => ts,
            None => match self.last_printer() {
                Some(ts) => ts + cfg.printer.interval_secs as i64,
                None => now_unix(),
            },
        };
        Some(Duration::from_secs((due - now_unix()).max(0) as u64))
    }

    pub fn last_race(&self) -> Option<i64> {
        *self.last_race.lock().unwrap()
    }

    fn set_last_race(&self, ts: i64) {
        let cfg = self.cfg();
        let next = ts + race_interval(&cfg.race);
        *self.last_race.lock().unwrap() = Some(ts);
        *self.next_race.lock().unwrap() = Some(next);
        self.persist();
    }

    fn set_next_race(&self, ts: i64) {
        *self.next_race.lock().unwrap() = Some(ts);
        self.persist();
    }

    pub fn next_race_at(&self) -> Option<i64> {
        *self.next_race.lock().unwrap()
    }

    fn reschedule_race(&self) {
        let Some(last) = self.last_race() else {
            return;
        };
        let cfg = self.cfg();
        *self.next_race.lock().unwrap() = Some(last + race_interval(&cfg.race));
    }

    /// Через сколько ехать на гонку. `None` — авто-гонка выключена.
    pub fn next_race_in(&self) -> Option<Duration> {
        let cfg = self.cfg();
        if !cfg.race.enabled {
            return None;
        }
        let due = match self.next_race_at() {
            Some(ts) => ts,
            None => match self.last_race() {
                Some(ts) => ts + cfg.race.interval_secs as i64,
                None => now_unix(),
            },
        };
        Some(Duration::from_secs((due - now_unix()).max(0) as u64))
    }
    pub fn last_business(&self) -> Option<i64> {
        *self.last_business.lock().unwrap()
    }

    fn set_last_business(&self, ts: i64) {
        let cfg = self.cfg();
        let next = ts + business_interval(&cfg.business);
        *self.last_business.lock().unwrap() = Some(ts);
        *self.next_business.lock().unwrap() = Some(next);
        self.persist();
    }

    fn set_next_business(&self, ts: i64) {
        *self.next_business.lock().unwrap() = Some(ts);
        self.persist();
    }

    pub fn next_business_at(&self) -> Option<i64> {
        *self.next_business.lock().unwrap()
    }

    fn reschedule_business(&self) {
        let Some(last) = self.last_business() else {
            return;
        };
        let cfg = self.cfg();
        *self.next_business.lock().unwrap() = Some(last + business_interval(&cfg.business));
    }

    /// Через сколько снимать бизнес. `None` — автоснятие выключено.
    pub fn next_business_in(&self) -> Option<Duration> {
        let cfg = self.cfg();
        if !cfg.business.enabled {
            return None;
        }
        let due = match self.next_business_at() {
            Some(ts) => ts,
            None => match self.last_business() {
                Some(ts) => ts + cfg.business.interval_secs as i64,
                None => now_unix(),
            },
        };
        Some(Duration::from_secs((due - now_unix()).max(0) as u64))
    }

    /// Последний профиль, полученный от бота.
    pub fn profile(&self) -> Option<Profile> {
        self.profile.lock().unwrap().clone()
    }

    /// Когда профиль обновляли в последний раз.
    pub fn last_profile(&self) -> Option<i64> {
        *self.last_profile.lock().unwrap()
    }

    fn set_profile(&self, profile: Profile) {
        *self.profile.lock().unwrap() = Some(profile);
        *self.last_profile.lock().unwrap() = Some(now_unix());
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
            s.authorized = true;
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

/// Шаг 1: открыть соединение и попросить код на телефон.
async fn login_request(shared: &Arc<Shared>, phone: String) {
    let phone = phone.trim().to_string();
    if phone.is_empty() || phone == "+" {
        shared.set_login(LoginStage::Failed(
            "Укажи телефон в формате +79991234567".into(),
        ));
        return;
    }
    // Прошлое соединение входа больше не нужно: `phone_code_hash` устарел.
    login_abort(shared, "").await;

    let cfg = shared.cfg();
    shared.log(format!("[вход] отправляю код на {phone}"));
    let conn = match crate::connect(&cfg, true).await {
        Ok(conn) => conn,
        Err(e) => {
            shared.log(format!("[вход] не удалось подключиться: {e}"));
            shared.set_login(LoginStage::Failed(format!("{e}")));
            return;
        }
    };

    let client = conn.0.clone();
    match client.request_login_code(&phone, &cfg.api_hash).await {
        Ok(token) => {
            *shared.login_conn.lock().await = Some(LoginConn {
                client,
                conn,
                token: Some(token),
                password: None,
            });
            shared.set_login(LoginStage::AwaitingCode);
        }
        Err(e) => {
            conn.2.quit();
            let text = format!("не удалось отправить код на {phone}: {e}");
            shared.log(format!("[вход] {text}"));
            shared.set_login(LoginStage::Failed(text));
        }
    }
}

/// Шаг 2: проверить код из Telegram (или перейти к паролю 2FA).
async fn login_code(shared: &Arc<Shared>, code: String) {
    let code = code.trim().to_string();
    let mut guard = shared.login_conn.lock().await;
    let Some(entry) = guard.as_mut() else {
        drop(guard);
        shared.set_login(LoginStage::Failed(
            "Соединение входа закрыто — отправь код заново".into(),
        ));
        return;
    };
    let Some(token) = entry.token.take() else {
        drop(guard);
        shared.set_login(LoginStage::Failed("Отправь код заново".into()));
        return;
    };

    match entry.client.sign_in(&token, &code).await {
        Ok(_) => {
            let entry = guard.take().expect("соединение входа на месте");
            drop(guard);
            login_success(shared, entry).await;
        }
        Err(SignInError::PasswordRequired(password)) => {
            entry.password = Some(password);
            drop(guard);
            shared.log("[вход] Telegram просит пароль 2FA");
            shared.set_login(LoginStage::AwaitingPassword(String::new()));
        }
        Err(e) => {
            entry.token = Some(token);
            drop(guard);
            shared.log(format!("[вход] код не принят: {e}"));
            shared.set_login(LoginStage::Failed(format!("Код не принят: {e}")));
        }
    }
}

/// Шаг 3: пароль двухфакторной авторизации.
async fn login_password(shared: &Arc<Shared>, password: String) {
    let mut guard = shared.login_conn.lock().await;
    let Some(entry) = guard.as_mut() else {
        drop(guard);
        shared.set_login(LoginStage::Failed(
            "Соединение входа закрыто — начни заново".into(),
        ));
        return;
    };
    let Some(token) = entry.password.take() else {
        drop(guard);
        shared.set_login(LoginStage::Failed("Пароль уже проверялся".into()));
        return;
    };

    match entry.client.check_password(token, password.trim()).await {
        Ok(_) => {
            let entry = guard.take().expect("соединение входа на месте");
            drop(guard);
            login_success(shared, entry).await;
        }
        Err(e) => {
            drop(guard);
            shared.log(format!("[вход] пароль не принят: {e}"));
            shared.set_login(LoginStage::Failed(format!("Пароль не принят: {e}")));
        }
    }
}

/// Вход завершён: узнать имя, закрыть временное соединение, отметить сессию.
async fn login_success(shared: &Arc<Shared>, entry: LoginConn) {
    let name = match entry.client.get_me().await {
        Ok(me) => me.first_name().unwrap_or("аккаунт").to_string(),
        Err(_) => "аккаунт".to_string(),
    };
    entry.conn.2.quit();
    let _ = entry.conn.3.await;
    shared.set_status(|s| {
        s.authorized = true;
        s.connected = false;
        s.message = "вход выполнен".into();
    });
    shared.set_login(LoginStage::Done(format!("Вошли как {name}")));
    shared.log(format!("[вход] готово, {name} — сессия сохранена"));
}

/// Закрыть соединение входа. С непустой причиной — ещё и сбросить этап.
async fn login_abort(shared: &Arc<Shared>, reason: &str) {
    let entry = shared.login_conn.lock().await.take();
    if let Some(entry) = entry {
        entry.conn.2.quit();
    }
    if !reason.is_empty() {
        shared.log(format!("[вход] {reason}"));
        shared.set_login(LoginStage::Idle);
    }
}

/// Состояние движка для обработчика капчи (его зовёт `wait_reply`).
static CAPTCHA_SHARED: OnceLock<Arc<Shared>> = OnceLock::new();

/// Сколько ждём разметки капчи в окне.
const CAPTCHA_WAIT_SECS: u64 = 600;

/// Бот прислал капчу: `true` — её надо разметить, а ответ ждать заново.
pub(crate) async fn solve_captcha(
    client: &Client,
    bot_ref: PeerRef,
    bot_id: PeerId,
    after_id: i32,
) -> bool {
    let Some(shared) = CAPTCHA_SHARED.get().cloned() else {
        eprintln!("[капча] пришла капча, а окна нет — размечать негде");
        return true;
    };
    match captcha_gate(&shared, client, bot_ref, bot_id, after_id).await {
        Ok(()) => true,
        Err(e) => {
            shared.log(format!("[капча] {e}"));
            shared.set_captcha(None);
            shared.set_captcha_note(format!("не получилось: {e}"));
            true
        }
    }
}

/// Сколько ждём подтверждения «проверка пройдена» после нажатия «Готово».
const CAPTCHA_VERDICT_SECS: u64 = 30;

/// Вердикт бота по капче: `Some(true)` — пройдена, `Some(false)` — не принял,
/// `None` — ничего не ответил.
async fn captcha_verdict(
    client: &Client,
    bot_ref: PeerRef,
    bot_id: PeerId,
    after_id: i32,
) -> Option<bool> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(CAPTCHA_VERDICT_SECS);
    loop {
        if let Some((_, text)) = crate::latest_reply(client, bot_ref, after_id, bot_id)
            .await
            .ok()
            .flatten()
        {
            let lower = text.to_lowercase();
            if lower.contains("проверка пройдена") || lower.contains("капча пройдена") {
                return Some(true);
            }
            if lower.contains("не пройдена")
                || lower.contains("заблокирован")
                || lower.contains("неверн")
            {
                return Some(false);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Скачивает картинку капчи, ждёт разметки из окна, нажимает кнопки
/// и дообучает модель на размеченных плитках.
async fn captcha_gate(
    shared: &Arc<Shared>,
    client: &Client,
    bot_ref: PeerRef,
    bot_id: PeerId,
    after_id: i32,
) -> Result<(), BoxError> {
    let Some(msg) = crate::latest_message(client, bot_ref, after_id, bot_id).await? else {
        return Err("сообщение с капчей не нашлось".into());
    };
    let buttons = crate::message_buttons(&msg);
    if buttons.is_empty() {
        return Err("у капчи нет inline-кнопок".into());
    }

    let dir = captcha::model_dir();
    std::fs::create_dir_all(&dir)?;
    let image_path = dir.join("last-captcha.img");
    msg.download_media(&image_path).await?;
    let bytes = std::fs::read(&image_path)?;

    let parsed = captcha::parse(msg.id(), msg.text().to_string(), &bytes, buttons)
        .map_err(|e| -> BoxError { e.into() })?;
    let message_id = parsed.message_id;
    let tiles = parsed.tiles.len();
    let hint = if parsed.has_guess() {
        parsed
            .suggested()
            .iter()
            .enumerate()
            .filter(|(_, on)| **on)
            .map(|(i, _)| (i + 1).to_string())
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        String::new()
    };
    shared.log(format!(
        "[капча] {tiles} плиток — разметь во вкладке «Капча»{}",
        if hint.is_empty() {
            String::new()
        } else {
            format!(" (модель думает: {hint})")
        }
    ));
    shared.set_captcha_note("отметь нужные плитки и нажми «Отправить»");
    shared.set_captcha(Some(Arc::new(parsed)));

    let deadline = tokio::time::Instant::now() + Duration::from_secs(CAPTCHA_WAIT_SECS);
    let mut choice: Option<Vec<bool>> = None;
    while tokio::time::Instant::now() < deadline {
        if let Some((id, tiles)) = shared.take_captcha_choice() {
            if id == message_id {
                choice = Some(tiles);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    let Some(captcha) = shared.captcha() else {
        return Err("капча исчезла".into());
    };
    let Some(chosen) = choice else {
        shared.set_captcha(None);
        return Err(format!("не размечена за {CAPTCHA_WAIT_SECS}с").into());
    };

    for button in &captcha.buttons {
        let Some(index) = button.tile() else { continue };
        if !chosen.get(index - 1).copied().unwrap_or(false) {
            continue;
        }
        crate::click_button(client, bot_ref, message_id, &button.data).await?;
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    if let Some(done) = captcha.buttons.iter().find(|b| b.is_done()) {
        crate::click_button(client, bot_ref, message_id, &done.data).await?;
    }
    shared.log("[капча] выбор отправлен, ждём подтверждения");
    let verdict = captcha_verdict(client, bot_ref, bot_id, message_id).await;
    match verdict {
        Some(true) => shared.log("[капча] проверка пройдена"),
        Some(false) => shared.log("[капча] бот не принял ответ — капча не пройдена"),
        None => shared.log("[капча] подтверждения нет: бот молчит"),
    }
    shared.set_captcha(None);

    // Дообучение: каждая плитка — один пример «нужна/не нужна».
    let mut added = 0usize;
    for (index, tile) in captcha.tiles.iter().enumerate() {
        let needed = chosen.get(index).copied().unwrap_or(false);
        if captcha::remember(&dir, &captcha::features(tile, captcha.tile_w, captcha.tile_h), needed)
            .is_ok()
        {
            added += 1;
        }
    }
    let train_dir = dir.clone();
    let trained = tokio::task::spawn_blocking(move || captcha::train(&train_dir))
        .await
        .ok()
        .flatten();
    let note = match trained {
        Some(model) => format!("модель обучена на {} плитках", model.samples),
        None => format!("добавлено {added} плиток — для модели пока мало"),
    };
    shared.log(format!("[капча] {note}"));
    shared.set_captcha_note(note);
    Ok(())
}

/// Запуск движка в отдельном потоке со своим tokio-рантаймом.
pub fn spawn(shared: Arc<Shared>, mut rx: mpsc::UnboundedReceiver<Command>) {
    // Обработчик капчи живёт в wait_reply, а состояние — здесь.
    let _ = CAPTCHA_SHARED.set(shared.clone());
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("не удалось создать tokio runtime для движка");
        rt.block_on(async move {
            spawn_pet(shared.clone());
            spawn_printer(shared.clone());
            spawn_bonus(shared.clone(), BonusKind::Channel);
            spawn_bonus(shared.clone(), BonusKind::Dm);
            spawn_race(shared.clone());
            spawn_business(shared.clone());

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
                    Command::PrinterNow => {
                        shared.printer_forced.store(true, Ordering::SeqCst);
                        shared.printer_wake.notify_waiters();
                    }
                    Command::PrinterRefillNow => {
                        shared.printer_refill_forced.store(true, Ordering::SeqCst);
                        shared.printer_wake.notify_waiters();
                    }
                    Command::PrinterWithdrawNow => {
                        shared.printer_withdraw_forced.store(true, Ordering::SeqCst);
                        shared.printer_wake.notify_waiters();
                    }
                    Command::BonusNow => shared.trigger_bonus(BonusKind::Channel),
                    Command::BonusDmNow => shared.trigger_bonus(BonusKind::Dm),
                    Command::RaceNow => {
                        shared.race_forced.store(true, Ordering::SeqCst);
                        shared.race_wake.notify_waiters();
                    }
                    Command::BusinessNow => {
                        shared.business_forced.store(true, Ordering::SeqCst);
                        shared.business_wake.notify_waiters();
                    }
                    Command::ProfileNow => {
                        let shared = shared.clone();
                        tokio::spawn(async move {
                            let cfg = shared.cfg();
                            if let Err(e) = profile_once(&shared, &cfg).await {
                                shared.log(format!("[профиль] ошибка: {e}"));
                            }
                        });
                    }
                    Command::LoginRequest { phone } => {
                        let shared = shared.clone();
                        tokio::spawn(async move {
                            login_request(&shared, phone).await;
                        });
                    }
                    Command::LoginCode { code } => {
                        let shared = shared.clone();
                        tokio::spawn(async move {
                            login_code(&shared, code).await;
                        });
                    }
                    Command::LoginPassword { password } => {
                        let shared = shared.clone();
                        tokio::spawn(async move {
                            login_password(&shared, password).await;
                        });
                    }
                    Command::LoginCancel => {
                        let shared = shared.clone();
                        tokio::spawn(async move {
                            login_abort(&shared, "вход отменён").await;
                        });
                    }
                    Command::SetConfig(cfg) => {
                        let pet_changed = {
                            let old = shared.cfg.lock().unwrap();
                            old.pet.enabled != cfg.pet.enabled
                                || old.pet.interval_secs != cfg.pet.interval_secs
                                || old.pet.interval_jitter_secs != cfg.pet.interval_jitter_secs
                        };
                        let printer_changed = {
                            let old = shared.cfg.lock().unwrap();
                            old.printer.enabled != cfg.printer.enabled
                                || old.printer.interval_secs != cfg.printer.interval_secs
                                || old.printer.interval_jitter_secs != cfg.printer.interval_jitter_secs
                        };
                        let bonus_changed = {
                            let old = shared.cfg.lock().unwrap();
                            bonus_differs(&old.bonus, &cfg.bonus)
                                || bonus_differs(&old.bonus_dm, &cfg.bonus_dm)
                        };
                        let race_changed = {
                            let old = shared.cfg.lock().unwrap();
                            old.race.enabled != cfg.race.enabled
                                || old.race.interval_secs != cfg.race.interval_secs
                                || old.race.interval_jitter_secs != cfg.race.interval_jitter_secs
                        };
                        let business_changed = {
                            let old = shared.cfg.lock().unwrap();
                            old.business.enabled != cfg.business.enabled
                                || old.business.interval_secs != cfg.business.interval_secs
                                || old.business.interval_jitter_secs
                                    != cfg.business.interval_jitter_secs
                        };
                        shared.set_cfg(cfg);
                        if business_changed {
                            shared.reschedule_business();
                            shared.business_wake.notify_waiters();
                        }
                        if race_changed {
                            shared.reschedule_race();
                            shared.race_wake.notify_waiters();
                        }
                        if pet_changed {
                            shared.reschedule_pet();
                            shared.pet_wake.notify_waiters();
                        }
                        if printer_changed {
                            shared.reschedule_printer();
                            shared.printer_wake.notify_waiters();
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

        // Сколько раз подряд переотправляли ставку, пока бот молчал.
        let mut resends = 0u32;

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
            // Переотправка — это та же ставка: раунд считаем один раз.
            if resends == 0 {
                mg.note_round();
            }
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
                    // Ответ разобран — ставка больше не «висит». Без сброса
                    // проверка в начале цикла нашла бы этот же ответ ещё раз
                    // и засчитала победу дважды.
                    pending = None;
                    fails = 0;
                    resends = 0;
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
                    // Бот промолчал во все окна: ставку считаем потерянной и
                    // отправляем её заново, а не останавливаем казино.
                    pending = None;
                    resends += 1;
                    if resends > MAX_RESENDS {
                        stop_casino(&shared, "бот не отвечает на ставки");
                        break 'outer;
                    }
                    shared.log(format!(
                        "[казино] бот не ответил — отправляю ставку заново ({resends}/{MAX_RESENDS})"
                    ));
                    continue;
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

/// Сколько раз переотправлять ставку без ответа, прежде чем сдаться.
const MAX_RESENDS: u32 = 5;

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

pub(crate) fn parse_duration(text: &str) -> Option<u64> {
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
            // Ignore already-consumed replies on later retry attempts.
            let after = after_id.unwrap_or(sent.id());
            let deadline =
                tokio::time::Instant::now() + Duration::from_secs(BONUS_REPLY_TIMEOUT);
            wait_reply(
                &chat.client,
                &mut none,
                peer,
                chat.bot_id,
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
            Ok(Wait::Reply(reply_id, text)) => {
                after_id = Some(reply_id);
                let line = one_line(&text);
                shared.set_status(|s| s.last_bot_reply = line.clone());
                shared.log(format!("{tag} бот: {line}"));
                match classify_bonus_reply(&text) {
                    BonusReply::Claimed(Some(secs)) => {
                        shared.log(format!("{tag} награда подтверждена, бот назвал срок: {}", fmt_secs(secs as i64)));
                        secs as i64
                    }
                    BonusReply::Claimed(None) => {
                        shared.log(format!("{tag} награда подтверждена, беру интервал {}", fmt_secs(bonus.interval_secs as i64)));
                        bonus.interval_secs as i64
                    }
                    BonusReply::Cooldown(secs) => {
                        shared.log(format!("{tag} награда не выдана — повтор после кулдауна {}", fmt_secs(secs as i64)));
                        shared.set_next_bonus(kind, now_unix() + secs.max(60) as i64);
                        return Ok(());
                    }
                    BonusReply::Unconfirmed => {
                        if attempt < BONUS_ATTEMPTS {
                            shared.log(format!("{tag} ответ не подтверждает выдачу бонуса — повтор {attempt}/{BONUS_ATTEMPTS}"));
                            tokio::time::sleep(Duration::from_secs(BONUS_RETRY_GAP)).await;
                            continue;
                        }
                        shared.log(format!("{tag} выдача бонуса не подтверждена после {BONUS_ATTEMPTS} попыток — повторю через {}", fmt_secs(BONUS_RETRY_SOON as i64)));
                        shared.set_next_bonus(kind, now_unix() + BONUS_RETRY_SOON as i64);
                        return Ok(());
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

/// Разбор статуса принтера из ответа бота:
/// * `cartridge_pct` — процент оставшегося картриджа («🎨 Осталось картриджа: 63%»);
/// * `can_withdraw` — сумма «🏧 Можно снять: 166.273.388.889$» (в долларах).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct PrinterInfo {
    pub cartridge_pct: Option<u8>,
    pub can_withdraw: Option<u64>,
}

pub(crate) fn parse_printer_info(reply: &str) -> PrinterInfo {
    let mut info = PrinterInfo::default();
    for line in reply.lines() {
        let lower = line.to_lowercase();
        if lower.contains("картридж") {
            if let Some(pct) = parse_percent_in_line(&lower) {
                info.cartridge_pct = Some(pct);
            }
        }
        if lower.contains("можно снять") {
            if let Some(amount) = parse_money_after_colon(&lower) {
                info.can_withdraw = Some(amount);
            }
        }
    }
    info
}

fn parse_percent_in_line(line: &str) -> Option<u8> {
    let pct_pos = line.find('%')?;
    let before = &line[..pct_pos];
    let digits: String = before
        .chars()
        .rev()
        .skip_while(|c| c.is_whitespace())
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    digits.parse::<u8>().ok().map(|v| v.min(100))
}

fn parse_money_after_colon(line: &str) -> Option<u64> {
    let tail = match line.find(':') {
        Some(pos) => &line[pos + 1..],
        None => line,
    };
    let digits: String = tail.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<u64>().ok()
}

/// Таймер принтера: спит до ближайшего срока и обслуживает принтер.
fn spawn_printer(shared: Arc<Shared>) {
    tokio::spawn(async move {
        loop {
            let forced_any = shared.printer_forced.load(Ordering::SeqCst)
                || shared.printer_refill_forced.load(Ordering::SeqCst)
                || shared.printer_withdraw_forced.load(Ordering::SeqCst);

            if !forced_any {
                let Some(wait) = shared.next_printer_in() else {
                    shared.printer_wake.notified().await;
                    continue;
                };
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = shared.printer_wake.notified() => {}
                }
            }

            let forced = shared.printer_forced.swap(false, Ordering::SeqCst);
            let refill_only = shared.printer_refill_forced.swap(false, Ordering::SeqCst);
            let withdraw_only = shared.printer_withdraw_forced.swap(false, Ordering::SeqCst);

            let cfg = shared.cfg();
            if refill_only || withdraw_only {
                if let Err(e) = printer_direct_actions(&shared, &cfg, refill_only, withdraw_only).await {
                    shared.log(format!("[принтер] ошибка: {e}"));
                    shared.reset_chat().await;
                }
                if !forced {
                    continue;
                }
            }

            if !cfg.printer.enabled && !forced {
                continue;
            }
            if !forced {
                match shared.next_printer_in() {
                    Some(left) if left > Duration::from_millis(0) => continue,
                    _ => {}
                }
            }

            if let Err(e) = printer_once(&shared, &cfg).await {
                shared.log(format!("[принтер] ошибка: {e}"));
                shared.reset_chat().await;
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        }
    });
}

async fn printer_direct_actions(
    shared: &Shared,
    cfg: &Config,
    do_refill: bool,
    do_withdraw: bool,
) -> Result<(), BoxError> {
    let chat = shared.chat().await?;
    if do_refill && !cfg.printer.refill_command.trim().is_empty() {
        printer_send_confirmed(
            shared,
            &chat,
            cfg.printer.refill_command.trim(),
            cfg.printer.reply_timeout_secs,
        )
        .await?;
    }
    if do_withdraw && !cfg.printer.withdraw_command.trim().is_empty() {
        if do_refill {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        printer_send_confirmed(
            shared,
            &chat,
            cfg.printer.withdraw_command.trim(),
            cfg.printer.reply_timeout_secs,
        )
        .await?;
    }
    Ok(())
}

pub(crate) async fn printer_once(shared: &Shared, cfg: &Config) -> Result<(), BoxError> {
    let chat = shared.chat().await?;
    let reply = printer_send_confirmed(
        shared,
        &chat,
        cfg.printer.command.trim(),
        cfg.printer.reply_timeout_secs,
    )
    .await?;

    let Some(text) = reply else {
        shared.log(format!(
            "[принтер] бот молчит после {PET_ATTEMPTS} попыток — повторю через {}",
            fmt_secs(PET_RETRY_SOON as i64)
        ));
        shared.set_next_printer(now_unix() + PET_RETRY_SOON as i64);
        return Ok(());
    };

    let info = parse_printer_info(&text);
    let lower = text.to_lowercase();
    let broken_or_jammed = lower.contains("слом") || lower.contains("заклин");

    let need_refill = cfg.printer.auto_refill
        && !cfg.printer.refill_command.trim().is_empty()
        && match info.cartridge_pct {
            Some(pct) => pct <= cfg.printer.refill_below_percent.min(100) && pct < 100,
            None => broken_or_jammed,
        };

    if need_refill {
        if let Some(pct) = info.cartridge_pct {
            shared.log(format!(
                "[принтер] картридж {pct}% (порог {}%) → {}",
                cfg.printer.refill_below_percent, cfg.printer.refill_command
            ));
        } else {
            shared.log(format!("[принтер] заправка → {}", cfg.printer.refill_command));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        printer_send_confirmed(
            shared,
            &chat,
            cfg.printer.refill_command.trim(),
            cfg.printer.reply_timeout_secs,
        )
        .await?;
    }

    let need_withdraw = cfg.printer.auto_withdraw
        && !cfg.printer.withdraw_command.trim().is_empty()
        && info.can_withdraw.map_or(false, |amt| amt > 0);

    if need_withdraw {
        if let Some(amt) = info.can_withdraw {
            shared.log(format!(
                "[принтер] можно снять {amt}$ → {}",
                cfg.printer.withdraw_command
            ));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        printer_send_confirmed(
            shared,
            &chat,
            cfg.printer.withdraw_command.trim(),
            cfg.printer.reply_timeout_secs,
        )
        .await?;
    }

    shared.set_last_printer(now_unix());
    Ok(())
}

async fn printer_send(
    shared: &Shared,
    chat: &Chat,
    cmd: &str,
    timeout_secs: u64,
) -> Result<Option<String>, BoxError> {
    let _guard = chat.peer_lock.lock().await;
    shared.log(format!("[принтер] → {cmd}"));
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
            shared.log(format!("[принтер] {}", one_line(&text)));
            Ok(Some(text))
        }
        Ok(Wait::Timeout) => {
            shared.log("[принтер] ответ не пришёл за отведённое время");
            Ok(None)
        }
        Ok(Wait::Cancelled) => Ok(None),
        Err(e) => {
            shared.log(format!("[принтер] {e}"));
            shared.reset_chat().await;
            Err(e)
        }
    }
}

async fn printer_send_confirmed(
    shared: &Shared,
    chat: &Chat,
    cmd: &str,
    timeout_secs: u64,
) -> Result<Option<String>, BoxError> {
    for attempt in 1..=PET_ATTEMPTS {
        if let Some(text) = printer_send(shared, chat, cmd, timeout_secs).await? {
            return Ok(Some(text));
        }
        if attempt < PET_ATTEMPTS {
            shared.log(format!(
                "[принтер] бот не ответил на «{cmd}» — повтор {attempt}/{PET_ATTEMPTS}"
            ));
            tokio::time::sleep(Duration::from_secs(PET_RETRY_GAP)).await;
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::{classify_bonus_reply, parse_bonus_delay, parse_printer_info, pet_prep_command, BonusReply, PrinterInfo};

    #[test]
    fn classifies_bonus_replies() {
        assert_eq!(
            classify_bonus_reply("Nekiplay, бонус 26.000.000$ и 400 GB 🙂"),
            BonusReply::Claimed(None)
        );
        assert_eq!(
            classify_bonus_reply("Роман, до получения бонуса 6 мин. 29 сек. ⏳"),
            BonusReply::Cooldown(389)
        );
        assert_eq!(
            classify_bonus_reply("Бонус временно недоступен"),
            BonusReply::Unconfirmed
        );
        assert_eq!(
            classify_bonus_reply("Роман (https://t.me/nekiplay), VIP бонус +200 GB 💵"),
            BonusReply::Claimed(None)
        );
    }

    #[test]
    fn parses_bonus_delay() {
        assert_eq!(parse_bonus_delay("Nekiplay, бонус 26.000.000$ и 400 GB 🙂\n🔥 Стрик: 1/7\n⏳ Следующий бонус через 12ч."), Some(12 * 3600));
        assert_eq!(parse_bonus_delay("Роман, до получения бонуса 6 мин. 29 сек. ⏳"), Some(389));
        assert_eq!(parse_bonus_delay("Роман, до получения бонуса 10 мин. 9 сек. ⏳"), Some(609));
        assert_eq!(parse_bonus_delay("Попробуйте через 3 часа 😉"), Some(3 * 3600));
        assert_eq!(parse_bonus_delay("Приходите через 2 дня"), Some(2 * 86_400));
        assert_eq!(parse_bonus_delay("Роман, VIP бонус +200 GB 💵"), None);
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

    #[test]
    fn parses_printer_reply() {
        let sample = "Nekiplay (http://t.me/nekiplay), информация «HP Color LaserJet»:\n\
                      💸 Печатает 2.300.000.000$/час\n\
                      💰 Напечатал: 166.273.388.889$\n\
                      🏧 Можно снять: 166.273.388.889$\n\
                      🎨 Осталось картриджа: 63%\n\n\
                      ✨ Не забывайте перезаправлять принтер краской иначе его заклинит и он сломается! Команда: «принтер заправить»\n\
                      🏧 Снять деньги с принтера, команда: «принтер снять»";
        assert_eq!(
            parse_printer_info(sample),
            PrinterInfo {
                cartridge_pct: Some(63),
                can_withdraw: Some(166_273_388_889),
            }
        );

        let empty = "Nekiplay, информация «HP Color LaserJet»:\n\
                     💸 Печатает 2.300.000.000$/час\n\
                     💰 Напечатал: 0$\n\
                     🏧 Можно снять: 0$\n\
                     🎨 Осталось картриджа: 100%";
        assert_eq!(
            parse_printer_info(empty),
            PrinterInfo {
                cartridge_pct: Some(100),
                can_withdraw: Some(0),
            }
        );
    }
}

// --- Гонка (заезд) -------------------------------------------------------

/// Сколько раз пробуем отправить команду, если бот молчит.
const RACE_ATTEMPTS: u32 = 3;
/// Пауза между попытками отправки.
const RACE_RETRY_GAP: u64 = 25;
/// Пауза перед повтором, если ответ не распознан или результата нет.
const RACE_RETRY_SOON: i64 = race::RETRY_SOON_SECS;

/// Цикл авто-гонки: едет по расписанию, пережидает техосмотр и ждёт результат.
fn spawn_race(shared: Arc<Shared>) {
    tokio::spawn(async move {
        loop {
            let Some(wait) = shared.next_race_in() else {
                shared.race_wake.notified().await;
                continue;
            };
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = shared.race_wake.notified() => {}
            }

            let forced = shared.race_forced.swap(false, Ordering::SeqCst);
            let cfg = shared.cfg();
            if !cfg.race.enabled && !forced {
                continue;
            }
            if !forced {
                match shared.next_race_in() {
                    Some(left) if left > Duration::from_millis(0) => continue,
                    _ => {}
                }
            }

            if let Err(e) = race_once(&shared, &cfg).await {
                shared.log(format!("[гонка] ошибка: {e}"));
                shared.reset_chat().await;
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        }
    });
}

fn race_result_line(win: bool, delta: Option<i64>) -> String {
    let mut line = if win {
        "[гонка] победа".to_string()
    } else {
        "[гонка] проигрыш".to_string()
    };
    if let Some(rating) = delta {
        line.push_str(&format!(", рейтинг {rating:+}"));
    }
    line
}

/// Один заезд: команда → техосмотр → ожидание финиша.
async fn race_once(shared: &Shared, cfg: &Config) -> Result<(), BoxError> {
    let chat = shared.chat().await?;
    let Some((start_id, reply)) = race_send_confirmed(
        shared,
        &chat,
        &cfg.race.command,
        cfg.race.reply_timeout_secs,
    )
    .await?
    else {
        shared.log("[гонка] бот молчит после нескольких попыток — повторю позже");
        shared.set_next_race(now_unix() + RACE_RETRY_SOON);
        return Ok(());
    };

    match race::parse_reply(&reply) {
        RaceReply::Inspection(secs) => {
            // Ждём окончания техосмотра и отправляем команду заново.
            let due = now_unix() + secs as i64 + cfg.race.cooldown_margin_secs as i64;
            shared.log(format!("[гонка] техосмотр: {}", fmt_secs(secs as i64)));
            shared.set_next_race(due);
            Ok(())
        }
        RaceReply::Started => {
            shared.log("[гонка] заезд начался, жду результат");
            let mut after_id = start_id;
            let deadline =
                tokio::time::Instant::now() + Duration::from_secs(cfg.race.result_timeout_secs);
            loop {
                let mut none: Option<UpdateStream> = None;
                let cancel = AtomicBool::new(false);
                let waited = wait_reply(
                    &chat.client,
                    &mut none,
                    chat.bot_ref,
                    chat.bot_id,
                    after_id,
                    deadline,
                    &cancel,
                    false,
                )
                .await?;
                match waited {
                    Wait::Reply(id, text) => {
                        after_id = id;
                        shared.set_status(|s| s.last_bot_reply = one_line(&text));
                        shared.log(format!("[гонка] {}", one_line(&text)));
                        match race::parse_reply(&text) {
                            RaceReply::Finish { win, delta } => {
                                shared.log(race_result_line(win, delta));
                                shared.set_last_race(now_unix());
                                return Ok(());
                            }
                            RaceReply::Inspection(secs) => {
                                let due = now_unix()
                                    + secs as i64
                                    + cfg.race.cooldown_margin_secs as i64;
                                shared.set_next_race(due);
                                return Ok(());
                            }
                            _ => continue,
                        }
                    }
                    Wait::Timeout => {
                        shared.log("[гонка] результат не пришёл — повторю позже");
                        shared.set_next_race(now_unix() + RACE_RETRY_SOON);
                        return Ok(());
                    }
                    Wait::Cancelled => return Ok(()),
                }
            }
        }
        RaceReply::Finish { win, delta } => {
            shared.log(race_result_line(win, delta));
            shared.set_last_race(now_unix());
            Ok(())
        }
        RaceReply::Unknown => {
            shared.log(format!("[гонка] ответ не распознан: {}", one_line(&reply)));
            shared.set_next_race(now_unix() + RACE_RETRY_SOON);
            Ok(())
        }
    }
}

/// Отправка команды гонки. Возвращает id сообщения и ответ бота.
async fn race_send(
    shared: &Shared,
    chat: &Chat,
    cmd: &str,
    timeout_secs: u64,
) -> Result<Option<(i32, String)>, BoxError> {
    let _guard = chat.peer_lock.lock().await;
    shared.log(format!("[гонка] → {cmd}"));
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
        Ok(Wait::Reply(id, text)) => Ok(Some((id, text))),
        Ok(Wait::Timeout) => Ok(None),
        Ok(Wait::Cancelled) => Ok(None),
        Err(e) => {
            shared.log(format!("[гонка] {e}"));
            shared.reset_chat().await;
            Err(e)
        }
    }
}

/// Отправка с повторами: бот иногда не отвечает с первого раза.
async fn race_send_confirmed(
    shared: &Shared,
    chat: &Chat,
    cmd: &str,
    timeout_secs: u64,
) -> Result<Option<(i32, String)>, BoxError> {
    for attempt in 1..=RACE_ATTEMPTS {
        if let Some(reply) = race_send(shared, chat, cmd, timeout_secs).await? {
            return Ok(Some(reply));
        }
        if attempt < RACE_ATTEMPTS {
            shared.log(format!(
                "[гонка] бот не ответил на «{cmd}» — повтор {attempt}/{RACE_ATTEMPTS}"
            ));
            tokio::time::sleep(Duration::from_secs(RACE_RETRY_GAP)).await;
        }
    }
    Ok(None)
}

const PROFILE_ATTEMPTS: u32 = 2;
const PROFILE_RETRY_GAP: u64 = 25;

/// Запрос профиля игрока: «Профиль» → разбор ответа бота.
async fn profile_once(shared: &Shared, cfg: &Config) -> Result<(), BoxError> {
    let chat = shared.chat().await?;
    let mut reply = None;
    for attempt in 1..=PROFILE_ATTEMPTS {
        if let Some(text) =
            profile_send(shared, &chat, "Профиль", cfg.casino.reply_timeout_secs).await?
        {
            reply = Some(text);
            break;
        }
        if attempt < PROFILE_ATTEMPTS {
            shared.log("[профиль] бот не ответил — повторяю");
            tokio::time::sleep(Duration::from_secs(PROFILE_RETRY_GAP)).await;
        }
    }

    let Some(text) = reply else {
        shared.log("[профиль] бот не ответил, профиль не обновлён");
        return Ok(());
    };

    match profile::parse(&text) {
        Some(profile) => {
            shared.log(format!(
                "[профиль] обновлён: уровень {}, наличные {}",
                profile.level.as_deref().unwrap_or("—"),
                profile.cash.as_deref().unwrap_or("—")
            ));
            shared.set_profile(profile);
        }
        None => shared.log(format!(
            "[профиль] ответ не распознан: {}",
            crate::one_line(&text)
        )),
    }
    Ok(())
}

/// Отправка «Профиль» с ожиданием ответа бота.
async fn profile_send(
    shared: &Shared,
    chat: &Chat,
    cmd: &str,
    timeout_secs: u64,
) -> Result<Option<String>, BoxError> {
    let _guard = chat.peer_lock.lock().await;
    shared.log(format!("[профиль] → {cmd}"));
    let sent = match chat.client.send_message(chat.bot_ref, cmd).await {
        Ok(sent) => sent,
        Err(e) => {
            shared.reset_chat().await;
            return Err(e.into());
        }
    };

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
        Ok(Wait::Reply(_, text)) => Ok(Some(text)),
        Ok(_) => Ok(None),
        Err(e) => {
            shared.log(format!("[профиль] {e}"));
            shared.reset_chat().await;
            Err(e)
        }
    }
}

// ── Бизнес: снятие дохода ──────────────────────────────────────────────────

const BUSINESS_ATTEMPTS: u32 = 3;
const BUSINESS_RETRY_GAP: u64 = 25;
const BUSINESS_RETRY_SOON: i64 = 5 * 60;

/// Проверяем, что сообщение бота адресовано нашему игроку.
fn reply_for_player(shared: &Shared, text: &str) -> bool {
    let cfg = shared.cfg();
    let name = cfg.player_name.trim();
    if name.is_empty() {
        return true;
    }
    text.to_lowercase().contains(&name.to_lowercase())
}

/// Достаём сумму из «вы сняли с бизнеса 82.936.250.000$».
fn business_amount(text: &str) -> Option<String> {
    let line = text.lines().find(|l| l.contains("сняли с бизнеса"))?;
    let digits: String = line
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ' ')
        .filter(|c| *c != ' ')
        .collect();
    if digits.is_empty() {
        None
    } else {
        Some(digits)
    }
}

/// Цикл автоснятия бизнеса: раз в час (по умолчанию 1 ч 4 мин).
fn spawn_business(shared: Arc<Shared>) {
    tokio::spawn(async move {
        loop {
            let Some(wait) = shared.next_business_in() else {
                shared.business_wake.notified().await;
                continue;
            };
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = shared.business_wake.notified() => {}
            }

            let forced = shared.business_forced.swap(false, Ordering::SeqCst);
            let cfg = shared.cfg();
            if !cfg.business.enabled && !forced {
                continue;
            }
            if !forced {
                match shared.next_business_in() {
                    Some(left) if left > Duration::from_millis(0) => continue,
                    _ => {}
                }
            }

            if let Err(e) = business_once(&shared, &cfg).await {
                shared.log(format!("[бизнес] ошибка: {e}"));
                shared.reset_chat().await;
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        }
    });
}

async fn business_once(shared: &Shared, cfg: &Config) -> Result<(), BoxError> {
    let chat = shared.chat().await?;
    let Some(reply) = business_send_confirmed(
        shared,
        &chat,
        &cfg.business.command,
        cfg.business.reply_timeout_secs,
    )
    .await?
    else {
        shared.log("[бизнес] бот молчит после нескольких попыток — повторю позже");
        shared.set_next_business(now_unix() + BUSINESS_RETRY_SOON);
        return Ok(());
    };

    if !reply_for_player(shared, &reply) {
        shared.log(format!(
            "[бизнес] ответ не для игрока «{}», пропускаю: {}",
            cfg.player_name,
            one_line(&reply)
        ));
        shared.set_next_business(now_unix() + BUSINESS_RETRY_SOON);
        return Ok(());
    }

    match business_amount(&reply) {
        Some(sum) => shared.log(format!("[бизнес] снято {sum}$")),
        None => shared.log(format!("[бизнес] ответ: {}", one_line(&reply))),
    }
    shared.set_last_business(now_unix());
    Ok(())
}

async fn business_send(
    shared: &Shared,
    chat: &Chat,
    cmd: &str,
    timeout_secs: u64,
) -> Result<Option<String>, BoxError> {
    let _guard = chat.peer_lock.lock().await;
    shared.log(format!("[бизнес] → {cmd}"));
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
        Ok(Wait::Reply(_id, text)) => Ok(Some(text)),
        Ok(Wait::Timeout) => Ok(None),
        Ok(Wait::Cancelled) => Ok(None),
        Err(e) => {
            shared.log(format!("[бизнес] {e}"));
            shared.reset_chat().await;
            Err(e)
        }
    }
}

async fn business_send_confirmed(
    shared: &Shared,
    chat: &Chat,
    cmd: &str,
    timeout_secs: u64,
) -> Result<Option<String>, BoxError> {
    for attempt in 1..=BUSINESS_ATTEMPTS {
        if let Some(reply) = business_send(shared, chat, cmd, timeout_secs).await? {
            return Ok(Some(reply));
        }
        if attempt < BUSINESS_ATTEMPTS {
            shared.log(format!(
                "[бизнес] бот не ответил на «{cmd}» — повтор {attempt}/{BUSINESS_ATTEMPTS}"
            ));
            tokio::time::sleep(Duration::from_secs(BUSINESS_RETRY_GAP)).await;
        }
    }
    Ok(None)
}
