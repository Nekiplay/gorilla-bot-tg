use std::time::Duration;

use eframe::egui::{self, *};

use super::App;
use super::common::*;
use crate::engine::Command;

    pub(super) fn printer_card(
        app: &mut App,
        ui: &mut egui::Ui,
        left: Option<Duration>,
        last_printer: Option<i64>,
    ) {
        card(ui, "Принтер", |ui| {
            ui.checkbox(
                &mut app.cfg.printer.enabled,
                RichText::new("Авто «Принтер» по расписанию").size(13.0).color(TEXT),
            );
            ui.checkbox(
                &mut app.cfg.printer.auto_refill,
                RichText::new("Автозаправка («Принтер заправить»)").size(13.0).color(TEXT),
            );
            ui.checkbox(
                &mut app.cfg.printer.auto_withdraw,
                RichText::new("Автоснятие денег («Принтер снять»)").size(13.0).color(TEXT),
            );

            row(ui, "Команда", |ui| {
                wide_edit(ui, &mut app.cfg.printer.command, "Принтер");
            });
            row(ui, "Заправка", |ui| {
                wide_edit(ui, &mut app.cfg.printer.refill_command, "Принтер заправить");
            });
            row(ui, "Снятие", |ui| {
                wide_edit(ui, &mut app.cfg.printer.withdraw_command, "Принтер снять");
            });
            row(ui, "Заправлять при ≤, %", |ui| {
                ui.add(
                    egui::DragValue::new(&mut app.cfg.printer.refill_below_percent)
                        .speed(1.0)
                        .range(1..=100),
                );
            });
            row(ui, "Интервал, с", |ui| {
                ui.add(
                    egui::DragValue::new(&mut app.cfg.printer.interval_secs)
                        .speed(60.0)
                        .range(60..=86_400),
                );
            });
            row(ui, "Разброс, с", |ui| {
                ui.add(
                    egui::DragValue::new(&mut app.cfg.printer.interval_jitter_secs)
                        .speed(10.0)
                        .range(0..=3_600),
                );
            });
            row(ui, "Ждать ответ, с", |ui| {
                ui.add(
                    egui::DragValue::new(&mut app.cfg.printer.reply_timeout_secs).range(5..=300),
                );
            });

            let interval = app.cfg.printer.interval_secs;
            let jitter = app.cfg.printer.interval_jitter_secs.min(interval);
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
            match last_printer {
                Some(ts) => hint(
                    ui,
                    &format!("Последняя проверка: {} ({})", local_hhmm(ts), ago(ts)),
                ),
                None => hint(ui, "Ещё не проверялся — уйдёт при старте."),
            }
            match left {
                Some(d) => {
                    ui.label(
                        RichText::new(format!("Следующий принтер через {}", fmt_dur(d)))
                            .color(ACCENT_LIGHT),
                    );
                }
                None => hint(ui, "Авто-принтер выключен"),
            }

            ui.add_space(4.0);
            if ui
                .add(
                    Button::new("Проверить / обслужить сейчас")
                        .min_size(Vec2::new(ui.available_width(), 26.0)),
                )
                .clicked()
            {
                let _ = app.tx.send(Command::PrinterNow);
            }
            ui.horizontal(|ui| {
                let half = ((ui.available_width() - 9.0) / 2.0).max(60.0);
                if ui
                    .add(Button::new("Заправить").min_size(Vec2::new(half, 26.0)))
                    .clicked()
                {
                    let _ = app.tx.send(Command::PrinterRefillNow);
                }
                if ui
                    .add(Button::new("Снять").min_size(Vec2::new(half, 26.0)))
                    .clicked()
                {
                    let _ = app.tx.send(Command::PrinterWithdrawNow);
                }
            });
        });
    }
