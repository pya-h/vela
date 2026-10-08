//! Generic tasks and shapes in the editor: the types a declaration takes, the
//! words that say what they must allow, the types a use gives, and what each of
//! them hovers, colors, and completes as. In the sources below, `@@` marks the
//! cursor.

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
    let types = ["int", "float", "bool", "text", "unit", "char", "T", "maybe"];
    assert_eq!(labels("task first<T>(x: T)\n    bind y: @@ = x\nfi\n"), types);
    // A task that takes no types has none to offer.
    assert_eq!(labels("task plain(x: int)\n    bind y: @@ = x\nfi\n"), ["int", "float", "bool", "text", "unit", "char", "maybe"]);
}

#[test]
fn completion_offers_the_words_a_type_can_be_asked_to_allow() {
    let words = ["frozen", "same", "similar", "shown", "ordered"];
    assert_eq!(labels("task f<T: @@>(x: T)\nfi\n"), words);
    assert_eq!(labels("task f<T: same & @@>(x: T)\nfi\n"), words);
    // The name of a type being declared is the writer's to choose.
    assert!(labels("task f<@@>(x: int)\nfi\n").is_empty());
}

#[test]
fn completion_offers_types_where_a_call_names_them() {
    for checks in [true, false] {
        let items = labels(&program(checks, "show(first<@@>(ns))"));
        assert_eq!(items, ["int", "float", "bool", "text", "unit", "char", "maybe"], "checks: {}", checks);
    }
}

#[test]
fn a_type_that_does_not_allow_what_a_task_asks_is_reported() {
    let source = program(true, "show(biggest(true, false))");
    let diagnostics = Analysis::new(&source).diagnostics();
    let [diagnostic] = diagnostics.as_slice() else { panic!("{:?}", diagnostics) };
    assert!(diagnostic.message.contains("takes a type that is 'ordered'"), "{}", diagnostic.message);
}

#[test]
fn a_call_that_names_no_type_and_shows_none_is_reported() {
    let source = "\
task empty<T>() -> maybe T
    <- none
fi

task main()
    bind x = empty()
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
task f<T: ordered & same>(x: T) -> T
    <- x
fi

task main()
    show(f(1))
fi
";
    let diagnostics = Analysis::new(source).diagnostics();
    let [diagnostic] = diagnostics.as_slice() else { panic!("{:?}", diagnostics) };
    assert!(diagnostic.message.contains("'ordered' already means 'same'"), "{}", diagnostic.message);
    // Any other pair asks for two things, which says something new.
    assert_eq!(Analysis::new(&source.replace("ordered & same", "same & frozen")).diagnostics(), Vec::new());
}

const SHAPES: &str = "\
shape Box<T: frozen>
    value: T
fi

shape Pair<A: frozen, B: frozen>
    left: A
    right: B
fi

task valueOf<T: frozen>(b: Box<T>@r) -> T
    <- b.value
fi

