// SPDX-License-Identifier: GPL-3.0-only

//! Shared macOS-style menu bar pill button, frosted popover container, and
//! symbolic SVG icon helpers for COSMIC panel applets.

pub mod icons;

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use cosmic::{
    Element,
    applet::{Context as AppletContext, cosmic_panel_config::PanelAnchor, menu_button, padded_control},
    iced::{
        Alignment, Background, Border, Color, Length, Limits, Padding, Shadow,
        alignment::{Horizontal, Vertical},
        font::{Font, Weight},
        widget::Container,
        window,
    },
    theme,
    widget::{
        Button, Id,
        autosize::{Autosize, autosize},
        button, container, divider, icon, text,
    },
};

/// Default height of a panel pill in pixels (32px bar minus 3px top & bottom margin).
pub const PILL_HEIGHT: f32 = 26.0;

/// Default corner radius of a panel pill in pixels.
pub const PILL_CORNER_RADIUS: f32 = 7.0;

/// Horizontal padding for text and mixed icon+text pills.
pub const PILL_HORIZONTAL_PADDING_TEXT: u16 = 9;

/// Horizontal padding for icon-only pills.
pub const PILL_HORIZONTAL_PADDING_ICON: u16 = 7;

/// Default panel bar text size in pixels.
pub const PANEL_TEXT_SIZE: f32 = 14.5;

/// Default panel bar symbolic icon size in pixels.
pub const PANEL_ICON_SIZE: u16 = 16;

const PANEL_STYLE_CONFIG_DIR: &str = "cosmic/dev.asw.MacPanelStyle/v1";
const STYLE_CACHE_TTL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy)]
struct PanelStyleValues {
    text_size: f32,
    pill_height: f32,
    icon_size: u16,
    corner_radius: f32,
}

static STYLE_CACHE: Mutex<Option<(Instant, PanelStyleValues)>> = Mutex::new(None);

fn panel_style_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map(|dir| dir.join(PANEL_STYLE_CONFIG_DIR))
}

fn parse_positive_f32(raw: &str, min: f32, max: f32) -> Option<f32> {
    let val = raw.trim().parse::<f32>().ok()?;
    val.is_finite().then(|| val.clamp(min, max))
}

/// Compute proportional pill height for a custom panel text size.
#[must_use]
pub fn pill_height_for_text_size(text_size: f32) -> f32 {
    (PILL_HEIGHT * (text_size / PANEL_TEXT_SIZE))
        .round()
        .clamp(18.0, 56.0)
}

/// Compute proportional symbolic icon size for a custom panel text size.
#[must_use]
pub fn icon_size_for_text_size(text_size: f32) -> u16 {
    (f32::from(PANEL_ICON_SIZE) * (text_size / PANEL_TEXT_SIZE))
        .round()
        .clamp(12.0, 32.0) as u16
}

fn read_style_values() -> PanelStyleValues {
    let text_size = std::env::var("MAC_PANEL_FONT_SIZE")
        .ok()
        .and_then(|s| parse_positive_f32(&s, 10.0, 32.0))
        .or_else(|| {
            panel_style_dir()
                .and_then(|dir| std::fs::read_to_string(dir.join("font_size")).ok())
                .and_then(|s| parse_positive_f32(&s, 10.0, 32.0))
        })
        .unwrap_or(PANEL_TEXT_SIZE);

    let pill_height = std::env::var("MAC_PANEL_PILL_HEIGHT")
        .ok()
        .and_then(|s| parse_positive_f32(&s, 18.0, 56.0))
        .or_else(|| {
            panel_style_dir()
                .and_then(|dir| std::fs::read_to_string(dir.join("pill_height")).ok())
                .and_then(|s| parse_positive_f32(&s, 18.0, 56.0))
        })
        .unwrap_or_else(|| pill_height_for_text_size(text_size));

    let icon_size = icon_size_for_text_size(text_size);
    let corner_radius = (PILL_CORNER_RADIUS * (pill_height / PILL_HEIGHT))
        .round()
        .clamp(4.0, 14.0);

    PanelStyleValues {
        text_size,
        pill_height,
        icon_size,
        corner_radius,
    }
}

fn current_style_values() -> PanelStyleValues {
    if let Ok(mut guard) = STYLE_CACHE.lock() {
        let now = Instant::now();
        if let Some((at, vals)) = *guard
            && now.duration_since(at) < STYLE_CACHE_TTL
        {
            return vals;
        }
        let vals = read_style_values();
        *guard = Some((now, vals));
        return vals;
    }
    read_style_values()
}

