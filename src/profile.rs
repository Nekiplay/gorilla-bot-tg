//! Разбор ответа бота на команду «Профиль»: только те поля,
//! которые бот реально присылает в тексте.

use std::collections::BTreeMap;

/// Профиль игрока, полученный из чата с ботом.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profile {
    pub id: Option<String>,
    pub telegram: Option<String>,
    pub vip: bool,
    pub clan: Option<String>,
    pub level: Option<String>,
    pub cash: Option<String>,
    pub bank: Option<String>,
    pub bitcoins: Option<String>,
    pub rating: Option<String>,
    pub energy: Option<String>,
    pub experience: Option<String>,
    pub gb: Option<String>,
    /// Имущество: «Машина» → «Lamborghini Urus [Е282ТУ 777]» и т.п.
    pub property: BTreeMap<String, String>,
    pub registered: Option<String>,
    pub age: Option<String>,
}

impl Profile {
    /// Удалось ли вытащить хоть что-то осмысленное.
    pub fn is_empty(&self) -> bool {
        self.id.is_none()
            && self.level.is_none()
            && self.cash.is_none()
            && self.property.is_empty()
            && self.registered.is_none()
    }
}

/// Ключи, которые показываем в интерфейсе (порядок как у бота).
const PROPERTY_KEYS: [&str; 10] = [
    "Машина",
    "Яхта",
    "Самолет",
    "Вертолет",
    "Телефон",
    "Компьютер",
    "Питомец",
    "Ферма",
    "Денежный принтер",
    "Колонизация Марса",
];

fn clean(line: &str) -> &str {
    line.trim_matches(|c: char| c.is_whitespace() || c == '\u{2800}')
        .trim()
}

/// Разбирает текст ответа бота. Возвращает `None`, если это не профиль.
pub fn parse(text: &str) -> Option<Profile> {
    if !text.to_lowercase().contains("профиль") && !text.contains("ID:") {
        return None;
    }

    let mut profile = Profile::default();
    let mut in_property = false;

    for raw in text.lines() {
        let line = clean(raw);
        if line.is_empty() {
            continue;
        }

        let lower = line.to_lowercase();
        if lower.contains("имущество") {
            in_property = true;
            continue;
        }
        if lower.contains("дата регистрации") {
            in_property = false;
        }

        if lower.contains("vip игрок") {
            profile.vip = true;
            continue;
        }

        // «💸 58.226 GB» приходит без двоеточия.
        if !line.contains(':') {
            if lower.contains(" gb") {
                let value = line.trim_start_matches(|c: char| !c.is_ascii_digit());
                if !value.is_empty() {
                    profile.gb = Some(value.to_string());
                }
            }
            continue;
        }

        // Эмодзи перед ключом отбрасываем: берём хвост после него.
        let (raw_key, raw_value) = line.split_once(':').unwrap();
        let value = raw_value.trim().trim_matches('\u{2800}').trim();
        if value.is_empty() {
            continue;
        }
        let key = raw_key
            .trim_matches(|c: char| !c.is_alphanumeric() && c != ' ')
            .trim()
            .to_string();
        if key.is_empty() {
            continue;
        }

        if in_property {
            for known in PROPERTY_KEYS {
                if key.ends_with(known) || key == known {
                    profile
                        .property
                        .insert(known.to_string(), value.to_string());
                    break;
                }
            }
            continue;
        }

        let value = Some(value.to_string());
        match key.as_str() {
            "ID" => profile.id = value,
            "Привязан Telegram" => profile.telegram = value,
            "Клан" => profile.clan = value,
            "Уровень" => profile.level = value,
            "Наличными" => profile.cash = value,
            "В банке" => profile.bank = value,
            "Биткоинов" => profile.bitcoins = value,
            "Рейтинг" => profile.rating = value,
            "Энергия" => profile.energy = value,
            "Опыт" => profile.experience = value,
            "Дата регистрации" => profile.registered = value,
            "Возраст" => profile.age = value,
            _ => {
                if key.ends_with("GB") {
                    profile.gb = value;
                }
            }
        }
    }

    if profile.is_empty() {
        return None;
    }
    Some(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Nekiplay, твой профиль:\n🔎 ID: 403684\n✈️ Привязан Telegram\n🔥 VIP игрок\n⚔️ Клан: Rich\n💠 Уровень: 4\n💰 Наличными: 659.776.857.684$\n💳 В банке: 0$\n💽 Биткоинов: 0,00฿\n👑 Рейтинг: 0\n🏋️ Энергия: 44\n🏆 Опыт: 214.374\n💸 58.226 GB\n\n🔑 Имущество:\n⠀🚗 Машина: Lamborghini Urus [Е282ТУ 777]\n⠀🛥 Яхта: Памперс\n⠀🛩 Самолет: B-2 Spirit Stealth Bomber\n⠀🚁 Вертолет: Bell V-22 Osprey\n⠀📱 Телефон: iPhone 12 Pro\n⠀🖥 Компьютер: Микроволновка\n⠀🐇 Питомец: Пасхальный кролик\n⠀🔋 Ферма: GigaWatt (2000 шт.)\n⠀🖨 Денежный принтер: HP Color LaserJet\n⠀💼 Колонизация Марса\n\n📚 Дата регистрации: 18.03.2019, 13:48:11\n👦🏾 Возраст: 7 лет. 7 мес. 28 дней.";

    #[test]
    fn parses_profile_fields() {
        let p = parse(SAMPLE).expect("профиль распознан");
        assert_eq!(p.id.as_deref(), Some("403684"));
        assert!(p.vip);
        assert_eq!(p.clan.as_deref(), Some("Rich"));
        assert_eq!(p.level.as_deref(), Some("4"));
        assert_eq!(p.cash.as_deref(), Some("659.776.857.684$"));
        assert_eq!(p.bank.as_deref(), Some("0$"));
        assert_eq!(p.bitcoins.as_deref(), Some("0,00฿"));
        assert_eq!(p.rating.as_deref(), Some("0"));
        assert_eq!(p.energy.as_deref(), Some("44"));
        assert_eq!(p.experience.as_deref(), Some("214.374"));
        assert_eq!(p.gb.as_deref(), Some("58.226 GB"));
        assert_eq!(
            p.property.get("Машина").map(String::as_str),
            Some("Lamborghini Urus [Е282ТУ 777]")
        );
        assert_eq!(
            p.property.get("Денежный принтер").map(String::as_str),
            Some("HP Color LaserJet")
        );
        assert_eq!(
            p.registered.as_deref(),
            Some("18.03.2019, 13:48:11")
        );
        assert!(p.age.is_some());
    }

    #[test]
    fn ignores_plain_messages() {
        assert!(parse("Машина противника:\n⛽ Максимальная скорость: 494").is_none());
    }
}