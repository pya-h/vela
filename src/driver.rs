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

pub const RUNTIME_H: &str = include_str!("../runtime/vela_rt.h");

/// CLI diagnostic cap so later cascaded errors do not flood output; editors show all.
pub const MAX_SHOWN: usize = 20;

pub fn front_end(source: &str) -> Result<Vec<Item>> {
    let partial = front_end_partial(source);
    match (partial.items, partial.errors.into_iter().next()) {
        (_, Some(error)) => Err(error),
        (Some(items), None) => Ok(items),
        (None, None) => Err(Error::internal(
            "the front end produced neither a program nor an error",
        )),
    }
}

#[derive(Debug)]
pub struct Partial {
    /// Populated even when type-checking fails so editors still have partial type annotations.
    pub items: Option<Vec<Item>>,
    pub errors: Vec<Error>,
    pub warnings: Vec<Error>,
    pub held_back: Vec<Error>,
    pub files: Vec<SourceFile>,
    pub spelling: Spelling,
}

impl Partial {
    pub fn first_error(&self) -> Option<&Error> {
        self.errors.first()
    }
}

pub fn front_end_partial(source: &str) -> Partial {
    front_end_program(Path::new(""), source, &mut no_modules)
}

pub fn front_end_program(path: &Path, source: &str, loader: &mut Loader) -> Partial {
    let (files, loaded) = modules::load(path, source, loader);
    match loaded {
        Err(error) => Partial {
            items: None,
            errors: vec![error],
            warnings: Vec::new(),
            held_back: Vec::new(),
            files,
            spelling: Spelling::default(),
        },
        Ok(mut program) => {
            let (errors, warnings, held_back) = Checker::check_program_held(&mut program);
            let spelling = program
                .modules
                .first()
                .map(|main| main.spelling.clone())
                .unwrap_or_default();
            Partial {
                items: Some(program.items),
                errors,
                warnings,
                held_back,
                files,
                spelling,
            }
        }
    }
}

fn no_modules(path: &Path) -> io::Result<String> {
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("'{}' isn't loaded here", path.display()),
    ))
}

pub fn read_from_disk(path: &Path) -> io::Result<String> {
    fs::read_to_string(path)
}

#[derive(Debug)]
pub struct ProgramError {
    pub errors: Vec<Error>,
    pub files: Vec<SourceFile>,
}

impl ProgramError {
    pub fn one(error: Error, files: Vec<SourceFile>) -> ProgramError {
        ProgramError {
            errors: vec![error],
            files,
        }
    }

    pub fn first(&self) -> &Error {
        self.errors.first().unwrap_or(&NO_ERROR)
    }

    pub fn render(&self, main_path: &str) -> String {
        render_all(
            main_path,
            self.errors
                .iter()
                .map(|error| render_program_error(main_path, &self.files, error))
                .collect(),
        )
    }

    pub fn error_file(&self) -> Option<String> {
        let file = self
            .first()
            .span
            .map(|span| span.file)
            .filter(|&file| file > 0)?;
        self.files
            .get(file)
            .map(|found| found.path.to_string_lossy().replace('\\', "/"))
    }
}

static NO_ERROR: LazyLock<Error> =
    LazyLock::new(|| Error::internal("a program was rejected without saying why"));

fn render_all(main_path: &str, rendered: Vec<String>) -> String {
    let more = rendered.len().saturating_sub(MAX_SHOWN);
    let mut shown: Vec<String> = rendered.into_iter().take(MAX_SHOWN).collect();
    if more > 0 {
        let mistakes = if more == 1 { "mistake" } else { "mistakes" };
        shown.push(format!(
            "{}: error: {} more {}, not shown; fix these first and check again",
            main_path, more, mistakes
        ));
    }
    shown.join("\n")
}

pub fn render_program_error(main_path: &str, files: &[SourceFile], e: &Error) -> String {
    let file = e.span.map_or(0, |span| span.file);
    match files.get(file) {
        Some(found) if file > 0 => {
            render_error(&found.path.display().to_string(), &found.source, e)
        }
        Some(main) => render_error(main_path, &main.source, e),
        None => render_error(main_path, "", e),
    }
}

#[derive(Debug, Default)]
pub struct Warnings {
    pub warnings: Vec<Error>,
    pub held_back: Vec<Error>,
    pub files: Vec<SourceFile>,
}

impl Warnings {
    pub fn render(&self, main_path: &str) -> String {
        self.render_of(&self.warnings, main_path)
    }

    pub fn render_repeating(&self, main_path: &str) -> String {
        let mut all: Vec<Error> = self
            .warnings
            .iter()
            .chain(&self.held_back)
            .cloned()
            .collect();
        all.sort_by_key(|warning| {
            warning
                .span
                .map_or((1, 0, 0, 0), |span| (0, span.file, span.line, span.col))
        });
        self.render_of(&all, main_path)
    }

