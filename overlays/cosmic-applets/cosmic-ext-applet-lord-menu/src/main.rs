// SPDX-License-Identifier: GPL-3.0-only

mod cosmic_session;
mod power;
mod processes;
mod session_manager;
mod sysinfo;
mod wayland;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use cctk::{
    sctk::{output::OutputInfo, reexports::calloop},
    toplevel_info::ToplevelInfo,
    wayland_client::protocol::wl_output,
    wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
};
use cosmic::{
    Element, Task, app,
    applet::token::subscription::{TokenRequest, TokenUpdate, activation_token_subscription},
    cosmic_config::{self, Config, CosmicConfigEntry, cosmic_config_derive::CosmicConfigEntry},
    desktop::fde::{self, DesktopEntry},
    iced::{
        Alignment, Background, Border, Color, Length, Padding, Size, Subscription,
        alignment::Horizontal,
        widget::{column, row},
        window,
    },
    theme,
    widget::{button, container, icon, scrollable, space, text},
};
use cosmic_ext_mac_applet_style::{
    PillVariant, bold_panel_font,
    icons::{chevron_left_icon, chevron_right_icon, lord_logo_icon},
    mac_app_popup, mac_bar_item, mac_menu_divider, mac_menu_item, mac_menu_label_row,
    mac_pill_button, mac_popover_container_sized, panel_icon_size,
};
use serde::{Deserialize, Serialize};

use crate::power::{PowerAction, current_username};
use crate::processes::{
    ProcEntry, RecentItem, RecentTarget, force_quit_pids, list_user_processes,
    load_recent_xbel_items, match_pid_candidates,
};
use crate::sysinfo::{SystemInfo, collect_system_info};
use crate::wayland::{WaylandRequest, WaylandUpdate};

