// SPDX-License-Identifier: GPL-3.0-only

//! Full-screen capture overlay: frozen, dimmed screen, crosshair, region or
//! window selection and the macOS-style floating toolbar.
//!
//! One layer-shell surface is created per output; the user picks the output
//! simply by working on it.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cctk::wayland_client::protocol::wl_output::WlOutput;
use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::event::{Status, listen_raw};
use cosmic::iced::keyboard::{Key, key::Named};
use cosmic::iced::mouse::Interaction;
use cosmic::iced::platform_specific::runtime::wayland::layer_surface::{
    IcedOutput, SctkLayerSurfaceSettings,
};
use cosmic::iced::platform_specific::shell::wayland::commands::layer_surface::{
    Anchor, KeyboardInteractivity, Layer, destroy_layer_surface, get_layer_surface,
};
use cosmic::iced::runtime::core::event::{PlatformSpecific, wayland};
use cosmic::iced::runtime::core::layout::Limits;
use cosmic::iced::runtime::core::window::Id as SurfaceId;
use cosmic::iced::widget::{column, container, image, opaque, pin, row, stack};
use cosmic::iced::{Background, Border, Color, ContentFit, Length, Point, Subscription};
use cosmic::theme::Container;
use cosmic::widget::{button, mouse_area, space::vertical as vertical_space, text};
use cosmic::{Element, app};

use crate::config::{AudioSource, Mode, Options, SaveTo, Timer};
use crate::flow::Request;
use crate::geometry::{Rect, window_at};
use crate::wayland::Snapshot;

type Task = app::Task<Message>;

/// Selections smaller than this (logical px) are treated as accidental clicks.
const MIN_SELECTION: f32 = 4.0;
/// How long to wait for per-output events before falling back to one
/// surface on the active output.
const OUTPUT_WAIT: Duration = Duration::from_millis(1500);

/// Result of the overlay session.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Screenshot(Request),
    Record(Request),
}

/// Result handed back to `main` once the event loop ends.
static RESULT: Mutex<Option<Outcome>> = Mutex::new(None);

#[derive(Clone)]
pub struct Flags {
    pub snapshot: Arc<Snapshot>,
    pub options: Options,
    pub mode: Mode,
    /// `false` for `--region` / `--window`: no toolbar, capture right away.
    pub toolbar: bool,
}

/// Run the overlay; `None` means cancelled.
pub fn run(flags: Flags) -> Option<Outcome> {
    let settings = app::Settings::default()
        .antialiasing(false)
        .client_decorations(false)
        .scale_factor(1.0)
        .no_main_window(true)
        .exit_on_close(true);
    if let Err(e) = app::run::<Overlay>(settings, flags) {
        tracing::error!("overlay failed: {e}");
    }
    RESULT.lock().ok().and_then(|mut r| r.take())
}

#[derive(Clone, Debug)]
pub enum Message {
    Output(Box<wayland::OutputEvent>, WlOutput),
    FallbackSurface,
    Moved(SurfaceId, Point),
    Pressed(SurfaceId),
    Released,
    Key(Key),
    SetMode(Mode),
    SetRecordMode(Mode),
    ToggleOptions,
    SetSaveTo(SaveTo),
    SetTimer(Timer),
    SetAudio(AudioSource),
    TogglePointer,
    Confirm,
    Cancel,
}

struct Drag {
    surface: SurfaceId,
    start: (f32, f32),
}

pub struct Overlay {
    core: app::Core,
    snapshot: Arc<Snapshot>,
    handles: Vec<image::Handle>,
    /// layer surface -> index into `snapshot.outputs`
    surfaces: HashMap<SurfaceId, usize>,
    created: HashSet<usize>,
    options: Options,
    mode: Mode,
    recording: bool,
    toolbar: bool,
    options_open: bool,
    /// Surface the pointer was last seen on (hosts the toolbar).
    active: Option<SurfaceId>,
    cursor: (f32, f32),
    drag: Option<Drag>,
    /// (output index, rect) of the region selection.
    selection: Option<(usize, Rect)>,
    /// (output index, rect) of the window under the pointer.
    hovered: Option<(usize, Rect)>,
    closing: bool,
}

/// The four rectangles that dim everything outside `hole`.
pub fn dim_blocks(hole: Rect, width: f32, height: f32) -> [Rect; 4] {
    let width = if width.is_finite() { width.max(0.0) } else { 0.0 };
    let height = if height.is_finite() { height.max(0.0) } else { 0.0 };
    let hole = hole.clamp_to(width, height);
    [
        Rect::new(0.0, 0.0, width, hole.y),
        Rect::new(0.0, hole.y + hole.h, width, (height - hole.y - hole.h).max(0.0)),
        Rect::new(0.0, hole.y, hole.x, hole.h),
        Rect::new(hole.x + hole.w, hole.y, (width - hole.x - hole.w).max(0.0), hole.h),
    ]
}

