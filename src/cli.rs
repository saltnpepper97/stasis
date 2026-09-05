// Author: Dustin Pilgrim
// License: GPL-3.0-only

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "stasis",
    version = env!("CARGO_PKG_VERSION"),
    about = "Stasis idle manager"
)]
pub struct Args {
    #[arg(short, long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    #[arg(short, long, action)]
    pub verbose: bool,

    #[arg(long, action)]
    pub no_console: bool,

    #[arg(long, action)]
    pub timestamps: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Command {
    #[command(about = "Reload the configuration without restarting Stasis")]
    Reload,

    #[command(
        about = "Pause timers indefinitely, for a duration, or until a time",
        disable_help_flag = true
    )]
    Pause {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    #[command(about = "Resume timers after a pause")]
    Resume,

    #[command(about = "List actions or profiles", disable_help_flag = true)]
    List {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    #[command(about = "Manually trigger an idle action by name")]
    Trigger { step: String },

    #[command(about = "Toggle manual idle inhibition")]
    ToggleInhibit,

    #[command(about = "Run the optional system tray frontend")]
    Tray,

    #[command(about = "Stop all running Stasis instances")]
    Stop,

    #[command(about = "Display current session information")]
    Info {
        #[arg(long)]
        json: bool,
    },

    #[command(about = "Explain what currently prevents idle progression")]
    Blame {
        #[arg(long)]
        json: bool,
    },

    #[command(about = "Stream state and profile changes as newline-delimited JSON")]
    Watch,

    #[command(about = "Dump recent log lines", disable_help_flag = true)]
    Dump {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    #[command(
        about = "Switch profile or return to base config",
        disable_help_flag = true
    )]
    Profile {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    #[command(about = "Show power-saving report (today or week)")]
    Report {
        #[arg(default_value = "today", help = "Time window: 'today' or 'week'")]
        range: String,
    },
}
