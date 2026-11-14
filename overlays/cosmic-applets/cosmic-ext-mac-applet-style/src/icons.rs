// SPDX-License-Identifier: GPL-3.0-only

//! Shared symbolic SVG helper icons for macOS-style COSMIC panel applets.

use cosmic::widget::{Icon, icon};

/// Default symbolic icon size in pixels for panel bar icons.
pub const DEFAULT_ICON_SIZE: u16 = 16;

/// Symbolic LordOS / apple-inspired geometric logo for the top-left system menu (16px).
pub const LORD_LOGO_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="currentColor">
  <path d="M9.6 1.4c.5-.6 1.2-1 2-.9.1.9-.3 1.7-.8 2.3-.5.6-1.2 1-2 .9-.1-.8.3-1.7.8-2.3z"/>
  <path d="M12.6 8.3c0-1.6 1.3-2.4 1.4-2.5-.8-1.1-1.9-1.3-2.3-1.3-1-.1-1.9.6-2.4.6-.5 0-1.3-.6-2.1-.6-1.1 0-2.1.6-2.6 1.6-1.1 2-.3 4.9.8 6.5.5.8 1.2 1.7 2 1.6.8 0 1.1-.5 2.1-.5s1.2.5 2.1.5c.9 0 1.4-.8 1.9-1.6.6-.9.9-1.8.9-1.8-.1 0-1.8-.7-1.8-2.5z"/>
</svg>"#;

/// Monochrome magnifier icon for the Spotlight launcher pill.
pub const SPOTLIGHT_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">
  <circle cx="6.8" cy="6.8" r="4.5"/>
  <path d="M10.2 10.2L14 14"/>
</svg>"#;

/// Stacked two-toggle Control Center icon.
pub const CONTROL_CENTER_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="currentColor">
  <path fill-rule="evenodd" d="M5 2.5a2.5 2.5 0 0 0 0 5h6a2.5 2.5 0 0 0 0-5H5zm6 4a1.5 1.5 0 1 1 0-3 1.5 1.5 0 0 1 0 3z"/>
  <path fill-rule="evenodd" d="M5 8.5a2.5 2.5 0 0 0 0 5h6a2.5 2.5 0 0 0 0-5H5zm-1.5 2.5a1.5 1.5 0 1 1 3 0 1.5 1.5 0 0 1-3 0z"/>
</svg>"#;

/// Monochrome clipboard icon for the Clipboard Manager pill.
pub const CLIPBOARD_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round">
  <rect x="3" y="3" width="10" height="11.5" rx="1.8"/>
  <path d="M6 3V2.2A1.2 1.2 0 0 1 7.2 1h1.6A1.2 1.2 0 0 1 10 2.2V3"/>
  <path d="M6 3h4"/>
  <path d="M5.5 7h5M5.5 10h3.5"/>
</svg>"#;

/// Slashed monochrome clipboard icon shown when clipboard history is paused.
pub const CLIPBOARD_SLASHED_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round">
  <path d="M4.5 3H4.8A1.8 1.8 0 0 0 3 4.8v8.4A1.8 1.8 0 0 0 4.8 15h6.4c.4 0 .8-.1 1.1-.4"/>
  <path d="M13 10.5V4.8A1.8 1.8 0 0 0 11.2 3h-1.2"/>
  <path d="M6 3V2.2A1.2 1.2 0 0 1 7.2 1h1.6A1.2 1.2 0 0 1 10 2.2V3H6z"/>
  <path d="M2 2l12 12"/>
</svg>"#;

/// Small submenu chevron pointing right (`›`).
pub const CHEVRON_RIGHT_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
  <path d="M6 3.5L10.5 8 6 12.5"/>
</svg>"#;

/// Small submenu back chevron pointing left (`‹`).
pub const CHEVRON_LEFT_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
  <path d="M10 3.5L5.5 8 10 12.5"/>
</svg>"#;

/// Small checkmark icon for active toggles / current workspace indicator.
pub const CHECKMARK_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
  <path d="M3.2 8.4L6.4 11.6 12.8 4.4"/>
</svg>"#;

/// Build a symbolic [`Icon`] widget from raw SVG bytes at the requested pixel size.
pub fn svg_icon(bytes: &'static [u8], size: u16) -> Icon {
    icon::from_svg_bytes(bytes).symbolic(true).icon().size(size)
}

/// Build the 16px symbolic Lord menu logo icon.
pub fn lord_logo_icon(size: u16) -> Icon {
    svg_icon(LORD_LOGO_SVG, size)
}

/// Build the 16px symbolic Spotlight icon.
pub fn spotlight_icon(size: u16) -> Icon {
    svg_icon(SPOTLIGHT_SVG, size)
}

/// Build the 16px symbolic Control Center icon.
pub fn control_center_icon(size: u16) -> Icon {
    svg_icon(CONTROL_CENTER_SVG, size)
}

/// Build the 16px symbolic Clipboard icon (slashed when paused).
pub fn clipboard_icon(paused: bool, size: u16) -> Icon {
    if paused {
        svg_icon(CLIPBOARD_SLASHED_SVG, size)
    } else {
        svg_icon(CLIPBOARD_SVG, size)
    }
}

/// Build a symbolic right chevron icon.
pub fn chevron_right_icon(size: u16) -> Icon {
    svg_icon(CHEVRON_RIGHT_SVG, size)
}

/// Build a symbolic left chevron icon.
pub fn chevron_left_icon(size: u16) -> Icon {
    svg_icon(CHEVRON_LEFT_SVG, size)
}

/// Build a symbolic checkmark icon.
pub fn checkmark_icon(size: u16) -> Icon {
    svg_icon(CHECKMARK_SVG, size)
}
