// SPDX-License-Identifier: GPL-3.0-only

//! Screen recording engine: `org.freedesktop.portal.ScreenCast` via `ashpd`
//! + GStreamer (`gst-launch-1.0 -e`) recording pipeline with clean EOS shutdown.

use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType, Stream};
use ashpd::desktop::{PersistMode, ResponseError, Session};
use ashpd::Error as AshpdError;
use rustix::io::{FdFlags, fcntl_setfd};
use rustix::process::{Pid, Signal, kill_process};

use crate::config::{AudioSource, Options};
use crate::geometry::{PixelRect, Rect};
use crate::output::{self, Error};

/// Target specification for starting a screen recording.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordTarget {
    pub output: String,
    pub logical_size: (i32, i32),
    pub pixel_size: (u32, u32),
    pub rect: Option<Rect>,
}

/// GStreamer `videocrop` edge margins (in pixels) with even-dimension output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoCrop {
    pub top: u32,
    pub bottom: u32,
    pub left: u32,
    pub right: u32,
}

impl VideoCrop {
    /// Compute even-aligned `videocrop` margins for `pixel_rect` inside a
    /// `frame_w x frame_h` video frame.
    ///
    /// H.264 YUV 4:2:0 encoders (`nvh264enc`, `vaapih264enc`, `x264enc`) require
    /// even width and height, and `left`/`top` are aligned down to even pixel
    /// boundaries while expanding right/bottom to cover the selection.
    pub fn from_pixel_rect(p: PixelRect, frame_w: u32, frame_h: u32) -> Option<Self> {
        let max_even_w = frame_w & !1;
        let max_even_h = frame_h & !1;
        if max_even_w < 2 || max_even_h < 2 || p.w == 0 || p.h == 0 {
            return None;
        }
        if p.x >= frame_w || p.y >= frame_h {
            return None;
        }

        let mut left = (p.x & !1).min(max_even_w - 2);
        let x1_raw = p.x.saturating_add(p.w).min(frame_w);
        let mut x1 = ((x1_raw.saturating_add(1)) & !1).min(max_even_w);
        if x1 <= left {
            left = x1.saturating_sub(2);
            x1 = left + 2;
        }

        let mut top = (p.y & !1).min(max_even_h - 2);
        let y1_raw = p.y.saturating_add(p.h).min(frame_h);
        let mut y1 = ((y1_raw.saturating_add(1)) & !1).min(max_even_h);
        if y1 <= top {
            top = y1.saturating_sub(2);
            y1 = top + 2;
        }

        let right = frame_w - x1;
        let bottom = frame_h - y1;
        Some(Self { top, bottom, left, right })
    }

    pub fn is_noop(&self) -> bool {
        self.top == 0 && self.bottom == 0 && self.left == 0 && self.right == 0
    }
}

/// Convert a logical region selection on a `logical_size` output into
/// even-aligned `videocrop` margins for a `frame_size` video stream.
pub fn region_to_videocrop(
    rect: Rect,
    logical_size: (i32, i32),
    frame_size: (u32, u32),
) -> Option<VideoCrop> {
    if logical_size.0 <= 0 || logical_size.1 <= 0 {
        return None;
    }
    let p = rect.to_pixels(
        logical_size.0 as f32,
        logical_size.1 as f32,
        frame_size.0,
        frame_size.1,
    )?;
    VideoCrop::from_pixel_rect(p, frame_size.0, frame_size.1)
}

/// Supported H.264 encoders in preference order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoEncoder {
    Nvenc,
    Vaapi,
    X264,
}

impl VideoEncoder {
    pub const CANDIDATES: [Self; 3] = [Self::Nvenc, Self::Vaapi, Self::X264];

