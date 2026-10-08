//! Loops that give a value, `fixed r = each x in xs … fi`: the type they give,
//! `break r` offered by the variable's name, and `LoopResult` from
//! `std/loops`.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, LineIndex};

const PROGRAM: &str = "\
include std/loops {LoopResult}

task main()
    fixed found = each x in 0..10
        each y in 0..10
            when x * y == 12
                break found = x
            fi
        fi
        x
    fi
    fixed sum: int ~ LoopResult = loop flex i = 0, s = 0 || i < 4 || i++
        s += i
        s
    fi
    show(found { 0 }, sum { 0 })
fi
";

fn hover(source: &str, needle: &str) -> Option<String> {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    Analysis::new(source).hover(LineIndex::new(source).utf16_of(byte)).map(|hover| hover.code)
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
    assert_eq!(Analysis::new(PROGRAM).diagnostics(), Vec::new());
}

#[test]
fn a_loop_that_gives_a_value_hovers_with_what_it_fails_with() {
    assert_eq!(hover(PROGRAM, "found =").as_deref(), Some("fixed found: int ~ LoopResult<int, bool>"));
    assert_eq!(hover(PROGRAM, "sum:").as_deref(), Some("fixed sum: int ~ LoopResult<bool, bool>"));
}

#[test]
fn the_variable_a_loop_gives_names_it_for_break() {
    let offered = labels(&PROGRAM.replace("break found = x", "break @@"));
    assert!(offered.iter().any(|label| label == "found"), "{:?}", offered);
}

#[test]
fn a_variable_written_with_its_type_names_its_loop_too() {
    let offered = labels(&PROGRAM.replace("        s += i\n", "        s += i\n        break @@\n"));
    assert!(offered.iter().any(|label| label == "sum"), "{:?}", offered);
}

#[test]
fn a_loop_named_by_its_variable_hovers_as_one() {
    assert_eq!(hover(PROGRAM, "found = x").as_deref(), Some("found = loop"));
}
