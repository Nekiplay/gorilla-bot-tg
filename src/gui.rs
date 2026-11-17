//! Окно настроек на gpui-kit: слева рейл иконок, в центре страница,
//! справа журнал.
//!
//! Движок живёт отдельно (см. `engine.rs`), здесь только редактирование
//! config.json, кнопки и лог.

mod assets;
mod bonus;
mod business;
mod captcha;
mod casino;
mod charts;
mod common;
mod log;
mod overview;
mod pet;
mod printer;
mod profile;
mod proxy;
mod race;
mod telegram;
mod theme;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_resizable, resizable_panel};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::label::Label;
use gpui_kit::component::Selectable;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::component::{Icon, Sizable, Size};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, point, px, size, AnyElement, Bounds, ClickEvent, Context, Div, ElementId, Entity,
    FontWeight, IntoElement, ParentElement, Render, SharedString, Styled, Task, WeakEntity, Window,
    WindowBounds, WindowOptions,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::Config;
use crate::engine::{self, Command, Shared};

/// Размер рейла иконок по ширине.
pub(super) const RAIL: f32 = 56.0;
/// Ширина журнала по умолчанию и пределы перетаскивания.
pub(super) const LOG_WIDTH: f32 = 470.0;
const LOG_MIN: f32 = 340.0;
const LOG_MAX: f32 = 760.0;

/// Вкладки окна. Порядок — как в рейле сверху вниз.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Overview,
    Profile,
    Pet,
    Printer,
    Race,
    Bonus,
    Business,
    Casino,
    Safes,
    Captcha,
    Telegram,
    Proxy,
}

impl Tab {
    /// Все вкладки сверху вниз.
    const ALL: [Tab; 12] = [
        Tab::Overview,
        Tab::Profile,
        Tab::Pet,
        Tab::Printer,
        Tab::Race,
        Tab::Bonus,
        Tab::Business,
        Tab::Casino,
        Tab::Safes,
        Tab::Captcha,
        Tab::Telegram,
        Tab::Proxy,
    ];

    /// Подпись для шапки и тултипа.
    fn title(self) -> &'static str {
        match self {
            Tab::Overview => "Обзор",
            Tab::Profile => "Профиль",
            Tab::Pet => "Питомец",
            Tab::Printer => "Принтер",
            Tab::Race => "Гонка",
            Tab::Bonus => "Бонус",
            Tab::Business => "Бизнес",
            Tab::Casino => "Казино",
            Tab::Safes => "Сейфы",
            Tab::Captcha => "Капча",
            Tab::Telegram => "Telegram",
            Tab::Proxy => "Прокси",
        }
    }

    /// Иконка рейла.
    fn icon(self) -> IconName {
        match self {
            Tab::Overview => IconName::LayoutDashboard,
            Tab::Profile => IconName::User,
            Tab::Pet => IconName::PawPrint,
            Tab::Printer => IconName::Printer,
            Tab::Race => IconName::Flag,
            Tab::Bonus => IconName::Gift,
            Tab::Business => IconName::Briefcase,
            Tab::Casino => IconName::Dices,
            Tab::Safes => IconName::Vault,
            Tab::Captcha => IconName::ScanEye,
            Tab::Telegram => IconName::Send,
            Tab::Proxy => IconName::Network,
        }
    }

    /// Устойчивый ключ элемента в дереве.
    fn id(self) -> ElementId {
        ElementId::Name(format!("tab-{}", self.title()).into())
    }
}

/// Окно бота.
struct Bot {
    shared: Arc<Shared>,
    tx: UnboundedSender<Command>,
    cfg: Config,
    config_path: PathBuf,
    saved: String,
    tab: Tab,
    /// Поля ввода по ключу настройки: создаются по требованию страницы.
    fields: HashMap<&'static str, Entity<InputState>>,
    /// Свой код сейфа задан вручную (страница казино).
    captcha: captcha::Viewer,
    /// Тикер: раз в полсекунды обновляет таймеры и лог.
    _ticker: Task<()>,
    /// [dbg] ручка прокрутки страницы — временно, для замера.
    page_scroll: gpui_kit::ScrollHandle,
}

