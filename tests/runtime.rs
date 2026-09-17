//! The C runtime, `runtime/vela_rt.h`:
//!
//! - `tests/runtime/NAME.c` tests it directly: each must compile as strict ISO
//!   C11 and print `ok`.
//! - Programs must be free of undefined behavior, and free all their memory:
//!   where the C compiler supports AddressSanitizer and UndefinedBehaviorSanitizer,
//!   the runtime tests and every `tests/run` and `examples` program run under
//!   them (with leak checks on Linux), and must print their expected output.
//!   Each `tests/trap` program must stop with its message and no sanitizer
//!   report, so nothing undefined happens before an integer check stops it.

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
    let _ = fs::remove_dir_all(&scratch);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn programs_run_clean_under_the_sanitizers() {
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
    // Each program with its expected output (`tests/run/NAME.out`,
    // `tests/examples/NAME.out`, or `tests/trap/NAME.out`), and for a trap, the
    // message it stops with.
    let programs: Vec<(PathBuf, PathBuf, String, Option<String>)> = files("tests/run", "vela")
        .into_iter()
        .map(|p| (p.with_extension("out"), format!("run-{}", stem(&p)), p, None))
        .chain(files("examples", "vela").into_iter().map(|p| (root().join("tests/examples").join(format!("{}.out", stem(&p))), format!("example-{}", stem(&p)), p, None)))
        .chain(files("tests/trap", "vela").into_iter().map(|p| {
            let message = fs::read_to_string(&p).unwrap().lines().next().and_then(|l| l.strip_prefix("# trap: ")).map(|m| m.trim().to_string());
            (p.with_extension("out"), format!("trap-{}", stem(&p)), p, Some(message.unwrap_or_default()))
        }))
        .map(|(expected, name, source, trap)| (source, expected, name, trap))
        .collect();
    assert!(programs.len() >= 20, "expected the run tests and examples, found {}", programs.len());
    // Sanitized builds are slow, so programs are checked on a few threads.
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get()).min(8);
    let chunks: Vec<_> = programs.chunks(programs.len().div_ceil(threads)).collect();
    let results: Vec<Vec<String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| scope.spawn(|| chunk.iter().filter_map(|(source, expected, name, trap)| check_sanitized(source, expected, name, trap.as_deref(), &scratch)).collect()))
            .collect();
        handles.into_iter().map(|handle| handle.join().unwrap()).collect()
    });
    failures.extend(results.into_iter().flatten());
    let _ = fs::remove_dir_all(&scratch);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Builds `source` with the sanitizers and runs it, expecting the output in
/// `expected` and success, or, when `trap` gives a message, to stop with exactly
/// that message; a description of the failure, if any.
fn check_sanitized(source: &Path, expected: &Path, name: &str, trap: Option<&str>, scratch: &Path) -> Option<String> {
    let emit = Command::new(VELA).arg(source).arg("--emit-c").output().unwrap();
    if !emit.status.success() {
        return Some(format!("{}: {}", name, String::from_utf8_lossy(&emit.stderr)));
    }
    let c_path = scratch.join(format!("{}.c", name));
    fs::write(&c_path, &emit.stdout).unwrap();
    let exe = scratch.join(name);
    if let Err(errors) = compile_c(&c_path, &exe, SANITIZER_FLAGS) {
        return Some(format!("{}: {}", name, errors));
    }
    let output = run(&exe);
    let stderr = String::from_utf8_lossy(&output.stderr);
    match trap {
        None if !output.status.success() => return Some(format!("{}:\n{}", name, stderr)),
        Some(message) if output.status.success() || stderr != format!("vela: {}\n", message) => {
            return Some(format!("{}: expected to stop with 'vela: {}' under the sanitizers, got:\n{}", name, message, stderr));
        }
        _ => {}
    }
    let wanted = fs::read_to_string(expected).unwrap_or_default();
    if String::from_utf8_lossy(&output.stdout) != wanted {
        return Some(format!("{}: output differs from {} under the sanitizers", name, expected.display()));
    }
    None
}

/// An allocation that can't be satisfied (one whose size overflows, one the
/// system refuses, and a slice's items whose size in bytes overflows) stops the
/// program with a message instead of returning memory that isn't there.
#[test]
fn an_impossible_allocation_stops_the_program() {
    let scratch = scratch_dir("runtime-out-of-memory");
    let allocations = [
        ("overflow", "vela__rt__region_alloc(r, SIZE_MAX - 8, 1)"),
        ("refused", "vela__rt__region_alloc(r, SIZE_MAX / 2, 1)"),
        ("items", "vela__rt__region_items(r, INT64_MAX, sizeof(int64_t), _Alignof(int64_t), \"1:1\")"),
    ];
    for (name, allocation) in allocations {
        let source = scratch.join(format!("{}.c", name));
        fs::write(
            &source,
            format!(
                "#include \"vela_rt.h\"\nint main(void) {{\n    vela__rt__region r[1] = {{ VELA__RT__REGION_EMPTY }};\n    unsigned char* p = {};\n    p[0] = 1;\n    puts(\"allocated\");\n    return 0;\n}}\n",
                allocation
            ),
        )
        .unwrap();
        let exe = scratch.join(name);
        compile_c(&source, &exe, STRICT_C_FLAGS).unwrap_or_else(|errors| panic!("{}: {}", name, errors));
        let output = Command::new(&exe).output().unwrap();
        assert!(!output.status.success(), "{}: the allocation succeeded", name);
        assert!(output.stdout.is_empty(), "{}: {}", name, String::from_utf8_lossy(&output.stdout));
        assert_eq!(String::from_utf8_lossy(&output.stderr), "vela: out of memory\n", "{}", name);
    }
    let _ = fs::remove_dir_all(&scratch);
}
