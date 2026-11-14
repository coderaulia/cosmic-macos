// SPDX-License-Identifier: GPL-3.0-only

//! Wayland toplevel & workspace subscription for the active application menu.

use std::collections::HashMap;
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::LazyLock;

use cctk::{
    sctk::{
        self,
        activation::{ActivationHandler, ActivationState, RequestData, RequestDataExt},
        output::{OutputHandler, OutputState},
        reexports::{calloop, calloop_wayland_source::WaylandSource},
        registry::{ProvidesRegistryState, RegistryState},
        seat::{SeatHandler, SeatState},
    },
    toplevel_info::{ToplevelInfo, ToplevelInfoHandler, ToplevelInfoState},
    toplevel_management::{ToplevelManagerHandler, ToplevelManagerState},
    wayland_client::{
        Connection, QueueHandle, WEnum,
        globals::registry_queue_init,
        protocol::{wl_output, wl_seat::WlSeat, wl_surface::WlSurface},
    },
    wayland_protocols::ext::{
        foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
        workspace::v1::client::ext_workspace_handle_v1::ExtWorkspaceHandleV1,
    },
    workspace::{WorkspaceHandler, WorkspaceState},
};
use cosmic::iced::{
    self, Subscription,
    futures::{
        SinkExt, StreamExt,
        channel::mpsc::{UnboundedReceiver, unbounded},
    },
    stream,
};
use cosmic_protocols::toplevel_management::v1::client::zcosmic_toplevel_manager_v1;
use tokio::sync::Mutex;

static WAYLAND_RX: LazyLock<Mutex<Option<UnboundedReceiver<WaylandUpdate>>>> =
    LazyLock::new(|| Mutex::new(None));

#[derive(Debug, Clone)]
pub struct WorkspaceEntry {
    pub handle: ExtWorkspaceHandleV1,
    pub name: String,
}

#[derive(Debug, Clone)]
pub enum WaylandUpdate {
    Init(calloop::channel::Sender<WaylandRequest>),
    ToplevelAdd(ToplevelInfo),
    ToplevelUpdate(ToplevelInfo),
    ToplevelRemove(ExtForeignToplevelHandleV1),
    Workspaces(Vec<WorkspaceEntry>),
    ActivationToken {
        token: Option<String>,
        exec: String,
        terminal: bool,
    },
    Finished,
}

#[derive(Debug, Clone)]
pub enum WaylandRequest {
    Minimize(ExtForeignToplevelHandleV1),
    Unminimize(ExtForeignToplevelHandleV1),
    SetMaximized(ExtForeignToplevelHandleV1),
    UnsetMaximized(ExtForeignToplevelHandleV1),
    SetFullscreen(ExtForeignToplevelHandleV1),
    UnsetFullscreen(ExtForeignToplevelHandleV1),
    MoveToWorkspace {
        toplevel: ExtForeignToplevelHandleV1,
        workspace: ExtWorkspaceHandleV1,
    },
    Close(ExtForeignToplevelHandleV1),
    TokenRequest {
        app_id: String,
        exec: String,
        terminal: bool,
    },
}

pub fn subscription() -> Subscription<WaylandUpdate> {
    Subscription::run_with(std::any::TypeId::of::<WaylandUpdate>(), |_| {
        stream::channel(
            50,
            |mut output: cosmic::iced::futures::channel::mpsc::Sender<WaylandUpdate>| async move {
            let mut finished = false;
            loop {
                if finished {
                    iced::futures::future::pending::<()>().await;
                }
                let mut guard = WAYLAND_RX.lock().await;
                let rx = {
                    if guard.is_none() {
                        let (req_tx, req_rx) = calloop::channel::channel();
                        let (upd_tx, upd_rx) = unbounded();
                        std::thread::spawn(move || run_wayland_loop(upd_tx, req_rx));
                        *guard = Some(upd_rx);
                        let _ = output.send(WaylandUpdate::Init(req_tx)).await;
                    }
                    guard.as_mut().unwrap()
                };
                if let Some(msg) = rx.next().await {
                    let _ = output.send(msg).await;
                } else {
                    let _ = output.send(WaylandUpdate::Finished).await;
                    finished = true;
                }
            }
            },
        )
    })
}

struct ExecRequestData {
    data: RequestData,
    exec: String,
    terminal: bool,
}

impl RequestDataExt for ExecRequestData {
    fn app_id(&self) -> Option<&str> {
        self.data.app_id()
    }
    fn seat_and_serial(&self) -> Option<(&WlSeat, u32)> {
        self.data.seat_and_serial()
    }
    fn surface(&self) -> Option<&WlSurface> {
        self.data.surface()
    }
}

