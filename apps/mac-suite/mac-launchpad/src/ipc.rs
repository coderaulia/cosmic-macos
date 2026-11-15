// SPDX-License-Identifier: GPL-3.0-only

//! Single-instance control over a Unix socket in `$XDG_RUNTIME_DIR`.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Toggle,
    Show,
    Hide,
}

impl Command {
    pub fn as_str(self) -> &'static str {
        match self {
            Command::Toggle => "toggle",
            Command::Show => "show",
            Command::Hide => "hide",
        }
    }

    pub fn parse(line: &str) -> Option<Self> {
        match line.trim() {
            "toggle" => Some(Command::Toggle),
            "show" => Some(Command::Show),
            "hide" => Some(Command::Hide),
            _ => None,
        }
    }
}

/// The control socket, or `None` when no private directory is available.
pub fn socket_path() -> Option<PathBuf> {
    private_runtime_dir().map(|dir| dir.join("mac-launchpad.sock"))
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


/// Sends a command to a running instance. `false` if none is listening.
pub fn send(cmd: Command) -> bool {
    let Some(path) = socket_path() else {
        return false;
    };
    let Ok(mut stream) = UnixStream::connect(path) else {
        return false;
    };
    writeln!(stream, "{}", cmd.as_str()).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_roundtrip() {
        for c in [Command::Toggle, Command::Show, Command::Hide] {
            assert_eq!(Command::parse(c.as_str()), Some(c));
        }
        assert_eq!(Command::parse(" toggle\n"), Some(Command::Toggle));
        assert_eq!(Command::parse("nope"), None);
    }
}
