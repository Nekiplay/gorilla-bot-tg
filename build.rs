//! Windows resources for the `gorilla-bot` binary.
//!
//! The artwork is shipped as one big PNG, but Windows wants a multi-size `.ico`
//! for the executable (Explorer, taskbar, Alt+Tab all pick different sizes).
//! So the build script renders every needed size here and hands the packed
//! `.ico` to the `winicon` crate, which stamps it into the binary.

use std::io::Cursor;
use std::path::PathBuf;

/// Размеры кадров в `.ico` — от строки заголовка до плитки 256×256.
const ICON_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", winicon::LOGO_PATH);

    let icon = build_icon();
    winicon::apply(
        winicon::Options {
            // Ничего не пишем в системные места — UAC не нужен.
            require_admin: false,
            // GUI-подсистема: окно консоли не открывается. Тот же флаг дублирует
            // `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]`
            // в main.rs.
            no_console: true,
        },
        &icon,
    );
}

/// Собирает многоразмерный `.ico` из логотипа и возвращает путь к нему.
///
/// Кадры хранятся внутри `.ico` как PNG (так делает и Windows начиная с
/// Vista): контейнер небольшой, а исходник у нас ровно один — PNG.
fn build_icon() -> PathBuf {
    let source = image::load_from_memory_with_format(winicon::LOGO, image::ImageFormat::Png)
        .expect("assets/logo.png не читается как PNG");

    let mut frames: Vec<(u32, Vec<u8>)> = Vec::with_capacity(ICON_SIZES.len());
    for size in ICON_SIZES {
        let frame = source.resize_exact(size, size, image::imageops::FilterType::Lanczos3);
        let mut png = Cursor::new(Vec::new());
        frame
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("не удалось закодировать кадр иконки");
        frames.push((size, png.into_inner()));
    }

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR не задан"));
    let path = out_dir.join("gorilla-bot.ico");
    std::fs::write(&path, pack_ico(&frames)).expect("не удалось записать .ico");
    path
}

/// Раскладывает PNG-кадры в контейнер ICO (ICONDIR + ICONDIRENTRY + данные).
fn pack_ico(frames: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&[0x00, 0x00, 0x01, 0x00]); // reserved = 0, type = 1 (icon)
    out.extend_from_slice(&(frames.len() as u16).to_le_bytes());

    // Данные начинаются сразу за таблицей: 6 байт заголовка + 16 байт на кадр.
    let mut offset = 6 + 16 * frames.len() as u32;
    for (size, frame) in frames {
        // 256 и больше кодируются нулём: поле ширины/высоты однобайтовое.
        let dim = if *size >= 256 { 0 } else { *size as u8 };
        out.push(dim); // width
        out.push(dim); // height
        out.push(0); // palette colors
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // color planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += frame.len() as u32;
    }

    for (_, frame) in frames {
        out.extend_from_slice(frame);
    }
    out
}
