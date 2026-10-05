//! Lambdas that keep what they use, `[@r](x: int) -> int … fi`: their types,
//! their parameters, the variables of the code around them they keep, and the
//! calls through them, hovered whether or not the file checks.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, LineIndex, TokenClass};

const PROGRAM: &str = "\
task counter(r: region, start: int) -> [hold @r]() -> int
    hold n = start
    <- [hold @r]() -> int
        n = n + 1
        <- n
    fi
fi

task main()
    region r
    hold next = counter(r, 10)
    bind base = 100
    bind add = [@r](x: int) -> int <- x + base fi
    bind got = add(1)
    bind made = [@r](k: int) -> [int]@r
        <- new [int]() at r
    fi
    bind list = made(2)
    CHECK
    show(next(), got, list)
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

fn class(source: &str, needle: &str) -> TokenClass {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    let at = LineIndex::new(source).utf16_of(byte);
    Analysis::new(source).tokens().into_iter().find(|token| token.from == at).unwrap_or_else(|| panic!("no token at {:?}", needle)).class
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    assert_eq!(Analysis::new(&program("checked")).diagnostics(), Vec::new());
}

#[test]
fn a_lambda_that_keeps_what_it_uses_hovers_with_its_type_whether_or_not_the_file_checks() {
    for when in WHEN {
        let source = program(when);
        assert_eq!(hover(&source, "next ="), "hold next: [hold @r]() -> int", "{}", when);
        assert_eq!(hover(&source, "add ="), "bind add: [@r](int) -> int", "{}", when);
        assert_eq!(hover(&source, "got ="), "bind got: int", "{}", when);
        assert_eq!(hover(&source, "made ="), "bind made: [@r](int) -> [int]@r", "{}", when);
        assert_eq!(hover(&source, "list ="), "bind list: [int]@r", "{}", when);
    }
}

#[test]
fn its_parameters_and_what_it_keeps_name_what_they_are() {
    for when in WHEN {
        let source = program(when);
        assert_eq!(hover(&source, "x: int"), "x: int", "{}", when);
        assert_eq!(hover(&source, "x + base"), "x: int", "{}", when);
        assert_eq!(hover(&source, "base fi"), "bind base: int", "{}", when);
        assert_eq!(hover(&source, "n = n"), "hold n: int", "{}", when);
        assert_eq!(hover(&source, "r](x"), "region r", "{}", when);
        assert_eq!(class(&source, "hold @r]() -> int\n        n"), TokenClass::Keyword, "{}", when);
    }
}

#[test]
fn a_place_it_keeps_names_the_variable_around_it() {
    let source = "\
task main()
    hold count = 0
    bind big = [1, 2, 3]
    region r
    bind add = [hold& count, bind& big, @r](n: int) -> nothing count = count + n + big[0] fi
    add(2)
    show(count)
fi
";
    for when in ["checked", "unparsed"] {
        let source = if when == "unparsed" { format!("{}task g(\n", source) } else { source.to_string() };
        assert_eq!(hover(&source, "count, bind"), "hold count: int", "{}", when);
        assert_eq!(hover(&source, "big, @r"), "bind big: [int#3]", "{}", when);
        assert_eq!(hover(&source, "n: int"), "n: int", "{}", when);
        assert_eq!(hover(&source, "add ="), "bind add: [@r](int) -> nothing", "{}", when);
    }
}
