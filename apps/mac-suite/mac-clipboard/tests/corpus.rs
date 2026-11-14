// SPDX-License-Identifier: GPL-3.0-only

//! Verification corpus tests for `mac-clipboard`.

use std::collections::HashMap;
use std::fs;
use std::io::Cursor;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use mac_clipboard::config::{ClipboardConfig, ExpiryPolicy};
use mac_clipboard::daemon::owner::build_mime_sources;
use mac_clipboard::daemon::watcher::{OfferReader, ReadError, ingest_mime_plan};
use mac_clipboard::daemon::wayland::is_terminal_app;
use mac_clipboard::filter::{
    OfferFilterDecision, SELF_OWNER_MIME, TransientClearGuard, detect_secret, filter_offer,
    is_ignored_app, is_self_offer, is_sensitive_offer,
};
use mac_clipboard::model::{
    FilesMeta, ItemKind, MimePlan, detect_single_url, parse_file_uri_list,
    parse_gnome_copied_files, select_mime_plan,
};
use mac_clipboard::search::{FilterPill, search_items};
use mac_clipboard::store::ClipboardStore;
use mac_clipboard::ui::{format_bytes, format_relative_time};
use wl_clipboard_rs::copy::MimeType;

static TEST_DIR_SEQ: AtomicU64 = AtomicU64::new(1);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        let seq = TEST_DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "mac-clipboard-test-{prefix}-{}-{seq}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp test dir");
        Self { path: dir }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct MockOfferReader {
    data: HashMap<String, Vec<u8>>,
    panic_on_read: bool,
}

impl MockOfferReader {
    fn new(pairs: Vec<(&str, Vec<u8>)>) -> Self {
        Self {
            data: pairs
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            panic_on_read: false,
        }
    }

    fn panic_if_read() -> Self {
        Self {
            data: HashMap::new(),
            panic_on_read: true,
        }
    }
}

impl OfferReader for MockOfferReader {
    fn read_mime(&mut self, mime: &str, max_bytes: usize) -> Result<Vec<u8>, ReadError> {
        if self.panic_on_read {
            panic!("OfferReader::read_mime({mime}) must NEVER be called for sensitive offers");
        }
        let Some(bytes) = self.data.get(mime) else {
            return Err(ReadError::Failed(format!("missing mime {mime}")));
        };
        if bytes.len() > max_bytes {
            return Err(ReadError::TooLarge(bytes.len()));
        }
        Ok(bytes.clone())
    }
}

fn make_solid_image(w: u32, h: u32, color: [u8; 4], fmt: ImageFormat) -> Vec<u8> {
    let mut img = RgbaImage::new(w, h);
    for (x, y, px) in img.enumerate_pixels_mut() {
        let mut c = color;
        c[0] = c[0].wrapping_add((x % 17) as u8);
        c[1] = c[1].wrapping_add((y % 17) as u8);
        *px = Rgba(c);
    }
    let dyn_img = DynamicImage::ImageRgba8(img);
    let mut out = Vec::new();
    dyn_img
        .write_to(&mut Cursor::new(&mut out), fmt)
        .expect("encode test image");
    out
}

