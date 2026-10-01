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
fn output_cannot_overwrite_a_module_of_the_program() {
    let dir = temp_dir("overwrite-module");
    let module = "task value() -> int\n    <- 1\nfi\n";
    std::fs::write(dir.join("numbers.vela"), module).unwrap();
    std::fs::write(dir.join("main.vela"), "include numbers\n\ntask main()\n    show(numbers.value())\nfi\n").unwrap();
    for args in [vec!["main.vela", "-o", "numbers.vela"], vec!["main.vela", "--emit-c", "-o", "./numbers.vela"]] {
        let out = Command::new(VELA).args(&args).current_dir(&dir).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{:?}", args);
        assert!(stderr(&out).contains("would overwrite a module the program includes"), "{:?}: {}", args, stderr(&out));
        assert_eq!(std::fs::read_to_string(dir.join("numbers.vela")).unwrap(), module, "{:?}", args);
    }
    let out = Command::new(VELA).args(["main.vela", "--emit-c", "-o", "main.vela"]).current_dir(&dir).output().unwrap();
    assert!(stderr(&out).contains("would overwrite the input file"), "{}", stderr(&out));
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn an_output_that_cannot_be_written_is_reported_before_compiling() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_dir("read-only-output");
    let exe = dir.join("hello");
    std::fs::write(&exe, "old").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o444)).unwrap();
    // Root may write a read-only file anyway.
    if std::fs::OpenOptions::new().write(true).open(&exe).is_err() {
        let out = vela(&[&example("hello"), "-o", exe.to_str().unwrap()]);
        assert_eq!(out.status.code(), Some(1));
        let err = stderr(&out);
        assert!(err.starts_with("error: cannot write the executable:") && err.contains("Permission denied"), "{}", err);
        assert!(!err.contains("C compilation failed"), "{}", err);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_module_reached_through_a_folder_above_is_read_once() {
    // From `app`, `../app/shapes` is `shapes`: one module, not two of one name.
    let dir = temp_dir("climb-back");
    let app = dir.join("app");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(app.join("shapes.vela"), "shape Point\n    x: int\nfi\n\ntask origin() -> Point\n    <- Point { x: 7 }\nfi\n").unwrap();
    std::fs::write(app.join("maker.vela"), "include ../app/shapes\n\ntask make() -> shapes.Point\n    <- shapes.origin()\nfi\n").unwrap();
    std::fs::write(app.join("main.vela"), "include maker\ninclude shapes\n\ntask main()\n    bind p: shapes.Point = maker.make()\n    show(p.x)\nfi\n").unwrap();
    let out = Command::new(VELA).args(["main.vela", "-o", "main"]).current_dir(&app).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let run = Command::new(app.join("main")).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout), "7\n");
    // Included twice by one file, under two spellings, is the error for including a module twice.
    std::fs::write(app.join("twice.vela"), "include ../app/shapes\ninclude shapes => other\n\ntask main()\nfi\n").unwrap();
    let out = Command::new(VELA).args(["twice.vela", "--check"]).current_dir(&app).output().unwrap();
    assert!(stderr(&out).starts_with("twice.vela:2:9: error: this module is already included at 1:16"), "{}", stderr(&out));
    let _ = std::fs::remove_dir_all(&dir);
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

/// The arguments `vela` gives the C compiler, recorded by a `CC` that writes
/// them down and then compiles, with `extra` after `vela`'s own arguments.
#[cfg(unix)]
fn c_compiler_arguments(test: &str, extra: &[&str], cc_args: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_dir(test);
    let log = dir.join("args");
    let script = dir.join("cc.sh");
    let real = std::env::var("CC").ok().filter(|cc| !cc.trim().is_empty()).unwrap_or_else(|| "cc".to_string());
    std::fs::write(&script, format!("#!/bin/sh\necho \"$@\" > '{}'\nexec {} \"$@\"\n", log.display(), real)).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let exe = dir.join("hello");
    let hello = example("hello");
    let mut args = vec![hello.as_str(), "-o", exe.to_str().unwrap()];
    args.extend(extra);
    let out = Command::new(VELA).args(&args).env("CC", format!("{} {}", script.display(), cc_args)).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), format!("compiled: {}\n", exe.display()));
    let written = std::fs::read_to_string(&log).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    written
}

#[test]
#[cfg(unix)]
fn quick_builds_without_optimizing_the_c() {
    // An optimized build by default, and `-O0` last with `--quick`, even when
    // `CC` names another level.
    let optimized = c_compiler_arguments("quick-default", &[], "");
    assert!(optimized.contains("-O2") && !optimized.contains("-O0"), "{}", optimized);
    let quick = c_compiler_arguments("quick", &["--quick"], "-O3");
    let levels: Vec<&str> = quick.split_whitespace().filter(|word| word.starts_with("-O")).collect();
    assert_eq!(levels, ["-O2", "-O3", "-O0"], "{}", quick);
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
    for (args, message) in [
        (vec![example("hello"), "--emit-c".to_string(), "--check".to_string()], "error: --emit-c and --check can't be used together"),
        (vec![example("hello"), "--quick".to_string(), "--emit-c".to_string()], "error: --quick builds an executable faster, and --emit-c builds none"),
        (vec![example("hello"), "--check".to_string(), "--quick".to_string()], "error: --quick builds an executable faster, and --check builds none"),
        (vec![example("hello"), "-o".to_string(), "a".to_string(), "-o".to_string(), "b".to_string()], "error: -o given more than once"),
        (vec![example("hello"), example("fib")], "error: only one input file is allowed"),
        (vec![example("hello"), "--".to_string(), example("fib")], "error: only one input file is allowed"),
    ] {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = vela(&args);
        assert_eq!(out.status.code(), Some(2), "{:?}: {}", args, stderr(&out));
        assert!(stderr(&out).starts_with(message), "{:?}: {}", args, stderr(&out));
        assert!(stderr(&out).contains("usage: vela"), "{}", stderr(&out));
    }
}

