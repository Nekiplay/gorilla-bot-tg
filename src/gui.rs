//! Окно настроек на egui. Движок живёт отдельно (см. `engine.rs`),
//! здесь только редактирование config.json, кнопки и лог.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use eframe::egui::{
    self, Align, Button, Color32, CornerRadius, FontFamily, FontId, Frame, Label, Layout, Margin,
    RichText, ScrollArea, Stroke, TextEdit, TextStyle, Vec2,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::Config;
use crate::engine::{BonusKind, Command, Shared, Status};

// Тёмная палитра с фиолетовым акцентом.
const BG: Color32 = Color32::from_rgb(15, 16, 22);
const CARD: Color32 = Color32::from_rgb(26, 28, 37);
const FIELD: Color32 = Color32::from_rgb(20, 22, 29);
const BORDER: Color32 = Color32::from_rgb(40, 43, 55);
const TEXT: Color32 = Color32::from_rgb(228, 231, 240);
const TEXT_DIM: Color32 = Color32::from_rgb(196, 201, 214);
const MUTED: Color32 = Color32::from_rgb(140, 146, 165);
const ACCENT: Color32 = Color32::from_rgb(124, 92, 255);
const ACCENT_LIGHT: Color32 = Color32::from_rgb(178, 160, 255);
const ACCENT_DIM: Color32 = Color32::from_rgb(64, 50, 128);
const OK: Color32 = Color32::from_rgb(80, 210, 130);
const WARN: Color32 = Color32::from_rgb(240, 180, 80);
const DANGER: Color32 = Color32::from_rgb(235, 90, 100);

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

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1120.0, 760.0])
            .with_min_inner_size([900.0, 620.0])
            .with_title("Gorilla Bot"),
        ..Default::default()
    };

    let app = App {
        shared,
        tx: tx.clone(),
        cfg,
        config_path,
        saved: String::new(),
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
        eprintln!("Можно работать из консоли: gorilla-bot casino | balance | login");
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
    v.widgets.noninteractive.corner_radius = CornerRadius::same(10);

    v.widgets.inactive.bg_fill = FIELD;
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(34, 37, 48);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.inactive.corner_radius = CornerRadius::same(9);

    v.widgets.hovered.bg_fill = Color32::from_rgb(46, 50, 66);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(46, 50, 66);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT.gamma_multiply(0.7));
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    v.widgets.hovered.corner_radius = CornerRadius::same(9);

    v.widgets.active.bg_fill = ACCENT_DIM;
    v.widgets.active.weak_bg_fill = ACCENT_DIM;
    v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    v.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    v.widgets.active.corner_radius = CornerRadius::same(9);

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

struct App {
    shared: Arc<Shared>,
    tx: UnboundedSender<Command>,
    cfg: Config,
    config_path: PathBuf,
    saved: String,
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
        let pet_left = self.shared.next_pet_in();
        let last_pet = self.shared.last_pet();
        let bonus_left = self.shared.next_bonus_in(BonusKind::Channel);
        let last_bonus = self.shared.last_bonus(BonusKind::Channel);
        let dm_left = self.shared.next_bonus_in(BonusKind::Dm);
        let last_dm = self.shared.last_bonus(BonusKind::Dm);

        egui::Panel::top("top")
            .frame(
                Frame::new()
                    .fill(CARD)
                    .inner_margin(Margin::symmetric(16, 9)),
            )
            .show(ui, |ui| self.top_bar(ui, &status));

        egui::Panel::left("settings")
            .resizable(true)
            .default_size(410.0)
            .size_range(340.0..=620.0)
            .frame(
                Frame::new()
                    .fill(BG)
                    .inner_margin(Margin::symmetric(12, 10)),
            )
            .show(ui, |ui| {
                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.telegram_card(ui);
                        self.proxy_card(ui);
                        self.casino_card(ui, status.casino_running);
                        self.autocommands_card(
                            ui, pet_left, last_pet, bonus_left, last_bonus, dm_left, last_dm,
                        );
                        ui.add_space(6.0);
                    });
            });

        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(BG)
                    .inner_margin(Margin::symmetric(12, 10)),
            )
            .show(ui, |ui| {
                self.status_card(ui, &status);
                ui.add_space(10.0);
                self.log_card(ui);
            });
    }
}

