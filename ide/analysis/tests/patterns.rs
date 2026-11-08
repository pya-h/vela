#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, CompletionItem, LineIndex, SymbolKind, TokenClass};

const PROGRAM: &str = "\
pattern Shaped
    name: text
    area() -> float
    start(), stop()
    describe(r: region) -> text@r
        <- \"{.name}: {.area()}\" at r
    fi
    larger(other: this) -> bool
        <- .area() > other.area()
    fi
fi

shape Circle::Shaped
    name: text
    radius: float

    area() -> float
        <- 3.0 * .radius * .radius
    fi
    start()
        show(1)
    fi
    stop()
        show(0)
    fi
fi

task total<T: Shaped>(xs: [T]@r) -> float
    flex sum = 0.0
    each x in xs
        sum += x.area()
    fi
    <- sum
fi

task main()
    region r default
    fixed c = Circle { name: \"ring\", radius: 1.0 }
    show(c.describe(r), total([c, ...] at r))
    @@
fi
";

fn program(code: &str) -> String {
    PROGRAM.replace("    @@\n", &format!("    {}\n", code))
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

fn class_at(source: &str, needle: &str) -> Option<TokenClass> {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    Analysis::new(source).tokens().into_iter().find(|t| t.from <= byte && byte < t.to).map(|t| t.class)
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    let source = program("show(c.larger(c))");
    assert_eq!(Analysis::new(&source).diagnostics(), Vec::new());
}

#[test]
fn a_pattern_and_its_members_are_colored() {
    let source = program("show(0)");
    assert_eq!(class_at(&source, "pattern Shaped"), Some(TokenClass::Keyword));
    assert_eq!(class_at(&source, "Shaped\n    name"), Some(TokenClass::Type));
    assert_eq!(class_at(&source, "name: text\n    area"), Some(TokenClass::Field));
    assert_eq!(class_at(&source, "area() -> float\n    start"), Some(TokenClass::Method));
    assert_eq!(class_at(&source, "stop()\n    describe"), Some(TokenClass::Method));
    assert_eq!(class_at(&source, "this) -> bool"), Some(TokenClass::Type));
    assert_eq!(class_at(&source, "Shaped\n    name: text\n    radius"), Some(TokenClass::Type));
    assert_eq!(class_at(&source, "Shaped>(xs"), Some(TokenClass::Type));
    assert_eq!(class_at(&source, "name}: {"), Some(TokenClass::Field));
    assert_eq!(class_at(&source, "area()}\" at r"), Some(TokenClass::Method));
    assert_eq!(class_at(&source, "other.area()"), Some(TokenClass::Parameter));
    assert_eq!(class_at(&source, "describe(r), total"), Some(TokenClass::Method));
    let plain = "task main()\n    fixed pattern = 1\n    show(pattern)\nfi\n";
    let classes: Vec<TokenClass> = Analysis::new(plain).tokens().into_iter().filter(|t| &plain[t.from..t.to] == "pattern").map(|t| t.class).collect();
    assert_eq!(classes, [TokenClass::Variable, TokenClass::Variable]);
}

#[test]
fn a_pattern_hovers_with_what_it_asks_for() {
    let source = program("show(0)");
    let (code, doc) = hover(&source, "Shaped\n    name");
    assert_eq!(
        code,
        "pattern Shaped\n    name: text\n    area() -> float\n    start()\n    stop()\n    describe(r: region) -> text@r ; default\n    larger(other: this) -> bool ; default\nfi"
    );
    assert!(doc.contains("follows it"), "{}", doc);
    let (code, _) = hover(&source, "Circle::");
    assert!(code.starts_with("shape Circle::Shaped\n"), "{}", code);
    assert!(code.contains("    describe(r: region) -> text@r ; from Shaped\n"), "{}", code);
    assert!(code.contains("    larger(other: this) -> bool ; from Shaped\n"), "{}", code);
}

#[test]
fn a_pattern_is_in_the_outline_with_its_members() {
    let source = program("show(0)");
    let outline = Analysis::new(&source).outline();
    let pattern = outline.iter().find(|item| item.name == "Shaped").expect("Shaped in the outline");
    assert_eq!(pattern.kind, SymbolKind::Pattern);
    let members: Vec<&str> = pattern.children.iter().map(|child| child.name.as_str()).collect();
    assert_eq!(members, ["name", "area", "start", "stop", "describe", "larger"]);
}

#[test]
fn patterns_and_their_defaults_are_offered() {
    let found = labels(&program("show(c.@@)"));
    for name in ["area", "describe", "larger", "radius"] {
        assert!(found.contains(&name.to_string()), "{:?}", found);
    }
    let found = labels("pattern Named\n    label() -> int\nfi\n\ntask f<T: N@@>(x: T)\nfi\n");
    assert!(found.contains(&"Named".to_string()), "{:?}", found);
    let found = completions("task main()\nfi\n\n@@");
    let snippet = found.iter().find(|item| item.label == "pattern").expect("a pattern snippet");
    assert_eq!(snippet.snippet.as_deref(), Some("pattern ${Name}\n\t${method}() -> ${type}\nfi"));
}

#[test]
fn a_mistake_in_following_a_pattern_is_shown_in_the_editor() {
    let source = "pattern Shaped\n    area() -> float\nfi\n\nshape Dot::Shaped\n    x: int\nfi\n\ntask main()\nfi\n";
    let diagnostics = Analysis::new(source).diagnostics();
    assert_eq!(diagnostics.len(), 1, "{:?}", diagnostics);
    assert_eq!(diagnostics[0].message, "'Dot' follows pattern 'Shaped', which asks for method 'area'; write it in 'Dot', as in 'area() -> float'");
}
