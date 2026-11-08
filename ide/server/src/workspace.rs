//! Confines all file access to the canonical workspace root; local external races are accepted.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::security::random_hex;

pub const MAX_FILE_BYTES: usize = 1024 * 1024;

const SKIPPED_FOLDERS: &[&str] = &["target", "node_modules"];
const MAX_DEPTH: usize = 16;
const MAX_ENTRIES: usize = 10_000;

#[derive(Debug)]
pub enum FsError {
    InvalidPath(String),
    Outside,
    NotFound,
    AlreadyExists,
    NotAFile,
    TooLarge,
    NotText,
    Changed {
        hash: String,
    },
    ReadOnly,
    Denied,
    Io(io::Error),
}

impl From<io::Error> for FsError {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::NotFound => FsError::NotFound,
            io::ErrorKind::AlreadyExists => FsError::AlreadyExists,
            io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem => FsError::Denied,
            _ => FsError::Io(error),
        }
    }
}

type Result<T> = std::result::Result<T, FsError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EntryKind {
    File,
    Folder,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub kind: EntryKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<Entry>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Tree {
    pub entries: Vec<Entry>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TextFile {
    pub content: String,
    pub hash: String,
}

pub struct Workspace {
    root: PathBuf,
    /// Serializes read-check-write steps inside the server.
    writes: Mutex<()>,
}

impl Workspace {
    pub fn open(folder: &Path) -> io::Result<Workspace> {
        let root = fs::canonicalize(folder)?;
        if !root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!("'{}' is not a folder", folder.display()),
            ));
        }
        Ok(Workspace {
            root,
            writes: Mutex::new(()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn name(&self) -> String {
        self.root.file_name().map_or_else(
            || self.root.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
    }

    pub fn resolve(&self, path: &str) -> Result<PathBuf> {
        let joined = self
            .root
            .join(segments(path)?.join(std::path::MAIN_SEPARATOR_STR));
        let resolved = fs::canonicalize(joined)?;
        if !resolved.starts_with(&self.root) {
            return Err(FsError::Outside);
        }
        Ok(resolved)
    }

    /// Checks parent containment without resolving the target symlink itself (for rename/delete).
    fn locate(&self, path: &str) -> Result<(PathBuf, bool)> {
        let segments = segments(path)?;
        let Some((name, parents)) = segments.split_last() else {
            return Err(FsError::InvalidPath("the path is empty".to_string()));
        };
        let parent = fs::canonicalize(self.root.join(parents.join(std::path::MAIN_SEPARATOR_STR)))?;
        if !parent.starts_with(&self.root) {
            return Err(FsError::Outside);
        }
        if !parent.is_dir() {
            return Err(FsError::NotFound);
        }
        let entry = parent.join(name);
        let exists = entry.symlink_metadata().is_ok();
        Ok((entry, exists))
    }

    pub fn tree(&self) -> Tree {
        let mut budget = MAX_ENTRIES;
        let mut truncated = false;
        let entries = self.list(&self.root, "", 0, &mut budget, &mut truncated);
        Tree { entries, truncated }
    }

    fn list(
        &self,
        folder: &Path,
        prefix: &str,
        depth: usize,
        budget: &mut usize,
        truncated: &mut bool,
    ) -> Vec<Entry> {
        let Ok(read) = fs::read_dir(folder) else {
            return Vec::new();
        };
        let mut found: Vec<(String, PathBuf, EntryKind)> = Vec::new();
        for item in read.flatten() {
            let Ok(name) = item.file_name().into_string() else {
                continue;
            };
            if name.starts_with('.') || !valid_segment(&name) {
                continue;
            }
            let path = item.path();
            let Ok(metadata) = path.symlink_metadata() else {
                continue;
            };
            let kind = if metadata.file_type().is_symlink() {
                // Only symlinked files inside root; skip symlinked folders to avoid cycles.
                match fs::canonicalize(&path) {
                    Ok(target) if target.starts_with(&self.root) && target.is_file() => {
                        EntryKind::File
                    }
                    _ => continue,
                }
            } else if metadata.is_dir() {
                if SKIPPED_FOLDERS.contains(&name.as_str()) {
                    continue;
                }
                EntryKind::Folder
            } else if metadata.is_file() {
                EntryKind::File
            } else {
                // Skip FIFOs, sockets, and devices that could block on open.
                continue;
            };
            found.push((name, path, kind));
        }
        found.sort_by(|a, b| {
            (a.2 == EntryKind::File, a.0.to_lowercase(), &a.0).cmp(&(
                b.2 == EntryKind::File,
                b.0.to_lowercase(),
                &b.0,
            ))
        });

        let mut entries = Vec::new();
        for (name, path, kind) in found {
            if *budget == 0 {
                *truncated = true;
                break;
            }
            *budget -= 1;
            let relative = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{}/{}", prefix, name)
            };
            let children = (kind == EntryKind::Folder).then(|| {
                if depth + 1 >= MAX_DEPTH {
                    *truncated = true;
                    Vec::new()
                } else {
                    self.list(&path, &relative, depth + 1, budget, truncated)
                }
            });
            entries.push(Entry {
                name,
                path: relative,
                kind,
                children,
            });
        }
        entries
    }

    pub fn read(&self, path: &str) -> Result<TextFile> {
        let resolved = self.resolve(path)?;
        let bytes = read_limited(&resolved)?;
        let hash = content_hash(&bytes);
        let content = String::from_utf8(bytes).map_err(|_| FsError::NotText)?;
        Ok(TextFile { content, hash })
    }

    /// Atomically replaces an existing file via temp + rename, failing with `Changed` if `base_hash` no longer matches.
    pub fn write(&self, path: &str, content: &str, base_hash: Option<&str>) -> Result<String> {
        if content.len() > MAX_FILE_BYTES {
            return Err(FsError::TooLarge);
        }
        let resolved = self.resolve(path)?;
        let _guard = self
            .writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let metadata = fs::metadata(&resolved)?;
        if !metadata.is_file() {
            return Err(FsError::NotAFile);
        }
        // Check file write permissions upfront because atomic rename only needs directory write permission.
        let writable = OpenOptions::new().write(true).open(&resolved);
        if metadata.permissions().readonly()
            || writable
                .as_ref()
                .is_err_and(|error| error.kind() == io::ErrorKind::PermissionDenied)
        {
            return Err(FsError::ReadOnly);
        }
        drop(writable);
        if let Some(base) = base_hash {
            // Anything larger than MAX_FILE_BYTES cannot match what the editor loaded.
            if metadata.len() > MAX_FILE_BYTES as u64 {
                return Err(FsError::Changed {
                    hash: String::new(),
                });
            }
            let current = content_hash(&fs::read(&resolved)?);
            if current != base {
                return Err(FsError::Changed { hash: current });
            }
        }
        let folder = resolved.parent().ok_or(FsError::NotAFile)?;
        // Short temp name so files near NAME_MAX still save.
        let temporary = folder.join(format!(".vela-studio-{}.tmp", random_hex(6)?));
        let created = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary);
        let mut file = match created {
            Ok(file) => file,
            // Read-only directory: overwrite in place when the file itself is writable.
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                let mut file = OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(&resolved)?;
                file.write_all(content.as_bytes())?;
                file.sync_all()?;
                return Ok(content_hash(content.as_bytes()));
            }
            Err(error) => return Err(error.into()),
        };
        let written = (|| {
            file.write_all(content.as_bytes())?;
            file.sync_all()?;
            fs::set_permissions(&temporary, metadata.permissions())?;
            fs::rename(&temporary, &resolved)
        })();
        if let Err(error) = written {
            let _ = fs::remove_file(&temporary);
            return Err(error.into());
        }
        Ok(content_hash(content.as_bytes()))
    }

    pub fn create(&self, path: &str, kind: EntryKind) -> Result<String> {
        let _guard = self
            .writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (entry, exists) = self.locate(path)?;
        if exists {
            return Err(FsError::AlreadyExists);
        }
        match kind {
            EntryKind::File => drop(
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&entry)?,
            ),
            EntryKind::Folder => fs::create_dir(&entry)?,
        }
        Ok(path.to_string())
    }

    pub fn rename(&self, from: &str, to: &str) -> Result<String> {
        let _guard = self
            .writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (source, exists) = self.locate(from)?;
        if !exists {
            return Err(FsError::NotFound);
        }
        let (target, exists) = self.locate(to)?;
        if exists {
            return Err(FsError::AlreadyExists);
        }
        if target.starts_with(&source) {
            return Err(FsError::InvalidPath(
                "a folder can't be moved into itself".to_string(),
            ));
        }
        fs::rename(&source, &target)?;
        Ok(to.to_string())
    }

    pub fn delete(&self, path: &str) -> Result<()> {
        let _guard = self
            .writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (entry, exists) = self.locate(path)?;
        if !exists {
            return Err(FsError::NotFound);
        }
        // `remove_dir_all` does not follow symlinks inside the folder.
        if entry.symlink_metadata()?.is_dir() {
            fs::remove_dir_all(&entry)?
        } else {
            fs::remove_file(&entry)?
        }
        Ok(())
    }
}

