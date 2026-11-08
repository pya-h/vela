use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process;

use vela::driver;

const USAGE: &str = "usage: vela <input.vela> [-o <output>] [--emit-c | --check | --quick] [--no-notices] [--repeat-warnings] [--]\n       vela --help | --version";
const VERSION: &str = concat!(env!("CARGO_PKG_NAME"), " ", env!("CARGO_PKG_VERSION"));

enum Action {
    Help,
    Version,
    Run(Options),
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

fn main() {
    let opts = match parse_args(env::args().skip(1)) {
        Ok(Action::Run(opts)) => opts,
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
            return;
        }
        Err(msg) => {
            eprintln!("error: {}\n{}", msg, USAGE);
            process::exit(2);
        }
    };
    if let Err(msg) = run(&opts) {
        eprintln!("{}", msg);
        process::exit(1);
    }
}

// `--help` wins at once, while `--version` still validates the remaining flags first.
fn parse_args(args: impl Iterator<Item = String>) -> Result<Action, String> {
    let mut input = None;
    let mut output = None;
    let mut emit_c = false;
    let mut check = false;
    let mut quick = false;
    let mut no_notices = false;
    let mut repeat_warnings = false;
    let mut version = false;
    let mut only_files = false;
    let mut args = args;
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
    Ok(Action::Run(Options {
        input,
        output,
        emit_c,
        check,
        quick,
        no_notices,
        repeat_warnings,
    }))
}

fn run(opts: &Options) -> Result<(), String> {
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
