//! `vela::driver`, the compile pipeline shared by the `vela` command and
//! other tools: it must report every failure as a structured error, rendered
//! exactly as the command prints it.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use vela::ast::{Item, Span, StmtKind, Type};
use vela::driver::{self, BuildError};

/// A fresh directory for one test; removed when the test passes.
struct TempDir(PathBuf);

impl TempDir {
    fn new(test: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!("vela-driver-{}-{}", std::process::id(), test));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn write(&self, name: &str, source: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, source).unwrap();
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

const HELLO: &str = "task main()\n    show(\"Hello, Vela!\")\nfi\n";

fn cc() -> String {
    driver::c_compiler_from_env()
}

#[test]
fn builds_a_program_that_runs() {
    let dir = TempDir::new("hello");
    let input = dir.write("hello.vela", HELLO);
    let exe = dir.0.join("hello");
    driver::build(HELLO, &input, &exe, &cc()).unwrap();
    let run = Command::new(&exe).output().unwrap();
    assert!(run.status.success());
    assert_eq!(String::from_utf8_lossy(&run.stdout), "Hello, Vela!\n");
}

#[test]
fn invalid_programs_report_the_vela_error() {
    let source = "task main()\n    show(missing)\nfi\n";
    let err = driver::build(source, Path::new("bad.vela"), Path::new("unused-output"), &cc()).unwrap_err();
    let BuildError::Vela(driver::ProgramError { error, .. }) = &err else { panic!("expected a Vela error, got {:?}", err) };
    assert_eq!(error.span, Some(Span::new(2, 10)));
    assert_eq!(error.message, "unknown variable 'missing'");
    assert_eq!(
        err.render("bad.vela", source),
        "bad.vela:2:10: error: unknown variable 'missing'\n 2 |     show(missing)\n   |          ^"
    );
    assert_eq!(driver::generate_c(source).unwrap_err(), *error);
}

#[test]
fn a_program_without_main_is_not_an_executable() {
    let source = "task helper()\n    show(1)\nfi\n";
    let err = driver::build(source, Path::new("lib.vela"), Path::new("unused-output"), &cc()).unwrap_err();
    assert!(matches!(err, BuildError::NoMain), "{:?}", err);
    assert_eq!(err.render("lib.vela", source), "lib.vela: error: program has no 'main' task");
    // Its C can still be generated.
    assert!(driver::generate_c(source).unwrap().contains("void vela_helper(void)"));
}

#[test]
fn the_output_cannot_be_the_input() {
    let dir = TempDir::new("overwrite");
    let input = dir.write("prog.vela", HELLO);
    let same = dir.0.join(".").join("prog.vela");
    let err = driver::build(HELLO, &input, &same, &cc()).unwrap_err();
    assert!(matches!(err, BuildError::OutputIsInput(_)), "{:?}", err);
    assert_eq!(fs::read_to_string(&input).unwrap(), HELLO);
}

#[test]
fn a_missing_c_compiler_is_reported() {
    let err = driver::build(HELLO, Path::new("hello.vela"), Path::new("unused-output"), "vela-no-such-cc --flag").unwrap_err();
    let BuildError::CompilerNotStarted { cc, .. } = &err else { panic!("{:?}", err) };
    assert_eq!(cc, "vela-no-such-cc");
    assert!(err.render("hello.vela", HELLO).starts_with("error: failed to invoke C compiler 'vela-no-such-cc': "));
}

#[test]
fn c_compiler_failures_keep_the_generated_c() {
    let dir = TempDir::new("c-failure");
    let input = dir.write("hello.vela", HELLO);
    // Makes the C compiler fail on otherwise valid generated C.
    let failing_cc = format!("{} -include vela-missing-header.h", cc());
    let err = driver::build(HELLO, &input, &dir.0.join("hello"), &failing_cc).unwrap_err();
    let BuildError::CFailed { messages, build_dir } = &err else { panic!("{:?}", err) };
    assert!(messages.contains("vela-missing-header.h"), "{}", messages);
    assert!(build_dir.join("hello.c").is_file() && build_dir.join("vela_rt.h").is_file());
    assert!(err.render("hello.vela", HELLO).ends_with(&format!("error: C compilation failed (generated C kept in {})", build_dir.display())));
    fs::remove_dir_all(build_dir).unwrap();
}

#[test]
fn partial_front_end_keeps_the_types_found_before_an_error() {
    let source = "task main()\n    bind x = 1.5\n    show(missing)\n    bind y = true\nfi\n";
    let partial = driver::front_end_partial(source);
    assert_eq!(partial.error, driver::front_end(source).err());
    let items = partial.items.unwrap();
    let Item::Task(main) = &items[0] else { panic!("expected a task") };
    let binding_type = |i: usize| match &main.body[i].kind {
        StmtKind::Binding { names, .. } => names.first().and_then(|bound| bound.ty.clone()),
        other => panic!("expected a binding, got {:?}", other),
    };
    // Checked before the error, and not after it.
    assert_eq!(binding_type(0), Some(Type::Float));
    assert_eq!(binding_type(2), None);

    // A program that doesn't parse has no items.
    let broken = driver::front_end_partial("task main(\n");
    assert!(broken.items.is_none() && broken.error.is_some());
}

#[test]
fn deep_nesting_is_an_error_not_a_crash() {
    // Test threads have small stacks (2 MiB), like the threads Vela Studio
    // compiles on, so a missing limit shows up here as a crash.
    let limit = vela::parser::MAX_NESTING;
    let program = |expr: String| format!("task main()\n    show({})\nfi\n", expr);
    let deep = limit + 10;
    let cases = [
        ("parentheses", program(format!("{}1{}", "(".repeat(deep), ")".repeat(deep)))),
        ("an operator chain", program(vec!["1"; deep].join(" + "))),
        ("unary operators", program(format!("{}1", "-".repeat(deep)))),
        ("calls", format!("task id(n: int) -> int\n    <- n\nfi\n{}", program(format!("{}1{}", "id(".repeat(deep), ")".repeat(deep))))),
        ("blocks", format!("task main()\n{}fi\n", "when true\n".repeat(deep) + &"fi\n".repeat(deep))),
        ("pointer types", format!("task f(p: {}int)\nfi\n", "^".repeat(deep))),
        ("maybe types", format!("task f(p: {}int)\nfi\n", "maybe ".repeat(deep))),
        ("plan B blocks", program(format!("{}1{}", "m {\n".repeat(deep), "\n}".repeat(deep)))),
        ("fail chains", format!("task f() -> int or text\n    <- {}\"x\"\nfi\n", "fail ".repeat(deep))),
        ("dereference chains", program(format!("p{}", "^".repeat(deep)))),
        ("field chains", program(format!("p{}", ".a".repeat(deep)))),
        ("nested matches", format!("task main()\n{}fi\n", "match k\ncase _\n".repeat(deep) + &"fi\n".repeat(deep))),
        ("nested loops", format!("task main()\n{}fi\n", "loop true\neach i in 0..1\n".repeat(deep) + &"fi\nfi\n".repeat(deep))),
        ("nested is conditions", format!("task main()\n{}fi\n", "when m is {v}\n".repeat(deep) + &"fi\n".repeat(deep))),
        ("nested new", program(format!("{}1{}", "new int(".repeat(deep), ") at r".repeat(deep)))),
        ("nested shape literals", program(format!("{}1{}", "P { x: ".repeat(deep), " }".repeat(deep)))),
        ("region pointer types", format!("task f(p: {}int@r)\nfi\n", "^".repeat(deep))),
    ];
    for (what, source) in cases {
        let error = driver::generate_c(&source).expect_err(what);
        assert!(error.message.contains("nested too deeply"), "{}: {}", what, error.message);
    }
    // Just below the limit still compiles.
    let near = limit - 5;
    let ok = program(format!("{}1{}", "(".repeat(near), ")".repeat(near)));
    driver::generate_c(&ok).unwrap();
    driver::generate_c(&program(vec!["1"; near].join(" + "))).unwrap();
    // Plan Bs nest three levels each (the postfix value, the block, and the
    // statement's expression inside it).
    let plan_bs = format!(
        "task main()\n    bind m: maybe int = none\n    show({}1{})\nfi\n",
        "m {\n".repeat(near / 3),
        "\n}".repeat(near / 3)
    );
    driver::generate_c(&plan_bs).unwrap();
    // A `new` counts two levels (itself, and its value).
    let news = format!(
        "task main()\n    region r\n    show({}new int(1) at r{}^)\nfi\n",
        "new int(".repeat(near / 2 - 1),
        "^) at r".repeat(near / 2 - 1)
    );
    driver::generate_c(&news).unwrap();
    let literals = format!("shape P\n    x: int\nfi\n{}", program(format!("{}1{}", "P { x: ".repeat(near), " }".repeat(near))));
    assert!(driver::generate_c(&literals).is_err_and(|e| !e.message.contains("nested too deeply")), "a deep literal reaches the checker");
    let fails = format!("task f() -> int or text\n    <- {}\"x\"\nfi\n", "fail ".repeat(near));
    assert!(driver::generate_c(&fails).is_err_and(|e| !e.message.contains("nested too deeply")), "a deep fail chain reaches the checker");
}

#[test]
fn long_chains_of_types_are_checked_without_a_crash() {
    // Each shape contains the next, by value or through a pointer, declared
    // before it: checking and ordering the types follows the whole chain.
    let count = 20_000;
    let chain = |field: &str, last: &str| {
        let shapes: String = (0..count).map(|i| format!("shape S{}\n    next: {}S{}\nfi\n", i, field, i + 1)).collect();
        format!("{}shape S{}\n    {}\nfi\ntask main()\nfi\n", shapes, count, last)
    };
    driver::generate_c(&chain("", "value: int")).unwrap();
    driver::generate_c(&chain("^", "value: int")).unwrap();
    let error = driver::generate_c(&chain("", "first: S0")).unwrap_err();
    assert!(error.message.starts_with("'S0' contains itself by value (S0 -> S1 -> "), "{}", error.message);
    let error = driver::generate_c(&chain("^", "first: ^S0")).unwrap_err();
    assert!(error.message.contains("can never be created"), "{}", error.message);
}

/// A loader over files held in memory, which records every path it is asked for.
fn memory_loader<'a>(files: &'a [(&'a str, &'a str)], asked: &'a std::cell::RefCell<Vec<String>>) -> impl FnMut(&Path) -> std::io::Result<String> + 'a {
    move |path: &Path| {
        let path = path.to_string_lossy().replace('\\', "/");
        asked.borrow_mut().push(path.clone());
        files
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, source)| source.to_string())
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))
    }
}

