// SPDX-License-Identifier: GPL-3.0-only

//! Wayland selection owner using `wl_clipboard_rs::copy::prepare_copy_multi`.

use wl_clipboard_rs::copy::{ClipboardType, MimeSource, MimeType, Options, Seat, Source};

use crate::filter::SELF_OWNER_MIME;
use crate::model::{
    ClipboardItem, GNOME_COPIED_FILES_MIME, HTML_MIME, ItemKind, URI_LIST_MIME,
    format_gnome_copied_files, format_uri_list,
};
use crate::store::ClipboardStore;

#[derive(Debug, thiserror::Error)]
pub enum OwnerError {
    #[error("item has no copyable payload")]
    EmptyPayload,
    #[error("store error: {0}")]
    Store(#[from] crate::store::StoreError),
    #[error("clipboard copy error: {0}")]
    Copy(#[from] wl_clipboard_rs::copy::Error),
}

/// Builds the list of `MimeSource`s to offer for `item`.
pub fn build_mime_sources(
    store: &ClipboardStore,
    item: &ClipboardItem,
    plain_only: bool,
) -> Result<Vec<MimeSource>, OwnerError> {
    if item.blob_refs.oversized {
        return Err(OwnerError::EmptyPayload);
    }

    let mut sources = Vec::new();

    if plain_only {
        let Some(text) = store.read_plain_text(item)? else {
            return Err(OwnerError::EmptyPayload);
        };
        sources.push(MimeSource {
            source: Source::Bytes(text.into_bytes().into_boxed_slice()),
            mime_type: MimeType::Text,
        });
    } else {
        match item.kind {
            ItemKind::Text | ItemKind::Link => {
                let bytes = store.read_blob(&item.blob_refs.primary)?;
                sources.push(MimeSource {
                    source: Source::Bytes(bytes.into_boxed_slice()),
                    mime_type: MimeType::Text,
                });
            }
            ItemKind::RichText => {
                let html_bytes = store.read_blob(&item.blob_refs.primary)?;
                sources.push(MimeSource {
                    source: Source::Bytes(html_bytes.into_boxed_slice()),
                    mime_type: MimeType::Specific(HTML_MIME.to_string()),
                });
                if let Some(plain) = store.read_plain_text(item)? {
                    sources.push(MimeSource {
                        source: Source::Bytes(plain.into_bytes().into_boxed_slice()),
                        mime_type: MimeType::Text,
                    });
                }
            }
            ItemKind::Image => {
                let png_bytes = store.read_blob(&item.blob_refs.primary)?;
                sources.push(MimeSource {
                    source: Source::Bytes(png_bytes.into_boxed_slice()),
                    mime_type: MimeType::Specific("image/png".to_string()),
                });
            }
            ItemKind::Files => {
                let Some(files) = item.files_meta.as_ref() else {
                    return Err(OwnerError::EmptyPayload);
                };
                let uri_list = format_uri_list(files);
                let gnome = format_gnome_copied_files(files);
                sources.push(MimeSource {
                    source: Source::Bytes(uri_list.into_bytes().into_boxed_slice()),
                    mime_type: MimeType::Specific(URI_LIST_MIME.to_string()),
                });
                sources.push(MimeSource {
                    source: Source::Bytes(gnome.into_bytes().into_boxed_slice()),
                    mime_type: MimeType::Specific(GNOME_COPIED_FILES_MIME.to_string()),
                });
            }
        }
    }

    // Mark our own selection so the watcher ignores the event it triggers.
    sources.push(MimeSource {
        source: Source::Bytes(b"1"[..].into()),
        mime_type: MimeType::Specific(SELF_OWNER_MIME.to_string()),
    });

    Ok(sources)
}

/// Offers `item` on the Wayland clipboard and spawns a background serving thread
/// that stays alive until another client takes ownership of the selection.
pub fn offer_item(
    store: &ClipboardStore,
    item: &ClipboardItem,
    plain_only: bool,
) -> Result<(), OwnerError> {
    let sources = build_mime_sources(store, item, plain_only)?;
    let mut opts = Options::new();
    opts.clipboard(ClipboardType::Regular);
    opts.seat(Seat::All);
    opts.foreground(true);
    let prepared = opts.prepare_copy_multi(sources)?;

    let _ = std::thread::Builder::new()
        .name("clipboard-owner".to_string())
        .spawn(move || {
            if let Err(err) = prepared.serve() {
                tracing::debug!("clipboard owner serving ended: {err}");
            }
        });

    Ok(())
}
