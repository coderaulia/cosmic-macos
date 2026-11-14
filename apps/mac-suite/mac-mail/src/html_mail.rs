// SPDX-License-Identifier: GPL-3.0-only

//! HTML and plain-text mail parsing, inline `cid:` resolution, and native
//! COSMIC/iced rendering.
//!
//! Supported elements: paragraphs, headings (`h1`–`h6`), bold/italic/underline/
//! strikethrough/code, clickable links (with actual destination URL shown on
//! hover), ordered/unordered lists, quote bars (`blockquote` and plain-text `>`),
//! simple data tables, layout-table flattening into a single column, horizontal
//! rules, inline `cid:`/`data:` images, remote images gated behind a per-message
//! "Load images" banner, and an "Open in browser" fallback.

use base64::Engine as _;
use cosmic::{
    Element, Theme, font,
    iced::{
        Alignment, Background, Border, Color, ContentFit, Event, Length, Pixels, Point,
        Rectangle, Shadow, Size, Vector,
        advanced::{
            Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer,
            text::{self as adv_text, LineHeight, Paragraph as _, Shaping, Span, Wrapping},
            widget::tree::{self, Tree},
        },
        widget::{container as iced_container, text as iced_text},
    },
    widget::{button, column, container, divider, image as cosmic_image, mouse_area, row, text, tooltip},
};
use ego_tree::NodeRef;
use mail_parser::{MessageParser, PartKind};
use scraper::{ElementRef, Html, Node};
use std::{collections::HashMap, io::Read, sync::Arc, time::Duration};

pub use crate::sanitizer::{InlineImage, SanitizedHtml};
use crate::sanitizer;

const MAX_IMAGE_WIDTH: f32 = 640.0;
const MAX_REMOTE_IMAGE_BYTES: usize = 10 * 1024 * 1024;
const REMOTE_IMAGE_TIMEOUT_SECS: u64 = 12;

/// A decoded raster image ready for `cosmic::widget::image`.
#[derive(Clone, Debug)]
pub struct DecodedImage {
    pub handle: cosmic_image::Handle,
    pub width: u32,
    pub height: u32,
}

/// Source of an image inside an email.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageSource {
    Cid(String),
    DataUri {
        mime: String,
        bytes: Arc<Vec<u8>>,
    },
    Remote(String),
}

/// An image node inside an email body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailImage {
    pub source: ImageSource,
    pub alt: String,
    pub link: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// A styled inline run of text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextSpan {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub code: bool,
    pub link: Option<String>,
}

impl TextSpan {
    fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            code: false,
            link: None,
        }
    }
}

/// A simple data table extracted from HTML.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataTable {
    pub headers: Vec<Vec<TextSpan>>,
    pub rows: Vec<Vec<Vec<TextSpan>>>,
}

/// Block-level AST nodes rendered natively into COSMIC widgets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Heading {
        level: u8,
        spans: Vec<TextSpan>,
    },
    Paragraph(Vec<TextSpan>),
    Quote(Vec<Block>),
    List {
        ordered: bool,
        start: u64,
        items: Vec<Vec<Block>>,
    },
    CodeBlock(String),
    HorizontalRule,
    Table(DataTable),
    Image(MailImage),
}

/// Processed email body ready for native rendering and "Open in browser".
#[derive(Clone, Debug)]
pub struct MailContent {
    pub is_html: bool,
    pub plain_text: String,
    pub sanitized_html: String,
    pub blocks: Vec<Block>,
    pub inline_images: HashMap<String, InlineImage>,
    pub decoded_inline: HashMap<String, DecodedImage>,
    pub remote_image_urls: Vec<String>,
    pub blocked_tracker_count: usize,
}

impl Default for MailContent {
    fn default() -> Self {
        Self::from_plain_text("")
    }
}

/// Per-message UI state for remote image loading and link hover feedback.
#[derive(Clone, Debug, Default)]
pub struct MailBodyState {
    pub remote_images_loaded: bool,
    pub loading_remote_images: bool,
    pub remote_images: HashMap<String, Result<DecodedImage, String>>,
    pub hovered_link: Option<String>,
    /// Sender email address for the "Always load images from <sender>" option.
    pub sender_address: String,
}

/// Interactive events emitted by the native mail body renderer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HtmlMailEvent {
    OpenLink(String),
    HoverLink(Option<String>),
    LoadRemoteImages,
    AlwaysLoadFromSender,
    OpenInBrowser,
}

impl MailContent {
    /// Parse a raw RFC 5322 / MIME email into sanitized, renderable blocks and
    /// decoded inline `cid:` images.
    pub fn from_raw_mime(raw: &[u8]) -> Self {
        let Some(message) = MessageParser::default().parse(raw) else {
            return Self::from_plain_text("(the message could not be parsed)");
        };

        let mut inline_images = HashMap::new();
        for part in message.parts() {
            let is_image_type = part
                .content_type()
                .is_some_and(|ct| ct.ctype().eq_ignore_ascii_case("image"));
            let has_cid = part.content_id().is_some();
            if !has_cid && !is_image_type {
                continue;
            }
            let mime = part
                .content_type()
                .map(|ct| match ct.subtype() {
                    Some(sub) => format!("{}/{}", ct.ctype(), sub),
                    None => ct.ctype().to_owned(),
                })
                .unwrap_or_else(|| "application/octet-stream".to_owned());
            if !mime.to_ascii_lowercase().starts_with("image/")
                && !matches!(part.kind(), PartKind::InlineBinary | PartKind::Binary)
            {
                continue;
            }
            let bytes = Arc::new(part.decoded().into_owned());
            if let Some(cid) = part.content_id() {
                insert_cid_aliases(&mut inline_images, cid, mime.clone(), bytes.clone());
            }
            if let Some(loc) = part.content_location() {
                insert_cid_aliases(&mut inline_images, loc, mime, bytes);
            }
        }

        let plain_fallback = message
            .body_text(0)
            .map(|s| s.into_owned())
            .unwrap_or_default();

        let html_parts: Vec<String> = message
            .html_body()
            .filter(|part| matches!(part.kind(), PartKind::Html))
            .filter_map(|part| part.text().map(|t| t.into_owned()))
            .collect();

        if !html_parts.is_empty() {
            let combined_html = html_parts.join("\n");
            let mut content = Self::from_html(&combined_html, inline_images);
            if !plain_fallback.is_empty() {
                content.plain_text = plain_fallback;
            }
            content
        } else {
            let text = if plain_fallback.is_empty() {
                "(this message has no text)".to_owned()
            } else {
                plain_fallback
            };
            Self::from_plain_text(&text)
        }
    }

    /// Sanitize an HTML body and parse it into native blocks, resolving inline
    /// `cid:` and `data:` images immediately.
    pub fn from_html(raw_html: &str, inline_images: HashMap<String, InlineImage>) -> Self {
        let sanitized = sanitizer::sanitize_html(raw_html);
        let blocks = parse_html_blocks(&sanitized.html);
        let mut decoded_inline = HashMap::new();

        for (key, img) in &inline_images {
            if let Ok(decoded) = decode_image_bytes(&img.bytes) {
                decoded_inline.insert(key.clone(), decoded);
            }
        }

        // Pre-decode any safe `data:` image URIs referenced in the AST.
        predecode_data_images(&blocks, &mut decoded_inline);

        let plain_text = blocks_to_plain_text(&blocks);

        Self {
            is_html: true,
            plain_text,
            sanitized_html: sanitized.html,
            blocks,
            inline_images,
            decoded_inline,
            remote_image_urls: sanitized.remote_image_urls,
            blocked_tracker_count: sanitized.blocked_tracker_count,
        }
    }

    /// Parse a plain-text email body into native blocks with clickable URLs and
    /// `>` quote bars.
    pub fn from_plain_text(text: &str) -> Self {
        let blocks = parse_plain_text_blocks(text);
        Self {
            is_html: false,
            plain_text: text.to_owned(),
            sanitized_html: String::new(),
            blocks,
            inline_images: HashMap::new(),
            decoded_inline: HashMap::new(),
            remote_image_urls: Vec::new(),
            blocked_tracker_count: 0,
        }
    }
}

fn insert_cid_aliases(
    map: &mut HashMap<String, InlineImage>,
    raw_id: &str,
    content_type: String,
    bytes: Arc<Vec<u8>>,
) {
    let norm = raw_id
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_ascii_lowercase();
    if norm.is_empty() {
        return;
    }
    let entry = InlineImage {
        content_type,
        bytes,
    };
    map.insert(norm.clone(), entry.clone());
    if let Some((local, _)) = norm.split_once('@')
        && !local.is_empty()
    {
        map.entry(local.to_owned()).or_insert(entry);
    }
}

