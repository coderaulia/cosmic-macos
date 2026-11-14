// SPDX-License-Identifier: GPL-3.0-only

use mac_mail::{
    html_mail::{
        Block, ImageSource, MailBodyState, MailContent, format_hover_url, is_disguised_link,
    },
    sanitizer::{prepare_browser_html, sanitize_html},
};
use std::collections::HashMap;

const NEWSLETTER_EML: &[u8] = include_bytes!("fixtures/newsletter.eml");
const OUTLOOK_TABLE_EML: &[u8] = include_bytes!("fixtures/outlook_table.eml");
const PLAIN_TEXT_LINKS_EML: &[u8] = include_bytes!("fixtures/plain_text_links.eml");
const INLINE_CID_IMAGE_EML: &[u8] = include_bytes!("fixtures/inline_cid_image.eml");
const NON_ENGLISH_UTF8_EML: &[u8] = include_bytes!("fixtures/non_english_utf8.eml");

#[test]
fn corpus_newsletter_sanitizes_flattens_and_blocks_remote_images() {
    let content = MailContent::from_raw_mime(NEWSLETTER_EML);
    assert!(content.is_html);

    // Scripts, <style> tags, <title>, external CSS url(...), and tracking pixels must be stripped.
    let lower_html = content.sanitized_html.to_ascii_lowercase();
    assert!(!lower_html.contains("<script"));
    assert!(!lower_html.contains("<style"));
    assert!(!lower_html.contains("@import"));
    assert!(!lower_html.contains("url("));
    assert!(!lower_html.contains("track.newsletter.example"));
    assert!(!lower_html.contains("google-analytics.com"));
    assert!(!content.sanitized_html.contains("Weekly Tech & Design Digest</title>"));
    assert_eq!(content.blocked_tracker_count, 2);

    // Legitimate remote hero image is preserved in remote_image_urls (blocked behind "Load images").
    assert_eq!(
        content.remote_image_urls,
        vec!["https://cdn.newsletter.example/hero-banner.jpg".to_owned()]
    );

    // Outer layout tables must be flattened, leaving heading, quote, list, hr, and inner data table.
    assert!(content.blocks.iter().any(|b| matches!(b, Block::Heading { level: 1, .. })));
    assert!(content.blocks.iter().any(|b| matches!(b, Block::Quote(_))));
    assert!(content.blocks.iter().any(|b| matches!(b, Block::List { ordered: false, .. })));
    assert!(content.blocks.iter().any(|b| matches!(b, Block::HorizontalRule)));

    let data_tables: Vec<_> = content
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Table(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(data_tables.len(), 1);
    assert_eq!(data_tables[0].headers.len(), 3);
    assert_eq!(data_tables[0].rows.len(), 2);

    // Disguised link must be detectable on hover.
    let mut found_disguised = false;
    for block in &content.blocks {
        if let Block::Paragraph(spans) = block {
            for s in spans {
                if let Some(href) = s.link.as_deref()
                    && is_disguised_link(&s.text, href)
                {
                    found_disguised = true;
                    let hover = format_hover_url(href, Some(&s.text));
                    assert!(hover.contains("https://redirect.tracker.example/click?id=42"));
                }
            }
        }
    }
    assert!(found_disguised, "expected disguised link in newsletter");
}

#[test]
fn corpus_outlook_table_strips_mso_comments_flattens_wrapper_keeps_data_table() {
    let content = MailContent::from_raw_mime(OUTLOOK_TABLE_EML);
    assert!(content.is_html);
    assert!(!content.sanitized_html.contains("[if mso]"));
    assert!(!content.sanitized_html.contains("OfficeDocumentSettings"));

    // Wrapper table is flattened; ordered list, data table, and blockquote remain.
    assert!(content.blocks.iter().any(|b| matches!(b, Block::List { ordered: true, .. })));
    assert!(content.blocks.iter().any(|b| matches!(b, Block::Quote(_))));
    let tables: Vec<_> = content
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Table(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(tables.len(), 1);
    assert_eq!(tables[0].headers.len(), 3);
    assert_eq!(tables[0].rows.len(), 2);
}

#[test]
fn corpus_plain_text_links_and_quote_bars() {
    let content = MailContent::from_raw_mime(PLAIN_TEXT_LINKS_EML);
    assert!(!content.is_html);

    let mut links = Vec::new();
    let mut has_quote = false;
    for block in &content.blocks {
        match block {
            Block::Paragraph(spans) => {
                for s in spans {
                    if let Some(url) = &s.link {
                        links.push(url.clone());
                    }
                }
            }
            Block::Quote(inner) => {
                has_quote = true;
                for b in inner {
                    if let Block::Paragraph(spans) = b {
                        for s in spans {
                            if let Some(url) = &s.link {
                                links.push(url.clone());
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    assert!(has_quote);
    assert_eq!(
        links,
        vec![
            "https://releases.example.org/v2.4/notes_(draft)",
            "http://staging.example.org:8080/status",
            "mailto:support@example.org",
            "https://wiki.example.org/checklist",
        ]
    );
}

#[test]
fn corpus_inline_cid_images_resolve_immediately_while_remote_stays_blocked() {
    let content = MailContent::from_raw_mime(INLINE_CID_IMAGE_EML);
    assert!(content.is_html);

    // Both cid: PNGs resolved and decoded immediately.
    let brand = content
        .decoded_inline
        .get("brand@macmail.test")
        .expect("brand cid image decoded");
    assert_eq!((brand.width, brand.height), (4, 4));

    let chart = content
        .decoded_inline
        .get("chart-01@macmail.test")
        .expect("chart cid image decoded");
    assert_eq!((chart.width, chart.height), (2, 3));

    // Linked cid image preserves href on the MailImage node.
    let linked_img = content.blocks.iter().find_map(|b| match b {
        Block::Image(img) if matches!(&img.source, ImageSource::Cid(c) if c == "brand@macmail.test") => {
            Some(img)
        }
        _ => None,
    });
    assert_eq!(
        linked_img.and_then(|i| i.link.as_deref()),
        Some("https://macmail.test/dashboard")
    );

    // Remote banner is tracked for the "Load images" banner.
    assert_eq!(
        content.remote_image_urls,
        vec!["https://cdn.macmail.test/footer-banner.png".to_owned()]
    );

    // Browser HTML embeds cid: images as data URIs and respects remote block flag.
    let browser_blocked =
        prepare_browser_html(&content.sanitized_html, &content.inline_images, false);
    assert!(browser_blocked.contains("data:image/png;base64,"));
    assert!(!browser_blocked.contains("https://cdn.macmail.test/footer-banner.png"));
    assert!(browser_blocked.contains("[Blocked image: Footer Banner]"));

    let browser_loaded =
        prepare_browser_html(&content.sanitized_html, &content.inline_images, true);
    assert!(browser_loaded.contains("https://cdn.macmail.test/footer-banner.png"));
}

#[test]
fn corpus_non_english_utf8_and_quoted_printable_preserved() {
    let content = MailContent::from_raw_mime(NON_ENGLISH_UTF8_EML);
    assert!(content.is_html);
    assert!(content.plain_text.contains("Bonjour"));
    assert!(content.plain_text.contains("こんにちは"));
    assert!(content.plain_text.contains("📬"));
    assert!(content.plain_text.contains("Café au lait"));
    assert!(content.plain_text.contains("€4.50"));
    assert!(content.plain_text.contains("Привет!"));

    let browser_doc = prepare_browser_html(&content.sanitized_html, &HashMap::new(), false);
    assert!(browser_doc.contains("<meta charset=\"utf-8\">"));
    assert!(browser_doc.contains("こんにちは"));
    assert!(browser_doc.contains("Привет!"));
}

#[test]
fn sanitizer_and_view_smoke_test() {
    let raw = r#"<p>Hello <script>evil()</script><a href="https://example.com">link</a></p>"#;
    let sanitized = sanitize_html(raw);
    assert!(!sanitized.html.contains("evil()"));
    let content = MailContent::from_html(raw, HashMap::new());
    let state = MailBodyState::default();
    let _el: cosmic::Element<'_, ()> = mac_mail::html_mail::view(&content, &state, |_| ());
}
