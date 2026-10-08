//! Lambdas that keep what they use, `[@r](x: int) -> int … fi`: their types,
//! their parameters, the variables of the code around them they keep, and the
//! calls through them, hovered whether or not the file checks.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, LineIndex, TokenClass};

const PROGRAM: &str = "\
task counter(r: region, start: int) -> [flex @r]() -> int
    flex n = start
    <- [flex @r]() -> int
        n = n + 1
        <- n
    fi
fi

task main()
    region r
    flex next = counter(r, 10)
    fixed base = 100
    fixed add = [@r](x: int) -> int <- x + base fi
    fixed got = add(1)
    fixed made = [@r](k: int) -> [int]@r
        <- new [int]() at r
    fi
    fixed list = made(2)
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
        assert_eq!(hover(&source, "next ="), "flex next: [flex @r]() -> int", "{}", when);
        assert_eq!(hover(&source, "add ="), "fixed add: [@r](int) -> int", "{}", when);
        assert_eq!(hover(&source, "got ="), "fixed got: int", "{}", when);
        assert_eq!(hover(&source, "made ="), "fixed made: [@r](int) -> [int]@r", "{}", when);
        assert_eq!(hover(&source, "list ="), "fixed list: [int]@r", "{}", when);
    }
}

#[test]
fn its_parameters_and_what_it_keeps_name_what_they_are() {
    for when in WHEN {
        let source = program(when);
        assert_eq!(hover(&source, "x: int"), "x: int", "{}", when);
        assert_eq!(hover(&source, "x + base"), "x: int", "{}", when);
        assert_eq!(hover(&source, "base fi"), "fixed base: int", "{}", when);
        assert_eq!(hover(&source, "n = n"), "flex n: int", "{}", when);
        assert_eq!(hover(&source, "r](x"), "region r", "{}", when);
        assert_eq!(class(&source, "flex @r]() -> int\n        n"), TokenClass::Keyword, "{}", when);
    }
}

#[test]
fn a_place_it_keeps_names_the_variable_around_it() {
    let source = "\
task main()
    flex count = 0
    fixed big = [1, 2, 3]
    region r
    fixed add = [flex& count, fixed& big, @r](n: int) -> nothing count = count + n + big[0] fi
    add(2)
    show(count)
fi
";
    for when in ["checked", "unparsed"] {
        let source = if when == "unparsed" { format!("{}task g(\n", source) } else { source.to_string() };
        assert_eq!(hover(&source, "count, fixed"), "flex count: int", "{}", when);
        assert_eq!(hover(&source, "big, @r"), "fixed big: [int#3]", "{}", when);
        assert_eq!(hover(&source, "n: int"), "n: int", "{}", when);
        assert_eq!(hover(&source, "add ="), "fixed add: [@r](int) -> nothing", "{}", when);
    }
}

#[test]
fn a_lambda_s_type_in_a_header_or_a_variable_is_a_type() {
    let source = "\
task poke(f: [@r](int) -> int, flex& n: int)
    n = f(n)
fi

task main()
    region r
    flex k = 1
    fixed g: [@r](flex& int) -> nothing = [@r](flex& j: int) -> nothing j = j + 1 fi
    g(&k)
    poke([@r](x: int) -> int <- x * 2 fi, &k)
    show(k)
fi
";
    assert_eq!(Analysis::new(source).diagnostics(), Vec::new());
    assert_eq!(hover(source, "n: int)"), "flex& n: int");
    assert_eq!(hover(source, "n = f"), "flex& n: int");
    assert_eq!(hover(source, "j: int"), "flex& j: int");
    assert_eq!(hover(source, "j + 1"), "flex& j: int");
    assert_eq!(class(source, "int) -> int, flex"), TokenClass::TypeBuiltin);
}