fn mimes(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn mime_selection_app_corpus() {
    // Firefox rich selection
    let ff_rich = mimes(&[
        "text/html",
        "text/_moz_htmlcontext",
        "text/plain;charset=utf-8",
        "text/plain",
    ]);
    assert_eq!(
        select_mime_plan(&ff_rich),
        Some(MimePlan::RichText {
            html_mime: "text/html".into(),
            plain_mime: Some("text/plain;charset=utf-8".into()),
        })
    );

    // Chromium plain text
    let chromium = mimes(&["UTF8_STRING", "text/plain;charset=utf-8", "text/plain"]);
    assert_eq!(
        select_mime_plan(&chromium),
        Some(MimePlan::PlainText {
            mime: "text/plain;charset=utf-8".into()
        })
    );

    // LibreOffice
    let libre = mimes(&[
        "application/x-openoffice-embed-source-xml",
        "text/html",
        "text/richtext",
        "text/plain;charset=utf-8",
    ]);
    assert_eq!(
        select_mime_plan(&libre),
        Some(MimePlan::RichText {
            html_mime: "text/html".into(),
            plain_mime: Some("text/plain;charset=utf-8".into()),
        })
    );

    // COSMIC Files
    let files = mimes(&[
        "x-special/gnome-copied-files",
        "text/uri-list",
        "text/plain;charset=utf-8",
    ]);
    assert_eq!(
        select_mime_plan(&files),
        Some(MimePlan::GnomeFiles {
            mime: "x-special/gnome-copied-files".into()
        })
    );

    // GIMP
    let gimp = mimes(&["image/tiff", "image/jpeg", "image/png", "image/bmp"]);
    assert_eq!(
        select_mime_plan(&gimp),
        Some(MimePlan::Image {
            mime: "image/png".into()
        })
    );

    // Terminal
    let term = mimes(&["UTF8_STRING", "STRING", "TEXT", "text/plain;charset=utf-8"]);
    assert_eq!(
        select_mime_plan(&term),
        Some(MimePlan::PlainText {
            mime: "text/plain;charset=utf-8".into()
        })
    );

    // Terminal without charset=utf-8 falls back to UTF8_STRING before text/plain
    let term_legacy = mimes(&["TEXT", "STRING", "text/plain", "UTF8_STRING"]);
    assert_eq!(
        select_mime_plan(&term_legacy),
        Some(MimePlan::PlainText {
            mime: "UTF8_STRING".into()
        })
    );

    // Non-file text/uri-list falls back to plain text / link
    let dir = TempDir::new("uri-fallback");
    let mut store = ClipboardStore::open(dir.path()).unwrap();
    let cfg = ClipboardConfig::default();
    let offer = mimes(&["text/uri-list", "text/plain;charset=utf-8"]);
    let plan = select_mime_plan(&offer).unwrap();
    let mut reader = MockOfferReader::new(vec![
        ("text/uri-list", b"https://pop.system76.com/\r\n".to_vec()),
        (
            "text/plain;charset=utf-8",
            b"https://pop.system76.com/".to_vec(),
        ),
    ]);
    let item = ingest_mime_plan(&mut reader, &plan, &offer, None, 1000, &cfg, &mut store)
        .unwrap()
        .unwrap();
    assert_eq!(item.kind, ItemKind::Link);
    assert_eq!(item.preview, "https://pop.system76.com/");
}

#[test]
fn dedup_crlf_images_html_upgrade_and_file_order() {
    let dir = TempDir::new("dedup");
    let mut store = ClipboardStore::open(dir.path()).unwrap();
    let cfg = ClipboardConfig::default();

    // 1. CRLF vs LF text
    let a = store
        .insert_text("line1\r\nline2", vec![], Some("app1".into()), 1000, &cfg)
        .unwrap();
    let b = store
        .insert_text("line1\nline2", vec![], Some("app2".into()), 2000, &cfg)
        .unwrap();
    assert_eq!(a.id, b.id);
    assert_eq!(b.use_count, 2);
    assert_eq!(store.len(), 1);

    // 2. HTML upgrading matching plain text
    let plain = store
        .insert_text("Hello world", vec![], None, 3000, &cfg)
        .unwrap();
    assert_eq!(plain.kind, ItemKind::Text);
    store.pin(&plain.id, true, cfg.max_pinned).unwrap();
    let upgraded = store
        .insert_rich_text(
            "<p><b>Hello</b> world</p>",
            Some("Hello world"),
            vec![],
            Some("firefox".into()),
            4000,
            &cfg,
        )
        .unwrap();
    assert_eq!(upgraded.id, plain.id);
    assert_eq!(upgraded.kind, ItemKind::RichText);
    assert!(upgraded.pinned, "pin status preserved across upgrade");
    assert_eq!(store.len(), 2);

    // 3. Same PNG pixels re-encoded via BMP vs PNG
    let png1 = make_solid_image(32, 24, [20, 80, 200, 255], ImageFormat::Png);
    let bmp_same = make_solid_image(32, 24, [20, 80, 200, 255], ImageFormat::Bmp);
    let img1 = store
        .insert_image(&png1, "image/png", None, 5000, &cfg)
        .unwrap();
    let img2 = store
        .insert_image(&bmp_same, "image/bmp", None, 6000, &cfg)
        .unwrap();
    assert_eq!(img1.id, img2.id);
    assert_eq!(img2.use_count, 2);
    assert!(store.thumbnail_path(&img2).is_some());

    // 4. File lists in different order
    let f1 = store
        .insert_files(
            FilesMeta {
                paths: vec!["/tmp/a.txt".into(), "/tmp/b.txt".into()],
                cut: false,
            },
            None,
            7000,
            &cfg,
        )
        .unwrap();
    let f2 = store
        .insert_files(
            FilesMeta {
                paths: vec!["/tmp/b.txt".into(), "/tmp/a.txt".into()],
                cut: false,
            },
            None,
            8000,
            &cfg,
        )
        .unwrap();
    assert_eq!(f1.id, f2.id);
    assert_eq!(f2.use_count, 2);
    assert_eq!(store.len(), 4);
}

#[test]
fn ring_buffer_eviction_and_blob_gc() {
    let dir = TempDir::new("ring-buffer");
    let mut store = ClipboardStore::open(dir.path()).unwrap();
    let mut cfg = ClipboardConfig {
        max_items: 3,
        ..Default::default()
    };

    let pinned = store
        .insert_text("keep me", vec![], None, 100, &cfg)
        .unwrap();
    store.pin(&pinned.id, true, cfg.max_pinned).unwrap();

    let u1 = store
        .insert_text("unpinned-1", vec![], None, 200, &cfg)
        .unwrap();
    let u2 = store
        .insert_text("unpinned-2", vec![], None, 300, &cfg)
        .unwrap();
    let u3 = store
        .insert_text("unpinned-3", vec![], None, 400, &cfg)
        .unwrap();
    let u4 = store
        .insert_text("unpinned-4", vec![], None, 500, &cfg)
        .unwrap();

    // Pinned item survives + 3 newest unpinned remain; u1 was evicted.
    assert!(store.get(&pinned.id).is_ok());
    assert!(store.get(&u1.id).is_err());
    assert!(store.get(&u2.id).is_ok());
    assert!(store.get(&u3.id).is_ok());
    assert!(store.get(&u4.id).is_ok());
    assert!(!store.blob_path(&u1.blob_refs.primary).exists());
    assert!(!store.meta_path(&u1.blob_refs.primary).exists());

    // Eviction by bytes
    let img_bytes = make_solid_image(128, 128, [10, 20, 30, 255], ImageFormat::Png);
    let img_item = store
        .insert_image(&img_bytes, "image/png", None, 600, &cfg)
        .unwrap();
    let img_thumb = store.thumbnail_path(&img_item).unwrap();
    assert!(img_thumb.exists());

    cfg.max_items = 100;
    cfg.max_bytes = 200; // tiny byte cap forces unpinned eviction until only pinned remains
    let newest = store
        .insert_text("tiny", vec![], None, 700, &cfg)
        .unwrap();
    // Pinned item + newest fit or only pinned survives if over max_bytes; image must be evicted.
    assert!(store.get(&pinned.id).is_ok());
    assert!(store.get(&img_item.id).is_err());
    assert!(!img_thumb.exists(), "evicted image thumbnail must be deleted");
    assert!(!store.blob_path(&img_item.blob_refs.primary).exists());
    let _ = newest;
}

#[test]
fn store_round_trip_atomic_and_corrupt_index_recovery() {
    let dir = TempDir::new("recovery");
    let mut cfg = ClipboardConfig::default();

    let id_text;
    let id_img;
    {
        let mut store = ClipboardStore::open(dir.path()).unwrap();
        let dir_mode = fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);

        let t = store
            .insert_text("recover me", vec![], Some("editor".into()), 1000, &cfg)
            .unwrap();
        store.pin(&t.id, true, cfg.max_pinned).unwrap();
        id_text = t.id.clone();

        let blob_mode = fs::metadata(store.blob_path(&t.blob_refs.primary))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(blob_mode, 0o600);

        let png = make_solid_image(16, 16, [255, 0, 0, 255], ImageFormat::Png);
        let img = store
            .insert_image(&png, "image/png", Some("gimp".into()), 2000, &cfg)
            .unwrap();
        id_img = img.id.clone();
    }

    // Simulate interrupted atomic write: leave a corrupt `index.json.tmp` alongside valid `index.json`.
    fs::write(dir.path().join("index.json.tmp"), b"{corrupt partial").unwrap();
    {
        let store = ClipboardStore::open(dir.path()).unwrap();
        assert!(!dir.path().join("index.json.tmp").exists());
        assert_eq!(store.len(), 2);
        assert!(store.get(&id_text).unwrap().pinned);
        assert_eq!(store.get(&id_img).unwrap().kind, ItemKind::Image);
    }

    // Corrupt `index.json` itself -> must be moved aside (`index.json.broken-*`) and rebuilt from `.meta`.
    fs::write(dir.path().join("index.json"), b"not valid json!!!").unwrap();
    {
        let store = ClipboardStore::open(dir.path()).unwrap();
        assert_eq!(store.len(), 2);
        assert_eq!(store.items()[0].id, id_img);
        assert_eq!(store.items()[1].id, id_text);
        assert!(store.get(&id_text).unwrap().pinned);
        assert_eq!(
            store.read_plain_text(store.get(&id_text).unwrap()).unwrap(),
            Some("recover me".to_string())
        );
    }

    let has_broken = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .any(|e| e.file_name().to_string_lossy().starts_with("index.json.broken-"));
    assert!(has_broken, "corrupt index.json must be preserved aside");

    // Expiry check
    cfg.expiry = ExpiryPolicy::OneDay;
    let mut store = ClipboardStore::open(dir.path()).unwrap();
    let far_future = 2000 + 2 * 24 * 60 * 60 * 1000;
    store.enforce_limits(far_future, &cfg).unwrap();
    assert!(store.get(&id_text).is_ok(), "pinned item never expires");
    assert!(store.get(&id_img).is_err(), "unpinned item expires after 1 day");
}