const APP_ID: &str = "dev.asw.CosmicExtAppletLordMenu";
const POPOVER_WIDTH: f32 = 260.0;
const ABOUT_WINDOW_SIZE: Size = Size::new(400.0, 380.0);
const FORCE_QUIT_WINDOW_SIZE: Size = Size::new(400.0, 400.0);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, CosmicConfigEntry)]
#[version = 1]
struct LordMenuConfig {
    /// Optional XDG icon name overriding the distribution logo from /etc/os-release.
    logo_icon: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PopoverPage {
    Main,
    RecentItems,
}

#[derive(Debug, Clone)]
struct RunningAppGroup {
    app_id: String,
    display_name: String,
    icon_name: Option<String>,
    handles: Vec<ExtForeignToplevelHandleV1>,
    pids: Vec<u32>,
}

#[derive(Debug, Clone)]
enum Message {
    TogglePopup,
    PopupPage(PopoverPage),
    OpenAbout,
    CloseAbout,
    OpenForceQuit,
    CloseForceQuit,
    SelectForceQuitApp(String),
    QuitSelectedApp,
    RequestForceQuitConfirm,
    CancelForceQuitConfirm,
    ConfirmForceQuitApp,
    LaunchCommand(String),
    LaunchRecentItem(usize),
    Power(PowerAction),
    PowerResult(Result<(), String>),
    Wayland(Box<WaylandUpdate>),
    Token(TokenUpdate),
    Closed(window::Id),
}

struct LordMenu {
    core: cosmic::app::Core,
    config: LordMenuConfig,
    popup: Option<window::Id>,
    popover_page: PopoverPage,
    about_window: Option<window::Id>,
    force_quit_window: Option<window::Id>,
    has_app_store: bool,
    username: String,
    system_info: SystemInfo,
    outputs: HashMap<wl_output::WlOutput, OutputInfo>,
    toplevels: HashMap<ExtForeignToplevelHandleV1, ToplevelInfo>,
    desktop_entries: Vec<DesktopEntry>,
    session_recent_apps: Vec<RecentItem>,
    recent_items_cache: Vec<RecentItem>,
    running_apps: Vec<RunningAppGroup>,
    selected_force_quit_app: Option<String>,
    confirm_force_quit: bool,
    wayland_tx: Option<calloop::channel::Sender<WaylandRequest>>,
    token_tx: Option<calloop::channel::Sender<TokenRequest>>,
    /// The running distribution's logo, found once at start-up.
    os_logo: Option<PathBuf>,
}

/// Find the distribution's logo: `LOGO=` (or `ID=`) from `/etc/os-release`
/// looked up as an icon file, then the generic "start-here" symbol.
fn find_os_logo() -> Option<PathBuf> {
    let release = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .unwrap_or_default();
    let field = |key: &str| {
        release.lines().find_map(|line| {
            let value = line.strip_prefix(key)?.strip_prefix('=')?;
            let value = value.trim().trim_matches('"').trim_matches('\'');
            // Icon names only: no path separators or parent references.
            (!value.is_empty() && !value.contains('/') && !value.contains(".."))
                .then(|| value.to_owned())
        })
    };
    let mut names = Vec::new();
    if let Some(logo) = field("LOGO") {
        names.push(format!("{logo}-symbolic"));
        names.push(logo);
    }
    if let Some(id) = field("ID") {
        names.push(format!("distributor-logo-{id}-symbolic"));
        names.push(format!("distributor-logo-{id}"));
        names.push(id);
    }
    let dirs = [
        "/usr/share/icons/hicolor/scalable/apps",
        "/usr/share/icons/hicolor/scalable/places",
        "/usr/share/icons/hicolor/symbolic/apps",
        "/usr/share/icons",
        "/usr/share/pixmaps",
        "/usr/share/icons/hicolor/256x256/apps",
        "/usr/share/icons/hicolor/48x48/apps",
    ];
    for name in &names {
        for dir in dirs {
            for ext in ["svg", "png"] {
                let path = PathBuf::from(dir).join(format!("{name}.{ext}"));
                if path.is_file() {
                    return Some(path);
                }
            }
        }
    }
    ["/usr/share/icons/Adwaita/symbolic/places/start-here-symbolic.svg"]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

impl LordMenu {
    fn logo_widget(&self) -> cosmic::widget::Icon {
        let size = panel_icon_size();
        if let Some(name) = self
            .config
            .logo_icon
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            icon::from_name(name)
                .symbolic(true)
                .size(size)
                .icon()
        } else if let Some(path) = self.os_logo.as_ref() {
            // Monochrome like the other bar icons: an SVG is tinted to the
            // panel text colour.
            icon::from_path(path.clone())
                .symbolic(path.extension().is_some_and(|e| e == "svg"))
                .icon()
                .size(size)
        } else {
            lord_logo_icon(size)
        }
    }

    fn spawn_with_token(&self, exec: String) -> Task<cosmic::Action<Message>> {
        if let Some(tx) = self.token_tx.as_ref() {
            let _ = tx.send(TokenRequest {
                app_id: APP_ID.to_string(),
                exec: exec.clone(),
            });
            Task::none()
        } else {
            spawn_shell_command(&exec, None);
            Task::none()
        }
    }

    fn close_popup_task(&mut self) -> app::Task<Message> {
        if let Some(id) = self.popup.take() {
            self.popover_page = PopoverPage::Main;
            return cosmic::surface::surface_task(cosmic::surface::action::destroy_popup(id));
        }
        Task::none()
    }

    fn refresh_recent_items(&mut self) {
        let mut combined = self.session_recent_apps.clone();
        for item in load_recent_xbel_items(10) {
            if combined.len() >= 10 {
                break;
            }
            if !combined.iter().any(|existing| existing.label == item.label) {
                combined.push(item);
            }
        }
        self.recent_items_cache = combined;
    }

    fn record_session_app(&mut self, info: &ToplevelInfo) {
        if info.app_id.is_empty() || info.app_id == APP_ID {
            return;
        }
        let (display_name, _, exec, terminal) = self.resolve_desktop(&info.app_id, &info.title);
        let Some(exec) = exec else {
            return;
        };
        let label = display_name;
        self.session_recent_apps.retain(|item| item.label != label);
        self.session_recent_apps.insert(
            0,
            RecentItem {
                label,
                target: RecentTarget::AppExec { exec, terminal },
            },
        );
        self.session_recent_apps.truncate(10);
    }

    fn resolve_desktop(
        &self,
        app_id: &str,
        title: &str,
    ) -> (String, Option<String>, Option<String>, bool) {
        if let Some(entry) = find_desktop_entry(&self.desktop_entries, app_id) {
            let locales: Vec<String> = Vec::new();
            let name = entry
                .name(&locales)
                .map(|c| c.into_owned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| fallback_app_name(app_id, title));
            let icon = entry.icon().map(ToString::to_string);
            let exec = entry.exec().map(ToString::to_string);
            let terminal = entry.terminal();
            return (name, icon, exec, terminal);
        }
        (fallback_app_name(app_id, title), None, None, false)
    }

    fn refresh_running_apps(&mut self) {
        let procs: Vec<ProcEntry> = list_user_processes();
        let mut grouped: BTreeMap<String, RunningAppGroup> = BTreeMap::new();

        for (handle, info) in &self.toplevels {
            if info.app_id == APP_ID {
                continue;
            }
            let key = if info.app_id.is_empty() {
                info.title.clone()
            } else {
                info.app_id.clone()
            };
            let (display_name, icon_name, exec, _) =
                self.resolve_desktop(&info.app_id, &info.title);
            let entry = grouped.entry(key.clone()).or_insert_with(|| {
                let pids = match_pid_candidates(&info.app_id, exec.as_deref(), &procs);
                RunningAppGroup {
                    app_id: key,
                    display_name,
                    icon_name,
                    handles: Vec::new(),
                    pids,
                }
            });
            entry.handles.push(handle.clone());
        }

        let mut list: Vec<RunningAppGroup> = grouped.into_values().collect();
        list.sort_by_key(|a| a.display_name.to_lowercase());
        self.running_apps = list;

        if let Some(sel) = &self.selected_force_quit_app {
            if !self.running_apps.iter().any(|g| &g.app_id == sel) {
                self.selected_force_quit_app = self.running_apps.first().map(|g| g.app_id.clone());
                self.confirm_force_quit = false;
            }
        } else {
            self.selected_force_quit_app = self.running_apps.first().map(|g| g.app_id.clone());
        }
    }

    fn view_popover(&self) -> Element<'_, Message> {
        let content: Element<'_, Message> = match self.popover_page {
            PopoverPage::Main => {
                let mut items = column![
                    mac_menu_item(text::body("About This Desktop"))
                        .on_press(Message::OpenAbout),
                    mac_menu_divider(),
                    mac_menu_item(text::body("System Settings…"))
                        .on_press(Message::LaunchCommand("cosmic-settings".to_string())),
                ];

                if self.has_app_store {
                    items = items.push(
                        mac_menu_item(text::body("App Store…"))
                            .on_press(Message::LaunchCommand("cosmic-store".to_string())),
                    );
                }

                items = items
                    .push(mac_menu_divider())
                    .push(
                        mac_menu_item(mac_menu_label_row(
                            "Recent Items",
                            Some(chevron_right_icon(12).into()),
                        ))
                        .on_press(Message::PopupPage(PopoverPage::RecentItems)),
                    )
                    .push(mac_menu_divider())
                    .push(
                        mac_menu_item(text::body("Force Quit…"))
                            .on_press(Message::OpenForceQuit),
                    )
                    .push(mac_menu_divider())
                    .push(
                        mac_menu_item(text::body("Sleep"))
                            .on_press(Message::Power(PowerAction::Sleep)),
                    )
                    .push(
                        mac_menu_item(text::body("Restart…"))
                            .on_press(Message::Power(PowerAction::Restart)),
                    )
                    .push(
                        mac_menu_item(text::body("Shut Down…"))
                            .on_press(Message::Power(PowerAction::ShutDown)),
                    )
                    .push(mac_menu_divider())
                    .push(
                        mac_menu_item(mac_menu_label_row(
                            "Lock Screen",
                            Some(text::caption("⌃⌘Q").into()),
                        ))
                        .on_press(Message::Power(PowerAction::LockScreen)),
                    )
                    .push(
                        mac_menu_item(mac_menu_label_row(
                            format!("Log Out {}…", self.username),
                            Some(text::caption("⇧⌘Q").into()),
                        ))
                        .on_press(Message::Power(PowerAction::LogOut)),
                    );

                items.padding([6, 0]).into()
            }
            PopoverPage::RecentItems => {
                let back_btn = mac_menu_item(
                    row![
                        chevron_left_icon(12),
                        text::body("Recent Items").font(bold_panel_font()),
                    ]
                    .align_y(Alignment::Center)
                    .spacing(6),
                )
                .on_press(Message::PopupPage(PopoverPage::Main));

                let mut list = column![back_btn, mac_menu_divider()].padding([6, 0]);
                if self.recent_items_cache.is_empty() {
                    list = list.push(
                        container(text::caption("No recent items")).padding([8, 16]),
                    );
                } else {
                    let mut inner = column![];
                    for (idx, item) in self.recent_items_cache.iter().enumerate() {
                        inner = inner.push(
                            mac_menu_item(text::body(item.label.clone()))
                                .on_press(Message::LaunchRecentItem(idx)),
                        );
                    }
                    list = list.push(scrollable(inner).height(Length::Shrink));
                }
                list.into()
            }
        };

