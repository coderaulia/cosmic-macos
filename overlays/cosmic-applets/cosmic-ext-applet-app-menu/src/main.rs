// SPDX-License-Identifier: GPL-3.0-only

mod dbus_menu;
mod desktop_lookup;
mod wayland;

use std::collections::HashMap;

use cctk::{
    sctk::reexports::calloop,
    toplevel_info::ToplevelInfo,
    wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
};
use cosmic::{
    Element, Task, app,
    cosmic_config::{self, Config, CosmicConfigEntry, cosmic_config_derive::CosmicConfigEntry},
    desktop::fde::get_languages_from_env,
    iced::{
        Alignment, Background, Color, Length, Padding, Size, Subscription,
        widget::{column, row},
        window,
    },
    widget::{button, container, icon, mouse_area, scrollable, space, text},
};
use cosmic_ext_mac_applet_style::{
    PillVariant, bold_panel_font,
    icons::{checkmark_icon, chevron_left_icon, chevron_right_icon},
    mac_app_popup, mac_bar_item, mac_menu_divider, mac_menu_item, mac_menu_label_row,
    mac_pill_button, mac_popover_container_sized, mac_text_pill_button, panel_text_size,
};
use cosmic_protocols::toplevel_info::v1::client::zcosmic_toplevel_handle_v1::State as ToplevelState;
use mac_appmenu::{MenuLayout, MenuSpec};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::dbus_menu::{DbusMenuCommand, DbusMenuUpdate, RemoteMenuTarget};
use crate::desktop_lookup::{
    DesktopAppInfo, ResolvedApp, elide_to_max_width, load_system_desktop_entries,
    resolve_active_app, split_new_window_action,
};
use crate::wayland::{WaylandRequest, WaylandUpdate, WorkspaceEntry};

const APP_ID: &str = "dev.asw.CosmicExtAppletAppMenu";
const MAX_APP_PILL_WIDTH: f32 = 220.0;
const POPOVER_WIDTH: f32 = 250.0;
const REMOTE_POPOVER_MIN_WIDTH: f32 = 260.0;
const REMOTE_POPOVER_MAX_WIDTH: f32 = 340.0;
const ABOUT_APP_WINDOW_SIZE: Size = Size::new(340.0, 260.0);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, CosmicConfigEntry)]
#[version = 1]
struct AppMenuConfig {
    /// Label shown when no window is focused (defaults to "Files").
    default_app_name: String,
}