fn predecode_data_images(blocks: &[Block], out: &mut HashMap<String, DecodedImage>) {
    for block in blocks {
        match block {
            Block::Image(img) => {
                if let ImageSource::DataUri { bytes, .. } = &img.source {
                    let key = data_uri_cache_key(bytes);
                    if !out.contains_key(&key)
                        && let Ok(decoded) = decode_image_bytes(bytes)
                    {
                        out.insert(key, decoded);
                    }
                }
            }
            Block::Quote(inner) => predecode_data_images(inner, out),
            Block::List { items, .. } => {
                for item in items {
                    predecode_data_images(item, out);
                }
            }
            _ => {}
        }
    }
}

fn data_uri_cache_key(bytes: &[u8]) -> String {
    format!("__data_{:p}_{}", bytes.as_ptr(), bytes.len())
}

/// Decode raster image bytes (PNG, JPEG, GIF, WebP, ICO, BMP) into an RGBA `Handle`.
pub fn decode_image_bytes(bytes: &[u8]) -> Result<DecodedImage, String> {
    let dyn_img =
        image::load_from_memory(bytes).map_err(|err| format!("Image decode failed: {err}"))?;
    let rgba = dyn_img.to_rgba8();
    let (width, height) = rgba.dimensions();
    if width == 0 || height == 0 {
        return Err("Image has zero dimensions".to_owned());
    }
    Ok(DecodedImage {
        handle: cosmic_image::Handle::from_rgba(width, height, rgba.into_raw()),
        width,
        height,
    })
}

/// Fetch remote `http(s)://` images when the user clicks "Load images".
///
/// Designed to run inside a blocking worker thread.
pub fn fetch_remote_images(urls: &[String]) -> HashMap<String, Result<DecodedImage, String>> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(REMOTE_IMAGE_TIMEOUT_SECS)))
        .build()
        .new_agent();

    let mut results = HashMap::new();
    for url in urls {
        if results.contains_key(url) {
            continue;
        }
        let outcome = fetch_one_remote_image(&agent, url);
        results.insert(url.clone(), outcome);
    }
    results
}

fn fetch_one_remote_image(agent: &ureq::Agent, url: &str) -> Result<DecodedImage, String> {
    if !sanitizer::is_remote_http_url(url) {
        return Err("Unsupported image URL scheme".to_owned());
    }
    let response = agent
        .get(url)
        .header("User-Agent", "MacMail/0.1")
        .call()
        .map_err(|err| format!("Failed to fetch image: {err}"))?;
    let mut reader = response
        .into_body()
        .into_reader()
        .take((MAX_REMOTE_IMAGE_BYTES + 1) as u64);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|err| format!("Failed reading image body: {err}"))?;
    if bytes.len() > MAX_REMOTE_IMAGE_BYTES {
        return Err("Image exceeds 10 MiB limit".to_owned());
    }
    decode_image_bytes(&bytes)
}

// ============================================================================
// HTML -> Block AST parser
// ============================================================================

#[derive(Clone, Debug, Default)]
struct InlineStyle {
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
    code: bool,
    link: Option<String>,
}

/// Parse sanitized HTML into a list of native `Block` elements.
pub fn parse_html_blocks(sanitized_html: &str) -> Vec<Block> {
    let fragment = Html::parse_fragment(sanitized_html);
    let mut blocks = Vec::new();
    let mut inline_buf = Vec::new();

    for child in fragment.tree.root().children() {
        collect_node(child, &InlineStyle::default(), &mut inline_buf, &mut blocks);
    }
    flush_inline(&mut inline_buf, &mut blocks);
    blocks
}

fn collect_node(
    node: NodeRef<'_, Node>,
    style: &InlineStyle,
    inline_buf: &mut Vec<TextSpan>,
    blocks: &mut Vec<Block>,
) {
    match node.value() {
        Node::Text(t) => {
            push_collapsed_text(inline_buf, &t.text, style);
        }
        Node::Element(el) => {
            let tag = el.name().to_ascii_lowercase();
            match tag.as_str() {
                "html" | "body" | "head" | "main" => {
                    for child in node.children() {
                        collect_node(child, style, inline_buf, blocks);
                    }
                }
                "br" => {
                    push_span(inline_buf, "\n", style);
                }
                "hr" => {
                    flush_inline(inline_buf, blocks);
                    blocks.push(Block::HorizontalRule);
                }
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                    flush_inline(inline_buf, blocks);
                    let level = tag[1..].parse::<u8>().unwrap_or(1);
                    let mut spans = Vec::new();
                    let mut nested_blocks = Vec::new();
                    let heading_style = InlineStyle {
                        bold: true,
                        ..style.clone()
                    };
                    for child in node.children() {
                        collect_node(child, &heading_style, &mut spans, &mut nested_blocks);
                    }
                    trim_spans(&mut spans);
                    if !spans.is_empty() {
                        blocks.push(Block::Heading { level, spans });
                    }
                    blocks.extend(nested_blocks);
                }
                "p" | "div" | "section" | "article" | "header" | "footer" | "aside"
                | "figcaption" | "figure" | "center" | "summary" | "details" => {
                    flush_inline(inline_buf, blocks);
                    let block_style = apply_css_inline_style(el.attr("style"), style);
                    let mut local_inline = Vec::new();
                    for child in node.children() {
                        collect_node(child, &block_style, &mut local_inline, blocks);
                    }
                    flush_inline(&mut local_inline, blocks);
                }
                "blockquote" => {
                    flush_inline(inline_buf, blocks);
                    let mut inner = Vec::new();
                    let mut inner_inline = Vec::new();
                    for child in node.children() {
                        collect_node(child, style, &mut inner_inline, &mut inner);
                    }
                    flush_inline(&mut inner_inline, &mut inner);
                    if !inner.is_empty() {
                        blocks.push(Block::Quote(inner));
                    }
                }
                "pre" => {
                    flush_inline(inline_buf, blocks);
                    let raw = collect_raw_text(node);
                    let trimmed = raw.trim_matches('\n');
                    if !trimmed.is_empty() {
                        blocks.push(Block::CodeBlock(trimmed.to_owned()));
                    }
                }
                "ul" | "ol" => {
                    flush_inline(inline_buf, blocks);
                    let ordered = tag == "ol";
                    let start = el
                        .attr("start")
                        .and_then(|s| s.trim().parse::<u64>().ok())
                        .unwrap_or(1);
                    let mut items = Vec::new();
                    for child in node.children() {
                        if let Some(child_el) = ElementRef::wrap(child)
                            && child_el.value().name().eq_ignore_ascii_case("li")
                        {
                            let mut item_blocks = Vec::new();
                            let mut item_inline = Vec::new();
                            for li_child in child.children() {
                                collect_node(li_child, style, &mut item_inline, &mut item_blocks);
                            }
                            flush_inline(&mut item_inline, &mut item_blocks);
                            if !item_blocks.is_empty() {
                                items.push(item_blocks);
                            }
                        }
                    }
                    if !items.is_empty() {
                        blocks.push(Block::List {
                            ordered,
                            start,
                            items,
                        });
                    }
                }
                "table" => {
                    flush_inline(inline_buf, blocks);
                    if let Some(el_ref) = ElementRef::wrap(node) {
                        if is_layout_table(el_ref) {
                            flatten_layout_table(el_ref, style, blocks);
                        } else if let Some(table) = parse_data_table(el_ref, style) {
                            blocks.push(Block::Table(table));
                        }
                    }
                }
                "img" => {
                    if let Some(img) = parse_image_element(el, style.link.clone()) {
                        flush_inline(inline_buf, blocks);
                        blocks.push(Block::Image(img));
                    }
                }
                "b" | "strong" => {
                    let mut next = style.clone();
                    next.bold = true;
                    for child in node.children() {
                        collect_node(child, &next, inline_buf, blocks);
                    }
                }
                "i" | "em" | "cite" | "var" | "dfn" => {
                    let mut next = style.clone();
                    next.italic = true;
                    for child in node.children() {
                        collect_node(child, &next, inline_buf, blocks);
                    }
                }
                "u" | "ins" => {
                    let mut next = style.clone();
                    next.underline = true;
                    for child in node.children() {
                        collect_node(child, &next, inline_buf, blocks);
                    }
                }
                "s" | "del" | "strike" => {
                    let mut next = style.clone();
                    next.strikethrough = true;
                    for child in node.children() {
                        collect_node(child, &next, inline_buf, blocks);
                    }
                }
                "code" | "tt" | "kbd" | "samp" => {
                    let mut next = style.clone();
                    next.code = true;
                    for child in node.children() {
                        collect_node(child, &next, inline_buf, blocks);
                    }
                }
                "a" => {
                    let mut next = style.clone();
                    if let Some(href) = el.attr("href").and_then(valid_clickable_url) {
                        next.link = Some(href);
                        next.underline = true;
                    }
                    for child in node.children() {
                        collect_node(child, &next, inline_buf, blocks);
                    }
                }
                _ => {
                    let next = apply_css_inline_style(el.attr("style"), style);
                    for child in node.children() {
                        collect_node(child, &next, inline_buf, blocks);
                    }
                }
            }
        }
        _ => {}
    }
}

