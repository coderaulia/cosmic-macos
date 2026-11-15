// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use cosmic::{
    Apply, Element, Task, app,
    applet::{cosmic_panel_config::PanelAnchor, menu_button, padded_control},
    cctk::sctk::reexports::calloop,
    cosmic_config::{Config, CosmicConfigEntry},
    cosmic_theme::Spacing,
    iced::stream,
    iced::widget::Column,
    iced::{
        Alignment, Length, Rectangle, Subscription,
        futures::{SinkExt, StreamExt, channel::mpsc},
        platform_specific::shell::wayland::commands::popup::destroy_popup,
        widget::{column, rich_text, row, rule},
        window,
    },
    surface, theme,
    widget::{
        Button, Grid, Id, autosize, button, container, divider, grid, icon, mouse_area,
        rectangle_tracker::*, scrollable, space, text, toggler,
    },
};
use cosmic_ext_mac_applet_style::{
    PillVariant, mac_pill_button, mac_popover_container_sized, panel_text_size,
};
use cosmic_notifications_config::NotificationsConfig;
use cosmic_notifications_util::{ActionId, Notification, markup};
use mpris2_zbus::player::PlaybackStatus;
use std::collections::HashMap;

use crate::widgets::{self, WeatherState};
use mac_widgets::{
    settings::WidgetSettings,
    stats::{CpuSample, Stats},
    weather::{self as mac_weather, Forecast, Location},
};
use crate::mpris_subscription::{self, MprisRequest, MprisUpdate, PlayerStatus};
use crate::subscriptions::{
    self,
    notifications::{self, NotificationsAppletProxy},
};
use jiff::{
    Timestamp, ToSpan, Zoned,
    civil::{Date, Weekday},
    fmt::strtime,
    tz::TimeZone,
};
use logind_zbus::manager::ManagerProxy;
use std::hash::Hash;
use std::sync::LazyLock;
use timedate_zbus::TimeDateProxy;
use tokio::{sync::watch, time};

use crate::{
    config::TimeAppletConfig,
    fl,
    time::{format_macos_clock, get_calendar_first},
};
use cosmic::applet::token::subscription::{
    TokenRequest, TokenUpdate, activation_token_subscription,
};
use icu::{
    datetime::{
        DateTimeFormatter, DateTimeFormatterPreferences, fieldsets,
        input::{Date as IcuDate, DateTime, Time},
        options::TimePrecision,
    },
    locale::{Locale, preferences::extensions::unicode::keywords::HourCycle},
};

/// One event from the Calendar app, as far as the popup needs it.
#[derive(Clone, Debug)]
pub(crate) struct AgendaEntry {
    pub(crate) date: Date,
    /// Start time as "HH:MM"; `None` for all-day events.
    pub(crate) time: Option<String>,
    pub(crate) title: String,
}

/// Events saved by the Calendar app (`mac-calendar`), if it is in use.
/// Each line is `date<TAB>start<TAB>end<TAB>title<TAB>…`.
fn load_agenda() -> Vec<AgendaEntry> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".local/share"))
        });
    let Some(contents) = base
        .map(|dir| dir.join("mac-calendar/events.tsv"))
        .and_then(|path| std::fs::read_to_string(path).ok())
    else {
        return Vec::new();
    };
    contents
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(6, '\t');
            let date: Date = fields.next()?.parse().ok()?;
            let start = fields.next()?;
            let _end = fields.next()?;
            let title = fields.next()?.to_owned();
            Some(AgendaEntry {
                date,
                time: (!start.is_empty()).then(|| start.to_owned()),
                title,
            })
        })
        .collect()
}

/// Two-column popup width so Calendar sits side-by-side with Weather/Widgets.
const POPUP_WIDTH: f32 = 760.0;
/// Left column width (Calendar + Up Next + Settings).
const LEFT_COL_WIDTH: f32 = 360.0;
/// Default fallback popup height: 60% of 1080p viewport (648 px).
const DEFAULT_POPUP_MAX_HEIGHT: f32 = 648.0;

/// Maximum popup height: 60% of the active display's logical viewport height,
/// clamped to `480.0..=1200.0`, falling back to `648.0`.
static POPUP_MAX_HEIGHT: LazyLock<f32> = LazyLock::new(detect_popup_max_height);

