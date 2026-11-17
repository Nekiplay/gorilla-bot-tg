//! Питомец: расписание «Питомец поход» и подготовка по просьбе бота.

use super::{common, theme, Bot};
use crate::engine::Command;
use gpui_kit::component::v_flex;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, ParentElement, Styled, Window};
/// Страница «Питомец».
pub(super) fn page(bot: &mut Bot, window: &mut Window, cx: &mut Context<Bot>) -> AnyElement {
    let command = bot.field("pet.command", window, cx);
    let feed = bot.field("pet.feed_command", window, cx);
    let interval = bot.field("pet.interval_secs", window, cx);
    let jitter = bot.field("pet.interval_jitter_secs", window, cx);
    let timeout = bot.field("pet.reply_timeout_secs", window, cx);

    let last = bot.shared.last_pet();
    let left = bot.shared.next_pet_in();
    let interval_secs = bot.cfg.pet.interval_secs;
    let jitter_secs = bot.cfg.pet.interval_jitter_secs;
    let enabled = bot.cfg.pet.enabled;
    let feed_when_hungry = bot.cfg.pet.feed_when_hungry;

    let hours = interval_secs / 3600;
    let minutes = interval_secs % 3600 / 60;
    let spread = (jitter_secs / 60).max(1);
    let hint = format!(
        "{hours} ч {minutes} мин +0..{spread} мин ({interval_secs}–{} с)",
        interval_secs + jitter_secs
    );

    let last_line = match last {
        Some(ts) => format!(
            "Последняя отправка: {} ({})",
            common::local_hhmm(ts),
            common::ago(Some(ts))
        ),
        None => "Ещё не отправлялся — уйдёт при старте.".to_string(),
    };
    let next_line = match left {
        Some(left) => format!("Следующий поход через {}", common::fmt_dur(left)),
        None => "Авто-поход выключен".to_string(),
    };

    v_flex()
        .w_full()
        .gap_3()
        .child(common::card_with(
            cx,
            "Питомец",
            v_flex()
                .w_full()
                .gap_2()
                .child(bot.flag("pet.enabled", "Авто «Питомец поход»", enabled, cx))
                .child(bot.flag(
                    "pet.feed_when_hungry",
                    "Выполнять подготовку, которую просит бот (покормить/поиграть)",
                    feed_when_hungry,
                    cx,
                ))
                .child(common::field_row(
                    &*cx,
                    "Команда",
                    common::wide_input(&command),
                ))
                .child(common::field_row(
                    &*cx,
                    "Подготовка",
                    common::wide_input(&feed),
                ))
                .child(common::hint(
                    &*cx,
                    "Команду подготовки бот называет сам в кавычках.",
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
                    bot.primary_action("pet-now", "Отправить сейчас", || Command::PetNow, cx)
                        .w_full(),
                ),
        ))
        .into_any_element()
}
