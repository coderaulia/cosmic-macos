// SPDX-License-Identifier: GPL-3.0-only

//! The Appearance page: automatic light/dark mode, schedule, location and
//! dynamic wallpaper, backed by the `mac-appearance` daemon.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Stdio,
};

use cosmic::{
    Element, Task, app,
    cctk::{
        sctk::{
            self,
            output::{OutputHandler, OutputState},
            registry::{ProvidesRegistryState, RegistryState},
        },
        wayland_client::{Connection, QueueHandle, globals::registry_queue_init, protocol::wl_output},
    },
    iced::{Alignment, ContentFit, Length, Size},
    widget::{self, button, column, row, settings, text},
};
use mac_appearance::{
    config::{
        self, Location, Mode, OutputWallpaper, Settings, WallpaperMode, WallpaperSpec, format_hhmm,
        parse_hhmm,
    },
    schedule::{self, Phase},
};

const APP_ID: &str = "dev.asw.MacTheme";
const SERVICE: &str = "mac-appearance.service";

const MODE_LABELS: [&str; 4] = [
    "Light",
    "Dark",
    "Automatic (sunrise to sunset)",
    "Custom schedule",
];
const WALLPAPER_LABELS: [&str; 3] = [
    "Leave wallpaper alone",
    "Light and dark picture",
    "Dynamic (dawn, day, dusk, night)",
];
const SCREEN_WALLPAPER_LABELS: [&str; 4] = [
    "Use \"All screens\" setting",
    "Leave wallpaper alone",
    "Light and dark picture",
    "Dynamic (dawn, day, dusk, night)",
];

#[derive(Clone, Debug, PartialEq, Eq)]
struct DetectedOutput {
    connector: String,
    label: String,
}

struct OutputProbe {
    registry_state: RegistryState,
    output_state: OutputState,
}

impl ProvidesRegistryState for OutputProbe {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    sctk::registry_handlers![OutputState];
}

impl OutputHandler for OutputProbe {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

sctk::delegate_registry!(OutputProbe);
sctk::delegate_output!(OutputProbe);

/// Query connected `wl_output` connector names and labels via Wayland.
fn detect_wayland_outputs() -> Vec<DetectedOutput> {
    let Ok(conn) = Connection::connect_to_env() else {
        return Vec::new();
    };
    let Ok((globals, mut event_queue)) = registry_queue_init::<OutputProbe>(&conn) else {
        return Vec::new();
    };
    let qh = event_queue.handle();
    let mut state = OutputProbe {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
    };
    // First roundtrip binds wl_output globals; second receives name/model/done events.
    if event_queue.roundtrip(&mut state).is_err() || event_queue.roundtrip(&mut state).is_err() {
        return Vec::new();
    }

    let mut out = Vec::new();
    for handle in state.output_state.outputs() {
        let Some(info) = state.output_state.info(&handle) else {
            continue;
        };
        let Some(connector) = info.name.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        if out.iter().any(|o: &DetectedOutput| o.connector == connector) {
            continue;
        }
        let model = info.model.trim();
        let label = if model.is_empty() {
            connector.to_owned()
        } else {
            format!("{connector} — {model}")
        };
        out.push(DetectedOutput {
            connector: connector.to_owned(),
            label,
        });
    }
    out
}

/// Merge live `wl_output`s with any extra connectors saved in `appearance.conf`.
fn merge_outputs(
    detected: Vec<DetectedOutput>,
    configured: &BTreeMap<String, OutputWallpaper>,
) -> Vec<DetectedOutput> {
    let mut merged = detected;
    for (conn, entry) in configured {
        if entry.is_empty() || merged.iter().any(|o| &o.connector == conn) {
            continue;
        }
        merged.push(DetectedOutput {
            connector: conn.clone(),
            label: format!("{conn} (disconnected)"),
        });
    }
    merged
}

pub fn run() -> cosmic::iced::Result {
    let settings = app::Settings::default().size(Size::new(660.0, 820.0));
    app::run::<Page>(settings, ())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Light,
    Dark,
    Folder,
}

#[derive(Clone, Debug)]
enum Message {
    Mode(usize),
    LightAt(String),
    DarkAt(String),
    City(String),
    Lat(String),
    Lon(String),
    Screen(usize),
    SelectScreen(Option<String>),
    OutputsDetected(Vec<DetectedOutput>),
    Wallpaper(usize),
    Path(Option<String>, Target, String),
    Browse(Option<String>, Target),
    Picked(Option<String>, Target, Option<PathBuf>),
    ResetScreen(String),
    Daemon(bool),
    DaemonState(bool),
    Save,
    ApplyTheme,
    Done(String),
}

struct Page {
    core: app::Core,
    mode: Mode,
    light_at: String,
    dark_at: String,
    city: String,
    lat: String,
    lon: String,
    wallpaper: WallpaperMode,
    light_path: String,
    dark_path: String,
    folder: String,
    per_output: BTreeMap<String, OutputWallpaper>,
    outputs: Vec<DetectedOutput>,
    selected_screen: Option<String>,
    daemon: bool,
    status: String,
}

/// Run a command to completion on a blocking thread; true if it exited 0.
async fn run_command(program: String, args: Vec<String>) -> bool {
    tokio::task::spawn_blocking(move || {
        std::process::Command::new(program)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
    .await
    .unwrap_or(false)
}

fn systemctl(args: &[&str]) -> (String, Vec<String>) {
    let mut all = vec!["--user".to_owned()];
    all.extend(args.iter().map(|a| (*a).to_owned()));
    ("systemctl".to_owned(), all)
}

/// `mac-appearance` next to this executable, else from PATH.
fn daemon_binary() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join("mac-appearance")))
        .filter(|p| p.is_file())
        .map_or_else(|| "mac-appearance".to_owned(), |p| p.to_string_lossy().into_owned())
}

