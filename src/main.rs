use eyre::Result;
use std::sync::Arc;
use std::path::PathBuf;
use tokio::task::LocalSet;

mod app_inhibit;
mod config;
mod idle_timer;
mod libinput;
mod wayland_input;
mod media;
mod actions;
mod utils;

use utils::log_to_cache;

#[tokio::main(flavor = "current_thread")] // single-threaded runtime for !Send tasks
async fn main() -> Result<()> {
    // Resolve config path with fallback
    let config_path = get_config_path()?;

    // Load configuration
    let cfg = Arc::new(config::load_config(config_path.to_str().unwrap())?);

    // Shared IdleTimer (tokio::sync::Mutex for all tasks)
    let idle_timer = Arc::new(tokio::sync::Mutex::new(idle_timer::IdleTimer::new(&cfg)));

    // Spawn periodic fallback idle task
    idle_timer::spawn_idle_task(Arc::clone(&idle_timer));

    // Spawn libinput monitoring
    libinput::spawn_libinput_task(Arc::clone(&idle_timer));

    // --- Spawn App Inhibit Task ---
    app_inhibit::spawn_app_inhibit_task(Arc::clone(&idle_timer), Arc::clone(&cfg));

    // Tokio LocalSet for !Send tasks (Wayland, MPRIS)
    let local = LocalSet::new();

    local.run_until(async {
        // Setup Wayland idle/compositor monitoring
        wayland_input::setup(Arc::clone(&idle_timer), cfg.respect_idle_inhibitors).await?;

        // Optional media (MPRIS) monitoring
        if cfg.monitor_media {
            media::setup(Arc::clone(&idle_timer))?;
        }

        log_to_cache(&format!("[Stasis] Running. Idle actions loaded: {}", cfg.actions.len()));

        // Keep main task alive indefinitely
        std::future::pending::<()>().await;

        #[allow(unreachable_code)]
        Ok::<(), eyre::Report>(())
    })
    .await?;

    Ok(())
}

/// Returns the appropriate config file path, falling back to /etc/stasis/stasis.rune
fn get_config_path() -> Result<PathBuf> {
    // Primary: $HOME/.config/stasis/stasis.rune
    if let Some(mut path) = dirs::home_dir() {
        path.push(".config/stasis/stasis.rune");
        if path.exists() {
            return Ok(path);
        }
    }

    // Fallback: /etc/stasis/stasis.rune
    let fallback = PathBuf::from("/etc/stasis/stasis.rune");
    if fallback.exists() {
        return Ok(fallback);
    }

    // If neither exists, error out
    Err(eyre::eyre!(
        "Could not find stasis configuration file in home or /etc/stasis/"
    ))
}