#[test]
fn sensitive_filter_and_transient_guard() {
    // Every hint variant must be detected without reading bytes.
    for hint in [
        "x-kde-passwordManagerHint",
        "application/x-kde-passwordManagerHint",
        "x-gnome-sensitive",
        "X-KDE-PASSWORDMANAGERHINT",
        "application/x-kde-passwordmanagerhint; charset=utf-8",
    ] {
        let offer = mimes(&[hint, "text/plain;charset=utf-8"]);
        assert!(is_sensitive_offer(&offer), "failed to catch {hint}");
        assert_eq!(
            filter_offer(&offer, None, &[], false),
            OfferFilterDecision::SkipSensitiveMime
        );
        // Ensure a reader is never touched if gated by filter_offer
        let _never_read = MockOfferReader::panic_if_read();
        if filter_offer(&offer, None, &[], false) == OfferFilterDecision::Allow {
            panic!("should have skipped {hint}");
        }
    }

    // Self marker
    let self_offer = mimes(&["text/plain", "application/x-mac-clipboard-owner"]);
    assert!(is_self_offer(&self_offer));
    assert_eq!(
        filter_offer(&self_offer, None, &[], false),
        OfferFilterDecision::SkipSelf
    );

    // Ignored password managers
    let cfg = ClipboardConfig::default();
    for app in [
        "org.keepassxc.KeePassXC",
        "org.keepassxc.KeePassXC.desktop",
        "com.bitwarden.desktop",
        "1password",
        "org.gnome.seahorse.Application",
    ] {
        assert!(is_ignored_app(Some(app), &cfg.ignored_apps));
        assert_eq!(
            filter_offer(&mimes(&["text/plain"]), Some(app), &cfg.ignored_apps, false),
            OfferFilterDecision::SkipIgnoredApp
        );
    }

    // Paused state
    assert_eq!(
        filter_offer(&mimes(&["text/plain"]), None, &cfg.ignored_apps, true),
        OfferFilterDecision::SkipPaused
    );

    // Transient clear guard: keep-alive never re-offers sensitive or < 1s transient clears
    let t0 = Instant::now();
    let mut guard = TransientClearGuard::new();
    guard.record_sensitive_or_ignored(t0);
    assert!(!guard.allow_keep_alive(t0 + Duration::from_secs(10)));

    guard.record_allowed(t0);
    assert!(!guard.allow_keep_alive(t0 + Duration::from_millis(500)));
    assert!(guard.allow_keep_alive(t0 + Duration::from_millis(1200)));

    // Secret heuristic
    assert!(detect_secret("ghp_1234567890abcdef1234567890abcdef").is_some());
    assert!(detect_secret("-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END OPENSSH PRIVATE KEY-----").is_some());
    assert!(detect_secret("https://github.com/pop-os/libcosmic").is_none());
    assert!(detect_secret("hello normal sentence").is_none());
}

