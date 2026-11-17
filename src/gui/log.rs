//! Журнал событий движка.

use super::{common, theme, Bot};
use gpui_kit::component::label::Label;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{div, px, AnyElement, Context, ParentElement, Styled};

/// Сколько строк журнала показываем.
const VISIBLE: usize = 400;

/// Тело журнала — используется и в правой панели.
pub(super) fn page_body(
    shared: &std::sync::Arc<crate::engine::Shared>,
    cx: &mut Context<Bot>,
) -> AnyElement {
    let logs = shared.logs();
    let total = logs.len();
    let start = total.saturating_sub(VISIBLE);

    let mut column = v_flex().w_full().gap_0p5();
    for line in &logs[start..] {
        column = column.child(
            Label::new(line.clone())
                .text_size(px(12.))
                .text_color(line_color(&*cx, line)),
        );
    }

    v_flex()
        .w_full()
        .h_full()
        .gap_2()
        .child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .child(common::card_title(&*cx, "Журнал"))
                .child(
                    Label::new(format!("{total} строк"))
                        .text_size(px(12.))
                        .text_color(theme::muted(&*cx)),
                ),
        )
        .child(
            div()
                .id("log-scroll")
                .flex_1()
                .min_h(px(140.))
                .w_full()
                .overflow_y_scrollbar()
                .child(column),
        )
        .into_any_element()
}

/// Цвет строки по ключевым словам.
fn line_color(cx: &gpui_kit::App, line: &str) -> gpui_kit::Hsla {
    let lower = line.to_lowercase();
    if lower.contains("ошибка") || lower.contains("не удалось") {
        theme::danger(cx)
    } else if lower.contains("выигрыш") || lower.contains("выиграли") {
        theme::ok(cx)
    } else if lower.contains("проигрыш") {
        theme::warn(cx)
    } else {
        theme::text_dim(cx)
    }
}
