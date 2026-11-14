// SPDX-License-Identifier: GPL-3.0-only

mod audio;
mod bluetooth;
mod mpris_subscription;
mod network;
mod tiling;

use audio::AudioState;
use bluetooth::BluetoothState;
use cosmic::{
    Apply, Element, Task,
    app::{self, Core},
    applet::token::subscription::{TokenRequest, TokenUpdate, activation_token_subscription},
    cctk::sctk::reexports::calloop::{self, channel::SyncSender},
    iced::{
        Alignment, Length, Rectangle, Subscription,
        futures::{SinkExt, channel::mpsc},
        platform_specific::shell::wayland::commands::popup::destroy_popup,
        stream,
        widget::{column, row},
        window,
    },
    theme,
    widget::{
        Id, autosize, button, container, divider, icon, rectangle_tracker::*, scrollable, slider,
        space, text,
    },
};
use cosmic_config::{Config, ConfigSet, CosmicConfigEntry, cosmic_config_derive::CosmicConfigEntry};
use cosmic_ext_mac_applet_style::{
    PillVariant, icons::control_center_icon, mac_pill_button, mac_popover_container_sized,
    panel_icon_size,
};
use cosmic_notifications_config::NotificationsConfig;
use cosmic_protocols::workspace::v2::client::zcosmic_workspace_handle_v2::TilingState;
use cosmic_settings_daemon_subscription as settings_daemon;
use mpris2_zbus::player::PlaybackStatus;
use mpris_subscription::{MprisRequest, MprisUpdate, PlayerStatus};
use network::{NetworkState, WifiNetwork};
use std::sync::LazyLock;
use std::time::Duration;
use tiling::TilingEvent;
use tokio::sync::mpsc::UnboundedSender;

const POPUP_WIDTH: f32 = 320.0;

static AUTOSIZE_MAIN_ID: LazyLock<Id> = LazyLock::new(|| Id::new("autosize-main"));

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt::init();
    let _ = tracing_log::LogTracer::init();
    cosmic::applet::run::<ControlCenterApplet>(())
}

/// Minimal slice of `com.system76.CosmicComp` v1 config used for autotile sync.
#[derive(Debug, Clone, Default, PartialEq, Eq, CosmicConfigEntry)]
#[version = 1]
struct CompAutotileConfig {
    autotile: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ExpandedSection {
    #[default]
    None,
    Wifi,
    Bluetooth,
    Sound,
}

struct ControlCenterApplet {
    core: Core,
    popup: Option<window::Id>,
    rectangle_tracker: Option<RectangleTracker<u32>>,
    rectangle: Rectangle,
    token_tx: Option<calloop::channel::Sender<TokenRequest>>,
    zbus_connection: Option<zbus::Connection>,

    expanded: ExpandedSection,

    // Wi-Fi
    network: NetworkState,

    // Bluetooth
    bluetooth: BluetoothState,

    // Focus (Do Not Disturb)
    notifications_config: NotificationsConfig,
    notifications_config_helper: Option<Config>,

    // Tiling
    comp_config: CompAutotileConfig,
    comp_config_helper: Option<Config>,
    autotiled: bool,
    tiling_tx: Option<SyncSender<TilingState>>,

    // Display brightness
    settings_daemon_sender: Option<UnboundedSender<settings_daemon::Request>>,
    max_screen_brightness: Option<i32>,
    screen_brightness: Option<i32>,
    dragging_brightness: bool,

    // Audio
    audio: AudioState,
    dragging_volume: bool,

    // Now Playing (MPRIS)
    player_status: Option<PlayerStatus>,
}

#[derive(Debug, Clone)]
enum Message {
    TogglePopup,
    CloseRequested(window::Id),
    Rectangle(RectangleUpdate<u32>),
    Token(TokenUpdate),
    ZbusConnection(Result<zbus::Connection, String>),

    // Network
    NetworkLoaded(Option<NetworkState>),
    ToggleWifi,
    ConnectWifi(WifiNetwork),

    // Bluetooth
    BluetoothLoaded(BluetoothState),
    ToggleBluetooth,
    ToggleBtDevice(String, bool),

    // Focus
    NotificationsConfig(NotificationsConfig),
    ToggleFocus,

    // Tiling
    CompConfig(Box<CompAutotileConfig>),
    Tiling(TilingEvent),
    ToggleTiling,

    // Brightness
    SettingsDaemon(settings_daemon::Event),
    SetBrightness(i32),
    SetBrightnessDebounced,
    ReleaseBrightness,