/// Which rectangle a confirm would capture.
pub fn pick_target(
    mode: Mode,
    selection: Option<(usize, Rect)>,
    hovered: Option<(usize, Rect)>,
    active_output: Option<usize>,
) -> Option<(usize, Option<Rect>)> {
    match mode {
        Mode::Region => selection
            .filter(|(_, r)| r.w >= MIN_SELECTION && r.h >= MIN_SELECTION)
            .map(|(o, r)| (o, Some(r))),
        Mode::Window => hovered.map(|(o, r)| (o, Some(r))),
        Mode::Screen => active_output.map(|o| (o, None)),
    }
}

fn fill(color: Color) -> Container<'static> {
    Container::custom(move |_| container::Style {
        background: Some(Background::Color(color)),
        ..Default::default()
    })
}

fn outline(color: Color, width: f32) -> Container<'static> {
    Container::custom(move |_| container::Style {
        border: Border { color, width, radius: 0.0.into() },
        ..Default::default()
    })
}

fn block<'a>(r: Rect, class: Container<'static>) -> Element<'a, Message> {
    pin(container(vertical_space()).width(r.w).height(r.h).class(class))
        .x(r.x)
        .y(r.y)
        .into()
}

impl Overlay {
    fn output_index(&self, name: Option<&str>, size: Option<(i32, i32)>) -> Option<usize> {
        let outputs = &self.snapshot.outputs;
        name.and_then(|n| outputs.iter().position(|o| o.name == n))
            .or_else(|| {
                size.and_then(|s| {
                    outputs
                        .iter()
                        .enumerate()
                        .position(|(i, o)| o.logical_size == s && !self.created.contains(&i))
                })
            })
    }

    fn open_surface(&mut self, index: usize, output: IcedOutput) -> Task {
        let id = SurfaceId::unique();
        self.surfaces.insert(id, index);
        self.created.insert(index);
        if self.active.is_none() {
            self.active = Some(id);
        }
        get_layer_surface(SctkLayerSurfaceSettings {
            id,
            layer: Layer::Overlay,
            keyboard_interactivity: KeyboardInteractivity::Exclusive,
            anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
            output,
            namespace: "mac-screenshot".into(),
            size: Some((None, None)),
            size_limits: Limits::NONE,
            exclusive_zone: -1,
            ..Default::default()
        })
    }

    fn active_output(&self) -> Option<usize> {
        self.active.and_then(|id| self.surfaces.get(&id).copied())
    }

    /// Space: region <-> window.
    fn toggle_window_mode(&mut self) {
        self.recording = false;
        self.mode = if self.mode == Mode::Window { Mode::Region } else { Mode::Window };
        self.reset_selection();
    }

    fn reset_selection(&mut self) {
        self.drag = None;
        self.selection = None;
        self.hovered = None;
    }

    fn finish(&mut self, result: Option<Outcome>) -> Task {
        if self.closing {
            return Task::none();
        }
        self.closing = true;
        if let Ok(mut r) = RESULT.lock() {
            *r = result;
        }
        let mut tasks: Vec<Task> = self.surfaces.keys().map(|id| destroy_layer_surface(*id)).collect();
        tasks.push(cosmic::iced::exit());
        Task::batch(tasks)
    }

    fn confirm(&mut self) -> Task {
        let Some((oi, rect)) = pick_target(self.mode, self.selection, self.hovered, self.active_output())
        else {
            return Task::none();
        };
        self.options.save();
        let request = Request { output: self.snapshot.outputs[oi].name.clone(), rect };
        let outcome = if self.recording {
            Outcome::Record(request)
        } else {
            Outcome::Screenshot(request)
        };
        self.finish(Some(outcome))
    }

    fn set_options(&mut self, f: impl FnOnce(&mut Options)) {
        f(&mut self.options);
        self.options.save();
    }