fn pick(scope: Option<String>, target: Target) -> app::Task<Message> {
    cosmic::task::future(async move {
        let dialog = cosmic::dialog::file_chooser::open::Dialog::new().title(match target {
            Target::Light => "Choose the light wallpaper",
            Target::Dark => "Choose the dark wallpaper",
            Target::Folder => "Choose the dynamic wallpaper folder",
        });
        let url = match target {
            Target::Folder => dialog.open_folder().await.map(|r| r.url().clone()),
            _ => dialog.open_file().await.map(|r| r.url().clone()),
        };
        Message::Picked(scope, target, url.ok().and_then(|u| u.to_file_path().ok()))
    })
}

fn preview_thumb<'a>(path: Option<&str>) -> Element<'a, Message> {
    if let Some(p) = path.map(str::trim).filter(|p| !p.is_empty() && Path::new(p).is_file()) {
        widget::image(widget::image::Handle::from_path(p))
            .width(Length::Fixed(52.0))
            .height(Length::Fixed(32.0))
            .content_fit(ContentFit::Cover)
            .border_radius([4.0; 4])
            .into()
    } else {
        text::caption("No preview")
            .width(Length::Fixed(52.0))
            .into()
    }
}

fn dynamic_preview_path(dir: &str) -> Option<String> {
    let dir = dir.trim();
    if dir.is_empty() {
        return None;
    }
    [Phase::Day, Phase::Dawn, Phase::Dusk, Phase::Night]
        .into_iter()
        .find_map(|phase| schedule::dynamic_image(dir, phase))
        .map(|p| p.to_string_lossy().into_owned())
}

fn screen_summary(spec: &WallpaperSpec, customized: bool) -> String {
    let mode_desc = match spec.mode {
        WallpaperMode::None => "Leave wallpaper alone".to_owned(),
        WallpaperMode::Pair => {
            let name = Path::new(spec.wallpaper_light.trim())
                .file_name()
                .or_else(|| Path::new(spec.wallpaper_dark.trim()).file_name())
                .and_then(|s| s.to_str())
                .filter(|s| !s.is_empty());
            match name {
                Some(n) => format!("Light & dark ({n})"),
                None => "Light & dark".to_owned(),
            }
        }
        WallpaperMode::Dynamic => {
            let name = Path::new(spec.dynamic_dir.trim())
                .file_name()
                .and_then(|s| s.to_str())
                .filter(|s| !s.is_empty());
            match name {
                Some(n) => format!("Dynamic ({n})"),
                None => "Dynamic".to_owned(),
            }
        }
    };
    if customized {
        format!("Custom: {mode_desc}")
    } else {
        format!("All screens: {mode_desc}")
    }
}