/// Parses `cosmic-randr list` output to compute 60% of the logical viewport height
/// from the first `(current)` mode height and its output `Scale:` percentage.
fn parse_randr_popup_max_height(output: &str) -> f32 {
    let mut current_scale = 100.0_f32;
    for line in output.lines() {
        let trimmed = line.trim();
        if !line.starts_with(char::is_whitespace) && !trimmed.is_empty() {
            current_scale = 100.0;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("Scale:") {
            if let Ok(scale) = rest.trim().trim_end_matches('%').parse::<f32>()
                && scale > 0.0
            {
                current_scale = scale;
            }
            continue;
        }
        if trimmed.contains("(current)")
            && let Some(mode) = trimmed.split_whitespace().next()
            && let Some((_, h_str)) = mode.split_once('x')
            && let Ok(height) = h_str.parse::<u32>()
            && height > 0
        {
            let logical_height = height as f32 / (current_scale / 100.0);
            return (logical_height * 0.60).clamp(480.0, 1200.0);
        }
    }
    DEFAULT_POPUP_MAX_HEIGHT
}

fn detect_popup_max_height() -> f32 {
    let Ok(output) = std::process::Command::new("cosmic-randr")
        .arg("list")
        .output()
    else {
        return DEFAULT_POPUP_MAX_HEIGHT;
    };
    if !output.status.success() {
        return DEFAULT_POPUP_MAX_HEIGHT;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    parse_randr_popup_max_height(&text)
}
/// Width of one calendar column, sized so seven columns plus padding fill
/// the left column.
const CELL_WIDTH: f32 = 46.0;

/// The stock time applet's config is reused so COSMIC Settings keeps working.
const TIME_CONFIG_ID: &str = "com.system76.CosmicAppletTime";

static AUTOSIZE_MAIN_ID: LazyLock<Id> = LazyLock::new(|| Id::new("autosize-main"));

// Specifiers for strftime that indicate seconds. Subsecond precision isn't supported by the applet
// so those specifiers aren't listed here. This list is non-exhaustive, and it's possible that %X
// and other specifiers have to be added depending on locales.
const STRFTIME_SECONDS: &[char] = &['S', 'T', '+', 's'];

fn get_system_locale() -> Locale {
    for var in ["LC_TIME", "LC_ALL", "LANG"] {
        if let Ok(locale_str) = std::env::var(var) {
            let cleaned_locale = locale_str
                .split('.')
                .next()
                .unwrap_or(&locale_str)
                .replace('_', "-");

            if let Ok(locale) = Locale::try_from_str(&cleaned_locale) {
                return locale;
            }

            // Try language-only fallback (e.g., "en" from "en-US")
            if let Some(lang) = cleaned_locale.split('-').next()
                && let Ok(locale) = Locale::try_from_str(lang)
            {
                return locale;
            }
        }
    }
    tracing::warn!("No valid locale found in environment, using fallback");
    Locale::try_from_str("en-US").expect("Failed to parse fallback locale 'en-US'")
}

pub struct Window {
    core: cosmic::app::Core,
    popup: Option<window::Id>,
    now: Zoned,
    timezone: Option<TimeZone>,
    date_today: Date,
    date_selected: Date,
    rectangle_tracker: Option<RectangleTracker<u32>>,
    rectangle: Rectangle,
    token_tx: Option<calloop::channel::Sender<TokenRequest>>,
    config: TimeAppletConfig,
    show_seconds_tx: watch::Sender<bool>,
    locale: Locale,
    /// Events from the Calendar app, reloaded each time the popup opens.
    agenda: Vec<AgendaEntry>,
    /// Active MPRIS player, if any.
    player_status: Option<PlayerStatus>,
    /// Notifications received since the applet started, oldest first.
    notifications: Vec<Notification>,
    notifications_config: NotificationsConfig,
    notifications_config_helper: Option<Config>,
    /// Connection to the notifications daemon handed over by the panel.
    /// `None` when the panel did not provide one; the list then stays empty.
    notifications_proxy: Option<NotificationsAppletProxy<'static>>,
    notifications_tx: Option<tokio::sync::mpsc::Sender<notifications::Input>>,
    dbus_sender: Option<tokio::sync::mpsc::Sender<subscriptions::dbus::Input>>,
    widget_settings: WidgetSettings,
    weather: WeatherState,
    stats: Stats,
    cpu_prev: Option<CpuSample>,
}

#[derive(Debug, Clone)]
pub enum Message {
    TogglePopup,
    CloseRequested(window::Id),
    Tick,
    Rectangle(RectangleUpdate<u32>),
    SelectDay(i8),
    /// Open the Calendar app.
    OpenCalendar,
    /// Open System Monitor.
    OpenSystemMonitor,
    /// Open Appearance settings.
    OpenAppearanceSettings,
    PreviousMonth,
    NextMonth,
    OpenDateTimeSettings,
    Token(TokenUpdate),
    ConfigChanged(TimeAppletConfig),
    TimezoneUpdate(String),
    #[allow(dead_code)]
    Surface(surface::Action<Message>),
    Mpris(MprisUpdate),
    MprisRequest(MprisRequest),
    NotificationEvent(notifications::Output),
    NotificationDbusEvent(subscriptions::dbus::Output),
    NotificationsConfig(NotificationsConfig),
    DoNotDisturb(bool),
    Dismissed(u32),
    ActivateNotification(u32),
    ClearAll,
    WeatherLoaded(Result<Forecast, String>),
    StatsTick,
}

impl Window {
    fn create_datetime(&self, date: &Date) -> DateTime<icu::calendar::Gregorian> {
        DateTime {
            date: IcuDate::try_new_gregorian(
                date.year() as i32,
                date.month() as u8,
                date.day() as u8,
            )
            .unwrap(),
            time: Time::try_new(
                self.now.hour() as u8,
                self.now.minute() as u8,
                self.now.second() as u8,
                0,
            )
            .unwrap(),
        }
    }

    fn calendar_grid(&self) -> Grid<'_, Message> {
        let mut calendar = grid().width(Length::Fill);
        let first_day_of_week = match self.config.first_day_of_week {
            0 => Weekday::Monday,
            1 => Weekday::Tuesday,
            2 => Weekday::Wednesday,
            3 => Weekday::Thursday,
            4 => Weekday::Friday,
            5 => Weekday::Saturday,
            _ => Weekday::Sunday,
        };

        let first_day = get_calendar_first(
            self.date_selected.year(),
            self.date_selected.month(),
            first_day_of_week,
        );

        let prefs = DateTimeFormatterPreferences::from(self.locale.clone());
        let weekday = DateTimeFormatter::try_new(prefs, fieldsets::E::short()).unwrap();

        for i in 0..7 {
            let date = first_day.checked_add(i.days()).unwrap();
            let datetime = self.create_datetime(&date);
            calendar = calendar.push(
                text::caption(weekday.format(&datetime).to_string())
                    .apply(container)
                    .center_x(Length::Fixed(CELL_WIDTH)),
            );
        }
        calendar = calendar.insert_row();

        for i in 0..42 {
            if i > 0 && i % 7 == 0 {
                calendar = calendar.insert_row();
            }

            let date = first_day
                .checked_add(i.days())
                .expect("valid date in calendar range");
            let is_month = date.first_of_month() == self.date_selected.first_of_month();
            let is_day = date == self.date_selected;
            let is_today = date == self.date_today;

            let has_events = self.agenda.iter().any(|event| event.date == date);
            calendar = calendar.push(
                date_button(date.day(), is_month, is_day, is_today, has_events)
                    .apply(container)
                    .center_x(Length::Fixed(CELL_WIDTH)),
            );
        }

        calendar
    }

    /// Format with strftime if non-empty and ignore errors.
    ///
    /// Do not use to_string(). The formatter panics on invalid specifiers.
    fn maybe_strftime(&self) -> Option<String> {
        // strftime may override locale specific elements so it stands alone rather
        // than using ICU.
        (!self.config.format_strftime.is_empty())
            .then(|| strtime::format(&self.config.format_strftime, &self.now).ok())
            .flatten()
    }

    fn vertical_layout(&self) -> Element<'_, Message> {
        let elements: Vec<Element<'_, Message>> = if let Some(strftime) = self.maybe_strftime() {
            strftime
                .split_whitespace()
                .map(|piece| self.core.applet.text(piece.to_owned()).into())
                .collect()
        } else {
            let mut elements = Vec::new();
            let date = self.now.date();
            let datetime = self.create_datetime(&date);
            let mut prefs = DateTimeFormatterPreferences::from(self.locale.clone());
            prefs.hour_cycle = Some(if self.config.military_time {
                HourCycle::H23
            } else {
                HourCycle::H12
            });

            if self.config.show_date_in_top_panel {
                let formatted_date = DateTimeFormatter::try_new(prefs, fieldsets::MD::medium())
                    .unwrap()
                    .format(&datetime)
                    .to_string();

                for p in formatted_date.split_whitespace() {
                    elements.push(self.core.applet.text(p.to_owned()).into());
                }
                elements.push(
                    rule::horizontal(2)
                        .width(self.core.applet.suggested_size(true).0)
                        .into(),
                );
            }
            let mut fs = fieldsets::T::medium();
            if !self.config.show_seconds {
                fs = fs.with_time_precision(TimePrecision::Minute);
            }
            let formatted_time = DateTimeFormatter::try_new(prefs, fs)
                .unwrap()
                .format(&datetime)
                .to_string();

            // todo: split using formatToParts when it is implemented
            // https://github.com/unicode-org/icu4x/issues/4936#issuecomment-2128812667
            for p in formatted_time.split_whitespace().flat_map(|s| s.split(':')) {
                elements.push(self.core.applet.text(p.to_owned()).into());
            }

            elements
        };

        let date_time_col = Column::with_children(elements)
            .align_x(Alignment::Center)
            .spacing(4);

        Element::from(
            column!(
                date_time_col,
                space::horizontal().width(Length::Fixed(
                    (self.core.applet.suggested_size(true).0
                        + 2 * self.core.applet.suggested_padding(true).1)
                        as f32
                ))
            )
            .align_x(Alignment::Center),
        )
    }

    /// Ask the notifications daemon to close a notification.
    fn close_notification(&self, id: u32) {
        if let Some(tx) = self.dbus_sender.clone() {
            tokio::spawn(async move {
                if let Err(err) = tx.send(subscriptions::dbus::Input::Dismiss(id)).await {
                    tracing::error!(?err, "Failed to dismiss notification");
                }
            });
        }
    }

    /// Album art, title, artist and transport controls for the active player.
    fn now_playing(&self, status: &PlayerStatus) -> Element<'_, Message> {
        let art: Element<'_, Message> = match status.icon.clone() {
            Some(path) => icon(icon::from_path(path)).size(48).into(),
            None => icon::from_name("audio-x-generic-symbolic")
                .size(32)
                .symbolic(true)
                .icon()
                .apply(container)
                .center(Length::Fixed(48.0))
                .into(),
        };

        let title = status
            .title
            .as_ref()
            .map_or_else(|| fl!("unknown-title"), |t| truncate(t, 26));
        let artists = status
            .artists
            .as_ref()
            .filter(|a| !a.is_empty())
            .map_or_else(|| fl!("unknown-artist"), |a| truncate(&a.join(", "), 30));

        let mut controls = row![].spacing(4).align_y(Alignment::Center);
        if status.can_go_previous {
            controls = controls.push(media_button(
                "media-skip-backward-symbolic",
                MprisRequest::Previous,
            ));
        }
        if status.status == PlaybackStatus::Playing {
            if status.can_pause {
                controls = controls.push(media_button(
                    "media-playback-pause-symbolic",
                    MprisRequest::Pause,
                ));
            }
        } else if status.can_play {
            controls = controls.push(media_button(
                "media-playback-start-symbolic",
                MprisRequest::Play,
            ));
        }
        if status.can_go_next {
            controls = controls.push(media_button(
                "media-skip-forward-symbolic",
                MprisRequest::Next,
            ));
        }

        let info = button::custom(
            row![
                art,
                column![text::body(title), text::caption(artists)].width(Length::Fill),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        )
        .class(cosmic::theme::Button::Text)
        .width(Length::Fill)
        .on_press(Message::MprisRequest(MprisRequest::Raise));

        row![info, controls]
            .spacing(8)
            .align_y(Alignment::Center)
            .padding([4, 16])
            .into()
    }

    /// Do Not Disturb toggle plus the list of received notifications.
    /// The selected day's events from the Calendar app; click to open it.
    fn agenda_section(&self) -> Option<Element<'_, Message>> {
        let day: Vec<&AgendaEntry> = self
            .agenda
            .iter()
            .filter(|event| event.date == self.date_selected)
            .collect();
        if day.is_empty() {
            return None;
        }
        let mut list = column![text::heading("Events")]
            .spacing(2)
            .padding([4, 20]);
        for event in day.iter().take(5) {
            let when = event.time.clone().unwrap_or_else(|| "All day".to_owned());
            list = list.push(
                button::custom(
                    row![
                        text::caption(when).width(Length::Fixed(64.0)),
                        text::body(event.title.clone()).width(Length::Fill),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                )
                .class(cosmic::theme::Button::Text)
                .width(Length::Fill)
                .on_press(Message::OpenCalendar),
            );
        }
        if day.len() > 5 {
            list = list.push(text::caption(format!("+{} more", day.len() - 5)));
        }
        Some(list.into())
    }

    /// Prepares the widget section when the popup opens: reloads settings,
    /// takes a first stats sample and fetches weather off the UI thread when
    /// the cache is stale.
    fn open_widgets(&mut self) -> app::Task<Message> {
        self.widget_settings = WidgetSettings::load();
        self.cpu_prev = None;
        self.stats = Stats::read(&mut self.cpu_prev);
        if !self.widget_settings.show_weather {
            return Task::none();
        }
        let Some(location) = Location::load() else {
            self.weather = WeatherState::NoLocation;
            return Task::none();
        };
        if let Some(forecast) = mac_weather::cached(&location) {
            self.weather = WeatherState::Ready(forecast);
            return Task::none();
        }
        if !matches!(self.weather, WeatherState::Ready(_)) {
            self.weather = WeatherState::Loading;
        }
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || mac_weather::load_or_fetch(&location))
                    .await
                    .map_err(|e| e.to_string())?
            },
            |result| cosmic::Action::App(Message::WeatherLoaded(result)),
        )
    }

    fn notification_center(&self) -> Element<'_, Message> {
        let mut header = row![text::heading(fl!("notifications")).width(Length::Fill)]
            .align_y(Alignment::Center)
            .padding([0, 20]);
        if !self.notifications.is_empty() {
            header = header.push(button::text(fl!("clear-all")).on_press(Message::ClearAll));
        }

        let do_not_disturb = padded_control(
            toggler(self.notifications_config.do_not_disturb)
                .on_toggle(Message::DoNotDisturb)
                .text_size(14)
                .width(Length::Fill)
                .label(fl!("do-not-disturb")),
        );

        let list: Element<'_, Message> = if self.notifications.is_empty() {
            text::body(fl!("no-notifications"))
                .apply(container)
                .center_x(Length::Fill)
                .padding([12, 0])
                .into()
        } else {
            let items = self.notifications.iter().rev().map(notification_item);
            scrollable(Column::with_children(items).spacing(4).padding([0, 12]))
                .apply(container)
                .max_height(260.0)
                .into()
        };

        column![header, do_not_disturb, list].spacing(4).into()
    }

    fn horizontal_layout(&self) -> Element<'_, Message> {
        let formatted_date = format_macos_clock(&self.now, &self.config, &self.locale);
        text(formatted_date).size(panel_text_size()).into()
    }
}

