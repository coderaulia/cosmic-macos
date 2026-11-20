// SPDX-License-Identifier: GPL-3.0-only
//! Sonoma-style frosted glass widget cards (`Weather`, `UpNext`, `System`,
//! `Clock`) in `Small`, `Medium`, and `Large` sizes, plus gallery tiles.

use cosmic::{
    Element,
    iced::{
        Alignment, Background, Border, Color, Length, Shadow, Vector,
        alignment::Horizontal,
        font::Weight,
        widget::{column, progress_bar, row},
    },
    theme::{Button, Container},
    widget::{button, container, divider, icon, space, text},
};
use jiff::{Zoned, civil::Date};
use mac_widgets::{
    settings::Units,
    stats::{Stats, format_bytes},
    weather::{Forecast, describe},
};

use crate::agenda::{AgendaEntry, upcoming};
use crate::battery::BatteryState;
use crate::cards_more::{NoteAction, battery_card, calendar_card, notes_card};
use crate::notes::Notes;
use crate::layout::{WidgetKind, WidgetSize};

/// Current weather state shared across surfaces.
#[derive(Clone, Debug, Default)]
pub enum WeatherState {
    #[default]
    Loading,
    NoLocation,
    Failed,
    Ready(Forecast),
}

/// Shared live data rendered inside widget cards.
#[derive(Clone, Copy)]
pub struct WidgetData<'a> {
    pub now: &'a Zoned,
    pub weather: &'a WeatherState,
    pub units: Units,
    pub stats: &'a Stats,
    pub agenda: &'a [AgendaEntry],
    pub battery: &'a BatteryState,
    pub notes: &'a Notes,
    pub note_draft: &'a str,
}

/// Frosted glass card style following the active COSMIC theme and dimming
/// slightly when a normal application window is focused.
pub fn frosted_card_class(dimmed: bool, editing: bool) -> Container<'static> {
    Container::custom(move |theme| {
        let c = theme.cosmic();
        let base = Color::from(c.bg_color());
        let fg = Color::from(c.on_bg_color());
        let accent = Color::from(c.accent_color());

        let bg_alpha = if editing {
            0.84
        } else if dimmed {
            0.54
        } else {
            0.76
        };
        let fg_alpha = if dimmed && !editing { 0.72 } else { 1.0 };
        let border_color = if editing {
            Color { a: 0.55, ..accent }
        } else {
            Color {
                a: if dimmed { 0.08 } else { 0.16 },
                ..fg
            }
        };

        cosmic::iced::widget::container::Style {
            text_color: Some(Color { a: fg_alpha, ..fg }),
            icon_color: Some(Color { a: fg_alpha, ..fg }),
            background: Some(Background::Color(Color {
                a: bg_alpha,
                ..base
            })),
            border: Border {
                radius: 20.0.into(),
                width: if editing { 1.5 } else { 1.0 },
                color: border_color,
            },
            shadow: Shadow {
                color: Color::from_rgba(0.0, 0.0, 0.0, if dimmed { 0.14 } else { 0.24 }),
                offset: Vector::new(0.0, 6.0),
                blur_radius: 18.0,
            },
            snap: true,
        }
    })
}

/// Frosted glass style for the bottom widget gallery strip and context menus.
pub fn frosted_chrome_class() -> Container<'static> {
    Container::custom(|theme| {
        let c = theme.cosmic();
        let base = Color::from(c.bg_color());
        let fg = Color::from(c.on_bg_color());
        cosmic::iced::widget::container::Style {
            text_color: Some(fg),
            icon_color: Some(fg),
            background: Some(Background::Color(Color { a: 0.90, ..base })),
            border: Border {
                radius: 16.0.into(),
                width: 1.0,
                color: Color { a: 0.18, ..fg },
            },
            shadow: Shadow {
                color: Color::from_rgba(0.0, 0.0, 0.0, 0.35),
                offset: Vector::new(0.0, 8.0),
                blur_radius: 24.0,
            },
            snap: true,
        }
    })
}

