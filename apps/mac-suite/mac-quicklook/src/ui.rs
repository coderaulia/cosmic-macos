// SPDX-License-Identifier: GPL-3.0-only

//! The Quick Look window.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use cosmic::iced::event::listen_raw;
use cosmic::iced::keyboard::{Key, Modifiers, key::Named};
use cosmic::iced::widget::{rich_text, span, stack};
use cosmic::iced::{Alignment, Color, Length, Size, Subscription, stream, window};
use cosmic::widget::{button, column, container, icon, image, mouse_area, row, scrollable, svg, text};
use cosmic::{Application as _, Element, app};
use cosmic::app::Task;

use crate::content::{self, Body, HEADER_H, PDF_FOOTER_H, Preview};
use crate::files;
use crate::ipc;
use crate::kind::Kind;

const DEFAULT_SCREEN: (f32, f32) = (1920.0, 1080.0);

pub fn run(path: PathBuf) -> cosmic::iced::Result {
    let dark = is_dark();
    let preview = content::load(&path, dark);
    let (w, h) = content::window_size(&preview.body, DEFAULT_SCREEN);
    let settings = app::Settings::default()
        .antialiasing(true)
        .default_text_size(14.0)
        .scale_factor(1.0)
        .size(Size::new(w, h))
        .exit_on_close(true);
    app::run::<QuickLook>(settings, preview)
}

fn is_dark() -> bool {
    cosmic::theme::system_preference().cosmic().is_dark
}

struct QuickLook {
    core: app::Core,
    preview: Preview,
    screen: (f32, f32),
    /// Bumped on every navigation so stale async results are dropped.
    generation: u64,
}

#[derive(Debug, Clone)]
enum Message {
    Key(Key, Modifiers),
    Loaded(u64, Box<Preview>),
    PdfPage(u64, u32, Option<(image::Handle, f32, f32)>),
    Remote(PathBuf),
    Monitor(Option<Size>),
    Step(isize),
    Page(i32),
    OpenDefault,
    Close,
    Drag,
    Noop,
}

impl QuickLook {
    fn current(&self) -> &Path {
        &self.preview.path
    }

    fn show(&mut self, path: PathBuf) -> Task<Message> {
        self.generation += 1;
        let generation = self.generation;
        let dark = is_dark();
        Task::perform(
            async move {
                let loaded = tokio::task::spawn_blocking(move || content::load(&path, dark)).await;
                match loaded {
                    Ok(p) => Message::Loaded(generation, Box::new(p)),
                    Err(_) => Message::Noop,
                }
            },
            cosmic::Action::App,
        )
    }

    fn resize_task(&self) -> Task<Message> {
        let (w, h) = content::window_size(&self.preview.body, self.screen);
        match self.core.main_window_id() {
            Some(id) => window::resize(id, Size::new(w, h)),
            None => Task::none(),
        }
    }

    fn close(&self) -> Task<Message> {
        content::cleanup_cache();
        if let Some(path) = ipc::socket_path() {
            let _ = std::fs::remove_file(path);
        }
        cosmic::iced::exit()
    }

    fn open_default(&self) -> Task<Message> {
        if let Ok(mut child) = Command::new("xdg-open")
            .arg(self.current())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        self.close()
    }
}

impl cosmic::Application for QuickLook {
    type Message = Message;
    type Executor = cosmic::executor::multi::Executor;
    type Flags = Preview;
    const APP_ID: &'static str = "dev.asw.MacQuickLook";

    fn init(mut core: app::Core, preview: Preview) -> (Self, Task<Message>) {
        core.window.show_headerbar = false;
        core.window.content_container = false;
        core.set_keyboard_nav(false);
        let this = Self { core, preview, screen: DEFAULT_SCREEN, generation: 0 };
        let task = match this.core.main_window_id() {
            Some(id) => window::monitor_size(id).map(|s| cosmic::Action::App(Message::Monitor(s))),
            None => Task::none(),
        };
        (this, task)
    }

