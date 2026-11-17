//! Профиль игрока из чата с ботом.

use super::{charts, common, theme, Bot};
use crate::engine::Command;
use gpui_kit::component::label::Label;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{px, AnyElement, Context, ParentElement, Styled, Window};

/// Страница «Профиль».
pub(super) fn page(bot: &mut Bot, _window: &mut Window, cx: &mut Context<Bot>) -> AnyElement {
    let profile = bot.shared.profile();
    let last = bot.shared.last_profile();
    let refresh = bot.primary_action("profile-refresh", "Обновить", || Command::ProfileNow, cx);

    let header = h_flex()
        .w_full()
        .items_center()
        .gap_3()
        .child(refresh)
        .child(common::hint(
            &*cx,
            match last {
                Some(ts) => format!("обновлён {}", common::ago(Some(ts))),
                None => "ещё не запрашивали".to_string(),
            },
        ));

    let mut body = v_flex().w_full().gap_2().child(header);

    let Some(profile) = profile else {
        return body
            .child(common::hint(
                &*cx,
                "Запросите профиль у бота кнопкой «Обновить».",
            ))
            .into_any_element();
    };

    let mut badges = h_flex().w_full().gap_2();
    if let Some(clan) = &profile.clan {
        badges = badges.child(common::pill(
            &*cx,
            format!("клан {clan}"),
            theme::accent_light(&*cx),
        ));
    }
    if profile.vip {
        badges = badges.child(common::pill(&*cx, "VIP", theme::warn(&*cx)));
    }
    if let Some(level) = &profile.level {
        badges = badges.child(common::pill(
            &*cx,
            format!("уровень {level}"),
            theme::ok(&*cx),
        ));
    }
    body = body.child(badges);

    for (label, value) in [
        ("ID", profile.id.clone()),
        ("Telegram", profile.telegram.clone()),
        ("Наличными", profile.cash.clone()),
        ("В банке", profile.bank.clone()),
        ("Биткоинов", profile.bitcoins.clone()),
        ("Рейтинг", profile.rating.clone()),
        ("Энергия", profile.energy.clone()),
        ("Опыт", profile.experience.clone()),
        ("GB", profile.gb.clone()),
        ("Регистрация", profile.registered.clone()),
        ("Возраст", profile.age.clone()),
        ("Клан", profile.clan.clone()),
    ] {
        if let Some(value) = value {
            body = body.child(common::row(&*cx, label, value));
        }
    }

    if !profile.property.is_empty() {
        body = body.child(
            Label::new("Имущество")
                .text_size(px(13.))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(theme::text_dim(&*cx)),
        );
        for (label, value) in &profile.property {
            body = body.child(common::row(&*cx, label.clone(), value.clone()));
        }
    }

    let history = bot.shared.history();

    v_flex()
        .w_full()
        .gap_3()
        .child(common::card_with(cx, "Профиль", body))
        .child(charts::cash(&*cx, &history, charts::money_color(&*cx)))
        .into_any_element()
}
