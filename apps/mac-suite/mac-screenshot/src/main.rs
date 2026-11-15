// SPDX-License-Identifier: GPL-3.0-only

//! macOS-style screenshot tool (Super+Shift+S).

mod config;
mod flow;
mod geometry;
mod output;
mod record;
mod ui;
mod wayland;

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;

use config::{Mode, Options};
use flow::Request;
use record::{RecordTarget, Recorder};
use ui::overlay::Outcome;

#[derive(Parser, Debug)]
#[command(name = "mac-screenshot", version, about = "macOS-style screenshot tool")]
struct Cli {
    /// Capture every screen immediately (no overlay).
    #[arg(long, conflicts_with_all = ["region", "window"])]
    screen: bool,
    /// Select a region, without the toolbar.
    #[arg(long, conflicts_with = "window")]
    region: bool,
    /// Select a window, without the toolbar.
    #[arg(long)]
    window: bool,
    /// Show the floating thumbnail for a saved screenshot (internal).
    #[arg(long, value_name = "FILE", hide = true)]
    thumbnail: Option<PathBuf>,
    /// Keep a PNG read from stdin on the clipboard (internal).
    #[arg(long, hide = true)]
    serve_clipboard: bool,
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let cli = Cli::parse();

    if cli.serve_clipboard {
        if let Err(e) = output::serve_clipboard() {
            tracing::error!("{e}");
        }
        return;
    }
    if let Some(path) = cli.thumbnail {
        ui::thumbnail::run(path);
        return;
    }
    // One capture at a time: a second shortcut press stops an active screen
    // recording (via the control socket), or exits silently if an overlay is open.
    let Some(_lock) = single_instance() else {
        let _ = try_stop_active_recording();
        return;
    };
    if let Err(e) = capture(&cli) {
        flow::notify(&e.to_string());
        std::process::exit(1);
    }
}

fn capture(cli: &Cli) -> Result<(), output::Error> {
    let options = Options::load();

    if cli.screen {
        return screens(options);
    }
    let (mode, toolbar) = match (cli.region, cli.window) {
        (_, true) => (Mode::Window, false),
        (true, _) => (Mode::Region, false),
        _ => (Mode::Region, true),
    };

    // Frozen capture for the overlay; without screencopy use the portal.
    let snapshot = match wayland::snapshot(true, options.show_pointer) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            tracing::warn!("screencopy failed ({e}); falling back to the portal");
            return portal_fallback(options);
        }
    };

    let Some(outcome) = ui::overlay::run(ui::overlay::Flags {
        snapshot: snapshot.clone(),
        options,
        mode,
        toolbar,
    }) else {
        return Ok(());
    };
    // The toolbar may have changed the options.
    let options = Options::load();
    match outcome {
        Outcome::Screenshot(req) => {
            flow::run_request(&req, Some(&snapshot), options)?;
        }
        Outcome::Record(req) => {
            run_recording(req, &snapshot, options)?;
        }
    }
    Ok(())
}

fn run_recording(
    req: Request,
    snapshot: &wayland::Snapshot,
    options: Options,
) -> Result<(), output::Error> {
    if options.timer.seconds() > 0 {
        std::thread::sleep(Duration::from_secs(options.timer.seconds()));
    }
    let shot = snapshot
        .outputs
        .iter()
        .find(|o| o.name == req.output)
        .or_else(|| snapshot.outputs.first())
        .ok_or_else(|| output::Error::Msg("no output".into()))?;
    let target = RecordTarget {
        output: shot.name.clone(),
        logical_size: shot.logical_size,
        pixel_size: (shot.image.width(), shot.image.height()),
        rect: req.rect,
    };
    let recorder = Recorder::start(&target, options)?;
    let path = ui::recording::run(control_socket_path(), recorder)?;
    flow::spawn_thumbnail(&path);
    Ok(())
}

fn screens(options: Options) -> Result<(), output::Error> {
    match flow::run_screens(options.clone()) {
        Ok(_) => Ok(()),
        Err(e) => {
            tracing::warn!("screencopy failed ({e}); falling back to the portal");
            portal_fallback(options)
        }
    }
}

/// Whole-screen capture through xdg-desktop-portal, then the usual delivery.
fn portal_fallback(options: Options) -> Result<(), output::Error> {
    let tmp = output::portal_screenshot()?;
    let img = image::open(&tmp)?.to_rgba8();
    let _ = std::fs::remove_file(&tmp);
    flow::deliver(&img, &options, None)?;
    Ok(())
}

fn control_socket_path() -> Option<PathBuf> {
    output::private_runtime_dir().map(|dir| dir.join("mac-screenshot.sock"))
}

fn try_stop_active_recording() -> bool {
    let Some(path) = control_socket_path() else { return false };
    let Ok(mut stream) = UnixStream::connect(&path) else { return false };
    stream.write_all(b"stop\n").is_ok()
}

/// Take an exclusive advisory lock in the runtime dir; `None` if another
/// capture already holds it. The lock is released when the process exits.
fn single_instance() -> Option<std::fs::File> {
    // No private directory: run without the single-instance guard.
    let Some(dir) = output::private_runtime_dir() else {
        return std::fs::File::open("/dev/null").ok();
    };
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("mac-screenshot.lock"))
        .ok()?;
    match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Some(file),
        Err(rustix::io::Errno::WOULDBLOCK) => None,
        // Locking unsupported: run anyway rather than block screenshots.
        Err(_) => Some(file),
    }
}
