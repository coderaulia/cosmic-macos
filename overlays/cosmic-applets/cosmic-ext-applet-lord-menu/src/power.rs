// SPDX-License-Identifier: GPL-3.0-only

//! Power and session actions using `cosmic-osd` with `logind` / session D-Bus fallback.

use logind_zbus::{
    manager::ManagerProxy,
    session::{SessionClass, SessionProxy, SessionType},
    user::UserProxy,
};
use rustix::process::getuid;
use tokio::process::Command;
use zbus::Connection;

use crate::{cosmic_session::CosmicSessionProxy, session_manager::SessionManagerProxy};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    Sleep,
    Restart,
    ShutDown,
    LockScreen,
    LogOut,
}

impl PowerAction {
    pub async fn execute(self) -> zbus::Result<()> {
        match self {
            PowerAction::Sleep => suspend().await,
            PowerAction::Restart => spawn_osd_or("restart", reboot()).await,
            PowerAction::ShutDown => spawn_osd_or("shutdown", power_off()).await,
            PowerAction::LockScreen => lock_session().await,
            PowerAction::LogOut => spawn_osd_or("log-out", log_out()).await,
        }
    }
}

async fn spawn_osd_or<F>(arg: &str, fallback: F) -> zbus::Result<()>
where
    F: std::future::Future<Output = zbus::Result<()>>,
{
    match Command::new("cosmic-osd").arg(arg).spawn() {
        Ok(_child) => Ok(()),
        Err(err) => {
            tracing::warn!("cosmic-osd {arg} unavailable ({err:?}), using D-Bus fallback");
            fallback.await
        }
    }
}

async fn suspend() -> zbus::Result<()> {
    let conn = Connection::system().await?;
    let manager = ManagerProxy::new(&conn).await?;
    manager.suspend(true).await
}

async fn reboot() -> zbus::Result<()> {
    let conn = Connection::system().await?;
    let manager = ManagerProxy::new(&conn).await?;
    manager.reboot(true).await
}

async fn power_off() -> zbus::Result<()> {
    let conn = Connection::system().await?;
    let manager = ManagerProxy::new(&conn).await?;
    manager.power_off(true).await
}

async fn lock_session() -> zbus::Result<()> {
    let conn = Connection::system().await?;
    let manager = ManagerProxy::new(&conn).await?;
    let uid = getuid().as_raw();
    let user_path = manager.get_user(uid).await?;
    let user = UserProxy::builder(&conn).path(user_path)?.build().await?;
    let sessions = user.sessions().await?;
    let mut locked = false;

    for (_, session_path) in sessions {
        let Ok(session) = SessionProxy::builder(&conn).path(session_path)?.build().await else {
            continue;
        };
        if session.class().await == Ok(SessionClass::User)
            && session.type_().await? != SessionType::TTY
            && session.lock().await.is_ok()
        {
            locked = true;
        }
    }

    if locked {
        Ok(())
    } else {
        Err(zbus::Error::Failure("locking session failed".to_string()))
    }
}

async fn log_out() -> zbus::Result<()> {
    let session_type = std::env::var("XDG_CURRENT_DESKTOP").ok();
    let conn = Connection::session().await?;
    if matches!(session_type.as_deref().map(str::trim), Some("pop:GNOME")) {
        let proxy = SessionManagerProxy::new(&conn).await?;
        proxy.logout(0).await
    } else {
        let proxy = CosmicSessionProxy::new(&conn).await?;
        proxy.exit().await
    }
}

/// Current user's display name for `Log Out <user>…`.
#[must_use]
pub fn current_username() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "User".to_string())
}
