//! Steps in the editor: `x++`, `x--`, and `x += v` with the other compound
//! operators, each one operator, checked as the compiler checks them.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, TokenClass};

const PROGRAM: &str = "\
task main()
    region r
    flex x = 1
    x++
    x--
    x += 2
    x -= 1
    x *= 3
    x /= 2
    x %= 5
    x **= 2
    flex t: text@r = \"a\" + \"b\" at r
    t += \"c\"
    show(x, t)
fi
";

#[test]
fn the_program_used_by_these_tests_compiles() {
    assert_eq!(Analysis::new(PROGRAM).diagnostics(), Vec::new());
}

#[test]
fn each_step_is_one_operator() {
    let analysis = Analysis::new(PROGRAM);
    let tokens = analysis.tokens();
    for symbol in ["++", "--", "+=", "-=", "*=", "/=", "%=", "**="] {
        let byte = PROGRAM.find(symbol).unwrap_or_else(|| panic!("no {:?}", symbol));
        let token = tokens.iter().find(|t| t.from == byte).unwrap_or_else(|| panic!("no token at {:?}", symbol));
        assert_eq!((&PROGRAM[token.from..token.to], token.class), (symbol, TokenClass::Operator));
    }
}

#[test]
fn a_step_on_a_float_is_an_error_in_the_editor_too() {
    let source = "task main()\n    flex f = 1.5\n    f++\nfi\n";
    let diagnostics = Analysis::new(source).diagnostics();
    assert_eq!(diagnostics.len(), 1, "{:?}", diagnostics);
    assert!(diagnostics[0].message.starts_with("'++' counts an int, a unit, or a char by one"), "{:?}", diagnostics);
}
