use std::{sync::Arc, time::Duration};
use eyre::Result;
use mpris::{PlayerFinder, PlaybackStatus};
use tokio::{task, time};

use crate::core::legacy::timer::LegacyIdleTimer;
use crate::log::log_error_message;

/// Setup MPRIS monitoring using a Tokio task
pub fn spawn_media_monitor(idle_timer: Arc<tokio::sync::Mutex<LegacyIdleTimer>>) -> Result<()> {
    let idle_timer_clone = Arc::clone(&idle_timer);
    let interval = Duration::from_secs(2);

    task::spawn(async move {
        let mut ticker = time::interval(interval);
        let mut media_playing = false;

        loop {
            ticker.tick().await;

            // Check media players fresh each tick
            let any_playing = match PlayerFinder::new() {
                Ok(finder) => match finder.find_all() {
                    Ok(players) => players.iter().any(|player| {
                        player.get_playback_status()
                            .map(|s| s == PlaybackStatus::Playing)
                            .unwrap_or(false)
                    }),
                    Err(e) => {
                        log_error_message(&format!("MPRIS: failed to list players: {:?}", e));
                        false
                    }
                },
                Err(e) => {
                    log_error_message(&format!("MPRIS: failed to create finder: {:?}", e));
                    false
                }
            };

            // Pause or resume idle timer based on media playback
            let mut timer = idle_timer_clone.lock().await;
            if any_playing && !media_playing {
                timer.pause(false);
                media_playing = true;
            } else if !any_playing && media_playing {
                timer.resume(false);
                media_playing = false;
            }
        }
    });

    Ok(())
}