impl Page {
    fn settings(&self) -> Result<Settings, String> {
        let time = |label: &str, s: &str| parse_hhmm(s).ok_or(format!("{label} must look like 07:30"));
        let mut outputs = self.per_output.clone();
        outputs.retain(|_, o| !o.is_empty());
        Ok(Settings {
            mode: self.mode,
            light_at: time("Light time", &self.light_at)?,
            dark_at: time("Dark time", &self.dark_at)?,
            wallpaper: self.wallpaper,
            wallpaper_light: self.light_path.trim().to_owned(),
            wallpaper_dark: self.dark_path.trim().to_owned(),
            dynamic_dir: self.folder.trim().to_owned(),
            outputs,
        })
    }

    fn effective_spec(&self, scope: Option<&str>) -> WallpaperSpec {
        let global = WallpaperSpec {
            mode: self.wallpaper,
            wallpaper_light: self.light_path.trim().to_owned(),
            wallpaper_dark: self.dark_path.trim().to_owned(),
            dynamic_dir: self.folder.trim().to_owned(),
        };
        let Some(conn) = scope else {
            return global;
        };
        let Some(entry) = self.per_output.get(conn) else {
            return global;
        };
        WallpaperSpec {
            mode: entry.wallpaper.unwrap_or(global.mode),
            wallpaper_light: entry
                .wallpaper_light
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(&global.wallpaper_light)
                .to_owned(),
            wallpaper_dark: entry
                .wallpaper_dark
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(&global.wallpaper_dark)
                .to_owned(),
            dynamic_dir: entry
                .dynamic_dir
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(&global.dynamic_dir)
                .to_owned(),
        }
    }

    fn set_scope_path(&mut self, scope: Option<String>, target: Target, value: String) {
        match scope {
            None => match target {
                Target::Light => self.light_path = value,
                Target::Dark => self.dark_path = value,
                Target::Folder => self.folder = value,
            },
            Some(conn) => {
                let entry = self.per_output.entry(conn.clone()).or_default();
                let opt = if value.trim().is_empty() { None } else { Some(value) };
                match target {
                    Target::Light => entry.wallpaper_light = opt,
                    Target::Dark => entry.wallpaper_dark = opt,
                    Target::Folder => entry.dynamic_dir = opt,
                }
                if entry.is_empty() {
                    self.per_output.remove(&conn);
                }
            }
        }
    }

    /// `Ok(None)` when no coordinates are entered, which clears the location.
    fn location(&self) -> Result<Option<Location>, String> {
        let (lat, lon) = (self.lat.trim(), self.lon.trim());
        if lat.is_empty() && lon.is_empty() {
            return Ok(None);
        }
        let lat = lat.parse::<f64>().ok().filter(|v| (-90.0..=90.0).contains(v));
        let lon = lon.parse::<f64>().ok().filter(|v| (-180.0..=180.0).contains(v));
        match (lat, lon) {
            (Some(lat), Some(lon)) => Ok(Some(Location { city: self.city.trim().to_owned(), lat, lon })),
            _ => Err("Latitude must be -90 to 90 and longitude -180 to 180".to_owned()),
        }
    }

    fn save(&mut self) -> app::Task<Message> {
        let settings = match self.settings() {
            Ok(s) => s,
            Err(err) => {
                self.status = err;
                return Task::none();
            }
        };
        let location = match self.location() {
            Ok(l) => l,
            Err(err) => {
                self.status = err;
                return Task::none();
            }
        };
        if let Err(err) = config::save_settings(&settings) {
            self.status = format!("Could not save settings: {err}");
            return Task::none();
        }
        match &location {
            Some(loc) => {
                if let Err(err) = config::save_location(loc) {
                    self.status = format!("Could not save location: {err}");
                    return Task::none();
                }
            }
            None => {
                let _ = std::fs::remove_file(config::location_path());
            }
        }
        self.status = "Saved".to_owned();
        let binary = daemon_binary();
        cosmic::task::future(async move {
            let applied = run_command(binary, vec!["--apply-now".to_owned()]).await;
            // Tell a running daemon to re-read its config; fails harmlessly if it is stopped.
            let (prog, args) = systemctl(&["kill", "--signal=HUP", SERVICE]);
            run_command(prog, args).await;
            Message::Done(if applied { "Saved and applied".to_owned() } else { "Saved, but applying failed".to_owned() })
        })
    }
}

impl cosmic::Application for Page {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = APP_ID;

    fn core(&self) -> &app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut app::Core {
        &mut self.core
    }

