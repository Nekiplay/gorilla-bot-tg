use std::time::Duration;

use eframe::egui::{self, *};

use super::App;
use super::common::*;
use crate::engine::Command;

    /// Автоснятие дохода с бизнеса: «Бизнес снять» по расписанию.
    pub(super) fn business_card(app: &mut App, ui: &mut egui::Ui, left: Option<Duration>, last_business: Option<i64>) {
        card(ui, "Бизнес", |ui| {
            ui.checkbox(
                &mut app.cfg.business.enabled,
                RichText::new("Авто «Бизнес снять»").size(13.0).color(TEXT),
            );

            row(ui, "Команда", |ui| {
                wide_edit(ui, &mut app.cfg.business.command, "Бизнес снять");
            });
            row(ui, "Интервал, с", |ui| {
                ui.add(
                    egui::DragValue::new(&mut app.cfg.business.interval_secs)
                        .speed(60.0)
                        .range(60..=86_400),
                );
            });
            row(ui, "Разброс, с", |ui| {
                ui.add(
                    egui::DragValue::new(&mut app.cfg.business.interval_jitter_secs)
                        .speed(10.0)
                        .range(0..=3_600),
                );
            });
            row(ui, "Ждать ответ, с", |ui| {
                ui.add(egui::DragValue::new(&mut app.cfg.business.reply_timeout_secs).range(5..=300));
            });

            let interval = app.cfg.business.interval_secs;
            let jitter = app.cfg.business.interval_jitter_secs.min(interval);
            hint(
                ui,
                &format!(
                    "{} ч {} мин +0..{} мин",
                    interval / 3_600,
                    (interval % 3_600) / 60,
                    jitter / 60
                ),
            );

            ui.add_space(4.0);
            match last_business {
                Some(ts) => hint(ui, &format!("Последнее снятие: {} ({})", local_hhmm(ts), ago(ts))),
                None => hint(ui, "Ещё не снимали — команда уйдёт при старте."),
            }
            match left {
                Some(d) => {
                    ui.label(
                        RichText::new(format!("Следующее снятие через {}", fmt_dur(d)))
                            .color(ACCENT_LIGHT),
                    );
                }
                None => hint(ui, "Автоснятие выключено"),
            }

            ui.add_space(4.0);
            if ui
                .add(
                    Button::new("Снять сейчас")
                        .min_size(Vec2::new(ui.available_width(), 26.0)),
                )
                .clicked()
            {
                let _ = app.tx.send(Command::BusinessNow);
            }
        });
    }