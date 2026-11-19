//! Native file pickers block the current frame while waiting for the user.
//! Pause the watchdog only for the duration of that native modal call.

use std::path::{Path, PathBuf};

pub struct Dialog(rfd::FileDialog);

pub fn file_dialog() -> Dialog {
    Dialog(rfd::FileDialog::new())
}

impl Dialog {
    pub fn set_title(mut self, s: impl Into<String>) -> Self {
        self.0 = self.0.set_title(s);
        self
    }

    pub fn set_file_name(mut self, s: impl Into<String>) -> Self {
        self.0 = self.0.set_file_name(s);
        self
    }

    pub fn set_directory(mut self, p: impl AsRef<Path>) -> Self {
        self.0 = self.0.set_directory(p);
        self
    }

    pub fn add_filter(mut self, name: impl Into<String>, extensions: &[impl ToString]) -> Self {
        self.0 = self.0.add_filter(name, extensions);
        self
    }

    pub fn pick_file(self) -> Option<PathBuf> {
        let _pause = super::pause_ui();
        self.0.pick_file()
    }

    pub fn pick_files(self) -> Option<Vec<PathBuf>> {
        let _pause = super::pause_ui();
        self.0.pick_files()
    }

    pub fn pick_folder(self) -> Option<PathBuf> {
        let _pause = super::pause_ui();
        self.0.pick_folder()
    }

    pub fn save_file(self) -> Option<PathBuf> {
        let _pause = super::pause_ui();
        self.0.save_file()
    }
}
