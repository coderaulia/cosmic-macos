// SPDX-License-Identifier: GPL-3.0-only
//! Calendar month, Batteries and Notes widget cards.

use std::collections::HashSet;
use std::f32::consts::{FRAC_PI_2, TAU};

use cosmic::{
    Element,
    iced::{
        Alignment, Background, Border, Color, Length, Radians, Rectangle, mouse,
        widget::{checkbox, column, row, stack},
    },
    theme::Container,
    widget::{
        button, canvas, container, divider, icon, space, text, text_input,
    },
};
use jiff::Zoned;

use crate::agenda::{AgendaEntry, upcoming};
use crate::battery::{self, BatteryState, ChargeState, DeviceInfo};
use crate::cards::{bold_font, semibold_font, truncate};
use crate::layout::WidgetSize;
use crate::monthgrid::{days_with_events, month_weeks};
use crate::notes::Notes;

/// User interaction inside the Notes card.
#[derive(Clone, Debug)]
pub enum NoteAction {
    Input(String),
    Submit,
    Toggle(usize),
    Delete(usize),
}

// ---------------------------------------------------------------- Calendar

fn month_grid<'a, M: 'a>(
    now: &Zoned,
    event_days: &HashSet<i8>,
    text_size: f32,
    cell_h: f32,
    circle: f32,
) -> Element<'a, M> {
    let today = now.date();
    let header = ["M", "T", "W", "T", "F", "S", "S"]
        .iter()
        .fold(row![], |r, &d| {
            r.push(
                container(text(d).size(text_size - 1.0))
                    .width(Length::Fill)
                    .center_x(Length::Fill)
                    .class(Container::custom(|theme| {
                        let fg = Color::from(theme.cosmic().on_bg_color());
                        cosmic::iced::widget::container::Style {
                            text_color: Some(Color { a: 0.55, ..fg }),
                            ..Default::default()
                        }
                    })),
            )
        });
    let mut grid = column![header];
    for week in month_weeks(today) {
        let mut line = row![];
        for cell in week {
            let Some(day) = cell else {
                line = line.push(space::horizontal().width(Length::Fill));
                continue;
            };
            let is_today = day == today.day();
            let num = container(if is_today {
                text(format!("{day}")).size(text_size).font(bold_font())
            } else {
                text(format!("{day}")).size(text_size)
            })
            .width(Length::Fixed(circle))
            .height(Length::Fixed(circle))
            .center_x(Length::Fixed(circle))
            .center_y(Length::Fixed(circle));
            let num = if is_today {
                num.class(Container::custom(move |theme| {
                    let accent = Color::from(theme.cosmic().accent_color());
                    cosmic::iced::widget::container::Style {
                        background: Some(Background::Color(accent)),
                        text_color: Some(Color::from_rgb(0.98, 0.99, 1.0)),
                        border: Border {
                            radius: (circle / 2.0).into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }
                }))
            } else {
                num
            };
            let dot: Element<'a, M> = if event_days.contains(&day) {
                container(space::horizontal())
                    .width(Length::Fixed(3.0))
                    .height(Length::Fixed(3.0))
                    .class(Container::custom(|theme| {
                        let c = theme.cosmic();
                        let fg = Color::from(c.on_bg_color());
                        cosmic::iced::widget::container::Style {
                            background: Some(Background::Color(Color { a: 0.6, ..fg })),
                            border: Border {
                                radius: 1.5.into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        }
                    }))
                    .into()
            } else {
                space::vertical().height(Length::Fixed(3.0)).into()
            };
            line = line.push(
                container(column![num, dot].align_x(Alignment::Center).spacing(1))
                    .width(Length::Fill)
                    .height(Length::Fixed(cell_h))
                    .center_x(Length::Fill),
            );
        }
        grid = grid.push(line);
    }
    grid.into()
}

fn event_lines<'a, M: 'a>(
    agenda: &'a [AgendaEntry],
    now: &Zoned,
    limit: usize,
) -> Element<'a, M> {
    let today = now.date();
    let now_hm = now.strftime("%H:%M").to_string();
    let events = upcoming(agenda, today, &now_hm, limit);
    if events.is_empty() {
        return text::caption("No upcoming events").into();
    }
    let mut col = column![].spacing(5);
    for e in events {
        let when = match (&e.time, e.date == today) {
            (Some(t), true) => t.clone(),
            (Some(t), false) => format!("{} {t}", e.date.strftime("%a")),
            (None, true) => "All day".to_owned(),
            (None, false) => format!("{} all day", e.date.strftime("%a")),
        };
        col = col.push(column![
            text::caption(when),
            text::body(truncate(&e.title, 22)).font(semibold_font()),
        ]);
    }
    col.into()
}

