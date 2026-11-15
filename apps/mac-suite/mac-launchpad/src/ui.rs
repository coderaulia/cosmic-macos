// SPDX-License-Identifier: GPL-3.0-only

//! Full-screen layer-shell overlay with the application grid.

use std::sync::LazyLock;
use std::time::{Duration, Instant};

use cosmic::desktop::spawn_desktop_exec;
use cosmic::iced::alignment::Horizontal;
use cosmic::iced::core::text::{Ellipsize, EllipsizeHeightLimit};
use cosmic::iced::event::{Status, listen_raw};
use cosmic::iced::keyboard::{Key, Modifiers, key::Named};
use cosmic::iced::mouse::{self, ScrollDelta};
use cosmic::iced::platform_specific::runtime::wayland::layer_surface::{
    IcedOutput, SctkLayerSurfaceSettings,
};
use cosmic::iced::platform_specific::shell::wayland::commands::layer_surface::{
    Anchor, KeyboardInteractivity, Layer, destroy_layer_surface, get_layer_surface,
};
use cosmic::iced::runtime::core::event::{PlatformSpecific, wayland};
use cosmic::iced::runtime::core::window::Id as SurfaceId;
use cosmic::iced::runtime::{Action, platform_specific, task};
use cosmic::iced::widget::{column, container, mouse_area, row};
use cosmic::iced::{Alignment, Border, Color, Length, Rectangle, Subscription, stream};
use cosmic::theme::{Button, Container};
use cosmic::widget::{button, icon, space::vertical as vertical_space, text, text_input};
use cosmic::{Element, app::{self, Task}};

use crate::apps::{self, AppEntry};
use crate::ipc::{self, Command};

const COLUMNS: usize = 7;
const ROWS: usize = 5;
const PER_PAGE: usize = COLUMNS * ROWS;
const ICON_SIZE: u16 = 96;
const CELL_W: f32 = 150.0;
const CELL_H: f32 = 140.0;
const WHEEL_COOLDOWN: Duration = Duration::from_millis(250);

static INPUT_ID: LazyLock<cosmic::widget::Id> =
    LazyLock::new(|| cosmic::widget::Id::new("launchpad-search"));

#[derive(Clone, Debug)]
pub enum Message {
    Command(Command),
    Loaded(Vec<AppEntry>),
    Query(String),
    Submit,
    Launch(usize),
    Page(isize),
    GoPage(usize),
    Wheel(f32),
    Hide,
    Key(Key, Modifiers, Status),
    Unfocused(SurfaceId),
    Noop,
}

pub struct Launchpad {
    core: app::Core,
    window_id: SurfaceId,
    visible: bool,
    apps: Vec<AppEntry>,
    query: String,
    /// Indices into `apps` for the current query.
    matches: Vec<usize>,
    page: usize,
    last_wheel: Option<Instant>,
}

pub fn run(show: bool) -> cosmic::iced::Result {
    let settings = app::Settings::default()
        .antialiasing(true)
        .client_decorations(false)
        .default_text_size(14.0)
        .scale_factor(1.0)
        .no_main_window(true)
        .exit_on_close(false);
    app::run::<Launchpad>(settings, show)
}

fn page_count(n: usize) -> usize {
    n.div_ceil(PER_PAGE).max(1)
}

fn step_page(page: usize, delta: isize, count: usize) -> usize {
    page.saturating_add_signed(delta).min(count - 1)
}

impl Launchpad {
    fn pages(&self) -> usize {
        page_count(self.matches.len())
    }

    fn refilter(&mut self) {
        self.matches = apps::filter(&self.apps, &self.query);
        self.page = self.page.min(self.pages() - 1);
    }

    fn load_task() -> Task<Message> {
        Task::perform(
            async {
                tokio::task::spawn_blocking(|| apps::into_entries(apps::load()))
                    .await
                    .unwrap_or_default()
            },
            |apps| cosmic::Action::App(Message::Loaded(apps)),
        )
    }

