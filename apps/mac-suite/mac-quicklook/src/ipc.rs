// SPDX-License-Identifier: GPL-3.0-only

//! Single-instance control over a Unix socket in `$XDG_RUNTIME_DIR`.
//!
//! The wire format is one line holding an absolute file path.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

/// The control socket, or `None` when no private directory is available.
pub fn socket_path() -> Option<PathBuf> {
    private_runtime_dir().map(|dir| dir.join("mac-quicklook.sock"))
}

/// A directory only this user can write to: `$XDG_RUNTIME_DIR`, or else a
/// private (0700) folder under `~/.cache`. Never the shared `/tmp`.
pub fn private_runtime_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let dir = PathBuf::from(std::env::var_os("HOME")?).join(".cache/mac-suite");
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).ok()?;
    let meta = std::fs::symlink_metadata(&dir).ok()?;
    (meta.is_dir() && meta.permissions().mode() & 0o077 == 0).then_some(dir)
}

/// Encodes a path as one wire line (newlines in names are not supported).
pub fn encode(path: &Path) -> Option<String> {
    let s = path.to_str()?;
    (!s.contains('\n')).then(|| format!("{s}\n"))
}

pub fn decode(line: &str) -> Option<PathBuf> {
    let s = line.strip_suffix('\n').unwrap_or(line);
    let s = s.strip_suffix('\r').unwrap_or(s);
    (!s.is_empty()).then(|| PathBuf::from(s))
}

/// Sends a path to a running instance. `false` if none is listening.
pub fn send(path: &Path) -> bool {
    let Some(sock) = socket_path() else {
        return false;
    };
    let Some(line) = encode(path) else {
        return false;
    };
    let Ok(mut stream) = UnixStream::connect(sock) else {
        return false;
    };
    stream.write_all(line.as_bytes()).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let p = Path::new("/home/a b/c.txt");
        assert_eq!(decode(&encode(p).unwrap()), Some(p.to_path_buf()));
        assert_eq!(decode("\n"), None);
        assert_eq!(encode(Path::new("/a\nb")), None);
    }
}