impl Bot {
    fn new(
        shared: Arc<Shared>,
        tx: UnboundedSender<Command>,
        cfg: Config,
        config_path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let _ = window;
        let ticker = cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            if this.update(cx, |_, cx| cx.notify()).is_err() {
                break;
            }
        });

        Self {
            shared,
            tx,
            cfg,
            config_path,
            saved: String::new(),
            page_scroll: gpui_kit::ScrollHandle::new(),
            tab: Tab::Overview,
            fields: HashMap::new(),
            captcha: captcha::Viewer::default(),
            _ticker: ticker,
        }
    }

    /// Отправляет команду движку.
    fn send(&self, command: Command) {
        let _ = self.tx.send(command);
    }

    /// Записывает config.json и отдаёт новые настройки движку.
    fn save(&mut self) {
        if self.cfg.api_id == 0 || self.cfg.api_hash.trim().is_empty() {
            self.saved = "API ID и API hash обязательны".into();
        } else {
            match serde_json::to_string_pretty(&self.cfg)
                .map_err(|e| e.to_string())
                .and_then(|json| std::fs::write(&self.config_path, json).map_err(|e| e.to_string()))
            {
                // Надпись под заголовком больше не показываем: сохранение молчаливое.
                Ok(()) => self.saved.clear(),
                Err(e) => self.saved = format!("Ошибка записи: {e}"),
            }
        }
        self.send(Command::SetConfig(self.cfg.clone()));
    }

    /// Поле ввода настройки: создаётся при первом обращении и пишет в cfg.
    fn field(
        &mut self,
        key: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if let Some(state) = self.fields.get(key) {
            return state.clone();
        }

        let value = self.value(key);
        let placeholder = placeholder(key).to_string();
        // Пароли 2FA показываем точками, как старый egui.
        let masked = key.ends_with(".password");
        let state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder)
                .default_value(value)
                .masked(masked)
        });
        cx.subscribe_in(
            &state,
            window,
            move |this: &mut Bot, state, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = state.read(cx).value().to_string();
                    this.apply(key, &text);
                    cx.notify();
                }
            },
        )
        .detach();

        self.fields.insert(key, state.clone());
        state
    }

    /// Текст, введённый в поле страницы (не настройка — живёт только в поле).
    fn field_text(&self, key: &str, cx: &Context<Self>) -> String {
        self.fields
            .get(key)
            .map(|state| state.read(cx).value().to_string())
            .unwrap_or_default()
    }

    /// Текущее значение настройки строкой — для поля ввода.
    fn value(&self, key: &str) -> String {
        match key {
            "api_id" => self.cfg.api_id.to_string(),
            "api_hash" => self.cfg.api_hash.clone(),
            "phone" => self.cfg.phone.clone(),
            "bot_username" => self.cfg.bot_username.clone(),
            "session_file" => self.cfg.session_file.clone(),
            "player_name" => self.cfg.player_name.clone(),
            "proxy" => self.cfg.proxy.clone().unwrap_or_default(),
            "mtproxy.host" => self.cfg.mtproxy.host.clone(),
            "mtproxy.port" => self.cfg.mtproxy.port.to_string(),
            "mtproxy.secret" => self.cfg.mtproxy.secret.clone(),
            "mtproxy.tgws" => self.cfg.mtproxy.tgws.clone(),
            "mtproxy.tgws_secret" => self.cfg.mtproxy.tgws_secret.clone(),
            "casino.command" => self.cfg.casino.command.clone(),
            "casino.start_bet" => self.cfg.casino.start_bet.to_string(),
            "casino.max_bet" => self.cfg.casino.max_bet.to_string(),
            "casino.bet_round_to" => self.cfg.casino.bet_round_to.to_string(),
            "casino.multiplier" => self.cfg.casino.multiplier.to_string(),
            "casino.multiplier_jitter" => self.cfg.casino.multiplier_jitter.to_string(),
            "casino.max_losses_in_row" => self.cfg.casino.max_losses_in_row.to_string(),
            "casino.round_gap_secs" => self.cfg.casino.round_gap_secs.to_string(),
            "casino.round_gap_jitter_secs" => self.cfg.casino.round_gap_jitter_secs.to_string(),
            "casino.reply_timeout_secs" => self.cfg.casino.reply_timeout_secs.to_string(),
            "casino.cooldown_margin_secs" => self.cfg.casino.cooldown_margin_secs.to_string(),
            "safe.command" => self.cfg.safe.command.clone(),
            "safe.code" => self
                .cfg
                .safe
                .code
                .map(|c| c.to_string())
                .unwrap_or_default(),
            "safe.delay_secs" => self.cfg.safe.delay_secs.to_string(),
            "pet.command" => self.cfg.pet.command.clone(),
            "pet.feed_command" => self.cfg.pet.feed_command.clone(),
            "pet.interval_secs" => self.cfg.pet.interval_secs.to_string(),
            "pet.interval_jitter_secs" => self.cfg.pet.interval_jitter_secs.to_string(),
            "pet.reply_timeout_secs" => self.cfg.pet.reply_timeout_secs.to_string(),
            "printer.command" => self.cfg.printer.command.clone(),
            "printer.refill_command" => self.cfg.printer.refill_command.clone(),
            "printer.withdraw_command" => self.cfg.printer.withdraw_command.clone(),
            "printer.refill_below_percent" => self.cfg.printer.refill_below_percent.to_string(),
            "printer.interval_secs" => self.cfg.printer.interval_secs.to_string(),
            "printer.interval_jitter_secs" => self.cfg.printer.interval_jitter_secs.to_string(),
            "printer.reply_timeout_secs" => self.cfg.printer.reply_timeout_secs.to_string(),
            "race.command" => self.cfg.race.command.clone(),
            "race.interval_secs" => self.cfg.race.interval_secs.to_string(),
            "race.interval_jitter_secs" => self.cfg.race.interval_jitter_secs.to_string(),
            "race.reply_timeout_secs" => self.cfg.race.reply_timeout_secs.to_string(),
            "race.cooldown_margin_secs" => self.cfg.race.cooldown_margin_secs.to_string(),
            "race.result_timeout_secs" => self.cfg.race.result_timeout_secs.to_string(),
            "bonus.target" => self.cfg.bonus.target.clone(),
            "bonus.command" => self.cfg.bonus.command.clone(),
            "bonus.jitter_secs" => (self.cfg.bonus.jitter_secs / 60).to_string(),
            "bonus_dm.target" => self.cfg.bonus_dm.target.clone(),
            "bonus_dm.command" => self.cfg.bonus_dm.command.clone(),
            "bonus_dm.jitter_secs" => (self.cfg.bonus_dm.jitter_secs / 60).to_string(),
            "business.command" => self.cfg.business.command.clone(),
            "business.interval_secs" => self.cfg.business.interval_secs.to_string(),
            "business.interval_jitter_secs" => self.cfg.business.interval_jitter_secs.to_string(),
            "business.reply_timeout_secs" => self.cfg.business.reply_timeout_secs.to_string(),
            _ => String::new(),
        }
    }

    /// Применяет введённый текст к cfg. Некорректное число оставляет старое.
    fn apply(&mut self, key: &str, text: &str) {
        let text = text.trim();
        match key {
            "api_id" => {
                if let Some(v) = parse_i32(text) {
                    self.cfg.api_id = v;
                }
            }
            "api_hash" => self.cfg.api_hash = text.to_string(),
            "phone" => self.cfg.phone = text.to_string(),
            "bot_username" => self.cfg.bot_username = text.to_string(),
            "session_file" => self.cfg.session_file = text.to_string(),
            "player_name" => self.cfg.player_name = text.to_string(),
            "proxy" => {
                self.cfg.proxy = if text.is_empty() {
                    None
                } else {
                    Some(text.to_string())
                }
            }
            "mtproxy.host" => self.cfg.mtproxy.host = text.to_string(),
            "mtproxy.port" => {
                if let Some(v) = parse_u16(text) {
                    self.cfg.mtproxy.port = v;
                }
            }
            "mtproxy.secret" => self.cfg.mtproxy.secret = text.to_string(),
            "mtproxy.tgws" => self.cfg.mtproxy.tgws = text.to_string(),
            "mtproxy.tgws_secret" => self.cfg.mtproxy.tgws_secret = text.to_string(),
            "casino.command" => self.cfg.casino.command = text.to_string(),
            "casino.start_bet" => {
                if let Some(v) = parse_u64(text) {
                    self.cfg.casino.start_bet = v.max(1);
                }
            }
            "casino.max_bet" => {
                if let Some(v) = parse_u64(text) {
                    self.cfg.casino.max_bet = v.max(1);
                }
            }
            "casino.bet_round_to" => {
                if let Some(v) = parse_u64(text) {
                    self.cfg.casino.bet_round_to = v;
                }
            }
            "casino.multiplier" => {
                if let Some(v) = parse_f64(text) {
                    self.cfg.casino.multiplier = v.max(1.0);
                }
            }
            "casino.multiplier_jitter" => {
                if let Some(v) = parse_f64(text) {
                    self.cfg.casino.multiplier_jitter = v.clamp(0.0, 0.9);
                }
            }
            "casino.max_losses_in_row" => {
                if let Some(v) = parse_u32(text) {
                    self.cfg.casino.max_losses_in_row = v.max(1);
                }
            }
            "casino.round_gap_secs" => {
                if let Some(v) = parse_u64(text) {
                    self.cfg.casino.round_gap_secs = v;
                }
            }
            "casino.round_gap_jitter_secs" => {
                if let Some(v) = parse_u64(text) {
                    self.cfg.casino.round_gap_jitter_secs = v;
                }
            }
            "casino.reply_timeout_secs" => {
                if let Some(v) = parse_u64(text) {
                    self.cfg.casino.reply_timeout_secs = v.max(1);
                }
            }
            "casino.cooldown_margin_secs" => {
                if let Some(v) = parse_u64(text) {
                    self.cfg.casino.cooldown_margin_secs = v;
                }
            }
            "safe.command" => self.cfg.safe.command = text.to_string(),
            "safe.code" => {
                // Поле кода живо только при включённом «слать всегда».
                if self.cfg.safe.code.is_some() {
                    self.cfg.safe.code = parse_u32(text)
                        .filter(|v| (crate::safe::MIN_CODE..=crate::safe::MAX_CODE).contains(v));
                }
            }
            "safe.delay_secs" => {
                if let Some(v) = parse_f64(text) {
                    self.cfg.safe.delay_secs = v.clamp(1.0, 120.0);
                }
            }
            "pet.command" => self.cfg.pet.command = text.to_string(),
            "pet.feed_command" => self.cfg.pet.feed_command = text.to_string(),
            "pet.interval_secs" => {
                if let Some(v) = parse_u64(text) {
                    self.cfg.pet.interval_secs = v.max(60);
                }
            }
            "pet.interval_jitter_secs" => {
                if let Some(v) = parse_u64(text) {
                    self.cfg.pet.interval_jitter_secs = v;
                }
            }
            "pet.reply_timeout_secs" => {
                if let Some(v) = parse_u64(text) {
                    self.cfg.pet.reply_timeout_secs = v.max(5);
                }
            }
            "printer.command" => self.cfg.printer.command = text.to_string(),
            "printer.refill_command" => self.cfg.printer.refill_command = text.to_string(),
            "printer.withdraw_command" => self.cfg.printer.withdraw_command = text.to_string(),
            "printer.refill_below_percent" => {
                if let Some(v) = parse_u8(text) {
                    self.cfg.printer.refill_below_percent = v.clamp(1, 100);
                }
            }
            "printer.interval_secs" => {
                if let Some(v) = parse_u64(text) {
                    self.cfg.printer.interval_secs = v.max(60);
                }
            }
            "printer.interval_jitter_secs" => {
                if let Some(v) = parse_u64(text) {
                    self.cfg.printer.interval_jitter_secs = v;
                }
            }
            "printer.reply_timeout_secs" => {
                if let Some(v) = parse_u64(text) {
                    self.cfg.printer.reply_timeout_secs = v.max(5);
                }
            }
            "race.command" => self.cfg.race.command = text.to_string(),
            "race.interval_secs" => {
                if let Some(v) = parse_u64(text) {
                    self.cfg.race.interval_secs = v.max(60);
                }
            }
            "race.interval_jitter_secs" => {
                if let Some(v) = parse_u64(text) {
                    self.cfg.race.interval_jitter_secs = v;
                }
            }
            "race.reply_timeout_secs" => {
                if let Some(v) = parse_u64(text) {
                    self.cfg.race.reply_timeout_secs = v.max(5);
                }
            }
            "race.cooldown_margin_secs" => {
                if let Some(v) = parse_u64(text) {
                    self.cfg.race.cooldown_margin_secs = v;
                }
            }
            "race.result_timeout_secs" => {
                if let Some(v) = parse_u64(text) {
                    self.cfg.race.result_timeout_secs = v.max(30);
                }
            }
            "bonus.target" => self.cfg.bonus.target = text.to_string(),
            "bonus.command" => self.cfg.bonus.command = text.to_string(),
            "bonus.jitter_secs" => {
                if let Some(v) = parse_u64(text) {
                    self.cfg.bonus.jitter_secs = v.min(120) * 60;
                }
            }
            "bonus_dm.target" => self.cfg.bonus_dm.target = text.to_string(),
            "bonus_dm.command" => self.cfg.bonus_dm.command = text.to_string(),
            "bonus_dm.jitter_secs" => {
                if let Some(v) = parse_u64(text) {
                    self.cfg.bonus_dm.jitter_secs = v.min(120) * 60;
                }
            }
            "business.command" => self.cfg.business.command = text.to_string(),
            "business.interval_secs" => {
                if let Some(v) = parse_u64(text) {
                    self.cfg.business.interval_secs = v.max(60);
                }
            }
            "business.interval_jitter_secs" => {
                if let Some(v) = parse_u64(text) {
                    self.cfg.business.interval_jitter_secs = v;
                }
            }
            "business.reply_timeout_secs" => {
                if let Some(v) = parse_u64(text) {
                    self.cfg.business.reply_timeout_secs = v.max(5);
                }
            }
            _ => {}
        }
    }

    /// Переключатель: чекбокс, который пишет флаг в cfg.
    fn flag(
        &self,
        id: &'static str,
        label: &'static str,
        checked: bool,
        cx: &Context<Self>,
    ) -> Checkbox {
        let this = cx.entity().downgrade();
        Checkbox::new(id)
            .label(label)
            .checked(checked)
            .on_change(move |on, window, cx| {
                let _ = this.update(cx, |this: &mut Bot, cx| {
                    this.set_flag(id, *on);
                    // Поле кода сейфа живёт только при включённом флаге: держим его
                    // текст в согласии с cfg, иначе при включении покажется пустота.
                    if id == "safe-fixed" {
                        let text = SharedString::from(
                            this.cfg
                                .safe
                                .code
                                .map(|c| c.to_string())
                                .unwrap_or_default(),
                        );
                        let state = this.field("safe.code", window, cx);
                        state.update(cx, |state, cx| state.set_value(text, window, cx));
                    }
                    cx.notify();
                });
            })
    }

    /// Кнопка действия: пишет настройки в файл и шлёт команду движку.
    fn action(
        &self,
        id: &'static str,
        label: &'static str,
        command: impl Fn() -> Command + 'static,
        cx: &Context<Self>,
    ) -> Button {
        let this = cx.entity().downgrade();
        Button::new(id)
            .label(label)
            .on_click(move |_: &ClickEvent, _window, cx| {
                let _ = this.update(cx, |this: &mut Bot, cx| {
                    this.save();
                    let command = command();
                    this.send(command);
                    cx.notify();
                });
            })
    }

    /// Кнопка действия с первичной подсветкой.
    fn primary_action(
        &self,
        id: &'static str,
        label: &'static str,
        command: impl Fn() -> Command + 'static,
        cx: &Context<Self>,
    ) -> Button {
        self.action(id, label, command, cx).primary()
    }
    /// Применяет флаг настройки.
    fn set_flag(&mut self, key: &str, on: bool) {
        match key {
            "pet.enabled" => self.cfg.pet.enabled = on,
            "pet.feed_when_hungry" => self.cfg.pet.feed_when_hungry = on,
            "printer.enabled" => self.cfg.printer.enabled = on,
            "printer.auto_refill" => self.cfg.printer.auto_refill = on,
            "printer.auto_withdraw" => self.cfg.printer.auto_withdraw = on,
            "race.enabled" => self.cfg.race.enabled = on,
            "bonus.enabled" => self.cfg.bonus.enabled = on,
            "bonus_dm.enabled" => self.cfg.bonus_dm.enabled = on,
            "business.enabled" => self.cfg.business.enabled = on,
            "safe-fixed" => {
                self.cfg.safe.code = if on {
                    Some(self.cfg.safe.code.unwrap_or(42))
                } else {
                    None
                }
            }
            _ => {}
        }
    }

    /// Страница текущей вкладки.
    fn page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.tab {
            Tab::Overview => overview::page(self, window, cx),
            Tab::Profile => profile::page(self, window, cx),
            Tab::Pet => pet::page(self, window, cx),
            Tab::Printer => printer::page(self, window, cx),
            Tab::Race => race::page(self, window, cx),
            Tab::Bonus => bonus::page(self, window, cx),
            Tab::Business => business::page(self, window, cx),
            Tab::Casino => casino::page(self, window, cx),
            Tab::Safes => casino::safe_page(self, window, cx),
            Tab::Captcha => captcha::page(self, window, cx),
            Tab::Telegram => telegram::page(self, window, cx),
            Tab::Proxy => proxy::page(self, window, cx),
        }
    }

    /// Рейл иконок: одна кнопка на вкладку, подпись — в тултипе.
    fn rail(&self, cx: &mut Context<Self>) -> Div {
        let this = cx.entity().downgrade();
        let mut rail = v_flex()
            .w(px(RAIL))
            .h_full()
            .flex_none()
            .items_center()
            .gap_1()
            .py_3()
            .bg(theme::card(&*cx))
            .border_r_1()
            .border_color(theme::border(&*cx))
            .child(
                div()
                    .w(px(RAIL - 12.0))
                    .h(px(44.))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .child(
                        Icon::default()
                            .path("icons/gorilla.svg")
                            .with_size(Size::Large)
                            .text_color(theme::accent_light(&*cx)),
                    ),
            );

        for tab in Tab::ALL {
            let active = self.tab == tab;
            let weak: WeakEntity<Bot> = this.clone();
            rail = rail.child(
                Button::new(tab.id())
                    .ghost()
                    .selected(active)
                    .w(px(RAIL - 12.0))
                    .h(px(38.))
                    .cursor_pointer()
                    .icon(
                        Icon::new(tab.icon())
                            .with_size(Size::Medium)
                            .text_color(if active {
                                theme::text(&*cx)
                            } else {
                                theme::muted(&*cx)
                            }),
                    )
                    .tooltip(tab.title())
                    .on_click(move |_: &ClickEvent, _window, cx| {
                        let _ = weak.update(cx, |this: &mut Bot, cx| {
                            this.tab = tab;
                            cx.notify();
                        });
                    }),
            );
        }

        rail
    }

    /// Шапка: название страницы, состояние и «Сохранить».
    fn header(&self, cx: &mut Context<Self>) -> Div {
        let status = self.shared.status();
        let this = cx.entity().downgrade();

        let mut pills = h_flex().items_center().gap_2();
        if status.casino_running {
            pills = pills.child(common::pill(&*cx, "казино идёт", theme::warn(&*cx)));
        }
        let (online, color) = if status.authorized {
            ("● онлайн", theme::ok(&*cx))
        } else if status.connected {
            ("● подключён", theme::warn(&*cx))
        } else {
            ("● офлайн", theme::muted(&*cx))
        };
        pills = pills.child(common::pill(&*cx, online, color));

        let mut header = v_flex()
            .w_full()
            .flex_none()
            .gap_1()
            .px_4()
            .py_3()
            .bg(theme::card(&*cx))
            .border_b_1()
            .border_color(theme::border(&*cx))
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_3()
                    .child(
                        Label::new(self.tab.title())
                            .text_size(px(19.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text(&*cx)),
                    )
                    .child(div().flex_1())
                    .child(pills)
                    .child(
                        Button::new("save-config")
                            .primary()
                            .label("Сохранить")
                            .on_click(move |_: &ClickEvent, _window, cx| {
                                let _ = this.update(cx, |this: &mut Bot, cx| {
                                    this.save();
                                    cx.notify();
                                });
                            }),
                    ),
            );

        if !self.saved.is_empty() {
            header = header.child(common::hint(&*cx, self.saved.clone()));
        }

        header
    }
}

