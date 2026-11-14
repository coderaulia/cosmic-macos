// SPDX-License-Identifier: GPL-3.0-only

use zbus::proxy;

#[proxy(interface = "org.gnome.SessionManager", assume_defaults = true)]
pub trait SessionManager {
    fn logout(&self, mode: u32) -> zbus::Result<()>;
}
