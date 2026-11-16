use std::time::Duration;

use eframe::egui::{self, *};

use super::App;
use super::common::*;
use crate::engine::{BonusKind, Command};

    pub(super) fn bonus_card(
        app: &mut App,
        ui: &mut egui::Ui,
        kind: BonusKind,
        left: Option<Duration>,
        last_bonus: Option<i64>,
    ) {
        let (title, target_hint) = match kind {
            BonusKind::Channel => ("Бонус в канал", "gorillabot_double"),
            BonusKind::Dm => ("Бонус в личку", "realgorillabot"),
        };
        let mut send_now = false;
        {
            let bonus = match kind {
                BonusKind::Channel => &mut app.cfg.bonus,
                BonusKind::Dm => &mut app.cfg.bonus_dm,
            };
            card(ui, title, |ui| {
                ui.checkbox(
                    &mut bonus.enabled,
                    RichText::new("Слать «Бонус» по расписанию")
                        .size(13.0)
                        .color(TEXT),
                );

                row(ui, "Чат / канал", |ui| {
                    wide_edit(ui, &mut bonus.target, target_hint);
                });
                row(ui, "Команда", |ui| {
                    wide_edit(ui, &mut bonus.command, "Бонус");
                });
                row(ui, "Разброс, мин", |ui| {
                    let mut minutes = (bonus.jitter_secs / 60) as i64;
                    if ui
                        .add(egui::DragValue::new(&mut minutes).speed(1.0).range(0..=120))
                        .changed()
                    {
                        bonus.jitter_secs = (minutes.max(0) as u64) * 60;
                    }
                });

                hint(
                    ui,
                    "Срок ищется в любом ответе бота: «12ч», «6 мин. 29 сек.», «через 3 часа».",
                );
                hint(
                    ui,
                    &format!(
                        "Если бот не назвал срок — запасные {} мин. Разброс ±{} мин.",
                        bonus.interval_secs / 60,
                        bonus.jitter_secs / 60
                    ),
                );

                ui.add_space(4.0);
                match last_bonus {
                    Some(ts) => hint(
                        ui,
                        &format!("Последняя отправка: {} ({})", local_hhmm(ts), ago(ts)),
                    ),
                    None => hint(ui, "Ещё не отправлялся — уйдёт при старте."),
                }
                match left {
                    Some(d) => {
                        ui.label(
                            RichText::new(format!("Следующий бонус через {}", fmt_dur(d)))
                                .color(ACCENT_LIGHT),
                        );
                    }
                    None => hint(ui, "Автоотправка выключена"),
                }

                ui.add_space(4.0);
                if ui
                    .add(
                        Button::new("Отправить сейчас")
                            .min_size(Vec2::new(ui.available_width(), 26.0)),
                    )
                    .clicked()
                {
                    send_now = true;
                }
            });
        }
        if send_now {
            let cmd = match kind {
                BonusKind::Channel => Command::BonusNow,
                BonusKind::Dm => Command::BonusDmNow,
            };
            let _ = app.tx.send(cmd);
        }
    }
