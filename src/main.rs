use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

use vela::ast::Item;
use vela::check::Checker;
use vela::codegen::Generator;
use vela::error::{Error, Result};
use vela::lexer::Lexer;
use vela::parser::Parser;

const RUNTIME_H: &str = include_str!("../runtime/vela_rt.h");

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        return Err(Error::new(0, 0, "usage: vela <input.vela> [-o output]"));
    }

    let input_path = &args[1];
    let output = args.windows(2)
        .find(|w| w[0] == "-o")
        .map(|w| w[1].clone())
        .unwrap_or_else(|| "a.out".to_string());

    let source = fs::read_to_string(input_path)
        .map_err(|e| Error::new(0, 0, format!("cannot read '{}': {}", input_path, e)))?;

    let (tokens, lines, cols) = Lexer::new(&source).tokenize()?;
    let mut parser = Parser::new(tokens, lines, cols);
    let items = parser.parse_file()?;

    let mut checker = Checker::new();
    checker.check_file(&items)?;

    if args.contains(&"--emit-c".to_string()) {
        let c_code = generate_c(&items)?;
        println!("{}", c_code);
        return Ok(());
    }

    compile_to_executable(&items, input_path, &output)?;
    println!("compiled: {}", output);
    Ok(())
}

fn generate_c(items: &[Item]) -> Result<String> {
    let mut generator = Generator::new();
    generator.generate(items)
}

fn compile_to_executable(items: &[Item], input_path: &str, output: &str) -> Result<()> {
    let base = Path::new(input_path).file_stem().unwrap_or("vela_out".as_ref());
    let tmp_dir = std::env::temp_dir();
    let c_path = tmp_dir.join(format!("{}.c", base.to_string_lossy()));
    let h_path = tmp_dir.join("vela_rt.h");

    let c_code = generate_c(items)?;
    fs::write(&c_path, c_code)
        .map_err(|e| Error::new(0, 0, format!("cannot write C file: {}", e)))?;
    fs::write(&h_path, RUNTIME_H)
        .map_err(|e| Error::new(0, 0, format!("cannot write runtime header: {}", e)))?;

    let status = Command::new("cc")
        .arg("-std=c11")
        .arg("-O2")
        .arg("-I")
        .arg(&tmp_dir)
        .arg(&c_path)
        .arg("-o")
        .arg(output)
        .status()
        .map_err(|e| Error::new(0, 0, format!("failed to invoke C compiler: {}", e)))?;

    if !status.success() {
        return Err(Error::new(0, 0, "C compilation failed"));
    }
    Ok(())
}
