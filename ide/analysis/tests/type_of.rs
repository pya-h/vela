//! `x.type` and numbers in other bases in the editor: `type` is a reserved
//! word, colored and hovered as one after a dot, offered after every value's
//! dot; `0x1F`, `0o17`, `0b101` are numbers, and a wrong one is reported. In
//! the sources below, `@@` marks the cursor.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, CompletionItem, LineIndex, TokenClass};

const PROGRAM: &str = "\
shape Point
    x: int
fi

task main()
    CHECK
    fixed xs = [0x1F, 0o17, 0b101]
    fixed p = Point { x: 0xff_ff }
    fixed word = xs.type
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

fn labels(items: &[CompletionItem]) -> Vec<&str> {
    items.iter().map(|item| item.label.as_str()).collect()
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    assert_eq!(Analysis::new(&program(true, "show(word, p.x)")).diagnostics(), Vec::new());
}

#[test]
fn type_is_a_keyword_and_numbers_in_other_bases_are_numbers() {
    let source = program(true, "show(word)");
    let analysis = Analysis::new(&source);
    let class_at = |needle: &str| {
        let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
        analysis.tokens().into_iter().find(|t| t.from <= byte && byte < t.to).map(|t| t.class)
    };
    assert_eq!(class_at("type\n"), Some(TokenClass::Keyword));
    assert_eq!(class_at("0x1F"), Some(TokenClass::Number));
    assert_eq!(class_at("0b101"), Some(TokenClass::Number));
    assert_eq!(class_at("0xff_ff"), Some(TokenClass::Number));
}

#[test]
fn type_hovers_with_what_it_reads_and_word_is_text() {
    for checks in [true, false] {
        let source = program(checks, "show(word)");
        let byte = source.find("type\n").unwrap();
        let hover = Analysis::new(&source).hover(LineIndex::new(&source).utf16_of(byte)).unwrap();
        assert_eq!(hover.code, "x.type: text", "checks: {}", checks);
        assert!(hover.doc.starts_with("The type of the value as code writes it"), "{}", hover.doc);
        let byte = source.find("word =").unwrap();
        let hover = Analysis::new(&source).hover(LineIndex::new(&source).utf16_of(byte)).unwrap();
        assert_eq!(hover.code, "fixed word: text", "checks: {}", checks);
    }
}

#[test]
fn every_value_offers_its_type_and_a_shape_or_statement_start_does_not() {
    for checks in [true, false] {
        for value in ["xs", "p", "p.x", "word", "xs[0]"] {
            let offered = completions(&program(checks, &format!("show({}.@@)", value)));
            assert!(labels(&offered).contains(&"type"), "{} checks {}: {:?}", value, checks, labels(&offered));
        }
        // A shape's name is no value: it offers its shared fields only.
        assert!(!labels(&completions(&program(checks, "show(Point.@@)"))).contains(&"type"), "checks {}", checks);
        assert!(!labels(&completions(&program(checks, "@@"))).contains(&"type"), "checks {}", checks);
    }
}

#[test]
fn mistakes_with_either_are_reported() {
    let cases = [
        ("show(0X1F)", "invalid number '0X1F': its base is written in lowercase, as in '0x1F'"),
        ("show(0b12)", "invalid number '0b12': '2' is not a binary digit (0 or 1)"),
        ("fixed type = 1", "expected a name, got 'type', which is a reserved word; every value has '.type', its type as code writes it, so nothing else takes the name"),
        ("show(p.type())", "'type' is the type of a value, read without '(…)', as in 'x.type'"),
    ];
    for (code, message) in cases {
        let diagnostics = Analysis::new(&program(true, code)).diagnostics();
        let [diagnostic] = diagnostics.as_slice() else { panic!("{}: {:?}", code, diagnostics) };
        assert_eq!(diagnostic.message, message, "{}", code);
    }
}
