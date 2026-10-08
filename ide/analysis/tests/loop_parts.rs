//! Loops in three parts, `loop hold i = 0, j = 9 || i < j || i++, j--`, and
//! `endless`: the names a start declares, where they are seen, and what is
//! offered around them, whether or not the file checks.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, LineIndex, TokenClass};

const PROGRAM: &str = "\
task main()
    hold total = 0
    loop hold i = 1, j = 10, bind limit = 7 || i < j and i < limit || i++, j--
        total += i * j
    fi
    outer => endless
        break outer
    fi
    CHECK
    show(total)
fi
";

/// The program as it checks, as it parses but doesn't check, and as it
/// doesn't parse.
const WHEN: [&str; 3] = ["checked", "declared", "unparsed"];

fn program(when: &str) -> String {
    let source = PROGRAM.replace("CHECK", if when == "checked" { "show(0)" } else { "show(unknown)" });
    if when == "unparsed" { source + "task g(\n" } else { source }
}

fn hover(source: &str, needle: &str) -> Option<String> {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    Analysis::new(source).hover(LineIndex::new(source).utf16_of(byte)).map(|hover| hover.code)
}

fn class(source: &str, needle: &str) -> TokenClass {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    let at = LineIndex::new(source).utf16_of(byte);
    Analysis::new(source).tokens().into_iter().find(|token| token.from == at).unwrap_or_else(|| panic!("no token at {:?}", needle)).class
}

/// The labels offered at `@@`.
fn labels(marked: &str) -> Vec<String> {
    let at = marked.find("@@").expect("a @@ marker");
    let source = marked.replacen("@@", "", 1);
    let offset = LineIndex::new(&source).utf16_of(at);
    Analysis::new(&source).complete(offset).items.into_iter().map(|item| item.label).collect()
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    assert_eq!(Analysis::new(&program("checked")).diagnostics(), Vec::new());
}

#[test]
fn the_names_a_start_declares_hover_with_the_word_that_covers_them() {
    for when in WHEN {
        let source = program(when);
        assert_eq!(hover(&source, "i = 1").as_deref(), Some("hold i: int"), "{}", when);
        assert_eq!(hover(&source, "j = 10").as_deref(), Some("hold j: int"), "{}", when);
        assert_eq!(hover(&source, "limit = 7").as_deref(), Some("bind limit: int"), "{}", when);
        // Each is seen after its item, in the condition, the step and the body.
        assert_eq!(hover(&source, "limit ||").as_deref(), Some("bind limit: int"), "{}", when);
        assert_eq!(hover(&source, "j--").as_deref(), Some("hold j: int"), "{}", when);
        assert_eq!(hover(&source, "j\n").as_deref(), Some("hold j: int"), "{}", when);
    }
}

#[test]
fn the_parts_and_endless_are_colored_as_the_language_s_own() {
    for when in WHEN {
        let source = program(when);
        assert_eq!(class(&source, "|| i <"), TokenClass::Operator, "{}", when);
        assert_eq!(class(&source, "endless"), TokenClass::Keyword, "{}", when);
        assert_eq!(hover(&source, "outer =>").as_deref(), Some("outer => loop"), "{}", when);
    }
}

#[test]
fn a_start_s_names_live_only_in_the_loop() {
    let offered = labels(&PROGRAM.replace("    CHECK\n", "    @@\n"));
    assert!(offered.iter().any(|label| label == "total"), "{:?}", offered);
    assert!(!offered.iter().any(|label| label == "i" || label == "limit"), "{:?}", offered);
    assert!(offered.iter().any(|label| label == "endless"), "{:?}", offered);
    let inside = labels(&PROGRAM.replace("        total += i * j\n", "        @@\n"));
    assert!(inside.iter().any(|label| label == "limit"), "{:?}", inside);
}

#[test]
fn endless_is_a_loop_that_break_stops() {
    let offered = labels(&PROGRAM.replace("        break outer\n", "        @@\n"));
    assert!(offered.iter().any(|label| label == "break"), "{:?}", offered);
    let named = labels(&PROGRAM.replace("break outer", "break @@"));
    assert_eq!(named, ["outer"]);
}

#[test]
fn values_in_a_start_are_no_names_of_its_own() {
    let source = "\
task sum3(a: int, b: int, c: int) -> int
    <- a + b + c
fi
task main()
    hold p = 1
    hold q = 2
    hold yy = 4
    loop hold i = 0, n = sum3(p, q, 3), a, b = p, yy || i < n || i++
        show(i, q, yy, a, b)
    fi
fi
";
    assert_eq!(Analysis::new(source).diagnostics(), Vec::new());
    assert_eq!(hover(source, "q, yy, a").as_deref(), Some("hold q: int"));
    assert_eq!(hover(source, "yy, a, b)").as_deref(), Some("hold yy: int"));
    assert_eq!(hover(source, "b)").as_deref(), Some("hold b: int"));
}
