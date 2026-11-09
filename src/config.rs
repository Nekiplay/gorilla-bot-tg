//! Чтение `config.json`.

use serde::Deserialize;
use std::fs;
use std::path::Path;

/// Настройки бота.
#[derive(Debug, Clone, Deserialize)]
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
    pub casino: CasinoConfig,
}

/// Настройки авто-казино (мартингейл).
#[derive(Debug, Clone, Deserialize)]
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
    /// Во сколько раз увеличивать ставку после проигрыша.
    #[serde(default = "default_multiplier")]
    pub multiplier: f64,
    /// Потолок ставки; выше — не поднимаемся.
    #[serde(default = "default_max_bet")]
    pub max_bet: u64,
    /// Допустимое число проигрышей подряд до остановки.
    #[serde(default = "default_max_losses")]
    pub max_losses_in_row: u32,
    /// Пауза между раундами поверх кулдауна бота, секунды.
    #[serde(default = "default_round_gap")]
    pub round_gap_secs: u64,
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
fn default_max_bet() -> u64 {
    12_800_000
}
fn default_max_losses() -> u32 {
    7
}
fn default_round_gap() -> u64 {
    2
}
fn default_reply_timeout() -> u64 {
    90
}
fn default_cooldown_margin() -> u64 {
    2
}

impl Config {
    /// Читает `config.json` из текущей папки.
    pub fn load() -> Result<Self, String> {
        Self::load_path("config.json")
    }

    pub fn load_path<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let path = path.as_ref();
        let raw = fs::read_to_string(path)
            .map_err(|e| format!("не удалось прочитать {}: {e}", path.display()))?;
        serde_json::from_str(&raw).map_err(|e| format!("ошибка в {}: {e}", path.display()))
    }
}
