// SPDX-License-Identifier: GPL-3.0-only

//! Launchpad: full-screen application grid.
//!
//! `mac-launchpad` toggles the resident instance (starting it when absent);
//! `mac-launchpad --daemon` starts it hidden.

mod apps;
mod ipc;
mod ui;

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let daemon = std::env::args().skip(1).any(|a| a == "--daemon");
    if std::env::args().skip(1).any(|a| a == "-h" || a == "--help") {
        println!("usage: mac-launchpad [--daemon]");
        return Ok(());
    }

    if daemon {
        if ipc::send(ipc::Command::Hide) {
            return Ok(()); // already running
        }
    } else if ipc::send(ipc::Command::Toggle) {
        return Ok(());
    }

    // Nobody is listening; the socket subscription replaces any stale file.
    ui::run(!daemon)
}
