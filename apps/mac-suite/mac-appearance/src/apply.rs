// SPDX-License-Identifier: GPL-3.0-only

//! Writers for the COSMIC config: dark mode and the cosmic-bg wallpaper.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use crate::config::config_home;

fn cosmic_dir() -> PathBuf {
    config_home().join("cosmic")
}

/// Write `text` only if the file does not already hold it. Returns whether it wrote.
fn write_if_changed(path: &Path, text: &str) -> io::Result<bool> {
    if fs::read_to_string(path).is_ok_and(|old| old.trim() == text) {
        return Ok(false);
    }
    crate::config::write_file(path, text)?;
    Ok(true)
}

/// Set dark or light mode and turn COSMIC's own auto-switch off.
pub fn set_dark(dark: bool) -> io::Result<()> {
    set_dark_in(&cosmic_dir(), dark)
}

fn set_dark_in(cosmic: &Path, dark: bool) -> io::Result<()> {
    let dir = cosmic.join("com.system76.CosmicTheme.Mode/v1");
    write_if_changed(&dir.join("auto_switch"), "false")?;
    write_if_changed(&dir.join("is_dark"), if dark { "true" } else { "false" })?;
    Ok(())
}

fn ron_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Replace the value of the `source:` field (which may span several lines)
/// with `new_value`, leaving the rest of the file untouched.
fn replace_source(text: &str, new_value: &str) -> Option<String> {
    let key = text
        .match_indices("source:")
        .find(|(i, _)| text[..*i].chars().rev().take_while(|c| *c != '\n').all(|c| c == ' ' || c == '\t'))?
        .0;
    let start = key + "source:".len();
    let (mut depth, mut in_str, mut escaped) = (0i32, false, false);
    let mut end = text.len();
    for (i, c) in text[start..].char_indices() {
        if in_str {
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' | '\n' if depth <= 0 => {
                end = start + i;
                break;
            }
            _ => {}
        }
    }
    Some(format!("{} {}{}", &text[..start], new_value, &text[end..]))
}

fn new_background(path: &str) -> String {
    format!(
        "(\n    output: \"all\",\n    source: Path({}),\n    filter_by_theme: false,\n    rotation_frequency: 300,\n    filter_method: Lanczos,\n    scaling_mode: Zoom,\n    sampling_method: Alphanumeric,\n)",
        ron_string(path)
    )
}

/// Point the cosmic-bg "all outputs" wallpaper at an image, keeping its other settings.
pub fn set_wallpaper(image: &Path) -> io::Result<()> {
    set_wallpaper_in(&cosmic_dir(), image)
}

fn set_wallpaper_in(cosmic: &Path, image: &Path) -> io::Result<()> {
    if !image.is_file() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("{} is not a file", image.display())));
    }
    let image = image.to_string_lossy();
    let dir = cosmic.join("com.system76.CosmicBackground/v1");
    let source = format!("Path({})", ron_string(&image));
    let file = dir.join("all");
    let text = match fs::read_to_string(&file) {
        Ok(old) => replace_source(&old, &source).unwrap_or_else(|| new_background(&image)),
        Err(_) => new_background(&image),
    };
    write_if_changed(&file, &text)?;
    // Make cosmic-bg use one wallpaper for every output.
    write_if_changed(&dir.join("same-on-all"), "true")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "(\n    output: \"all\",\n    source: Path(\"/old/a.jpg\"),\n    filter_by_theme: false,\n    rotation_frequency: 300,\n)";

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mac-appearance-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn replaces_path_source() {
        let out = replace_source(SAMPLE, "Path(\"/new/b.png\")").unwrap();
        assert!(out.contains("    source: Path(\"/new/b.png\"),\n    filter_by_theme: false,"));
        assert!(!out.contains("/old/a.jpg"));
    }

    #[test]
    fn replaces_multiline_source() {
        let text = "(\n    source: Color(Gradient((\n        colors: [[0.1, 0.2, 0.3]],\n        radius: 1.0,\n    ))),\n    output: \"all\",\n)";
        let out = replace_source(text, "Path(\"/x.jpg\")").unwrap();
        assert_eq!(out, "(\n    source: Path(\"/x.jpg\"),\n    output: \"all\",\n)");
    }

    #[test]
    fn source_with_comma_in_path() {
        let text = "(\n    source: Path(\"/a,b)/c.jpg\"),\n    x: 1,\n)";
        let out = replace_source(text, "Path(\"/z\")").unwrap();
        assert_eq!(out, "(\n    source: Path(\"/z\"),\n    x: 1,\n)");
    }

    #[test]
    fn wallpaper_written_once() {
        let root = tmp("wall");
        let img = root.join("pic.jpg");
        fs::write(&img, b"x").unwrap();
        let bg = root.join("com.system76.CosmicBackground/v1");
        fs::create_dir_all(&bg).unwrap();
        fs::write(bg.join("all"), SAMPLE).unwrap();
        set_wallpaper_in(&root, &img).unwrap();
        let text = fs::read_to_string(bg.join("all")).unwrap();
        assert!(text.contains(&format!("source: Path(\"{}\"),", img.display())));
        assert!(text.contains("rotation_frequency: 300"));
        assert!(set_wallpaper_in(&root, &root.join("missing.jpg")).is_err());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn dark_mode_only_writes_on_change() {
        let root = tmp("dark");
        set_dark_in(&root, true).unwrap();
        let dir = root.join("com.system76.CosmicTheme.Mode/v1");
        assert_eq!(fs::read_to_string(dir.join("is_dark")).unwrap(), "true");
        assert_eq!(fs::read_to_string(dir.join("auto_switch")).unwrap(), "false");
        assert!(!write_if_changed(&dir.join("is_dark"), "true").unwrap());
        set_dark_in(&root, false).unwrap();
        assert_eq!(fs::read_to_string(dir.join("is_dark")).unwrap(), "false");
        fs::remove_dir_all(&root).unwrap();
    }
}
