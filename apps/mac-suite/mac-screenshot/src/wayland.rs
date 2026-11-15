// SPDX-License-Identifier: GPL-3.0-only

//! Screen capture through COSMIC's ext-image-copy-capture screencopy,
//! plus toplevel geometry for window selection.
//!
//! Everything here is synchronous: connect, roundtrip, copy every output into
//! an shm buffer, return RGBA images. It runs once before the overlay opens
//! (frozen screen) or after the timer expires.

use std::collections::HashSet;
use std::os::fd::AsFd;
use std::sync::{Arc, Mutex};

use cctk::{
    screencopy::{
        CaptureFrame, CaptureOptions, CaptureSession, CaptureSource, FailureReason, Formats, Frame,
        ScreencopyFrameData, ScreencopyFrameDataExt, ScreencopyHandler, ScreencopySessionData,
        ScreencopySessionDataExt, ScreencopyState,
    },
    sctk::{
        self,
        output::{OutputHandler, OutputState},
        registry::{ProvidesRegistryState, RegistryState},
        shm::{Shm, ShmHandler},
    },
    toplevel_info::{ToplevelInfoHandler, ToplevelInfoState},
    wayland_client::{
        Connection, Dispatch, QueueHandle, WEnum,
        globals::registry_queue_init,
        protocol::{wl_buffer, wl_output, wl_shm, wl_shm_pool},
    },
    wayland_protocols::ext::{
        foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
        workspace::v1::client::{
            ext_workspace_handle_v1::{ExtWorkspaceHandleV1, State as WorkspaceFlags},
        },
    },
    workspace::{WorkspaceHandler, WorkspaceState},
};
use cctk::cosmic_protocols::toplevel_info::v1::client::zcosmic_toplevel_handle_v1;
use image::RgbaImage;

use crate::geometry::Rect;
use crate::output::Error;

#[derive(Clone, Debug)]
pub struct WindowInfo {
    /// Output-local logical coordinates.
    pub rect: Rect,
}

pub struct OutputShot {
    pub name: String,
    pub logical_size: (i32, i32),
    pub image: RgbaImage,
    pub windows: Vec<WindowInfo>,
}

pub struct Snapshot {
    pub outputs: Vec<OutputShot>,
}

#[derive(Default)]
struct SessionInner {
    formats: Option<Formats>,
    result: Option<Result<(), String>>,
}

#[derive(Default)]
struct SessionData {
    inner: Arc<Mutex<SessionInner>>,
    session_data: ScreencopySessionData,
}

impl ScreencopySessionDataExt for SessionData {
    fn screencopy_session_data(&self) -> &ScreencopySessionData {
        &self.session_data
    }
}

struct FrameData {
    frame_data: ScreencopyFrameData,
    inner: Arc<Mutex<SessionInner>>,
}

impl ScreencopyFrameDataExt for FrameData {
    fn screencopy_frame_data(&self) -> &ScreencopyFrameData {
        &self.frame_data
    }
}

struct State {
    registry_state: RegistryState,
    output_state: OutputState,
    shm: Shm,
    screencopy: ScreencopyState,
    toplevels: Option<ToplevelInfoState>,
    workspaces: WorkspaceState,
    active_workspaces: HashSet<ExtWorkspaceHandleV1>,
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    sctk::registry_handlers!();
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl WorkspaceHandler for State {
    fn workspace_state(&mut self) -> &mut WorkspaceState {
        &mut self.workspaces
    }

    fn done(&mut self) {
        self.active_workspaces = self
            .workspaces
            .workspaces()
            .filter(|w| w.state.contains(WorkspaceFlags::Active))
            .map(|w| w.handle.clone())
            .collect();
    }
}

impl ToplevelInfoHandler for State {
    fn toplevel_info_state(&mut self) -> &mut ToplevelInfoState {
        // Only dispatched when the globals were bound in `try_new`.
        self.toplevels.as_mut().expect("toplevel info bound")
    }
    fn new_toplevel(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &ExtForeignToplevelHandleV1) {}
    fn update_toplevel(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &ExtForeignToplevelHandleV1) {}
    fn toplevel_closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &ExtForeignToplevelHandleV1) {}
}

impl ScreencopyHandler for State {
    fn screencopy_state(&mut self) -> &mut ScreencopyState {
        &mut self.screencopy
    }

    fn init_done(&mut self, _: &Connection, _: &QueueHandle<Self>, session: &CaptureSession, formats: &Formats) {
        if let Some(d) = session.data::<SessionData>() {
            d.inner.lock().unwrap().formats = Some(formats.clone());
        }
    }

