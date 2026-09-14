//! Building a workspace file with `vela::driver`, the same code the `vela`
//! command runs, into a temporary folder.

use std::path::PathBuf;

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

    match driver::build(&file.content, &input, &executable, cc) {
        Ok(()) => Ok(Ok(Program { executable, folder, scratch })),
        Err(error) => Ok(Err(failure(&error, path, &file.content))),
    }
}

fn failure(error: &BuildError, path: &str, source: &str) -> Failure {
    let rendered = error.render(path, source);
    let (kind, message, span) = match error {
        BuildError::Vela(e) => (FailureKind::Vela, e.message.clone(), e.span),
        BuildError::NoMain => (FailureKind::NoMain, "program has no 'main' task".to_string(), None),
        BuildError::CompilerNotStarted { cc, error } => {
            (FailureKind::CompilerMissing, format!("failed to invoke C compiler '{}': {}", cc, error), None)
        }
        BuildError::CFailed { .. } => (FailureKind::CFailed, "C compilation failed".to_string(), None),
        BuildError::OutputIsInput(_) | BuildError::Io { .. } => (FailureKind::Io, rendered.clone(), None),
    };
    Failure { kind, message, line: span.map(|s| s.line), col: span.map(|s| s.col), rendered }
}