    fn render_of(&self, warnings: &[Error], main_path: &str) -> String {
        let rendered: Vec<String> = warnings
            .iter()
            .map(|warning| {
                let file = warning.span.map_or(0, |span| span.file);
                match self.files.get(file) {
                    Some(found) if file > 0 => render_diagnostic(
                        &found.path.display().to_string(),
                        &found.source,
                        warning,
                        "warning",
                    ),
                    Some(main) => render_diagnostic(main_path, &main.source, warning, "warning"),
                    None => render_diagnostic(main_path, "", warning, "warning"),
                }
            })
            .collect();
        rendered.join("\n")
    }
}

pub fn check_program(
    path: &Path,
    source: &str,
    loader: &mut Loader,
) -> std::result::Result<(Vec<Item>, Warnings), ProgramError> {
    checked(front_end_program(path, source, loader))
}

fn checked(partial: Partial) -> std::result::Result<(Vec<Item>, Warnings), ProgramError> {
    match (partial.items, partial.errors) {
        (Some(items), errors) if errors.is_empty() => Ok((
            items,
            Warnings {
                warnings: partial.warnings,
                held_back: partial.held_back,
                files: partial.files,
            },
        )),
        (_, errors) if errors.is_empty() => Err(ProgramError::one(
            Error::internal("the front end produced neither a program nor an error"),
            partial.files,
        )),
        (_, errors) => Err(ProgramError {
            errors,
            files: partial.files,
        }),
    }
}

pub fn generate_c(source: &str) -> Result<String> {
    let items = front_end(source)?;
    Generator::new().generate(&items)
}

pub fn generate_c_program(
    path: &Path,
    source: &str,
    loader: &mut Loader,
    notices: bool,
) -> std::result::Result<(String, Warnings), ProgramError> {
    let (items, warnings) = check_program(path, source, loader)?;
    let c_code = Generator::new()
        .with_notices(notices)
        .generate(&items)
        .map_err(|error| ProgramError::one(error, Vec::new()))?;
    Ok((c_code, warnings))
}

pub fn generate_c_file(
    path: &Path,
    input: &Path,
    source: &str,
    output: &Path,
    loader: &mut Loader,
    notices: bool,
) -> std::result::Result<(String, Warnings), BuildError> {
    let partial = front_end_program(path, source, loader);
    let modules = module_paths(&partial);
    let (items, warnings) = checked(partial)?;
    let c_code = Generator::new().with_notices(notices).generate(&items)?;
    overwrites_source(input, &modules, output)?;
    Ok((c_code, warnings))
}

fn module_paths(partial: &Partial) -> Vec<PathBuf> {
    partial
        .files
        .iter()
        .skip(1)
        .map(|file| file.path.clone())
        .collect()
}

fn overwrites_source(
    input: &Path,
    modules: &[PathBuf],
    output: &Path,
) -> std::result::Result<(), BuildError> {
    if same_file(input, output) {
        return Err(BuildError::OutputIsInput(output.to_path_buf()));
    }
    if modules.iter().any(|module| same_file(module, output)) {
        return Err(BuildError::OutputIsModule(output.to_path_buf()));
    }
    Ok(())
}

#[derive(Debug)]
pub enum BuildError {
    Vela(ProgramError),
    NoMain,
    OutputIsInput(PathBuf),
    OutputIsModule(PathBuf),
    Io {
        action: &'static str,
        error: io::Error,
    },
    CompilerNotStarted { cc: String, error: io::Error },
    CFailed {
        messages: String,
        build_dir: PathBuf,
    },
}

