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
fn version_prints_version_and_succeeds() {
    let out = vela(&["--version"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("vela "), "{}", stdout);
}

#[test]
fn check_only_type_checks_without_building() {
    let dir = temp_dir("check");
    let out = vela(&[&example("hello"), "--check"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("checked:"), "{}", stdout);
    assert!(out.stderr.is_empty(), "{}", stderr(&out));

    let exe = dir.join("should_not_be_created");
    let out = vela(&[&example("hello"), "--check", "-o", exe.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!exe.exists(), "--check should not create an output file");

    let out = vela(&["does/not/exist.vela", "--check"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).starts_with("error: cannot read 'does/not/exist.vela'"));
    let _ = std::fs::remove_dir_all(&dir);
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

#[test]
fn emit_c_writes_to_the_output_file() {
    let dir = temp_dir("emit-c-file");
    let c_file = dir.join("hello.c");
    let out = vela(&[&example("hello"), "--emit-c", "-o", c_file.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(out.stdout.is_empty());
    assert!(std::fs::read_to_string(&c_file).unwrap().contains("int main(void)"));

    // It never overwrites the input.
    let input = dir.join("hello.vela");
    std::fs::copy(example("hello"), &input).unwrap();
    let out = vela(&[input.to_str().unwrap(), "--emit-c", "-o", input.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("would overwrite the input file"), "{}", stderr(&out));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_file_named_like_an_option_follows_a_double_dash() {
    let dir = temp_dir("double-dash");
    std::fs::copy(example("hello"), dir.join("-x.vela")).unwrap();
    let out = Command::new(VELA).current_dir(&dir).args(["--emit-c", "--", "-x.vela"]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    // An output starting with `-` isn't read as a C compiler option.
    let out = Command::new(VELA).current_dir(&dir).args(["-o", "-lm", "--", "-x.vela"]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(dir.join("-lm").is_file());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_output_in_a_missing_folder_is_reported_before_compiling() {
    let dir = temp_dir("missing-folder");
    let output = dir.join("nowhere").join("hello");
    let out = vela(&[&example("hello"), "-o", output.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(err.starts_with("error: cannot write the executable: the folder"), "{}", err);
    assert!(!err.contains("C compilation failed"), "{}", err);
    let out = vela(&[&example("hello"), "-o", dir.to_str().unwrap()]);
    assert!(stderr(&out).contains("is a folder"), "{}", stderr(&out));
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(target_os = "linux")]
#[test]
fn a_failed_write_to_standard_output_is_an_error_not_a_crash() {
    let full = std::fs::OpenOptions::new().write(true).open("/dev/full").unwrap();
    let out = Command::new(VELA).args([&example("hello"), "--emit-c"]).stdout(full).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).starts_with("error: cannot write output:"), "{}", stderr(&out));
}

#[test]
fn version_is_an_option_like_the_others() {
    // A wrong argument is still an error, even beside --version.
    let out = vela(&[&example("hello"), "--bogus", "--version"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(out.stdout.is_empty());
    // After -o, `--version` is the output's name.
    let dir = temp_dir("version-output");
    let out = Command::new(VELA).args([&example("hello"), "-o", "--version"]).current_dir(&dir).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(dir.join("--version").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn wrong_argument_combinations_are_rejected() {
    for args in [
        vec![example("hello"), "--emit-c".to_string(), "--check".to_string()],
        vec![example("hello"), "-o".to_string(), "a".to_string(), "-o".to_string(), "b".to_string()],
        vec![example("hello"), example("fib")],
    ] {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = vela(&args);
        assert_eq!(out.status.code(), Some(2), "{:?}: {}", args, stderr(&out));
        assert!(stderr(&out).contains("usage: vela"), "{}", stderr(&out));
    }
}

#[test]
fn the_default_output_is_a_out() {
    let dir = temp_dir("a-out");
    let out = Command::new(VELA).arg(example("hello")).current_dir(&dir).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "compiled: a.out\n");
    assert!(dir.join("a.out").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_reports_what_a_build_would() {
    let dir = temp_dir("check-errors");
    let invalid = dir.join("invalid.vela");
    std::fs::write(&invalid, "task main()\n    show(missing)\nfi\n").unwrap();
    let out = vela(&[invalid.to_str().unwrap(), "--check"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("2:10: error: unknown variable 'missing'"), "{}", stderr(&out));
    assert!(out.stdout.is_empty());
    // A program without `main` can't be built, so --check rejects it too;
    // --emit-c still gives its C.
    let library = dir.join("library.vela");
    std::fs::write(&library, "task helper() -> int\n    <- 1\nfi\n").unwrap();
    let out = vela(&[library.to_str().unwrap(), "--check"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).ends_with("error: program has no 'main' task\n"), "{}", stderr(&out));
    let out = vela(&[library.to_str().unwrap(), "--emit-c"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_c_compiler_failure_keeps_the_generated_c() {
    let dir = temp_dir("cc-fails");
    let exe = dir.join("hello");
    let out = Command::new(VELA).args([&example("hello"), "-o", exe.to_str().unwrap()]).env("CC", "false").output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    let kept = err.trim_end().strip_suffix(')').and_then(|e| e.split_once("error: C compilation failed (generated C kept in ")).map(|(_, folder)| folder.to_string());
    let kept = kept.unwrap_or_else(|| panic!("{}", err));
    assert!(std::path::Path::new(&kept).is_dir(), "{}", kept);
    let _ = std::fs::remove_dir_all(&kept);
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn an_output_that_cant_be_written_is_reported_before_compiling() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_dir("read-only-folder");
    let folder = dir.join("locked");
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555)).unwrap();
    // Root can write anywhere, so there is nothing to test.
    let writable = std::fs::File::create(folder.join("probe")).is_ok();
    if !writable {
        let out = vela(&[&example("hello"), "-o", folder.join("hello").to_str().unwrap()]);
        assert_eq!(out.status.code(), Some(1));
        let err = stderr(&out);
        assert!(err.starts_with("error: cannot write the executable:"), "{}", err);
        assert!(!err.contains("C compilation failed"), "{}", err);
    }
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn modules_are_read_next_to_the_program() {
    let out = vela(&[&example("shop"), "--check"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let out = vela(&[&example("shop"), "--emit-c"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("vela_cart__0__add("));

    // An error in a module is shown in that module's file.
    let dir = temp_dir("modules");
    std::fs::create_dir_all(dir.join("parts")).unwrap();
    std::fs::write(dir.join("main.vela"), "include parts/lib\n\ntask main()\n    show(lib.twice(2))\nfi\n").unwrap();
    std::fs::write(dir.join("parts/lib.vela"), "task twice(n: int) -> int\n    <- n * \"2\"\nfi\n").unwrap();
    let main = dir.join("main.vela");
    let out = vela(&[main.to_str().unwrap(), "--check"]);
    assert_eq!(out.status.code(), Some(1));
    let module = dir.join("parts/lib.vela");
    assert_eq!(
        stderr(&out),
        format!("{}:2:10: error: arithmetic requires numeric operands\n 2 |     <- n * \"2\"\n   |          ^\n", module.display())
    );
    let _ = std::fs::remove_dir_all(&dir);
}
