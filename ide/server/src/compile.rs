use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use vela::driver::{self, BuildError};

use crate::scratch::ScratchDir;
use crate::workspace::{FsError, Workspace};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailureKind {
    Vela,
    NoMain,
    CompilerMissing,
    CFailed,
    Io,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Failure {
    pub kind: FailureKind,
    pub message: String,
    pub file: Option<String>,
    pub line: Option<usize>,
    pub col: Option<usize>,
    pub rendered: String,
}

pub struct Program {
    pub executable: PathBuf,
    pub folder: PathBuf,
    pub scratch: ScratchDir,
}

/// Outer `Err` is a workspace/I/O error; inner `Err` is a compilation failure.
pub fn build(
    workspace: &Workspace,
    path: &str,
    cc: &str,
) -> Result<Result<Program, Failure>, FsError> {
    let file = workspace.read(path)?;
    let input = workspace.resolve(path)?;
    let scratch = ScratchDir::create()?;
    let stem = input.file_stem().map_or_else(
        || "program".to_string(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    let executable = scratch
        .path()
        .join(format!("{}{}", stem, std::env::consts::EXE_SUFFIX));
    let folder = input
        .parent()
        .map_or_else(|| workspace.root().to_path_buf(), PathBuf::from);

    // Read modules through Workspace to stay confined.
    let mut loader = |module: &Path| -> io::Result<String> {
        let relative = module.to_string_lossy().replace('\\', "/");
        workspace
            .read(&relative)
            .map(|file| file.content)
            .map_err(|error| match error {
                FsError::NotFound => io::Error::from(io::ErrorKind::NotFound),
                FsError::Outside | FsError::InvalidPath(_) => {
                    io::Error::other("it isn't in the workspace")
                }
                other => io::Error::other(format!("{:?}", other)),
            })
    };
    match driver::build_program(
        &file.content,
        Path::new(path),
        &input,
        &executable,
        cc,
        &mut loader,
        true,
    ) {
        // Live analysis already surfaces warnings while typing.
        Ok(_warnings) => Ok(Ok(Program {
            executable,
            folder,
            scratch,
        })),
        Err(error) => Ok(Err(failure(&error, path, &file.content))),
    }
}

fn failure(error: &BuildError, path: &str, source: &str) -> Failure {
    let rendered = error.render(path, source);
    let (kind, message, span) = match error {
        BuildError::Vela(e) => (FailureKind::Vela, e.first().message.clone(), e.first().span),
        BuildError::NoMain => (
            FailureKind::NoMain,
            "program has no 'main' task".to_string(),
            None,
        ),
        BuildError::CompilerNotStarted { cc, error } => (
            FailureKind::CompilerMissing,
            format!("failed to invoke C compiler '{}': {}", cc, error),
            None,
        ),
        BuildError::CFailed {
            messages,
            build_dir,
        } => {
            // Unlike the CLI, Studio builds don't keep failing C temps on disk.
            let _ = std::fs::remove_dir_all(build_dir);
            let rendered = format!("{}error: C compilation failed", messages);
            return Failure {
                kind: FailureKind::CFailed,
                message: "C compilation failed".to_string(),
                file: None,
                line: None,
                col: None,
                rendered,
            };
        }
        BuildError::OutputIsInput(_) | BuildError::OutputIsModule(_) | BuildError::Io { .. } => {
            (FailureKind::Io, rendered.clone(), None)
        }
    };
    let file = match (error, span) {
        (BuildError::Vela(e), Some(span)) if span.file > 0 => e.error_file(),
        (_, Some(_)) => Some(path.to_string()),
        _ => None,
    };
    Failure {
        kind,
        message,
        file,
        line: span.map(|s| s.line),
        col: span.map(|s| s.col),
        rendered,
    }
}
