//! Compiling Vela source to C and to an executable: the steps the `vela`
//! command runs, shared with other tools such as Vela Studio so that every
//! tool compiles programs exactly the same way.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{self, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::ast::Item;
use crate::check::Checker;
use crate::codegen::Generator;
use crate::error::{Error, Result};
use crate::lexer::Lexer;
use crate::parser::Parser;

/// The runtime header that generated C includes as `vela_rt.h`.
pub const RUNTIME_H: &str = include_str!("../runtime/vela_rt.h");

/// Lexes, parses, and type-checks `source`, returning the checked program.
pub fn front_end(source: &str) -> Result<Vec<Item>> {
    match front_end_partial(source) {
        Partial { error: Some(error), .. } => Err(error),
        Partial { items: Some(items), error: None } => Ok(items),
        Partial { items: None, error: None } => Err(Error::internal("the front end produced neither a program nor an error")),
    }
}

/// What `front_end_partial` found.
#[derive(Debug)]
pub struct Partial {
    /// The program, if it lexes and parses. When checking fails, the checker's
    /// annotations (expression types, inferred binding types) are filled in
    /// for the code it checked before the error, and missing after it.
    pub items: Option<Vec<Item>>,
    /// The first error, exactly as `front_end` reports it.
    pub error: Option<Error>,
}

/// Like `front_end`, but keeps the program when checking fails, for tools
/// such as editors that want the types found before the first error.
pub fn front_end_partial(source: &str) -> Partial {
    let parsed = Lexer::new(source).tokenize().and_then(|(tokens, spans)| Parser::new(tokens, spans).parse_file());
    match parsed {
        Err(error) => Partial { items: None, error: Some(error) },
        Ok(mut items) => {
            let error = Checker::new().check_file(&mut items).err();
            Partial { items: Some(items), error }
        }
    }
}

/// Checks `source` and generates its C code. A program without `main` is
/// allowed here; its C just has no `main` function.
pub fn generate_c(source: &str) -> Result<String> {
    let items = front_end(source)?;
    Generator::new().generate(&items)
}

