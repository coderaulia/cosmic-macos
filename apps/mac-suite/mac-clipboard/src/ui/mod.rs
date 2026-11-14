// SPDX-License-Identifier: GPL-3.0-only

//! Spotlight-style quick-paste overlay (`mac-clipboard toggle` / `show`).

use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::core::text::{Ellipsize, EllipsizeHeightLimit};
use cosmic::iced::event::{Status, listen_raw};
use cosmic::iced::keyboard::{Key, Modifiers, key::Named};
use cosmic::iced::platform_specific::runtime::wayland::layer_surface::{
    IcedOutput, SctkLayerSurfaceSettings,
};
use cosmic::iced::platform_specific::shell::wayland::commands::layer_surface::{
    Anchor, KeyboardInteractivity, Layer, destroy_layer_surface, get_layer_surface,
};
use cosmic::iced::runtime::core::event::{PlatformSpecific, wayland};
use cosmic::iced::runtime::core::layout::Limits;
use cosmic::iced::runtime::core::window::{Event as WindowEvent, Id as SurfaceId};
use cosmic::iced::runtime::{Action, platform_specific, task};
use cosmic::iced::widget::scrollable::RelativeOffset;
use cosmic::iced::widget::{Column, column, container, operation, rich_text, row, span};
use cosmic::iced::{
    Alignment, Border, Color, ContentFit, Font, Length, Rectangle, Shadow, Subscription,
    font::Weight, stream,
};
use cosmic::theme::{Button, Container};
use cosmic::widget::{
    button, divider, icon, image as cosmic_image, scrollable,
    space::{horizontal as horizontal_space, vertical as vertical_space},
    text, text_input,
};
use cosmic::{Element, app::{self, Task}};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as NucleoConfig, Matcher, Utf32Str};
use url::Url;
use zbus::{Connection, interface};

use crate::config::ClipboardConfig;
use crate::dbus::{
    DbusListItem, MacClipboardProxy, OVERLAY_BUS_NAME, OVERLAY_OBJECT_PATH,
};
use crate::model::ItemKind;
use crate::search::FilterPill;
use crate::store::now_unix_ms;

const WINDOW_WIDTH: f32 = 760.0;
const WINDOW_HEIGHT: f32 = 460.0;
const LIST_LIMIT: u32 = 200;

static INPUT_ID: LazyLock<cosmic::widget::Id> = LazyLock::new(|| cosmic::widget::Id::new("clip-search"));
static LIST_SCROLL_ID: LazyLock<cosmic::widget::Id> =
    LazyLock::new(|| cosmic::widget::Id::new("clip-list-scroll"));
static PREVIEW_SCROLL_ID: LazyLock<cosmic::widget::Id> =
    LazyLock::new(|| cosmic::widget::Id::new("clip-preview-scroll"));

struct OverlayControl {
    tx: cosmic::iced::futures::channel::mpsc::Sender<Message>,
}

#[interface(name = "dev.asw.MacClipboard.Overlay")]
impl OverlayControl {
    async fn close(&self) {
        let mut tx = self.tx.clone();
        let _ = tx.try_send(Message::Close);
    }

    async fn ping(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone)]
pub struct ItemDetails {
    pub id: String,
    pub kind: String,
    pub preview: String,
    pub text: Option<String>,
    pub html: Option<String>,
    pub paths: Vec<String>,
    pub cut: bool,
    pub blob_path: Option<String>,
    pub thumbnail_path: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub size_bytes: u64,
    pub created: i64,
    pub last_used: i64,
    pub pinned: bool,
    pub source_app: String,
}

#[derive(Debug, Clone)]
pub enum Message {
    ItemsLoaded(Result<Vec<DbusListItem>, String>),
    DetailsLoaded(Result<Box<ItemDetails>, String>),
    QueryChanged(String),
    SelectFilter(FilterPill),
    CycleFilter(isize),
    SelectIndex(usize),
    MoveSelection(isize),
    Activate {
        index: Option<usize>,
        plain: bool,
        paste: bool,
    },
    TogglePin,
    DeleteSelected,
    FocusSearch,
    EscapePressed,
    ActivatedResult {
        copied_only: bool,
        pasted: bool,
        keep_open: bool,
    },
    ClearToast,
    Unfocused(SurfaceId),
    KeyPressed(Key, Modifiers, Status),
    Close,
    Noop,
}

pub struct QuickPasteApp {
    core: app::Core,
    window_id: SurfaceId,
    config: ClipboardConfig,
    query: String,
    filter: FilterPill,
    items: Vec<DbusListItem>,
    selected: usize,
    details_cache: HashMap<String, ItemDetails>,
    toast: Option<String>,
    closing: bool,
}

