// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashMap;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

const NM_SERVICE: &str = "org.freedesktop.NetworkManager";
const NM_DEVICE_TYPE_WIFI: u32 = 2;
const NM_DEVICE_TYPE_ETHERNET: u32 = 1;

#[derive(Debug, Clone)]
pub struct WifiNetwork {
    pub ssid: String,
    pub strength: u8,
    pub active: bool,
    pub ap_path: String,
    pub device_path: String,
}

#[derive(Debug, Clone, Default)]
pub struct NetworkState {
    pub wifi_present: bool,
    pub wifi_enabled: bool,
    pub active_ssid: Option<String>,
    pub wired_connected: bool,
    pub networks: Vec<WifiNetwork>,
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait NetworkManager {
    fn get_devices(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    fn activate_connection(
        &self,
        connection: &zbus::zvariant::ObjectPath<'_>,
        device: &zbus::zvariant::ObjectPath<'_>,
        specific_object: &zbus::zvariant::ObjectPath<'_>,
    ) -> zbus::Result<OwnedObjectPath>;

    #[zbus(property)]
    fn wireless_enabled(&self) -> zbus::Result<bool>;

    #[zbus(property)]
    fn set_wireless_enabled(&self, value: bool) -> zbus::Result<()>;

    #[zbus(property)]
    fn active_connections(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Device",
    default_service = "org.freedesktop.NetworkManager"
)]
trait NmDevice {
    #[zbus(property)]
    fn device_type(&self) -> zbus::Result<u32>;

    #[zbus(property)]
    fn state(&self) -> zbus::Result<u32>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Device.Wireless",
    default_service = "org.freedesktop.NetworkManager"
)]
trait NmWireless {
    fn get_access_points(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    #[zbus(property)]
    fn active_access_point(&self) -> zbus::Result<OwnedObjectPath>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.AccessPoint",
    default_service = "org.freedesktop.NetworkManager"
)]
trait NmAccessPoint {
    #[zbus(property)]
    fn ssid(&self) -> zbus::Result<Vec<u8>>;

    #[zbus(property)]
    fn strength(&self) -> zbus::Result<u8>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Settings",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager/Settings"
)]
trait NmSettings {
    fn list_connections(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Settings.Connection",
    default_service = "org.freedesktop.NetworkManager"
)]
trait NmConnection {
    fn get_settings(&self) -> zbus::Result<HashMap<String, HashMap<String, OwnedValue>>>;
}