struct AppData {
    exit: bool,
    tx: cosmic::iced::futures::channel::mpsc::UnboundedSender<WaylandUpdate>,
    queue_handle: QueueHandle<Self>,
    output_state: OutputState,
    workspace_state: WorkspaceState,
    toplevel_info_state: ToplevelInfoState,
    toplevel_manager_state: ToplevelManagerState,
    registry_state: RegistryState,
    seat_state: SeatState,
    activation_state: Option<ActivationState>,
    workspace_outputs: HashMap<ExtWorkspaceHandleV1, wl_output::WlOutput>,
    outputs: Vec<wl_output::WlOutput>,
}

impl ProvidesRegistryState for AppData {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    sctk::registry_handlers!();
}

impl OutputHandler for AppData {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if !self.outputs.contains(&output) {
            self.outputs.push(output);
        }
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.outputs.retain(|o| o != &output);
    }
}

impl WorkspaceHandler for AppData {
    fn workspace_state(&mut self) -> &mut WorkspaceState {
        &mut self.workspace_state
    }

    fn done(&mut self) {
        self.workspace_outputs.clear();
        let mut entries = Vec::new();
        for group in self.workspace_state.workspace_groups() {
            let group_output = group.outputs.first().cloned();
            let mut group_workspaces: Vec<_> = group
                .workspaces
                .iter()
                .filter_map(|h| self.workspace_state.workspace_info(h))
                .collect();
            group_workspaces.sort_by(|a, b| a.coordinates.cmp(&b.coordinates).then(a.name.cmp(&b.name)));
            for ws in group_workspaces {
                if let Some(out) = &group_output {
                    self.workspace_outputs.insert(ws.handle.clone(), out.clone());
                }
                let name = if ws.name.trim().is_empty() {
                    format!("Workspace {}", entries.len() + 1)
                } else {
                    ws.name.clone()
                };
                entries.push(WorkspaceEntry {
                    handle: ws.handle.clone(),
                    name,
                });
            }
        }
        let _ = self.tx.unbounded_send(WaylandUpdate::Workspaces(entries));
    }
}

impl ActivationHandler for AppData {
    type RequestData = ExecRequestData;
    fn new_token(&mut self, token: String, data: &ExecRequestData) {
        let _ = self.tx.unbounded_send(WaylandUpdate::ActivationToken {
            token: Some(token),
            exec: data.exec.clone(),
            terminal: data.terminal,
        });
    }
}

impl SeatHandler for AppData {
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

impl ToplevelManagerHandler for AppData {
    fn toplevel_manager_state(&mut self) -> &mut ToplevelManagerState {
        &mut self.toplevel_manager_state
    }

    fn capabilities(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: Vec<WEnum<zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1>>,
    ) {
    }
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
        if let Some(info) = self.toplevel_info_state.info(toplevel) {
            let _ = self
                .tx
                .unbounded_send(WaylandUpdate::ToplevelAdd(info.clone()));
        }
    }

    fn update_toplevel(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        toplevel: &ExtForeignToplevelHandleV1,
    ) {
        if let Some(info) = self.toplevel_info_state.info(toplevel) {
            let _ = self
                .tx
                .unbounded_send(WaylandUpdate::ToplevelUpdate(info.clone()));
        }
    }

    fn toplevel_closed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        toplevel: &ExtForeignToplevelHandleV1,
    ) {
        let _ = self
            .tx
            .unbounded_send(WaylandUpdate::ToplevelRemove(toplevel.clone()));
    }
}

