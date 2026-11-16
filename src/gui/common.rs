use std::time::Duration;

use eframe::egui::{self, *};

// Тёмная палитра с фиолетовым акцентом.
pub(super) const BG: Color32 = Color32::from_rgb(15, 16, 22);
pub(super) const CARD: Color32 = Color32::from_rgb(26, 28, 37);
pub(super) const FIELD: Color32 = Color32::from_rgb(20, 22, 29);
pub(super) const BORDER: Color32 = Color32::from_rgb(40, 43, 55);
pub(super) const TEXT: Color32 = Color32::from_rgb(228, 231, 240);
pub(super) const TEXT_DIM: Color32 = Color32::from_rgb(196, 201, 214);
pub(super) const MUTED: Color32 = Color32::from_rgb(140, 146, 165);
pub(super) const ACCENT: Color32 = Color32::from_rgb(124, 92, 255);
pub(super) const ACCENT_LIGHT: Color32 = Color32::from_rgb(178, 160, 255);
pub(super) const ACCENT_DIM: Color32 = Color32::from_rgb(64, 50, 128);
pub(super) const OK: Color32 = Color32::from_rgb(80, 210, 130);
pub(super) const WARN: Color32 = Color32::from_rgb(240, 180, 80);
pub(super) const DANGER: Color32 = Color32::from_rgb(235, 90, 100);

/// Карточка-секция: скруглённая подложка с заголовком.
/// Круговой таймер до следующей отправки команды.
pub(super) fn timer_ring(
    ui: &mut egui::Ui,
    title: &str,
    enabled: bool,
    left: Option<Duration>,
    total_secs: u64,
    last: Option<i64>,
    color: Color32,
) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(164.0, 202.0), Sense::hover());
    let painter = ui.painter();
    let center = rect.center();
    let radius = 54.0;

    painter.circle_stroke(center, radius, Stroke::new(9.0, BORDER));

    let frac = match (enabled, left) {
        (false, _) => 0.0,
        (true, None) => 1.0,
        (true, Some(left)) => (1.0 - left.as_secs_f32() / total_secs.max(1) as f32).clamp(0.0, 1.0),
    };

    if frac > 0.004 {
        let steps = 96;
        let start = -std::f32::consts::FRAC_PI_2;
        let points: Vec<egui::Pos2> = (0..=steps)
            .map(|i| {
                let angle = start + frac * std::f32::consts::TAU * (i as f32 / steps as f32);
                egui::pos2(
                    center.x + radius * angle.cos(),
                    center.y + radius * angle.sin(),
                )
            })
            .collect();
        painter.add(egui::Shape::line(points, Stroke::new(9.0, color)));
    }

    let (main, sub) = match (enabled, left) {
        (false, _) => ("выкл".to_string(), "автокоманда".to_string()),
        (true, None) => ("—".to_string(), "ждёт запуска".to_string()),
        (true, Some(left)) => (
            fmt_dur(left),
            format!("из {}", fmt_dur(Duration::from_secs(total_secs))),
        ),
    };
    painter.text(
        center - Vec2::new(0.0, 6.0),
        Align2::CENTER_CENTER,
        main,
        FontId::proportional(18.0),
        TEXT,
    );
    painter.text(
        center + Vec2::new(0.0, 14.0),
        Align2::CENTER_CENTER,
        sub,
        FontId::proportional(10.5),
        MUTED,
    );

    painter.text(
        egui::pos2(center.x, rect.bottom() - 32.0),
        Align2::CENTER_TOP,
        title,
        FontId::proportional(14.0),
        TEXT,
    );
    painter.text(
        egui::pos2(center.x, rect.bottom() - 13.0),
        Align2::CENTER_TOP,
        match last {
            Some(ts) => format!("была в {}", local_hhmm(ts)),
            None => "ещё не запускалась".to_string(),
        },
        FontId::proportional(11.0),
        MUTED,
    );
}

/// Карточка-секция: скруглённая подложка с заголовком.
pub(super) fn card(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(6)
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).strong().size(14.5).color(TEXT));
            ui.add_space(6.0);
            add(ui);
        });
    ui.add_space(9.0);
}

pub(super) fn row(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [140.0, 22.0],
            Label::new(RichText::new(label).size(13.0).color(MUTED)),
        );
        add(ui);
    });
}

pub(super) fn wide_edit(ui: &mut egui::Ui, text: &mut String, hint_text: &str) {
    let width = ui.available_width().max(90.0);
    ui.add(
        TextEdit::singleline(text)
            .hint_text(hint_text)
            .desired_width(width),
    );
}

pub(super) fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(11.5).color(MUTED));
}

pub(super) fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    Frame::new()
        .fill(color.gamma_multiply(0.18))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.45)))
        .corner_radius(5)
        .inner_margin(Margin::symmetric(9, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(12.0).color(color));
        });
}

pub(super) fn stat(ui: &mut egui::Ui, label: &str, value: &str, color: Color32) {
    Frame::new()
        .fill(FIELD)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(5)
        .inner_margin(Margin::symmetric(12, 7))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(label).size(11.0).color(MUTED));
                ui.label(RichText::new(value).size(18.0).strong().color(color));
            });
        });
}

pub(super) fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

pub(super) fn local_hhmm(ts: i64) -> String {
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

pub(super) fn ago(ts: i64) -> String {
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
