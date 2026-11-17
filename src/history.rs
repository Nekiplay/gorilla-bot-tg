//! История метрик для графиков: кольцевой буфер замеров, который лежит
//! в `gorilla_state.json` рядом со временем отправок.
//!
//! Замер снимает счётчики движка (казино, сейф, капча) и деньги из профиля.
//! Старый `gorilla_state.json` без ключа `history` читается как пустая
//! история: все поля помечены `#[serde(default)]`.

use serde::{Deserialize, Serialize};

/// Сколько замеров держим: при замере раз в минуту это 12 часов.
pub const MAX_SAMPLES: usize = 720;

/// Один замер состояния движка.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    #[serde(default)]
    pub ts: i64,
    #[serde(default)]
    pub rounds: u64,
    #[serde(default)]
    pub wins: u64,
    #[serde(default)]
    pub bet: u64,
    #[serde(default)]
    pub safe_attempts: u64,
    #[serde(default)]
    pub safe_wins: u64,
    #[serde(default)]
    pub safe_found: u64,
    #[serde(default)]
    pub captcha_solved: u64,
    #[serde(default)]
    pub captcha_failed: u64,
    #[serde(default)]
    pub cash: Option<f64>,
    #[serde(default)]
    pub bank: Option<f64>,
    #[serde(default)]
    pub bitcoins: Option<f64>,
}

impl Sample {
    /// Доля побед казино в процентах; `None` — раундов ещё не было.
    pub fn win_rate(&self) -> Option<f64> {
        if self.rounds == 0 {
            return None;
        }
        Some(self.wins as f64 * 100.0 / self.rounds as f64)
    }

    /// Похож ли замер на предыдущий по всем счётчикам и деньгам.
    fn same_counters(&self, other: &Self) -> bool {
        self.rounds == other.rounds
            && self.wins == other.wins
            && self.bet == other.bet
            && self.safe_attempts == other.safe_attempts
            && self.safe_wins == other.safe_wins
            && self.safe_found == other.safe_found
            && self.captcha_solved == other.captcha_solved
            && self.captcha_failed == other.captcha_failed
            && self.cash == other.cash
            && self.bank == other.bank
            && self.bitcoins == other.bitcoins
    }
}

/// Кольцевая история замеров, сериализуется в `gorilla_state.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct History {
    #[serde(default)]
    samples: Vec<Sample>,
}

impl History {
    /// Все замеры от старых к новым.
    pub fn samples(&self) -> &[Sample] {
        &self.samples
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// Последний замер.
    pub fn last(&self) -> Option<&Sample> {
        self.samples.last()
    }

    /// Первый замер (с него считаем прогресс за историю).
    pub fn first(&self) -> Option<&Sample> {
        self.samples.first()
    }

    /// Добавляет замер. Повтор без единого изменения счётчиков только
    /// обновляет время, чтобы линия графика не рвалась, но файл не пух.
    pub fn push(&mut self, sample: Sample) {
        if let Some(last) = self.samples.last_mut() {
            if last.same_counters(&sample) {
                last.ts = sample.ts;
                return;
            }
        }
        self.samples.push(sample);
        let extra = self.samples.len().saturating_sub(MAX_SAMPLES);
        if extra > 0 {
            self.samples.drain(0..extra);
        }
    }

    /// Разница счётчиков от первого замера до последнего: (раунды, победы,
    /// сейфы, капчи). Нужна карточкам «за историю».
    pub fn progress(&self) -> (u64, u64, u64, u64) {
        let (Some(first), Some(last)) = (self.first(), self.last()) else {
            return (0, 0, 0, 0);
        };
        (
            last.rounds.saturating_sub(first.rounds),
            last.wins.saturating_sub(first.wins),
            last.safe_wins.saturating_sub(first.safe_wins),
            (last.captcha_solved + last.captcha_failed)
                .saturating_sub(first.captcha_solved + first.captcha_failed),
        )
    }
}

/// Число из строки профиля: «1 234 567 $», «1,234,567», «0.005 BTC».
/// Пустая строка и мусор дают `None`.
pub fn parse_money(raw: Option<&String>) -> Option<f64> {
    let raw = raw?;
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == ',' || *c == '-')
        .collect();
    if cleaned.is_empty() {
        return None;
    }

    // Разделители: «1,234,567» и «1.234.567» — тысячи, «0.005» и «12,5» — дробь.
    let dots = cleaned.matches('.').count();
    let commas = cleaned.matches(',').count();
    let normalized = if dots > 0 && commas > 0 {
        // Десятичный — тот, что правее.
        let dot_at = cleaned.rfind('.').unwrap_or(0);
        let comma_at = cleaned.rfind(',').unwrap_or(0);
        let decimal = if dot_at > comma_at { '.' } else { ',' };
        let (decimal_char, thousands) = if decimal == '.' {
            ('.', ',')
        } else {
            (',', '.')
        };
        cleaned.replace(thousands, "").replace(decimal_char, ".")
    } else if dots > 1 || commas > 1 {
        cleaned.replace(['.', ','], "")
    } else if dots == 1 || commas == 1 {
        let sep = if dots == 1 { '.' } else { ',' };
        let (whole, frac) = cleaned.split_once(sep).unwrap_or((&cleaned, ""));
        // «0.005» — десятичная дробь, «1.234» — тысячи: ведущий ноль решает.
        let groups_ok =
            frac.len() == 3 && !whole.is_empty() && whole.len() <= 3 && !whole.starts_with('0');
        if groups_ok {
            format!("{whole}{frac}")
        } else {
            cleaned.replace(sep, ".")
        }
    } else {
        cleaned
    };

    normalized.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    #[test]
    fn money_with_spaces() {
        assert_eq!(parse_money(s("1 234 567 $").as_ref()), Some(1_234_567.0));
    }

    #[test]
    fn money_with_commas() {
        assert_eq!(parse_money(s("1,234,567").as_ref()), Some(1_234_567.0));
    }

    #[test]
    fn money_decimal_bitcoins() {
        let v = parse_money(s("0.005").as_ref()).unwrap();
        assert!((v - 0.005).abs() < 1e-9);
    }

    #[test]
    fn money_empty_and_none() {
        assert_eq!(parse_money(None), None);
        assert_eq!(parse_money(s("").as_ref()), None);
        assert_eq!(parse_money(s("—").as_ref()), None);
    }

    #[test]
    fn history_keeps_cap_and_progress() {
        let mut history = History::default();
        history.push(Sample {
            ts: 1,
            rounds: 2,
            wins: 1,
            ..Sample::default()
        });
        history.push(Sample {
            ts: 2,
            rounds: 10,
            wins: 4,
            ..Sample::default()
        });
        assert_eq!(history.progress(), (8, 3, 0, 0));
        // Повтор без изменений не добавляет замер, только обновляет время.
        history.push(Sample {
            ts: 3,
            rounds: 10,
            wins: 4,
            ..Sample::default()
        });
        assert_eq!(history.len(), 2);
        assert_eq!(history.last().unwrap().ts, 3);
    }
}
