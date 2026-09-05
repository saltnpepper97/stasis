// Author: Dustin Pilgrim
// License: GPL-3.0-only

use tokio::{
    sync::{mpsc, oneshot},
    time::{Duration, timeout},
};

use crate::core::manager_msg::ManagerMsg;

const IPC_REPLY_TIMEOUT: Duration = Duration::from_secs(2);

pub async fn route_command(cmd: &str, tx: &mpsc::Sender<ManagerMsg>) -> String {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return "ERROR: empty command".to_string();
    }

    // ---------------- info ----------------
    if cmd == "info" || cmd.starts_with("info ") {
        let as_json = cmd.split_whitespace().any(|t| t == "--json");

        let (reply_tx, reply_rx) = oneshot::channel();

        // If the manager loop is gone / channel closed, daemon is *effectively* not running.
        if tx
            .send(ManagerMsg::GetInfo { reply: reply_tx })
            .await
            .is_err()
        {
            return if as_json {
                r#"{"text":"","alt":"not_running","class":"not_running","tooltip":"Stasis not running","profile":null}"#
                    .to_string()
            } else {
                "Stasis daemon not running".to_string()
            };
        }

        // Avoid hanging forever if daemon is wedged.
        return match timeout(IPC_REPLY_TIMEOUT, reply_rx).await {
            Ok(Ok(snapshot)) => {
                if as_json {
                    serde_json::to_string(&snapshot.waybar).unwrap_or_else(|_| {
                        r#"{"text":"","alt":"not_running","class":"not_running","tooltip":"stasis: json encode failed","profile":null}"#
                            .to_string()
                    })
                } else {
                    snapshot.pretty_text
                }
            }

            // oneshot sender dropped (daemon didn't reply)
            Ok(Err(_)) => {
                if as_json {
                    r#"{"text":"","alt":"not_running","class":"not_running","tooltip":"No response from daemon","profile":null}"#
                        .to_string()
                } else {
                    "No response from daemon".to_string()
                }
            }

            // timeout waiting for daemon reply
            Err(_) => {
                if as_json {
                    r#"{"text":"","alt":"not_running","class":"not_running","tooltip":"Timed out waiting for daemon","profile":null}"#
                        .to_string()
                } else {
                    "Timed out waiting for daemon".to_string()
                }
            }
        };
    }

    // ---------------- blame ----------------
    if cmd == "blame" || cmd.starts_with("blame ") {
        let as_json = cmd.split_whitespace().any(|t| t == "--json");
        let (reply_tx, reply_rx) = oneshot::channel();

        if tx
            .send(ManagerMsg::GetBlame { reply: reply_tx })
            .await
            .is_err()
        {
            return if as_json {
                r#"{"error":"daemon not running"}"#.to_string()
            } else {
                "Stasis daemon not running".to_string()
            };
        }

        return match timeout(IPC_REPLY_TIMEOUT, reply_rx).await {
            Ok(Ok(snapshot)) => {
                if as_json {
                    serde_json::to_string(&snapshot)
                        .unwrap_or_else(|_| r#"{"error":"json encode failed"}"#.to_string())
                } else {
                    snapshot.render_human()
                }
            }
            Ok(Err(_)) => {
                if as_json {
                    r#"{"error":"no response from daemon"}"#.to_string()
                } else {
                    "No response from daemon".to_string()
                }
            }
            Err(_) => {
                if as_json {
                    r#"{"error":"timed out waiting for daemon"}"#.to_string()
                } else {
                    "Timed out waiting for daemon".to_string()
                }
            }
        };
    }

    // ---------------- reload ----------------
    if cmd == "reload" {
        return crate::ipc::handlers::reload::handle_reload(tx).await;
    }

    // ---------------- toggle-inhibit ----------------
    if cmd == "toggle-inhibit" {
        return crate::ipc::handlers::toggle_inhibit::handle_toggle_inhibit(tx).await;
    }

    // ---------------- stop ----------------
    if cmd == "stop" {
        return crate::ipc::handlers::stop::handle_stop(tx).await;
    }

    // ---------------- resume ----------------
    if cmd == "resume" {
        return crate::ipc::handlers::pause::handle_resume(tx).await;
    }

    // ---------------- pause ----------------
    if cmd == "pause" || cmd.starts_with("pause ") {
        let args = cmd.strip_prefix("pause").unwrap_or("").trim();
        return crate::ipc::handlers::pause::handle_pause(args, tx).await;
    }

    // ---------------- trigger ----------------
    if cmd == "trigger" || cmd.starts_with("trigger ") {
        let args = cmd.strip_prefix("trigger").unwrap_or("").trim();
        return crate::ipc::handlers::trigger::handle_trigger(args, tx).await;
    }

    // ---------------- dump ----------------
    if cmd == "dump" || cmd.starts_with("dump ") {
        let args = cmd.strip_prefix("dump").unwrap_or("").trim();
        return crate::ipc::handlers::dump::handle_dump(args).await;
    }

    // ---------------- profile ----------------
    if cmd == "profile" || cmd.starts_with("profile ") {
        let args = cmd.strip_prefix("profile").unwrap_or("").trim();
        return crate::ipc::handlers::profile::handle_profile(args, tx).await;
    }

    // ---------------- list ----------------
    if cmd == "list" || cmd.starts_with("list ") {
        let args = cmd.strip_prefix("list").unwrap_or("").trim();
        return crate::ipc::handlers::list::handle_list(args, tx).await;
    }

    "ERROR: unknown command".to_string()
}
