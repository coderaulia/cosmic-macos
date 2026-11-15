// SPDX-License-Identifier: GPL-3.0-only
//! Multi-output Bottom-layer Wayland desktop widgets UI, drag-to-move with
//! 20 px grid snapping, right-click sizing/removal, and Edit Widgets gallery.

use std::collections::HashMap;
use std::time::Duration;

use cctk::sctk::output::OutputInfo;
use cctk::sctk::reexports::client::protocol::wl_output::WlOutput;
use cosmic::{
    Element,
    app::{self, Task},
    iced::{
        Alignment, Background, Border, Color, Length, Point, Rectangle, Size, Subscription,
        event::listen_raw,
        keyboard::{Key, key::Named},
        mouse,
        platform_specific::runtime::wayland::layer_surface::{IcedOutput, SctkLayerSurfaceSettings},
        platform_specific::shell::wayland::commands::layer_surface::{
            Anchor, KeyboardInteractivity, Layer, destroy_layer_surface, get_layer_surface,
            set_input_zone, set_keyboard_interactivity,
        },
        runtime::core::event::{
            PlatformSpecific,
            wayland::{self, OutputEvent},
        },
        runtime::core::layout::Limits,
        runtime::core::window::Id as SurfaceId,
        widget::{column, container, mouse_area, pin, row, stack},
        window,
    },
    theme::{Button, Container},
    widget::{button, divider, space, text},
};
use jiff::Zoned;
use mac_widgets::{
    settings::WidgetSettings,
    stats::{CpuSample, Stats},
    weather::{self as mac_weather, Forecast, Location},
};

use crate::agenda::{self, AgendaEntry};
use crate::cards::{
    WeatherState, WidgetData, frosted_chrome_class, gallery_tile, remove_badge_button,
    render_widget,
};
use crate::ipc::{self, Command as IpcCommand};
use crate::layout::{
    DesktopWidgetsLayout, WidgetKind, WidgetSize, clamp_and_snap, layout_path,
};
use crate::wayland::{FocusEvent, focus_subscription};

const DRAG_THRESHOLD: f32 = 6.0;
const GALLERY_STRIP_CLEARANCE: f32 = 150.0;
const CONTEXT_MENU_WIDTH: f32 = 176.0;
const CONTEXT_MENU_HEIGHT: f32 = 196.0;

#[derive(Clone, Debug)]
struct OutputSurface {
    wl_output: WlOutput,
    surface_id: SurfaceId,
    name: Option<String>,
    logical_size: (u32, u32),
}

#[derive(Clone, Debug)]
enum DragState {
    PendingWidget {
        index: usize,
        surface_id: SurfaceId,
        start: Point,
        orig_x: i32,
        orig_y: i32,
    },
    ActiveWidget {
        index: usize,
        surface_id: SurfaceId,
        offset_x: f32,
        offset_y: f32,
        current_x: f32,
        current_y: f32,
    },
    PendingGallery {
        kind: WidgetKind,
        size: WidgetSize,
        surface_id: SurfaceId,
        start: Point,
    },
    ActiveGallery {
        kind: WidgetKind,
        size: WidgetSize,
        surface_id: SurfaceId,
        pos: Point,
    },
}

#[derive(Clone, Debug)]
struct ContextMenu {
    index: usize,
    surface_id: SurfaceId,
    x: f32,
    y: f32,
}

#[derive(Clone, Debug)]
pub enum Message {
    Output(OutputEvent, WlOutput),
    Resized(SurfaceId, Size),
    CursorMoved(SurfaceId, Point),
    MouseReleased(SurfaceId, mouse::Button),
    KeyPressed(Key),
    WidgetPress(SurfaceId, usize),
    WidgetRightPress(SurfaceId, usize),
    GalleryPress(SurfaceId, WidgetKind),
    SelectGallerySize(WidgetSize),
    ResizeWidget(usize, WidgetSize),
    RemoveWidget(usize),
    SetEditing(bool),
    CloseContextMenu,
    Focus(FocusEvent),
    Ipc(IpcCommand),
    StatsTick,
    ClockTick,
    WeatherTick,
    WeatherLoaded(Result<Forecast, String>),
    Noop,
}