    fn toolbar_view(&self) -> Element<'_, Message> {
        let mode_button = |label: &'static str, mode: Mode, rec: bool| {
            let active = self.recording == rec && self.mode == mode;
            let b = if active { button::suggested(label) } else { button::standard(label) };
            b.on_press(if rec {
                Message::SetRecordMode(mode)
            } else {
                Message::SetMode(mode)
            })
        };
        let confirm_label = if self.recording { "● Start Recording" } else { "Capture" };
        let mut bar = column![
            row![
                mode_button("Entire Screen", Mode::Screen, false),
                mode_button("Window", Mode::Window, false),
                mode_button("Portion", Mode::Region, false),
                text::body(" "),
                mode_button("Record Entire Screen", Mode::Screen, true),
                mode_button("Record Selected Portion", Mode::Region, true),
                text::body(" "),
                if self.options_open {
                    button::suggested("Options")
                } else {
                    button::standard("Options")
                }
                .on_press(Message::ToggleOptions),
                button::suggested(confirm_label).on_press(Message::Confirm),
                button::text("Cancel").on_press(Message::Cancel),
            ]
            .spacing(6)
            .align_y(Vertical::Center),
        ]
        .spacing(10)
        .align_x(Horizontal::Center);

        if self.options_open {
            let pick = |selected: bool, label: &'static str, msg: Message| {
                let b = if selected { button::suggested(label) } else { button::standard(label) };
                b.on_press(msg)
            };
            let save_row = SaveTo::ALL.iter().fold(
                row![text::body("Save to").width(80)].spacing(6).align_y(Vertical::Center),
                |r, s| r.push(pick(self.options.save_to == *s, s.label(), Message::SetSaveTo(*s))),
            );
            let timer_row = Timer::ALL.iter().fold(
                row![text::body("Timer").width(80)].spacing(6).align_y(Vertical::Center),
                |r, t| r.push(pick(self.options.timer == *t, t.label(), Message::SetTimer(*t))),
            );
            let audio_row = AudioSource::ALL.iter().fold(
                row![text::body("Audio").width(80)].spacing(6).align_y(Vertical::Center),
                |r, a| r.push(pick(self.options.audio == *a, a.label(), Message::SetAudio(*a))),
            );
            let pointer_row = row![
                text::body("Pointer").width(80),
                pick(self.options.show_pointer, "Show Mouse Pointer", Message::TogglePointer),
            ]
            .spacing(6)
            .align_y(Vertical::Center);
            bar = bar.push(save_row).push(timer_row).push(audio_row).push(pointer_row);
        }

        let card = container(bar).padding(12).class(Container::custom(|t| {
            let c = t.cosmic();
            container::Style {
                background: Some(Background::Color(Color::from(c.bg_color()))),
                text_color: Some(c.on_bg_color().into()),
                border: Border {
                    radius: c.corner_radii.radius_m.into(),
                    width: 1.0,
                    color: c.bg_divider().into(),
                },
                ..Default::default()
            }
        }));
        container(opaque(card))
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Horizontal::Center)
            .align_y(Vertical::Bottom)
            .padding(48)
            .into()
    }

    fn surface_view(&self, id: SurfaceId, oi: usize) -> Element<'_, Message> {
        let shot = &self.snapshot.outputs[oi];
        let (w, h) = ((shot.logical_size.0 as f32).max(0.0), (shot.logical_size.1 as f32).max(0.0));
        let sx = shot.image.width() as f32 / w.max(1.0);
        let sy = shot.image.height() as f32 / h.max(1.0);
        let is_active = self.active == Some(id);

        let mut layers: Vec<Element<'_, Message>> = vec![
            image(self.handles[oi].clone())
                .width(Length::Fill)
                .height(Length::Fill)
                .content_fit(ContentFit::Fill)
                .into(),
        ];

        let dim = Color::from_rgba(0.0, 0.0, 0.0, 0.45);
        let hole = match self.mode {
            Mode::Region => self.selection.filter(|(o, _)| *o == oi).map(|(_, r)| r),
            Mode::Window => self.hovered.filter(|(o, _)| *o == oi).map(|(_, r)| r),
            Mode::Screen => is_active.then(|| Rect::new(0.0, 0.0, w, h)),
        }
        .map(|r| r.clamp_to(w, h));
        match hole {
            Some(hole) => {
                for r in dim_blocks(hole, w, h) {
                    layers.push(block(r, fill(dim)));
                }
                layers.push(block(hole, outline(Color::WHITE, 2.0)));
                if self.mode == Mode::Window {
                    layers.push(block(hole, fill(Color::from_rgba(0.2, 0.5, 1.0, 0.18))));
                }
                if self.mode != Mode::Screen {
                    let label = format!("{} × {}", (hole.w * sx).round(), (hole.h * sy).round());
                    let y = if hole.y >= 28.0 { hole.y - 26.0 } else { hole.y + 4.0 };
                    layers.push(
                        pin(container(
                            text::caption(label).class(cosmic::theme::Text::Color(Color::WHITE)),
                        )
                        .padding([2, 6])
                        .class(fill(Color::from_rgba(0.0, 0.0, 0.0, 0.7))))
                        .x(hole.x)
                        .y(y)
                        .into(),
                    );
                }
            }
            None => layers.push(block(Rect::new(0.0, 0.0, w, h), fill(dim))),
        }

        if is_active && self.mode == Mode::Region && self.selection.is_none() {
            let line = Color::from_rgba(1.0, 1.0, 1.0, 0.7);
            let cx = self.cursor.0.clamp(0.0, w);
            let cy = self.cursor.1.clamp(0.0, h);
            layers.push(block(Rect::new(cx, 0.0, 1.0, h), fill(line)));
            layers.push(block(Rect::new(0.0, cy, w, 1.0), fill(line)));
        }

        let area = mouse_area(stack(layers).width(Length::Fill).height(Length::Fill))
            .on_move(move |p| Message::Moved(id, p))
            .on_press(Message::Pressed(id))
            .on_release(Message::Released)
            .interaction(match self.mode {
                Mode::Region => Interaction::Crosshair,
                _ => Interaction::Pointer,
            });

        let mut top: Vec<Element<'_, Message>> = vec![area.into()];
        if self.toolbar && is_active {
            top.push(self.toolbar_view());
        }
        stack(top).width(Length::Fill).height(Length::Fill).into()
    }
}

