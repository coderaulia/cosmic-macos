// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use crate::config::TimeAppletConfig;
use icu::{
    datetime::{
        DateTimeFormatter, DateTimeFormatterPreferences, fieldsets,
        input::{Date as IcuDate, DateTime, Time as IcuTime},
    },
    locale::Locale,
};
use jiff::{
    ToSpan, Zoned,
    civil::{Date, Weekday},
    fmt::strtime,
};

/// Gets the first date that will be visible on the calendar
pub fn get_calendar_first(year: i16, month: i8, from_weekday: Weekday) -> Date {
    let date = Date::new(year, month, 1).expect("valid date");
    let num_days = date.weekday().since(from_weekday);
    date.checked_sub(num_days.days()).expect("valid date")
}

/// Format the top-panel clock string in macOS menu bar style:
/// `Sat 14 Nov 12:46` (24h) or `Sat 14 Nov 12:46 PM` (12h),
/// honoring `format_strftime`, `show_date_in_top_panel`, `military_time`, and `show_seconds`.
pub fn format_macos_clock(now: &Zoned, config: &TimeAppletConfig, locale: &Locale) -> String {
    if !config.format_strftime.is_empty()
        && let Ok(formatted) = strtime::format(&config.format_strftime, now)
    {
        return formatted;
    }

    let time_fmt = match (config.military_time, config.show_seconds) {
        (true, false) => "%H:%M",
        (true, true) => "%H:%M:%S",
        (false, false) => "%-I:%M %p",
        (false, true) => "%-I:%M:%S %p",
    };
    let time_str = strtime::format(time_fmt, now).unwrap_or_default();

    if !config.show_date_in_top_panel {
        return time_str;
    }

    let date_str = if locale.id.language.as_str() == "en" {
        strtime::format("%a %-d %b", now).unwrap_or_default()
    } else {
        let datetime = DateTime {
            date: IcuDate::try_new_gregorian(
                now.year() as i32,
                now.month() as u8,
                now.day() as u8,
            )
            .unwrap(),
            time: IcuTime::try_new(now.hour() as u8, now.minute() as u8, now.second() as u8, 0)
                .unwrap(),
        };
        let prefs = DateTimeFormatterPreferences::from(locale.clone());
        DateTimeFormatter::try_new(prefs, fieldsets::MDE::medium())
            .map(|fmt| fmt.format(&datetime).to_string())
            .unwrap_or_else(|_| strtime::format("%a %-d %b", now).unwrap_or_default())
    };

    format!("{date_str} {time_str}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::tz::TimeZone;

    fn sample_zoned(year: i16, month: i8, day: i8, hour: i8, min: i8, sec: i8) -> Zoned {
        jiff::civil::date(year, month, day)
            .at(hour, min, sec, 0)
            .to_zoned(TimeZone::UTC)
            .unwrap()
    }

    #[test]
    fn formats_24h_macos_date_time() {
        let now = sample_zoned(2026, 11, 14, 12, 46, 5);
        let locale = Locale::try_from_str("en-US").unwrap();
        let cfg = TimeAppletConfig {
            military_time: true,
            show_seconds: false,
            show_date_in_top_panel: true,
            ..Default::default()
        };
        assert_eq!(format_macos_clock(&now, &cfg, &locale), "Sat 14 Nov 12:46");

        let morning = sample_zoned(2026, 11, 14, 9, 5, 0);
        assert_eq!(
            format_macos_clock(&morning, &cfg, &locale),
            "Sat 14 Nov 09:05"
        );
    }

    #[test]
    fn formats_12h_macos_date_time() {
        let afternoon = sample_zoned(2026, 11, 14, 14, 5, 9);
        let locale = Locale::try_from_str("en-US").unwrap();
        let cfg = TimeAppletConfig {
            military_time: false,
            show_seconds: false,
            show_date_in_top_panel: true,
            ..Default::default()
        };
        assert_eq!(
            format_macos_clock(&afternoon, &cfg, &locale),
            "Sat 14 Nov 2:05 PM"
        );

        let midnight = sample_zoned(2026, 11, 14, 0, 5, 0);
        assert_eq!(
            format_macos_clock(&midnight, &cfg, &locale),
            "Sat 14 Nov 12:05 AM"
        );
    }

    #[test]
    fn formats_seconds_and_time_only_and_strftime() {
        let now = sample_zoned(2026, 11, 14, 12, 46, 8);
        let locale = Locale::try_from_str("en-US").unwrap();

        let with_sec = TimeAppletConfig {
            military_time: true,
            show_seconds: true,
            show_date_in_top_panel: true,
            ..Default::default()
        };
        assert_eq!(
            format_macos_clock(&now, &with_sec, &locale),
            "Sat 14 Nov 12:46:08"
        );

        let time_only_12h = TimeAppletConfig {
            military_time: false,
            show_seconds: false,
            show_date_in_top_panel: false,
            ..Default::default()
        };
        assert_eq!(
            format_macos_clock(&now, &time_only_12h, &locale),
            "12:46 PM"
        );

        let custom = TimeAppletConfig {
            format_strftime: "%Y-%m-%d %H:%M".to_string(),
            ..Default::default()
        };
        assert_eq!(
            format_macos_clock(&now, &custom, &locale),
            "2026-11-14 12:46"
        );
    }
}