impl cosmic::Application for Window {
    type Message = Message;
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();
    const APP_ID: &str = "dev.asw.CosmicExtAppletClockCenter";

    fn init(core: app::Core, _flags: Self::Flags) -> (Self, app::Task<Self::Message>) {
        let locale = get_system_locale();
        let now = Zoned::now();
        // get today's date for highlighting purposes
        let today = now.date();

        // Synch `show_seconds` from the config within the time subscription
        let (show_seconds_tx, _) = watch::channel(true);

        let notifications_config_helper = Config::new(
            cosmic_notifications_config::ID,
            NotificationsConfig::VERSION,
        )
        .ok();
        let notifications_config = notifications_config_helper
            .as_ref()
            .map(|helper| {
                NotificationsConfig::get_entry(helper).unwrap_or_else(|(errors, config)| {
                    for err in errors {
                        tracing::error!(?err, "Error loading notifications config");
                    }
                    config
                })
            })
            .unwrap_or_default();
        let notifications_proxy =
            match cosmic::iced::futures::executor::block_on(notifications::get_proxy()) {
                Ok(proxy) => Some(proxy),
                Err(err) => {
                    tracing::warn!(?err, "No notifications daemon connection; list disabled");
                    None
                }
            };

        (
            Self {
                core,
                popup: None,
                now,
                timezone: None,
                date_today: today,
                date_selected: today,
                rectangle_tracker: None,
                rectangle: Rectangle::default(),
                token_tx: None,
                config: TimeAppletConfig::default(),
                show_seconds_tx,
                locale,
                agenda: Vec::new(),
                player_status: None,
                notifications: Vec::new(),
                notifications_config,
                notifications_config_helper,
                notifications_proxy,
                notifications_tx: None,
                dbus_sender: None,
                widget_settings: WidgetSettings::default(),
                weather: WeatherState::default(),
                stats: Stats::default(),
                cpu_prev: None,
            },
            Task::none(),
        )
    }

