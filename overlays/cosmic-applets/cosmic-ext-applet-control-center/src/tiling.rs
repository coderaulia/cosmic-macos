// SPDX-License-Identifier: GPL-3.0-only

use cctk::{
    sctk::{
        self,
        output::{OutputHandler, OutputState},
        reexports::{
            calloop::{self, channel::SyncSender},
            calloop_wayland_source::WaylandSource,
            client::{
                Connection, QueueHandle,
                globals::registry_queue_init,
                protocol::wl_output::WlOutput,
            },
            protocols::ext::workspace::v1::client::ext_workspace_handle_v1,
        },
        registry::{ProvidesRegistryState, RegistryState},
    },
    wayland_client::WEnum,
    workspace::{WorkspaceHandler, WorkspaceState},
};
use cosmic::iced::{
    self, Subscription,
    futures::{SinkExt, channel::mpsc},
    stream,
};
use cosmic_protocols::workspace::v2::client::zcosmic_workspace_handle_v2::TilingState;
use std::os::{
    fd::{FromRawFd, RawFd},
    unix::net::UnixStream,
};

#[derive(Debug, Clone)]
pub enum TilingEvent {
    Ready(SyncSender<TilingState>),
    State(TilingState),
}

pub fn tiling_subscription() -> Subscription<TilingEvent> {
    Subscription::run_with("control-center-tiling", |_| {
        stream::channel(16, |mut output: mpsc::Sender<TilingEvent>| async move {
            let (tx, mut rx) = mpsc::channel::<TilingState>(16);
            if let Some(req_tx) = spawn_wayland_tiling(tx) {
                let _ = output.send(TilingEvent::Ready(req_tx)).await;
                use cosmic::iced::futures::StreamExt;
                while let Some(state) = rx.next().await {
                    let _ = output.send(TilingEvent::State(state)).await;
                }
            }
            std::future::pending().await
        })
    })
}

fn spawn_wayland_tiling(tx: mpsc::Sender<TilingState>) -> Option<SyncSender<TilingState>> {
    let socket = std::env::var("X_PRIVILEGED_WAYLAND_SOCKET")
        .ok()
        .and_then(|fd| {
            fd.parse::<RawFd>()
                .ok()
                .map(|fd| unsafe { UnixStream::from_raw_fd(fd) })
        });
    let conn = if let Some(sock) = socket {
        Connection::from_socket(sock).ok()?
    } else {
        Connection::connect_to_env().ok()?
    };

    let (req_tx, req_rx) = calloop::channel::sync_channel::<TilingState>(32);
    std::thread::spawn(move || {
        let Ok(mut event_loop) = calloop::EventLoop::<WaylandTilingState>::try_new() else {
            return;
        };
        let Ok((globals, event_queue)) = registry_queue_init(&conn) else {
            return;
        };
        let qhandle = event_queue.handle();
        if WaylandSource::new(conn, event_queue)
            .insert(event_loop.handle())
            .is_err()
        {
            return;
        }
        let registry_state = RegistryState::new(&globals);
        let mut state = WaylandTilingState {
            output_state: OutputState::new(&globals, &qhandle),
            workspace_state: WorkspaceState::new(&registry_state, &qhandle),
            registry_state,
            configured_output: std::env::var("COSMIC_PANEL_OUTPUT").unwrap_or_default(),
            expected_output: None,
            tx,
            running: true,
        };
        let _ = event_loop.handle().insert_source(req_rx, |ev, (), state| {
            if let calloop::channel::Event::Msg(target_state) = ev
                && let Some(w) = state.active_workspace()
                && let Some(cosmic_handle) = &w.cosmic_handle
            {
                cosmic_handle.set_tiling_state(target_state);
                if let Ok(mgr) = state.workspace_state.workspace_manager().get() {
                    mgr.commit();
                }
            }
        });
        while state.running {
            if event_loop.dispatch(None, &mut state).is_err() {
                break;
            }
        }
    });
    Some(req_tx)
}

struct WaylandTilingState {
    output_state: OutputState,
    workspace_state: WorkspaceState,
    registry_state: RegistryState,
    configured_output: String,
    expected_output: Option<WlOutput>,
    tx: mpsc::Sender<TilingState>,
    running: bool,
}

impl WaylandTilingState {
    fn active_workspace(&self) -> Option<&cctk::workspace::Workspace> {
        self.workspace_state.workspace_groups().find_map(|g| {
            if let Some(o) = self.expected_output.as_ref()
                && !g.outputs.contains(o)
            {
                return None;
            }
            g.workspaces
                .iter()
                .filter_map(|h| self.workspace_state.workspace_info(h))
                .find(|w| w.state.contains(ext_workspace_handle_v1::State::Active))
        })
    }

    fn current_tiling(&self) -> Option<TilingState> {
        let w = self.active_workspace()?;
        if let Some(WEnum::Value(v)) = w.tiling {
            Some(v)
        } else {
            None
        }
    }
}

impl ProvidesRegistryState for WaylandTilingState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    sctk::registry_handlers![OutputState];
}

impl OutputHandler for WaylandTilingState {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, output: WlOutput) {
        if let Some(info) = self.output_state.info(&output)
            && info.name.as_deref() == Some(&self.configured_output)
        {
            self.expected_output = Some(output);
        }
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
}

impl WorkspaceHandler for WaylandTilingState {
    fn workspace_state(&mut self) -> &mut WorkspaceState {
        &mut self.workspace_state
    }

    fn done(&mut self) {
        if let Some(t) = self.current_tiling() {
            let _ = iced::futures::executor::block_on(self.tx.send(t));
        }
    }
}

cctk::delegate_workspace!(WaylandTilingState);
sctk::delegate_output!(WaylandTilingState);
sctk::delegate_registry!(WaylandTilingState);
