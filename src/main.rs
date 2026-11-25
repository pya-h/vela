use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use vela::driver;

const USAGE: &str = "usage: vela <input.vela> [-o <output>] [--emit-c | --check | --quick] [--no-notices] [--repeat-warnings] [--]\n       vela run [--release | --quick] [--no-notices] [--repeat-warnings] <input.vela> [--] [args...]\n       vela test [<path>] [--filter <pattern>] [--release | --quick] [--no-notices] [--repeat-warnings]\n       vela lsp\n       vela --help | --version";
const VERSION: &str = concat!(env!("CARGO_PKG_NAME"), " ", env!("CARGO_PKG_VERSION"));

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

enum Action {
    Help,
    Version,
    Lsp,
    Build(Options),
    Run(RunOptions),
    Test(TestOptions),
}

struct Options {
    input: String,
    output: Option<String>,
    emit_c: bool,
    check: bool,
    quick: bool,
    no_notices: bool,
    repeat_warnings: bool,
}

struct RunOptions {
    input: String,
    args: Vec<String>,
    quick: bool,
    no_notices: bool,
    repeat_warnings: bool,
}

struct TestOptions {
    path: String,
    filter: Option<String>,
    quick: bool,
    no_notices: bool,
    repeat_warnings: bool,
}

fn main() {
    match parse_args(env::args().skip(1)) {
        Ok(Action::Build(opts)) => {
            if let Err(msg) = build(&opts) {
                eprintln!("{}", msg);
                process::exit(1);
            }
        }
        Ok(Action::Run(opts)) => match run_program(&opts) {
            Ok(code) => {
                if code != 0 {
                    process::exit(code);
                }
            }
            Err(msg) => {
                eprintln!("{}", msg);
                process::exit(1);
            }
        },
        Ok(Action::Test(opts)) => match run_tests(&opts) {
            Ok(true) => {}
            Ok(false) => process::exit(1),
            Err(msg) => {
                eprintln!("{}", msg);
                process::exit(1);
            }
        },
        Ok(Action::Lsp) => {
            if let Err(msg) = run_lsp() {
                eprintln!("{}", msg);
                process::exit(1);
            }
        }
        Ok(action) => {
            let text = if matches!(action, Action::Version) {
                VERSION
            } else {
                USAGE
            };
            if let Err(msg) = write_stdout(&format!("{}\n", text)) {
                eprintln!("{}", msg);
                process::exit(1);
            }
        }
        Err(msg) => {
            eprintln!("error: {}\n{}", msg, USAGE);
            process::exit(2);
        }
    }
}

// `--help` wins at once, while `--version` still validates the remaining flags first.
fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Action, String> {
    let first = match args.next() {
        Some(arg) => arg,
        None => return Err("no input file".to_string()),
    };
    match first.as_str() {
        "lsp" => {
            return match args.next().as_deref() {
                None => Ok(Action::Lsp),
                Some("-h" | "--help") => Ok(Action::Help),
                Some(extra) => Err(format!("unknown lsp option '{}'", extra)),
            };
        }
        "run" => return parse_run_args(args),
        "test" => return parse_test_args(args),
        _ => {}
    }

    let mut input = None;
    let mut output = None;
    let mut emit_c = false;
    let mut check = false;
    let mut quick = false;
    let mut no_notices = false;
    let mut repeat_warnings = false;
    let mut version = false;
    let mut only_files = false;
    let mut args = std::iter::once(first).chain(args);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--" if !only_files => only_files = true,
            _ if only_files => {
                if input.is_some() {
                    return Err("only one input file is allowed".to_string());
                }
                input = Some(arg);
            }
            "-h" | "--help" => return Ok(Action::Help),
            "--version" => version = true,
            "--emit-c" => emit_c = true,
            "--check" => check = true,
            "--quick" => quick = true,
            "--no-notices" => no_notices = true,
            "--repeat-warnings" => repeat_warnings = true,
            "-o" => match args.next() {
                Some(path) if output.is_none() => output = Some(path),
                Some(_) => return Err("-o given more than once".to_string()),
                None => return Err("-o requires an output path".to_string()),
            },
            flag if flag.starts_with('-') && flag != "-" => {
                return Err(format!("unknown option '{}'", flag));
            }
            _ if input.is_some() => return Err("only one input file is allowed".to_string()),
            _ => input = Some(arg),
        }
    }
    if version {
        return Ok(Action::Version);
    }
    if emit_c && check {
        return Err("--emit-c and --check can't be used together".to_string());
    }
    if quick && (emit_c || check) {
        let other = if emit_c { "--emit-c" } else { "--check" };
        return Err(format!(
            "--quick builds an executable faster, and {} builds none, so they can't be used together",
            other
        ));
    }
    if no_notices && check {
        return Err("--no-notices changes the program built, and --check builds none, so they can't be used together".to_string());
    }
    let input = input.ok_or_else(|| "no input file".to_string())?;
    Ok(Action::Build(Options {
        input,
        output,
        emit_c,
        check,
        quick,
        no_notices,
        repeat_warnings,
    }))
}

