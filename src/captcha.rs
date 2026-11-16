//! Капча Bot Gorilla: «выбери картинки, где изображены машины».
//!
//! Бот присылает одну картинку-сетку, а варианты выносит в inline-кнопки
//! «1»…«9» и «Готово»: сначала отмечаем нужные плитки, потом жмём «Готово».
//! Здесь живёт всё, что нужно, чтобы не кликать руками:
//!
//! * нарезка картинки на плитки (сетку ищем по однотонным разделителям);
//! * признаки плитки и своя модель (логистическая регрессия);
//! * обучение на разметке пользователя — каждая пройденная капча пополняет
//!   датасет, после чего модель начинает подсказывать плитки сама.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use image::imageops::FilterType;
use image::RgbaImage;

/// Сторона уменьшенной копии плитки для признаков.
const FEATURE_SIDE: u32 = 16;
/// Сколько корзин на цветовой канал в гистограмме.
const BINS: usize = 8;
/// Размерность вектора признаков: яркость + три гистограммы.
pub const FEATURES: usize = (FEATURE_SIDE * FEATURE_SIDE) as usize + 3 * BINS;
/// Меньше этой разметки модель молчит: на десятке плиток она угадывает случайно.
pub const MIN_SAMPLES: usize = 24;
/// Кнопка подтверждения выбора.
const DONE_LABEL: &str = "готов";

/// Inline-кнопка под сообщением с капчей.
#[derive(Clone, Debug)]
pub struct Button {
    pub text: String,
    pub data: Vec<u8>,
}

impl Button {
    /// Номер плитки, если кнопка подписана числом.
    pub fn tile(&self) -> Option<usize> {
        self.text.trim().parse::<usize>().ok().filter(|n| *n > 0)
    }

    /// Та самая кнопка «Готово».
    pub fn is_done(&self) -> bool {
        self.text.trim().to_lowercase().starts_with(DONE_LABEL)
    }
}

/// Разобранная капча: что показать в окне и что отправить боту.
pub struct Captcha {
    pub message_id: i32,
    pub prompt: String,
    pub rows: usize,
    pub cols: usize,
    pub tile_w: u32,
    pub tile_h: u32,
    /// Плитки в порядке слева-направо, сверху-вниз; RGBA-пиксели.
    pub tiles: Vec<Vec<u8>>,
    pub buttons: Vec<Button>,
    /// Вероятность «нужная плитка» по версии модели (0.0, если модели нет).
    pub guess: Vec<f32>,
}

impl Captcha {
    /// Плитки, которые модель считает нужными.
    pub fn suggested(&self) -> Vec<bool> {
        self.guess.iter().map(|p| *p >= 0.5).collect()
    }

    /// Умеет ли модель хоть что-то подсказывать.
    pub fn has_guess(&self) -> bool {
        self.guess.iter().any(|p| *p > 0.0)
    }
}

/// Похоже ли сообщение бота на капчу.
pub fn is_captcha(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("капч") || lower.contains("captcha")
}

/// Плитки из переотредактированного ботом текста: «Выбрано: 1, 4».
pub fn selected(text: &str) -> Option<Vec<usize>> {
    let lower = text.to_lowercase();
    let start = lower.find("выбрано")?;
    let line = text[start..].lines().next().unwrap_or("");
    let tail = line.split_once(':').map(|(_, tail)| tail).unwrap_or("");
    let tiles: Vec<usize> = tail
        .split([',', ' ', '.'])
        .filter_map(|token| token.trim().parse::<usize>().ok())
        .filter(|tile| *tile > 0)
        .collect();
    if tiles.is_empty() {
        None
    } else {
        Some(tiles)
    }
}

/// Разбирает картинку капчи: сетка + плитки.
pub fn parse(message_id: i32, prompt: String, image: &[u8], buttons: Vec<Button>) -> Result<Captcha, String> {
    let img = trim_padding(
        image::load_from_memory(image)
            .map_err(|e| format!("картинка капчи не читается: {e}"))?
            .to_rgba8(),
    );
    let (cols, rows) = grid(&img);
    let tile_w = img.width() / cols as u32;
    let tile_h = img.height() / rows as u32;
    if tile_w == 0 || tile_h == 0 {
        return Err("капча пустая".into());
    }
    let mut tiles = Vec::with_capacity(cols * rows);
    for row in 0..rows as u32 {
        for col in 0..cols as u32 {
            let tile = image::imageops::crop_imm(
                &img,
                col * tile_w,
                row * tile_h,
                tile_w,
                tile_h,
            )
            .to_image();
            tiles.push(tile.into_raw());
        }
    }
    let mut captcha = Captcha {
        message_id,
        prompt,
        rows,
        cols,
        tile_w,
        tile_h,
        tiles,
        buttons,
        guess: Vec::new(),
    };
    if let Some(model) = Model::load(&model_dir()) {
        if model.samples >= MIN_SAMPLES {
            captcha.guess = captcha
                .tiles
                .iter()
                .map(|tile| model.predict(&features(tile, captcha.tile_w, captcha.tile_h)))
                .collect();
        }
    }
    Ok(captcha)
}

