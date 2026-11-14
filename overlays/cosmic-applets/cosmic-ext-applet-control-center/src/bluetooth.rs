// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashMap;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

const BLUEZ_SERVICE: &str = "org.bluez";

#[derive(Debug, Clone)]
pub struct BtDevice {
    pub name: String,
    pub object_path: String,
    pub connected: bool,
}

#[derive(Debug, Clone, Default)]
pub struct BluetoothState {
    pub has_adapter: bool,
    pub adapter_path: Option<String>,
    pub powered: bool,
    pub devices: Vec<BtDevice>,
}

type ManagedObjects =
    HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

#[zbus::proxy(
    interface = "org.freedesktop.DBus.ObjectManager",
    default_service = "org.bluez",
    default_path = "/"
)]
trait BluezObjectManager {
    fn get_managed_objects(&self) -> zbus::Result<ManagedObjects>;
}

#[zbus::proxy(interface = "org.bluez.Adapter1", default_service = "org.bluez")]
trait BluezAdapter1 {
    #[zbus(property)]
    fn set_powered(&self, value: bool) -> zbus::Result<()>;
}

#[zbus::proxy(interface = "org.bluez.Device1", default_service = "org.bluez")]
trait BluezDevice1 {
    fn connect(&self) -> zbus::Result<()>;
    fn disconnect(&self) -> zbus::Result<()>;
}

pub async fn fetch_bluetooth_state() -> BluetoothState {
    let Ok(conn) = zbus::Connection::system().await else {
        return BluetoothState::default();
    };
    let Ok(om) = BluezObjectManagerProxy::new(&conn).await else {
        return BluetoothState::default();
    };
    let Ok(objects) = om.get_managed_objects().await else {
        return BluetoothState::default();
    };

    let mut adapter_path: Option<String> = None;
    let mut powered = false;
    let mut devices = Vec::new();

    for (path, ifaces) in objects {
        if let Some(adapter) = ifaces.get("org.bluez.Adapter1")
            && adapter_path.is_none()
        {
            adapter_path = Some(path.as_str().to_string());
            powered = adapter
                .get("Powered")
                .and_then(|v| bool::try_from(v.clone()).ok())
                .unwrap_or(false);
        }
        if let Some(dev) = ifaces.get("org.bluez.Device1") {
            let paired = dev
                .get("Paired")
                .and_then(|v| bool::try_from(v.clone()).ok())
                .unwrap_or(false);
            let connected = dev
                .get("Connected")
                .and_then(|v| bool::try_from(v.clone()).ok())
                .unwrap_or(false);
            if !paired && !connected {
                continue;
            }
            let name = dev
                .get("Alias")
                .and_then(|v| String::try_from(v.clone()).ok())
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    dev.get("Name")
                        .and_then(|v| String::try_from(v.clone()).ok())
                        .filter(|s| !s.is_empty())
                })
                .unwrap_or_else(|| path.as_str().to_string());
            devices.push(BtDevice {
                name,
                object_path: path.as_str().to_string(),
                connected,
            });
        }
    }

    devices.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    BluetoothState {
        has_adapter: adapter_path.is_some(),
        adapter_path,
        powered,
        devices,
    }
}

pub async fn set_bluetooth_powered(adapter_path: Option<String>, enabled: bool) -> BluetoothState {
    if let (Some(path), Ok(conn)) = (adapter_path, zbus::Connection::system().await)
        && let Ok(builder) = BluezAdapter1Proxy::builder(&conn).destination(BLUEZ_SERVICE)
        && let Ok(builder) = builder.path(path)
        && let Ok(proxy) = builder.build().await
    {
        let _ = proxy.set_powered(enabled).await;
    }
    fetch_bluetooth_state().await
}

pub async fn toggle_device_connection(object_path: String, connect: bool) -> BluetoothState {
    if let Ok(conn) = zbus::Connection::system().await
        && let Ok(builder) = BluezDevice1Proxy::builder(&conn).destination(BLUEZ_SERVICE)
        && let Ok(builder) = builder.path(object_path)
        && let Ok(proxy) = builder.build().await
    {
        if connect {
            let _ = proxy.connect().await;
        } else {
            let _ = proxy.disconnect().await;
        }
    }
    fetch_bluetooth_state().await
}