#[test]
fn short_help_and_the_exact_version() {
    let out = vela(&["-h"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("usage: vela"));
    let out = vela(&["--version"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), format!("vela {}\n", env!("CARGO_PKG_VERSION")));
}

#[test]
fn emitted_c_into_a_missing_folder_is_an_error() {
    let out = vela(&[&example("hello"), "--emit-c", "-o", "/nonexistent-vela-folder/hello.c"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).starts_with("error: cannot write '/nonexistent-vela-folder/hello.c':"), "{}", stderr(&out));
}

#[cfg(target_os = "linux")]
#[test]
fn help_and_version_written_to_a_full_disk_are_errors() {
    for flag in ["--help", "--version"] {
        let full = std::fs::OpenOptions::new().write(true).open("/dev/full").unwrap();
        let out = Command::new(VELA).arg(flag).stdout(full).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{}", flag);
        assert!(stderr(&out).starts_with("error: cannot write output:"), "{}: {}", flag, stderr(&out));
    }
}

#[cfg(unix)]
#[test]
fn a_build_folder_that_cannot_be_made_is_an_error() {
    let dir = temp_dir("no-tmp");
    let out = Command::new(VELA).args([&example("hello"), "-o", dir.join("hello").to_str().unwrap()]).env("TMPDIR", "/nonexistent-vela-tmp").output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).starts_with("error: cannot create build directory:"), "{}", stderr(&out));
    let _ = std::fs::remove_dir_all(&dir);
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
fn every_mistake_is_reported_and_too_many_are_counted() {
    let dir = temp_dir("many-errors");
    let file = dir.join("many.vela");
    // Three mistakes: each is shown with its line and a caret under it.
    std::fs::write(&file, "task main()\n    show(a)\n    show(b)\n    show(c)\nfi\n").unwrap();
    let out = vela(&[file.to_str().unwrap(), "--check"]);
    assert_eq!(out.status.code(), Some(1));
    let shown = stderr(&out);
    for (line, name) in [(2, "a"), (3, "b"), (4, "c")] {
        assert!(shown.contains(&format!("{}:{}: error: unknown variable '{}'", line, 10, name)), "{}", shown);
    }
    assert_eq!(shown.matches("error:").count(), 3, "{}", shown);

    // More than the command shows at once: the first `MAX_SHOWN`, then a count.
    let many: String = (0..vela::driver::MAX_SHOWN + 2).map(|i| format!("    show(v{})\n", i)).collect();
    std::fs::write(&file, format!("task main()\n{}fi\n", many)).unwrap();
    let out = vela(&[file.to_str().unwrap(), "--check"]);
    let shown = stderr(&out);
    assert_eq!(shown.matches("error: unknown variable").count(), vela::driver::MAX_SHOWN, "{}", shown);
    assert!(shown.ends_with("error: 2 more mistakes, not shown; fix these first and check again\n"), "{}", shown);
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
    assert!(String::from_utf8_lossy(&out.stdout).contains("vela_shop__1__cart__0__add("));

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

/// A program that builds, with one warning: `==` on a plain parameter's
/// shape, which the compiler gives as a copy.
const WARNED: &str = "shape Point\n    x: int\nfi\n\ntask same(a: Point, bind& b: Point) -> bool\n    <- a == b\nfi\n\ntask main()\n    bind p = Point { x: 1 }\n    show(same(p, &p))\nfi\n";

#[test]
fn warnings_go_to_stderr_and_the_program_still_builds() {
    let dir = temp_dir("warned");
    let input = dir.join("warned.vela");
    std::fs::write(&input, WARNED).unwrap();
    let input = input.to_str().unwrap();
    let wanted = format!("{}:6:8: warning: '==' compares Point by where it is, and 'a' is a parameter, given as the compiler chose", input);

    let out = vela(&[input, "--check"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).starts_with(&wanted), "{}", stderr(&out));
    assert!(stderr(&out).contains(" 6 |     <- a == b\n   |        ^"), "{}", stderr(&out));
    assert_eq!(stderr(&out).matches(": warning: ").count(), 1, "{}", stderr(&out));

    let out = vela(&[input, "--emit-c"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).starts_with(&wanted), "{}", stderr(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("int main"), "the C is still written");

    let exe = dir.join("warned");
    let out = vela(&[input, "-o", exe.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).starts_with(&wanted), "{}", stderr(&out));
    let run = Command::new(&exe).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout), "false\n");

    // Saying how the parameter is given silences it; a mistake is reported
    // alone, its warnings left until it is fixed.
    std::fs::write(dir.join("said.vela"), WARNED.replace("same(a: Point", "same(bind a: Point")).unwrap();
    let out = vela(&[dir.join("said.vela").to_str().unwrap(), "--check"]);
    assert!(out.status.success() && out.stderr.is_empty(), "{}", stderr(&out));
    std::fs::write(dir.join("broken.vela"), WARNED.replace("show(same(p, &p))", "show(same(p, &p), missing)")).unwrap();
    let out = vela(&[dir.join("broken.vela").to_str().unwrap(), "--check"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains(": error: unknown variable 'missing'") && !stderr(&out).contains(": warning: "), "{}", stderr(&out));
}
