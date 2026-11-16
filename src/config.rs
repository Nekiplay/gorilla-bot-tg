//! Чтение `config.json`.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Настройки бота.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    /// API ID с my.telegram.org.
    pub api_id: i32,
    /// API hash с my.telegram.org.
    pub api_hash: String,
    /// Телефон в международном формате (+7...). Пусто — спросит при `login`.
    #[serde(default)]
    pub phone: String,
    /// Username Bot Gorilla без «@».
    #[serde(default = "default_bot_username")]
    pub bot_username: String,
    /// Файл сессии (sqlite).
    #[serde(default = "default_session_file")]
    pub session_file: String,
    /// SOCKS5-прокси для соединения с Telegram, например
    /// `socks5://user:pass@host:port`. Пусто/null — прямое подключение.
    #[serde(default)]
    pub proxy: Option<String>,
    /// MTProto-прокси (например, TgWsProxy): бот поднимет локальный
    /// SOCKS5-мост и будет ходить через него.
    #[serde(default)]
    pub mtproxy: MtProxyConfig,
    /// Настройки авто-казино (мартингейл).
    #[serde(default)]
    pub casino: CasinoConfig,
    /// Автоматический поход питомца.
    #[serde(default)]
    pub pet: PetConfig,
    /// Автоматическое обслуживание принтера («Принтер», «Принтер заправить», «Принтер снять»).
    #[serde(default)]
    pub printer: PrinterConfig,
    /// Автоотправка «Бонус» в чат/канал по расписанию.
    #[serde(default)]
    pub bonus: BonusConfig,
    /// Второй таймер: «Бонус» в личке бота (цикл около 12 часов).
    #[serde(default = "default_bonus_dm")]
    pub bonus_dm: BonusConfig,
    /// Автоматическая «Гонка» (заезд) по расписанию.
    #[serde(default)]
    pub race: RaceConfig,
    /// Автоснятие денег с бизнеса («Бизнес снять»).
    #[serde(default)]
    pub business: BusinessConfig,
    /// Автоподбор кода «Сейф» (две цифры, попытки не ограничены).
    #[serde(default)]
    pub safe: SafeConfig,
    /// Ник игрока в боте: по нему проверяем, что ответ адресован нам.
    #[serde(default)]
    pub player_name: String,
    /// Папка, где лежит config.json; от неё считаются пути сессии/логов.
    #[serde(skip)]
    pub base_dir: PathBuf,
}

/// Параметры настоящего MTProto-прокси (например, TgWsProxy на 127.0.0.1:1443).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MtProxyConfig {
    /// Адрес прокси.
    #[serde(default = "default_mtp_host")]
    pub host: String,
    /// Порт прокси.
    #[serde(default = "default_mtp_port")]
    pub port: u16,
    /// Секрет (hex, с префиксом dd или без). Пусто — MTProto-мост выключен.
    #[serde(default)]
    pub secret: String,
    /// Cloudflare-Worker (например, `name.workers.dev`). Если задан —
    /// используется WSS-труба через него вместо obfuscated2-прокси.
    /// Пусто — берутся стандартные CF-прокси из tg-ws-proxy;
    /// `off`/`none` — WSS-туннеля нет вовсе.
    #[serde(default)]
    pub worker: String,
}

fn default_mtp_host() -> String {
    "127.0.0.1".to_string()
}

fn default_mtp_port() -> u16 {
    1443
}

impl Default for MtProxyConfig {
    fn default() -> Self {
        Self {
            host: default_mtp_host(),
            port: default_mtp_port(),
            secret: String::new(),
            worker: String::new(),
        }
    }
}

/// Маршрут WSS-туннеля к DC Telegram (DPI его не видит: снаружи это TLS).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsTunnel {
    /// Свой Cloudflare-worker (`name.workers.dev`).
    Worker(String),
    /// Стандартные CF-прокси из tg-ws-proxy (`kws<dc>.<домен>`).
    Defaults,
}

impl MtProxyConfig {
    /// Свой воркер, если он задан (пусто/`off`/`none` — нет).
    pub fn worker_domain(&self) -> Option<&str> {
        let worker = self.worker.trim();
        let disabled = worker.is_empty()
            || worker == "-"
            || worker.eq_ignore_ascii_case("off")
            || worker.eq_ignore_ascii_case("none");
        if disabled {
            None
        } else {
            Some(worker)
        }
    }

