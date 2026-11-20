// SPDX-License-Identifier: GPL-3.0-only

//! File type detection: extension first, then content sniffing.

use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Image,
    Svg,
    Pdf,
    Text,
    Video,
    Audio,
    Other,
}

/// Maps a MIME type to a kind; `None` when the type is not decisive.
pub fn kind_of_mime(mime: &str) -> Option<Kind> {
    match mime {
        "image/svg+xml" => Some(Kind::Svg),
        "application/pdf" => Some(Kind::Pdf),
        "application/json" | "application/toml" | "application/xml" | "application/x-sh"
        | "application/javascript" | "application/x-yaml" | "application/yaml"
        | "application/x-shellscript" | "application/sql" | "application/x-httpd-php" => {
            Some(Kind::Text)
        }
        m if m.starts_with("image/") => Some(Kind::Image),
        m if m.starts_with("video/") => Some(Kind::Video),
        m if m.starts_with("audio/") => Some(Kind::Audio),
        m if m.starts_with("text/") => Some(Kind::Text),
        _ => None,
    }
}

/// Magic-number sniffing for the leading bytes of a file.
pub fn sniff_bytes(head: &[u8]) -> Option<(Kind, &'static str)> {
    const MAGICS: &[(&[u8], Kind, &str)] = &[
        (b"\x89PNG\r\n\x1a\n", Kind::Image, "image/png"),
        (b"\xFF\xD8\xFF", Kind::Image, "image/jpeg"),
        (b"GIF87a", Kind::Image, "image/gif"),
        (b"GIF89a", Kind::Image, "image/gif"),
        (b"%PDF-", Kind::Pdf, "application/pdf"),
        (b"OggS", Kind::Audio, "audio/ogg"),
        (b"fLaC", Kind::Audio, "audio/flac"),
        (b"ID3", Kind::Audio, "audio/mpeg"),
        (b"\x1A\x45\xDF\xA3", Kind::Video, "video/x-matroska"),
    ];
    for (magic, kind, mime) in MAGICS {
        if head.starts_with(magic) {
            return Some((*kind, mime));
        }
    }
    if head.len() >= 12 && &head[..4] == b"RIFF" {
        match &head[8..12] {
            b"WEBP" => return Some((Kind::Image, "image/webp")),
            b"WAVE" => return Some((Kind::Audio, "audio/wav")),
            b"AVI " => return Some((Kind::Video, "video/x-msvideo")),
            _ => {}
        }
    }
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        return Some((Kind::Video, "video/mp4"));
    }
    None
}

/// Heuristic: no NUL bytes and (almost) valid UTF-8.
pub fn looks_like_text(head: &[u8]) -> bool {
    if head.is_empty() {
        return true;
    }
    if head.contains(&0) {
        return false;
    }
    let text = String::from_utf8_lossy(head);
    let bad = text.chars().filter(|c| *c == '\u{FFFD}').count();
    // A multi-byte char cut at the end of the buffer yields one replacement.
    bad <= 1 && bad * 100 <= head.len()
}

/// Detects `(kind, mime)` of a file.
pub fn detect(path: &Path) -> (Kind, String) {
    let guess = mime_guess::from_path(path).first().map(|m| m.essence_str().to_string());
    let mut head = vec![0u8; 8192];
    let n = std::fs::File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .unwrap_or(0);
    head.truncate(n);
    if let Some(mime) = &guess
        && let Some(kind) = kind_of_mime(mime)
    {
        // `.ts` is TypeScript as often as MPEG-TS: trust the content.
        let media = matches!(kind, Kind::Video | Kind::Audio);
        if !(media && !head.is_empty() && looks_like_text(&head)) {
            return (kind, mime.clone());
        }
        return (Kind::Text, "text/plain".to_string());
    }
    if let Some((kind, mime)) = sniff_bytes(&head) {
        return (kind, mime.to_string());
    }
    if looks_like_text(&head) {
        return (Kind::Text, guess.unwrap_or_else(|| "text/plain".to_string()));
    }
    (Kind::Other, guess.unwrap_or_else(|| "application/octet-stream".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_mapping() {
        assert_eq!(kind_of_mime("image/png"), Some(Kind::Image));
        assert_eq!(kind_of_mime("image/svg+xml"), Some(Kind::Svg));
        assert_eq!(kind_of_mime("application/pdf"), Some(Kind::Pdf));
        assert_eq!(kind_of_mime("video/mp4"), Some(Kind::Video));
        assert_eq!(kind_of_mime("audio/mpeg"), Some(Kind::Audio));
        assert_eq!(kind_of_mime("text/x-rust"), Some(Kind::Text));
        assert_eq!(kind_of_mime("application/json"), Some(Kind::Text));
        assert_eq!(kind_of_mime("application/zip"), None);
    }

    #[test]
    fn sniffing() {
        assert_eq!(sniff_bytes(b"%PDF-1.7 ...").map(|s| s.0), Some(Kind::Pdf));
        assert_eq!(sniff_bytes(b"\x89PNG\r\n\x1a\nxxxx").map(|s| s.0), Some(Kind::Image));
        assert_eq!(sniff_bytes(b"\0\0\0\x18ftypmp42").map(|s| s.0), Some(Kind::Video));
        assert_eq!(sniff_bytes(b"RIFF\0\0\0\0WEBPVP8 ").map(|s| s.0), Some(Kind::Image));
        assert_eq!(sniff_bytes(b"hello"), None);
    }

    #[test]
    fn text_heuristic() {
        assert!(looks_like_text(b"fn main() {}\n"));
        assert!(looks_like_text("héllo wörld".as_bytes()));
        assert!(looks_like_text(b""));
        assert!(!looks_like_text(b"abc\0def"));
        assert!(!looks_like_text(&[0xff, 0xfe, 0xfd, 0xfc, 0xfb, 0xfa, 0xf9, 0xf8]));
    }

    #[test]
    fn detect_by_extension_then_content() {
        let dir = std::env::temp_dir().join(format!("mql-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let noext = dir.join("README_noext");
        std::fs::write(&noext, "just text\n").unwrap();
        assert_eq!(detect(&noext).0, Kind::Text);
        let pdf = dir.join("blob");
        std::fs::write(&pdf, b"%PDF-1.4\n").unwrap();
        assert_eq!(detect(&pdf).0, Kind::Pdf);
        let bin = dir.join("data.bin");
        std::fs::write(&bin, [0u8, 1, 2, 3]).unwrap();
        assert_eq!(detect(&bin).0, Kind::Other);
        let ts = dir.join("a.ts");
        std::fs::write(&ts, "export const x = 1;\n").unwrap();
        assert_eq!(detect(&ts).0, Kind::Text);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