    fn core(&self) -> &cosmic::app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::app::Core {
        &mut self.core
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }

    fn subscription(&self) -> Subscription<Message> {
        fn time_subscription(show_seconds: watch::Receiver<bool>) -> Subscription<Message> {
            struct Wrapper {
                inner: watch::Receiver<bool>,
                id: &'static str,
            }
            impl Hash for Wrapper {
                fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                    self.id.hash(state);
                }
            }
            Subscription::run_with(
                Wrapper {
                    inner: show_seconds,
                    id: "time-sub",
                },
                |Wrapper { inner, id: _ }| {
                    let mut show_seconds = inner.clone();
                    stream::channel(1, move |mut output: mpsc::Sender<Message>| async move {
                        // Mark this receiver's state as changed so that it always receives an initial
                        // update during the loop below
                        // This allows us to avoid duplicating code from the loop
                        show_seconds.mark_changed();
                        let mut period = 1;
                        let mut timer = time::interval(time::Duration::from_secs(period));
                        timer.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

                        loop {
                            tokio::select! {
                                    _ = timer.tick() => {
                                        #[cfg(debug_assertions)]
                                        if let Err(err) = output.send(Message::Tick).await {
                                            tracing::error!(?err, "Failed sending tick request to applet");
                                        }
                                        #[cfg(not(debug_assertions))]
                                        let _ = output.send(Message::Tick).await;

                                        // Calculate a delta if we're ticking per minute to keep ticks stable
                                        // Based on i3status-rust
                                        let current = Timestamp::now().as_second() as u64 % period;
                                        if current != 0 {
                                            timer.reset_after(time::Duration::from_secs(period - current));
                                        }
                                    },
                                // Update timer if the user toggles show_seconds
                                Ok(()) = show_seconds.changed() => {
                                    let seconds = *show_seconds.borrow_and_update();
                                    if seconds {
                                        period = 1;
                                        // Subsecond precision isn't needed; skip calculating offset
                                        let period = time::Duration::from_secs(period);
                                        let start = time::Instant::now() + period;
                                        timer = time::interval_at(start, period);
                                    } else {
                                        period = 60;
                                        let delta = time::Duration::from_secs(period - Timestamp::now().as_second() as u64 % period);
                                        let now = time::Instant::now();
                                        // Start ticking from the next minute to update the time properly
                                        let start = now + delta;
                                        let period = time::Duration::from_secs(period);
                                        timer = time::interval_at(start, period);

                                        timer.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
                                    }
                                }
                            }
                        }
                    })
                },
            )
        }

        // Update applet's timezone if the system's timezone changes
        async fn timezone_update(output: &mut mpsc::Sender<Message>) -> zbus::Result<()> {
            let conn = zbus::Connection::system().await?;
            let proxy = TimeDateProxy::new(&conn).await?;

            // The stream always returns the current timezone as its first item even if it wasn't
            // updated. If the proxy is recreated in a loop somehow, the resulting stream will
            // always yield an update immediately which could lead to spammed false updates.
            let mut stream_tz = proxy.receive_timezone_changed().await;
            while let Some(property) = stream_tz.next().await {
                let tz = property.get().await?;
                output
                    .send(Message::TimezoneUpdate(tz))
                    .await
                    .map_err(|e| {
                        zbus::Error::InputOutput(std::sync::Arc::new(std::io::Error::other(e)))
                    })?;
            }
            Ok(())
        }

