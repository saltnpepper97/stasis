// Author: Dustin Pilgrim
// License: GPL-3.0-only

mod engine;
mod info;
mod list;
mod snapshot;

use crate::core::config::ConfigFile;

#[derive(Debug, Clone)]
pub struct Manager {
    cfg_file: ConfigFile,
}

impl Manager {
    pub fn new(cfg_file: ConfigFile) -> Self {
        Self { cfg_file }
    }

    pub fn set_config(&mut self, cfg_file: ConfigFile) {
        self.cfg_file = cfg_file;
    }

    pub fn cfg_file_ref(&self) -> &crate::core::config::ConfigFile {
        &self.cfg_file
    }
}
