use eframe::egui::{self, Button, Color32, RichText};

use super::common::*;
use super::App;
use crate::engine::{Command, LoginStage};

pub(super) fn telegram_card(app: &mut App, ui: &mut egui::Ui) {
    account_card(app, ui);
    settings_card(app, ui);
}

/// Вход в Telegram: код и пароль 2FA вводятся здесь, а не в консоли.
fn account_card(app: &mut App, ui: &mut egui::Ui) {
    let stage = app.shared.login_stage();
    let authorized = app.shared.status().authorized || matches!(stage, LoginStage::Done(_));

    card(ui, "Вход в Telegram", |ui| {
        if authorized && matches!(stage, LoginStage::Idle) {
            pill(ui, "● сессия активна", OK);
            hint(ui, "Вход уже выполнен. Сессия лежит в папке tdata рядом с exe.");
            return;
        }

        match &stage {
            LoginStage::Idle | LoginStage::Failed(_) => {
                row(ui, "Телефон", |ui| {
                    wide_edit(ui, &mut app.cfg.phone, "+79991234567");
                });
                if let LoginStage::Failed(text) = &stage {
                    ui.label(RichText::new(text).size(12.0).color(WARN));
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if accent_button(ui, "Отправить код").clicked() {
                        app.save();
                        let phone = app.cfg.phone.clone();
                        let _ = app.tx.send(Command::LoginRequest { phone });
                    }
                    hint(ui, "Код придёт в Telegram (или SMS).");
                });
            }
            LoginStage::AwaitingCode => {
                row(ui, "Код из Telegram", |ui| {
                    wide_edit(ui, &mut app.code_input, "12345");
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if accent_button(ui, "Войти").clicked() {
                        let code = app.code_input.clone();
                        let _ = app.tx.send(Command::LoginCode { code });
                    }
                    if ui.button("Отмена").clicked() {
                        app.code_input.clear();
                        let _ = app.tx.send(Command::LoginCancel);
                    }
                });
            }
            LoginStage::AwaitingPassword(hint_text) => {
                row(ui, "Пароль 2FA", |ui| {
                    let width = ui.available_width().max(90.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut app.password_input)
                            .password(true)
                            .hint_text("облачный пароль")
                            .desired_width(width),
                    );
                });
                if !hint_text.is_empty() {
                    hint(ui, &format!("Подсказка от Telegram: {hint_text}"));
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if accent_button(ui, "Подтвердить").clicked() {
                        let password = app.password_input.clone();
                        let _ = app.tx.send(Command::LoginPassword { password });
                    }
                    if ui.button("Отмена").clicked() {
                        app.password_input.clear();
                        let _ = app.tx.send(Command::LoginCancel);
                    }
                });
            }
            LoginStage::Done(text) => {
                pill(ui, "● сессия активна", OK);
                ui.label(RichText::new(text).size(12.5).color(TEXT));
            }
        }
    });
}

/// Настройки приложения: API-ключи, бот, ник, файл сессии.
fn settings_card(app: &mut App, ui: &mut egui::Ui) {
    card(ui, "Telegram", |ui| {
        row(ui, "API ID", |ui| {
            ui.add(egui::DragValue::new(&mut app.cfg.api_id).speed(1.0));
        });
        row(ui, "API hash", |ui| {
            wide_edit(ui, &mut app.cfg.api_hash, "0123456789abcdef");
        });
        row(ui, "Телефон", |ui| {
            wide_edit(ui, &mut app.cfg.phone, "+79991234567");
        });
        row(ui, "Бот", |ui| {
            wide_edit(ui, &mut app.cfg.bot_username, "realgorillabot");
        });
        row(ui, "Ник игрока", |ui| {
            wide_edit(ui, &mut app.cfg.player_name, "");
        });
        hint(
            ui,
            "По нику проверяем, что ответ бота адресован вам, а не другому игроку.",
        );
        row(ui, "Файл сессии", |ui| {
            wide_edit(ui, &mut app.cfg.session_file, "gorilla.session");
        });
        hint(ui, "Лежит в папке tdata рядом с exe.");
    });
}

fn accent_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        Button::new(RichText::new(text).color(Color32::WHITE))
            .fill(ACCENT)
            .min_size(egui::Vec2::new(0.0, 26.0)),
    )
}