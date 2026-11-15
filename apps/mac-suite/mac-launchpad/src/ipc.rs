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

pub fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join("mac-launchpad.sock")
}

/// Sends a command to a running instance. `false` if none is listening.
pub fn send(cmd: Command) -> bool {
    let Ok(mut stream) = UnixStream::connect(socket_path()) else {
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