impl Render for Bot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = self.page(window, cx);
        let rail = self.rail(cx);
        let header = self.header(cx);
        let log_panel = log::page_body(&self.shared, cx);

        h_flex()
            .size_full()
            .bg(theme::background(&*cx))
            .text_color(theme::text(&*cx))
            .child(rail)
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .min_h_0()
                    .child(header)
                    .child(
                        h_resizable("main")
                            .child(
                                resizable_panel().child(
                                    // Прокрутка страницы.
                                    // Внешний flex-столбец задаёт определённую высоту окна,
                                    // внутренний элемент с flex_1 получает остаток высоты
                                    // и уже сам прокручивает содержимое (иначе область
                                    // прокрутки равна высоте окна и низ страницы недоступен).
                                    v_flex().size_full().min_h_0().child(
                                        v_flex()
                                            .id("page-scroll")
                                            .flex_1()
                                            .min_h_0()
                                            .w_full()
                                            .track_scroll(&self.page_scroll)
                                            .overflow_y_scroll()
                                            .scrollbar_width(px(8.))
                                            .px_4()
                                            .pt_4()
                                            .pb_16()
                                            .child(page),
                                    ),
                                ),
                            )
                            .child(
                                resizable_panel()
                                    .size(px(LOG_WIDTH))
                                    .size_range(px(LOG_MIN)..px(LOG_MAX))
                                    .child(div().size_full().p_3().child(log_panel)),
                            ),
                    ),
            )
    }
}