pub fn calendar_card<'a, M: 'a>(
    size: WidgetSize,
    agenda: &'a [AgendaEntry],
    now: &'a Zoned,
) -> Element<'a, M> {
    let today = now.date();
    let days = days_with_events(agenda, today);
    let month_name = now.strftime("%B").to_string().to_uppercase();
    let month_title = || {
        text::caption(month_name.clone())
            .font(bold_font())
            .class(cosmic::theme::Text::Accent)
    };

    match size {
        WidgetSize::Small => column![
            month_title(),
            month_grid(now, &days, 10.0, 19.0, 16.0)
        ]
        .spacing(2)
        .into(),
        WidgetSize::Medium => {
            let left = column![
                month_title(),
                month_grid(now, &days, 10.0, 19.0, 16.0)
            ]
            .spacing(2)
            .width(Length::Fixed(152.0));
            let day_header = column![
                text::caption(now.strftime("%A").to_string().to_uppercase())
                    .font(bold_font())
                    .class(cosmic::theme::Text::Accent),
                text::title2(format!("{}", today.day())).font(bold_font()),
            ];
            let right = column![day_header, event_lines(agenda, now, 2)]
                .spacing(6)
                .width(Length::Fill);
            row![left, right].spacing(14).into()
        }
        WidgetSize::Large => column![
            row![
                month_title(),
                text::caption(now.strftime("%Y").to_string())
                    .width(Length::Fill)
                    .align_x(cosmic::iced::alignment::Horizontal::Right),
            ],
            month_grid(now, &days, 12.0, 27.0, 22.0),
            divider::horizontal::default(),
            event_lines(agenda, now, 2),
        ]
        .spacing(6)
        .into(),
    }
}

// ---------------------------------------------------------------- Battery

struct Ring {
    fraction: f32,
    color: Color,
    track: Color,
    width: f32,
}

impl<M> canvas::Program<M, cosmic::Theme, cosmic::Renderer> for Ring {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &cosmic::Renderer,
        _theme: &cosmic::Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry<cosmic::Renderer>> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let center = frame.center();
        let radius = (bounds.width.min(bounds.height) - self.width) / 2.0;
        frame.stroke(
            &canvas::Path::circle(center, radius),
            canvas::Stroke::default()
                .with_width(self.width)
                .with_color(self.track),
        );
        if self.fraction > 0.0 {
            let start = -FRAC_PI_2;
            let end = start + TAU * self.fraction.clamp(0.0, 1.0);
            let arc = canvas::Path::new(|b| {
                b.arc(canvas::path::Arc {
                    center,
                    radius,
                    start_angle: Radians(start),
                    end_angle: Radians(end),
                });
            });
            frame.stroke(
                &arc,
                canvas::Stroke::default()
                    .with_width(self.width)
                    .with_color(self.color)
                    .with_line_cap(canvas::LineCap::Round),
            );
        }
        vec![frame.into_geometry()]
    }
}

fn level_color(percent: u8, state: ChargeState) -> Color {
    match battery::level(percent, state) {
        0 => Color::from_rgb(0.20, 0.78, 0.35),
        1 => Color::from_rgb(1.0, 0.62, 0.04),
        _ => Color::from_rgb(1.0, 0.27, 0.23),
    }
}

/// Ring with an icon and the percentage inside.
fn battery_ring<'a, M: 'static>(dev: &DeviceInfo, diameter: f32, show_icon: bool) -> Element<'a, M> {
    let stroke = (diameter / 9.0).max(4.0);
    let ring = canvas(Ring {
        fraction: f32::from(dev.percent) / 100.0,
        color: level_color(dev.percent, dev.state),
        track: Color::from_rgba(0.5, 0.5, 0.5, 0.25),
        width: stroke,
    })
    .width(Length::Fixed(diameter))
    .height(Length::Fixed(diameter));

    let pct_size = (diameter * 0.26).max(10.0);
    let mut inner = column![].align_x(Alignment::Center).spacing(0);
    if show_icon {
        inner = inner.push(icon::from_name(dev.kind.icon_name()).size((diameter * 0.26) as u16));
    }
    let suffix = if dev.state == ChargeState::Charging { "⚡" } else { "" };
    inner = inner.push(
        text(format!("{}%{suffix}", dev.percent))
            .size(pct_size)
            .font(semibold_font()),
    );
    stack![
        ring,
        container(inner)
            .width(Length::Fixed(diameter))
            .height(Length::Fixed(diameter))
            .center_x(Length::Fixed(diameter))
            .center_y(Length::Fixed(diameter)),
    ]
    .width(Length::Fixed(diameter))
    .height(Length::Fixed(diameter))
    .into()
}

