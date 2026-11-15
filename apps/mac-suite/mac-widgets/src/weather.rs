// SPDX-License-Identifier: GPL-3.0-only
//! Weather from Open-Meteo with a 30 minute on-disk cache.

use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

const CACHE_MAX_AGE_SECS: u64 = 30 * 60;
const ENDPOINT: &str = "https://api.open-meteo.com/v1/forecast";

#[derive(Clone, Debug, PartialEq)]
pub struct Location {
    pub city: String,
    pub lat: f64,
    pub lon: f64,
}

impl Location {
    pub fn parse(text: &str) -> Option<Self> {
        let (mut city, mut lat, mut lon) = (None, None, None);
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let v = v.trim();
            match k.trim() {
                "city" => city = Some(v.to_owned()),
                "lat" => lat = v.parse::<f64>().ok(),
                "lon" => lon = v.parse::<f64>().ok(),
                _ => {}
            }
        }
        Some(Self {
            city: city.unwrap_or_default(),
            lat: lat.filter(|l| (-90.0..=90.0).contains(l))?,
            lon: lon.filter(|l| (-180.0..=180.0).contains(l))?,
        })
    }

    /// Reads `~/.config/mac-suite/location.conf`; `None` when absent or invalid.
    pub fn load() -> Option<Self> {
        let dir = crate::settings::config_dir()?;
        Self::parse(&std::fs::read_to_string(dir.join("location.conf")).ok()?)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Day {
    /// ISO date, `YYYY-MM-DD`.
    pub date: String,
    pub code: u8,
    pub min_c: f64,
    pub max_c: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Forecast {
    pub city: String,
    pub lat: f64,
    pub lon: f64,
    /// Unix seconds when fetched.
    pub fetched_at: u64,
    pub temp_c: f64,
    pub code: u8,
    pub days: Vec<Day>,
}

#[derive(Deserialize)]
struct ApiResponse {
    current: ApiCurrent,
    daily: ApiDaily,
}

#[derive(Deserialize)]
struct ApiCurrent {
    temperature_2m: f64,
    weather_code: u8,
}

#[derive(Deserialize)]
struct ApiDaily {
    time: Vec<String>,
    weather_code: Vec<u8>,
    temperature_2m_max: Vec<f64>,
    temperature_2m_min: Vec<f64>,
}

/// Parses an Open-Meteo response (Celsius) into a forecast.
pub fn parse_forecast(json: &str, loc: &Location, now: u64) -> Result<Forecast, String> {
    let r: ApiResponse = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let d = r.daily;
    let days = d
        .time
        .into_iter()
        .zip(d.weather_code)
        .zip(d.temperature_2m_min.into_iter().zip(d.temperature_2m_max))
        .take(5)
        .map(|((date, code), (min_c, max_c))| Day {
            date,
            code,
            min_c,
            max_c,
        })
        .collect();
    Ok(Forecast {
        city: loc.city.clone(),
        lat: loc.lat,
        lon: loc.lon,
        fetched_at: now,
        temp_c: r.current.temperature_2m,
        code: r.current.weather_code,
        days,
    })
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn cache_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("mac-suite/weather.json"))
}

/// Cached forecast for `loc` if younger than 30 minutes.
pub fn cached(loc: &Location) -> Option<Forecast> {
    let f = read_cache()?;
    is_fresh(&f, loc, now_secs()).then_some(f)
}

fn read_cache() -> Option<Forecast> {
    serde_json::from_str(&std::fs::read_to_string(cache_path()?).ok()?).ok()
}

fn is_fresh(f: &Forecast, loc: &Location, now: u64) -> bool {
    f.lat == loc.lat
        && f.lon == loc.lon
        && f.city == loc.city
        && now.saturating_sub(f.fetched_at) < CACHE_MAX_AGE_SECS
}

/// Returns fresh cached weather or fetches it (blocking). On a network error
/// the stale cache for the same place is returned if there is one.
pub fn load_or_fetch(loc: &Location) -> Result<Forecast, String> {
    if let Some(f) = cached(loc) {
        return Ok(f);
    }
    match fetch(loc) {
        Ok(f) => {
            write_cache(&f);
            Ok(f)
        }
        Err(e) => read_cache()
            .filter(|f| f.lat == loc.lat && f.lon == loc.lon)
            .ok_or(e),
    }
}

fn fetch(loc: &Location) -> Result<Forecast, String> {
    let url = format!(
        "{ENDPOINT}?latitude={}&longitude={}&current=temperature_2m,weather_code\
         &daily=weather_code,temperature_2m_max,temperature_2m_min&timezone=auto&forecast_days=5",
        loc.lat, loc.lon
    );
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(10)))
        .build()
        .into();
    let body = agent
        .get(&url)
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    parse_forecast(&body, loc, now_secs())
}

