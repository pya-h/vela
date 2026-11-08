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
    fixed sum = call(add, 2, 3)
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
    assert_eq!(hover(PROGRAM, "sum ="), "fixed sum: int");
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

const AROUND: &str = "\
task traced<P..., T>(next: (@a, P...) -> [T]@a, r: region, args: P...) -> [T]@r
    <- next(r, args...)
fi

task labelled<P...>(next: (P..., @a) -> text@a, args: P..., r: region) -> text@r
    <- next(args..., r)
fi

$(traced)
task squares(r: region, flex& n: int) -> [int]@r
    n = n + 1
    <- new [int]() at r
fi

$(labelled)
task label(n: int, r: region) -> text@r
    <- \"n\" + \"!\" at r
fi

task main()
    region kept
    flex k = 3
    fixed made = squares(kept, &k)
    show(made, label(1, kept))
fi
";

#[test]
fn values_named_around_a_group_read_as_declared() {
    assert_eq!(Analysis::new(AROUND).diagnostics(), Vec::new());
    assert_eq!(hover(AROUND, "next: (@"), "next: (@r, P...) -> [T]@r");
    assert_eq!(hover(AROUND, "next: (P"), "next: (P..., @r) -> text@r");
    assert_eq!(hover(AROUND, "r: region, args"), "r: region");
    assert_eq!(class(AROUND, "args..., r)"), Some(TokenClass::Parameter));
    assert_eq!(class(AROUND, "a, P...) -> [T]"), Some(TokenClass::Region));
    let source = AROUND.replace("<- next(r, args...)", "<- next(a, args...)");
    let diagnostics = Analysis::new(&source).diagnostics();
    assert!(diagnostics.iter().any(|d| d.message.contains("'a'")), "{:?}", diagnostics);
}
