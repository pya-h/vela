//! Loops that give a value, `bind r = each x in xs … fi`: the type they give,
//! `break r` offered by the variable's name, and `LoopResult` from
//! `std/loops`.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, LineIndex};

const PROGRAM: &str = "\
include std/loops {LoopResult}

task main()
    bind found = each x in 0..10
        each y in 0..10
            when x * y == 12
                break found = x
            fi
        fi
        x
    fi
    bind sum: int ~ LoopResult = loop hold i = 0, s = 0 || i < 4 || i++
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
    assert_eq!(hover(PROGRAM, "found =").as_deref(), Some("bind found: int ~ LoopResult<int, bool>"));
    assert_eq!(hover(PROGRAM, "sum:").as_deref(), Some("bind sum: int ~ LoopResult<bool, bool>"));
}

#[test]
fn the_variable_a_loop_gives_names_it_for_break() {
    let offered = labels(&PROGRAM.replace("break found = x", "break @@"));
    assert!(offered.iter().any(|label| label == "found"), "{:?}", offered);
}
