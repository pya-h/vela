//! Lambdas in the editor: a task with no name, written where its value goes,
//! hovered with its type whether or not the file checks, and left out of the
//! outline.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, LineIndex, TokenClass};

const PROGRAM: &str = "\
task apply(f: (int) -> int, x: int) -> int
    <- f(x)
fi

task main()
    CHECK
    bind double = (x: int) -> int <- x * 2 fi
    bind squared = apply((n: int) -> int
        bind kept = n
        <- kept * n
    fi, 3)
    bind say = (t: text) -> nothing show(t) fi
    bind after = double(4)
    show(double, squared, after)
    say(\"hi\")
fi
";

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
fn a_lambda_hovers_with_its_type_whether_or_not_the_file_checks() {
    for when in WHEN {
        let source = program(when);
        assert_eq!(hover(&source, "double ="), "bind double: (int) -> int", "{}", when);
        assert_eq!(hover(&source, "say ="), "bind say: (text) -> nothing", "{}", when);
        assert_eq!(hover(&source, "after ="), "bind after: int", "{}", when);
    }
    // A call spanning lines is typed from the checked program.
    for when in ["checked", "declared"] {
        assert_eq!(hover(&program(when), "squared ="), "bind squared: int", "{}", when);
    }
}

#[test]
fn a_lambda_declares_its_parameters_for_its_body() {
    for when in WHEN {
        let source = program(when);
        assert_eq!(hover(&source, "n: int"), "n: int", "{}", when);
        assert_eq!(hover(&source, "t: text"), "t: text", "{}", when);
        assert_eq!(hover(&source, "kept ="), "bind kept: int", "{}", when);
        let at = |needle: &str| LineIndex::new(&source).utf16_of(source.find(needle).unwrap());
        let tokens = Analysis::new(&source).tokens();
        let class = |needle: &str| tokens.iter().find(|t| t.from == at(needle)).map(|t| t.class);
        assert_eq!(class("x: int) -> int <-"), Some(TokenClass::Parameter), "{}", when);
        assert_eq!(class("x * 2"), Some(TokenClass::Parameter), "{}", when);
        assert_eq!(class("n\n        <- kept"), Some(TokenClass::Parameter), "{}", when);
        assert_eq!(class("t) fi"), Some(TokenClass::Parameter), "{}", when);
    }
}

#[test]
fn a_lambda_closes_with_its_own_fi_and_stays_out_of_the_outline() {
    for when in ["checked", "declared"] {
        let source = program(when);
        let outline = Analysis::new(&source).outline();
        let names: Vec<&str> = outline.iter().map(|item| item.name.as_str()).collect();
        assert_eq!(names, ["apply", "main"], "{}", when);
        // `main` ends at its own `fi`, after every lambda's.
        let main = outline.iter().find(|item| item.name == "main").unwrap();
        assert_eq!(main.to, LineIndex::new(&source).utf16_of(source.rfind("fi").unwrap() + 2), "{}", when);
    }
}

#[test]
fn a_task_type_parameter_is_no_lambda() {
    let source = "task run(f: () -> int, g: (int) -> int) -> () -> int\n    <- f\nfi\n";
    let tokens = Analysis::new(source).tokens();
    let at = |needle: &str| LineIndex::new(source).utf16_of(source.find(needle).unwrap());
    assert_eq!(tokens.iter().find(|t| t.from == at("g:")).map(|t| t.class), Some(TokenClass::Parameter));
    assert_eq!(Analysis::new(source).outline().len(), 1);
}

