use eyre::Result;
use mpris::{PlayerFinder, PlaybackStatus};
use crate::idle_timer::IdleTimer;
use std::sync::Arc;
use std::time::Duration;
use tokio::time;

use tokio::task;

/// Setup MPRIS monitoring using a repeating Tokio-local task
pub fn setup(idle_timer: Arc<tokio::sync::Mutex<IdleTimer>>) -> Result<()> {
    let finder = PlayerFinder::new()?; // may return Error -> propagated
    let idle_timer_clone = Arc::clone(&idle_timer);
    let interval = Duration::from_secs(1);

    // Spawn a Tokio-local task because MPRIS types are !Send
    task::spawn_local(async move {
        let mut ticker = time::interval(interval);
        loop {
            ticker.tick().await;

            match finder.find_all() {
                Ok(players) => {
                    for player in players {
                        if let Ok(status) = player.get_playback_status() {
                            if status == PlaybackStatus::Playing {
                                let mut timer = idle_timer_clone.lock().await;
                                timer.reset();
                                // println!("[idleman] Media playing, timer reset");
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[idleman] MPRIS: failed to list players: {:?}", e);
                }
            }
        }
    });

    Ok(())
}