    fn blur_task(&self) -> Task<Message> {
        let blur = self
            .core
            .system_theme()
            .cosmic()
            .frosted_system_interface
            .then(|| {
                vec![Rectangle {
                    x: 0.0,
                    y: 0.0,
                    width: f32::MAX,
                    height: f32::MAX,
                }]
            });
        task::effect(Action::PlatformSpecific(platform_specific::Action::Wayland(
            cosmic::iced::runtime::platform_specific::wayland::Action::BlurSurface(
                self.window_id,
                blur,
            ),
        )))
    }

    fn show(&mut self) -> Task<Message> {
        if self.visible {
            return Task::none();
        }
        self.visible = true;
        self.window_id = SurfaceId::unique();
        self.query.clear();
        self.page = 0;
        self.refilter();
        let surface = get_layer_surface(SctkLayerSurfaceSettings {
            id: self.window_id,
            layer: Layer::Overlay,
            keyboard_interactivity: KeyboardInteractivity::Exclusive,
            anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
            output: IcedOutput::Active,
            namespace: "mac-launchpad".into(),
            size: Some((None, None)),
            exclusive_zone: -1,
            ..Default::default()
        });
        Task::batch([
            surface,
            self.blur_task(),
            text_input::focus(INPUT_ID.clone()),
            Self::load_task(),
        ])
    }

    fn hide(&mut self) -> Task<Message> {
        if !self.visible {
            return Task::none();
        }
        self.visible = false;
        destroy_layer_surface(self.window_id)
    }

    fn launch(&mut self, app_idx: usize) -> Task<Message> {
        let Some(app) = self.apps.get(app_idx) else {
            return Task::none();
        };
        let (exec, id, terminal) = (app.exec.clone(), app.id.clone(), app.terminal);
        Task::batch([
            self.hide(),
            Task::perform(
                async move {
                    spawn_desktop_exec(exec, Vec::<(String, String)>::new(), Some(&id), terminal)
                        .await;
                },
                |()| cosmic::Action::App(Message::Noop),
            ),
        ])
    }

    fn cell(&self, app_idx: usize) -> Element<'_, Message> {
        let app = &self.apps[app_idx];
        let content = column![
            icon::icon(app.icon.clone()).size(ICON_SIZE),
            text::body(app.name.as_str())
                .width(Length::Fill)
                .align_x(Horizontal::Center)
                .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1))),
        ]
        .spacing(8)
        .align_x(Alignment::Center)
        .width(Length::Fill);

        button::custom(content)
            .width(Length::Fixed(CELL_W))
            .height(Length::Fixed(CELL_H))
            .padding([10, 6])
            .on_press(Message::Launch(app_idx))
            .class(Button::Custom {
                active: Box::new(|_, t| cell_style(t, false)),
                hovered: Box::new(|_, t| cell_style(t, true)),
                pressed: Box::new(|_, t| cell_style(t, true)),
                disabled: Box::new(|t| cell_style(t, false)),
            })
            .into()
    }

    fn grid(&self) -> Element<'_, Message> {
        if self.matches.is_empty() {
            return container(text::title3("No results"))
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .into();
        }
        let start = self.page * PER_PAGE;
        let page_items = &self.matches[start..(start + PER_PAGE).min(self.matches.len())];
        let rows = page_items.chunks(COLUMNS).map(|chunk| {
            row(chunk.iter().map(|&i| self.cell(i)))
                .spacing(8)
                .align_y(Alignment::Start)
                .into()
        });
        container(column(rows).spacing(8).align_x(Alignment::Start))
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
    }

    fn dots(&self) -> Element<'_, Message> {
        let count = self.pages();
        if count < 2 {
            return vertical_space().height(Length::Fixed(14.0)).into();
        }
        row((0..count).map(|i| {
            let active = i == self.page;
            mouse_area(
                container(vertical_space().height(Length::Fixed(0.0)))
                    .width(Length::Fixed(10.0))
                    .height(Length::Fixed(10.0))
                    .class(Container::custom(move |t| {
                        let c = Color::from(t.cosmic().on_bg_color());
                        container::Style {
                            background: Some(c.scale_alpha(if active { 0.9 } else { 0.3 }).into()),
                            border: Border {
                                radius: 5.0.into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        }
                    })),
            )
            .on_press(Message::GoPage(i))
            .into()
        }))
        .spacing(12)
        .align_y(Alignment::Center)
        .into()
    }
}