fn apply_css_inline_style(style_attr: Option<&str>, base: &InlineStyle) -> InlineStyle {
    let Some(raw) = style_attr else {
        return base.clone();
    };
    let mut next = base.clone();
    for decl in raw.split(';') {
        let Some((prop, val)) = decl.split_once(':') else {
            continue;
        };
        let prop = prop.trim().to_ascii_lowercase();
        let val = val.trim().to_ascii_lowercase();
        match prop.as_str() {
            "font-weight" => {
                if val == "bold" || val == "bolder" || val.parse::<u16>().is_ok_and(|n| n >= 600) {
                    next.bold = true;
                }
            }
            "font-style" => {
                if val == "italic" || val == "oblique" {
                    next.italic = true;
                }
            }
            "text-decoration" => {
                if val.contains("underline") {
                    next.underline = true;
                }
                if val.contains("line-through") {
                    next.strikethrough = true;
                }
            }
            _ => {}
        }
    }
    next
}

fn valid_clickable_url(href: &str) -> Option<String> {
    let trimmed = href.trim();
    let parsed = url::Url::parse(trimmed).ok()?;
    match parsed.scheme() {
        "http" | "https" | "mailto" | "tel" => Some(trimmed.to_owned()),
        _ => None,
    }
}

fn parse_image_element(el: &scraper::node::Element, link: Option<String>) -> Option<MailImage> {
    let src = el.attr("src")?.trim();
    if src.is_empty() {
        return None;
    }
    let source = if let Some(cid) = sanitizer::strip_cid_scheme(src) {
        ImageSource::Cid(cid.to_owned())
    } else if let Some((mime, bytes)) = decode_data_image_uri(src) {
        ImageSource::DataUri {
            mime,
            bytes: Arc::new(bytes),
        }
    } else if sanitizer::is_remote_http_url(src) {
        ImageSource::Remote(src.to_owned())
    } else {
        return None;
    };

    let width = el.attr("width").and_then(parse_px_attr);
    let height = el.attr("height").and_then(parse_px_attr);
    let alt = el.attr("alt").unwrap_or("").trim().to_owned();

    Some(MailImage {
        source,
        alt,
        link,
        width,
        height,
    })
}

fn parse_px_attr(val: &str) -> Option<u32> {
    val.trim()
        .trim_end_matches("px")
        .trim_end_matches("PX")
        .trim()
        .parse::<u32>()
        .ok()
}

fn decode_data_image_uri(uri: &str) -> Option<(String, Vec<u8>)> {
    let lower = uri.to_ascii_lowercase();
    if !lower.starts_with("data:image/") {
        return None;
    }
    let (meta, data) = uri[5..].split_once(',')?;
    let mime = meta.split(';').next()?.trim().to_ascii_lowercase();
    if !meta.to_ascii_lowercase().contains(";base64") {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.trim())
        .ok()?;
    Some((mime, bytes))
}

fn collect_raw_text(node: NodeRef<'_, Node>) -> String {
    let mut out = String::new();
    match node.value() {
        Node::Text(t) => out.push_str(&t.text),
        Node::Element(el) if el.name().eq_ignore_ascii_case("br") => out.push('\n'),
        _ => {
            for child in node.children() {
                out.push_str(&collect_raw_text(child));
            }
        }
    }
    out
}

fn push_collapsed_text(buf: &mut Vec<TextSpan>, raw: &str, style: &InlineStyle) {
    let mut collapsed = String::with_capacity(raw.len());
    let mut prev_ws = buf
        .last()
        .is_some_and(|s| s.text.ends_with(|c: char| c.is_whitespace()));

    for ch in raw.chars() {
        // Treat non-breaking spaces in spacer cells like whitespace only when collapsing?
        // Keep U+00A0 in normal text as a space so `&nbsp;` doesn't disappear mid-sentence.
        if ch.is_whitespace() {
            if !prev_ws {
                collapsed.push(' ');
                prev_ws = true;
            }
        } else {
            collapsed.push(ch);
            prev_ws = false;
        }
    }

    if !collapsed.is_empty() {
        push_span(buf, &collapsed, style);
    }
}

fn push_span(buf: &mut Vec<TextSpan>, text: &str, style: &InlineStyle) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = buf.last_mut()
        && last.bold == style.bold
        && last.italic == style.italic
        && last.underline == style.underline
        && last.strikethrough == style.strikethrough
        && last.code == style.code
        && last.link == style.link
    {
        last.text.push_str(text);
        return;
    }
    buf.push(TextSpan {
        text: text.to_owned(),
        bold: style.bold,
        italic: style.italic,
        underline: style.underline,
        strikethrough: style.strikethrough,
        code: style.code,
        link: style.link.clone(),
    });
}

fn trim_spans(spans: &mut Vec<TextSpan>) {
    while let Some(first) = spans.first_mut() {
        let trimmed = first.text.trim_start();
        if trimmed.is_empty() {
            spans.remove(0);
        } else {
            if trimmed.len() != first.text.len() {
                first.text = trimmed.to_owned();
            }
            break;
        }
    }
    while let Some(last) = spans.last_mut() {
        let trimmed = last.text.trim_end();
        if trimmed.is_empty() {
            spans.pop();
        } else {
            if trimmed.len() != last.text.len() {
                last.text = trimmed.to_owned();
            }
            break;
        }
    }
}

fn flush_inline(inline_buf: &mut Vec<TextSpan>, blocks: &mut Vec<Block>) {
    trim_spans(inline_buf);
    if !inline_buf.is_empty() {
        blocks.push(Block::Paragraph(std::mem::take(inline_buf)));
    }
}

// ============================================================================
// Tables: layout table flattening vs simple data tables
// ============================================================================

/// Collect direct `<tr>` rows of `table` without descending into nested tables.
fn direct_table_rows<'a>(table: ElementRef<'a>) -> Vec<ElementRef<'a>> {
    let mut rows = Vec::new();
    for child in table.children() {
        let Some(child_el) = ElementRef::wrap(child) else {
            continue;
        };
        let name = child_el.value().name().to_ascii_lowercase();
        match name.as_str() {
            "tr" => rows.push(child_el),
            "thead" | "tbody" | "tfoot" => {
                for sub in child_el.children() {
                    if let Some(sub_el) = ElementRef::wrap(sub)
                        && sub_el.value().name().eq_ignore_ascii_case("tr")
                    {
                        rows.push(sub_el);
                    }
                }
            }
            _ => {}
        }
    }
    rows
}

fn row_cells<'a>(tr: ElementRef<'a>) -> Vec<ElementRef<'a>> {
    tr.children()
        .filter_map(ElementRef::wrap)
        .filter(|el| {
            let n = el.value().name();
            n.eq_ignore_ascii_case("td") || n.eq_ignore_ascii_case("th")
        })
        .collect()
}

fn contains_nested_table(el: ElementRef<'_>) -> bool {
    for desc in el.descendants() {
        if desc.id() == el.id() {
            continue;
        }
        if let Some(desc_el) = ElementRef::wrap(desc)
            && desc_el.value().name().eq_ignore_ascii_case("table")
        {
            return true;
        }
    }
    false
}

