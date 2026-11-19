// SPDX-License-Identifier: GPL-3.0-only

//! Red "● MM:SS   ■ Stop" pill shown top-right on the overlay layer while
//! screen recording is active.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::platform_specific::runtime::wayland::layer_surface::{
    IcedMargin, IcedOutput, SctkLayerSurfaceSettings,
};
use cosmic::iced::platform_specific::shell::wayland::commands::layer_surface::{
    Anchor, KeyboardInteractivity, Layer, destroy_layer_surface, get_layer_surface,
};
use cosmic::iced::runtime::core::layout::Limits;
use cosmic::iced::runtime::core::window::Id as SurfaceId;
use cosmic::iced::stream;
use cosmic::iced::widget::container;
use cosmic::iced::{Background, Border, Color, Length, Subscription};
use cosmic::theme::Container;
use cosmic::widget::{button, space::vertical as vertical_space, text};
use cosmic::{Element, app};

use crate::output::Error;
use crate::record::Recorder;

type Task = app::Task<Message>;

const PILL_WIDTH: u32 = 168;
const PILL_HEIGHT: u32 = 36;

#[derive(Clone)]
pub struct Flags {
    pub socket_path: Option<PathBuf>,
    pub recorder: Arc<Mutex<Option<Recorder>>>,
}

/// Format elapsed seconds as `"● MM:SS   ■ Stop"`.
pub fn format_elapsed(secs: u64) -> String {
    let mins = secs / 60;
    let rem = secs % 60;
    format!("● {mins:02}:{rem:02}   ■ Stop")
}

/// Run the stop pill until the user clicks it, a second invocation sends
/// `"stop"` over `socket_path`, or the recorder exits on its own.
pub fn run(socket_path: Option<PathBuf>, recorder: Recorder) -> Result<PathBuf, Error> {
    let shared = Arc::new(Mutex::new(Some(recorder)));
    let flags = Flags { socket_path: socket_path.clone(), recorder: shared.clone() };
    let settings = app::Settings::default()
        .antialiasing(true)
        .client_decorations(false)
        .scale_factor(1.0)
        .no_main_window(true)
        .exit_on_close(true);
    if let Err(e) = app::run::<RecordingPill>(settings, flags) {
        tracing::error!("recording pill failed: {e}");
    }
    if let Some(ref path) = socket_path {
        let _ = std::fs::remove_file(path);
    }
    let rec = shared
        .lock()
        .ok()
        .and_then(|mut g| g.take())
        .ok_or_else(|| Error::Msg("recorder missing".into()))?;
    rec.stop()
}

#[derive(Clone, Debug)]
pub enum Message {
    Tick,
    Stop,
}

pub struct RecordingPill {
    core: app::Core,
    id: SurfaceId,
    started_at: Instant,
    elapsed_secs: u64,
    socket_path: Option<PathBuf>,
    recorder: Arc<Mutex<Option<Recorder>>>,
    closing: bool,
}

impl RecordingPill {
    fn close(&mut self) -> Task {
        if self.closing {
            return Task::none();
        }
        self.closing = true;
        if let Some(ref path) = self.socket_path {
            let _ = std::fs::remove_file(path);
        }
        Task::batch([destroy_layer_surface(self.id), cosmic::iced::exit()])
    }
}

impl cosmic::Application for RecordingPill {
    type Message = Message;
    type Executor = cosmic::executor::single::Executor;
    type Flags = Flags;
    const APP_ID: &'static str = "dev.asw.MacScreenshot.Recording";

    fn init(mut core: app::Core, flags: Flags) -> (Self, Task) {
        core.set_app_type(cosmic::core::AppType::System);
        core.set_keyboard_nav(false);

        let id = SurfaceId::unique();
        let app = Self {
            core,
            id,
            started_at: Instant::now(),
            elapsed_secs: 0,
            socket_path: flags.socket_path,
            recorder: flags.recorder,
            closing: false,
        };
        let surface = get_layer_surface(SctkLayerSurfaceSettings {
            id,
            layer: Layer::Overlay,
            keyboard_interactivity: KeyboardInteractivity::None,
            anchor: Anchor::TOP | Anchor::RIGHT,
            output: IcedOutput::Active,
            namespace: "mac-screenshot-recording".into(),
            margin: IcedMargin { top: 16, right: 20, bottom: 0, left: 0 },
            size: Some((Some(PILL_WIDTH), Some(PILL_HEIGHT))),
            size_limits: Limits::NONE,
            exclusive_zone: -1,
            ..Default::default()
        });
        (app, surface)
    }

    fn core(&self) -> &app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut app::Core {
        &mut self.core
    }

    fn update(&mut self, message: Message) -> Task {
        match message {
            Message::Tick => {
                self.elapsed_secs = self.started_at.elapsed().as_secs();
                let exited = self
                    .recorder
                    .lock()
                    .ok()
                    .and_then(|mut g| g.as_mut().and_then(Recorder::try_wait))
                    .is_some();
                if exited {
                    return self.close();
                }
            }
            Message::Stop => return self.close(),
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        unreachable!("layer-shell recording pill uses view_window")
    }

    fn view_window(&self, id: SurfaceId) -> Element<'_, Message> {
        if id != self.id {
            return vertical_space().height(Length::Fixed(1.0)).into();
        }
        let label = text::body(format_elapsed(self.elapsed_secs))
            .class(cosmic::theme::Text::Color(Color::WHITE));
        let pill = container(label)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Horizontal::Center)
            .align_y(Vertical::Center)
            .class(Container::custom(|_| container::Style {
                background: Some(Background::Color(Color::from_rgb(0.86, 0.16, 0.16))),
                text_color: Some(Color::WHITE),
                border: Border {
                    radius: 18.0.into(),
                    width: 1.0,
                    color: Color::from_rgba(1.0, 1.0, 1.0, 0.22),
                },
                ..Default::default()
            }));
        button::custom(pill)
            .padding(0)
            .width(Length::Fill)
            .height(Length::Fill)
            .on_press(Message::Stop)
            .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        let tick = cosmic::iced::time::every(Duration::from_millis(250)).map(|_| Message::Tick);
        let Some(ref path) = self.socket_path else {
            return tick;
        };
        Subscription::batch([
            tick,
            Subscription::run_with(path.clone(), |p| socket_stream(p)),
        ])
    }
}

fn socket_stream(path: &Path) -> impl cosmic::iced::futures::Stream<Item = Message> + use<> {
    let path = path.to_path_buf();
    stream::channel(
        4,
        |mut tx: cosmic::iced::futures::channel::mpsc::Sender<Message>| async move {
            use tokio::io::{AsyncBufReadExt, BufReader};

            let _ = std::fs::remove_file(&path);
            let listener = match tokio::net::UnixListener::bind(&path) {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!("cannot bind control socket {}: {e}", path.display());
                    return;
                }
            };
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                let mut line = String::new();
                if BufReader::new(stream).read_line(&mut line).await.is_ok()
                    && line.trim().eq_ignore_ascii_case("stop")
                {
                    let _ = tx.try_send(Message::Stop);
                    break;
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_stop_pill_elapsed_time() {
        assert_eq!(format_elapsed(0), "● 00:00   ■ Stop");
        assert_eq!(format_elapsed(12), "● 00:12   ■ Stop");
        assert_eq!(format_elapsed(65), "● 01:05   ■ Stop");
        assert_eq!(format_elapsed(3601), "● 60:01   ■ Stop");
    }
}
