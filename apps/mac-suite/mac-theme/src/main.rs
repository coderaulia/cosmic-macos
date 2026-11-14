// SPDX-License-Identifier: GPL-3.0-only

//! Gives the COSMIC theme a macOS-like look: the system blue accent and
//! rounder corners. Colours are derived the same way COSMIC Settings does it
//! (builder first, then the theme built from it), so every shade stays
//! consistent. Run it again at any time; `cosmic-preset` can switch back.

use cosmic::{
    cosmic_config::{Config, CosmicConfigEntry},
    cosmic_theme::{CornerRadii, Theme, ThemeBuilder, palette::Srgb},
};

/// macOS system blue for dark and light appearances.
const BLUE_DARK: (f32, f32, f32) = (10.0 / 255.0, 132.0 / 255.0, 1.0);
const BLUE_LIGHT: (f32, f32, f32) = (0.0, 122.0 / 255.0, 1.0);

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

fn apply(
    builder_config: Result<Config, cosmic::cosmic_config::Error>,
    theme_config: Result<Config, cosmic::cosmic_config::Error>,
    blue: (f32, f32, f32),
) -> Result<(), String> {
    let builder_config = builder_config.map_err(|err| err.to_string())?;
    let theme_config = theme_config.map_err(|err| err.to_string())?;
    let mut builder = match ThemeBuilder::get_entry(&builder_config) {
        Ok(builder) => builder,
        Err((_, builder)) => builder,
    };
    builder.accent = Some(Srgb::new(blue.0, blue.1, blue.2));
    builder.corner_radii = radii();
    builder
        .write_entry(&builder_config)
        .map_err(|err| err.to_string())?;
    builder
        .build()
        .write_entry(&theme_config)
        .map_err(|err| err.to_string())
}

fn main() {
    let results = [
        (
            "dark",
            apply(ThemeBuilder::dark_config(), Theme::dark_config(), BLUE_DARK),
        ),
        (
            "light",
            apply(ThemeBuilder::light_config(), Theme::light_config(), BLUE_LIGHT),
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
