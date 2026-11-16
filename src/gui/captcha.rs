//! Вкладка «Капча»: бот присылает картинку, игрок отмечает нужные плитки,
//! движок нажимает inline-кнопки «1»…«Готово» и дообучает модель.

use eframe::egui::{self, Color32, FontId, RichText, Sense, Stroke, Vec2};

use super::common::*;
use super::App;
use crate::captcha as model;

/// Что окно помнит о текущей капче: текстуры плиток и отметки.
#[derive(Default)]
pub(super) struct Viewer {
    message_id: i32,
    tiles: Vec<egui::TextureHandle>,
    selected: Vec<bool>,
}

/// Страница «Капча».
pub(super) fn captcha_card(app: &mut App, ui: &mut egui::Ui) {
    let note = app.shared.captcha_note();
    let pending = app.shared.captcha();
    let samples = sample_count();

    card(ui, "Капча", |ui| {
        if !note.is_empty() {
            hint(ui, &note);
            ui.add_space(4.0);
        }

        let Some(captcha) = pending else {
            ui.label(
                RichText::new("Сейчас капчи нет.")
                    .size(13.5)
                    .color(TEXT_DIM),
            );
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!(
                    "Размечено плиток: {samples}. Модель включается от {} штук.",
                    model::MIN_SAMPLES
                ))
                .size(12.0)
                .color(MUTED),
            );
            return;
        };

        load_tiles(app, ui, &captcha);
        if !captcha.prompt.trim().is_empty() {
            ui.label(
                RichText::new(captcha.prompt.lines().next().unwrap_or("").trim())
                    .size(13.5)
                    .color(TEXT),
            );
        }
        ui.label(
            RichText::new(format!(
                "{} плиток ({}×{}) — отметь нужные и нажми «Отправить».",
                captcha.tiles.len(),
                captcha.cols,
                captcha.rows
            ))
            .size(12.5)
            .color(MUTED),
        );
        ui.add_space(6.0);

        let side = tile_side(ui, captcha.tile_w, captcha.tile_h, captcha.cols);
        let mut flips = Vec::new();
        for row in 0..captcha.rows {
            ui.horizontal(|ui| {
                for col in 0..captcha.cols {
                    let index = row * captcha.cols + col;
                    let Some(tex) = app.captcha_view.tiles.get(index) else {
                        continue;
                    };
                    let selected = app.captcha_view.selected.get(index).copied().unwrap_or(false);
                    let guess = captcha.guess.get(index).copied().unwrap_or(0.0);
                    if draw_tile(ui, tex, side, selected, guess) {
                        flips.push(index);
                    }
                }
            });
        }
        for index in flips {
            if let Some(cell) = app.captcha_view.selected.get_mut(index) {
                *cell = !*cell;
            }
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Отправить").clicked() {
                app.shared
                    .submit_captcha(captcha.message_id, app.captcha_view.selected.clone());
                app.shared.set_captcha_note("отправляю выбор боту…");
            }
            if ui.button("Очистить").clicked() {
                app.captcha_view.selected = vec![false; captcha.tiles.len()];
            }
            if captcha.has_guess() && ui.button("Подсказка модели").clicked() {
                app.captcha_view.selected = captcha.suggested();
            }
        });

        let picked = app
            .captcha_view
            .selected
            .iter()
            .enumerate()
            .filter(|(_, on)| **on)
            .count();
        ui.add_space(4.0);
        ui.label(
            RichText::new(format!("Отмечено плиток: {picked}"))
                .size(12.0)
                .color(TEXT_DIM),
        );
    });
}

/// Сколько примеров уже размечено (строк в samples.txt).
fn sample_count() -> usize {
    std::fs::read_to_string(model::model_dir().join("samples.txt"))
        .map(|text| text.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0)
}

/// Пересобирает текстуры, когда пришла новая капча.
fn load_tiles(app: &mut App, ui: &egui::Ui, captcha: &model::Captcha) {
    if app.captcha_view.message_id == captcha.message_id {
        return;
    }
    let size = [captcha.tile_w as usize, captcha.tile_h as usize];
    let tiles = captcha
        .tiles
        .iter()
        .enumerate()
        .map(|(index, pixels)| {
            let image = egui::ColorImage::from_rgba_unmultiplied(size, pixels);
            ui.ctx().load_texture(
                format!("captcha-{}-{index}", captcha.message_id),
                image,
                egui::TextureOptions::LINEAR,
            )
        })
        .collect();
    app.captcha_view.message_id = captcha.message_id;
    app.captcha_view.tiles = tiles;
    app.captcha_view.selected = captcha.suggested();
}

/// Размер плитки на экране: чтобы вся сетка влезла по ширине.
fn tile_side(ui: &egui::Ui, tile_w: u32, tile_h: u32, cols: usize) -> Vec2 {
    let max_w = (ui.available_width() - 12.0 * cols as f32) / cols.max(1) as f32;
    let scale = (max_w / tile_w.max(1) as f32).clamp(0.35, 1.6);
    Vec2::new(
        tile_w as f32 * scale,
        tile_h as f32 * scale,
    )
}

/// Одна плитка: картинка, рамка выбора, метка модели. `true` — клик.
fn draw_tile(
    ui: &mut egui::Ui,
    tex: &egui::TextureHandle,
    side: Vec2,
    selected: bool,
    guess: f32,
) -> bool {
    let (rect, response) = ui.allocate_exact_size(side, Sense::click());
    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    ui.painter().image(tex.id(), rect, uv, Color32::WHITE);

    let stroke = if selected {
        Stroke::new(3.0, ACCENT_LIGHT)
    } else if response.hovered() {
        Stroke::new(2.0, ACCENT)
    } else {
        Stroke::new(1.0, BORDER)
    };
    ui.painter().rect_stroke(rect, 4, stroke, egui::StrokeKind::Inside);

    if guess >= 0.5 {
        let dot = egui::Rect::from_min_size(
            rect.left_top() + Vec2::new(4.0, 4.0),
            Vec2::splat(7.0),
        );
        ui.painter().circle_filled(dot.center(), 3.5, OK);
    }
    if selected {
        ui.painter().text(
            rect.right_bottom() - Vec2::new(8.0, 8.0),
            egui::Align2::RIGHT_BOTTOM,
            "✓",
            FontId::proportional(14.0),
            ACCENT_LIGHT,
        );
    }
    response.clicked()
}