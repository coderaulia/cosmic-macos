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
use cosmic::iced::{Alignment, Border, Color, Length, Rectangle, Size, Subscription, stream, window};
use cosmic::theme::{Button, Container};
use cosmic::widget::{button, icon, space::vertical as vertical_space, text, text_input};
use cosmic::{Element, app::{self, Task}};

use crate::apps::{self, AppEntry};
use crate::ipc::{self, Command};

const COLUMNS: usize = 7;
const ROWS: usize = 5;
const ICON_SIZE: u16 = 96;
const CELL_W: f32 = 150.0;
const CELL_H: f32 = 140.0;
const WHEEL_COOLDOWN: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, PartialEq)]
struct GridLayout {
    columns: usize,
    rows: usize,
    cell_w: f32,
    cell_h: f32,
    icon_size: u16,
    spacing: f32,
    search_w: f32,
    top_pad: u16,
    bottom_pad: u16,
}

impl Default for GridLayout {
    fn default() -> Self {
        Self {
            columns: COLUMNS,
            rows: ROWS,
            cell_w: CELL_W,
            cell_h: CELL_H,
            icon_size: ICON_SIZE,
            spacing: 8.0,
            search_w: 360.0,
            top_pad: 40,
            bottom_pad: 36,
        }
    }
}

impl GridLayout {
    fn per_page(self) -> usize {
        self.columns.saturating_mul(self.rows).max(1)
    }
}

fn compute_layout(viewport_w: f32, viewport_h: f32) -> GridLayout {
    if !viewport_w.is_finite()
        || !viewport_h.is_finite()
        || viewport_w <= 0.0
        || viewport_h <= 0.0
    {
        return GridLayout::default();
    }
    let scale = (viewport_w / 1200.0).min(viewport_h / 900.0).clamp(0.55, 1.0);
    let spacing = (8.0 * scale).round().clamp(4.0, 8.0);
    let top_pad = (viewport_h * 0.045).round().clamp(8.0, 40.0) as u16;
    let bottom_pad = (viewport_h * 0.04).round().clamp(8.0, 36.0) as u16;
    let search_w = 360.0_f32
        .min((viewport_w - 32.0).max(120.0))
        .min(viewport_w.max(1.0));
    let chrome_h = f32::from(top_pad) + f32::from(bottom_pad) + 56.0;
    let horiz_margin = (viewport_w * 0.04).clamp(16.0, 64.0);
    let avail_w = (viewport_w - horiz_margin).max(1.0);
    let avail_h = (viewport_h - chrome_h).max(1.0);

    let cell_w = (CELL_W * scale).clamp(84.0, CELL_W).min(avail_w);
    let cell_h = (CELL_H * scale).clamp(78.0, CELL_H).min(avail_h);
    let icon_size = (f32::from(ICON_SIZE) * scale)
        .round()
        .clamp(16.0, f32::from(ICON_SIZE))
        .min((cell_h * (f32::from(ICON_SIZE) / CELL_H)).round().max(16.0)) as u16;

    let columns = (((avail_w + spacing) / (cell_w + spacing)).floor() as usize).clamp(1, COLUMNS);
    let rows = (((avail_h + spacing) / (cell_h + spacing)).floor() as usize).clamp(1, ROWS);

    GridLayout {
        columns,
        rows,
        cell_w,
        cell_h,
        icon_size,
        spacing,
        search_w,
        top_pad,
        bottom_pad,
    }
}

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
    Resized(SurfaceId, Size),
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
    layout: GridLayout,
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

fn page_count(n: usize, per_page: usize) -> usize {
    n.div_ceil(per_page.max(1)).max(1)
}

fn step_page(page: usize, delta: isize, count: usize) -> usize {
    page.saturating_add_signed(delta).min(count.saturating_sub(1))
}

fn page_slice(matches: &[usize], page: usize, per_page: usize) -> &[usize] {
    if matches.is_empty() {
        return &[];
    }
    let per_page = per_page.max(1);
    let clamped_page = page.min(page_count(matches.len(), per_page).saturating_sub(1));
    let start = clamped_page.saturating_mul(per_page).min(matches.len());
    let end = start.saturating_add(per_page).min(matches.len());
    &matches[start..end]
}

impl Launchpad {
    fn pages(&self) -> usize {
        page_count(self.matches.len(), self.layout.per_page())
    }

