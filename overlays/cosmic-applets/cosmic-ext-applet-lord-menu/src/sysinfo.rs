// SPDX-License-Identifier: GPL-3.0-only

//! System information collection and parsing for the "About This Desktop" window.

use std::process::Command;

use cctk::sctk::output::OutputInfo;

/// Snapshot of system hardware and OS information shown in "About This Desktop".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemInfo {
    pub distro: String,
    pub cosmic_version: String,
    pub hostname: String,
    pub cpu: String,
    pub memory: String,
    pub gpu: String,
    pub display: String,
}

/// Collect system information from standard Linux files, commands, and Wayland outputs.
#[must_use]
pub fn collect_system_info(outputs: &[OutputInfo]) -> SystemInfo {
    let distro = std::fs::read_to_string("/etc/os-release")
        .map(|s| parse_os_release(&s))
        .unwrap_or_else(|_| "Linux".to_string());

    let cosmic_version = detect_cosmic_version();
    let hostname = detect_hostname();

    let cpu = std::fs::read_to_string("/proc/cpuinfo")
        .map(|s| parse_cpuinfo(&s))
        .unwrap_or_else(|_| "Unknown CPU".to_string());

    let memory = std::fs::read_to_string("/proc/meminfo")
        .map(|s| parse_meminfo(&s))
        .unwrap_or_else(|_| "Unknown".to_string());

    let gpu = detect_gpus();
    let display = format_displays(outputs);

    SystemInfo {
        distro,
        cosmic_version,
        hostname,
        cpu,
        memory,
        gpu,
        display,
    }
}

/// Parse `/etc/os-release` into a concise distribution name.
#[must_use]
pub fn parse_os_release(content: &str) -> String {
    let mut pretty = None;
    let mut name = None;
    let mut version = None;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, raw_val)) = line.split_once('=') else {
            continue;
        };
        let val = raw_val
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .trim()
            .to_string();
        if val.is_empty() {
            continue;
        }
        match key.trim() {
            "PRETTY_NAME" => pretty = Some(val),
            "NAME" => name = Some(val),
            "VERSION" | "VERSION_ID" => {
                if version.is_none() {
                    version = Some(val);
                }
            }
            _ => {}
        }
    }

    if let Some(p) = pretty {
        return p;
    }
    match (name, version) {
        (Some(n), Some(v)) => format!("{n} {v}"),
        (Some(n), None) => n,
        _ => "Linux".to_string(),
    }
}

/// Parse `cosmic-comp --version` or `cosmic-panel --version` stdout into a readable COSMIC version.
#[must_use]
pub fn parse_cosmic_version(output: &str) -> String {
    let first_line = output.lines().next().unwrap_or("").trim();
    if first_line.is_empty() {
        return "COSMIC".to_string();
    }
    // e.g. "cosmic-comp 1.0.0 (git commit 82049ae9...)" -> "COSMIC 1.0.0"
    let without_bin = first_line
        .strip_prefix("cosmic-comp ")
        .or_else(|| first_line.strip_prefix("cosmic-panel "))
        .unwrap_or(first_line);
    let version_token = without_bin
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim();
    if version_token.is_empty() {
        "COSMIC".to_string()
    } else {
        format!("COSMIC {version_token}")
    }
}

fn detect_cosmic_version() -> String {
    for bin in ["cosmic-comp", "cosmic-panel"] {
        if let Ok(out) = Command::new(bin).arg("--version").output()
            && out.status.success()
        {
            let text = String::from_utf8_lossy(&out.stdout);
            let parsed = parse_cosmic_version(&text);
            if parsed != "COSMIC" {
                return parsed;
            }
        }
    }
    "COSMIC Desktop".to_string()
}

