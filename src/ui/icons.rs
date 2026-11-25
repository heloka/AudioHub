use std::borrow::Cow;

use gpui::{AssetSource, Result, Rgba, SharedString, Svg, prelude::*, px, svg};

pub struct UiAssets;

impl AssetSource for UiAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let data = match path {
            ICON_AUDIO => Some(SVG_AUDIO),
            ICON_PLAY => Some(SVG_PLAY),
            ICON_STOP => Some(SVG_STOP),
            ICON_PLUS => Some(SVG_PLUS),
            ICON_SCAN => Some(SVG_SCAN),
            ICON_SLIDERS => Some(SVG_SLIDERS),
            ICON_EDIT => Some(SVG_EDIT),
            ICON_TRASH => Some(SVG_TRASH),
            ICON_CLOSE => Some(SVG_CLOSE),
            ICON_CHECK => Some(SVG_CHECK),
            ICON_ALERT => Some(SVG_ALERT),
            ICON_DEVICE => Some(SVG_DEVICE),
            _ => None,
        };
        Ok(data.map(Cow::Borrowed))
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}

pub const ICON_AUDIO: &str = "icons/audio.svg";
pub const ICON_PLAY: &str = "icons/play.svg";
pub const ICON_STOP: &str = "icons/stop.svg";
pub const ICON_PLUS: &str = "icons/plus.svg";
pub const ICON_SCAN: &str = "icons/scan.svg";
pub const ICON_SLIDERS: &str = "icons/sliders.svg";
pub const ICON_EDIT: &str = "icons/edit.svg";
pub const ICON_TRASH: &str = "icons/trash.svg";
pub const ICON_CLOSE: &str = "icons/close.svg";
pub const ICON_CHECK: &str = "icons/check.svg";
pub const ICON_ALERT: &str = "icons/alert.svg";
pub const ICON_DEVICE: &str = "icons/device.svg";

pub fn icon(path: &'static str, size_px: f32, color: Rgba) -> Svg {
    svg()
        .path(path)
        .size(px(size_px))
        .flex_none()
        .text_color(color)
}

const SVG_AUDIO: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="#000000" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="M11 5 6 9H3v6h3l5 4V5Z"/><path d="M15.5 8.5a5 5 0 0 1 0 7"/><path d="M18.8 5.8a9 9 0 0 1 0 12.4"/></svg>"##;

const SVG_PLAY: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="#000000"><path d="M7.5 5.4c0-1.1 1.2-1.8 2.1-1.2l10.2 6.6c.9.6.9 1.8 0 2.4L9.6 19.8c-.9.6-2.1-.1-2.1-1.2V5.4Z"/></svg>"##;

const SVG_STOP: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="#000000"><rect x="6" y="6" width="12" height="12" rx="2.4"/></svg>"##;

const SVG_PLUS: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="#000000" stroke-width="2" stroke-linecap="round"><path d="M12 5v14"/><path d="M5 12h14"/></svg>"##;

const SVG_SCAN: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="#000000" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12.55a11 11 0 0 1 14.08 0"/><path d="M8.53 16.11a6 6 0 0 1 6.95 0"/><circle cx="12" cy="19.5" r="1.2" fill="#000000" stroke="none"/></svg>"##;

const SVG_SLIDERS: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="#000000" stroke-width="1.9" stroke-linecap="round"><path d="M4 7h16"/><path d="M4 17h16"/><circle cx="9" cy="7" r="2.2" fill="#000000" stroke="none"/><circle cx="15" cy="17" r="2.2" fill="#000000" stroke="none"/></svg>"##;

const SVG_EDIT: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="#000000" stroke-width="1.85" stroke-linecap="round" stroke-linejoin="round"><path d="M12 20h9"/><path d="M16.5 3.5a2.12 2.12 0 1 1 3 3L7 19l-4 1 1-4Z"/></svg>"##;

const SVG_TRASH: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="#000000" stroke-width="1.85" stroke-linecap="round" stroke-linejoin="round"><path d="M3 6h18"/><path d="M8 6V4.5A1.5 1.5 0 0 1 9.5 3h5A1.5 1.5 0 0 1 16 4.5V6"/><path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6"/><path d="M10 11v6"/><path d="M14 11v6"/></svg>"##;

const SVG_CLOSE: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="#000000" stroke-width="2" stroke-linecap="round"><path d="M6 6l12 12"/><path d="M18 6 6 18"/></svg>"##;

const SVG_CHECK: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="#000000" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12.5 9.5 17 19 7.5"/></svg>"##;

const SVG_ALERT: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="#000000" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="9"/><path d="M12 8v4.5"/><circle cx="12" cy="16.2" r="1" fill="#000000" stroke="none"/></svg>"##;

const SVG_DEVICE: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="#000000" stroke-width="1.85" stroke-linecap="round" stroke-linejoin="round"><rect x="6" y="3" width="12" height="18" rx="2.5"/><path d="M10.5 17.5h3"/></svg>"##;