/// Distinguish layout tables (newsletters, Outlook wrappers) from simple data tables.
fn is_layout_table(table: ElementRef<'_>) -> bool {
    if let Some(role) = table.value().attr("role")
        && (role.eq_ignore_ascii_case("presentation") || role.eq_ignore_ascii_case("none"))
    {
        return true;
    }

    if contains_nested_table(table) {
        return true;
    }

    let rows = direct_table_rows(table);
    if rows.len() <= 1 {
        return true;
    }

    let mut max_cols = 0usize;
    let mut th_count = 0usize;
    let mut has_rich_block = false;

    for row in &rows {
        let cells = row_cells(*row);
        max_cols = max_cols.max(cells.len());
        for cell in cells {
            if cell.value().name().eq_ignore_ascii_case("th") {
                th_count += 1;
            }
            let mut para_count = 0usize;
            for desc in cell.descendants() {
                if let Some(d) = ElementRef::wrap(desc) {
                    let name = d.value().name().to_ascii_lowercase();
                    match name.as_str() {
                        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "ul" | "ol" | "blockquote"
                        | "hr" => {
                            has_rich_block = true;
                        }
                        "p" | "div" => {
                            para_count += 1;
                            if para_count >= 2 {
                                has_rich_block = true;
                            }
                        }
                        "img"
                            if d.value()
                                .attr("width")
                                .and_then(parse_px_attr)
                                .is_some_and(|w| w >= 150) =>
                        {
                            has_rich_block = true;
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    if max_cols <= 1 || max_cols > 8 {
        return true;
    }

    if th_count == 0 && (has_rich_block || table.value().attr("border") == Some("0")) {
        return true;
    }

    false
}

fn flatten_layout_table(table: ElementRef<'_>, style: &InlineStyle, blocks: &mut Vec<Block>) {
    for row in direct_table_rows(table) {
        for cell in row_cells(row) {
            let cell_style = apply_css_inline_style(cell.value().attr("style"), style);
            let mut local_inline = Vec::new();
            for child in cell.children() {
                collect_node(child, &cell_style, &mut local_inline, blocks);
            }
            flush_inline(&mut local_inline, blocks);
        }
    }
}

fn parse_data_table(table: ElementRef<'_>, style: &InlineStyle) -> Option<DataTable> {
    let trs = direct_table_rows(table);
    if trs.is_empty() {
        return None;
    }

    let mut headers: Vec<Vec<TextSpan>> = Vec::new();
    let mut rows: Vec<Vec<Vec<TextSpan>>> = Vec::new();

    for (idx, tr) in trs.into_iter().enumerate() {
        let cells = row_cells(tr);
        if cells.is_empty() {
            continue;
        }
        let in_thead = tr
            .parent()
            .and_then(ElementRef::wrap)
            .is_some_and(|p| p.value().name().eq_ignore_ascii_case("thead"));
        let all_th = cells
            .iter()
            .all(|c| c.value().name().eq_ignore_ascii_case("th"));

        let parsed_cells: Vec<Vec<TextSpan>> = cells
            .into_iter()
            .map(|cell| {
                let mut cell_spans = Vec::new();
                let mut cell_blocks = Vec::new();
                let cell_style = InlineStyle {
                    bold: style.bold || cell.value().name().eq_ignore_ascii_case("th"),
                    ..apply_css_inline_style(cell.value().attr("style"), style)
                };
                for child in cell.children() {
                    collect_node(child, &cell_style, &mut cell_spans, &mut cell_blocks);
                }
                for block in cell_blocks {
                    match block {
                        Block::Paragraph(s) | Block::Heading { spans: s, .. } => {
                            if !cell_spans.is_empty() {
                                cell_spans.push(TextSpan::plain(" "));
                            }
                            cell_spans.extend(s);
                        }
                        _ => {}
                    }
                }
                trim_spans(&mut cell_spans);
                cell_spans
            })
            .collect();

        if idx == 0 && headers.is_empty() && (in_thead || all_th) {
            headers = parsed_cells;
        } else {
            rows.push(parsed_cells);
        }
    }

    if headers.is_empty() && rows.is_empty() {
        return None;
    }
    Some(DataTable { headers, rows })
}

// ============================================================================
// Plain-text -> Block AST + Linkification
// ============================================================================

/// Parse plain-text email into paragraphs and `> ` quote blocks with clickable URLs.
pub fn parse_plain_text_blocks(text: &str) -> Vec<Block> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    let mut blocks = Vec::new();
    let mut idx = 0usize;

    while idx < lines.len() {
        let line = lines[idx];
        if line.trim().is_empty() {
            idx += 1;
            continue;
        }

        if let Some(level) = quote_depth(line)
            && level > 0
        {
            let mut quoted_lines = Vec::new();
            while idx < lines.len() {
                let l = lines[idx];
                if quote_depth(l).unwrap_or(0) >= 1 {
                    let stripped = l.trim_start().strip_prefix('>').unwrap_or(l);
                    let stripped = stripped.strip_prefix(' ').unwrap_or(stripped);
                    quoted_lines.push(stripped);
                    idx += 1;
                } else {
                    break;
                }
            }
            let inner = parse_plain_text_blocks(&quoted_lines.join("\n"));
            if !inner.is_empty() {
                blocks.push(Block::Quote(inner));
            }
            continue;
        }

        let mut para_lines = Vec::new();
        while idx < lines.len() {
            let l = lines[idx];
            if l.trim().is_empty() || quote_depth(l).unwrap_or(0) > 0 {
                break;
            }
            para_lines.push(l);
            idx += 1;
        }
        let para_text = para_lines.join("\n");
        let spans = linkify_plain_text(&para_text);
        if !spans.is_empty() {
            blocks.push(Block::Paragraph(spans));
        }
    }

    blocks
}

fn quote_depth(line: &str) -> Option<usize> {
    let mut rest = line.trim_start();
    let mut depth = 0usize;
    while let Some(after) = rest.strip_prefix('>') {
        depth += 1;
        rest = after.strip_prefix(' ').unwrap_or(after);
    }
    (depth > 0).then_some(depth)
}

/// Detect `http://`, `https://`, and `mailto:` links inside plain text.
pub fn linkify_plain_text(text: &str) -> Vec<TextSpan> {
    let mut spans = Vec::new();
    let mut cursor = 0usize;

    while cursor < text.len() {
        let rest = &text[cursor..];
        let next_match = ["https://", "http://", "mailto:"]
            .iter()
            .filter_map(|scheme| rest.find(scheme).map(|pos| (pos, *scheme)))
            .min_by_key(|(pos, _)| *pos);

        let Some((rel_pos, _scheme)) = next_match else {
            spans.push(TextSpan::plain(rest));
            break;
        };

        if rel_pos > 0 {
            spans.push(TextSpan::plain(&rest[..rel_pos]));
        }

        let start = cursor + rel_pos;
        let tail = &text[start..];
        let raw_end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\''))
            .unwrap_or(tail.len());
        let candidate = &tail[..raw_end];
        let trimmed = trim_url_trailing_punct(candidate);

        if let Some(valid) = valid_clickable_url(trimmed) {
            spans.push(TextSpan {
                text: trimmed.to_owned(),
                bold: false,
                italic: false,
                underline: true,
                strikethrough: false,
                code: false,
                link: Some(valid),
            });
            cursor = start + trimmed.len();
        } else {
            spans.push(TextSpan::plain(candidate));
            cursor = start + candidate.len();
        }
    }

    spans
}

fn trim_url_trailing_punct(mut s: &str) -> &str {
    loop {
        let before = s;
        s = s.trim_end_matches(['.', ',', ';', ':', '!', '?', '\'']);
        if s.ends_with(')') {
            let opens = s.chars().filter(|&c| c == '(').count();
            let closes = s.chars().filter(|&c| c == ')').count();
            if closes > opens {
                s = &s[..s.len() - 1];
            }
        }
        if s.ends_with(']') {
            let opens = s.chars().filter(|&c| c == '[').count();
            let closes = s.chars().filter(|&c| c == ']').count();
            if closes > opens {
                s = &s[..s.len() - 1];
            }
        }
        if s.len() == before.len() {
            break;
        }
    }
    s
}

fn blocks_to_plain_text(blocks: &[Block]) -> String {
    let mut out = Vec::new();
    for block in blocks {
        match block {
            Block::Heading { spans, .. } | Block::Paragraph(spans) => {
                let s: String = spans.iter().map(|sp| sp.text.as_str()).collect();
                if !s.is_empty() {
                    out.push(s);
                }
            }
            Block::Quote(inner) => {
                let inner_txt = blocks_to_plain_text(inner);
                for line in inner_txt.lines() {
                    out.push(format!("> {line}"));
                }
            }
            Block::List {
                ordered,
                start,
                items,
            } => {
                for (i, item) in items.iter().enumerate() {
                    let prefix = if *ordered {
                        format!("{}. ", *start + i as u64)
                    } else {
                        "• ".to_owned()
                    };
                    out.push(format!("{prefix}{}", blocks_to_plain_text(item)));
                }
            }
            Block::CodeBlock(code) => out.push(code.clone()),
            Block::HorizontalRule => out.push("---".to_owned()),
            Block::Table(table) => {
                if !table.headers.is_empty() {
                    let row: Vec<String> = table
                        .headers
                        .iter()
                        .map(|c| c.iter().map(|s| s.text.as_str()).collect())
                        .collect();
                    out.push(row.join(" | "));
                }
                for r in &table.rows {
                    let row: Vec<String> = r
                        .iter()
                        .map(|c| c.iter().map(|s| s.text.as_str()).collect())
                        .collect();
                    out.push(row.join(" | "));
                }
            }
            Block::Image(img) => {
                if !img.alt.is_empty() {
                    out.push(format!("[Image: {}]", img.alt));
                }
            }
        }
    }
    out.join("\n\n")
}

// ============================================================================
// Disguised link inspection
// ============================================================================

/// Returns `true` if `display_text` looks like a URL whose host differs from `href`.
pub fn is_disguised_link(display_text: &str, href: &str) -> bool {
    let text = display_text.trim();
    if !(text.starts_with("http://") || text.starts_with("https://")) {
        return false;
    }
    let (Ok(shown), Ok(actual)) = (url::Url::parse(text), url::Url::parse(href.trim())) else {
        return false;
    };
    match (shown.host_str(), actual.host_str()) {
        (Some(a), Some(b)) => !a.eq_ignore_ascii_case(b),
        _ => false,
    }
}

/// Format the hover destination URL (adding a warning if the link text disguises a different host).
pub fn format_hover_url(href: &str, display_text: Option<&str>) -> String {
    if let Some(text) = display_text
        && is_disguised_link(text, href)
    {
        format!("⚠ Goes to: {href}")
    } else {
        href.to_owned()
    }
}

// ============================================================================
// Native COSMIC rendering
// ============================================================================

/// Render a processed `MailContent` and its per-message `MailBodyState` into a
/// native COSMIC `Element`.
pub fn view<'a, M: Clone + 'static>(
    content: &'a MailContent,
    state: &'a MailBodyState,
    map: impl Fn(HtmlMailEvent) -> M + Copy + 'a,
) -> Element<'a, M> {
    let mut root = column![].spacing(10).width(Length::Fill);

    // Top banner: remote image privacy gate + "Open in browser" fallback.
    let show_remote_banner = !content.remote_image_urls.is_empty() && !state.remote_images_loaded;
    if show_remote_banner || content.is_html {
        let mut bar = row![].spacing(8).align_y(Alignment::Center).width(Length::Fill);

        if show_remote_banner {
            let count = content.remote_image_urls.len();
            let notice = if count == 1 {
                "1 remote image blocked".to_owned()
            } else {
                format!("{count} remote images blocked")
            };
            bar = bar.push(text::caption(notice));
            let load_label = if state.loading_remote_images {
                "Loading images…"
            } else {
                "Load images"
            };
            let mut load_btn = button::standard(load_label);
            if !state.loading_remote_images {
                load_btn = load_btn.on_press(map(HtmlMailEvent::LoadRemoteImages));
            }
            bar = bar.push(load_btn);
            if !state.sender_address.trim().is_empty() {
                let mut always_btn = button::text(format!(
                    "Always load images from {}",
                    state.sender_address.trim()
                ));
                if !state.loading_remote_images {
                    always_btn = always_btn.on_press(map(HtmlMailEvent::AlwaysLoadFromSender));
                }
                bar = bar.push(always_btn);
            }
        }

        if content.blocked_tracker_count > 0 {
            let label = if content.blocked_tracker_count == 1 {
                "1 tracker stripped".to_owned()
            } else {
                format!("{} trackers stripped", content.blocked_tracker_count)
            };
            bar = bar.push(text::caption(label));
        }

        bar = bar.push(cosmic::widget::space::horizontal().width(Length::Fill));

        if content.is_html {
            bar = bar.push(
                button::text("Open in browser").on_press(map(HtmlMailEvent::OpenInBrowser)),
            );
        }

        let banner = container(bar)
            .padding([6, 10])
            .width(Length::Fill)
            .class(cosmic::theme::Container::Card);
        root = root.push(banner);
    }

    for block in &content.blocks {
        root = root.push(render_block(block, content, state, map, 0));
    }

    if let Some(hovered) = state.hovered_link.as_deref() {
        let preview = container(text::caption(hovered))
            .padding([4, 8])
            .width(Length::Fill)
            .class(cosmic::theme::Container::Card);
        root = root.push(preview);
    }

    root.into()
}

fn render_block<'a, M: Clone + 'static>(
    block: &'a Block,
    content: &'a MailContent,
    state: &'a MailBodyState,
    map: impl Fn(HtmlMailEvent) -> M + Copy + 'a,
    quote_depth: usize,
) -> Element<'a, M> {
    match block {
        Block::Heading { level, spans } => {
            let size = match level {
                1 => 24.0,
                2 => 20.0,
                3 => 17.5,
                4 => 15.5,
                5 => 14.5,
                _ => 13.5,
            };
            render_rich_spans(spans, size, true, map)
        }
        Block::Paragraph(spans) => render_rich_spans(spans, 14.0, false, map),
        Block::Quote(inner) => {
            let mut inner_col = column![].spacing(6).width(Length::Fill);
            for child in inner {
                inner_col = inner_col.push(render_block(
                    child,
                    content,
                    state,
                    map,
                    quote_depth + 1,
                ));
            }
            let bar_color = match quote_depth % 3 {
                0 => Color::from_rgba(0.28, 0.55, 0.92, 0.8),
                1 => Color::from_rgba(0.30, 0.72, 0.45, 0.8),
                _ => Color::from_rgba(0.62, 0.42, 0.88, 0.8),
            };
            let bar = container(cosmic::widget::space::horizontal().width(Length::Fixed(3.0)))
                .height(Length::Fill)
                .class(cosmic::theme::Container::custom(move |_| {
                    iced_container::Style {
                        background: Some(Background::Color(bar_color)),
                        border: Border {
                            radius: 2.0.into(),
                            ..Border::default()
                        },
                        ..Default::default()
                    }
                }));

            row![bar, container(inner_col).padding([2, 0, 2, 10]).width(Length::Fill)]
                .spacing(4)
                .width(Length::Fill)
                .into()
        }
        Block::List {
            ordered,
            start,
            items,
        } => {
            let mut list_col = column![].spacing(4).width(Length::Fill);
            for (idx, item_blocks) in items.iter().enumerate() {
                let marker = if *ordered {
                    format!("{}.", *start + idx as u64)
                } else {
                    "•".to_owned()
                };
                let mut item_col = column![].spacing(4).width(Length::Fill);
                for b in item_blocks {
                    item_col = item_col.push(render_block(b, content, state, map, quote_depth));
                }
                let item_row = row![
                    text::body(marker).width(Length::Fixed(24.0)),
                    item_col,
                ]
                .spacing(6)
                .align_y(Alignment::Start)
                .width(Length::Fill);
                list_col = list_col.push(item_row);
            }
            container(list_col)
                .padding([0, 0, 0, 8])
                .width(Length::Fill)
                .into()
        }
        Block::CodeBlock(code) => container(
            text::monotext(code.as_str())
                .size(13.0)
                .width(Length::Fill),
        )
        .padding(10)
        .width(Length::Fill)
        .class(cosmic::theme::Container::Card)
        .into(),
        Block::HorizontalRule => divider::horizontal::default().into(),
        Block::Table(table) => render_data_table(table, map),
        Block::Image(img) => render_image_block(img, content, state, map),
    }
}

fn render_data_table<'a, M: Clone + 'static>(
    table: &'a DataTable,
    map: impl Fn(HtmlMailEvent) -> M + Copy + 'a,
) -> Element<'a, M> {
    let mut table_col = column![].spacing(0).width(Length::Fill);

    if !table.headers.is_empty() {
        let mut header_row = row![].spacing(0).width(Length::Fill);
        for cell in &table.headers {
            let cell_el: Element<'a, M> = if cell.is_empty() {
                text::body("").into()
            } else {
                render_rich_spans(cell, 13.5, true, map)
            };
            header_row = header_row.push(
                container(cell_el)
                    .padding([6, 8])
                    .width(Length::FillPortion(1)),
            );
        }
        table_col = table_col
            .push(
                container(header_row)
                    .width(Length::Fill)
                    .class(cosmic::theme::Container::Card),
            )
            .push(divider::horizontal::default());
    }

    for (r_idx, r) in table.rows.iter().enumerate() {
        let mut data_row = row![].spacing(0).width(Length::Fill);
        for cell in r {
            let cell_el: Element<'a, M> = if cell.is_empty() {
                text::body("").into()
            } else {
                render_rich_spans(cell, 13.5, false, map)
            };
            data_row = data_row.push(
                container(cell_el)
                    .padding([6, 8])
                    .width(Length::FillPortion(1)),
            );
        }
        table_col = table_col.push(data_row);
        if r_idx + 1 < table.rows.len() {
            table_col = table_col.push(divider::horizontal::light());
        }
    }

    container(table_col)
        .padding(1)
        .width(Length::Fill)
        .class(cosmic::theme::Container::custom(|_| {
            iced_container::Style {
                border: Border {
                    color: Color::from_rgba(0.5, 0.5, 0.5, 0.3),
                    width: 1.0,
                    radius: 6.0.into(),
                },
                ..Default::default()
            }
        }))
        .into()
}

