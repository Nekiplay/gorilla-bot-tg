//! Обзор: таймеры автокоманд и состояние казино.

use super::{charts, common, theme, Bot};
use crate::engine::BonusKind;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{div, px, AnyElement, Context, ParentElement, Styled, Window};

/// Страница «Обзор».
pub(super) fn page(bot: &mut Bot, window: &mut Window, cx: &mut Context<Bot>) -> AnyElement {
    let cfg = bot.cfg.clone();
    let shared = bot.shared.clone();

    // Кольца таймеров: в строку, при нехватке места — следующими рядами.
    let cols = common::columns(window, 168.);
    let rows = common::rows(
        vec![
            common::timer_ring(
                &*cx,
                "Питомец",
                cfg.pet.enabled,
                shared.next_pet_in(),
                cfg.pet.interval_secs,
                shared.last_pet(),
                theme::ok(&*cx),
            ),
            common::timer_ring(
                &*cx,
                "Принтер",
                cfg.printer.enabled,
                shared.next_printer_in(),
                cfg.printer.interval_secs,
                shared.last_printer(),
                theme::warn(&*cx),
            ),
            common::timer_ring(
                &*cx,
                "Гонка",
                cfg.race.enabled,
                shared.next_race_in(),
                cfg.race.interval_secs,
                shared.last_race(),
                theme::accent_light(&*cx),
            ),
            common::timer_ring(
                &*cx,
                "Бизнес",
                cfg.business.enabled,
                shared.next_business_in(),
                cfg.business.interval_secs,
                shared.last_business(),
                theme::ok(&*cx),
            ),
            common::timer_ring(
                &*cx,
                "Бонус в канал",
                cfg.bonus.enabled,
                shared.next_bonus_in(BonusKind::Channel),
                cfg.bonus.interval_secs,
                shared.last_bonus(BonusKind::Channel),
                theme::accent(&*cx),
            ),
            common::timer_ring(
                &*cx,
                "Бонус в личку",
                cfg.bonus_dm.enabled,
                shared.next_bonus_in(BonusKind::Dm),
                cfg.bonus_dm.interval_secs,
                shared.last_bonus(BonusKind::Dm),
                theme::text_dim(&*cx),
            ),
        ],
        cols,
    );

    v_flex()
        .w_full()
        .gap_3()
        .child(common::card_with(
            cx,
            "Обзор",
            v_flex().w_full().gap_3().child(rows).child(common::hint(
                &*cx,
                "Сколько осталось до отправки каждой команды.",
            )),
        ))
        .child(status_card(bot, cx))
        .child(history_charts(bot, cx))
        .into_any_element()
}

/// Графики сейфов и капчи из истории.
fn history_charts(bot: &Bot, cx: &mut Context<Bot>) -> AnyElement {
    let history = bot.shared.history();

    v_flex()
        .w_full()
        .gap_3()
        .child(charts::safe_rate(&*cx, &history, theme::accent_light(&*cx)))
        .child(charts::captcha_rate(&*cx, &history, theme::ok(&*cx)))
        .child(common::hint(&*cx, history_note(&history)))
        .into_any_element()
}

/// Карточка «Статус» с показателями казино.
fn status_card(bot: &Bot, cx: &mut Context<Bot>) -> AnyElement {
    let status = bot.shared.status();
    let badge = if status.casino_running {
        common::pill(&*cx, "казино идёт", theme::warn(&*cx))
    } else {
        common::pill(&*cx, "простой", theme::muted(&*cx))
    };

    let stats = common::rows(
        vec![
            common::stat(&*cx, "Раунды", status.rounds.to_string(), theme::text(&*cx)),
            common::stat(&*cx, "Победы", status.wins.to_string(), theme::ok(&*cx)),
            common::stat(
                &*cx,
                "Проигрыши подряд",
                status.losses_in_row.to_string(),
                if status.losses_in_row > 0 {
                    theme::warn(&*cx)
                } else {
                    theme::text(&*cx)
                },
            ),
            common::stat(
                &*cx,
                "Ставка",
                status.bet.to_string(),
                theme::accent_light(&*cx),
            ),
        ],
        4,
    );

    let mut card = v_flex()
        .w_full()
        .gap_2()
        .child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .child(common::card_title(&*cx, "Статус"))
                .child(badge),
        )
        .child(stats);

    if !status.message.is_empty() {
        card = card.child(common::hint(&*cx, status.message));
    }
    if !status.last_bot_reply.is_empty() {
        card = card.child(
            div()
                .w_full()
                .p_2()
                .rounded(px(6.))
                .bg(theme::field(&*cx))
                .child(common::hint(
                    &*cx,
                    format!("бот: {}", status.last_bot_reply),
                )),
        );
    }

    card.into_any_element()
}
/// Сводка по истории: сколько замеров и с какого времени.
fn history_note(history: &crate::history::History) -> String {
    if history.is_empty() {
        return "история ещё пуста: первый замер появится через минуту".to_string();
    }
    let Some(first) = history.first() else {
        return String::new();
    };
    let (rounds, wins, safes, captcha) = history.progress();
    format!(
        "замеров: {} (с {}), за это время: раундов +{}, побед +{}, сейфов {}, капч {}",
        history.len(),
        common::local_hhmm(first.ts),
        rounds,
        wins,
        safes,
        captcha,
    )
}
