// SPDX-License-Identifier: GPL-3.0-only

//! Floating thumbnail shown bottom-right after a capture (macOS style).

use std::path::PathBuf;
use std::time::Duration;

use cosmic::iced::alignment::Horizontal;
use cosmic::iced::platform_specific::runtime::wayland::layer_surface::{
    IcedMargin, IcedOutput, SctkLayerSurfaceSettings,
};
use cosmic::iced::platform_specific::shell::wayland::commands::layer_surface::{
    Anchor, KeyboardInteractivity, Layer, get_layer_surface,
};
use cosmic::iced::runtime::core::layout::Limits;
use cosmic::iced::runtime::core::window::Id as SurfaceId;
use cosmic::iced::widget::{column, container, image, row};
use cosmic::iced::{Background, Border, Color, ContentFit, Length};
use cosmic::theme::{Button, Container};
use cosmic::widget::{button, mouse_area, space::vertical as vertical_space, text};
use cosmic::{Element, app};

use crate::output;

type Task = app::Task<Message>;

const DISMISS_AFTER: Duration = Duration::from_secs(5);
const THUMB_WIDTH: u32 = 280;
const THUMB_MAX_HEIGHT: u32 = 180;
const CHROME_HEIGHT: u32 = 96;

#[derive(Clone)]
pub struct Flags {
    pub path: PathBuf,
}

pub fn run(path: PathBuf) {
    let settings = app::Settings::default()
        .antialiasing(true)
        .client_decorations(false)
        .scale_factor(1.0)
        .no_main_window(true)
        .exit_on_close(true);
    if let Err(e) = app::run::<Thumbnail>(settings, Flags { path }) {
        tracing::error!("thumbnail failed: {e}");
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    Open,
    Copy,
    ShowInFiles,
    Delete,
    Hover(bool),
    Tick,
}

pub struct Thumbnail {
    core: app::Core,
    id: SurfaceId,
    path: PathBuf,
    handle: Option<image::Handle>,
    hovered: bool,
}

/// Fit `(w, h)` into the thumbnail box, never upscaling.
pub fn thumb_size(w: u32, h: u32) -> (u32, u32) {
    if w == 0 || h == 0 {
        return (THUMB_WIDTH, THUMB_MAX_HEIGHT);
    }
    let scale = (THUMB_WIDTH as f32 / w as f32)
        .min(THUMB_MAX_HEIGHT as f32 / h as f32)
        .min(1.0);
    (((w as f32 * scale).round() as u32).max(1), ((h as f32 * scale).round() as u32).max(1))
}

fn dismiss_task() -> Task {
    Task::perform(
        async {
            tokio::time::sleep(DISMISS_AFTER).await;
        },
        |()| cosmic::Action::App(Message::Tick),
    )
}

impl cosmic::Application for Thumbnail {
    type Message = Message;
    type Executor = cosmic::executor::single::Executor;
    type Flags = Flags;
    const APP_ID: &'static str = "dev.asw.MacScreenshot.Thumbnail";

    fn init(mut core: app::Core, flags: Flags) -> (Self, Task) {
        core.set_app_type(cosmic::core::AppType::System);
        core.set_keyboard_nav(false);

        let decoded = ::image::open(&flags.path).ok().map(|i| i.to_rgba8());
        let (handle, (tw, th)) = match decoded {
            Some(img) => {
                let (w, h) = thumb_size(img.width(), img.height());
                let small = ::image::imageops::thumbnail(&img, w, h);
                (Some(image::Handle::from_rgba(w, h, small.into_raw())), (w, h))
            }
            None => (None, (THUMB_WIDTH, THUMB_MAX_HEIGHT)),
        };
        let _ = tw;
        let id = SurfaceId::unique();
        let app = Self { core, id, path: flags.path, handle, hovered: false };
        let surface = get_layer_surface(SctkLayerSurfaceSettings {
            id,
            layer: Layer::Overlay,
            keyboard_interactivity: KeyboardInteractivity::None,
            anchor: Anchor::BOTTOM | Anchor::RIGHT,
            output: IcedOutput::Active,
            namespace: "mac-screenshot-thumbnail".into(),
            margin: IcedMargin { top: 0, right: 20, bottom: 20, left: 0 },
            size: Some((Some(THUMB_WIDTH + 24), Some(th + CHROME_HEIGHT))),
            size_limits: Limits::NONE,
            exclusive_zone: -1,
            ..Default::default()
        });
        (app, Task::batch([surface, dismiss_task()]))
    }

    fn core(&self) -> &app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut app::Core {
        &mut self.core
    }

    fn update(&mut self, message: Message) -> Task {
        match message {
            Message::Open => {
                output::xdg_open(&self.path);
                return cosmic::iced::exit();
            }
            Message::Copy => {
                if let Err(e) = output::copy_file(&self.path) {
                    tracing::error!("copy failed: {e}");
                }
                return cosmic::iced::exit();
            }
            Message::ShowInFiles => {
                output::show_in_files(&self.path);
                return cosmic::iced::exit();
            }
            Message::Delete => {
                if let Err(e) = std::fs::remove_file(&self.path) {
                    tracing::error!("delete failed: {e}");
                }
                return cosmic::iced::exit();
            }
            Message::Hover(h) => self.hovered = h,
            Message::Tick => {
                // Stay while the pointer is over the thumbnail.
                return if self.hovered { dismiss_task() } else { cosmic::iced::exit() };
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        unreachable!("layer-shell overlay uses view_window")
    }

    fn view_window(&self, id: SurfaceId) -> Element<'_, Message> {
        if id != self.id {
            return vertical_space().height(Length::Fixed(1.0)).into();
        }
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let preview: Element<'_, Message> = match &self.handle {
            Some(h) => image(h.clone()).content_fit(ContentFit::ScaleDown).into(),
            None => text::body("Preview unavailable").into(),
        };
        let body = column![
            button::custom(container(preview).center_x(Length::Fill))
                .class(Button::Image)
                .padding(0)
                .on_press(Message::Open),
            text::caption(name),
            row![
                button::standard("Copy").on_press(Message::Copy),
                button::standard("Show in Files").on_press(Message::ShowInFiles),
                button::destructive("Delete").on_press(Message::Delete),
            ]
            .spacing(6),
        ]
        .spacing(6)
        .align_x(Horizontal::Center);

        let card = container(body)
            .padding(10)
            .width(Length::Fill)
            .height(Length::Fill)
            .class(Container::custom(|t| {
                let c = t.cosmic();
                cosmic::iced::widget::container::Style {
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
        mouse_area(card)
            .on_enter(Message::Hover(true))
            .on_exit(Message::Hover(false))
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumb_fits_box_without_upscaling() {
        assert_eq!(thumb_size(1920, 1080), (280, 158));
        assert_eq!(thumb_size(100, 50), (100, 50));
        assert_eq!(thumb_size(500, 2000), (45, 180));
        assert_eq!(thumb_size(0, 0), (THUMB_WIDTH, THUMB_MAX_HEIGHT));
    }
}