fn parse_run_args(mut args: impl Iterator<Item = String>) -> Result<Action, String> {
    let mut quick = true;
    let mut saw_quick = false;
    let mut saw_release = false;
    let mut no_notices = false;
    let mut repeat_warnings = false;
    let mut only_file = false;
    let mut input = None;
    for arg in &mut args {
        match arg.as_str() {
            "--" if !only_file => only_file = true,
            _ if only_file => {
                input = Some(arg);
                break;
            }
            "-h" | "--help" => return Ok(Action::Help),
            "--quick" => {
                quick = true;
                saw_quick = true;
            }
            "--release" => {
                quick = false;
                saw_release = true;
            }
            "--no-notices" => no_notices = true,
            "--repeat-warnings" => repeat_warnings = true,
            flag if flag.starts_with('-') && flag != "-" => {
                return Err(format!("unknown run option '{}'", flag));
            }
            _ => {
                input = Some(arg);
                break;
            }
        }
    }
    if saw_quick && saw_release {
        return Err("--quick and --release can't be used together".to_string());
    }
    let input = input.ok_or_else(|| "no input file".to_string())?;
    let mut forwarded: Vec<String> = args.collect();
    if forwarded.first().map(String::as_str) == Some("--") {
        forwarded.remove(0);
    }
    Ok(Action::Run(RunOptions {
        input,
        args: forwarded,
        quick,
        no_notices,
        repeat_warnings,
    }))
}

fn parse_test_args(mut args: impl Iterator<Item = String>) -> Result<Action, String> {
    let mut path = None;
    let mut filter = None;
    let mut quick = true;
    let mut saw_quick = false;
    let mut saw_release = false;
    let mut no_notices = false;
    let mut repeat_warnings = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Action::Help),
            "--quick" => {
                quick = true;
                saw_quick = true;
            }
            "--release" => {
                quick = false;
                saw_release = true;
            }
            "--no-notices" => no_notices = true,
            "--repeat-warnings" => repeat_warnings = true,
            "--filter" => match args.next() {
                Some(pattern) if filter.is_none() => filter = Some(pattern),
                Some(_) => return Err("--filter given more than once".to_string()),
                None => return Err("--filter requires a pattern".to_string()),
            },
            flag if flag.starts_with('-') => {
                return Err(format!("unknown test option '{}'", flag));
            }
            _ if path.is_some() => return Err("only one test path is allowed".to_string()),
            _ => path = Some(arg),
        }
    }
    if saw_quick && saw_release {
        return Err("--quick and --release can't be used together".to_string());
    }
    Ok(Action::Test(TestOptions {
        path: path.unwrap_or_else(|| ".".to_string()),
        filter,
        quick,
        no_notices,
        repeat_warnings,
    }))
}

fn temp_exe(prefix: &str) -> PathBuf {
    let seq = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = if cfg!(windows) {
        format!("{}-{}-{}.exe", prefix, process::id(), seq)
    } else {
        format!("{}-{}-{}", prefix, process::id(), seq)
    };
    env::temp_dir().join(name)
}

fn cc_for(quick: bool) -> String {
    let cc = driver::c_compiler_from_env();
    // Placed after `$CC` words so `-O0` wins over any `-O` flag in `CC`.
    if quick {
        format!("{} -O0", cc)
    } else {
        cc
    }
}

