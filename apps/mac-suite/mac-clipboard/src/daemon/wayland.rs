// SPDX-License-Identifier: GPL-3.0-only

//! Wayland integration for the daemon: tracks the focused application via
//! `ext-foreign-toplevel-list` / `zcosmic-toplevel-info` and sends `Ctrl+V`
//! (or `Ctrl+Shift+V` for terminals) via `zwp_virtual_keyboard_v1`.

use std::collections::HashMap;
use std::io::Write;
use std::os::fd::AsFd;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use cosmic::cctk::{
    self,
    cosmic_protocols::toplevel_info::v1::client::zcosmic_toplevel_handle_v1::State as CosmicState,
    sctk::{
        self,
        reexports::{calloop, calloop_wayland_source::WaylandSource},
        registry::{ProvidesRegistryState, RegistryState},
        seat::{SeatHandler, SeatState},
    },
    toplevel_info::{ToplevelInfo, ToplevelInfoHandler, ToplevelInfoState},
    wayland_client::{
        Connection, Dispatch, Proxy, QueueHandle,
        globals::registry_queue_init,
        protocol::{wl_keyboard::KeymapFormat, wl_seat::WlSeat},
    },
    wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
};
use rustix::fs::MemfdFlags;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};

const KEY_V_EVDEV: u32 = 47;
const MOD_SHIFT: u32 = 1;
const MOD_CONTROL: u32 = 4;

const MINIMAL_XKB_KEYMAP: &str = r#"xkb_keymap {
  xkb_keycodes "evdev" {
    minimum = 8;
    maximum = 255;
    <LCTL> = 37;
    <LFSH> = 50;
    <AB04> = 55;
  };
  xkb_types "complete" {
    virtual_modifiers NumLock;
    type "ONE_LEVEL" { modifiers = None; map[None] = Level1; level_name[Level1] = "Any"; };
    type "TWO_LEVEL" { modifiers = Shift; map[Shift] = Level2; level_name[Level1] = "Base"; level_name[Level2] = "Shift"; };
  };
  xkb_compatibility "complete" {
    interpret.useModMapMods = AnyLevel;
    interpret.repeat = False;
    interpret Control_L+AnyOf(all) { action = SetMods(modifiers=Control); };
    interpret Shift_L+AnyOf(all) { action = SetMods(modifiers=Shift); };
  };
  xkb_symbols "pc" {
    key <LCTL> { [ Control_L ] };
    key <LFSH> { [ Shift_L ] };
    key <AB04> { [ v, V ] };
    modifier_map Control { <LCTL> };
    modifier_map Shift { <LFSH> };
  };
};
"#;

#[derive(Debug)]
struct PasteRequest {
    terminal_apps: Vec<String>,
    reply: mpsc::Sender<bool>,
}

#[derive(Clone)]
pub struct WaylandHandle {
    focused_app: Arc<Mutex<Option<String>>>,
    paste_tx: Option<calloop::channel::Sender<PasteRequest>>,
}

impl WaylandHandle {
    /// Connects to the Wayland compositor and spawns the background event thread.
    /// Falls back to a no-op handle if no Wayland display is available.
    #[must_use]
    pub fn start() -> Self {
        let focused_app = Arc::new(Mutex::new(None));
        let (paste_tx, paste_rx) = calloop::channel::channel::<PasteRequest>();
        let (ready_tx, ready_rx) = mpsc::channel::<bool>();

        let focused_clone = Arc::clone(&focused_app);
        let spawned = std::thread::Builder::new()
            .name("clipboard-wayland".to_string())
            .spawn(move || run_wayland_loop(focused_clone, paste_rx, ready_tx));

        if spawned.is_ok() && ready_rx.recv_timeout(Duration::from_secs(2)).unwrap_or(false) {
            Self {
                focused_app,
                paste_tx: Some(paste_tx),
            }
        } else {
            Self {
                focused_app,
                paste_tx: None,
            }
        }
    }

