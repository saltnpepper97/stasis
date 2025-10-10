use crate::config::model::IdleConfig;
use crate::utils;

impl IdleConfig {
    pub fn pretty_print(
        &self,
        idle_time: Option<std::time::Duration>,
        uptime: Option<std::time::Duration>,
        is_inhibited: Option<bool>,
    ) -> String {
        let mut out = String::new();

        // General settings
        out.push_str("General:\n");
        out.push_str(&format!(
            "  ResumeCommand      = {}\n",
            self.resume_command.as_deref().unwrap_or("-")
        ));
        out.push_str(&format!(
            "  PreSuspendCommand  = {}\n",
            self.pre_suspend_command.as_deref().unwrap_or("-")
        ));
        out.push_str(&format!(
            "  MonitorMedia       = {}\n",
            if self.monitor_media { "true" } else { "false" }
        ));
        out.push_str(&format!(
            "  RespectInhibitors  = {}\n",
            if self.respect_idle_inhibitors { "true" } else { "false" }
        ));
        out.push_str(&format!(
            "  DebounceSeconds  = {}\n",
            self.debounce_seconds
        ));

        let apps = if self.inhibit_apps.is_empty() {
            "-".to_string()
        } else {
            self.inhibit_apps
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(",")
        };
        out.push_str(&format!("  InhibitApps        = {}\n", apps));

        if let Some(idle) = idle_time {
            out.push_str(&format!("  IdleTime           = {}\n", utils::format_duration(idle)));
        }
        if let Some(up) = uptime {
            out.push_str(&format!("  Uptime             = {}\n", utils::format_duration(up)));
        }
        if let Some(inhibited) = is_inhibited {
            out.push_str(&format!("  IdleInhibited      = {}\n", inhibited));
        }

        // Actions
        out.push_str("\nActions:\n");

        let mut grouped: std::collections::BTreeMap<&str, Vec<(&String, &crate::config::model::IdleAction)>> =
            std::collections::BTreeMap::new();

        for (key, action) in &self.actions {
            let prefix = if key.starts_with("ac.") {
                "AC"
            } else if key.starts_with("battery.") {
                "Battery"
            } else {
                "Desktop"
            };
            grouped.entry(prefix).or_default().push((key, action));
        }

        for (group, actions) in grouped {
            out.push_str(&format!("  [{}]\n", group));

            let mut sorted = actions.clone();
            sorted.sort_by(|a, b| a.0.cmp(b.0));

            for (key, action) in sorted {
                out.push_str(&format!(
                    "    {:<20} Timeout={} Kind={} Command=\"{}\"\n",
                    key,
                    action.timeout_seconds,
                    action.kind,
                    action.command
                ));
            }
        }

        out
    }
}
