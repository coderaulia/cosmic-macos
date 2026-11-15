// SPDX-License-Identifier: GPL-3.0-only
//! Widget section of the popup: weather, up next and system cards.

use cosmic::{
    Element,
    iced::{
        Alignment, Length,
        widget::{column, progress_bar, row},
    },
    widget::{container, icon, text},
};
use mac_widgets::{
    settings::{Units, WidgetSettings},
    stats::{Stats, format_bytes},
    weather::{Forecast, describe},
};

use crate::window::{AgendaEntry, Message};

/// State of the weather card.
#[derive(Clone, Debug, Default)]
pub enum WeatherState {
    /// Not requested yet, or a fetch is in flight.
    #[default]
    Loading,
    /// No `location.conf`.
    NoLocation,
    Failed,
    Ready(Forecast),
}

/// A rounded, subtly tinted card.
fn card<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content)
        .padding(12)
        .width(Length::Fill)
        .class(cosmic::theme::Container::Card)
        .into()
}

fn weekday(date: &str) -> String {
    date.parse::<jiff::civil::Date>()
        .map(|d| d.strftime("%a").to_string())
        .unwrap_or_default()
}

pub fn weather_card<'a>(state: &'a WeatherState, units: Units) -> Element<'a, Message> {
    match state {
        WeatherState::Loading => card(text::caption("Loading weather…")),
        WeatherState::NoLocation => card(text::caption(
            "Weather unavailable: set a location in Appearance settings",
        )),
        WeatherState::Failed => card(text::caption("Weather unavailable")),
        WeatherState::Ready(f) => {
            let (label, icon_name) = describe(f.code);
            let current = row![
                icon::from_name(icon_name).size(40),
                column![
                    text::title2(units.format(f.temp_c)),
                    text::caption(label),
                ],
                column![text::heading(f.city.clone())]
                    .width(Length::Fill)
                    .align_x(Alignment::End),
            ]
            .spacing(12)
            .align_y(Alignment::Center);

            let days = f.days.iter().fold(row![].spacing(4), |r, d| {
                r.push(
                    column![
                        text::caption(weekday(&d.date)),
                        icon::from_name(describe(d.code).1).size(20),
                        text::caption(units.format(d.max_c)),
                        text::caption(units.format(d.min_c)),
                    ]
                    .spacing(2)
                    .width(Length::Fill)
                    .align_x(Alignment::Center),
                )
            });
            card(column![current, days].spacing(10))
        }
    }
}

/// The next (up to) three events at or after `now`; `agenda` is unsorted.
pub fn upcoming<'a>(
    agenda: &'a [AgendaEntry],
    today: jiff::civil::Date,
    now_hm: &str,
) -> Vec<&'a AgendaEntry> {
    let mut next: Vec<&AgendaEntry> = agenda
        .iter()
        .filter(|e| match &e.time {
            _ if e.date > today => true,
            Some(t) => e.date == today && t.as_str() >= now_hm,
            None => e.date == today,
        })
        .collect();
    next.sort_by(|a, b| (a.date, a.time.as_deref()).cmp(&(b.date, b.time.as_deref())));
    next.truncate(3);
    next
}

pub fn up_next_card<'a>(
    events: Vec<&'a AgendaEntry>,
    today: jiff::civil::Date,
) -> Element<'a, Message> {
    let mut list = column![text::heading("Up Next")].spacing(6);
    if events.is_empty() {
        list = list.push(text::caption("No upcoming events"));
    }
    for e in events {
        let when = match (&e.time, e.date == today) {
            (Some(t), true) => t.clone(),
            (Some(t), false) => format!("{} {t}", e.date.strftime("%a")),
            (None, true) => "All day".to_owned(),
            (None, false) => format!("{} all day", e.date.strftime("%a")),
        };
        list = list.push(
            row![
                text::caption(when).width(Length::Fixed(84.0)),
                text::body(e.title.clone()).width(Length::Fill),
            ]
            .spacing(8),
        );
    }
    card(list)
}

fn meter<'a>(label: &'static str, fraction: f32, detail: String) -> Element<'a, Message> {
    column![
        row![
            text::caption(label).width(Length::Fill),
            text::caption(detail),
        ],
        progress_bar(0.0..=1.0, fraction.clamp(0.0, 1.0)).girth(6),
    ]
    .spacing(3)
    .into()
}

pub fn system_card<'a>(stats: &Stats) -> Element<'a, Message> {
    let mut list = column![
        text::heading("System"),
        meter(
            "CPU",
            stats.cpu_percent / 100.0,
            format!("{:.0}%", stats.cpu_percent)
        ),
        meter(
            "Memory",
            stats.ram.fraction(),
            format!(
                "{} / {}",
                format_bytes(stats.ram.used),
                format_bytes(stats.ram.total)
            ),
        ),
    ]
    .spacing(8);
    if stats.swap.total > 0 {
        list = list.push(meter(
            "Swap",
            stats.swap.fraction(),
            format!(
                "{} / {}",
                format_bytes(stats.swap.used),
                format_bytes(stats.swap.total)
            ),
        ));
    }
    list = list.push(meter(
        "Disk",
        stats.disk.fraction(),
        format!(
            "{} / {}",
            format_bytes(stats.disk.used),
            format_bytes(stats.disk.total)
        ),
    ));
    if let Some(b) = stats.battery {
        list = list.push(meter(
            "Battery",
            f32::from(b.percent) / 100.0,
            format!("{}%{}", b.percent, if b.charging { " charging" } else { "" }),
        ));
    }
    card(list)
}

/// All enabled cards stacked, or `None` when everything is switched off.
pub fn section<'a>(
    settings: &WidgetSettings,
    weather: &'a WeatherState,
    stats: &Stats,
    agenda: &'a [AgendaEntry],
    today: jiff::civil::Date,
    now_hm: &str,
) -> Option<Element<'a, Message>> {
    let mut cards = column![].spacing(8).padding([4, 16]);
    let mut any = false;
    if settings.show_weather {
        cards = cards.push(weather_card(weather, settings.units));
        any = true;
    }
    if settings.show_up_next {
        cards = cards.push(up_next_card(upcoming(agenda, today, now_hm), today));
        any = true;
    }
    if settings.show_system {
        cards = cards.push(system_card(stats));
        any = true;
    }
    any.then(|| cards.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn entry(d: jiff::civil::Date, t: Option<&str>, title: &str) -> AgendaEntry {
        AgendaEntry {
            date: d,
            time: t.map(str::to_owned),
            title: title.to_owned(),
        }
    }

    #[test]
    fn upcoming_filters_and_sorts() {
        let today = date(2026, 1, 10);
        let agenda = vec![
            entry(date(2026, 1, 11), Some("09:00"), "tomorrow"),
            entry(today, Some("08:00"), "past"),
            entry(today, Some("15:00"), "later"),
            entry(date(2026, 1, 9), None, "yesterday"),
            entry(today, None, "all day"),
            entry(date(2026, 1, 20), None, "far"),
        ];
        let titles: Vec<_> = upcoming(&agenda, today, "12:00")
            .iter()
            .map(|e| e.title.as_str())
            .collect();
        assert_eq!(titles, ["all day", "later", "tomorrow"]);
    }
}