    fn stopped(&mut self, _: &Connection, _: &QueueHandle<Self>, session: &CaptureSession) {
        if let Some(d) = session.data::<SessionData>() {
            let mut g = d.inner.lock().unwrap();
            g.result.get_or_insert(Err("capture session stopped".into()));
            // Unblock a wait for formats too.
            g.formats.get_or_insert_with(Formats::default);
        }
    }

    fn ready(&mut self, _: &Connection, _: &QueueHandle<Self>, frame: &CaptureFrame, _: Frame) {
        if let Some(d) = frame.data::<FrameData>() {
            d.inner.lock().unwrap().result = Some(Ok(()));
        }
    }

    fn failed(&mut self, _: &Connection, _: &QueueHandle<Self>, frame: &CaptureFrame, reason: WEnum<FailureReason>) {
        if let Some(d) = frame.data::<FrameData>() {
            d.inner.lock().unwrap().result = Some(Err(format!("capture failed: {reason:?}")));
        }
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for State {
    fn event(_: &mut Self, _: &wl_shm_pool::WlShmPool, _: wl_shm_pool::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_buffer::WlBuffer, ()> for State {
    fn event(_: &mut Self, _: &wl_buffer::WlBuffer, _: wl_buffer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

sctk::delegate_registry!(State);
sctk::delegate_output!(State);
sctk::delegate_shm!(State);
cctk::delegate_toplevel_info!(State);
cctk::delegate_workspace!(State);
cctk::delegate_screencopy!(State);

/// Convert a raw shm frame to opaque RGBA. `None` for unsupported formats.
pub fn to_rgba(format: wl_shm::Format, width: u32, height: u32, raw: &[u8]) -> Option<RgbaImage> {
    let n = (width as usize) * (height as usize) * 4;
    if raw.len() < n {
        return None;
    }
    let swap = match format {
        wl_shm::Format::Abgr8888 | wl_shm::Format::Xbgr8888 => false,
        wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888 => true,
        _ => return None,
    };
    let mut px = raw[..n].to_vec();
    for p in px.chunks_exact_mut(4) {
        if swap {
            p.swap(0, 2);
        }
        p[3] = 255;
    }
    RgbaImage::from_raw(width, height, px)
}

/// Pick the first shm format we can convert.
fn pick_format(formats: &Formats) -> Option<wl_shm::Format> {
    [
        wl_shm::Format::Abgr8888,
        wl_shm::Format::Xbgr8888,
        wl_shm::Format::Argb8888,
        wl_shm::Format::Xrgb8888,
    ]
    .into_iter()
    .find(|f| formats.shm_formats.contains(f))
}

fn bad(msg: impl Into<String>) -> Error {
    Error::Msg(msg.into())
}

/// Capture every output. `with_windows` also collects toplevel rectangles.
pub fn snapshot(with_windows: bool, paint_cursor: bool) -> Result<Snapshot, Error> {
    let conn = Connection::connect_to_env().map_err(|e| bad(format!("wayland: {e}")))?;
    let (globals, mut queue) =
        registry_queue_init::<State>(&conn).map_err(|e| bad(format!("wayland: {e}")))?;
    let qh = queue.handle();

    let has_capture = globals.contents().with_list(|l| {
        l.iter().any(|g| g.interface == "ext_image_copy_capture_manager_v1")
            && l.iter().any(|g| g.interface == "ext_output_image_capture_source_manager_v1")
    });
    if !has_capture {
        return Err(bad("compositor lacks ext-image-copy-capture"));
    }

    let registry_state = RegistryState::new(&globals);
    let mut state = State {
        output_state: OutputState::new(&globals, &qh),
        shm: Shm::bind(&globals, &qh).map_err(|e| bad(format!("wl_shm: {e}")))?,
        screencopy: ScreencopyState::new(&globals, &qh),
        toplevels: if with_windows {
            ToplevelInfoState::try_new(&registry_state, &qh)
        } else {
            None
        },
        workspaces: WorkspaceState::new(&registry_state, &qh),
        active_workspaces: HashSet::new(),
        registry_state,
    };
    // Outputs, then their xdg-output / toplevel / workspace follow-ups.
    for _ in 0..3 {
        queue.roundtrip(&mut state).map_err(|e| bad(format!("wayland: {e}")))?;
    }

    let capturer = state.screencopy.capturer().clone();
    let outputs: Vec<wl_output::WlOutput> = state.output_state.outputs().collect();
    let mut shots = Vec::new();

    for (idx, output) in outputs.iter().enumerate() {
        let info = state.output_state.info(output);
        let name = info
            .as_ref()
            .and_then(|i| i.name.clone())
            .unwrap_or_else(|| format!("output-{idx}"));

        let inner = Arc::new(Mutex::new(SessionInner::default()));
        let options = if paint_cursor {
            CaptureOptions::PaintCursors
        } else {
            CaptureOptions::empty()
        };
        let session = capturer
            .create_session(
                &CaptureSource::Output(output.clone()),
                options,
                &qh,
                SessionData { inner: inner.clone(), session_data: ScreencopySessionData::default() },
            )
            .map_err(|e| bad(format!("capture source: {e:?}")))?;
        conn.flush().map_err(|e| bad(e.to_string()))?;

        while inner.lock().unwrap().formats.is_none() {
            queue.blocking_dispatch(&mut state).map_err(|e| bad(format!("wayland: {e}")))?;
        }
        let formats = inner.lock().unwrap().formats.clone().unwrap_or_default();
        let (w, h) = formats.buffer_size;
        let format = pick_format(&formats)
            .ok_or_else(|| bad(format!("no usable shm format in {:?}", formats.shm_formats)))?;
        if w == 0 || h == 0 {
            return Err(bad("empty capture buffer"));
        }

        let len = (w as usize) * (h as usize) * 4;
        let fd = rustix::fs::memfd_create("mac-screenshot", rustix::fs::MemfdFlags::CLOEXEC)
            .map_err(|e| bad(format!("memfd: {e}")))?;
        rustix::fs::ftruncate(&fd, len as u64).map_err(|e| bad(format!("ftruncate: {e}")))?;
        let pool = state.shm.wl_shm().create_pool(fd.as_fd(), len as i32, &qh, ());
        let buffer = pool.create_buffer(0, w as i32, h as i32, (w * 4) as i32, format, &qh, ());
        session.capture(
            &buffer,
            &[],
            &qh,
            FrameData { frame_data: ScreencopyFrameData::default(), inner: inner.clone() },
        );
        conn.flush().map_err(|e| bad(e.to_string()))?;

        loop {
            if let Some(res) = inner.lock().unwrap().result.clone() {
                res.map_err(bad)?;
                break;
            }
            queue.blocking_dispatch(&mut state).map_err(|e| bad(format!("wayland: {e}")))?;
        }

        // SAFETY: the memfd is private to this process and compositor writes
        // are finished (`ready`), so the mapping is read-only and stable.
        let map = unsafe { memmap2::Mmap::map(&fd) }.map_err(|e| bad(format!("mmap: {e}")))?;
        let image = to_rgba(format, w, h, &map).ok_or_else(|| bad("bad frame data"))?;
        pool.destroy();
        buffer.destroy();

        let logical_size = info
            .as_ref()
            .and_then(|i| i.logical_size)
            .unwrap_or((w as i32, h as i32));
        let windows = if with_windows { windows_on(&state, output) } else { Vec::new() };
        shots.push(OutputShot { name, logical_size, image, windows });
    }

    if shots.is_empty() {
        return Err(bad("no outputs"));
    }
    Ok(Snapshot { outputs: shots })
}

fn windows_on(state: &State, output: &wl_output::WlOutput) -> Vec<WindowInfo> {
    let Some(toplevels) = state.toplevels.as_ref() else { return Vec::new() };
    toplevels
        .toplevels()
        .filter(|t| !t.state.contains(&zcosmic_toplevel_handle_v1::State::Minimized))
        .filter(|t| {
            state.active_workspaces.is_empty()
                || t.workspace.is_empty()
                || t.workspace.iter().any(|w| state.active_workspaces.contains(w))
        })
        .filter_map(|t| {
            let g = t.geometry.get(output)?;
            (g.width > 0 && g.height > 0).then(|| WindowInfo {
                rect: Rect::new(g.x as f32, g.y as f32, g.width as f32, g.height as f32),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_formats() {
        let raw = [1u8, 2, 3, 0, 4, 5, 6, 0];
        let abgr = to_rgba(wl_shm::Format::Xbgr8888, 2, 1, &raw).unwrap();
        assert_eq!(abgr.as_raw(), &[1, 2, 3, 255, 4, 5, 6, 255]);
        let argb = to_rgba(wl_shm::Format::Argb8888, 2, 1, &raw).unwrap();
        assert_eq!(argb.as_raw(), &[3, 2, 1, 255, 6, 5, 4, 255]);
        assert!(to_rgba(wl_shm::Format::Rgb565, 2, 1, &raw).is_none());
        assert!(to_rgba(wl_shm::Format::Abgr8888, 4, 4, &raw).is_none());
    }
}
