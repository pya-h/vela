//! Generic tasks in the editor: the types a task takes, the words that say what
//! they must allow, the types a call gives, and what each of them hovers,
//! colors, and completes as. In the sources below, `@@` marks the cursor.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, CompletionItem, LineIndex, SymbolKind, TokenClass};

const PROGRAM: &str = "\
task first<T>(xs: [T]@r) -> maybe T
    each v in xs
        <- v
    fi
    <- none
fi

task biggest<T: ordered>(a: T, b: T) -> T
    when a > b
        <- a
    fi
    <- b
fi

task tell<T: shown>(label: text, value: T)
    show(label)
    show(value)
fi

task main()
    CHECK
    region r
    bind ns: [int]@r = new [int]([3, 1]) at r
    bind names: [text]@r = new [text]([\"ada\"]) at r
    show(first(ns))
    show(first<text@r>(names))
    show(biggest(2, 5))
    tell(\"n\", 7)
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
    assert_eq!(Analysis::new(&program(true, "show(1)")).diagnostics(), Vec::new());
}

#[test]
fn a_type_a_task_takes_hovers_as_one_wherever_it_is_written() {
    for checks in [true, false] {
        let source = program(checks, "show(1)");
        let when = if checks { "checked" } else { "declared" };
        let (code, doc) = hover(&source, "T>(xs");
        assert_eq!(code, "T", "{}", when);
        assert!(doc.starts_with("A type task first takes"), "{}", doc);
        let (code, doc) = hover(&source, "T]@r");
        assert_eq!(code, "T", "{}", when);
        assert!(doc.starts_with("A type task first takes"), "{}", doc);
        // A word says what every type given for it must allow.
        let (_, doc) = hover(&source, "T: ordered");
        assert!(doc.contains("has an order"), "{}", doc);
        let (_, doc) = hover(&source, "T: shown");
        assert!(doc.contains("shown"), "{}", doc);
    }
}

#[test]
fn a_task_and_the_types_it_takes_are_outlined_together() {
    let outline = Analysis::new(&program(true, "show(1)")).outline();
    let first = outline.first().expect("the first task");
    assert_eq!(first.name, "first");
    assert_eq!(first.detail, "<T>(xs: [T]@r) -> maybe T");
    let types: Vec<(&str, SymbolKind, &str)> = first.children.iter().map(|c| (c.name.as_str(), c.kind, c.detail.as_str())).collect();
    assert_eq!(types, [("T", SymbolKind::TypeParam, "")]);
    let biggest = outline.get(1).expect("the second task");
    let types: Vec<(&str, &str)> = biggest.children.iter().map(|c| (c.name.as_str(), c.detail.as_str())).collect();
    assert_eq!(types, [("T", "ordered")]);
}

#[test]
fn the_types_a_task_takes_and_the_words_for_them_are_colored() {
    let source = program(true, "show(1)");
    let analysis = Analysis::new(&source);
    let class_at = |needle: &str| {
        let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
        analysis.tokens().into_iter().find(|t| t.from <= byte && byte < t.to).map(|t| t.class)
    };
    assert_eq!(class_at("T>(xs"), Some(TokenClass::Type));
    assert_eq!(class_at("T]@r"), Some(TokenClass::Type));
    assert_eq!(class_at("ordered>"), Some(TokenClass::Keyword));
    assert_eq!(class_at("T: shown"), Some(TokenClass::Type));
    assert_eq!(class_at("shown>"), Some(TokenClass::Keyword));
    // The types a call gives are types, and the call is still a call.
    assert_eq!(class_at("text@r>(names)"), Some(TokenClass::TypeBuiltin));
    assert_eq!(class_at("first<text@r>"), Some(TokenClass::Task));
}

#[test]
fn a_type_a_task_takes_completes_as_a_type_in_its_own_body() {
    let types = ["int", "float", "bool", "text", "T", "maybe"];
    assert_eq!(labels("task first<T>(x: T)\n    bind y: @@ = x\nfi\n"), types);
    // A task that takes no types has none to offer.
    assert_eq!(labels("task plain(x: int)\n    bind y: @@ = x\nfi\n"), ["int", "float", "bool", "text", "maybe"]);
}

#[test]
fn completion_offers_the_words_a_type_can_be_asked_to_allow() {
    let words = ["fixed", "same", "shown", "ordered"];
    assert_eq!(labels("task f<T: @@>(x: T)\nfi\n"), words);
    assert_eq!(labels("task f<T: same & @@>(x: T)\nfi\n"), words);
    // The name of a type being declared is the writer's to choose.
    assert!(labels("task f<@@>(x: int)\nfi\n").is_empty());
}

#[test]
fn completion_offers_types_where_a_call_names_them() {
    for checks in [true, false] {
        let items = labels(&program(checks, "show(first<@@>(ns))"));
        assert_eq!(items, ["int", "float", "bool", "text", "maybe"], "checks: {}", checks);
    }
}

#[test]
fn a_type_that_does_not_allow_what_a_task_asks_is_reported() {
    let source = program(true, "show(biggest(\"a\", \"b\"))");
    let diagnostics = Analysis::new(&source).diagnostics();
    let [diagnostic] = diagnostics.as_slice() else { panic!("{:?}", diagnostics) };
    assert!(diagnostic.message.contains("takes a type that is 'ordered'"), "{}", diagnostic.message);
}

#[test]
fn a_call_that_names_no_type_and_shows_none_is_reported() {
    let source = "\
task nothing<T>() -> maybe T
    <- none
fi

task main()
    bind x = nothing()
    show(1)
fi
";
    let diagnostics = Analysis::new(source).diagnostics();
    let [diagnostic] = diagnostics.as_slice() else { panic!("{:?}", diagnostics) };
    assert!(diagnostic.message.starts_with("can't tell which type 'T' is"), "{}", diagnostic.message);
}

#[test]
fn a_word_that_says_nothing_new_is_reported() {
    let source = "\
task f<T: same & fixed>(x: T) -> T
    <- x
fi

task main()
    show(f(1))
fi
";
    let diagnostics = Analysis::new(source).diagnostics();
    let [diagnostic] = diagnostics.as_slice() else { panic!("{:?}", diagnostics) };
    assert!(diagnostic.message.contains("already means 'fixed'"), "{}", diagnostic.message);
}
