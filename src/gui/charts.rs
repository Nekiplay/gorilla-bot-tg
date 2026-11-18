//! Графики истории: линии по замерам из `gorilla_state.json`.
//!
//! Чарты gpui-kit рисуют ряд значений по подписям-строкам, поэтому замер
//! превращается в точку «подпись времени — число».

use gpui_kit::component::chart::LineChart;
use gpui_kit::component::plot::PlotElement;
use gpui_kit::prelude::*;
use gpui_kit::{div, px, App, Div, Hsla, SharedString};

use super::{common, theme};
use crate::history::{History, Sample};

/// Сколько часовых точек показываем: 240 часов — десять суток.
const MAX_POINTS: usize = 240;

/// Ширина одной точки на полотне: график шире карточки, его пролистывают.
const POINT_WIDTH: f32 = 44.0;

/// Сколько подписей времени оставляем на оси — остальные наезжают друг на друга.
const X_TICKS: usize = 6;

/// Высота полотна графика.
/// Высота полотна графика: 150 мало — нижние подписи оси обрезались.
const PLOT_HEIGHT: f32 = 190.0;

/// Точка линии: подпись по времени и значение.
#[derive(Clone, PartialEq)]
struct Point {
    label: SharedString,
    value: f64,
}

/// Замеры в часовые точки линии; `take` достаёт метрику из замера.
///
/// В каждом часе берём последний замер — поминутные точки сливались в кашу.
/// Отдаём от старых к новым: время на оси идёт слева направо.
fn series(history: &History, take: impl Fn(&Sample) -> Option<f64>) -> Vec<Point> {
    let mut points: Vec<(i64, Point)> = Vec::new();
    for sample in history.samples() {
        let Some(value) = take(sample) else {
            continue;
        };
        let hour = sample.ts.div_euclid(3600);
        let point = Point {
            label: SharedString::from(common::local_hour(sample.ts)),
            value,
        };
        match points.last_mut() {
            Some((last_hour, last)) if *last_hour == hour => *last = point,
            _ => points.push((hour, point)),
        }
    }
    let start = points.len().saturating_sub(MAX_POINTS);
    points[start..]
        .iter()
        .map(|(_, point)| point.clone())
        .collect()
}

/// Карточка с линейным графиком метрики из истории.
fn line(
    cx: &App,
    id: &'static str,
    title: &'static str,
    color: Hsla,
    data: Vec<Point>,
    suffix: &'static str,
) -> Div {
    if data.len() < 2 {
        return common::card_with(
            cx,
            title,
            common::hint(cx, "мало замеров: история копится раз в минуту"),
        );
    }

    let data_len = data.len();
    let chart = LineChart::new(data)
        .id(id)
        .x(|point: &Point| point.label.clone())
        .y(|point: &Point| point.value)
        .stroke(color)
        .linear()
        .x_tick_count(X_TICKS)
        .y_tick_format(move |value| format!("{value:.0}{suffix}"));

    // Полотно шире карточки — тогда его можно пролистать по горизонтали.
    let width = (data_len as f32 * POINT_WIDTH).max(240.);
    common::card_with(
        cx,
        title,
        div()
            .id(gpui_kit::ElementId::Name(format!("{id}-strip").into()))
            .w_full()
            .h(px(PLOT_HEIGHT))
            .flex_shrink_0()
            .overflow_x_scroll()
            .child(
                div()
                    .h_full()
                    .w(px(width))
                    .min_w_full()
                    .child(PlotElement::new(chart)),
            ),
    )
}

/// Винрейт казино, процент.
pub fn casino_win_rate(cx: &App, history: &History, color: Hsla) -> Div {
    line(
        cx,
        "chart-casino-win-rate",
        "Винрейт казино, %",
        color,
        series(history, Sample::win_rate),
        "%",
    )
}

/// Ставка казино, монеты.
pub fn casino_bet(cx: &App, history: &History, color: Hsla) -> Div {
    line(
        cx,
        "chart-casino-bet",
        "Ставка казино",
        color,
        series(history, |sample| Some(sample.bet as f64)),
        "",
    )
}

/// Доля вскрытых сейфов, процент.
pub fn safe_rate(cx: &App, history: &History, color: Hsla) -> Div {
    line(
        cx,
        "chart-safe-rate",
        "Сейфы: вскрыто, %",
        color,
        series(history, |sample| {
            (sample.safe_attempts > 0)
                .then(|| sample.safe_wins as f64 * 100.0 / sample.safe_attempts as f64)
        }),
        "%",
    )
}

/// Доля решённых капч, процент.
pub fn captcha_rate(cx: &App, history: &History, color: Hsla) -> Div {
    line(
        cx,
        "chart-captcha-rate",
        "Капча: решено, %",
        color,
        series(history, |sample| {
            let total = sample.captcha_solved + sample.captcha_failed;
            (total > 0).then(|| sample.captcha_solved as f64 * 100.0 / total as f64)
        }),
        "%",
    )
}

/// Деньги на руках.
pub fn cash(cx: &App, history: &History, color: Hsla) -> Div {
    line(
        cx,
        "chart-cash",
        "Наличными",
        color,
        series(history, |sample| sample.cash),
        "",
    )
}

/// Цвет линии для денег — акцент темы.
pub fn money_color(cx: &App) -> Hsla {
    theme::accent_light(cx)
}
