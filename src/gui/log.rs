
use eframe::egui::{self, *};

use super::App;
use super::common::*;

    pub(super) fn log_card(app: &mut App, ui: &mut egui::Ui) {
        let logs = app.shared.logs();
        let height = ui.available_height().max(140.0);
        Frame::new()
            .fill(CARD)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(6)
            .inner_margin(Margin::same(14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_height(height);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Журнал").strong().size(14.5).color(TEXT));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("{} строк", logs.len()))
                                .size(11.5)
                                .color(MUTED),
                        );
                    });
                });

                ui.add_space(5.0);
                let body = (height - 58.0).max(80.0);
                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .max_height(body)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let start = logs.len().saturating_sub(400);
                        for line in &logs[start..] {
                            let color = if line.contains("ошибка") || line.contains("не удалось")
                            {
                                DANGER
                            } else if line.contains("выигрыш") || line.contains("выиграли") {
                                OK
                            } else if line.contains("проигрыш") {
                                WARN
                            } else {
                                TEXT_DIM
                            };
                            ui.add(
                                Label::new(
                                    RichText::new(line).monospace().size(12.0).color(color),
                                )
                                .selectable(true),
                            );
                        }
                    });
            });
    }