pub fn run_overlay() -> cosmic::iced::Result {
    let settings = app::Settings::default()
        .antialiasing(true)
        .client_decorations(false)
        .default_text_size(14.0)
        .scale_factor(1.0)
        .no_main_window(true)
        .exit_on_close(true);
    app::run::<QuickPasteApp>(settings, ())
}

impl QuickPasteApp {
    fn selected_item(&self) -> Option<&DbusListItem> {
        self.items.get(self.selected)
    }

    fn fetch_items_task(&self) -> Task<Message> {
        let filter = self.filter.as_str().to_string();
        let query = self.query.clone();
        Task::perform(
            async move {
                let conn = Connection::session()
                    .await
                    .map_err(|e| e.to_string())?;
                let proxy = MacClipboardProxy::new(&conn)
                    .await
                    .map_err(|e| e.to_string())?;
                proxy
                    .list(&filter, &query, LIST_LIMIT)
                    .await
                    .map_err(|e| e.to_string())
            },
            |res| cosmic::Action::App(Message::ItemsLoaded(res)),
        )
    }

    fn fetch_selected_details_task(&self) -> Task<Message> {
        let Some(item) = self.selected_item() else {
            return Task::none();
        };
        if self.details_cache.contains_key(&item.id) {
            return Task::none();
        }
        let id = item.id.clone();
        Task::perform(
            async move {
                let conn = Connection::session()
                    .await
                    .map_err(|e| e.to_string())?;
                let proxy = MacClipboardProxy::new(&conn)
                    .await
                    .map_err(|e| e.to_string())?;
                let map = proxy.get(&id).await.map_err(|e| e.to_string())?;
                Ok(Box::new(parse_details_map(&id, map)))
            },
            |res| cosmic::Action::App(Message::DetailsLoaded(res)),
        )
    }

    fn blur_task(&self) -> Task<Message> {
        let blur = if self.core.system_theme().cosmic().frosted_system_interface {
            Some(vec![Rectangle {
                x: 0.0,
                y: 0.0,
                width: f32::MAX,
                height: f32::MAX,
            }])
        } else {
            None
        };
        task::effect(Action::PlatformSpecific(platform_specific::Action::Wayland(
            cosmic::iced::runtime::platform_specific::wayland::Action::BlurSurface(
                self.window_id,
                blur,
            ),
        )))
    }

    fn close_window(&mut self) -> Task<Message> {
        if self.closing {
            return Task::none();
        }
        self.closing = true;
        Task::batch([
            destroy_layer_surface(self.window_id),
            cosmic::iced::exit(),
        ])
    }