fn detect_hostname() -> String {
    if let Ok(name) = std::fs::read_to_string("/etc/hostname") {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let uname = rustix::system::uname();
    let nodename = uname.nodename().to_string_lossy().trim().to_string();
    if nodename.is_empty() {
        "localhost".to_string()
    } else {
        nodename
    }
}

/// Parse `/proc/cpuinfo` into a concise CPU summary (model + logical thread count).
#[must_use]
pub fn parse_cpuinfo(content: &str) -> String {
    let mut model: Option<String> = None;
    let mut count: usize = 0;

    for line in content.lines() {
        let Some((key, val)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let val = val.trim();
        if key == "processor" {
            count += 1;
        } else if model.is_none()
            && matches!(key, "model name" | "Model" | "Hardware" | "cpu model")
            && !val.is_empty()
        {
            model = Some(clean_cpu_name(val));
        }
    }

    match (model, count) {
        (Some(m), n) if n > 0 => format!("{m} ({n} threads)"),
        (Some(m), _) => m,
        (None, n) if n > 0 => format!("{n}-core CPU"),
        _ => "Unknown CPU".to_string(),
    }
}

fn clean_cpu_name(raw: &str) -> String {
    raw.replace("(R)", "")
        .replace("(TM)", "")
        .replace("CPU ", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse `/proc/meminfo` `MemTotal:` line into a human-readable GiB/MiB string.
#[must_use]
pub fn parse_meminfo(content: &str) -> String {
    for line in content.lines() {
        let Some(rest) = line.strip_prefix("MemTotal:") else {
            continue;
        };
        let kb: Option<u64> = rest
            .split_whitespace()
            .next()
            .and_then(|num| num.parse::<u64>().ok());
        if let Some(kb) = kb {
            let gib = kb as f64 / (1024.0 * 1024.0);
            if gib >= 1.0 {
                return format!("{gib:.1} GiB");
            }
            let mib = kb as f64 / 1024.0;
            return format!("{mib:.0} MiB");
        }
    }
    "Unknown".to_string()
}

/// Parse `lspci` output lines into clean GPU adapter names.
#[must_use]
pub fn parse_lspci_gpus(output: &str) -> Vec<String> {
    let mut gpus = Vec::new();
    for line in output.lines() {
        let lower = line.to_ascii_lowercase();
        if !(lower.contains("vga compatible controller:")
            || lower.contains("3d controller:")
            || lower.contains("display controller:"))
        {
            continue;
        }
        let Some((_, after_type)) = line.split_once(": ") else {
            continue;
        };
        // Strip trailing "(rev xx)"
        let cleaned = match after_type.rsplit_once(" (rev ") {
            Some((prefix, _)) => prefix,
            None => after_type,
        }
        .trim();
        if !cleaned.is_empty() {
            gpus.push(cleaned.to_string());
        }
    }
    gpus
}

/// Parse `/sys/class/drm/card*/device/uevent` into a fallback GPU label.
#[must_use]
pub fn parse_drm_uevent(content: &str) -> Option<String> {
    let mut driver = None;
    let mut pci_id = None;
    for line in content.lines() {
        if let Some(val) = line.strip_prefix("DRIVER=") {
            driver = Some(val.trim().to_string());
        } else if let Some(val) = line.strip_prefix("PCI_ID=") {
            pci_id = Some(val.trim().to_string());
        }
    }
    match (driver, pci_id) {
        (Some(d), Some(id)) => Some(format!("{d} ({id})")),
        (Some(d), None) => Some(d),
        (None, Some(id)) => Some(format!("GPU {id}")),
        (None, None) => None,
    }
}

fn detect_gpus() -> String {
    if let Ok(out) = Command::new("lspci").output()
        && out.status.success()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        let gpus = parse_lspci_gpus(&text);
        if !gpus.is_empty() {
            return gpus.join(", ");
        }
    }

    if let Ok(entries) = std::fs::read_dir("/sys/class/drm") {
        let mut fallback = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with("card") && !name_str.contains('-') {
                let uevent_path = entry.path().join("device/uevent");
                if let Ok(content) = std::fs::read_to_string(uevent_path)
                    && let Some(label) = parse_drm_uevent(&content)
                {
                    fallback.push(label);
                }
            }
        }
        if !fallback.is_empty() {
            return fallback.join(", ");
        }
    }

    "Unknown GPU".to_string()
}

/// Format Wayland output info (or `/sys/class/drm` fallback) into a concise Display string.
#[must_use]
pub fn format_displays(outputs: &[OutputInfo]) -> String {
    let mut parts = Vec::new();
    for out in outputs {
        let current_mode = out
            .modes
            .iter()
            .find(|m| m.current)
            .or_else(|| out.modes.first());
        let label = match current_mode {
            Some(mode) => {
                let (w, h) = mode.dimensions;
                let hz = mode.refresh_rate as f64 / 1000.0;
                if !out.model.is_empty() {
                    format!("{} ({w}×{h} @ {hz:.0} Hz)", out.model.trim())
                } else if let Some(name) = &out.name {
                    format!("{name} ({w}×{h} @ {hz:.0} Hz)")
                } else {
                    format!("{w}×{h} @ {hz:.0} Hz")
                }
            }
            None => out
                .name
                .clone()
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "Display".to_string()),
        };
        parts.push(label);
    }

    if !parts.is_empty() {
        return parts.join(", ");
    }

    if let Ok(entries) = std::fs::read_dir("/sys/class/drm") {
        let mut drm_displays = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let status = std::fs::read_to_string(path.join("status")).unwrap_or_default();
            if status.trim() != "connected" {
                continue;
            }
            if let Ok(modes) = std::fs::read_to_string(path.join("modes"))
                && let Some(first_mode) = modes.lines().next().map(str::trim)
                && !first_mode.is_empty()
            {
                drm_displays.push(first_mode.to_string());
            }
        }
        if !drm_displays.is_empty() {
            return drm_displays.join(", ");
        }
    }

    "Unknown Display".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_os_release_pretty_name_and_fallback() {
        let cachy = r#"
NAME="CachyOS Linux"
PRETTY_NAME="CachyOS"
ID=cachyos
"#;
        assert_eq!(parse_os_release(cachy), "CachyOS");

        let minimal = r#"
NAME="Pop!_OS"
VERSION_ID="24.04"
"#;
        assert_eq!(parse_os_release(minimal), "Pop!_OS 24.04");
        assert_eq!(parse_os_release(""), "Linux");
    }

    #[test]
    fn parses_cosmic_version_output() {
        assert_eq!(
            parse_cosmic_version("cosmic-comp 1.0.0 (git commit 82049ae9)"),
            "COSMIC 1.0.0"
        );
        assert_eq!(parse_cosmic_version("cosmic-panel 1.11.0"), "COSMIC 1.11.0");
        assert_eq!(parse_cosmic_version(""), "COSMIC");
    }

    #[test]
    fn parses_cpuinfo_model_and_threads() {
        let sample = "\
processor\t: 0
model name\t: Intel(R) Core(TM) i3-10100 CPU @ 3.60GHz

processor\t: 1
model name\t: Intel(R) Core(TM) i3-10100 CPU @ 3.60GHz
";
        assert_eq!(
            parse_cpuinfo(sample),
            "Intel Core i3-10100 @ 3.60GHz (2 threads)"
        );
    }

    #[test]
    fn parses_meminfo_in_gib_and_mib() {
        let sample = "MemTotal:       32496640 kB\nMemFree:         4000000 kB\n";
        assert_eq!(parse_meminfo(sample), "31.0 GiB");

        let small = "MemTotal:         524288 kB\n";
        assert_eq!(parse_meminfo(small), "512 MiB");
    }

    #[test]
    fn parses_lspci_gpu_lines() {
        let sample = "\
00:00.0 Host bridge: Intel Corporation Device 9b53
00:02.0 VGA compatible controller: Intel Corporation CometLake-S GT2 [UHD Graphics 630] (rev 03)
01:00.0 VGA compatible controller: NVIDIA Corporation GP107 [GeForce GTX 1050 Ti] (rev a1)
";
        assert_eq!(
            parse_lspci_gpus(sample),
            vec![
                "Intel Corporation CometLake-S GT2 [UHD Graphics 630]",
                "NVIDIA Corporation GP107 [GeForce GTX 1050 Ti]",
            ]
        );
    }

    #[test]
    fn parses_drm_uevent_fallback() {
        let sample = "DRIVER=i915\nPCI_ID=8086:9BC8\n";
        assert_eq!(parse_drm_uevent(sample).as_deref(), Some("i915 (8086:9BC8)"));
    }
}