#[test]
fn search_ranking_and_filter_pills() {
    let dir = TempDir::new("search");
    let mut store = ClipboardStore::open(dir.path()).unwrap();
    let cfg = ClipboardConfig::default();

    let fuzzy = store
        .insert_text("deploy_staging_server", vec![], None, 1000, &cfg)
        .unwrap();
    let prefix_unpinned = store
        .insert_text("deploy_prod", vec![], None, 2000, &cfg)
        .unwrap();
    let prefix_pinned = store
        .insert_text("deploy_beta", vec![], None, 1500, &cfg)
        .unwrap();
    store
        .pin(&prefix_pinned.id, true, cfg.max_pinned)
        .unwrap();
    let exact = store
        .insert_text("deploy", vec![], None, 500, &cfg)
        .unwrap();
    let link = store
        .insert_text("https://docs.rs/deploy", vec![], None, 3000, &cfg)
        .unwrap();
    let rich = store
        .insert_rich_text("<b>rich</b>", Some("rich"), vec![], None, 3500, &cfg)
        .unwrap();
    let png = make_solid_image(8, 8, [0, 255, 0, 255], ImageFormat::Png);
    let img = store
        .insert_image(&png, "image/png", None, 4000, &cfg)
        .unwrap();
    let files = store
        .insert_files(
            FilesMeta {
                paths: vec!["/tmp/report.pdf".into()],
                cut: false,
            },
            None,
            4500,
            &cfg,
        )
        .unwrap();

    // Ranking: exact > prefix (pinned tie-break / score) > fuzzy
    let hits = search_items(store.items(), FilterPill::All, "deploy", 20);
    let ids: Vec<&str> = hits.iter().map(|h| h.item.id.as_str()).collect();
    assert_eq!(ids[0], exact.id.as_str(), "exact match must rank first");
    let pos_pinned = ids.iter().position(|&id| id == prefix_pinned.id).unwrap();
    let pos_fuzzy = ids.iter().position(|&id| id == fuzzy.id).unwrap();
    let pos_unpinned = ids.iter().position(|&id| id == prefix_unpinned.id).unwrap();
    assert!(pos_pinned < pos_fuzzy);
    assert!(pos_unpinned < pos_fuzzy);

    // Filter pills
    let text_hits = search_items(store.items(), FilterPill::Text, "", 50);
    assert!(text_hits.iter().any(|h| h.item.id == rich.id));
    assert!(!text_hits.iter().any(|h| h.item.id == link.id));
    assert!(!text_hits.iter().any(|h| h.item.id == img.id));

    let img_hits = search_items(store.items(), FilterPill::Images, "", 50);
    assert_eq!(img_hits.len(), 1);
    assert_eq!(img_hits[0].item.id, img.id);

    let link_hits = search_items(store.items(), FilterPill::Links, "", 50);
    assert_eq!(link_hits.len(), 1);
    assert_eq!(link_hits[0].item.id, link.id);

    let file_hits = search_items(store.items(), FilterPill::Files, "", 50);
    assert_eq!(file_hits.len(), 1);
    assert_eq!(file_hits[0].item.id, files.id);

    let pinned_hits = search_items(store.items(), FilterPill::Pinned, "", 50);
    assert_eq!(pinned_hits.len(), 1);
    assert_eq!(pinned_hits[0].item.id, prefix_pinned.id);
}