        fn timezone_subscription() -> Subscription<Message> {
            Subscription::run_with("timezone-sub", |_| {
                stream::channel(1, |mut output| async move {
                    'retry: loop {
                        match timezone_update(&mut output).await {
                            Ok(()) => break 'retry,
                            Err(err) => {
                                tracing::error!(
                                    ?err,
                                    "Automatic timezone updater failed; retrying in one minute"
                                );
                                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                            }
                        }
                    }

                    std::future::pending().await
                })
            })
        }

        // Update the time when waking from sleep
        async fn wake_from_sleep(output: &mut mpsc::Sender<Message>) -> zbus::Result<()> {
            let connection = zbus::Connection::system().await?;
            let proxy = ManagerProxy::new(&connection).await?;

            while let Some(property) = proxy.receive_prepare_for_sleep().await?.next().await {
                let waking = !property.args()?.start();
                if waking {
                    let _ = output.send(Message::Tick).await;
                }
            }
            Ok(())
        }

        fn wake_from_sleep_subscription() -> Subscription<Message> {
            Subscription::run_with("wake-from-suspend-sub", |_| {
                stream::channel(1, |mut output| async move {
                    if let Err(err) = wake_from_sleep(&mut output).await {
                        tracing::error!(?err, "Failed to subscribe to wake-from-sleep signal");
                    }
                })
            })
        }

        let show_seconds_rx = self.show_seconds_tx.subscribe();
        let notifications_sub = match self.notifications_proxy.clone() {
            Some(proxy) => Subscription::batch([
                notifications::notifications(proxy).map(Message::NotificationEvent),
                subscriptions::dbus::proxy().map(Message::NotificationDbusEvent),
            ]),
            None => Subscription::none(),
        };
        let stats_sub = if self.popup.is_some() && self.widget_settings.show_system {
            cosmic::iced::time::every(std::time::Duration::from_secs(2)).map(|_| Message::StatsTick)
        } else {
            Subscription::none()
        };
        Subscription::batch([
            stats_sub,
            notifications_sub,
            mpris_subscription::mpris_subscription(0).map(Message::Mpris),
            self.core
                .watch_config(cosmic_notifications_config::ID)
                .map(|u| Message::NotificationsConfig(u.config)),
            rectangle_tracker_subscription(0).map(|e| Message::Rectangle(e.1)),
            time_subscription(show_seconds_rx),
            activation_token_subscription(0).map(Message::Token),
            timezone_subscription(),
            wake_from_sleep_subscription(),
            self.core.watch_config(TIME_CONFIG_ID).map(|u| {
                for err in u.errors {
                    tracing::error!(?err, "Error watching config");
                }
                Message::ConfigChanged(u.config)
            }),
        ])
    }

    fn update(&mut self, message: Self::Message) -> app::Task<Self::Message> {
        match message {
            Message::TogglePopup => {
                if let Some(p) = self.popup.take() {
                    destroy_popup(p)
                } else {
                    let widgets_task = self.open_widgets();
                    let popup_task = cosmic::surface::surface_task(cosmic::surface::action::app_popup(
                        |_| Default::default(),
                        |app: &mut Self| {
                            app.date_today = app.now.date();
                            app.date_selected = app.date_today;
                            app.agenda = load_agenda();

                            let new_id = window::Id::unique();
                            app.popup = Some(new_id);

                            let mut popup_settings = app.core.applet.get_popup_settings(
                                app.core.main_window_id().unwrap(),
                                new_id,
                                None,
                                None,
                                None,
                            );
                            let Rectangle {
                                x,
                                y,
                                width,
                                height,
                            } = app.rectangle;
                            popup_settings.positioner.anchor_rect = Rectangle::<i32> {
                                x: x.max(1.) as i32,
                                y: y.max(1.) as i32,
                                width: width.max(1.) as i32,
                                height: height.max(1.) as i32,
                            };

                            popup_settings.positioner.size = None;
                            popup_settings.positioner.size_limits = cosmic::iced::Limits::NONE
                                .min_height(1.0)
                                .min_width(POPUP_WIDTH)
                                .max_width(POPUP_WIDTH)
                                .max_height(*POPUP_MAX_HEIGHT);
                            popup_settings
                        },
                        None,
                    ));
                    Task::batch([popup_task, widgets_task])
                }
            }
            Message::WeatherLoaded(result) => {
                self.weather = match result {
                    Ok(forecast) => WeatherState::Ready(forecast),
                    Err(err) => {
                        tracing::warn!(err, "Weather fetch failed");
                        WeatherState::Failed
                    }
                };
                Task::none()
            }
            Message::StatsTick => {
                if self.popup.is_some() {
                    self.stats = Stats::read(&mut self.cpu_prev);
                }
                Task::none()
            }
            Message::Tick => {
                self.now = self.timezone.as_ref().map_or_else(
                    Zoned::now,
                    |tz| Zoned::now().with_time_zone(tz.clone()),
                );
                Task::none()
            }
            Message::Rectangle(u) => {
                match u {
                    RectangleUpdate::Rectangle(r) => {
                        self.rectangle = r.1;
                    }
                    RectangleUpdate::Init(tracker) => {
                        self.rectangle_tracker = Some(tracker);
                    }
                }
                Task::none()
            }
            Message::CloseRequested(id) => {
                if Some(id) == self.popup {
                    self.popup = None;
                }
                Task::none()
            }
            Message::SelectDay(day) => {
                if let Ok(date) = self.date_selected.with().day(day).build() {
                    self.date_selected = date;
                } else {
                    tracing::error!("invalid date");
                }
                Task::none()
            }
            Message::PreviousMonth => {
                if let Ok(date) = self.date_selected.checked_sub(1.month()) {
                    self.date_selected = date;
                } else {
                    tracing::error!("invalid date");
                }
                Task::none()
            }
            Message::NextMonth => {
                if let Ok(date) = self.date_selected.checked_add(1.month()) {
                    self.date_selected = date;
                } else {
                    tracing::error!("invalid date");
                }
                Task::none()
            }
            Message::OpenCalendar => {
                self.open_app_and_close_popup("mac-calendar", "cosmic-settings", &["time"])
            }
            Message::OpenSystemMonitor => {
                self.open_app_and_close_popup("cosmic-monitor", "cosmic-settings", &[])
            }
            Message::OpenAppearanceSettings => {
                self.open_app_and_close_popup("mac-appearance", "cosmic-settings", &[])
            }
            Message::OpenDateTimeSettings => {
                let exec = "cosmic-settings time".to_string();
                if let Some(tx) = self.token_tx.as_ref() {
                    let _ = tx.send(TokenRequest {
                        app_id: Self::APP_ID.to_string(),
                        exec,
                    });
                } else {
                    tracing::error!("Wayland tx is None");
                }
                Task::none()
            }
            Message::Token(u) => {
                match u {
                    TokenUpdate::Init(tx) => {
                        self.token_tx = Some(tx);
                    }
                    TokenUpdate::Finished => {
                        self.token_tx = None;
                    }
                    TokenUpdate::ActivationToken { token, .. } => {
                        let mut cmd = std::process::Command::new("cosmic-settings");
                        cmd.arg("time");
                        if let Some(token) = token {
                            cmd.env("XDG_ACTIVATION_TOKEN", &token);
                            cmd.env("DESKTOP_STARTUP_ID", &token);
                        }
                        tokio::spawn(cosmic::process::spawn(cmd));
                    }
                }
                Task::none()
            }
            Message::ConfigChanged(c) => {
                // Don't interrupt the tick subscription unless necessary
                self.show_seconds_tx.send_if_modified(|show_seconds| {
                    if !c.format_strftime.is_empty() {
                        if c.format_strftime.split('%').any(|s| {
                            STRFTIME_SECONDS.contains(&s.chars().next().unwrap_or_default())
                        }) && !*show_seconds
                        {
                            // The strftime formatter contains a seconds specifier. Force enable
                            // ticking per seconds internally regardless of the user setting.
                            // This does not change the user's setting. It's invisible to the user.
                            *show_seconds = true;
                            true
                        } else {
                            false
                        }
                    } else if *show_seconds == c.show_seconds {
                        false
                    } else {
                        *show_seconds = c.show_seconds;
                        true
                    }
                });
                self.config = c;
                Task::none()
            }
            Message::TimezoneUpdate(timezone) => {
                if let Ok(timezone) = TimeZone::get(&timezone) {
                    self.now = Zoned::now().with_time_zone(timezone.clone());
                    self.date_today = self.now.date();
                    self.date_selected = self.date_today;
                    self.timezone = Some(timezone);
                }

                self.update(Message::Tick)
            }
            Message::Surface(a) => {
                cosmic::task::message(cosmic::Action::Surface(a))
            }
            Message::Mpris(MprisUpdate::Player(p)) => {
                self.player_status = Some(p);
                Task::none()
            }
            Message::Mpris(MprisUpdate::Setup | MprisUpdate::Finished) => {
                self.player_status = None;
                Task::none()
            }
            Message::MprisRequest(request) => {
                let Some(status) = self.player_status.as_ref() else {
                    return Task::none();
                };
                let player = status.player.clone();
                tokio::spawn(async move {
                    // The calls return different error types, so compare as text.
                    let res = match request {
                        MprisRequest::Play => player.play().await.map_err(|e| e.to_string()),
                        MprisRequest::Pause => player.pause().await.map_err(|e| e.to_string()),
                        MprisRequest::Next => player.next().await.map_err(|e| e.to_string()),
                        MprisRequest::Previous => {
                            player.previous().await.map_err(|e| e.to_string())
                        }
                        MprisRequest::Raise => match player.media_player().await {
                            Ok(media_player) => {
                                media_player.raise().await.map_err(|e| e.to_string())
                            }
                            Err(err) => Err(err.to_string()),
                        },
                    };
                    if let Err(err) = res {
                        tracing::error!(?err, "MPRIS request failed");
                    }
                });
                Task::none()
            }
            Message::NotificationEvent(notifications::Output::Ready(tx)) => {
                self.notifications_tx = Some(tx);
                Task::none()
            }
            Message::NotificationEvent(notifications::Output::Notification(n)) => {
                if let Some(existing) = self.notifications.iter_mut().find(|e| e.id == n.id) {
                    *existing = n;
                } else {
                    self.notifications.push(n);
                }
                Task::none()
            }
            Message::NotificationDbusEvent(subscriptions::dbus::Output::Ready(tx)) => {
                self.dbus_sender = Some(tx);
                Task::none()
            }
            Message::NotificationDbusEvent(subscriptions::dbus::Output::CloseEvent(id)) => {
                self.notifications.retain(|n| n.id != id);
                Task::none()
            }
            Message::NotificationsConfig(config) => {
                self.notifications_config = config;
                Task::none()
            }
            Message::DoNotDisturb(enabled) => {
                self.notifications_config.do_not_disturb = enabled;
                if let Some(helper) = &self.notifications_config_helper
                    && let Err(err) = self.notifications_config.write_entry(helper)
                {
                    tracing::error!(?err, "Failed to save Do Not Disturb");
                }
                Task::none()
            }
            Message::Dismissed(id) => {
                self.notifications.retain(|n| n.id != id);
                self.close_notification(id);
                Task::none()
            }
            Message::ClearAll => {
                for n in std::mem::take(&mut self.notifications) {
                    self.close_notification(n.id);
                }
                Task::none()
            }
            Message::ActivateNotification(id) => {
                let action = self
                    .notifications
                    .iter()
                    .find(|n| n.id == id)
                    .and_then(|n| {
                        n.actions
                            .iter()
                            .find(|a| matches!(a.0, ActionId::Default))
                            .or_else(|| n.actions.first())
                            .map(|a| a.0.to_string())
                    });
                if let (Some(action), Some(tx)) = (action, self.notifications_tx.clone()) {
                    tokio::spawn(async move {
                        if let Err(err) = tx.send(notifications::Input::Activated(id, action)).await
                        {
                            tracing::error!(?err, "Failed to activate notification");
                        }
                    });
                }
                self.update(Message::Dismissed(id))
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let horizontal = matches!(
            self.core.applet.anchor,
            PanelAnchor::Top | PanelAnchor::Bottom
        );

        let btn = mac_pill_button(
            if horizontal {
                self.horizontal_layout()
            } else {
                self.vertical_layout()
            },
            self.popup.is_some(),
            if horizontal {
                PillVariant::Text
            } else {
                PillVariant::IconOnly
            },
        )
        .on_press_down(Message::TogglePopup);

        autosize::autosize(
            if let Some(tracker) = self.rectangle_tracker.as_ref() {
                Element::from(tracker.container(0, btn).ignore_bounds(true))
            } else {
                btn.into()
            },
            AUTOSIZE_MAIN_ID.clone(),
        )
        .into()
    }

    fn view_window(&self, _id: window::Id) -> Element<'_, Message> {
        let Spacing {
            space_xxs, space_s, ..
        } = theme::active().cosmic().spacing;

        let datetime = self.create_datetime(&self.date_selected);
        let prefs = DateTimeFormatterPreferences::from(self.locale.clone());

        let date = text(
            DateTimeFormatter::try_new(prefs, fieldsets::YMD::long())
                .unwrap()
                .format(&datetime)
                .to_string(),
        )
        .size(18);
        let day_of_week = text::body(
            DateTimeFormatter::try_new(prefs, fieldsets::E::long())
                .unwrap()
                .format(&datetime)
                .to_string(),
        );

        let month_controls = row![
            button::icon(icon::from_name("go-previous-symbolic"))
                .padding(8)
                .on_press(Message::PreviousMonth),
            button::icon(icon::from_name("go-next-symbolic"))
                .padding(8)
                .on_press(Message::NextMonth)
        ]
        .spacing(8);

        let calendar = self.calendar_grid();

        let divider =
            || padded_control(divider::horizontal::default()).padding([space_xxs, space_s]);

        let now_hm = format!("{:02}:{:02}", self.now.hour(), self.now.minute());

        let mut left_col = column![
            row![
                column![date, day_of_week],
                space::horizontal().width(Length::Fill),
                month_controls,
            ]
            .align_y(Alignment::Center)
            .padding([12, 20]),
            calendar.padding([0, 12].into()),
        ]
        .padding([8, 0])
        .width(Length::Fixed(LEFT_COL_WIDTH));

        if let Some(agenda) = self.agenda_section() {
            left_col = left_col.push(divider()).push(agenda);
        }

        if self.widget_settings.show_up_next {
            let up_next = mouse_area(widgets::up_next_card(
                widgets::upcoming(&self.agenda, self.now.date(), &now_hm),
                self.now.date(),
            ))
            .on_press(Message::OpenCalendar);
            left_col = left_col
                .push(divider())
                .push(container(up_next).padding([4, 16]));
        }

        left_col = left_col.push(divider()).push(
            menu_button(text::body(fl!("datetime-settings")))
                .on_press(Message::OpenDateTimeSettings),
        );

        let mut right_col = column![].spacing(8).padding([12, 16]).width(Length::Fill);
        let mut has_top_widgets = false;

        if self.widget_settings.show_weather {
            right_col = right_col.push(
                mouse_area(widgets::weather_card(
                    &self.weather,
                    self.widget_settings.units,
                ))
                .on_press(Message::OpenAppearanceSettings),
            );
            has_top_widgets = true;
        }

        if self.widget_settings.show_system {
            right_col = right_col.push(
                mouse_area(widgets::system_card(&self.stats))
                    .on_press(Message::OpenSystemMonitor),
            );
            has_top_widgets = true;
        }

        if !control_center_installed()
            && let Some(status) = self.player_status.as_ref()
        {
            if has_top_widgets {
                right_col = right_col.push(divider::horizontal::default());
            }
            right_col = right_col.push(self.now_playing(status));
            has_top_widgets = true;
        }

        if has_top_widgets {
            right_col = right_col.push(divider::horizontal::default());
        }
        right_col = right_col.push(self.notification_center());

        let body = row![
            scrollable(left_col).width(Length::Fixed(LEFT_COL_WIDTH)),
            divider::vertical::default(),
            scrollable(right_col).width(Length::Fill),
        ]
        .height(Length::Shrink)
        .width(Length::Fixed(POPUP_WIDTH));

        mac_popover_container_sized(
            &self.core.applet,
            container(body).max_height(*POPUP_MAX_HEIGHT),
            POPUP_WIDTH,
            POPUP_WIDTH,
        )
        .into()
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Message> {
        Some(Message::CloseRequested(id))
    }
}

/// Hide Now Playing inside the Clock popover when Control Center is installed
/// so media controls are not shown twice.
fn control_center_installed() -> bool {
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share"))
        });
    data_home
        .map(|d| d.join("applications/dev.asw.CosmicExtAppletControlCenter.desktop").is_file())
        .unwrap_or(false)
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() > max_chars {
        let mut out: String = value.chars().take(max_chars.saturating_sub(1)).collect();
        out.push('…');
        out
    } else {
        value.to_owned()
    }
}