/// Why `build` failed.
#[derive(Debug)]
pub enum BuildError {
    /// The program is not valid Vela (or the compiler has a bug, reported as
    /// an internal compiler error).
    Vela(Error),
    /// The program has no `main` task, so it can't be an executable.
    NoMain,
    /// The output path names the input file, which building would overwrite.
    OutputIsInput(PathBuf),
    /// Creating the build directory or writing a file into it failed.
    Io { action: &'static str, error: io::Error },
    /// The C compiler could not be started.
    CompilerNotStarted { cc: String, error: io::Error },
    /// The C compiler rejected the generated C. `messages` is its output, and
    /// the generated C is kept in `build_dir` for inspection.
    CFailed { messages: String, build_dir: PathBuf },
}

impl BuildError {
    /// Formats the error as the `vela` command prints it. `path` names the
    /// input file and `source` is its text, for Vela diagnostics.
    pub fn render(&self, path: &str, source: &str) -> String {
        match self {
            BuildError::Vela(error) => render_error(path, source, error),
            BuildError::NoMain => format!("{}: error: program has no 'main' task", path),
            BuildError::OutputIsInput(output) => {
                format!("error: output '{}' would overwrite the input file", output.display())
            }
            BuildError::Io { action, error } => format!("error: cannot {}: {}", action, error),
            BuildError::CompilerNotStarted { cc, error } => {
                format!("error: failed to invoke C compiler '{}': {}", cc, error)
            }
            BuildError::CFailed { messages, build_dir } => format!(
                "{}error: C compilation failed (generated C kept in {})",
                messages,
                build_dir.display()
            ),
        }
    }
}

impl From<Error> for BuildError {
    fn from(error: Error) -> Self {
        BuildError::Vela(error)
    }
}

/// The C compiler command from `$CC`, like make and cargo: it may include
/// arguments (`CC="ccache gcc"`). Unset or empty means `cc`.
pub fn c_compiler_from_env() -> String {
    env::var("CC").ok().filter(|cc| !cc.trim().is_empty()).unwrap_or_else(|| "cc".to_string())
}

/// Compiles `source`, the contents of the file at `input`, into an
/// executable at `output`, using `cc` as the C compiler command (see
/// `c_compiler_from_env`). `input` names the generated C file and guards
/// against overwriting the source.
pub fn build(source: &str, input: &Path, output: &Path, cc: &str) -> std::result::Result<(), BuildError> {
    let items = front_end(source)?;
    let has_main = items.iter().any(|item| matches!(item, Item::Task(t) if t.name == "main"));
    if !has_main {
        return Err(BuildError::NoMain);
    }
    let c_code = Generator::new().generate(&items)?;
    if same_file(input, output) {
        return Err(BuildError::OutputIsInput(output.to_path_buf()));
    }
    check_output(output)?;
    compile_c(&c_code, input, output, cc)
}

/// Rejects an output path the C compiler couldn't write, before compiling, so
/// the problem isn't reported as a failure of the generated C.
fn check_output(output: &Path) -> std::result::Result<(), BuildError> {
    let problem = if output.is_dir() {
        Some(format!("'{}' is a folder", output.display()))
    } else {
        match output.parent() {
            Some(folder) if !folder.as_os_str().is_empty() && !folder.is_dir() => {
                Some(format!("the folder '{}' doesn't exist", folder.display()))
            }
            _ => None,
        }
    };
    match problem {
        Some(problem) => Err(BuildError::Io { action: "write the executable", error: io::Error::other(problem) }),
        None => Ok(()),
    }
}

/// Formats a diagnostic as `path:line:col: error: message`, followed by the
/// offending source line with a caret under the error column.
pub fn render_error(path: &str, source: &str, e: &Error) -> String {
    let Some(span) = e.span else {
        return format!("{}: error: {}", path, e.message);
    };
    let mut out = format!("{}:{}:{}: error: {}", path, span.line, span.col, e.message);
    if let Some(line) = span.line.checked_sub(1).and_then(|i| source.lines().nth(i)) {
        let gutter = span.line.to_string();
        // Keep tabs so the caret lines up with the source line.
        let indent: String = line
            .chars()
            .take(span.col.saturating_sub(1))
            .map(|c| if c == '\t' { '\t' } else { ' ' })
            .collect();
        let _ = write!(out, "\n {} | {}\n {} | {}^", gutter, line, " ".repeat(gutter.len()), indent);
    }
    out
}

/// Whether two paths name the same existing file.
fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn compile_c(c_code: &str, input: &Path, output: &Path, cc: &str) -> std::result::Result<(), BuildError> {
    let mut build_dir = BuildDir::create()
        .map_err(|error| BuildError::Io { action: "create build directory", error })?;
    let base = input.file_stem().unwrap_or("vela_out".as_ref());
    let c_path = build_dir.path.join(format!("{}.c", base.to_string_lossy()));
    fs::write(&c_path, c_code).map_err(|error| BuildError::Io { action: "write C file", error })?;
    fs::write(build_dir.path.join("vela_rt.h"), RUNTIME_H)
        .map_err(|error| BuildError::Io { action: "write runtime header", error })?;

    let mut words = cc.split_whitespace();
    let program = words.next().unwrap_or("cc");
    // A relative output starting with `-` would read as an option.
    let output = if output.is_relative() && output.to_string_lossy().starts_with('-') {
        Path::new(".").join(output)
    } else {
        output.to_path_buf()
    };
    let result = Command::new(program)
        .arg("-std=c11")
        .arg("-O2")
        // After the defaults, so `CC="gcc -O0 -g"` can override them.
        .args(words)
        .arg("-I")
        .arg(&build_dir.path)
        .arg(&c_path)
        .arg("-o")
        .arg(&output)
        .arg("-lm") // math library, for vela__rt__fmod
        .output()
        .map_err(|error| BuildError::CompilerNotStarted { cc: program.to_string(), error })?;

    // The C compiler's messages are about generated code, so they are shown
    // only when compilation fails; warnings on success would just be noise.
    if !result.status.success() {
        build_dir.keep = true;
        return Err(BuildError::CFailed {
            messages: format!(
                "{}{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            ),
            build_dir: build_dir.path.clone(),
        });
    }
    Ok(())
}

/// A fresh temporary directory for one build, removed when dropped unless
/// `keep` is set. Each build gets its own, so concurrent builds (in one process
/// or many) never share files.
struct BuildDir {
    path: PathBuf,
    keep: bool,
}

impl BuildDir {
    fn create() -> io::Result<BuildDir> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
        let mut last_error = None;
        for _ in 0..16 {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!("vela-build-{}-{}-{}", process::id(), nanos, n));
            // `create_dir`, unlike `create_dir_all`, fails if the directory
            // already exists, so a stale or planted directory is never reused.
            match fs::create_dir(&path) {
                Ok(()) => return Ok(BuildDir { path, keep: false }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => last_error = Some(error),
                Err(error) => return Err(error),
            }
        }
        Err(last_error.unwrap_or_else(|| io::Error::other("no unused build directory name")))
    }
}

impl Drop for BuildDir {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

    use super::*;

    #[test]
    fn build_dirs_are_unique_and_removed_unless_kept() {
        let first = BuildDir::create().unwrap();
        let mut second = BuildDir::create().unwrap();
        assert_ne!(first.path, second.path);
        assert!(first.path.is_dir() && second.path.is_dir());

        let (first_path, second_path) = (first.path.clone(), second.path.clone());
        second.keep = true;
        drop(first);
        drop(second);
        assert!(!first_path.exists());
        assert!(second_path.is_dir());
        fs::remove_dir_all(second_path).unwrap();
    }
}
