
use eframe::egui::{self, *};

use super::App;
use super::common::*;

    pub(super) fn proxy_card(app: &mut App, ui: &mut egui::Ui) {
        card(ui, "Прокси", |ui| {
            row(ui, "SOCKS5", |ui| {
                let mut proxy = app.cfg.proxy.clone().unwrap_or_default();
                let w = ui.available_width();
                if ui
                    .add(
                        TextEdit::singleline(&mut proxy)
                            .hint_text("socks5://user:pass@host:port")
                            .desired_width(w.max(80.0)),
                    )
                    .changed()
                {
                    app.cfg.proxy = if proxy.trim().is_empty() {
                        None
                    } else {
                        Some(proxy.trim().to_string())
                    };
                }
            });
            row(ui, "Worker", |ui| {
                wide_edit(ui, &mut app.cfg.mtproxy.worker, "name.workers.dev");
            });
            row(ui, "MTProxy host", |ui| {
                wide_edit(ui, &mut app.cfg.mtproxy.host, "127.0.0.1");
            });
            row(ui, "MTProxy port", |ui| {
                ui.add(egui::DragValue::new(&mut app.cfg.mtproxy.port).speed(1.0));
            });
            row(ui, "MTProxy secret", |ui| {
                wide_edit(ui, &mut app.cfg.mtproxy.secret, "hex-секрет");
            });
            hint(
                ui,
                "Пусто — стандартные CF-прокси tg-ws-proxy (пул прогретых туннелей); свой \
                 worker — WSS-труба; secret — MTProto-прокси; `off` — прямое подключение.",
            );
        });
    }
