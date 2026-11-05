//! The app's icons, then the component library's own.
use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;

#[derive(rust_embed::RustEmbed)]
#[folder = "assets"]
#[include = "icons/*.svg"]
struct Own;

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(file) = Own::get(path) {
            return Ok(Some(file.data));
        }
        gpui_kit_assets::Assets::new("").load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut names: Vec<SharedString> = Own::iter()
            .filter(|name| name.starts_with(path))
            .map(|name| name.to_string().into())
            .collect();
        names.extend(gpui_kit_assets::Assets::new("").list(path)?);
        Ok(names)
    }
}
