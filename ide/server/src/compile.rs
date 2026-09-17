//! Building a workspace file with `vela::driver`, the same code the `vela`
//! command runs, into a temporary folder.

use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use vela::driver::{self, BuildError};

use crate::scratch::ScratchDir;
use crate::workspace::{FsError, Workspace};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailureKind {
    /// The program isn't valid Vela; `line` and `col` locate the error.
    Vela,
    /// The program has no `main` task.
    NoMain,
    /// The C compiler couldn't be started.
    CompilerMissing,
    /// The C compiler rejected the generated C (a compiler bug).
    CFailed,
    /// Writing the build files failed.
    Io,
}

/// Why a build failed, for the Problems and Output panels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Failure {
    pub kind: FailureKind,
    /// The error on its own, such as `unknown variable 'x'`.
    pub message: String,
    /// The workspace path of the file the error is in: the file built, or a
    /// module it includes.
    pub file: Option<String>,
    pub line: Option<usize>,
    pub col: Option<usize>,
    /// The error exactly as the `vela` command prints it, with the source
    /// line and a caret.
    pub rendered: String,
}

/// A built program, which lives as long as its folder.
pub struct Program {
    pub executable: PathBuf,
    /// The source file's folder, where the program runs.
    pub folder: PathBuf,
    pub scratch: ScratchDir,
}

/// Builds the file at workspace path `path`. The outer error is about the
/// file itself (such as a bad path); the inner one is a failed build.
pub fn build(workspace: &Workspace, path: &str, cc: &str) -> Result<Result<Program, Failure>, FsError> {
    let file = workspace.read(path)?;
    let input = workspace.resolve(path)?;
    let scratch = ScratchDir::create()?;
    let stem = input.file_stem().map_or_else(|| "program".to_string(), |stem| stem.to_string_lossy().into_owned());
    let executable = scratch.path().join(format!("{}{}", stem, std::env::consts::EXE_SUFFIX));
    let folder = input.parent().map_or_else(|| workspace.root().to_path_buf(), PathBuf::from);

    // Modules are read from the workspace, by their paths in it, so a program
    // can't use a file outside it.
    let mut loader = |module: &Path| -> io::Result<String> {
        let relative = module.to_string_lossy().replace('\\', "/");
        workspace.read(&relative).map(|file| file.content).map_err(|error| match error {
            FsError::NotFound => io::Error::from(io::ErrorKind::NotFound),
            FsError::Outside | FsError::InvalidPath(_) => io::Error::other("it isn't in the workspace"),
            other => io::Error::other(format!("{:?}", other)),
        })
    };
    match driver::build_program(&file.content, Path::new(path), &input, &executable, cc, &mut loader) {
        Ok(()) => Ok(Ok(Program { executable, folder, scratch })),
        Err(error) => Ok(Err(failure(&error, path, &file.content))),
    }
}

fn failure(error: &BuildError, path: &str, source: &str) -> Failure {
    let rendered = error.render(path, source);
    let (kind, message, span) = match error {
        BuildError::Vela(e) => (FailureKind::Vela, e.error.message.clone(), e.error.span),
        BuildError::NoMain => (FailureKind::NoMain, "program has no 'main' task".to_string(), None),
        BuildError::CompilerNotStarted { cc, error } => {
            (FailureKind::CompilerMissing, format!("failed to invoke C compiler '{}': {}", cc, error), None)
        }
        BuildError::CFailed { messages, build_dir } => {
            // The `vela` command keeps the generated C for inspection; here
            // nobody would find it, so it is removed like every build folder.
            let _ = std::fs::remove_dir_all(build_dir);
            let rendered = format!("{}error: C compilation failed", messages);
            return Failure { kind: FailureKind::CFailed, message: "C compilation failed".to_string(), file: None, line: None, col: None, rendered };
        }
        BuildError::OutputIsInput(_) | BuildError::OutputIsModule(_) | BuildError::Io { .. } => (FailureKind::Io, rendered.clone(), None),
    };
    let file = match (error, span) {
        (BuildError::Vela(e), Some(span)) if span.file > 0 => e.error_file(),
        (_, Some(_)) => Some(path.to_string()),
        _ => None,
    };
    Failure { kind, message, file, line: span.map(|s| s.line), col: span.map(|s| s.col), rendered }
}