fn segments(path: &str) -> Result<Vec<&str>> {
    if path.is_empty() {
        return Err(FsError::InvalidPath("the path is empty".to_string()));
    }
    let segments: Vec<&str> = path.split('/').collect();
    match segments.iter().find(|segment| !valid_segment(segment)) {
        Some(bad) => Err(FsError::InvalidPath(format!(
            "'{}' is not a valid name in a workspace path",
            bad
        ))),
        None => Ok(segments),
    }
}

fn valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && !segment
            .chars()
            .any(|c| c == '\\' || c == ':' || c.is_control())
}

fn read_limited(path: &Path) -> Result<Vec<u8>> {
    // Check before opening because opening a FIFO blocks until written to.
    if !fs::metadata(path)?.is_file() {
        return Err(FsError::NotAFile);
    }
    let file = fs::File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(FsError::NotAFile);
    }
    let mut bytes = Vec::new();
    // Read one extra byte so concurrent growth past the limit is still caught.
    file.take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(FsError::TooLarge);
    }
    Ok(bytes)
}

/// 64-bit FNV-1a for optimistic concurrency (not cryptographic).
pub fn content_hash(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, &b| {
        (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{:016x}", hash)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )]

    use super::*;

    #[test]
    fn request_paths_are_plain_relative_names() {
        assert_eq!(segments("a/b.vela").unwrap(), ["a", "b.vela"]);
        for bad in [
            "",
            "/etc/passwd",
            "a//b",
            "a/",
            "./a",
            "a/../b",
            "..",
            "a\\b",
            "C:x",
            "a\0b",
        ] {
            assert!(
                matches!(segments(bad), Err(FsError::InvalidPath(_))),
                "{:?}",
                bad
            );
        }
    }

    #[test]
    fn content_hash_is_fnv1a() {
        assert_eq!(content_hash(b""), "cbf29ce484222325");
        assert_eq!(content_hash(b"a"), "af63dc4c8601ec8c");
    }
}