#[test]
fn a_program_loads_each_module_once_through_its_loader() {
    let main = "use shapes/geometry\nuse text/labels (label)\n\ntask main()\n    show(label(geometry.area(3)))\nfi\n";
    let files = [
        ("app/shapes/geometry.vela", "use ../text/labels\n\ntask area(side: int) -> int\n    <- labels.width(side) * side\nfi\n"),
        ("app/text/labels.vela", "task label(n: int) -> text\n    <- \"shape\"\nfi\n\ntask width(n: int) -> int\n    <- n\nfi\n"),
    ];
    let asked = std::cell::RefCell::new(Vec::new());
    let items = driver::check_program(Path::new("app/main.vela"), main, &mut memory_loader(&files, &asked)).unwrap();
    // Paths are relative to the file with the `use`, with `..` settled, and a
    // module used twice is read once.
    assert_eq!(*asked.borrow(), ["app/shapes/geometry.vela", "app/text/labels.vela"]);
    let names: Vec<&str> = items.iter().map(|item| match item {
        Item::Task(t) => t.name.as_str(),
        _ => "",
    }).collect();
    assert_eq!(names, ["labels.label", "labels.width", "geometry.area", "main"]);
    let c = driver::generate_c_program(Path::new("app/main.vela"), main, &mut memory_loader(&files, &asked)).unwrap();
    assert!(c.contains("int64_t vela_geometry__0__area(int64_t side)"), "{}", c);
}

