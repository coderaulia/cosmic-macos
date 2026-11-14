// SPDX-License-Identifier: GPL-3.0-only

//! Clipboard item data model, MIME offer selection, canonical hashing,
//! URL detection, and `file://` URI list parsing.

use std::fmt;

use image::{DynamicImage, GenericImageView, ImageFormat, imageops::FilterType};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

pub const PLAIN_TEXT_MIMES: &[&str] = &[
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "STRING",
    "TEXT",
];

pub const IMAGE_MIMES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/webp",
    "image/bmp",
    "image/tiff",
];

pub const GNOME_COPIED_FILES_MIME: &str = "x-special/gnome-copied-files";
pub const URI_LIST_MIME: &str = "text/uri-list";
pub const HTML_MIME: &str = "text/html";
pub const PREVIEW_MAX_CHARS: usize = 512;
pub const THUMBNAIL_MAX_DIM: u32 = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Text,
    Link,
    RichText,
    Image,
    Files,
}

impl ItemKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Link => "link",
            Self::RichText => "rich_text",
            Self::Image => "image",
            Self::Files => "files",
        }
    }

    #[must_use]
    pub fn from_str_kind(s: &str) -> Option<Self> {
        match s {
            "text" | "Text" => Some(Self::Text),
            "link" | "Link" => Some(Self::Link),
            "rich_text" | "RichText" => Some(Self::RichText),
            "image" | "Image" => Some(Self::Image),
            "files" | "Files" => Some(Self::Files),
            _ => None,
        }
    }
}

impl fmt::Display for ItemKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobRefs {
    /// SHA-256 hex of the primary payload blob on disk (`blobs/xx/<hash>`).
    pub primary: String,
    /// Optional SHA-256 hex of the plain-text blob (used by `RichText`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plain_text: Option<String>,
    /// True when this entry is an oversized placeholder with no stored bytes.
    #[serde(default)]
    pub oversized: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesMeta {
    pub paths: Vec<String>,
    pub cut: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardItem {
    pub id: String,
    pub kind: ItemKind,
    /// Canonical content SHA-256 hex used for deduplication.
    pub hash: String,
    /// Canonical SHA-256 hex of the plain text (for `Text`, `Link`, and `RichText` upgrade matching).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plain_hash: Option<String>,
    pub mime_types: Vec<String>,
    pub blob_refs: BlobRefs,
    pub size_bytes: u64,
    pub preview: String,
    pub created: i64,
    pub last_used: i64,
    pub use_count: u64,
    pub pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_dims: Option<(u32, u32)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_meta: Option<FilesMeta>,
    #[serde(default)]
    pub masked_secret: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}

impl ClipboardItem {
    /// All blob hashes referenced by this item.
    #[must_use]
    pub fn referenced_blobs(&self) -> Vec<&str> {
        let mut refs = Vec::new();
        if !self.blob_refs.oversized && !self.blob_refs.primary.is_empty() {
            refs.push(self.blob_refs.primary.as_str());
        }
        if let Some(plain) = self.blob_refs.plain_text.as_deref()
            && !plain.is_empty()
        {
            refs.push(plain);
        }
        refs
    }

    #[must_use]
    pub fn thumbnail_hash(&self) -> Option<&str> {
        if self.kind == ItemKind::Image && !self.blob_refs.oversized {
            Some(self.blob_refs.primary.as_str())
        } else {
            None
        }
    }
}

/// Ordered MIME selection plan determined before reading offer bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MimePlan {
    GnomeFiles {
        mime: String,
    },
    UriList {
        uri_mime: String,
        fallback: Option<Box<MimePlan>>,
    },
    Image {
        mime: String,
    },
    RichText {
        html_mime: String,
        plain_mime: Option<String>,
    },
    PlainText {
        mime: String,
    },
}

/// Selects which MIME type(s) to fetch from an offer according to priority order:
/// Files -> Image -> RichText -> Text/Link.
#[must_use]
pub fn select_mime_plan(offered: &[String]) -> Option<MimePlan> {
    if let Some(mime) = find_mime(offered, GNOME_COPIED_FILES_MIME) {
        return Some(MimePlan::GnomeFiles { mime });
    }

    let non_file_plan = select_non_uri_mime_plan(offered);

    if let Some(uri_mime) = find_mime(offered, URI_LIST_MIME) {
        return Some(MimePlan::UriList {
            uri_mime,
            fallback: non_file_plan.map(Box::new),
        });
    }

    non_file_plan
}

fn select_non_uri_mime_plan(offered: &[String]) -> Option<MimePlan> {
    for candidate in IMAGE_MIMES {
        if let Some(mime) = find_mime(offered, candidate) {
            return Some(MimePlan::Image { mime });
        }
    }

    if let Some(html_mime) = find_mime(offered, HTML_MIME) {
        let plain_mime = find_best_plain_mime(offered);
        return Some(MimePlan::RichText {
            html_mime,
            plain_mime,
        });
    }

    find_best_plain_mime(offered).map(|mime| MimePlan::PlainText { mime })
}

#[must_use]
pub fn find_best_plain_mime(offered: &[String]) -> Option<String> {
    for candidate in PLAIN_TEXT_MIMES {
        if let Some(mime) = find_mime(offered, candidate) {
            return Some(mime);
        }
    }
    None
}

fn find_mime(offered: &[String], target: &str) -> Option<String> {
    // Exact case-insensitive match first.
    if let Some(found) = offered.iter().find(|m| m.eq_ignore_ascii_case(target)) {
        return Some(found.clone());
    }
    // Match MIME essence when target has no parameters (e.g. `text/html;charset=utf-8`).
    if !target.contains(';') {
        return offered
            .iter()
            .find(|m| {
                let essence = m.split(';').next().unwrap_or(m).trim();
                essence.eq_ignore_ascii_case(target)
            })
            .cloned();
    }
    None
}

/// Normalizes CRLF and lone CR line endings to LF.
#[must_use]
pub fn normalize_line_endings(input: &str) -> String {
    if !input.contains('\r') {
        return input.to_string();
    }
    input.replace("\r\n", "\n").replace('\r', "\n")
}

/// Computes SHA-256 hex digest of raw bytes.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

#[must_use]
pub fn canonical_text_hash(normalized_text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"text:");
    hasher.update(normalized_text.as_bytes());
    bytes_to_hex(&hasher.finalize())
}

