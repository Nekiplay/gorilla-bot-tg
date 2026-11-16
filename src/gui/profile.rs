use eframe::egui::{self, *};

use super::App;
use super::common::*;
use super::overview::status_card;
use crate::engine::{Command, Status};

/// Профиль игрока (ответ бота на «Профиль») и статистика казино.
pub(super) fn profile_page(app: &mut App, ui: &mut egui::Ui, status: &Status) {
    let profile = app.shared.profile();
    let last = app.shared.last_profile();

    card(ui, "Профиль", |ui| {
        ui.horizontal(|ui| {
            if ui
                .add(
                    Button::new(RichText::new("Обновить").size(13.0).color(Color32::WHITE))
                        .fill(ACCENT)
                        .corner_radius(4)
                        .min_size(Vec2::new(0.0, 28.0)),
                )
                .clicked()
            {
                let _ = app.tx.send(Command::ProfileNow);
            }
            match last {
                Some(ts) => hint(ui, &format!("обновлён {}", ago(ts))),
                None => hint(ui, "ещё не запрашивали"),
            }
        });
        ui.add_space(8.0);

        let Some(p) = profile.as_ref() else {
            hint(ui, "Запросите профиль у бота кнопкой «Обновить».");
            return;
        };

        ui.horizontal(|ui| {
            if let Some(clan) = &p.clan {
                pill(ui, &format!("клан {clan}"), ACCENT_LIGHT);
            }
            if p.vip {
                pill(ui, "VIP", WARN);
            }
            if let Some(level) = &p.level {
                pill(ui, &format!("уровень {level}"), OK);
            }
        });
        ui.add_space(8.0);

        let fields: [(&str, &Option<String>); 12] = [
            ("ID", &p.id),
            ("Telegram", &p.telegram),
            ("Наличными", &p.cash),
            ("В банке", &p.bank),
            ("Биткоинов", &p.bitcoins),
            ("Рейтинг", &p.rating),
            ("Энергия", &p.energy),
            ("Опыт", &p.experience),
            ("GB", &p.gb),
            ("Регистрация", &p.registered),
            ("Возраст", &p.age),
            ("Клан", &p.clan),
        ];
        for (label, value) in fields {
            if let Some(value) = value {
                row(ui, label, |ui| {
                    ui.label(RichText::new(value).size(13.5).color(TEXT));
                });
            }
        }

        if !p.property.is_empty() {
            ui.add_space(6.0);
            ui.label(RichText::new("Имущество").size(13.0).strong().color(TEXT_DIM));
            for (key, value) in &p.property {
                row(ui, key, |ui| {
                    ui.label(RichText::new(value).size(13.0).color(TEXT));
                });
            }
        }
    });

    ui.add_space(12.0);
    status_card(app, ui, status);
}