fn write_cache(f: &Forecast) {
    let Some(path) = cache_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string(f) {
        // Write then rename so concurrent readers never see a partial file.
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        if std::fs::write(&tmp, json).is_ok() && std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

/// WMO weather code to (label, freedesktop symbolic icon name).
pub fn describe(code: u8) -> (&'static str, &'static str) {
    match code {
        0 => ("Clear", "weather-clear-symbolic"),
        1 => ("Mostly clear", "weather-few-clouds-symbolic"),
        2 => ("Partly cloudy", "weather-few-clouds-symbolic"),
        3 => ("Overcast", "weather-overcast-symbolic"),
        45 | 48 => ("Fog", "weather-fog-symbolic"),
        51..=57 => ("Drizzle", "weather-showers-scattered-symbolic"),
        61..=65 | 66 | 67 => ("Rain", "weather-showers-symbolic"),
        71..=77 | 85 | 86 => ("Snow", "weather-snow-symbolic"),
        80..=82 => ("Showers", "weather-showers-symbolic"),
        95..=99 => ("Thunderstorm", "weather-storm-symbolic"),
        _ => ("Unknown", "weather-severe-alert-symbolic"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc() -> Location {
        Location {
            city: "Berlin".into(),
            lat: 52.5,
            lon: 13.4,
        }
    }

    #[test]
    fn parses_location() {
        let l = Location::parse("city=Berlin\nlat=52.52\nlon=13.4\n").unwrap();
        assert_eq!((l.city.as_str(), l.lat, l.lon), ("Berlin", 52.52, 13.4));
        assert!(Location::parse("city=X\nlat=95\nlon=1").is_none());
        assert!(Location::parse("city=X").is_none());
    }

    #[test]
    fn maps_codes() {
        assert_eq!(describe(0).0, "Clear");
        assert_eq!(describe(63).1, "weather-showers-symbolic");
        assert_eq!(describe(96).0, "Thunderstorm");
        assert_eq!(describe(200).0, "Unknown");
    }

    #[test]
    fn parses_response() {
        let json = r#"{"current":{"temperature_2m":12.3,"weather_code":3},
            "daily":{"time":["2026-01-01","2026-01-02"],"weather_code":[3,61],
            "temperature_2m_max":[14.0,10.0],"temperature_2m_min":[5.0,4.0]}}"#;
        let f = parse_forecast(json, &loc(), 42).unwrap();
        assert_eq!((f.temp_c, f.code, f.fetched_at), (12.3, 3, 42));
        assert_eq!(f.days.len(), 2);
        assert_eq!((f.days[1].min_c, f.days[1].max_c, f.days[1].code), (4.0, 10.0, 61));
        assert!(parse_forecast("{}", &loc(), 0).is_err());
    }

    #[test]
    fn cache_freshness() {
        let f = parse_forecast(
            r#"{"current":{"temperature_2m":1.0,"weather_code":0},
            "daily":{"time":[],"weather_code":[],"temperature_2m_max":[],"temperature_2m_min":[]}}"#,
            &loc(),
            1000,
        )
        .unwrap();
        assert!(is_fresh(&f, &loc(), 1000 + 1799));
        assert!(!is_fresh(&f, &loc(), 1000 + 1800));
        let other = Location { lat: 1.0, ..loc() };
        assert!(!is_fresh(&f, &other, 1001));
    }
}