    // Audio
    AudioLoaded(AudioState),
    SetVolume(u32),
    ReleaseVolume,
    ToggleMute,
    SelectSink(String),

    // MPRIS
    Mpris(MprisUpdate),
    MprisRequest(MprisRequest),

    // Navigation & Settings
    ToggleSection(ExpandedSection),
    OpenSettings(&'static str),
    TickRefresh,
}

impl ControlCenterApplet {
    fn refresh_all(&self) -> Task<cosmic::Action<Message>> {
        Task::batch([
            cosmic::task::future(async {
                Message::NetworkLoaded(network::fetch_network_state().await)
            })
            .map(cosmic::Action::App),
            cosmic::task::future(async {
                Message::BluetoothLoaded(bluetooth::fetch_bluetooth_state().await)
            })
            .map(cosmic::Action::App),
            cosmic::task::future(async { Message::AudioLoaded(audio::fetch_audio_state()) })
                .map(cosmic::Action::App),
        ])
    }

    fn card_style(active: bool) -> theme::Container<'static> {
        theme::Container::custom(move |theme| {
            let cosmic = theme.cosmic();
            let bg = if active {
                cosmic::iced::Color::from(cosmic.accent_color())
            } else {
                cosmic::iced::Color::from(cosmic.background(false).component.base)
            };
            let on = if active {
                cosmic::iced::Color::from(cosmic.accent_text_color())
            } else {
                cosmic::iced::Color::from(cosmic.background(false).on)
            };
            container::Style {
                text_color: Some(on),
                icon_color: Some(on),
                background: Some(bg.into()),
                border: cosmic::iced::Border {
                    radius: cosmic.corner_radii.radius_s.into(),
                    width: 1.0,
                    color: cosmic::iced::Color::from(cosmic.background(false).divider),
                },
                ..Default::default()
            }
        })
    }

    fn circle_toggle_style(active: bool) -> theme::Container<'static> {
        theme::Container::custom(move |theme| {
            let cosmic = theme.cosmic();
            let bg = if active {
                cosmic::iced::Color::from(cosmic.accent_color())
            } else {
                let mut c = cosmic::iced::Color::from(cosmic.background(false).on);
                c.a = 0.14;
                c
            };
            let on = if active {
                cosmic::iced::Color::from(cosmic.accent_text_color())
            } else {
                cosmic::iced::Color::from(cosmic.background(false).on)
            };
            container::Style {
                text_color: Some(on),
                icon_color: Some(on),
                background: Some(bg.into()),
                border: cosmic::iced::Border {
                    radius: 14.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        })
    }

    fn view_wifi_row(&self) -> Element<'_, Message> {
        let active = self.network.wifi_enabled;
        let sub = wifi_subtitle(&self.network);

        let circle = button::custom(
            container(
                icon::from_name(if active {
                    "network-wireless-symbolic"
                } else {
                    "network-wireless-disabled-symbolic"
                })
                .size(14)
                .symbolic(true),
            )
            .width(Length::Fixed(28.0))
            .height(Length::Fixed(28.0))
            .center(Length::Fixed(28.0))
            .class(Self::circle_toggle_style(active)),
        )
        .padding(0)
        .class(cosmic::theme::Button::Text)
        .on_press(Message::ToggleWifi);

        let label_btn = button::custom(
            row![
                column![
                    text::body("Wi-Fi").size(13),
                    text::caption(sub).size(11),
                ]
                .spacing(1)
                .width(Length::Fill),
                icon::from_name("go-next-symbolic").size(14).symbolic(true),
            ]
            .spacing(4)
            .align_y(Alignment::Center),
        )
        .padding([2, 4])
        .class(cosmic::theme::Button::Text)
        .width(Length::Fill)
        .on_press(Message::ToggleSection(ExpandedSection::Wifi));

        row![circle, label_btn]
            .spacing(8)
            .align_y(Alignment::Center)
            .into()
    }

    fn view_bluetooth_row(&self) -> Element<'_, Message> {
        let active = self.bluetooth.powered;
        let sub = bluetooth_subtitle(&self.bluetooth);

        let circle = button::custom(
            container(
                icon::from_name(if active {
                    "bluetooth-active-symbolic"
                } else {
                    "bluetooth-disabled-symbolic"
                })
                .size(14)
                .symbolic(true),
            )
            .width(Length::Fixed(28.0))
            .height(Length::Fixed(28.0))
            .center(Length::Fixed(28.0))
            .class(Self::circle_toggle_style(active)),
        )
        .padding(0)
        .class(cosmic::theme::Button::Text)
        .on_press(Message::ToggleBluetooth);

