// SPDX-License-Identifier: GPL-3.0-only

//! Loading file previews (blocking; call from a worker thread).

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use cosmic::widget::{image, svg};

use crate::files;
use crate::ipc;
use crate::kind::{self, Kind};

pub const MAX_LINES: usize = 2000;
const MAX_TEXT_BYTES: usize = 512 * 1024;
const MAX_IMAGE_SIDE: u32 = 2560;
pub const HEADER_H: f32 = 48.0;
pub const PDF_FOOTER_H: f32 = 40.0;

/// A run of text with one colour.
#[derive(Debug, Clone, PartialEq)]
pub struct Piece {
    pub text: String,
    pub color: Option<[u8; 3]>,
}

#[derive(Debug, Clone)]
pub enum Body {
    Image { handle: image::Handle, w: f32, h: f32 },
    Svg { handle: svg::Handle, w: f32, h: f32 },
    Pdf { pages: u32, page: u32, handle: image::Handle, w: f32, h: f32 },
    Text { pieces: Vec<Piece>, truncated: bool },
    /// Video poster frame (None when no thumbnailer could produce one).
    Video { poster: Option<(image::Handle, f32, f32)> },
    Info,
}

#[derive(Debug, Clone)]
pub struct Preview {
    pub path: PathBuf,
    pub name: String,
    pub kind: Kind,
    pub mime: String,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub open_with: Option<String>,
    pub body: Body,
}

pub fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

pub fn load(path: &Path, dark: bool) -> Preview {
    let meta = std::fs::metadata(path).ok();
    let (kind, mime) = kind::detect(path);
    let body = match kind {
        Kind::Image => decode_image(path)
            .map(|(handle, w, h)| Body::Image { handle, w, h })
            .unwrap_or(Body::Info),
        Kind::Svg => {
            let (w, h) = std::fs::read_to_string(path)
                .ok()
                .and_then(|s| svg_size(&s))
                .unwrap_or((640.0, 480.0));
            Body::Svg { handle: svg::Handle::from_path(path), w, h }
        }
        Kind::Pdf => load_pdf(path, 1).unwrap_or(Body::Info),
        Kind::Text => load_text(path, dark),
        Kind::Video => Body::Video { poster: video_poster(path) },
        Kind::Audio | Kind::Other => Body::Info,
    };
    Preview {
        name: file_name(path),
        open_with: default_app_name(&mime),
        path: path.to_path_buf(),
        kind,
        mime,
        size: meta.as_ref().map_or(0, |m| m.len()),
        modified: meta.and_then(|m| m.modified().ok()),
        body,
    }
}

/// Decodes a raster image to an RGBA handle, downscaling huge ones.
pub fn decode_image(path: &Path) -> Option<(image::Handle, f32, f32)> {
    let mut img = ::image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?;
    if img.width().max(img.height()) > MAX_IMAGE_SIDE {
        img = img.thumbnail(MAX_IMAGE_SIDE, MAX_IMAGE_SIDE);
    }
    let rgba = img.into_rgba8();
    let (w, h) = rgba.dimensions();
    Some((image::Handle::from_rgba(w, h, rgba.into_raw()), w as f32, h as f32))
}