#[test]
fn an_error_in_a_module_names_that_file() {
    let main = "use lib\n\ntask main()\n    show(lib.twice(2))\nfi\n";
    let files = [("lib.vela", "task twice(n: int) -> int\n    <- n * \"2\"\nfi\n")];
    let asked = std::cell::RefCell::new(Vec::new());
    let error = driver::check_program(Path::new("main.vela"), main, &mut memory_loader(&files, &asked)).unwrap_err();
    assert_eq!(error.error.span, Some(Span { line: 2, col: 10, file: 1 }));
    assert_eq!(error.error_file().as_deref(), Some("lib.vela"));
    assert_eq!(
        error.render("main.vela"),
        "lib.vela:2:10: error: arithmetic requires numeric operands\n 2 |     <- n * \"2\"\n   |          ^"
    );
    // A program in one file can't load modules, and says which is missing.
    let partial = driver::front_end_partial(main);
    assert_eq!(partial.error.unwrap().message, "can't find module 'lib': there is no file 'lib.vela'");
}

#[test]
fn long_chains_of_modules_are_an_error_not_a_crash() {
    // Module i uses module i + 1: loading follows the chain recursively, on a
    // test thread's small stack.
    let chain = |length: usize| -> Vec<(String, String)> {
        (1..=length)
            .map(|i| {
                let uses = if i < length { format!("use m{}\n\n", i + 1) } else { String::new() };
                (format!("m{}.vela", i), format!("{}task value{}() -> int\n    <- {}\nfi\n", uses, i, i))
            })
            .collect()
    };
    let main = "use m1\n\ntask main()\n    show(m1.value1())\nfi\n";
    let check = |length: usize| {
        let files = chain(length);
        let borrowed: Vec<(&str, &str)> = files.iter().map(|(name, source)| (name.as_str(), source.as_str())).collect();
        let asked = std::cell::RefCell::new(Vec::new());
        driver::check_program(Path::new("main.vela"), main, &mut memory_loader(&borrowed, &asked))
    };
    let limit = vela::parser::MAX_NESTING;
    check(limit - 1).unwrap();
    let error = check(limit + 50).unwrap_err();
    assert_eq!(error.error.message, format!("modules use one another too deeply (more than {} in a chain)", limit));
}