/// Сколько пикселей подложки у капчи сверху и снизу.
const PAD_PX: u32 = 25;

/// В капче сверху и снизу по 25 px фона — это не плитки, их отрезаем.
fn trim_padding(img: RgbaImage) -> RgbaImage {
    let height = img.height();
    if height <= PAD_PX * 2 + 40 {
        return img;
    }
    image::imageops::crop_imm(&img, 0, PAD_PX, img.width(), height - PAD_PX * 2).to_image()
}

/// Сколько столбцов и строк в сетке.
///
/// Однотонные полосы между плитками дают разрезы; если их не нашлось —
/// делим картинку на 3×3 (обычная раскладка) или на 2×2 для неквадратных.
fn grid(img: &RgbaImage) -> (usize, usize) {
    let cols = tiles_along(img, true);
    let rows = tiles_along(img, false);
    let fallback = if img.width() == img.height() { 3 } else { 2 };
    let cols = cols.unwrap_or(fallback);
    let rows = rows.unwrap_or(fallback);
    (cols.clamp(1, 5), rows.clamp(1, 5))
}

/// Сколько плиток по оси: полос разреза всегда на одну меньше.
fn tiles_along(img: &RgbaImage, along_x: bool) -> Option<usize> {
    match cuts(img, along_x) {
        0 => None,
        bands => Some((bands + 1).clamp(1, 5)),
    }
}

/// Сколько плиток по оси: считаем однотонные полосы разрезами.
fn cuts(img: &RgbaImage, along_x: bool) -> usize {
    let (len, other) = if along_x {
        (img.width(), img.height())
    } else {
        (img.height(), img.width())
    };
    let mut flat = vec![false; len as usize];
    for i in 0..len {
        let mut lo = u8::MAX;
        let mut hi = 0u8;
        for j in 0..other {
            let px = if along_x { img.get_pixel(i, j) } else { img.get_pixel(j, i) };
            lo = lo.min(px[0]).min(px[1]).min(px[2]);
            hi = hi.max(px[0]).max(px[1]).max(px[2]);
        }
        flat[i as usize] = hi.saturating_sub(lo) < 10;
    }
    // Полосы разреза: непрерывные участки однотонных линий внутри картинки.
    let mut bands = 0usize;
    let mut in_band = false;
    for (i, is_flat) in flat.iter().enumerate() {
        let border = i < 2 || (i as u32) + 2 >= len;
        if *is_flat && !border {
            if !in_band {
                in_band = true;
                bands += 1;
            }
        } else {
            in_band = false;
        }
    }
    bands
}

/// Признаки плитки: яркость 16×16 и гистограммы каналов.
pub fn features(rgba: &[u8], w: u32, h: u32) -> Vec<f32> {
    let mut out = vec![0.0f32; FEATURES];
    if w == 0 || h == 0 {
        return out;
    }
    let img = RgbaImage::from_raw(w, h, rgba.to_vec()).unwrap_or_else(|| RgbaImage::new(1, 1));
    let small = image::imageops::resize(&img, FEATURE_SIDE, FEATURE_SIDE, FilterType::Triangle);
    let mut hist = [[0.0f32; BINS]; 3];
    let total = (w * h) as f32;
    for px in img.pixels() {
        for (c, bin) in hist.iter_mut().enumerate() {
            let idx = ((px[c] as usize) * BINS) / 256;
            bin[idx.min(BINS - 1)] += 1.0;
        }
    }
    for (y, row) in small.rows().enumerate() {
        for (x, px) in row.enumerate() {
            let gray = (0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32) / 255.0;
            out[y * FEATURE_SIDE as usize + x] = gray - 0.5;
        }
    }
    let base = (FEATURE_SIDE * FEATURE_SIDE) as usize;
    for (c, bins) in hist.iter().enumerate() {
        for (b, count) in bins.iter().enumerate() {
            out[base + c * BINS + b] = count / total;
        }
    }
    out
}

/// Папка с моделью и разметкой.
pub fn model_dir() -> PathBuf {
    crate::tdata_path().join("captcha")
}

/// Логистическая регрессия: «нужная плитка» против «не нужная».
pub struct Model {
    pub bias: f32,
    pub w: Vec<f32>,
    /// Сколько плиток модель видела при обучении.
    pub samples: usize,
}

impl Model {
    fn empty() -> Self {
        Self {
            bias: 0.0,
            w: vec![0.0; FEATURES],
            samples: 0,
        }
    }

    /// Читает `model.bin`; отсутствие или порча файла — просто пустая модель.
    pub fn load(dir: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(dir.join("model.bin")).ok()?;
        let mut lines = text.lines();
        let meta = lines.next()?;
        let samples = meta.split_whitespace().nth(2)?.parse().ok()?;
        let bias = lines.next()?.trim().parse().ok()?;
        let mut w = Vec::with_capacity(FEATURES);
        for line in lines {
            for part in line.split_whitespace() {
                w.push(part.parse().ok()?);
            }
        }
        if w.len() != FEATURES {
            return None;
        }
        Some(Self { bias, w, samples })
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let mut text = format!("gorilla-captcha 1 {}\n{}\n", self.samples, self.bias);
        for (i, w) in self.w.iter().enumerate() {
            if i % 16 == 0 && i > 0 {
                text.push('\n');
            } else if i > 0 {
                text.push(' ');
            }
            let _ = write!(text, "{w:.5}");
        }
        text.push('\n');
        std::fs::write(dir.join("model.bin"), text)
    }

