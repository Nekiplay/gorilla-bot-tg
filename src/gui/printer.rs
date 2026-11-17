//! Принтер: расписание «Принтер», заправка и снятие монет.

use super::{common, theme, Bot};
use crate::engine::Command;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, ParentElement, Styled, Window};

/// Страница «Принтер».
pub(super) fn page(bot: &mut Bot, window: &mut Window, cx: &mut Context<Bot>) -> AnyElement {
    let command = bot.field("printer.command", window, cx);
    let refill = bot.field("printer.refill_command", window, cx);
    let withdraw = bot.field("printer.withdraw_command", window, cx);
    let below = bot.field("printer.refill_below_percent", window, cx);
    let interval = bot.field("printer.interval_secs", window, cx);
    let jitter = bot.field("printer.interval_jitter_secs", window, cx);
    let timeout = bot.field("printer.reply_timeout_secs", window, cx);

    let cfg = bot.cfg.printer.clone();
    let last = bot.shared.last_printer();
    let left = bot.shared.next_printer_in();
    let spread = (cfg.interval_jitter_secs / 60).max(1);
    let hint = format!(
        "{} ч {} мин +0..{spread} мин ({}–{} с)",
        cfg.interval_secs / 3600,
        cfg.interval_secs % 3600 / 60,
        cfg.interval_secs,
        cfg.interval_secs + cfg.interval_jitter_secs
    );

    let last_line = match last {
        Some(ts) => format!("Последняя проверка: {}", common::ago(Some(ts))),
        None => "Ещё не проверялся — уйдёт при старте.".to_string(),
    };
    let next_line = match left {
        Some(left) => format!("Следующий принтер через {}", common::fmt_dur(left)),
        None => "Авто-принтер выключен".to_string(),
    };

    v_flex()
        .w_full()
        .gap_3()
        .child(common::card_with(
            cx,
            "Принтер",
            v_flex()
                .w_full()
                .gap_2()
                .child(bot.flag("printer.enabled", "Авто «Принтер»", cfg.enabled, cx))
                .child(bot.flag(
                    "printer.auto_refill",
                    "Заправлять, когда монет мало",
                    cfg.auto_refill,
                    cx,
                ))
                .child(bot.flag(
                    "printer.auto_withdraw",
                    "Снимать монеты в банк",
                    cfg.auto_withdraw,
                    cx,
                ))
                .child(common::field_row(
                    &*cx,
                    "Команда",
                    common::wide_input(&command),
                ))
                .child(common::field_row(
                    &*cx,
                    "Заправка",
                    common::wide_input(&refill),
                ))
                .child(common::field_row(
                    &*cx,
                    "Снятие",
                    common::wide_input(&withdraw),
                ))
                .child(common::field_row(
                    &*cx,
                    "Заправлять при ≤ %",
                    common::wide_input(&below),
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
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(
                            bot.primary_action(
                                "printer-now",
                                "Проверить / обслужить сейчас",
                                || Command::PrinterNow,
                                cx,
                            )
                            .flex_1(),
                        )
                        .child(
                            bot.action(
                                "printer-refill",
                                "Заправить",
                                || Command::PrinterRefillNow,
                                cx,
                            )
                            .flex_1(),
                        )
                        .child(
                            bot.action(
                                "printer-withdraw",
                                "Снять",
                                || Command::PrinterWithdrawNow,
                                cx,
                            )
                            .flex_1(),
                        ),
                ),
        ))
        .into_any_element()
}
