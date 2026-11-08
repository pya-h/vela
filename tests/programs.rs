//! End-to-end `.vela` suite compiled with the real `vela` binary:
//! - `tests/run/` & `examples/`: strict ISO C11, exact stdout vs `.out` and `; warning:` order.
//! - `tests/fail/`: rejected with `; error: ...` (exact ordered set when multiple are listed).
//! - `tests/trap/`: strict C11, exits with `vela: <; trap: ...>` on stderr and matching `.out`.
//! - `<NAME>.in` next to `.out` provides stdin when present.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const VELA: &str = env!("CARGO_BIN_EXE_vela");

const STRICT_C_FLAGS: &[&str] = &["-std=c11", "-pedantic-errors", "-Wall", "-Wextra", "-Werror"];

fn c_compiler() -> Command {
    let cc = std::env::var("CC").unwrap_or_default();
    let mut words = cc.split_whitespace();
    let mut command = Command::new(words.next().unwrap_or("cc"));
    command.args(words);
    command
}

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

fn check_run(source: &Path, expected_path: &Path, trap: Option<&str>, scratch: &Path) -> Option<String> {
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
    if let Some(failure) = check_warnings(source, &String::from_utf8_lossy(&compile.stderr)) {
        return Some(failure);
    }
    if let Some(failure) = check_strict_c(source, scratch) {
        return Some(failure);
    }
    let input = fs::read(expected_path.with_extension("in")).unwrap_or_default();
    let run = match run_with_timeout(&exe, input, &run_folder(source, &name, scratch)) {
        Some(run) => run,
        None => return Some(format!("{}: still running after {} seconds", name, RUN_TIMEOUT.as_secs())),
    };
    match trap {
        None if !run.status.success() => {
            return Some(format!("{}: program exited with {}\n{}", name, run.status, String::from_utf8_lossy(&run.stderr)));
        }
        Some(_) if run.status.success() => return Some(format!("{}: program finished, but was expected to stop", name)),
        Some(message) if String::from_utf8_lossy(&run.stderr) != format!("vela: {}\n", message) => {
            return Some(format!(
                "{}: expected the program to stop with\nvela: {}\ngot:\n{}",
                name,
                message,
                String::from_utf8_lossy(&run.stderr)
            ));
        }
        _ => {}
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

fn run_folder(source: &Path, name: &str, scratch: &Path) -> PathBuf {
    let folder = scratch.join(format!("{}.files", name));
    let _ = fs::remove_dir_all(&folder);
    fs::create_dir_all(&folder).unwrap();
    copy_folder(&source.with_extension(""), &folder);
    folder
}

fn copy_folder(from: &Path, to: &Path) {
    let Ok(entries) = fs::read_dir(from) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() {
            fs::create_dir_all(&target).unwrap();
            copy_folder(&path, &target);
        } else {
            fs::copy(&path, &target).unwrap();
        }
    }
}

const RUN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn run_with_timeout(exe: &Path, input: Vec<u8>, folder: &Path) -> Option<std::process::Output> {
    use std::process::Stdio;
    let mut child = Command::new(exe).current_dir(folder).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    // Written on a thread, and closed after, so the program sees the input end;
    // a program that stops reading early closes the pipe, which isn't an error.
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || {
        let _ = std::io::Write::write_all(&mut stdin, &input);
    });
    // Read the output on threads, so a program writing a lot doesn't block on a full pipe.
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut stdout, &mut bytes).map(|_| bytes).unwrap_or_default()
    });
    let err = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut stderr, &mut bytes).map(|_| bytes).unwrap_or_default()
    });
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > RUN_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    writer.join().unwrap();
    Some(std::process::Output { status, stdout: out.join().unwrap(), stderr: err.join().unwrap() })
}

fn check_strict_c(source: &Path, scratch: &Path) -> Option<String> {
    let name = stem(source);
    let emit = Command::new(VELA).arg(source).arg("--emit-c").output().unwrap();
    if !emit.status.success() {
        return Some(format!("{}: --emit-c failed\n{}", name, String::from_utf8_lossy(&emit.stderr)));
    }
    let c_path = scratch.join(format!("{}.c", name));
    fs::write(&c_path, &emit.stdout).unwrap();
    let cc = c_compiler()
        .args(STRICT_C_FLAGS)
        .arg("-I")
        .arg(project_root().join("runtime"))
        .arg(&c_path)
        .arg("-o")
        .arg(scratch.join(format!("{}.strict", name)))
        .arg("-lm")
        .output()
        .unwrap();
    if !cc.status.success() {
        return Some(format!(
            "{}: generated C is not strict ISO C11\n{}",
            name,
            String::from_utf8_lossy(&cc.stderr)
        ));
    }
    None
}

