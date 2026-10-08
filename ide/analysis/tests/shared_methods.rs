//! Shared methods in the editor (`shared new(…) -> X`): each is a method of the
//! shape or kind itself, in the outline under it, colored as a method where it
//! is declared and where `X.new(…)` calls it, hovered with `shared`, completed
//! after the name of its shape or kind and never after a value's; all of it
//! holds while the file doesn't check. In the sources below, `@@` marks the
//! cursor.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, CompletionItem, LineIndex, SymbolKind, TokenClass};

const PROGRAM: &str = "\
shape Point
    x: int
    shared read made: int = 0

    shared new(x: int) -> Point
        Point.made = Point.made + 1
        <- Point { x: x }
    fi

    priv shared checked(x: int) -> int
        <- x
    fi

    moved(by: int) -> Point
        <- Point.new(.x + by)
    fi
fi

kind Light
    Red
    Green

    shared start() -> Light
        <- Light.Red
    fi
fi

task main()
    CHECK
    fixed p = Point.new(1)
    fixed light = Light.start()
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

fn class_at(source: &str, needle: &str) -> TokenClass {
    let from = LineIndex::new(source).utf16_of(source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle)));
    Analysis::new(source).tokens().into_iter().find(|t| t.from == from).unwrap_or_else(|| panic!("no token at {:?}", needle)).class
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    assert_eq!(Analysis::new(&program(true, "show(p, light)")).diagnostics(), Vec::new());
}

#[test]
fn shared_methods_are_methods_of_their_shape_or_kind_in_the_outline() {
    for checks in [true, false] {
        let source = program(checks, "show(p)");
        let outline = Analysis::new(&source).outline();
        let point: Vec<(&str, SymbolKind)> = outline[0].children.iter().map(|c| (c.name.as_str(), c.kind)).collect();
        assert_eq!(point, [("x", SymbolKind::Field), ("made", SymbolKind::Field), ("new", SymbolKind::Method), ("checked", SymbolKind::Method), ("moved", SymbolKind::Method)]);
        // The kind and the task after them are still found: each method's
        // `fi` closes the method, not the shape.
        let names: Vec<&str> = outline.iter().map(|item| item.name.as_str()).collect();
        assert_eq!(names, ["Point", "Light", "main"]);
        assert_eq!(outline[1].children.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["Red", "Green", "start"]);
    }
}

#[test]
fn a_shared_method_is_colored_as_a_method_where_declared_and_called() {
    for checks in [true, false] {
        let source = program(checks, "show(p)");
        assert_eq!(class_at(&source, "new(x"), TokenClass::Method);
        assert_eq!(class_at(&source, "new(1)"), TokenClass::Method);
        assert_eq!(class_at(&source, "new(.x"), TokenClass::Method);
        assert_eq!(class_at(&source, "start()\n    show"), TokenClass::Method);
        assert_eq!(class_at(&source, "checked(x"), TokenClass::Method);
    }
}

#[test]
fn a_shared_method_is_hovered_with_its_word() {
    for checks in [true, false] {
        let source = program(checks, "show(p)");
        let (code, doc) = hover(&source, "new(1)");
        assert_eq!(code, "shared new(x: int) -> Point");
        assert!(doc.contains("called on 'Point' itself"), "{}", doc);
        let (code, _) = hover(&source, "start()\n    show");
        assert_eq!(code, "shared start() -> Light");
        // What it makes is known where it is bound.
        assert_eq!(hover(&source, "p = Point").0, "bind p: Point");
    }
}

#[test]
fn shared_methods_are_completed_after_the_name_and_never_after_a_value() {
    for checks in [true, false] {
        let on_shape = labels(&completions(&program(checks, "fixed q = Point.@@"))).into_iter().map(str::to_string).collect::<Vec<_>>();
        assert!(on_shape.contains(&"new".to_string()) && on_shape.contains(&"made".to_string()), "{:?}", on_shape);
        // A private one is its file's, and this is its file.
        assert!(on_shape.contains(&"checked".to_string()), "{:?}", on_shape);
        assert!(!on_shape.contains(&"moved".to_string()) && !on_shape.contains(&"x".to_string()), "{:?}", on_shape);
        let on_kind = labels(&completions(&program(checks, "fixed l = Light.@@"))).into_iter().map(str::to_string).collect::<Vec<_>>();
        assert!(on_kind.contains(&"start".to_string()) && on_kind.contains(&"Red".to_string()), "{:?}", on_kind);
        let on_value = labels(&completions(&program(checks, "fixed q = p.@@"))).into_iter().map(str::to_string).collect::<Vec<_>>();
        assert!(on_value.contains(&"moved".to_string()) && !on_value.contains(&"new".to_string()), "{:?}", on_value);
    }
}

#[test]
fn a_shared_method_leads_to_its_declaration() {
    let source = program(true, "show(p)");
    let analysis = Analysis::new(&source);
    let index = LineIndex::new(&source);
    let called = index.utf16_of(source.find("new(1)").unwrap());
    let declared = index.utf16_of(source.find("new(x").unwrap());
    assert_eq!(analysis.definition(called).map(|l| l.from), Some(declared));
}

#[test]
fn a_kind_shared_method_named_without_a_call_is_a_method_not_a_variant() {
    for checks in [true, false] {
        let source = program(checks, "fixed made = Light.start\n    fixed red = Light.Red");
        assert_eq!(class_at(&source, "start\n"), TokenClass::Method);
        assert_eq!(class_at(&source, "Red\nfi"), TokenClass::Variant);
        assert_eq!(hover(&source, "start\n").0, "shared start() -> Light");
        let analysis = Analysis::new(&source);
        let index = LineIndex::new(&source);
        let named = index.utf16_of(source.find("start\n").unwrap());
        let declared = index.utf16_of(source.find("start() -> Light").unwrap());
        assert_eq!(analysis.definition(named).map(|l| l.from), Some(declared));
    }
}
