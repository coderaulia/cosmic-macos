// SPDX-License-Identifier: GPL-3.0-only

//! Works out what the desktop should look like now and when that changes.

use std::path::PathBuf;

use jiff::{
    Timestamp,
    civil::{Date, date},
    tz::TimeZone,
};

use crate::{
    config::{DEFAULT_DARK_AT, DEFAULT_LIGHT_AT, Location, Mode, Settings, WallpaperMode},
    solar,
};

/// Part of the day for the dynamic wallpaper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Dawn,
    Day,
    Dusk,
    Night,
}

impl Phase {
    pub fn name(self) -> &'static str {
        match self {
            Phase::Dawn => "dawn",
            Phase::Day => "day",
            Phase::Dusk => "dusk",
            Phase::Night => "night",
        }
    }
}

/// Desired state at one moment.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub dark: bool,
    pub phase: Phase,
    /// Unix second of the next change of `dark` or `phase`.
    pub next: Option<i64>,
}

/// Last event at or before `now` and the first one after it.
fn current_and_next<T: Copy>(events: &mut [(i64, T)], now: i64) -> (Option<T>, Option<i64>) {
    events.sort_by_key(|e| e.0);
    let current = events.iter().rev().find(|e| e.0 <= now).map(|e| e.1);
    let next = events.iter().find(|e| e.0 > now).map(|e| e.0);
    (current, next)
}

fn minutes_to_ts(day: Date, minutes: u32, tz: &TimeZone) -> Option<i64> {
    let dt = day.at((minutes / 60) as i8, (minutes % 60) as i8, 0, 0);
    tz.to_timestamp(dt).ok().map(|t| t.as_second())
}

/// Sunrise and sunset of a local day as Unix seconds.
fn sun_ts(day: Date, loc: &Location) -> Option<(i64, i64)> {
    let (rise, set) =
        solar::sunrise_sunset_utc(i32::from(day.year()), day.month().into(), day.day().into(), loc.lat, loc.lon)?;
    let midnight = tz_midnight_utc(day)?;
    Some((midnight + (rise * 60.0).round() as i64, midnight + (set * 60.0).round() as i64))
}

fn tz_midnight_utc(day: Date) -> Option<i64> {
    TimeZone::UTC.to_timestamp(day.at(0, 0, 0, 0)).ok().map(|t| t.as_second())
}

/// Light/dark switch times (rise, set) of a local day as Unix seconds.
fn switch_times(
    mode: Mode,
    settings: &Settings,
    location: Option<&Location>,
    day: Date,
    tz: &TimeZone,
) -> Option<(i64, i64)> {
    let fixed = |l, d| Some((minutes_to_ts(day, l, tz)?, minutes_to_ts(day, d, tz)?));
    match mode {
        Mode::Schedule => fixed(settings.light_at, settings.dark_at),
        // Fixed light/dark still need the sun for the dynamic wallpaper phases.
        Mode::Auto | Mode::Light | Mode::Dark => location
            .and_then(|loc| sun_ts(day, loc))
            .or_else(|| fixed(DEFAULT_LIGHT_AT, DEFAULT_DARK_AT)),
    }
}

/// Compute the plan for the Unix second `now`.
pub fn plan(settings: &Settings, location: Option<&Location>, now: i64, tz: &TimeZone) -> Plan {
    let today = Timestamp::from_second(now).map(|t| t.to_zoned(tz.clone()).date()).unwrap_or(date(1970, 1, 1));
    let days = [today.yesterday(), Ok(today), today.tomorrow()];

    let mut dark_events: Vec<(i64, bool)> = Vec::new();
    let mut phase_events: Vec<(i64, Phase)> = Vec::new();
    for day in days.into_iter().flatten() {
        let Some((rise, set)) = switch_times(settings.mode, settings, location, day, tz) else {
            continue;
        };
        dark_events.push((rise, false));
        dark_events.push((set, true));
        phase_events.push((rise - 30 * 60, Phase::Dawn));
        phase_events.push((rise + 60 * 60, Phase::Day));
        phase_events.push((set - 60 * 60, Phase::Dusk));
        phase_events.push((set + 30 * 60, Phase::Night));
    }

    let (day_dark, next_dark) = current_and_next(&mut dark_events, now);
    let (phase, next_phase) = current_and_next(&mut phase_events, now);
    let dark = match settings.mode {
        Mode::Light => false,
        Mode::Dark => true,
        Mode::Auto | Mode::Schedule => day_dark.unwrap_or(false),
    };
    let phase = phase.unwrap_or(if dark { Phase::Night } else { Phase::Day });
    let next_dark = matches!(settings.mode, Mode::Auto | Mode::Schedule).then_some(next_dark).flatten();
    let next_phase = (settings.wallpaper == WallpaperMode::Dynamic).then_some(next_phase).flatten();
    let next = [next_dark, next_phase].into_iter().flatten().min();
    Plan { dark, phase, next }
}

/// Find `dawn.*`, `day.*`, `dusk.*` or `night.*` in a folder.
pub fn dynamic_image(dir: &str, phase: Phase) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.file_stem().is_some_and(|s| s.eq_ignore_ascii_case(phase.name()))
                && p.extension().is_some_and(|e| {
                    ["jpg", "jpeg", "png", "webp", "avif", "jxl", "bmp", "tiff"]
                        .iter()
                        .any(|x| e.eq_ignore_ascii_case(x))
                })
        })
        .collect();
    found.sort();
    found.into_iter().next()
}

