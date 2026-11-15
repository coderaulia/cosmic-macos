// SPDX-License-Identifier: GPL-3.0-only

//! Saving, clipboard and desktop integration.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use image::RgbaImage;

use crate::config::SaveTo;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Msg(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Image(#[from] image::ImageError),
}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Self::Msg(s)
    }
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// Resolve `key` (e.g. `XDG_DESKTOP_DIR`) from the text of `user-dirs.dirs`.
pub fn parse_user_dir(text: &str, key: &str, home: &Path) -> Option<PathBuf> {
    text.lines().find_map(|line| {
        let (k, v) = line.trim().split_once('=')?;
        if k != key {
            return None;
        }
        let v = v.trim().trim_matches('"');
        match v.strip_prefix("$HOME") {
            Some(rest) => Some(home.join(rest.trim_start_matches('/'))),
            None => v.starts_with('/').then(|| PathBuf::from(v)),
        }
    })
}

fn user_dir(key: &str, fallback: &str) -> PathBuf {
    let home = home();
    let cfg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    std::fs::read_to_string(cfg.join("user-dirs.dirs"))
        .ok()
        .and_then(|t| parse_user_dir(&t, key, &home))
        .unwrap_or_else(|| home.join(fallback))
}

/// Target directory for a destination; `None` for the clipboard.
pub fn target_dir(save_to: SaveTo) -> Option<PathBuf> {
    match save_to {
        SaveTo::Desktop => Some(user_dir("XDG_DESKTOP_DIR", "Desktop")),
        SaveTo::Pictures => Some(user_dir("XDG_PICTURES_DIR", "Pictures")),
        SaveTo::Screenshots => Some(user_dir("XDG_PICTURES_DIR", "Pictures").join("Screenshots")),
        SaveTo::Clipboard => None,
    }
}

/// `Screenshot 2026-11-15 at 14.03.09.png`; `suffix` disambiguates multiple
/// captures in the same second (e.g. one per output).
pub fn file_name(stamp: &str, suffix: Option<usize>) -> String {
    match suffix {
        Some(n) if n > 1 => format!("Screenshot {stamp} ({n}).png"),
        _ => format!("Screenshot {stamp}.png"),
    }
}

pub fn timestamp() -> String {
    jiff::Zoned::now().strftime("%Y-%m-%d at %H.%M.%S").to_string()
}

/// First non-existing path for `name` in `dir` ("name (2).png", ...).
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let stem = name.strip_suffix(".png").unwrap_or(name);
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}).png")))
        .find(|p| !p.exists())
        .expect("infinite iterator")
}

pub fn save_png(img: &RgbaImage, dir: &Path, name: &str) -> Result<PathBuf, Error> {
    std::fs::create_dir_all(dir)?;
    let path = unique_path(dir, name);
    img.save_with_format(&path, image::ImageFormat::Png)?;
    Ok(path)
}

pub fn encode_png(img: &RgbaImage) -> Result<Vec<u8>, Error> {
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png)?;
    Ok(buf.into_inner())
}