fn media_button(icon_name: &'static str, request: MprisRequest) -> Element<'static, Message> {
    button::icon(icon::from_name(icon_name).symbolic(true))
        .extra_small()
        .on_press(Message::MprisRequest(request))
        .into()
}

fn duration_ago(notification: &Notification) -> String {
    notification.duration_since().map_or_else(String::new, |d| {
        let minutes = d.as_secs() / 60;
        let hours = minutes / 60;
        if hours > 0 {
            fl!("hours-ago", HashMap::from([("duration", hours)]))
        } else {
            fl!("minutes-ago", HashMap::from([("duration", minutes)]))
        }
    })
}

/// One notification: clicking the body activates it, the close button dismisses it.
fn notification_item(n: &Notification) -> Element<'_, Message> {
    let mut title = row![].spacing(8).align_y(Alignment::Center);
    if let Some(app_icon) = n.notification_icon() {
        title = title.push(app_icon.size(16));
    }
    title = title
        .push(text::caption(n.app_name.as_str()).width(Length::Fill))
        .push(text::caption(duration_ago(n)));

    let mut body = column![
        title,
        text::body(n.summary.lines().next().unwrap_or_default()).width(Length::Fill),
    ]
    .spacing(2)
    .width(Length::Fill);
    if !n.body.is_empty() {
        body = body.push(
            rich_text(markup::html_to_spans(&n.body))
                .size(12.0)
                .width(Length::Fill),
        );
    }

    let close = button::icon(icon::from_name("window-close-symbolic").symbolic(true))
        .extra_small()
        .on_press(Message::Dismissed(n.id));

    row![
        button::custom(body)
            .class(cosmic::theme::Button::Text)
            .width(Length::Fill)
            .on_press(Message::ActivateNotification(n.id)),
        close,
    ]
    .spacing(4)
    .align_y(Alignment::Center)
    .into()
}

