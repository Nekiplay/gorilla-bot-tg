
use eframe::egui::{self, *};

use super::App;
use super::common::*;
use crate::engine::Command;

    pub(super) fn casino_card(app: &mut App, ui: &mut egui::Ui, running: bool) {
        card(ui, "Казино", |ui| {
            let (label, fill, cmd) = if running {
                ("Стоп казино", DANGER, Command::StopCasino)
            } else {
                ("Старт казино", OK, Command::StartCasino)
            };
            if ui
                .add(
                    Button::new(RichText::new(label).size(15.0).strong().color(Color32::WHITE))
                        .fill(fill)
                        .min_size(Vec2::new(ui.available_width(), 34.0)),
                )
                .clicked()
            {
                let _ = app.tx.send(cmd);
            }
            hint(
                ui,
                if running {
                    "идут ставки; поздний ответ бота всё равно будет учтён"
                } else {
                    "запускает мартингейл в этом чате"
                },
            );
            ui.add_space(2.0);
            row(ui, "Команда", |ui| {
                wide_edit(ui, &mut app.cfg.casino.command, "Казино {bet}");
            });
            row(ui, "Начальная ставка", |ui| {
                ui.add(
                    egui::DragValue::new(&mut app.cfg.casino.start_bet)
                        .speed(10_000.0)
                        .range(1..=u64::MAX),
                );
            });
            row(ui, "Потолок ставки", |ui| {
                ui.add(
                    egui::DragValue::new(&mut app.cfg.casino.max_bet)
                        .speed(100_000.0)
                        .range(1..=u64::MAX),
                );
            });
            row(ui, "Округление", |ui| {
                ui.add(
                    egui::DragValue::new(&mut app.cfg.casino.bet_round_to)
                        .speed(100.0)
                        .range(0..=1_000_000),
                );
            });
            row(ui, "Множитель", |ui| {
                ui.add(
                    egui::DragValue::new(&mut app.cfg.casino.multiplier)
                        .speed(0.05)
                        .range(1.0..=5.0)
                        .max_decimals(2),
                );
            });
            row(ui, "Разброс множителя", |ui| {
                ui.add(
                    egui::DragValue::new(&mut app.cfg.casino.multiplier_jitter)
                        .speed(0.01)
                        .range(0.0..=0.9)
                        .max_decimals(2),
                );
            });
            row(ui, "Проигрышей подряд", |ui| {
                ui.add(
                    egui::DragValue::new(&mut app.cfg.casino.max_losses_in_row).range(1..=50),
                );
            });
            row(ui, "Пауза, с", |ui| {
                ui.add(egui::DragValue::new(&mut app.cfg.casino.round_gap_secs).range(0..=3600));
            });
            row(ui, "Разброс паузы, с", |ui| {
                ui.add(
                    egui::DragValue::new(&mut app.cfg.casino.round_gap_jitter_secs)
                        .range(0..=60),
                );
            });
            row(ui, "Ждать ответ, с", |ui| {
                ui.add(
                    egui::DragValue::new(&mut app.cfg.casino.reply_timeout_secs).range(5..=600),
                );
            });
            row(ui, "Запас кулдауна, с", |ui| {
                ui.add(
                    egui::DragValue::new(&mut app.cfg.casino.cooldown_margin_secs).range(0..=600),
                );
            });

            let base = app.cfg.casino.round_gap_secs;
            let jitter = app.cfg.casino.round_gap_jitter_secs.min(base);
            hint(
                ui,
                &format!(
                    "ставка растёт в {:.2}× ± {:.0}%, пауза {}–{} с",
                    app.cfg.casino.multiplier,
                    app.cfg.casino.multiplier_jitter * 100.0,
                    base.saturating_sub(jitter),
                    base + jitter
                ),
            );
        });
    }
