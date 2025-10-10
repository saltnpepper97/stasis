pub mod actions;
pub mod brightness;
pub mod legacy;
pub mod pre_suspend;

pub use legacy::LegacyIdleTimer;
use crate::config::IdleConfig;

/// Build idle timer (legacy only for now)
pub fn build_idle_timer(cfg: &IdleConfig) -> LegacyIdleTimer {
    LegacyIdleTimer::new(cfg)
}
