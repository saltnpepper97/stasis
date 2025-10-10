use std::fs;
use std::path::Path;

use crate::log::{log_error_message, log_message}; // assuming you have this

/// Represents brightness state as absolute value (not percent)
#[derive(Clone, Debug)]
pub struct BrightnessState {
    pub value: u32,
    pub device: String,
}

pub fn capture_brightness() -> Option<BrightnessState> {
    let base = Path::new("/sys/class/backlight");
    let device = fs::read_dir(base).ok()?.next()?.ok()?.file_name();
    let device = device.to_string_lossy().to_string();

    let current = fs::read_to_string(base.join(&device).join("brightness")).ok()?;

    Some(BrightnessState {
        value: current.trim().parse().ok()?,
        device,
    })
}

pub fn restore_brightness(state: &BrightnessState) {
    let path = format!("/sys/class/backlight/{}/brightness", state.device);
    if let Err(e) = fs::write(&path, state.value.to_string()) {
        log_error_message(&format!(
            "Warning: Failed to restore brightness at {}: {}. \
            You may need root privileges or a udev rule to write to this file.",
            path, e
        ));
    } else {
        log_message(&format!("Brightness restored to {} for device {}", state.value, state.device));
    }
}

