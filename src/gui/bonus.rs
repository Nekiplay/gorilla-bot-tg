//! Бонус: рассылка «Бонус» в канал и в личку боту.

use super::{common, theme, Bot};
use crate::engine::{BonusKind, Command};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, ParentElement, Styled, Window};

/// Страница «Бонус».
pub(super) fn page(bot: &mut Bot, window: &mut Window, cx: &mut Context<Bot>) -> AnyElement {
    v_flex()
        .w_full()
        .gap_3()
        .child(bonus_card(bot, window, cx, BonusKind::Channel))
        .child(bonus_card(bot, window, cx, BonusKind::Dm))
        .into_any_element()
}

/// Карточка одного бонуса.
fn bonus_card(
    bot: &mut Bot,
    window: &mut Window,
    cx: &mut Context<Bot>,
    kind: BonusKind,
) -> AnyElement {
    let (title, target_hint) = match kind {
        BonusKind::Channel => ("Бонус в канал", "gorillabot_double"),
        BonusKind::Dm => ("Бонус в личку", "realgorillabot"),
    };
    let target_key = match kind {
        BonusKind::Channel => "bonus.target",
        BonusKind::Dm => "bonus_dm.target",
    };
    let command_key = match kind {
        BonusKind::Channel => "bonus.command",
        BonusKind::Dm => "bonus_dm.command",
    };
    let jitter_key = match kind {
        BonusKind::Channel => "bonus.jitter_secs",
        BonusKind::Dm => "bonus_dm.jitter_secs",
    };
    let flag_key = match kind {
        BonusKind::Channel => "bonus.enabled",
        BonusKind::Dm => "bonus_dm.enabled",
    };

    let target = bot.field(target_key, window, cx);
    let command = bot.field(command_key, window, cx);
    let jitter = bot.field(jitter_key, window, cx);

    let (enabled, interval_secs, jitter_secs) = match kind {
        BonusKind::Channel => (
            bot.cfg.bonus.enabled,
            bot.cfg.bonus.interval_secs,
            bot.cfg.bonus.jitter_secs,
        ),
        BonusKind::Dm => (
            bot.cfg.bonus_dm.enabled,
            bot.cfg.bonus_dm.interval_secs,
            bot.cfg.bonus_dm.jitter_secs,
        ),
    };
    let last = bot.shared.last_bonus(kind);
    let left = bot.shared.next_bonus_in(kind);
    let hint = format!(
        "Если бот не назвал срок — запасные {} мин. Разброс +0..{} мин.",
        interval_secs / 60,
        (jitter_secs / 60).max(1)
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
        Some(left) => format!("Следующий бонус через {}", common::fmt_dur(left)),
        None => "Автоотправка выключена".to_string(),
    };
    let start_id: &'static str = match kind {
        BonusKind::Channel => "bonus-now",
        BonusKind::Dm => "bonus-dm-now",
    };
    let start_label = "Отправить сейчас";

    common::card_with(
        cx,
        title,
        v_flex()
            .w_full()
            .gap_2()
            .child(bot.flag(flag_key, "Слать «Бонус» по расписанию", enabled, cx))
            .child(common::field_row(
                &*cx,
                "Чат / канал",
                common::wide_input(&target),
            ))
            .child(common::hint(
                &*cx,
                format!("Цель по умолчанию: {target_hint}"),
            ))
            .child(common::field_row(
                &*cx,
                "Команда",
                common::wide_input(&command),
            ))
            .child(common::field_row(
                &*cx,
                "Разброс, мин",
                common::wide_input(&jitter),
            ))
            .child(common::hint(
                &*cx,
                "Срок ищется в любом ответе бота: «12ч», «6 мин. 29 сек.», «через 3 часа».",
            ))
            .child(common::hint(&*cx, hint))
            .child(common::hint(&*cx, last_line))
            .child(common::hint(&*cx, next_line).text_color(if left.is_some() {
                theme::accent_light(&*cx)
            } else {
                theme::muted(&*cx)
            }))
            .child(
                h_flex().w_full().child(
                    bot.primary_action(
                        start_id,
                        start_label,
                        move || match kind {
                            BonusKind::Channel => Command::BonusNow,
                            BonusKind::Dm => Command::BonusDmNow,
                        },
                        cx,
                    )
                    .w_full(),
                ),
            ),
    )
    .into_any_element()
}
