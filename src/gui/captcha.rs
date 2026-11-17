//! Капча: сетка плиток, выбор нужных и отправка боту.

use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::label::Label;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::{common, theme, Bot};
use crate::captcha::{self, Captcha};

/// Кэш картинок плиток между кадрами.
#[derive(Default)]
pub(super) struct Viewer {
    message_id: i32,
    /// Текстуры плиток в том же порядке, что и у капчи.
    tiles: Vec<Arc<RenderImage>>,
    /// Отметки игрока по плиткам.
    selected: Vec<bool>,
}

impl Viewer {
    /// Пересобирает картинки, если бот прислал новую капчу.
    fn sync(&mut self, captcha: &Captcha) {
        if self.message_id == captcha.message_id && self.tiles.len() == captcha.tiles.len() {
            return;
        }
        self.message_id = captcha.message_id;
        self.tiles = captcha
            .tiles
            .iter()
            .filter_map(|rgba| render_tile(rgba, captcha))
            .collect();
        self.selected = vec![false; self.tiles.len()];
    }
}

/// RGBA-плитка → картинка gpui.
fn render_tile(rgba: &[u8], captcha: &Captcha) -> Option<Arc<RenderImage>> {
    let buffer = image::RgbaImage::from_raw(captcha.tile_w, captcha.tile_h, rgba.to_vec())?;
    let frame = image::Frame::new(buffer);
    Some(Arc::new(RenderImage::new(vec![frame])))
}

/// Страница «Капча».
pub(super) fn page(bot: &mut Bot, _window: &mut Window, cx: &mut Context<Bot>) -> AnyElement {
    let captcha = bot.shared.captcha();
    let note = bot.shared.captcha_note();

    let Some(captcha) = captcha else {
        return no_captcha(&note, cx);
    };
    bot.captcha.sync(&captcha);

    let cols = captcha.cols.max(1);
    let rows = captcha.rows.max(1);
    let scale = (700.0 / (captcha.tile_w as f32 * cols as f32)).clamp(0.35, 1.4);
    let tile_w = px(captcha.tile_w as f32 * scale);
    let tile_h = px(captcha.tile_h as f32 * scale);

    let mut grid = v_flex().w_full().gap_2();
    for row in 0..rows {
        let mut line = h_flex().w_full().gap_2();
        for col in 0..cols {
            let index = row * cols + col;
            if index >= bot.captcha.tiles.len() {
                break;
            }
            let selected = bot.captcha.selected.get(index).copied().unwrap_or(false);
            let guess = captcha.guess.get(index).copied().unwrap_or(0.0);
            let image = bot.captcha.tiles[index].clone();
            let border = if selected {
                theme::accent_light(&*cx)
            } else {
                theme::border(&*cx)
            };

            let mut tile = div()
                .id(("tile", index))
                .relative()
                .w(tile_w)
                .h(tile_h)
                .rounded(px(6.))
                .border_1()
                .border_color(border)
                .when(selected, |el| el.border_2())
                .cursor_pointer()
                .child(img(image).w(tile_w).h(tile_h).rounded(px(5.)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(flag) = this.captcha.selected.get_mut(index) {
                        *flag = !*flag;
                        cx.notify();
                    }
                }));

            if guess >= 0.5 {
                tile = tile.child(
                    div()
                        .absolute()
                        .top(px(4.))
                        .left(px(4.))
                        .w(px(8.))
                        .h(px(8.))
                        .rounded_full()
                        .bg(theme::ok(&*cx)),
                );
            }
            if selected {
                tile = tile.child(
                    div().absolute().bottom(px(2.)).right(px(4.)).child(
                        Label::new("✓")
                            .text_size(px(16.))
                            .text_color(theme::accent_light(&*cx)),
                    ),
                );
            }

            line = line.child(tile);
        }
        grid = grid.child(line);
    }

    let picked = bot.captcha.selected.iter().filter(|on| **on).count();
    let message_id = captcha.message_id;
    let send = Button::new("captcha-submit")
        .primary()
        .label("Отправить")
        .on_click(cx.listener(move |this, _, _, cx| {
            this.shared
                .submit_captcha(message_id, this.captcha.selected.clone());
            this.shared.set_captcha_note("отправляю выбор боту…");
            cx.notify();
        }));

    let clear = Button::new("captcha-clear")
        .label("Очистить")
        .on_click(cx.listener(|this, _, _, cx| {
            for flag in &mut this.captcha.selected {
                *flag = false;
            }
            cx.notify();
        }));

    let mut buttons = h_flex().w_full().gap_2().child(send).child(clear);
    if captcha.has_guess() {
        let hint = captcha.suggested();
        buttons = buttons.child(
            Button::new("captcha-hint")
                .label("Подсказка модели")
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.captcha.selected = hint.clone();
                    cx.notify();
                })),
        );
    }

    let title = captcha.prompt.lines().next().unwrap_or("Капча").to_string();

    common::card_with(
        cx,
        "Капча",
        v_flex()
            .w_full()
            .gap_2()
            .child(
                Label::new(title)
                    .text_size(px(15.))
                    .text_color(theme::text(&*cx)),
            )
            .child(common::hint(
                &*cx,
                format!(
                    "{} плиток ({}×{}) — отметь нужные и нажми «Отправить».",
                    captcha.tiles.len(),
                    cols,
                    rows
                ),
            ))
            .when(!note.is_empty(), |el| el.child(common::hint(&*cx, note)))
            .child(grid)
            .child(buttons)
            .child(common::hint(&*cx, format!("Отмечено плиток: {picked}"))),
    )
    .into_any_element()
}

/// Заглушка, когда капчи нет.
fn no_captcha(note: &str, cx: &mut Context<Bot>) -> AnyElement {
    let marked = std::fs::read_to_string(captcha::model_dir().join("samples.txt"))
        .map(|text| text.lines().filter(|line| !line.trim().is_empty()).count())
        .unwrap_or(0);

    let mut body = v_flex().w_full().gap_2().child(
        Label::new("Сейчас капчи нет.")
            .text_size(px(14.))
            .text_color(theme::text(&*cx)),
    );

    if !note.is_empty() {
        body = body.child(common::hint(&*cx, note));
    }

    body = body.child(common::hint(
        &*cx,
        format!(
            "Размечено плиток: {marked}. Модель включается от {} штук.",
            captcha::MIN_SAMPLES
        ),
    ));

    common::card_with(cx, "Капча", body).into_any_element()
}