    #[must_use]
    pub fn focused_app(&self) -> Option<String> {
        self.focused_app.lock().ok().and_then(|g| g.clone())
    }

    /// Sends `Ctrl+V` (or `Ctrl+Shift+V` for terminals) after waiting ~80 ms for
    /// focus to return to the target window. Returns `true` if sent via virtual keyboard.
    #[must_use]
    pub fn trigger_paste(&self, terminal_apps: &[String]) -> bool {
        let Some(tx) = &self.paste_tx else {
            return false;
        };
        std::thread::sleep(Duration::from_millis(80));
        let (reply_tx, reply_rx) = mpsc::channel();
        if tx
            .send(PasteRequest {
                terminal_apps: terminal_apps.to_vec(),
                reply: reply_tx,
            })
            .is_err()
        {
            return false;
        }
        reply_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap_or(false)
    }
}

/// Checks if `app_id` matches any configured terminal app id.
#[must_use]
pub fn is_terminal_app(app_id: Option<&str>, terminal_apps: &[String]) -> bool {
    let Some(app_id) = app_id.map(str::trim).filter(|s| !s.is_empty()) else {
        return false;
    };
    let normalized = app_id.strip_suffix(".desktop").unwrap_or(app_id);
    terminal_apps.iter().any(|entry| {
        let entry = entry.trim();
        let entry_norm = entry.strip_suffix(".desktop").unwrap_or(entry);
        !entry_norm.is_empty() && normalized.eq_ignore_ascii_case(entry_norm)
    })
}

use cosmic::cctk::wayland_client::backend::ObjectId as WaylandObjectId;

struct WaylandState {
    conn: Connection,
    qh: QueueHandle<Self>,
    registry_state: RegistryState,
    seat_state: SeatState,
    toplevel_info_state: ToplevelInfoState,
    virtual_keyboard_manager: Option<ZwpVirtualKeyboardManagerV1>,
    toplevels: HashMap<WaylandObjectId, ToplevelInfo>,
    focused_app: Arc<Mutex<Option<String>>>,
    start_time: Instant,
    exit: bool,
}

impl WaylandState {
    fn refresh_focused_app(&self) {
        let active = self
            .toplevels
            .values()
            .find(|info| {
                info.state.contains(&CosmicState::Activated)
                    && !info.app_id.eq_ignore_ascii_case("dev.asw.MacClipboard")
            })
            .map(|info| info.app_id.clone())
            .filter(|id| !id.is_empty());

        if let Ok(mut guard) = self.focused_app.lock() {
            *guard = active;
        }
    }

    fn send_paste_shortcut(&self, terminal_apps: &[String]) -> bool {
        let Some(vk_mgr) = &self.virtual_keyboard_manager else {
            return false;
        };
        let Some(seat) = self.seat_state.seats().next() else {
            return false;
        };

        let keymap_bytes = MINIMAL_XKB_KEYMAP.as_bytes();
        let Ok(fd) = rustix::fs::memfd_create("mac-clipboard-keymap", MemfdFlags::CLOEXEC) else {
            return false;
        };
        let mut file = std::fs::File::from(fd);
        if file.write_all(keymap_bytes).is_err() {
            return false;
        }

        let vk = vk_mgr.create_virtual_keyboard(&seat, &self.qh, ());
        vk.keymap(
            u32::from(KeymapFormat::XkbV1),
            file.as_fd(),
            keymap_bytes.len() as u32,
        );

        let focused = self.focused_app.lock().ok().and_then(|g| g.clone());
        let mods = if is_terminal_app(focused.as_deref(), terminal_apps) {
            MOD_CONTROL | MOD_SHIFT
        } else {
            MOD_CONTROL
        };

        let ts = self.start_time.elapsed().as_millis() as u32;
        vk.modifiers(mods, 0, 0, 0);
        vk.key(ts, KEY_V_EVDEV, 1);
        vk.key(ts.saturating_add(5), KEY_V_EVDEV, 0);
        vk.modifiers(0, 0, 0, 0);
        let _ = self.conn.flush();
        vk.destroy();
        let _ = self.conn.flush();
        true
    }
}

