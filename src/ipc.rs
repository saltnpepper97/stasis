use std::sync::Arc;
use tokio::net::UnixListener;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;

use crate::{config, idle_timer::IdleTimer, log::{log_message, log_error_message}};
use crate::SOCKET_PATH;
use crate::app_inhibit::AppInhibitor;

/// Spawn the control socket task using a pre-bound listener
pub async fn spawn_control_socket_with_listener(
    idle_timer: Arc<tokio::sync::Mutex<IdleTimer>>,
    app_inhibitor: Arc<tokio::sync::Mutex<AppInhibitor>>,
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
                                    timer.update_from_config(&new_cfg).await;
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

                                // Cleanup socket file before exit
                                let _ = std::fs::remove_file(SOCKET_PATH);

                                std::process::exit(0);
                            });
                        }
                        "info" => {
                            let idle = idle_timer.lock().await;
                            let idle_time = idle.elapsed_idle();
                            let mut inhibitor = app_inhibitor.lock().await;
                            let app_blocking = inhibitor.is_any_app_running().await;
                            let idle_inhibited = idle.paused || app_blocking;
                            let uptime = idle.start_time.elapsed();

                            // Pass runtime info into pretty_print
                            let stats = idle.cfg.pretty_print(
                                Some(idle_time),
                                Some(uptime),
                                Some(idle_inhibited),
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
