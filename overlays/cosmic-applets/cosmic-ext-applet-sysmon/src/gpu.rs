// SPDX-License-Identifier: GPL-3.0-only

//! GPU load and video memory: AMD/Intel from sysfs, NVIDIA via `nvidia-smi`.

use std::{fs, path::Path, process::Command};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Gpu {
    pub name: String,
    pub busy_percent: f32,
    /// Used and total video memory in bytes, when known.
    pub vram: Option<(u64, u64)>,
    pub temp_c: Option<f32>,
}

/// Read the first GPU that reports its load. Blocking (may run a process):
/// call it off the UI thread.
pub fn read() -> Option<Gpu> {
    read_sysfs().or_else(read_nvidia)
}

fn read_sysfs() -> Option<Gpu> {
    for entry in fs::read_dir("/sys/class/drm").ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let dev = entry.path().join("device");
        let Some(busy) = read_num(&dev.join("gpu_busy_percent")) else {
            continue;
        };
        let vram = read_num(&dev.join("mem_info_vram_used"))
            .zip(read_num(&dev.join("mem_info_vram_total")))
            .map(|(u, t)| (u as u64, t as u64));
        return Some(Gpu {
            name: "GPU".into(),
            busy_percent: busy as f32,
            vram,
            temp_c: None,
        });
    }
    None
}

fn read_num(path: &Path) -> Option<f64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn read_nvidia() -> Option<Gpu> {
    let out = Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,utilization.gpu,memory.used,memory.total,temperature.gpu",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_nvidia(&String::from_utf8_lossy(&out.stdout))
}

/// Parse one `nvidia-smi` CSV line (memory in MiB).
pub fn parse_nvidia(text: &str) -> Option<Gpu> {
    let fields: Vec<&str> = text.lines().next()?.split(',').map(str::trim).collect();
    let [name, busy, used, total, temp] = fields.as_slice() else {
        return None;
    };
    const MIB: u64 = 1024 * 1024;
    Some(Gpu {
        name: (*name).to_owned(),
        busy_percent: busy.parse().ok()?,
        vram: used
            .parse::<u64>()
            .ok()
            .zip(total.parse::<u64>().ok())
            .map(|(u, t)| (u * MIB, t * MIB)),
        temp_c: temp.parse().ok(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nvidia_line() {
        let gpu = parse_nvidia("NVIDIA GeForce GTX 1050 Ti, 7, 508, 4096, 37\n").unwrap();
        assert_eq!(gpu.name, "NVIDIA GeForce GTX 1050 Ti");
        assert_eq!(gpu.busy_percent, 7.0);
        assert_eq!(gpu.vram, Some((508 * 1024 * 1024, 4096 * 1024 * 1024)));
        assert_eq!(gpu.temp_c, Some(37.0));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_nvidia("").is_none());
        assert!(parse_nvidia("a, b").is_none());
        assert!(parse_nvidia("x, [N/A], 1, 2, 3").is_none());
    }
}
