#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, LineIndex};

const PROGRAM: &str = "\
shape Point
    x: int = 1
fi

shape Box<T>
    count: int = 0
fi

task main()
    FIRST
    region r default
    fixed a = new [text]
    fixed b = new [text: int] at r
    fixed c = new@r [int]
    fixed p = new Point
    fixed q = new Point at r
    fixed made = new [text: int]() at r
    fixed names = new@r [:int]()
    show(a.len, b.len, c.len, p^.x, q^.x, made.len, names.len)
    @@
fi
";

const FIRSTS: [&str; 3] = ["show(0)", "show(unknown)", "fixed = ="];

fn program(first: &str, code: &str) -> String {
    PROGRAM.replace("FIRST", first).replace("@@", code)
}

fn labels(marked: &str) -> Vec<String> {
    let at = marked.find("@@").expect("a @@ marker");
    let source = marked.replacen("@@", "", 1);
    Analysis::new(&source).complete(LineIndex::new(&source).utf16_of(at)).items.into_iter().map(|item| item.label).collect()
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
fn hover_shows_what_a_new_without_parentheses_makes() {
    for first in FIRSTS {
        let source = program(first, "");
        // Where nothing names the region, only the checker knows the default.
        let checked = first == "show(0)" || first == "show(unknown)";
        let region = if checked { "@r" } else { "" };
        assert_eq!(hover(&source, "a ="), format!("fixed a: [text]{}", region), "{}", first);
        assert_eq!(hover(&source, "p ="), format!("fixed p: ^Point{}", region), "{}", first);
        assert_eq!(hover(&source, "b ="), "fixed b: [text: int]@r", "{}", first);
        assert_eq!(hover(&source, "c ="), "fixed c: [int]@r", "{}", first);
        assert_eq!(hover(&source, "q ="), "fixed q: ^Point@r", "{}", first);
        assert_eq!(hover(&source, "made ="), "fixed made: [text: int]@r", "{}", first);
        assert_eq!(hover(&source, "names ="), "fixed names: [text: int]@r", "{}", first);
    }
}

#[test]
fn completion_offers_the_region_after_a_new_without_parentheses() {
    for first in FIRSTS {
        for code in ["fixed z = new [int] @@", "fixed z = new Point @@", "fixed z = new [text: int] @@", "fixed z = new Box<int> @@", "fixed z = new [:int]() @@"] {
            let items = labels(&program(first, code));
            assert!(items.contains(&"at".to_string()), "{} in {:?} ({})", code, items, first);
            // Not the `~` that goes on a written type: a `new` makes no error.
            assert!(!items.contains(&"~".to_string()), "{} in {:?} ({})", code, items, first);
        }
        assert_eq!(labels(&program(first, "fixed z = new [int] at @@")), ["r"], "{}", first);
        for code in ["fixed z = new Point at r @@", "fixed z = new@r [int] @@"] {
            assert!(!labels(&program(first, code)).contains(&"at".to_string()), "{} ({})", code, first);
        }
        assert!(!labels(&program(first, "show(new [int], p @@")).contains(&"at".to_string()), "{}", first);
    }
}
