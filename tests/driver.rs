//! `vela::driver`, the compile pipeline shared by the `vela` command and
//! other tools: it must report every failure as a structured error, rendered
//! exactly as the command prints it.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use vela::ast::Span;
use vela::driver::{self, BuildError};

/// A fresh directory for one test; removed when the test passes.
struct TempDir(PathBuf);

impl TempDir {
    fn new(test: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!("vela-driver-{}-{}", std::process::id(), test));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn write(&self, name: &str, source: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, source).unwrap();
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

const HELLO: &str = "task main()\n    show(\"Hello, Vela!\")\nfi\n";

fn cc() -> String {
    driver::c_compiler_from_env()
}

#[test]
fn builds_a_program_that_runs() {
    let dir = TempDir::new("hello");
    let input = dir.write("hello.vela", HELLO);
    let exe = dir.0.join("hello");
    driver::build(HELLO, &input, &exe, &cc()).unwrap();
    let run = Command::new(&exe).output().unwrap();
    assert!(run.status.success());
    assert_eq!(String::from_utf8_lossy(&run.stdout), "Hello, Vela!\n");
}

#[test]
fn invalid_programs_report_the_vela_error() {
    let source = "task main()\n    show(missing)\nfi\n";
    let err = driver::build(source, Path::new("bad.vela"), Path::new("unused-output"), &cc()).unwrap_err();
    let BuildError::Vela(error) = &err else { panic!("expected a Vela error, got {:?}", err) };
    assert_eq!(error.span, Some(Span::new(2, 10)));
    assert_eq!(error.message, "unknown variable 'missing'");
    assert_eq!(
        err.render("bad.vela", source),
        "bad.vela:2:10: error: unknown variable 'missing'\n 2 |     show(missing)\n   |          ^"
    );
    assert_eq!(driver::generate_c(source).unwrap_err(), *error);
}

#[test]
fn a_program_without_main_is_not_an_executable() {
    let source = "task helper()\n    show(1)\nfi\n";
    let err = driver::build(source, Path::new("lib.vela"), Path::new("unused-output"), &cc()).unwrap_err();
    assert!(matches!(err, BuildError::NoMain), "{:?}", err);
    assert_eq!(err.render("lib.vela", source), "lib.vela: error: program has no 'main' task");
    // Its C can still be generated.
    assert!(driver::generate_c(source).unwrap().contains("void vela_helper(void)"));
}

#[test]
fn the_output_cannot_be_the_input() {
    let dir = TempDir::new("overwrite");
    let input = dir.write("prog.vela", HELLO);
    let same = dir.0.join(".").join("prog.vela");
    let err = driver::build(HELLO, &input, &same, &cc()).unwrap_err();
    assert!(matches!(err, BuildError::OutputIsInput(_)), "{:?}", err);
    assert_eq!(fs::read_to_string(&input).unwrap(), HELLO);
}

#[test]
fn a_missing_c_compiler_is_reported() {
    let err = driver::build(HELLO, Path::new("hello.vela"), Path::new("unused-output"), "vela-no-such-cc --flag").unwrap_err();
    let BuildError::CompilerNotStarted { cc, .. } = &err else { panic!("{:?}", err) };
    assert_eq!(cc, "vela-no-such-cc");
    assert!(err.render("hello.vela", HELLO).starts_with("error: failed to invoke C compiler 'vela-no-such-cc': "));
}

#[test]
fn c_compiler_failures_keep_the_generated_c() {
    let dir = TempDir::new("c-failure");
    let input = dir.write("hello.vela", HELLO);
    // Makes the C compiler fail on otherwise valid generated C.
    let failing_cc = format!("{} -include vela-missing-header.h", cc());
    let err = driver::build(HELLO, &input, &dir.0.join("hello"), &failing_cc).unwrap_err();
    let BuildError::CFailed { messages, build_dir } = &err else { panic!("{:?}", err) };
    assert!(messages.contains("vela-missing-header.h"), "{}", messages);
    assert!(build_dir.join("hello.c").is_file() && build_dir.join("vela_rt.h").is_file());
    assert!(err.render("hello.vela", HELLO).ends_with(&format!("error: C compilation failed (generated C kept in {})", build_dir.display())));
    fs::remove_dir_all(build_dir).unwrap();
}
