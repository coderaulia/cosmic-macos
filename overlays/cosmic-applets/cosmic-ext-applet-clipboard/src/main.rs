// SPDX-License-Identifier: GPL-3.0-only

mod dbus_client;

use cosmic::{
    Apply, Element, Task,
    app::{self, Core},
    applet::{menu_button, padded_control},
    cosmic_theme::Spacing,
    iced::{
        Alignment, Length, Rectangle, Subscription,
        futures::{SinkExt, StreamExt, channel::mpsc},
        platform_specific::shell::wayland::commands::popup::destroy_popup,
        stream,
        widget::{column, row},
        window,
    },
    theme,
    widget::{
        Id, autosize, button, container, divider, icon, image, mouse_area, rectangle_tracker::*,
        scrollable, text, text_input,
    },
};
use cosmic_ext_mac_applet_style::{
    PillVariant, icons::clipboard_icon, mac_pill_button, mac_popover_container_sized,
    panel_icon_size,
};
use dbus_client::{ClipboardItemSummary, MacClipboardProxy, Snapshot};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

const POPUP_WIDTH: f32 = 360.0;
const MAX_POPUP_ITEMS: u32 = 10;

static AUTOSIZE_MAIN_ID: LazyLock<Id> = LazyLock::new(|| Id::new("autosize-main"));
static SEARCH_INPUT_ID: LazyLock<Id> = LazyLock::new(|| Id::new("clipboard-search"));

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt::init();
    let _ = tracing_log::LogTracer::init();
    cosmic::applet::run::<ClipboardApplet>(())
}

struct ClipboardApplet {
    core: Core,
    popup: Option<window::Id>,
    rectangle_tracker: Option<RectangleTracker<u32>>,
    rectangle: Rectangle,
    query: String,
    items: Vec<ClipboardItemSummary>,
    paused: bool,
    daemon_available: bool,
    context_item_id: Option<String>,
    confirm_clear: bool,
}

#[derive(Debug, Clone)]
enum Message {
    TogglePopup,
    CloseRequested(window::Id),
    Rectangle(RectangleUpdate<u32>),
    SnapshotLoaded(Snapshot),
    QueryChanged(String),
    Activate(String, bool),
    ToggleContext(String),
    Pin(String, bool),
    Remove(String),
    ShowAll,
    TogglePaused,
    RequestClear,
    ConfirmClear,
    CancelClear,
    OpenSettings,
    Refresh,
}

impl ClipboardApplet {
    fn refresh_task(&self) -> Task<cosmic::Action<Message>> {
        let q = self.query.clone();
        cosmic::task::future(async move {
            let snap = dbus_client::fetch_snapshot(&q, MAX_POPUP_ITEMS).await;
            Message::SnapshotLoaded(snap)
        })
        .map(cosmic::Action::App)
    }

    fn item_row(&self, item: &ClipboardItemSummary) -> Element<'_, Message> {
        let thumb_or_icon: Element<'_, Message> = if !item.thumb_path.is_empty() {
            image(image::Handle::from_path(&item.thumb_path))
                .width(Length::Fixed(28.0))
                .height(Length::Fixed(28.0))
                .into()
        } else {
            icon::from_name(kind_icon_name(&item.kind))
                .size(16)
                .symbolic(true)
                .into()
        };

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let rel = format_relative_time(item.created_unix, now);
        let preview = single_line_preview(&item.preview, 44);

        let mut meta = row![text::caption(rel)].spacing(6).align_y(Alignment::Center);
        if item.pinned {
            meta = meta.push(
                icon::from_name("view-pin-symbolic")
                    .size(12)
                    .symbolic(true),
            );
        }

        let main_btn = button::custom(
            row![
                container(thumb_or_icon)
                    .width(Length::Fixed(28.0))
                    .height(Length::Fixed(28.0))
                    .center(Length::Fixed(28.0)),
                column![text::body(preview), meta]
                    .spacing(2)
                    .width(Length::Fill),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .padding([4, 6]),
        )
        .class(cosmic::theme::Button::Text)
        .width(Length::Fill)
        .on_press(Message::Activate(item.id.clone(), false));

        let more_btn = button::icon(icon::from_name("view-more-symbolic").symbolic(true))
            .extra_small()
            .on_press(Message::ToggleContext(item.id.clone()));

        let row_widget = mouse_area(
            row![main_btn, more_btn]
                .spacing(2)
                .align_y(Alignment::Center),
        )
        .on_right_press(Message::ToggleContext(item.id.clone()));

        if self.context_item_id.as_deref() == Some(&item.id) {
            let pin_label = if item.pinned { "Unpin" } else { "Pin" };
            let actions = row![
                button::text(pin_label)
                    .on_press(Message::Pin(item.id.clone(), !item.pinned)),
                button::text("Paste as Plain Text")
                    .on_press(Message::Activate(item.id.clone(), true)),
                button::destructive("Delete")
                    .on_press(Message::Remove(item.id.clone())),
            ]
            .spacing(6)
            .padding([2, 8]);
            column![row_widget, actions].spacing(2).into()
        } else {
            row_widget.into()
        }
    }
}

