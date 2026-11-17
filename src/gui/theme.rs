//! Палитра бота: единственное место, где заданы цвета.
//!
//! Тёмная схема с фиолетовым акцентом. Значения раскладываются по токенам
//! темы gpui-kit, поэтому страницы берут цвет у темы (`cx.theme()`), а не у
//! констант.

use gpui_kit::component::{ActiveTheme, Theme, ThemeMode};
use gpui_kit::{rgb, App, Hsla};
/// Фон окна.
const BG: u32 = 0x0f10_16;
/// Фон карточек, рейла и журнала.
const CARD: u32 = 0x1a1c_25;
/// Фон полей ввода и вложенных плашек.
const FIELD: u32 = 0x1416_1d;
/// Границы.
const BORDER: u32 = 0x282b_37;
/// Основной текст.
const TEXT: u32 = 0xe4e7_f0;
/// Второстепенный текст: подписи строк.
const TEXT_DIM: u32 = 0xc4c9_d6;
/// Приглушённый текст: подсказки, счётчики.
const MUTED: u32 = 0x8c92_a5;
/// Акцент: кнопки, активная вкладка.
const ACCENT: u32 = 0x7c5c_ff;
/// Акцент для текста на тёмном.
const ACCENT_LIGHT: u32 = 0xb2a0_ff;
/// Заливка выбранного пункта.
const ACCENT_DIM: u32 = 0x4032_80;
/// Успех.
const OK: u32 = 0x50d2_82;
/// Предупреждение.
const WARN: u32 = 0xf0b4_50;
/// Ошибка.
const DANGER: u32 = 0xeb5a_64;

/// Переводит цвет палитры в цвет gpui.
fn color(hex: u32) -> Hsla {
    rgb(hex).into()
}

/// Красит токены темы под палитру бота. Вызывать после `gpui_kit::init`.
pub fn install(cx: &mut App) {
    Theme::change(ThemeMode::Dark, None, cx);

    let colors = &mut Theme::global_mut(cx).colors;
    colors.background = color(BG);
    colors.foreground = color(TEXT);
    colors.border = color(BORDER);
    colors.muted = color(CARD);
    colors.muted_foreground = color(MUTED);

    colors.primary = color(ACCENT);
    colors.primary_hover = color(ACCENT_LIGHT);
    colors.primary_active = color(ACCENT);
    colors.primary_foreground = color(TEXT);

    colors.secondary = color(FIELD);
    colors.secondary_hover = color(0x2225_30);
    colors.secondary_active = color(ACCENT_DIM);
    colors.secondary_foreground = color(TEXT);

    colors.accent = color(ACCENT_DIM);
    colors.accent_foreground = color(TEXT);
    colors.group_box = color(CARD);
    colors.group_box_foreground = color(TEXT);
    colors.input = color(FIELD);
    colors.popover = color(CARD);
    colors.popover_foreground = color(TEXT);
    colors.ring = color(ACCENT);
    colors.selection = color(ACCENT_DIM);
    colors.link = color(ACCENT_LIGHT);
    colors.link_hover = color(ACCENT);
    colors.link_active = color(ACCENT);

    colors.sidebar = color(CARD);
    colors.sidebar_border = color(BORDER);
    colors.sidebar_foreground = color(MUTED);
    colors.sidebar_accent = color(ACCENT_DIM);
    colors.sidebar_accent_foreground = color(TEXT);
    colors.sidebar_primary = color(ACCENT);
    colors.sidebar_primary_foreground = color(TEXT);

    colors.success = color(OK);
    colors.warning = color(WARN);
    colors.danger = color(DANGER);
    colors.info = color(ACCENT_LIGHT);
    colors.progress_bar = color(ACCENT);
    colors.chart_bullish = color(OK);
    colors.chart_bearish = color(DANGER);
    colors.chart_grid = color(BORDER);
}

/// Фон окна.
pub fn background(cx: &App) -> Hsla {
    cx.theme().colors.background
}

/// Основной текст.
pub fn text(cx: &App) -> Hsla {
    cx.theme().colors.foreground
}

/// Второстепенный текст строк.
pub fn text_dim(_cx: &App) -> Hsla {
    color(TEXT_DIM)
}

/// Подсказки и счётчики.
pub fn muted(cx: &App) -> Hsla {
    cx.theme().colors.muted_foreground
}

/// Фон карточек.
pub fn card(cx: &App) -> Hsla {
    cx.theme().colors.group_box
}

/// Фон полей и вложенных плашек.
pub fn field(cx: &App) -> Hsla {
    cx.theme().colors.input
}

/// Границы карточек и полей.
pub fn border(cx: &App) -> Hsla {
    cx.theme().colors.border
}

/// Акцент.
pub fn accent(cx: &App) -> Hsla {
    cx.theme().colors.primary
}

/// Светлый акцент для текста.
pub fn accent_light(_cx: &App) -> Hsla {
    color(ACCENT_LIGHT)
}

/// Успех.
pub fn ok(cx: &App) -> Hsla {
    cx.theme().colors.success
}

/// Предупреждение.
pub fn warn(cx: &App) -> Hsla {
    cx.theme().colors.warning
}

/// Ошибка.
pub fn danger(cx: &App) -> Hsla {
    cx.theme().colors.danger
}
