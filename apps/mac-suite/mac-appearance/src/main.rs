// SPDX-License-Identifier: GPL-3.0-only

//! Daemon: switches COSMIC between light and dark and swaps the wallpaper.
//!
//! `mac-appearance` runs until stopped, `mac-appearance --apply-now` applies once.

use std::{
    fs,
    os::fd::RawFd,
    path::PathBuf,
    sync::atomic::{AtomicI32, Ordering},
    time::{Duration, SystemTime},
};

use jiff::{Timestamp, tz::TimeZone};
use mac_appearance::{
    apply,
    config::{self, Location, Settings},
    schedule::{self, Plan},
};

/// Longest sleep, so config edits and clock jumps are noticed.
const MAX_SLEEP: Duration = Duration::from_secs(60);

/// Write end of the self-pipe the signal handler pokes.
static WAKE_FD: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_signal(_: libc::c_int) {
    let fd = WAKE_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        // SAFETY: write(2) is async-signal-safe; the buffer is one valid byte.
        unsafe { libc::write(fd, [1u8].as_ptr().cast(), 1) };
    }
}

/// Install signal handlers; returns the read end of the wake pipe.
fn install_signals() -> Option<RawFd> {
    let mut fds = [0; 2];
    // SAFETY: `fds` has room for the two descriptors pipe2 writes.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) } != 0 {
        return None;
    }
    WAKE_FD.store(fds[1], Ordering::Relaxed);
    // SAFETY: the handlers only call async-signal-safe functions.
    unsafe {
        libc::signal(libc::SIGHUP, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
        for sig in [libc::SIGTERM, libc::SIGINT] {
            libc::signal(sig, on_quit as extern "C" fn(libc::c_int) as libc::sighandler_t);
        }
    }
    Some(fds[0])
}

/// What a wake-up was for.
enum Wake {
    Timeout,
    Hup,
    Quit,
}

/// Sleep up to `timeout` or until a signal arrives.
fn wait(read_fd: Option<RawFd>, timeout: Duration) -> Wake {
    let Some(fd) = read_fd else {
        std::thread::sleep(timeout);
        return Wake::Timeout;
    };
    let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    let ms = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
    // SAFETY: one valid pollfd.
    let n = unsafe { libc::poll(&mut pfd, 1, ms) };
    if n <= 0 {
        return Wake::Timeout;
    }
    // The pipe only says "a signal arrived"; QUIT tells stop from reload.
    let mut buf = [0u8; 16];
    // SAFETY: reads into a valid buffer of the stated length.
    unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
    if QUIT.load(Ordering::Relaxed) { Wake::Quit } else { Wake::Hup }
}

static QUIT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn on_quit(_: libc::c_int) {
    QUIT.store(true, Ordering::Relaxed);
    on_signal(0);
}

fn mtime(path: &PathBuf) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// What was last written to the desktop.
#[derive(PartialEq)]
struct Applied {
    dark: bool,
    wallpaper: Option<PathBuf>,
    output_wallpapers: Vec<(String, PathBuf)>,
}

fn desired_wallpapers(settings: &Settings, plan: &Plan) -> (Option<PathBuf>, Vec<(String, PathBuf)>) {
    let global = schedule::wallpaper_for(settings, plan);
    let mut outputs = Vec::new();
    for conn in settings.configured_outputs() {
        if let Some(path) = schedule::wallpaper_for_output(settings, conn, plan) {
            outputs.push((conn.to_owned(), path));
        }
    }
    (global, outputs)
}

/// Apply the desired state; only reports and returns what changed.
fn apply_plan(settings: &Settings, plan: &Plan) -> Applied {
    let (wallpaper, output_wallpapers) = desired_wallpapers(settings, plan);
    if let Err(err) = apply::set_dark(plan.dark) {
        eprintln!("mac-appearance: setting dark mode failed: {err}");
    }
    if wallpaper.is_some() || !output_wallpapers.is_empty() {
        let refs: Vec<(&str, &std::path::Path)> = output_wallpapers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_path()))
            .collect();
        if let Err(err) = apply::set_wallpapers(wallpaper.as_deref(), &refs) {
            eprintln!("mac-appearance: setting wallpaper failed: {err}");
        }
    }
    Applied { dark: plan.dark, wallpaper, output_wallpapers }
}

fn compute(settings: &Settings, location: Option<&Location>) -> Plan {
    schedule::plan(settings, location, Timestamp::now().as_second(), &TimeZone::system())
}

fn main() {
    if std::env::args().skip(1).any(|a| a == "--apply-now") {
        let (settings, location) = (config::load_settings(), config::load_location());
        apply_plan(&settings, &compute(&settings, location.as_ref()));
        return;
    }

    let read_fd = install_signals();

    let (settings_path, location_path) = (config::settings_path(), config::location_path());
    let mut applied: Option<Applied> = None;
    let mut seen = (mtime(&settings_path), mtime(&location_path));
    let mut settings = config::load_settings();
    let mut location = config::load_location();
    loop {
        let plan = compute(&settings, location.as_ref());
        let (wallpaper, output_wallpapers) = desired_wallpapers(&settings, &plan);
        let want = Applied { dark: plan.dark, wallpaper, output_wallpapers };
        // Only touch the desktop when the wanted state changed, so manual
        // changes in between are not overwritten every minute.
        if applied.as_ref() != Some(&want) {
            applied = Some(apply_plan(&settings, &plan));
        }

        let until_next = plan
            .next
            .map(|t| Duration::from_secs((t - Timestamp::now().as_second()).max(1) as u64))
            .unwrap_or(MAX_SLEEP);
        // Sleep at most 60 s, plus a second so we land after the transition.
        let timeout = (until_next + Duration::from_secs(1)).min(MAX_SLEEP);
        match wait(read_fd, timeout) {
            Wake::Quit => return,
            Wake::Hup => {
                settings = config::load_settings();
                location = config::load_location();
                applied = None;
            }
            Wake::Timeout => {
                let now_seen = (mtime(&settings_path), mtime(&location_path));
                if now_seen != seen {
                    seen = now_seen;
                    settings = config::load_settings();
                    location = config::load_location();
                    applied = None;
                }
            }
        }
    }
}
