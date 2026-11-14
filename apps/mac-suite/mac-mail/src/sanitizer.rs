// SPDX-License-Identifier: GPL-3.0-only

//! HTML email sanitization, tracker stripping, and safe browser fallback.
//!
//! Uses `ammonia` to enforce an allowlist of safe HTML tags, attributes, URL
//! schemes (including `cid:` for inline MIME parts), and non-network CSS
//! properties, followed by a DOM pass to strip 1x1 / hidden tracking pixels
//! and collect remote image URLs.

use base64::Engine as _;
use ego_tree::NodeRef;
use scraper::{Html, Node};
use std::{
    collections::{HashMap, HashSet},
    io::Write,
    path::PathBuf,
    sync::Arc,
};

/// An inline MIME attachment (such as a `cid:` image).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineImage {
    pub content_type: String,
    pub bytes: Arc<Vec<u8>>,
}

/// Result of sanitizing an HTML email body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SanitizedHtml {
    /// Clean HTML fragment with scripts, trackers, and external-resource styles removed.
    pub html: String,
    /// Number of tracking pixel `<img>` elements stripped.
    pub blocked_tracker_count: usize,
    /// Distinct remote `http://` / `https://` image URLs remaining in the HTML.
    pub remote_image_urls: Vec<String>,
}

const SAFE_STYLE_PROPERTIES: &[&str] = &[
    "color",
    "background-color",
    "font-weight",
    "font-style",
    "font-size",
    "text-decoration",
    "text-align",
    "vertical-align",
    "line-height",
    "margin",
    "margin-top",
    "margin-bottom",
    "margin-left",
    "margin-right",
    "padding",
    "padding-top",
    "padding-bottom",
    "padding-left",
    "padding-right",
    "border",
    "border-top",
    "border-bottom",
    "border-left",
    "border-right",
    "border-color",
    "border-collapse",
    "border-radius",
    "width",
    "max-width",
    "height",
    "display",
    "visibility",
    "opacity",
];

const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// Sanitize raw HTML from an email using `ammonia` and strip tracking pixels.
///
/// Strips `<script>`, `<style>`, `<link>`, `<title>`, `<iframe>`, `<object>`,
/// `<embed>`, event handlers (`on*`), `javascript:` / `vbscript:` URLs,
/// CSS properties/values that load external resources (`url(...)`, `@import`),
/// and tracking pixel `<img>` elements (1x1, 0x0, hidden, or known beacons).
pub fn sanitize_html(raw_html: &str) -> SanitizedHtml {
    let style_props: HashSet<&'static str> = SAFE_STYLE_PROPERTIES.iter().copied().collect();

    let mut builder = ammonia::Builder::new();
    builder
        .add_url_schemes(["cid", "data"])
        .add_clean_content_tags(["title", "iframe", "object", "embed"])
        .add_generic_attributes(["style"])
        .add_tag_attributes("table", ["role", "border", "cellpadding", "cellspacing", "width"])
        .add_tag_attributes("td", ["width", "valign"])
        .add_tag_attributes("th", ["width", "valign"])
        .filter_style_properties(style_props)
        .attribute_filter(|element, attribute, value| match (element, attribute) {
            ("img", "src") => filter_img_src(value),
            (_, "href") | (_, "cite") => {
                if starts_with_ignore_ascii_case(value.trim(), "data:") {
                    None
                } else {
                    Some(value.into())
                }
            }
            (_, "style") => sanitize_style_value(value).map(Into::into),
            ("table", "role") => {
                let lower = value.trim().to_ascii_lowercase();
                if lower == "presentation" || lower == "none" {
                    Some(value.into())
                } else {
                    None
                }
            }
            _ => Some(value.into()),
        });

    let ammonia_cleaned = builder.clean(raw_html).to_string();
    strip_trackers_and_collect_remote(&ammonia_cleaned)
}

fn filter_img_src(value: &str) -> Option<std::borrow::Cow<'_, str>> {
    let trimmed = value.trim();
    if starts_with_ignore_ascii_case(trimmed, "data:") {
        if is_safe_raster_data_uri(trimmed) {
            Some(trimmed.into())
        } else {
            None
        }
    } else {
        Some(trimmed.into())
    }
}