    /// Вероятность, что плитка нужная.
    pub fn predict(&self, x: &[f32]) -> f32 {
        let mut sum = self.bias;
        for (w, v) in self.w.iter().zip(x) {
            sum += w * v;
        }
        1.0 / (1.0 + (-sum).exp())
    }
}

/// Запоминает разметку одной плитки: её признаки и правильный ответ.
pub fn remember(dir: &Path, x: &[f32], needed: bool) -> std::io::Result<()> {
    use std::io::Write as _;
    std::fs::create_dir_all(dir)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("samples.txt"))?;
    let mut line = if needed { String::from("1") } else { String::from("0") };
    for v in x {
        let _ = write!(line, " {v:.5}");
    }
    line.push('\n');
    file.write_all(line.as_bytes())
}

/// Переобучает модель по всей накопленной разметке.
pub fn train(dir: &Path) -> Option<Model> {
    let text = std::fs::read_to_string(dir.join("samples.txt")).ok()?;
    let mut samples: Vec<(Vec<f32>, bool)> = Vec::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(label) = parts.next() else { continue };
        let needed = label == "1";
        let x: Vec<f32> = match parts.map(|p| p.parse::<f32>()).collect::<Result<_, _>>() {
            Ok(x) => x,
            Err(_) => continue,
        };
        if x.len() == FEATURES {
            samples.push((x, needed));
        }
    }
    if samples.len() < MIN_SAMPLES {
        return None;
    }
    let mut model = Model::empty();
    model.samples = samples.len();
    // Балансируем классы: «нужных» плиток всегда меньше.
    let pos = samples.iter().filter(|(_, n)| *n).count() as f32;
    let neg = samples.len() as f32 - pos;
    let w_pos = if pos > 0.0 { samples.len() as f32 / (2.0 * pos) } else { 1.0 };
    let w_neg = if neg > 0.0 { samples.len() as f32 / (2.0 * neg) } else { 1.0 };

    let mut rng = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };

    let epochs = 200;
    for _ in 0..epochs {
        // Перемешиваем порядок: без этого модель застревает на последнем классе.
        let mut order: Vec<usize> = (0..samples.len()).collect();
        for i in (1..order.len()).rev() {
            let j = (next() % (i as u64 + 1)) as usize;
            order.swap(i, j);
        }
        for idx in order {
            let (x, needed) = &samples[idx];
            let target = if *needed { 1.0 } else { 0.0 };
            let weight = if *needed { w_pos } else { w_neg };
            let mut sum = model.bias;
            for (w, v) in model.w.iter().zip(x) {
                sum += w * v;
            }
            let p = 1.0 / (1.0 + (-sum).exp());
            let grad = (p - target) * weight * 0.05;
            model.bias -= grad;
            for (w, v) in model.w.iter_mut().zip(x) {
                *w -= grad * v + 0.0001 * *w;
            }
        }
    }
    let _ = model.save(dir);
    Some(model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captcha_text_is_recognized() {
        assert!(is_captcha("Nekiplay, сначала пройди капчу ❗"));
        assert!(is_captcha("Пройди CAPTCHA"));
        assert!(!is_captcha("Ты выиграл 500$"));
    }

    #[test]
    fn buttons_are_read_as_tiles() {
        let b = Button {
            text: " 7 ".into(),
            data: b"x".to_vec(),
        };
        assert_eq!(b.tile(), Some(7));
        let done = Button {
            text: "Готово".into(),
            data: b"y".to_vec(),
        };
        assert!(done.is_done());
        assert_eq!(done.tile(), None);
    }

    #[test]
    fn selected_tiles_are_parsed() {
        assert_eq!(
            selected("🤖 Выбрано: 1, 4\nОсталось времени: 9 мин."),
            Some(vec![1, 4])
        );
        assert_eq!(selected("Nekiplay, капча: выбери картинки"), None);
        assert_eq!(selected("Выбрано: "), None);
    }

    #[test]
    fn flat_strips_split_the_grid() {
        // 2×2 сетка с белыми разделителями по 4 пикселя.
        let mut img = RgbaImage::from_pixel(24, 24, image::Rgba([0, 0, 255, 255]));
        for i in 0..24 {
            for j in 10..14 {
                img.put_pixel(i, j, image::Rgba([255, 255, 255, 255]));
                img.put_pixel(j, i, image::Rgba([255, 255, 255, 255]));
            }
        }
        let (cols, rows) = grid(&img);
        assert_eq!((cols, rows), (2, 2));
    }

    #[test]
    fn features_are_normalized() {
        let tile = vec![128u8; 40 * 40 * 4];
        let x = features(&tile, 40, 40);
        assert_eq!(x.len(), FEATURES);
        assert!(x.iter().all(|v| v.abs() <= 1.0));
    }
}