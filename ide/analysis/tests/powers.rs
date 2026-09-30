//! `**` in the editor: colored as an operator, and a power's type hovered as
//! the checker gives it, whether or not the file checks.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, LineIndex, TokenClass};

const PROGRAM: &str = "\
task main()
    CHECK
    hold n = 3
    hold b: unit = 2
    bind exact = n ** 2
    bind fraction = n ** n
    bind by_unit = n ** b
    bind small = b ** 3
    bind root = 2.0 ** 0.5
    bind mixed = 9 ** 0.5
    bind chained = 2 * n ** 2
    show(exact, fraction, by_unit, small, root, mixed, chained)
fi
";

fn program(checks: bool) -> String {
    PROGRAM.replace("CHECK", if checks { "show(0)" } else { "show(unknown)" })
}

fn hover(source: &str, needle: &str) -> String {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    Analysis::new(source).hover(LineIndex::new(source).utf16_of(byte)).unwrap_or_else(|| panic!("no hover at {:?}", needle)).code
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    assert_eq!(Analysis::new(&program(true)).diagnostics(), Vec::new());
}

#[test]
fn a_power_hovers_with_its_type_whether_or_not_the_file_checks() {
    for checks in [true, false] {
        let source = program(checks);
        let when = if checks { "checked" } else { "declared" };
        assert_eq!(hover(&source, "exact ="), "bind exact: int", "{}", when);
        assert_eq!(hover(&source, "fraction ="), "bind fraction: float", "{}", when);
        assert_eq!(hover(&source, "by_unit ="), "bind by_unit: int", "{}", when);
        assert_eq!(hover(&source, "small ="), "bind small: unit", "{}", when);
        assert_eq!(hover(&source, "root ="), "bind root: float", "{}", when);
        assert_eq!(hover(&source, "mixed ="), "bind mixed: float", "{}", when);
        assert_eq!(hover(&source, "chained ="), "bind chained: int", "{}", when);
    }
}

#[test]
fn a_power_is_one_operator() {
    let source = program(true);
    let byte = source.find("**").unwrap();
    let token = Analysis::new(&source).tokens().into_iter().find(|t| t.from == byte).unwrap();
    assert_eq!((token.to - token.from, token.class), (2, TokenClass::Operator));
}