fn run_wayland_loop(
    tx: cosmic::iced::futures::channel::mpsc::UnboundedSender<WaylandUpdate>,
    rx: calloop::channel::Channel<WaylandRequest>,
) {
    let socket = std::env::var("X_PRIVILEGED_WAYLAND_SOCKET")
        .ok()
        .and_then(|fd| fd.parse::<RawFd>().ok())
        .map(|fd| unsafe { UnixStream::from_raw_fd(fd) });

    let conn = match socket {
        Some(sock) => Connection::from_socket(sock),
        None => Connection::connect_to_env(),
    };
    let Ok(conn) = conn else {
        tracing::warn!("Wayland connection unavailable for app-menu");
        return;
    };

    let Ok((globals, event_queue)) = registry_queue_init(&conn) else {
        return;
    };
    let Ok(mut event_loop) = calloop::EventLoop::<AppData>::try_new() else {
        return;
    };
    let qh = event_queue.handle();
    let loop_handle = event_loop.handle();
    if WaylandSource::new(conn, event_queue)
        .insert(loop_handle.clone())
        .is_err()
    {
        return;
    }

    if loop_handle
        .insert_source(rx, |event, (), state| match event {
            calloop::channel::Event::Msg(req) => match req {
                WaylandRequest::Minimize(handle) => {
                    if let Some(ct) = state
                        .toplevel_info_state
                        .info(&handle)
                        .and_then(|i| i.cosmic_toplevel.as_ref())
                    {
                        state.toplevel_manager_state.manager.set_minimized(ct);
                    }
                }
                WaylandRequest::Unminimize(handle) => {
                    if let Some(ct) = state
                        .toplevel_info_state
                        .info(&handle)
                        .and_then(|i| i.cosmic_toplevel.as_ref())
                    {
                        state.toplevel_manager_state.manager.unset_minimized(ct);
                    }
                }
                WaylandRequest::SetMaximized(handle) => {
                    if let Some(ct) = state
                        .toplevel_info_state
                        .info(&handle)
                        .and_then(|i| i.cosmic_toplevel.as_ref())
                    {
                        state.toplevel_manager_state.manager.set_maximized(ct);
                    }
                }
                WaylandRequest::UnsetMaximized(handle) => {
                    if let Some(ct) = state
                        .toplevel_info_state
                        .info(&handle)
                        .and_then(|i| i.cosmic_toplevel.as_ref())
                    {
                        state.toplevel_manager_state.manager.unset_maximized(ct);
                    }
                }
                WaylandRequest::SetFullscreen(handle) => {
                    if let Some(info) = state.toplevel_info_state.info(&handle)
                        && let Some(ct) = &info.cosmic_toplevel
                    {
                        let output = info.output.iter().next();
                        state
                            .toplevel_manager_state
                            .manager
                            .set_fullscreen(ct, output);
                    }
                }
                WaylandRequest::UnsetFullscreen(handle) => {
                    if let Some(ct) = state
                        .toplevel_info_state
                        .info(&handle)
                        .and_then(|i| i.cosmic_toplevel.as_ref())
                    {
                        state.toplevel_manager_state.manager.unset_fullscreen(ct);
                    }
                }
                WaylandRequest::MoveToWorkspace {
                    toplevel,
                    workspace,
                } => {
                    if let Some(info) = state.toplevel_info_state.info(&toplevel)
                        && let Some(ct) = &info.cosmic_toplevel
                    {
                        let target_output = state
                            .workspace_outputs
                            .get(&workspace)
                            .cloned()
                            .or_else(|| info.output.iter().next().cloned())
                            .or_else(|| state.outputs.first().cloned());
                        if let Some(out) = target_output {
                            state
                                .toplevel_manager_state
                                .manager
                                .move_to_ext_workspace(ct, &workspace, &out);
                        }
                    }
                }
                WaylandRequest::Close(handle) => {
                    if let Some(ct) = state
                        .toplevel_info_state
                        .info(&handle)
                        .and_then(|i| i.cosmic_toplevel.as_ref())
                    {
                        state.toplevel_manager_state.manager.close(ct);
                    }
                }
                WaylandRequest::TokenRequest {
                    app_id,
                    exec,
                    terminal,
                } => {
                    if let Some(activation_state) = state.activation_state.as_ref() {
                        activation_state.request_token_with_data(
                            &state.queue_handle,
                            ExecRequestData {
                                data: RequestData {
                                    app_id: Some(app_id),
                                    seat_and_serial: state
                                        .seat_state
                                        .seats()
                                        .next()
                                        .map(|seat| (seat, 0)),
                                    surface: None,
                                },
                                exec,
                                terminal,
                            },
                        );
                    } else {
                        let _ = state.tx.unbounded_send(WaylandUpdate::ActivationToken {
                            token: None,
                            exec,
                            terminal,
                        });
                    }
                }
            },
            calloop::channel::Event::Closed => {
                state.exit = true;
            }
        })
        .is_err()
    {
        return;
    }

    let registry_state = RegistryState::new(&globals);
    let mut app_data = AppData {
        exit: false,
        tx,
        output_state: OutputState::new(&globals, &qh),
        workspace_state: WorkspaceState::new(&registry_state, &qh),
        toplevel_info_state: ToplevelInfoState::new(&registry_state, &qh),
        toplevel_manager_state: ToplevelManagerState::new(&registry_state, &qh),
        registry_state,
        seat_state: SeatState::new(&globals, &qh),
        activation_state: ActivationState::bind::<AppData>(&globals, &qh).ok(),
        queue_handle: qh,
        workspace_outputs: HashMap::new(),
        outputs: Vec::new(),
    };

    while !app_data.exit {
        if event_loop.dispatch(None, &mut app_data).is_err() {
            break;
        }
    }
}

sctk::delegate_seat!(AppData);
sctk::delegate_registry!(AppData);
cctk::delegate_toplevel_info!(AppData);
cctk::delegate_workspace!(AppData);
cctk::delegate_toplevel_manager!(AppData);
sctk::delegate_activation!(AppData, ExecRequestData);
sctk::delegate_output!(AppData);
