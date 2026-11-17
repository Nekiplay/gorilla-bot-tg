//! Прокси: SOCKS5, Cloudflare-воркер и локальный MTProxy.

use super::{common, Bot};
use gpui_kit::component::v_flex;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, ParentElement, Styled, Window};

/// Страница «Прокси».
pub(super) fn page(bot: &mut Bot, window: &mut Window, cx: &mut Context<Bot>) -> AnyElement {
    let socks5 = bot.field("proxy", window, cx);
    let worker = bot.field("mtproxy.worker", window, cx);
    let host = bot.field("mtproxy.host", window, cx);
    let port = bot.field("mtproxy.port", window, cx);
    let secret = bot.field("mtproxy.secret", window, cx);

    v_flex()
        .w_full()
        .gap_3()
        .child(common::card_with(
            cx,
            "Прокси",
            v_flex()
                .w_full()
                .gap_2()
                .child(common::field_row(
                    &*cx,
                    "SOCKS5",
                    common::wide_input(&socks5),
                ))
                .child(common::hint(
                    &*cx,
                    "Пустое поле — без прокси. Формат: socks5://user:pass@host:port",
                ))
                .child(common::field_row(
                    &*cx,
                    "Worker",
                    common::wide_input(&worker),
                ))
                .child(common::field_row(
                    &*cx,
                    "MTProxy host",
                    common::wide_input(&host),
                ))
                .child(common::field_row(
                    &*cx,
                    "MTProxy port",
                    common::wide_input(&port),
                ))
                .child(common::field_row(
                    &*cx,
                    "MTProxy secret",
                    common::wide_input(&secret),
                ))
                .child(common::hint(
                    &*cx,
                    "Воркер Cloudflare поднимает MTProxy рядом с ботом. Секрет — из вывода воркера.",
                )),
        ))
        .into_any_element()
}
