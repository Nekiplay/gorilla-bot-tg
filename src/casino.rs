//! Разбор ответов Bot Gorilla и стратегия мартингейла
//! (удвоение ставки после проигрыша, сброс после выигрыша).

use crate::config::CasinoConfig;
use rand::Rng as _;
use std::time::Duration;

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
    multiplier_jitter: f64,
    bet_round_to: u64,
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
            // ±jitter к базовому множителю, но не уходим ниже 1.05x:
            // иначе после проигрыша ставка почти не растёт и не отбивает минус.
            multiplier_jitter: cfg.multiplier_jitter.clamp(0.0, 0.9),
            bet_round_to: cfg.bet_round_to,
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

    /// Проигрыш: ставка вырастает в случайные `multiplier ± jitter` раз
    /// (не выше `max_bet`). Возвращает `false`, когда пора останавливаться
    /// (лимит проигрышей подряд).
    pub fn on_lose(&mut self) -> bool {
        self.losses_in_row += 1;
        let mut next = self.grow_bet();
        if next >= self.max_bet {
            next = self.max_bet;
            self.bet_capped = true;
        }
        self.bet = next;
        self.losses_in_row < self.max_losses_in_row
    }

    /// Следующая ставка: `bet * multiplier`, размазанная случайным множителем
    /// и округлённая до «человеческого» шага (чтобы не было ровных x2).
    fn grow_bet(&self) -> u64 {
        let factor = if self.multiplier_jitter > 0.0 {
            let j = self.multiplier_jitter;
            self.multiplier * rand::rng().random_range(1.0 - j..=1.0 + j)
        } else {
            self.multiplier
        };
        let mut next = (self.bet as f64 * factor.max(1.05)).round() as u64;
        if self.bet_round_to > 1 {
            let step = self.bet_round_to;
            next = ((next + step / 2) / step) * step;
        }
        // Хотя бы +5% к прошлой ставке — иначе мартингейл не растёт.
        next.max(self.bet + (self.bet / 20).max(1))
    }

    /// Засчитать отправленную команду раундом.
    pub fn note_round(&mut self) {
        self.rounds += 1;
    }

    /// Сколько раундов уже сыграно.
    pub fn rounds(&self) -> u64 {
        self.rounds
    }

    /// Сколько побед.
    pub fn wins(&self) -> u64 {
        self.wins
    }

    /// Сколько проигрышей подряд.
    pub fn losses_in_row(&self) -> u32 {
        self.losses_in_row
    }

    /// Подхватывает изменённые настройки, не сбрасывая текущую ставку
    /// (например, если пользователь поправил их в GUI посреди игры).
    pub fn apply_config(&mut self, cfg: &CasinoConfig) {
        self.multiplier = cfg.multiplier.max(1.0);
        self.multiplier_jitter = cfg.multiplier_jitter.clamp(0.0, 0.9);
        self.bet_round_to = cfg.bet_round_to;
        self.max_bet = cfg.max_bet.max(1);
        self.max_losses_in_row = cfg.max_losses_in_row.max(1);
        if self.bet > self.max_bet {
            self.bet = self.max_bet;
        }
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

/// Пауза между раундами с разбросом: `round_gap_secs ± round_gap_jitter_secs`.
/// Разброс не больше самой паузы — она остаётся положительной.
pub fn round_gap(cfg: &CasinoConfig) -> Duration {
    let base = cfg.round_gap_secs as i64;
    let jitter = (cfg.round_gap_jitter_secs as i64).min(base.max(0));
    let secs = if jitter > 0 {
        rand::rng().random_range(base - jitter..=base + jitter)
    } else {
        base
    };
    Duration::from_secs(secs.max(0) as u64)
}
