//! Methods of shapes and kinds: they are outlined under their shape or kind,
//! colored, followed to their declarations, described on hover, and completed
//! after a dot, whether or not the file checks. In the sources below, `@@`
//! marks the cursor.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::collections::HashMap;

use vela_analysis::{Analysis, CompletionItem, CompletionKind, LineIndex, Location, SymbolKind, TokenClass};

const PROGRAM: &str = "\
shape Counter
    label: text
    count: int

    total() -> int
        <- .count
    fi

    nameOf() -> text@this
        <- .label
    fi

    alters step(by: int)
        .count = .count + by
        .cap()
    fi

    priv alters cap()
        when .count > 9
            .count = 9
        fi
    fi
fi

kind Light
    Red
    Green

    alters flip()
        match .this
            case Red
                .this = Green
            case Green
                .this = Red
        fi
    fi

    shown() -> text
        match .this
            case Red
                <- \"red\"
            case Green
                <- \"green\"
        fi
    fi
fi

task main()
    CHECK
    hold c = Counter { label: \"clicks\", count: 0 }
    c.step(2)
    hold light = Red
    light.flip()
    show(light.shown())
    @@
fi
";

/// `PROGRAM`, which checks or (with an unknown name above everything else in
/// `main`) doesn't, with `code` in place of its `@@` line.
fn program(checks: bool, code: &str) -> String {
    let first = if checks { "show(0)" } else { "show(unknown)" };
    PROGRAM.replace("CHECK", first).replace("    @@\n", &format!("    {}\n", code))
}

/// `PROGRAM` with `code` on a line of its own in the body of `step`.
fn in_step(checks: bool, code: &str) -> String {
    program(checks, "show(1)").replace("        .cap()\n", &format!("        .cap()\n        {}\n", code))
}

/// The UTF-16 offset of the `nth` (0-based) occurrence of `needle`.
fn at(source: &str, needle: &str, nth: usize) -> usize {
    let byte = source.match_indices(needle).nth(nth).unwrap_or_else(|| panic!("no {:?} #{}", needle, nth)).0;
    LineIndex::new(source).utf16_of(byte)
}

fn class_at(source: &str, needle: &str, nth: usize) -> TokenClass {
    let offset = at(source, needle, nth);
    Analysis::new(source).tokens().into_iter().find(|t| t.from == offset).unwrap_or_else(|| panic!("no token at {:?}", needle)).class
}

/// Asserts that the name at the `nth` occurrence of `name_use` is declared at
/// the `nth` occurrence of `declaration`.
#[track_caller]
fn assert_definition(source: &str, name_use: (&str, usize), declaration: (&str, usize)) {
    let name: String = declaration.0.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
    let found = Analysis::new(source).definition(at(source, name_use.0, name_use.1));
    let expected = at(source, declaration.0, declaration.1);
    assert_eq!(found, Some(Location { from: expected, to: expected + name.len(), path: None }), "{:?} should go to {:?}", name_use, declaration);
}

fn hover(source: &str, needle: &str, nth: usize) -> (String, String) {
    let hover = Analysis::new(source).hover(at(source, needle, nth)).unwrap_or_else(|| panic!("no hover at {:?}", needle));
    (hover.code, hover.doc)
}

fn completions(marked: &str) -> Vec<CompletionItem> {
    let at = marked.find("@@").expect("a @@ marker");
    let source = marked.replacen("@@", "", 1);
    Analysis::new(&source).complete(LineIndex::new(&source).utf16_of(at)).items
}

fn labels(marked: &str) -> Vec<String> {
    completions(marked).into_iter().map(|item| item.label).collect()
}

#[track_caller]
fn assert_offers(marked: &str, present: &[&str], absent: &[&str]) {
    let labels = labels(marked);
    for label in present {
        assert!(labels.iter().any(|l| l == label), "{:?} should be offered; got {:?}", label, labels);
    }
    for label in absent {
        assert!(!labels.iter().any(|l| l == label), "{:?} should not be offered; got {:?}", label, labels);
    }
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    assert_eq!(Analysis::new(&program(true, "show(1)")).diagnostics(), Vec::new());
    assert_eq!(Analysis::new(&program(false, "show(1)")).diagnostics().len(), 1);
}

#[test]
fn methods_are_outlined_under_their_shape_or_kind_below_what_it_holds() {
    let source = program(true, "show(1)");
    let outline = Analysis::new(&source).outline();
    let members = |name: &str| -> Vec<(String, SymbolKind, String)> {
        let item = outline.iter().find(|item| item.name == name).unwrap();
        item.children.iter().map(|child| (child.name.clone(), child.kind, child.detail.clone())).collect()
    };
    let owned = |list: &[(&str, SymbolKind, &str)]| -> Vec<(String, SymbolKind, String)> {
        list.iter().map(|(name, kind, detail)| (name.to_string(), *kind, detail.to_string())).collect()
    };
    assert_eq!(
        members("Counter"),
        owned(&[
            ("label", SymbolKind::Field, "text"),
            ("count", SymbolKind::Field, "int"),
            ("total", SymbolKind::Method, "() -> int"),
            ("nameOf", SymbolKind::Method, "() -> text@this"),
            ("step", SymbolKind::Method, "(by: int)"),
            ("cap", SymbolKind::Method, "()"),
        ])
    );
    assert_eq!(
        members("Light"),
        owned(&[
            ("Red", SymbolKind::Variant, ""),
            ("Green", SymbolKind::Variant, ""),
            ("flip", SymbolKind::Method, "()"),
            ("shown", SymbolKind::Method, "() -> text"),
        ])
    );
    // A method's `fi` closes the method, not its shape: `main` is outlined too,
    // and a method covers its lines from its first word.
    assert!(outline.iter().any(|item| item.name == "main"));
    let counter = outline.iter().find(|item| item.name == "Counter").unwrap();
    let cap = counter.children.iter().find(|child| child.name == "cap").unwrap();
    assert_eq!(cap.from, at(&source, "priv alters cap", 0));
    assert!(source[cap.from..cap.to].ends_with("            .count = 9\n        fi\n    fi"));
}

#[test]
fn methods_their_words_and_their_parameters_are_colored() {
    for checks in [true, false] {
        let source = program(checks, "show(1)");
        let when = if checks { "checked" } else { "declared" };
        let cases: &[(&str, usize, TokenClass)] = &[
            ("alters step", 0, TokenClass::Keyword),
            ("step(by", 0, TokenClass::Method),
            ("by: int", 0, TokenClass::Parameter),
            ("by\n", 0, TokenClass::Parameter),
            ("count = .count", 0, TokenClass::Field),
            ("cap()\n", 0, TokenClass::Method),
            ("this\n", 0, TokenClass::Region),
            ("this\n", 1, TokenClass::Keyword),
            ("this = Green", 0, TokenClass::Keyword),
            ("step(2)", 0, TokenClass::Method),
            ("flip()\n", 1, TokenClass::Method),
            ("shown())", 0, TokenClass::Method),
            // A kind's method is not read as a variant.
            ("shown() -> text", 0, TokenClass::Method),
        ];
        for &(needle, nth, class) in cases {
            assert_eq!(class_at(&source, needle, nth), class, "{:?} #{} ({})", needle, nth, when);
        }
    }
}

#[test]
fn a_word_called_alters_or_this_elsewhere_is_a_name() {
    let source = "task main()\n    bind alters = 1\n    bind this = alters + 1\n    show(this)\nfi\n";
    assert_eq!(Analysis::new(source).diagnostics(), Vec::new());
    assert_eq!(class_at(source, "alters = 1", 0), TokenClass::Variable);
    assert_eq!(class_at(source, "this)", 0), TokenClass::Variable);
}

#[test]
fn a_method_and_its_parameters_are_followed_to_their_declarations() {
    for checks in [true, false] {
        let source = program(checks, "show(1)");
        assert_definition(&source, ("step(2)", 0), ("step(by", 0));
        assert_definition(&source, ("cap()\n", 0), ("cap()\n", 1));
        assert_definition(&source, ("by\n", 0), ("by: int", 0));
        assert_definition(&source, ("count = .count", 0), ("count: int", 0));
        assert_definition(&source, ("flip()\n", 1), ("flip()", 0));
        assert_definition(&source, ("shown())", 0), ("shown() -> text", 0));
    }
}

#[test]
fn methods_and_what_they_are_called_on_are_described_on_hover() {
    for checks in [true, false] {
        let source = program(checks, "show(1)");
        let (code, doc) = hover(&source, "step(2)", 0);
        assert_eq!(code, "alters step(by: int)");
        assert!(doc.starts_with("A method of shape Counter, called on a value of it: value.step(…)."), "{}", doc);
        assert!(doc.contains("only a place that can change may call it"), "{}", doc);
        assert_eq!(hover(&source, "cap()\n", 1).0, "priv alters cap()");
        let (code, doc) = hover(&source, "shown())", 0);
        assert_eq!(code, "shown() -> text");
        assert!(doc.starts_with("A method of kind Light"), "{}", doc);
        assert!(!doc.contains("can change"), "{}", doc);
        assert_eq!(hover(&source, "by\n", 0), ("by: int".to_string(), "A parameter of method Counter.step.".to_string()));
        assert_eq!(hover(&source, "this = Green", 0).0, ".this: Light");
        assert!(hover(&source, "alters flip", 0).1.starts_with("Marks a method that changes the value it is called on"));
        let (code, doc) = hover(&source, "this\n", 0);
        assert_eq!(code, "region this");
        assert!(doc.starts_with("The region the value this method is called on lives in."), "{}", doc);
        // A shape lists its methods below its fields, as written.
        let (code, _) = hover(&source, "Counter\n", 0);
        assert!(code.contains("    count: int\n    total() -> int\n"), "{}", code);
        assert!(code.contains("    alters step(by: int)\n    priv alters cap()\nfi"), "{}", code);
    }
}

#[test]
fn a_member_of_the_value_has_its_type_inside_a_method() {
    for checks in [true, false] {
        let source = program(checks, "show(1)");
        assert_eq!(hover(&source, "count = .count", 0).0, "count: int");
        // `.label` is a member of the value, which lives in `@this`.
        let marked = in_step(checks, "bind name = .label\n        show(name.@@)");
        assert_offers(&marked, &["copy"], &["count"]);
    }
}

#[test]
fn completion_after_a_values_dot_offers_its_fields_and_methods() {
    for checks in [true, false] {
        let items = completions(&program(checks, "c.@@"));
        let item = |label: &str| items.iter().find(|item| item.label == label).unwrap_or_else(|| panic!("no {:?} in {:?}", label, items));
        assert_eq!(item("count").kind, CompletionKind::Field);
        let step = item("step");
        assert_eq!((step.kind, step.detail.as_str(), step.snippet.as_deref()), (CompletionKind::Method, "alters (by: int)", Some("step(${by})")));
        let total = item("total");
        assert_eq!((total.kind, total.apply.as_deref()), (CompletionKind::Method, Some("total()")));
        // A method of this file's own shape is offered even when private.
        item("cap");
        // A kind's value has its methods, not its variants.
        assert_offers(&program(checks, "light.@@"), &["flip", "shown"], &["Red", "Green"]);
        // A pointer's members are reached through `^`.
        let items = completions(&program(checks, "region r\n    bind p = new Counter(c) at r\n    p.@@"));
        let step = items.iter().find(|item| item.label == "step").unwrap();
        assert_eq!(step.snippet.as_deref(), Some("^.step(${by})"));
    }
}

#[test]
fn completion_after_a_dot_starting_a_value_inside_a_method_offers_the_value_and_its_members() {
    for checks in [true, false] {
        assert_offers(&in_step(checks, "show(.@@)"), &["this", "label", "count", "total", "step", "cap"], &["Red", "main"]);
        // In a kind's method: its methods, and no variants.
        let marked = program(checks, "show(1)").replace("                .this = Green\n", "                .this = Green\n                show(.@@)\n");
        assert_offers(&marked, &["this", "flip", "shown"], &["Red", "count"]);
        // Outside a method, nothing is called on a value.
        assert_offers(&program(checks, "show(.@@)"), &[], &["this", "count"]);
    }
}

#[test]
fn a_methods_body_completes_as_a_tasks_does() {
    for checks in [true, false] {
        assert_offers(&in_step(checks, "@@"), &["bind", "hold", "when", "fi", "by", "show"], &["priv", "alters"]);
        assert_offers(&in_step(checks, "show(b@@)"), &["by"], &["bind"]);
    }
    // A method's parameter is being named after `(`, and a type follows `:`.
    let source = program(true, "show(1)");
    assert_offers(&source.replace("alters step(by: int)", "alters step(@@"), &[], &["int", "by"]);
    assert_offers(&source.replace("alters step(by: int)", "alters step(by: @@"), &["int", "Counter", "region"], &[]);
}

#[test]
fn a_member_line_of_a_shape_or_a_kind_offers_priv_and_alters() {
    let source = program(true, "show(1)");
    assert_offers(&source.replace("    count: int\n", "    count: int\n    @@\n"), &["priv", "alters"], &["bind"]);
    assert_offers(&source.replace("    Green\n", "    Green\n    @@\n"), &["priv", "alters"], &["bind"]);
    assert_offers(&source.replace("    count: int\n", "    count: int\n    priv @@\n"), &["alters"], &["priv"]);
    // A method's name is being declared after `alters`.
    assert_offers(&source.replace("    count: int\n", "    count: int\n    alters @@\n"), &[], &["alters", "count"]);
}

const LIBRARY: &str = "\
shape Card
    owner: text

    ownerOf() -> text@this
        <- .owner
    fi

    priv checked() -> bool
        <- true
    fi
fi
";

const USER: &str = "\
include library

task main()
    bind card = library.Card { owner: \"ada\" }
    show(card.ownerOf())
    @@
fi
";

#[test]
fn another_modules_methods_are_its_public_ones() {
    let sources: HashMap<String, String> = [("app/library.vela".to_string(), LIBRARY.to_string())].into_iter().collect();
    let marked = USER.replace("    @@\n", "    show(card.@@)\n");
    let offset = marked.find("@@").unwrap();
    let source = marked.replacen("@@", "", 1);
    let analysis = Analysis::with_modules(&source, "app/main.vela", &sources);
    let labels: Vec<String> = analysis.complete(LineIndex::new(&source).utf16_of(offset)).items.into_iter().map(|item| item.label).collect();
    assert!(labels.contains(&"ownerOf".to_string()) && labels.contains(&"owner".to_string()), "{:?}", labels);
    assert!(!labels.contains(&"checked".to_string()), "{:?}", labels);

    let source = USER.replace("    @@\n", "");
    let analysis = Analysis::with_modules(&source, "app/main.vela", &sources);
    assert_eq!(analysis.diagnostics(), Vec::new());
    let offset = at(&source, "ownerOf", 0);
    let token = analysis.tokens().into_iter().find(|t| t.from == offset).unwrap();
    assert_eq!(token.class, TokenClass::Method);
    let hover = analysis.hover(offset).unwrap();
    assert_eq!(hover.code, "ownerOf() -> text@this");
    assert!(hover.doc.ends_with("From module library (app/library.vela)."), "{}", hover.doc);
    let found = analysis.definition(offset).unwrap();
    assert_eq!(found.path.as_deref(), Some("app/library.vela"));
    assert_eq!(&LIBRARY[found.from..found.to], "ownerOf");
}
