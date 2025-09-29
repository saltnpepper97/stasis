use eyre::Result;
use std::sync::Arc;
use std::path::PathBuf;
use tokio::task::LocalSet;
use clap::{Parser, Subcommand};
use tokio::net::UnixListener;
use std::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

mod actions;
mod app_inhibit;
mod brightness;
mod config;
mod control;
mod idle_timer;
mod libinput;
mod log;
mod media;
mod power_detection;
mod utils;
mod wayland_input;

use log::{log_message, log_error_message, set_verbose};

#[derive(Parser, Debug)]
#[command(
    name = "Stasis",
    version = env!("CARGO_PKG_VERSION"), 
    about = "Capable idle manager for Wayland\n\nFor configuration details, see `man 5 stasis`", 
    long_about = None
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
    ReloadConfig,
    Pause,
    Resume,
    TriggerIdle,
    TriggerPreSuspend,
    Stop,
    Stats,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();

    // --- Check Wayland environment ---
    if std::env::var("WAYLAND_DISPLAY").is_err() {
        eprintln!("Error: Wayland is not detected. Stasis requires Wayland to run.");
        std::process::exit(1);
    }

    let socket_path = "/tmp/stasis.sock";

    // --- HANDLE SUBCOMMAND ---
    if let Some(cmd) = &args.command {
        use tokio::net::UnixStream;

        let msg = match cmd {
            Commands::ReloadConfig => "reload",
            Commands::Pause => "pause",
            Commands::Resume => "resume",
            Commands::TriggerIdle => "trigger_idle",
            Commands::TriggerPreSuspend => "trigger_presuspend",
            Commands::Stop => "stop",
            Commands::Stats => "stats",
        };

        match UnixStream::connect(socket_path).await {
            Ok(mut stream) => {
                let _ = stream.write_all(msg.as_bytes()).await;
                if msg == "stats" {
                    let mut response = Vec::new();
                    let _ = stream.read_to_end(&mut response).await;
                    println!("{}", String::from_utf8_lossy(&response));
                }
            }
            Err(_) => log_error_message("No running instance found"),
        }

        return Ok(());
    }

    // --- CHECK IF HELP OR VERSION ARGUMENTS ARE PRESENT ---
    let just_help_or_version = std::env::args().any(|a| {
        matches!(a.as_str(), "-V" | "--version" | "-h" | "--help" | "help")
    });

    // --- SINGLE INSTANCE CHECK ---
    if let Ok(_) = tokio::net::UnixStream::connect(socket_path).await {
        if !just_help_or_version {
            println!("Another instance of Stasis is already running.");
        }
        log_error_message("Another instance is already running.");
        return Ok(());
    }

    // Remove stale socket before binding
    let _ = fs::remove_file(socket_path);

    // --- BIND SOCKET ---
    let listener = match UnixListener::bind(socket_path) {
        Ok(l) => l,
        Err(_) => {
            log_error_message("Failed to bind control socket. Another instance may be running.");
            return Ok(());
        }
    };

    // --- CONFIG ---
    let config_path = if let Some(path) = args.config {
        path
    } else {
        get_config_path()?
    };

    if args.verbose {
        log_message("Verbose mode enabled");
        set_verbose(true);
    }

    let cfg = Arc::new(config::load_config(config_path.to_str().unwrap())?);
    let idle_timer = Arc::new(tokio::sync::Mutex::new(idle_timer::IdleTimer::new(&cfg)));

    idle_timer::spawn_idle_task(Arc::clone(&idle_timer));
    libinput::spawn_libinput_task(Arc::clone(&idle_timer));
    app_inhibit::spawn_app_inhibit_task(Arc::clone(&idle_timer), Arc::clone(&cfg));

    // Use pre-bound listener for control socket
    control::spawn_control_socket_with_listener(
        Arc::clone(&idle_timer),
        config_path.to_str().unwrap().to_string(),
        listener,
    );

    let local = LocalSet::new();
    local.run_until(async {
        wayland_input::setup(Arc::clone(&idle_timer), cfg.respect_idle_inhibitors).await?;
        if cfg.monitor_media {
            media::setup(Arc::clone(&idle_timer))?;
        }
        log_message(&format!("Running. Idle actions loaded: {}", cfg.actions.len()));
        std::future::pending::<()>().await;
        #[allow(unreachable_code)]
        Ok::<(), eyre::Report>(())
    })
    .await?;

    Ok(())
}

/// Returns the appropriate config file path
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

    Err(eyre::eyre!(
        "Could not find stasis configuration file in home or /etc/stasis/"
    ))
}