/// Renders one widget card at the requested `size`.
pub fn render_widget<'a, M: Clone + From<NoteAction> + 'static>(
    kind: WidgetKind,
    size: WidgetSize,
    data: WidgetData<'a>,
    dimmed: bool,
    editing: bool,
) -> Element<'a, M> {
    let (w, h) = size.dimensions();
    let inner: Element<'a, M> = match kind {
        WidgetKind::Weather => weather_card(size, data.weather, data.units),
        WidgetKind::UpNext => up_next_card(size, data.agenda, data.now),
        WidgetKind::System => system_card(size, data.stats),
        WidgetKind::Clock => clock_card(size, data.now),
        WidgetKind::CalendarMonth => calendar_card(size, data.agenda, data.now),
        WidgetKind::Battery => battery_card(size, data.battery),
        WidgetKind::Notes => notes_card(size, data.notes, data.note_draft, editing),
    };

    container(inner)
        .width(Length::Fixed(w as f32))
        .height(Length::Fixed(h as f32))
        .padding(14)
        .class(frosted_card_class(dimmed, editing))
        .into()
}

pub(crate) fn bold_font() -> cosmic::iced::Font {
    cosmic::iced::Font {
        weight: Weight::Bold,
        ..cosmic::font::default()
    }
}

pub(crate) fn semibold_font() -> cosmic::iced::Font {
    cosmic::iced::Font {
        weight: Weight::Semibold,
        ..cosmic::font::default()
    }
}

fn weekday_short(date: &str) -> String {
    date.parse::<Date>()
        .map(|d| d.strftime("%a").to_string())
        .unwrap_or_default()
}

fn weather_card<'a, M: 'static>(
    size: WidgetSize,
    state: &'a WeatherState,
    units: Units,
) -> Element<'a, M> {
    match state {
        WeatherState::Loading => column![
            text::heading("Weather"),
            space::vertical().height(Length::Fill),
            text::caption("Loading weather…"),
            space::vertical().height(Length::Fill),
        ]
        .into(),
        WeatherState::NoLocation => column![
            text::heading("Weather"),
            space::vertical().height(Length::Fill),
            text::caption("Set a location in Appearance settings"),
            space::vertical().height(Length::Fill),
        ]
        .into(),
        WeatherState::Failed => column![
            text::heading("Weather"),
            space::vertical().height(Length::Fill),
            text::caption("Weather unavailable"),
            space::vertical().height(Length::Fill),
        ]
        .into(),
        WeatherState::Ready(f) => {
            let (label, icon_name) = describe(f.code);
            let temp = units.format(f.temp_c);
            let city = if f.city.is_empty() {
                "Local".to_owned()
            } else {
                f.city.clone()
            };

            match size {
                WidgetSize::Small => {
                    let hl = f.days.first().map(|d| {
                        format!("H:{}  L:{}", units.format(d.max_c), units.format(d.min_c))
                    });
                    column![
                        row![
                            text::body(city).font(semibold_font()).width(Length::Fill),
                            icon::from_name(icon_name).size(24),
                        ]
                        .align_y(Alignment::Center),
                        space::vertical().height(Length::Fill),
                        text(temp).size(36.0).font(bold_font()),
                        text::caption(label),
                        if let Some(hl_text) = hl {
                            text::caption(hl_text)
                        } else {
                            text::caption("")
                        },
                    ]
                    .spacing(2)
                    .into()
                }
                WidgetSize::Medium => {
                    let current = row![
                        icon::from_name(icon_name).size(38),
                        column![
                            text(temp).size(28.0).font(bold_font()),
                            text::caption(label),
                        ],
                        column![text::heading(city)]
                            .width(Length::Fill)
                            .align_x(Alignment::End),
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center);

                    let days = f.days.iter().take(5).fold(row![].spacing(4), |r, d| {
                        r.push(
                            column![
                                text::caption(weekday_short(&d.date)),
                                icon::from_name(describe(d.code).1).size(18),
                                text::caption(units.format(d.max_c)),
                            ]
                            .spacing(2)
                            .width(Length::Fill)
                            .align_x(Alignment::Center),
                        )
                    });
                    column![current, space::vertical().height(Length::Fill), days]
                        .spacing(6)
                        .into()
                }
                WidgetSize::Large => {
                    let header = row![
                        column![
                            text::title3(city).font(semibold_font()),
                            text(temp).size(44.0).font(bold_font()),
                            text::body(label),
                        ]
                        .width(Length::Fill),
                        icon::from_name(icon_name).size(56),
                    ]
                    .align_y(Alignment::Center);

                    let mut rows = column![].spacing(8);
                    for d in f.days.iter().take(5) {
                        let (day_label, day_icon) = describe(d.code);
                        rows = rows.push(
                            row![
                                text::body(weekday_short(&d.date)).width(Length::Fixed(46.0)),
                                icon::from_name(day_icon).size(20),
                                text::caption(day_label).width(Length::Fill),
                                text::caption(format!(
                                    "{} / {}",
                                    units.format(d.min_c),
                                    units.format(d.max_c)
                                )),
                            ]
                            .spacing(8)
                            .align_y(Alignment::Center),
                        );
                    }
                    column![header, divider::horizontal::default(), rows]
                        .spacing(10)
                        .into()
                }
            }
        }
    }
}

