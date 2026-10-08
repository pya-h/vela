//! `break` and `continue`, and loops named for them, `outer => each …`: the
//! names' classes and hovers, and what is offered after the words, whether or
//! not the file checks.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, LineIndex, TokenClass};

const PROGRAM: &str = "\
task main()
    flex total = 0
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

#[test]
fn a_lambda_inside_a_loop_is_a_task_of_its_own() {
    let program = "task main()\n    outer => each a in 0..4\n        fixed f = (x: int) -> int\n            INSIDE\n            <- x\n        fi\n        AFTER\n    fi\nfi\n";
    let offers_jumps = |marked: &str| labels(marked).iter().any(|label| label == "break");
    assert!(!offers_jumps(&program.replace("INSIDE", "@@").replace("AFTER", "show(1)")));
    assert!(offers_jumps(&program.replace("            INSIDE\n", "").replace("AFTER", "@@")));
    let one_line = "task main()\n    each a in 0..4\n        fixed f = (x: int) -> int <- x * 2 fi\n        @@\n    fi\nfi\n";
    assert!(offers_jumps(one_line));
    // Only the lambda's own loops are named in it.
    let named = program.replace("INSIDE", "inner => each j in 0..2\n                break @@\n            fi").replace("AFTER", "show(1)");
    assert_eq!(labels(&named), ["inner"]);
}

#[test]
fn what_an_each_goes_through_is_outside_its_loop() {
    let source = "task main()\n    fixed xs = [1, 2]\n    each x in xs[#5] { @@\n    }\n        show(x)\n    fi\nfi\n";
    assert!(!labels(source).iter().any(|label| label == "break"));
}

#[test]
fn only_a_loop_is_offered_after_a_loop_s_name() {
    let offered = labels("task main()\n    outer => @@\nfi\n");
    assert!(offered.iter().all(|label| label.starts_with("loop") || label == "each" || label == "endless"), "{:?}", offered);
    for word in ["loop", "each", "endless", "loop … || … || …"] {
        assert!(offered.iter().any(|label| label == word), "{:?}", offered);
    }
}

#[test]
fn a_loop_s_name_is_no_value() {
    let source = "task main()\n    outer => each a in 0..4\n        show(outer)\n    fi\nfi\n";
    let byte = source.find("outer)").expect("a use");
    assert_eq!(Analysis::new(source).hover(LineIndex::new(source).utf16_of(byte)).map(|hover| hover.code), None);
}