pub fn run(start_editing: bool) -> cosmic::iced::Result {
    let settings = app::Settings::default()
        .antialiasing(true)
        .client_decorations(false)
        .no_main_window(true)
        .exit_on_close(false);
    app::run::<DesktopWidgets>(settings, start_editing)
}

pub struct DesktopWidgets {
    core: app::Core,
    outputs: Vec<OutputSurface>,
    layout: DesktopWidgetsLayout,
    layout_initialized: bool,
    editing: bool,
    gallery_size: WidgetSize,
    drag: Option<DragState>,
    context_menu: Option<ContextMenu>,
    cursor_pos: HashMap<SurfaceId, Point>,
    normal_window_focused: bool,
    now: Zoned,
    widget_settings: WidgetSettings,
    weather: WeatherState,
    stats: Stats,
    cpu_prev: Option<CpuSample>,
    agenda: Vec<AgendaEntry>,
}

impl DesktopWidgets {
    fn output_by_surface(&self, surface_id: SurfaceId) -> Option<&OutputSurface> {
        self.outputs.iter().find(|o| o.surface_id == surface_id)
    }

    fn output_name_or_fallback(&self, surface_id: SurfaceId) -> String {
        self.output_by_surface(surface_id)
            .and_then(|o| o.name.clone())
            .unwrap_or_else(|| "default".to_owned())
    }

    fn output_size(&self, output_name: &str) -> (u32, u32) {
        self.outputs
            .iter()
            .find(|o| o.name.as_deref() == Some(output_name))
            .map(|o| o.logical_size)
            .unwrap_or((1920, 1080))
    }

    fn input_zone_for_output(&self, output_name: Option<&str>) -> Option<Vec<Rectangle>> {
        if self.editing || self.context_menu.is_some() {
            return None;
        }
        let Some(name) = output_name else {
            return Some(Vec::new());
        };
        let rects = self
            .layout
            .widgets
            .iter()
            .filter(|w| w.output == name)
            .map(|w| {
                let (ww, hh) = w.size.dimensions();
                Rectangle::new(
                    Point::new(w.x as f32, w.y as f32),
                    Size::new(ww as f32, hh as f32),
                )
            })
            .collect();
        Some(rects)
    }

    fn sync_input_zones(&self) -> Task<Message> {
        let tasks: Vec<Task<Message>> = self
            .outputs
            .iter()
            .map(|o| set_input_zone(o.surface_id, self.input_zone_for_output(o.name.as_deref())))
            .collect();
        Task::batch(tasks)
    }

    fn sync_keyboard_interactivity(&self) -> Task<Message> {
        let mode = if self.editing || self.context_menu.is_some() {
            KeyboardInteractivity::OnDemand
        } else {
            KeyboardInteractivity::None
        };
        let tasks: Vec<Task<Message>> = self
            .outputs
            .iter()
            .map(|o| set_keyboard_interactivity(o.surface_id, mode))
            .collect();
        Task::batch(tasks)
    }

    fn sync_surface_modes(&self) -> Task<Message> {
        Task::batch([self.sync_input_zones(), self.sync_keyboard_interactivity()])
    }

    fn save_layout(&self) {
        if let Some(path) = layout_path()
            && let Err(err) = self.layout.save_to(&path)
        {
            tracing::error!(?err, ?path, "Failed to save desktop-widgets.ron");
        }
    }

    fn maybe_init_layout(&mut self, output_name: &str, size: (u32, u32)) {
        if self.layout_initialized {
            return;
        }
        let (loaded, _created) =
            DesktopWidgetsLayout::load_or_default_at(layout_path().as_deref(), output_name, size.0, size.1);
        self.layout = loaded;
        self.layout_initialized = true;
    }

