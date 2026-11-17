//! Общие кусочки страниц: карточка, подпись, строка «ключ — значение»,
//! пилюля состояния и круговой таймер.

use std::time::Duration;

use super::theme;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::label::Label;
use gpui_kit::component::progress::ProgressCircle;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::component::{Sizable, Size};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, px, App, Div, Entity, FontWeight, Hsla, IntoElement, SharedString, Styled, Window,
};

/// Карточка: фон группы, мягкая граница, скругление.
pub fn card(cx: &App) -> Div {
    v_flex()
        .w_full()
        .gap_2()
        .p_4()
        .rounded(px(8.))
        .bg(theme::card(cx))
        .border_1()
        .border_color(theme::border(cx))
        // В карточке фиксированные высоты (графики, поля): сжиматься нельзя,
        // иначе на маленьком окне их режет, а страница не прокручивается.
        .flex_shrink_0()
}

/// Карточка с заголовком и готовым содержимым.
pub fn card_with(cx: &App, title: impl Into<SharedString>, body: impl IntoElement) -> Div {
    card(cx).child(card_title(cx, title)).child(body)
}
/// Заголовок карточки.
pub fn card_title(cx: &App, text: impl Into<SharedString>) -> Div {
    v_flex().w_full().pb_1().child(
        Label::new(text.into())
            .text_size(px(14.5))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme::text(cx)),
    )
}

/// Пояснение под заголовком.
pub fn hint(cx: &App, text: impl Into<SharedString>) -> Div {
    div().w_full().child(
        Label::new(text.into())
            .text_size(px(12.))
            .text_color(theme::muted(cx)),
    )
}

/// Строка «ключ — значение»: слева приглушённая подпись, справа текст.
pub fn row(cx: &App, label: impl Into<SharedString>, value: impl Into<SharedString>) -> Div {
    h_flex()
        .w_full()
        .gap_3()
        .items_baseline()
        .child(
            div().w(px(190.)).flex_none().child(
                Label::new(label.into())
                    .text_size(px(13.))
                    .text_color(theme::muted(cx)),
            ),
        )
        .child(
            div().flex_1().min_w_0().child(
                Label::new(value.into())
                    .text_size(px(13.))
                    .text_color(theme::text(cx)),
            ),
        )
}

