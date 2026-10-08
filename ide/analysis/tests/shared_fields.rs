//! A shape's shared fields in the editor (`shared made: int = 0`): each is a
//! field of the shape itself, colored and hovered as one where it is declared
//! and where `Shape.f` reads it, completed after the shape's name and never
//! after a value's, and never offered in a literal; all of it holds while the
//! file doesn't check. In the sources below, `@@` marks the cursor.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, CompletionItem, LineIndex, TokenClass};

const PROGRAM: &str = "\
shape Meter
    count: int = 0
    shared start: int = 7
    shared flex made: int = 0
    shared read seen: int = 0
    priv shared flex secret: int = 1

    alters tick()
        .count = .count + Meter.start
        Meter.seen = Meter.seen + 1
    fi
fi

task main()
    CHECK
    flex meter = Meter {}
    meter.tick()
    Meter.made = Meter.made + 1
    fixed counted = Meter.made
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

fn hover(source: &str, needle: &str) -> (String, String) {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    let hover = Analysis::new(source).hover(LineIndex::new(source).utf16_of(byte)).unwrap_or_else(|| panic!("no hover at {:?}", needle));
    (hover.code, hover.doc)
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    assert_eq!(Analysis::new(&program(true, "show(counted)")).diagnostics(), Vec::new());
}

#[test]
fn a_shared_field_hovers_with_its_words_and_says_how_it_is_read() {
    for checks in [true, false] {
        let source = program(checks, "show(counted)");
        let when = if checks { "checked" } else { "declared" };
        let (code, doc) = hover(&source, "start: int");
        assert_eq!(code, "shared start: int", "{}", when);
        assert!(doc.contains("read as Meter.start") && doc.contains("never changes"), "{}: {}", when, doc);
        let (code, doc) = hover(&source, "made: int");
        assert_eq!(code, "shared flex made: int", "{}", when);
        assert!(doc.contains("Any code that sees it may change it"), "{}: {}", when, doc);
        let (code, doc) = hover(&source, "seen: int");
        assert_eq!(code, "shared read seen: int", "{}", when);
        assert!(doc.contains("only the methods of its shape change it"), "{}: {}", when, doc);
        assert_eq!(hover(&source, "secret: int").0, "priv shared flex secret: int", "{}", when);
        // The shape lists them with their words.
        let shape = hover(&source, "Meter\n").0;
        assert!(shape.contains("    shared start: int\n") && shape.contains("    shared read seen: int\n"), "{}: {}", when, shape);
        assert!(shape.contains("    priv shared flex secret: int\n"), "{}: {}", when, shape);
        // An ordinary field beside them is as it was.
        assert_eq!(hover(&source, "count: int").0, "count: int", "{}", when);
    }
    // Where the program reads one, the hover is the field's.
    let source = program(true, "show(counted)");
    assert_eq!(hover(&source, "made + 1").0, "shared flex made: int");
    assert_eq!(hover(&source, "start\n").0, "shared start: int");
}

#[test]
fn the_words_are_keywords_and_the_names_fields() {
    let source = program(true, "show(counted)");
    let analysis = Analysis::new(&source);
    let class_at = |needle: &str| {
        let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
        analysis.tokens().into_iter().find(|t| t.from <= byte && byte < t.to).map(|t| t.class)
    };
    assert_eq!(class_at("shared start"), Some(TokenClass::Keyword));
    assert_eq!(class_at("flex made"), Some(TokenClass::Keyword));
    assert_eq!(class_at("read seen"), Some(TokenClass::Keyword));
    assert_eq!(class_at("start: int"), Some(TokenClass::Field));
    assert_eq!(class_at("seen: int"), Some(TokenClass::Field));
    // `Meter.made`: the shape, then its field.
    assert_eq!(class_at("Meter.made = "), Some(TokenClass::Type));
    assert_eq!(class_at("made = Meter"), Some(TokenClass::Field));
}

#[test]
fn the_shapes_name_completes_its_shared_fields_and_a_value_never_does() {
    for checks in [true, false] {
        let on_shape = completions(&program(checks, "show(Meter.@@)"));
        assert_eq!(labels(&on_shape), ["start", "made", "seen", "secret"], "checks: {}", checks);
        let on_value = completions(&program(checks, "show(meter.@@)"));
        let offered = labels(&on_value);
        assert!(offered.contains(&"count") && offered.contains(&"tick"), "checks: {}: {:?}", checks, offered);
        assert!(!offered.iter().any(|label| ["start", "made", "seen", "secret"].contains(label)), "checks: {}: {:?}", checks, offered);
    }
}

#[test]
fn a_literal_never_offers_a_shared_field() {
    for checks in [true, false] {
        let inside = completions(&program(checks, "flex other = Meter { @@ }"));
        assert_eq!(labels(&inside), ["count"], "checks: {}", checks);
        let items = completions(&program(checks, "flex other = Me@@"));
        let meter = items.iter().find(|item| item.label == "Meter").unwrap_or_else(|| panic!("no Meter: {:?}", items));
        assert_eq!(meter.snippet.as_deref(), Some("Meter {}"), "checks: {}", checks);
    }
}

#[test]
fn changing_one_where_it_can_not_be_is_reported() {
    let cases = [
        ("Meter.start = 1", "'Meter.start' never changes (declared at 3:12); a shared field that changes is 'shared flex start', or 'shared read start', which only the methods of 'Meter' change"),
        ("Meter.seen = 1", "'Meter.seen' is a read-only shared field, which only the methods of 'Meter' change"),
        ("show(meter.made)", "'made' is a shared field of 'Meter', one value for the whole program rather than part of each value, so it is read as 'Meter.made'"),
    ];
    for (code, message) in cases {
        let diagnostics = Analysis::new(&program(true, code)).diagnostics();
        let [diagnostic] = diagnostics.as_slice() else { panic!("{}: {:?}", code, diagnostics) };
        assert_eq!(diagnostic.message, message, "{}", code);
    }
}

#[test]
fn a_shapes_member_line_offers_the_words_of_a_shared_field() {
    let shape = |line: &str| format!("shape Meter\n    count: int = 0\n    {}\nfi\n\ntask main()\nfi\n", line);
    assert!(labels(&completions(&shape("@@"))).contains(&"shared"));
    assert!(labels(&completions(&shape("priv @@"))).contains(&"shared"));
    // Or `new`, the name of a shared method.
    assert_eq!(labels(&completions(&shape("shared @@"))), ["flex", "read", "new"]);
    assert_eq!(labels(&completions(&shape("priv shared @@"))), ["flex", "read", "new"]);
    // A kind has no fields, so no shared ones, but shared methods.
    let kind = "kind Mood\n    Calm\n    shared @@\nfi\n";
    assert_eq!(labels(&completions(kind)), ["new"]);
}
