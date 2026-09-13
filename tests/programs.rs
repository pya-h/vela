//! End-to-end tests: every test is a `.vela` source file compiled with the
//! real `vela` binary.
//!
//! - `tests/run/NAME.vela` must compile and run; its stdout must equal
//!   `tests/run/NAME.out`.
//! - `tests/fail/NAME.vela` must be rejected by the Vela compiler itself (not by
//!   the C compiler). Its first line is `# error: LINE:COL: MESSAGE`, and the
//!   compiler must report exactly that message at that position.
//! - `examples/NAME.vela` must compile and run; its stdout must equal
//!   `tests/examples/NAME.out`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const VELA: &str = env!("CARGO_BIN_EXE_vela");

fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn vela_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {}", dir.display(), e))
        .map(|entry| entry.unwrap().path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "vela"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no .vela files in {}", dir.display());
    files
}

fn scratch_dir(suite: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vela-tests-{}-{}", std::process::id(), suite));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn stem(path: &Path) -> String {
    path.file_stem().unwrap().to_string_lossy().into_owned()
}

/// Compiles and runs `source`, comparing stdout with `expected_path`.
/// Returns a description of the failure, if any.
fn check_run(source: &Path, expected_path: &Path, scratch: &Path) -> Option<String> {
    let name = stem(source);
    let expected = match fs::read_to_string(expected_path) {
        Ok(s) => s,
        Err(_) => return Some(format!("{}: missing expected output {}", name, expected_path.display())),
    };
    let exe = scratch.join(&name);
    let compile = Command::new(VELA).arg(source).arg("-o").arg(&exe).output().unwrap();
    if !compile.status.success() {
        return Some(format!(
            "{}: compilation failed\n{}",
            name,
            String::from_utf8_lossy(&compile.stderr)
        ));
    }
    let run = Command::new(&exe).output().unwrap();
    if !run.status.success() {
        return Some(format!("{}: program exited with {}", name, run.status));
    }
    let actual = String::from_utf8_lossy(&run.stdout);
    if actual != expected {
        return Some(format!(
            "{}: output mismatch\n--- expected ---\n{}--- actual ---\n{}",
            name, expected, actual
        ));
    }
    None
}

/// Compiles `source`, expecting a Vela diagnostic that contains the text given
/// on the file's first line (`# error: TEXT`).
fn check_fail(source: &Path, scratch: &Path) -> Option<String> {
    let name = stem(source);
    let text = fs::read_to_string(source).unwrap();
    let expected = match text.lines().next().and_then(|l| l.strip_prefix("# error: ")) {
        Some(e) => e.trim(),
        None => return Some(format!("{}: first line must be '# error: TEXT'", name)),
    };
    let exe = scratch.join(&name);
    let compile = Command::new(VELA).arg(source).arg("-o").arg(&exe).output().unwrap();
    let stderr = String::from_utf8_lossy(&compile.stderr);
    if compile.status.success() {
        return Some(format!("{}: compiled, but expected error '{}'", name, expected));
    }
    if stderr.contains("C compilation failed") {
        return Some(format!("{}: rejected by the C compiler, not by Vela\n{}", name, stderr));
    }
    let needle = diagnostic_needle(expected);
    if !stderr.contains(&needle) {
        return Some(format!("{}: expected error containing '{}', got:\n{}", name, needle, stderr));
    }
    None
}

/// Turns a fixture's `LINE:COL: message` into the text the compiler prints
/// (`:LINE:COL: error: message`). Text without a location is matched as-is.
fn diagnostic_needle(expected: &str) -> String {
    if let Some((location, message)) = expected.split_once(": ") {
        let is_location = location
            .split_once(':')
            .is_some_and(|(l, c)| {
                !l.is_empty() && !c.is_empty()
                    && l.chars().all(|ch| ch.is_ascii_digit())
                    && c.chars().all(|ch| ch.is_ascii_digit())
            });
        if is_location {
            return format!(":{}: error: {}", location, message);
        }
    }
    expected.to_string()
}

fn report(suite: &str, failures: Vec<String>) {
    if !failures.is_empty() {
        panic!("{} failure(s) in {}:\n\n{}", failures.len(), suite, failures.join("\n\n"));
    }
}

#[test]
fn run_programs() {
    let dir = project_root().join("tests/run");
    let scratch = scratch_dir("run");
    let failures = vela_files(&dir)
        .iter()
        .filter_map(|src| check_run(src, &src.with_extension("out"), &scratch))
        .collect();
    let _ = fs::remove_dir_all(&scratch);
    report("tests/run", failures);
}

#[test]
fn failing_programs() {
    let dir = project_root().join("tests/fail");
    let scratch = scratch_dir("fail");
    let failures = vela_files(&dir)
        .iter()
        .filter_map(|src| check_fail(src, &scratch))
        .collect();
    let _ = fs::remove_dir_all(&scratch);
    report("tests/fail", failures);
}

#[test]
fn examples() {
    let root = project_root();
    let scratch = scratch_dir("examples");
    let failures = vela_files(&root.join("examples"))
        .iter()
        .filter_map(|src| {
            let expected = root.join("tests/examples").join(format!("{}.out", stem(src)));
            check_run(src, &expected, &scratch)
        })
        .collect();
    let _ = fs::remove_dir_all(&scratch);
    report("examples", failures);
}