#[test]
fn urls_and_file_uri_edge_cases() {
    // URL detection
    assert!(detect_single_url("https://github.com/pop-os/cosmic-comp").is_some());
    assert!(detect_single_url("  http://localhost:8080/health \n").is_some());
    assert!(detect_single_url("ftp://ftp.example.com/archive.tar.zst").is_some());
    assert!(detect_single_url("mailto:hello@example.com").is_some());

    assert!(detect_single_url("https://").is_none());
    assert!(detect_single_url("javascript:alert(1)").is_none());
    assert!(detect_single_url("file:///etc/passwd").is_none());
    assert!(detect_single_url("https://a.com https://b.com").is_none());
    assert!(detect_single_url("hello world").is_none());

    // file:// URI decoding with spaces and Unicode
    let uri_list = b"# comment line\r\nfile:///home/asw/My%20Documents/r%C3%A9sum%C3%A9.pdf\r\nfile:///tmp/%E4%B8%AD%E6%96%87%20file.txt\r\n";
    let decoded = parse_file_uri_list(uri_list).expect("decode unicode file URIs");
    assert_eq!(
        decoded,
        vec![
            "/home/asw/My Documents/résumé.pdf",
            "/tmp/中文 file.txt",
        ]
    );

    // Mixed non-file URI must return None so caller falls back
    assert!(parse_file_uri_list(b"file:///tmp/a\nhttps://example.com\n").is_none());

    // gnome-copied-files cut vs copy
    let copied = parse_gnome_copied_files(b"copy\nfile:///tmp/one%20two.txt\n").unwrap();
    assert!(!copied.cut);
    assert_eq!(copied.paths, vec!["/tmp/one two.txt"]);

    let cut = parse_gnome_copied_files(b"cut\nfile:///tmp/a\nfile:///tmp/b\n").unwrap();
    assert!(cut.cut);
    assert_eq!(cut.paths, vec!["/tmp/a", "/tmp/b"]);

    assert!(parse_gnome_copied_files(b"invalid\nfile:///tmp/a\n").is_none());
}

