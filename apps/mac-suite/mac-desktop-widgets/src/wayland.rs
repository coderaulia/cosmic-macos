// SPDX-License-Identifier: GPL-3.0-only
//! Wayland toplevel focus tracker so desktop widgets dim slightly when a
//! normal application window is focused (like macOS Sonoma).

use std::collections::HashSet;

use cctk::{
    cosmic_protocols::toplevel_info::v1::client::zcosmic_toplevel_handle_v1::State as ToplevelState,
    sctk::{
        self,
        output::{OutputHandler, OutputState},
        reexports::{calloop, calloop_wayland_source::WaylandSource},
        registry::{ProvidesRegistryState, RegistryState},
    },
    toplevel_info::{ToplevelInfoHandler, ToplevelInfoState},
    wayland_client::{
        Connection, QueueHandle,
        globals::registry_queue_init,
        protocol::wl_output,
    },
    wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
};
use cosmic::iced::{
    self, Subscription,
    futures::{SinkExt, StreamExt, channel::mpsc},
    stream,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusEvent {
    NormalWindowFocused(bool),
}

pub fn focus_subscription() -> Subscription<FocusEvent> {
    Subscription::run_with(std::any::TypeId::of::<FocusEvent>(), |_| {
        stream::channel(16, |mut output: mpsc::Sender<FocusEvent>| async move {
            let (tx, mut rx) = mpsc::unbounded();
            std::thread::spawn(move || run_wayland_focus_loop(tx));
            while let Some(evt) = rx.next().await {
                if output.send(evt).await.is_err() {
                    break;
                }
            }
            iced::futures::future::pending::<()>().await;
        })
    })
}

struct AppData {
    tx: mpsc::UnboundedSender<FocusEvent>,
    registry_state: RegistryState,
    output_state: OutputState,
    toplevel_info_state: ToplevelInfoState,
    activated: HashSet<ExtForeignToplevelHandleV1>,
    last_sent: Option<bool>,
}

impl AppData {
    fn emit_if_changed(&mut self) {
        let focused = !self.activated.is_empty();
        if self.last_sent != Some(focused) {
            self.last_sent = Some(focused);
            let _ = self
                .tx
                .unbounded_send(FocusEvent::NormalWindowFocused(focused));
        }
    }

    fn sync_toplevel(&mut self, toplevel: &ExtForeignToplevelHandleV1) {
        let is_active = self.toplevel_info_state.info(toplevel).is_some_and(|info| {
            info.state.contains(&ToplevelState::Activated)
                && !info.state.contains(&ToplevelState::Minimized)
        });
        if is_active {
            self.activated.insert(toplevel.clone());
        } else {
            self.activated.remove(toplevel);
        }
        self.emit_if_changed();
    }
}

impl ProvidesRegistryState for AppData {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    sctk::registry_handlers![OutputState];
}

impl OutputHandler for AppData {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ToplevelInfoHandler for AppData {
    fn toplevel_info_state(&mut self) -> &mut ToplevelInfoState {
        &mut self.toplevel_info_state
    }

    fn new_toplevel(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        toplevel: &ExtForeignToplevelHandleV1,
    ) {
        self.sync_toplevel(toplevel);
    }

    fn update_toplevel(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        toplevel: &ExtForeignToplevelHandleV1,
    ) {
        self.sync_toplevel(toplevel);
    }

    fn toplevel_closed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        toplevel: &ExtForeignToplevelHandleV1,
    ) {
        self.activated.remove(toplevel);
        self.emit_if_changed();
    }
}

fn run_wayland_focus_loop(tx: mpsc::UnboundedSender<FocusEvent>) {
    let Ok(conn) = Connection::connect_to_env() else {
        tracing::warn!("Wayland connection unavailable for focus tracker");
        return;
    };
    let Ok((globals, event_queue)) = registry_queue_init::<AppData>(&conn) else {
        return;
    };
    let Ok(mut event_loop) = calloop::EventLoop::<AppData>::try_new() else {
        return;
    };
    let qh = event_queue.handle();
    if WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .is_err()
    {
        return;
    }

    let registry_state = RegistryState::new(&globals);
    let mut data = AppData {
        tx,
        output_state: OutputState::new(&globals, &qh),
        toplevel_info_state: ToplevelInfoState::new(&registry_state, &qh),
        registry_state,
        activated: HashSet::new(),
        last_sent: None,
    };

    loop {
        if event_loop.dispatch(None, &mut data).is_err() {
            break;
        }
    }
}

sctk::delegate_registry!(AppData);
sctk::delegate_output!(AppData);
cctk::delegate_toplevel_info!(AppData);