    /// Есть ли WSS-туннель и какой. Свой воркер > секрет MTProto-прокси >
    /// стандартные CF-прокси tg-ws-proxy.
    pub fn ws_tunnel(&self) -> Option<WsTunnel> {
        if let Some(worker) = self.worker_domain() {
            return Some(WsTunnel::Worker(worker.to_string()));
        }
        // Свой воркер явно выключен или есть локальный MTProto-прокси:
        // стандартные CF-прокси не навязываем.
        if !self.worker.trim().is_empty() || !self.secret.trim().is_empty() {
            return None;
        }
        Some(WsTunnel::Defaults)
    }
}

/// Настройки авто-казино (мартингейл).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CasinoConfig {
    /// Шаблон команды: `{bet}` заменяется на текущую ставку.
    #[serde(default = "default_command")]
    pub command: String,
    /// Команда запроса баланса.
    #[serde(default = "default_balance_command")]
    pub balance_command: String,
    /// Начальная ставка.
    #[serde(default = "default_start_bet")]
    pub start_bet: u64,
    /// Во сколько раз увеличивать ставку после проигрыша (базовый множитель).
    #[serde(default = "default_multiplier")]
    pub multiplier: f64,
    /// Разброс множителя: фактический рост = multiplier * (1 ± jitter),
    /// чтобы бот не видел идеальный x2. 0.25 = от x1.5 до x2.5.
    #[serde(default = "default_multiplier_jitter")]
    pub multiplier_jitter: f64,
    /// Округление ставки (0 или 1 — без округления).
    #[serde(default = "default_bet_round_to")]
    pub bet_round_to: u64,
    /// Потолок ставки; выше — не поднимаемся.
    #[serde(default = "default_max_bet")]
    pub max_bet: u64,
    /// Допустимое число проигрышей подряд до остановки.
    #[serde(default = "default_max_losses")]
    pub max_losses_in_row: u32,
    /// Пауза между раундами поверх кулдауна бота, секунды.
    #[serde(default = "default_round_gap")]
    pub round_gap_secs: u64,
    /// Случайный разброс паузы: фактическая = round_gap_secs ± jitter,
    /// чтобы бот не видел ровный ритм ставок.
    #[serde(default = "default_round_gap_jitter")]
    pub round_gap_jitter_secs: u64,
    /// Сколько секунд ждать ответ бота на команду.
    #[serde(default = "default_reply_timeout")]
    pub reply_timeout_secs: u64,
    /// Запас к кулдауну, который назвал сам бот, секунды.
    #[serde(default = "default_cooldown_margin")]
    pub cooldown_margin_secs: u64,
}

fn default_bot_username() -> String {
    "realgorillabot".to_string()
}
fn default_session_file() -> String {
    "gorilla.session".to_string()
}
fn default_command() -> String {
    "Казино {bet}".to_string()
}
fn default_balance_command() -> String {
    "Баланс".to_string()
}
fn default_start_bet() -> u64 {
    100_000
}
fn default_multiplier() -> f64 {
    2.0
}
fn default_multiplier_jitter() -> f64 {
    0.25
}
fn default_bet_round_to() -> u64 {
    1_000
}
fn default_max_bet() -> u64 {
    12_800_000
}
fn default_max_losses() -> u32 {
    7
}
fn default_round_gap() -> u64 {
    2
}
fn default_round_gap_jitter() -> u64 {
    3
}
fn default_reply_timeout() -> u64 {
    90
}
fn default_cooldown_margin() -> u64 {
    2
}

impl Default for CasinoConfig {
    fn default() -> Self {
        Self {
            command: default_command(),
            balance_command: default_balance_command(),
            start_bet: default_start_bet(),
            multiplier: default_multiplier(),
            multiplier_jitter: default_multiplier_jitter(),
            bet_round_to: default_bet_round_to(),
            max_bet: default_max_bet(),
            max_losses_in_row: default_max_losses(),
            round_gap_secs: default_round_gap(),
            round_gap_jitter_secs: default_round_gap_jitter(),
            reply_timeout_secs: default_reply_timeout(),
            cooldown_margin_secs: default_cooldown_margin(),
        }
    }
}