        mac_popover_container_sized(&self.core.applet, content, 250.0, POPOVER_WIDTH).into()
    }

    fn view_about_window(&self) -> Element<'_, Message> {
        let info = &self.system_info;
        let info_row = |label: &'static str, value: String| {
            row![
                container(text::body(label))
                    .width(Length::Fixed(108.0))
                    .align_x(Horizontal::Right),
                text::body(value),
            ]
            .spacing(12)
            .align_y(Alignment::Center)
        };

        let body = column![
            container(self.logo_widget().size(48))
                .center_x(Length::Fill)
                .padding([4, 0]),
            container(text::title3(info.distro.clone()))
                .center_x(Length::Fill),
            container(text::caption(info.cosmic_version.clone()))
                .center_x(Length::Fill),
            space::vertical().height(Length::Fixed(8.0)),
            info_row("Hostname", info.hostname.clone()),
            info_row("Processor", info.cpu.clone()),
            info_row("Memory", info.memory.clone()),
            info_row("Graphics", info.gpu.clone()),
            info_row("Display", info.display.clone()),
            space::vertical().height(Length::Fill),
            row![
                button::standard("More Info…")
                    .on_press(Message::LaunchCommand("cosmic-settings about".to_string())),
                space::horizontal().width(Length::Fill),
                button::suggested("Close").on_press(Message::CloseAbout),
            ]
            .align_y(Alignment::Center),
        ]
        .spacing(8)
        .padding(20);

        window_surface_container(body)
    }

    fn view_force_quit_window(&self) -> Element<'_, Message> {
        let selected_group = self
            .selected_force_quit_app
            .as_deref()
            .and_then(|id| self.running_apps.iter().find(|g| g.app_id == id));

        let mut list_col = column![].spacing(4);
        if self.running_apps.is_empty() {
            list_col = list_col.push(
                container(text::body("No running applications found."))
                    .padding(16)
                    .center_x(Length::Fill),
            );
        } else {
            for group in &self.running_apps {
                let is_selected = self.selected_force_quit_app.as_deref() == Some(&group.app_id);
                let icon_widget: Element<'_, Message> = if let Some(name) = &group.icon_name {
                    icon::from_name(name.as_str()).size(20).icon().into()
                } else {
                    icon::from_name("application-x-executable")
                        .size(20)
                        .icon()
                        .into()
                };
                let subtitle = if group.pids.is_empty() {
                    format!("{} window(s)", group.handles.len())
                } else {
                    format!(
                        "{} window(s) • PID {}",
                        group.handles.len(),
                        group.pids[0]
                    )
                };
                let item_row = row![
                    icon_widget,
                    column![
                        text::body(group.display_name.clone()),
                        text::caption(subtitle),
                    ]
                    .spacing(2),
                    space::horizontal().width(Length::Fill),
                ]
                .spacing(10)
                .align_y(Alignment::Center);

                list_col = list_col.push(
                    button::custom(item_row)
                        .width(Length::Fill)
                        .padding([8, 10])
                        .selected(is_selected)
                        .class(theme::Button::ListItem([8.0; 4]))
                        .on_press(Message::SelectForceQuitApp(group.app_id.clone())),
                );
            }
        }

        let confirm_banner: Option<Element<'_, Message>> =
            if self.confirm_force_quit {
                selected_group.map(|g| {
                    container(
                        column![
                            text::body(format!(
                                "Force quit \"{}\"? Unsaved changes may be lost.",
                                g.display_name
                            )),
                            row![
                                space::horizontal().width(Length::Fill),
                                button::standard("Cancel")
                                    .on_press(Message::CancelForceQuitConfirm),
                                button::destructive("Confirm Force Quit")
                                    .on_press(Message::ConfirmForceQuitApp),
                            ]
                            .spacing(8),
                        ]
                        .spacing(8),
                    )
                    .padding(10)
                    .style(confirm_box_style)
                    .into()
                })
            } else {
                None
            };

        let mut body = column![
            text::title4("Force Quit Applications"),
            text::caption("If an app doesn't respond, select it and click Force Quit."),
            container(scrollable(list_col).height(Length::Fill))
                .height(Length::Fill)
                .padding(6)
                .style(list_box_style),
        ]
        .spacing(10)
        .padding(16);

        if let Some(banner) = confirm_banner {
            body = body.push(banner);
        }

        let mut actions = row![
            button::standard("Close").on_press(Message::CloseForceQuit),
            space::horizontal().width(Length::Fill),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        if selected_group.is_some() {
            actions = actions
                .push(button::standard("Quit").on_press(Message::QuitSelectedApp))
                .push(button::destructive("Force Quit").on_press(Message::RequestForceQuitConfirm));
        } else {
            actions = actions
                .push(button::standard("Quit"))
                .push(button::destructive("Force Quit"));
        }

        body = body.push(actions);
        window_surface_container(body)
    }
}