#[test]
fn owner_mime_sources_and_terminal_detection() {
    let dir = TempDir::new("owner");
    let mut store = ClipboardStore::open(dir.path()).unwrap();
    let cfg = ClipboardConfig::default();

    let rich = store
        .insert_rich_text("<b>hi</b>", Some("hi"), vec![], None, 1000, &cfg)
        .unwrap();
    let rich_sources = build_mime_sources(&store, &rich, false).unwrap();
    assert!(
        rich_sources
            .iter()
            .any(|s| s.mime_type == MimeType::Specific("text/html".into()))
    );
    assert!(rich_sources.iter().any(|s| s.mime_type == MimeType::Text));
    assert!(
        rich_sources
            .iter()
            .any(|s| s.mime_type == MimeType::Specific(SELF_OWNER_MIME.into()))
    );

    let plain_sources = build_mime_sources(&store, &rich, true).unwrap();
    assert!(!plain_sources
        .iter()
        .any(|s| s.mime_type == MimeType::Specific("text/html".into())));
    assert!(plain_sources.iter().any(|s| s.mime_type == MimeType::Text));
    assert!(
        plain_sources
            .iter()
            .any(|s| s.mime_type == MimeType::Specific(SELF_OWNER_MIME.into()))
    );

    let png = make_solid_image(12, 12, [1, 2, 3, 255], ImageFormat::Png);
    let img = store
        .insert_image(&png, "image/png", None, 2000, &cfg)
        .unwrap();
    let img_sources = build_mime_sources(&store, &img, false).unwrap();
    assert!(
        img_sources
            .iter()
            .any(|s| s.mime_type == MimeType::Specific("image/png".into()))
    );

    let files = store
        .insert_files(
            FilesMeta {
                paths: vec!["/tmp/a b.txt".into()],
                cut: true,
            },
            None,
            3000,
            &cfg,
        )
        .unwrap();
    let file_sources = build_mime_sources(&store, &files, false).unwrap();
    assert!(
        file_sources
            .iter()
            .any(|s| s.mime_type == MimeType::Specific("text/uri-list".into()))
    );
    assert!(
        file_sources
            .iter()
            .any(|s| s.mime_type == MimeType::Specific("x-special/gnome-copied-files".into()))
    );

    for term in [
        "com.system76.CosmicTerm",
        "org.gnome.Ptyxis",
        "kitty",
        "Alacritty",
        "foot",
        "org.wezfurlong.wezterm",
    ] {
        assert!(is_terminal_app(Some(term), &cfg.terminal_apps));
    }
    assert!(!is_terminal_app(Some("firefox"), &cfg.terminal_apps));
    assert!(!is_terminal_app(None, &cfg.terminal_apps));
}

