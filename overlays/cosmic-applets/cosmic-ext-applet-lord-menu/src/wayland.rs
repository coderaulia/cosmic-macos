// SPDX-License-Identifier: GPL-3.0-only

//! Wayland toplevel & output tracking for Force Quit and About This Desktop.

use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::LazyLock;

use cctk::{
    sctk::{
        self,
        output::{OutputHandler, OutputInfo, OutputState},
        reexports::{calloop, calloop_wayland_source::WaylandSource},
        registry::{ProvidesRegistryState, RegistryState},
        seat::{SeatHandler, SeatState},
    },
    toplevel_info::{ToplevelInfo, ToplevelInfoHandler, ToplevelInfoState},
    toplevel_management::{ToplevelManagerHandler, ToplevelManagerState},
    wayland_client::{
        Connection, QueueHandle, WEnum,
        globals::registry_queue_init,
        protocol::{wl_output, wl_seat::WlSeat},
    },
    wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
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
pub enum WaylandUpdate {
    Init(calloop::channel::Sender<WaylandRequest>),
    ToplevelAdd(ToplevelInfo),
    ToplevelUpdate(ToplevelInfo),
    ToplevelRemove(ExtForeignToplevelHandleV1),
    OutputAdd(wl_output::WlOutput, OutputInfo),
    OutputUpdate(wl_output::WlOutput, OutputInfo),
    OutputRemove(wl_output::WlOutput),
    Finished,
}

#[derive(Debug, Clone)]
pub enum WaylandRequest {
    CloseToplevel(ExtForeignToplevelHandleV1),
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

struct AppData {
    exit: bool,
    tx: cosmic::iced::futures::channel::mpsc::UnboundedSender<WaylandUpdate>,
    output_state: OutputState,
    toplevel_info_state: ToplevelInfoState,
    toplevel_manager_state: ToplevelManagerState,
    registry_state: RegistryState,
    seat_state: SeatState,
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
        if let Some(info) = self.output_state.info(&output) {
            let _ = self
                .tx
                .unbounded_send(WaylandUpdate::OutputAdd(output, info));
        }
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if let Some(info) = self.output_state.info(&output) {
            let _ = self
                .tx
                .unbounded_send(WaylandUpdate::OutputUpdate(output, info));
        }
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        let _ = self.tx.unbounded_send(WaylandUpdate::OutputRemove(output));
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
        tracing::warn!("Wayland connection unavailable for lord-menu");
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
            calloop::channel::Event::Msg(WaylandRequest::CloseToplevel(handle)) => {
                if let Some(info) = state.toplevel_info_state.info(&handle)
                    && let Some(cosmic_toplevel) = &info.cosmic_toplevel
                {
                    state
                        .toplevel_manager_state
                        .manager
                        .close(cosmic_toplevel);
                }
            }
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
        toplevel_info_state: ToplevelInfoState::new(&registry_state, &qh),
        toplevel_manager_state: ToplevelManagerState::new(&registry_state, &qh),
        registry_state,
        seat_state: SeatState::new(&globals, &qh),
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
cctk::delegate_toplevel_manager!(AppData);
sctk::delegate_output!(AppData);
