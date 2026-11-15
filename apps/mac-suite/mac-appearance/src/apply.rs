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

fn new_background(output: &str, path: &str) -> String {
    format!(
        "(\n    output: {},\n    source: Path({}),\n    filter_by_theme: false,\n    rotation_frequency: 300,\n    filter_method: Lanczos,\n    scaling_mode: Zoom,\n    sampling_method: Alphanumeric,\n)",
        ron_string(output),
        ron_string(path),
    )
}

fn format_backgrounds(outputs: &[(&str, &Path)]) -> String {
    if outputs.is_empty() {
        return "[]".to_owned();
    }
    let mut out = String::from("[\n");
    for (conn, _) in outputs {
        out.push_str("    ");
        out.push_str(&ron_string(conn));
        out.push_str(",\n");
    }
    out.push(']');
    out
}

fn write_entry(dir: &Path, file_name: &str, output: &str, image: &Path) -> io::Result<()> {
    let image = image.to_string_lossy();
    let source = format!("Path({})", ron_string(&image));
    let file = dir.join(file_name);
    let text = match fs::read_to_string(&file) {
        Ok(old) => replace_source(&old, &source).unwrap_or_else(|| new_background(output, &image)),
        Err(_) => new_background(output, &image),
    };
    write_if_changed(&file, &text)?;
    Ok(())
}

/// Point the cosmic-bg "all outputs" wallpaper at an image, keeping its other settings.
pub fn set_wallpaper(image: &Path) -> io::Result<()> {
    set_wallpapers_in(&cosmic_dir(), Some(image), &[])
}

/// Write the `all` wallpaper (for shared/future outputs) and any per-output
/// entries into `com.system76.CosmicBackground/v1`.
///
/// Sets `same-on-all` to `false` only when `outputs` is non-empty, and `true`
/// otherwise.
pub fn set_wallpapers(all: Option<&Path>, outputs: &[(&str, &Path)]) -> io::Result<()> {
    set_wallpapers_in(&cosmic_dir(), all, outputs)
}

fn set_wallpapers_in(
    cosmic: &Path,
    all: Option<&Path>,
    outputs: &[(&str, &Path)],
) -> io::Result<()> {
    if let Some(image) = all
        && !image.is_file()
    {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} is not a file", image.display()),
        ));
    }
    for (_, image) in outputs {
        if !image.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} is not a file", image.display()),
            ));
        }
    }

    let dir = cosmic.join("com.system76.CosmicBackground/v1");
    if let Some(image) = all {
        write_entry(&dir, "all", "all", image)?;
    }

    if outputs.is_empty() {
        write_if_changed(&dir.join("same-on-all"), "true")?;
        if dir.join("backgrounds").exists() {
            write_if_changed(&dir.join("backgrounds"), "[]")?;
        }
    } else {
        for (conn, image) in outputs {
            write_entry(&dir, &format!("output.{conn}"), conn, image)?;
        }
        write_if_changed(&dir.join("same-on-all"), "false")?;
        write_if_changed(&dir.join("backgrounds"), &format_backgrounds(outputs))?;
    }
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
        set_wallpapers_in(&root, Some(&img), &[]).unwrap();
        let text = fs::read_to_string(bg.join("all")).unwrap();
        assert!(text.contains(&format!("source: Path(\"{}\"),", img.display())));
        assert!(text.contains("rotation_frequency: 300"));
        assert_eq!(fs::read_to_string(bg.join("same-on-all")).unwrap(), "true");
        assert!(set_wallpapers_in(&root, Some(&root.join("missing.jpg")), &[]).is_err());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn per_screen_cosmic_bg_golden() {
        let root = tmp("per-screen");
        let img_all = root.join("all.jpg");
        let img_dp2 = root.join("dp2.jpg");
        let img_hdmi = root.join("hdmi.png");
        fs::write(&img_all, b"a").unwrap();
        fs::write(&img_dp2, b"b").unwrap();
        fs::write(&img_hdmi, b"c").unwrap();

        set_wallpapers_in(
            &root,
            Some(&img_all),
            &[("DP-2", &img_dp2), ("HDMI-A-2", &img_hdmi)],
        )
        .unwrap();

        let bg = root.join("com.system76.CosmicBackground/v1");
        assert_eq!(fs::read_to_string(bg.join("same-on-all")).unwrap(), "false");
        assert_eq!(
            fs::read_to_string(bg.join("backgrounds")).unwrap(),
            "[\n    \"DP-2\",\n    \"HDMI-A-2\",\n]"
        );
        assert_eq!(
            fs::read_to_string(bg.join("all")).unwrap(),
            format!(
                "(\n    output: \"all\",\n    source: Path(\"{}\"),\n    filter_by_theme: false,\n    rotation_frequency: 300,\n    filter_method: Lanczos,\n    scaling_mode: Zoom,\n    sampling_method: Alphanumeric,\n)",
                img_all.display()
            )
        );
        assert_eq!(
            fs::read_to_string(bg.join("output.DP-2")).unwrap(),
            format!(
                "(\n    output: \"DP-2\",\n    source: Path(\"{}\"),\n    filter_by_theme: false,\n    rotation_frequency: 300,\n    filter_method: Lanczos,\n    scaling_mode: Zoom,\n    sampling_method: Alphanumeric,\n)",
                img_dp2.display()
            )
        );
        assert_eq!(
            fs::read_to_string(bg.join("output.HDMI-A-2")).unwrap(),
            format!(
                "(\n    output: \"HDMI-A-2\",\n    source: Path(\"{}\"),\n    filter_by_theme: false,\n    rotation_frequency: 300,\n    filter_method: Lanczos,\n    scaling_mode: Zoom,\n    sampling_method: Alphanumeric,\n)",
                img_hdmi.display()
            )
        );

        // Switching back to global-only sets same-on-all back to true and clears backgrounds.
        set_wallpapers_in(&root, Some(&img_all), &[]).unwrap();
        assert_eq!(fs::read_to_string(bg.join("same-on-all")).unwrap(), "true");
        assert_eq!(fs::read_to_string(bg.join("backgrounds")).unwrap(), "[]");

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
