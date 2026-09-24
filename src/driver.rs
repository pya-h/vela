//! Compiling Vela source to C and to an executable: the steps the `vela`
//! command runs, shared with other tools such as Vela Studio so that every
//! tool compiles programs exactly the same way.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{self, Command};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::ast::Item;
use crate::check::Checker;
use crate::codegen::Generator;
use crate::error::{Error, Result};
use crate::modules::{self, Loader, SourceFile, Spelling};

/// The runtime header that generated C includes as `vela_rt.h`.
pub const RUNTIME_H: &str = include_str!("../runtime/vela_rt.h");

/// How many mistakes the `vela` command shows at once. A program with more of
/// them has something wrong that the later ones may well follow from, so the
/// first few are the ones to fix; the rest are counted instead of shown. Tools
/// that mark mistakes in place, such as Vela Studio, show them all.
pub const MAX_SHOWN: usize = 20;

/// Lexes, parses, and type-checks `source`, a program in one file, returning
/// the checked program. Only the first of its mistakes is reported; see
/// `front_end_partial` for every one. An `include` in it can't be loaded; see
/// `check_program`.
pub fn front_end(source: &str) -> Result<Vec<Item>> {
    let partial = front_end_partial(source);
    match (partial.items, partial.errors.into_iter().next()) {
        (_, Some(error)) => Err(error),
        (Some(items), None) => Ok(items),
        (None, None) => Err(Error::internal("the front end produced neither a program nor an error")),
    }
}

/// What `front_end_partial` found.
#[derive(Debug)]
pub struct Partial {
    /// The program, if it lexes and parses. When checking fails, the checker's
    /// annotations (expression types, inferred binding types) are filled in for
    /// the code it checked: it goes on after a mistake, so only the statements
    /// and declarations that have one are left unannotated.
    pub items: Option<Vec<Item>>,
    /// Every mistake the program has, in source order (see
    /// `check::Checker::check_program`). Lexing and parsing stop at the first
    /// one, so a program that doesn't parse has exactly one.
    pub errors: Vec<Error>,
    /// The program's files, the main file first; `Span::file` indexes them.
    pub files: Vec<SourceFile>,
    /// How the main file writes the program's names, whose names in `items`
    /// start with their module's path (see `modules::Spelling`).
    pub spelling: Spelling,
}

impl Partial {
    /// The first mistake, the one a tool that shows only one shows.
    pub fn first_error(&self) -> Option<&Error> {
        self.errors.first()
    }
}

/// Like `front_end`, but keeps the program when checking fails, and reports
/// every mistake, for tools such as editors that show them all and want the
/// types found around them.
pub fn front_end_partial(source: &str) -> Partial {
    front_end_program(Path::new(""), source, &mut no_modules)
}

/// `front_end_partial` for a program whose main file is `source`, at the
/// path `path`, and whose modules `loader` reads (see `modules::load`).
pub fn front_end_program(path: &Path, source: &str, loader: &mut Loader) -> Partial {
    let (files, loaded) = modules::load(path, source, loader);
    match loaded {
        Err(error) => Partial { items: None, errors: vec![error], files, spelling: Spelling::default() },
        Ok(mut program) => {
            let errors = Checker::check_program(&mut program);
            let spelling = program.modules.first().map(|main| main.spelling.clone()).unwrap_or_default();
            Partial { items: Some(program.items), errors, files, spelling }
        }
    }
}

/// A loader for a program in one file, which has no other files to read.
fn no_modules(path: &Path) -> io::Result<String> {
    Err(io::Error::new(io::ErrorKind::NotFound, format!("'{}' isn't loaded here", path.display())))
}

/// Reads a module from the disk: the loader of the `vela` command.
pub fn read_from_disk(path: &Path) -> io::Result<String> {
    fs::read_to_string(path)
}

/// Why a program of several files was rejected: every mistake it has, in source
/// order, with the files, to show them. Its `errors` are never empty.
#[derive(Debug)]
pub struct ProgramError {
    pub errors: Vec<Error>,
    /// The program's files, the main file first.
    pub files: Vec<SourceFile>,
}

impl ProgramError {
    /// One mistake, with the program's files.
    pub fn one(error: Error, files: Vec<SourceFile>) -> ProgramError {
        ProgramError { errors: vec![error], files }
    }

    /// The first mistake, the one a tool that shows only one shows.
    pub fn first(&self) -> &Error {
        self.errors.first().unwrap_or(&NO_ERROR)
    }

