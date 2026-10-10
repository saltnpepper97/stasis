// Author: Dustin Pilgrim
// License: GPL-3.0-only

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::mpsc;

use crate::core::events::Event;
use crate::core::manager_msg::ManagerMsg;

// Any new pause/resume bumps this generation, so old scheduled resumes become no-ops.
static PAUSE_GEN: AtomicU64 = AtomicU64::new(1);

pub const PAUSE_HELP_MESSAGE: &str = r#"Usage:
  stasis pause
  stasis pause for <duration>
  stasis pause until <time>

Examples:
  stasis pause
  stasis pause for 5m
  stasis pause for 1h30m
  stasis pause for 250ms
  stasis pause until 1:30pm
  stasis pause until 13:30

Duration format:
  - a sequence of <number><unit> parts, like: 1h30m, 5m, 10s, 250ms
  - units: ms, s, m, h, d

Notes:
  - `pause` with no args pauses until you run `stasis resume`.
  - `pause for/until` schedules an automatic resume in the daemon.
"#;

pub async fn handle_pause(args: &str, tx: &mpsc::Sender<ManagerMsg>) -> String {
    let args = args.trim();

    // Match old behavior: `pause help` prints usage.
    if args.eq_ignore_ascii_case("help") || args == "-h" || args == "--help" {
        return PAUSE_HELP_MESSAGE.to_string();
    }

    // Always pause first.
    let now_ms = crate::core::utils::now_ms();
    if tx
        .send(ManagerMsg::Event(Event::ManualPause { now_ms }))
        .await
        .is_err()
    {
        return "ERROR: daemon event channel closed".to_string();
    }

    // No args => indefinite pause until manual resume.
    if args.is_empty() {
        // Invalidate any previous scheduled resumes (since user explicitly paused again).
        PAUSE_GEN.fetch_add(1, Ordering::SeqCst);
        return "Idle timers paused".to_string();
    }

    // Parse: "for ..." | "until ..."
    let parts: Vec<&str> = args.split_whitespace().collect();
    let (mode, rest) = match parts.as_slice() {
        ["for", rest @ ..] if !rest.is_empty() => ("for", rest.join(" ")),
        ["until", rest @ ..] if !rest.is_empty() => ("until", rest.join(" ")),
        _ => {
            return format!("ERROR: invalid pause syntax\n\n{}", PAUSE_HELP_MESSAGE);
        }
    };

    // Compute delay for auto-resume.
    let delay = match mode {
        "for" => match parse_duration(rest.trim()) {
            Ok(d) => d,
            Err(e) => return format!("ERROR: {e}\n\n{}", PAUSE_HELP_MESSAGE),
        },
        "until" => match parse_until_local_time(rest.trim()) {
            Ok(d) => d,
            Err(e) => return format!("ERROR: {e}\n\n{}", PAUSE_HELP_MESSAGE),
        },
        _ => unreachable!(),
    };

    // Human-facing message for the notification when the pause expires.
    // (Manager will emit this if notify_on_unpause=true)
    let notify_message: String = match mode {
        "for" => format!("Resume idle manager after {} pause", rest.trim()),
        "until" => format!(
            "Resume idle manager: pause-until time reached ({})",
            rest.trim()
        ),
        _ => "Resume idle manager".to_string(),
    };

    // If delay is zero-ish, resume immediately (still goes through daemon state machine).
    // Also: bump generation so only this scheduled resume is valid.
    let my_gen = PAUSE_GEN.fetch_add(1, Ordering::SeqCst) + 1;

    {
        let tx2 = tx.clone();
        let msg2 = notify_message.clone();
        tokio::spawn(async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }

            // Only resume if we're still the latest scheduled pause/resume intent.
            if PAUSE_GEN.load(Ordering::SeqCst) != my_gen {
                return;
            }

            let now_ms = crate::core::utils::now_ms();
            let _ = tx2
                .send(ManagerMsg::Event(Event::PauseExpired {
                    now_ms,
                    message: msg2,
                }))
                .await;
        });
    }

    match mode {
        "for" => format!("Idle timers paused for {}", rest.trim()),
        "until" => format!("Idle timers paused until {}", rest.trim()),
        _ => "Idle timers paused".to_string(),
    }
}

pub async fn handle_resume(tx: &mpsc::Sender<ManagerMsg>) -> String {
    // Invalidate any pending scheduled resumes.
    PAUSE_GEN.fetch_add(1, Ordering::SeqCst);

    let now_ms = crate::core::utils::now_ms();
    if tx
        .send(ManagerMsg::Event(Event::ManualResume { now_ms }))
        .await
        .is_err()
    {
        return "ERROR: daemon event channel closed".to_string();
    }
    // Commands run by the daemon may call resume themselves. Acknowledge the
    // queued request without waiting on that same daemon's action executor.
    "Manual pause release requested; other pause sources still apply".to_string()
}

// ---------------- parsing ----------------

fn parse_duration(s: &str) -> Result<Duration, String> {
    // Accept "1h30m", "5m", "10s", "250ms", "1d2h3m4s"
    let s = s.trim();
    if s.is_empty() {
        return Err("missing duration after 'for'".into());
    }

    let mut i = 0usize;
    let bytes = s.as_bytes();
    let mut total_ms: u128 = 0;

    while i < bytes.len() {
        // skip whitespace
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }

        // parse number
        let start_num = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if start_num == i {
            return Err(format!("Duration format: expected number at '{}'", &s[i..]));
        }
        let n: u128 = s[start_num..i]
            .parse()
            .map_err(|_| "Duration format: invalid number".to_string())?;

        // parse unit (ms|s|m|h|d)
        let start_unit = i;
        while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        if start_unit == i {
            return Err("Duration format: missing unit (ms/s/m/h/d)".into());
        }
        let unit = &s[start_unit..i].to_ascii_lowercase();

        let add_ms: u128 = match unit.as_str() {
            "ms" => n,
            "s" => n * 1000,
            "m" => n * 60 * 1000,
            "h" => n * 60 * 60 * 1000,
            "d" => n * 24 * 60 * 60 * 1000,
            _ => {
                return Err(format!(
                    "Duration format: unknown unit '{unit}' (ms/s/m/h/d)"
                ));
            }
        };

        total_ms = total_ms
            .checked_add(add_ms)
            .ok_or_else(|| "Duration too large".to_string())?;
    }

    Ok(Duration::from_millis(
        u64::try_from(total_ms).map_err(|_| "Duration too large".to_string())?,
    ))
}