    fn preview_pane(&self) -> Element<'_, Message> {
        let Some(item) = self.selected_item() else {
            return container(
                text::body("No matching clipboard items")
                    .align_x(Horizontal::Center)
                    .align_y(Vertical::Center),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into();
        };

        let details = self.details_cache.get(&item.id);
        let kind = ItemKind::from_str_kind(&item.kind).unwrap_or(ItemKind::Text);

        let body: Element<'_, Message> = match kind {
            ItemKind::Image => {
                let img_path = details
                    .and_then(|d| d.blob_path.as_deref().or(d.thumbnail_path.as_deref()))
                    .or_else(|| {
                        (!item.thumbnail_path.is_empty()).then_some(item.thumbnail_path.as_str())
                    });
                let mut col = Column::new()
                    .spacing(10)
                    .align_x(Alignment::Center)
                    .width(Length::Fill);
                if let Some(path) = img_path
                    && Path::new(path).exists()
                {
                    let handle = cosmic_image::Handle::from_path(path);
                    col = col.push(
                        cosmic_image::Image::new(handle)
                            .content_fit(ContentFit::Contain)
                            .width(Length::Fill)
                            .height(Length::FillPortion(1)),
                    );
                }
                if let Some((w, h)) = details.and_then(|d| d.width.zip(d.height)) {
                    col = col.push(text::caption(format!("{w} × {h} px · {}", format_bytes(item.size_bytes))));
                }
                container(col)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .center_x(Length::Fill)
                    .center_y(Length::Fill)
                    .into()
            }
            ItemKind::Link => {
                let full_url = details
                    .and_then(|d| d.text.as_deref())
                    .unwrap_or(&item.preview);
                let host = Url::parse(full_url.trim())
                    .ok()
                    .and_then(|u| u.host_str().map(str::to_string))
                    .unwrap_or_default();
                let mut col = Column::new().spacing(8).width(Length::Fill);
                if !host.is_empty() {
                    col = col.push(
                        container(text::caption(host))
                            .padding([4, 10])
                            .class(Container::custom(|t| {
                                let c = t.cosmic();
                                container::Style {
                                    background: Some(Color::from(c.accent_color()).scale_alpha(0.18).into()),
                                    text_color: Some(c.accent_color().into()),
                                    border: Border {
                                        radius: c.corner_radii.radius_s.into(),
                                        ..Default::default()
                                    },
                                    ..Default::default()
                                }
                            })),
                    );
                }
                col = col.push(
                    scrollable(text::body(full_url.to_string()).width(Length::Fill))
                        .id(PREVIEW_SCROLL_ID.clone())
                        .height(Length::Fill),
                );
                col.into()
            }
            ItemKind::Files => {
                let paths: Vec<String> = details
                    .map(|d| d.paths.clone())
                    .unwrap_or_else(|| vec![item.preview.clone()]);
                let cut = details.is_some_and(|d| d.cut);
                let mut list = Column::new().spacing(6).width(Length::Fill);
                if cut {
                    list = list.push(text::caption("Cut operation (move on paste)"));
                }
                for p in paths {
                    let path = Path::new(&p);
                    let name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(&p)
                        .to_string();
                    let folder = path
                        .parent()
                        .and_then(|par| par.to_str())
                        .unwrap_or("")
                        .to_string();
                    list = list.push(
                        row![
                            icon::from_name("text-x-generic-symbolic").size(16),
                            column![text::body(name), text::caption(folder)].spacing(2),
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center),
                    );
                }
                scrollable(list)
                    .id(PREVIEW_SCROLL_ID.clone())
                    .height(Length::Fill)
                    .into()
            }
            ItemKind::Text | ItemKind::RichText => {
                let content = details
                    .and_then(|d| d.text.as_deref())
                    .unwrap_or(&item.preview);
                let use_mono = looks_like_code(content);
                let mut txt = text::body(content.to_string()).width(Length::Fill);
                if use_mono {
                    txt = txt.font(Font::MONOSPACE);
                }
                scrollable(txt)
                    .id(PREVIEW_SCROLL_ID.clone())
                    .height(Length::Fill)
                    .into()
            }
        };

        let now_ms = now_unix_ms();
        let rel = format_relative_time(item.created, now_ms);
        let source = if item.source_app.is_empty() {
            "Clipboard".to_string()
        } else {
            item.source_app.clone()
        };

        let meta_detail = match kind {
            ItemKind::Image => details
                .and_then(|d| d.width.zip(d.height))
                .map(|(w, h)| format!("{w}×{h} · {}", format_bytes(item.size_bytes)))
                .unwrap_or_else(|| format_bytes(item.size_bytes)),
            ItemKind::Files => {
                let count = details.map_or(1, |d| d.paths.len().max(1));
                format!("{count} item(s)")
            }
            _ => {
                let content = details
                    .and_then(|d| d.text.as_deref())
                    .unwrap_or(&item.preview);
                let chars = content.chars().count();
                let lines = content.lines().count().max(1);
                let rich_tag = if kind == ItemKind::RichText {
                    " · Rich text"
                } else {
                    ""
                };
                format!("{chars} characters · {lines} lines{rich_tag}")
            }
        };

        column![
            container(body).height(Length::Fill).width(Length::Fill),
            divider::horizontal::light(),
            column![
                text::caption(format!("Copied from {source} · {rel}")),
                text::caption(meta_detail),
            ]
            .spacing(2),
        ]
        .spacing(8)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding([4, 8])
        .into()
    }
}

impl cosmic::Application for QuickPasteApp {
    type Message = Message;
    type Executor = cosmic::executor::single::Executor;
    type Flags = ();
    const APP_ID: &'static str = "dev.asw.MacClipboard";

    fn init(mut core: app::Core, _flags: ()) -> (Self, Task<Self::Message>) {
        core.set_app_type(cosmic::core::AppType::System);
        core.set_keyboard_nav(false);

        let window_id = SurfaceId::unique();
        let config = ClipboardConfig::load();
        let app = Self {
            core,
            window_id,
            config,
            query: String::new(),
            filter: FilterPill::All,
            items: Vec::new(),
            selected: 0,
            details_cache: HashMap::new(),
            toast: None,
            closing: false,
        };

        let layer_task = get_layer_surface(SctkLayerSurfaceSettings {
            id: window_id,
            layer: Layer::Overlay,
            keyboard_interactivity: KeyboardInteractivity::Exclusive,
            anchor: Anchor::empty(),
            output: IcedOutput::Active,
            namespace: "mac-clipboard".into(),
            size: Some((Some(WINDOW_WIDTH as u32), Some(WINDOW_HEIGHT as u32))),
            size_limits: Limits::NONE
                .min_width(WINDOW_WIDTH)
                .max_width(WINDOW_WIDTH)
                .min_height(WINDOW_HEIGHT)
                .max_height(WINDOW_HEIGHT),
            exclusive_zone: -1,
            ..Default::default()
        });

        let task = Task::batch([
            layer_task,
            app.blur_task(),
            text_input::focus(INPUT_ID.clone()),
            app.fetch_items_task(),
        ]);

        (app, task)
    }

