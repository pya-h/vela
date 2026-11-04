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
    fixed double = (x: int) -> int <- x * 2 fi
    fixed squared = apply((n: int) -> int
        fixed kept = n
        <- kept * n
    fi, 3)
    fixed say = (t: text) -> nothing show(t) fi
    fixed after = double(4)
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
        assert_eq!(hover(&source, "double ="), "fixed double: (int) -> int", "{}", when);
        assert_eq!(hover(&source, "say ="), "fixed say: (text) -> nothing", "{}", when);
        assert_eq!(hover(&source, "after ="), "fixed after: int", "{}", when);
    }
    // A call spanning lines is typed from the checked program.
    for when in ["checked", "declared"] {
        assert_eq!(hover(&program(when), "squared ="), "fixed squared: int", "{}", when);
    }
}

#[test]
fn a_lambda_declares_its_parameters_for_its_body() {
    for when in WHEN {
        let source = program(when);
        assert_eq!(hover(&source, "n: int"), "n: int", "{}", when);
        assert_eq!(hover(&source, "t: text"), "t: text", "{}", when);
        assert_eq!(hover(&source, "kept ="), "fixed kept: int", "{}", when);
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


#[test]
fn a_task_type_after_a_comma_or_a_parenthesis_is_no_lambda() {
    // A result that is a task's type, and a tuple holding one: neither opens a
    // block, so the methods and tasks after them are outlined.
    let source = "\
task one() -> int
    <- 1
fi

shape Box
    v: int

    pair() -> int, () -> int
        <- .v, one
    fi

    get() -> int
        flex t: (() -> int, int) = (one, 1)
        <- .v
    fi

    last() -> int
        <- .v
    fi
fi

task both() -> int, () -> int
    <- 1, one
fi

task main()
    fixed b = Box { v: 2 }
    show(b.get(), b.last())
fi
";
    let analysis = Analysis::new(source);
    assert_eq!(analysis.diagnostics(), Vec::new());
    let outline = analysis.outline();
    let names: Vec<&str> = outline.iter().map(|item| item.name.as_str()).collect();
    assert_eq!(names, ["one", "Box", "both", "main"]);
    let members: Vec<&str> = outline[1].children.iter().map(|item| item.name.as_str()).collect();
    assert_eq!(members, ["v", "pair", "get", "last"]);
    assert_eq!(hover(source, "get()"), "get() -> int");
    // A lambda after a comma is still one.
    let lambda = "task apply(n: int, f: () -> int) -> int\n    <- f()\nfi\n\ntask main()\n    show(apply(1, () -> int <- 2 fi))\nfi\n";
    assert_eq!(Analysis::new(lambda).diagnostics(), Vec::new());
    assert_eq!(Analysis::new(lambda).outline().len(), 2);
}

#[test]
fn a_shortcut_lambda_declares_its_parameters_for_its_body_only() {
    let source = "\
task apply(f: (int) -> int, x: int) -> int
    <- f(x)
fi

task main()
    region r
    fixed total = 3
    show(apply(y -> y * total, 2), apply((a) -> a + 1, 2))
    fixed kept: [@r](int) -> int = [@r] n -> n + total
    show(kept(1), y)
fi
";
    let index = LineIndex::new(source);
    let at = |needle: &str| index.utf16_of(source.find(needle).unwrap());
    let analysis = Analysis::new(source);
    let tokens = analysis.tokens();
    let class = |needle: &str| tokens.iter().find(|t| t.from == at(needle)).map(|t| t.class);
    // The names before the arrow and in the body are its parameters; the body
    // is a value, not a type.
    assert_eq!(class("y -> y"), Some(TokenClass::Parameter));
    assert_eq!(class("y * total"), Some(TokenClass::Parameter));
    assert_eq!(class("total, 2)"), Some(TokenClass::Variable));
    assert_eq!(class("a + 1"), Some(TokenClass::Parameter));
    assert_eq!(class("n + total"), Some(TokenClass::Parameter));
    let definition = analysis.definition(at("y * total")).expect("a definition");
    assert_eq!(definition.from, at("y -> y"));
    // Past the lambda's body the name is gone.
    assert!(analysis.definition(at("y)\nfi")).is_none());
    // After the arrow a value goes, so names are offered, not only types.
    let typing = "task main()\n    fixed total = 3\n    fixed f: (int) -> int = x -> t\nfi\n";
    let byte = typing.find("-> t\n").unwrap() + 4;
    let items = Analysis::new(typing).complete(LineIndex::new(typing).utf16_of(byte)).items;
    assert!(items.iter().any(|item| item.label == "total"), "{:?}", items.iter().map(|item| &item.label).collect::<Vec<_>>());
}
