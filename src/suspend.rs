use std::sync::Arc;
use futures::StreamExt;
use tokio::sync::Mutex;
use zbus::{Connection, fdo::Result as ZbusResult, Proxy};
use crate::idle_timer::IdleTimer;
use crate::log;

pub async fn listen_for_suspend_events(idle_timer: Arc<Mutex<IdleTimer>>) -> ZbusResult<()> {
    // Connect to the system bus
    let connection = Connection::system().await?;
    
    // Create proxy (v5+)
    let proxy = Proxy::new(
        &connection,
        "org.freedesktop.login1",        // destination
        "/org/freedesktop/login1",       // path
        "org.freedesktop.login1.Manager" // interface
    ).await?;
    
    // Listen to PrepareForSleep signals
    let mut stream = proxy.receive_signal("PrepareForSleep").await?;
    
    log::log_message("Listening for D-Bus suspend events...");
    
    while let Some(signal) = stream.next().await {
        // Deserialize the body directly to bool
        let going_to_sleep: bool = signal.body().deserialize()
            .unwrap_or(false);
        
        let mut timer = idle_timer.lock().await;
        
        if going_to_sleep {
            log::log_message("System is preparing to suspend...");
            timer.trigger_pre_suspend(false, true).await;
        } else {
            log::log_message("System resumed from sleep");
            if let Some(cmd) = &timer.resume_command {
                let cmd_clone = cmd.clone();
                timer.spawn_task_limited(async move {
                    let _ = crate::actions::run_command_silent(&cmd_clone).await;
                });
            }
        }
    }
    
    Ok(())
}
