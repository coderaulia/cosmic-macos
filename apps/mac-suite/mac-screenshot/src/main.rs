// SPDX-License-Identifier: GPL-3.0-only

//! macOS-style screenshot tool (Super+Shift+S).

mod config;
mod flow;
mod geometry;
mod output;
mod ui;
mod wayland;

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;

use config::{Mode, Options};
use flow::Request;

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
    // One capture at a time: a second shortcut press while the overlay is up
    // (or a capture is in flight) must not stack another overlay.
    let Some(_lock) = single_instance() else { return };
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

    let Some(Request { output, rect }) = ui::overlay::run(ui::overlay::Flags {
        snapshot: snapshot.clone(),
        options,
        mode,
        toolbar,
    }) else {
        return Ok(());
    };
    // The toolbar may have changed the options.
    let options = Options::load();
    flow::run_request(&Request { output, rect }, Some(&snapshot), options)?;
    Ok(())
}

fn screens(options: Options) -> Result<(), output::Error> {
    match flow::run_screens(options) {
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
    flow::deliver(&img, options, None)?;
    Ok(())
}

/// A directory only this user can write to: `$XDG_RUNTIME_DIR`, or else a
/// private (0700) folder under `~/.cache`. Never the shared `/tmp`.
fn private_runtime_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let dir = PathBuf::from(std::env::var_os("HOME")?).join(".cache/mac-suite");
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).ok()?;
    let meta = std::fs::symlink_metadata(&dir).ok()?;
    (meta.is_dir() && meta.permissions().mode() & 0o077 == 0).then_some(dir)
}

/// Take an exclusive advisory lock in the runtime dir; `None` if another
/// capture already holds it. The lock is released when the process exits.
fn single_instance() -> Option<std::fs::File> {
    // No private directory: run without the single-instance guard.
    let Some(dir) = private_runtime_dir() else {
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
