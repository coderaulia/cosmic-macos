// SPDX-License-Identifier: GPL-3.0-only

//! CLI entrypoint for `mac-clipboard`.

use clap::{Parser, Subcommand, ValueEnum};
use zbus::Connection;

use mac_clipboard::config::ClipboardConfig;
use mac_clipboard::daemon::run_daemon;
use mac_clipboard::dbus::{
    MacClipboardProxy, close_overlay_if_open, ensure_daemon_running, is_daemon_running,
    is_overlay_open,
};
use mac_clipboard::search::{FilterPill, search_items};
use mac_clipboard::store::ClipboardStore;
use mac_clipboard::ui::run_overlay;

#[derive(Parser, Debug)]
#[command(
    name = "mac-clipboard",
    version,
    about = "macOS-style clipboard history daemon, CLI, and quick-paste overlay"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run the clipboard watcher, owner, and D-Bus daemon.
    Daemon,
    /// Toggle the quick-paste window (starting the daemon if needed).
    Toggle,
    /// Show the quick-paste window (starting the daemon if needed).
    Show,
    /// Hide the quick-paste window if currently open.
    Hide,
    /// List recorded clipboard items.
    List {
        #[arg(long, default_value = "all")]
        filter: String,
        #[arg(long, default_value = "")]
        query: String,
        #[arg(long, default_value_t = 50)]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
    /// Show full details/content for an item id (or unique id prefix).
    Get {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Put an item back onto the Wayland clipboard.
    Copy {
        id: String,
        #[arg(long)]
        plain: bool,
        #[arg(long)]
        paste: bool,
    },
    /// Pin or unpin an item.
    Pin {
        id: String,
        #[arg(long)]
        unpin: bool,
    },
    /// Remove an item from history.
    Rm { id: String },
    /// Clear clipboard history and stored blobs.
    Clear {
        #[arg(long)]
        keep_pinned: bool,
    },
    /// Pause, resume, or query clipboard recording.
    Pause {
        #[arg(value_enum, default_value_t = PauseAction::Toggle)]
        action: PauseAction,
    },
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum PauseAction {
    On,
    Off,
    Toggle,
    Status,
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let code = match cli.command {
        Commands::Daemon => run_async(async {
            run_daemon().await.map_err(|e| e.to_string())
        }),
        Commands::Toggle => {
            let should_open = run_async_val(async {
                let conn = Connection::session().await.map_err(|e| e.to_string())?;
                ensure_daemon_running(&conn).await?;
                if close_overlay_if_open(&conn).await {
                    Ok(false)
                } else {
                    Ok(true)
                }
            });
            match should_open {
                Ok(true) => match run_overlay() {
                    Ok(()) => 0,
                    Err(err) => {
                        eprintln!("error: {err}");
                        1
                    }
                },
                Ok(false) => 0,
                Err(err) => {
                    eprintln!("error: {err}");
                    1
                }
            }
        }
        Commands::Show => {
            let should_open = run_async_val(async {
                let conn = Connection::session().await.map_err(|e| e.to_string())?;
                ensure_daemon_running(&conn).await?;
                Ok(!is_overlay_open(&conn).await)
            });
            match should_open {
                Ok(true) => match run_overlay() {
                    Ok(()) => 0,
                    Err(err) => {
                        eprintln!("error: {err}");
                        1
                    }
                },
                Ok(false) => 0,
                Err(err) => {
                    eprintln!("error: {err}");
                    1
                }
            }
        }
        Commands::Hide => run_async(async {
            let conn = Connection::session().await.map_err(|e| e.to_string())?;
            let _ = close_overlay_if_open(&conn).await;
            Ok(())
        }),
        Commands::List {
            filter,
            query,
            limit,
            json,
        } => run_async(async move {
            let conn = Connection::session().await.ok();
            if let Some(conn) = conn
                && is_daemon_running(&conn).await
                && let Ok(proxy) = MacClipboardProxy::new(&conn).await
            {
                let items = proxy
                    .list(&filter, &query, limit)
                    .await
                    .map_err(|e| e.to_string())?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&items).map_err(|e| e.to_string())?
                    );
                } else {
                    for item in items {
                        let pin = if item.pinned { "*" } else { " " };
                        println!(
                            "{pin} {:<20} {:<9} {}",
                            item.id, item.kind, item.preview
                        );
                    }
                }
                return Ok(());
            }

            let store = ClipboardStore::open(ClipboardStore::default_data_dir())
                .map_err(|e| e.to_string())?;
            let pill = FilterPill::from_filter_str(&filter);
            let hits = search_items(store.items(), pill, &query, limit as usize);
            if json {
                let raw: Vec<_> = hits.into_iter().map(|h| h.item).collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&raw).map_err(|e| e.to_string())?
                );
            } else {
                for hit in hits {
                    let item = hit.item;
                    let pin = if item.pinned { "*" } else { " " };
                    println!("{pin} {:<20} {:<9} {}", item.id, item.kind, item.preview);
                }
            }
            Ok(())
        }),
        Commands::Get { id, json } => run_async(async move {
            let conn = Connection::session().await.ok();
            if let Some(conn) = conn
                && is_daemon_running(&conn).await
                && let Ok(proxy) = MacClipboardProxy::new(&conn).await
            {
                let map = proxy.get(&id).await.map_err(|e| e.to_string())?;
                if json {
                    let simple: serde_json::Map<String, serde_json::Value> = map
                        .into_iter()
                        .map(|(k, v)| (k, serde_json::Value::String(format!("{v:?}"))))
                        .collect();
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&simple).map_err(|e| e.to_string())?
                    );
                } else if let Some(text) = map
                    .get("text")
                    .and_then(|v| String::try_from(v.clone()).ok())
                {
                    println!("{text}");
                } else if let Some(paths) = map
                    .get("paths")
                    .and_then(|v| Vec::<String>::try_from(v.clone()).ok())
                {
                    for p in paths {
                        println!("{p}");
                    }
                } else if let Some(preview) = map
                    .get("preview")
                    .and_then(|v| String::try_from(v.clone()).ok())
                {
                    println!("{preview}");
                }
                return Ok(());
            }

            let store = ClipboardStore::open(ClipboardStore::default_data_dir())
                .map_err(|e| e.to_string())?;
            let item = store.get(&id).map_err(|e| e.to_string())?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(item).map_err(|e| e.to_string())?
                );
            } else if let Some(text) = store.read_plain_text(item).map_err(|e| e.to_string())? {
                println!("{text}");
            } else {
                println!("{}", item.preview);
            }
            Ok(())
        }),
        Commands::Copy { id, plain, paste } => run_async(async move {
            let conn = Connection::session().await.map_err(|e| e.to_string())?;
            ensure_daemon_running(&conn).await?;
            let proxy = MacClipboardProxy::new(&conn)
                .await
                .map_err(|e| e.to_string())?;
            let _ = proxy
                .activate(&id, plain, paste)
                .await
                .map_err(|e| e.to_string())?;
            Ok(())
        }),
        Commands::Pin { id, unpin } => run_async(async move {
            let pinned = !unpin;
            let conn = Connection::session().await.ok();
            if let Some(conn) = conn
                && is_daemon_running(&conn).await
                && let Ok(proxy) = MacClipboardProxy::new(&conn).await
            {
                proxy.pin(&id, pinned).await.map_err(|e| e.to_string())?;
                return Ok(());
            }
            let cfg = ClipboardConfig::load();
            let mut store = ClipboardStore::open(ClipboardStore::default_data_dir())
                .map_err(|e| e.to_string())?;
            store
                .pin(&id, pinned, cfg.max_pinned)
                .map_err(|e| e.to_string())?;
            Ok(())
        }),
        Commands::Rm { id } => run_async(async move {
            let conn = Connection::session().await.ok();
            if let Some(conn) = conn
                && is_daemon_running(&conn).await
                && let Ok(proxy) = MacClipboardProxy::new(&conn).await
            {
                proxy.remove(&id).await.map_err(|e| e.to_string())?;
                return Ok(());
            }
            let mut store = ClipboardStore::open(ClipboardStore::default_data_dir())
                .map_err(|e| e.to_string())?;
            store.remove(&id).map_err(|e| e.to_string())?;
            Ok(())
        }),
        Commands::Clear { keep_pinned } => run_async(async move {
            let conn = Connection::session().await.ok();
            if let Some(conn) = conn
                && is_daemon_running(&conn).await
                && let Ok(proxy) = MacClipboardProxy::new(&conn).await
            {
                let count = proxy.clear(keep_pinned).await.map_err(|e| e.to_string())?;
                println!("cleared {count} item(s)");
                return Ok(());
            }
            let mut store = ClipboardStore::open(ClipboardStore::default_data_dir())
                .map_err(|e| e.to_string())?;
            let count = store.clear(keep_pinned).map_err(|e| e.to_string())?;
            println!("cleared {count} item(s)");
            Ok(())
        }),
        Commands::Pause { action } => run_async(async move {
            let conn = Connection::session().await.ok();
            if let Some(conn) = conn
                && is_daemon_running(&conn).await
                && let Ok(proxy) = MacClipboardProxy::new(&conn).await
            {
                let current = proxy.paused().await.map_err(|e| e.to_string())?;
                let next = match action {
                    PauseAction::On => true,
                    PauseAction::Off => false,
                    PauseAction::Toggle => !current,
                    PauseAction::Status => {
                        println!("{}", if current { "paused" } else { "active" });
                        return Ok(());
                    }
                };
                proxy.set_paused(next).await.map_err(|e| e.to_string())?;
                println!("{}", if next { "paused" } else { "active" });
                return Ok(());
            }
            let mut cfg = ClipboardConfig::load();
            match action {
                PauseAction::Status => {
                    println!("{}", if cfg.paused { "paused" } else { "active" });
                }
                PauseAction::On => {
                    cfg.paused = true;
                    cfg.paused_until_unix = None;
                    let _ = cfg.save();
                    println!("paused");
                }
                PauseAction::Off => {
                    cfg.paused = false;
                    cfg.paused_until_unix = None;
                    let _ = cfg.save();
                    println!("active");
                }
                PauseAction::Toggle => {
                    cfg.paused = !cfg.paused;
                    cfg.paused_until_unix = None;
                    let _ = cfg.save();
                    println!("{}", if cfg.paused { "paused" } else { "active" });
                }
            }
            Ok(())
        }),
    };

    if code != 0 {
        std::process::exit(code);
    }
}

fn run_async<F>(fut: F) -> i32
where
    F: std::future::Future<Output = Result<(), String>>,
{
    match run_async_val(fut) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("error: {err}");
            1
        }
    }
}

fn run_async_val<T, F>(fut: F) -> Result<T, String>
where
    F: std::future::Future<Output = Result<T, String>>,
{
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(fut)
}