fn up_next_card<'a, M: 'a>(
    size: WidgetSize,
    agenda: &'a [AgendaEntry],
    now: &'a Zoned,
) -> Element<'a, M> {
    let today = now.date();
    let now_hm = now.strftime("%H:%M").to_string();
    let limit = match size {
        WidgetSize::Small => 2,
        WidgetSize::Medium => 3,
        WidgetSize::Large => 6,
    };
    let events = upcoming(agenda, today, &now_hm, limit);
    let date_badge = now.strftime("%a %-d").to_string();

    let mut list = column![
        row![
            text::heading("Up Next").width(Length::Fill),
            text::caption(date_badge),
        ]
        .align_y(Alignment::Center)
    ]
    .spacing(6);

    if events.is_empty() {
        list = list
            .push(space::vertical().height(Length::Fill))
            .push(text::caption("No upcoming events"))
            .push(space::vertical().height(Length::Fill));
        return list.into();
    }

    for e in events {
        let when = match (&e.time, e.date == today) {
            (Some(t), true) => t.clone(),
            (Some(t), false) => format!("{} {t}", e.date.strftime("%a")),
            (None, true) => "All day".to_owned(),
            (None, false) => format!("{} all day", e.date.strftime("%a")),
        };
        if size == WidgetSize::Small {
            list = list.push(column![
                text::caption(when),
                text::body(truncate(&e.title, 18)).font(semibold_font()),
            ]);
        } else {
            list = list.push(
                row![
                    text::caption(when).width(Length::Fixed(82.0)),
                    text::body(truncate(&e.title, 28)).width(Length::Fill),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            );
        }
    }
    list.into()
}