impl BuildError {
    pub fn render(&self, path: &str, source: &str) -> String {
        match self {
            BuildError::Vela(ProgramError { errors, files }) if files.is_empty() => render_all(
                path,
                errors
                    .iter()
                    .map(|error| render_error(path, source, error))
                    .collect(),
            ),
            BuildError::Vela(error) => error.render(path),
            BuildError::NoMain => format!("{}: error: program has no 'main' task", path),
            BuildError::OutputIsInput(output) => {
                format!(
                    "error: output '{}' would overwrite the input file",
                    output.display()
                )
            }
            BuildError::OutputIsModule(output) => {
                format!(
                    "error: output '{}' would overwrite a module the program includes",
                    output.display()
                )
            }
            BuildError::Io { action, error } => format!("error: cannot {}: {}", action, error),
            BuildError::CompilerNotStarted { cc, error } => {
                format!("error: failed to invoke C compiler '{}': {}", cc, error)
            }
            BuildError::CFailed {
                messages,
                build_dir,
            } => format!(
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

pub fn c_compiler_from_env() -> String {
    env::var("CC")
        .ok()
        .filter(|cc| !cc.trim().is_empty())
        .unwrap_or_else(|| "cc".to_string())
}

pub fn has_main(items: &[Item]) -> bool {
    items
        .iter()
        .any(|item| matches!(item, Item::Task(t) if t.name == "main"))
}

pub fn build(
    source: &str,
    input: &Path,
    output: &Path,
    cc: &str,
    notices: bool,
) -> std::result::Result<Warnings, BuildError> {
    build_program(
        source,
        input,
        input,
        output,
        cc,
        &mut read_from_disk,
        notices,
    )
}

pub fn build_program(
    source: &str,
    path: &Path,
    input: &Path,
    output: &Path,
    cc: &str,
    loader: &mut Loader,
    notices: bool,
) -> std::result::Result<Warnings, BuildError> {
    let partial = front_end_program(path, source, loader);
    let modules = module_paths(&partial);
    let (items, warnings) = checked(partial)?;
    if !has_main(&items) {
        return Err(BuildError::NoMain);
    }
    let c_code = Generator::new().with_notices(notices).generate(&items)?;
    overwrites_source(input, &modules, output)?;
    check_output(output)?;
    compile_c(&c_code, input, output, cc)?;
    Ok(warnings)
}

// Probe unwritable outputs up front so cc doesn't report them as C build failures.
fn check_output(output: &Path) -> std::result::Result<(), BuildError> {
    let folder = output
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let io_error = |error: io::Error| BuildError::Io {
        action: "write the executable",
        error,
    };
    if output.is_dir() {
        return Err(io_error(io::Error::other(format!(
            "'{}' is a folder",
            output.display()
        ))));
    }
    if !folder.is_dir() {
        return Err(io_error(io::Error::other(format!(
            "the folder '{}' doesn't exist",
            folder.display()
        ))));
    }
    let denied = |error: io::Error| {
        io_error(io::Error::new(
            error.kind(),
            format!("'{}': {}", output.display(), error),
        ))
    };
    let probe = folder.join(format!(".vela-output-check-{}", process::id()));
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
        }
        Err(error) if output.exists() => {
            fs::OpenOptions::new()
                .write(true)
                .open(output)
                .map_err(|_| denied(error))?;
        }
        Err(error) => return Err(denied(error)),
    }
    if output.exists() {
        fs::OpenOptions::new()
            .write(true)
            .open(output)
            .map_err(denied)?;
    }
    Ok(())
}

pub fn render_error(path: &str, source: &str, e: &Error) -> String {
    render_diagnostic(path, source, e, "error")
}

pub fn render_diagnostic(path: &str, source: &str, e: &Error, kind: &str) -> String {
    let Some(span) = e.span else {
        return format!("{}: {}: {}", path, kind, e.message);
    };
    let mut out = format!(
        "{}:{}:{}: {}: {}",
        path, span.line, span.col, kind, e.message
    );
    if let Some(line) = span.line.checked_sub(1).and_then(|i| source.lines().nth(i)) {
        let gutter = span.line.to_string();
        // Preserve tabs so the caret aligns under the source column.
        let indent: String = line
            .chars()
            .take(span.col.saturating_sub(1))
            .map(|c| if c == '\t' { '\t' } else { ' ' })
            .collect();
        let _ = write!(
            out,
            "\n {} | {}\n {} | {}^",
            gutter,
            line,
            " ".repeat(gutter.len()),
            indent
        );
    }
    out
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn compile_c(
    c_code: &str,
    input: &Path,
    output: &Path,
    cc: &str,
) -> std::result::Result<(), BuildError> {
    let mut build_dir = BuildDir::create().map_err(|error| BuildError::Io {
        action: "create build directory",
        error,
    })?;
    let base = input.file_stem().unwrap_or("vela_out".as_ref());
    let c_path = build_dir.path.join(format!("{}.c", base.to_string_lossy()));
    fs::write(&c_path, c_code).map_err(|error| BuildError::Io {
        action: "write C file",
        error,
    })?;
    fs::write(build_dir.path.join("vela_rt.h"), RUNTIME_H).map_err(|error| BuildError::Io {
        action: "write runtime header",
        error,
    })?;

    let mut words = cc.split_whitespace();
    let program = words.next().unwrap_or("cc");
    // Prefix `./` so a filename starting with `-` isn't parsed as a compiler flag.
    let output = if output.is_relative() && output.to_string_lossy().starts_with('-') {
        Path::new(".").join(output)
    } else {
        output.to_path_buf()
    };
    let result = Command::new(program)
        .arg("-std=c11")
        .arg("-O2")
        // After `-O2` so user flags in `CC` override defaults.
        .args(words)
        .arg("-I")
        .arg(&build_dir.path)
        .arg(&c_path)
        .arg("-o")
        .arg(&output)
        .arg("-lm")
        .output()
        .map_err(|error| BuildError::CompilerNotStarted {
            cc: program.to_string(),
            error,
        })?;

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

struct BuildDir {
    path: PathBuf,
    keep: bool,
}

impl BuildDir {
    fn create() -> io::Result<BuildDir> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let mut last_error = None;
        for _ in 0..16 {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path =
                env::temp_dir().join(format!("vela-build-{}-{}-{}", process::id(), nanos, n));
            // `create_dir` fails on an existing dir so a planted directory isn't reused.
            match fs::create_dir(&path) {
                Ok(()) => return Ok(BuildDir { path, keep: false }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    last_error = Some(error)
                }
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
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )]

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
