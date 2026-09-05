// Author: Dustin Pilgrim
// License: GPL-3.0-only

use crate::daemon::Daemon;
use eventline::journal::rotation::LogPolicy;
use eventline::{FileSetup, RunHeader, Setup};
use std::future::pending;
use std::path::PathBuf;

use crate::cli::Args;

type AnyError = Box<dyn std::error::Error + Send + Sync>;

#[cfg(unix)]
async fn wait_for_shutdown_signal() -> String {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigterm = signal(SignalKind::terminate()).ok();
    let mut sighup = signal(SignalKind::hangup()).ok();

    tokio::select! {
        _ = tokio::signal::ctrl_c() => "SIGINT".to_string(),
        _ = async {
            match sigterm.as_mut() {
                Some(stream) => { let _ = stream.recv().await; }
                None => pending::<()>().await,
            }
        } => "SIGTERM".to_string(),
        _ = async {
            match sighup.as_mut() {
                Some(stream) => { let _ = stream.recv().await; }
                None => pending::<()>().await,
            }
        } => "SIGHUP".to_string(),
    }
}

#[cfg(not(unix))]
async fn wait_for_shutdown_signal() -> String {
    let _ = tokio::signal::ctrl_c().await;
    "SIGINT".to_string()
}

pub async fn run(args: Args) -> Result<(), AnyError> {
    let file = crate::app::platform::default_log_path().map(|path| {
        let header = RunHeader::new("stasis daemon run start");
        let policy = LogPolicy::default();

        FileSetup::Rotating {
            path,
            policy,
            header: Some(header),
        }
    });

    if let Err(e) = eventline::setup(Setup {
        verbose: args.verbose,
        level: None,
        file,
    })
    .await
    {
        // Keep running even if logging can't be configured.
        // If console is disabled (non-verbose), this will likely be silent,
        // which is acceptable for "logging is best-effort".
        eprintln!("[eventline] failed to configure logging: {e}");
    }

    if args.verbose {
        eventline::debug!("debug logging enabled");
    }

    // ------------------------------
    // single-instance
    // ------------------------------
    let _instance_lock = match crate::app::platform::acquire_single_instance_lock() {
        Ok(l) => l,

        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            eventline::warn!("{e}");

            if !args.verbose {
                eprintln!("Error: {e}");
            }

            return Ok(()); // clean exit
        }

        Err(e) => {
            eventline::error!("failed to acquire instance lock: {e}");
            return Err(Box::new(e));
        }
    };

    // ------------------------------
    // wayland sanity
    // ------------------------------
    if let Err(e) = crate::app::platform::ensure_wayland_alive() {
        eventline::warn!("not running in wayland session: {e}");

        if !args.verbose {
            eprintln!("Error: not running in wayland session: {e}");
        }

        return Ok(()); // clean exit
    }

    eventline::info!("stasis starting");

    // resolve config path
    let mut config_path: PathBuf = match args.config.as_deref() {
        Some(p) => p.to_path_buf(),
        None => crate::config::resolve_default_config_path(),
    };

    if args.config.is_none() {
        if let Err(e) = crate::config::bootstrap::ensure_user_config_exists() {
            eventline::warn!("failed to bootstrap default config: {e}");
        }

        config_path = crate::config::resolve_default_config_path();
    }

    if config_path.exists() {
        match crate::config::migrate::migrate_in_place(&config_path) {
            Ok(crate::config::migrate::MigrateOutcome::Migrated {
                backup_path,
                changes,
            }) => {
                eventline::info!(
                    "migrated config in place ({}); backup at {}",
                    changes.join(", "),
                    backup_path.display()
                );
            }
            Ok(crate::config::migrate::MigrateOutcome::Current) => {}
            Err(e) => {
                eventline::error!("config migration failed: {e}");
            }
        }
    }

    let cfg_file = crate::config::load_from_path(&config_path).map_err(|e| {
        eventline::error!("{e}");
        e
    })?;

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    crate::app::platform::spawn_wayland_socket_watcher(shutdown_tx.clone());

    let mut daemon = Daemon::new(cfg_file.cfg, config_path, args.verbose);

    let mut daemon_task = tokio::spawn({
        let shutdown_tx = shutdown_tx.clone();
        async move { daemon.run(shutdown_rx, shutdown_tx).await }
    });

    tokio::select! {
        res = &mut daemon_task => {
            match res {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(e),
                Err(join_err) => Err(Box::new(join_err) as AnyError),
            }?;
            Ok(())
        }

        sig = wait_for_shutdown_signal() => {
            eventline::info!("received {}, shutting down", sig);
            let _ = shutdown_tx.send(true);

            match daemon_task.await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(e),
                Err(join_err) => Err(Box::new(join_err)),
            }
        }
    }
}
