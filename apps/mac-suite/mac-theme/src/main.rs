// SPDX-License-Identifier: GPL-3.0-only

//! Gives the COSMIC theme a soft, cohesive macOS Sonoma/Sequoia look: calm slate
//! surfaces, soft blue accent, subtle 1px window outlines, neutral text buttons,
//! crisp off-white text on accent pills, and rounder corners.

use cosmic::{
    cosmic_config::{Config, CosmicConfigEntry},
    cosmic_theme::{
        CornerRadii, Theme, ThemeBuilder,
        palette::{Srgb, Srgba},
    },
};

const ON_ACCENT: Srgba = Srgba::new(0.98, 0.99, 1.0, 1.0);

#[derive(Clone, Copy, Debug)]
struct MacPalette {
    is_dark: bool,
    accent: Srgb,
    bg_color: Srgba,
    primary_container_bg: Srgba,
    secondary_container_bg: Srgba,
    neutral_tint: Srgb,
    text_tint: Srgb,
    window_hint: Srgb,
    accent_text: Srgba,
}

const DARK_PALETTE: MacPalette = MacPalette {
    is_dark: true,
    accent: Srgb::new(0.290, 0.565, 0.886), // #4A90E2
    bg_color: Srgba::new(0.125, 0.133, 0.157, 1.0), // #202228
    primary_container_bg: Srgba::new(0.157, 0.169, 0.196, 1.0), // #282B32
    secondary_container_bg: Srgba::new(0.196, 0.208, 0.243, 1.0), // #32353E
    neutral_tint: Srgb::new(0.56, 0.59, 0.65),
    text_tint: Srgb::new(0.91, 0.92, 0.95),
    window_hint: Srgb::new(0.25, 0.27, 0.32),
    accent_text: Srgba::new(0.48, 0.72, 0.96, 1.0),
};

const LIGHT_PALETTE: MacPalette = MacPalette {
    is_dark: false,
    accent: Srgb::new(0.208, 0.494, 0.910), // #357EE8
    bg_color: Srgba::new(0.945, 0.949, 0.965, 1.0), // #F1F2F6
    primary_container_bg: Srgba::new(0.980, 0.984, 0.992, 1.0), // #FAFBFD
    secondary_container_bg: Srgba::new(0.918, 0.925, 0.945, 1.0), // #EAECEF
    neutral_tint: Srgb::new(0.45, 0.48, 0.54),
    text_tint: Srgb::new(0.14, 0.16, 0.20),
    window_hint: Srgb::new(0.80, 0.82, 0.86),
    accent_text: Srgba::new(0.18, 0.45, 0.85, 1.0),
};

fn radii() -> CornerRadii {
    CornerRadii {
        radius_0: [0.0; 4],
        radius_xs: [4.0; 4],
        radius_s: [8.0; 4],
        radius_m: [12.0; 4],
        radius_l: [16.0; 4],
        radius_xl: [22.0; 4],
    }
}

fn configure_builder(builder: &mut ThemeBuilder, spec: &MacPalette) {
    if !spec.is_dark && builder.palette.is_dark() {
        builder.palette = ThemeBuilder::light().palette;
    } else if spec.is_dark && !builder.palette.is_dark() {
        builder.palette = ThemeBuilder::dark().palette;
    }
    builder.accent = Some(spec.accent);
    builder.bg_color = Some(spec.bg_color);
    builder.primary_container_bg = Some(spec.primary_container_bg);
    builder.secondary_container_bg = Some(spec.secondary_container_bg);
    builder.neutral_tint = Some(spec.neutral_tint);
    builder.text_tint = Some(spec.text_tint);
    builder.active_hint = 1;
    builder.window_hint = Some(spec.window_hint);
    builder.corner_radii = radii();
}

fn build_theme(mut builder: ThemeBuilder, spec: &MacPalette) -> Theme {
    configure_builder(&mut builder, spec);
    let mut theme = builder.build();
    let bg_component = theme.background(false).component.clone();
    theme.text_button.on = bg_component.on;
    theme.text_button.on_disabled = bg_component.on_disabled;
    theme.accent_button.on = ON_ACCENT;
    theme.accent_button.on_disabled = Srgba::new(ON_ACCENT.red, ON_ACCENT.green, ON_ACCENT.blue, 0.5);
    theme.accent.on = ON_ACCENT;
    theme.accent_text = Some(spec.accent_text);
    theme
}

fn apply(
    builder_config: Result<Config, cosmic::cosmic_config::Error>,
    theme_config: Result<Config, cosmic::cosmic_config::Error>,
    spec: &MacPalette,
) -> Result<(), String> {
    let builder_config = builder_config.map_err(|err| err.to_string())?;
    let theme_config = theme_config.map_err(|err| err.to_string())?;
    let mut builder = match ThemeBuilder::get_entry(&builder_config) {
        Ok(builder) => builder,
        Err((_, _)) => {
            if spec.is_dark {
                ThemeBuilder::dark()
            } else {
                ThemeBuilder::light()
            }
        }
    };
    configure_builder(&mut builder, spec);
    builder
        .write_entry(&builder_config)
        .map_err(|err| err.to_string())?;
    build_theme(builder, spec)
        .write_entry(&theme_config)
        .map_err(|err| err.to_string())
}