        let label_btn = button::custom(
            row![
                column![
                    text::body("Bluetooth").size(13),
                    text::caption(sub).size(11),
                ]
                .spacing(1)
                .width(Length::Fill),
                icon::from_name("go-next-symbolic").size(14).symbolic(true),
            ]
            .spacing(4)
            .align_y(Alignment::Center),
        )
        .padding([2, 4])
        .class(cosmic::theme::Button::Text)
        .width(Length::Fill)
        .on_press(Message::ToggleSection(ExpandedSection::Bluetooth));

        row![circle, label_btn]
            .spacing(8)
            .align_y(Alignment::Center)
            .into()
    }

    fn view_small_tile(
        &self,
        icon_name: &'static str,
        title: &'static str,
        active: bool,
        msg: Message,
    ) -> Element<'_, Message> {
        let status = if active { "On" } else { "Off" };
        button::custom(
            container(
                row![
                    icon::from_name(icon_name).size(16).symbolic(true),
                    column![text::body(title).size(13), text::caption(status).size(11)].spacing(1),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .padding([8, 10])
            .width(Length::Fill)
            .class(Self::card_style(active)),
        )
        .padding(0)
        .class(cosmic::theme::Button::Text)
        .width(Length::Fill)
        .on_press(msg)
        .into()
    }

    fn view_brightness_card(&self) -> Option<Element<'_, Message>> {
        let max = self.max_screen_brightness?;
        let val = self.screen_brightness?;
        if max <= 0 {
            return None;
        }
        let pct = brightness_percent(val, max);
        Some(
            container(
                column![
                    row![
                        text::caption("Display").width(Length::Fill),
                        text::caption(format!("{pct:.0}%")),
                    ],
                    row![
                        icon::from_name("display-brightness-symbolic")
                            .size(16)
                            .symbolic(true),
                        slider(0..=max, val, Message::SetBrightness)
                            .on_release(Message::ReleaseBrightness)
                            .width(Length::Fill),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                ]
                .spacing(4),
            )
            .padding([8, 10])
            .width(Length::Fill)
            .class(Self::card_style(false))
            .into(),
        )
    }

    fn view_sound_card(&self) -> Element<'_, Message> {
        let vol = self.audio.active_volume.min(100);
        let mute = self.audio.active_mute;
        let icon_name = volume_icon_name(vol, mute);
        let dev_name = self.audio.active_sink_description().unwrap_or("Output");

        container(
            column![
                row![
                    text::caption("Sound").width(Length::Fill),
                    text::caption(truncate(dev_name, 22)),
                ],
                row![
                    button::icon(icon::from_name(icon_name).symbolic(true))
                        .extra_small()
                        .on_press(Message::ToggleMute),
                    slider(0..=100, vol, Message::SetVolume)
                        .on_release(Message::ReleaseVolume)
                        .width(Length::Fill),
                    button::icon(icon::from_name("go-next-symbolic").symbolic(true))
                        .extra_small()
                        .on_press(Message::ToggleSection(ExpandedSection::Sound)),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            ]
            .spacing(4),
        )
        .padding([8, 10])
        .width(Length::Fill)
        .class(Self::card_style(false))
        .into()
    }

    fn view_now_playing_card(&self) -> Option<Element<'_, Message>> {
        let status = self.player_status.as_ref()?;
        let art: Element<'_, Message> = match status.icon.clone() {
            Some(path) => icon(icon::from_path(path)).size(36).into(),
            None => icon::from_name("audio-x-generic-symbolic")
                .size(22)
                .symbolic(true)
                .icon()
                .apply(container)
                .center(Length::Fixed(36.0))
                .into(),
        };

        let title = status
            .title
            .as_deref()
            .map(|t| truncate(t, 20))
            .unwrap_or_else(|| "Playing".to_string());
        let artist = status
            .artists
            .as_ref()
            .filter(|a| !a.is_empty())
            .map(|a| truncate(&a.join(", "), 22))
            .unwrap_or_default();

