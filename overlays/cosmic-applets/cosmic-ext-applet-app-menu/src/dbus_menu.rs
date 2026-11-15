// SPDX-License-Identifier: GPL-3.0-only

//! Session D-Bus client subscription for querying `dev.asw.AppMenu1` layouts
//! from the currently focused application and activating menu items.

use std::any::TypeId;

use cosmic::iced::{
    Subscription,
    futures::{SinkExt, StreamExt, channel::mpsc as futures_mpsc},
    stream,
};
use mac_appmenu::{AppMenu1Proxy, MenuLayout, candidate_bus_names};
use tokio::sync::mpsc;
use zbus::Connection;
use zbus::fdo::DBusProxy;
use zbus::names::BusName;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RemoteMenuTarget {
    pub app_id: String,
    pub desktop_id: Option<String>,
}

#[derive(Debug, Clone)]
pub enum DbusMenuUpdate {
    Init(mpsc::UnboundedSender<DbusMenuCommand>),
    Layout {
        target: RemoteMenuTarget,
        bus_name: String,
        layout: MenuLayout,
    },
    Cleared {
        target: RemoteMenuTarget,
    },
}

#[derive(Debug, Clone)]
pub enum DbusMenuCommand {
    SetTarget(Option<RemoteMenuTarget>),
    Activate {
        bus_name: String,
        item_id: String,
    },
}

pub fn subscription() -> Subscription<DbusMenuUpdate> {
    Subscription::run_with(TypeId::of::<DbusMenuUpdate>(), |_| {
        stream::channel(
            64,
            |mut output: futures_mpsc::Sender<DbusMenuUpdate>| async move {
                let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<DbusMenuCommand>();
                let _ = output.send(DbusMenuUpdate::Init(cmd_tx)).await;

                let Ok(conn) = Connection::session().await else {
                    tracing::warn!("Session D-Bus unavailable for app-menu global menu client");
                    while cmd_rx.recv().await.is_some() {}
                    return;
                };
                let Ok(dbus) = DBusProxy::new(&conn).await else {
                    while cmd_rx.recv().await.is_some() {}
                    return;
                };
                let mut owner_changes = dbus.receive_name_owner_changed().await.ok();

                let mut current_target: Option<RemoteMenuTarget> = None;
                let mut current_candidates: Vec<String> = Vec::new();
                let mut active_bus_name: Option<String> = None;
                let mut layout_stream: Option<mac_appmenu::LayoutChangedStream> = None;

                loop {
                    let layout_fut = async {
                        match layout_stream.as_mut() {
                            Some(s) => s.next().await,
                            None => std::future::pending().await,
                        }
                    };
                    let owner_fut = async {
                        match owner_changes.as_mut() {
                            Some(s) => s.next().await,
                            None => std::future::pending().await,
                        }
                    };

                    tokio::select! {
                        cmd = cmd_rx.recv() => {
                            let Some(cmd) = cmd else {
                                break;
                            };
                            match cmd {
                                DbusMenuCommand::SetTarget(target) => {
                                    current_target = target;
                                    current_candidates = current_target
                                        .as_ref()
                                        .map(|t| candidate_bus_names(&t.app_id, t.desktop_id.as_deref()))
                                        .unwrap_or_default();

                                    refresh_active_target(
                                        &conn,
                                        &dbus,
                                        current_target.as_ref(),
                                        &current_candidates,
                                        &mut active_bus_name,
                                        &mut layout_stream,
                                        &mut output,
                                    )
                                    .await;
                                }
                                DbusMenuCommand::Activate { bus_name, item_id } => {
                                    let conn = conn.clone();
                                    tokio::spawn(async move {
                                        if let Ok(builder) = AppMenu1Proxy::builder(&conn).destination(bus_name.clone())
                                            && let Ok(proxy) = builder.build().await
                                            && let Err(err) = proxy.activate(&item_id).await
                                        {
                                            tracing::debug!(
                                                "AppMenu1 Activate({item_id}) on {bus_name} failed: {err}"
                                            );
                                        }
                                    });
                                }
                            }
                        }
                        sig = layout_fut => {
                            match sig {
                                Some(_) => {
                                    if let (Some(target), Some(bus_name)) =
                                        (current_target.as_ref(), active_bus_name.as_deref())
                                    {
                                        match fetch_layout(&conn, bus_name).await {
                                            Some(layout) => {
                                                let _ = output
                                                    .send(DbusMenuUpdate::Layout {
                                                        target: target.clone(),
                                                        bus_name: bus_name.to_string(),
                                                        layout,
                                                    })
                                                    .await;
                                            }
                                            None => {
                                                refresh_active_target(
                                                    &conn,
                                                    &dbus,
                                                    current_target.as_ref(),
                                                    &current_candidates,
                                                    &mut active_bus_name,
                                                    &mut layout_stream,
                                                    &mut output,
                                                )
                                                .await;
                                            }
                                        }
                                    }
                                }
                                None => {
                                    layout_stream = None;
                                }
                            }
                        }
                        owner_sig = owner_fut => {
                            let Some(sig) = owner_sig else {
                                owner_changes = None;
                                continue;
                            };
                            let Ok(args) = sig.args() else {
                                continue;
                            };
                            let changed_name = args.name().as_str();
                            if current_candidates.iter().any(|c| c == changed_name) {
                                refresh_active_target(
                                    &conn,
                                    &dbus,
                                    current_target.as_ref(),
                                    &current_candidates,
                                    &mut active_bus_name,
                                    &mut layout_stream,
                                    &mut output,
                                )
                                .await;
                            }
                        }
                    }
                }
            },
        )
    })
}

async fn refresh_active_target(
    conn: &Connection,
    dbus: &DBusProxy<'_>,
    target: Option<&RemoteMenuTarget>,
    candidates: &[String],
    active_bus_name: &mut Option<String>,
    layout_stream: &mut Option<mac_appmenu::LayoutChangedStream>,
    output: &mut futures_mpsc::Sender<DbusMenuUpdate>,
) {
    let Some(target) = target else {
        *active_bus_name = None;
        *layout_stream = None;
        return;
    };

    for bus_name in candidates {
        let Ok(bus_name_ref) = BusName::try_from(bus_name.as_str()) else {
            continue;
        };
        if dbus.name_has_owner(bus_name_ref).await != Ok(true) {
            continue;
        }
        let Ok(builder) = AppMenu1Proxy::builder(conn).destination(bus_name.clone()) else {
            continue;
        };
        let Ok(proxy) = builder.build().await else {
            continue;
        };
        if let Ok(layout) = proxy.get_layout().await {
            if active_bus_name.as_deref() != Some(bus_name.as_str()) || layout_stream.is_none() {
                *layout_stream = proxy.receive_layout_changed().await.ok();
            }
            *active_bus_name = Some(bus_name.clone());
            let _ = output
                .send(DbusMenuUpdate::Layout {
                    target: target.clone(),
                    bus_name: bus_name.clone(),
                    layout,
                })
                .await;
            return;
        }
    }

    *active_bus_name = None;
    *layout_stream = None;
    let _ = output
        .send(DbusMenuUpdate::Cleared {
            target: target.clone(),
        })
        .await;
}

async fn fetch_layout(conn: &Connection, bus_name: &str) -> Option<MenuLayout> {
    let builder = AppMenu1Proxy::builder(conn)
        .destination(bus_name.to_string())
        .ok()?;
    let proxy = builder.build().await.ok()?;
    proxy.get_layout().await.ok()
}
