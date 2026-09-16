use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process;

use vela::driver;

const USAGE: &str = "usage: vela <input.vela> [-o <output>] [--emit-c | --check] [--]\n       vela --help | --version";
const VERSION: &str = concat!(env!("CARGO_PKG_NAME"), " ", env!("CARGO_PKG_VERSION"));

/// What the command line asks for.
enum Action {
    Help,
    Version,
    Run(Options),
}

struct Options {
    input: String,
    /// The executable, or with `--emit-c` the C file (standard output if absent).
    output: Option<String>,
    emit_c: bool,
    /// Only parse and type-check; do not generate C or build.
    check: bool,
}

fn main() {
    let opts = match parse_args(env::args().skip(1)) {
        Ok(Action::Run(opts)) => opts,
        Ok(action) => {
            let text = if matches!(action, Action::Version) { VERSION } else { USAGE };
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

/// Reads the command line. `--help` wins at once; `--version` wins once the
/// rest of the arguments are valid, so a wrong argument is still an error.
fn parse_args(args: impl Iterator<Item = String>) -> Result<Action, String> {
    let mut input = None;
    let mut output = None;
    let mut emit_c = false;
    let mut check = false;
    let mut version = false;
    let mut only_files = false;
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            // After `--`, every argument is a file, even one starting with `-`.
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
    let input = input.ok_or_else(|| "no input file".to_string())?;
    Ok(Action::Run(Options { input, output, emit_c, check }))
}

fn run(opts: &Options) -> Result<(), String> {
    let source = fs::read_to_string(&opts.input)
        .map_err(|e| format!("error: cannot read '{}': {}", opts.input, e))?;

    let path = Path::new(&opts.input);
    if opts.check {
        // Everything a build checks before generating C, so a program that
        // passes can be built.
        let items = driver::check_program(path, &source, &mut driver::read_from_disk).map_err(|e| e.render(&opts.input))?;
        if !driver::has_main(&items) {
            return Err(driver::BuildError::NoMain.render(&opts.input, &source));
        }
        return write_stdout(&format!("checked: {}\n", opts.input));
    }

    if opts.emit_c {
        let c_code = driver::generate_c_program(path, &source, &mut driver::read_from_disk).map_err(|e| e.render(&opts.input))?;
        return match &opts.output {
            Some(path) if fs::canonicalize(path).ok() == fs::canonicalize(&opts.input).ok() => {
                Err(format!("error: output '{}' would overwrite the input file", path))
            }
            Some(path) => fs::write(path, c_code).map_err(|e| format!("error: cannot write '{}': {}", path, e)),
            None => write_stdout(&c_code),
        };
    }

    let output = opts.output.as_deref().unwrap_or("a.out");
    let cc = driver::c_compiler_from_env();
    driver::build(&source, Path::new(&opts.input), Path::new(output), &cc)
        .map_err(|e| e.render(&opts.input, &source))?;
    write_stdout(&format!("compiled: {}\n", output))
}

/// Writes to standard output. A closed pipe (`vela x.vela --emit-c | head`)
/// just ends the output; other failures are errors, not panics.
fn write_stdout(text: &str) -> Result<(), String> {
    let mut stdout = io::stdout().lock();
    match stdout.write_all(text.as_bytes()).and_then(|()| stdout.flush()) {
        Err(e) if e.kind() != io::ErrorKind::BrokenPipe => Err(format!("error: cannot write output: {}", e)),
        _ => Ok(()),
    }
}