fn meter<'a, M: 'a>(label: &'static str, fraction: f32, detail: String) -> Element<'a, M> {
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

fn system_card<'a, M: 'a>(size: WidgetSize, stats: &Stats) -> Element<'a, M> {
    match size {
        WidgetSize::Small => {
            let mut col = column![
                text::heading("System"),
                meter(
                    "CPU",
                    stats.cpu_percent / 100.0,
                    format!("{:.0}%", stats.cpu_percent),
                ),
                meter(
                    "RAM",
                    stats.ram.fraction(),
                    format!("{:.0}%", stats.ram.fraction() * 100.0),
                ),
            ]
            .spacing(8);
            if let Some(b) = stats.battery {
                col = col.push(meter(
                    "Battery",
                    f32::from(b.percent) / 100.0,
                    format!("{}%", b.percent),
                ));
            }
            col.into()
        }
        WidgetSize::Medium => {
            let left = column![
                meter(
                    "CPU",
                    stats.cpu_percent / 100.0,
                    format!("{:.0}%", stats.cpu_percent),
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
            .spacing(10)
            .width(Length::Fill);

            let mut right = column![meter(
                "Disk",
                stats.disk.fraction(),
                format!(
                    "{} / {}",
                    format_bytes(stats.disk.used),
                    format_bytes(stats.disk.total)
                ),
            )]
            .spacing(10)
            .width(Length::Fill);

            if let Some(b) = stats.battery {
                right = right.push(meter(
                    "Battery",
                    f32::from(b.percent) / 100.0,
                    format!("{}%{}", b.percent, if b.charging { "+" } else { "" }),
                ));
            } else if stats.swap.total > 0 {
                right = right.push(meter(
                    "Swap",
                    stats.swap.fraction(),
                    format!(
                        "{} / {}",
                        format_bytes(stats.swap.used),
                        format_bytes(stats.swap.total)
                    ),
                ));
            }

            column![
                text::heading("System"),
                row![left, right].spacing(12),
            ]
            .spacing(10)
            .into()
        }
        WidgetSize::Large => {
            let mut list = column![
                text::heading("System"),
                meter(
                    "CPU",
                    stats.cpu_percent / 100.0,
                    format!("{:.0}%", stats.cpu_percent),
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
            .spacing(12);
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
            list.into()
        }
    }
}

fn clock_card<'a, M: 'a>(size: WidgetSize, now: &'a Zoned) -> Element<'a, M> {
    let hm = now.strftime("%H:%M").to_string();
    let weekday = now.strftime("%A").to_string();
    let date_short = now.strftime("%a, %b %-d").to_string();
    let date_long = now.strftime("%B %-d, %Y").to_string();

    match size {
        WidgetSize::Small => column![
            text::caption(date_short).font(semibold_font()),
            space::vertical().height(Length::Fill),
            text(hm).size(44.0).font(bold_font()),
            space::vertical().height(Length::Fill),
            text::caption(now.time_zone().iana_name().unwrap_or("Local")),
        ]
        .align_x(Alignment::Center)
        .width(Length::Fill)
        .into(),
        // No seconds: like macOS, and a mixed-size "HH:MM:SS" row never
        // lined up on one baseline.
        WidgetSize::Medium => column![
            row![
                text::body(weekday).font(semibold_font()).width(Length::Fill),
                text::caption(date_long),
            ],
            space::vertical().height(Length::Fill),
            text(hm).size(64.0).font(bold_font()),
            space::vertical().height(Length::Fill),
        ]
        .into(),
        WidgetSize::Large => {
            let today = now.date();
            let first = today.first_of_month();
            let days_in_month = today.days_in_month();
            let start_offset = first.weekday().to_monday_zero_offset();

            let header = row![
                column![
                    text(hm).size(42.0).font(bold_font()),
                    text::caption(format!("{weekday}, {date_long}")),
                ]
                .width(Length::Fill),
                text::title4(now.strftime("%b").to_string().to_uppercase()).font(bold_font()),
            ]
            .align_y(Alignment::Center);

            let weekdays = ["M", "T", "W", "T", "F", "S", "S"].iter().fold(
                row![].spacing(4),
                |r, &d| {
                    r.push(
                        container(text::caption(d))
                            .width(Length::Fill)
                            .align_x(Horizontal::Center),
                    )
                },
            );

            let mut grid = column![weekdays].spacing(4);
            let mut week = row![].spacing(4);
            for _ in 0..start_offset {
                week = week.push(container(text::caption("")).width(Length::Fill));
            }
            let mut col_idx = start_offset;
            for day in 1..=days_in_month {
                let is_today = day == today.day();
                let cell = if is_today {
                    container(text::caption(format!("{day}")).font(bold_font()))
                        .width(Length::Fill)
                        .padding([2, 0])
                        .align_x(Horizontal::Center)
                        .class(Container::custom(|theme| {
                            let accent = Color::from(theme.cosmic().accent_color());
                            cosmic::iced::widget::container::Style {
                                background: Some(Background::Color(accent)),
                                text_color: Some(Color::from_rgb(0.98, 0.99, 1.0)),
                                border: Border {
                                    radius: 10.0.into(),
                                    ..Default::default()
                                },
                                ..Default::default()
                            }
                        }))
                } else {
                    container(text::caption(format!("{day}")))
                        .width(Length::Fill)
                        .padding([2, 0])
                        .align_x(Horizontal::Center)
                };
                week = week.push(cell);
                col_idx += 1;
                if col_idx == 7 {
                    grid = grid.push(week);
                    week = row![].spacing(4);
                    col_idx = 0;
                }
            }
            if col_idx > 0 {
                while col_idx < 7 {
                    week = week.push(container(text::caption("")).width(Length::Fill));
                    col_idx += 1;
                }
                grid = grid.push(week);
            }

            column![header, divider::horizontal::default(), grid]
                .spacing(10)
                .into()
        }
    }
}

/// Compact badge button used for the edit-mode `×` remove overlay.
pub fn remove_badge_button<'a, M: Clone + 'a>(on_press: M) -> Element<'a, M> {
    button::custom(
        container(text("×").size(14.0).font(bold_font()))
            .center_x(Length::Fill)
            .center_y(Length::Fill),
    )
    .width(Length::Fixed(24.0))
    .height(Length::Fixed(24.0))
    .padding(0)
    .class(Button::Custom {
        active: Box::new(|_, theme| {
            let fg = Color::from(theme.cosmic().on_bg_color());
            let bg = Color::from(theme.cosmic().bg_color());
            button::Style {
                background: Some(Background::Color(Color { a: 0.92, ..bg })),
                text_color: Some(fg),
                border_radius: 12.0.into(),
                border_width: 1.0,
                border_color: Color { a: 0.25, ..fg },
                ..Default::default()
            }
        }),
        hovered: Box::new(|_, theme| {
            let fg = Color::from(theme.cosmic().on_bg_color());
            button::Style {
                background: Some(Background::Color(Color::from_rgb(0.85, 0.24, 0.22))),
                text_color: Some(Color::WHITE),
                border_radius: 12.0.into(),
                border_width: 1.0,
                border_color: Color { a: 0.3, ..fg },
                ..Default::default()
            }
        }),
        pressed: Box::new(|_, _| button::Style {
            background: Some(Background::Color(Color::from_rgb(0.72, 0.18, 0.18))),
            text_color: Some(Color::WHITE),
            border_radius: 12.0.into(),
            ..Default::default()
        }),
        disabled: Box::new(|_| button::Style::new()),
    })
    .on_press(on_press)
    .into()
}

/// Preview tile shown inside the bottom "Edit Widgets…" gallery strip.
pub fn gallery_tile<'a, M: 'static>(kind: WidgetKind, size: WidgetSize) -> Element<'a, M> {
    container(
        column![
            icon::from_name(kind.icon_name()).size(28),
            text::body(kind.label()).font(semibold_font()),
            text::caption(size.label()),
        ]
        .spacing(4)
        .align_x(Alignment::Center),
    )
    .width(Length::Fixed(116.0))
    .height(Length::Fixed(88.0))
    .center_x(Length::Fixed(116.0))
    .center_y(Length::Fixed(88.0))
    .class(Container::custom(|theme| {
        let c = theme.cosmic();
        let fg = Color::from(c.on_bg_color());
        cosmic::iced::widget::container::Style {
            text_color: Some(fg),
            icon_color: Some(fg),
            background: Some(Background::Color(Color { a: 0.08, ..fg })),
            border: Border {
                radius: 14.0.into(),
                width: 1.0,
                color: Color { a: 0.16, ..fg },
            },
            ..Default::default()
        }
    }))
    .into()
}

pub(crate) fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() > max_chars {
        let mut out: String = value.chars().take(max_chars.saturating_sub(1)).collect();
        out.push('…');
        out
    } else {
        value.to_owned()
    }
}
