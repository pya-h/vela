use std::env;
use std::fs;
use std::path::Path;
use std::process;

use vela::driver;

const USAGE: &str = "usage: vela <input.vela> [-o <output>] [--emit-c]";

struct Options {
    input: String,
    output: String,
    emit_c: bool,
}

fn main() {
    let opts = match parse_args(env::args().skip(1)) {
        Ok(Some(opts)) => opts,
        Ok(None) => {
            println!("{}", USAGE);
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

/// Returns `Ok(None)` when help was requested.
fn parse_args(args: impl Iterator<Item = String>) -> Result<Option<Options>, String> {
    let mut input = None;
    let mut output = None;
    let mut emit_c = false;
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--emit-c" => emit_c = true,
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
    let input = input.ok_or_else(|| "no input file".to_string())?;
    Ok(Some(Options {
        input,
        output: output.unwrap_or_else(|| "a.out".to_string()),
        emit_c,
    }))
}

fn run(opts: &Options) -> Result<(), String> {
    let source = fs::read_to_string(&opts.input)
        .map_err(|e| format!("error: cannot read '{}': {}", opts.input, e))?;

    if opts.emit_c {
        let c_code = driver::generate_c(&source).map_err(|e| driver::render_error(&opts.input, &source, &e))?;
        print!("{}", c_code);
        return Ok(());
    }

    let cc = driver::c_compiler_from_env();
    driver::build(&source, Path::new(&opts.input), Path::new(&opts.output), &cc)
        .map_err(|e| e.render(&opts.input, &source))?;
    println!("compiled: {}", opts.output);
    Ok(())
}
