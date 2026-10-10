// Author: Dustin Pilgrim
// License: GPL-3.0-only

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Configuration selection/semantics failed.
    ///
    /// Examples:
    /// - profile not found (effective config cannot be selected)
    /// - profile name is empty / invalid
    InvalidConfig(ConfigError),

    /// An event was rejected because it is invalid in the current state.
    ///
    /// Examples:
    /// - resume while not paused
    /// - pause while already paused
    /// - unlock without lock
    InvalidState(StateError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// The requested profile does not exist / cannot be selected.
    ProfileNotFound,

    /// Profile name was empty/whitespace when one was required.
    InvalidProfileName,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateError {
    AlreadyPaused,
}

// ---------------- Display ----------------

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidConfig(e) => write!(f, "{e}"),
            Error::InvalidState(e) => write!(f, "{e}"),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::ProfileNotFound => write!(f, "profile not found"),
            ConfigError::InvalidProfileName => write!(f, "invalid profile name"),
        }
    }
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StateError::AlreadyPaused => write!(f, "already paused"),
        }
    }
}

impl std::error::Error for Error {}