/// Автоматический «Питомец поход»: раз в час бот считает поход завершённым,
/// но серверу нужно время на обработку, поэтому интервал — около 1 ч 4 мин,
/// со случайным разбросом (1 ч 3 мин … 1 ч 7 мин).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PetConfig {
    /// Слать команду автоматически.
    #[serde(default = "default_pet_enabled")]
    pub enabled: bool,
    /// Команда похода.
    #[serde(default = "default_pet_command")]
    pub command: String,
    /// Команда кормления, если бот ответил «покормите своего питомца».
    /// Пусто — не кормить, просто писать в лог.
    #[serde(default = "default_feed_command")]
    pub feed_command: String,
    /// Кормить автоматически, когда бот просит.
    #[serde(default = "default_pet_enabled")]
    pub feed_when_hungry: bool,
    /// Интервал между отправками, секунды (по умолчанию 3600 + 240).
    #[serde(default = "default_pet_interval")]
    pub interval_secs: u64,
    /// Разброс интервала: фактический = interval_secs ± jitter
    /// (3840 ± 240 с → от 1 ч 3 мин до 1 ч 7 мин).
    #[serde(default = "default_pet_jitter")]
    pub interval_jitter_secs: u64,
    /// Сколько секунд ждать ответ бота.
    #[serde(default = "default_pet_timeout")]
    pub reply_timeout_secs: u64,
}

fn default_pet_enabled() -> bool {
    true
}
fn default_pet_command() -> String {
    "Питомец поход".to_string()
}
fn default_feed_command() -> String {
    "Питомец покормить".to_string()
}
fn default_pet_interval() -> u64 {
    3_600 + 240
}
fn default_pet_jitter() -> u64 {
    240
}
fn default_pet_timeout() -> u64 {
    30
}

impl Default for PetConfig {
    fn default() -> Self {
        Self {
            enabled: default_pet_enabled(),
            command: default_pet_command(),
            feed_command: default_feed_command(),
            feed_when_hungry: true,
            interval_secs: default_pet_interval(),
            interval_jitter_secs: default_pet_jitter(),
            reply_timeout_secs: default_pet_timeout(),
        }
    }
}

/// Автоматическое обслуживание принтера: проверяет статус («Принтер»),
/// заправляет картридж («Принтер заправить») и снимает деньги («Принтер снять»).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PrinterConfig {
    /// Включить авто-принтер по расписанию.
    #[serde(default = "default_printer_enabled")]
    pub enabled: bool,
    /// Команда проверки принтера.
    #[serde(default = "default_printer_command")]
    pub command: String,
    /// Команда заправки картриджа.
    #[serde(default = "default_printer_refill_command")]
    pub refill_command: String,
    /// Команда снятия денег с принтера.
    #[serde(default = "default_printer_withdraw_command")]
    pub withdraw_command: String,
    /// Заправлять принтер автоматически.
    #[serde(default = "default_printer_enabled")]
    pub auto_refill: bool,
    /// Заправлять, если картриджа осталось <= этого процента (100 = всегда, если < 100%).
    #[serde(default = "default_printer_refill_below")]
    pub refill_below_percent: u8,
    /// Снимать деньги с принтера автоматически (когда есть что снять).
    #[serde(default = "default_printer_enabled")]
    pub auto_withdraw: bool,
    /// Интервал между проверками/обслуживанием, секунды (по умолчанию 1 час).
    #[serde(default = "default_printer_interval")]
    pub interval_secs: u64,
    /// Разброс интервала: фактический = interval_secs ± jitter.
    #[serde(default = "default_printer_jitter")]
    pub interval_jitter_secs: u64,
    /// Сколько секунд ждать ответ бота.
    #[serde(default = "default_printer_timeout")]
    pub reply_timeout_secs: u64,
}

