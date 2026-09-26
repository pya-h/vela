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
    let BuildError::Vela(program) = &err else { panic!("expected a Vela error, got {:?}", err) };
    let error = program.first();
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
fn partial_front_end_keeps_the_types_found_around_an_error() {
    let source = "task main()\n    bind x = 1.5\n    show(missing)\n    bind y = true\nfi\n";
    let partial = driver::front_end_partial(source);
    assert_eq!(partial.errors.first().cloned(), driver::front_end(source).err());
    let items = partial.items.unwrap();
    let Item::Task(main) = &items[0] else { panic!("expected a task") };
    let binding_type = |i: usize| match &main.body[i].kind {
        StmtKind::Binding { names, .. } => names.first().and_then(|bound| bound.ty.clone()),
        other => panic!("expected a binding, got {:?}", other),
    };
    // The checker goes on after a mistake, so the lines on both sides of it are
    // checked and only the line with the mistake is left without types.
    assert_eq!(binding_type(0), Some(Type::Float));
    assert_eq!(binding_type(2), Some(Type::Bool));

    // A program that doesn't parse has no items.
    let broken = driver::front_end_partial("task main(\n");
    assert!(broken.items.is_none() && broken.errors.len() == 1);
}

#[test]
fn every_mistake_is_reported_once_in_source_order() {
    // One in a declaration, one in each task body, and one that the first would
    // have hidden: the checker reports them all, in the order they are written.
    let source = "task twice(n: Nope) -> int\n    <- n\nfi\n\ntask main()\n    bind a = twice(2)\n    show(missing)\n    show(a, gone)\nfi\n";
    let partial = driver::front_end_partial(source);
    let found: Vec<(usize, usize, &str)> =
        partial.errors.iter().map(|error| (error.span.unwrap().line, error.span.unwrap().col, error.message.as_str())).collect();
    assert_eq!(
        found,
        vec![
            (1, 15, "unknown type 'Nope'"),
            (7, 10, "unknown variable 'missing'"),
            (8, 13, "unknown variable 'gone'"),
        ]
    );

    // Every line with a mistake is reported, however many there are; how many
    // the command shows at once is `MAX_SHOWN` (see the `cli` tests).
    let many: String = (0..driver::MAX_SHOWN + 3).map(|i| format!("    show(missing{})\n", i)).collect();
    let errors = driver::front_end_partial(&format!("task main()\n{}fi\n", many)).errors;
    assert_eq!(errors.len(), driver::MAX_SHOWN + 3);
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
        ("fail chains", format!("task f() -> int ~ text\n    <- {}\"x\"\nfi\n", "fail ".repeat(deep))),
        ("dereference chains", program(format!("p{}", "^".repeat(deep)))),
        ("field chains", program(format!("p{}", ".a".repeat(deep)))),
        ("nested matches", format!("task main()\n{}fi\n", "match k\ncase _\n".repeat(deep) + &"fi\n".repeat(deep))),
        ("nested loops", format!("task main()\n{}fi\n", "loop true\neach i in 0..1\n".repeat(deep) + &"fi\nfi\n".repeat(deep))),
        ("nested is conditions", format!("task main()\n{}fi\n", "when m is {v}\n".repeat(deep) + &"fi\n".repeat(deep))),
        // One `fi` closes a whole `else when` chain, which nests all the same.
        ("else when chains", format!("task main()\nwhen true\n{}fi\nfi\n", "else when true\n".repeat(deep))),
        ("nested new", program(format!("{}1{}", "new int(".repeat(deep), ") at r".repeat(deep)))),
        ("nested shape literals", program(format!("{}1{}", "P { x: ".repeat(deep), " }".repeat(deep)))),
        ("region pointer types", format!("task f(p: {}int@r)\nfi\n", "^".repeat(deep))),
        ("array types", format!("task f(p: {}int{})\nfi\n", "[".repeat(deep), "; 1]".repeat(deep))),
        ("slice types", format!("task f(p: {}int{}@r)\nfi\n", "[".repeat(deep), "]".repeat(deep))),
        ("array literals", program(format!("{}1{}", "[".repeat(deep), "]".repeat(deep)))),
        ("repeated values", program(format!("{}1{}", "[".repeat(deep), "; 1]".repeat(deep)))),
        ("index chains", program(format!("p{}", "[0]".repeat(deep)))),
        ("slice changes", program(format!("{}0{}", "xs.del(".repeat(deep), ")".repeat(deep)))),
        ("scans", program(format!("{}\"\"{}", "scan(".repeat(deep), ") at r".repeat(deep)))),
        ("copies", program(format!("line{}", ".copy()@r".repeat(deep)))),
        ("tuples", program(format!("{}1{}", "(".repeat(deep), ", 1)".repeat(deep)))),
        ("tuple types", format!("task f(p: {}int{})\nfi\n", "(".repeat(deep), ", int)".repeat(deep))),
        ("tuple items", program(format!("t{}", ".0".repeat(deep)))),
        ("unpacked names", format!("task main()\n    bind {}a{} = t\nfi\n", "(".repeat(deep), ", b)".repeat(deep))),
        ("unpacked places", format!("task main()\n    {}a{} = t\nfi\n", "(".repeat(deep), ", b)".repeat(deep))),
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
    let calls = format!("task id(n: int) -> int\n    <- n\nfi\n{}", program(format!("{}1{}", "id(".repeat(near), ")".repeat(near))));
    driver::generate_c(&calls).unwrap();
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
    // An array literal counts two levels (itself, and its values); nested
    // just below the limit, arrays are checked and generated, and so is a
    // chain of indexes as long.
    let arrays = format!("task main()\n    bind a = {}1{}\n    show(a{}.len)\nfi\n", "[".repeat(near / 2 - 1), "]".repeat(near / 2 - 1), "[0]".repeat(near / 2 - 2));
    driver::generate_c(&arrays).unwrap();
    let indexes = format!("task f(a: {}int{})\n    show(a{})\nfi\n", "[".repeat(near), "; 1]".repeat(near), "[0]".repeat(near));
    driver::generate_c(&indexes).unwrap();
    // A change to a slice counts two levels (its slice, and its arguments),
    // and each writes its prelude before the one outside it.
    let dels = format!("task main()\n    region r\n    hold xs = new [int]([0; 1]) at r\n    show({}0{})\nfi\n", "xs.del(".repeat(near / 2 - 1), ")".repeat(near / 2 - 1));
    driver::generate_c(&dels).unwrap();
    // A prompt read by another `scan`, through a plan B: each nests a scan, a
    // plan B, and its block.
    let scans = format!("task main()\n    region r\n    show({}\"\"{})\nfi\n", "scan(".repeat(near / 3), ") at r { \"\" }".repeat(near / 3));
    driver::generate_c(&scans).unwrap();
    // Tuples in tuples, read back item by item, and unpacked into names.
    let tuples = format!(
        "task main()\n    bind t = {}1{}\n    show(t{})\n    bind {}a{} = t\n    show(a)\nfi\n",
        "(".repeat(near / 2 - 1),
        ", 1)".repeat(near / 2 - 1),
        ".0".repeat(near / 2 - 1),
        "(".repeat(near / 2 - 1),
        ", _)".repeat(near / 2 - 1)
    );
    driver::generate_c(&tuples).unwrap();
    // A copy of a copy, each in the region, named with `@` since a copy goes on
    // from the one before it: every one counts a level.
    let copies = format!("task main()\n    region r\n    bind line = scan(\"\") at r {{ \"\" }}\n    show(line{})\nfi\n", ".copy()@r".repeat(near));
    driver::generate_c(&copies).unwrap();
    let fails = format!("task f() -> int ~ text\n    <- {}\"x\"\nfi\n", "fail ".repeat(near));
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
    let main = "include shapes/geometry\ninclude text/labels (label)\n\ntask main()\n    show(label(geometry.area(3)))\nfi\n";
    let files = [
        ("app/shapes/geometry.vela", "include ../text/labels\n\ntask area(side: int) -> int\n    <- labels.width(side) * side\nfi\n"),
        ("app/text/labels.vela", "task label(n: int) -> text\n    <- \"shape\"\nfi\n\ntask width(n: int) -> int\n    <- n\nfi\n"),
    ];
    let asked = std::cell::RefCell::new(Vec::new());
    let items = driver::check_program(Path::new("app/main.vela"), main, &mut memory_loader(&files, &asked)).unwrap();
    // Paths are relative to the file with the `include`, with `..` settled, and a
    // module used twice is read once. A module's names in the program start with
    // its path from the main file's folder.
    assert_eq!(*asked.borrow(), ["app/shapes/geometry.vela", "app/text/labels.vela"]);
    // The tasks, in order; the program also holds the builtin kind that reading
    // a number sends, which no file declares.
    let names: Vec<&str> = items.iter().filter_map(|item| match item {
        Item::Task(t) => Some(t.name.as_str()),
        _ => None,
    }).collect();
    assert_eq!(names, ["text/labels.label", "text/labels.width", "shapes/geometry.area", "main"]);
    let c = driver::generate_c_program(Path::new("app/main.vela"), main, &mut memory_loader(&files, &asked)).unwrap();
    assert!(c.contains("int64_t vela_shapes__1__geometry__0__area(int64_t side)"), "{}", c);
}

