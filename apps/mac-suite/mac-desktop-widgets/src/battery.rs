// SPDX-License-Identifier: GPL-3.0-only
//! Battery devices (laptop battery plus Bluetooth peripherals) from UPower.

use zbus::{Connection, proxy::CacheProperties, zvariant::OwnedObjectPath};

const UPOWER: &str = "org.freedesktop.UPower";
const UPOWER_PATH: &str = "/org/freedesktop/UPower";
const DEVICE_IFACE: &str = "org.freedesktop.UPower.Device";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    Battery,
    Ups,
    Mouse,
    Keyboard,
    Phone,
    Tablet,
    Gamepad,
    Headset,
    Speakers,
    Other,
}

impl DeviceKind {
    /// Maps the UPower `Type` property.
    pub fn from_upower(value: u32) -> Self {
        match value {
            2 => Self::Battery,
            3 => Self::Ups,
            5 | 14 => Self::Mouse,
            6 => Self::Keyboard,
            7 | 8 => Self::Phone,
            10 => Self::Tablet,
            12 => Self::Gamepad,
            17 | 19 => Self::Headset,
            18 | 21 => Self::Speakers,
            _ => Self::Other,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Battery => "Battery",
            Self::Ups => "UPS",
            Self::Mouse => "Mouse",
            Self::Keyboard => "Keyboard",
            Self::Phone => "Phone",
            Self::Tablet => "Tablet",
            Self::Gamepad => "Controller",
            Self::Headset => "Headphones",
            Self::Speakers => "Speakers",
            Self::Other => "Device",
        }
    }