impl Default for AppMenuConfig {
    fn default() -> Self {
        Self {
            default_app_name: "Files".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuKind {
    App,
    Remote(usize),
    Window,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowMenuPage {
    Main,
    MoveToWorkspace,
}

#[derive(Debug, Clone)]
enum Message {
    ToggleMenu(MenuKind),
    HoverMenu(MenuKind),
    ActivateRemoteItem(String),
    SetWindowMenuPage(WindowMenuPage),
    OpenAboutApp,
    CloseAboutApp,
    NewWindow,
    RunDesktopAction(String),
    HideApp,
    ShowAll,
    QuitApp,
    MinimizeWindow,
    ToggleZoomWindow,
    ToggleFullscreenWindow,
    MoveToWorkspace(usize),
    CloseWindow,
    Wayland(Box<WaylandUpdate>),
    DbusMenu(Box<DbusMenuUpdate>),
    Closed(window::Id),
}

struct AppMenu {
    core: cosmic::app::Core,
    config: AppMenuConfig,
    popup: Option<window::Id>,
    active_menu: Option<MenuKind>,
    window_page: WindowMenuPage,
    about_window: Option<window::Id>,
    about_snapshot: Option<ResolvedApp>,
    desktop_entries: Vec<DesktopAppInfo>,
    toplevels: HashMap<ExtForeignToplevelHandleV1, ToplevelInfo>,
    last_active_handle: Option<ExtForeignToplevelHandleV1>,
    workspaces: Vec<WorkspaceEntry>,
    wayland_tx: Option<calloop::channel::Sender<WaylandRequest>>,
    dbus_tx: Option<mpsc::UnboundedSender<DbusMenuCommand>>,
    remote_target: Option<RemoteMenuTarget>,
    remote_bus_name: Option<String>,
    remote_layout: MenuLayout,
    last_queried_target: Option<RemoteMenuTarget>,
}

impl AppMenu {
    fn current_active_toplevel(&self) -> Option<&ToplevelInfo> {
        if let Some(active) = self.toplevels.values().find(|info| {
            info.app_id != APP_ID && info.state.contains(&ToplevelState::Activated)
        }) {
            return Some(active);
        }
        // Keep the active window pinned while a menu popup is open so Wayland popup
        // grab doesn't flip the menu to the fallback state mid-click.
        if self.popup.is_some()
            && let Some(handle) = &self.last_active_handle
        {
            return self.toplevels.get(handle);
        }
        None
    }

    fn resolved_active_app(&self) -> ResolvedApp {
        if let Some(tl) = self.current_active_toplevel() {
            resolve_active_app(
                &tl.app_id,
                &tl.title,
                &self.desktop_entries,
                &self.config.default_app_name,
            )
        } else {
            resolve_active_app("", "", &self.desktop_entries, &self.config.default_app_name)
        }
    }

    fn active_app_toplevels(&self, active: &ToplevelInfo) -> Vec<ExtForeignToplevelHandleV1> {
        self.toplevels
            .iter()
            .filter(|(_, info)| {
                if !active.app_id.is_empty() {
                    info.app_id.eq_ignore_ascii_case(&active.app_id)
                } else {
                    info.foreign_toplevel == active.foreign_toplevel
                }
            })
            .map(|(h, _)| h.clone())
            .collect()
    }

    fn sync_remote_menu_target(&mut self) {
        let target = self.current_active_toplevel().map(|tl| {
            let resolved = resolve_active_app(
                &tl.app_id,
                &tl.title,
                &self.desktop_entries,
                &self.config.default_app_name,
            );
            RemoteMenuTarget {
                app_id: tl.app_id.clone(),
                desktop_id: resolved.desktop.map(|d| d.id),
            }
        });

        if target != self.last_queried_target {
            self.last_queried_target = target.clone();
            if target.is_none() {
                self.remote_target = None;
                self.remote_bus_name = None;
                self.remote_layout.clear();
                if matches!(self.active_menu, Some(MenuKind::Remote(_))) {
                    self.active_menu = None;
                }
            }
            if let Some(tx) = &self.dbus_tx {
                let _ = tx.send(DbusMenuCommand::SetTarget(target));
            }
        }
    }

    fn remote_layout_has_window_menu(&self) -> bool {
        self.remote_layout.iter().any(|menu| {
            menu.id.eq_ignore_ascii_case("window") || menu.label.eq_ignore_ascii_case("window")
        })
    }

    fn close_popup_task(&mut self) -> app::Task<Message> {
        self.active_menu = None;
        self.window_page = WindowMenuPage::Main;
        if let Some(id) = self.popup.take() {
            return cosmic::surface::surface_task(cosmic::surface::action::destroy_popup(id));
        }
        Task::none()
    }

    fn launch_desktop_exec(&self, app_id: &str, exec: &str, terminal: bool) {
        if let Some(tx) = &self.wayland_tx {
            let _ = tx.send(WaylandRequest::TokenRequest {
                app_id: app_id.to_string(),
                exec: exec.to_string(),
                terminal,
            });
        } else {
            let exec = exec.to_string();
            tokio::spawn(async move {
                cosmic::desktop::spawn_desktop_exec(
                    exec,
                    Vec::<(&str, &str)>::new(),
                    None::<&str>,
                    terminal,
                )
                .await;
            });
        }
    }

    fn view_app_menu(&self) -> Element<'_, Message> {
        let resolved = self.resolved_active_app();
        let active_tl = self.current_active_toplevel();
        let has_active_window = active_tl.is_some();
        let has_minimized = self
            .toplevels
            .values()
            .any(|info| info.state.contains(&ToplevelState::Minimized));

        let mut items = column![
            mac_menu_item(text::body(format!("About {}", resolved.display_name)))
                .on_press(Message::OpenAboutApp),
            mac_menu_divider(),
        ]
        .padding([6, 0]);

        let can_new_window = resolved
            .desktop
            .as_ref()
            .is_some_and(|d| d.exec.is_some() || !d.actions.is_empty());
        let new_window_item = mac_menu_item(text::body("New Window"));
        items = items.push(if can_new_window {
            new_window_item.on_press(Message::NewWindow)
        } else {
            new_window_item
        });

        if let Some(desktop) = &resolved.desktop {
            let (_, extra_actions) = split_new_window_action(&desktop.actions);
            if !extra_actions.is_empty() {
                items = items.push(mac_menu_divider());
                for action in extra_actions {
                    items = items.push(
                        mac_menu_item(text::body(action.name.clone()))
                            .on_press(Message::RunDesktopAction(action.id.clone())),
                    );
                }
            }
        }

        items = items.push(mac_menu_divider());

        let hide_item = mac_menu_item(text::body(format!("Hide {}", resolved.display_name)));
        items = items.push(if has_active_window {
            hide_item.on_press(Message::HideApp)
        } else {
            hide_item
        });

        let show_all_item = mac_menu_item(text::body("Show All"));
        items = items.push(if has_minimized {
            show_all_item.on_press(Message::ShowAll)
        } else {
            show_all_item
        });

        items = items.push(mac_menu_divider());

        let quit_item = mac_menu_item(text::body(format!("Quit {}", resolved.display_name)));
        items = items.push(if has_active_window {
            quit_item.on_press(Message::QuitApp)
        } else {
            quit_item
        });

        mac_popover_container_sized(&self.core.applet, items, POPOVER_WIDTH, 280.0).into()
    }

    fn view_window_menu(&self) -> Element<'_, Message> {
        let active_tl = self.current_active_toplevel();
        let has_active = active_tl.is_some();
        let is_maximized = active_tl.is_some_and(|t| t.state.contains(&ToplevelState::Maximized));
        let is_fullscreen = active_tl.is_some_and(|t| t.state.contains(&ToplevelState::Fullscreen));

        let content: Element<'_, Message> = match self.window_page {
            WindowMenuPage::Main => {
                let minimize_item = mac_menu_item(text::body("Minimize"));
                let zoom_label = if is_maximized { "Restore Zoom" } else { "Zoom" };
                let zoom_item = mac_menu_item(mac_menu_label_row(
                    zoom_label,
                    is_maximized.then(|| checkmark_icon(12).into()),
                ));
                let fs_label = if is_fullscreen {
                    "Exit Full Screen"
                } else {
                    "Enter Full Screen"
                };
                let fs_item = mac_menu_item(text::body(fs_label));
                let move_item = mac_menu_item(mac_menu_label_row(
                    "Move to Workspace",
                    Some(chevron_right_icon(12).into()),
                ));
                let close_item = mac_menu_item(text::body("Close Window"));

                let mut items = column![].padding([6, 0]);
                items = items
                    .push(if has_active {
                        minimize_item.on_press(Message::MinimizeWindow)
                    } else {
                        minimize_item
                    })
                    .push(if has_active {
                        zoom_item.on_press(Message::ToggleZoomWindow)
                    } else {
                        zoom_item
                    })
                    .push(if has_active {
                        fs_item.on_press(Message::ToggleFullscreenWindow)
                    } else {
                        fs_item
                    })
                    .push(mac_menu_divider())
                    .push(if has_active && !self.workspaces.is_empty() {
                        move_item.on_press(Message::SetWindowMenuPage(
                            WindowMenuPage::MoveToWorkspace,
                        ))
                    } else {
                        move_item
                    })
                    .push(mac_menu_divider())
                    .push(if has_active {
                        close_item.on_press(Message::CloseWindow)
                    } else {
                        close_item
                    });

                items.into()
            }
            WindowMenuPage::MoveToWorkspace => {
                let back = mac_menu_item(
                    row![
                        chevron_left_icon(12),
                        text::body("Move to Workspace").font(bold_panel_font()),
                    ]
                    .align_y(Alignment::Center)
                    .spacing(6),
                )
                .on_press(Message::SetWindowMenuPage(WindowMenuPage::Main));

                let mut items = column![back, mac_menu_divider()].padding([6, 0]);
                for (idx, ws) in self.workspaces.iter().enumerate() {
                    let on_ws = active_tl.is_some_and(|tl| tl.workspace.contains(&ws.handle));
                    items = items.push(
                        mac_menu_item(mac_menu_label_row(
                            ws.name.clone(),
                            on_ws.then(|| checkmark_icon(12).into()),
                        ))
                        .on_press(Message::MoveToWorkspace(idx)),
                    );
                }
                items.into()
            }
        };

        mac_popover_container_sized(&self.core.applet, content, 230.0, 260.0).into()
    }

    fn view_remote_menu(&self, idx: usize) -> Element<'_, Message> {
        let Some(menu) = self.remote_layout.get(idx) else {
            return self.view_app_menu();
        };

        render_remote_menu_popover(&self.core.applet, menu)
    }

