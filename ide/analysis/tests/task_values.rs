//! Task values in the editor: a task named without `()`, a task's type written
//! `(int) -> int`, and the call of a value that holds a task, hovered with their
//! types whether or not the file checks.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, LineIndex, TokenClass};

const PROGRAM: &str = "\
task square(n: int) -> int
    <- n * n
fi

task head(t: text@r) -> text@r
    <- t[0..1]
fi

task apply(f: (int) -> int, x: int) -> int
    <- f(x)
fi

task add(a: int, b: int) -> int
    <- a + b
fi

task pick() -> (int, int) -> int
    <- add
fi

task main()
    CHECK
    fixed value = square
    fixed lettered = head
    fixed applied = apply(square, 2)
    fixed called = value(3)
    fixed picked = pick()(1, 2)
    flex kept: maybe (int) -> int = none
    show(value, lettered, applied, called, picked, kept)
fi
";

/// The program as it checks, as it parses but doesn't check, and as it
/// doesn't parse, where the types are read from the source alone.
const WHEN: [&str; 3] = ["checked", "declared", "unparsed"];

fn program(when: &str) -> String {
    let source = PROGRAM.replace("CHECK", if when == "checked" { "show(0)" } else { "show(unknown)" });
    if when == "unparsed" { source + "task g(\n" } else { source }
}

fn hover(source: &str, needle: &str) -> String {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    Analysis::new(source).hover(LineIndex::new(source).utf16_of(byte)).unwrap_or_else(|| panic!("no hover at {:?}", needle)).code
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    assert_eq!(Analysis::new(&program("checked")).diagnostics(), Vec::new());
}

#[test]
fn a_task_value_hovers_with_its_type_whether_or_not_the_file_checks() {
    for when in WHEN {
        let source = program(when);
        assert_eq!(hover(&source, "value ="), "bind value: (int) -> int", "{}", when);
        assert_eq!(hover(&source, "lettered ="), "bind lettered: (text@r) -> text@r", "{}", when);
        assert_eq!(hover(&source, "applied ="), "bind applied: int", "{}", when);
        assert_eq!(hover(&source, "called ="), "bind called: int", "{}", when);
        assert_eq!(hover(&source, "picked ="), "bind picked: int", "{}", when);
        assert_eq!(hover(&source, "kept:"), "hold kept: maybe (int) -> int", "{}", when);
    }
}

#[test]
fn a_parameter_of_a_task_type_hovers_with_it() {
    let source = program("checked");
    assert_eq!(hover(&source, "f: (int)"), "f: (int) -> int");
}

#[test]
fn a_parameter_after_one_of_a_task_type_is_a_parameter() {
    let source = "task twice(f: (int) -> nothing, n: int) -> int\n    f(n)\n    <- n\nfi\n";
    let byte = source.find("n: int").unwrap();
    let token = Analysis::new(source).tokens().into_iter().find(|t| t.from == byte).unwrap();
    assert_eq!(token.class, TokenClass::Parameter);
    // The header's own arrow still starts its results.
    let ret = source.rfind("int").unwrap();
    let token = Analysis::new(source).tokens().into_iter().find(|t| t.from == ret).unwrap();
    assert_eq!(token.class, TokenClass::TypeBuiltin);
}

#[test]
fn a_task_value_taking_a_place_or_a_region_hovers_with_how_it_takes_them() {
    let program = "\
task bump(flex& n: int)
    n = n + 1
fi

task total(fixed& p: int) -> int
    <- p
fi

task squares(r: region, n: int) -> [int]@r
    <- new [int]() at r
fi

task defaulted(r: region default, n: int) -> [int]@r
    <- new [int]()
fi

task main()
    CHECK
    fixed bumped = bump
    fixed totalled = total
    fixed squared = squares
    fixed kinder = defaulted
    flex kept: maybe (@s, int) -> [int]@s = none
    region q
    fixed made = squared(q, 3)
    show(bumped, totalled, squared, kinder, kept, made)
fi
";
    for when in WHEN {
        let source = program.replace("CHECK", if when == "checked" { "show(0)" } else { "show(unknown)" });
        let source = if when == "unparsed" { source + "task g(\n" } else { source };
        if when == "checked" {
            assert_eq!(Analysis::new(&source).diagnostics(), Vec::new());
        }
        assert_eq!(hover(&source, "bumped ="), "bind bumped: (flex& int) -> nothing", "{}", when);
        assert_eq!(hover(&source, "totalled ="), "bind totalled: (fixed& int) -> int", "{}", when);
        assert_eq!(hover(&source, "squared ="), "bind squared: (@r, int) -> [int]@r", "{}", when);
        assert_eq!(hover(&source, "kept:"), "hold kept: maybe (@r, int) -> [int]@r", "{}", when);
        // A region taken by default is a region still, and what a call through
        // the value sends is in the region the call gives.
        assert_eq!(hover(&source, "kinder ="), "bind kinder: (@r, int) -> [int]@r", "{}", when);
        assert_eq!(hover(&source, "made ="), "bind made: [int]@q", "{}", when);
    }
}

#[test]
fn a_region_in_a_tuple_type_or_a_lambda_header_names_what_is_around_it() {
    let source = "\
task named(r: region, line: text@r) -> (text@r, int)
    <- (line, 1)
fi

task main()
    region q
    flex t: (text@q, int) = named(q, \"a\")
    show(t.1)
    fixed f = (s: region, n: int) -> [int]@s <- new [int]() at s fi
    fixed made = f(q, 2)
    show(made.len)
fi
";
    assert_eq!(Analysis::new(source).diagnostics(), Vec::new());
    assert_eq!(hover(source, "r, int)"), "r: region");
    assert_eq!(hover(source, "q, int)"), "region q");
    assert_eq!(hover(source, "s <- new"), "s: region");
}

#[test]
fn a_value_given_where_a_region_goes_names_no_region() {
    // A file that doesn't parse, read from the source alone; the spaces
    // before `default` don't matter.
    let source = "\
task pick(r: region  default, xs: [int]@r) -> [int]@r
    <- xs
fi

task main()
    region q
    fixed ys = new [int]() at q
    fixed got = pick(ys)
    fixed kept = pick(q, ys)
    show(got.len, kept.len)
fi
task g(
";
    assert_eq!(hover(source, "got ="), "bind got: [int]@_");
    assert_eq!(hover(source, "kept ="), "bind kept: [int]@q");
    assert_eq!(hover(source, "r: region"), "r: region");
}