fn is_safe_raster_data_uri(uri: &str) -> bool {
    let lower = uri.to_ascii_lowercase();
    [
        "data:image/png;base64,",
        "data:image/jpeg;base64,",
        "data:image/jpg;base64,",
        "data:image/gif;base64,",
        "data:image/webp;base64,",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
}

/// Defense-in-depth filter for inline `style` attribute values.
///
/// Rejects any value containing `url(`, `@import`, `expression(`, `-moz-binding`,
/// or `javascript:`.
pub fn sanitize_style_value(style: &str) -> Option<String> {
    let lower = style.to_ascii_lowercase();
    let compact: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.contains("url(")
        || compact.contains("@import")
        || compact.contains("expression(")
        || compact.contains("-moz-binding")
        || compact.contains("javascript:")
        || compact.contains("vbscript:")
    {
        return None;
    }
    let trimmed = style.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn starts_with_ignore_ascii_case(haystack: &str, prefix: &str) -> bool {
    haystack
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

fn strip_trackers_and_collect_remote(cleaned_html: &str) -> SanitizedHtml {
    let fragment = Html::parse_fragment(cleaned_html);
    let mut blocked_tracker_count = 0usize;
    let mut remote_image_urls = Vec::new();
    let mut seen_remote = HashSet::new();
    let mut out = String::with_capacity(cleaned_html.len());

    for child in fragment.tree.root().children() {
        serialize_filtered(
            child,
            &mut out,
            &mut blocked_tracker_count,
            &mut remote_image_urls,
            &mut seen_remote,
            None,
        );
    }

    SanitizedHtml {
        html: out,
        blocked_tracker_count,
        remote_image_urls,
    }
}

fn serialize_filtered(
    node: NodeRef<'_, Node>,
    out: &mut String,
    blocked_tracker_count: &mut usize,
    remote_image_urls: &mut Vec<String>,
    seen_remote: &mut HashSet<String>,
    browser_opts: Option<(&HashMap<String, InlineImage>, bool)>,
) {
    match node.value() {
        Node::Text(text) => push_escaped_text(out, &text.text),
        Node::Element(el) => {
            let tag = el.name();
            if tag.eq_ignore_ascii_case("html")
                || tag.eq_ignore_ascii_case("body")
                || tag.eq_ignore_ascii_case("head")
            {
                for child in node.children() {
                    serialize_filtered(
                        child,
                        out,
                        blocked_tracker_count,
                        remote_image_urls,
                        seen_remote,
                        browser_opts,
                    );
                }
                return;
            }

            if tag.eq_ignore_ascii_case("img") {
                let src = el.attr("src").map(str::trim).unwrap_or("");
                if src.is_empty() {
                    return;
                }
                if is_tracker_image(
                    src,
                    el.attr("width"),
                    el.attr("height"),
                    el.attr("style"),
                ) {
                    *blocked_tracker_count += 1;
                    return;
                }

                let is_remote = is_remote_http_url(src);
                if is_remote && seen_remote.insert(src.to_owned()) {
                    remote_image_urls.push(src.to_owned());
                }

                // In browser mode, resolve `cid:` to `data:` and optionally block remote `src`.
                if let Some((inline_images, include_remote)) = browser_opts {
                    if is_remote && !include_remote {
                        let alt = el.attr("alt").unwrap_or("").trim();
                        if !alt.is_empty() {
                            out.push_str("[Blocked image: ");
                            push_escaped_text(out, alt);
                            out.push(']');
                        }
                        return;
                    }
                    out.push_str("<img");
                    for (k, v) in el.attrs() {
                        if k.eq_ignore_ascii_case("src")
                            && let Some(cid) = strip_cid_scheme(v)
                            && let Some(img) = lookup_cid(inline_images, cid)
                        {
                            let encoded = base64::engine::general_purpose::STANDARD
                                .encode(img.bytes.as_slice());
                            let data_uri =
                                format!("data:{};base64,{}", img.content_type, encoded);
                            out.push_str(" src=\"");
                            push_escaped_attr(out, &data_uri);
                            out.push('"');
                            continue;
                        }
                        out.push(' ');
                        out.push_str(k);
                        out.push_str("=\"");
                        push_escaped_attr(out, v);
                        out.push('"');
                    }
                    out.push('>');
                    return;
                }
            }

            out.push('<');
            out.push_str(tag);
            for (k, v) in el.attrs() {
                out.push(' ');
                out.push_str(k);
                out.push_str("=\"");
                push_escaped_attr(out, v);
                out.push('"');
            }
            out.push('>');

            if VOID_ELEMENTS.iter().any(|v| v.eq_ignore_ascii_case(tag)) {
                return;
            }

            for child in node.children() {
                serialize_filtered(
                    child,
                    out,
                    blocked_tracker_count,
                    remote_image_urls,
                    seen_remote,
                    browser_opts,
                );
            }

            out.push_str("</");
            out.push_str(tag);
            out.push('>');
        }
        Node::Document | Node::Fragment => {
            for child in node.children() {
                serialize_filtered(
                    child,
                    out,
                    blocked_tracker_count,
                    remote_image_urls,
                    seen_remote,
                    browser_opts,
                );
            }
        }
        _ => {}
    }
}

fn push_escaped_text(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
}

fn push_escaped_attr(out: &mut String, attr: &str) {
    for ch in attr.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
}

/// Returns `true` if the URL uses `http://` or `https://`.
pub fn is_remote_http_url(url: &str) -> bool {
    let trimmed = url.trim();
    starts_with_ignore_ascii_case(trimmed, "http://")
        || starts_with_ignore_ascii_case(trimmed, "https://")
}

/// Extracts the Content-ID from a `cid:...` URI (stripping `cid:` and angle brackets).
pub fn strip_cid_scheme(src: &str) -> Option<&str> {
    let trimmed = src.trim();
    if starts_with_ignore_ascii_case(trimmed, "cid:") {
        let rest = trimmed[4..].trim().trim_start_matches('<').trim_end_matches('>');
        (!rest.is_empty()).then_some(rest)
    } else {
        None
    }
}

/// Look up an inline image by Content-ID (case-insensitive, with or without angle brackets).
pub fn lookup_cid<'a>(
    inline_images: &'a HashMap<String, InlineImage>,
    cid: &str,
) -> Option<&'a InlineImage> {
    let norm = cid
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_ascii_lowercase();
    if let Some(found) = inline_images.get(&norm) {
        return Some(found);
    }
    if let Some((local, _)) = norm.split_once('@')
        && let Some(found) = inline_images.get(local)
    {
        return Some(found);
    }
    None
}

/// Heuristic to identify 1x1 / 0x0 / hidden / known open-tracking pixel images.
pub fn is_tracker_image(
    src: &str,
    width_attr: Option<&str>,
    height_attr: Option<&str>,
    style_attr: Option<&str>,
) -> bool {
    if !is_remote_http_url(src) {
        return false;
    }

    if width_attr.is_some_and(is_tiny_dimension) || height_attr.is_some_and(is_tiny_dimension) {
        return true;
    }

    if let Some(style) = style_attr {
        let compact: String = style
            .to_ascii_lowercase()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        if compact.contains("display:none")
            || compact.contains("visibility:hidden")
            || compact.contains("opacity:0;")
            || compact.ends_with("opacity:0")
            || compact.contains("width:0")
            || compact.contains("width:1px")
            || compact.contains("height:0")
            || compact.contains("height:1px")
        {
            return true;
        }
    }

    let lower_src = src.to_ascii_lowercase();
    [
        "google-analytics.com/collect",
        "/track/open",
        "/wf/open",
        "/t/open",
        "mandrillapp.com/track/",
        "list-manage.com/track/open",
        "/pixel.gif",
        "/pixel.png",
        "/open.gif",
        "/open.png",
        "/1x1.gif",
        "/1x1.png",
        "/spacer.gif",
        "/beacon",
    ]
    .iter()
    .any(|pattern| lower_src.contains(pattern))
}

fn is_tiny_dimension(raw: &str) -> bool {
    let trimmed = raw
        .trim()
        .trim_end_matches("px")
        .trim_end_matches("PX")
        .trim();
    matches!(trimmed.parse::<u32>(), Ok(n) if n <= 1)
}

/// Build a self-contained UTF-8 HTML5 document suitable for "Open in browser".
///
/// Includes a strict CSP meta tag blocking script execution, resolves `cid:`
/// images to inline `data:` URIs, and respects `include_remote_images`.
pub fn prepare_browser_html(
    sanitized_html: &str,
    inline_images: &HashMap<String, InlineImage>,
    include_remote_images: bool,
) -> String {
    let fragment = Html::parse_fragment(sanitized_html);
    let mut dummy_blocked = 0;
    let mut dummy_remote = Vec::new();
    let mut dummy_seen = HashSet::new();
    let mut body = String::with_capacity(sanitized_html.len());

    for child in fragment.tree.root().children() {
        serialize_filtered(
            child,
            &mut body,
            &mut dummy_blocked,
            &mut dummy_remote,
            &mut dummy_seen,
            Some((inline_images, include_remote_images)),
        );
    }

    let img_src_csp = if include_remote_images {
        "data: https: http:"
    } else {
        "data:"
    };

    format!(
        "<!DOCTYPE html>\n\
<html>\n\
<head>\n\
<meta charset=\"utf-8\">\n\
<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src {img_src_csp}; style-src 'unsafe-inline';\">\n\
<style>\n\
body {{ font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; line-height: 1.55; max-width: 760px; margin: 28px auto; padding: 0 20px; color: #1d1d1f; background: #ffffff; }}\n\
img {{ max-width: 100%; height: auto; }}\n\
blockquote {{ border-left: 3px solid #007aff; margin: 12px 0; padding: 4px 14px; color: #555; }}\n\
table {{ border-collapse: collapse; max-width: 100%; }}\n\
th, td {{ padding: 6px 10px; border: 1px solid #d2d2d7; text-align: left; }}\n\
pre, code {{ background: #f5f5f7; border-radius: 4px; }}\n\
pre {{ padding: 10px 12px; overflow-x: auto; }}\n\
a {{ color: #0066cc; }}\n\
</style>\n\
</head>\n\
<body>\n\
{body}\n\
</body>\n\
</html>\n"
    )
}

/// Write `html` to a secure temporary `.html` file (`0600` permissions, unique
/// name in the OS temp directory) and launch it in the system browser.
pub fn open_in_browser(html: &str) -> Result<PathBuf, String> {
    let mut file = tempfile::Builder::new()
        .prefix("mac-mail-")
        .suffix(".html")
        .tempfile()
        .map_err(|err| format!("Could not create temporary HTML file: {err}"))?;
    file.write_all(html.as_bytes())
        .map_err(|err| format!("Could not write temporary HTML file: {err}"))?;
    file.flush()
        .map_err(|err| format!("Could not flush temporary HTML file: {err}"))?;

    let (_, path) = file
        .keep()
        .map_err(|err| format!("Could not persist temporary HTML file: {err}"))?;

    open::that_detached(&path)
        .map_err(|err| format!("Could not open browser for {}: {err}", path.display()))?;
    Ok(path)
}

/// Open a link URL in the system browser, permitting only `http:`, `https:`, and `mailto:`.
pub fn open_external_url(raw_url: &str) -> Result<(), String> {
    let trimmed = raw_url.trim();
    let parsed = url::Url::parse(trimmed).map_err(|err| format!("Invalid URL: {err}"))?;
    match parsed.scheme() {
        "http" | "https" | "mailto" => open::that_detached(parsed.as_str())
            .map_err(|err| format!("Could not open URL {}: {err}", parsed.as_str())),
        other => Err(format!("Blocked unsupported URL scheme: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_scripts_event_handlers_and_javascript_urls() {
        let raw = r#"
            <div>
                <h1>Hello</h1>
                <script>alert('xss')</script>
                <p onclick="steal()" onmouseover="hack()">Click <a href="javascript:alert(1)">bad</a> or <a href="https://example.com/ok">good</a>.</p>
                <iframe src="https://evil.example/frame"></iframe>
                <title>Hidden Title Leak</title>
            </div>
        "#;
        let sanitized = sanitize_html(raw);
        let lower = sanitized.html.to_ascii_lowercase();
        assert!(!lower.contains("<script"));
        assert!(!lower.contains("alert("));
        assert!(!lower.contains("onclick"));
        assert!(!lower.contains("onmouseover"));
        assert!(!lower.contains("javascript:"));
        assert!(!lower.contains("<iframe"));
        assert!(!sanitized.html.contains("Hidden Title Leak"));
        assert!(sanitized.html.contains("<h1>Hello</h1>"));
        assert!(sanitized.html.contains("https://example.com/ok"));
    }

    #[test]
    fn strips_external_resource_styles_and_style_tags() {
        let raw = r#"
            <style>
                @import url("https://evil.example/leak.css");
                body { background-image: url("https://evil.example/bg.png"); }
            </style>
            <div style="color: #222; background-image: url('https://tracker.example/css.png');">Visible</div>
            <p style="background: url(https://tracker.example/bg2.png)">Keep text</p>
            <span style="font-weight: bold; color: blue;">Styled</span>
        "#;
        let sanitized = sanitize_html(raw);
        let lower = sanitized.html.to_ascii_lowercase();
        assert!(!lower.contains("<style"));
        assert!(!lower.contains("@import"));
        assert!(!lower.contains("url("));
        assert!(!lower.contains("tracker.example"));
        assert!(sanitized.html.contains("Visible"));
        assert!(sanitized.html.contains("Keep text"));
        assert!(sanitized.html.contains("Styled"));
        assert!(lower.contains("color:"));
    }

    #[test]
    fn strips_tracking_pixels_and_keeps_legitimate_remote_and_cid_images() {
        let raw = r#"
            <p>Newsletter</p>
            <img src="https://track.example.com/open.gif" width="1" height="1" alt="">
            <img src="https://track.example.com/pixel.png" style="display: none;" alt="">
            <img src="https://www.google-analytics.com/collect?v=1&tid=UA-123" alt="">
            <img src="https://cdn.example.com/hero.jpg" width="600" height="300" alt="Hero">
            <img src="cid:logo@example.com" width="1" height="1" alt="Logo">
        "#;
        let sanitized = sanitize_html(raw);
        assert_eq!(sanitized.blocked_tracker_count, 3);
        assert_eq!(
            sanitized.remote_image_urls,
            vec!["https://cdn.example.com/hero.jpg".to_owned()]
        );
        assert!(!sanitized.html.contains("track.example.com"));
        assert!(!sanitized.html.contains("google-analytics.com"));
        assert!(sanitized.html.contains("https://cdn.example.com/hero.jpg"));
        assert!(sanitized.html.contains("cid:logo@example.com"));
    }

    #[test]
    fn blocks_data_html_and_data_svg_while_allowing_raster_data_uri() {
        let raw = r#"
            <a href="data:text/html,<script>alert(1)</script>">link</a>
            <img src="data:image/svg+xml;base64,PHN2Zz48L3N2Zz4=" alt="svg">
            <img src="data:image/png;base64,iVBORw0KGgo=" alt="png">
        "#;
        let sanitized = sanitize_html(raw);
        assert!(!sanitized.html.contains("data:text/html"));
        assert!(!sanitized.html.contains("image/svg+xml"));
        assert!(sanitized.html.contains("data:image/png;base64,iVBORw0KGgo="));
    }

    #[test]
    fn browser_html_resolves_cid_and_blocks_remote_when_requested() {
        let mut inline = HashMap::new();
        inline.insert(
            "logo@example.com".to_owned(),
            InlineImage {
                content_type: "image/png".to_owned(),
                bytes: Arc::new(vec![1, 2, 3, 4]),
            },
        );
        let html = r#"<p>Hi</p><img src="cid:logo@example.com" alt="Logo"><img src="https://cdn.example.com/pic.jpg" alt="Banner">"#;

        let blocked = prepare_browser_html(html, &inline, false);
        assert!(blocked.contains("<meta charset=\"utf-8\">"));
        assert!(blocked.contains("Content-Security-Policy"));
        assert!(blocked.contains("data:image/png;base64,AQIDBA=="));
        assert!(!blocked.contains("https://cdn.example.com/pic.jpg"));
        assert!(blocked.contains("[Blocked image: Banner]"));

        let allowed = prepare_browser_html(html, &inline, true);
        assert!(allowed.contains("https://cdn.example.com/pic.jpg"));
        assert!(allowed.contains("data:image/png;base64,AQIDBA=="));
    }

    #[test]
    fn open_external_url_rejects_dangerous_schemes() {
        assert!(open_external_url("javascript:alert(1)").is_err());
        assert!(open_external_url("file:///etc/passwd").is_err());
    }
}
