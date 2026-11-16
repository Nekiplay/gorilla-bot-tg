
use eframe::egui::{self, *};

use super::App;
use super::common::*;
use crate::engine::{BonusKind, Status};

/// Главная: круговые таймеры до следующей отправки по каждой команде.
pub(super) fn overview(app: &mut App, ui: &mut egui::Ui, status: &Status) {
        let rows = [
            ("Питомец", app.cfg.pet.enabled, app.shared.next_pet_in(), app.cfg.pet.interval_secs, app.shared.last_pet(), OK),
            ("Принтер", app.cfg.printer.enabled, app.shared.next_printer_in(), app.cfg.printer.interval_secs, app.shared.last_printer(), WARN),
            ("Гонка", app.cfg.race.enabled, app.shared.next_race_in(), app.cfg.race.interval_secs, app.shared.last_race(), ACCENT_LIGHT),
            ("Бизнес", app.cfg.business.enabled, app.shared.next_business_in(), app.cfg.business.interval_secs, app.shared.last_business(), OK),
            ("Бонус", app.cfg.bonus.enabled, app.shared.next_bonus_in(BonusKind::Channel), app.cfg.bonus.interval_secs, app.shared.last_bonus(BonusKind::Channel), ACCENT),
            ("Бонус в ЛС", app.cfg.bonus_dm.enabled, app.shared.next_bonus_in(BonusKind::Dm), app.cfg.bonus_dm.interval_secs, app.shared.last_bonus(BonusKind::Dm), TEXT_DIM),
        ];

        ui.horizontal(|ui| {
            ui.label(RichText::new("Обзор").size(24.0).strong().color(TEXT));
            ui.add_space(6.0);
            let (label, color) = if status.connected {
                ("● онлайн", OK)
            } else {
                ("● офлайн", MUTED)
            };
            pill(ui, label, color);
            if status.casino_running {
                pill(ui, "казино идёт", WARN);
            }
        });
        hint(ui, "Сколько осталось до отправки каждой команды. Настройки — во вкладках сверху.");
        ui.add_space(10.0);

        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(18.0, 18.0);
            for (title, enabled, left, total, last, color) in rows {
                timer_ring(ui, title, enabled, left, total, last, color);
            }
        });

    }

    pub(super) fn status_card(_app: &mut App, ui: &mut egui::Ui, status: &Status) {
        Frame::new()
            .fill(CARD)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(6)
            .inner_margin(Margin::same(14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Статус").strong().size(14.5).color(TEXT));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let (text, color) = if status.casino_running {
                            ("казино идёт", WARN)
                        } else {
                            ("простой", MUTED)
                        };
                        ui.label(RichText::new(text).size(12.0).color(color));
                    });
                });

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    stat(ui, "Раунды", &status.rounds.to_string(), TEXT);
                    stat(ui, "Победы", &status.wins.to_string(), OK);
                    stat(
                        ui,
                        "Проигрыши подряд",
                        &status.losses_in_row.to_string(),
                        if status.losses_in_row > 0 { WARN } else { TEXT },
                    );
                    stat(ui, "Ставка", &status.bet.to_string(), ACCENT_LIGHT);
                });

                if !status.message.is_empty() {
                    ui.add_space(7.0);
                    ui.label(RichText::new(&status.message).size(13.0).color(MUTED));
                }
                if !status.last_bot_reply.is_empty() {
                    ui.add_space(7.0);
                    Frame::new()
                        .fill(FIELD)
                        .corner_radius(4)
                        .inner_margin(Margin::symmetric(10, 7))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(format!("бот: {}", status.last_bot_reply))
                                    .size(12.5)
                                    .color(TEXT_DIM),
                            );
                        });
                }
            });
    }
