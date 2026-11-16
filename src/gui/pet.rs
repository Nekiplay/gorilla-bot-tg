use std::time::Duration;

use eframe::egui::{self, *};

use super::App;
use super::common::*;
use crate::engine::Command;

    pub(super) fn pet_card(app: &mut App, ui: &mut egui::Ui, left: Option<Duration>, last_pet: Option<i64>) {
        card(ui, "Питомец", |ui| {
            ui.checkbox(
                &mut app.cfg.pet.enabled,
                RichText::new("Авто «Питомец поход»").size(13.0).color(TEXT),
            );
            ui.checkbox(
                &mut app.cfg.pet.feed_when_hungry,
                RichText::new("Выполнять подготовку, которую просит бот (покормить/поиграть)")
                    .size(13.0)
                    .color(TEXT),
            );

            row(ui, "Команда", |ui| {
                wide_edit(ui, &mut app.cfg.pet.command, "Питомец поход");
            });
            row(ui, "Подготовка", |ui| {
                wide_edit(ui, &mut app.cfg.pet.feed_command, "Питомец покормить");
            });
            hint(
                ui,
                "Команду подготовки бот называет сам в кавычках — она берётся из ответа.",
            );
            row(ui, "Интервал, с", |ui| {
                ui.add(
                    egui::DragValue::new(&mut app.cfg.pet.interval_secs)
                        .speed(60.0)
                        .range(60..=86_400),
                );
            });
            row(ui, "Разброс, с", |ui| {
                ui.add(
                    egui::DragValue::new(&mut app.cfg.pet.interval_jitter_secs)
                        .speed(10.0)
                        .range(0..=3_600),
                );
            });
            row(ui, "Ждать ответ, с", |ui| {
                ui.add(egui::DragValue::new(&mut app.cfg.pet.reply_timeout_secs).range(5..=300));
            });

            let interval = app.cfg.pet.interval_secs;
            let jitter = app.cfg.pet.interval_jitter_secs.min(interval);
            hint(
                ui,
                &format!(
                    "{} ч {} мин ± {} мин ({}–{} с)",
                    interval / 3600,
                    interval % 3600 / 60,
                    jitter / 60,
                    interval.saturating_sub(jitter),
                    interval + jitter
                ),
            );

            ui.add_space(4.0);
            match last_pet {
                Some(ts) => hint(ui, &format!("Последняя отправка: {} ({})", local_hhmm(ts), ago(ts))),
                None => hint(ui, "Ещё не отправлялся — уйдёт при старте."),
            }
            match left {
                Some(d) => {
                    ui.label(
                        RichText::new(format!("Следующий поход через {}", fmt_dur(d)))
                            .color(ACCENT_LIGHT),
                    );
                }
                None => hint(ui, "Авто-поход выключен"),
            }

            ui.add_space(4.0);
            if ui
                .add(
                    Button::new("Отправить сейчас")
                        .min_size(Vec2::new(ui.available_width(), 26.0)),
                )
                .clicked()
            {
                let _ = app.tx.send(Command::PetNow);
            }
        });
    }