    /// Every mistake as the `vela` command prints it (see `render_error`), one
    /// after another, each with the path and source line of the file it is in;
    /// `main_path` names the main file, and every other file is named by its
    /// path as loaded.
    pub fn render(&self, main_path: &str) -> String {
        render_all(main_path, self.errors.iter().map(|error| render_program_error(main_path, &self.files, error)).collect())
    }

    /// The path of the file the first error is in, other than the main file, as
    /// loaded (`/`-separated).
    pub fn error_file(&self) -> Option<String> {
        let file = self.first().span.map(|span| span.file).filter(|&file| file > 0)?;
        self.files.get(file).map(|found| found.path.to_string_lossy().replace('\\', "/"))
    }
}

/// The error a `ProgramError` with no errors would report, which is a compiler
/// bug: `ProgramError::first` never has to use it.
static NO_ERROR: LazyLock<Error> = LazyLock::new(|| Error::internal("a program was rejected without saying why"));

/// The diagnostics as the `vela` command prints them: at most `MAX_SHOWN` of
/// them, one after another, and a last line counting any it leaves out.
fn render_all(main_path: &str, rendered: Vec<String>) -> String {
    let more = rendered.len().saturating_sub(MAX_SHOWN);
    let mut shown: Vec<String> = rendered.into_iter().take(MAX_SHOWN).collect();
    if more > 0 {
        let mistakes = if more == 1 { "mistake" } else { "mistakes" };
        shown.push(format!("{}: error: {} more {}, not shown; fix these first and check again", main_path, more, mistakes));
    }
    shown.join("\n")
}

/// Formats `e`, an error in one of `files`, like `render_error`.
pub fn render_program_error(main_path: &str, files: &[SourceFile], e: &Error) -> String {
    let file = e.span.map_or(0, |span| span.file);
    match files.get(file) {
        Some(found) if file > 0 => render_error(&found.path.display().to_string(), &found.source, e),
        Some(main) => render_error(main_path, &main.source, e),
        None => render_error(main_path, "", e),
    }
}

/// Checks the program whose main file is `source`, at `path`, loading its
/// modules with `loader`.
pub fn check_program(path: &Path, source: &str, loader: &mut Loader) -> std::result::Result<Vec<Item>, ProgramError> {
    checked(front_end_program(path, source, loader))
}

/// The checked program, or every mistake that stopped it.
fn checked(partial: Partial) -> std::result::Result<Vec<Item>, ProgramError> {
    match (partial.items, partial.errors) {
        (Some(items), errors) if errors.is_empty() => Ok(items),
        (_, errors) if errors.is_empty() => Err(ProgramError::one(
            Error::internal("the front end produced neither a program nor an error"),
            partial.files,
        )),
        (_, errors) => Err(ProgramError { errors, files: partial.files }),
    }
}

/// Checks `source` and generates its C code. A program without `main` is
/// allowed here; its C just has no `main` function.
pub fn generate_c(source: &str) -> Result<String> {
    let items = front_end(source)?;
    Generator::new().generate(&items)
}

/// `generate_c` for a program whose main file is `source`, at `path`, loading
/// its modules with `loader`.
pub fn generate_c_program(path: &Path, source: &str, loader: &mut Loader) -> std::result::Result<String, ProgramError> {
    let items = check_program(path, source, loader)?;
    Generator::new().generate(&items).map_err(|error| ProgramError::one(error, Vec::new()))
}

/// `generate_c_program` for C to be written to the file `output`, which must
/// not be a file of the program: the main file, at `input` on the disk, or a
/// module it includes.
pub fn generate_c_file(path: &Path, input: &Path, source: &str, output: &Path, loader: &mut Loader) -> std::result::Result<String, BuildError> {
    let partial = front_end_program(path, source, loader);
    let modules = module_paths(&partial);
    let items = checked(partial)?;
    let c_code = Generator::new().generate(&items)?;
    overwrites_source(input, &modules, output)?;
    Ok(c_code)
}

/// The paths of the modules a program read, as its loader knows them.
fn module_paths(partial: &Partial) -> Vec<PathBuf> {
    partial.files.iter().skip(1).map(|file| file.path.clone()).collect()
}

/// Rejects an `output` that names the main file, at `input`, or one of `modules`.
fn overwrites_source(input: &Path, modules: &[PathBuf], output: &Path) -> std::result::Result<(), BuildError> {
    if same_file(input, output) {
        return Err(BuildError::OutputIsInput(output.to_path_buf()));
    }
    if modules.iter().any(|module| same_file(module, output)) {
        return Err(BuildError::OutputIsModule(output.to_path_buf()));
    }
    Ok(())
}