impl cosmic::Application for Overlay {
    type Message = Message;
    type Executor = cosmic::executor::single::Executor;
    type Flags = Flags;
    const APP_ID: &'static str = "dev.asw.MacScreenshot";

    fn init(mut core: app::Core, flags: Flags) -> (Self, Task) {
        core.set_app_type(cosmic::core::AppType::System);
        core.set_keyboard_nav(false);
        // Full-screen surface showing a frozen capture: compositor blur behind
        // it is never visible and only costs GPU time every frame.
        core.set_auto_blur(Default::default());
        let handles = flags
            .snapshot
            .outputs
            .iter()
            .map(|o| image::Handle::from_rgba(o.image.width(), o.image.height(), o.image.as_raw().clone()))
            .collect();
        let app = Self {
            core,
            snapshot: flags.snapshot,
            handles,
            surfaces: HashMap::new(),
            created: HashSet::new(),
            options: flags.options,
            mode: flags.mode,
            recording: false,
            toolbar: flags.toolbar,
            options_open: false,
            active: None,
            cursor: (0.0, 0.0),
            drag: None,
            selection: None,
            hovered: None,
            closing: false,
        };
        let fallback = Task::perform(
            async {
                tokio::time::sleep(OUTPUT_WAIT).await;
            },
            |()| cosmic::Action::App(Message::FallbackSurface),
        );
        (app, fallback)
    }

    fn core(&self) -> &app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut app::Core {
        &mut self.core
    }

