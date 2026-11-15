// SPDX-License-Identifier: GPL-3.0-only

//! From a user selection to a saved file: crop, timer, save, clipboard,
//! thumbnail.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use image::RgbaImage;

use crate::config::Options;
use crate::geometry::Rect;
use crate::output::{self, Error};
use crate::wayland::{self, OutputShot, Snapshot};

/// What to capture: an output, or a rectangle in its logical coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub output: String,
    pub rect: Option<Rect>,
}

/// Cut `rect` (output-local logical) out of an output capture.
pub fn crop(shot: &OutputShot, rect: Option<Rect>) -> Option<RgbaImage> {
    let Some(rect) = rect else { return Some(shot.image.clone()) };
    let (lw, lh) = (shot.logical_size.0 as f32, shot.logical_size.1 as f32);
    let p = rect.to_pixels(lw, lh, shot.image.width(), shot.image.height())?;
    Some(image::imageops::crop_imm(&shot.image, p.x, p.y, p.w, p.h).to_image())
}

/// Execute a request: wait for the timer (re-capturing afterwards), crop,
/// then deliver. `frozen` is the capture the overlay showed.
pub fn run_request(
    req: &Request,
    frozen: Option<&Snapshot>,
    options: Options,
) -> Result<Option<PathBuf>, Error> {
    let fresh;
    let snap = match frozen {
        Some(f) if options.timer.seconds() == 0 => f,
        _ => {
            std::thread::sleep(Duration::from_secs(options.timer.seconds()));
            fresh = wayland::snapshot(false, options.show_pointer)?;
            &fresh
        }
    };
    let shot = snap
        .outputs
        .iter()
        .find(|o| o.name == req.output)
        .or_else(|| snap.outputs.first())
        .ok_or_else(|| Error::Msg("no output".into()))?;
    let img = crop(shot, req.rect).ok_or_else(|| Error::Msg("empty selection".into()))?;
    deliver(&img, &options, None)
}

/// Capture every output (macOS Cmd+Shift+3 style); one file per output.
pub fn run_screens(options: Options) -> Result<Vec<PathBuf>, Error> {
    std::thread::sleep(Duration::from_secs(options.timer.seconds()));
    let snap = wayland::snapshot(false, options.show_pointer)?;
    let many = snap.outputs.len() > 1;
    let stamp = output::timestamp();
    let mut paths = Vec::new();
    for (i, shot) in snap.outputs.iter().enumerate() {
        // Only the first image goes to the clipboard.
        if i > 0 && options.save_to == crate::config::SaveTo::Clipboard {
            break;
        }
        let suffix = many.then_some(i + 1);
        if let Some(p) = deliver_at(&shot.image, &options, &stamp, suffix)? {
            paths.push(p);
        }
    }
    Ok(paths)
}

pub fn deliver(img: &RgbaImage, options: &Options, suffix: Option<usize>) -> Result<Option<PathBuf>, Error> {
    deliver_at(img, options, &output::timestamp(), suffix)
}

fn deliver_at(
    img: &RgbaImage,
    options: &Options,
    stamp: &str,
    suffix: Option<usize>,
) -> Result<Option<PathBuf>, Error> {
    match output::target_dir(options.save_to) {
        Some(dir) => {
            let path = output::save_png(img, &dir, &output::file_name(stamp, suffix))?;
            spawn_thumbnail(&path);
            Ok(Some(path))
        }
        None => {
            output::copy_png(&output::encode_png(img)?)?;
            Ok(None)
        }
    }
}

/// Show the floating thumbnail in a detached copy of ourselves.
pub fn spawn_thumbnail(path: &std::path::Path) {
    let Ok(exe) = std::env::current_exe() else { return };
    let _ = Command::new(exe)
        .arg("--thumbnail")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// Best-effort desktop notification for failures.
pub fn notify(msg: &str) {
    eprintln!("mac-screenshot: {msg}");
    let _ = Command::new("notify-send")
        .args(["-a", "Screenshot", "Screenshot failed", msg])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shot(w: u32, h: u32, lw: i32, lh: i32) -> OutputShot {
        OutputShot {
            name: "X".into(),
            logical_size: (lw, lh),
            image: RgbaImage::new(w, h),
            windows: Vec::new(),
        }
    }

    #[test]
    fn crop_scales_logical_to_pixels() {
        let s = shot(200, 100, 100, 50);
        let img = crop(&s, Some(Rect::new(10.0, 10.0, 20.0, 10.0))).unwrap();
        assert_eq!(img.dimensions(), (40, 20));
        assert_eq!(crop(&s, None).unwrap().dimensions(), (200, 100));
        assert!(crop(&s, Some(Rect::new(0.0, 0.0, 0.0, 5.0))).is_none());
    }

    #[test]
    fn crop_clamps_out_of_bounds_and_fractional_scales() {
        // 3840x2160 at 1.5x -> logical 2560x1440 (using smaller proportional dims for speed).
        let s = shot(384, 216, 256, 144);
        let img = crop(&s, Some(Rect::new(-20.0, -10.0, 300.0, 200.0))).unwrap();
        assert_eq!(img.dimensions(), (384, 216));

        let edge = crop(&s, Some(Rect::new(200.0, 100.0, 100.0, 100.0))).unwrap();
        assert_eq!(edge.dimensions(), (84, 66));

        // Negative / zero / offscreen selections return None cleanly.
        assert!(crop(&s, Some(Rect::new(10.0, 10.0, -5.0, 20.0))).is_none());
        assert!(crop(&s, Some(Rect::new(300.0, 10.0, 20.0, 20.0))).is_none());
        assert!(crop(&shot(100, 100, 0, 100), Some(Rect::new(0.0, 0.0, 10.0, 10.0))).is_none());
    }
}
