use std::{fs, path::PathBuf, sync::Arc, time::Duration};

use clap::{Parser, Subcommand};
use eyre::Result;
use tokio::net::UnixListener;
use tokio::task::LocalSet;
use tokio::sync::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

mod actions;
mod app_inhibit;
mod brightness;
mod config;
mod idle_timer;
mod input;
mod ipc;
mod log;
mod media;
mod power_detection;
mod suspend;
mod utils;
mod wayland;

use log::{log_message, log_error_message, set_verbose};
use crate::wayland::{WaylandIdleData, setup as setup_wayland};

#[derive(Parser, Debug)]
#[command(
    name = "Stasis",
    version = env!("CARGO_PKG_VERSION"), 
    about = "Capable idle manager for Wayland\n\nFor configuration details, see `man 5 stasis`"
)]
struct Args {
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,
    #[arg(short, long, action)]
    verbose: bool,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    #[command(about = "Reload the configuration without restarting")]
    Reload,

    #[command(about = "Pause all idle timers")]
    Pause,

    #[command(about = "Resume idle timers after a pause")]
    Resume,

    #[command(about = "Manually trigger idle actions")]
    TriggerIdle,

    #[command(about = "Trigger pre-suspend action manually")]
    TriggerPreSuspend,

    #[command(about = "Toggle manual idle inhibition (for Waybar etc.)")]
    ToggleInhibit,

    #[command(about = "Stop the currently running instances of Stasis")]
    Stop,

    #[command(about = "Display current session information")]
    Info {
        #[arg(long, help = "Output as JSON (for Waybar or scripts)")]
        json: bool,
    },
}

const SOCKET_PATH: &str = "/tmp/stasis.sock";

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Must be bound to wayland session,
    // don't be naughty.
    if std::env::var("WAYLAND_DISPLAY").is_err() {
        eprintln!("Error: Wayland is not detected. Stasis requires Wayland to run.");
        std::process::exit(1);
    }

    // --- Handle subcommands via socket ---
    if let Some(cmd) = &args.command {
        use tokio::net::UnixStream;

        match cmd {
            Commands::Info { json } => {
                if let Ok(mut stream) = UnixStream::connect(SOCKET_PATH).await {
                    let msg = if *json { "info --json" } else { "info" };
                    let _ = stream.write_all(msg.as_bytes()).await;

                    let mut response = Vec::new();
                    let _ = stream.read_to_end(&mut response).await;
                    println!("{}", String::from_utf8_lossy(&response));
                } else {
                    // Waybar-friendly "Stasis not running"
                    if *json {
                        println!(r#"{{"text":"😴","tooltip":"Stasis is not running"}}"#);
                    } else {
                        println!("Stasis is not running");
                    }
                }
            }
            _ => {
                let msg = match cmd {
                    Commands::Reload => "reload",
                    Commands::Pause => "pause",
                    Commands::Resume => "resume",
                    Commands::TriggerIdle => "trigger_idle",
                    Commands::TriggerPreSuspend => "trigger_presuspend",
                    Commands::ToggleInhibit => "toggle_inhibit",
                    Commands::Stop => "stop",
                    _ => unreachable!(),
                };

                if let Ok(mut stream) = UnixStream::connect(SOCKET_PATH).await {
                    let _ = stream.write_all(msg.as_bytes()).await;

                    if msg == "info" || msg == "toggle_inhibit" {
                        let mut response = Vec::new();
                        let _ = stream.read_to_end(&mut response).await;
                        println!("{}", String::from_utf8_lossy(&response));
                    }
                } else {
                    log_error_message("No running instance found");
                }
            }
        }

        return Ok(());
    }

    // --- Single instance enforcement ---
    let just_help_or_version = std::env::args().any(|a| matches!(a.as_str(), "-V" | "--version" | "-h" | "--help" | "help"));
    if let Ok(_) = tokio::net::UnixStream::connect(SOCKET_PATH).await {
        if !just_help_or_version {
            println!("Another instance of Stasis is already running.");
        }
        log_error_message("Another instance is already running.");
        return Ok(());
    }
    let _ = fs::remove_file(SOCKET_PATH);

    let listener = UnixListener::bind(SOCKET_PATH).map_err(|_| {
        eyre::eyre!("Failed to bind control socket. Another instance may be running.")
    })?;

    setup_cleanup_handler();

    // --- Load config ---
    let config_path = args.config.unwrap_or(get_config_path()?);
    if args.verbose {
        log_message("Verbose mode enabled");
        set_verbose(true);
    }
    let cfg = Arc::new(config::load_config(config_path.to_str().unwrap())?);
    let idle_timer = Arc::new(Mutex::new(idle_timer::IdleTimer::new(&cfg)));
    idle_timer.lock().await.init().await;

    // --- Spawn background tasks ---
    idle_timer::spawn_idle_task(Arc::clone(&idle_timer)).await;
    input::spawn_input_task(Arc::clone(&idle_timer));

    // --- Spawn suspend event listener ---
    let lid_idle_timer = Arc::clone(&idle_timer);
    tokio::spawn(async move {
        if let Err(e) = suspend::listen_for_suspend_events(lid_idle_timer).await {
            log_error_message(&format!("D-Bus suspend event listener failed: {}", e));
        }
    });

    // AC/Battery Detection
    let idle_clone = Arc::clone(&idle_timer);
    tokio::spawn(async move {
        // Detect laptop or desktop
        let is_laptop = crate::utils::is_laptop();

        // Detect initial power state and log it
        let last_on_ac = crate::power_detection::detect_initial_power_state(is_laptop);

        // Set initial state in IdleTimer
        {
            let mut timer = idle_clone.lock().await;
            timer.on_ac = last_on_ac;
        }

        // Poll every 5 seconds (or any interval you want)
        let mut ticker = tokio::time::interval(Duration::from_secs(5));
        let mut last_on_ac = last_on_ac;
        loop {
            ticker.tick().await;

            // Skip desktop (always AC)
            if !is_laptop {
                continue;
            }

            // Check current AC state
            let on_ac = crate::power_detection::is_on_ac_power(is_laptop);

            // Only update if state changed
            if on_ac != last_on_ac {
                last_on_ac = on_ac;
                log_message(&format!("Power source changed: {}", if on_ac { "AC" } else { "Battery" }));

                // Update IdleTimer
                idle_clone.lock().await.update_power_source(on_ac).await;
            }
        }
    });

    // --- Spawn app inhibit task ---
    let app_inhibitor = app_inhibit::spawn_app_inhibit_task(
        Arc::clone(&idle_timer),
        Arc::clone(&cfg)
    );

    // --- Wayland setup ---
    let wl_data = setup_wayland(Arc::clone(&idle_timer), cfg.respect_idle_inhibitors).await?;

    // --- Control socket ---  
    ipc::spawn_control_socket_with_listener(
        Arc::clone(&idle_timer),
        Arc::clone(&app_inhibitor),
        config_path.to_str().unwrap().to_string(),
        listener,
    ).await;

    // --- Shutdown handler ---    
    setup_shutdown_handler(
        Arc::clone(&idle_timer),
        Arc::clone(&wl_data),
        Arc::clone(&app_inhibitor),
    ).await;

    // --- Run main async tasks ---
    let local = LocalSet::new();
    local.run_until(async {
        if cfg.monitor_media {
            media::spawn_media_monitor(Arc::clone(&idle_timer))?;
        }
        log_message(&format!("Running. Idle actions loaded: {}", cfg.actions.len()));
        std::future::pending::<()>().await;
        #[allow(unreachable_code)]
        Ok::<(), eyre::Report>(())
    }).await?;

    Ok(())
}