/// Крупное число с подписью: для метрик казино и обзора.
pub fn stat(
    cx: &App,
    label: impl Into<SharedString>,
    value: impl Into<SharedString>,
    color: Hsla,
) -> Div {
    v_flex()
        .gap_0p5()
        .px_3()
        .py_2()
        .rounded(px(6.))
        .bg(theme::field(cx))
        .child(
            Label::new(value.into())
                .text_size(px(19.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(color),
        )
        .child(
            Label::new(label.into())
                .text_size(px(11.5))
                .text_color(theme::muted(cx)),
        )
}

/// Пилюля состояния: подкрашенный фон и цветной текст.
pub fn pill(_cx: &App, text: impl Into<SharedString>, color: Hsla) -> Div {
    div()
        .px_2()
        .py_0p5()
        .rounded(px(999.))
        .bg(color.opacity(0.14))
        .child(
            Label::new(text.into())
                .text_size(px(11.5))
                .text_color(color),
        )
}

/// Остаток времени как `чч:мм:сс` (`мм:сс`, если меньше часа).
pub fn fmt_dur(left: Duration) -> String {
    let total = left.as_secs();
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// Время суток `чч:мм` по местному часовому поясу.
pub fn local_hhmm(unix: i64) -> String {
    let local = time::OffsetDateTime::from_unix_timestamp(unix)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
        .to_offset(local_offset());
    format!("{:02}:{:02}", local.hour(), local.minute())
}

/// Подпись часа: `14:00` — графики строятся по часовым замерам.
pub fn local_hour(unix: i64) -> String {
    let local = time::OffsetDateTime::from_unix_timestamp(unix)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
        .to_offset(local_offset());
    format!("{:02}:00", local.hour())
}

/// Смещение местного пояса от UTC.
fn local_offset() -> time::UtcOffset {
    time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC)
}

/// Когда отправляли в прошлый раз: `был 14:02` или `ещё не было`.
pub fn ago(unix: Option<i64>) -> String {
    match unix {
        Some(unix) => format!("был {}", local_hhmm(unix)),
        None => "ещё не было".into(),
    }
}

/// Круговой таймер команды: кольцо прогресса, подпись и остаток.
/// Раскладывает элементы по строкам вручную: `flex_wrap` в GPUI считает высоту
/// контейнера как одну строку, поэтому перенос делаем сами — тогда высота
/// страницы верная и прокрутка доходит до низа.
pub fn rows<T: IntoElement>(cells: Vec<T>, per_row: usize) -> Div {
    let per_row = per_row.max(1);
    let mut column = v_flex().w_full().gap_2();
    let mut rest = cells.into_iter();

    loop {
        let mut row = h_flex().w_full().gap_2().items_stretch();
        let mut taken = 0;
        for cell in rest.by_ref().take(per_row) {
            row = row.child(div().flex_1().min_w_0().child(cell));
            taken += 1;
        }
        if taken == 0 {
            break;
        }
        column = column.child(row);
    }

    column
}

/// Сколько ячеек шириной `cell` влезает в страницу при текущем размере окна.
pub fn columns(window: &Window, cell: f32) -> usize {
    let width = window.viewport_size().width.as_f32() - super::RAIL - super::LOG_WIDTH - 32. - 8.;
    (((width + 8.) / (cell + 8.)).floor() as usize).clamp(1, 7)
}

pub fn timer_ring(
    cx: &App,
    title: &'static str,
    enabled: bool,
    left: Option<Duration>,
    total_secs: u64,
    last: Option<i64>,
    color: Hsla,
) -> Div {
    let total = Duration::from_secs(total_secs.max(1));
    let left = left.unwrap_or_default();
    let done = total.saturating_sub(left).as_secs_f32() / total.as_secs_f32();
    let percent = (done * 100.).clamp(0., 100.);

    let (caption, caption_color) = if !enabled {
        ("выключено".to_string(), theme::muted(cx))
    } else if left.is_zero() {
        ("отправляю".to_string(), color)
    } else {
        (fmt_dur(left), theme::text_dim(cx))
    };

    v_flex()
        .w(px(168.))
        .items_center()
        .gap_2()
        .p_3()
        .rounded(px(8.))
        .bg(theme::card(cx))
        .border_1()
        .border_color(theme::border(cx))
        .child(
            ProgressCircle::new(title_key(&title))
                .value(percent)
                .color(if enabled { color } else { theme::muted(cx) })
                .with_size(Size::Large),
        )
        .child(
            Label::new(title)
                .text_size(px(13.))
                .text_color(theme::text(cx)),
        )
        .child(
            Label::new(caption)
                .text_size(px(15.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(caption_color),
        )
        .child(
            Label::new(ago(last))
                .text_size(px(11.))
                .text_color(theme::muted(cx)),
        )
}

/// Устойчивый идентификатор кольца из его подписи.
fn title_key(title: &str) -> SharedString {
    SharedString::from(format!("ring-{}", title.to_string()))
}

/// Поле ввода на всю ширину карточки.
pub fn wide_input(state: &Entity<InputState>) -> Input {
    Input::new(state).w_full()
}

/// Строка с полем ввода: подпись слева, поле справа.
pub fn field_row(cx: &App, label: impl Into<SharedString>, field: impl IntoElement) -> Div {
    h_flex()
        .w_full()
        .gap_3()
        .items_center()
        .child(
            div().w(px(190.)).flex_none().child(
                Label::new(label.into())
                    .text_size(px(13.))
                    .text_color(theme::muted(cx)),
            ),
        )
        .child(div().flex_1().min_w_0().child(field))
}
