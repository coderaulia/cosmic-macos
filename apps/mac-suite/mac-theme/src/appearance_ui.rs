// SPDX-License-Identifier: GPL-3.0-only

//! The Appearance page: automatic light/dark mode, schedule, location and
//! dynamic wallpaper, backed by the `mac-appearance` daemon.

use std::{path::PathBuf, process::Stdio};

use cosmic::{
    Element, Task, app,
    iced::{Length, Size},
    widget::{self, button, column, row, settings, text},
};
use mac_appearance::config::{
    self, Location, Mode, Settings, WallpaperMode, format_hhmm, parse_hhmm,
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

pub fn run() -> cosmic::iced::Result {
    let settings = app::Settings::default().size(Size::new(640.0, 760.0));
    app::run::<Page>(settings, ())
}

#[derive(Clone, Copy, Debug)]
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
    Wallpaper(usize),
    Path(Target, String),
    Browse(Target),
    Picked(Target, Option<PathBuf>),
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

fn pick(target: Target) -> app::Task<Message> {
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
        Message::Picked(target, url.ok().and_then(|u| u.to_file_path().ok()))
    })
}

impl Page {
    fn settings(&self) -> Result<Settings, String> {
        let time = |label: &str, s: &str| parse_hhmm(s).ok_or(format!("{label} must look like 07:30"));
        Ok(Settings {
            mode: self.mode,
            light_at: time("Light time", &self.light_at)?,
            dark_at: time("Dark time", &self.dark_at)?,
            wallpaper: self.wallpaper,
            wallpaper_light: self.light_path.trim().to_owned(),
            wallpaper_dark: self.dark_path.trim().to_owned(),
            dynamic_dir: self.folder.trim().to_owned(),
        })
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
            daemon: false,
            status: String::new(),
        };
        let (prog, args) = systemctl(&["is-enabled", "--quiet", SERVICE]);
        let task = cosmic::task::future(async move { Message::DaemonState(run_command(prog, args).await) });
        (page, task)
    }

    fn update(&mut self, message: Message) -> app::Task<Message> {
        match message {
            Message::Mode(i) => self.mode = Mode::ALL.get(i).copied().unwrap_or(self.mode),
            Message::LightAt(v) => self.light_at = v,
            Message::DarkAt(v) => self.dark_at = v,
            Message::City(v) => self.city = v,
            Message::Lat(v) => self.lat = v,
            Message::Lon(v) => self.lon = v,
            Message::Wallpaper(i) => {
                self.wallpaper = WallpaperMode::ALL.get(i).copied().unwrap_or(self.wallpaper);
            }
            Message::Path(Target::Light, v) => self.light_path = v,
            Message::Path(Target::Dark, v) => self.dark_path = v,
            Message::Path(Target::Folder, v) => self.folder = v,
            Message::Browse(target) => return pick(target),
            Message::Picked(target, Some(path)) => {
                let path = path.to_string_lossy().into_owned();
                return self.update(Message::Path(target, path));
            }
            Message::Picked(_, None) => {}
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
        let path_row = |hint: &'static str, value: &str, target: Target| {
            row::with_capacity(2)
                .push(
                    widget::text_input(hint, value.to_owned())
                        .on_input(move |v| Message::Path(target, v))
                        .width(Length::Fixed(220.0)),
                )
                .push(button::standard("Choose…").on_press(Message::Browse(target)))
                .spacing(8)
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

        let mut wallpaper = settings::section().title("Wallpaper").add(settings::item(
            "Wallpaper",
            widget::dropdown(
                &WALLPAPER_LABELS[..],
                WallpaperMode::ALL.iter().position(|m| *m == self.wallpaper),
                Message::Wallpaper,
            ),
        ));
        match self.wallpaper {
            WallpaperMode::None => {}
            WallpaperMode::Pair => {
                wallpaper = wallpaper
                    .add(settings::item("Light", path_row("Path to image", &self.light_path, Target::Light)))
                    .add(settings::item("Dark", path_row("Path to image", &self.dark_path, Target::Dark)));
            }
            WallpaperMode::Dynamic => {
                wallpaper = wallpaper
                    .add(settings::item("Folder", path_row("Folder with dawn, day, dusk, night", &self.folder, Target::Folder)));
            }
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
            .align_y(cosmic::iced::Alignment::Center);

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