    fn refresh_weather(&mut self) -> Task<Message> {
        self.widget_settings = WidgetSettings::load();
        if !self.layout.has_kind(WidgetKind::Weather) && !self.editing {
            return Task::none();
        }
        let Some(loc) = Location::load() else {
            self.weather = WeatherState::NoLocation;
            return Task::none();
        };
        if let Some(cached) = mac_weather::cached(&loc) {
            self.weather = WeatherState::Ready(cached);
            return Task::none();
        }
        if !matches!(self.weather, WeatherState::Ready(_)) {
            self.weather = WeatherState::Loading;
        }
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || mac_weather::load_or_fetch(&loc))
                    .await
                    .map_err(|e| e.to_string())?
            },
            |res| cosmic::Action::App(Message::WeatherLoaded(res)),
        )
    }

    fn refresh_stats_if_visible(&mut self) {
        if self.layout.has_kind(WidgetKind::System) || self.editing {
            self.stats = Stats::read(&mut self.cpu_prev);
        }
    }

    fn refresh_agenda_if_visible(&mut self) {
        if self.layout.has_kind(WidgetKind::UpNext) || self.editing {
            self.agenda = agenda::load_agenda();
        }
    }

    fn apply_output_info(&mut self, wl_output: &WlOutput, info: &OutputInfo) {
        let mut newly_named: Option<(String, (u32, u32))> = None;
        if let Some(entry) = self.outputs.iter_mut().find(|o| &o.wl_output == wl_output) {
            if let Some(name) = info.name.as_ref() {
                entry.name = Some(name.clone());
            }
            if let Some((w, h)) = info.logical_size
                && w > 0
                && h > 0
            {
                entry.logical_size = (w as u32, h as u32);
            }
            if !self.layout_initialized
                && let Some(name) = entry.name.clone()
            {
                newly_named = Some((name, entry.logical_size));
            }
        }
        if let Some((name, size)) = newly_named {
            self.maybe_init_layout(&name, size);
        }
    }
}

impl cosmic::Application for DesktopWidgets {
    type Message = Message;
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = bool;
    const APP_ID: &'static str = "dev.asw.MacDesktopWidgets";

    fn init(mut core: app::Core, start_editing: Self::Flags) -> (Self, Task<Self::Message>) {
        core.set_app_type(cosmic::core::AppType::System);
        core.set_keyboard_nav(false);

        let mut cpu_prev = None;
        let stats = Stats::read(&mut cpu_prev);
        let path = layout_path();
        let (layout, layout_initialized) = match path.as_deref().and_then(DesktopWidgetsLayout::load_from) {
            Some(existing) => (existing, true),
            None => (DesktopWidgetsLayout::default(), false),
        };

        let mut app = Self {
            core,
            outputs: Vec::new(),
            layout,
            layout_initialized,
            editing: start_editing,
            gallery_size: WidgetSize::Medium,
            drag: None,
            context_menu: None,
            cursor_pos: HashMap::new(),
            normal_window_focused: false,
            now: Zoned::now(),
            widget_settings: WidgetSettings::load(),
            weather: WeatherState::Loading,
            stats,
            cpu_prev,
            agenda: agenda::load_agenda(),
        };
        let weather_task = app.refresh_weather();
        (app, weather_task)
    }

    fn core(&self) -> &app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut app::Core {
        &mut self.core
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::iced::theme::Style {
            background_color: Color::TRANSPARENT,
            icon_color: self.core.system_theme().cosmic().on_bg_color().into(),
            text_color: self.core.system_theme().cosmic().on_bg_color().into(),
        })
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        let mut subs = vec![
            listen_raw(|event, _status, id| match event {
                cosmic::iced::Event::PlatformSpecific(PlatformSpecific::Wayland(
                    wayland::Event::Output(evt, wl_output),
                )) => Some(Message::Output(evt, wl_output)),
                cosmic::iced::Event::Window(window::Event::Resized(size)) => {
                    Some(Message::Resized(id, size))
                }
                cosmic::iced::Event::Mouse(mouse::Event::CursorMoved { position }) => {
                    Some(Message::CursorMoved(id, position))
                }
                cosmic::iced::Event::Mouse(mouse::Event::ButtonReleased(button)) => {
                    Some(Message::MouseReleased(id, button))
                }
                cosmic::iced::Event::Keyboard(cosmic::iced::keyboard::Event::KeyPressed {
                    key,
                    ..
                }) => Some(Message::KeyPressed(key)),
                _ => None,
            }),
            ipc::subscription().map(Message::Ipc),
            focus_subscription().map(Message::Focus),
        ];

