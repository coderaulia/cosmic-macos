// SPDX-License-Identifier: GPL-3.0-only

use serde::Deserialize;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct AudioSink {
    pub name: String,
    pub description: String,
    pub volume: u32,
    pub mute: bool,
}

#[derive(Debug, Clone, Default)]
pub struct AudioState {
    pub active_volume: u32,
    pub active_mute: bool,
    pub default_sink: Option<String>,
    pub sinks: Vec<AudioSink>,
}

impl AudioState {
    pub fn active_sink_description(&self) -> Option<&str> {
        let def = self.default_sink.as_deref()?;
        self.sinks
            .iter()
            .find(|s| s.name == def)
            .map(|s| s.description.as_str())
    }
}

#[derive(Debug, Deserialize)]
struct PactlSink {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    mute: bool,
    #[serde(default)]
    volume: std::collections::HashMap<String, PactlChannel>,
}

#[derive(Debug, Deserialize)]
struct PactlChannel {
    #[serde(default)]
    value_percent: String,
}

pub fn parse_pactl_sinks(json: &str, default_sink: Option<String>) -> AudioState {
    let parsed: Vec<PactlSink> = serde_json::from_str(json).unwrap_or_default();
    let sinks: Vec<AudioSink> = parsed
        .into_iter()
        .map(|s| {
            let vol = s
                .volume
                .values()
                .filter_map(|c| c.value_percent.trim().trim_end_matches('%').parse::<u32>().ok())
                .next()
                .unwrap_or(0);
            let desc = s
                .description
                .replace("High Definition Audio Controller", "Audio")
                .replace("Digital Stereo", "Stereo")
                .trim()
                .to_string();
            AudioSink {
                name: s.name,
                description: desc,
                volume: vol,
                mute: s.mute,
            }
        })
        .collect();

    let active = default_sink
        .as_deref()
        .and_then(|d| sinks.iter().find(|s| s.name == d))
        .or_else(|| sinks.first());

    let (active_volume, active_mute) = active.map(|s| (s.volume, s.mute)).unwrap_or((0, false));
    AudioState {
        active_volume,
        active_mute,
        default_sink,
        sinks,
    }
}

pub fn fetch_audio_state() -> AudioState {
    let default_sink = Command::new("pactl")
        .arg("get-default-sink")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());

    let json = Command::new("pactl")
        .args(["--format=json", "list", "sinks"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();

    parse_pactl_sinks(&json, default_sink)
}

pub fn set_sink_volume(percent: u32) {
    let clamped = percent.min(100);
    let _ = Command::new("pactl")
        .args(["set-sink-mute", "@DEFAULT_SINK@", "0"])
        .status();
    let _ = Command::new("pactl")
        .args(["set-sink-volume", "@DEFAULT_SINK@", &format!("{clamped}%")])
        .status();
}

pub fn toggle_sink_mute() {
    let _ = Command::new("pactl")
        .args(["set-sink-mute", "@DEFAULT_SINK@", "toggle"])
        .status();
}

pub fn set_default_sink(name: &str) {
    let _ = Command::new("pactl")
        .args(["set-default-sink", name])
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pactl_json_sinks() {
        let raw = r#"[
            {
                "name": "alsa_output.hdmi",
                "description": "HDMI Output",
                "mute": false,
                "volume": {"front-left": {"value_percent": "40%"}}
            },
            {
                "name": "alsa_output.usb",
                "description": "USB Headphones",
                "mute": true,
                "volume": {"front-left": {"value_percent": "75%"}}
            }
        ]"#;
        let state = parse_pactl_sinks(raw, Some("alsa_output.usb".to_string()));
        assert_eq!(state.sinks.len(), 2);
        assert_eq!(state.active_volume, 75);
        assert!(state.active_mute);
        assert_eq!(state.active_sink_description(), Some("USB Headphones"));
    }
}