    pub fn element_name(self) -> &'static str {
        match self {
            Self::Nvenc => "nvh264enc",
            Self::Vaapi => "vaapih264enc",
            Self::X264 => "x264enc",
        }
    }

    pub fn gst_args(self) -> Vec<&'static str> {
        match self {
            Self::Nvenc => vec!["nvh264enc", "rc-mode=vbr", "zerolatency=true"],
            Self::Vaapi => vec!["vaapih264enc"],
            Self::X264 => vec!["x264enc", "speed-preset=veryfast", "tune=zerolatency"],
        }
    }
}

/// Select the highest-priority video encoder for which `probe` returns `true`.
pub fn select_encoder_with(mut probe: impl FnMut(&str) -> bool) -> Option<VideoEncoder> {
    VideoEncoder::CANDIDATES.into_iter().find(|e| probe(e.element_name()))
}

/// Supported audio encoders for MP4 muxing in preference order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioEncoder {
    Fdkaac,
    Faac,
    AvencAac,
    Opus,
}

impl AudioEncoder {
    pub const CANDIDATES: [Self; 4] = [Self::Fdkaac, Self::Faac, Self::AvencAac, Self::Opus];

    pub fn element_name(self) -> &'static str {
        match self {
            Self::Fdkaac => "fdkaacenc",
            Self::Faac => "faac",
            Self::AvencAac => "avenc_aac",
            Self::Opus => "opusenc",
        }
    }

    pub fn gst_args(self) -> Vec<&'static str> {
        match self {
            Self::Fdkaac => vec!["fdkaacenc", "!", "aacparse"],
            Self::Faac => vec!["faac", "!", "aacparse"],
            Self::AvencAac => vec!["avenc_aac", "!", "aacparse"],
            Self::Opus => vec!["opusenc"],
        }
    }
}

/// Select the highest-priority audio encoder for which `probe` returns `true`.
pub fn select_audio_encoder_with(mut probe: impl FnMut(&str) -> bool) -> Option<AudioEncoder> {
    AudioEncoder::CANDIDATES.into_iter().find(|e| probe(e.element_name()))
}