fn default_printer_enabled() -> bool {
    true
}
fn default_printer_command() -> String {
    "Принтер".to_string()
}
fn default_printer_refill_command() -> String {
    "Принтер заправить".to_string()
}
fn default_printer_withdraw_command() -> String {
    "Принтер снять".to_string()
}
fn default_printer_refill_below() -> u8 {
    95
}
fn default_printer_interval() -> u64 {
    3_600
}
fn default_printer_jitter() -> u64 {
    300
}
fn default_printer_timeout() -> u64 {
    30
}

impl Default for PrinterConfig {
    fn default() -> Self {
        Self {
            enabled: default_printer_enabled(),
            command: default_printer_command(),
            refill_command: default_printer_refill_command(),
            withdraw_command: default_printer_withdraw_command(),
            auto_refill: true,
            refill_below_percent: default_printer_refill_below(),
            auto_withdraw: true,
            interval_secs: default_printer_interval(),
            interval_jitter_secs: default_printer_jitter(),
            reply_timeout_secs: default_printer_timeout(),
        }
    }
}

/// Автоотправка команды «Бонус» в канал/чат (по умолчанию — канал
/// t.me/gorillabot_double, «Double Bot Gorilla»).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BonusConfig {
    /// Слать автоматически.
    #[serde(default = "default_pet_enabled")]
    pub enabled: bool,
    /// @username бота или название чата/канала из списка диалогов.
    #[serde(default = "default_bonus_target")]
    pub target: String,
    /// Команда, которая уходит в чат.
    #[serde(default = "default_bonus_command")]
    pub command: String,
    /// Запасной интервал, секунды: используется, только если бот не назвал
    /// срок в ответе (по умолчанию 15 минут).
    #[serde(default = "default_bonus_interval")]
    pub interval_secs: u64,
    /// Разброс вокруг срока из ответа бота, ± секунды.
    #[serde(default = "default_bonus_jitter")]
    pub jitter_secs: u64,
}

fn default_bonus_target() -> String {
    "Double Bot Gorilla".to_string()
}
fn default_bonus_command() -> String {
    "Бонус".to_string()
}
fn default_bonus_interval() -> u64 {
    15 * 60
}
fn default_bonus_jitter() -> u64 {
    5 * 60
}

/// Бонус в личных сообщениях бота живёт на своём цикле (~12 часов).
fn default_bonus_dm() -> BonusConfig {
    BonusConfig {
        enabled: true,
        target: "realgorillabot".to_string(),
        command: default_bonus_command(),
        interval_secs: 12 * 60 * 60,
        jitter_secs: 5 * 60,
    }
}

impl Default for BonusConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            target: default_bonus_target(),
            command: default_bonus_command(),
            interval_secs: default_bonus_interval(),
            jitter_secs: default_bonus_jitter(),
        }
    }
}

/// Автоматическая «Гонка»: бот сначала проводит техосмотр (срок называет
/// сам), затем заезд против другого игрока. Результат приходит отдельным
/// сообщением, поэтому таймер ждёт и его.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RaceConfig {
    /// Слать команду автоматически.
    #[serde(default = "default_race_enabled")]
    pub enabled: bool,
    /// Команда заезда.
    #[serde(default = "default_race_command")]
    pub command: String,
    /// Интервал между заездами, секунды (по умолчанию 30 минут).
    #[serde(default = "default_race_interval")]
    pub interval_secs: u64,
    /// Разброс интервала: фактический = interval_secs ± jitter.
    #[serde(default = "default_race_jitter")]
    pub interval_jitter_secs: u64,
    /// Сколько секунд ждать ответ бота на команду.
    #[serde(default = "default_race_timeout")]
    pub reply_timeout_secs: u64,
    /// Запас к сроку техосмотра, который назвал бот, секунды.
    #[serde(default = "default_race_inspection_margin")]
    pub cooldown_margin_secs: u64,
    /// Сколько ждать сообщение с результатом уже начавшегося заезда.
    #[serde(default = "default_race_result_timeout")]
    pub result_timeout_secs: u64,
}

fn default_race_enabled() -> bool {
    true
}
fn default_race_command() -> String {
    "Гонка".to_string()
}
fn default_race_interval() -> u64 {
    30 * 60
}
fn default_race_jitter() -> u64 {
    5 * 60
}
fn default_race_timeout() -> u64 {
    45
}
fn default_race_inspection_margin() -> u64 {
    10
}
fn default_race_result_timeout() -> u64 {
    4 * 60
}