    fn core(&self) -> &app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut app::Core {
        &mut self.core
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Noop => {}
            Message::Close => return self.close(),
            Message::Drag => {
                if let Some(id) = self.core.main_window_id() {
                    return window::drag(id);
                }
            }
            Message::OpenDefault => return self.open_default(),
            Message::Monitor(Some(size)) => {
                self.screen = (size.width, size.height);
                return self.resize_task();
            }
            Message::Monitor(None) => {}
            Message::Key(key, mods) => return self.on_key(key, mods),
            Message::Step(delta) => {
                if let Some(next) = files::step(self.current(), delta) {
                    return self.show(next);
                }
            }
            Message::Remote(path) => {
                let same = path.canonicalize().ok() == self.current().canonicalize().ok();
                if same {
                    return self.close();
                }
                return self.show(path);
            }
            Message::Loaded(generation, preview) => {
                if generation == self.generation {
                    self.preview = *preview;
                    return self.resize_task();
                }
            }
            Message::Page(delta) => {
                if let Body::Pdf { pages, page, .. } = &self.preview.body {
                    let target = (*page as i64 + delta as i64).clamp(1, *pages as i64) as u32;
                    if target != *page {
                        self.generation += 1;
                        let generation = self.generation;
                        let path = self.preview.path.clone();
                        return Task::perform(
                            async move {
                                let r = tokio::task::spawn_blocking(move || content::render_pdf_page(&path, target))
                                    .await
                                    .ok()
                                    .flatten();
                                Message::PdfPage(generation, target, r)
                            },
                            cosmic::Action::App,
                        );
                    }
                }
            }
            Message::PdfPage(generation, target, rendered) => {
                if generation == self.generation
                    && let Some((handle, w, h)) = rendered
                    && let Body::Pdf { page, handle: hd, w: bw, h: bh, .. } = &mut self.preview.body
                {
                    *page = target;
                    *hd = handle;
                    *bw = w;
                    *bh = h;
                }
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let body: Element<'_, Message> = match &self.preview.body {
            Body::Image { handle, .. } => fit_image(handle.clone()),
            Body::Svg { handle, .. } => svg(handle.clone())
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
            Body::Pdf { pages, page, handle, .. } => column::with_capacity(2)
                .push(fit_image(handle.clone()))
                .push(self.pdf_footer(*page, *pages))
                .into(),
            Body::Text { pieces, truncated } => text_view(pieces, *truncated),
            Body::Video { poster: Some((handle, ..)) } => stack![
                fit_image(handle.clone()),
                container(
                    button::icon(icon::from_name("media-playback-start-symbolic").size(32))
                        .extra_large()
                        .on_press(Message::OpenDefault)
                )
                .center(Length::Fill),
            ]
            .into(),
            Body::Video { poster: None } | Body::Info => self.info_view(),
        };
        let content = column::with_capacity(2).push(self.header()).push(body);
        container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .class(cosmic::theme::Container::WindowBackground)
            .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            listen_raw(|event, _status, _id| match event {
                cosmic::iced::Event::Keyboard(cosmic::iced::keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                    Some(Message::Key(key, modifiers))
                }
                _ => None,
            }),
            Subscription::run(socket_stream),
        ])
    }
}

impl QuickLook {
    fn on_key(&mut self, key: Key, mods: Modifiers) -> Task<Message> {
        if mods.control() || mods.alt() || mods.logo() {
            return Task::none();
        }
        let named = match key {
            Key::Named(n) => n,
            Key::Character(c) if c.as_str() == " " => return self.close(),
            _ => return Task::none(),
        };
        match named {
            Named::Escape => self.close(),
            Named::Enter => self.open_default(),
            Named::ArrowLeft | Named::ArrowUp => self.update(Message::Step(-1)),
            Named::ArrowRight | Named::ArrowDown => self.update(Message::Step(1)),
            Named::PageUp => self.update(Message::Page(-1)),
            Named::PageDown => self.update(Message::Page(1)),
            _ => Task::none(),
        }
    }

