//! Ассеты окна: каталог Lucide из gpui-kit плюс свои иконки.

use std::borrow::Cow;

use gpui_kit::assets::AllAssets;
use gpui_kit::{AssetSource, Result, SharedString};
/// Свои иконки: путь в ассетах и байты SVG.
const CUSTOM: &[(&str, &[u8])] = &[(
    "icons/gorilla.svg",
    include_bytes!("../../assets/icons/gorilla.svg"),
)];

/// Отдаёт иконки gpui-kit, а недостающие пути ищет среди своих.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Ok(Some(bytes)) = AllAssets.load(path) {
            return Ok(Some(bytes));
        }
        if let Some((_, bytes)) = CUSTOM.iter().find(|(name, _)| *name == path) {
            return Ok(Some(Cow::Borrowed(*bytes)));
        }
        Ok(None)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut names = AllAssets.list(path).unwrap_or_default();
        names.extend(
            CUSTOM
                .iter()
                .filter(|(name, _)| name.starts_with(path))
                .map(|(name, _)| SharedString::from(*name)),
        );
        Ok(names)
    }
}