        if self.layout.has_kind(WidgetKind::System) || self.editing {
            subs.push(
                cosmic::iced::time::every(Duration::from_secs(2)).map(|_| Message::StatsTick),
            );
        }

        if self.layout.has_kind(WidgetKind::Clock) || self.layout.has_kind(WidgetKind::UpNext) || self.editing {
            let interval = if self.layout.widgets.iter().any(|w| w.kind == WidgetKind::Clock && w.size == WidgetSize::Medium) {
                Duration::from_secs(1)
            } else {
                Duration::from_secs(15)
            };
            subs.push(cosmic::iced::time::every(interval).map(|_| Message::ClockTick));
        }

        if self.layout.has_kind(WidgetKind::Weather) || self.editing {
            subs.push(
                cosmic::iced::time::every(Duration::from_secs(300)).map(|_| Message::WeatherTick),
            );
        }

        Subscription::batch(subs)
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::Output(evt, wl_output) => match evt {
                OutputEvent::Created(info) => {
                    if let Some(info) = info.as_ref()
                        && self.outputs.iter().any(|o| o.wl_output == wl_output)
                    {
                        self.apply_output_info(&wl_output, info);
                        return self.sync_surface_modes();
                    }
                    if self.outputs.iter().any(|o| o.wl_output == wl_output) {
                        return Task::none();
                    }

                    let surface_id = SurfaceId::unique();
                    let name = info.as_ref().and_then(|i| i.name.clone());
                    let logical_size = info
                        .as_ref()
                        .and_then(|i| i.logical_size)
                        .filter(|&(w, h)| w > 0 && h > 0)
                        .map(|(w, h)| (w as u32, h as u32))
                        .unwrap_or((1920, 1080));

                    self.outputs.push(OutputSurface {
                        wl_output: wl_output.clone(),
                        surface_id,
                        name: name.clone(),
                        logical_size,
                    });

                    if let Some(ref output_name) = name {
                        self.maybe_init_layout(output_name, logical_size);
                    }

                    let surface_task = get_layer_surface(SctkLayerSurfaceSettings {
                        id: surface_id,
                        layer: Layer::Bottom,
                        keyboard_interactivity: if self.editing {
                            KeyboardInteractivity::OnDemand
                        } else {
                            KeyboardInteractivity::None
                        },
                        anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                        output: IcedOutput::Output(wl_output),
                        namespace: "mac-desktop-widgets".into(),
                        exclusive_zone: -1,
                        size_limits: Limits::NONE,
                        input_zone: self.input_zone_for_output(name.as_deref()),
                        ..Default::default()
                    });
                    let weather_task = self.refresh_weather();
                    return Task::batch([surface_task, weather_task, self.sync_surface_modes()]);
                }
                OutputEvent::InfoUpdate(info) => {
                    self.apply_output_info(&wl_output, &info);
                    return Task::batch([self.sync_surface_modes(), self.refresh_weather()]);
                }
                OutputEvent::Removed => {
                    if let Some(idx) = self.outputs.iter().position(|o| o.wl_output == wl_output) {
                        let removed = self.outputs.remove(idx);
                        self.cursor_pos.remove(&removed.surface_id);
                        return destroy_layer_surface(removed.surface_id);
                    }
                }
            },
            Message::Resized(id, size) => {
                if let Some(o) = self.outputs.iter_mut().find(|o| o.surface_id == id) {
                    o.logical_size = (
                        size.width.round().max(1.0) as u32,
                        size.height.round().max(1.0) as u32,
                    );
                }
            }
            Message::CursorMoved(surface_id, pos) => {
                self.cursor_pos.insert(surface_id, pos);
                match &mut self.drag {
                    Some(DragState::PendingWidget {
                        index,
                        surface_id: drag_sid,
                        start,
                        orig_x,
                        orig_y,
                    }) if *drag_sid == surface_id => {
                        if pos.distance(*start) >= DRAG_THRESHOLD {
                            let offset_x = start.x - *orig_x as f32;
                            let offset_y = start.y - *orig_y as f32;
                            self.drag = Some(DragState::ActiveWidget {
                                index: *index,
                                surface_id,
                                offset_x,
                                offset_y,
                                current_x: pos.x - offset_x,
                                current_y: pos.y - offset_y,
                            });
                        }
                    }
                    Some(DragState::ActiveWidget {
                        surface_id: drag_sid,
                        offset_x,
                        offset_y,
                        current_x,
                        current_y,
                        ..
                    }) if *drag_sid == surface_id => {
                        *current_x = pos.x - *offset_x;
                        *current_y = pos.y - *offset_y;
                    }
                    Some(DragState::PendingGallery {
                        kind,
                        size,
                        surface_id: drag_sid,
                        start,
                    }) if *drag_sid == surface_id => {
                        if pos.distance(*start) >= DRAG_THRESHOLD {
                            self.drag = Some(DragState::ActiveGallery {
                                kind: *kind,
                                size: *size,
                                surface_id,
                                pos,
                            });
                        }
                    }
                    Some(DragState::ActiveGallery {
                        surface_id: drag_sid,
                        pos: drag_pos,
                        ..
                    }) if *drag_sid == surface_id => {
                        *drag_pos = pos;
                    }
                    _ => {}
                }
            }
            Message::WidgetPress(surface_id, index) => {
                if self.context_menu.is_some() {
                    self.context_menu = None;
                    return self.sync_surface_modes();
                }
                if let Some(w) = self.layout.widgets.get(index) {
                    let start = self
                        .cursor_pos
                        .get(&surface_id)
                        .copied()
                        .unwrap_or(Point::new(w.x as f32, w.y as f32));
                    self.drag = Some(DragState::PendingWidget {
                        index,
                        surface_id,
                        start,
                        orig_x: w.x,
                        orig_y: w.y,
                    });
                }
            }
            Message::WidgetRightPress(surface_id, index) => {
                self.drag = None;
                let (out_w, out_h) = self
                    .output_by_surface(surface_id)
                    .map(|o| o.logical_size)
                    .unwrap_or((1920, 1080));
                let cursor = self
                    .cursor_pos
                    .get(&surface_id)
                    .copied()
                    .unwrap_or(Point::new(100.0, 100.0));
                let x = cursor.x.clamp(12.0, (out_w as f32 - CONTEXT_MENU_WIDTH - 12.0).max(12.0));
                let y = cursor.y.clamp(12.0, (out_h as f32 - CONTEXT_MENU_HEIGHT - 12.0).max(12.0));
                self.context_menu = Some(ContextMenu {
                    index,
                    surface_id,
                    x,
                    y,
                });
                return self.sync_surface_modes();
            }
            Message::GalleryPress(surface_id, kind) => {
                self.context_menu = None;
                let start = self
                    .cursor_pos
                    .get(&surface_id)
                    .copied()
                    .unwrap_or(Point::new(0.0, 0.0));
                self.drag = Some(DragState::PendingGallery {
                    kind,
                    size: self.gallery_size,
                    surface_id,
                    start,
                });
            }
            Message::MouseReleased(_surface_id, mouse::Button::Left) => {
                let Some(drag) = self.drag.take() else {
                    return Task::none();
                };
                match drag {
                    DragState::ActiveWidget {
                        index,
                        surface_id,
                        current_x,
                        current_y,
                        ..
                    } => {
                        let (out_w, out_h) = self
                            .output_by_surface(surface_id)
                            .map(|o| o.logical_size)
                            .unwrap_or((1920, 1080));
                        if self.layout.move_widget(
                            index,
                            current_x.round() as i32,
                            current_y.round() as i32,
                            out_w,
                            out_h,
                        ) {
                            self.save_layout();
                            return self.sync_input_zones();
                        }
                    }
                    DragState::PendingWidget { .. } => {}
                    DragState::ActiveGallery {
                        kind,
                        size,
                        surface_id,
                        pos,
                    } => {
                        let out_name = self.output_name_or_fallback(surface_id);
                        let (out_w, out_h) = self.output_size(&out_name);
                        if pos.y < out_h as f32 - GALLERY_STRIP_CLEARANCE {
                            let (w, h) = size.dimensions();
                            let x = pos.x.round() as i32 - (w as i32 / 2);
                            let y = pos.y.round() as i32 - (h as i32 / 2);
                            self.layout.add_widget(kind, size, out_name, x, y, (out_w, out_h));
                            self.save_layout();
                            self.refresh_stats_if_visible();
                            self.refresh_agenda_if_visible();
                            return Task::batch([self.sync_input_zones(), self.refresh_weather()]);
                        }
                    }
                    DragState::PendingGallery { kind, surface_id, .. } => {
                        let out_name = self.output_name_or_fallback(surface_id);
                        let (out_w, out_h) = self.output_size(&out_name);
                        let (x, y) =
                            self.layout
                                .next_free_slot(&out_name, self.gallery_size, out_w, out_h);
                        self.layout
                            .add_widget(kind, self.gallery_size, out_name, x, y, (out_w, out_h));
                        self.save_layout();
                        self.refresh_stats_if_visible();
                        self.refresh_agenda_if_visible();
                        return Task::batch([self.sync_input_zones(), self.refresh_weather()]);
                    }
                }
            }
            Message::MouseReleased(_, _) => {}
            Message::KeyPressed(Key::Named(Named::Escape)) => {
                if self.context_menu.take().is_some() {
                    return self.sync_surface_modes();
                }
                if self.editing {
                    self.editing = false;
                    self.drag = None;
                    return self.sync_surface_modes();
                }
            }
            Message::KeyPressed(_) => {}
            Message::SelectGallerySize(size) => {
                self.gallery_size = size;
            }
            Message::ResizeWidget(index, size) => {
                self.context_menu = None;
                if let Some(w) = self.layout.widgets.get(index) {
                    let (out_w, out_h) = self.output_size(&w.output);
                    if self.layout.resize_widget(index, size, out_w, out_h) {
                        self.save_layout();
                    }
                }
                return self.sync_surface_modes();
            }
            Message::RemoveWidget(index) => {
                self.context_menu = None;
                if self.layout.remove_widget(index) {
                    self.save_layout();
                }
                return self.sync_surface_modes();
            }
            Message::SetEditing(editing) => {
                self.editing = editing;
                self.context_menu = None;
                self.drag = None;
                if editing {
                    self.refresh_stats_if_visible();
                    self.refresh_agenda_if_visible();
                    return Task::batch([self.sync_surface_modes(), self.refresh_weather()]);
                }
                return self.sync_surface_modes();
            }
            Message::CloseContextMenu => {
                if self.context_menu.take().is_some() {
                    return self.sync_surface_modes();
                }
            }
            Message::Focus(FocusEvent::NormalWindowFocused(focused)) => {
                self.normal_window_focused = focused;
            }
            Message::Ipc(IpcCommand::Edit) => {
                return self.update(Message::SetEditing(!self.editing));
            }
            Message::Ipc(IpcCommand::Quit) => {
                return cosmic::iced::exit();
            }
            Message::StatsTick => {
                self.refresh_stats_if_visible();
            }
            Message::ClockTick => {
                self.now = Zoned::now();
                self.refresh_agenda_if_visible();
            }
            Message::WeatherTick => {
                return self.refresh_weather();
            }
            Message::WeatherLoaded(res) => {
                self.weather = match res {
                    Ok(f) => WeatherState::Ready(f),
                    Err(err) => {
                        tracing::warn!(err, "Weather fetch failed");
                        if matches!(self.weather, WeatherState::Ready(_)) {
                            self.weather.clone()
                        } else {
                            WeatherState::Failed
                        }
                    }
                };
            }
            Message::Noop => {}
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Self::Message> {
        space::vertical().height(Length::Fixed(1.0)).into()
    }