/// Active panel text size in pixels, honoring `MAC_PANEL_FONT_SIZE` or
/// `~/.config/cosmic/dev.asw.MacPanelStyle/v1/font_size` (default `14.5`).
#[must_use]
pub fn panel_text_size() -> f32 {
    current_style_values().text_size
}

/// Active panel pill height in pixels, honoring `MAC_PANEL_PILL_HEIGHT`,
/// `~/.config/cosmic/dev.asw.MacPanelStyle/v1/pill_height`, or scaling from
/// [`panel_text_size`] (default `26.0`).
#[must_use]
pub fn pill_height() -> f32 {
    current_style_values().pill_height
}

/// Active panel symbolic icon size in pixels, scaled from [`panel_text_size`]
/// (default `16`).
#[must_use]
pub fn panel_icon_size() -> u16 {
    current_style_values().icon_size
}

/// Active panel pill corner radius in pixels (default `7.0`).
#[must_use]
pub fn pill_corner_radius() -> f32 {
    current_style_values().corner_radius
}

/// Gap between the top panel bar and an open popover in pixels.
pub const POPOVER_GAP: i32 = 6;

/// Popover corner radius (`radius_m`).
pub const POPOVER_CORNER_RADIUS: f32 = 12.0;

/// Default popover width in pixels.
pub const POPOVER_DEFAULT_WIDTH: f32 = 360.0;

/// Horizontal padding variant for [`mac_pill_button`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PillVariant {
    /// Text or mixed icon+text pill: 8px horizontal padding.
    #[default]
    Text,
    /// Icon-only pill: 6px horizontal padding.
    IconOnly,
}

/// Return the horizontal/vertical padding for the given [`PillVariant`].
#[must_use]
pub const fn pill_padding(variant: PillVariant) -> [u16; 2] {
    match variant {
        PillVariant::Text => [0, PILL_HORIZONTAL_PADDING_TEXT],
        PillVariant::IconOnly => [0, PILL_HORIZONTAL_PADDING_ICON],
    }
}

/// Return the foreground color used for panel bar text, icons, and pill highlights.
#[must_use]
pub fn panel_foreground_color(theme: &cosmic::Theme) -> Color {
    Color::from(theme.cosmic().background(theme.transparent).on)
}

/// Return the theme's default interface font with bold (700) weight.
#[must_use]
pub fn bold_panel_font() -> Font {
    let mut font = cosmic::font::default();
    font.weight = Weight::Bold;
    font
}

/// Return the [`theme::Button`] style implementing the macOS menu bar pill states:
/// - transparent idle
/// - 12% foreground alpha on hover
/// - 20% foreground alpha when active/popover open or pressed
/// - 6px corner radius
#[must_use]
pub fn pill_button_class(is_open: bool) -> theme::Button {
    let corner_radius = pill_corner_radius();
    theme::Button::Custom {
        active: Box::new(move |_focused, theme| {
            let fg = panel_foreground_color(theme);
            let mut style = button::Style::new();
            style.border_radius = [corner_radius; 4].into();
            style.text_color = Some(fg);
            style.icon_color = Some(fg);
            if is_open {
                style.background = Some(Background::Color(Color { a: 0.20, ..fg }));
            }
            style
        }),
        hovered: Box::new(move |_focused, theme| {
            let fg = panel_foreground_color(theme);
            let mut style = button::Style::new();
            style.border_radius = [corner_radius; 4].into();
            style.text_color = Some(fg);
            style.icon_color = Some(fg);
            let alpha = if is_open { 0.20 } else { 0.12 };
            style.background = Some(Background::Color(Color { a: alpha, ..fg }));
            style
        }),
        pressed: Box::new(move |_focused, theme| {
            let fg = panel_foreground_color(theme);
            let mut style = button::Style::new();
            style.border_radius = [corner_radius; 4].into();
            style.text_color = Some(fg);
            style.icon_color = Some(fg);
            style.background = Some(Background::Color(Color { a: 0.20, ..fg }));
            style
        }),
        disabled: Box::new(move |theme| {
            let fg = panel_foreground_color(theme);
            let muted = Color {
                a: fg.a * 0.5,
                ..fg
            };
            let mut style = button::Style::new();
            style.border_radius = [corner_radius; 4].into();
            style.text_color = Some(muted);
            style.icon_color = Some(muted);
            style
        }),
    }
}