/// The wallpaper the settings ask for under `plan`, if any.
pub fn wallpaper_for(settings: &Settings, plan: &Plan) -> Option<PathBuf> {
    match settings.wallpaper {
        WallpaperMode::None => None,
        WallpaperMode::Pair => {
            let p = if plan.dark { &settings.wallpaper_dark } else { &settings.wallpaper_light };
            let p = p.trim();
            (!p.is_empty()).then(|| PathBuf::from(p))
        }
        WallpaperMode::Dynamic => dynamic_image(settings.dynamic_dir.trim(), plan.phase),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc_ts(y: i16, mo: i8, d: i8, h: i8, mi: i8) -> i64 {
        TimeZone::UTC.to_timestamp(date(y, mo, d).at(h, mi, 0, 0)).unwrap().as_second()
    }

    fn schedule_settings() -> Settings {
        Settings { mode: Mode::Schedule, light_at: 6 * 60, dark_at: 20 * 60, ..Settings::default() }
    }

    #[test]
    fn schedule_day_and_night() {
        let s = schedule_settings();
        let tz = TimeZone::UTC;
        let noon = plan(&s, None, utc_ts(2024, 5, 1, 12, 0), &tz);
        assert!(!noon.dark);
        assert_eq!(noon.next, Some(utc_ts(2024, 5, 1, 20, 0)));
        let late = plan(&s, None, utc_ts(2024, 5, 1, 22, 0), &tz);
        assert!(late.dark);
        assert_eq!(late.next, Some(utc_ts(2024, 5, 2, 6, 0)));
        let early = plan(&s, None, utc_ts(2024, 5, 1, 3, 0), &tz);
        assert!(early.dark);
        assert_eq!(early.next, Some(utc_ts(2024, 5, 1, 6, 0)));
    }

    #[test]
    fn fixed_modes_have_no_transition() {
        let mut s = Settings { mode: Mode::Dark, ..Settings::default() };
        let p = plan(&s, None, utc_ts(2024, 5, 1, 12, 0), &TimeZone::UTC);
        assert!(p.dark);
        assert_eq!(p.next, None);
        s.mode = Mode::Light;
        assert!(!plan(&s, None, utc_ts(2024, 5, 1, 23, 0), &TimeZone::UTC).dark);
    }

    #[test]
    fn auto_without_location_uses_defaults() {
        let s = Settings::default();
        let tz = TimeZone::UTC;
        assert!(!plan(&s, None, utc_ts(2024, 5, 1, 8, 0), &tz).dark);
        assert!(plan(&s, None, utc_ts(2024, 5, 1, 19, 30), &tz).dark);
    }

    #[test]
    fn auto_follows_the_sun() {
        let s = Settings::default();
        let london = Location { city: "London".into(), lat: 51.5074, lon: -0.1278 };
        let tz = TimeZone::UTC;
        // Summer: sunrise ~03:43 UTC, sunset ~20:21 UTC.
        assert!(plan(&s, Some(&london), utc_ts(2024, 6, 21, 3, 0), &tz).dark);
        assert!(!plan(&s, Some(&london), utc_ts(2024, 6, 21, 4, 0), &tz).dark);
        assert!(!plan(&s, Some(&london), utc_ts(2024, 6, 21, 20, 0), &tz).dark);
        assert!(plan(&s, Some(&london), utc_ts(2024, 6, 21, 21, 0), &tz).dark);
    }

    #[test]
    fn dynamic_phases_and_wakeups() {
        let s = Settings { wallpaper: WallpaperMode::Dynamic, ..schedule_settings() };
        let tz = TimeZone::UTC;
        assert_eq!(plan(&s, None, utc_ts(2024, 5, 1, 5, 45), &tz).phase, Phase::Dawn);
        assert_eq!(plan(&s, None, utc_ts(2024, 5, 1, 12, 0), &tz).phase, Phase::Day);
        assert_eq!(plan(&s, None, utc_ts(2024, 5, 1, 19, 30), &tz).phase, Phase::Dusk);
        let night = plan(&s, None, utc_ts(2024, 5, 1, 23, 0), &tz);
        assert_eq!(night.phase, Phase::Night);
        assert_eq!(night.next, Some(utc_ts(2024, 5, 2, 5, 30)));
    }

    #[test]
    fn pair_picks_by_mode() {
        let s = Settings {
            wallpaper: WallpaperMode::Pair,
            wallpaper_light: "/l.jpg".into(),
            wallpaper_dark: "/d.jpg".into(),
            ..Settings::default()
        };
        let p = Plan { dark: true, phase: Phase::Night, next: None };
        assert_eq!(wallpaper_for(&s, &p), Some(PathBuf::from("/d.jpg")));
    }

    #[test]
    fn dynamic_image_lookup() {
        let dir = std::env::temp_dir().join(format!("mac-appearance-dyn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Dusk.PNG"), b"x").unwrap();
        std::fs::write(dir.join("day.txt"), b"x").unwrap();
        let found = dynamic_image(dir.to_str().unwrap(), Phase::Dusk);
        assert_eq!(found, Some(dir.join("Dusk.PNG")));
        assert_eq!(dynamic_image(dir.to_str().unwrap(), Phase::Day), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
