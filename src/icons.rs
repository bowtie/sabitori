use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

/// Filled check mark, used by the gpui-component Start-with-Windows
/// checkbox, which requests the crate's `icons/check.svg` asset path.
pub(crate) const CHECK_ICON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="#000000"><path d="M9 16.17L4.83 12l-1.42 1.41L9 19 21 7l-1.41-1.41z"/></svg>"##;

/// Warning triangle (Phosphor `warning`), for the failure banner.
pub(crate) const WARNING_ICON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256" fill="#000000"><path d="M236.8,188.09,149.35,36.22h0a24.76,24.76,0,0,0-42.7,0L19.2,188.09a23.51,23.51,0,0,0,0,23.72A24.35,24.35,0,0,0,40.55,224h174.9a24.35,24.35,0,0,0,21.33-12.19A23.51,23.51,0,0,0,236.8,188.09ZM222.93,203.8a8.5,8.5,0,0,1-7.48,4.2H40.55a8.5,8.5,0,0,1-7.48-4.2,7.59,7.59,0,0,1,0-7.72L120.52,44.21a8.75,8.75,0,0,1,15,0l87.45,151.87A7.59,7.59,0,0,1,222.93,203.8ZM120,144V104a8,8,0,0,1,16,0v40a8,8,0,0,1-16,0Zm20,36a12,12,0,1,1-12-12A12,12,0,0,1,140,180Z"/></svg>"##;

/// In-memory asset source for the settings UI's icons. `load` serves the
/// embedded SVG bytes for known paths and `None` for everything else;
/// `list` is never used by the svg element.
pub(crate) struct IconAssets;

impl AssetSource for IconAssets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        let svg: &'static [u8] = match path {
            "icons/mouse.svg" => include_bytes!("../assets/icons/mouse.svg"),
            "icons/mouse-off.svg" => include_bytes!("../assets/icons/mouse-off.svg"),
            "icons/arrows-move-vertical.svg" => include_bytes!("../assets/icons/arrows-move-vertical.svg"),
            "icons/check.svg" => CHECK_ICON_SVG.as_bytes(),
            "icons/warning.svg" => WARNING_ICON_SVG.as_bytes(),
            _ => return Ok(None),
        };
        Ok(Some(Cow::Borrowed(svg)))
    }

    fn list(&self, _path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}
