// SPDX-License-Identifier: GPL-3.0-only

mod app;
mod wayland_handler;
mod wayland_subscription;

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt::init();
    let _ = tracing_log::LogTracer::init();

    let settings = cosmic::app::Settings::default()
        .no_main_window(true)
        .transparent(true)
        .client_decorations(false);
    cosmic::app::run::<app::Dock>(settings, ())
}
