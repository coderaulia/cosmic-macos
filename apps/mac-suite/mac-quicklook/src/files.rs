// SPDX-License-Identifier: GPL-3.0-only

//! Folder navigation: sibling ordering like the Files app, and small helpers.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

/// Case-insensitive "natural" order: digit runs compare by value.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = a.next_if(char::is_ascii_digit) {
                    na.push(c);
                }
                let mut nb = String::new();
                while let Some(c) = b.next_if(char::is_ascii_digit) {
                    nb.push(c);
                }
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                a.next();
                b.next();
            }
        }
    }
}

/// File names of a folder in display order, directories skipped.
/// Hidden files are listed only when `show_hidden` is set.
pub fn sort_names(mut names: Vec<String>, show_hidden: bool) -> Vec<String> {
    if !show_hidden {
        names.retain(|n| !n.starts_with('.'));
    }
    names.sort_by(|a, b| natural_cmp(a, b).then_with(|| a.cmp(b)));
    names
}

/// Sibling files of `path` (including itself) in Files order.
pub fn siblings(path: &Path) -> Vec<PathBuf> {
    let Some(dir) = path.parent() else {
        return vec![path.to_path_buf()];
    };
    let hidden = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('.'));
    let names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    let mut sorted = sort_names(names, hidden);
    if let Some(n) = path.file_name().and_then(|n| n.to_str())
        && !sorted.iter().any(|s| s == n)
    {
        sorted.push(n.to_string());
        sorted.sort_by(|a, b| natural_cmp(a, b).then_with(|| a.cmp(b)));
    }
    sorted.into_iter().map(|n| dir.join(n)).collect()
}

/// The file `delta` steps away from `path`, wrapping around; `None` if alone.
pub fn step(path: &Path, delta: isize) -> Option<PathBuf> {
    let list = siblings(path);
    step_in(&list, path, delta)
}

pub fn step_in(list: &[PathBuf], path: &Path, delta: isize) -> Option<PathBuf> {
    if list.len() < 2 {
        return None;
    }
    let idx = list.iter().position(|p| p == path)?;
    let n = list.len() as isize;
    let next = (idx as isize + delta).rem_euclid(n) as usize;
    Some(list[next].clone())
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    format!("{v:.1} {}", UNITS[u])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn natural_order() {
        assert_eq!(natural_cmp("img2.png", "img10.png"), Ordering::Less);
        assert_eq!(natural_cmp("B", "a"), Ordering::Greater);
        assert_eq!(natural_cmp("a", "A"), Ordering::Equal);
        assert_eq!(natural_cmp("a", "ab"), Ordering::Less);
        assert_eq!(natural_cmp("x007", "x7"), Ordering::Equal);
    }

    #[test]
    fn sorting_hides_dotfiles() {
        let s = sort_names(v(&["b.txt", ".hidden", "A.txt", "a10", "a9"]), false);
        assert_eq!(s, v(&["A.txt", "a9", "a10", "b.txt"]));
        assert_eq!(sort_names(v(&[".x", "a"]), true), v(&[".x", "a"]));
    }

    #[test]
    fn stepping_wraps() {
        let list: Vec<PathBuf> = ["/d/a", "/d/b", "/d/c"].iter().map(PathBuf::from).collect();
        assert_eq!(step_in(&list, Path::new("/d/a"), -1), Some(PathBuf::from("/d/c")));
        assert_eq!(step_in(&list, Path::new("/d/c"), 1), Some(PathBuf::from("/d/a")));
        assert_eq!(step_in(&list, Path::new("/d/b"), 1), Some(PathBuf::from("/d/c")));
        assert_eq!(step_in(&list[..1], Path::new("/d/a"), 1), None);
        assert_eq!(step_in(&list, Path::new("/zz"), 1), None);
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(12), "12 B");
        assert_eq!(human_size(1500), "1.5 KB");
        assert_eq!(human_size(2_000_000), "2.0 MB");
    }
}
