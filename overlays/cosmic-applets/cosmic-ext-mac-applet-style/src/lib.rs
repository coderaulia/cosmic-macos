// SPDX-License-Identifier: GPL-3.0-only

//! Shared macOS-style menu bar pill button, frosted popover container, and
//! symbolic SVG icon helpers for COSMIC panel applets.

pub mod icons;

use std::borrow::Cow;

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

/// Height of a panel pill in pixels (28px bar minus 3px top & bottom margin).
pub const PILL_HEIGHT: f32 = 22.0;

/// Corner radius of a panel pill in pixels (`radius_s` minus 2).
pub const PILL_CORNER_RADIUS: f32 = 6.0;

/// Horizontal padding for text and mixed icon+text pills.
pub const PILL_HORIZONTAL_PADDING_TEXT: u16 = 8;

/// Horizontal padding for icon-only pills.
pub const PILL_HORIZONTAL_PADDING_ICON: u16 = 6;

/// Panel bar text size in pixels.
pub const PANEL_TEXT_SIZE: f32 = 13.0;

/// Panel bar symbolic icon size in pixels.
pub const PANEL_ICON_SIZE: u16 = 14;

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
    theme::Button::Custom {
        active: Box::new(move |_focused, theme| {
            let fg = panel_foreground_color(theme);
            let mut style = button::Style::new();
            style.border_radius = [PILL_CORNER_RADIUS; 4].into();
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
            style.border_radius = [PILL_CORNER_RADIUS; 4].into();
            style.text_color = Some(fg);
            style.icon_color = Some(fg);
            let alpha = if is_open { 0.20 } else { 0.12 };
            style.background = Some(Background::Color(Color { a: alpha, ..fg }));
            style
        }),
        pressed: Box::new(|_focused, theme| {
            let fg = panel_foreground_color(theme);
            let mut style = button::Style::new();
            style.border_radius = [PILL_CORNER_RADIUS; 4].into();
            style.text_color = Some(fg);
            style.icon_color = Some(fg);
            style.background = Some(Background::Color(Color { a: 0.20, ..fg }));
            style
        }),
        disabled: Box::new(|theme| {
            let fg = panel_foreground_color(theme);
            let muted = Color {
                a: fg.a * 0.5,
                ..fg
            };
            let mut style = button::Style::new();
            style.border_radius = [PILL_CORNER_RADIUS; 4].into();
            style.text_color = Some(muted);
            style.icon_color = Some(muted);
            style
        }),
    }
}

/// Build a macOS menu bar pill button (`22px` tall, `6px` corner radius,
/// `8px` horizontal padding for text/mixed or `6px` for icon-only,
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
    .height(Length::Fixed(PILL_HEIGHT))
    .padding(pill_padding(variant))
    .selected(is_open)
    .class(pill_button_class(is_open))
}

/// Convenience helper for an icon-only 14px symbolic [`mac_pill_button`].
pub fn mac_icon_pill_button<'a, Message: Clone + 'a>(
    handle: icon::Handle,
    is_open: bool,
) -> Button<'a, Message> {
    let icon_widget = icon::icon(handle).size(PANEL_ICON_SIZE);
    mac_pill_button(icon_widget, is_open, PillVariant::IconOnly)
}

/// Convenience helper for a `13px` text [`mac_pill_button`] (regular or bold 700).
pub fn mac_text_pill_button<'a, Message: Clone + 'a>(
    label: impl Into<Cow<'a, str>>,
    bold: bool,
    is_open: bool,
) -> Button<'a, Message> {
    let label_widget = text(label.into())
        .size(PANEL_TEXT_SIZE)
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
        assert_eq!(PILL_HEIGHT, 22.0);
        assert_eq!(PILL_CORNER_RADIUS, 6.0);
        assert_eq!(PANEL_TEXT_SIZE, 13.0);
        assert_eq!(PANEL_ICON_SIZE, 14);
        assert_eq!(POPOVER_GAP, 6);
        assert_eq!(POPOVER_CORNER_RADIUS, 12.0);
    }

    #[test]
    fn pill_padding_distinguishes_text_and_icon_only() {
        assert_eq!(pill_padding(PillVariant::Text), [0, 8]);
        assert_eq!(pill_padding(PillVariant::IconOnly), [0, 6]);
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
