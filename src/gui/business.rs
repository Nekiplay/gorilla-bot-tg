//! Бизнес: автоснятие дохода командой «Бизнес снять».

use super::{common, theme, Bot};
use crate::engine::Command;
use gpui_kit::component::v_flex;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, ParentElement, Styled, Window};

/// Страница «Бизнес».
pub(super) fn page(bot: &mut Bot, window: &mut Window, cx: &mut Context<Bot>) -> AnyElement {
    let command = bot.field("business.command", window, cx);
    let interval = bot.field("business.interval_secs", window, cx);
    let jitter = bot.field("business.interval_jitter_secs", window, cx);
    let timeout = bot.field("business.reply_timeout_secs", window, cx);

    let cfg = bot.cfg.business.clone();
    let last = bot.shared.last_business();
    let left = bot.shared.next_business_in();
    let spread = (cfg.interval_jitter_secs / 60).max(1);
    let hint = format!(
        "{} ч {} мин +0..{spread} мин",
        cfg.interval_secs / 3600,
        cfg.interval_secs % 3600 / 60
    );

    let last_line = match last {
        Some(ts) => format!("Последнее снятие: {}", common::ago(Some(ts))),
        None => "Ещё не снимали — команда уйдёт при старте.".to_string(),
    };
    let next_line = match left {
        Some(left) => format!("Следующее снятие через {}", common::fmt_dur(left)),
        None => "Автоснятие выключено".to_string(),
    };

    v_flex()
        .w_full()
        .gap_3()
        .child(common::card_with(
            cx,
            "Бизнес",
            v_flex()
                .w_full()
                .gap_2()
                .child(bot.flag("business.enabled", "Авто «Бизнес снять»", cfg.enabled, cx))
                .child(common::field_row(
                    &*cx,
                    "Команда",
                    common::wide_input(&command),
                ))
                .child(common::field_row(
                    &*cx,
                    "Интервал, с",
                    common::wide_input(&interval),
                ))
                .child(common::field_row(
                    &*cx,
                    "Разброс, с",
                    common::wide_input(&jitter),
                ))
                .child(common::field_row(
                    &*cx,
                    "Ждать ответ, с",
                    common::wide_input(&timeout),
                ))
                .child(common::hint(&*cx, hint))
                .child(common::hint(&*cx, last_line))
                .child(common::hint(&*cx, next_line).text_color(if left.is_some() {
                    theme::accent_light(&*cx)
                } else {
                    theme::muted(&*cx)
                }))
                .child(
                    bot.primary_action("business-now", "Снять сейчас", || Command::BusinessNow, cx)
                        .w_full(),
                ),
        ))
        .into_any_element()
}
