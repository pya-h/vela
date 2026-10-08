//! `**` in the editor: colored as an operator, and a power's type hovered as
//! the checker gives it, whether or not the file checks.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, LineIndex, TokenClass};

const PROGRAM: &str = "\
task main()
    CHECK
    flex n = 3
    flex b: unit = 2
    fixed exact = n ** 2
    fixed fraction = n ** n
    fixed by_unit = n ** b
    fixed small = b ** 3
    fixed root = 2.0 ** 0.5
    fixed mixed = 9 ** 0.5
    fixed chained = 2 * n ** 2
    fixed grouped = n ** (1 + 1)
    fixed inverse = n ** -1
    show(exact, fraction, by_unit, small, root, mixed, chained, grouped, inverse)
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
fn a_power_hovers_with_its_type_whether_or_not_the_file_checks() {
    for when in WHEN {
        let source = program(when);
        assert_eq!(hover(&source, "exact ="), "bind exact: int", "{}", when);
        assert_eq!(hover(&source, "fraction ="), "bind fraction: float", "{}", when);
        assert_eq!(hover(&source, "by_unit ="), "bind by_unit: int", "{}", when);
        assert_eq!(hover(&source, "small ="), "bind small: unit", "{}", when);
        assert_eq!(hover(&source, "root ="), "bind root: float", "{}", when);
        assert_eq!(hover(&source, "mixed ="), "bind mixed: float", "{}", when);
        assert_eq!(hover(&source, "chained ="), "bind chained: int", "{}", when);
        assert_eq!(hover(&source, "grouped ="), "bind grouped: int", "{}", when);
        assert_eq!(hover(&source, "inverse ="), "bind inverse: float", "{}", when);
    }
}

#[test]
fn a_power_is_one_operator() {
    let source = program("checked");
    let byte = source.find("**").unwrap();
    let token = Analysis::new(&source).tokens().into_iter().find(|t| t.from == byte).unwrap();
    assert_eq!((token.to - token.from, token.class), (2, TokenClass::Operator));
}