fn check_fail(source: &Path, scratch: &Path) -> Option<String> {
    let name = stem(source);
    let text = fs::read_to_string(source).unwrap();
    let expected: Vec<String> = text
        .lines()
        .take_while(|line| line.starts_with(';'))
        .filter_map(|line| line.strip_prefix("; error: "))
        .map(|error| diagnostic_line(source, error.trim()))
        .collect();
    let Some(first) = expected.first() else {
        return Some(format!("{}: first line must be '; error: TEXT'", name));
    };
    let exe = scratch.join(&name);
    let compile = Command::new(VELA).arg(source).arg("-o").arg(&exe).output().unwrap();
    let stderr = String::from_utf8_lossy(&compile.stderr);
    if compile.status.success() {
        return Some(format!("{}: compiled, but expected error '{}'", name, first));
    }
    if stderr.contains("C compilation failed") {
        return Some(format!("{}: rejected by the C compiler, not by Vela\n{}", name, stderr));
    }
    if stderr.lines().next() != Some(first.as_str()) {
        return Some(format!("{}: expected the error\n{}\ngot:\n{}", name, first, stderr));
    }
    if expected.len() > 1 {
        let reported: Vec<&str> = stderr.lines().filter(|line| line.contains(": error: ")).collect();
        if reported != expected {
            return Some(format!(
                "{}: expected these mistakes and no others\n{}\ngot:\n{}",
                name,
                expected.join("\n"),
                reported.join("\n")
            ));
        }
    }
    None
}

fn diagnostic_line(source: &Path, expected: &str) -> String {
    diagnostic_line_of(source, expected, "error")
}

fn diagnostic_line_of(source: &Path, expected: &str, kind: &str) -> String {
    if let Some((file, rest)) = expected.split_once(".vela:")
        && !file.contains(' ')
    {
        let path = source.parent().unwrap().join(format!("{}.vela", file));
        let (location, message) = rest.split_once(": ").unwrap();
        return format!("{}:{}: {}: {}", path.display(), location, kind, message);
    }
    if let Some((location, message)) = expected.split_once(": ") {
        let is_location = location
            .split_once(':')
            .is_some_and(|(l, c)| {
                !l.is_empty() && !c.is_empty()
                    && l.chars().all(|ch| ch.is_ascii_digit())
                    && c.chars().all(|ch| ch.is_ascii_digit())
            });
        if is_location {
            return format!("{}:{}: {}: {}", source.display(), location, kind, message);
        }
    }
    format!("{}: {}: {}", source.display(), kind, expected)
}

fn check_warnings(source: &Path, stderr: &str) -> Option<String> {
    let text = fs::read_to_string(source).unwrap();
    let wanted: Vec<String> = text.lines().filter_map(|line| line.strip_prefix("; warning: ")).map(|warning| diagnostic_line_of(source, warning.trim(), "warning")).collect();
    let warned: Vec<String> = stderr.lines().filter(|line| line.contains(": warning: ")).map(str::to_string).collect();
    if warned == wanted {
        return None;
    }
    Some(format!("{}: expected the warnings\n{}\ngot:\n{}", stem(source), wanted.join("\n"), warned.join("\n")))
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
        .filter_map(|src| check_run(src, &src.with_extension("out"), None, &scratch))
        .collect();
    let _ = fs::remove_dir_all(&scratch);
    report("tests/run", failures);
}

fn trap_message(source: &Path) -> Option<String> {
    let text = fs::read_to_string(source).unwrap();
    text.lines().next()?.strip_prefix("; trap: ").map(|message| message.trim().to_string())
}

#[test]
fn trapping_programs() {
    let dir = project_root().join("tests/trap");
    let scratch = scratch_dir("trap");
    let failures = vela_files(&dir)
        .iter()
        .filter_map(|src| match trap_message(src) {
            Some(message) => check_run(src, &src.with_extension("out"), Some(&message), &scratch),
            None => Some(format!("{}: first line must be '; trap: MESSAGE'", stem(src))),
        })
        .collect();
    let _ = fs::remove_dir_all(&scratch);
    report("tests/trap", failures);
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

fn html_text(html: &str) -> String {
    let mut text = String::new();
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        text.push_str(&rest[..open]);
        rest = rest[open..].split_once('>').map_or("", |(_, after)| after);
    }
    text.push_str(rest);
    text.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#x27;", "'").replace("&amp;", "&")
}

fn doc_programs(page: &str) -> Vec<(String, String, String)> {
    const PROGRAM: &str = "<pre class=\"program\" data-file=\"";
    const OUTPUT: &str = "<pre class=\"output\" aria-label=\"Output\">";
    let mut programs = Vec::new();
    let mut rest = page;
    while let Some(start) = rest.find(PROGRAM) {
        rest = &rest[start + PROGRAM.len()..];
        let (name, after) = rest.split_once("\">").unwrap();
        let (code, after) = after.split_once("</pre>").unwrap();
        let output = after.trim_start().strip_prefix(OUTPUT).map(|o| o.split_once("</pre>").unwrap().0).unwrap_or("");
        programs.push((name.to_string(), html_text(code) + "\n", html_text(output) + "\n"));
        rest = after;
    }
    programs
}