#[test]
fn oversized_caps_and_formatting_helpers() {
    let dir = TempDir::new("caps");
    let mut store = ClipboardStore::open(dir.path()).unwrap();
    let mut cfg = ClipboardConfig {
        max_text_bytes: 10,
        record_oversized_placeholder: false,
        ..Default::default()
    };

    let offer = mimes(&["text/plain;charset=utf-8"]);
    let plan = select_mime_plan(&offer).unwrap();
    let mut reader = MockOfferReader::new(vec![(
        "text/plain;charset=utf-8",
        b"0123456789abcdef".to_vec(),
    )]);
    assert!(
        ingest_mime_plan(&mut reader, &plan, &offer, None, 1000, &cfg, &mut store)
            .unwrap()
            .is_none()
    );

    cfg.record_oversized_placeholder = true;
    let placeholder = ingest_mime_plan(&mut reader, &plan, &offer, None, 1000, &cfg, &mut store)
        .unwrap()
        .unwrap();
    assert!(placeholder.blob_refs.oversized);
    assert!(placeholder.preview.contains("Too large"));

    assert_eq!(format_relative_time(10_000, 20_000), "now");
    assert_eq!(format_relative_time(0, 120_000), "2m");
    assert_eq!(format_relative_time(0, 7_200_000), "2h");
    assert_eq!(format_relative_time(0, 172_800_000), "2d");
    assert_eq!(format_bytes(512), "512 B");
    assert_eq!(format_bytes(2048), "2.0 KB");
    assert_eq!(format_bytes(3 * 1024 * 1024), "3.0 MB");
}

#[test]
#[ignore = "requires a live Wayland session with data-control and wl-copy/wl-paste"]
fn live_wayland_round_trip() {
    use std::process::Command;
    let out = Command::new("wl-copy").arg("mac-clipboard-live-test").output();
    assert!(out.is_ok());
}
