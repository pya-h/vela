//! Parameters that fall back on values, `width: int = 8`, in the editor: the
//! types hover shows with the values written after them, and the call a
//! completion fills in, which gives only the parameters a call must give,
//! whether the file checks, stops the checker, or doesn't parse. In the
//! sources below, `@@` marks the cursor.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, CompletionItem, LineIndex};

const PROGRAM: &str = "\
task pad(t: text, width: int = 8, fill: text = \", )\", f: (int) -> int = (x: int) -> int <- x fi) -> int
    <- width
fi

shape Box
    v: int = 0

    grown(by: int = 1) -> int
        <- .v + by
    fi

    shared made(v: int, extra: int = 0) -> Box
        <- Box { v: v + extra }
    fi
fi

task main()
    FIRST
    fixed b = Box.made(1)
    fixed w = pad(\"a\")
    fixed g = pad
    fixed n = b.grown()
    show(w, n)
    @@
fi
";

/// The first lines `main` may start with: one that checks, one that stops
/// the checker, and one that stops the parser.
const FIRSTS: [&str; 3] = ["show(0)", "show(unknown)", "fixed = ="];

fn program(first: &str, code: &str) -> String {
    PROGRAM.replace("FIRST", first).replace("@@", code)
}

fn completions(marked: &str) -> Vec<CompletionItem> {
    let at = marked.find("@@").expect("a @@ marker");
    let source = marked.replacen("@@", "", 1);
    Analysis::new(&source).complete(LineIndex::new(&source).utf16_of(at)).items
}

fn item(marked: &str, label: &str) -> CompletionItem {
    completions(marked).into_iter().find(|item| item.label == label).unwrap_or_else(|| panic!("no {} in {}", label, marked))
}

fn hover(source: &str, needle: &str) -> String {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    Analysis::new(source).hover(LineIndex::new(source).utf16_of(byte)).map(|hover| hover.code).unwrap_or_default()
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    let source = program("show(0)", "");
    assert_eq!(Analysis::new(&source).diagnostics(), Vec::new());
}

#[test]
fn hover_shows_the_types_of_parameters_with_values_to_fall_back_on() {
    for first in FIRSTS {
        let source = program(first, "");
        // The header as written, values and all.
        assert!(hover(&source, "pad(t").contains("width: int = 8"), "{}", first);
        assert_eq!(hover(&source, "width:"), "width: int", "{}", first);
        assert_eq!(hover(&source, "fill:"), "fill: text", "{}", first);
        assert_eq!(hover(&source, "w ="), "fixed w: int", "{}", first);
        // A task named as a value takes every value, as its type says.
        assert_eq!(hover(&source, "g ="), "fixed g: (text, int, text, (int) -> int) -> int", "{}", first);
        assert_eq!(hover(&source, "n ="), "fixed n: int", "{}", first);
    }
}

#[test]
fn completion_fills_in_only_the_values_a_call_must_give() {
    for first in FIRSTS {
        assert_eq!(item(&program(first, "pa@@"), "pad").snippet.as_deref(), Some("pad(${t})"), "{}", first);
        // A method whose values all fall back is called with none.
        let grown = item(&program(first, "show(b.@@"), "grown");
        assert_eq!((grown.apply.as_deref(), grown.snippet), (Some("grown()"), None), "{}", first);
        assert_eq!(item(&program(first, "show(Box.@@"), "made").snippet.as_deref(), Some("made(${v})"), "{}", first);
        // A value given still has the type of its parameter.
        let values: Vec<String> = completions(&program(first, "show(pad(\"a\", @@")).into_iter().map(|item| item.label).collect();
        assert!(values.contains(&"w".to_string()), "{:?} ({})", values, first);
        // The lambda's parameter is its own, not the task's.
        let body = program(first, "").replace("    <- width\n", "    <- wi@@\n");
        let names: Vec<String> = completions(&body).into_iter().map(|item| item.label).collect();
        assert!(names.contains(&"width".to_string()) && !names.contains(&"x".to_string()), "{:?} ({})", names, first);
    }
}
