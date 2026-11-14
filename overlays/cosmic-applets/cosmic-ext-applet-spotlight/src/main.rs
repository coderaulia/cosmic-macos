// SPDX-License-Identifier: GPL-3.0-only

use cosmic::{
    Element, Task,
    app::{self, Core},
    applet::token::subscription::{TokenRequest, TokenUpdate, activation_token_subscription},
    cctk::sctk::reexports::calloop,
    iced::Subscription,
    widget::autosize,
};
use cosmic_ext_mac_applet_style::{
    PillVariant, icons::spotlight_icon, mac_pill_button, panel_icon_size,
};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

static AUTOSIZE_MAIN_ID: LazyLock<cosmic::widget::Id> =
    LazyLock::new(|| cosmic::widget::Id::new("autosize-main"));

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt::init();
    let _ = tracing_log::LogTracer::init();
    cosmic::applet::run::<SpotlightApplet>(())
}

struct SpotlightApplet {
    core: Core,
    token_tx: Option<calloop::channel::Sender<TokenRequest>>,
}

#[derive(Debug, Clone)]
enum Message {
    Launch,
    Token(TokenUpdate),
}

/// Prefer `~/.local/bin/cosmic-launcher` (our restyled Spotlight launcher),
/// falling back to `cosmic-launcher` on `PATH`.
pub fn resolve_launcher_binary(home: Option<&Path>) -> PathBuf {
    if let Some(home) = home {
        let local = home.join(".local/bin/cosmic-launcher");
        if local.is_file() {
            return local;
        }
    }
    PathBuf::from("cosmic-launcher")
}

fn spawn_launcher(token: Option<&str>) {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let bin = resolve_launcher_binary(home.as_deref());
    let mut cmd = std::process::Command::new(bin);
    if let Some(token) = token {
        cmd.env("XDG_ACTIVATION_TOKEN", token);
        cmd.env("DESKTOP_STARTUP_ID", token);
    }
    tokio::spawn(cosmic::process::spawn(cmd));
}

impl cosmic::Application for SpotlightApplet {
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = "dev.asw.CosmicExtAppletSpotlight";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, _flags: Self::Flags) -> (Self, app::Task<Self::Message>) {
        (
            Self {
                core,
                token_tx: None,
            },
            Task::none(),
        )
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        activation_token_subscription(0).map(Message::Token)
    }

    fn update(&mut self, message: Self::Message) -> app::Task<Self::Message> {
        match message {
            Message::Launch => {
                if let Some(tx) = self.token_tx.as_ref() {
                    let _ = tx.send(TokenRequest {
                        app_id: Self::APP_ID.to_string(),
                        exec: "cosmic-launcher".to_string(),
                    });
                } else {
                    spawn_launcher(None);
                }
            }
            Message::Token(u) => match u {
                TokenUpdate::Init(tx) => self.token_tx = Some(tx),
                TokenUpdate::Finished => self.token_tx = None,
                TokenUpdate::ActivationToken { token, .. } => {
                    spawn_launcher(token.as_deref());
                }
            },
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Self::Message> {
        let max_icon = panel_icon_size();
        let icon_size = self.core.applet.suggested_size(true).0.min(max_icon);
        let btn = mac_pill_button(spotlight_icon(icon_size), false, PillVariant::IconOnly)
            .on_press(Message::Launch);

        autosize::autosize(btn, AUTOSIZE_MAIN_ID.clone()).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn falls_back_to_path_when_local_missing() {
        let dir = std::env::temp_dir().join(format!("spotlight_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(
            resolve_launcher_binary(Some(&dir)),
            PathBuf::from("cosmic-launcher")
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prefers_local_bin_launcher_when_present() {
        let dir = std::env::temp_dir().join(format!("spotlight_test_local_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let bin_dir = dir.join(".local/bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let local_bin = bin_dir.join("cosmic-launcher");
        fs::write(&local_bin, b"#!/bin/sh\n").unwrap();
        assert_eq!(resolve_launcher_binary(Some(&dir)), local_bin);
        let _ = fs::remove_dir_all(&dir);
    }
}