#[must_use]
pub fn canonical_rich_text_hash(normalized_html: &str, normalized_plain: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"html:");
    hasher.update(normalized_html.as_bytes());
    hasher.update(b"\0");
    hasher.update(normalized_plain.as_bytes());
    bytes_to_hex(&hasher.finalize())
}

#[must_use]
pub fn canonical_files_hash(paths: &[String]) -> String {
    let mut sorted: Vec<&str> = paths.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    sorted.dedup();
    let mut hasher = Sha256::new();
    hasher.update(b"files:");
    for p in sorted {
        hasher.update(p.as_bytes());
        hasher.update(b"\0");
    }
    bytes_to_hex(&hasher.finalize())
}

#[must_use]
pub fn canonical_image_hash(img: &DynamicImage) -> String {
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let mut hasher = Sha256::new();
    hasher.update(b"image:");
    hasher.update(w.to_le_bytes());
    hasher.update(h.to_le_bytes());
    hasher.update(rgba.as_raw());
    bytes_to_hex(&hasher.finalize())
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

/// Detects whether `text` is a single `http(s)://`, `mailto:`, or `ftp://` URL.
#[must_use]
pub fn detect_single_url(text: &str) -> Option<Url> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_whitespace) {
        return None;
    }
    let parsed = Url::parse(trimmed).ok()?;
    match parsed.scheme() {
        "http" | "https" | "ftp" => {
            if parsed.host_str().is_some_and(|h| !h.is_empty()) {
                Some(parsed)
            } else {
                None
            }
        }
        "mailto" => {
            let path = parsed.path();
            if !path.is_empty() && path.contains('@') {
                Some(parsed)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Parses `x-special/gnome-copied-files` payload:
/// first line `copy` or `cut`, followed by `file://` URIs.
#[must_use]
pub fn parse_gnome_copied_files(bytes: &[u8]) -> Option<FilesMeta> {
    let text = std::str::from_utf8(bytes).ok()?;
    let normalized = normalize_line_endings(text);
    let mut lines = normalized
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty());
    let action = lines.next()?;
    let cut = match action {
        "copy" => false,
        "cut" => true,
        _ => return None,
    };
    let mut paths = Vec::new();
    for line in lines {
        let path = decode_file_uri(line)?;
        paths.push(path);
    }
    if paths.is_empty() {
        return None;
    }
    Some(FilesMeta { paths, cut })
}

/// Parses `text/uri-list` payload, succeeding only if every non-comment line is a `file://` URI.
#[must_use]
pub fn parse_file_uri_list(bytes: &[u8]) -> Option<Vec<String>> {
    let text = std::str::from_utf8(bytes).ok()?;
    let normalized = normalize_line_endings(text);
    let mut paths = Vec::new();
    for line in normalized.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let path = decode_file_uri(trimmed)?;
        paths.push(path);
    }
    if paths.is_empty() {
        return None;
    }
    Some(paths)
}

/// Decodes a `file://` URI (handling spaces and UTF-8 percent encoding) into a local path string.
#[must_use]
pub fn decode_file_uri(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let parsed = Url::parse(trimmed).ok()?;
    if parsed.scheme() != "file" {
        return None;
    }
    let path = parsed.to_file_path().ok()?;
    Some(path.to_string_lossy().into_owned())
}

/// Encodes a local path into a `file://` URI.
#[must_use]
pub fn encode_file_uri(path: &str) -> String {
    Url::from_file_path(path)
        .map(|u| u.to_string())
        .unwrap_or_else(|()| format!("file://{path}"))
}

/// Formats `FilesMeta` as a `text/uri-list` payload.
#[must_use]
pub fn format_uri_list(files: &FilesMeta) -> String {
    files
        .paths
        .iter()
        .map(|p| encode_file_uri(p))
        .collect::<Vec<_>>()
        .join("\r\n")
}

/// Formats `FilesMeta` as an `x-special/gnome-copied-files` payload.
#[must_use]
pub fn format_gnome_copied_files(files: &FilesMeta) -> String {
    let action = if files.cut { "cut" } else { "copy" };
    let mut out = String::from(action);
    for p in &files.paths {
        out.push('\n');
        out.push_str(&encode_file_uri(p));
    }
    out
}

/// Truncates a string to at most `PREVIEW_MAX_CHARS` characters on a single-line-friendly basis.
#[must_use]
pub fn make_text_preview(text: &str) -> String {
    let trimmed = text.trim();
    let mut out = String::new();
    let mut chars = 0usize;
    let mut prev_ws = false;
    for ch in trimmed.chars() {
        if chars >= PREVIEW_MAX_CHARS {
            out.push('…');
            break;
        }
        if ch.is_whitespace() {
            if !prev_ws {
                out.push(' ');
                chars += 1;
                prev_ws = true;
            }
        } else {
            out.push(ch);
            chars += 1;
            prev_ws = false;
        }
    }
    out
}

#[must_use]
pub fn make_files_preview(files: &FilesMeta) -> String {
    let names: Vec<&str> = files
        .paths
        .iter()
        .map(|p| {
            std::path::Path::new(p)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(p.as_str())
        })
        .collect();
    let summary = match names.as_slice() {
        [] => "Files".to_string(),
        [only] => (*only).to_string(),
        [first, rest @ ..] => format!("{first}, {} more", rest.len()),
    };
    let clipped: String = summary.chars().take(PREVIEW_MAX_CHARS).collect();
    if files.cut {
        format!("{clipped} (cut)")
    } else {
        clipped
    }
}

/// Decodes raw image bytes (validating pixel cap), converts to PNG bytes,
/// computes the canonical pixel hash, and builds a 256 px PNG thumbnail.
pub struct ProcessedImage {
    pub canonical_hash: String,
    pub png_bytes: Vec<u8>,
    pub thumb_png_bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum ImageProcessError {
    #[error("failed to decode image: {0}")]
    Decode(String),
    #[error("image dimensions {0}x{1} exceed {2} pixel limit")]
    TooManyPixels(u32, u32, u64),
    #[error("failed to encode PNG: {0}")]
    Encode(String),
}

pub fn process_image_bytes(
    raw: &[u8],
    mime: &str,
    max_pixels: u64,
) -> Result<ProcessedImage, ImageProcessError> {
    let essence = mime.split(';').next().unwrap_or(mime).trim();
    let format = match essence.to_ascii_lowercase().as_str() {
        "image/png" => Some(ImageFormat::Png),
        "image/jpeg" | "image/jpg" => Some(ImageFormat::Jpeg),
        "image/webp" => Some(ImageFormat::WebP),
        "image/bmp" => Some(ImageFormat::Bmp),
        "image/tiff" => Some(ImageFormat::Tiff),
        _ => None,
    };

    let mut reader = match format {
        Some(fmt) => image::ImageReader::with_format(std::io::Cursor::new(raw), fmt),
        None => image::ImageReader::new(std::io::Cursor::new(raw))
            .with_guessed_format()
            .map_err(|e| ImageProcessError::Decode(e.to_string()))?,
    };
    reader.no_limits();

    let (w, h) = reader
        .into_dimensions()
        .map_err(|e| ImageProcessError::Decode(e.to_string()))?;
    let total_pixels = u64::from(w) * u64::from(h);
    if total_pixels > max_pixels {
        return Err(ImageProcessError::TooManyPixels(w, h, max_pixels));
    }

    let reader2 = match format {
        Some(fmt) => image::ImageReader::with_format(std::io::Cursor::new(raw), fmt),
        None => image::ImageReader::new(std::io::Cursor::new(raw))
            .with_guessed_format()
            .map_err(|e| ImageProcessError::Decode(e.to_string()))?,
    };
    let img = reader2
        .decode()
        .map_err(|e| ImageProcessError::Decode(e.to_string()))?;

    let (width, height) = img.dimensions();
    let canonical_hash = canonical_image_hash(&img);

    let png_bytes = if format == Some(ImageFormat::Png) {
        raw.to_vec()
    } else {
        encode_png(&img)?
    };

    let thumb_img = if width > THUMBNAIL_MAX_DIM || height > THUMBNAIL_MAX_DIM {
        img.resize(THUMBNAIL_MAX_DIM, THUMBNAIL_MAX_DIM, FilterType::Triangle)
    } else {
        img
    };
    let thumb_png_bytes = encode_png(&thumb_img)?;

    Ok(ProcessedImage {
        canonical_hash,
        png_bytes,
        thumb_png_bytes,
        width,
        height,
    })
}

fn encode_png(img: &DynamicImage) -> Result<Vec<u8>, ImageProcessError> {
    let mut buf = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut buf), ImageFormat::Png)
        .map_err(|e| ImageProcessError::Encode(e.to_string()))?;
    Ok(buf)
}

/// Minimal HTML-to-plain-text fallback when an offer provides `text/html` without plain text.
#[must_use]
pub fn html_to_plain_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut tag_buf = String::new();
    let mut skip_depth = 0usize;

    for ch in html.chars() {
        if in_tag {
            if ch == '>' {
                in_tag = false;
                let tag = tag_buf.trim().to_ascii_lowercase();
                let name = tag.split_whitespace().next().unwrap_or("");
                if name == "script" || name == "style" {
                    skip_depth += 1;
                } else if name == "/script" || name == "/style" {
                    skip_depth = skip_depth.saturating_sub(1);
                } else if skip_depth == 0
                    && matches!(
                        name,
                        "br" | "br/" | "/p" | "/div" | "/li" | "/tr" | "/h1" | "/h2" | "/h3"
                            | "/h4" | "/h5" | "/h6"
                    )
                    && !out.ends_with('\n')
                {
                    out.push('\n');
                }
                tag_buf.clear();
            } else {
                tag_buf.push(ch);
            }
        } else if ch == '<' {
            in_tag = true;
            tag_buf.clear();
        } else if skip_depth == 0 {
            out.push(ch);
        }
    }

    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    normalize_line_endings(&decoded).trim().to_string()
}