    fn header(&self) -> Element<'_, Message> {
        let p = &self.preview;
        let close = button::icon(icon::from_name("window-close-symbolic").size(16))
            .on_press(Message::Close);
        let label = match &p.open_with {
            Some(app) => format!("Open with {app}"),
            None => "Open".to_string(),
        };
        let open = button::standard(label).on_press(Message::OpenDefault);
        let title = text::heading(p.name.clone())
            .width(Length::Fill)
            .align_x(Alignment::Center)
            .wrapping(cosmic::iced::widget::text::Wrapping::None);
        let bar = row::with_capacity(3)
            .push(container(close).width(Length::Fixed(140.0)).align_x(Alignment::Start))
            .push(title)
            .push(container(open).width(Length::Fixed(140.0)).align_x(Alignment::End))
            .align_y(Alignment::Center)
            .padding([0, 12]);
        mouse_area(container(bar).height(Length::Fixed(HEADER_H)).center_y(Length::Fixed(HEADER_H)))
            .on_press(Message::Drag)
            .into()
    }

    fn pdf_footer(&self, page: u32, pages: u32) -> Element<'_, Message> {
        let prev = button::icon(icon::from_name("go-previous-symbolic").size(16))
            .on_press_maybe((page > 1).then_some(Message::Page(-1)));
        let next = button::icon(icon::from_name("go-next-symbolic").size(16))
            .on_press_maybe((page < pages).then_some(Message::Page(1)));
        container(
            row::with_capacity(3)
                .push(prev)
                .push(text::body(format!("Page {page} of {pages}")))
                .push(next)
                .spacing(12)
                .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .height(Length::Fixed(PDF_FOOTER_H))
        .center_x(Length::Fill)
        .center_y(Length::Fixed(PDF_FOOTER_H))
        .into()
    }

    fn info_view(&self) -> Element<'_, Message> {
        let p = &self.preview;
        let icon_name = mime_icon(&p.mime, p.kind);
        let mut col = column::with_capacity(6)
            .push(icon::from_name(icon_name).size(128).icon())
            .push(text::title3(p.name.clone()))
            .push(text::body(format!("{} · {}", kind_label(p), content::size_string(p.size))))
            .spacing(10)
            .align_x(Alignment::Center);
        if let Some(t) = p.modified {
            col = col.push(text::caption(format!("Modified {}", content::modified_string(t))));
        }
        if matches!(p.kind, Kind::Video | Kind::Audio) {
            col = col.push(
                button::suggested("Play")
                    .leading_icon(icon::from_name("media-playback-start-symbolic").size(16))
                    .on_press(Message::OpenDefault),
            );
        }
        container(col).center(Length::Fill).into()
    }
}

fn fit_image<'a>(handle: image::Handle) -> Element<'a, Message> {
    container(
        image(handle)
            .content_fit(cosmic::iced::ContentFit::Contain)
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .padding([0, 12, 12, 12])
    .into()
}

fn text_view(pieces: &[content::Piece], truncated: bool) -> Element<'_, Message> {
    let spans: Vec<_> = pieces
        .iter()
        .map(|p| {
            let s = span::<(), _>(p.text.clone());
            match p.color {
                Some([r, g, b]) => s.color(Color::from_rgb8(r, g, b)),
                None => s,
            }
        })
        .collect();
    let mut col = column::with_capacity(2)
        .push(rich_text(spans).font(cosmic::font::mono()).size(13))
        .padding([0, 16, 12, 16]);
    if truncated {
        col = col.push(text::caption(format!("Showing the first {} lines", content::MAX_LINES)));
    }
    scrollable(col).width(Length::Fill).height(Length::Fill).into()
}

fn kind_label(p: &Preview) -> String {
    match p.kind {
        Kind::Image | Kind::Svg => "Image".into(),
        Kind::Pdf => "PDF document".into(),
        Kind::Text => "Text".into(),
        Kind::Video => "Video".into(),
        Kind::Audio => "Audio".into(),
        Kind::Other => p.mime.clone(),
    }
}

fn mime_icon(mime: &str, kind: Kind) -> String {
    match kind {
        Kind::Video => "video-x-generic".into(),
        Kind::Audio => "audio-x-generic".into(),
        Kind::Other if mime == "application/octet-stream" => "application-x-generic".into(),
        _ => mime.replace('/', "-"),
    }
}

fn socket_stream() -> impl cosmic::iced::futures::Stream<Item = Message> {
    stream::channel(
        8,
        |mut tx: cosmic::iced::futures::channel::mpsc::Sender<Message>| async move {
            use tokio::io::{AsyncBufReadExt, BufReader};

            let Some(path) = ipc::socket_path() else {
                tracing::error!("no private runtime directory; cannot accept new files");
                return;
            };
            let listener = match tokio::net::UnixListener::bind(&path) {
                Ok(l) => l,
                Err(first) => {
                    // A live peer owns the socket: we lost a start-up race.
                    if tokio::net::UnixStream::connect(&path).await.is_ok() {
                        std::process::exit(0);
                    }
                    let _ = std::fs::remove_file(&path);
                    match tokio::net::UnixListener::bind(&path) {
                        Ok(l) => l,
                        Err(e) => {
                            tracing::error!("cannot bind {}: {first}; {e}", path.display());
                            return;
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
                    && let Some(path) = ipc::decode(&line)
                {
                    let _ = tx.try_send(Message::Remote(path));
                }
            }
        },
    )
}
