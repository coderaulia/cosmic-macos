// SPDX-License-Identifier: GPL-3.0-only

//! Quick Look: `mac-quicklook <file>` shows a floating preview window.
//!
//! A second launch hands its path to the running window (replacing the shown
//! file, or closing the window when it is the same file).

mod content;
mod files;
mod ipc;
mod kind;
mod ui;

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let mut args = std::env::args_os().skip(1);
    let Some(arg) = args.next() else {
        eprintln!("usage: mac-quicklook <file>");
        return Ok(());
    };
    if arg == "-h" || arg == "--help" {
        println!("usage: mac-quicklook <file>");
        return Ok(());
    }
    let path = std::path::absolute(&arg).unwrap_or_else(|_| arg.into());
    if !path.is_file() {
        eprintln!("mac-quicklook: not a file: {}", path.display());
        return Ok(());
    }

    if ipc::send(&path) {
        return Ok(()); // the running window takes over
    }
    ui::run(path)
}
