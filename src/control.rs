use std::sync::Arc;
use tokio::net::UnixListener;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use crate::{config, idle_timer::IdleTimer, log::{log_message, log_error_message}};

/// Spawn the control socket task using a pre-bound listener
pub fn spawn_control_socket_with_listener(
    idle_timer: Arc<tokio::sync::Mutex<IdleTimer>>,
    cfg_path: String,
    listener: UnixListener,
) {
    tokio::spawn(async move {
        let listener = listener; // Already bound in main.rs

        loop {
            if let Ok((mut stream, _addr)) = listener.accept().await {
                let mut buf = vec![0u8; 32];               
                if let Ok(n) = stream.read(&mut buf).await {
                    let cmd = String::from_utf8_lossy(&buf[..n]).trim().to_string();

                    match cmd.as_str() {
                        "reload" => {
                            match config::load_config(&cfg_path) {
                                Ok(new_cfg) => {
                                    let mut timer = idle_timer.lock().await;
                                    timer.update_from_config(&new_cfg);
                                    log_message("Config reloaded successfully");
                                }
                                Err(_) => {
                                    log_error_message("Failed to reload config");
                                }
                            }
                        }
                        "pause" => {
                            let mut timer = idle_timer.lock().await;
                            timer.pause();
                            log_message("Idle timers paused");
                        }
                        "resume" => {
                            let mut timer = idle_timer.lock().await;
                            timer.resume();
                            log_message("Idle timers resumed");
                        }
                        "trigger_idle" => {
                            let mut timer = idle_timer.lock().await;
                            timer.trigger_idle();
                            log_message("Forced idle actions triggered");
                        }
                        "trigger_presuspend" => {
                            let mut timer = idle_timer.lock().await;
                            timer.trigger_pre_suspend(false);
                            log_message("Pre-suspend command triggered");
                        }
                        "stop" => {
                            log_message("Received stop command, shutting down");
                            std::process::exit(0);
                        }
                        "stats" => {
                            let idle = idle_timer.lock().await;
                            let stats = format!(
                                "Idle time: {:.2?}\n\n{}",
                                idle.elapsed_idle(),
                                idle.cfg.pretty_print() // Assuming you store the config in IdleTimer or pass it here
                            );
                            if let Err(e) = stream.write_all(stats.as_bytes()).await {
                                log_error_message(&format!("Failed to send stats: {e}"));
                            }
                        }

                        _ => {
                            log_error_message(&format!("Unknown control command: {}", cmd));
                        }
                    }
                }
            }
        }
    });
}