    fn view_about_app_window(&self) -> Element<'_, Message> {
        let resolved = self
            .about_snapshot
            .as_ref()
            .cloned()
            .unwrap_or_else(|| self.resolved_active_app());

        let icon_widget: Element<'_, Message> = resolved
            .desktop
            .as_ref()
            .and_then(|d| d.icon.as_deref())
            .map(|name| icon::from_name(name).size(48).icon().into())
            .unwrap_or_else(|| {
                icon::from_name("application-x-executable")
                    .size(48)
                    .icon()
                    .into()
            });

        let mut body = column![
            container(icon_widget).center_x(Length::Fill),
            container(text::title3(resolved.display_name.clone())).center_x(Length::Fill),
        ]
        .spacing(8)
        .padding(20);

        if let Some(desktop) = &resolved.desktop {
            if let Some(version) = &desktop.version {
                body = body.push(
                    container(text::caption(format!("Version {version}"))).center_x(Length::Fill),
                );
            }
            if let Some(comment) = &desktop.comment {
                body = body.push(container(text::body(comment.clone())).center_x(Length::Fill));
            }
            body = body.push(container(text::caption(desktop.id.clone())).center_x(Length::Fill));
        }

        body = body
            .push(space::vertical().height(Length::Fill))
            .push(container(button::suggested("Close").on_press(Message::CloseAboutApp)).center_x(Length::Fill));

