use std::time::Duration;

use eframe::egui::{self, *};

use super::App;
use super::common::*;
use crate::engine::Command;

    /// «Папка» со всеми авто-командами: питомец, принтер и два таймера бонуса.
    /// Автоматический заезд: «Гонка» → техосмотр → ожидание финиша.
    pub(super) fn race_card(app: &mut App, ui: &mut egui::Ui, left: Option<Duration>, last_race: Option<i64>) {
        card(ui, "Гонка", |ui| {
            ui.checkbox(
                &mut app.cfg.race.enabled,
                RichText::new("Авто «Гонка» (заезд)").size(13.0).color(TEXT),
            );

            row(ui, "Команда", |ui| {
                wide_edit(ui, &mut app.cfg.race.command, "Гонка");
            });
            row(ui, "Интервал, с", |ui| {
                ui.add(
                    egui::DragValue::new(&mut app.cfg.race.interval_secs)
                        .speed(60.0)
                        .range(60..=86_400),
                );
            });
            row(ui, "Разброс, с", |ui| {
                ui.add(
                    egui::DragValue::new(&mut app.cfg.race.interval_jitter_secs)
                        .speed(10.0)
                        .range(0..=3_600),
                );
            });
            row(ui, "Ждать ответ, с", |ui| {
                ui.add(egui::DragValue::new(&mut app.cfg.race.reply_timeout_secs).range(5..=300));
            });
            row(ui, "Ждать финиш, с", |ui| {
                ui.add(egui::DragValue::new(&mut app.cfg.race.result_timeout_secs).range(30..=1_200));
            });
            row(ui, "Запас после ТО, с", |ui| {
                ui.add(egui::DragValue::new(&mut app.cfg.race.cooldown_margin_secs).range(0..=120));
            });

            let interval = app.cfg.race.interval_secs;
            let jitter = app.cfg.race.interval_jitter_secs.min(interval);
            hint(
                ui,
                &format!(
                    "{} мин ± {} мин ({}–{} с)",
                    interval / 60,
                    jitter / 60,
                    interval.saturating_sub(jitter),
                    interval + jitter
                ),
            );

            ui.add_space(4.0);
            match last_race {
                Some(ts) => hint(ui, &format!("Последний заезд: {} ({})", local_hhmm(ts), ago(ts))),
                None => hint(ui, "Ещё не ездили — заезд уйдёт при старте."),
            }
            match left {
                Some(d) => {
                    ui.label(
                        RichText::new(format!("Следующий заезд через {}", fmt_dur(d)))
                            .color(ACCENT_LIGHT),
                    );
                }
                None => hint(ui, "Авто-гонка выключена"),
            }

            ui.add_space(4.0);
            if ui
                .add(
                    Button::new("Отправить сейчас")
                        .min_size(Vec2::new(ui.available_width(), 26.0)),
                )
                .clicked()
            {
                let _ = app.tx.send(Command::RaceNow);
            }
        });
    }