fn render_image_block<'a, M: Clone + 'static>(
    img: &'a MailImage,
    content: &'a MailContent,
    state: &'a MailBodyState,
    map: impl Fn(HtmlMailEvent) -> M + Copy + 'a,
) -> Element<'a, M> {
    let decoded: Option<&DecodedImage> = match &img.source {
        ImageSource::Cid(cid) => {
            let norm = cid
                .trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_ascii_lowercase();
            content.decoded_inline.get(&norm).or_else(|| {
                norm.split_once('@')
                    .and_then(|(local, _)| content.decoded_inline.get(local))
            })
        }
        ImageSource::DataUri { bytes, .. } => {
            let key = data_uri_cache_key(bytes);
            content.decoded_inline.get(&key)
        }
        ImageSource::Remote(url) => {
            if !state.remote_images_loaded {
                let label = if img.alt.is_empty() {
                    "[Blocked remote image]".to_owned()
                } else {
                    format!("[Blocked image: {}]", img.alt)
                };
                return container(text::caption(label))
                    .padding([4, 8])
                    .class(cosmic::theme::Container::Card)
                    .into();
            }
            match state.remote_images.get(url) {
                Some(Ok(decoded)) => Some(decoded),
                Some(Err(_)) => {
                    let label = if img.alt.is_empty() {
                        "[Failed to load image]".to_owned()
                    } else {
                        format!("[Failed to load: {}]", img.alt)
                    };
                    return text::caption(label).into();
                }
                None => {
                    return text::caption("Loading image…").into();
                }
            }
        }
    };

    let Some(decoded) = decoded else {
        let label = if img.alt.is_empty() {
            "[Image]".to_owned()
        } else {
            format!("[Image: {}]", img.alt)
        };
        return text::caption(label).into();
    };

    let natural_w = img.width.unwrap_or(decoded.width).max(1) as f32;
    let display_w = natural_w.min(MAX_IMAGE_WIDTH);
    let aspect = decoded.height as f32 / decoded.width.max(1) as f32;
    let display_h = (display_w * aspect).max(1.0);

    let img_widget: Element<'a, M> = cosmic_image::Image::new(decoded.handle.clone())
        .width(Length::Fixed(display_w))
        .height(Length::Fixed(display_h))
        .content_fit(ContentFit::Contain)
        .into();

    if let Some(link) = img.link.as_deref() {
        let area = mouse_area(img_widget)
            .on_press(map(HtmlMailEvent::OpenLink(link.to_owned())))
            .on_enter(map(HtmlMailEvent::HoverLink(Some(link.to_owned()))))
            .on_exit(map(HtmlMailEvent::HoverLink(None)))
            .interaction(mouse::Interaction::Pointer);
        tooltip(
            area,
            container(text::caption(link))
                .padding([4, 8])
                .class(cosmic::theme::Container::Tooltip),
            tooltip::Position::Bottom,
        )
        .into()
    } else {
        img_widget
    }
}