/// Build a macOS menu bar pill button (default `26px` tall, `7px` corner radius,
/// `9px` horizontal padding for text/mixed or `7px` for icon-only,
/// transparent idle, `12%` hover, `20%` active/popover open).
pub fn mac_pill_button<'a, Message: Clone + 'a>(
    content: impl Into<Element<'a, Message>>,
    is_open: bool,
    variant: PillVariant,
) -> Button<'a, Message> {
    button::custom(
        container(content)
            .center_y(Length::Fill)
            .align_x(Horizontal::Center),
    )
    .height(Length::Fixed(pill_height()))
    .padding(pill_padding(variant))
    .selected(is_open)
    .class(pill_button_class(is_open))
}

/// Convenience helper for an icon-only symbolic [`mac_pill_button`].
pub fn mac_icon_pill_button<'a, Message: Clone + 'a>(
    handle: icon::Handle,
    is_open: bool,
) -> Button<'a, Message> {
    let icon_widget = icon::icon(handle).size(panel_icon_size());
    mac_pill_button(icon_widget, is_open, PillVariant::IconOnly)
}

/// Convenience helper for a text [`mac_pill_button`] (regular or bold 700).
pub fn mac_text_pill_button<'a, Message: Clone + 'a>(
    label: impl Into<Cow<'a, str>>,
    bold: bool,
    is_open: bool,
) -> Button<'a, Message> {
    let label_widget = text(label.into())
        .size(panel_text_size())
        .font(if bold {
            bold_panel_font()
        } else {
            cosmic::font::default()
        });
    mac_pill_button(label_widget, is_open, PillVariant::Text)
}

/// Wrap a panel pill or row of pills so it centers vertically inside the 28px bar.
pub fn mac_bar_item<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    padding: Padding,
) -> Element<'a, Message> {
    container(content)
        .padding(padding)
        .center_y(Length::Shrink)
        .into()
}

/// Frosted popover container style (`radius_m` 12px corners, 1px inner border at 10% alpha).
#[must_use]
pub fn frosted_popover_style(theme: &cosmic::Theme) -> cosmic::iced::widget::container::Style {
    let cosmic = theme.cosmic();
    let bg = Color::from(cosmic.background(theme.transparent).base);
    let fg = Color::from(cosmic.background(theme.transparent).on);
    let radius = cosmic.corner_radii.radius_m;

    cosmic::iced::widget::container::Style {
        text_color: Some(fg),
        background: Some(bg.into()),
        border: Border {
            radius: radius.into(),
            width: 1.0,
            color: Color { a: 0.10, ..fg },
        },
        shadow: Shadow::default(),
        icon_color: Some(fg),
        snap: true,
    }
}

/// Build a frosted popover container at the default width (`360px`).
pub fn mac_popover_container<'a, Message: 'static>(
    applet: &AppletContext,
    content: impl Into<Element<'a, Message>>,
) -> Autosize<'a, Message, cosmic::Theme, cosmic::Renderer> {
    mac_popover_container_sized(applet, content, POPOVER_DEFAULT_WIDTH, POPOVER_DEFAULT_WIDTH)
}

/// Build a frosted popover container with explicit min/max width bounds.
pub fn mac_popover_container_sized<'a, Message: 'static>(
    applet: &AppletContext,
    content: impl Into<Element<'a, Message>>,
    min_width: f32,
    max_width: f32,
) -> Autosize<'a, Message, cosmic::Theme, cosmic::Renderer> {
    let (vertical_align, horizontal_align) = match applet.anchor {
        PanelAnchor::Left => (Vertical::Center, Horizontal::Left),
        PanelAnchor::Right => (Vertical::Center, Horizontal::Right),
        PanelAnchor::Top => (Vertical::Top, Horizontal::Center),
        PanelAnchor::Bottom => (Vertical::Bottom, Horizontal::Center),
    };

    let inner = Container::<Message, cosmic::Theme, cosmic::Renderer>::new(content)
        .style(frosted_popover_style);

    autosize(
        Container::<Message, cosmic::Theme, cosmic::Renderer>::new(inner)
            .height(Length::Shrink)
            .align_x(horizontal_align)
            .align_y(vertical_align),
        Id::new("mac-applet-popover-autosize"),
    )
    .limits(
        Limits::NONE
            .min_height(1.0)
            .min_width(min_width)
            .max_width(max_width)
            .max_height(1000.0),
    )
}

