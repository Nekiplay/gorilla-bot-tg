//! Гонка: расписание «Гонка» и ожидание финиша.

use super::{common, theme, Bot};
use crate::engine::Command;
use gpui_kit::component::v_flex;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, ParentElement, Styled, Window};

/// Страница «Гонка».
pub(super) fn page(bot: &mut Bot, window: &mut Window, cx: &mut Context<Bot>) -> AnyElement {
    let command = bot.field("race.command", window, cx);
    let interval = bot.field("race.interval_secs", window, cx);
    let jitter = bot.field("race.interval_jitter_secs", window, cx);
    let timeout = bot.field("race.reply_timeout_secs", window, cx);
    let finish = bot.field("race.result_timeout_secs", window, cx);
    let margin = bot.field("race.cooldown_margin_secs", window, cx);

    let cfg = bot.cfg.race.clone();
    let last = bot.shared.last_race();
    let left = bot.shared.next_race_in();
    let hint = format!(
        "{} мин ± {} мин ({}–{} с)",
        cfg.interval_secs / 60,
        cfg.interval_jitter_secs / 60,
        cfg.interval_secs.saturating_sub(cfg.interval_jitter_secs),
        cfg.interval_secs + cfg.interval_jitter_secs
    );

    let last_line = match last {
        Some(ts) => format!("Последний заезд: {}", common::ago(Some(ts))),
        None => "Ещё не ехали — заезд уйдёт при старте.".to_string(),
    };
    let next_line = match left {
        Some(left) => format!("Следующий заезд через {}", common::fmt_dur(left)),
        None => "Авто-гонка выключена".to_string(),
    };

    v_flex()
        .w_full()
        .gap_3()
        .child(common::card_with(
            cx,
            "Гонка",
            v_flex()
                .w_full()
                .gap_2()
                .child(bot.flag("race.enabled", "Авто «Гонка» (заезд)", cfg.enabled, cx))
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
                .child(common::field_row(
                    &*cx,
                    "Ждать финиш, с",
                    common::wide_input(&finish),
                ))
                .child(common::field_row(
                    &*cx,
                    "Запас после ТО, с",
                    common::wide_input(&margin),
                ))
                .child(common::hint(&*cx, hint))
                .child(common::hint(&*cx, last_line))
                .child(common::hint(&*cx, next_line).text_color(if left.is_some() {
                    theme::accent_light(&*cx)
                } else {
                    theme::muted(&*cx)
                }))
                .child(
                    bot.primary_action("race-now", "Отправить сейчас", || Command::RaceNow, cx)
                        .w_full(),
                ),
        ))
        .into_any_element()
}
