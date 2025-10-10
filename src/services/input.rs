use std::{
    fs::OpenOptions,
    os::unix::{
        fs::OpenOptionsExt,
        io::{AsRawFd, OwnedFd},
    },
    sync::Arc,
    time::Duration,
};
use input::{Libinput, LibinputInterface};
use input::event::Event;
use tokio::sync::Mutex;

use crate::core::legacy::timer::LegacyIdleTimer;

/// Minimal libinput interface
struct MyInterface;

impl LibinputInterface for MyInterface {
    fn open_restricted(
        &mut self,
        path: &std::path::Path,
        flags: i32,
    ) -> Result<OwnedFd, i32> {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(flags)
            .open(path)
            .map(|f| f.into())
            .map_err(|_| -1)
    }

    fn close_restricted(&mut self, fd: OwnedFd) {
        drop(fd)
    }
}

/// Spawn a blocking task that watches libinput events
/// and resets the IdleTimer when input occurs.
pub fn spawn_input_task(idle_timer: Arc<Mutex<LegacyIdleTimer>>) {
    let idle_timer_clone = Arc::clone(&idle_timer);

    tokio::task::spawn_blocking(move || {
        // Silence libinput errors
        silence_stderr();

        let mut li = Libinput::new_with_udev(MyInterface);
        if let Err(e) = li.udev_assign_seat("seat0") {
            eprintln!("Failed to assign seat: {:?}", e);
            return;
        }

        let rt = tokio::runtime::Handle::current();

        loop {
            // Dispatch events
            if li.dispatch().is_err() {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }

            // Batch events
            let mut reset_needed = false;
            while let Some(event) = li.next() {
                match event {
                    Event::Keyboard(_) | Event::Pointer(_) => {
                        reset_needed = true;
                    }
                    _ => {}
                }
            }

            if reset_needed {
                rt.block_on(async {
                    let mut timer = idle_timer_clone.lock().await;
                    timer.reset();
                });
            }

            std::thread::sleep(Duration::from_millis(10));
        }
    });
}

/// Redirect libinput stderr to /dev/null to avoid spam
fn silence_stderr() {
    if let Ok(dev_null) = OpenOptions::new().write(true).open("/dev/null") {
        unsafe {
            libc::dup2(dev_null.as_raw_fd(), libc::STDERR_FILENO);
        }
    }
}