        let mut controls = row![].spacing(2).align_y(Alignment::Center);
        if status.can_go_previous {
            controls = controls.push(
                button::icon(icon::from_name("media-skip-backward-symbolic").symbolic(true))
                    .extra_small()
                    .on_press(Message::MprisRequest(MprisRequest::Previous)),
            );
        }
        if status.status == PlaybackStatus::Playing {
            if status.can_pause {
                controls = controls.push(
                    button::icon(icon::from_name("media-playback-pause-symbolic").symbolic(true))
                        .extra_small()
                        .on_press(Message::MprisRequest(MprisRequest::Pause)),
                );
            }
        } else if status.can_play {
            controls = controls.push(
                button::icon(icon::from_name("media-playback-start-symbolic").symbolic(true))
                    .extra_small()
                    .on_press(Message::MprisRequest(MprisRequest::Play)),
            );
        }
        if status.can_go_next {
            controls = controls.push(
                button::icon(icon::from_name("media-skip-forward-symbolic").symbolic(true))
                    .extra_small()
                    .on_press(Message::MprisRequest(MprisRequest::Next)),
            );
        }

        let info = button::custom(
            row![
                art,
                column![text::body(title).size(13), text::caption(artist).size(11)]
                    .spacing(1)
                    .width(Length::Fill),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .padding(0)
        .class(cosmic::theme::Button::Text)
        .width(Length::Fill)
        .on_press(Message::MprisRequest(MprisRequest::Raise));

        Some(
            container(
                row![info, controls]
                    .spacing(6)
                    .align_y(Alignment::Center),
            )
            .padding([8, 10])
            .width(Length::Fill)
            .class(Self::card_style(false))
            .into(),
        )
    }

    fn view_expanded_section(&self) -> Option<Element<'_, Message>> {
        match self.expanded {
            ExpandedSection::None => None,
            ExpandedSection::Wifi => {
                let mut col = column![].spacing(4);
                if self.network.networks.is_empty() {
                    col = col.push(container(text::caption("No Wi-Fi networks found")).padding([4, 6]));
                } else {
                    for net in self.network.networks.iter().take(6) {
                        let mark = if net.active { "✓ " } else { "" };
                        let label = format!("{mark}{}", net.ssid);
                        col = col.push(
                            button::custom(
                                row![
                                    text::body(label).size(13).width(Length::Fill),
                                    text::caption(format!("{}%", net.strength)),
                                ]
                                .align_y(Alignment::Center),
                            )
                            .class(cosmic::theme::Button::Text)
                            .width(Length::Fill)
                            .on_press(Message::ConnectWifi(net.clone())),
                        );
                    }
                }
                col = col.push(divider::horizontal::default()).push(
                    button::text("Network Settings…")
                        .width(Length::Fill)
                        .on_press(Message::OpenSettings("wireless")),
                );
                Some(
                    container(scrollable(col).apply(container).max_height(180.0))
                        .padding([6, 8])
                        .width(Length::Fill)
                        .class(Self::card_style(false))
                        .into(),
                )
            }
            ExpandedSection::Bluetooth => {
                let mut col = column![].spacing(4);
                if self.bluetooth.devices.is_empty() {
                    col = col.push(container(text::caption("No paired Bluetooth devices")).padding([4, 6]));
                } else {
                    for dev in self.bluetooth.devices.iter().take(6) {
                        let state = if dev.connected {
                            "Connected"
                        } else {
                            "Not Connected"
                        };
                        col = col.push(
                            button::custom(
                                row![
                                    text::body(dev.name.clone()).size(13).width(Length::Fill),
                                    text::caption(state),
                                ]
                                .align_y(Alignment::Center),
                            )
                            .class(cosmic::theme::Button::Text)
                            .width(Length::Fill)
                            .on_press(Message::ToggleBtDevice(
                                dev.object_path.clone(),
                                !dev.connected,
                            )),
                        );
                    }
                }
                col = col.push(divider::horizontal::default()).push(
                    button::text("Bluetooth Settings…")
                        .width(Length::Fill)
                        .on_press(Message::OpenSettings("bluetooth")),
                );
                Some(
                    container(scrollable(col).apply(container).max_height(180.0))
                        .padding([6, 8])
                        .width(Length::Fill)
                        .class(Self::card_style(false))
                        .into(),
                )
            }
            ExpandedSection::Sound => {
                let mut col = column![].spacing(4);
                if self.audio.sinks.is_empty() {
                    col = col.push(container(text::caption("No output devices")).padding([4, 6]));
                } else {
                    for sink in &self.audio.sinks {
                        let is_default = self.audio.default_sink.as_deref() == Some(&sink.name);
                        let mark = if is_default { "✓ " } else { "" };
                        col = col.push(
                            button::text(format!("{mark}{}", sink.description))
                                .width(Length::Fill)
                                .on_press(Message::SelectSink(sink.name.clone())),
                        );
                    }
                }
                col = col.push(divider::horizontal::default()).push(
                    button::text("Sound Settings…")
                        .width(Length::Fill)
                        .on_press(Message::OpenSettings("sound")),
                );
                Some(
                    container(scrollable(col).apply(container).max_height(180.0))
                        .padding([6, 8])
                        .width(Length::Fill)
                        .class(Self::card_style(false))
                        .into(),
                )
            }
        }
    }
}

