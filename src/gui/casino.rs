//! Казино: мартингейл и перебор сейфов (две отдельные вкладки).

use super::{charts, common, theme, Bot};
use crate::engine::Command;
use gpui_kit::component::label::Label;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{px, AnyElement, Context, ParentElement, Styled, Window};

/// Страница «Казино»: только мартингейл.
pub(super) fn page(bot: &mut Bot, window: &mut Window, cx: &mut Context<Bot>) -> AnyElement {
    let status = bot.shared.status();

    v_flex()
        .w_full()
        .gap_3()
        .child(casino_card(bot, window, cx, status.casino_running))
        .child(status_card(bot, cx))
        .child(history_charts(bot, cx))
        .into_any_element()
}

/// Страница «Сейфы»: только перебор кодов.
pub(super) fn safe_page(bot: &mut Bot, window: &mut Window, cx: &mut Context<Bot>) -> AnyElement {
    let status = bot.shared.status();

    v_flex()
        .w_full()
        .gap_3()
        .child(safe_card(bot, window, cx, status.safe_running))
        .child(safe_charts(bot, cx))
        .into_any_element()
}

/// График вскрытий сейфов.
fn safe_charts(bot: &Bot, cx: &mut Context<Bot>) -> AnyElement {
    let history = bot.shared.history();

    v_flex()
        .w_full()
        .gap_3()
        .child(charts::safe_rate(&*cx, &history, theme::accent_light(&*cx)))
        .into_any_element()
}

/// Графики винрейта и ставки из истории.
fn history_charts(bot: &Bot, cx: &mut Context<Bot>) -> AnyElement {
    let history = bot.shared.history();

    v_flex()
        .w_full()
        .gap_3()
        .child(charts::casino_win_rate(
            &*cx,
            &history,
            theme::accent_light(&*cx),
        ))
        .child(charts::casino_bet(&*cx, &history, theme::ok(&*cx)))
        .into_any_element()
}

/// Карточка мартингейла.
fn casino_card(
    bot: &mut Bot,
    window: &mut Window,
    cx: &mut Context<Bot>,
    running: bool,
) -> AnyElement {
    let toggle = if running {
        bot.action("casino-stop", "Стоп казино", || Command::StopCasino, cx)
    } else {
        bot.primary_action("casino-start", "Старт казино", || Command::StartCasino, cx)
    };
    let hint = if running {
        "идут ставки; поздний ответ бота всё равно будет учтён"
    } else {
        "запускает мартингейл в этом чате"
    };

    let fields = [
        ("casino.command", "Команда"),
        ("casino.start_bet", "Начальная ставка"),
        ("casino.max_bet", "Потолок ставки"),
        ("casino.bet_round_to", "Округление"),
        ("casino.multiplier", "Множитель"),
        ("casino.multiplier_jitter", "Разброс множителя"),
        ("casino.max_losses_in_row", "Проигрышей подряд"),
        ("casino.round_gap_secs", "Пауза, с"),
        ("casino.round_gap_jitter_secs", "Разброс паузы, с"),
        ("casino.reply_timeout_secs", "Ждать ответ, с"),
        ("casino.cooldown_margin_secs", "Запас кулдауна, с"),
    ];

    let mut body = v_flex()
        .w_full()
        .gap_2()
        .child(toggle.w_full())
        .child(common::hint(&*cx, hint));
    for (key, label) in fields {
        let field = bot.field(key, window, cx);
        body = body.child(common::field_row(&*cx, label, common::wide_input(&field)));
    }
    body = body.child(common::hint(
        &*cx,
        format!(
            "ставка растёт в {mult}× ± {jitter}%, пауза {base}+0..{top} с",
            mult = bot.cfg.casino.multiplier,
            jitter = bot.cfg.casino.multiplier_jitter * 100.0,
            base = bot.cfg.casino.round_gap_secs,
            top = bot.cfg.casino.round_gap_secs + bot.cfg.casino.round_gap_jitter_secs,
        ),
    ));

    common::card_with(cx, "Казино", body).into_any_element()
}

/// Карточка перебора кодов сейфа.
fn safe_card(
    bot: &mut Bot,
    window: &mut Window,
    cx: &mut Context<Bot>,
    running: bool,
) -> AnyElement {
    let toggle = if running {
        bot.action("safe-stop", "Стоп сейф", || Command::StopSafe, cx)
    } else {
        bot.primary_action("safe-start", "Старт сейф", || Command::StartSafe, cx)
    };

    let status = bot.shared.status();
    let command = bot.field("safe.command", window, cx);
    let delay = bot.field("safe.delay_secs", window, cx);

    let fixed = bot.cfg.safe.code.is_some();
    let checkbox = bot.flag("safe-fixed", "слать всегда", fixed, cx);
    let code = bot.field("safe.code", window, cx);

    let codes = h_flex()
        .w_full()
        .items_center()
        .gap_3()
        .child(
            v_flex()
                .w(px(190.))
                .flex_none()
                .gap_1()
                .child(
                    Label::new("Свой код")
                        .text_size(px(13.))
                        .text_color(theme::muted(&*cx)),
                )
                .child(checkbox),
        )
        .when(fixed, |el| el.child(common::wide_input(&code)));

    common::card_with(
        cx,
        "Сейф",
        v_flex()
            .w_full()
            .gap_2()
            .child(toggle.w_full())
            .child(common::hint(
                &*cx,
                "перебирает коды 1..99 без повторов; попытки бот не ограничивает",
            ))
            .child(common::field_row(
                &*cx,
                "Команда",
                common::wide_input(&command),
            ))
            .child(codes)
            .child(common::field_row(
                &*cx,
                "Пауза, с",
                common::wide_input(&delay),
            ))
            .child(common::row(&*cx, "Код", status.safe_code.to_string()))
            .child(common::row(
                &*cx,
                "Попыток",
                status.safe_attempts.to_string(),
            ))
            .child(common::row(&*cx, "Вскрыто", status.safe_wins.to_string()))
            .child(common::row(
                &*cx,
                "Найдено",
                format!("{} $", status.safe_found),
            )),
    )
    .into_any_element()
}

/// Карточка состояния мартингейла.
fn status_card(bot: &Bot, cx: &mut Context<Bot>) -> AnyElement {
    let status = bot.shared.status();

    let chips = common::rows(
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
    let mut body = v_flex().w_full().gap_2().child(chips);

    if !status.message.is_empty() {
        body = body.child(common::hint(&*cx, status.message));
    }

    common::card_with(cx, "Состояние", body).into_any_element()
}
