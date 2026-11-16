//! Окно настроек на egui: каждая страница — отдельный файл.
//!
//! Движок живёт отдельно (см. `engine.rs`), здесь только редактирование
//! config.json, кнопки и лог.


mod bonus;
mod business;
mod casino;
mod common;
mod log;
mod overview;
mod pet;
mod printer;
mod profile;
mod proxy;
mod race;
mod telegram;

use common::*;
use overview::overview;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use eframe::egui::{
    self, Align, Button, Color32, CornerRadius, FontFamily, FontId, Frame, Layout, Margin,
    RichText, ScrollArea, Stroke, TextStyle, Vec2,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::Config;
use crate::engine::{BonusKind, Command, Shared, Status};


/// Иконка окна и панели задач: тот же логотип, из которого собирается `.ico`
/// для exe, только разобранный в RGBA (egui ждёт сырые пиксели).
///
/// Верхний размер — 256: столько Windows берёт для крупных плиток, а больше
/// держать в памяти незачем.
fn window_icon() -> Option<egui::IconData> {
    let logo = image::load_from_memory_with_format(winicon::LOGO, image::ImageFormat::Png).ok()?;
    let image = if logo.width() > ICON_MAX_SIDE {
        logo.resize(ICON_MAX_SIDE, ICON_MAX_SIDE, image::imageops::FilterType::Lanczos3)
    } else {
        logo
    }
    .to_rgba8();
    let (width, height) = image.dimensions();
    Some(egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    })
}

/// Максимальная сторона иконки окна, в пикселях.
const ICON_MAX_SIDE: u32 = 256;

/// Запуск окна. Без config.json откроется с настройками по умолчанию —
/// их можно заполнить и сохранить прямо здесь.
pub fn start() {
    let (cfg, config_path) = match Config::load() {
        Ok(cfg) => {
            let path = cfg.base_dir.join("config.json");
            (cfg, path)
        }
        Err(_) => {
            let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let mut cfg = Config::default();
            cfg.base_dir = dir.clone();
            (cfg, dir.join("config.json"))
        }
    };
    let _ = crate::DATA_DIR.set(cfg.base_dir.clone());

    let shared = Shared::new(cfg.clone());
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    crate::engine::spawn(shared.clone(), rx);

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1120.0, 760.0])
        .with_min_inner_size([900.0, 620.0])
        .with_title("Gorilla Bot");
    if let Some(icon) = window_icon() {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    let app = App {
        shared,
        tx: tx.clone(),
        cfg,
        config_path,
        saved: String::new(),
        tab: Tab::Overview,
        code_input: String::new(),
        password_input: String::new(),
    };

    let result = eframe::run_native(
        "gorilla-bot",
        options,
        Box::new(move |cc| {
            install_style(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    );
    let _ = tx.send(Command::Shutdown);
    if let Err(e) = result {
        eprintln!("Не удалось открыть окно: {e}");
        eprintln!("Можно работать из консоли: gorilla-bot casino | balance | check");
    }
}

/// Тёмная тема: скруглённые поля, мягкие границы, крупные подписи.
fn install_style(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = CARD;
    v.extreme_bg_color = FIELD;
    v.faint_bg_color = CARD;
    v.code_bg_color = FIELD;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.selection.bg_fill = ACCENT.gamma_multiply(0.45);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;

    v.widgets.noninteractive.bg_fill = CARD;
    v.widgets.noninteractive.weak_bg_fill = CARD;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.noninteractive.corner_radius = CornerRadius::same(6);

    v.widgets.inactive.bg_fill = FIELD;
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(34, 37, 48);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.inactive.corner_radius = CornerRadius::same(4);

    v.widgets.hovered.bg_fill = Color32::from_rgb(46, 50, 66);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(46, 50, 66);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT.gamma_multiply(0.7));
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    v.widgets.hovered.corner_radius = CornerRadius::same(4);

    v.widgets.active.bg_fill = ACCENT_DIM;
    v.widgets.active.weak_bg_fill = ACCENT_DIM;
    v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    v.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    v.widgets.active.corner_radius = CornerRadius::same(4);

    style.visuals = v;
    style.spacing.item_spacing = Vec2::new(9.0, 7.0);
    style.spacing.button_padding = Vec2::new(13.0, 6.0);
    style.spacing.interact_size.y = 26.0;
    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(21.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Body,
        FontId::new(14.5, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(14.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Small,
        FontId::new(12.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Monospace,
        FontId::new(12.5, FontFamily::Monospace),
    );
    });
}

/// Вкладки настроек в шапке окна.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Overview,
    Telegram,
    Proxy,
    Casino,
    Pet,
    Printer,
    Race,
    Bonus,
    Profile,
    Business,
}

struct App {
    shared: Arc<Shared>,
    tx: UnboundedSender<Command>,
    cfg: Config,
    config_path: PathBuf,
    saved: String,
    tab: Tab,
    /// Ввод кода входа из Telegram.
    code_input: String,
    /// Ввод пароля 2FA.
    password_input: String,
}

impl App {
    fn save(&mut self) {
        if self.cfg.api_id <= 0 || self.cfg.api_hash.trim().is_empty() {
            self.saved = "API ID и API hash обязательны".into();
        } else {
            self.saved = match serde_json::to_string_pretty(&self.cfg)
                .map_err(|e| e.to_string())
                .and_then(|text| {
                    std::fs::write(&self.config_path, text).map_err(|e| e.to_string())
                }) {
                Ok(()) => format!("Сохранено: {}", self.config_path.display()),
                Err(e) => format!("Ошибка записи: {e}"),
            };
        }
        let _ = self.tx.send(Command::SetConfig(self.cfg.clone()));
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.ctx().request_repaint_after(Duration::from_millis(500));

        let status = self.shared.status();

        egui::Panel::top("top")
            .frame(
                Frame::new()
                    .fill(CARD)
                    .inner_margin(Margin::symmetric(16, 9)),
            )
            .show(ui, |ui| self.top_bar(ui, &status));

        egui::Panel::right("log")
            .resizable(true)
            .default_size(470.0)
            .size_range(340.0..=760.0)
            .frame(
                Frame::new()
                    .fill(BG)
                    .inner_margin(Margin::symmetric(12, 10)),
            )
            .show(ui, |ui| log::log_card(self, ui));

        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(BG)
                    .inner_margin(Margin::symmetric(16, 12)),
            )
            .show(ui, |ui| {
                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| match self.tab {
                        Tab::Overview => overview(self, ui, &status),
                        Tab::Profile => profile::profile_page(self, ui, &status),
                        Tab::Telegram => telegram::telegram_card(self, ui),
                        Tab::Proxy => proxy::proxy_card(self, ui),
                        Tab::Casino => casino::casino_card(self, ui, status.casino_running),
                        Tab::Pet => {
                            let left = self.shared.next_pet_in();
                            let last = self.shared.last_pet();
                            pet::pet_card(self, ui, left, last);
                        }
                        Tab::Printer => {
                            let left = self.shared.next_printer_in();
                            let last = self.shared.last_printer();
                            printer::printer_card(self, ui, left, last);
                        }
                        Tab::Race => {
                            let left = self.shared.next_race_in();
                            let last = self.shared.last_race();
                            race::race_card(self, ui, left, last);
                        }
                        Tab::Business => {
                            let left = self.shared.next_business_in();
                            let last = self.shared.last_business();
                            business::business_card(self, ui, left, last);
                        }
                        Tab::Bonus => {
                            let left = self.shared.next_bonus_in(BonusKind::Channel);
                            let last = self.shared.last_bonus(BonusKind::Channel);
                            bonus::bonus_card(self, ui, BonusKind::Channel, left, last);
                            ui.add_space(10.0);
                            let left = self.shared.next_bonus_in(BonusKind::Dm);
                            let last = self.shared.last_bonus(BonusKind::Dm);
                            bonus::bonus_card(self, ui, BonusKind::Dm, left, last);
                        }
                    });
            });
    }
}