impl cosmic::Application for ControlCenterApplet {
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = "dev.asw.CosmicExtAppletControlCenter";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, _flags: Self::Flags) -> (Self, app::Task<Self::Message>) {
        let notifications_config_helper = Config::new(
            cosmic_notifications_config::ID,
            NotificationsConfig::VERSION,
        )
        .ok();
        let notifications_config = notifications_config_helper
            .as_ref()
            .and_then(|h| NotificationsConfig::get_entry(h).ok())
            .unwrap_or_default();

        let comp_config_helper =
            Config::new("com.system76.CosmicComp", CompAutotileConfig::VERSION).ok();
        let comp_config = comp_config_helper
            .as_ref()
            .and_then(|h| CompAutotileConfig::get_entry(h).ok())
            .unwrap_or_default();
        let autotiled = comp_config.autotile;

        let app = Self {
            core,
            popup: None,
            rectangle_tracker: None,
            rectangle: Rectangle::default(),
            token_tx: None,
            zbus_connection: None,
            expanded: ExpandedSection::None,
            network: NetworkState::default(),
            bluetooth: BluetoothState::default(),
            notifications_config,
            notifications_config_helper,
            comp_config,
            comp_config_helper,
            autotiled,
            tiling_tx: None,
            settings_daemon_sender: None,
            max_screen_brightness: None,
            screen_brightness: None,
            dragging_brightness: false,
            audio: AudioState::default(),
            dragging_volume: false,
            player_status: None,
        };

        let init_tasks = Task::batch([
            Task::perform(zbus::Connection::session(), |res| {
                cosmic::Action::App(Message::ZbusConnection(res.map_err(|e| e.to_string())))
            }),
            app.refresh_all(),
        ]);

        (app, init_tasks)
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        fn periodic_refresh() -> Subscription<Message> {
            Subscription::run_with("cc-poll", |_| {
                stream::channel(1, |mut output: mpsc::Sender<Message>| async move {
                    let mut interval = tokio::time::interval(Duration::from_secs(5));
                    loop {
                        interval.tick().await;
                        let _ = output.send(Message::TickRefresh).await;
                    }
                })
            })
        }

        let mut subs = vec![
            rectangle_tracker_subscription(0).map(|e| Message::Rectangle(e.1)),
            activation_token_subscription(0).map(Message::Token),
            mpris_subscription::mpris_subscription(0).map(Message::Mpris),
            tiling::tiling_subscription().map(Message::Tiling),
            periodic_refresh(),
            self.core
                .watch_config(cosmic_notifications_config::ID)
                .map(|u| Message::NotificationsConfig(u.config)),
            self.core
                .watch_config::<CompAutotileConfig>("com.system76.CosmicComp")
                .map(|u| Message::CompConfig(Box::new(u.config))),
        ];
        if let Some(conn) = self.zbus_connection.clone() {
            subs.push(settings_daemon::subscription(conn).map(Message::SettingsDaemon));
        }
        Subscription::batch(subs)
    }

    fn update(&mut self, message: Self::Message) -> app::Task<Self::Message> {
        match message {
            Message::TogglePopup => {
                if let Some(p) = self.popup.take() {
                    self.expanded = ExpandedSection::None;
                    return destroy_popup(p);
                } else {
                    self.expanded = ExpandedSection::None;
                    let refresh = self.refresh_all();
                    let popup_task =
                        cosmic::surface::surface_task(cosmic::surface::action::app_popup(
                            |_| Default::default(),
                            |app: &mut Self| {
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
                                    .max_height(800.0);
                                popup_settings
                            },
                            None,
                        ));
                    return Task::batch([refresh, popup_task]);
                }
            }
            Message::CloseRequested(id) => {
                if Some(id) == self.popup {
                    self.popup = None;
                    self.expanded = ExpandedSection::None;
                }
            }
            Message::Rectangle(u) => match u {
                RectangleUpdate::Rectangle(r) => self.rectangle = r.1,
                RectangleUpdate::Init(tracker) => self.rectangle_tracker = Some(tracker),
            },
            Message::ZbusConnection(Ok(conn)) => {
                self.zbus_connection = Some(conn);
            }
            Message::ZbusConnection(Err(err)) => {
                tracing::warn!(%err, "Session D-Bus connection failed");
            }
            Message::TickRefresh => {
                if self.popup.is_some() {
                    return self.refresh_all();
                }
            }
            Message::NetworkLoaded(Some(state)) => {
                self.network = state;
            }
            Message::NetworkLoaded(None) => {}
            Message::ToggleWifi => {
                let target = !self.network.wifi_enabled;
                self.network.wifi_enabled = target;
                return cosmic::task::future(async move {
                    Message::NetworkLoaded(network::set_wifi_enabled(target).await)
                })
                .map(cosmic::Action::App);
            }
            Message::ConnectWifi(net) => {
                return cosmic::task::future(async move {
                    match network::connect_saved_network(net).await {
                        Ok(state) => Message::NetworkLoaded(state),
                        Err(_) => Message::OpenSettings("wireless"),
                    }
                })
                .map(cosmic::Action::App);
            }
            Message::BluetoothLoaded(state) => {
                self.bluetooth = state;
            }
            Message::ToggleBluetooth => {
                let target = !self.bluetooth.powered;
                let adapter_path = self.bluetooth.adapter_path.clone();
                self.bluetooth.powered = target;
                return cosmic::task::future(async move {
                    Message::BluetoothLoaded(
                        bluetooth::set_bluetooth_powered(adapter_path, target).await,
                    )
                })
                .map(cosmic::Action::App);
            }
            Message::ToggleBtDevice(path, connect) => {
                return cosmic::task::future(async move {
                    Message::BluetoothLoaded(
                        bluetooth::toggle_device_connection(path, connect).await,
                    )
                })
                .map(cosmic::Action::App);
            }
            Message::NotificationsConfig(cfg) => {
                self.notifications_config = cfg;
            }
            Message::ToggleFocus => {
                self.notifications_config.do_not_disturb =
                    !self.notifications_config.do_not_disturb;
                if let Some(helper) = &self.notifications_config_helper
                    && let Err(err) = self.notifications_config.write_entry(helper)
                {
                    tracing::error!(?err, "Failed to update Do Not Disturb");
                }
            }
            Message::CompConfig(cfg) => {
                self.autotiled = cfg.autotile;
                self.comp_config = *cfg;
            }
            Message::Tiling(ev) => match ev {
                TilingEvent::Ready(tx) => self.tiling_tx = Some(tx),
                TilingEvent::State(s) => {
                    self.autotiled = matches!(s, TilingState::TilingEnabled);
                }
            },
            Message::ToggleTiling => {
                let next = !self.autotiled;
                self.autotiled = next;
                self.comp_config.autotile = next;
                if let Some(helper) = &self.comp_config_helper
                    && let Err(err) = helper.set("autotile", next)
                {
                    tracing::error!(?err, "Failed to update cosmic-comp autotile");
                }
                if let Some(tx) = &self.tiling_tx {
                    let target = if next {
                        TilingState::TilingEnabled
                    } else {
                        TilingState::FloatingOnly
                    };
                    let _ = tx.send(target);
                }
            }
            Message::SettingsDaemon(ev) => match ev {
                settings_daemon::Event::Sender(tx) => self.settings_daemon_sender = Some(tx),
                settings_daemon::Event::MaxDisplayBrightness(max) => {
                    self.max_screen_brightness = Some(max);
                }
                settings_daemon::Event::DisplayBrightness(val) => {
                    if !self.dragging_brightness {
                        self.screen_brightness = Some(val);
                    }
                }
            },
            Message::SetBrightness(val) => {
                let snapped = match self.max_screen_brightness {
                    Some(max) if max > 0 && max <= 20 => {
                        let k =
                            ((val as i64 * 20 + (max as i64) / 2) / (max as i64)).clamp(0, 20);
                        (((k * (max as i64)) + 10) / 20) as i32
                    }
                    _ => val,
                };
                self.screen_brightness = Some(snapped);
                if !self.dragging_brightness {
                    self.dragging_brightness = true;
                    return cosmic::task::message(cosmic::Action::App(
                        Message::SetBrightnessDebounced,
                    ));
                }
            }
            Message::SetBrightnessDebounced => {
                if !self.dragging_brightness {
                    return Task::none();
                }
                if let (Some(tx), Some(b)) = (&self.settings_daemon_sender, self.screen_brightness)
                {
                    let _ = tx.send(settings_daemon::Request::SetDisplayBrightness(b));
                }
                return cosmic::iced::Task::perform(
                    tokio::time::sleep(Duration::from_millis(160)),
                    |()| cosmic::Action::App(Message::SetBrightnessDebounced),
                );
            }
            Message::ReleaseBrightness => {
                self.dragging_brightness = false;
                if let (Some(tx), Some(b)) = (&self.settings_daemon_sender, self.screen_brightness)
                {
                    let _ = tx.send(settings_daemon::Request::SetDisplayBrightness(b));
                }
            }
            Message::AudioLoaded(state) => {
                if !self.dragging_volume {
                    self.audio = state;
                }
            }
            Message::SetVolume(vol) => {
                self.dragging_volume = true;
                self.audio.active_mute = false;
                self.audio.active_volume = vol;
                audio::set_sink_volume(vol);
            }
            Message::ReleaseVolume => {
                self.dragging_volume = false;
                let vol = self.audio.active_volume;
                return cosmic::task::future(async move {
                    audio::set_sink_volume(vol);
                    Message::AudioLoaded(audio::fetch_audio_state())
                })
                .map(cosmic::Action::App);
            }
            Message::ToggleMute => {
                self.audio.active_mute = !self.audio.active_mute;
                return cosmic::task::future(async {
                    audio::toggle_sink_mute();
                    Message::AudioLoaded(audio::fetch_audio_state())
                })
                .map(cosmic::Action::App);
            }
            Message::SelectSink(name) => {
                self.audio.default_sink = Some(name.clone());
                return cosmic::task::future(async move {
                    audio::set_default_sink(&name);
                    Message::AudioLoaded(audio::fetch_audio_state())
                })
                .map(cosmic::Action::App);
            }
            Message::Mpris(MprisUpdate::Player(p)) => self.player_status = Some(p),
            Message::Mpris(MprisUpdate::Setup | MprisUpdate::Finished) => {
                self.player_status = None;
            }
            Message::MprisRequest(req) => {
                if let Some(status) = self.player_status.as_ref() {
                    let player = status.player.clone();
                    tokio::spawn(async move {
                        let _ = match req {
                            MprisRequest::Play => player.play().await.map_err(|e| e.to_string()),
                            MprisRequest::Pause => player.pause().await.map_err(|e| e.to_string()),
                            MprisRequest::Next => player.next().await.map_err(|e| e.to_string()),
                            MprisRequest::Previous => {
                                player.previous().await.map_err(|e| e.to_string())
                            }
                            MprisRequest::Raise => match player.media_player().await {
                                Ok(mp) => mp.raise().await.map_err(|e| e.to_string()),
                                Err(e) => Err(e.to_string()),
                            },
                        };
                    });
                }
            }
            Message::ToggleSection(sec) => {
                self.expanded = if self.expanded == sec {
                    ExpandedSection::None
                } else {
                    sec
                };
            }
            Message::OpenSettings(panel) => {
                if let Some(tx) = self.token_tx.as_ref() {
                    let _ = tx.send(TokenRequest {
                        app_id: Self::APP_ID.to_string(),
                        exec: if panel.is_empty() {
                            "cosmic-settings".to_string()
                        } else {
                            format!("cosmic-settings {panel}")
                        },
                    });
                } else {
                    let mut cmd = std::process::Command::new("cosmic-settings");
                    if !panel.is_empty() {
                        cmd.arg(panel);
                    }
                    tokio::spawn(cosmic::process::spawn(cmd));
                }
                if let Some(p) = self.popup.take() {
                    return destroy_popup(p);
                }
            }
            Message::Token(u) => match u {
                TokenUpdate::Init(tx) => self.token_tx = Some(tx),
                TokenUpdate::Finished => self.token_tx = None,
                TokenUpdate::ActivationToken { token, exec } => {
                    let mut parts = exec.split_whitespace();
                    if let Some(bin) = parts.next() {
                        let mut cmd = std::process::Command::new(bin);
                        for arg in parts {
                            cmd.arg(arg);
                        }
                        if let Some(token) = token {
                            cmd.env("XDG_ACTIVATION_TOKEN", &token);
                            cmd.env("DESKTOP_STARTUP_ID", &token);
                        }
                        tokio::spawn(cosmic::process::spawn(cmd));
                    }
                }
            },
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Self::Message> {
        let max_icon = panel_icon_size();
        let icon_size = self.core.applet.suggested_size(true).0.min(max_icon);
        let btn = mac_pill_button(
            control_center_icon(icon_size),
            self.popup.is_some(),
            PillVariant::IconOnly,
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

    fn view_window(&self, _id: window::Id) -> Element<'_, Self::Message> {
        let mut left_col = column![self.view_wifi_row()].spacing(8);
        if self.bluetooth.has_adapter {
            left_col = left_col.push(self.view_bluetooth_row());
        }
        let left_card = container(left_col)
            .padding([8, 10])
            .width(Length::FillPortion(1))
            .class(Self::card_style(false));

        let right_col = column![
            self.view_small_tile(
                "weather-clear-night-symbolic",
                "Focus",
                self.notifications_config.do_not_disturb,
                Message::ToggleFocus,
            ),
            self.view_small_tile(
                "view-grid-symbolic",
                "Tiling",
                self.autotiled,
                Message::ToggleTiling,
            ),
        ]
        .spacing(8)
        .width(Length::FillPortion(1));

        let top_grid = row![left_card, right_col]
            .spacing(8)
            .align_y(Alignment::Center);

        let mut root = column![top_grid].spacing(8);

        if let Some(expanded) = self.view_expanded_section() {
            root = root.push(expanded);
        }
        if let Some(brightness) = self.view_brightness_card() {
            root = root.push(brightness);
        }
        root = root.push(self.view_sound_card());
        if let Some(now_playing) = self.view_now_playing_card() {
            root = root.push(now_playing);
        }

        let quick_links = row![
            button::text("Edit Controls…")
                .on_press(Message::OpenSettings("")),
            space::horizontal().width(Length::Fill),
            button::icon(icon::from_name("preferences-desktop-display-symbolic").symbolic(true))
                .extra_small()
                .on_press(Message::OpenSettings("displays")),
            button::icon(icon::from_name("preferences-system-notifications-symbolic").symbolic(true))
                .extra_small()
                .on_press(Message::OpenSettings("notifications")),
        ]
        .spacing(4)
        .align_y(Alignment::Center);

        root = root.push(quick_links);

        mac_popover_container_sized(
            &self.core.applet,
            root.padding(10).width(Length::Fixed(POPUP_WIDTH)),
            POPUP_WIDTH,
            POPUP_WIDTH,
        )
        .into()
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Message> {
        Some(Message::CloseRequested(id))
    }
}

pub fn volume_icon_name(volume: u32, mute: bool) -> &'static str {
    if mute || volume == 0 {
        "audio-volume-muted-symbolic"
    } else if volume < 33 {
        "audio-volume-low-symbolic"
    } else if volume < 66 {
        "audio-volume-medium-symbolic"
    } else {
        "audio-volume-high-symbolic"
    }
}

pub fn brightness_percent(raw: i32, max: i32) -> f64 {
    if max <= 0 {
        return 0.0;
    }
    ((raw as f64 / max as f64) * 100.0).clamp(0.0, 100.0)
}

pub fn wifi_subtitle(net: &NetworkState) -> String {
    if !net.wifi_enabled {
        "Off".to_string()
    } else if let Some(ssid) = &net.active_ssid {
        truncate(ssid, 14)
    } else if net.wired_connected {
        "Ethernet".to_string()
    } else {
        "On".to_string()
    }
}

pub fn bluetooth_subtitle(bt: &BluetoothState) -> String {
    if !bt.powered {
        "Off".to_string()
    } else if let Some(dev) = bt.devices.iter().find(|d| d.connected) {
        truncate(&dev.name, 14)
    } else {
        "On".to_string()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_icon_buckets() {
        assert_eq!(volume_icon_name(0, false), "audio-volume-muted-symbolic");
        assert_eq!(volume_icon_name(50, true), "audio-volume-muted-symbolic");
        assert_eq!(volume_icon_name(20, false), "audio-volume-low-symbolic");
        assert_eq!(volume_icon_name(50, false), "audio-volume-medium-symbolic");
        assert_eq!(volume_icon_name(80, false), "audio-volume-high-symbolic");
    }

    #[test]
    fn brightness_percent_clamps() {
        assert_eq!(brightness_percent(50, 100), 50.0);
        assert_eq!(brightness_percent(200, 100), 100.0);
        assert_eq!(brightness_percent(10, 0), 0.0);
    }

    #[test]
    fn radio_subtitles() {
        let off = NetworkState::default();
        assert_eq!(wifi_subtitle(&off), "Off");
        let on = NetworkState {
            wifi_present: true,
            wifi_enabled: true,
            active_ssid: Some("Home-5G".into()),
            wired_connected: false,
            networks: vec![],
        };
        assert_eq!(wifi_subtitle(&on), "Home-5G");
        let bt = BluetoothState {
            has_adapter: true,
            adapter_path: Some("/org/bluez/hci0".into()),
            powered: true,
            devices: vec![],
        };
        assert_eq!(bluetooth_subtitle(&bt), "On");
    }
}
