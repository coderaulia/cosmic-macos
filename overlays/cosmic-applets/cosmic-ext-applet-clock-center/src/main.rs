// SPDX-License-Identifier: GPL-3.0-only

pub use cosmic_applets_config::time as config;

mod localize;
mod mpris_subscription;
mod subscriptions;
mod time;
mod window;

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt::init();
    let _ = tracing_log::LogTracer::init();

    localize::localize();
    cosmic::applet::run::<window::Window>(())
}
