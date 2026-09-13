use std::env;
use std::fs;
use std::path::Path;
use std::process::{self, Command};

use vela::ast::Item;
use vela::check::Checker;
use vela::codegen::Generator;
use vela::error::{Error, Result};
use vela::lexer::Lexer;
use vela::parser::Parser;

const RUNTIME_H: &str = include_str!("../runtime/vela_rt.h");
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
fn parse_args(args: impl Iterator<Item = String>) -> std::result::Result<Option<Options>, String> {
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

fn run(opts: &Options) -> std::result::Result<(), String> {
    let source = fs::read_to_string(&opts.input)
        .map_err(|e| format!("error: cannot read '{}': {}", opts.input, e))?;
    let report = |e: Error| render_error(&opts.input, &source, &e);

    let items = front_end(&source).map_err(report)?;
    let has_main = items.iter().any(|item| matches!(item, Item::Task(t) if t.name == "main"));
    if !has_main && !opts.emit_c {
        return Err(format!("{}: error: program has no 'main' task", opts.input));
    }
    let c_code = Generator::new().generate(&items).map_err(report)?;

    if opts.emit_c {
        print!("{}", c_code);
        return Ok(());
    }

    compile_c(&c_code, &opts.input, &opts.output)?;
    println!("compiled: {}", opts.output);
    Ok(())
}

/// Lexes, parses, and type-checks a source file.
fn front_end(source: &str) -> Result<Vec<Item>> {
    let (tokens, spans) = Lexer::new(source).tokenize()?;
    let mut items = Parser::new(tokens, spans).parse_file()?;
    Checker::new().check_file(&mut items)?;
    Ok(items)
}

/// Formats a diagnostic as `path:line:col: error: message`, followed by the
/// offending source line with a caret under the error column.
fn render_error(path: &str, source: &str, e: &Error) -> String {
    let Some(span) = e.span else {
        return format!("{}: error: {}", path, e.message);
    };
    let mut out = format!("{}:{}:{}: error: {}", path, span.line, span.col, e.message);
    if let Some(line) = span.line.checked_sub(1).and_then(|i| source.lines().nth(i)) {
        let gutter = span.line.to_string();
        // Keep tabs so the caret lines up with the source line.
        let indent: String = line
            .chars()
            .take(span.col.saturating_sub(1))
            .map(|c| if c == '\t' { '\t' } else { ' ' })
            .collect();
        out.push_str(&format!(
            "\n {} | {}\n {} | {}^",
            gutter,
            line,
            " ".repeat(gutter.len()),
            indent
        ));
    }
    out
}

fn compile_c(c_code: &str, input_path: &str, output: &str) -> std::result::Result<(), String> {
    let base = Path::new(input_path).file_stem().unwrap_or("vela_out".as_ref());
    // A per-process build directory keeps concurrent compilations from
    // overwriting each other's generated files.
    let build_dir = std::env::temp_dir().join(format!("vela-build-{}", process::id()));
    fs::create_dir_all(&build_dir)
        .map_err(|e| format!("error: cannot create build directory: {}", e))?;
    let c_path = build_dir.join(format!("{}.c", base.to_string_lossy()));
    let h_path = build_dir.join("vela_rt.h");

    fs::write(&c_path, c_code)
        .map_err(|e| format!("error: cannot write C file: {}", e))?;
    fs::write(&h_path, RUNTIME_H)
        .map_err(|e| format!("error: cannot write runtime header: {}", e))?;

    // Like make and cargo, honor $CC to pick the C compiler.
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let result = Command::new(&cc)
        .arg("-std=c11")
        .arg("-O2")
        .arg("-I")
        .arg(&build_dir)
        .arg(&c_path)
        .arg("-o")
        .arg(output)
        .arg("-lm") // math library, for vela__rt__fmod
        .output()
        .map_err(|e| format!("error: failed to invoke C compiler '{}': {}", cc, e))?;

    // The C compiler's messages are about generated code, so they are shown
    // only when compilation fails; warnings on success would just be noise.
    if !result.status.success() {
        return Err(format!(
            "{}{}error: C compilation failed (generated C kept in {})",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr),
            build_dir.display()
        ));
    }
    let _ = fs::remove_dir_all(&build_dir);
    Ok(())
}