fn render_rich_spans<'a, M: Clone + 'static>(
    spans: &'a [TextSpan],
    size: f32,
    force_bold: bool,
    map: impl Fn(HtmlMailEvent) -> M + Copy + 'a,
) -> Element<'a, M> {
    let link_color = Color::from_rgb(0.18, 0.52, 0.95);
    let iced_spans: Vec<Span<'a, String, font::Font>> = spans
        .iter()
        .map(|s| {
            let mut span_font = if s.code {
                font::mono()
            } else {
                font::default()
            };
            if s.bold || force_bold {
                span_font.weight = cosmic::iced::font::Weight::Bold;
            }
            if s.italic {
                span_font.style = cosmic::iced::font::Style::Italic;
            }

            let mut span = Span::new(s.text.as_str())
                .font(span_font)
                .size(Pixels(size))
                .underline(s.underline)
                .strikethrough(s.strikethrough);

            if let Some(link) = &s.link {
                span = span.link(link.clone()).color(link_color).underline(true);
            }
            if s.code {
                span = span.background(Background::Color(Color::from_rgba(0.5, 0.5, 0.5, 0.14)));
            }
            span
        })
        .collect();

    RichMailText::new(iced_spans)
        .size(Pixels(size))
        .width(Length::Fill)
        .on_link_click(move |url| map(HtmlMailEvent::OpenLink(url)))
        .on_link_hover(move |opt| map(HtmlMailEvent::HoverLink(opt)))
        .into()
}

// ============================================================================
// RichMailText custom widget: inline spans + hover URL tooltip + click/hover
// ============================================================================

struct RichMailText<'a, Message> {
    spans: Vec<Span<'a, String, font::Font>>,
    size: Pixels,
    line_height: LineHeight,
    width: Length,
    on_link_click: Option<Box<dyn Fn(String) -> Message + 'a>>,
    on_link_hover: Option<Box<dyn Fn(Option<String>) -> Message + 'a>>,
}

impl<'a, Message> RichMailText<'a, Message> {
    fn new(spans: Vec<Span<'a, String, font::Font>>) -> Self {
        Self {
            spans,
            size: Pixels(14.0),
            line_height: LineHeight::Relative(1.45),
            width: Length::Fill,
            on_link_click: None,
            on_link_hover: None,
        }
    }

    fn size(mut self, size: Pixels) -> Self {
        self.size = size;
        self
    }

    fn width(mut self, width: Length) -> Self {
        self.width = width;
        self
    }

    fn on_link_click(mut self, f: impl Fn(String) -> Message + 'a) -> Self {
        self.on_link_click = Some(Box::new(f));
        self
    }

    fn on_link_hover(mut self, f: impl Fn(Option<String>) -> Message + 'a) -> Self {
        self.on_link_hover = Some(Box::new(f));
        self
    }
}

struct RichState<P: adv_text::Paragraph> {
    spans: Vec<Span<'static, String, P::Font>>,
    hovered_link: Option<usize>,
    cursor_position: Point,
    span_pressed: Option<usize>,
    paragraph: P,
    last_reported_hover: Option<String>,
}