impl App {
    fn top_bar(&mut self, ui: &mut egui::Ui, status: &Status) {
        ui.horizontal(|ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                for (tab, label) in [
                    (Tab::Overview, "Обзор"),
                    (Tab::Profile, "Профиль"),
                    (Tab::Pet, "Питомец"),
                    (Tab::Printer, "Принтер"),
                    (Tab::Race, "Гонка"),
                    (Tab::Bonus, "Бонус"),
                    (Tab::Business, "Бизнес"),
                    (Tab::Casino, "Казино"),
                    (Tab::Telegram, "Telegram"),
                    (Tab::Proxy, "Прокси"),
                ] {
                    let active = self.tab == tab;
                    let button = Button::new(
                        RichText::new(label)
                            .size(13.5)
                            .color(if active { Color32::WHITE } else { TEXT_DIM }),
                    )
                    .fill(if active { ACCENT_DIM } else { FIELD })
                    .stroke(Stroke::new(1.0, if active { ACCENT } else { BORDER }))
                    .corner_radius(CornerRadius::same(4))
                    .min_size(Vec2::new(0.0, 28.0));
                    if ui.add(button).clicked() {
                        self.tab = tab;
                    }
                }
            });

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(
                        Button::new(RichText::new("Сохранить").color(Color32::WHITE))
                            .fill(ACCENT)
                            .min_size(Vec2::new(0.0, 28.0)),
                    )
                    .clicked()
                {
                    self.save();
                }
                if status.casino_running {
                    pill(ui, "казино идёт", WARN);
                }
                let (label, color) = if status.connected {
                    ("● онлайн", OK)
                } else {
                    ("● офлайн", MUTED)
                };
                pill(ui, label, color);
            });
        });

        if !self.saved.is_empty() {
            ui.add_space(2.0);
            ui.label(RichText::new(&self.saved).size(12.0).color(MUTED));
        }
    }


















}





