fn gst_inspect_exists(element: &str) -> bool {
    Command::new("gst-inspect-1.0")
        .arg(element)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn probe_video_encoder(element: &str) -> bool {
    if !gst_inspect_exists(element) {
        return false;
    }
    if element == "x264enc" {
        return true;
    }
    // Verify hardware encoders can actually initialize on this machine's GPU/driver.
    Command::new("gst-launch-1.0")
        .args([
            "-q",
            "videotestsrc",
            "num-buffers=1",
            "!",
            "video/x-raw,width=64,height=64,format=NV12",
            "!",
            element,
            "!",
            "fakesink",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

pub fn select_encoder() -> Option<VideoEncoder> {
    select_encoder_with(probe_video_encoder)
}

pub fn select_audio_encoder() -> Option<AudioEncoder> {
    select_audio_encoder_with(gst_inspect_exists)
}

/// Resolve the PulseAudio/PipeWire device name for the chosen audio source.
pub fn resolve_audio_device(source: AudioSource) -> Option<String> {
    match source {
        AudioSource::None => None,
        AudioSource::System => {
            let sink = pactl_stdout(&["get-default-sink"]);
            let sources = pactl_stdout(&["list", "short", "sources"]);
            if let Some(sink) = sink {
                let candidate = format!("{sink}.monitor");
                if sources
                    .as_deref()
                    .is_none_or(|list| list.lines().any(|l| l.split_whitespace().nth(1) == Some(&candidate)))
                {
                    return Some(candidate);
                }
            }
            sources.and_then(|list| {
                list.lines()
                    .filter_map(|l| l.split_whitespace().nth(1))
                    .find(|name| name.ends_with(".monitor"))
                    .map(str::to_owned)
            })
        }
        AudioSource::Microphone => pactl_stdout(&["get-default-source"]),
    }
}

fn pactl_stdout(args: &[&str]) -> Option<String> {
    let out = Command::new("pactl")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!s.is_empty()).then_some(s)
}

/// Build the `gst-launch-1.0` argument list for recording.
pub fn build_pipeline_args(
    pw_fd: RawFd,
    node_id: u32,
    encoder: VideoEncoder,
    crop: Option<VideoCrop>,
    audio: Option<(Option<&str>, AudioEncoder)>,
    output_path: &Path,
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-e".into(),
        "pipewiresrc".into(),
        format!("fd={pw_fd}"),
        format!("path={node_id}"),
        "do-timestamp=true".into(),
        "keepalive-time=1000".into(),
        "!".into(),
        "videorate".into(),
        "!".into(),
        "video/x-raw,framerate=30/1".into(),
        "!".into(),
        "videoconvert".into(),
    ];

    if let Some(c) = crop.filter(|c| !c.is_noop()) {
        args.extend([
            "!".into(),
            "videocrop".into(),
            format!("top={}", c.top),
            format!("bottom={}", c.bottom),
            format!("left={}", c.left),
            format!("right={}", c.right),
        ]);
    }

    args.extend([
        "!".into(),
        "videoconvert".into(),
        "!".into(),
        "video/x-raw,format=NV12".into(),
        "!".into(),
        "queue".into(),
        "!".into(),
    ]);
    args.extend(encoder.gst_args().into_iter().map(str::to_owned));
    args.extend([
        "!".into(),
        "h264parse".into(),
        "!".into(),
        "queue".into(),
        "!".into(),
        "mp4mux".into(),
        "name=mux".into(),
        "!".into(),
        "filesink".into(),
        format!("location={}", output_path.display()),
    ]);

    if let Some((device, audio_enc)) = audio {
        args.push("pulsesrc".into());
        if let Some(dev) = device.filter(|d| !d.is_empty()) {
            args.push(format!("device={dev}"));
        }
        args.extend([
            "do-timestamp=true".into(),
            "!".into(),
            "audioconvert".into(),
            "!".into(),
            "audioresample".into(),
            "!".into(),
            "queue".into(),
            "!".into(),
        ]);
        args.extend(audio_enc.gst_args().into_iter().map(str::to_owned));
        args.extend(["!".into(), "queue".into(), "!".into(), "mux.".into()]);
    }

    args
}

/// Pick the portal stream matching `output` (via `mapping_id` or `logical_size`),
/// falling back to the first stream.
fn pick_stream<'a>(
    streams: &'a [Stream],
    output: &str,
    logical_size: (i32, i32),
) -> Option<&'a Stream> {
    streams
        .iter()
        .find(|s| s.mapping_id() == Some(output))
        .or_else(|| streams.iter().find(|s| s.size() == Some(logical_size)))
        .or_else(|| streams.first())
}

struct PortalSession {
    proxy: Screencast<'static>,
    session: Session<'static, Screencast<'static>>,
    stream: Stream,
    pw_fd: OwnedFd,
}

async fn open_screencast(
    target: &RecordTarget,
    options: &mut Options,
) -> Result<PortalSession, Error> {
    let cursor_mode = if options.show_pointer {
        CursorMode::Embedded
    } else {
        CursorMode::Hidden
    };
    let source_types = SourceType::Monitor.into();

    let stored_token = options.restore_token.clone();
    let res = open_screencast_once(cursor_mode, source_types, stored_token.as_deref(), target).await;

    let (portal, new_token) = match res {
        Ok(ok) => ok,
        Err(AshpdError::Response(ResponseError::Cancelled)) => {
            return Err(Error::Msg("screen recording cancelled".into()));
        }
        Err(e) if stored_token.is_some() => {
            tracing::warn!("stored screencast restore_token failed ({e}); retrying without token");
            options.restore_token = None;
            options.save();
            match open_screencast_once(cursor_mode, source_types, None, target).await {
                Ok(ok) => ok,
                Err(AshpdError::Response(ResponseError::Cancelled)) => {
                    return Err(Error::Msg("screen recording cancelled".into()));
                }
                Err(e) => return Err(Error::Msg(format!("screencast portal: {e}"))),
            }
        }
        Err(e) => return Err(Error::Msg(format!("screencast portal: {e}"))),
    };

    if let Some(tok) = new_token.filter(|t| !t.is_empty())
        && options.restore_token.as_deref() != Some(tok.as_str())
    {
        options.restore_token = Some(tok);
        options.save();
    }

    Ok(portal)
}

