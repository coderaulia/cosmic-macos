// SPDX-License-Identifier: GPL-3.0-only
//! macOS Sonoma-style desktop widgets (`Weather`, `UpNext`, `System`, `Clock`).

mod agenda;
mod cards;
mod ipc;
mod layout;
mod ui;
mod wayland;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "mac-desktop-widgets",
    version,
    about = "macOS Sonoma-style desktop widgets for COSMIC"
)]
struct Cli {
    /// Open or toggle the "Edit Widgets…" gallery strip.
    #[arg(long)]
    edit: bool,
    /// Stop a running desktop widgets daemon.
    #[arg(long)]
    quit: bool,
}

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    if cli.quit {
        let _ = ipc::send(ipc::Command::Quit);
        return Ok(());
    }
    if cli.edit && ipc::send(ipc::Command::Edit) {
        return Ok(());
    }
    if !cli.edit && ipc::is_running() {
        return Ok(());
    }

    ui::run(cli.edit)
}