impl cosmic::Application for LordMenu {
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
        let config = Config::new(APP_ID, LordMenuConfig::VERSION)
            .ok()
            .and_then(|c| LordMenuConfig::get_entry(&c).ok())
            .unwrap_or_default();

        let desktop_entries: Vec<DesktopEntry> = fde::Iter::new(fde::default_paths())
            .filter_map(|p| DesktopEntry::from_path(p, None::<&[&str]>).ok())
            .collect();

        let mut app = Self {
            core,
            config,
            popup: None,
            popover_page: PopoverPage::Main,
            about_window: None,
            force_quit_window: None,
            has_app_store: is_binary_installed("cosmic-store"),
            username: current_username(),
            system_info: collect_system_info(&[]),
            outputs: HashMap::new(),
            toplevels: HashMap::new(),
            desktop_entries,
            session_recent_apps: Vec::new(),
            recent_items_cache: Vec::new(),
            running_apps: Vec::new(),
            selected_force_quit_app: None,
            confirm_force_quit: false,
            wayland_tx: None,
            token_tx: None,
            os_logo: find_os_logo(),
        };
        app.refresh_recent_items();
        (app, Task::none())
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Message> {
        Some(Message::Closed(id))
    }

    fn update(&mut self, message: Self::Message) -> app::Task<Self::Message> {
        match message {
            Message::TogglePopup => {
                if let Some(id) = self.popup.take() {
                    self.popover_page = PopoverPage::Main;
                    return cosmic::surface::surface_task(cosmic::surface::action::destroy_popup(id));
                }
                self.has_app_store = is_binary_installed("cosmic-store");
                self.popover_page = PopoverPage::Main;
                self.refresh_recent_items();
                return cosmic::surface::surface_task(mac_app_popup(
                    |app: &mut LordMenu, id| {
                        app.popup.replace(id);
                    },
                    Some(POPOVER_WIDTH),
                    None,
                ));
            }
            Message::PopupPage(page) => {
                if page == PopoverPage::RecentItems {
                    self.refresh_recent_items();
                }
                self.popover_page = page;
            }
            Message::OpenAbout => {
                let close_task = self.close_popup_task();
                let outputs: Vec<OutputInfo> = self.outputs.values().cloned().collect();
                self.system_info = collect_system_info(&outputs);
                if self.about_window.is_some() {
                    return close_task;
                }
                let (id, action) = cosmic::surface::action::app_window(
                    |_| Default::default(),
                    |_app: &mut LordMenu| window::Settings {
                        size: ABOUT_WINDOW_SIZE,
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
            Message::CloseAbout => {
                if let Some(id) = self.about_window.take() {
                    return cosmic::surface::surface_task(
                        cosmic::surface::action::destroy_window(id),
                    );
                }
            }
            Message::OpenForceQuit => {
                let close_task = self.close_popup_task();
                self.refresh_running_apps();
                self.confirm_force_quit = false;
                if self.force_quit_window.is_some() {
                    return close_task;
                }
                let (id, action) = cosmic::surface::action::app_window(
                    |_| Default::default(),
                    |_app: &mut LordMenu| window::Settings {
                        size: FORCE_QUIT_WINDOW_SIZE,
                        resizable: true,
                        decorations: true,
                        exit_on_close_request: false,
                        ..Default::default()
                    },
                    None,
                );
                self.force_quit_window = Some(id);
                return Task::batch([close_task, cosmic::surface::surface_task(action)]);
            }
            Message::CloseForceQuit => {
                self.confirm_force_quit = false;
                if let Some(id) = self.force_quit_window.take() {
                    return cosmic::surface::surface_task(
                        cosmic::surface::action::destroy_window(id),
                    );
                }
            }
            Message::SelectForceQuitApp(app_id) => {
                self.selected_force_quit_app = Some(app_id);
                self.confirm_force_quit = false;
            }
            Message::QuitSelectedApp => {
                if let Some(sel) = &self.selected_force_quit_app
                    && let Some(group) = self.running_apps.iter().find(|g| &g.app_id == sel)
                    && let Some(tx) = &self.wayland_tx
                {
                    for handle in &group.handles {
                        let _ = tx.send(WaylandRequest::CloseToplevel(handle.clone()));
                    }
                }
            }
            Message::RequestForceQuitConfirm => {
                self.confirm_force_quit = true;
            }
            Message::CancelForceQuitConfirm => {
                self.confirm_force_quit = false;
            }
            Message::ConfirmForceQuitApp => {
                self.confirm_force_quit = false;
                if let Some(sel) = &self.selected_force_quit_app
                    && let Some(group) = self.running_apps.iter().find(|g| &g.app_id == sel)
                {
                    if !group.pids.is_empty() {
                        force_quit_pids(&group.pids);
                    } else if let Some(tx) = &self.wayland_tx {
                        for handle in &group.handles {
                            let _ = tx.send(WaylandRequest::CloseToplevel(handle.clone()));
                        }
                    }
                }
            }
            Message::LaunchCommand(cmd) => {
                let close_task = self.close_popup_task();
                let spawn_task = self.spawn_with_token(cmd);
                return Task::batch([close_task, spawn_task]);
            }
            Message::LaunchRecentItem(idx) => {
                let close_task = self.close_popup_task();
                if let Some(item) = self.recent_items_cache.get(idx).cloned() {
                    match item.target {
                        RecentTarget::AppExec { exec, terminal } => {
                            let desktop_exec = exec;
                            tokio::spawn(async move {
                                cosmic::desktop::spawn_desktop_exec(
                                    desktop_exec,
                                    Vec::<(&str, &str)>::new(),
                                    None::<&str>,
                                    terminal,
                                )
                                .await;
                            });
                        }
                        RecentTarget::Uri(uri) => {
                            let _ = std::process::Command::new("xdg-open").arg(&uri).spawn();
                        }
                    }
                }
                return close_task;
            }
            Message::Power(action) => {
                let close_task = self.close_popup_task();
                let power_task = Task::perform(
                    async move { action.execute().await.map_err(|e| e.to_string()) },
                    |res| cosmic::action::app(Message::PowerResult(res)),
                );
                return Task::batch([close_task, power_task]);
            }
            Message::PowerResult(Err(err)) => {
                tracing::error!("Power action failed: {err}");
            }
            Message::PowerResult(Ok(())) => {}
            Message::Wayland(upd) => match *upd {
                WaylandUpdate::Init(tx) => {
                    self.wayland_tx = Some(tx);
                }
                WaylandUpdate::ToplevelAdd(info) => {
                    self.record_session_app(&info);
                    self.toplevels.insert(info.foreign_toplevel.clone(), info);
                    if self.force_quit_window.is_some() {
                        self.refresh_running_apps();
                    }
                }
                WaylandUpdate::ToplevelUpdate(info) => {
                    self.toplevels.insert(info.foreign_toplevel.clone(), info);
                    if self.force_quit_window.is_some() {
                        self.refresh_running_apps();
                    }
                }
                WaylandUpdate::ToplevelRemove(handle) => {
                    self.toplevels.remove(&handle);
                    if self.force_quit_window.is_some() {
                        self.refresh_running_apps();
                    }
                }
                WaylandUpdate::OutputAdd(output, info)
                | WaylandUpdate::OutputUpdate(output, info) => {
                    self.outputs.insert(output, info);
                }
                WaylandUpdate::OutputRemove(output) => {
                    self.outputs.remove(&output);
                }
                WaylandUpdate::Finished => {
                    self.wayland_tx = None;
                }
            },
            Message::Token(u) => match u {
                TokenUpdate::Init(tx) => self.token_tx = Some(tx),
                TokenUpdate::Finished => self.token_tx = None,
                TokenUpdate::ActivationToken { token, exec, .. } => {
                    spawn_shell_command(&exec, token.as_deref());
                }
            },
            Message::Closed(id) => {
                if self.popup == Some(id) {
                    self.popup = None;
                    self.popover_page = PopoverPage::Main;
                } else if self.about_window == Some(id) {
                    self.about_window = None;
                } else if self.force_quit_window == Some(id) {
                    self.force_quit_window = None;
                    self.confirm_force_quit = false;
                }
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let is_open = self.popup.is_some();
        let pill = mac_pill_button(self.logo_widget(), is_open, PillVariant::IconOnly)
            .on_press_down(Message::TogglePopup);

        // 10px left edge inset for the leftmost bar item
        let item = mac_bar_item(
            pill,
            Padding {
                top: 0.0,
                right: 0.0,
                bottom: 0.0,
                left: 6.0,
            },
        );
        self.core.applet.autosize_window(item).into()
    }

    fn view_window(&self, id: window::Id) -> Element<'_, Message> {
        if self.popup == Some(id) {
            self.view_popover()
        } else if self.about_window == Some(id) {
            self.view_about_window()
        } else if self.force_quit_window == Some(id) {
            self.view_force_quit_window()
        } else {
            text("").into()
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            wayland::subscription().map(|u| Message::Wayland(Box::new(u))),
            activation_token_subscription(0).map(Message::Token),
        ])
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}

fn window_surface_container<'a>(
    content: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    container(content)
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

fn list_box_style(theme: &cosmic::Theme) -> cosmic::iced::widget::container::Style {
    let cosmic = theme.cosmic();
    let fg = Color::from(cosmic.background(theme.transparent).on);
    cosmic::iced::widget::container::Style {
        background: Some(Background::Color(Color::from(
            cosmic.primary(theme.transparent).base,
        ))),
        border: Border {
            radius: cosmic.corner_radii.radius_s.into(),
            width: 1.0,
            color: Color { a: 0.12, ..fg },
        },
        ..Default::default()
    }
}

fn confirm_box_style(theme: &cosmic::Theme) -> cosmic::iced::widget::container::Style {
    let cosmic = theme.cosmic();
    let destructive = Color::from(cosmic.destructive.base);
    cosmic::iced::widget::container::Style {
        background: Some(Background::Color(Color {
            a: 0.12,
            ..destructive
        })),
        border: Border {
            radius: cosmic.corner_radii.radius_s.into(),
            width: 1.0,
            color: Color {
                a: 0.35,
                ..destructive
            },
        },
        ..Default::default()
    }
}

fn is_binary_installed(name: &str) -> bool {
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return true;
            }
        }
    }
    PathBuf::from("/usr/bin").join(name).is_file()
}

fn spawn_shell_command(cmd_line: &str, token: Option<&str>) {
    let mut parts = cmd_line.split_whitespace();
    let Some(bin) = parts.next() else {
        return;
    };
    let mut cmd = std::process::Command::new(bin);
    cmd.args(parts);
    if let Some(t) = token {
        cmd.env("XDG_ACTIVATION_TOKEN", t);
        cmd.env("DESKTOP_STARTUP_ID", t);
    }
    tokio::spawn(cosmic::process::spawn(cmd));
}

fn find_desktop_entry<'a>(entries: &'a [DesktopEntry], app_id: &str) -> Option<&'a DesktopEntry> {
    if app_id.is_empty() {
        return None;
    }
    let clean = app_id.strip_suffix(".desktop").unwrap_or(app_id);
    if let Some(found) = fde::find_app_by_id(entries, fde::unicase::Ascii::new(clean)) {
        return Some(found);
    }
    for entry in entries {
        if let Some(wm) = entry.startup_wm_class()
            && wm.eq_ignore_ascii_case(clean)
        {
            return Some(entry);
        }
    }
    if let Some(last) = clean.rsplit('.').next() {
        for entry in entries {
            let eid = entry.id().strip_suffix(".desktop").unwrap_or(entry.id());
            if eid.eq_ignore_ascii_case(last)
                || eid
                    .rsplit('.')
                    .next()
                    .is_some_and(|seg| seg.eq_ignore_ascii_case(last))
            {
                return Some(entry);
            }
        }
    }
    None
}

fn fallback_app_name(app_id: &str, title: &str) -> String {
    if !app_id.is_empty() {
        let last = app_id
            .strip_suffix(".desktop")
            .unwrap_or(app_id)
            .rsplit('.')
            .next()
            .unwrap_or(app_id);
        if !last.is_empty() {
            return last.to_string();
        }
    }
    if !title.trim().is_empty() {
        return title.trim().to_string();
    }
    "Application".to_string()
}

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt::init();
    let _ = tracing_log::LogTracer::init();
    cosmic::applet::run::<LordMenu>(())
}
