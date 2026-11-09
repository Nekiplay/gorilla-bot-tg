//! Разбор ответов Bot Gorilla и стратегия мартингейла
//! (удвоение ставки после проигрыша, сброс после выигрыша).

use crate::config::CasinoConfig;

/// Что бот ответил на команду.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Выигрыш — ставку сбросить до начальной.
    Win,
    /// Проигрыш — ставку увеличить.
    Lose,
    /// Бот назвал кулдаун: ждать столько секунд и повторить команду.
    Cooldown(u64),
    ///Средств на ставку не хватает.
    NoFunds,
    /// Ответ не распознан (внутри — нормализованный текст, чтобы логировать).
    Unknown(String),
}

/// Разбирает текст ответа бота.
///
/// Правила подобраны по типовым текстам Bot Gorilla; всё нераспознанное
/// возвращается как [`Outcome::Unknown`] — вызывающий код при этом
/// останавливается, чтобы не ставить вслепую.
pub fn parse_reply(text: &str) -> Outcome {
    let lower = text.to_lowercase();

    if lower.contains("недостаточно") || lower.contains("не хватает") {
        return Outcome::NoFunds;
    }
    if let Some(secs) = find_cooldown_seconds(&lower) {
        return Outcome::Cooldown(secs);
    }
    if lower.contains("выигр") || lower.contains("побед") {
        return Outcome::Win;
    }
    if lower.contains("проигр") || lower.contains("выпал") && !lower.contains("выигр") {
        return Outcome::Lose;
    }
    Outcome::Unknown(lower)
}

/// Ищет «<число> сек» (сек/секунду/секунд...) — кулдаун, названный ботом.
fn find_cooldown_seconds(lower: &str) -> Option<u64> {
    let mut last_number: Option<u64> = None;
    for word in lower.split_whitespace() {
        if let Some(n) = leading_number(word) {
            // Явно кулдаун, а не случайный ID из текста.
            last_number = (n <= 86_400).then_some(n);
        } else if word.starts_with("сек") && last_number.is_some() {
            return last_number;
        }
    }
    None
}

/// Число в начале слова: «30,» → 30.
fn leading_number(word: &str) -> Option<u64> {
    let digits: String = word.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

/// Состояние мартингейла.
pub struct Martingale {
    start_bet: u64,
    multiplier: f64,
    max_bet: u64,
    max_losses_in_row: u32,
    bet: u64,
    losses_in_row: u32,
    rounds: u64,
    wins: u64,
    bet_capped: bool,
}

impl Martingale {
    pub fn new(cfg: &CasinoConfig) -> Self {
        let start = cfg.start_bet.max(1).min(cfg.max_bet);
        Self {
            start_bet: start,
            multiplier: cfg.multiplier.max(1.0),
            max_bet: cfg.max_bet,
            max_losses_in_row: cfg.max_losses_in_row,
            bet: start,
            losses_in_row: 0,
            rounds: 0,
            wins: 0,
            bet_capped: cfg.start_bet > cfg.max_bet,
        }
    }

    /// Текущая ставка.
    pub fn current_bet(&self) -> u64 {
        self.bet
    }

    /// Победа: сброс ставки до начальной.
    pub fn on_win(&mut self) {
        self.wins += 1;
        self.losses_in_row = 0;
        self.bet = self.start_bet;
    }

    /// Проигрыш: ставка вырастает в `multiplier` раз (не выше `max_bet`).
    /// Возвращает `false`, когда пора останавливаться (лимит проигрышей подряд).
    pub fn on_lose(&mut self) -> bool {
        self.losses_in_row += 1;
        let next = (self.bet as f64 * self.multiplier).round() as u64;
        if next >= self.max_bet {
            self.bet = self.max_bet;
            self.bet_capped = true;
        } else {
            self.bet = next;
        }
        self.losses_in_row < self.max_losses_in_row
    }

    /// Засчитать отправленную команду раундом.
    pub fn note_round(&mut self) {
        self.rounds += 1;
    }

    /// Итог для лога/консоли.
    pub fn stats(&self) -> String {
        format!(
            "раундов: {}, побед: {}, проигрышей подряд: {}, ставка сейчас: {}{}",
            self.rounds,
            self.wins,
            self.losses_in_row,
            self.bet,
            if self.bet_capped { " (упёрлись в потолок)" } else { "" }
        )
    }
}