fn cell_style(theme: &cosmic::Theme, hovered: bool) -> button::Style {
    let c = theme.cosmic();
    button::Style {
        background: hovered.then(|| Color::from(c.on_bg_color()).scale_alpha(0.14).into()),
        text_color: Some(c.on_bg_color().into()),
        icon_color: Some(c.on_bg_color().into()),
        border_radius: c.corner_radii.radius_m.into(),
        ..Default::default()
    }
}

impl cosmic::Application for Launchpad {
    type Message = Message;
    type Executor = cosmic::executor::single::Executor;
    type Flags = bool;
    const APP_ID: &'static str = "dev.asw.MacLaunchpad";

    fn init(mut core: app::Core, show: bool) -> (Self, Task<Self::Message>) {
        core.set_app_type(cosmic::core::AppType::System);
        core.set_keyboard_nav(false);
        let mut this = Self {
            core,
            window_id: SurfaceId::unique(),
            visible: false,
            apps: Vec::new(),
            query: String::new(),
            matches: Vec::new(),
            page: 0,
            last_wheel: None,
        };
        // Preload so the first show is instant; refreshed again on every show.
        let task = if show {
            this.show()
        } else {
            Self::load_task()
        };
        (this, task)
    }

    fn core(&self) -> &app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut app::Core {
        &mut self.core
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::Command(Command::Toggle) => {
                return if self.visible { self.hide() } else { self.show() };
            }
            Message::Command(Command::Show) => return self.show(),
            Message::Command(Command::Hide) | Message::Hide => return self.hide(),
            Message::Loaded(apps) => {
                self.apps = apps;
                self.refilter();
            }
            Message::Query(q) => {
                self.query = q;
                self.page = 0;
                self.refilter();
            }
            Message::Submit => {
                if let Some(&i) = self.matches.first() {
                    return self.launch(i);
                }
            }
            Message::Launch(i) => return self.launch(i),
            Message::Page(d) => self.page = step_page(self.page, d, self.pages()),
            Message::GoPage(p) => self.page = p.min(self.pages() - 1),
            Message::Wheel(dy) => {
                let ready = self.last_wheel.is_none_or(|t| t.elapsed() >= WHEEL_COOLDOWN);
                if self.visible && ready && dy != 0.0 {
                    self.last_wheel = Some(Instant::now());
                    // Wheel down / scroll forward moves to the next page.
                    self.page = step_page(self.page, if dy < 0.0 { 1 } else { -1 }, self.pages());
                }
            }
            Message::Key(key, mods, _status) => {
                if !self.visible {
                    return Task::none();
                }
                match key {
                    Key::Named(Named::Escape) => {
                        if self.query.is_empty() {
                            return self.hide();
                        }
                        self.query.clear();
                        self.page = 0;
                        self.refilter();
                    }
                    Key::Named(Named::PageDown) => return self.update(Message::Page(1)),
                    Key::Named(Named::PageUp) => return self.update(Message::Page(-1)),
                    Key::Named(Named::ArrowRight) if self.query.is_empty() || mods.control() => {
                        return self.update(Message::Page(1));
                    }
                    Key::Named(Named::ArrowLeft) if self.query.is_empty() || mods.control() => {
                        return self.update(Message::Page(-1));
                    }
                    _ => {}
                }
            }
            Message::Unfocused(id) => {
                if id == self.window_id {
                    return self.hide();
                }
            }
            Message::Noop => {}
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Self::Message> {
        unreachable!("layer-shell overlay uses view_window")
    }

    fn view_window(&self, id: SurfaceId) -> Element<'_, Self::Message> {
        if id != self.window_id || !self.visible {
            return vertical_space().height(Length::Fixed(1.0)).into();
        }

        let search = text_input::search_input("Search", &self.query)
            .id(INPUT_ID.clone())
            .on_input(Message::Query)
            .on_paste(Message::Query)
            .on_submit(|_| Message::Submit)
            .width(Length::Fixed(360.0))
            .size(16.0)
            .padding([8, 12])
            .always_active();

        let body = column![
            container(search).center_x(Length::Fill).padding([40, 0, 0, 0]),
            container(self.grid()).width(Length::Fill).height(Length::Fill),
            container(self.dots()).center_x(Length::Fill).padding([0, 0, 36, 0]),
        ]
        .width(Length::Fill)
        .height(Length::Fill);

        // Clicks on empty space (not captured by a child) hide Launchpad.
        mouse_area(
            container(body)
                .width(Length::Fill)
                .height(Length::Fill)
                .class(Container::custom(|theme| {
                    let t = theme.cosmic();
                    container::Style {
                        text_color: Some(t.on_bg_color().into()),
                        icon_color: Some(t.on_bg_color().into()),
                        background: Some(Color::from(t.bg_color()).scale_alpha(0.55).into()),
                        ..Default::default()
                    }
                })),
        )
        .on_press(Message::Hide)
        .into()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        Subscription::batch([
            listen_raw(|event, status, _id| match event {
                cosmic::iced::Event::PlatformSpecific(PlatformSpecific::Wayland(
                    wayland::Event::Layer(wayland::LayerEvent::Unfocused, _, layer_id),
                )) => Some(Message::Unfocused(layer_id)),
                cosmic::iced::Event::Keyboard(cosmic::iced::keyboard::Event::KeyPressed {
                    key,
                    modifiers,
                    ..
                }) => Some(Message::Key(key, modifiers, status)),
                cosmic::iced::Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                    let (x, y) = match delta {
                        ScrollDelta::Lines { x, y } | ScrollDelta::Pixels { x, y } => (x, y),
                    };
                    Some(Message::Wheel(if y.abs() >= x.abs() { y } else { x }))
                }
                _ => None,
            }),
            Subscription::run(socket_stream),
        ])
    }
}

