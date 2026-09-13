//! Command-line interface behavior of the `vela` binary.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::process::{Command, Output};

const VELA: &str = env!("CARGO_BIN_EXE_vela");

fn vela(args: &[&str]) -> Output {
    Command::new(VELA).args(args).output().unwrap()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn example(name: &str) -> String {
    format!("{}/examples/{}.vela", env!("CARGO_MANIFEST_DIR"), name)
}

#[test]
fn no_arguments_prints_usage() {
    let out = vela(&[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("error: no input file"));
    assert!(stderr(&out).contains("usage: vela"));
}

#[test]
fn help_prints_usage_and_succeeds() {
    let out = vela(&["--help"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("usage: vela"));
}

#[test]
fn unknown_option_is_rejected() {
    let out = vela(&[&example("hello"), "--bogus"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("unknown option '--bogus'"));
}

#[test]
fn output_flag_requires_a_value() {
    let out = vela(&[&example("hello"), "-o"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("-o requires an output path"));
}

#[test]
fn options_may_come_before_the_input() {
    let out = vela(&["--emit-c", &example("hello")]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("int main(void)"));
}

#[test]
fn unreadable_input_has_no_fake_location() {
    let out = vela(&["does/not/exist.vela"]);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(err.starts_with("error: cannot read 'does/not/exist.vela'"), "{}", err);
}

#[test]
fn diagnostics_show_the_source_line_and_caret() {
    let dir = std::env::temp_dir().join(format!("vela-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("bad.vela");
    std::fs::write(&src, "task main()\n\tshow(missing)\nfi\n").unwrap();
    let out = vela(&[src.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(out.status.code(), Some(1));
    let expected = format!(
        "{}:2:7: error: unknown variable 'missing'\n 2 | \tshow(missing)\n   | \t     ^\n",
        src.display()
    );
    assert_eq!(stderr(&out), expected);
}

/// A fresh directory for one test, removed by the caller.
fn temp_dir(test: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("vela-cli-{}-{}", std::process::id(), test));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn output_cannot_overwrite_the_input() {
    let dir = temp_dir("overwrite");
    let src = dir.join("prog.vela");
    let source = "task main()\n    show(1)\nfi\n";
    std::fs::write(&src, source).unwrap();
    let out = vela(&[src.to_str().unwrap(), "-o", src.to_str().unwrap()]);
    let kept = std::fs::read_to_string(&src).unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("would overwrite the input file"), "{}", stderr(&out));
    assert_eq!(kept, source);
}

#[test]
fn cc_may_include_arguments() {
    let dir = temp_dir("cc-args");
    let exe = dir.join("hello");
    let cc = std::env::var("CC").unwrap_or_default();
    let cc = if cc.trim().is_empty() { "cc".to_string() } else { cc };
    for cc_value in [format!("{} -DVELA_UNUSED_MACRO=1", cc), String::new()] {
        let out = Command::new(VELA)
            .args([&example("hello"), "-o", exe.to_str().unwrap()])
            .env("CC", &cc_value)
            .output()
            .unwrap();
        assert!(out.status.success(), "CC={:?}: {}", cc_value, stderr(&out));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn nul_character_is_reported() {
    let dir = temp_dir("nul");
    let src = dir.join("nul.vela");
    std::fs::write(&src, "task main()\n    show(1)\0\nfi\n").unwrap();
    let out = vela(&[src.to_str().unwrap(), "--emit-c"]);
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(out.status.code(), Some(1));
    let first = stderr(&out).lines().next().unwrap_or_default().to_string();
    assert_eq!(first, format!("{}:2:12: error: unexpected character '\\0'", src.display()));
}
