use std::sync::Arc;
use std::time::Duration;
use std::os::unix::io::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::fs::OpenOptions;
use input::{Libinput, LibinputInterface};
use input::event::Event;

use crate::idle_timer::IdleTimer;

/// Minimal Libinput interface
struct MyInterface;

impl LibinputInterface for MyInterface {
    fn open_restricted(
        &mut self,
        path: &std::path::Path,
        flags: i32,
    ) -> Result<std::os::unix::io::OwnedFd, i32> {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(flags)
            .open(path)
            .map(|f| f.into())
            .map_err(|_| -1)
    }

    fn close_restricted(&mut self, fd: std::os::unix::io::OwnedFd) {
        drop(fd)
    }
}

/// Spawn a blocking task that watches libinput and resets IdleTimer
pub fn spawn_libinput_task(idle_timer: Arc<tokio::sync::Mutex<IdleTimer>>) {
    tokio::task::spawn_blocking(move || {
        // Silence libinput errors
        silence_stderr();

        let mut li = Libinput::new_with_udev(MyInterface);
        li.udev_assign_seat("seat0").expect("Failed to assign seat");

        let rt = tokio::runtime::Handle::current();

        loop {
            if let Err(_) = li.dispatch() {
                continue;
            }

            while let Some(event) = li.next() {
                match event {
                    Event::Keyboard(_) | Event::Pointer(_) => {
                        let idle_timer_clone = Arc::clone(&idle_timer);
                        rt.block_on(async move {
                            let mut timer = idle_timer_clone.lock().await;
                            timer.reset(); // <-- no cfg needed
                        });
                    }
                    _ => {}
                }
            }

            std::thread::sleep(Duration::from_millis(5));
        }
    });
}

fn silence_stderr() {
    // Open /dev/null
    let dev_null = OpenOptions::new()
        .write(true)
        .open("/dev/null")
        .unwrap();

    // Replace stderr with /dev/null
    unsafe {
        libc::dup2(dev_null.as_raw_fd(), libc::STDERR_FILENO);
    }
}