pub fn battery_card<'a, M: 'static>(size: WidgetSize, state: &'a BatteryState) -> Element<'a, M> {
    let devices: &[DeviceInfo] = match state {
        BatteryState::Loading => {
            return column![
                text::heading("Batteries"),
                space::vertical().height(Length::Fill),
                text::caption("Loading…"),
                space::vertical().height(Length::Fill),
            ]
            .into();
        }
        BatteryState::Ready(d) => d,
    };
    if devices.is_empty() {
        return column![
            text::heading("Batteries"),
            space::vertical().height(Length::Fill),
            container(
                column![
                    icon::from_name("battery-missing-symbolic").size(28),
                    text::body("No batteries"),
                ]
                .align_x(Alignment::Center)
                .spacing(4)
            )
            .width(Length::Fill)
            .center_x(Length::Fill),
            space::vertical().height(Length::Fill),
        ]
        .into();
    }

    match size {
        WidgetSize::Small => {
            let d = &devices[0];
            column![
                row![
                    text::caption(truncate(&d.name, 16))
                        .font(semibold_font())
                        .width(Length::Fill),
                    icon::from_name(d.kind.icon_name()).size(16),
                ]
                .align_y(Alignment::Center),
                container(battery_ring(d, 96.0, false))
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .center_x(Length::Fill)
                    .center_y(Length::Fill),
            ]
            .into()
        }
        WidgetSize::Medium => {
            let mut rings = row![].spacing(8).width(Length::Fill);
            for d in devices.iter().take(4) {
                rings = rings.push(
                    column![
                        battery_ring(d, 64.0, true),
                        text::caption(truncate(&d.name, 11)),
                    ]
                    .spacing(4)
                    .width(Length::Fill)
                    .align_x(Alignment::Center),
                );
            }
            column![
                text::heading("Batteries"),
                container(rings)
                    .height(Length::Fill)
                    .center_y(Length::Fill),
            ]
            .into()
        }
        WidgetSize::Large => {
            let mut list = column![text::heading("Batteries")].spacing(8);
            for d in devices.iter().take(5) {
                list = list.push(
                    row![
                        battery_ring(d, 44.0, false),
                        column![
                            text::body(truncate(&d.name, 24)).font(semibold_font()),
                            text::caption(if d.state.label().is_empty() {
                                d.kind.label()
                            } else {
                                d.state.label()
                            }),
                        ]
                        .width(Length::Fill),
                        icon::from_name(d.kind.icon_name()).size(18),
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                );
            }
            list.into()
        }
    }
}

// ---------------------------------------------------------------- Notes

pub fn notes_card<'a, M: Clone + From<NoteAction> + 'static>(
    size: WidgetSize,
    notes: &'a Notes,
    draft: &'a str,
    editing: bool,
) -> Element<'a, M> {
    let (limit, label_chars) = match size {
        WidgetSize::Small => (3, 13),
        WidgetSize::Medium => (3, 38),
        WidgetSize::Large => (10, 32),
    };
    let remaining = notes.items.iter().filter(|i| !i.done).count();

    let mut input = text_input("Add item…", draft).size(12).padding([4, 8]);
    if !editing {
        input = input
            .on_input(|s| NoteAction::Input(s).into())
            .on_submit(|_| NoteAction::Submit.into());
    }

    let mut list = column![
        row![
            text::heading("Notes").width(Length::Fill),
            text::caption(format!("{remaining} left")),
        ]
        .align_y(Alignment::Center),
        input,
    ]
    .spacing(6);

    let order = notes.display_order();
    if order.is_empty() {
        list = list.push(text::caption("Nothing to do"));
    }
    for &idx in order.iter().take(limit) {
        let item = &notes.items[idx];
        let mut cb = checkbox(item.done)
            .label(truncate(&item.text, label_chars))
            .size(14)
            .spacing(6)
            .width(Length::Fill);
        let mut del = button::icon(icon::from_name("window-close-symbolic").size(12)).padding(2);
        if !editing {
            cb = cb.on_toggle(move |_| NoteAction::Toggle(idx).into());
            del = del.on_press(NoteAction::Delete(idx).into());
        }
        list = list.push(row![cb, del].spacing(4).align_y(Alignment::Center));
    }
    if order.len() > limit {
        list = list.push(text::caption(format!("+{} more", order.len() - limit)));
    }
    list.into()
}