task main()
    region r
    bind kept: Box<text>@r = Box { value: \"ada\".copy() at r }
    bind count = Box<int> { value: 2 }
    bind pair: Pair<int, text> = Pair { left: 1, right: \"one\" }
    show(valueOf(count))
    show(kept.value)
    show(pair.right)
    @@
fi
";

fn shapes(code: &str) -> String {
    SHAPES.replace("    @@\n", &format!("    {}\n", code))
}

#[test]
fn the_shapes_program_used_by_these_tests_compiles() {
    assert_eq!(Analysis::new(&shapes("show(1)")).diagnostics(), Vec::new());
}

#[test]
fn a_value_of_a_shape_given_types_hovers_as_the_program_writes_it() {
    let source = shapes("show(1)");
    // The shape the compiler makes for the types is never shown: the type reads
    // as it is written.
    assert_eq!(hover(&source, "kept:").0, "bind kept: Box<text>@r");
    assert_eq!(hover(&source, "count =").0, "bind count: Box<int>");
    assert_eq!(hover(&source, "pair:").0, "bind pair: Pair<int, text>");
    let (code, doc) = hover(&source, "Box<text>@r");
    assert_eq!(code, "shape Box<T: frozen>\n    value: T\nfi");
    assert_eq!(doc, "");
    let (_, doc) = hover(&source, "T: frozen");
    assert!(doc.starts_with("A type shape Box takes"), "{}", doc);
}

#[test]
fn a_shape_and_the_types_it_takes_are_outlined_together() {
    let outline = Analysis::new(&shapes("show(1)")).outline();
    let box_shape = outline.first().expect("the first shape");
    assert_eq!((box_shape.name.as_str(), box_shape.kind, box_shape.detail.as_str()), ("Box", SymbolKind::Shape, "<T: frozen>"));
    let members: Vec<(&str, SymbolKind, &str)> = box_shape.children.iter().map(|c| (c.name.as_str(), c.kind, c.detail.as_str())).collect();
    assert_eq!(members, [("T", SymbolKind::TypeParam, "frozen"), ("value", SymbolKind::Field, "T")]);
}

#[test]
fn the_types_a_shape_is_given_are_colored_as_types() {
    let source = shapes("show(1)");
    let analysis = Analysis::new(&source);
    let class_at = |needle: &str| {
        let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
        analysis.tokens().into_iter().find(|t| t.from <= byte && byte < t.to).map(|t| t.class)
    };
    // In a written type, in a literal, and in the header of the shape itself.
    assert_eq!(class_at("text>@r"), Some(TokenClass::TypeBuiltin));
    assert_eq!(class_at("int> { value: 2 }"), Some(TokenClass::TypeBuiltin));
    assert_eq!(class_at("T: frozen"), Some(TokenClass::Type));
    assert_eq!(class_at("frozen>"), Some(TokenClass::Keyword));
    assert_eq!(class_at("Box<text>"), Some(TokenClass::Type));
}

#[test]
fn completion_offers_types_where_a_shape_is_given_them() {
    let types = ["int", "float", "bool", "text", "unit", "char", "Box", "Pair", "maybe"];
    // In a written type, and in a literal.
    assert_eq!(labels(&shapes("bind other: Box<@@> = count")), types);
    assert_eq!(labels(&shapes("bind other = Box<@@> { value: 1 }")), types);
    // A type the shape takes is a type inside its own fields.
    assert_eq!(labels("shape Box<T>\n    value: @@\nfi\n"), ["int", "float", "bool", "text", "unit", "char", "T", "Box", "maybe"]);
    // The words a type can be asked to allow, and nothing where a name is
    // being declared.
    assert_eq!(labels("shape Box<T: @@>\n    value: T\nfi\n"), ["frozen", "same", "similar", "shown", "ordered"]);
    assert!(labels("shape Box<@@>\n    value: int\nfi\n").is_empty());
}

#[test]
fn a_shape_used_without_the_types_it_takes_is_reported() {
    let diagnostics = Analysis::new(&shapes("bind plain: Box = count")).diagnostics();
    let [diagnostic] = diagnostics.as_slice() else { panic!("{:?}", diagnostics) };
    assert_eq!(diagnostic.message, "shape 'Box' takes 1 type, so name it, as in 'Box<T>'");
}

#[test]
fn a_literal_whose_types_nothing_names_is_reported() {
    let diagnostics = Analysis::new(&shapes("bind empty = Box { value: none }")).diagnostics();
    let [diagnostic] = diagnostics.as_slice() else { panic!("{:?}", diagnostics) };
    assert!(diagnostic.message.starts_with("can't tell which type 'T' is for this 'Box'"), "{}", diagnostic.message);
}

#[test]
fn a_shape_made_for_its_types_is_named_as_written_from_a_plain_header() {
    // A declaration that takes no types of its own may name a shape given a
    // set of them, and one given types may be given to another. The editor
    // names both as the program writes them, never as the shape made for them.
    let source = "\
shape Box<T: frozen>
    value: T
fi

shape Pair<A: frozen, B: frozen>
    left: A
    right: B
fi

task textOf(b: Box<text>@r) -> text@r
    <- b.value
fi

task leftText(p: Pair<Box<text>, int>@r) -> text@r
    <- p.left.value
fi

task main()
    region r
    bind kept: Box<text>@r = Box { value: \"ada\".copy() at r }
    bind paired: Pair<Box<text>, int>@r = Pair { left: kept, right: 1 }
    show(textOf(kept))
    show(leftText(paired))
fi
";
    assert_eq!(Analysis::new(source).diagnostics(), Vec::new());
    assert_eq!(hover(source, "b: Box<text>@r").0, "b: Box<text>@r");
    assert_eq!(hover(source, "paired:").0, "bind paired: Pair<Box<text>, int>@r");
    assert_eq!(hover(source, "textOf(b").0, "task textOf(b: Box<text>@r) -> text@r");
    assert_eq!(hover(source, "leftText(p").0, "task leftText(p: Pair<Box<text>, int>@r) -> text@r");
}

#[test]
fn similarity_is_an_operator_giving_a_bool_and_its_word_has_a_hover() {
    let source = "task alike<T: similar & shown>(a: T, b: T) -> bool\n    bind same_items = a ~= b\n    bind apart = a <> b\n    show(a)\n    <- same_items and not apart\nfi\n";
    assert_eq!(Analysis::new(source).diagnostics(), Vec::new());
    assert_eq!(class_at_in(source, "~= b"), Some(TokenClass::Operator));
    assert_eq!(class_at_in(source, "<> b"), Some(TokenClass::Operator));
    let (_, doc) = hover(source, "T: similar");
    assert!(doc.contains("item by item with '~=' and '<>'") && doc.contains("can be shown"), "{}", doc);
    let (code, _) = hover(source, "same_items =");
    assert!(code.contains("bool"), "{}", code);
    // Without the word, '~=' is refused and says which word to ask for.
    let diagnostics = Analysis::new(&source.replace("similar & shown", "shown")).diagnostics();
    assert!(diagnostics.iter().any(|d| d.message.contains("require it too, as in '<T: shown & similar>'")), "{:?}", diagnostics);
}

#[test]
fn types_are_offered_between_a_closed_pair_of_angle_brackets() {
    // `<>` reads as 'not similar', but an editor closing a `<` leaves one.
    let labels_at = labels("task first<T>(x: T) -> T\n    <- x\nfi\ntask main()\n    show(first<@@>(1))\nfi\n");
    assert!(labels_at.contains(&"int".to_string()), "{:?}", labels_at);
}

/// The class of the token at the first occurrence of `needle` in `source`,
/// which is ASCII, so bytes and UTF-16 offsets agree.
fn class_at_in(source: &str, needle: &str) -> Option<TokenClass> {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    Analysis::new(source).tokens().into_iter().find(|t| t.from <= byte && byte < t.to).map(|t| t.class)
}

#[test]
fn a_shape_given_types_is_read_from_the_source_when_the_file_does_not_parse() {
    let source = shapes("show(1)") + "task g(\n";
    assert_eq!(hover(&source, "kept:").0, "bind kept: Box<text>@r");
    assert_eq!(hover(&source, "pair:").0, "bind pair: Pair<int, text>");
}