pub async fn fetch_network_state() -> Option<NetworkState> {
    let conn = zbus::Connection::system().await.ok()?;
    let nm = NetworkManagerProxy::new(&conn).await.ok()?;
    let wifi_enabled = nm.wireless_enabled().await.unwrap_or(false);
    let devices = nm.get_devices().await.unwrap_or_default();

    let mut wifi_present = false;
    let mut wired_connected = false;
    let mut active_ssid: Option<String> = None;
    let mut by_ssid: HashMap<String, WifiNetwork> = HashMap::new();

    for dev_path in devices {
        let Ok(dev) = NmDeviceProxy::builder(&conn)
            .destination(NM_SERVICE)
            .ok()?
            .path(dev_path.as_ref())
            .ok()?
            .build()
            .await
        else {
            continue;
        };
        let dtype = dev.device_type().await.unwrap_or(0);
        if dtype == NM_DEVICE_TYPE_ETHERNET {
            if dev.state().await.unwrap_or(0) == 100 {
                wired_connected = true;
            }
            continue;
        }
        if dtype != NM_DEVICE_TYPE_WIFI {
            continue;
        }
        wifi_present = true;

        let Ok(wireless) = NmWirelessProxy::builder(&conn)
            .destination(NM_SERVICE)
            .ok()?
            .path(dev_path.as_ref())
            .ok()?
            .build()
            .await
        else {
            continue;
        };
        let active_ap = wireless
            .active_access_point()
            .await
            .ok()
            .map(|p| p.as_str().to_string());

        for ap_path in wireless.get_access_points().await.unwrap_or_default() {
            let Ok(ap) = NmAccessPointProxy::builder(&conn)
                .destination(NM_SERVICE)
                .ok()?
                .path(ap_path.as_ref())
                .ok()?
                .build()
                .await
            else {
                continue;
            };
            let ssid_bytes = ap.ssid().await.unwrap_or_default();
            let ssid = String::from_utf8_lossy(&ssid_bytes).trim().to_string();
            if ssid.is_empty() {
                continue;
            }
            let strength = ap.strength().await.unwrap_or(0);
            let is_active = active_ap.as_deref() == Some(ap_path.as_str());
            if is_active {
                active_ssid = Some(ssid.clone());
            }
            let entry = by_ssid.entry(ssid.clone()).or_insert_with(|| WifiNetwork {
                ssid,
                strength,
                active: is_active,
                ap_path: ap_path.as_str().to_string(),
                device_path: dev_path.as_str().to_string(),
            });
            if is_active {
                entry.active = true;
            }
            if strength > entry.strength {
                entry.strength = strength;
                entry.ap_path = ap_path.as_str().to_string();
            }
        }
    }

    let mut networks: Vec<WifiNetwork> = by_ssid.into_values().collect();
    networks.sort_by(|a, b| {
        b.active
            .cmp(&a.active)
            .then_with(|| b.strength.cmp(&a.strength))
            .then_with(|| a.ssid.cmp(&b.ssid))
    });

    Some(NetworkState {
        wifi_present,
        wifi_enabled,
        active_ssid,
        wired_connected,
        networks,
    })
}

pub async fn set_wifi_enabled(enabled: bool) -> Option<NetworkState> {
    let conn = zbus::Connection::system().await.ok()?;
    let nm = NetworkManagerProxy::new(&conn).await.ok()?;
    let _ = nm.set_wireless_enabled(enabled).await;
    fetch_network_state().await
}

pub async fn connect_saved_network(net: WifiNetwork) -> Result<Option<NetworkState>, String> {
    let conn = zbus::Connection::system().await.map_err(|e| e.to_string())?;
    let settings = NmSettingsProxy::new(&conn).await.map_err(|e| e.to_string())?;
    let connections = settings.list_connections().await.map_err(|e| e.to_string())?;

    let mut matched_conn: Option<OwnedObjectPath> = None;
    for c_path in connections {
        let Ok(c_proxy) = NmConnectionProxy::builder(&conn)
            .destination(NM_SERVICE)
            .map_err(|e| e.to_string())?
            .path(c_path.as_ref())
            .map_err(|e| e.to_string())?
            .build()
            .await
        else {
            continue;
        };
        if let Ok(map) = c_proxy.get_settings().await
            && let Some(wifi_sec) = map.get("802-11-wireless")
            && let Some(val) = wifi_sec.get("ssid")
            && let Ok(bytes) = Vec::<u8>::try_from(val.clone())
            && String::from_utf8_lossy(&bytes) == net.ssid
        {
            matched_conn = Some(c_path);
            break;
        }
    }

    let Some(conn_path) = matched_conn else {
        return Err("auth-required".to_string());
    };
    let nm = NetworkManagerProxy::new(&conn).await.map_err(|e| e.to_string())?;
    let dev_obj = zbus::zvariant::ObjectPath::try_from(net.device_path.as_str())
        .map_err(|e| e.to_string())?;
    let ap_obj = zbus::zvariant::ObjectPath::try_from(net.ap_path.as_str())
        .map_err(|e| e.to_string())?;
    let conn_obj = conn_path.as_ref();
    nm.activate_connection(&conn_obj, &dev_obj, &ap_obj)
        .await
        .map_err(|e| e.to_string())?;
    Ok(fetch_network_state().await)
}
