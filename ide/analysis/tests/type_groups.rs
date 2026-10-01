//! Type groups in the editor: `P...` among a task's types, `args: P...`,
//! `(P...) -> R`, and `f(args...)`, highlighted and hovered as they are
//! declared.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, LineIndex, TokenClass};

const PROGRAM: &str = "\
task call<P..., R>(f: (P...) -> R, args: P...) -> R
    <- f(args...)
fi

task add(a: int, b: int) -> int
    <- a + b
fi

task main()
    bind sum = call(add, 2, 3)
    show(sum)
fi
";

fn hover(source: &str, needle: &str) -> String {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    Analysis::new(source).hover(LineIndex::new(source).utf16_of(byte)).unwrap_or_else(|| panic!("no hover at {:?}", needle)).code
}

fn class(source: &str, needle: &str) -> Option<TokenClass> {
    let at = LineIndex::new(source).utf16_of(source.find(needle).unwrap());
    Analysis::new(source).tokens().into_iter().find(|t| t.from == at).map(|t| t.class)
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    assert_eq!(Analysis::new(PROGRAM).diagnostics(), Vec::new());
}

#[test]
fn a_group_and_its_values_read_as_declared() {
    assert_eq!(hover(PROGRAM, "args: P"), "args: P...");
    assert_eq!(hover(PROGRAM, "f: ("), "f: (P...) -> R");
    assert_eq!(hover(PROGRAM, "sum ="), "bind sum: int");
    assert_eq!(hover(PROGRAM, "...) -> R"), "P..., args: P..., (P...) -> R, f(args...)");
    assert_eq!(class(PROGRAM, "P..., R"), Some(TokenClass::Type));
    assert_eq!(class(PROGRAM, "args...)"), Some(TokenClass::Parameter));
}

#[test]
fn a_group_misused_is_reported_where_it_is_written() {
    let source = PROGRAM.replace("<- f(args...)", "<- f(args)");
    let diagnostics = Analysis::new(&source).diagnostics();
    assert_eq!((diagnostics[0].line, diagnostics[0].col), (Some(2), Some(10)));
    assert!(diagnostics[0].message.starts_with("'args' holds the values of the group 'P...'"), "{}", diagnostics[0].message);
}