    fn init(mut core: app::Core, _flags: ()) -> (Self, app::Task<Message>) {
        core.window.header_title = "Appearance".to_owned();
        let s = config::load_settings();
        let loc = config::load_location();
        let outputs = merge_outputs(Vec::new(), &s.outputs);
        let page = Self {
            core,
            mode: s.mode,
            light_at: format_hhmm(s.light_at),
            dark_at: format_hhmm(s.dark_at),
            city: loc.as_ref().map(|l| l.city.clone()).unwrap_or_default(),
            lat: loc.as_ref().map(|l| l.lat.to_string()).unwrap_or_default(),
            lon: loc.as_ref().map(|l| l.lon.to_string()).unwrap_or_default(),
            wallpaper: s.wallpaper,
            light_path: s.wallpaper_light,
            dark_path: s.wallpaper_dark,
            folder: s.dynamic_dir,
            per_output: s.outputs,
            outputs,
            selected_screen: None,
            daemon: false,
            status: String::new(),
        };
        let (prog, args) = systemctl(&["is-enabled", "--quiet", SERVICE]);
        let daemon_task =
            cosmic::task::future(async move { Message::DaemonState(run_command(prog, args).await) });
        let outputs_task = cosmic::task::future(async {
            let detected = tokio::task::spawn_blocking(detect_wayland_outputs)
                .await
                .unwrap_or_default();
            Message::OutputsDetected(detected)
        });
        (page, Task::batch([daemon_task, outputs_task]))
    }

