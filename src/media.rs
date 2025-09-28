use std::{sync::Arc, time::Duration};
use eyre::Result;
use mpris::{PlayerFinder, PlaybackStatus};
use tokio::{task, time};

use crate::idle_timer::IdleTimer;
use crate::log::{log_message, log_error_message};

/// Setup MPRIS monitoring using a repeating Tokio-local task
pub fn setup(idle_timer: Arc<tokio::sync::Mutex<IdleTimer>>) -> Result<()> {
    let finder = PlayerFinder::new()?; // may return Error -> propagated
    let idle_timer_clone = Arc::clone(&idle_timer);
    let interval = Duration::from_secs(1);

    // Track if we already detected media playing
    let mut media_playing = false;

    task::spawn_local(async move {
        let mut ticker = time::interval(interval);
        loop {
            ticker.tick().await;

            let mut any_playing = false;

            match finder.find_all() {
                Ok(players) => {
                    for player in players {
                        if let Ok(status) = player.get_playback_status() {
                            if status == PlaybackStatus::Playing {
                                any_playing = true;
                                break; // no need to check others
                            }
                        }
                    }
                }
                Err(e) => {
                    log_error_message(&format!("MPRIS: failed to list players: {:?}", e));
                }
            }

            if any_playing {
                if !media_playing {
                    // Only log when playback starts
                    log_message("Media playback detected, timers paused");
                    let mut timer = idle_timer_clone.lock().await;
                    timer.pause();
                }
                media_playing = true;
            } else {
                if media_playing {
                    // Playback just stopped
                    log_message("Media playback stopped, timers resumed");
                    let mut timer = idle_timer_clone.lock().await;
                    timer.resume();
                }
                media_playing = false;
            }
        }
    });

    Ok(())
}