impl Default for RaceConfig {
    fn default() -> Self {
        Self {
            enabled: default_race_enabled(),
            command: default_race_command(),
            interval_secs: default_race_interval(),
            interval_jitter_secs: default_race_jitter(),
            reply_timeout_secs: default_race_timeout(),
            cooldown_margin_secs: default_race_inspection_margin(),
            result_timeout_secs: default_race_result_timeout(),
        }
    }
}

/// Автоподбор кода сейфа: «Сейф 42», попытки не ограничены.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafeConfig {
    /// Шаблон команды: `{code}` заменяется на код из 1..99.
    #[serde(default = "default_safe_command")]
    pub command: String,
    /// Пауза между попытками, секунды.
    #[serde(default = "default_safe_delay")]
    pub delay_secs: f64,
}

fn default_safe_command() -> String {
    "Сейф {code}".into()
}

fn default_safe_delay() -> f64 {
    4.0
}

impl Default for SafeConfig {
    fn default() -> Self {
        Self {
            command: default_safe_command(),
            delay_secs: default_safe_delay(),
        }
    }
}


/// Автоснятие дохода с бизнеса: команда «Бизнес снять» раз в час.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusinessConfig {
    /// Слать команду автоматически.
    #[serde(default = "default_business_enabled")]
    pub enabled: bool,
    /// Что именно отправляем боту.
    #[serde(default = "default_business_command")]
    pub command: String,
    /// Интервал между отправками, секунды (по умолчанию 1 час 4 минуты).
    #[serde(default = "default_business_interval")]
    pub interval_secs: u64,
    /// Разброс интервала: фактический = interval_secs ± jitter.
    #[serde(default)]
    pub interval_jitter_secs: u64,
    /// Сколько секунд ждать ответ бота на команду.
    #[serde(default = "default_business_timeout")]
    pub reply_timeout_secs: u64,
}

fn default_business_enabled() -> bool {
    true
}
fn default_business_command() -> String {
    "Бизнес снять".to_string()
}
fn default_business_interval() -> u64 {
    64 * 60
}
fn default_business_timeout() -> u64 {
    45
}

impl Default for BusinessConfig {
    fn default() -> Self {
        Self {
            enabled: default_business_enabled(),
            command: default_business_command(),
            interval_secs: default_business_interval(),
            interval_jitter_secs: 0,
            reply_timeout_secs: default_business_timeout(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_id: 0,
            api_hash: String::new(),
            phone: String::new(),
            bot_username: default_bot_username(),
            session_file: default_session_file(),
            proxy: None,
            mtproxy: MtProxyConfig::default(),
            casino: CasinoConfig::default(),
            pet: PetConfig::default(),
            printer: PrinterConfig::default(),
            bonus: BonusConfig::default(),
            bonus_dm: default_bonus_dm(),
            race: RaceConfig::default(),
            business: BusinessConfig::default(),
            safe: SafeConfig::default(),
            player_name: String::new(),
            base_dir: PathBuf::from("."),
        }
    }
}

impl Config {
    /// Читает `config.json`: сначала в текущей папке, затем рядом с exe
    /// и вверх по дереву (чтобы exe работал и из `target\release`).
    pub fn load() -> Result<Self, String> {
        // Конфиг живёт рядом с exe: релиз запускают из своей папки, и чужие
        // config.json из дерева проекта подхватываться не должны.
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf));
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(dir) = &exe_dir {
            candidates.push(dir.join("config.json"));
        }
        candidates.push(PathBuf::from("config.json"));

        for path in &candidates {
            if path.is_file() {
                return Self::load_path(path);
            }
        }
        Err("не найден config.json рядом с gorilla-bot.exe".into())
    }

    pub fn load_path<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let path = path.as_ref();
        let raw = fs::read_to_string(path)
            .map_err(|e| format!("не удалось прочитать {}: {e}", path.display()))?;
        let mut cfg: Config =
            serde_json::from_str(&raw).map_err(|e| format!("ошибка в {}: {e}", path.display()))?;
        cfg.base_dir = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        Ok(cfg)
    }
}
