//! Command-line interface behavior of the `vela` binary.

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
