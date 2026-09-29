//! The byte-sized types in the editor: `unit`, a whole number from 0 to 255,
//! and `char`, a character, with `t[i]` and `t.at(i)`, hovered, completed, and
//! colored with the compiler's types, and with the same types
//! read from the declarations while the file doesn't check. In the sources
//! below, `@@` marks the cursor.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, CompletionItem, LineIndex, TokenClass};

const PROGRAM: &str = "\
task twice(b: unit) -> unit
    <- b * 2
fi

task main()
    CHECK
    hold b: unit = 200
    bind next = b + 1
    bind wide = int(b)
    bind n = 7
    bind cast = unit(n)
    bind doubled = twice(3)
    hold c = 'a'
    bind next_char = c + 1
    bind apart = 'z' - c
    bind word = \"hello\"
    bind first = word[0]
    bind found = word.at(9)
    bind quote = '\\''
    @@
fi
";

fn program(checks: bool, code: &str) -> String {
    let first = if checks { "show(0)" } else { "show(unknown)" };
    PROGRAM.replace("CHECK", first).replace("    @@\n", &format!("    {}\n", code))
}

fn completions(marked: &str) -> Vec<CompletionItem> {
    let at = marked.find("@@").expect("a @@ marker");
    let source = marked.replacen("@@", "", 1);
    Analysis::new(&source).complete(LineIndex::new(&source).utf16_of(at)).items
}

fn labels(marked: &str) -> Vec<String> {
    completions(marked).into_iter().map(|item| item.label).collect()
}

fn hover(source: &str, needle: &str) -> (String, String) {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    let hover = Analysis::new(source).hover(LineIndex::new(source).utf16_of(byte)).unwrap_or_else(|| panic!("no hover at {:?}", needle));
    (hover.code, hover.doc)
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    let source = program(true, "show(next, wide, cast, doubled, next_char, apart, first, found, quote)");
    assert_eq!(Analysis::new(&source).diagnostics(), Vec::new());
}

#[test]
fn units_hover_with_their_type_whether_or_not_the_file_checks() {
    for checks in [true, false] {
        let source = program(checks, "show(0)");
        let code = |needle: &str| hover(&source, needle).0;
        let when = if checks { "checked" } else { "declared" };
        assert_eq!(code("b: unit ="), "hold b: unit", "{}", when);
        assert_eq!(code("next ="), "bind next: unit", "{}", when);
        assert_eq!(code("wide ="), "bind wide: int", "{}", when);
        assert_eq!(code("cast ="), "bind cast: unit", "{}", when);
        assert_eq!(code("doubled ="), "bind doubled: unit", "{}", when);
    }
    let source = program(true, "show(0)");
    assert!(hover(&source, "unit =").1.starts_with("A whole number from 0 to 255"));
}

#[test]
fn unit_is_offered_as_a_type_and_for_a_scan() {
    assert!(labels("task f(a: @@").contains(&"unit".to_string()));
    let items = completions(&program(true, "bind age = scan(\"Age: \", @@)"));
    assert!(items.iter().any(|item| item.label == "unit"), "{:?}", items);
}

#[test]
fn a_unit_has_str_after_a_dot() {
    for checks in [true, false] {
        let found = labels(&program(checks, "b.@@"));
        assert!(found.contains(&"str".to_string()), "{:?}", found);
    }
}

#[test]
fn a_unit_mixed_with_an_int_is_an_error_in_the_editor_too() {
    let source = program(true, "show(n + b)");
    let diagnostics = Analysis::new(&source).diagnostics();
    assert!(diagnostics.iter().any(|d| d.message.starts_with("an int and a unit don't mix")), "{:?}", diagnostics);
}

#[test]
fn chars_hover_with_their_type_whether_or_not_the_file_checks() {
    for checks in [true, false] {
        let source = program(checks, "show(0)");
        let code = |needle: &str| hover(&source, needle).0;
        let when = if checks { "checked" } else { "declared" };
        assert_eq!(code("c = 'a'"), "hold c: char", "{}", when);
        assert_eq!(code("next_char ="), "bind next_char: char", "{}", when);
        assert_eq!(code("apart ="), "bind apart: int", "{}", when);
        assert_eq!(code("first ="), "bind first: char", "{}", when);
        assert_eq!(code("found ="), "bind found: maybe char", "{}", when);
    }
}

#[test]
fn a_char_is_colored_as_text_and_its_quote_escape_is_valid() {
    let source = program(true, "show(quote)");
    let analysis = Analysis::new(&source);
    let class_at = |needle: &str, offset: usize| {
        let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle)) + offset;
        analysis.tokens().into_iter().find(|t| t.from <= byte && byte < t.to).map(|t| t.class)
    };
    assert_eq!(class_at("'a'", 1), Some(TokenClass::String));
    assert_eq!(class_at("'\\''", 1), Some(TokenClass::Escape));
    assert_eq!(class_at("at(9)", 0), Some(TokenClass::Method));
}

#[test]
fn text_offers_at_and_a_char_str() {
    for checks in [true, false] {
        let found = labels(&program(checks, "word.@@"));
        assert!(found.contains(&"at".to_string()), "{:?}", found);
        let found = labels(&program(checks, "c.@@"));
        assert!(found.contains(&"str".to_string()), "{:?}", found);
    }
    assert!(labels("task f(a: @@").contains(&"char".to_string()));
}

#[test]
fn nothing_is_offered_inside_a_char() {
    assert!(completions(&program(true, "show('a@@')")).is_empty());
    assert!(completions(&program(true, "show('@@")).is_empty());
}