    fn update(&mut self, message: Message) -> Task {
        match message {
            Message::Output(ev, output) => {
                if let wayland::OutputEvent::Created(Some(info)) = *ev {
                    match self.output_index(info.name.as_deref(), info.logical_size) {
                        Some(i) if !self.created.contains(&i) => {
                            return self.open_surface(i, IcedOutput::Output(output));
                        }
                        _ => {}
                    }
                }
            }
            Message::FallbackSurface => {
                // No per-output events arrived: use the active output and
                // assume the first capture belongs to it.
                if self.surfaces.is_empty() && !self.snapshot.outputs.is_empty() {
                    tracing::warn!("no output events; falling back to the active output");
                    return self.open_surface(0, IcedOutput::Active);
                }
            }
            Message::Moved(id, p) => {
                self.active = Some(id);
                self.cursor = (p.x, p.y);
                let Some(&oi) = self.surfaces.get(&id) else { return Task::none() };
                if let Some(d) = self.drag.as_ref().filter(|d| d.surface == id) {
                    let s = &self.snapshot.outputs[oi].logical_size;
                    let r = Rect::from_points(d.start, self.cursor).clamp_to(s.0 as f32, s.1 as f32);
                    self.selection = Some((oi, r));
                }
                if self.mode == Mode::Window {
                    let windows = &self.snapshot.outputs[oi].windows;
                    let rects: Vec<Rect> = windows.iter().map(|w| w.rect).collect();
                    self.hovered = window_at(&rects, self.cursor).map(|i| (oi, rects[i]));
                }
            }
            Message::Pressed(id) => {
                self.active = Some(id);
                let Some(&oi) = self.surfaces.get(&id) else { return Task::none() };
                match self.mode {
                    Mode::Region => {
                        let inside = self
                            .selection
                            .is_some_and(|(o, r)| o == oi && r.contains(self.cursor) && self.drag.is_none());
                        if inside {
                            return self.confirm();
                        }
                        self.selection = None;
                        self.drag = Some(Drag { surface: id, start: self.cursor });
                    }
                    Mode::Window | Mode::Screen => return self.confirm(),
                }
            }
            Message::Released => {
                if self.drag.take().is_some() {
                    let small = self
                        .selection
                        .is_none_or(|(_, r)| r.w < MIN_SELECTION || r.h < MIN_SELECTION);
                    if small {
                        self.selection = None;
                    } else if !self.toolbar {
                        return self.confirm();
                    }
                }
            }
            Message::Key(key) => match key {
                Key::Named(Named::Escape) => {
                    if self.options_open {
                        self.options_open = false;
                    } else {
                        return self.finish(None);
                    }
                }
                Key::Named(Named::Enter) => return self.confirm(),
                Key::Character(ref c) if c.as_str() == " " => self.toggle_window_mode(),
                _ => {}
            },
            Message::SetMode(m) => {
                self.recording = false;
                if self.mode != m {
                    self.mode = m;
                    self.reset_selection();
                }
            }
            Message::SetRecordMode(m) => {
                self.recording = true;
                if self.mode != m {
                    self.mode = m;
                    self.reset_selection();
                }
                // Whole screen needs no selection: start right away.
                if m == Mode::Screen {
                    return self.confirm();
                }
            }
            Message::ToggleOptions => self.options_open = !self.options_open,
            Message::SetSaveTo(s) => self.set_options(|o| o.save_to = s),
            Message::SetTimer(t) => self.set_options(|o| o.timer = t),
            Message::SetAudio(a) => self.set_options(|o| o.audio = a),
            Message::TogglePointer => self.set_options(|o| o.show_pointer = !o.show_pointer),
            Message::Confirm => return self.confirm(),
            Message::Cancel => return self.finish(None),
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        unreachable!("layer-shell overlay uses view_window")
    }

    fn view_window(&self, id: SurfaceId) -> Element<'_, Message> {
        match self.surfaces.get(&id) {
            Some(&oi) => self.surface_view(id, oi),
            None => vertical_space().height(Length::Fixed(1.0)).into(),
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        listen_raw(|event, _status: Status, _id| match event {
            cosmic::iced::Event::PlatformSpecific(PlatformSpecific::Wayland(
                wayland::Event::Output(ev, output),
            )) => Some(Message::Output(Box::new(ev), output)),
            cosmic::iced::Event::Keyboard(cosmic::iced::keyboard::Event::KeyPressed {
                key, ..
            }) => Some(Message::Key(key)),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dim_blocks_surround_the_hole() {
        let b = dim_blocks(Rect::new(10.0, 20.0, 30.0, 40.0), 100.0, 100.0);
        assert_eq!(b[0], Rect::new(0.0, 0.0, 100.0, 20.0));
        assert_eq!(b[1], Rect::new(0.0, 60.0, 100.0, 40.0));
        assert_eq!(b[2], Rect::new(0.0, 20.0, 10.0, 40.0));
        assert_eq!(b[3], Rect::new(40.0, 20.0, 60.0, 40.0));
        let total: f32 = b.iter().map(Rect::area).sum::<f32>() + 30.0 * 40.0;
        assert_eq!(total, 100.0 * 100.0);
    }

    #[test]
    fn targets_follow_mode() {
        let r = Rect::new(0.0, 0.0, 50.0, 50.0);
        assert_eq!(pick_target(Mode::Region, Some((1, r)), None, Some(0)), Some((1, Some(r))));
        assert_eq!(pick_target(Mode::Region, Some((1, Rect::new(0.0, 0.0, 2.0, 2.0))), None, None), None);
        assert_eq!(pick_target(Mode::Window, Some((1, r)), None, Some(0)), None);
        assert_eq!(pick_target(Mode::Window, None, Some((2, r)), None), Some((2, Some(r))));
        assert_eq!(pick_target(Mode::Screen, None, None, Some(3)), Some((3, None)));
        assert_eq!(pick_target(Mode::Screen, None, None, None), None);
    }

    #[test]
    fn dim_blocks_handles_out_of_bounds_and_negative_viewport() {
        let b = dim_blocks(Rect::new(-10.0, -10.0, 50.0, 50.0), 100.0, 100.0);
        let total: f32 = b.iter().map(Rect::area).sum::<f32>() + 40.0 * 40.0;
        assert_eq!(total, 100.0 * 100.0);

        let neg = dim_blocks(Rect::new(10.0, 10.0, 20.0, 20.0), -10.0, 0.0);
        assert!(neg.iter().all(|r| r.w >= 0.0 && r.h >= 0.0 && r.area() == 0.0));
    }
}