        container(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|theme: &cosmic::Theme| {
                let cosmic = theme.cosmic();
                let bg = cosmic.background(theme.transparent);
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(Color::from(bg.base))),
                    text_color: Some(Color::from(bg.on)),
                    icon_color: Some(Color::from(bg.on)),
                    ..Default::default()
                }
            })
            .into()
    }
}

impl cosmic::Application for AppMenu {
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = APP_ID;

    fn core(&self) -> &cosmic::app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::app::Core {
        &mut self.core
    }

    fn init(core: cosmic::app::Core, _flags: ()) -> (Self, app::Task<Self::Message>) {
        let config = Config::new(APP_ID, AppMenuConfig::VERSION)
            .ok()
            .and_then(|c| AppMenuConfig::get_entry(&c).ok())
            .unwrap_or_default();

        let locales = get_languages_from_env();
        let desktop_entries = load_system_desktop_entries(&locales);

        let app = Self {
            core,
            config,
            popup: None,
            active_menu: None,
            window_page: WindowMenuPage::Main,
            about_window: None,
            about_snapshot: None,
            desktop_entries,
            toplevels: HashMap::new(),
            last_active_handle: None,
            workspaces: Vec::new(),
            wayland_tx: None,
            dbus_tx: None,
            remote_target: None,
            remote_bus_name: None,
            remote_layout: Vec::new(),
            last_queried_target: None,
        };
        (app, Task::none())
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Message> {
        Some(Message::Closed(id))
    }

    fn update(&mut self, message: Self::Message) -> app::Task<Self::Message> {
        match message {
            Message::ToggleMenu(kind) => {
                if self.popup.is_some() && self.active_menu == Some(kind) {
                    return self.close_popup_task();
                }
                if self.popup.is_some() {
                    // Switch directly between menu pills inside the open popup.
                    self.active_menu = Some(kind);
                    self.window_page = WindowMenuPage::Main;
                    return Task::none();
                }
                self.active_menu = Some(kind);
                self.window_page = WindowMenuPage::Main;
                return cosmic::surface::surface_task(mac_app_popup(
                    |app: &mut AppMenu, id| {
                        app.popup.replace(id);
                    },
                    None,
                    None,
                ));
            }
            Message::HoverMenu(kind) => {
                if self.popup.is_some() && self.active_menu != Some(kind) {
                    self.active_menu = Some(kind);
                    self.window_page = WindowMenuPage::Main;
                }
            }
            Message::ActivateRemoteItem(item_id) => {
                if let (Some(bus_name), Some(tx)) = (&self.remote_bus_name, &self.dbus_tx) {
                    let _ = tx.send(DbusMenuCommand::Activate {
                        bus_name: bus_name.clone(),
                        item_id,
                    });
                }
                return self.close_popup_task();
            }
            Message::SetWindowMenuPage(page) => {
                self.window_page = page;
            }
            Message::OpenAboutApp => {
                self.about_snapshot = Some(self.resolved_active_app());
                let close_task = self.close_popup_task();
                if self.about_window.is_some() {
                    return close_task;
                }
                let (id, action) = cosmic::surface::action::app_window(
                    |_| Default::default(),
                    |_app: &mut AppMenu| window::Settings {
                        size: ABOUT_APP_WINDOW_SIZE,
                        resizable: false,
                        decorations: true,
                        exit_on_close_request: false,
                        ..Default::default()
                    },
                    None,
                );
                self.about_window = Some(id);
                return Task::batch([close_task, cosmic::surface::surface_task(action)]);
            }
            Message::CloseAboutApp => {
                self.about_snapshot = None;
                if let Some(id) = self.about_window.take() {
                    return cosmic::surface::surface_task(
                        cosmic::surface::action::destroy_window(id),
                    );
                }
            }
            Message::NewWindow => {
                let resolved = self.resolved_active_app();
                if let Some(desktop) = &resolved.desktop {
                    let (new_win, _) = split_new_window_action(&desktop.actions);
                    if let Some(action) = new_win {
                        self.launch_desktop_exec(&desktop.id, &action.exec, desktop.terminal);
                    } else if let Some(exec) = &desktop.exec {
                        self.launch_desktop_exec(&desktop.id, exec, desktop.terminal);
                    }
                }
                return self.close_popup_task();
            }
            Message::RunDesktopAction(action_id) => {
                let resolved = self.resolved_active_app();
                if let Some(desktop) = &resolved.desktop
                    && let Some(action) = desktop.actions.iter().find(|a| a.id == action_id)
                {
                    self.launch_desktop_exec(&desktop.id, &action.exec, desktop.terminal);
                }
                return self.close_popup_task();
            }
            Message::HideApp => {
                if let Some(active) = self.current_active_toplevel().cloned() {
                    let handles = self.active_app_toplevels(&active);
                    if let Some(tx) = &self.wayland_tx {
                        for h in handles {
                            let _ = tx.send(WaylandRequest::Minimize(h));
                        }
                    }
                }
                return self.close_popup_task();
            }
            Message::ShowAll => {
                let minimized: Vec<_> = self
                    .toplevels
                    .iter()
                    .filter(|(_, info)| info.state.contains(&ToplevelState::Minimized))
                    .map(|(h, _)| h.clone())
                    .collect();
                if let Some(tx) = &self.wayland_tx {
                    for h in minimized {
                        let _ = tx.send(WaylandRequest::Unminimize(h));
                    }
                }
                return self.close_popup_task();
            }
            Message::QuitApp => {
                if let Some(active) = self.current_active_toplevel().cloned() {
                    let handles = self.active_app_toplevels(&active);
                    if let Some(tx) = &self.wayland_tx {
                        for h in handles {
                            let _ = tx.send(WaylandRequest::Close(h));
                        }
                    }
                }
                return self.close_popup_task();
            }
            Message::MinimizeWindow => {
                if let Some(active) = self.current_active_toplevel()
                    && let Some(tx) = &self.wayland_tx
                {
                    let _ = tx.send(WaylandRequest::Minimize(active.foreign_toplevel.clone()));
                }
                return self.close_popup_task();
            }
            Message::ToggleZoomWindow => {
                if let Some(active) = self.current_active_toplevel()
                    && let Some(tx) = &self.wayland_tx
                {
                    let req = if active.state.contains(&ToplevelState::Maximized) {
                        WaylandRequest::UnsetMaximized(active.foreign_toplevel.clone())
                    } else {
                        WaylandRequest::SetMaximized(active.foreign_toplevel.clone())
                    };
                    let _ = tx.send(req);
                }
                return self.close_popup_task();
            }
            Message::ToggleFullscreenWindow => {
                if let Some(active) = self.current_active_toplevel()
                    && let Some(tx) = &self.wayland_tx
                {
                    let req = if active.state.contains(&ToplevelState::Fullscreen) {
                        WaylandRequest::UnsetFullscreen(active.foreign_toplevel.clone())
                    } else {
                        WaylandRequest::SetFullscreen(active.foreign_toplevel.clone())
                    };
                    let _ = tx.send(req);
                }
                return self.close_popup_task();
            }
            Message::MoveToWorkspace(idx) => {
                if let (Some(active), Some(ws)) =
                    (self.current_active_toplevel(), self.workspaces.get(idx))
                    && let Some(tx) = &self.wayland_tx
                {
                    let _ = tx.send(WaylandRequest::MoveToWorkspace {
                        toplevel: active.foreign_toplevel.clone(),
                        workspace: ws.handle.clone(),
                    });
                }
                return self.close_popup_task();
            }
            Message::CloseWindow => {
                if let Some(active) = self.current_active_toplevel()
                    && let Some(tx) = &self.wayland_tx
                {
                    let _ = tx.send(WaylandRequest::Close(active.foreign_toplevel.clone()));
                }
                return self.close_popup_task();
            }
            Message::Wayland(upd) => match *upd {
                WaylandUpdate::Init(tx) => {
                    self.wayland_tx = Some(tx);
                }
                WaylandUpdate::ToplevelAdd(info) | WaylandUpdate::ToplevelUpdate(info) => {
                    if info.app_id != APP_ID && info.state.contains(&ToplevelState::Activated) {
                        self.last_active_handle = Some(info.foreign_toplevel.clone());
                    }
                    self.toplevels.insert(info.foreign_toplevel.clone(), info);
                    self.sync_remote_menu_target();
                }
                WaylandUpdate::ToplevelRemove(handle) => {
                    self.toplevels.remove(&handle);
                    if self.last_active_handle.as_ref() == Some(&handle) {
                        self.last_active_handle = None;
                    }
                    self.sync_remote_menu_target();
                }
                WaylandUpdate::Workspaces(ws) => {
                    self.workspaces = ws;
                }
                WaylandUpdate::ActivationToken {
                    token,
                    exec,
                    terminal,
                } => {
                    tokio::spawn(async move {
                        let mut envs = Vec::new();
                        if let Some(t) = &token {
                            envs.push(("XDG_ACTIVATION_TOKEN", t.as_str()));
                            envs.push(("DESKTOP_STARTUP_ID", t.as_str()));
                        }
                        cosmic::desktop::spawn_desktop_exec(exec, envs, None::<&str>, terminal)
                            .await;
                    });
                }
                WaylandUpdate::Finished => {
                    self.wayland_tx = None;
                }
            },
            Message::DbusMenu(upd) => match *upd {
                DbusMenuUpdate::Init(tx) => {
                    self.dbus_tx = Some(tx);
                    self.last_queried_target = None;
                    self.sync_remote_menu_target();
                }
                DbusMenuUpdate::Layout {
                    target,
                    bus_name,
                    layout,
                } => {
                    if self.last_queried_target.as_ref() == Some(&target) {
                        self.remote_target = Some(target);
                        self.remote_bus_name = Some(bus_name);
                        self.remote_layout = layout;
                        if let Some(MenuKind::Remote(idx)) = self.active_menu
                            && idx >= self.remote_layout.len()
                        {
                            self.active_menu = Some(MenuKind::App);
                        }
                    }
                }
                DbusMenuUpdate::Cleared { target } => {
                    if self.last_queried_target.as_ref() == Some(&target) {
                        self.remote_target = None;
                        self.remote_bus_name = None;
                        self.remote_layout.clear();
                        if matches!(self.active_menu, Some(MenuKind::Remote(_))) {
                            self.active_menu = Some(MenuKind::App);
                        }
                    }
                }
            },
            Message::Closed(id) => {
                if self.popup == Some(id) {
                    self.popup = None;
                    self.active_menu = None;
                    self.window_page = WindowMenuPage::Main;
                    self.sync_remote_menu_target();
                } else if self.about_window == Some(id) {
                    self.about_window = None;
                    self.about_snapshot = None;
                }
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let resolved = self.resolved_active_app();
        let elided = elide_to_max_width(&resolved.display_name, MAX_APP_PILL_WIDTH);
        let is_app_open = self.popup.is_some() && self.active_menu == Some(MenuKind::App);
        let is_window_open = self.popup.is_some() && self.active_menu == Some(MenuKind::Window);

        let app_label = text(elided).size(panel_text_size()).font(bold_panel_font());
        let app_pill = mouse_area(
            mac_pill_button(
                container(app_label).max_width(MAX_APP_PILL_WIDTH),
                is_app_open,
                PillVariant::Text,
            )
            .on_press_down(Message::ToggleMenu(MenuKind::App)),
        )
        .on_enter(Message::HoverMenu(MenuKind::App));

        let mut pills = row![app_pill]
            .spacing(2)
            .align_y(Alignment::Center);

        if !self.remote_layout.is_empty() {
            for (idx, menu) in self.remote_layout.iter().enumerate() {
                let kind = MenuKind::Remote(idx);
                let is_open = self.popup.is_some() && self.active_menu == Some(kind);
                let pill = mac_text_pill_button(&menu.label, false, is_open)
                    .on_press_down(Message::ToggleMenu(kind));
                pills = pills.push(mouse_area(pill).on_enter(Message::HoverMenu(kind)));
            }
            if !self.remote_layout_has_window_menu() {
                let window_pill = mac_text_pill_button("Window", false, is_window_open)
                    .on_press_down(Message::ToggleMenu(MenuKind::Window));
                pills = pills.push(
                    mouse_area(window_pill).on_enter(Message::HoverMenu(MenuKind::Window)),
                );
            }
        } else {
            let window_pill = mac_text_pill_button("Window", false, is_window_open)
                .on_press_down(Message::ToggleMenu(MenuKind::Window));
            pills = pills.push(
                mouse_area(window_pill).on_enter(Message::HoverMenu(MenuKind::Window)),
            );
        }

        self.core
            .applet
            .autosize_window(mac_bar_item(pills, Padding::ZERO))
            .into()
    }

    fn view_window(&self, id: window::Id) -> Element<'_, Message> {
        if self.popup == Some(id) {
            match self.active_menu {
                Some(MenuKind::Window) => self.view_window_menu(),
                Some(MenuKind::Remote(idx)) => self.view_remote_menu(idx),
                _ => self.view_app_menu(),
            }
        } else if self.about_window == Some(id) {
            self.view_about_app_window()
        } else {
            text("").into()
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            wayland::subscription().map(|u| Message::Wayland(Box::new(u))),
            dbus_menu::subscription().map(|u| Message::DbusMenu(Box::new(u))),
        ])
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}

fn render_remote_menu_popover<'a>(
    applet: &cosmic::applet::Context,
    menu: &'a MenuSpec,
) -> Element<'a, Message> {
    let any_checked = menu.items.iter().any(|item| !item.is_separator() && item.checked);
    let mut items = column![].padding([6, 0]);

