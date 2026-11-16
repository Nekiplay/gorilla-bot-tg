//! Разбор ответов Bot Gorilla на «Гонку» (заезд) и тайминги.
//!
//! Бот отвечает в три приёма:
//! * «машина проходит техосмотр, подождите 9 мин. 48 сек.!» — ехать пока
//!   нельзя, срок назван в ответе;
//! * «начался заезд против «…»! Ожидаем прибытие машин к финишу...» — заезд
//!   пошёл, результат придёт отдельным сообщением;
//! * «первым к финишу пришёл противник! -200» — итог и дельта рейтинга.
//!
//! Парсер не привязан к точным формулировкам: важны «техосмотр»/«подождите»,
//! «ожидаем прибытие» и слово «финиш» вместе с упоминанием противника.

use crate::config::RaceConfig;
use rand::Rng as _;

/// Ответ бота на команду «Гонка».
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RaceReply {
    /// Техосмотр: бот просит подождать `u64` секунд перед заездом.
    Inspection(u64),
    /// Заезд начался — ждём отдельное сообщение с результатом.
    Started,
    /// Заезд завершён; `win` — победил игрок, `delta` — изменение рейтинга.
    Finish { win: bool, delta: Option<i64> },
    /// Ничего знакомого в ответе.
    Unknown,
}

/// Сколько ждём, если в ответе про техосмотр срок не распознан.
pub const DEFAULT_INSPECTION_SECS: u64 = 60;

/// Разбирает одно сообщение бота.
pub fn parse_reply(text: &str) -> RaceReply {
    let lower = text.to_lowercase();

    // Техосмотр проверяем первым: в этом ответе тоже бывает «заезд».
    if lower.contains("техосмотр") || lower.contains("подождите") {
        let secs = crate::engine::parse_duration(&lower).unwrap_or(DEFAULT_INSPECTION_SECS);
        return RaceReply::Inspection(secs.max(1));
    }

    // Старт заезда: «Ожидаем прибытие машин к финишу» — ещё не результат.
    if lower.contains("начался заезд")
        || lower.contains("ожидаем прибытие")
        || lower.contains("заезд начался")
    {
        return RaceReply::Started;
    }

    if lower.contains("финиш") {
        // «первым к финишу пришёл противник» — проигрыш; иначе победил игрок.
        let win = !(lower.contains("противник")
            || lower.contains("соперник")
            || lower.contains("вы проиграли")
            || lower.contains("проиграли вы"));
        return RaceReply::Finish {
            win,
            delta: parse_delta(&lower),
        };
    }

    RaceReply::Unknown
}

/// Дельта рейтинга из итога: «-200», «+1500». Суммы с точками
/// («-1.200.000») отбрасываются — это не рейтинг.
fn parse_delta(text: &str) -> Option<i64> {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i + 1 < chars.len() {
        let sign = chars[i];
        let prev_ok = i == 0 || (!chars[i - 1].is_ascii_digit() && chars[i - 1] != '.');
        if (sign == '-' || sign == '+') && prev_ok {
            let mut digits = String::new();
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_ascii_digit() && digits.len() < 12 {
                digits.push(chars[j]);
                j += 1;
            }
            let dotted = j + 1 < chars.len() && chars[j] == '.' && chars[j + 1].is_ascii_digit();
            if !digits.is_empty() && !dotted {
                if let Ok(n) = digits.parse::<i64>() {
                    if n > 0 {
                        return Some(if sign == '-' { -n } else { n });
                    }
                }
            }
        }
        i += 1;
    }
    None
}

/// Интервал между заездами с разбросом: interval_secs + (0..=interval_jitter_secs).
pub fn interval(cfg: &RaceConfig) -> i64 {
    let base = cfg.interval_secs as i64;
    let jitter = (cfg.interval_jitter_secs as i64).min(base.max(1));
    let secs = if jitter > 0 {
        rand::rng().random_range(base..=base + jitter)
    } else {
        base
    };
    secs.max(1)
}

/// Повтор после молчания бота или нераспознанного ответа.
pub const RETRY_SOON_SECS: i64 = 5 * 60;

#[cfg(test)]
mod tests {
    use super::{parse_reply, RaceReply};

    #[test]
    fn parses_inspection() {
        assert_eq!(
            parse_reply("Nekiplay, машина проходит техосмотр, подождите 9 мин. 48 сек.! 🚧"),
            RaceReply::Inspection(588)
        );
        assert_eq!(
            parse_reply("Подождите 30 секунд"),
            RaceReply::Inspection(30)
        );
        assert_eq!(
            parse_reply("машина проходит техосмотр"),
            RaceReply::Inspection(60)
        );
    }

    #[test]
    fn parses_start_and_finish() {
        let start = "Nekiplay, начался заезд против «Lamborghini Urus»! 🏁\n⏳ Ожидаем прибытие машин к финишу...";
        assert_eq!(parse_reply(start), RaceReply::Started);

        assert_eq!(
            parse_reply("Nekiplay, первым к финишу пришёл противник! -200 🏆"),
            RaceReply::Finish {
                win: false,
                delta: Some(-200)
            }
        );
        assert_eq!(
            parse_reply("Nekiplay, первым к финишу пришли вы! +350 🏆"),
            RaceReply::Finish {
                win: true,
                delta: Some(350)
            }
        );
    }

    #[test]
    fn ignores_money_for_delta() {
        let text = "первым к финишу пришёл противник! -1.200.000 $";
        assert_eq!(
            parse_reply(text),
            RaceReply::Finish {
                win: false,
                delta: None
            }
        );
    }

    #[test]
    fn unknown_reply() {
        assert_eq!(parse_reply("Баланс: 1 000$"), RaceReply::Unknown);
    }
    #[test]
    fn parses_real_win_with_money_and_cups() {
        let text = "Nekiplay, вы пришли первым к финишу! 🏆\n⚙ Улучшайте свой автомобиль чтобы стать быстрее.\n\nВаш выигрыш:\n⠀⠀🏆 +400 кубков\n⠀⠀💰 +5.000.000.000$";
        assert_eq!(
            parse_reply(text),
            RaceReply::Finish {
                win: true,
                delta: Some(400)
            }
        );
    }
}