fn run_wayland_loop(
    focused_app: Arc<Mutex<Option<String>>>,
    paste_rx: calloop::channel::Channel<PasteRequest>,
    ready_tx: mpsc::Sender<bool>,
) {
    let Ok(conn) = Connection::connect_to_env() else {
        let _ = ready_tx.send(false);
        return;
    };
    let Ok((globals, event_queue)) = registry_queue_init::<WaylandState>(&conn) else {
        let _ = ready_tx.send(false);
        return;
    };
    let Ok(mut event_loop) = calloop::EventLoop::<WaylandState>::try_new() else {
        let _ = ready_tx.send(false);
        return;
    };

    let qh = event_queue.handle();
    let registry_state = RegistryState::new(&globals);
    let seat_state = SeatState::new(&globals, &qh);
    let toplevel_info_state = ToplevelInfoState::new(&registry_state, &qh);
    let virtual_keyboard_manager = globals
        .bind::<ZwpVirtualKeyboardManagerV1, _, _>(&qh, 1..=1, ())
        .ok();

    let handle = event_loop.handle();
    let wayland_source = WaylandSource::new(conn.clone(), event_queue);
    if wayland_source.insert(handle.clone()).is_err() {
        let _ = ready_tx.send(false);
        return;
    }

    if handle
        .insert_source(paste_rx, |event, (), state| match event {
            calloop::channel::Event::Msg(req) => {
                let ok = state.send_paste_shortcut(&req.terminal_apps);
                let _ = req.reply.send(ok);
            }
            calloop::channel::Event::Closed => {
                state.exit = true;
            }
        })
        .is_err()
    {
        let _ = ready_tx.send(false);
        return;
    }

    let mut state = WaylandState {
        conn,
        qh,
        registry_state,
        seat_state,
        toplevel_info_state,
        virtual_keyboard_manager,
        toplevels: HashMap::new(),
        focused_app,
        start_time: Instant::now(),
        exit: false,
    };

    let _ = ready_tx.send(true);

    while !state.exit {
        if event_loop.dispatch(None, &mut state).is_err() {
            break;
        }
    }
}

impl ProvidesRegistryState for WaylandState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    sctk::registry_handlers!(SeatState);
}

impl SeatHandler for WaylandState {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: WlSeat,
        _: sctk::seat::Capability,
    ) {
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: WlSeat,
        _: sctk::seat::Capability,
    ) {
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
}

impl ToplevelInfoHandler for WaylandState {
    fn toplevel_info_state(&mut self) -> &mut ToplevelInfoState {
        &mut self.toplevel_info_state
    }

    fn new_toplevel(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        toplevel: &ExtForeignToplevelHandleV1,
    ) {
        if let Some(info) = self.toplevel_info_state.info(toplevel) {
            self.toplevels.insert(toplevel.id(), info.clone());
            self.refresh_focused_app();
        }
    }

    fn update_toplevel(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        toplevel: &ExtForeignToplevelHandleV1,
    ) {
        if let Some(info) = self.toplevel_info_state.info(toplevel) {
            self.toplevels.insert(toplevel.id(), info.clone());
            self.refresh_focused_app();
        }
    }

    fn toplevel_closed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        toplevel: &ExtForeignToplevelHandleV1,
    ) {
        self.toplevels.remove(&toplevel.id());
        self.refresh_focused_app();
    }
}

impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for WaylandState {
    fn event(
        _: &mut Self,
        _: &ZwpVirtualKeyboardManagerV1,
        _: <ZwpVirtualKeyboardManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardV1, ()> for WaylandState {
    fn event(
        _: &mut Self,
        _: &ZwpVirtualKeyboardV1,
        _: <ZwpVirtualKeyboardV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

sctk::delegate_registry!(WaylandState);
sctk::delegate_seat!(WaylandState);
cctk::delegate_toplevel_info!(WaylandState);