fn parse_until_local_time(s: &str) -> Result<Duration, String> {
    // Accept:
    //  - "13:30"
    //  - "1:30pm" / "1:30 pm"
    //  - "1pm" / "1 pm"
    let raw = s.trim();
    if raw.is_empty() {
        return Err("missing time after 'until'".into());
    }

    let mut t = raw.to_ascii_lowercase();
    t.retain(|c| !c.is_whitespace());

    let (is_pm, is_am, t) = if let Some(x) = t.strip_suffix("pm") {
        (true, false, x.to_string())
    } else if let Some(x) = t.strip_suffix("am") {
        (false, true, x.to_string())
    } else {
        (false, false, t)
    };

    let (hour, min) = if let Some((hh, mm)) = t.split_once(':') {
        let h: i32 = hh.parse().map_err(|_| "Invalid time (hour)".to_string())?;
        let m: i32 = mm
            .parse()
            .map_err(|_| "Invalid time (minute)".to_string())?;
        (h, m)
    } else {
        // "1pm" style
        let h: i32 = t.parse().map_err(|_| "Invalid time".to_string())?;
        (h, 0)
    };

    if min < 0 || min > 59 {
        return Err("Invalid time: minute must be 0..59".into());
    }

    let mut hour = hour;

    if is_am || is_pm {
        // 12-hour clock
        if hour < 1 || hour > 12 {
            return Err("Invalid time: hour must be 1..12 for am/pm".into());
        }
        if is_pm && hour != 12 {
            hour += 12;
        }
        if is_am && hour == 12 {
            hour = 0;
        }
    } else {
        // 24-hour clock
        if hour < 0 || hour > 23 {
            return Err("Invalid time: hour must be 0..23".into());
        }
    }

    // Compute next occurrence of that local time using chrono (no libc).
    use chrono::{Datelike, Local, LocalResult, NaiveDate, NaiveDateTime, TimeZone};

    let now = Local::now();

    let today: NaiveDate = NaiveDate::from_ymd_opt(now.year(), now.month(), now.day())
        .ok_or_else(|| "Failed to read local date".to_string())?;

    let target_naive_today: NaiveDateTime = today
        .and_hms_opt(hour as u32, min as u32, 0)
        .ok_or_else(|| "Invalid time".to_string())?;

    fn resolve_local(dt: NaiveDateTime) -> Option<chrono::DateTime<Local>> {
        match Local.from_local_datetime(&dt) {
            LocalResult::Single(x) => Some(x),
            LocalResult::Ambiguous(a, b) => {
                // Pick the earlier instant (matches typical “next occurrence” expectation).
                Some(std::cmp::min(a, b))
            }
            LocalResult::None => None, // Nonexistent due to DST jump.
        }
    }

    // Try today; if nonexistent due to DST, search forward up to 2 hours to find a valid local time.
    let mut target = resolve_local(target_naive_today);
    if target.is_none() {
        for add_min in 1..=120 {
            let dt = target_naive_today + chrono::Duration::minutes(add_min);
            if let Some(x) = resolve_local(dt) {
                target = Some(x);
                break;
            }
        }
        if target.is_none() {
            return Err("Invalid time (local time does not exist)".into());
        }
    }

    let mut target = target.ok_or_else(|| "Invalid time".to_string())?;

    // If target <= now, schedule for tomorrow (same wall-clock time).
    if target <= now {
        let tomorrow = today
            .succ_opt()
            .ok_or_else(|| "Failed to compute tomorrow".to_string())?;
        let target_naive_tomorrow = tomorrow
            .and_hms_opt(hour as u32, min as u32, 0)
            .ok_or_else(|| "Invalid time".to_string())?;

        let mut target2 = resolve_local(target_naive_tomorrow);
        if target2.is_none() {
            for add_min in 1..=120 {
                let dt = target_naive_tomorrow + chrono::Duration::minutes(add_min);
                if let Some(x) = resolve_local(dt) {
                    target2 = Some(x);
                    break;
                }
            }
        }

        target = target2.ok_or_else(|| "Invalid time (local time does not exist)".to_string())?;
    }

    let delta = target.signed_duration_since(now);
    if delta.num_milliseconds() <= 0 {
        return Ok(Duration::from_millis(0));
    }

    Ok(Duration::from_millis(
        delta
            .num_milliseconds()
            .try_into()
            .map_err(|_| "Duration too large".to_string())?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resume_acknowledges_request_without_waiting_for_action_executor() {
        let (tx, mut rx) = mpsc::channel(1);
        let reply = tokio::time::timeout(Duration::from_secs(1), handle_resume(&tx))
            .await
            .unwrap();
        assert_eq!(
            reply,
            "Manual pause release requested; other pause sources still apply"
        );
        assert!(matches!(
            rx.recv().await,
            Some(ManagerMsg::Event(Event::ManualResume { .. }))
        ));
        assert!(rx.try_recv().is_err());
    }
}