impl<'a, Message> Widget<Message, Theme, cosmic::Renderer> for RichMailText<'a, Message>
where
    Message: 'a,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<RichState<<cosmic::Renderer as adv_text::Renderer>::Paragraph>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(RichState {
            spans: Vec::new(),
            hovered_link: None,
            cursor_position: Point::ORIGIN,
            span_pressed: None,
            paragraph: <cosmic::Renderer as adv_text::Renderer>::Paragraph::default(),
            last_reported_hover: None,
        })
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &cosmic::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree
            .state
            .downcast_mut::<RichState<<cosmic::Renderer as adv_text::Renderer>::Paragraph>>();

        layout::sized(limits, self.width, Length::Shrink, |limits| {
            let bounds = limits.max();
            let font = font::default();

            let make_text = || adv_text::Text {
                content: self.spans.as_slice(),
                bounds,
                size: self.size,
                line_height: self.line_height,
                font,
                align_x: iced_text::Alignment::Default,
                align_y: cosmic::iced::alignment::Vertical::Top,
                shaping: Shaping::Advanced,
                wrapping: Wrapping::WordOrGlyph,
                ellipsize: iced_text::Ellipsize::None,
            };

            if state.spans != self.spans {
                state.paragraph =
                    <cosmic::Renderer as adv_text::Renderer>::Paragraph::with_spans(make_text());
                state.spans = self.spans.iter().cloned().map(Span::to_static).collect();
                state.hovered_link = None;
                state.span_pressed = None;
            } else {
                match state.paragraph.compare(adv_text::Text {
                    content: (),
                    bounds,
                    size: self.size,
                    line_height: self.line_height,
                    font,
                    align_x: iced_text::Alignment::Default,
                    align_y: cosmic::iced::alignment::Vertical::Top,
                    shaping: Shaping::Advanced,
                    wrapping: Wrapping::WordOrGlyph,
                    ellipsize: iced_text::Ellipsize::None,
                }) {
                    adv_text::Difference::None => {}
                    adv_text::Difference::Bounds => state.paragraph.resize(bounds),
                    adv_text::Difference::Shape => {
                        state.paragraph =
                            <cosmic::Renderer as adv_text::Renderer>::Paragraph::with_spans(
                                make_text(),
                            );
                    }
                }
            }
            let _ = renderer;
            state.paragraph.min_bounds()
        })
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut cosmic::Renderer,
        theme: &Theme,
        defaults: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        if !layout.bounds().intersects(viewport) {
            return;
        }
        let state = tree
            .state
            .downcast_ref::<RichState<<cosmic::Renderer as adv_text::Renderer>::Paragraph>>();
        let text_style = <Theme as iced_text::Catalog>::style(theme, &<Theme as iced_text::Catalog>::default());

        for (index, span) in self.spans.iter().enumerate() {
            let is_hovered = state.hovered_link == Some(index) && span.link.is_some();
            if span.highlight.is_some() || span.underline || span.strikethrough || is_hovered {
                let translation = layout.position() - Point::ORIGIN;
                let regions = state.paragraph.span_bounds(index);

                if let Some(highlight) = span.highlight {
                    for bounds in &regions {
                        let rect = Rectangle::new(
                            bounds.position() - Vector::new(span.padding.left, span.padding.top),
                            bounds.size() + Size::new(span.padding.x(), span.padding.y()),
                        );
                        <cosmic::Renderer as cosmic::iced::advanced::Renderer>::fill_quad(
                            renderer,
                            renderer::Quad {
                                bounds: rect + translation,
                                border: highlight.border,
                                ..Default::default()
                            },
                            highlight.background,
                        );
                    }
                }

                if span.underline || span.strikethrough || is_hovered {
                    let size = span.size.unwrap_or(self.size);
                    let line_height = span
                        .line_height
                        .unwrap_or(self.line_height)
                        .to_absolute(size);
                    let color = span
                        .color
                        .or(text_style.color)
                        .unwrap_or(defaults.text_color);
                    let baseline =
                        translation + Vector::new(0.0, size.0 + (line_height.0 - size.0) / 2.0);

                    if span.underline || is_hovered {
                        for bounds in &regions {
                            <cosmic::Renderer as cosmic::iced::advanced::Renderer>::fill_quad(
                                renderer,
                                renderer::Quad {
                                    bounds: Rectangle::new(
                                        bounds.position() + baseline
                                            - Vector::new(0.0, size.0 * 0.08),
                                        Size::new(bounds.width, 1.0),
                                    ),
                                    ..Default::default()
                                },
                                color,
                            );
                        }
                    }

                    if span.strikethrough {
                        for bounds in &regions {
                            <cosmic::Renderer as cosmic::iced::advanced::Renderer>::fill_quad(
                                renderer,
                                renderer::Quad {
                                    bounds: Rectangle::new(
                                        bounds.position() + baseline - Vector::new(0.0, size.0 / 2.0),
                                        Size::new(bounds.width, 1.0),
                                    ),
                                    ..Default::default()
                                },
                                color,
                            );
                        }
                    }
                }
            }
        }

        <cosmic::Renderer as adv_text::Renderer>::fill_paragraph(
            renderer,
            &state.paragraph,
            layout.position(),
            text_style.color.unwrap_or(defaults.text_color),
            *viewport,
        );
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &cosmic::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let state = tree
            .state
            .downcast_mut::<RichState<<cosmic::Renderer as adv_text::Renderer>::Paragraph>>();

        let prev_hovered = state.hovered_link;
        if let Some(pos) = cursor.position_in(layout.bounds()) {
            state.cursor_position = cursor.position().unwrap_or(Point::ORIGIN);
            state.hovered_link = state.paragraph.hit_span(pos).and_then(|idx| {
                self.spans.get(idx)?.link.as_ref().map(|_| idx)
            });
        } else {
            state.hovered_link = None;
        }

        if prev_hovered != state.hovered_link {
            shell.request_redraw();
        }

        let current_hover_label = state.hovered_link.and_then(|idx| {
            let span = self.spans.get(idx)?;
            let url = span.link.as_deref()?;
            Some(format_hover_url(url, Some(span.text.as_ref())))
        });

        if state.last_reported_hover != current_hover_label {
            state.last_reported_hover = current_hover_label.clone();
            if let Some(on_hover) = &self.on_link_hover {
                shell.publish(on_hover(current_hover_label));
            }
        }

        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if state.hovered_link.is_some() {
                    state.span_pressed = state.hovered_link;
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if let Some(pressed) = state.span_pressed
                    && Some(pressed) == state.hovered_link
                    && let Some(url) = self.spans.get(pressed).and_then(|s| s.link.clone())
                    && let Some(on_click) = &self.on_link_click
                {
                    shell.publish(on_click(url));
                }
                state.span_pressed = None;
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &cosmic::Renderer,
    ) -> mouse::Interaction {
        let state = tree
            .state
            .downcast_ref::<RichState<<cosmic::Renderer as adv_text::Renderer>::Paragraph>>();
        if state
            .hovered_link
            .and_then(|idx| self.spans.get(idx)?.link.as_ref())
            .is_some()
        {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::None
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        _layout: Layout<'b>,
        _renderer: &cosmic::Renderer,
        _viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, cosmic::Renderer>> {
        let state = tree
            .state
            .downcast_ref::<RichState<<cosmic::Renderer as adv_text::Renderer>::Paragraph>>();
        let idx = state.hovered_link?;
        let span = self.spans.get(idx)?;
        let url = span.link.as_deref()?;
        let label = format_hover_url(url, Some(span.text.as_ref()));
        Some(overlay::Element::new(Box::new(LinkTooltipOverlay {
            label,
            anchor: state.cursor_position + translation,
            paragraph: <cosmic::Renderer as adv_text::Renderer>::Paragraph::default(),
        })))
    }
}

impl<'a, Message: 'a> From<RichMailText<'a, Message>> for Element<'a, Message> {
    fn from(widget: RichMailText<'a, Message>) -> Self {
        Element::new(widget)
    }
}

struct LinkTooltipOverlay {
    label: String,
    anchor: Point,
    paragraph: <cosmic::Renderer as adv_text::Renderer>::Paragraph,
}

impl<Message> overlay::Overlay<Message, Theme, cosmic::Renderer> for LinkTooltipOverlay {
    fn layout(&mut self, _renderer: &cosmic::Renderer, bounds: Size) -> layout::Node {
        let pad_x = 8.0;
        let pad_y = 5.0;
        let max_w = (bounds.width - 16.0).clamp(120.0, 520.0);

        self.paragraph =
            <cosmic::Renderer as adv_text::Renderer>::Paragraph::with_text(adv_text::Text {
            content: self.label.as_str(),
            bounds: Size::new(max_w - pad_x * 2.0, 120.0),
            size: Pixels(12.0),
            line_height: LineHeight::Relative(1.3),
            font: font::default(),
            align_x: iced_text::Alignment::Left,
            align_y: cosmic::iced::alignment::Vertical::Top,
            shaping: Shaping::Advanced,
            wrapping: Wrapping::WordOrGlyph,
            ellipsize: iced_text::Ellipsize::None,
        });

        let text_size = self.paragraph.min_bounds();
        let box_w = text_size.width + pad_x * 2.0;
        let box_h = text_size.height + pad_y * 2.0;

        let mut x = self.anchor.x + 10.0;
        let mut y = self.anchor.y + 18.0;
        if x + box_w > bounds.width - 8.0 {
            x = (bounds.width - box_w - 8.0).max(8.0);
        }
        if y + box_h > bounds.height - 8.0 {
            y = (self.anchor.y - box_h - 8.0).max(8.0);
        }

        layout::Node::new(Size::new(box_w, box_h)).translate(Vector::new(x, y))
    }

    fn draw(
        &self,
        renderer: &mut cosmic::Renderer,
        _theme: &Theme,
        _defaults: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
    ) {
        let bounds = layout.bounds();
        <cosmic::Renderer as cosmic::iced::advanced::Renderer>::fill_quad(
            renderer,
            renderer::Quad {
                bounds,
                border: Border {
                    color: Color::from_rgba(1.0, 1.0, 1.0, 0.16),
                    width: 1.0,
                    radius: 6.0.into(),
                },
                shadow: Shadow {
                    color: Color::from_rgba(0.0, 0.0, 0.0, 0.25),
                    offset: Vector::new(0.0, 2.0),
                    blur_radius: 6.0,
                },
                ..Default::default()
            },
            Background::Color(Color::from_rgba(0.11, 0.12, 0.14, 0.96)),
        );

        <cosmic::Renderer as adv_text::Renderer>::fill_paragraph(
            renderer,
            &self.paragraph,
            bounds.position() + Vector::new(8.0, 5.0),
            Color::WHITE,
            bounds,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate a valid 2x2 RGBA PNG image in memory for `cid:` tests.
    fn tiny_png_bytes() -> Vec<u8> {
        let img = image::RgbaImage::from_fn(2, 2, |x, y| {
            image::Rgba([(x * 120) as u8, (y * 120) as u8, 200, 255])
        });
        let mut cursor = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut cursor, image::ImageFormat::Png)
            .unwrap();
        cursor.into_inner()
    }

    #[test]
    fn parses_headings_formatting_lists_quotes_rules_and_links() {
        let html = r#"
            <h1>Release Notes</h1>
            <p>See <strong>bold</strong>, <em>italic</em>, <u>underlined</u>, and <a href="https://example.com/release">details</a>.</p>
            <blockquote><p>Quoted message line.</p></blockquote>
            <ol start="3">
                <li>Third item</li>
                <li>Fourth item</li>
            </ol>
            <hr>
        "#;
        let content = MailContent::from_html(html, HashMap::new());
        assert!(content.is_html);
        assert!(matches!(
            content.blocks.first(),
            Some(Block::Heading { level: 1, .. })
        ));
        assert!(content.blocks.iter().any(|b| matches!(b, Block::Quote(_))));
        assert!(content.blocks.iter().any(|b| matches!(
            b,
            Block::List {
                ordered: true,
                start: 3,
                ..
            }
        )));
        assert!(content
            .blocks
            .iter()
            .any(|b| matches!(b, Block::HorizontalRule)));

        let Some(Block::Paragraph(spans)) = content.blocks.get(1) else {
            panic!("expected paragraph");
        };
        assert!(spans.iter().any(|s| s.bold && s.text.contains("bold")));
        assert!(spans.iter().any(|s| s.italic && s.text.contains("italic")));
        assert!(
            spans
                .iter()
                .any(|s| s.underline && s.text.contains("underlined"))
        );
        assert!(spans.iter().any(|s| {
            s.link.as_deref() == Some("https://example.com/release") && s.text == "details"
        }));
    }

    #[test]
    fn flattens_layout_tables_and_preserves_data_tables() {
        let html = r#"
            <table role="presentation" width="600">
                <tr>
                    <td>
                        <table width="100%">
                            <tr><td><h2>Newsletter Header</h2></td></tr>
                            <tr><td><p>Column body text</p></td></tr>
                        </table>
                    </td>
                    <td></td>
                </tr>
            </table>
            <table>
                <thead>
                    <tr><th>Item</th><th>Qty</th></tr>
                </thead>
                <tbody>
                    <tr><td>Apples</td><td>4</td></tr>
                    <tr><td>Pears</td><td>2</td></tr>
                </tbody>
            </table>
        "#;
        let content = MailContent::from_html(html, HashMap::new());
        assert!(
            content
                .blocks
                .iter()
                .any(|b| matches!(b, Block::Heading { level: 2, .. }))
        );
        assert!(
            content
                .blocks
                .iter()
                .any(|b| matches!(b, Block::Paragraph(_)))
        );
        let tables: Vec<&DataTable> = content
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Table(t) => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].headers.len(), 2);
        assert_eq!(tables[0].rows.len(), 2);
    }

    #[test]
    fn linkifies_plain_text_and_parses_quote_bars() {
        let plain = "Hey team,\n\nCheck https://example.org/docs_(v2). Also email mailto:help@example.org!\n\n> Earlier note from https://old.example.com/\n> Second quoted line";
        let content = MailContent::from_plain_text(plain);
        assert!(!content.is_html);

        let Some(Block::Paragraph(spans)) = content.blocks.get(1) else {
            panic!("expected second paragraph, got {:?}", content.blocks);
        };
        let links: Vec<&str> = spans.iter().filter_map(|s| s.link.as_deref()).collect();
        assert_eq!(
            links,
            vec![
                "https://example.org/docs_(v2)",
                "mailto:help@example.org"
            ]
        );

        let Some(Block::Quote(quoted)) = content.blocks.get(2) else {
            panic!("expected quote block");
        };
        let Block::Paragraph(q_spans) = &quoted[0] else {
            panic!("expected paragraph inside quote");
        };
        assert!(
            q_spans
                .iter()
                .any(|s| s.link.as_deref() == Some("https://old.example.com/"))
        );
    }

    #[test]
    fn detects_disguised_links_for_hover_visibility() {
        assert!(is_disguised_link(
            "https://bank.example.com/login",
            "https://phish.invalid/steal"
        ));
        assert!(!is_disguised_link(
            "https://bank.example.com/login",
            "https://bank.example.com/reset"
        ));
        assert!(!is_disguised_link("Click here", "https://example.com"));
        let formatted = format_hover_url(
            "https://phish.invalid/steal",
            Some("https://bank.example.com/login"),
        );
        assert!(formatted.contains("https://phish.invalid/steal"));
        assert!(formatted.starts_with("⚠"));
    }

    #[test]
    fn resolves_cid_images_from_mime_and_keeps_remote_images_blocked_list() {
        let png = tiny_png_bytes();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
        let raw_eml = format!(
            "From: sender@example.com\r\n\
             To: user@example.com\r\n\
             Subject: Inline image test\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/related; boundary=\"REL\"\r\n\
             \r\n\
             --REL\r\n\
             Content-Type: text/html; charset=utf-8\r\n\
             \r\n\
             <p>Logo:</p>\
             <img src=\"cid:logo@macmail.test\" alt=\"Logo\">\
             <img src=\"https://cdn.example.com/banner.png\" width=\"600\" height=\"200\" alt=\"Banner\">\r\n\
             --REL\r\n\
             Content-Type: image/png\r\n\
             Content-Transfer-Encoding: base64\r\n\
             Content-ID: <logo@macmail.test>\r\n\
             \r\n\
             {b64}\r\n\
             --REL--\r\n"
        );

        let content = MailContent::from_raw_mime(raw_eml.as_bytes());
        assert!(content.decoded_inline.contains_key("logo@macmail.test"));
        let decoded = &content.decoded_inline["logo@macmail.test"];
        assert_eq!((decoded.width, decoded.height), (2, 2));
        assert_eq!(
            content.remote_image_urls,
            vec!["https://cdn.example.com/banner.png".to_owned()]
        );
    }

    #[test]
    fn rich_mail_text_preserves_hover_state_in_tree_across_rebuilds() {
        let make_widget = || -> Element<'static, ()> {
            RichMailText::new(vec![
                Span::new("See "),
                Span::new("Click here").link("https://example.com/actual".to_owned()),
            ])
            .into()
        };

        let first = make_widget();
        let mut tree = Tree::new(&first);
        {
            let state = tree
                .state
                .downcast_mut::<RichState<<cosmic::Renderer as adv_text::Renderer>::Paragraph>>();
            assert_eq!(state.hovered_link, None);
            assert_eq!(state.cursor_position, Point::ORIGIN);
            state.hovered_link = Some(1);
            state.cursor_position = Point::new(42.0, 24.0);
        }

        // Simulate `Mail::view()` rebuilding the widget tree on `HoverLink`.
        let rebuilt = make_widget();
        tree.diff(rebuilt);

        let state = tree
            .state
            .downcast_ref::<RichState<<cosmic::Renderer as adv_text::Renderer>::Paragraph>>();
        assert_eq!(state.hovered_link, Some(1));
        assert_eq!(state.cursor_position, Point::new(42.0, 24.0));
    }
}
