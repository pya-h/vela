//! The C runtime, `runtime/vela_rt.h`:
//!
//! - `tests/runtime/NAME.c` tests it directly: each must compile as strict ISO
//!   C11 and print `ok`.
//! - Programs that use regions must free all their memory and stay within it:
//!   where the C compiler supports AddressSanitizer and UndefinedBehaviorSanitizer,
//!   the runtime tests and every `tests/run` and `examples` program that
//!   declares a region run under them (with leak checks on Linux).

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const VELA: &str = env!("CARGO_BIN_EXE_vela");

const STRICT_C_FLAGS: &[&str] = &["-std=c11", "-pedantic-errors", "-Wall", "-Wextra", "-Werror"];

const SANITIZER_FLAGS: &[&str] = &["-std=c11", "-g", "-fsanitize=address,undefined", "-fno-sanitize-recover=all"];

/// A command running the C compiler: `$CC`, which may include arguments, or `cc`.
fn c_compiler() -> Command {
    let cc = std::env::var("CC").unwrap_or_default();
    let mut words = cc.split_whitespace();
    let mut command = Command::new(words.next().unwrap_or("cc"));
    command.args(words);
    command
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn scratch_dir(suite: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vela-tests-{}-{}", std::process::id(), suite));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn files(dir: &str, extension: &str) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(root().join(dir))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|p| p.extension().is_some_and(|ext| ext == extension))
        .collect();
    files.sort();
    files
}

fn stem(path: &Path) -> String {
    path.file_stem().unwrap().to_string_lossy().into_owned()
}

/// Compiles the C file `source` with `flags` into `exe`; the compiler's
/// errors on failure.
fn compile_c(source: &Path, exe: &Path, flags: &[&str]) -> Result<(), String> {
    let output = c_compiler()
        .args(flags)
        .arg("-I")
        .arg(root().join("runtime"))
        .arg(source)
        .arg("-o")
        .arg(exe)
        .arg("-lm")
        .output()
        .unwrap();
    if output.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&output.stderr).into_owned()) }
}

/// Runs `exe` with the sanitizers' options, returning its output.
fn run(exe: &Path) -> std::process::Output {
    let mut command = Command::new(exe);
    // Leak checking is part of AddressSanitizer only on Linux.
    let leaks = if cfg!(target_os = "linux") { "detect_leaks=1" } else { "detect_leaks=0" };
    command.env("ASAN_OPTIONS", format!("{}:abort_on_error=0", leaks));
    command.output().unwrap()
}

/// Whether the C compiler can build and run a sanitized program here.
fn sanitizers_work(scratch: &Path) -> bool {
    let source = scratch.join("probe.c");
    fs::write(&source, "int main(void) { return 0; }\n").unwrap();
    let exe = scratch.join("probe");
    compile_c(&source, &exe, SANITIZER_FLAGS).is_ok() && run(&exe).status.success()
}

#[test]
fn runtime_tests_pass_as_strict_c() {
    let scratch = scratch_dir("runtime-strict");
    let mut failures = Vec::new();
    for source in files("tests/runtime", "c") {
        let exe = scratch.join(stem(&source));
        if let Err(errors) = compile_c(&source, &exe, STRICT_C_FLAGS) {
            failures.push(format!("{}: not strict C11\n{}", stem(&source), errors));
            continue;
        }
        let output = run(&exe);
        if !output.status.success() || String::from_utf8_lossy(&output.stdout) != "ok\n" {
            failures.push(format!("{}: {}", stem(&source), String::from_utf8_lossy(&output.stdout)));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn regions_free_everything_under_the_sanitizers() {
    let scratch = scratch_dir("runtime-sanitized");
    if !sanitizers_work(&scratch) {
        eprintln!("skipped: the C compiler can't build sanitized programs here");
        return;
    }
    let mut failures = Vec::new();
    for source in files("tests/runtime", "c") {
        let exe = scratch.join(stem(&source));
        match compile_c(&source, &exe, SANITIZER_FLAGS) {
            Ok(()) => {
                let output = run(&exe);
                if !output.status.success() {
                    failures.push(format!("{}:\n{}", stem(&source), String::from_utf8_lossy(&output.stderr)));
                }
            }
            Err(errors) => failures.push(format!("{}: {}", stem(&source), errors)),
        }
    }
    let programs = files("tests/run", "vela").into_iter().chain(files("examples", "vela"));
    let mut checked = 0;
    for source in programs.filter(|p| fs::read_to_string(p).unwrap().contains("region")) {
        checked += 1;
        let name = stem(&source);
        let emit = Command::new(VELA).arg(&source).arg("--emit-c").output().unwrap();
        assert!(emit.status.success(), "{}: {}", name, String::from_utf8_lossy(&emit.stderr));
        let c_path = scratch.join(format!("{}.c", name));
        fs::write(&c_path, &emit.stdout).unwrap();
        let exe = scratch.join(&name);
        if let Err(errors) = compile_c(&c_path, &exe, SANITIZER_FLAGS) {
            failures.push(format!("{}: {}", name, errors));
            continue;
        }
        let output = run(&exe);
        if !output.status.success() {
            failures.push(format!("{}:\n{}", name, String::from_utf8_lossy(&output.stderr)));
        }
    }
    assert!(checked >= 5, "expected several region programs, found {}", checked);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
