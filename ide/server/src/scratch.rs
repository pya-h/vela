use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::security::random_hex;

#[derive(Debug)]
pub struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    pub fn create() -> io::Result<ScratchDir> {
        let mut last_error = None;
        for _ in 0..16 {
            let path = std::env::temp_dir().join(format!(
                "vela-studio-{}-{}",
                std::process::id(),
                random_hex(8)?
            ));
            // `create_dir` fails on existing paths so a folder is never reused; 0700 keeps builds and launch pages private.
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            match builder.create(&path) {
                Ok(()) => return Ok(ScratchDir { path }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    last_error = Some(error)
                }
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