impl App {
    fn top_bar(&mut self, ui: &mut egui::Ui, status: &Status) {
        ui.horizontal(|ui| {
            ui.label(
                    RichText::new("G")
                        .size(16.0)
                        .strong()
                        .color(ACCENT_LIGHT),
                );
            ui.label(RichText::new("Gorilla Bot").size(17.0).strong().color(TEXT));
            ui.add_space(4.0);

            let (label, color) = if status.connected {
                ("● онлайн", OK)
            } else {
                ("● офлайн", MUTED)
            };
            pill(ui, label, color);
            if status.casino_running {
                pill(ui, "казино идёт", WARN);
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(
                        Button::new(RichText::new("Сохранить").color(Color32::WHITE))
                            .fill(ACCENT)
                            .min_size(Vec2::new(0.0, 30.0)),
                    )
                    .clicked()
                {
                    self.save();
                }

            });
        });

        if !self.saved.is_empty() {
            ui.add_space(2.0);
            ui.label(RichText::new(&self.saved).size(12.0).color(MUTED));
        }
    }

    fn telegram_card(&mut self, ui: &mut egui::Ui) {
        card(ui, "Telegram", |ui| {
            row(ui, "API ID", |ui| {
                ui.add(egui::DragValue::new(&mut self.cfg.api_id).speed(1.0));
            });
            row(ui, "API hash", |ui| {
                wide_edit(ui, &mut self.cfg.api_hash, "0123456789abcdef");
            });
            row(ui, "Телефон", |ui| {
                wide_edit(ui, &mut self.cfg.phone, "+79991234567");
            });
            row(ui, "Бот", |ui| {
                wide_edit(ui, &mut self.cfg.bot_username, "realgorillabot");
            });
            row(ui, "Файл сессии", |ui| {
                wide_edit(ui, &mut self.cfg.session_file, "gorilla.session");
            });
            hint(ui, "Первый вход — из консоли: gorilla-bot login (код придёт в Telegram).");
        });
    }

    fn proxy_card(&mut self, ui: &mut egui::Ui) {
        card(ui, "Прокси", |ui| {
            row(ui, "SOCKS5", |ui| {
                let mut proxy = self.cfg.proxy.clone().unwrap_or_default();
                let w = ui.available_width();
                if ui
                    .add(
                        TextEdit::singleline(&mut proxy)
                            .hint_text("socks5://user:pass@host:port")
                            .desired_width(w.max(80.0)),
                    )
                    .changed()
                {
                    self.cfg.proxy = if proxy.trim().is_empty() {
                        None
                    } else {
                        Some(proxy.trim().to_string())
                    };
                }
            });
            row(ui, "Worker", |ui| {
                wide_edit(ui, &mut self.cfg.mtproxy.worker, "name.workers.dev");
            });
            row(ui, "MTProxy host", |ui| {
                wide_edit(ui, &mut self.cfg.mtproxy.host, "127.0.0.1");
            });
            row(ui, "MTProxy port", |ui| {
                ui.add(egui::DragValue::new(&mut self.cfg.mtproxy.port).speed(1.0));
            });
            row(ui, "MTProxy secret", |ui| {
                wide_edit(ui, &mut self.cfg.mtproxy.secret, "hex-секрет");
            });
            hint(ui, "Заполнен worker — WSS-труба; заполнен secret — MTProto-прокси.");
        });
    }

    fn casino_card(&mut self, ui: &mut egui::Ui, running: bool) {
        card(ui, "Казино", |ui| {
            let (label, fill, cmd) = if running {
                ("Стоп казино", DANGER, Command::StopCasino)
            } else {
                ("Старт казино", OK, Command::StartCasino)
            };
            if ui
                .add(
                    Button::new(RichText::new(label).size(15.0).strong().color(Color32::WHITE))
                        .fill(fill)
                        .min_size(Vec2::new(ui.available_width(), 34.0)),
                )
                .clicked()
            {
                let _ = self.tx.send(cmd);
            }
            hint(
                ui,
                if running {
                    "идут ставки; поздний ответ бота всё равно будет учтён"
                } else {
                    "запускает мартингейл в этом чате"
                },
            );
            ui.add_space(2.0);
            row(ui, "Команда", |ui| {
                wide_edit(ui, &mut self.cfg.casino.command, "Казино {bet}");
            });
            row(ui, "Начальная ставка", |ui| {
                ui.add(
                    egui::DragValue::new(&mut self.cfg.casino.start_bet)
                        .speed(10_000.0)
                        .range(1..=u64::MAX),
                );
            });
            row(ui, "Потолок ставки", |ui| {
                ui.add(
                    egui::DragValue::new(&mut self.cfg.casino.max_bet)
                        .speed(100_000.0)
                        .range(1..=u64::MAX),
                );
            });
            row(ui, "Округление", |ui| {
                ui.add(
                    egui::DragValue::new(&mut self.cfg.casino.bet_round_to)
                        .speed(100.0)
                        .range(0..=1_000_000),
                );
            });
            row(ui, "Множитель", |ui| {
                ui.add(
                    egui::DragValue::new(&mut self.cfg.casino.multiplier)
                        .speed(0.05)
                        .range(1.0..=5.0)
                        .max_decimals(2),
                );
            });
            row(ui, "Разброс множителя", |ui| {
                ui.add(
                    egui::DragValue::new(&mut self.cfg.casino.multiplier_jitter)
                        .speed(0.01)
                        .range(0.0..=0.9)
                        .max_decimals(2),
                );
            });
            row(ui, "Проигрышей подряд", |ui| {
                ui.add(
                    egui::DragValue::new(&mut self.cfg.casino.max_losses_in_row).range(1..=50),
                );
            });
            row(ui, "Пауза, с", |ui| {
                ui.add(egui::DragValue::new(&mut self.cfg.casino.round_gap_secs).range(0..=3600));
            });
            row(ui, "Разброс паузы, с", |ui| {
                ui.add(
                    egui::DragValue::new(&mut self.cfg.casino.round_gap_jitter_secs)
                        .range(0..=60),
                );
            });
            row(ui, "Ждать ответ, с", |ui| {
                ui.add(
                    egui::DragValue::new(&mut self.cfg.casino.reply_timeout_secs).range(5..=600),
                );
            });
            row(ui, "Запас кулдауна, с", |ui| {
                ui.add(
                    egui::DragValue::new(&mut self.cfg.casino.cooldown_margin_secs).range(0..=600),
                );
            });

            let base = self.cfg.casino.round_gap_secs;
            let jitter = self.cfg.casino.round_gap_jitter_secs.min(base);
            hint(
                ui,
                &format!(
                    "ставка растёт в {:.2}× ± {:.0}%, пауза {}–{} с",
                    self.cfg.casino.multiplier,
                    self.cfg.casino.multiplier_jitter * 100.0,
                    base.saturating_sub(jitter),
                    base + jitter
                ),
            );
        });
    }

    fn pet_card(&mut self, ui: &mut egui::Ui, left: Option<Duration>, last_pet: Option<i64>) {
        card(ui, "Питомец", |ui| {
            ui.checkbox(
                &mut self.cfg.pet.enabled,
                RichText::new("Авто «Питомец поход»").size(13.0).color(TEXT),
            );
            ui.checkbox(
                &mut self.cfg.pet.feed_when_hungry,
                RichText::new("Выполнять подготовку, которую просит бот (покормить/поиграть)")
                    .size(13.0)
                    .color(TEXT),
            );

            row(ui, "Команда", |ui| {
                wide_edit(ui, &mut self.cfg.pet.command, "Питомец поход");
            });
            row(ui, "Подготовка", |ui| {
                wide_edit(ui, &mut self.cfg.pet.feed_command, "Питомец покормить");
            });
            hint(
                ui,
                "Команду подготовки бот называет сам в кавычках — она берётся из ответа.",
            );
            row(ui, "Интервал, с", |ui| {
                ui.add(
                    egui::DragValue::new(&mut self.cfg.pet.interval_secs)
                        .speed(60.0)
                        .range(60..=86_400),
                );
            });
            row(ui, "Разброс, с", |ui| {
                ui.add(
                    egui::DragValue::new(&mut self.cfg.pet.interval_jitter_secs)
                        .speed(10.0)
                        .range(0..=3_600),
                );
            });
            row(ui, "Ждать ответ, с", |ui| {
                ui.add(egui::DragValue::new(&mut self.cfg.pet.reply_timeout_secs).range(5..=300));
            });

            let interval = self.cfg.pet.interval_secs;
            let jitter = self.cfg.pet.interval_jitter_secs.min(interval);
            hint(
                ui,
                &format!(
                    "{} ч {} мин ± {} мин ({}–{} с)",
                    interval / 3600,
                    interval % 3600 / 60,
                    jitter / 60,
                    interval.saturating_sub(jitter),
                    interval + jitter
                ),
            );

            ui.add_space(4.0);
            match last_pet {
                Some(ts) => hint(ui, &format!("Последняя отправка: {} ({})", local_hhmm(ts), ago(ts))),
                None => hint(ui, "Ещё не отправлялся — уйдёт при старте."),
            }
            match left {
                Some(d) => {
                    ui.label(
                        RichText::new(format!("Следующий поход через {}", fmt_dur(d)))
                            .color(ACCENT_LIGHT),
                    );
                }
                None => hint(ui, "Авто-поход выключен"),
            }

            ui.add_space(4.0);
            if ui
                .add(
                    Button::new("Отправить сейчас")
                        .min_size(Vec2::new(ui.available_width(), 26.0)),
                )
                .clicked()
            {
                let _ = self.tx.send(Command::PetNow);
            }
        });
    }

    /// «Папка» со всеми авто-командами: питомец и два таймера бонуса.
    fn autocommands_card(
        &mut self,
        ui: &mut egui::Ui,
        pet_left: Option<Duration>,
        last_pet: Option<i64>,
        bonus_left: Option<Duration>,
        last_bonus: Option<i64>,
        dm_left: Option<Duration>,
        last_dm: Option<i64>,
    ) {
        card(ui, "Автокоманды", |ui| {
            hint(
                ui,
                "Таймеры работают в фоне: питомец, бонус в канал и бонус в личку бота.",
            );
            ui.add_space(6.0);
            self.pet_card(ui, pet_left, last_pet);
            ui.add_space(8.0);
            self.bonus_card(ui, BonusKind::Channel, bonus_left, last_bonus);
            ui.add_space(8.0);
            self.bonus_card(ui, BonusKind::Dm, dm_left, last_dm);
        });
    }

    fn bonus_card(
        &mut self,
        ui: &mut egui::Ui,
        kind: BonusKind,
        left: Option<Duration>,
        last_bonus: Option<i64>,
    ) {
        let (title, target_hint) = match kind {
            BonusKind::Channel => ("Бонус в канал", "gorillabot_double"),
            BonusKind::Dm => ("Бонус в личку", "realgorillabot"),
        };
        let mut send_now = false;
        {
            let bonus = match kind {
                BonusKind::Channel => &mut self.cfg.bonus,
                BonusKind::Dm => &mut self.cfg.bonus_dm,
            };
            card(ui, title, |ui| {
                ui.checkbox(
                    &mut bonus.enabled,
                    RichText::new("Слать «Бонус» по расписанию")
                        .size(13.0)
                        .color(TEXT),
                );

                row(ui, "Чат / канал", |ui| {
                    wide_edit(ui, &mut bonus.target, target_hint);
                });
                row(ui, "Команда", |ui| {
                    wide_edit(ui, &mut bonus.command, "Бонус");
                });
                row(ui, "Разброс, мин", |ui| {
                    let mut minutes = (bonus.jitter_secs / 60) as i64;
                    if ui
                        .add(egui::DragValue::new(&mut minutes).speed(1.0).range(0..=120))
                        .changed()
                    {
                        bonus.jitter_secs = (minutes.max(0) as u64) * 60;
                    }
                });

                hint(
                    ui,
                    "Срок ищется в любом ответе бота: «12ч», «6 мин. 29 сек.», «через 3 часа».",
                );
                hint(
                    ui,
                    &format!(
                        "Если бот не назвал срок — запасные {} мин. Разброс ±{} мин.",
                        bonus.interval_secs / 60,
                        bonus.jitter_secs / 60
                    ),
                );

                ui.add_space(4.0);
                match last_bonus {
                    Some(ts) => hint(
                        ui,
                        &format!("Последняя отправка: {} ({})", local_hhmm(ts), ago(ts)),
                    ),
                    None => hint(ui, "Ещё не отправлялся — уйдёт при старте."),
                }
                match left {
                    Some(d) => {
                        ui.label(
                            RichText::new(format!("Следующий бонус через {}", fmt_dur(d)))
                                .color(ACCENT_LIGHT),
                        );
                    }
                    None => hint(ui, "Автоотправка выключена"),
                }

                ui.add_space(4.0);
                if ui
                    .add(
                        Button::new("Отправить сейчас")
                            .min_size(Vec2::new(ui.available_width(), 26.0)),
                    )
                    .clicked()
                {
                    send_now = true;
                }
            });
        }
        if send_now {
            let cmd = match kind {
                BonusKind::Channel => Command::BonusNow,
                BonusKind::Dm => Command::BonusDmNow,
            };
            let _ = self.tx.send(cmd);
        }
    }

    fn status_card(&mut self, ui: &mut egui::Ui, status: &Status) {
        Frame::new()
            .fill(CARD)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(14)
            .inner_margin(Margin::same(14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Статус").strong().size(14.5).color(TEXT));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let (text, color) = if status.casino_running {
                            ("казино идёт", WARN)
                        } else {
                            ("простой", MUTED)
                        };
                        ui.label(RichText::new(text).size(12.0).color(color));
                    });
                });

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    stat(ui, "Раунды", &status.rounds.to_string(), TEXT);
                    stat(ui, "Победы", &status.wins.to_string(), OK);
                    stat(
                        ui,
                        "Проигрыши подряд",
                        &status.losses_in_row.to_string(),
                        if status.losses_in_row > 0 { WARN } else { TEXT },
                    );
                    stat(ui, "Ставка", &status.bet.to_string(), ACCENT_LIGHT);
                });

                if !status.message.is_empty() {
                    ui.add_space(7.0);
                    ui.label(RichText::new(&status.message).size(13.0).color(MUTED));
                }
                if !status.last_bot_reply.is_empty() {
                    ui.add_space(7.0);
                    Frame::new()
                        .fill(FIELD)
                        .corner_radius(9)
                        .inner_margin(Margin::symmetric(10, 7))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(format!("бот: {}", status.last_bot_reply))
                                    .size(12.5)
                                    .color(TEXT_DIM),
                            );
                        });
                }
            });
    }

    fn log_card(&mut self, ui: &mut egui::Ui) {
        let logs = self.shared.logs();
        let height = ui.available_height().max(140.0);
        Frame::new()
            .fill(CARD)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(14)
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
}

