// SPDX-License-Identifier: GPL-3.0-only
//! System statistics from `/proc` and `/sys`.

use std::fs;

/// Cumulative CPU jiffies from `/proc/stat`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuSample {
    idle: u64,
    total: u64,
}

impl CpuSample {
    pub fn read() -> Option<Self> {
        Self::parse(&fs::read_to_string("/proc/stat").ok()?)
    }

    fn parse(text: &str) -> Option<Self> {
        let line = text.lines().next()?;
        let mut fields = line.split_whitespace();
        if fields.next()? != "cpu" {
            return None;
        }
        let v: Vec<u64> = fields.filter_map(|f| f.parse().ok()).collect();
        if v.len() < 4 {
            return None;
        }
        // idle + iowait count as idle; guest time is already inside user.
        let idle = v[3] + v.get(4).copied().unwrap_or(0);
        let total = v.iter().take(8).sum();
        Some(Self { idle, total })
    }

    /// Busy percentage (0..=100) between two samples, `self` being the later.
    pub fn percent_since(&self, earlier: &Self) -> f32 {
        let total = self.total.saturating_sub(earlier.total);
        if total == 0 {
            return 0.0;
        }
        let idle = self.idle.saturating_sub(earlier.idle).min(total);
        (total - idle) as f32 * 100.0 / total as f32
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub used: u64,
    pub total: u64,
}

impl Usage {
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.used as f64 / self.total as f64) as f32
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
}

/// One snapshot of everything the System card shows.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub cpu_percent: f32,
    /// Bytes.
    pub ram: Usage,
    /// Bytes.
    pub swap: Usage,
    /// Bytes, root filesystem.
    pub disk: Usage,
    pub battery: Option<Battery>,
}

impl Stats {
    /// Reads all stats; `previous` is the last CPU sample, replaced by the new one.
    pub fn read(previous: &mut Option<CpuSample>) -> Self {
        let mut stats = Stats::default();
        if let Some(now) = CpuSample::read() {
            if let Some(prev) = previous.as_ref() {
                stats.cpu_percent = now.percent_since(prev);
            }
            *previous = Some(now);
        }
        if let Ok(text) = fs::read_to_string("/proc/meminfo") {
            (stats.ram, stats.swap) = parse_meminfo(&text);
        }
        stats.disk = root_disk().unwrap_or_default();
        stats.battery = battery();
        stats
    }
}

fn parse_meminfo(text: &str) -> (Usage, Usage) {
    let get = |key: &str| -> u64 {
        text.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
            .and_then(|rest| rest.split_whitespace().next()?.parse::<u64>().ok())
            .map_or(0, |kb| kb * 1024)
    };
    let ram_total = get("MemTotal");
    let ram = Usage {
        used: ram_total.saturating_sub(get("MemAvailable")),
        total: ram_total,
    };
    let swap_total = get("SwapTotal");
    let swap = Usage {
        used: swap_total.saturating_sub(get("SwapFree")),
        total: swap_total,
    };
    (ram, swap)
}

fn root_disk() -> Option<Usage> {
    let st = rustix::fs::statvfs("/").ok()?;
    let total = st.f_blocks * st.f_frsize;
    let free = st.f_bavail * st.f_frsize;
    Some(Usage {
        used: total.saturating_sub(free),
        total,
    })
}

fn battery() -> Option<Battery> {
    let mut dirs: Vec<_> = fs::read_dir("/sys/class/power_supply")
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with("BAT"))
        .collect();
    dirs.sort_by_key(|e| e.file_name());
    let dir = dirs.first()?.path();
    let percent: u8 = fs::read_to_string(dir.join("capacity"))
        .ok()?
        .trim()
        .parse::<u8>()
        .ok()?
        .min(100);
    let status = fs::read_to_string(dir.join("status")).unwrap_or_default();
    Some(Battery {
        percent,
        charging: status.trim() == "Charging",
    })
}

/// Human readable size, e.g. `7.6 GB`.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_percent_between_samples() {
        let a = CpuSample::parse("cpu  100 0 100 800 0 0 0 0 0 0\n").unwrap();
        let b = CpuSample::parse("cpu  200 0 200 1000 0 0 0 0 0 0\n").unwrap();
        // delta total 400, idle 200 -> 50%
        assert!((b.percent_since(&a) - 50.0).abs() < 0.01);
        assert_eq!(a.percent_since(&a), 0.0);
    }

    #[test]
    fn meminfo() {
        let text = "MemTotal: 1000 kB\nMemAvailable: 400 kB\nSwapTotal: 200 kB\nSwapFree: 150 kB\n";
        let (ram, swap) = parse_meminfo(text);
        assert_eq!((ram.used, ram.total), (600 * 1024, 1000 * 1024));
        assert_eq!((swap.used, swap.total), (50 * 1024, 200 * 1024));
        assert!((ram.fraction() - 0.6).abs() < 1e-6);
    }

    #[test]
    fn bytes() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}