    fn update(&mut self, message: Message) -> app::Task<Message> {
        match message {
            Message::Mode(i) => self.mode = Mode::ALL.get(i).copied().unwrap_or(self.mode),
            Message::LightAt(v) => self.light_at = v,
            Message::DarkAt(v) => self.dark_at = v,
            Message::City(v) => self.city = v,
            Message::Lat(v) => self.lat = v,
            Message::Lon(v) => self.lon = v,
            Message::Screen(i) => {
                self.selected_screen = if i == 0 {
                    None
                } else {
                    self.outputs.get(i - 1).map(|o| o.connector.clone())
                };
            }
            Message::SelectScreen(scope) => self.selected_screen = scope,
            Message::OutputsDetected(detected) => {
                self.outputs = merge_outputs(detected, &self.per_output);
            }
            Message::Wallpaper(i) => match &self.selected_screen {
                None => {
                    self.wallpaper = WallpaperMode::ALL.get(i).copied().unwrap_or(self.wallpaper);
                }
                Some(conn) => {
                    let conn = conn.clone();
                    let entry = self.per_output.entry(conn.clone()).or_default();
                    entry.wallpaper = if i == 0 {
                        None
                    } else {
                        WallpaperMode::ALL.get(i - 1).copied()
                    };
                    if entry.is_empty() {
                        self.per_output.remove(&conn);
                    }
                }
            },
            Message::Path(scope, target, v) => self.set_scope_path(scope, target, v),
            Message::Browse(scope, target) => return pick(scope, target),
            Message::Picked(scope, target, Some(path)) => {
                let path = path.to_string_lossy().into_owned();
                self.set_scope_path(scope, target, path);
            }
            Message::Picked(_, _, None) => {}
            Message::ResetScreen(conn) => {
                self.per_output.remove(&conn);
            }
            Message::Daemon(on) => {
                self.daemon = on;
                let (prog, args) = systemctl(&if on {
                    vec!["enable", "--now", SERVICE]
                } else {
                    vec!["disable", "--now", SERVICE]
                });
                return cosmic::task::future(async move {
                    if run_command(prog.clone(), args.clone()).await {
                        Message::DaemonState(on)
                    } else {
                        // Failed: re-read the real state.
                        let (p, a) = systemctl(&["is-enabled", "--quiet", SERVICE]);
                        Message::DaemonState(run_command(p, a).await)
                    }
                });
            }
            Message::DaemonState(on) => self.daemon = on,
            Message::Save => return self.save(),
            Message::ApplyTheme => {
                return cosmic::task::future(async {
                    let ok = tokio::task::spawn_blocking(super::apply_theme).await.unwrap_or(false);
                    Message::Done(if ok { "macOS look applied".to_owned() } else { "Applying the look failed".to_owned() })
                });
            }
            Message::Done(status) => self.status = status,
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let field = |hint: &'static str, value: &str, on: fn(String) -> Message| {
            widget::text_input(hint, value.to_owned()).on_input(on).width(Length::Fixed(220.0))
        };
        let path_row = |hint: &'static str,
                        value: &str,
                        preview: Option<String>,
                        scope: Option<String>,
                        target: Target| {
            let scope_input = scope.clone();
            let scope_btn = scope;
            row::with_capacity(3)
                .push(preview_thumb(preview.as_deref()))
                .push(
                    widget::text_input(hint, value.to_owned())
                        .on_input(move |v| Message::Path(scope_input.clone(), target, v))
                        .width(Length::Fixed(200.0)),
                )
                .push(button::standard("Choose…").on_press(Message::Browse(scope_btn, target)))
                .spacing(8)
                .align_y(Alignment::Center)
        };

        let mut mode = settings::section().title("Appearance").add(settings::item(
            "Mode",
            widget::dropdown(
                &MODE_LABELS[..],
                Mode::ALL.iter().position(|m| *m == self.mode),
                Message::Mode,
            ),
        ));
        if self.mode == Mode::Schedule {
            mode = mode
                .add(settings::item("Switch to light at", field("07:00", &self.light_at, Message::LightAt)))
                .add(settings::item("Switch to dark at", field("19:00", &self.dark_at, Message::DarkAt)));
        }

        let location = settings::section()
            .title("Location (used for sunrise and sunset)")
            .add(settings::item("City", field("Optional label", &self.city, Message::City)))
            .add(settings::item("Latitude", field("-6.2", &self.lat, Message::Lat)))
            .add(settings::item("Longitude", field("106.8", &self.lon, Message::Lon)));

        let mut screen_labels = Vec::with_capacity(self.outputs.len() + 1);
        screen_labels.push("All screens".to_owned());
        for out in &self.outputs {
            screen_labels.push(out.label.clone());
        }
        let selected_screen_idx = match &self.selected_screen {
            None => Some(0),
            Some(conn) => self
                .outputs
                .iter()
                .position(|o| &o.connector == conn)
                .map(|idx| idx + 1)
                .or(Some(0)),
        };

        let mut wallpaper = settings::section().title("Wallpaper").add(settings::item(
            "Screens",
            widget::dropdown(screen_labels, selected_screen_idx, Message::Screen),
        ));

        let scope = self.selected_screen.clone();
        let active_entry = scope.as_deref().and_then(|c| self.per_output.get(c));
        let effective = self.effective_spec(scope.as_deref());

        match &scope {
            None => {
                wallpaper = wallpaper.add(settings::item(
                    "Wallpaper",
                    widget::dropdown(
                        &WALLPAPER_LABELS[..],
                        WallpaperMode::ALL.iter().position(|m| *m == self.wallpaper),
                        Message::Wallpaper,
                    ),
                ));
            }
            Some(_) => {
                let idx = match active_entry.and_then(|e| e.wallpaper) {
                    None => Some(0),
                    Some(m) => WallpaperMode::ALL.iter().position(|x| *x == m).map(|p| p + 1),
                };
                wallpaper = wallpaper.add(settings::item(
                    "Wallpaper",
                    widget::dropdown(&SCREEN_WALLPAPER_LABELS[..], idx, Message::Wallpaper),
                ));
            }
        }

        let hint_or = |global_hint: &'static str| {
            if scope.is_some() {
                "Same as All screens"
            } else {
                global_hint
            }
        };

        match effective.mode {
            WallpaperMode::None => {}
            WallpaperMode::Pair => {
                let light_val = match active_entry {
                    Some(e) => e.wallpaper_light.as_deref().unwrap_or(""),
                    None => self.light_path.as_str(),
                };
                let dark_val = match active_entry {
                    Some(e) => e.wallpaper_dark.as_deref().unwrap_or(""),
                    None => self.dark_path.as_str(),
                };
                wallpaper = wallpaper
                    .add(settings::item(
                        "Light",
                        path_row(
                            hint_or("Path to image"),
                            light_val,
                            Some(effective.wallpaper_light.clone()),
                            scope.clone(),
                            Target::Light,
                        ),
                    ))
                    .add(settings::item(
                        "Dark",
                        path_row(
                            hint_or("Path to image"),
                            dark_val,
                            Some(effective.wallpaper_dark.clone()),
                            scope.clone(),
                            Target::Dark,
                        ),
                    ));
            }
            WallpaperMode::Dynamic => {
                let dir_val = match active_entry {
                    Some(e) => e.dynamic_dir.as_deref().unwrap_or(""),
                    None => self.folder.as_str(),
                };
                wallpaper = wallpaper.add(settings::item(
                    "Folder",
                    path_row(
                        hint_or("Folder with dawn, day, dusk, night"),
                        dir_val,
                        dynamic_preview_path(&effective.dynamic_dir),
                        scope.clone(),
                        Target::Folder,
                    ),
                ));
            }
        }