fn run_program(opts: &RunOptions) -> Result<i32, String> {
    let source = fs::read_to_string(&opts.input)
        .map_err(|e| format!("error: cannot read '{}': {}", opts.input, e))?;
    let exe = temp_exe("vela-run");
    let cc = cc_for(opts.quick);
    let warnings = driver::build(
        &source,
        Path::new(&opts.input),
        &exe,
        &cc,
        !opts.no_notices,
    )
    .map_err(|e| {
        let _ = fs::remove_file(&exe);
        e.render(&opts.input, &source)
    })?;
    warn(&warnings, &opts.input, opts.repeat_warnings);

    let status = process::Command::new(&exe)
        .args(&opts.args)
        .status()
        .map_err(|e| {
            let _ = fs::remove_file(&exe);
            format!("error: failed to run '{}': {}", opts.input, e)
        })?;
    let _ = fs::remove_file(&exe);
    Ok(status.code().unwrap_or(1))
}

fn collect_test_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    if root.is_file() {
        return Ok(vec![root.to_path_buf()]);
    }
    if !root.is_dir() {
        return Err(format!("error: cannot read '{}': no such file or directory", root.display()));
    }
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
        let entries = fs::read_dir(dir)
            .map_err(|e| format!("error: cannot read '{}': {}", dir.display(), e))?;
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            if path.is_dir() {
                let skip = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with('.') || n == "target");
                if !skip {
                    walk(&path, out)?;
                }
            } else if path.extension().and_then(|e| e.to_str()) == Some("vela")
                && let Ok(text) = fs::read_to_string(&path)
                && text.lines().any(|line| line.trim() == "#test")
            {
                out.push(path);
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, &mut out)?;
    Ok(out)
}

fn run_tests(opts: &TestOptions) -> Result<bool, String> {
    let files = collect_test_files(Path::new(&opts.path))?;
    if files.is_empty() {
        write_stdout("running 0 tests\ntest result: ok. 0 passed; 0 failed\n")?;
        return Ok(true);
    }

    let cc = cc_for(opts.quick);
    let mut compiled: Vec<(PathBuf, String, PathBuf, Vec<String>)> = Vec::new();
    let mut total = 0usize;

    for file in files {
        let display = file.display().to_string();
        let source = fs::read_to_string(&file)
            .map_err(|e| format!("error: cannot read '{}': {}", display, e))?;
        let exe = temp_exe("vela-test");
        let warnings = driver::build_tests(&source, &file, &exe, &cc, !opts.no_notices)
            .map_err(|e| {
                let _ = fs::remove_file(&exe);
                e.render(&display, &source)
            })?;
        warn(&warnings, &display, opts.repeat_warnings);

        let list_out = process::Command::new(&exe)
            .arg("--list")
            .output()
            .map_err(|e| {
                let _ = fs::remove_file(&exe);
                format!("error: failed to list tests in '{}': {}", display, e)
            })?;
        if !list_out.status.success() {
            let _ = fs::remove_file(&exe);
            return Err(format!("error: failed to list tests in '{}'", display));
        }
        let names: Vec<String> = String::from_utf8_lossy(&list_out.stdout)
            .lines()
            .map(str::to_string)
            .collect();
        total += names
            .iter()
            .filter(|name| opts.filter.as_deref().is_none_or(|f| name.contains(f)))
            .count();
        compiled.push((file, display, exe, names));
    }

    let noun = if total == 1 { "test" } else { "tests" };
    write_stdout(&format!("\nrunning {} {}\n", total, noun))?;

    let mut passed = 0usize;
    let mut failed = 0usize;
    let multi_file = compiled.len() > 1;

    for (_file, display, exe, names) in &compiled {
        for (idx, name) in names.iter().enumerate() {
            if let Some(filter) = opts.filter.as_deref()
                && !name.contains(filter)
            {
                continue;
            }
            let label = if multi_file {
                format!("{}::{}", display, name)
            } else {
                name.clone()
            };
            let out = process::Command::new(exe)
                .arg(format!("--run={}", idx))
                .output()
                .map_err(|e| format!("error: failed to run test '{}': {}", label, e))?;
            if out.status.success() {
                passed += 1;
                write_stdout(&format!("test {} ... ok\n", label))?;
            } else {
                failed += 1;
                write_stdout(&format!("test {} ... FAILED\n", label))?;
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                if !stdout.is_empty() {
                    let _ = write_stdout(&stdout);
                }
                if !stderr.is_empty() {
                    eprint!("{}", stderr);
                }
            }
        }
    }

    for (_, _, exe, _) in compiled {
        let _ = fs::remove_file(exe);
    }

    let status = if failed == 0 { "ok" } else { "FAILED" };
    write_stdout(&format!(
        "\ntest result: {}. {} passed; {} failed\n",
        status, passed, failed
    ))?;
    Ok(failed == 0)
}