    fn core(&self) -> &app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut app::Core {
        &mut self.core
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::ItemsLoaded(Ok(items)) => {
                self.items = items;
                self.details_cache.clear();
                if self.items.is_empty() {
                    self.selected = 0;
                    return Task::none();
                }
                if self.selected >= self.items.len() {
                    self.selected = self.items.len() - 1;
                }
                return self.fetch_selected_details_task();
            }
            Message::ItemsLoaded(Err(err)) => {
                tracing::warn!("failed to load clipboard items: {err}");
            }
            Message::DetailsLoaded(Ok(details)) => {
                self.details_cache.insert(details.id.clone(), *details);
            }
            Message::DetailsLoaded(Err(err)) => {
                tracing::debug!("failed to load item details: {err}");
            }
            Message::QueryChanged(query) => {
                self.query = query;
                self.selected = 0;
                return Task::batch([
                    self.fetch_items_task(),
                    operation::snap_to(LIST_SCROLL_ID.clone(), RelativeOffset::START),
                ]);
            }
            Message::SelectFilter(pill) => {
                self.filter = pill;
                self.selected = 0;
                return Task::batch([
                    self.fetch_items_task(),
                    operation::snap_to(LIST_SCROLL_ID.clone(), RelativeOffset::START),
                ]);
            }
            Message::CycleFilter(dir) => {
                self.filter = if dir >= 0 {
                    self.filter.next()
                } else {
                    self.filter.prev()
                };
                self.selected = 0;
                return Task::batch([
                    self.fetch_items_task(),
                    operation::snap_to(LIST_SCROLL_ID.clone(), RelativeOffset::START),
                ]);
            }
            Message::SelectIndex(idx) => {
                if idx < self.items.len() {
                    if self.selected == idx {
                        return self.update(Message::Activate {
                            index: Some(idx),
                            plain: false,
                            paste: true,
                        });
                    }
                    self.selected = idx;
                    return self.fetch_selected_details_task();
                }
            }
            Message::MoveSelection(delta) => {
                if self.items.is_empty() {
                    return Task::none();
                }
                let len = self.items.len() as isize;
                self.selected = (self.selected as isize + delta).rem_euclid(len) as usize;
                return self.fetch_selected_details_task();
            }
            Message::Activate {
                index,
                plain,
                paste,
            } => {
                let idx = index.unwrap_or(self.selected);
                let Some(item) = self.items.get(idx) else {
                    return self.close_window();
                };
                let id = item.id.clone();
                let keep_open = !paste;
                return Task::perform(
                    async move {
                        let conn = Connection::session().await.ok();
                        let pasted = if let Some(conn) = conn
                            && let Ok(proxy) = MacClipboardProxy::new(&conn).await
                        {
                            proxy.activate(&id, plain, paste).await.unwrap_or(false)
                        } else {
                            false
                        };
                        (paste, pasted, keep_open)
                    },
                    |(want_paste, pasted, keep_open)| {
                        cosmic::Action::App(Message::ActivatedResult {
                            copied_only: !want_paste,
                            pasted,
                            keep_open,
                        })
                    },
                );
            }
            Message::ActivatedResult {
                copied_only,
                pasted,
                keep_open,
            } => {
                if keep_open {
                    self.toast = Some("Copied".to_string());
                    return Task::perform(
                        async {
                            tokio::time::sleep(Duration::from_millis(1200)).await;
                        },
                        |()| cosmic::Action::App(Message::ClearToast),
                    );
                }
                if !copied_only && !pasted {
                    self.toast = Some("Copied".to_string());
                }
                return self.close_window();
            }
            Message::ClearToast => {
                self.toast = None;
            }
            Message::Unfocused(id) => {
                if id == self.window_id {
                    return self.close_window();
                }
            }
            Message::KeyPressed(key, modifiers, status) => {
                if let Some(msg) = handle_key(key, modifiers, status, self.query.is_empty()) {
                    return self.update(msg);
                }
            }
            Message::TogglePin => {
                let Some(item) = self.selected_item() else {
                    return Task::none();
                };
                let id = item.id.clone();
                let next_pinned = !item.pinned;
                return Task::perform(
                    async move {
                        if let Ok(conn) = Connection::session().await
                            && let Ok(proxy) = MacClipboardProxy::new(&conn).await
                        {
                            let _ = proxy.pin(&id, next_pinned).await;
                        }
                    },
                    |()| cosmic::Action::App(Message::Noop),
                );
            }
            Message::DeleteSelected => {
                let Some(item) = self.selected_item() else {
                    return Task::none();
                };
                let id = item.id.clone();
                return Task::perform(
                    async move {
                        if let Ok(conn) = Connection::session().await
                            && let Ok(proxy) = MacClipboardProxy::new(&conn).await
                        {
                            let _ = proxy.remove(&id).await;
                        }
                    },
                    |()| cosmic::Action::App(Message::Noop),
                );
            }
            Message::FocusSearch => {
                return text_input::focus(INPUT_ID.clone());
            }
            Message::EscapePressed => {
                if !self.query.is_empty() {
                    self.query.clear();
                    self.selected = 0;
                    return self.fetch_items_task();
                }
                return self.close_window();
            }
            Message::Close => {
                return self.close_window();
            }
            Message::Noop => {
                return self.fetch_items_task();
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Self::Message> {
        unreachable!("layer-shell overlay uses view_window")
    }

    fn view_window(&self, id: SurfaceId) -> Element<'_, Self::Message> {
        if id != self.window_id {
            return vertical_space().height(Length::Fixed(1.0)).into();
        }

        let search_input = text_input::search_input("Search clipboard", &self.query)
            .id(INPUT_ID.clone())
            .on_input(Message::QueryChanged)
            .on_paste(Message::QueryChanged)
            .on_submit(|_| Message::Activate {
                index: None,
                plain: false,
                paste: true,
            })
            .width(Length::Fill)
            .size(16.0)
            .padding([8, 12])
            .always_active();

        let hotkey_badge = container(text::caption(self.config.hotkey_hint.clone()))
            .padding([4, 8])
            .class(Container::custom(|t| {
                let c = t.cosmic();
                container::Style {
                    background: Some(Color::from(c.bg_component_color()).into()),
                    text_color: Some(c.on_bg_color().into()),
                    border: Border {
                        radius: c.corner_radii.radius_s.into(),
                        width: 1.0,
                        color: c.bg_divider().into(),
                    },
                    ..Default::default()
                }
            }));

        let search_row = row![search_input, hotkey_badge]
            .spacing(8)
            .align_y(Alignment::Center);

        let pills_row = row(FilterPill::ALL.iter().map(|&pill| {
            let active = self.filter == pill;
            button::custom(text::caption(pill.label()))
                .padding([4, 12])
                .on_press(Message::SelectFilter(pill))
                .class(Button::Custom {
                    active: Box::new(move |focused, theme| {
                        pill_style(theme, active, focused, false)
                    }),
                    hovered: Box::new(move |focused, theme| {
                        pill_style(theme, active, focused, true)
                    }),
                    pressed: Box::new(move |focused, theme| {
                        pill_style(theme, active, focused, true)
                    }),
                    disabled: Box::new(|theme| pill_style(theme, false, false, false)),
                })
                .into()
        }))
        .spacing(6);

        let now_ms = now_unix_ms();
        let list_items: Vec<Element<'_, Message>> = self
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let is_selected = i == self.selected;
                let badge = if i < 9 {
                    format!("{}", i + 1)
                } else {
                    String::new()
                };
                let kind = ItemKind::from_str_kind(&item.kind).unwrap_or(ItemKind::Text);
                let kind_icon_name = match kind {
                    ItemKind::Text => "text-x-generic-symbolic",
                    ItemKind::RichText => "x-office-document-symbolic",
                    ItemKind::Link => "web-browser-symbolic",
                    ItemKind::Image => "image-x-generic-symbolic",
                    ItemKind::Files => "folder-symbolic",
                };

                let mut row_items: Vec<Element<'_, Message>> = Vec::with_capacity(6);
                row_items.push(
                    container(text::caption(badge))
                        .width(Length::Fixed(16.0))
                        .align_x(Horizontal::Center)
                        .into(),
                );

                if item.pinned {
                    row_items.push(
                        icon::from_name("view-pin-symbolic")
                            .size(14)
                            .into(),
                    );
                }

                if kind == ItemKind::Image
                    && !item.thumbnail_path.is_empty()
                    && Path::new(&item.thumbnail_path).exists()
                {
                    row_items.push(
                        cosmic_image::Image::new(cosmic_image::Handle::from_path(
                            &item.thumbnail_path,
                        ))
                        .width(Length::Fixed(22.0))
                        .height(Length::Fixed(22.0))
                        .content_fit(ContentFit::Cover)
                        .into(),
                    );
                } else {
                    row_items.push(icon::from_name(kind_icon_name).size(16).into());
                }

                let preview_el = render_highlighted_preview(&item.preview, &self.query);
                row_items.push(container(preview_el).width(Length::Fill).into());

                row_items.push(
                    text::caption(format_relative_time(item.created, now_ms))
                        .align_x(Horizontal::Right)
                        .into(),
                );

                button::custom(
                    row(row_items)
                        .spacing(8)
                        .align_y(Alignment::Center)
                        .width(Length::Fill),
                )
                .width(Length::Fill)
                .padding([7, 10])
                .on_press(Message::SelectIndex(i))
                .class(Button::Custom {
                    active: Box::new(move |focused, theme| {
                        row_button_style(theme, is_selected, focused, false)
                    }),
                    hovered: Box::new(move |focused, theme| {
                        row_button_style(theme, is_selected, focused, true)
                    }),
                    pressed: Box::new(move |focused, theme| {
                        row_button_style(theme, is_selected, focused, true)
                    }),
                    disabled: Box::new(|theme| row_button_style(theme, false, false, false)),
                })
                .into()
            })
            .collect();