        if let Some(conn) = &scope
            && active_entry.is_some_and(|e| !e.is_empty())
        {
            wallpaper = wallpaper.add(settings::item(
                "Override",
                button::standard("Reset to All screens")
                    .on_press(Message::ResetScreen(conn.clone())),
            ));
        }

        for out in &self.outputs {
            let customized = self.per_output.get(&out.connector).is_some_and(|e| !e.is_empty());
            let spec = self.effective_spec(Some(&out.connector));
            let thumb_path = match spec.mode {
                WallpaperMode::None => None,
                WallpaperMode::Pair => {
                    let l = spec.wallpaper_light.trim();
                    let d = spec.wallpaper_dark.trim();
                    if !l.is_empty() && Path::new(l).is_file() {
                        Some(l.to_owned())
                    } else if !d.is_empty() && Path::new(d).is_file() {
                        Some(d.to_owned())
                    } else {
                        None
                    }
                }
                WallpaperMode::Dynamic => dynamic_preview_path(&spec.dynamic_dir),
            };
            let is_selected = self.selected_screen.as_deref() == Some(out.connector.as_str());
            let action_btn = if is_selected {
                button::standard("Editing")
            } else {
                button::standard("Configure")
                    .on_press(Message::SelectScreen(Some(out.connector.clone())))
            };
            wallpaper = wallpaper.add(settings::item(
                out.label.clone(),
                row::with_capacity(3)
                    .push(preview_thumb(thumb_path.as_deref()))
                    .push(text::caption(screen_summary(&spec, customized)).width(Length::Fill))
                    .push(action_btn)
                    .spacing(8)
                    .align_y(Alignment::Center),
            ));
        }

        let daemon = settings::section().title("Automation").add(settings::item(
            "Run in the background",
            widget::toggler(self.daemon).on_toggle(Message::Daemon),
        ));

        let actions = row::with_capacity(3)
            .push(button::suggested("Save and apply").on_press(Message::Save))
            .push(button::standard("Apply macOS look").on_press(Message::ApplyTheme))
            .push(text::body(self.status.as_str()))
            .spacing(12)
            .align_y(Alignment::Center);

        widget::scrollable(
            column::with_capacity(5)
                .push(mode)
                .push(location)
                .push(wallpaper)
                .push(daemon)
                .push(actions)
                .spacing(16)
                .padding(16),
        )
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_outputs_keeps_disconnected_configured_screens() {
        let detected = vec![DetectedOutput {
            connector: "DP-2".into(),
            label: "DP-2 — Dell U2720Q".into(),
        }];
        let mut configured = BTreeMap::new();
        configured.insert(
            "DP-2".into(),
            OutputWallpaper {
                wallpaper: Some(WallpaperMode::Pair),
                ..OutputWallpaper::default()
            },
        );
        configured.insert(
            "HDMI-A-2".into(),
            OutputWallpaper {
                wallpaper: Some(WallpaperMode::Dynamic),
                dynamic_dir: Some("/wall/dyn".into()),
                ..OutputWallpaper::default()
            },
        );
        configured.insert("EMPTY".into(), OutputWallpaper::default());

        let merged = merge_outputs(detected, &configured);
        assert_eq!(
            merged,
            vec![
                DetectedOutput {
                    connector: "DP-2".into(),
                    label: "DP-2 — Dell U2720Q".into(),
                },
                DetectedOutput {
                    connector: "HDMI-A-2".into(),
                    label: "HDMI-A-2 (disconnected)".into(),
                },
            ]
        );
    }

    #[test]
    fn screen_summary_formats_global_and_custom() {
        let spec = WallpaperSpec {
            mode: WallpaperMode::Pair,
            wallpaper_light: "/home/user/Monterey-light.jpg".into(),
            wallpaper_dark: "/home/user/Monterey-dark.jpg".into(),
            dynamic_dir: String::new(),
        };
        assert_eq!(
            screen_summary(&spec, false),
            "All screens: Light & dark (Monterey-light.jpg)"
        );
        assert_eq!(
            screen_summary(&spec, true),
            "Custom: Light & dark (Monterey-light.jpg)"
        );
    }
}