fn build(opts: &Options) -> Result<(), String> {
    let source = fs::read_to_string(&opts.input)
        .map_err(|e| format!("error: cannot read '{}': {}", opts.input, e))?;

    let path = Path::new(&opts.input);
    if opts.check {
        let (items, warnings) = driver::check_program(path, &source, &mut driver::read_from_disk)
            .map_err(|e| e.render(&opts.input))?;
        if !driver::has_main(&items) {
            return Err(driver::BuildError::NoMain.render(&opts.input, &source));
        }
        warn(&warnings, &opts.input, opts.repeat_warnings);
        return write_stdout(&format!("checked: {}\n", opts.input));
    }

    if opts.emit_c {
        return match &opts.output {
            Some(output) => {
                let (c_code, warnings) = driver::generate_c_file(
                    path,
                    path,
                    &source,
                    Path::new(output),
                    &mut driver::read_from_disk,
                    !opts.no_notices,
                )
                .map_err(|e| e.render(&opts.input, &source))?;
                warn(&warnings, &opts.input, opts.repeat_warnings);
                fs::write(output, c_code)
                    .map_err(|e| format!("error: cannot write '{}': {}", output, e))
            }
            None => {
                let (c_code, warnings) = driver::generate_c_program(
                    path,
                    &source,
                    &mut driver::read_from_disk,
                    !opts.no_notices,
                )
                .map_err(|e| e.render(&opts.input))?;
                warn(&warnings, &opts.input, opts.repeat_warnings);
                write_stdout(&c_code)
            }
        };
    }

    let output = opts.output.as_deref().unwrap_or("a.out");
    let cc = driver::c_compiler_from_env();
    // Placed after `$CC` words so `-O0` wins over any `-O` flag in `CC`.
    let cc = if opts.quick {
        format!("{} -O0", cc)
    } else {
        cc
    };
    let warnings = driver::build(
        &source,
        Path::new(&opts.input),
        Path::new(output),
        &cc,
        !opts.no_notices,
    )
    .map_err(|e| e.render(&opts.input, &source))?;
    warn(&warnings, &opts.input, opts.repeat_warnings);
    write_stdout(&format!("compiled: {}\n", output))
}

fn warn(warnings: &driver::Warnings, input: &str, repeat: bool) {
    let rendered = if repeat {
        warnings.render_repeating(input)
    } else {
        warnings.render(input)
    };
    if !rendered.is_empty() {
        eprintln!("{}", rendered);
    }
}

fn write_stdout(text: &str) -> Result<(), String> {
    let mut stdout = io::stdout().lock();
    match stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Err(e) if e.kind() != io::ErrorKind::BrokenPipe => {
            Err(format!("error: cannot write output: {}", e))
        }
        _ => Ok(()),
    }
}

fn run_lsp() -> Result<(), String> {
    let current_exe = env::current_exe().ok();
    let sibling_lsp = current_exe
        .as_ref()
        .and_then(|p| p.parent())
        .map(|dir| dir.join("vela-lsp"));
    let exe = if let Some(ref sibling) = sibling_lsp
        && sibling.is_file()
    {
        sibling.clone()
    } else {
        std::path::PathBuf::from("vela-lsp")
    };

    let status = process::Command::new(&exe)
        .status()
        .map_err(|_| "error: 'vela-lsp' not found. Build or install it with:\n    cargo install --path ide/lsp".to_string())?;

    if status.success() {
        Ok(())
    } else {
        process::exit(status.code().unwrap_or(1));
    }
}