impl cosmic::Application for ClipboardApplet {
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = "dev.asw.CosmicExtAppletClipboard";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, _flags: Self::Flags) -> (Self, app::Task<Self::Message>) {
        let app = Self {
            core,
            popup: None,
            rectangle_tracker: None,
            rectangle: Rectangle::default(),
            query: String::new(),
            items: Vec::new(),
            paused: false,
            daemon_available: false,
            context_item_id: None,
            confirm_clear: false,
        };
        let task = app.refresh_task();
        (app, task)
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        fn dbus_changed_subscription() -> Subscription<Message> {
            Subscription::run_with("clipboard-dbus-sub", |_| {
                stream::channel(4, |mut output: mpsc::Sender<Message>| async move {
                    loop {
                        match zbus::Connection::session().await {
                            Ok(conn) => {
                                if let Ok(proxy) = MacClipboardProxy::new(&conn).await
                                    && let Ok(mut stream) = proxy.receive_changed().await
                                {
                                    let _ = output.send(Message::Refresh).await;
                                    while stream.next().await.is_some() {
                                        let _ = output.send(Message::Refresh).await;
                                    }
                                }
                            }
                            Err(err) => {
                                tracing::debug!(?err, "Clipboard D-Bus unavailable");
                            }
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    }
                })
            })
        }

        Subscription::batch([
            rectangle_tracker_subscription(0).map(|e| Message::Rectangle(e.1)),
            dbus_changed_subscription(),
        ])
    }

    fn update(&mut self, message: Self::Message) -> app::Task<Self::Message> {
        match message {
            Message::TogglePopup => {
                if let Some(p) = self.popup.take() {
                    self.context_item_id = None;
                    self.confirm_clear = false;
                    return destroy_popup(p);
                } else {
                    self.context_item_id = None;
                    self.confirm_clear = false;
                    let refresh = self.refresh_task();
                    let popup_task = cosmic::surface::surface_task(cosmic::surface::action::app_popup(
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
                    self.context_item_id = None;
                    self.confirm_clear = false;
                }
            }
            Message::Rectangle(u) => match u {
                RectangleUpdate::Rectangle(r) => self.rectangle = r.1,
                RectangleUpdate::Init(tracker) => self.rectangle_tracker = Some(tracker),
            },
            Message::SnapshotLoaded(snap) => {
                self.items = snap.items;
                self.paused = snap.paused;
                self.daemon_available = snap.daemon_available;
            }
            Message::QueryChanged(q) => {
                self.query = q;
                return self.refresh_task();
            }
            Message::Refresh => {
                return self.refresh_task();
            }
            Message::Activate(id, plain) => {
                let close = self.popup.take().map(destroy_popup).unwrap_or_else(Task::none);
                self.context_item_id = None;
                let act = cosmic::task::future(async move {
                    dbus_client::activate_item(id, plain, plain).await;
                    Message::Refresh
                })
                .map(cosmic::Action::App);
                return Task::batch([close, act]);
            }
            Message::ToggleContext(id) => {
                if self.context_item_id.as_deref() == Some(&id) {
                    self.context_item_id = None;
                } else {
                    self.context_item_id = Some(id);
                }
            }
            Message::Pin(id, pinned) => {
                self.context_item_id = None;
                return cosmic::task::future(async move {
                    dbus_client::pin_item(id, pinned).await;
                    Message::Refresh
                })
                .map(cosmic::Action::App);
            }
            Message::Remove(id) => {
                self.context_item_id = None;
                return cosmic::task::future(async move {
                    dbus_client::remove_item(id).await;
                    Message::Refresh
                })
                .map(cosmic::Action::App);
            }
            Message::ShowAll => {
                let close = self.popup.take().map(destroy_popup).unwrap_or_else(Task::none);
                let toggle = cosmic::task::future(async {
                    dbus_client::toggle_quick_paste().await;
                    Message::Refresh
                })
                .map(cosmic::Action::App);
                return Task::batch([close, toggle]);
            }
            Message::TogglePaused => {
                let next = !self.paused;
                self.paused = next;
                return cosmic::task::future(async move {
                    dbus_client::set_paused(next).await;
                    Message::Refresh
                })
                .map(cosmic::Action::App);
            }
            Message::RequestClear => {
                self.confirm_clear = true;
            }
            Message::CancelClear => {
                self.confirm_clear = false;
            }
            Message::ConfirmClear => {
                self.confirm_clear = false;
                return cosmic::task::future(async {
                    dbus_client::clear_history(true).await;
                    Message::Refresh
                })
                .map(cosmic::Action::App);
            }
            Message::OpenSettings => {
                dbus_client::spawn_settings();
                if let Some(p) = self.popup.take() {
                    return destroy_popup(p);
                }
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Self::Message> {
        let max_icon = panel_icon_size();
        let icon_size = self.core.applet.suggested_size(true).0.min(max_icon);
        let btn = mac_pill_button(
            clipboard_icon(self.paused, icon_size),
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
        let Spacing {
            space_xxs, space_s, ..
        } = theme::active().cosmic().spacing;

        let search = text_input::search_input("Search clipboard…", &self.query)
            .id(SEARCH_INPUT_ID.clone())
            .on_input(Message::QueryChanged)
            .width(Length::Fill);

        let mut list_col = column![].spacing(4).padding([0, 8]);
        if self.items.is_empty() {
            let empty_msg = if self.query.trim().is_empty() {
                if self.paused {
                    "History paused"
                } else {
                    "Clipboard history is empty"
                }
            } else {
                "No matching items"
            };
            list_col = list_col.push(
                text::caption(empty_msg)
                    .apply(container)
                    .center_x(Length::Fill)
                    .padding([16, 0]),
            );
        } else {
            for item in &self.items {
                list_col = list_col.push(self.item_row(item));
            }
        }

        let pause_label = if self.paused {
            "Resume History"
        } else {
            "Pause History"
        };

        let clear_row: Element<'_, Message> = if self.confirm_clear {
            row![
                text::body("Clear unpinned history?").width(Length::Fill),
                button::destructive("Clear").on_press(Message::ConfirmClear),
                button::text("Cancel").on_press(Message::CancelClear),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .padding([4, 16])
            .into()
        } else {
            menu_button(text::body("Clear History…"))
                .on_press(Message::RequestClear)
                .into()
        };

        let divider_fn =
            || padded_control(divider::horizontal::default()).padding([space_xxs, space_s]);

        let content = column![
            container(search).padding([8, 12]),
            scrollable(list_col)
                .apply(container)
                .max_height(360.0),
            divider_fn(),
            menu_button(
                row![
                    text::body("Show All…").width(Length::Fill),
                    text::caption("⌘V"),
                ]
                .align_y(Alignment::Center)
            )
            .on_press(Message::ShowAll),
            menu_button(text::body(pause_label)).on_press(Message::TogglePaused),
            clear_row,
            menu_button(text::body("Clipboard Settings…")).on_press(Message::OpenSettings),
        ]
        .padding([4, 0])
        .width(Length::Fixed(POPUP_WIDTH));

        mac_popover_container_sized(&self.core.applet, content, POPUP_WIDTH, POPUP_WIDTH).into()
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Message> {
        Some(Message::CloseRequested(id))
    }
}

pub fn kind_icon_name(kind: &str) -> &'static str {
    match kind.to_ascii_lowercase().as_str() {
        "image" => "image-x-generic-symbolic",
        "link" => "insert-link-symbolic",
        "files" => "folder-symbolic",
        "richtext" | "rich_text" => "x-office-document-symbolic",
        _ => "text-x-generic-symbolic",
    }
}

pub fn single_line_preview(raw: &str, max_chars: usize) -> String {
    let collapsed: String = raw
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if collapsed.is_empty() {
        return "(empty)".to_string();
    }
    if collapsed.chars().count() > max_chars {
        let mut out: String = collapsed.chars().take(max_chars.saturating_sub(1)).collect();
        out.push('…');
        out
    } else {
        collapsed
    }
}

pub fn format_relative_time(created_unix: i64, now_unix: i64) -> String {
    let diff = now_unix.saturating_sub(created_unix);
    if diff < 60 {
        "Just now".to_string()
    } else if diff < 3600 {
        format!("{}m", diff / 60)
    } else if diff < 86400 {
        format!("{}h", diff / 3600)
    } else {
        format!("{}d", diff / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_relative_time_buckets() {
        assert_eq!(format_relative_time(1000, 1020), "Just now");
        assert_eq!(format_relative_time(1000, 1180), "3m");
        assert_eq!(format_relative_time(1000, 8200), "2h");
        assert_eq!(format_relative_time(1000, 1000 + 3 * 86400), "3d");
    }

    #[test]
    fn collapses_and_truncates_preview() {
        assert_eq!(single_line_preview("  hello \n\t world  ", 32), "hello world");
        assert_eq!(single_line_preview("   ", 32), "(empty)");
        assert_eq!(single_line_preview("abcdefghij", 5), "abcd…");
    }
}