fn date_button(
    day: i8,
    is_month: bool,
    is_day: bool,
    is_today: bool,
    has_events: bool,
) -> Button<'static, Message> {
    let style = button::ButtonClass::Custom {
        active: Box::new(move |_focused, theme| {
            let cosmic = theme.cosmic();
            let accent = cosmic::iced::Color::from(cosmic.accent_color());
            let fg = cosmic::iced::Color::from(cosmic.on_bg_color());
            let off_white = cosmic::iced::Color::from_rgb(0.98, 0.99, 1.0);
            let mut s = button::Style::new();
            s.border_radius = 22.0.into();
            if is_today {
                s.background = Some(cosmic::iced::Background::Color(accent));
                s.text_color = Some(off_white);
                s.icon_color = Some(off_white);
            } else if is_day {
                s.background = Some(cosmic::iced::Background::Color(cosmic::iced::Color {
                    a: 0.78,
                    ..accent
                }));
                s.text_color = Some(off_white);
                s.icon_color = Some(off_white);
            } else if is_month {
                let text = cosmic::iced::Color { a: 0.90, ..fg };
                s.text_color = Some(text);
                s.icon_color = Some(text);
            } else {
                let muted = cosmic::iced::Color { a: 0.35, ..fg };
                s.text_color = Some(muted);
                s.icon_color = Some(muted);
            }
            s
        }),
        hovered: Box::new(move |_focused, theme| {
            let cosmic = theme.cosmic();
            let accent = cosmic::iced::Color::from(cosmic.accent_color());
            let fg = cosmic::iced::Color::from(cosmic.on_bg_color());
            let off_white = cosmic::iced::Color::from_rgb(0.98, 0.99, 1.0);
            let mut s = button::Style::new();
            s.border_radius = 22.0.into();
            if is_today {
                s.background = Some(cosmic::iced::Background::Color(cosmic::iced::Color {
                    a: 0.90,
                    ..accent
                }));
                s.text_color = Some(off_white);
                s.icon_color = Some(off_white);
            } else if is_day {
                s.background = Some(cosmic::iced::Background::Color(cosmic::iced::Color {
                    a: 0.88,
                    ..accent
                }));
                s.text_color = Some(off_white);
                s.icon_color = Some(off_white);
            } else if is_month {
                s.background = Some(cosmic::iced::Background::Color(cosmic::iced::Color {
                    a: 0.10,
                    ..fg
                }));
                s.text_color = Some(fg);
                s.icon_color = Some(fg);
            } else {
                let muted = cosmic::iced::Color { a: 0.35, ..fg };
                s.text_color = Some(muted);
                s.icon_color = Some(muted);
            }
            s
        }),
        pressed: Box::new(move |_focused, theme| {
            let cosmic = theme.cosmic();
            let accent = cosmic::iced::Color::from(cosmic.accent_color());
            let fg = cosmic::iced::Color::from(cosmic.on_bg_color());
            let off_white = cosmic::iced::Color::from_rgb(0.98, 0.99, 1.0);
            let mut s = button::Style::new();
            s.border_radius = 22.0.into();
            if is_today || is_day {
                s.background = Some(cosmic::iced::Background::Color(accent));
                s.text_color = Some(off_white);
                s.icon_color = Some(off_white);
            } else if is_month {
                s.background = Some(cosmic::iced::Background::Color(cosmic::iced::Color {
                    a: 0.18,
                    ..fg
                }));
                s.text_color = Some(fg);
                s.icon_color = Some(fg);
            } else {
                let muted = cosmic::iced::Color { a: 0.35, ..fg };
                s.text_color = Some(muted);
                s.icon_color = Some(muted);
            }
            s
        }),
        disabled: Box::new(move |theme| {
            let cosmic = theme.cosmic();
            let fg = cosmic::iced::Color::from(cosmic.on_bg_color());
            let muted = cosmic::iced::Color { a: 0.35, ..fg };
            let mut s = button::Style::new();
            s.border_radius = 22.0.into();
            s.text_color = Some(muted);
            s.icon_color = Some(muted);
            s
        }),
    };

    // A small dot under the number marks days with events.
    let label: Element<'static, Message> = if has_events {
        column![
            text::body(format!("{day}")),
            container(space::horizontal().width(Length::Fixed(4.0)))
                .height(Length::Fixed(4.0))
                .class(theme::Container::custom(move |theme| {
                    let dot_color = if is_today || is_day {
                        cosmic::iced::Color::from_rgba(0.98, 0.99, 1.0, 0.9)
                    } else {
                        theme.cosmic().accent_color().into()
                    };
                    cosmic::iced::widget::container::Style {
                        background: Some(cosmic::iced::Background::Color(dot_color)),
                        border: cosmic::iced::Border {
                            radius: 2.0.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }
                })),
        ]
        .align_x(Alignment::Center)
        .spacing(1)
        .into()
    } else {
        text::body(format!("{day}")).into()
    };
    let button = button::custom(label.apply(container).center(Length::Fill))
        .class(style)
        .height(Length::Fixed(44.0))
        .width(Length::Fixed(44.0));

    if is_month {
        button.on_press(Message::SelectDay(day))
    } else {
        button
    }
}