fn main() {
    let results = [
        (
            "dark",
            apply(ThemeBuilder::dark_config(), Theme::dark_config(), &DARK_PALETTE),
        ),
        (
            "light",
            apply(ThemeBuilder::light_config(), Theme::light_config(), &LIGHT_PALETTE),
        ),
    ];
    let mut failed = false;
    for (name, result) in results {
        match result {
            Ok(()) => println!("{name} theme updated"),
            Err(err) => {
                eprintln!("{name} theme failed: {err}");
                failed = true;
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic::cosmic_theme::palette::color_difference::Wcag21RelativeContrast;

    fn assert_close_srgba(a: Srgba, b: Srgba) {
        assert!((a.red - b.red).abs() < 1e-4, "red {a:?} != {b:?}");
        assert!((a.green - b.green).abs() < 1e-4, "green {a:?} != {b:?}");
        assert!((a.blue - b.blue).abs() < 1e-4, "blue {a:?} != {b:?}");
        assert!((a.alpha - b.alpha).abs() < 1e-4, "alpha {a:?} != {b:?}");
    }

    fn assert_close_srgb(a: Srgb, b: Srgb) {
        assert!((a.red - b.red).abs() < 1e-4, "red {a:?} != {b:?}");
        assert!((a.green - b.green).abs() < 1e-4, "green {a:?} != {b:?}");
        assert!((a.blue - b.blue).abs() < 1e-4, "blue {a:?} != {b:?}");
    }

    #[test]
    fn dark_theme_colors_and_contrast() {
        let theme = build_theme(ThemeBuilder::dark(), &DARK_PALETTE);
        assert!(theme.is_dark);
        assert_close_srgba(theme.background(false).base, DARK_PALETTE.bg_color);
        assert_close_srgba(theme.primary(false).base, DARK_PALETTE.primary_container_bg);
        assert_close_srgba(theme.secondary(false).base, DARK_PALETTE.secondary_container_bg);
        assert_close_srgb(theme.accent.base.color, DARK_PALETTE.accent);
        assert_eq!(theme.active_hint, 1);
        assert_close_srgb(theme.window_hint.expect("window_hint"), DARK_PALETTE.window_hint);
        assert_close_srgba(theme.accent_text.expect("accent_text"), DARK_PALETTE.accent_text);

        // Text buttons must use neutral foreground text, never bright blue.
        assert_close_srgba(theme.text_button.on, theme.background(false).component.on);
        // Accent pills must render crisp off-white text on blue.
        assert_close_srgba(theme.accent_button.on, ON_ACCENT);
        assert_close_srgba(theme.accent.on, ON_ACCENT);

        // Readable contrast on base, primary, secondary, and link text.
        assert!(theme.background(false).base.relative_contrast(theme.background(false).on.color) >= 7.0);
        assert!(theme.primary(false).base.relative_contrast(theme.primary(false).on.color) >= 7.0);
        assert!(theme.secondary(false).base.relative_contrast(theme.secondary(false).on.color) >= 7.0);
        assert!(theme.background(false).base.relative_contrast(theme.text_button.on.color) >= 7.0);
        let link_contrast = theme
            .primary(false)
            .base
            .relative_contrast(theme.accent_text.unwrap().color);
        assert!(
            link_contrast >= 4.0,
            "light accent_text contrast too low: {link_contrast}"
        );
    }

    #[test]
    fn light_theme_colors_and_contrast() {
        let theme = build_theme(ThemeBuilder::light(), &LIGHT_PALETTE);
        assert!(!theme.is_dark);
        assert_close_srgba(theme.background(false).base, LIGHT_PALETTE.bg_color);
        assert_close_srgba(theme.primary(false).base, LIGHT_PALETTE.primary_container_bg);
        assert_close_srgba(theme.secondary(false).base, LIGHT_PALETTE.secondary_container_bg);
        assert_close_srgb(theme.accent.base.color, LIGHT_PALETTE.accent);
        assert_eq!(theme.active_hint, 1);
        assert_close_srgb(theme.window_hint.expect("window_hint"), LIGHT_PALETTE.window_hint);
        assert_close_srgba(theme.accent_text.expect("accent_text"), LIGHT_PALETTE.accent_text);

        // Text buttons must use neutral foreground text, never bright blue.
        assert_close_srgba(theme.text_button.on, theme.background(false).component.on);
        // Accent pills must render crisp off-white text on blue.
        assert_close_srgba(theme.accent_button.on, ON_ACCENT);
        assert_close_srgba(theme.accent.on, ON_ACCENT);

        // Readable contrast on base, primary, secondary, and link text.
        assert!(theme.background(false).base.relative_contrast(theme.background(false).on.color) >= 7.0);
        assert!(theme.primary(false).base.relative_contrast(theme.primary(false).on.color) >= 7.0);
        assert!(theme.secondary(false).base.relative_contrast(theme.secondary(false).on.color) >= 7.0);
        assert!(theme.background(false).base.relative_contrast(theme.text_button.on.color) >= 7.0);
        let link_contrast = theme
            .primary(false)
            .base
            .relative_contrast(theme.accent_text.unwrap().color);
        assert!(
            link_contrast >= 4.0,
            "light accent_text contrast too low: {link_contrast}"
        );
    }

    #[test]
    fn libcosmic_builder_defaults_text_and_accent_button_on_colors() {
        // Verify libcosmic-mac's ThemeBuilder::build() directly (without post-processing).
        let mut dark_builder = ThemeBuilder::dark();
        configure_builder(&mut dark_builder, &DARK_PALETTE);
        let dark_raw = dark_builder.build();
        assert_close_srgba(dark_raw.text_button.on, dark_raw.background(false).component.on);
        assert_close_srgba(dark_raw.accent_button.on, ON_ACCENT);

        let mut light_builder = ThemeBuilder::light();
        configure_builder(&mut light_builder, &LIGHT_PALETTE);
        let light_raw = light_builder.build();
        assert_close_srgba(light_raw.text_button.on, light_raw.background(false).component.on);
        assert_close_srgba(light_raw.accent_button.on, ON_ACCENT);
    }
}
