//! Temporary folders for built programs, outside the workspace.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::security::random_hex;

/// A fresh folder in the system's temporary directory, removed when dropped.
#[derive(Debug)]
pub struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    pub fn create() -> io::Result<ScratchDir> {
        let mut last_error = None;
        for _ in 0..16 {
            let path = std::env::temp_dir().join(format!("vela-studio-{}-{}", std::process::id(), random_hex(8)?));
            // `create_dir` fails on an existing folder, so none is ever reused.
            match fs::create_dir(&path) {
                Ok(()) => return Ok(ScratchDir { path }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => last_error = Some(error),
                Err(error) => return Err(error),
            }
        }
        Err(last_error.unwrap_or_else(|| io::Error::other("no unused temporary folder name")))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