    fn refilter(&mut self) {
        self.matches = apps::filter(&self.apps, &self.query);
        self.page = self.page.min(self.pages().saturating_sub(1));
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
            icon::icon(app.icon.clone()).size(self.layout.icon_size),
            text::body(app.name.as_str())
                .width(Length::Fill)
                .align_x(Horizontal::Center)
                .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1))),
        ]
        .spacing(8)
        .align_x(Alignment::Center)
        .width(Length::Fill);

        button::custom(content)
            .width(Length::Fixed(self.layout.cell_w))
            .height(Length::Fixed(self.layout.cell_h))
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
        let page_items = page_slice(&self.matches, self.page, self.layout.per_page());
        let rows = page_items.chunks(self.layout.columns.max(1)).map(|chunk| {
            row(chunk.iter().map(|&i| self.cell(i)))
                .spacing(self.layout.spacing)
                .align_y(Alignment::Start)
                .into()
        });
        container(
            column(rows)
                .spacing(self.layout.spacing)
                .align_x(Alignment::Start),
        )
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
            layout: GridLayout::default(),
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
            Message::GoPage(p) => self.page = p.min(self.pages().saturating_sub(1)),
            Message::Wheel(dy) => {
                let ready = self.last_wheel.is_none_or(|t| t.elapsed() >= WHEEL_COOLDOWN);
                if self.visible && ready && dy != 0.0 {
                    self.last_wheel = Some(Instant::now());
                    // Wheel down / scroll forward moves to the next page.
                    self.page = step_page(self.page, if dy < 0.0 { 1 } else { -1 }, self.pages());
                }
            }
            Message::Resized(id, size) => {
                if id == self.window_id {
                    self.layout = compute_layout(size.width, size.height);
                    self.page = self.page.min(self.pages().saturating_sub(1));
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
            .width(Length::Fixed(self.layout.search_w))
            .size(16.0)
            .padding([8, 12])
            .always_active();

        let body = column![
            container(search)
                .center_x(Length::Fill)
                .padding([self.layout.top_pad, 0, 0, 0]),
            container(self.grid()).width(Length::Fill).height(Length::Fill),
            container(self.dots())
                .center_x(Length::Fill)
                .padding([0, 0, self.layout.bottom_pad, 0]),
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
            listen_raw(|event, status, id| match event {
                cosmic::iced::Event::PlatformSpecific(PlatformSpecific::Wayland(
                    wayland::Event::Layer(wayland::LayerEvent::Unfocused, _, layer_id),
                )) => Some(Message::Unfocused(layer_id)),
                cosmic::iced::Event::Window(window::Event::Resized(size)) => {
                    Some(Message::Resized(id, size))
                }
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

            let Some(path) = ipc::socket_path() else {
                tracing::error!("no private runtime directory; Launchpad cannot be toggled");
                return;
            };
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
        let per_page = COLUMNS * ROWS;
        assert_eq!(page_count(0, per_page), 1);
        assert_eq!(page_count(per_page, per_page), 1);
        assert_eq!(page_count(per_page + 1, per_page), 2);
        assert_eq!(page_count(10, 0), 10);
    }

    #[test]
    fn step_page_clamps_and_avoids_underflow() {
        assert_eq!(step_page(0, -1, 0), 0);
        assert_eq!(step_page(5, 1, 0), 0);
        assert_eq!(step_page(0, -1, 3), 0);
        assert_eq!(step_page(1, 1, 3), 2);
        assert_eq!(step_page(2, 1, 3), 2);
    }

    #[test]
    fn page_slice_handles_empty_and_out_of_bounds_pages() {
        assert_eq!(page_slice(&[], 0, 35), &[] as &[usize]);
        assert_eq!(page_slice(&[], 4, 0), &[] as &[usize]);
        let items: Vec<usize> = (0..10).collect();
        assert_eq!(page_slice(&items, 0, 4), &[0, 1, 2, 3]);
        assert_eq!(page_slice(&items, 2, 4), &[8, 9]);
        // Out-of-bounds page clamps to the last valid page rather than panicking.
        assert_eq!(page_slice(&items, 99, 4), &[8, 9]);
        assert_eq!(page_slice(&items, usize::MAX, 4), &[8, 9]);
    }

    #[test]
    fn compute_layout_adapts_to_viewports_and_fractional_scales() {
        let fhd = compute_layout(1920.0, 1080.0);
        assert_eq!((fhd.columns, fhd.rows, fhd.icon_size), (7, 5, 96));

        // 1920x1080 at 150% fractional scale -> 1280x720 logical.
        let hidpi_150 = compute_layout(1280.0, 720.0);
        assert!(hidpi_150.columns >= 5 && hidpi_150.rows >= 4);
        assert!(hidpi_150.cell_w * hidpi_150.columns as f32 <= 1280.0);
        assert!(hidpi_150.cell_h * hidpi_150.rows as f32 <= 720.0);

        // 1920x1080 at 200% scale -> 960x540 logical.
        let hidpi_200 = compute_layout(960.0, 540.0);
        assert!(hidpi_200.columns >= 1 && hidpi_200.rows >= 1);
        assert!(hidpi_200.cell_w * hidpi_200.columns as f32 <= 960.0);

        // Tiny and invalid viewports remain safe and non-zero.
        for (w, h) in [(240.0, 180.0), (80.0, 60.0), (0.0, 0.0), (-100.0, 500.0), (f32::NAN, 720.0)] {
            let l = compute_layout(w, h);
            assert!(l.columns >= 1);
            assert!(l.rows >= 1);
            assert!(l.per_page() >= 1);
            assert!(l.icon_size >= 16);
            assert!(l.cell_w > 0.0 && l.cell_h > 0.0);
        }
    }
}