    fn view_window(&self, id: SurfaceId) -> Element<'_, Self::Message> {
        let Some(output) = self.output_by_surface(id) else {
            return space::vertical().height(Length::Fixed(1.0)).into();
        };
        let Some(output_name) = output.name.as_deref() else {
            return space::vertical().height(Length::Fixed(1.0)).into();
        };
        let (out_w, out_h) = output.logical_size;
        let dimmed = self.normal_window_focused && !self.editing;
        let data = WidgetData {
            now: &self.now,
            weather: &self.weather,
            units: self.widget_settings.units,
            stats: &self.stats,
            agenda: &self.agenda,
        };

        let mut layers: Vec<Element<'_, Message>> = Vec::new();

        // Invisible backdrop when context menu is open so clicking outside closes it.
        if self.context_menu.is_some() {
            layers.push(
                mouse_area(space::horizontal().width(Length::Fill).height(Length::Fill))
                    .on_press(Message::CloseContextMenu)
                    .into(),
            );
        }

        // Placed widgets for this output.
        for (idx, placement) in self.layout.widgets.iter().enumerate() {
            if placement.output != output_name {
                continue;
            }
            let (w_px, h_px) = placement.size.dimensions();
            let (render_x, render_y) = match &self.drag {
                Some(DragState::ActiveWidget {
                    index,
                    surface_id,
                    current_x,
                    current_y,
                    ..
                }) if *index == idx && *surface_id == id => {
                    let (snap_x, snap_y) = clamp_and_snap(
                        current_x.round() as i32,
                        current_y.round() as i32,
                        placement.size,
                        out_w,
                        out_h,
                    );
                    // Subtle snap target ghost on the 20 px grid.
                    let ghost = container(space::horizontal())
                        .width(Length::Fixed(w_px as f32))
                        .height(Length::Fixed(h_px as f32))
                        .class(Container::custom(|theme| {
                            let accent = Color::from(theme.cosmic().accent_color());
                            cosmic::iced::widget::container::Style {
                                background: Some(Background::Color(Color { a: 0.14, ..accent })),
                                border: Border {
                                    radius: 20.0.into(),
                                    width: 1.5,
                                    color: Color { a: 0.55, ..accent },
                                },
                                ..Default::default()
                            }
                        }));
                    layers.push(pin(ghost).x(snap_x as f32).y(snap_y as f32).into());
                    (
                        current_x.clamp(0.0, out_w.saturating_sub(w_px) as f32),
                        current_y.clamp(0.0, out_h.saturating_sub(h_px) as f32),
                    )
                }
                _ => (placement.x as f32, placement.y as f32),
            };

            let card = render_widget(placement.kind, placement.size, data, dimmed, self.editing);
            let interactive = mouse_area(card)
                .on_press(Message::WidgetPress(id, idx))
                .on_right_press(Message::WidgetRightPress(id, idx));

            let widget_elem: Element<'_, Message> = if self.editing {
                stack![
                    interactive,
                    pin(remove_badge_button(Message::RemoveWidget(idx)))
                        .x(6.0)
                        .y(6.0),
                ]
                .into()
            } else {
                interactive.into()
            };