/// Cleanup socket on exit or panic
fn setup_cleanup_handler() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static CLEANUP_REGISTERED: AtomicBool = AtomicBool::new(false);

    if CLEANUP_REGISTERED.swap(true, Ordering::SeqCst) {
        return;
    }

    let _ = ctrlc::set_handler(move || {
        let _ = fs::remove_file(SOCKET_PATH);
        std::process::exit(0);
    });

    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = fs::remove_file(SOCKET_PATH);
        default_panic(panic_info);
    }));
}

/// Determine default config path
fn get_config_path() -> Result<PathBuf> {
    if let Some(mut path) = dirs::home_dir() {
        path.push(".config/stasis/stasis.rune");
        if path.exists() {
            return Ok(path);
        }
    }
    let fallback = PathBuf::from("/etc/stasis/stasis.rune");
    if fallback.exists() {
        return Ok(fallback);
    }
    Err(eyre::eyre!("Could not find stasis configuration file"))
}

/// Async shutdown handler (Ctrl+C / SIGTERM)
async fn setup_shutdown_handler(
    idle_timer: Arc<Mutex<idle_timer::IdleTimer>>,
    wl_data: Arc<Mutex<WaylandIdleData>>,
    app_inhibitor: Arc<Mutex<app_inhibit::AppInhibitor>>,
) {
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).unwrap();
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();

    tokio::spawn({
        let idle_timer = Arc::clone(&idle_timer);
        let wl_data = Arc::clone(&wl_data);
        let app_inhibitor = Arc::clone(&app_inhibitor);
        async move {
            tokio::select! {
                _ = sigint.recv() => log_message("Received SIGINT, shutting down..."),
                _ = sigterm.recv() => log_message("Received SIGTERM, shutting down..."),
            }

            // Shutdown idle timer
            idle_timer.lock().await.shutdown().await;

            // Shutdown app inhibitor
            app_inhibitor.lock().await.shutdown().await;

            // Notify Wayland event loop
            let shutdown_notify = {
                let wl_locked = wl_data.lock().await;
                Arc::clone(&wl_locked.shutdown)
            };
            shutdown_notify.notify_waiters();

            let _ = std::fs::remove_file(SOCKET_PATH);
            std::process::exit(0);
        }
    });
}