async fn open_screencast_once(
    cursor_mode: CursorMode,
    source_types: ashpd::enumflags2::BitFlags<SourceType>,
    restore_token: Option<&str>,
    target: &RecordTarget,
) -> Result<(PortalSession, Option<String>), AshpdError> {
    let proxy = Screencast::new().await?;
    let session = proxy.create_session().await?;
    proxy
        .select_sources(
            &session,
            cursor_mode,
            source_types,
            false,
            restore_token,
            PersistMode::ExplicitlyRevoked,
        )
        .await?
        .response()?;

    let response = proxy.start(&session, None).await?.response()?;
    let new_token = response.restore_token().map(str::to_owned);
    let stream = pick_stream(response.streams(), &target.output, target.logical_size)
        .cloned()
        .ok_or_else(|| AshpdError::Response(ResponseError::Other))?;
    let pw_fd = proxy.open_pipe_wire_remote(&session).await?;
    let _ = &proxy;
    Ok((
        PortalSession { proxy, session, stream, pw_fd },
        new_token,
    ))
}

/// Active screen recording handle.
pub struct Recorder {
    rt: tokio::runtime::Runtime,
    portal: PortalSession,
    child: Child,
    output_path: PathBuf,
}

impl Recorder {
    /// Start a screen recording session and launch the GStreamer pipeline.
    pub fn start(target: &RecordTarget, mut options: Options) -> Result<Self, Error> {
        let encoder = select_encoder().ok_or_else(|| {
            Error::Msg(
                "no GStreamer H.264 encoder found (tried nvh264enc, vaapih264enc, x264enc)".into(),
            )
        })?;

        let audio = if options.audio == AudioSource::None {
            None
        } else {
            let audio_enc = select_audio_encoder().ok_or_else(|| {
                Error::Msg("no GStreamer audio encoder found for MP4 recording".into())
            })?;
            let dev = resolve_audio_device(options.audio);
            Some((dev, audio_enc))
        };

        let dir = output::recordings_dir();
        std::fs::create_dir_all(&dir)?;
        let file_name = output::recording_file_name(&output::timestamp(), None);
        let output_path = output::unique_path(&dir, &file_name);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let portal = rt.block_on(open_screencast(target, &mut options))?;

        let frame_size = target.pixel_size;
        let crop = target
            .rect
            .and_then(|r| region_to_videocrop(r, target.logical_size, frame_size));
        if target.rect.is_some() && crop.is_none() {
            let _ = rt.block_on(portal.session.close());
            return Err(Error::Msg("selected recording region is empty".into()));
        }

        let raw_fd = portal.pw_fd.as_raw_fd();
        let node_id = portal.stream.pipe_wire_node_id();
        let audio_ref = audio.as_ref().map(|(d, e)| (d.as_deref(), *e));
        let args = build_pipeline_args(raw_fd, node_id, encoder, crop, audio_ref, &output_path);

        tracing::info!(
            encoder = encoder.element_name(),
            node_id,
            path = %output_path.display(),
            "starting gst-launch-1.0 recording"
        );

        let mut cmd = Command::new("gst-launch-1.0");
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        // Clear FD_CLOEXEC only in the forked child before exec so `raw_fd` is
        // passed to `gst-launch-1.0` without leaking to unrelated processes.
        unsafe {
            cmd.pre_exec(move || {
                let borrowed = BorrowedFd::borrow_raw(raw_fd);
                fcntl_setfd(borrowed, FdFlags::empty())
                    .map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))?;
                Ok(())
            });
        }
        let mut child = cmd.spawn().map_err(|e| {
            let _ = rt.block_on(portal.session.close());
            Error::Msg(format!("failed to start gst-launch-1.0: {e}"))
        })?;

        // Catch immediate pipeline startup failures (e.g. missing plugin / bad caps).
        std::thread::sleep(Duration::from_millis(150));
        if let Some(status) = child.try_wait()? {
            let mut err_txt = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                use std::io::Read;
                let _ = stderr.read_to_string(&mut err_txt);
            }
            let _ = rt.block_on(portal.session.close());
            let _ = std::fs::remove_file(&output_path);
            let detail = err_txt.trim();
            return Err(Error::Msg(if detail.is_empty() {
                format!("gst-launch-1.0 exited early ({status})")
            } else {
                format!("gst-launch-1.0 failed: {detail}")
            }));
        }

        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut std::io::BufReader::new(stderr), &mut std::io::sink());
            });
        }

        let _ = &portal.proxy;
        Ok(Self { rt, portal, child, output_path })
    }

    /// Check if `gst-launch-1.0` exited unexpectedly while recording.
    pub fn try_wait(&mut self) -> Option<Result<(), Error>> {
        match self.child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    Some(Ok(()))
                } else {
                    Some(Err(Error::Msg(format!("recording stopped unexpectedly ({status})"))))
                }
            }
            Ok(None) => None,
            Err(e) => Some(Err(Error::Io(e))),
        }
    }

    /// Stop recording cleanly by sending `SIGINT` so `gst-launch-1.0 -e`
    /// flushes EOS and finalizes the MP4 container.
    pub fn stop(mut self) -> Result<PathBuf, Error> {
        if self.child.try_wait()?.is_none() {
            let pid = Pid::from_child(&self.child);
            let _ = kill_process(pid, Signal::INT);
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if self.child.try_wait()?.is_some() {
                    break;
                }
                if Instant::now() >= deadline {
                    tracing::warn!("gst-launch-1.0 did not exit on SIGINT; terminating");
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }

        let _ = self.rt.block_on(self.portal.session.close());

        let meta = std::fs::metadata(&self.output_path).map_err(|e| {
            Error::Msg(format!("recording file missing ({}): {e}", self.output_path.display()))
        })?;
        if meta.len() == 0 {
            let _ = std::fs::remove_file(&self.output_path);
            return Err(Error::Msg("recording produced an empty file".into()));
        }
        Ok(self.output_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn videocrop_preserves_even_rects() {
        let crop = region_to_videocrop(
            Rect::new(10.0, 20.0, 100.0, 50.0),
            (1920, 1080),
            (1920, 1080),
        )
        .unwrap();
        assert_eq!(crop, VideoCrop { top: 20, bottom: 1010, left: 10, right: 1810 });
        assert_eq!(1920 - crop.left - crop.right, 100);
        assert_eq!(1080 - crop.top - crop.bottom, 50);
    }

    #[test]
    fn videocrop_aligns_odd_coords_and_sizes_to_even_pixels() {
        let crop = VideoCrop::from_pixel_rect(
            PixelRect { x: 21, y: 15, w: 101, h: 51 },
            1920,
            1080,
        )
        .unwrap();
        assert_eq!(crop.left, 20);
        assert_eq!(crop.top, 14);
        assert_eq!(crop.right, 1920 - 122);
        assert_eq!(crop.bottom, 1080 - 66);
        let cropped_w = 1920 - crop.left - crop.right;
        let cropped_h = 1080 - crop.top - crop.bottom;
        assert_eq!(cropped_w % 2, 0);
        assert_eq!(cropped_h % 2, 0);
        assert!(cropped_w >= 101);
        assert!(cropped_h >= 51);
    }

    #[test]
    fn videocrop_handles_hidpi_edges_and_1px_selections() {
        // 1.5x HiDPI: logical 1280x720 -> physical 1920x1080.
        let crop = region_to_videocrop(
            Rect::new(100.0, 100.0, 200.0, 100.0),
            (1280, 720),
            (1920, 1080),
        )
        .unwrap();
        assert_eq!(crop.left, 150);
        assert_eq!(crop.top, 150);
        assert_eq!(1920 - crop.left - crop.right, 300);
        assert_eq!(1080 - crop.top - crop.bottom, 150);

        // 1px selection at the far bottom-right edge still produces >=2x2 even dims.
        let tiny = VideoCrop::from_pixel_rect(
            PixelRect { x: 1919, y: 1079, w: 1, h: 1 },
            1920,
            1080,
        )
        .unwrap();
        assert_eq!(1920 - tiny.left - tiny.right, 2);
        assert_eq!(1080 - tiny.top - tiny.bottom, 2);

        // Odd frame dimensions still yield even cropped width & height.
        let odd_frame = VideoCrop::from_pixel_rect(
            PixelRect { x: 0, y: 0, w: 1365, h: 767 },
            1365,
            767,
        )
        .unwrap();
        assert_eq!((1365 - odd_frame.left - odd_frame.right) % 2, 0);
        assert_eq!((767 - odd_frame.top - odd_frame.bottom) % 2, 0);
    }

    #[test]
    fn videocrop_rejects_empty_or_out_of_bounds() {
        assert!(region_to_videocrop(Rect::new(0.0, 0.0, 0.0, 10.0), (100, 100), (100, 100)).is_none());
        assert!(region_to_videocrop(Rect::new(200.0, 0.0, 10.0, 10.0), (100, 100), (100, 100)).is_none());
        assert!(VideoCrop::from_pixel_rect(PixelRect { x: 0, y: 0, w: 1, h: 1 }, 1, 1).is_none());
    }

    #[test]
    fn encoder_selection_prefers_nvenc_then_vaapi_then_x264() {
        assert_eq!(select_encoder_with(|_| true), Some(VideoEncoder::Nvenc));
        assert_eq!(
            select_encoder_with(|e| e == "vaapih264enc" || e == "x264enc"),
            Some(VideoEncoder::Vaapi)
        );
        assert_eq!(
            select_encoder_with(|e| e == "x264enc"),
            Some(VideoEncoder::X264)
        );
        assert_eq!(select_encoder_with(|_| false), None);

        assert_eq!(
            select_audio_encoder_with(|e| e == "avenc_aac" || e == "opusenc"),
            Some(AudioEncoder::AvencAac)
        );
        assert_eq!(
            select_audio_encoder_with(|e| e == "opusenc"),
            Some(AudioEncoder::Opus)
        );
    }

    #[test]
    fn pipeline_args_include_expected_elements() {
        let out = Path::new("/tmp/Screen Recording 2026-11-15 at 14.03.09.mp4");
        let args = build_pipeline_args(
            17,
            42,
            VideoEncoder::X264,
            Some(VideoCrop { top: 10, bottom: 20, left: 30, right: 40 }),
            Some((Some("alsa_output.monitor"), AudioEncoder::AvencAac)),
            out,
        );
        let joined = args.join(" ");
        assert!(args.first().map(String::as_str) == Some("-e"));
        assert!(joined.contains("pipewiresrc fd=17 path=42"));
        assert!(joined.contains("videorate ! video/x-raw,framerate=30/1"));
        assert!(joined.contains("videocrop top=10 bottom=20 left=30 right=40"));
        assert!(joined.contains("x264enc speed-preset=veryfast"));
        assert!(joined.contains("mp4mux name=mux ! filesink"));
        assert!(joined.contains("pulsesrc device=alsa_output.monitor"));
        assert!(joined.contains("avenc_aac ! aacparse"));
    }
}