#[test]
fn a_module_outside_the_main_folder_keeps_its_path_in_its_names() {
    let main = "include ../common/text => words\n\ntask main()\n    show(words.twice(2))\nfi\n";
    let files = [("common/text.vela", "task twice(n: int) -> int\n    <- n * 2\nfi\n")];
    let asked = std::cell::RefCell::new(Vec::new());
    let items = driver::check_program(Path::new("app/main.vela"), main, &mut memory_loader(&files, &asked)).unwrap();
    // The tasks, in order; the program also holds the builtin kind that reading
    // a number sends, which no file declares.
    let names: Vec<&str> = items.iter().filter_map(|item| match item {
        Item::Task(t) => Some(t.name.as_str()),
        _ => None,
    }).collect();
    assert_eq!(names, ["../common/text.twice", "main"]);
    // In C, `..` is `__2__`, each `/` `__1__`, and the `.` before the name `__0__`.
    let c = driver::generate_c_program(Path::new("app/main.vela"), main, &mut memory_loader(&files, &asked)).unwrap();
    assert!(c.contains("int64_t vela___2____1__common__1__text__0__twice(int64_t n)"), "{}", c);
}

#[test]
fn a_module_that_cannot_be_read_says_why() {
    let main = "include lib\n\ntask main()\nfi\n";
    let mut loader = |_: &Path| -> std::io::Result<String> { Err(std::io::Error::other("Is a directory")) };
    let error = driver::check_program(Path::new("main.vela"), main, &mut loader).unwrap_err();
    assert_eq!(error.first().span, Some(Span { line: 1, col: 9, file: 0 }));
    assert_eq!(error.first().message, "can't read module 'lib' (lib.vela): Is a directory");
}