/// Build an [`Action::AppPopup`](cosmic::surface::Action::AppPopup) with a `6px` gap
/// below the bar and optional fixed width limits.
#[must_use]
pub fn mac_app_popup<App: cosmic::Application>(
    on_open: impl Fn(&mut App, window::Id) + Send + Sync + 'static,
    width: Option<f32>,
    anchor_rect_width: Option<i32>,
) -> cosmic::surface::Action<App::Message> {
    cosmic::surface::action::app_popup(
        |_| Default::default(),
        move |app: &mut App| {
            let id = window::Id::unique();
            on_open(app, id);
            let mut settings = app.core().applet.get_popup_settings(
                app.core().main_window_id().unwrap(),
                id,
                None,
                anchor_rect_width,
                None,
            );

            settings.positioner.offset = match app.core().applet.anchor {
                PanelAnchor::Left => (POPOVER_GAP, 0),
                PanelAnchor::Right => (-POPOVER_GAP, 0),
                PanelAnchor::Top => (0, POPOVER_GAP),
                PanelAnchor::Bottom => (0, -POPOVER_GAP),
            };

            if let Some(w) = width {
                settings.positioner.size_limits = Limits::NONE
                    .min_height(1.0)
                    .min_width(w)
                    .max_width(w)
                    .max_height(1080.0);
            }

            settings
        },
        None,
    )
}

/// Build a menu row inside a frosted popover using COSMIC's `menu_button` styling.
pub fn mac_menu_item<'a, Message: Clone + 'a>(
    content: impl Into<Element<'a, Message>>,
) -> Button<'a, Message> {
    menu_button(content)
}

/// Build a horizontal divider matching COSMIC applet popovers.
pub fn mac_menu_divider<'a, Message: 'a>() -> Element<'a, Message> {
    padded_control(divider::horizontal::default())
        .padding([4, 12])
        .into()
}

/// Build a standard text + optional trailing hint/chevron menu row.
pub fn mac_menu_label_row<'a, Message: 'a>(
    label: impl Into<Cow<'a, str>>,
    trailing: Option<Element<'a, Message>>,
) -> Element<'a, Message> {
    let mut r = cosmic::iced::widget::row![
        text::body(label.into()),
        cosmic::widget::space::horizontal().width(Length::Fill),
    ]
    .align_y(Alignment::Center)
    .spacing(8);

    if let Some(trail) = trailing {
        r = r.push(trail);
    }
    r.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pill_constants_match_spec() {
        assert_eq!(PILL_HEIGHT, 26.0);
        assert_eq!(PILL_CORNER_RADIUS, 7.0);
        assert_eq!(PANEL_TEXT_SIZE, 14.5);
        assert_eq!(PANEL_ICON_SIZE, 16);
        assert_eq!(POPOVER_GAP, 6);
        assert_eq!(POPOVER_CORNER_RADIUS, 12.0);
    }

    #[test]
    fn pill_padding_distinguishes_text_and_icon_only() {
        assert_eq!(pill_padding(PillVariant::Text), [0, 9]);
        assert_eq!(pill_padding(PillVariant::IconOnly), [0, 7]);
    }

    #[test]
    fn panel_style_scaling_and_parsing() {
        assert_eq!(pill_height_for_text_size(PANEL_TEXT_SIZE), 26.0);
        assert_eq!(icon_size_for_text_size(PANEL_TEXT_SIZE), 16);
        assert_eq!(pill_height_for_text_size(16.0), 29.0);
        assert_eq!(icon_size_for_text_size(16.0), 18);
        assert_eq!(parse_positive_f32(" 15.5\n", 10.0, 32.0), Some(15.5));
        assert_eq!(parse_positive_f32("40.0", 10.0, 32.0), Some(32.0));
        assert_eq!(parse_positive_f32("not-a-number", 10.0, 32.0), None);
    }

    #[test]
    fn symbolic_svgs_are_valid_svg_markup() {
        for svg in [
            icons::LORD_LOGO_SVG,
            icons::SPOTLIGHT_SVG,
            icons::CONTROL_CENTER_SVG,
            icons::CLIPBOARD_SVG,
            icons::CLIPBOARD_SLASHED_SVG,
            icons::CHEVRON_RIGHT_SVG,
            icons::CHEVRON_LEFT_SVG,
            icons::CHECKMARK_SVG,
        ] {
            let s = std::str::from_utf8(svg).expect("SVG must be UTF-8");
            assert!(s.contains("<svg"), "missing <svg tag: {s}");
            assert!(s.contains("currentColor"), "symbolic SVG should use currentColor: {s}");
        }
    }
}
