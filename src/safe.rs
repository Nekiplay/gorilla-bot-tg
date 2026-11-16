//! Сейф Bot Gorilla: подбор двухзначного кода.
//!
//! Бот принимает команду вида «Сейф 42», попытки не ограничены, приз до
//! 50.000.000$. Пароль после неудачи бот называет открытым текстом:
//! «вы не угадали пароль к сейфу, пароль был «40»» — это не помогает, потому
//! что следующий сейф получает новый код, поэтому просто перебираем.

use std::collections::VecDeque;

/// Диапазон кодов сейфа: 1..=99 (ноль бот не принимает).
pub const MIN_CODE: u32 = 1;
pub const MAX_CODE: u32 = 99;

/// Заменяет `{code}` в шаблоне команды на код.
pub fn command(template: &str, code: u32) -> String {
    template.replace("{code}", &code.to_string())
}

/// Очередь перемешанных кодов: каждый сейф — один код, порядок случайный,
/// но без повторов, пока не кончились все 99.
pub struct Bag {
    left: VecDeque<u32>,
}

impl Bag {
    pub fn new() -> Self {
        let mut bag = Bag {
            left: VecDeque::new(),
        };
        bag.refill(None);
        bag
    }

    fn refill(&mut self, skip: Option<u32>) {
        let mut order: Vec<u32> = (MIN_CODE..=MAX_CODE).collect();
        let mut state = seed();
        for i in (1..order.len()).rev() {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            let j = (state >> 33) as usize % (i + 1);
            order.swap(i, j);
        }
        if let Some(skip) = skip {
            order.retain(|code| *code != skip);
        }
        self.left = order.into();
    }

    /// Следующий код; `previous` — код прошлой попытки (чтобы не повторить).
    pub fn next(&mut self, previous: Option<u32>) -> u32 {
        if self.left.is_empty() {
            self.refill(previous);
        }
        let code = self.left.pop_front().unwrap_or(0);
        if Some(code) == previous {
            self.left.push_back(code);
            return self.next(previous);
        }
        code
    }
}

impl Default for Bag {
    fn default() -> Self {
        Self::new()
    }
}

fn seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x2545_f491_4f6c_dd1d);
    nanos | 1
}

/// Что бот ответил на попытку.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Угадали: в сумме — сколько нашли в сейфе.
    Win(u64),
    /// Не угадали: в поле — названный ботом пароль (если разобрали).
    Fail(Option<u32>),
    /// Ответ не про сейф (кулдаун, ошибка и т. п.).
    Other,
}

/// Разбирает текст ответа бота.
pub fn parse(text: &str) -> Outcome {
    let lower = text.to_lowercase();
    if lower.contains("не угадали пароль") || lower.contains("пароль был") {
        return Outcome::Fail(quoted_code(text));
    }
    if lower.contains("угадали пароль") || lower.contains("нашли в сейфе") {
        return Outcome::Win(amount(text).unwrap_or(0));
    }
    Outcome::Other
}

/// Сумма после «в сейфе»: «✅ Вы нашли в сейфе 17.902.076.393$».
fn amount(text: &str) -> Option<u64> {
    let start = text.find("сейфе")? + "сейфе".len();
    let digits: String = text[start..]
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',' || *c == ' ')
        .filter(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// Пароль в кавычках-ёлочках: «пароль был «40»».
fn quoted_code(text: &str) -> Option<u32> {
    let open = text.find('«')? + '«'.len_utf8();
    let close = text[open..].find('»')? + open;
    text[open..close].trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win_is_parsed() {
        let text = "Nekiplay, вы угадали пароль к сейфу!\n✅ Вы нашли в сейфе 17.902.076.393$";
        assert_eq!(parse(text), Outcome::Win(17_902_076_393));
    }

    #[test]
    fn fail_is_parsed() {
        let text = "Nekiplay, вы не угадали пароль к сейфу, пароль был «40». Не отчаивайтесь, попытки не ограничены, но если Вы угадаете, получите до 50.000.000$ 🙂";
        assert_eq!(parse(text), Outcome::Fail(Some(40)));
    }

    #[test]
    fn other_is_parsed() {
        assert_eq!(parse("Кулдаун, попробуйте через 10 секунд"), Outcome::Other);
    }

    #[test]
    fn command_substitutes_code() {
        assert_eq!(command("Сейф {code}", 7), "Сейф 7");
        assert_eq!(command("Сейф {code}", 42), "Сейф 42");
    }

    #[test]
    fn bag_gives_each_code_once() {
        let mut bag = Bag::new();
        let mut seen = Vec::new();
        for _ in 0..(MAX_CODE - MIN_CODE + 1) {
            let code = bag.next(None);
            assert!(!seen.contains(&code), "код {code} повторился");
            assert!((MIN_CODE..=MAX_CODE).contains(&code), "код {code} вне 1..=99");
            seen.push(code);
        }
        assert_eq!(seen.len() as u32, MAX_CODE - MIN_CODE + 1);
    }
}