/// Подсказка в пустом поле ввода.
fn placeholder(key: &str) -> &'static str {
    match key {
        "api_id" => "1234567",
        "api_hash" => "0123456789abcdef0123456789abcdef",
        "phone" => "+79991234567",
        "bot_username" => "realgorillabot",
        "session_file" => "gorilla.session",
        "player_name" => "ваш ник в боте",
        "proxy" => "socks5://user:pass@host:port",
        "mtproxy.host" => "127.0.0.1",
        "mtproxy.port" => "8444",
        "mtproxy.secret" => "hex-секрет",
        "mtproxy.tgws" => "127.0.0.1:1443",
        "mtproxy.tgws_secret" => "hex-секрет TgWsProxy",
        "casino.command" => "Казино {bet}",
        "safe.command" => "Сейф {code}",
        "pet.command" => "Питомец поход",
        "pet.feed_command" => "Питомец покормить",
        "printer.command" => "Принтер",
        "printer.refill_command" => "Принтер заправить",
        "printer.withdraw_command" => "Принтер снять",
        "race.command" => "Гонка",
        "bonus.command" | "bonus_dm.command" => "Бонус",
        "business.command" => "Бизнес снять",
        _ => "",
    }
}

fn parse_u64(text: &str) -> Option<u64> {
    text.parse().ok()
}