            layers.push(pin(widget_elem).x(render_x).y(render_y).into());
        }

        // Gallery strip at the bottom when in "Edit Widgets…" mode.
        if self.editing {
            let size_pills = WidgetSize::ALL.into_iter().fold(row![].spacing(6), |r, sz| {
                let selected = sz == self.gallery_size;
                r.push(
                    button::custom(text::caption(sz.label()))
                        .padding([4, 10])
                        .class(Button::Custom {
                            active: Box::new(move |_, theme| {
                                let c = theme.cosmic();
                                let fg = Color::from(c.on_bg_color());
                                let accent = Color::from(c.accent_color());
                                button::Style {
                                    background: Some(Background::Color(if selected {
                                        accent
                                    } else {
                                        Color { a: 0.10, ..fg }
                                    })),
                                    text_color: Some(if selected {
                                        Color::from_rgb(0.98, 0.99, 1.0)
                                    } else {
                                        fg
                                    }),
                                    border_radius: 10.0.into(),
                                    ..Default::default()
                                }
                            }),
                            hovered: Box::new(move |_, theme| {
                                let c = theme.cosmic();
                                let fg = Color::from(c.on_bg_color());
                                let accent = Color::from(c.accent_color());
                                button::Style {
                                    background: Some(Background::Color(if selected {
                                        accent
                                    } else {
                                        Color { a: 0.18, ..fg }
                                    })),
                                    text_color: Some(if selected {
                                        Color::from_rgb(0.98, 0.99, 1.0)
                                    } else {
                                        fg
                                    }),
                                    border_radius: 10.0.into(),
                                    ..Default::default()
                                }
                            }),
                            pressed: Box::new(|_, theme| button::Style {
                                background: Some(Background::Color(Color::from(
                                    theme.cosmic().accent_color(),
                                ))),
                                text_color: Some(Color::from_rgb(0.98, 0.99, 1.0)),
                                border_radius: 10.0.into(),
                                ..Default::default()
                            }),
                            disabled: Box::new(|_| button::Style::new()),
                        })
                        .on_press(Message::SelectGallerySize(sz)),
                )
            });

            let header = row![
                text::heading("Edit Widgets").width(Length::Fill),
                size_pills,
                button::suggested("Done").on_press(Message::SetEditing(false)),
            ]
            .spacing(12)
            .align_y(Alignment::Center);

            let tiles = WidgetKind::ALL.into_iter().fold(row![].spacing(10), |r, kind| {
                r.push(
                    mouse_area(gallery_tile(kind, self.gallery_size))
                        .on_press(Message::GalleryPress(id, kind)),
                )
            });

            let strip = container(
                column![
                    header,
                    tiles,
                    text::caption("Click or drag a widget to place • Drag widgets to move (20 px grid)"),
                ]
                .spacing(10)
                .align_x(Alignment::Center),
            )
            .padding([12, 18])
            .class(frosted_chrome_class());

            let strip_w = 540.0_f32.min((out_w as f32 - 32.0).max(200.0));
            let strip_x = ((out_w as f32 - strip_w) / 2.0).max(16.0);
            let strip_y = (out_h as f32 - 196.0).max(20.0);
            layers.push(
                pin(container(strip).width(Length::Fixed(strip_w)))
                    .x(strip_x)
                    .y(strip_y)
                    .into(),
            );

            // Floating preview while dragging a widget out of the gallery.
            if let Some(DragState::ActiveGallery {
                kind,
                size,
                surface_id,
                pos,
            }) = &self.drag
                && *surface_id == id
            {
                let (w_px, h_px) = size.dimensions();
                let preview = render_widget(*kind, *size, data, false, true);
                let px = (pos.x - w_px as f32 / 2.0).clamp(0.0, out_w.saturating_sub(w_px) as f32);
                let py = (pos.y - h_px as f32 / 2.0).clamp(0.0, out_h.saturating_sub(h_px) as f32);
                layers.push(pin(preview).x(px).y(py).into());
            }
        }

        // Right-click context menu on a widget.
        if let Some(menu) = &self.context_menu
            && menu.surface_id == id
            && let Some(target) = self.layout.widgets.get(menu.index)
        {
            let mut items = column![].spacing(2);
            for sz in WidgetSize::ALL {
                let prefix = if target.size == sz { "✓ " } else { "   " };
                items = items.push(
                    button::text(format!("{prefix}{}", sz.label()))
                        .width(Length::Fill)
                        .on_press(Message::ResizeWidget(menu.index, sz)),
                );
            }
            items = items
                .push(divider::horizontal::default())
                .push(
                    button::text("Edit Widgets…")
                        .width(Length::Fill)
                        .on_press(Message::SetEditing(true)),
                )
                .push(
                    button::destructive("Remove")
                        .width(Length::Fill)
                        .on_press(Message::RemoveWidget(menu.index)),
                );

            let menu_box = container(items)
                .width(Length::Fixed(CONTEXT_MENU_WIDTH))
                .padding(8)
                .class(frosted_chrome_class());

            // Absorb clicks inside the menu so backdrop doesn't swallow them.
            layers.push(
                pin(mouse_area(menu_box).on_press(Message::Noop))
                    .x(menu.x)
                    .y(menu.y)
                    .into(),
            );
        }

        stack(layers)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}
