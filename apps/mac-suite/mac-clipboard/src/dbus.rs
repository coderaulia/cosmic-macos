// SPDX-License-Identifier: GPL-3.0-only

//! D-Bus protocol definitions and client helpers for `dev.asw.MacClipboard`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use zbus::zvariant::{OwnedValue, Type};
use zbus::{Connection, proxy};

pub const BUS_NAME: &str = "dev.asw.MacClipboard";
pub const OBJECT_PATH: &str = "/dev/asw/MacClipboard";

pub const OVERLAY_BUS_NAME: &str = "dev.asw.MacClipboard.Overlay";
pub const OVERLAY_OBJECT_PATH: &str = "/dev/asw/MacClipboard/Overlay";

/// Wire representation returned by `dev.asw.MacClipboard.List`.
/// Signature: `(ssssxtbs)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DbusListItem {
    pub id: String,
    pub kind: String,
    pub preview: String,
    pub thumbnail_path: String,
    pub size_bytes: u64,
    pub created: i64,
    pub pinned: bool,
    pub source_app: String,
}

#[proxy(
    interface = "dev.asw.MacClipboard",
    default_service = "dev.asw.MacClipboard",
    default_path = "/dev/asw/MacClipboard"
)]
pub trait MacClipboard {
    fn list(
        &self,
        filter: &str,
        query: &str,
        limit: u32,
    ) -> zbus::Result<Vec<DbusListItem>>;

    fn get(&self, id: &str) -> zbus::Result<HashMap<String, OwnedValue>>;

    fn activate(&self, id: &str, plain: bool, paste: bool) -> zbus::Result<bool>;

    fn pin(&self, id: &str, pinned: bool) -> zbus::Result<()>;

    fn remove(&self, id: &str) -> zbus::Result<()>;

    fn clear(&self, keep_pinned: bool) -> zbus::Result<u32>;

    fn set_paused(&self, paused: bool) -> zbus::Result<()>;

    fn toggle(&self) -> zbus::Result<()>;

    fn show(&self) -> zbus::Result<()>;

    fn hide(&self) -> zbus::Result<()>;

    #[zbus(signal)]
    fn changed(&self, revision: u64) -> zbus::Result<()>;

    #[zbus(property)]
    fn paused(&self) -> zbus::Result<bool>;

    #[zbus(property)]
    fn count(&self) -> zbus::Result<u32>;
}

#[proxy(
    interface = "dev.asw.MacClipboard.Overlay",
    default_service = "dev.asw.MacClipboard.Overlay",
    default_path = "/dev/asw/MacClipboard/Overlay"
)]
pub trait MacClipboardOverlay {
    fn close(&self) -> zbus::Result<()>;
    fn ping(&self) -> zbus::Result<bool>;
}

/// Checks if `dev.asw.MacClipboard` is currently registered on the session bus.
pub async fn is_daemon_running(conn: &Connection) -> bool {
    let Ok(dbus) = zbus::fdo::DBusProxy::new(conn).await else {
        return false;
    };
    let Ok(name) = zbus::names::BusName::try_from(BUS_NAME) else {
        return false;
    };
    dbus.name_has_owner(name).await.unwrap_or(false)
}

/// Checks if the quick-paste overlay is currently open.
pub async fn is_overlay_open(conn: &Connection) -> bool {
    let Ok(dbus) = zbus::fdo::DBusProxy::new(conn).await else {
        return false;
    };
    let Ok(name) = zbus::names::BusName::try_from(OVERLAY_BUS_NAME) else {
        return false;
    };
    dbus.name_has_owner(name).await.unwrap_or(false)
}

/// Closes an open quick-paste overlay if present. Returns true if one was closed.
pub async fn close_overlay_if_open(conn: &Connection) -> bool {
    if !is_overlay_open(conn).await {
        return false;
    }
    let Ok(proxy) = MacClipboardOverlayProxy::new(conn).await else {
        return false;
    };
    proxy.close().await.is_ok()
}

/// Ensures the daemon is running on the session bus, spawning `mac-clipboard daemon`
/// in the background if necessary.
pub async fn ensure_daemon_running(conn: &Connection) -> Result<(), String> {
    if is_daemon_running(conn).await {
        return Ok(());
    }

    // Try D-Bus activation first (in case the .service file is installed), then fallback to self-spawn.
    if let Ok(proxy) = MacClipboardProxy::new(conn).await
        && proxy.count().await.is_ok()
    {
        return Ok(());
    }

    let exe: PathBuf =
        std::env::current_exe().map_err(|e| format!("failed to locate current exe: {e}"))?;

    Command::new(exe)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to spawn mac-clipboard daemon: {e}"))?;

    for _ in 0..40 {
        if is_daemon_running(conn).await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    Err("timed out waiting for dev.asw.MacClipboard on session D-Bus".to_string())
}