    for item in &menu.items {
        if item.is_separator() {
            items = items.push(mac_menu_divider());
            continue;
        }

        let mut row_items = row![].align_y(Alignment::Center).spacing(8);
        if any_checked {
            let check_slot: Element<'_, Message> = if item.checked {
                checkmark_icon(12).into()
            } else {
                space::horizontal().width(12).into()
            };
            row_items = row_items.push(container(check_slot).width(Length::Fixed(14.0)));
        }

        row_items = row_items
            .push(text::body(&item.label))
            .push(space::horizontal().width(Length::Fill));

        if !item.shortcut.is_empty() {
            row_items = row_items.push(text::caption(&item.shortcut).class(cosmic::theme::Text::Default));
        }

        let btn = mac_menu_item(row_items);
        items = items.push(if item.enabled {
            btn.on_press(Message::ActivateRemoteItem(item.id.clone()))
        } else {
            btn
        });
    }

    let content: Element<'_, Message> = if menu.items.len() > 16 {
        scrollable(items).height(Length::Shrink).into()
    } else {
        items.into()
    };

    mac_popover_container_sized(
        applet,
        content,
        REMOTE_POPOVER_MIN_WIDTH,
        REMOTE_POPOVER_MAX_WIDTH,
    )
    .into()
}

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt::init();
    let _ = tracing_log::LogTracer::init();
    cosmic::applet::run::<AppMenu>(())
}