fn socket_stream() -> impl cosmic::iced::futures::Stream<Item = Message> {
    stream::channel(
        8,
        |mut tx: cosmic::iced::futures::channel::mpsc::Sender<Message>| async move {
            use tokio::io::{AsyncBufReadExt, BufReader};

            let path = ipc::socket_path();
            let listener = match tokio::net::UnixListener::bind(&path) {
                Ok(l) => l,
                Err(first) => {
                    // A live peer owns the socket: we lost a start-up race, so
                    // quit instead of lingering as a deaf second daemon.
                    if tokio::net::UnixStream::connect(&path).await.is_ok() {
                        std::process::exit(0);
                    }
                    // Stale socket file from a crashed instance: replace it.
                    let _ = std::fs::remove_file(&path);
                    match tokio::net::UnixListener::bind(&path) {
                        Ok(l) => l,
                        Err(e) => {
                            tracing::error!("cannot bind {}: {first}; {e}", path.display());
                            std::process::exit(1);
                        }
                    }
                }
            };
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                let mut line = String::new();
                if BufReader::new(stream).read_line(&mut line).await.is_ok()
                    && let Some(cmd) = Command::parse(&line)
                {
                    let _ = tx.try_send(Message::Command(cmd));
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_count_rounds_up_and_is_at_least_one() {
        assert_eq!(page_count(0), 1);
        assert_eq!(page_count(PER_PAGE), 1);
        assert_eq!(page_count(PER_PAGE + 1), 2);
    }

    #[test]
    fn step_page_clamps() {
        assert_eq!(step_page(0, -1, 3), 0);
        assert_eq!(step_page(1, 1, 3), 2);
        assert_eq!(step_page(2, 1, 3), 2);
    }
}