        let list_col: Element<'_, Message> = if list_items.is_empty() {
            container(text::caption("Empty")).center_x(Length::Fill).center_y(Length::Fill).into()
        } else {
            scrollable(Column::with_children(list_items).spacing(2))
                .id(LIST_SCROLL_ID.clone())
                .height(Length::Fill)
                .into()
        };

        let split_body = row![
            container(list_col)
                .width(Length::FillPortion(5))
                .height(Length::Fill),
            divider::vertical::light(),
            container(self.preview_pane())
                .width(Length::FillPortion(6))
                .height(Length::Fill),
        ]
        .spacing(12)
        .height(Length::Fill);

        let footer_hints = text::caption(
            "↩ Paste   ⇧↩ Paste as Text   P Pin   ⌫ Delete   Tab Filter   esc Close",
        );
        let footer: Element<'_, Message> = if let Some(toast) = &self.toast {
            row![
                footer_hints,
                horizontal_space().width(Length::Fill),
                container(text::caption(toast.clone()))
                    .padding([2, 8])
                    .class(Container::custom(|t| {
                        let c = t.cosmic();
                        container::Style {
                            background: Some(Color::from(c.accent_color()).into()),
                            text_color: Some(c.on_accent_color().into()),
                            border: Border {
                                radius: c.corner_radii.radius_s.into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        }
                    })),
            ]
            .align_y(Alignment::Center)
            .into()
        } else {
            footer_hints.into()
        };

        let main_col = column![
            search_row,
            pills_row,
            divider::horizontal::light(),
            split_body,
            divider::horizontal::light(),
            footer,
        ]
        .spacing(10)
        .width(Length::Fill)
        .height(Length::Fill);

        container(main_col)
            .width(Length::Fixed(WINDOW_WIDTH))
            .height(Length::Fixed(WINDOW_HEIGHT))
            .padding([14, 16])
            .class(Container::Custom(Box::new(|theme| {
                let t = theme.cosmic();
                container::Style {
                    text_color: Some(t.on_bg_color().into()),
                    icon_color: Some(t.on_bg_color().into()),
                    background: Some(Color::from(t.background(theme.transparent).base).into()),
                    border: Border {
                        radius: t.corner_radii.radius_l.into(),
                        width: 1.0,
                        color: t.bg_divider().into(),
                    },
                    shadow: Shadow::default(),
                    snap: true,
                }
            })))
            .into()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        Subscription::batch([
            listen_raw(|event, status, id| match event {
                cosmic::iced::Event::PlatformSpecific(PlatformSpecific::Wayland(
                    wayland::Event::Layer(wayland::LayerEvent::Unfocused, _, layer_id),
                )) => Some(Message::Unfocused(layer_id)),
                cosmic::iced::Event::Window(WindowEvent::Unfocused) => {
                    Some(Message::Unfocused(id))
                }
                cosmic::iced::Event::Keyboard(cosmic::iced::keyboard::Event::KeyPressed {
                    key,
                    modifiers,
                    ..
                }) => Some(Message::KeyPressed(key, modifiers, status)),
                _ => None,
            }),
            Subscription::run(dbus_event_stream),
        ])
    }
}

