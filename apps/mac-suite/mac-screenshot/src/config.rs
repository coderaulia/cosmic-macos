// SPDX-License-Identifier: GPL-3.0-only

//! Persistent options in `~/.config/mac-suite/screenshot.conf` (`key=value`).

use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SaveTo {
    Desktop,
    Pictures,
    #[default]
    Screenshots,
    Clipboard,
}

impl SaveTo {
    pub const ALL: [SaveTo; 4] = [Self::Desktop, Self::Pictures, Self::Screenshots, Self::Clipboard];

    pub fn key(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Pictures => "pictures",
            Self::Screenshots => "screenshots",
            Self::Clipboard => "clipboard",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Desktop => "Desktop",
            Self::Pictures => "Pictures",
            Self::Screenshots => "Screenshots",
            Self::Clipboard => "Clipboard",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.key() == s)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Timer {
    #[default]
    None,
    Five,
    Ten,
}

impl Timer {
    pub const ALL: [Timer; 3] = [Self::None, Self::Five, Self::Ten];

    pub fn seconds(self) -> u64 {
        match self {
            Self::None => 0,
            Self::Five => 5,
            Self::Ten => 10,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Five => "5s",
            Self::Ten => "10s",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.seconds().to_string() == s)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AudioSource {
    #[default]
    None,
    System,
    Microphone,
}

impl AudioSource {
    pub const ALL: [AudioSource; 3] = [Self::None, Self::System, Self::Microphone];

    pub fn key(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::System => "system",
            Self::Microphone => "microphone",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::System => "System Audio",
            Self::Microphone => "Microphone",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.key() == s)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    Screen,
    Window,
    #[default]
    Region,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub save_to: SaveTo,
    pub timer: Timer,
    pub show_pointer: bool,
    pub audio: AudioSource,
    pub restore_token: Option<String>,
}

pub fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("mac-suite").join("screenshot.conf"))
}

impl Options {
    pub fn parse(text: &str) -> Self {
        let mut o = Self::default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "save_to" => o.save_to = SaveTo::parse(v).unwrap_or(o.save_to),
                "timer" => o.timer = Timer::parse(v).unwrap_or(o.timer),
                "show_pointer" => o.show_pointer = v == "true",
                "audio" => o.audio = AudioSource::parse(v).unwrap_or(o.audio),
                "restore_token" => {
                    o.restore_token = (!v.is_empty()).then(|| v.to_owned());
                }
                _ => {}
            }
        }
        o
    }

    pub fn serialize(&self) -> String {
        let mut out = format!(
            "save_to={}\ntimer={}\nshow_pointer={}\naudio={}\n",
            self.save_to.key(),
            self.timer.seconds(),
            self.show_pointer,
            self.audio.key(),
        );
        if let Some(token) = self.restore_token.as_deref().filter(|t| !t.is_empty()) {
            out.push_str(&format!("restore_token={token}\n"));
        }
        out
    }

    pub fn load() -> Self {
        config_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| Self::parse(&t))
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = config_path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&path, self.serialize()) {
            tracing::warn!("cannot write {}: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let o = Options {
            save_to: SaveTo::Clipboard,
            timer: Timer::Ten,
            show_pointer: true,
            audio: AudioSource::System,
            restore_token: Some("portal-token-abc123".into()),
        };
        assert_eq!(Options::parse(&o.serialize()), o);
    }

    #[test]
    fn restore_token_empty_or_absent_is_none() {
        let o = Options::parse("save_to=screenshots\nrestore_token=\n");
        assert_eq!(o.restore_token, None);
        assert!(!o.serialize().contains("restore_token="));

        let with_mic = Options {
            audio: AudioSource::Microphone,
            restore_token: Some("tok-42".into()),
            ..Default::default()
        };
        let reparsed = Options::parse(&with_mic.serialize());
        assert_eq!(reparsed.audio, AudioSource::Microphone);
        assert_eq!(reparsed.restore_token.as_deref(), Some("tok-42"));
    }

    #[test]
    fn garbage_falls_back_to_defaults() {
        let o = Options::parse("save_to=nope\ntimer=7\nfoo\n=\nshow_pointer=maybe\naudio=wat\n");
        assert_eq!(o, Options::default());
        assert_eq!(o.save_to, SaveTo::Screenshots);
        assert_eq!(o.audio, AudioSource::None);
    }
}