#[test]
fn a_name_the_file_cant_write_is_told_apart_from_one_it_can() {
    // `maker` returns `c.Thing` of `c.vela`; main names the module `x.vela` `c`.
    let main = "include maker\ninclude x => c\n\ntask main()\n    bind thing: c.Thing = maker.make()\nfi\n";
    let files = [
        ("c.vela", "shape Thing\n    v: int\nfi\n"),
        ("x.vela", "shape Thing\n    w: int\nfi\n"),
        ("maker.vela", "include c\n\ntask make() -> c.Thing\n    <- c.Thing { v: 1 }\nfi\n"),
    ];
    let asked = std::cell::RefCell::new(Vec::new());
    let error = driver::check_program(Path::new("main.vela"), main, &mut memory_loader(&files, &asked)).unwrap_err();
    assert_eq!(error.first().message, "expected type c.Thing, got ./c.Thing");
}

#[test]
fn an_error_in_a_module_names_that_file() {
    let main = "include lib\n\ntask main()\n    show(lib.twice(2))\nfi\n";
    let files = [("lib.vela", "task twice(n: int) -> int\n    <- n * \"2\"\nfi\n")];
    let asked = std::cell::RefCell::new(Vec::new());
    let error = driver::check_program(Path::new("main.vela"), main, &mut memory_loader(&files, &asked)).unwrap_err();
    assert_eq!(error.first().span, Some(Span { line: 2, col: 10, file: 1 }));
    assert_eq!(error.error_file().as_deref(), Some("lib.vela"));
    assert_eq!(
        error.render("main.vela"),
        "lib.vela:2:10: error: arithmetic requires numeric operands\n 2 |     <- n * \"2\"\n   |          ^"
    );
    // A program in one file can't load modules, and says which is missing.
    let partial = driver::front_end_partial(main);
    assert_eq!(partial.errors[0].message, "can't find module 'lib': there is no file 'lib.vela'");
}

#[test]
fn long_chains_of_modules_are_an_error_not_a_crash() {
    // Module i includes module i + 1: loading follows the chain recursively, on a
    // test thread's small stack.
    let chain = |length: usize| -> Vec<(String, String)> {
        (1..=length)
            .map(|i| {
                let includes = if i < length { format!("include m{}\n\n", i + 1) } else { String::new() };
                (format!("m{}.vela", i), format!("{}task value{}() -> int\n    <- {}\nfi\n", includes, i, i))
            })
            .collect()
    };
    let main = "include m1\n\ntask main()\n    show(m1.value1())\nfi\n";
    let check = |length: usize| {
        let files = chain(length);
        let borrowed: Vec<(&str, &str)> = files.iter().map(|(name, source)| (name.as_str(), source.as_str())).collect();
        let asked = std::cell::RefCell::new(Vec::new());
        driver::check_program(Path::new("main.vela"), main, &mut memory_loader(&borrowed, &asked))
    };
    let limit = vela::parser::MAX_NESTING;
    // The main file and `limit - 1` modules: `limit` files in the chain.
    check(limit - 1).unwrap();
    for length in [limit, limit + 50] {
        let error = check(length).unwrap_err();
        assert_eq!(error.first().message, format!("modules include one another too deeply (more than {} in a chain)", limit));
        // At the include that would make the chain too long, even when the last
        // module includes nothing.
        let span = error.first().span.unwrap();
        let file = error.files.get(span.file).unwrap();
        assert_eq!((file.path.to_str(), span.line, span.col), (Some(format!("m{}.vela", limit - 1).as_str()), 1, 9));
    }
}