    pub fn icon_name(self) -> &'static str {
        match self {
            Self::Battery | Self::Ups | Self::Other => "battery-symbolic",
            Self::Mouse => "input-mouse-symbolic",
            Self::Keyboard => "input-keyboard-symbolic",
            Self::Phone => "phone-symbolic",
            Self::Tablet => "input-tablet-symbolic",
            Self::Gamepad => "input-gaming-symbolic",
            Self::Headset => "audio-headphones-symbolic",
            Self::Speakers => "audio-speakers-symbolic",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChargeState {
    Charging,
    Discharging,
    Full,
    Unknown,
}

impl ChargeState {
    /// Maps the UPower `State` property.
    pub fn from_upower(value: u32) -> Self {
        match value {
            1 | 5 => Self::Charging,
            2 | 3 | 6 => Self::Discharging,
            4 => Self::Full,
            _ => Self::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Charging => "Charging",
            Self::Discharging => "On battery",
            Self::Full => "Fully charged",
            Self::Unknown => "",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeviceInfo {
    pub name: String,
    pub kind: DeviceKind,
    pub percent: u8,
    pub state: ChargeState,
}

#[derive(Clone, Debug, Default)]
pub enum BatteryState {
    #[default]
    Loading,
    Ready(Vec<DeviceInfo>),
}

/// Raw UPower properties of one device.
#[derive(Clone, Debug, Default)]
pub struct RawDevice {
    pub kind: u32,
    pub state: u32,
    pub percentage: f64,
    pub is_present: bool,
    pub power_supply: bool,
    pub model: String,
}

/// Turns raw properties into a displayable device, skipping line power,
/// monitors, absent batteries and other non-battery entries.
pub fn to_device(raw: &RawDevice) -> Option<DeviceInfo> {
    // 1 line power, 4 monitor, 15 modem, 16 network, 20 video
    if matches!(raw.kind, 0 | 1 | 4 | 15 | 16 | 20) || !raw.is_present {
        return None;
    }
    let kind = DeviceKind::from_upower(raw.kind);
    let model = raw.model.trim();
    let name = if kind == DeviceKind::Battery && raw.power_supply {
        "Battery".to_owned()
    } else if model.is_empty() {
        kind.label().to_owned()
    } else {
        model.to_owned()
    };
    Some(DeviceInfo {
        name,
        kind,
        percent: raw.percentage.round().clamp(0.0, 100.0) as u8,
        state: ChargeState::from_upower(raw.state),
    })
}

/// Internal battery first, then by name.
pub fn sort_devices(devices: &mut [DeviceInfo]) {
    devices.sort_by(|a, b| {
        let rank = |d: &DeviceInfo| u8::from(d.kind != DeviceKind::Battery);
        (rank(a), &a.name).cmp(&(rank(b), &b.name))
    });
}

/// Ring colour bucket: 0 normal, 1 low (<=20), 2 critical (<=10).
pub fn level(percent: u8, state: ChargeState) -> u8 {
    if state == ChargeState::Charging || percent > 20 {
        0
    } else if percent > 10 {
        1
    } else {
        2
    }
}

/// Queries UPower on the system bus. A missing UPower service yields no devices.
pub async fn fetch() -> Vec<DeviceInfo> {
    match fetch_inner().await {
        Ok(devices) => devices,
        Err(err) => {
            tracing::debug!(%err, "UPower unavailable");
            Vec::new()
        }
    }
}

async fn device_proxy<'a>(
    conn: &Connection,
    path: &'a str,
) -> zbus::Result<zbus::Proxy<'a>> {
    zbus::proxy::Builder::new(conn)
        .destination(UPOWER)?
        .path(path)?
        .interface(DEVICE_IFACE)?
        .cache_properties(CacheProperties::No)
        .build()
        .await
}

async fn fetch_inner() -> zbus::Result<Vec<DeviceInfo>> {
    let conn = Connection::system().await?;
    let upower = zbus::proxy::Builder::<zbus::Proxy>::new(&conn)
        .destination(UPOWER)?
        .path(UPOWER_PATH)?
        .interface(UPOWER)?
        .cache_properties(CacheProperties::No)
        .build()
        .await?;
    let paths: Vec<OwnedObjectPath> = upower.call("EnumerateDevices", &()).await?;
    let mut devices = Vec::new();
    for path in &paths {
        let Ok(dev) = device_proxy(&conn, path.as_str()).await else {
            continue;
        };
        let raw = RawDevice {
            kind: dev.get_property("Type").await.unwrap_or(0),
            state: dev.get_property("State").await.unwrap_or(0),
            percentage: dev.get_property("Percentage").await.unwrap_or(0.0),
            is_present: dev.get_property("IsPresent").await.unwrap_or(false),
            power_supply: dev.get_property("PowerSupply").await.unwrap_or(false),
            model: dev.get_property("Model").await.unwrap_or_default(),
        };
        if let Some(d) = to_device(&raw) {
            devices.push(d);
        }
    }
    sort_devices(&mut devices);
    Ok(devices)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(kind: u32, pct: f64) -> RawDevice {
        RawDevice {
            kind,
            state: 2,
            percentage: pct,
            is_present: true,
            power_supply: false,
            model: String::new(),
        }
    }

    #[test]
    fn skips_line_power_monitor_and_absent() {
        assert!(to_device(&raw(1, 0.0)).is_none());
        assert!(to_device(&raw(4, 50.0)).is_none());
        let mut absent = raw(2, 50.0);
        absent.is_present = false;
        assert!(to_device(&absent).is_none());
    }

    #[test]
    fn names_and_percent() {
        let mut laptop = raw(2, 87.6);
        laptop.power_supply = true;
        laptop.model = "BAT0 junk".into();
        let d = to_device(&laptop).unwrap();
        assert_eq!((d.name.as_str(), d.percent), ("Battery", 88));
        let mut mouse = raw(5, 130.0);
        mouse.model = " MX Master ".into();
        let m = to_device(&mouse).unwrap();
        assert_eq!((m.name.as_str(), m.percent, m.kind), ("MX Master", 100, DeviceKind::Mouse));
        assert_eq!(to_device(&raw(6, 10.0)).unwrap().name, "Keyboard");
    }

    #[test]
    fn states_levels_and_sorting() {
        assert_eq!(ChargeState::from_upower(1), ChargeState::Charging);
        assert_eq!(ChargeState::from_upower(4), ChargeState::Full);
        assert_eq!(ChargeState::from_upower(0), ChargeState::Unknown);
        assert_eq!(level(50, ChargeState::Discharging), 0);
        assert_eq!(level(15, ChargeState::Discharging), 1);
        assert_eq!(level(5, ChargeState::Discharging), 2);
        assert_eq!(level(5, ChargeState::Charging), 0);

        let mk = |name: &str, kind| DeviceInfo {
            name: name.into(),
            kind,
            percent: 50,
            state: ChargeState::Unknown,
        };
        let mut v = vec![mk("Zed", DeviceKind::Mouse), mk("Battery", DeviceKind::Battery), mk("Abc", DeviceKind::Keyboard)];
        sort_devices(&mut v);
        let names: Vec<_> = v.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["Battery", "Abc", "Zed"]);
    }
}