/// Put PNG bytes on the Wayland clipboard. Prefers `wl-copy` (which forks
/// into the background itself), falls back to wl-clipboard-rs.
pub fn copy_png(png: &[u8]) -> Result<(), Error> {
    let spawned = Command::new("wl-copy")
        .args(["--type", "image/png"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Ok(mut child) = spawned {
        let wrote = match child.stdin.take() {
            Some(mut stdin) => stdin.write_all(png).is_ok(),
            None => false,
        };
        if child.wait().is_ok_and(|s| s.success()) && wrote {
            return Ok(());
        }
    }
    use wl_clipboard_rs::copy::{MimeType, Options, Source};
    Options::new()
        .copy(Source::Bytes(png.to_vec().into()), MimeType::Specific("image/png".into()))
        .map_err(|e| Error::Msg(format!("clipboard: {e}")))
}

pub fn copy_file(path: &Path) -> Result<(), Error> {
    copy_png(&std::fs::read(path)?)
}

/// Open a file or directory with the default application.
pub fn xdg_open(path: &Path) {
    let _ = Command::new("xdg-open")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

pub fn show_in_files(path: &Path) {
    xdg_open(path.parent().unwrap_or(path));
}

/// Ask the xdg-desktop-portal for a screenshot and return the file it wrote.
pub fn portal_screenshot() -> Result<PathBuf, Error> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        use zbus::zvariant::{OwnedValue, Value};
        use zbus::{Connection, Proxy};
        let err = |e: zbus::Error| Error::Msg(format!("portal: {e}"));

        let conn = Connection::session().await.map_err(err)?;
        let sender = conn
            .unique_name()
            .ok_or_else(|| Error::Msg("portal: no bus name".into()))?
            .trim_start_matches(':')
            .replace('.', "_");
        let token = format!("macshot{}", std::process::id());
        let request_path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");

        // Subscribe before calling so the Response cannot be missed.
        let request = Proxy::new(
            &conn,
            "org.freedesktop.portal.Desktop",
            request_path,
            "org.freedesktop.portal.Request",
        )
        .await
        .map_err(err)?;
        let mut responses = request.receive_signal("Response").await.map_err(err)?;

        let portal = Proxy::new(
            &conn,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Screenshot",
        )
        .await
        .map_err(err)?;
        let mut options: std::collections::HashMap<&str, Value<'_>> = Default::default();
        options.insert("handle_token", Value::from(token.as_str()));
        options.insert("interactive", Value::from(false));
        let _: zbus::zvariant::OwnedObjectPath =
            portal.call("Screenshot", &("", options)).await.map_err(err)?;

        use futures_util::StreamExt;
        let msg = responses
            .next()
            .await
            .ok_or_else(|| Error::Msg("portal: no response".into()))?;
        let (code, results): (u32, std::collections::HashMap<String, OwnedValue>) =
            msg.body().deserialize().map_err(|e| Error::Msg(format!("portal: {e}")))?;
        if code != 0 {
            return Err(Error::Msg("portal: screenshot denied or cancelled".into()));
        }
        let uri: String = results
            .get("uri")
            .and_then(|v| v.downcast_ref::<&str>().ok().map(str::to_owned))
            .ok_or_else(|| Error::Msg("portal: no uri in response".into()))?;
        uri.strip_prefix("file://")
            .map(|p| PathBuf::from(percent_decode(p)))
            .ok_or_else(|| Error::Msg(format!("portal: unsupported uri {uri}")))
    })
}

/// Minimal `%XX` decoder for file URIs.
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(
            file_name("2026-11-15 at 14.03.09", None),
            "Screenshot 2026-11-15 at 14.03.09.png"
        );
        assert_eq!(
            file_name("2026-11-15 at 14.03.09", Some(2)),
            "Screenshot 2026-11-15 at 14.03.09 (2).png"
        );
    }

    #[test]
    fn user_dirs() {
        let t = "XDG_DESKTOP_DIR=\"$HOME/Schreibtisch\"\nXDG_PICTURES_DIR=\"/data/pics\"\n";
        let home = Path::new("/home/u");
        assert_eq!(parse_user_dir(t, "XDG_DESKTOP_DIR", home), Some(PathBuf::from("/home/u/Schreibtisch")));
        assert_eq!(parse_user_dir(t, "XDG_PICTURES_DIR", home), Some(PathBuf::from("/data/pics")));
        assert_eq!(parse_user_dir(t, "XDG_MISSING", home), None);
    }

    #[test]
    fn percent() {
        assert_eq!(percent_decode("/tmp/a%20b.png"), "/tmp/a b.png");
        assert_eq!(percent_decode("/tmp/100%"), "/tmp/100%");
    }

    #[test]
    fn unique_paths_do_not_collide() {
        let dir = std::env::temp_dir().join(format!("macshot-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = unique_path(&dir, "S.png");
        std::fs::write(&a, b"x").unwrap();
        assert_eq!(unique_path(&dir, "S.png"), dir.join("S (2).png"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