fn resolve_command(bin: &str) -> Option<std::path::PathBuf> {
    if let Some(home) = std::env::var_os("HOME") {
        let local_bin = std::path::PathBuf::from(home).join(".local/bin").join(bin);
        if local_bin.is_file() {
            return Some(local_bin);
        }
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(bin);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

impl Window {
    fn open_app_and_close_popup(
        &mut self,
        primary: &str,
        fallback: &str,
        fallback_args: &[&str],
    ) -> app::Task<Message> {
        let cmd = if let Some(bin) = resolve_command(primary) {
            std::process::Command::new(bin)
        } else {
            let mut fallback_cmd = std::process::Command::new(fallback);
            fallback_cmd.args(fallback_args);
            fallback_cmd
        };
        tokio::spawn(cosmic::process::spawn(cmd));

        if let Some(p) = self.popup.take() {
            destroy_popup(p)
        } else {
            Task::none()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_randr_popup_max_height_handles_scales_clamps_and_fallback() {
        let sample_1080p = "\
HDMI-A-1 (enabled)
  Scale: 100%
  Modes:
    1920x1080 @  60.000 Hz (current) (preferred)
    1280x720 @  60.000 Hz
";
        assert!((parse_randr_popup_max_height(sample_1080p) - 648.0).abs() < 0.01);

        let sample_hidpi = "\
eDP-1 (enabled)
  Scale: 200%
  Modes:
    3840x2160 @  60.000 Hz (current) (preferred)
";
        assert!((parse_randr_popup_max_height(sample_hidpi) - 648.0).abs() < 0.01);

        let sample_1440p = "\
DP-1 (enabled)
  Scale: 100%
  Modes:
    2560x1440 @ 144.000 Hz (current)
";
        assert!((parse_randr_popup_max_height(sample_1440p) - 864.0).abs() < 0.01);

        let sample_small_clamped = "\
VGA-1 (enabled)
  Scale: 100%
  Modes:
    1280x720 @ 60.000 Hz (current)
";
        assert!((parse_randr_popup_max_height(sample_small_clamped) - 480.0).abs() < 0.01);

        let sample_large_clamped = "\
DP-2 (enabled)
  Scale: 100%
  Modes:
    3840x2160 @ 60.000 Hz (current)
";
        assert!((parse_randr_popup_max_height(sample_large_clamped) - 1200.0).abs() < 0.01);

        assert!((parse_randr_popup_max_height("") - DEFAULT_POPUP_MAX_HEIGHT).abs() < 0.01);
        assert!(
            (parse_randr_popup_max_height("HDMI-A-1 (disabled)\n  Scale: 150%\n")
                - DEFAULT_POPUP_MAX_HEIGHT)
                .abs()
                < 0.01
        );
    }
}
