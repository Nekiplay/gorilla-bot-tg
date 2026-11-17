//! Telegram: вход по коду из Telegram и настройки подключения.

use super::{common, theme, Bot};
use crate::engine::{Command, LoginStage};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, ClickEvent, Context, ParentElement, Styled, Window};

/// Страница «Telegram».
pub(super) fn page(bot: &mut Bot, window: &mut Window, cx: &mut Context<Bot>) -> AnyElement {
    let api_id = bot.field("api_id", window, cx);
    let api_hash = bot.field("api_hash", window, cx);
    let phone = bot.field("phone", window, cx);
    let bot_username = bot.field("bot_username", window, cx);
    let session_file = bot.field("session_file", window, cx);
    let player_name = bot.field("player_name", window, cx);

    let account = account_card(bot, window, cx);

    v_flex()
        .w_full()
        .gap_3()
        .child(common::card_with(cx, "Аккаунт", account))
        .child(common::card_with(
            cx,
            "Telegram",
            v_flex()
                .w_full()
                .gap_2()
                .child(common::field_row(
                    &*cx,
                    "API ID",
                    common::wide_input(&api_id),
                ))
                .child(common::field_row(
                    &*cx,
                    "API hash",
                    common::wide_input(&api_hash),
                ))
                .child(common::field_row(
                    &*cx,
                    "Телефон",
                    common::wide_input(&phone),
                ))
                .child(common::field_row(
                    &*cx,
                    "Бот",
                    common::wide_input(&bot_username),
                ))
                .child(common::field_row(
                    &*cx,
                    "Ник игрока",
                    common::wide_input(&player_name),
                ))
                .child(common::hint(
                    &*cx,
                    "По нику проверяем, что ответ бота адресован вам, а не другому игроку.",
                ))
                .child(common::field_row(
                    &*cx,
                    "Файл сессии",
                    common::wide_input(&session_file),
                ))
                .child(common::hint(&*cx, "Лежит в папке tdata рядом с exe.")),
        ))
        .into_any_element()
}

/// Блок состояния входа.
fn account_card(bot: &mut Bot, window: &mut Window, cx: &mut Context<Bot>) -> AnyElement {
    let stage = bot.shared.login_stage();
    match stage {
        LoginStage::Idle | LoginStage::Failed(_) => {
            let phone = bot.field("phone", window, cx);
            let start = login_button(cx);

            let mut column = v_flex()
                .w_full()
                .gap_2()
                .child(common::hint(&*cx, "Код придёт в Telegram (или SMS)."));
            if let LoginStage::Failed(reason) = &stage {
                column =
                    column.child(common::hint(&*cx, reason.clone()).text_color(theme::warn(&*cx)));
            }
            column
                .child(common::field_row(
                    &*cx,
                    "Телефон",
                    common::wide_input(&phone),
                ))
                .child(h_flex().w_full().child(start))
                .into_any_element()
        }
        LoginStage::AwaitingCode => {
            let code = bot.field("login.code", window, cx);
            v_flex()
                .w_full()
                .gap_2()
                .child(common::hint(
                    &*cx,
                    "Код уже отправлен. Если в логе «уходит в приложение Telegram» — искать его в чате «Telegram» (777000), а не в SMS.",
                ))
                .child(common::field_row(
                    &*cx,
                    "Код из Telegram",
                    common::wide_input(&code),
                ))
                .child(
                    h_flex()
                        .gap_2()
                        .child(login_code_button(cx))
                        .child(cancel_button(cx)),
                )
                .into_any_element()
        }
        LoginStage::AwaitingPassword(hint_text) => {
            let password = bot.field("login.password", window, cx);
            v_flex()
                .w_full()
                .gap_2()
                .child(common::hint(
                    &*cx,
                    format!("Подсказка от Telegram: {hint_text}"),
                ))
                .child(common::field_row(
                    &*cx,
                    "Пароль 2FA",
                    common::wide_input(&password),
                ))
                .child(
                    h_flex()
                        .gap_2()
                        .child(password_button(cx))
                        .child(cancel_button(cx)),
                )
                .into_any_element()
        }
        LoginStage::Done(who) => v_flex()
            .w_full()
            .gap_2()
            .child(common::pill(&*cx, "● сессия активна", theme::ok(&*cx)))
            .child(common::hint(&*cx, format!("Вошли как {who}.")))
            .child(common::hint(
                &*cx,
                "Сессия лежит в папке tdata рядом с exe.",
            ))
            .into_any_element(),
    }
}

fn login_button(cx: &Context<Bot>) -> Button {
    let this = cx.entity().downgrade();
    Button::new("login-request")
        .label("Войти по коду")
        .on_click(move |_: &ClickEvent, _window, cx| {
            let _ = this.update(cx, |this: &mut Bot, cx| {
                this.save();
                let phone = this.value("phone");
                this.send(Command::LoginRequest { phone });
                cx.notify();
            });
        })
}

fn login_code_button(cx: &Context<Bot>) -> Button {
    let this = cx.entity().downgrade();
    Button::new("login-code").label("Войти").primary().on_click(
        move |_: &ClickEvent, _window, cx| {
            let _ = this.update(cx, |this: &mut Bot, cx| {
                let code = this.field_text("login.code", cx);
                this.send(Command::LoginCode { code });
                cx.notify();
            });
        },
    )
}

fn password_button(cx: &Context<Bot>) -> Button {
    let this = cx.entity().downgrade();
    Button::new("login-password")
        .label("Подтвердить")
        .primary()
        .on_click(move |_: &ClickEvent, _window, cx| {
            let _ = this.update(cx, |this: &mut Bot, cx| {
                let password = this.field_text("login.password", cx);
                this.send(Command::LoginPassword { password });
                cx.notify();
            });
        })
}

fn cancel_button(cx: &Context<Bot>) -> Button {
    let this = cx.entity().downgrade();
    Button::new("login-cancel")
        .label("Отмена")
        .on_click(move |_: &ClickEvent, _window, cx| {
            let _ = this.update(cx, |this: &mut Bot, cx| {
                this.send(Command::LoginCancel);
                cx.notify();
            });
        })
}