/// Карточка-секция: скруглённая подложка с заголовком.
fn card(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(14)
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).strong().size(14.5).color(TEXT));
            ui.add_space(6.0);
            add(ui);
        });
    ui.add_space(9.0);
}

fn row(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [140.0, 22.0],
            Label::new(RichText::new(label).size(13.0).color(MUTED)),
        );
        add(ui);
    });
}

fn wide_edit(ui: &mut egui::Ui, text: &mut String, hint_text: &str) {
    let width = ui.available_width().max(90.0);
    ui.add(
        TextEdit::singleline(text)
            .hint_text(hint_text)
            .desired_width(width),
    );
}

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(11.5).color(MUTED));
}

fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    Frame::new()
        .fill(color.gamma_multiply(0.18))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.45)))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(9, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(12.0).color(color));
        });
}

fn stat(ui: &mut egui::Ui, label: &str, value: &str, color: Color32) {
    Frame::new()
        .fill(FIELD)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(12, 7))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(label).size(11.0).color(MUTED));
                ui.label(RichText::new(value).size(18.0).strong().color(color));
            });
        });
}

fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

fn local_hhmm(ts: i64) -> String {
    use time::OffsetDateTime;
    let offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    match OffsetDateTime::from_unix_timestamp(ts) {
        Ok(t) => {
            let t = t.to_offset(offset);
            format!(
                "{:02}.{:02} {:02}:{:02}",
                t.day(),
                t.month() as u8,
                t.hour(),
                t.minute()
            )
        }
        Err(_) => "—".to_string(),
    }
}

fn ago(ts: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let diff = (now - ts).max(0) as u64;
    if diff < 60 {
        "только что".to_string()
    } else if diff < 3600 {
        format!("{} мин назад", diff / 60)
    } else {
        format!("{} ч {} мин назад", diff / 3600, diff % 3600 / 60)
    }
}