/// Per-process scratch directory for rendered pages and posters.
pub fn cache_dir() -> Option<PathBuf> {
    let dir = ipc::private_runtime_dir()?.join(format!("mac-quicklook-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

pub fn cleanup_cache() {
    if let Some(dir) = ipc::private_runtime_dir() {
        let _ = std::fs::remove_dir_all(dir.join(format!("mac-quicklook-{}", std::process::id())));
    }
}

fn pdf_page_count(path: &Path) -> u32 {
    let Ok(out) = Command::new("pdfinfo").arg(path).stderr(Stdio::null()).output() else {
        return 1;
    };
    parse_pdf_pages(&String::from_utf8_lossy(&out.stdout)).unwrap_or(1)
}

pub fn parse_pdf_pages(info: &str) -> Option<u32> {
    info.lines()
        .find_map(|l| l.strip_prefix("Pages:"))
        .and_then(|v| v.trim().parse().ok())
        .filter(|n| *n > 0)
}

/// Renders one PDF page (1-based) with poppler.
pub fn render_pdf_page(path: &Path, page: u32) -> Option<(image::Handle, f32, f32)> {
    let dir = cache_dir()?;
    let stem = dir.join(format!("page-{page}"));
    let status = Command::new("pdftocairo")
        .args(["-png", "-r", "110", "-singlefile", "-f"])
        .arg(page.to_string())
        .arg("-l")
        .arg(page.to_string())
        .arg(path)
        .arg(&stem)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let png = stem.with_extension("png");
    let r = decode_image(&png);
    let _ = std::fs::remove_file(png);
    r
}

fn load_pdf(path: &Path, page: u32) -> Option<Body> {
    let pages = pdf_page_count(path);
    let (handle, w, h) = render_pdf_page(path, page)?;
    Some(Body::Pdf { pages, page, handle, w, h })
}

fn video_poster(path: &Path) -> Option<(image::Handle, f32, f32)> {
    let dir = cache_dir()?;
    let out = dir.join("poster.png");
    let status = Command::new("ffmpegthumbnailer")
        .arg("-i")
        .arg(path)
        .arg("-o")
        .arg(&out)
        .args(["-s", "1280", "-t", "10%"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let r = decode_image(&out);
    let _ = std::fs::remove_file(out);
    r
}

/// Reads at most `MAX_LINES` lines / `MAX_TEXT_BYTES` bytes.
pub fn read_lines(path: &Path) -> (Vec<String>, bool) {
    let Ok(file) = std::fs::File::open(path) else {
        return (Vec::new(), false);
    };
    // Bound the reader itself, so a huge file without newlines can never be
    // read into memory whole; one extra byte tells whether the file is longer.
    let mut reader = BufReader::new(file.take(MAX_TEXT_BYTES as u64 + 1));
    let mut lines = Vec::new();
    let mut bytes = 0;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return (lines, false),
            Ok(n) => {
                if lines.len() >= MAX_LINES {
                    return (lines, true);
                }
                if bytes + n > MAX_TEXT_BYTES {
                    // Keep what fits of the last (possibly partial) line.
                    buf.truncate(MAX_TEXT_BYTES - bytes);
                    if !buf.is_empty() {
                        lines.push(String::from_utf8_lossy(&buf).replace('\t', "    "));
                    }
                    return (lines, true);
                }
                bytes += n;
                let s = String::from_utf8_lossy(&buf);
                lines.push(s.trim_end_matches(['\n', '\r']).replace('\t', "    "));
            }
        }
    }
}

fn load_text(path: &Path, dark: bool) -> Body {
    let (lines, truncated) = read_lines(path);
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let pieces = highlight(&lines, ext, dark).unwrap_or_else(|| plain_pieces(&lines));
    Body::Text { pieces, truncated }
}

pub fn plain_pieces(lines: &[String]) -> Vec<Piece> {
    let mut text = lines.join("\n");
    text.push('\n');
    vec![Piece { text, color: None }]
}

fn highlight(lines: &[String], ext: &str, dark: bool) -> Option<Vec<Piece>> {
    use syntect::easy::HighlightLines;
    use syntect::highlighting::ThemeSet;
    use syntect::parsing::SyntaxSet;

    let syntaxes = SyntaxSet::load_defaults_newlines();
    let themes = ThemeSet::load_defaults();
    let syntax = syntaxes
        .find_syntax_by_extension(ext)
        .or_else(|| lines.first().and_then(|l| syntaxes.find_syntax_by_first_line(l)))?;
    let theme = themes.themes.get(if dark { "base16-ocean.dark" } else { "InspiredGitHub" })?;
    let mut hl = HighlightLines::new(syntax, theme);
    let mut pieces: Vec<Piece> = Vec::new();
    for line in lines {
        let with_nl = format!("{line}\n");
        let ranges = hl.highlight_line(&with_nl, &syntaxes).ok()?;
        for (style, text) in ranges {
            let c = style.foreground;
            let color = Some([c.r, c.g, c.b]);
            match pieces.last_mut() {
                Some(p) if p.color == color => p.text.push_str(text),
                _ => pieces.push(Piece { text: text.to_string(), color }),
            }
        }
    }
    Some(pieces)
}

/// Reads `width`/`height` (or the `viewBox`) of an SVG document.
pub fn svg_size(src: &str) -> Option<(f32, f32)> {
    let start = src.find("<svg")?;
    let tag = &src[start..start + src[start..].find('>')?];
    let attr = |name: &str| -> Option<String> {
        let mut from = 0;
        while let Some(i) = tag[from..].find(name) {
            let at = from + i;
            let before_ok = at == 0 || tag.as_bytes()[at - 1].is_ascii_whitespace();
            let rest = tag[at + name.len()..].trim_start();
            if before_ok && let Some(rest) = rest.strip_prefix('=') {
                let rest = rest.trim_start();
                let q = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
                let body = &rest[1..];
                return body.find(q).map(|e| body[..e].to_string());
            }
            from = at + name.len();
        }
        None
    };
    let num = |s: String| -> Option<f32> {
        let s = s.trim();
        if s.ends_with('%') {
            return None;
        }
        let end = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
        s[..end].parse::<f32>().ok().filter(|v| *v > 0.0)
    };
    if let (Some(w), Some(h)) = (attr("width").and_then(num), attr("height").and_then(num)) {
        return Some((w, h));
    }
    let vb = attr("viewBox")?;
    let v: Vec<f32> = vb
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    (v.len() == 4 && v[2] > 0.0 && v[3] > 0.0).then(|| (v[2], v[3]))
}

/// Name of the default application for a MIME type.
fn default_app_name(mime: &str) -> Option<String> {
    let out = Command::new("xdg-mime").args(["query", "default", mime]).stderr(Stdio::null()).output().ok()?;
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if id.is_empty() {
        return None;
    }
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    let data_dirs = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    let dirs = data_home.into_iter().chain(data_dirs.split(':').map(PathBuf::from));
    for dir in dirs {
        if let Ok(text) = std::fs::read_to_string(dir.join("applications").join(&id))
            && let Some(name) = desktop_name(&text)
        {
            return Some(name);
        }
    }
    None
}

pub fn desktop_name(text: &str) -> Option<String> {
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
        } else if in_entry && let Some(n) = line.strip_prefix("Name=") {
            return Some(n.to_string());
        }
    }
    None
}

/// Content size (without header) wanted for a preview at 1:1, as `(w, h)`.
pub fn content_size(body: &Body) -> (f32, f32) {
    match body {
        Body::Image { w, h, .. } | Body::Svg { w, h, .. } => (*w, *h),
        Body::Pdf { w, h, .. } => (*w, *h + PDF_FOOTER_H),
        Body::Video { poster: Some((_, w, h)) } => (*w, *h),
        Body::Text { .. } => (760.0, 560.0),
        Body::Video { poster: None } | Body::Info => (440.0, 340.0),
    }
}

/// Window size for a preview: fits the content, capped at 70% of the screen.
pub fn window_size(body: &Body, screen: (f32, f32)) -> (f32, f32) {
    fit_window(content_size(body), screen)
}

pub fn fit_window(content: (f32, f32), screen: (f32, f32)) -> (f32, f32) {
    const MIN_W: f32 = 440.0;
    let (max_w, max_h) = (screen.0 * 0.7, screen.1 * 0.7);
    let (mut w, mut h) = (content.0.max(1.0), content.1.max(1.0) + HEADER_H);
    let scale = (max_w / w).min(max_h / h).min(1.0);
    w *= scale;
    h *= scale;
    (w.max(MIN_W).min(max_w.max(MIN_W)).round(), h.max(HEADER_H + 120.0).round())
}

pub fn modified_string(t: SystemTime) -> String {
    jiff::Timestamp::try_from(t)
        .ok()
        .map(|ts| ts.to_zoned(jiff::tz::TimeZone::system()).strftime("%b %-d, %Y at %H:%M").to_string())
        .unwrap_or_default()
}

pub fn size_string(size: u64) -> String {
    files::human_size(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_dimensions() {
        assert_eq!(svg_size(r#"<svg width="120" height="80px" xmlns="x">"#), Some((120.0, 80.0)));
        assert_eq!(svg_size(r#"<?xml version="1.0"?><svg viewBox="0 0 24 12">"#), Some((24.0, 12.0)));
        assert_eq!(svg_size(r#"<svg width="100%" height="100%" viewBox='0,0,10,5'>"#), Some((10.0, 5.0)));
        assert_eq!(svg_size("<html></html>"), None);
    }

    #[test]
    fn pdf_pages() {
        assert_eq!(parse_pdf_pages("Title: x\nPages:          12\nEncrypted: no"), Some(12));
        assert_eq!(parse_pdf_pages("Pages: 0"), None);
        assert_eq!(parse_pdf_pages("nothing"), None);
    }

    #[test]
    fn desktop_names() {
        let t = "[Desktop Action x]\nName=No\n[Desktop Entry]\nName=Firefox\nName[de]=Feuerfuchs\n";
        assert_eq!(desktop_name(t), Some("Firefox".into()));
        assert_eq!(desktop_name("[Other]\nName=x"), None);
    }

    #[test]
    fn window_fits_screen() {
        // Small content keeps its size (plus header), min width applies.
        assert_eq!(fit_window((600.0, 400.0), (1920.0, 1080.0)), (600.0, 448.0));
        assert_eq!(fit_window((100.0, 100.0), (1920.0, 1080.0)).0, 440.0);
        // Large content shrinks to 70% of the screen, keeping aspect.
        let (w, h) = fit_window((4000.0, 2000.0), (1920.0, 1080.0));
        assert!(w <= 1344.0 && h <= 756.0, "{w}x{h}");
        let ratio = w / (h - HEADER_H);
        assert!((ratio - 2.0).abs() < 0.1, "{ratio}");
    }

    #[test]
    fn text_reading_limits() {
        let dir = std::env::temp_dir().join(format!("mql-content-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("big.txt");
        std::fs::write(&f, "x\n".repeat(MAX_LINES + 50)).unwrap();
        let (lines, truncated) = read_lines(&f);
        assert_eq!(lines.len(), MAX_LINES);
        assert!(truncated);
        let g = dir.join("small.txt");
        std::fs::write(&g, "a\tb\r\nc").unwrap();
        let (lines, truncated) = read_lines(&g);
        assert_eq!(lines, vec!["a    b".to_string(), "c".to_string()]);
        assert!(!truncated);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn highlighting_known_and_unknown() {
        let lines = vec!["fn main() {}".to_string()];
        let p = highlight(&lines, "rs", true).expect("rust syntax");
        assert!(p.len() > 1);
        assert_eq!(p.iter().map(|p| p.text.as_str()).collect::<String>(), "fn main() {}\n");
        assert!(highlight(&lines, "zzzz", true).is_none());
    }

    #[test]
    fn huge_single_line_is_capped() {
        let dir = std::env::temp_dir().join(format!("mac-quicklook-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("long.txt");
        std::fs::write(&path, vec![b'x'; MAX_TEXT_BYTES * 2]).unwrap();
        let (lines, truncated) = read_lines(&path);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(truncated);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].len(), MAX_TEXT_BYTES);
    }
}