fn handle_key(
    key: Key,
    modifiers: Modifiers,
    _status: Status,
    query_empty: bool,
) -> Option<Message> {
    match key {
        Key::Named(Named::Escape) => Some(Message::EscapePressed),
        Key::Named(Named::ArrowUp) => Some(Message::MoveSelection(-1)),
        Key::Named(Named::ArrowDown) => Some(Message::MoveSelection(1)),
        Key::Named(Named::Enter) => Some(Message::Activate {
            index: None,
            plain: modifiers.shift(),
            paste: !modifiers.control(),
        }),
        Key::Named(Named::Tab) => Some(Message::CycleFilter(if modifiers.shift() { -1 } else { 1 })),
        Key::Named(Named::Delete) => Some(Message::DeleteSelected),
        Key::Named(Named::Backspace) if modifiers.control() => Some(Message::DeleteSelected),
        Key::Character(ref c) if modifiers.control() && c.eq_ignore_ascii_case("p") => {
            if modifiers.shift() {
                Some(Message::TogglePin)
            } else {
                Some(Message::MoveSelection(-1))
            }
        }
        Key::Character(ref c) if modifiers.control() && c.eq_ignore_ascii_case("n") => {
            Some(Message::MoveSelection(1))
        }
        Key::Character(ref c) if modifiers.control() && c.eq_ignore_ascii_case("f") => {
            Some(Message::FocusSearch)
        }
        Key::Character(ref c) if modifiers.control() => {
            digit_index(c).map(|idx| Message::Activate {
                index: Some(idx),
                plain: false,
                paste: true,
            })
        }
        Key::Character(ref c) if query_empty && modifiers.is_empty() => {
            if c.eq_ignore_ascii_case("p") {
                Some(Message::TogglePin)
            } else {
                digit_index(c).map(|idx| Message::Activate {
                    index: Some(idx),
                    plain: false,
                    paste: true,
                })
            }
        }
        _ => None,
    }
}

