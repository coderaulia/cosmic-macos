// SPDX-License-Identifier: GPL-3.0-only
//! Single-instance IPC over a Unix socket in `$XDG_RUNTIME_DIR`.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use cosmic::iced::{
    Subscription,
    futures::{SinkExt, channel::mpsc},
    stream,
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixListener;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Edit,
    Quit,
}

impl Command {
    pub fn as_str(self) -> &'static str {
        match self {
            Command::Edit => "edit",
            Command::Quit => "quit",
        }
    }

    pub fn parse(line: &str) -> Option<Self> {
        match line.trim() {
            "edit" => Some(Command::Edit),
            "quit" => Some(Command::Quit),
            _ => None,
        }
    }
}

pub fn socket_path() -> Option<PathBuf> {
    private_runtime_dir().map(|dir| dir.join("mac-desktop-widgets.sock"))
}

fn private_runtime_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let dir = PathBuf::from(std::env::var_os("HOME")?).join(".cache/mac-suite");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .ok()?;
    let meta = std::fs::symlink_metadata(&dir).ok()?;
    (meta.is_dir() && meta.permissions().mode() & 0o077 == 0).then_some(dir)
}

/// Returns `true` if a desktop-widgets instance is already listening on the IPC socket.
pub fn is_running() -> bool {
    socket_path().is_some_and(|path| UnixStream::connect(path).is_ok())
}

/// Sends a command to an already-running instance. Returns `false` if none is listening.
pub fn send(cmd: Command) -> bool {
    let Some(path) = socket_path() else {
        return false;
    };
    let Ok(mut stream) = UnixStream::connect(path) else {
        return false;
    };
    writeln!(stream, "{}", cmd.as_str()).is_ok()
}

pub fn subscription() -> Subscription<Command> {
    Subscription::run_with(std::any::TypeId::of::<Command>(), |_| {
        stream::channel(16, |mut output: mpsc::Sender<Command>| async move {
            let Some(path) = socket_path() else {
                cosmic::iced::futures::future::pending::<()>().await;
                return;
            };
            let _ = std::fs::remove_file(&path);
            let Ok(listener) = UnixListener::bind(&path) else {
                tracing::warn!(?path, "Failed to bind desktop-widgets IPC socket");
                cosmic::iced::futures::future::pending::<()>().await;
                return;
            };
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).await.is_ok()
                    && let Some(cmd) = Command::parse(&line)
                {
                    let _ = output.send(cmd).await;
                }
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_roundtrip() {
        assert_eq!(Command::parse(Command::Edit.as_str()), Some(Command::Edit));
        assert_eq!(Command::parse(Command::Quit.as_str()), Some(Command::Quit));
        assert_eq!(Command::parse(" edit \n"), Some(Command::Edit));
        assert_eq!(Command::parse("unknown"), None);
    }
}
