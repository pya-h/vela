//! `break` and `continue`, and loops named for them, `outer => each …`: the
//! names' classes and hovers, and what is offered after the words, whether or
//! not the file checks.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, LineIndex, TokenClass};

const PROGRAM: &str = "\
task main()
    hold total = 0
    outer => each a in 0..4
        inner => loop total < 100
            total = total + a
            when total > 50
                break outer
            fi
            continue inner
        fi
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

fn hover(source: &str, needle: &str) -> String {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    Analysis::new(source).hover(LineIndex::new(source).utf16_of(byte)).unwrap_or_else(|| panic!("no hover at {:?}", needle)).code
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
fn a_loop_s_name_hovers_as_one_where_it_is_given_and_where_it_is_used() {
    for when in WHEN {
        let source = program(when);
        assert_eq!(hover(&source, "outer =>"), "outer => loop", "{}", when);
        assert_eq!(hover(&source, "outer\n"), "outer => loop", "{}", when);
        assert_eq!(hover(&source, "inner\n"), "inner => loop", "{}", when);
        assert_eq!(class(&source, "outer =>"), TokenClass::Variable, "{}", when);
        assert_eq!(class(&source, "break"), TokenClass::Keyword, "{}", when);
        assert_eq!(class(&source, "continue"), TokenClass::Keyword, "{}", when);
        // The loop's variable is still declared after its name.
        assert_eq!(hover(&source, "a in"), "each a: int", "{}", when);
    }
}

#[test]
fn break_and_continue_are_offered_inside_a_loop_and_take_the_names_of_the_loops_around_them() {
    let inside = PROGRAM.replace("            continue inner\n", "            @@\n");
    let offered = labels(&inside);
    assert!(offered.iter().any(|label| label == "break") && offered.iter().any(|label| label == "continue"), "{:?}", offered);
    // A loop's name is no value.
    assert!(!offered.iter().any(|label| label == "outer"), "{:?}", offered);
    let outside = PROGRAM.replace("    CHECK\n", "    @@\n");
    let offered = labels(&outside);
    assert!(!offered.iter().any(|label| label == "break" || label == "continue"), "{:?}", offered);
    let named = PROGRAM.replace("continue inner", "continue @@");
    assert_eq!(labels(&named), ["inner", "outer"]);
}