fn digit_index(s: &str) -> Option<usize> {
    let ch = s.chars().next()?;
    if s.len() == 1 && ('1'..='9').contains(&ch) {
        Some((ch as usize) - ('1' as usize))
    } else {
        None
    }
}

fn dbus_event_stream() -> impl cosmic::iced::futures::Stream<Item = Message> {
    stream::channel(32, |mut tx: cosmic::iced::futures::channel::mpsc::Sender<Message>| async move {
        if let Ok(conn) = Connection::session().await {
            let _ = conn
                .object_server()
                .at(OVERLAY_OBJECT_PATH, OverlayControl { tx: tx.clone() })
                .await;
            let _ = conn.request_name(OVERLAY_BUS_NAME).await;

            if let Ok(proxy) = MacClipboardProxy::new(&conn).await
                && let Ok(mut changes) = proxy.receive_changed().await
            {
                use cosmic::iced::futures::StreamExt;
                while changes.next().await.is_some() {
                    if tx.try_send(Message::Noop).is_err() {
                        break;
                    }
                }
            } else {
                std::future::pending::<()>().await;
            }
        }
    })
}

fn pill_style(
    theme: &cosmic::Theme,
    active: bool,
    focused: bool,
    hovered: bool,
) -> button::Style {
    let c = theme.cosmic();
    let accent = Color::from(c.accent_color());
    let bg = if active {
        accent.scale_alpha(0.24)
    } else if hovered || focused {
        Color::from(c.bg_component_color()).scale_alpha(0.85)
    } else {
        Color::from(c.bg_component_color()).scale_alpha(0.45)
    };
    let text_color = if active {
        accent
    } else {
        Color::from(c.on_bg_color())
    };
    button::Style {
        background: Some(bg.into()),
        text_color: Some(text_color),
        border_radius: [999.0; 4].into(),
        border_width: if active { 1.0 } else { 0.0 },
        border_color: if active { accent } else { Color::TRANSPARENT },
        ..Default::default()
    }
}

fn row_button_style(
    theme: &cosmic::Theme,
    selected: bool,
    focused: bool,
    hovered: bool,
) -> button::Style {
    let c = theme.cosmic();
    let accent = Color::from(c.accent_color());
    let bg = if selected {
        Some(accent.scale_alpha(0.22).into())
    } else if hovered || focused {
        Some(Color::from(c.bg_component_color()).scale_alpha(0.7).into())
    } else {
        None
    };
    button::Style {
        background: bg,
        text_color: Some(c.on_bg_color().into()),
        icon_color: Some(c.on_bg_color().into()),
        border_radius: c.corner_radii.radius_s.into(),
        border_width: if selected { 1.0 } else { 0.0 },
        border_color: if selected {
            accent.scale_alpha(0.6)
        } else {
            Color::TRANSPARENT
        },
        ..Default::default()
    }
}

