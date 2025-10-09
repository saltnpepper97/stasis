use std::sync::Arc;
use tokio::net::UnixListener;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{
    app_inhibit::AppInhibitor,
    config,
    idle_timer::IdleTimer,
    log::{log_error_message, log_message},
    SOCKET_PATH,
};

/// Spawn the control socket task using a pre-bound listener
pub async fn spawn_control_socket_with_listener(
    idle_timer: Arc<tokio::sync::Mutex<IdleTimer>>,
    app_inhibitor: Arc<tokio::sync::Mutex<AppInhibitor>>,
    cfg_path: String,
    listener: UnixListener,
) {
    tokio::spawn(async move {
        loop {
            if let Ok((mut stream, _addr)) = listener.accept().await {
                let mut buf = vec![0u8; 64];               
                if let Ok(n) = stream.read(&mut buf).await {
                    let cmd = String::from_utf8_lossy(&buf[..n]).trim().to_string();

                    match cmd.as_str() {
                        "reload" => {
                            match config::load_config(&cfg_path) {
                                Ok(new_cfg) => {
                                    let mut timer = idle_timer.lock().await;
                                    timer.update_from_config(&new_cfg).await;
                                    log_message("Config reloaded successfully");
                                }
                                Err(_) => log_error_message("Failed to reload config"),
                            }
                        }

                        "pause" => {
                            let mut timer = idle_timer.lock().await;
                            timer.pause(true);
                            log_message("Idle timers paused");
                        }

                        "resume" => {
                            let mut timer = idle_timer.lock().await;
                            timer.resume(true);
                            log_message("Idle timers resumed");
                        }

                        "trigger_idle" => {
                            let mut timer = idle_timer.lock().await;
                            timer.trigger_idle().await;
                            log_message("Forced idle actions triggered");
                        }

                        "trigger_presuspend" => {
                            let mut timer = idle_timer.lock().await;
                            timer.trigger_pre_suspend(false, true).await;
                            log_message("Pre-suspend command triggered");
                        }

                        "stop" => {
                            log_message("Received stop command, shutting down gracefully");

                            let idle_timer_clone = Arc::clone(&idle_timer);
                            tokio::spawn(async move {
                                let mut timer = idle_timer_clone.lock().await;
                                timer.shutdown().await;
                                log_message("IdleTimer shutdown complete, exiting process");
                                let _ = std::fs::remove_file(SOCKET_PATH);
                                std::process::exit(0);
                            });
                        }

                        // ✅ NEW COMMAND: toggle manual inhibition (Waybar support)
                        "toggle_inhibit" => {
                            let mut timer = idle_timer.lock().await;
                            let currently_inhibited = timer.is_manually_inhibited();

                            if currently_inhibited {
                                timer.set_manual_inhibit(false).await;
                                log_message("Manual inhibit disabled (toggle)");
                            } else {
                                timer.set_manual_inhibit(true).await;
                                log_message("Manual inhibit enabled (toggle)");
                            }

                            // Send JSON response for Waybar feedback
                            let response = if currently_inhibited {
                                serde_json::json!({
                                    "text": "⌚",
                                    "tooltip": "Idle inhibition cleared"
                                })
                            } else {
                                serde_json::json!({
                                    "text": "🚫",
                                    "tooltip": "Idle inhibition active"
                                })
                            };

                            if let Err(e) = stream.write_all(response.to_string().as_bytes()).await {
                                log_error_message(&format!("Failed to send toggle response: {e}"));
                            }
                        }

                        "info" | "info --json" => {
                            let as_json = cmd.contains("--json");

                            let idle = idle_timer.lock().await;
                            let idle_time = idle.elapsed_idle();
                            let mut inhibitor = app_inhibitor.lock().await;
                            let app_blocking = inhibitor.is_any_app_running().await;
                            let idle_inhibited = idle.paused || idle.manually_paused || app_blocking;
                            let uptime = idle.start_time.elapsed();

                            if as_json {
                                let output = if idle_inhibited {
                                    serde_json::json!({
                                        "text": "☕",
                                        "tooltip": format!(
                                            "Idle inhibited\nIdle time: {}s\nUptime: {}s\nPaused: {}\nManually paused: {}\nApp blocking: {}",
                                            idle_time.as_secs(),
                                            uptime.as_secs(),
                                            idle.paused,
                                            idle.manually_paused,
                                            app_blocking
                                        )
                                    })
                                } else {
                                    serde_json::json!({
                                        "text": "⌚",
                                        "tooltip": format!(
                                            "Idle active\nIdle time: {}s\nUptime: {}s\nPaused: {}\nManually paused: {}\nApp blocking: {}",
                                            idle_time.as_secs(),
                                            uptime.as_secs(),
                                            idle.paused,
                                            idle.manually_paused,
                                            app_blocking
                                        )
                                    })
                                };

                                if let Err(e) = stream.write_all(output.to_string().as_bytes()).await {
                                    log_error_message(&format!("Failed to send JSON info: {e}"));
                                }
                            } else {
                                let stats = idle.cfg.pretty_print(
                                    Some(idle_time),
                                    Some(uptime),
                                    Some(idle_inhibited),
                                );

                                if let Err(e) = stream.write_all(stats.as_bytes()).await {
                                    log_error_message(&format!("Failed to send info: {e}"));
                                }
                            }
                        }

                        _ => log_error_message(&format!("Unknown control command: {}", cmd)),
                    }
                }
            }
        }
    });
}