/// Why `build` failed.
#[derive(Debug)]
pub enum BuildError {
    /// The program is not valid Vela (or the compiler has a bug, reported as
    /// an internal compiler error), with its files, where the error is.
    Vela(ProgramError),
    /// The program has no `main` task, so it can't be an executable.
    NoMain,
    /// The output path names the input file, which building would overwrite.
    OutputIsInput(PathBuf),
    /// The output path names a module the program includes.
    OutputIsModule(PathBuf),
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
            BuildError::Vela(ProgramError { errors, files }) if files.is_empty() => {
                render_all(path, errors.iter().map(|error| render_error(path, source, error)).collect())
            }
            BuildError::Vela(error) => error.render(path),
            BuildError::NoMain => format!("{}: error: program has no 'main' task", path),
            BuildError::OutputIsInput(output) => {
                format!("error: output '{}' would overwrite the input file", output.display())
            }
            BuildError::OutputIsModule(output) => {
                format!("error: output '{}' would overwrite a module the program includes", output.display())
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
        BuildError::Vela(ProgramError::one(error, Vec::new()))
    }
}

impl From<ProgramError> for BuildError {
    fn from(error: ProgramError) -> Self {
        BuildError::Vela(error)
    }
}

/// The C compiler command from `$CC`, like make and cargo: it may include
/// arguments (`CC="ccache gcc"`). Unset or empty means `cc`.
pub fn c_compiler_from_env() -> String {
    env::var("CC").ok().filter(|cc| !cc.trim().is_empty()).unwrap_or_else(|| "cc".to_string())
}

/// Whether a checked program has a `main` task, which an executable needs.
pub fn has_main(items: &[Item]) -> bool {
    items.iter().any(|item| matches!(item, Item::Task(t) if t.name == "main"))
}

/// Compiles `source`, the contents of the file at `input`, into an
/// executable at `output`, using `cc` as the C compiler command (see
/// `c_compiler_from_env`). `input` names the generated C file and guards
/// against overwriting the source. The modules the program includes are read
/// from the disk, next to `input`.
pub fn build(source: &str, input: &Path, output: &Path, cc: &str) -> std::result::Result<(), BuildError> {
    build_program(source, input, input, output, cc, &mut read_from_disk)
}

/// `build`, with the program's main file at `path` as `loader` knows it (for
/// its modules' paths and for messages) and at `input` on the disk.
pub fn build_program(source: &str, path: &Path, input: &Path, output: &Path, cc: &str, loader: &mut Loader) -> std::result::Result<(), BuildError> {
    let partial = front_end_program(path, source, loader);
    let modules = module_paths(&partial);
    let items = checked(partial)?;
    if !has_main(&items) {
        return Err(BuildError::NoMain);
    }
    let c_code = Generator::new().generate(&items)?;
    overwrites_source(input, &modules, output)?;
    check_output(output)?;
    compile_c(&c_code, input, output, cc)
}

/// Rejects an output path the C compiler couldn't write, before compiling, so
/// the problem isn't reported as a failure of the generated C.
fn check_output(output: &Path) -> std::result::Result<(), BuildError> {
    let folder = output.parent().filter(|folder| !folder.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let io_error = |error: io::Error| BuildError::Io { action: "write the executable", error };
    if output.is_dir() {
        return Err(io_error(io::Error::other(format!("'{}' is a folder", output.display()))));
    }
    if !folder.is_dir() {
        return Err(io_error(io::Error::other(format!("the folder '{}' doesn't exist", folder.display()))));
    }
    // The C compiler (its linker) replaces the output, which needs a folder it
    // can create files in, or else an existing output it can write.
    let denied = |error: io::Error| io_error(io::Error::new(error.kind(), format!("'{}': {}", output.display(), error)));
    let probe = folder.join(format!(".vela-output-check-{}", process::id()));
    match fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
        }
        Err(error) if output.exists() => {
            fs::OpenOptions::new().write(true).open(output).map_err(|_| denied(error))?;
        }
        Err(error) => return Err(denied(error)),
    }
    // An existing output that can't be written, in a folder that can.
    if output.exists() {
        fs::OpenOptions::new().write(true).open(output).map_err(denied)?;
    }
    Ok(())
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