#[test]
fn docs_programs() {
    let page = fs::read_to_string(project_root().join("docs/regions.html")).unwrap();
    let programs = doc_programs(&page);
    assert!(programs.len() >= 10, "found only {} programs in docs/regions.html", programs.len());
    let scratch = scratch_dir("docs");
    let failures = programs
        .iter()
        .filter_map(|(name, code, output)| {
            let source = scratch.join(name);
            let expected = source.with_extension("out");
            fs::write(&source, code).unwrap();
            fs::write(&expected, output).unwrap();
            check_run(&source, &expected, None, &scratch).map(|failure| format!("docs/regions.html, {}", failure))
        })
        .collect();
    let _ = fs::remove_dir_all(&scratch);
    report("docs/regions.html", failures);
}

fn guide_programs(page: &str) -> Vec<(String, Option<String>)> {
    let mut programs = Vec::new();
    let mut rest = page;
    while let Some(start) = rest.find("<pre") {
        rest = rest[start..].split_once('>').map_or("", |(_, after)| after);
        let Some((code, after)) = rest.split_once("</pre>") else { break };
        rest = after;
        let code = html_text(code) + "\n";
        if !code.lines().any(|line| line == "task main()") {
            continue;
        }
        // A line the page marks with `; error:` is one it shows being refused;
        // the message is cut short there with an ellipsis.
        let wanted = code
            .lines()
            .find_map(|line| line.split_once("; error: "))
            .map(|(_, message)| message.trim().trim_end_matches('…').trim().to_string());
        programs.push((code, wanted));
    }
    programs
}

#[test]
fn docs_guide_programs() {
    let page = fs::read_to_string(project_root().join("docs/vela.html")).unwrap();
    let programs = guide_programs(&page);
    assert!(programs.len() >= 15, "found only {} whole programs in docs/vela.html", programs.len());
    let scratch = scratch_dir("guide");
    let mut failures = Vec::new();
    for (i, (code, wanted)) in programs.iter().enumerate() {
        // A program that includes a module needs the module's file, which the
        // page shows separately; `tests/run/modules.vela` covers those. Vela's
        // library needs none.
        if code.lines().any(|line| line.trim_start().starts_with("include ") && !line.trim_start().starts_with("include std/")) {
            continue;
        }
        let source = scratch.join(format!("guide{:02}.vela", i));
        fs::write(&source, code).unwrap();
        match wanted {
            None => failures.extend(check_strict_c(&source, &scratch)),
            Some(wanted) => {
                let out = Command::new(VELA).arg(&source).arg("--emit-c").output().unwrap();
                let printed = String::from_utf8_lossy(&out.stderr).to_string();
                // Only the message itself, since the lines under it quote the
                // source, comment and all, and would match anything the page says.
                let said = printed.lines().next().unwrap_or_default().split_once(": error: ").map(|(_, message)| message);
                if out.status.success() {
                    failures.push(format!("guide{:02}: the page shows this refused, but it compiled", i));
                } else if !said.is_some_and(|said| said.contains(wanted.as_str())) {
                    failures.push(format!("guide{:02}: the page says\n{}\ngot:\n{}", i, wanted, printed));
                }
            }
        }
    }
    let _ = fs::remove_dir_all(&scratch);
    report("docs/vela.html", failures);
}

#[test]
fn docs_generated_c() {
    let page = fs::read_to_string(project_root().join("docs/regions.html")).unwrap();
    let (_, code, _) = doc_programs(&page).into_iter().find(|(name, _, _)| name == "01_first.vela").unwrap();
    let listing = page.split_once("The generated C for Lesson 1's").unwrap().1;
    let listing = html_text(listing.split_once("<pre>").unwrap().1.split_once("</pre>").unwrap().0);
    let scratch = scratch_dir("docs-c");
    let source = scratch.join("01_first.vela");
    fs::write(&source, code).unwrap();
    let out = Command::new(VELA).arg(&source).arg("--emit-c").output().unwrap();
    let _ = fs::remove_dir_all(&scratch);
    let c = String::from_utf8(out.stdout).unwrap();
    let main = c.split_once("void vela_main(void) {").map(|(_, body)| format!("void vela_main(void) {{{}\n}}", body.split_once("\n}").unwrap().0));
    assert_eq!(main.as_deref(), Some(listing.as_str()), "docs/regions.html shows C that the compiler no longer generates");
}

#[test]
fn examples() {
    let root = project_root();
    let scratch = scratch_dir("examples");
    let failures = vela_files(&root.join("examples"))
        .iter()
        .filter_map(|src| {
            let expected = root.join("tests/examples").join(format!("{}.out", stem(src)));
            check_run(src, &expected, None, &scratch)
        })
        .collect();
    let _ = fs::remove_dir_all(&scratch);
    report("examples", failures);
}