fn parse_u32(text: &str) -> Option<u32> {
    text.parse().ok()
}

fn parse_u16(text: &str) -> Option<u16> {
    text.parse().ok()
}

fn parse_u8(text: &str) -> Option<u8> {
    text.parse().ok()
}

fn parse_i32(text: &str) -> Option<i32> {
    text.parse().ok()
}

fn parse_f64(text: &str) -> Option<f64> {
    text.parse().ok()
}

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
    engine::spawn(shared.clone(), rx);

    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(120.), px(90.)),
            size: size(px(1120.), px(760.)),
        })),
        titlebar: Some(gpui_kit::TitlebarOptions {
            title: Some(SharedString::from("Gorilla Bot")),
            ..Default::default()
        }),
        ..Default::default()
    };

    let app = gpui_kit::application().with_assets(assets::Assets);
    app.run(move |cx| {
        gpui_kit::init(cx);
        theme::install(cx);

        let tx = tx.clone();
        if let Err(error) = gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| {
                Bot::new(
                    shared.clone(),
                    tx.clone(),
                    cfg.clone(),
                    config_path.clone(),
                    window,
                    cx,
                )
            })
        }) {
            eprintln!("Не удалось открыть окно: {error}");
            eprintln!("Можно работать из консоли: gorilla-bot casino | balance | check");
        }
    });
}