fn render_highlighted_preview<'a>(preview: &'a str, query: &str) -> Element<'a, Message> {
    let q = query.trim();
    if q.is_empty() {
        return text::body(preview.to_string())
            .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)))
            .into();
    }

    let mut matcher = Matcher::new(NucleoConfig::DEFAULT);
    let pattern = Pattern::parse(q, CaseMatching::Smart, Normalization::Smart);
    let mut buf = Vec::new();
    let mut indices = Vec::new();
    let hay = Utf32Str::new(preview, &mut buf);
    let _ = pattern.indices(hay, &mut matcher, &mut indices);
    indices.sort_unstable();
    indices.dedup();

    if indices.is_empty() {
        return text::body(preview.to_string())
            .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)))
            .into();
    }

    let mut spans = Vec::new();
    let mut current = String::new();
    let mut current_match = false;
    let mut idx_pos = 0usize;

    for (char_idx, ch) in preview.chars().take(120).enumerate() {
        let is_match = indices
            .get(idx_pos)
            .is_some_and(|&m| m as usize == char_idx);
        if is_match {
            idx_pos += 1;
        }
        if !current.is_empty() && is_match != current_match {
            spans.push(make_preview_span(std::mem::take(&mut current), current_match));
        }
        current_match = is_match;
        current.push(ch);
    }
    if !current.is_empty() {
        spans.push(make_preview_span(current, current_match));
    }

    rich_text(spans).size(13.5).into()
}

fn make_preview_span<'a>(text: String, highlighted: bool) -> cosmic::iced::widget::text::Span<'a, ()> {
    let s = span(text);
    if highlighted {
        let mut bold = Font::DEFAULT;
        bold.weight = Weight::Semibold;
        s.font(bold).color(Color::from_rgb(0.24, 0.56, 1.0))
    } else {
        s
    }
}

fn parse_details_map(id: &str, map: HashMap<String, zbus::zvariant::OwnedValue>) -> ItemDetails {
    let get_str = |k: &str| -> Option<String> {
        map.get(k).and_then(|v| String::try_from(v.clone()).ok())
    };
    let get_u64 = |k: &str| -> u64 {
        map.get(k)
            .and_then(|v| u64::try_from(v.clone()).ok())
            .unwrap_or(0)
    };
    let get_i64 = |k: &str| -> i64 {
        map.get(k)
            .and_then(|v| i64::try_from(v.clone()).ok())
            .unwrap_or(0)
    };
    let get_u32 = |k: &str| -> Option<u32> {
        map.get(k).and_then(|v| u32::try_from(v.clone()).ok())
    };
    let get_bool = |k: &str| -> bool {
        map.get(k)
            .and_then(|v| bool::try_from(v.clone()).ok())
            .unwrap_or(false)
    };
    let paths: Vec<String> = map
        .get("paths")
        .and_then(|v| Vec::<String>::try_from(v.clone()).ok())
        .unwrap_or_default();

    ItemDetails {
        id: get_str("id").unwrap_or_else(|| id.to_string()),
        kind: get_str("kind").unwrap_or_else(|| "text".to_string()),
        preview: get_str("preview").unwrap_or_default(),
        text: get_str("text"),
        html: get_str("html"),
        paths,
        cut: get_bool("cut"),
        blob_path: get_str("blob_path"),
        thumbnail_path: get_str("thumbnail_path"),
        width: get_u32("width"),
        height: get_u32("height"),
        size_bytes: get_u64("size"),
        created: get_i64("created"),
        last_used: get_i64("last_used"),
        pinned: get_bool("pinned"),
        source_app: get_str("source_app").unwrap_or_default(),
    }
}

fn looks_like_code(text: &str) -> bool {
    text.lines().take(5).any(|line| {
        line.starts_with("  ")
            || line.starts_with('\t')
            || line.contains("->")
            || line.contains("::")
            || line.contains('{')
            || line.starts_with("ssh ")
            || line.starts_with("cargo ")
    })
}

#[must_use]
pub fn format_relative_time(ts_ms: i64, now_ms: i64) -> String {
    let diff_secs = (now_ms.saturating_sub(ts_ms)) / 1000;
    if diff_secs < 45 {
        "now".to_string()
    } else if diff_secs < 3600 {
        format!("{}m", diff_secs / 60)
    } else if diff_secs < 86_400 {
        format!("{}h", diff_secs / 3600)
    } else {
        format!("{}d", diff_secs / 86_400)
    }
}

#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}
