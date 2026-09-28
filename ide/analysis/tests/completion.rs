//! Completion and hover: what is offered at each kind of position, with the
//! compiler's types when the file checks and declared types when it doesn't.
//! In the sources below, `@@` marks the cursor.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, CompletionItem, CompletionKind, Completions, Hover, LineIndex};

const PROGRAM: &str = "\
shape Point
    x: int
    y: int
fi

shape Line
    start: Point
    finish: ^Point
fi

kind Shape
    Circle(radius: float)
    Rectangle(width: float, height: float)
    Dot
fi

task area(s: Shape) -> float
    <- 0.0
fi

task origin() -> Point
    <- Point { x: 0, y: 0 }
fi

task main()
    region points
    bind p = Point { x: 1, y: 2 }
    bind line = Line { start: p, finish: new Point(p) at points }
    bind figure = Circle(1.0)
    hold total = 0
    when total > 0
        bind hidden = 1
        show(hidden)
    fi
    @@
fi
";

/// Replaces the `@@` in `PROGRAM` with `code` (which has its own `@@`).
fn in_main(code: &str) -> String {
    PROGRAM.replace("    @@\n", &format!("    {}\n", code))
}

/// The completions at `@@`, and the source without the marker.
fn complete(marked: &str) -> (Completions, String) {
    let at = marked.find("@@").expect("a @@ marker");
    let source = marked.replacen("@@", "", 1);
    let offset = LineIndex::new(&source).utf16_of(at);
    let completions = Analysis::new(&source).complete(offset);
    (completions, source)
}

fn labels(marked: &str) -> Vec<String> {
    complete(marked).0.items.into_iter().map(|item| item.label).collect()
}

fn item(marked: &str, label: &str) -> CompletionItem {
    let (completions, _) = complete(marked);
    completions.items.into_iter().find(|i| i.label == label).unwrap_or_else(|| panic!("no completion {:?}", label))
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
fn item_level_offers_declarations() {
    assert_offers("@@", &["task", "shape", "kind", "task -> type"], &["bind", "show", "Point"]);
    assert_offers(&PROGRAM.replace("    @@\n", "").replace("fi\n", "fi\n@@\n"), &["task", "kind"], &["when"]);
    let task = complete("@@").0.items.into_iter().find(|i| i.kind == CompletionKind::Snippet && i.label == "task").unwrap();
    assert_eq!(task.snippet.as_deref(), Some("task ${name}(${})\n\t${}\nfi"));
    let sending = complete("@@").0.items.into_iter().find(|i| i.label == "task -> type").unwrap();
    assert_eq!(sending.snippet.as_deref(), Some("task ${name}(${}) -> ${type}\n\t<- ${value}\nfi"));
}

#[test]
fn private_offers_the_declarations_it_can_mark() {
    let after = "task main()\nfi\n\npriv @@";
    assert_offers(after, &["task", "shape", "kind", "task -> type"], &["priv", "include", "bind"]);
    // The word being typed is completed too.
    assert_offers("priv sh@@", &["shape"], &["priv"]);
    // A shape's private field is a name being declared, a private method may
    // say it changes its value, and a private field may be shared.
    assert_eq!(labels("shape S\n    priv @@\nfi\n"), ["alters", "shared"]);
    // Not a statement.
    assert_eq!(labels(&in_main("priv @@")), Vec::<String>::new());
}

#[test]
fn statements_offer_keywords_snippets_and_visible_names() {
    assert_offers(
        PROGRAM,
        &["bind", "hold", "when", "each", "match", "fi", "p", "line", "figure", "total", "area", "origin", "show", "match figure"],
        &["task", "else", "case", "Circle", "x", "hidden", "true", "send"],
    );
    // The word being typed is replaced.
    let (completions, source) = complete(&in_main("ma@@"));
    assert_eq!(completions.from, LineIndex::new(&source).utf16_of(source.find("ma\n").unwrap()));
    assert!(completions.items.iter().any(|i| i.label == "match figure"));

    // A match with every case of the value's kind.
    assert_eq!(
        item(PROGRAM, "match figure").snippet.as_deref(),
        Some("match figure\ncase Circle ${radius}\n\t\ncase Rectangle ${width} ${height}\n\t\ncase Dot\n\t${}\nfi")
    );
    // Calls get a field per parameter.
    assert_eq!(item(PROGRAM, "area").snippet.as_deref(), Some("area(${s})"));
    assert_eq!(item(PROGRAM, "origin").apply.as_deref(), Some("origin()"));
    assert_eq!(item(PROGRAM, "total").detail, "int");
    // `else` and `case` where they can close a branch.
    assert_offers(&in_main("when true\n        @@"), &["else", "fi"], &["case"]);
    assert_offers(&in_main("match figure\n    @@"), &["case"], &["else"]);
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    // So the tests below see the compiler's types, not only the fallbacks
    // used while a file has an error.
    assert_eq!(Analysis::new(&PROGRAM.replace("    @@\n", "")).diagnostics(), Vec::new());
}

#[test]
fn expressions_offer_values() {
    assert_offers(&in_main("bind q = @@"), &["p", "total", "area", "Circle", "Dot", "true", "none", "new", "show"], &["bind", "q", "hidden", "int"]);
    assert_eq!(item(&in_main("bind q = @@"), "Circle").snippet.as_deref(), Some("Circle(${radius})"));
    assert_eq!(item(&in_main("bind q = @@"), "Dot").snippet, None);
    assert_eq!(item(&in_main("bind q = @@"), "Rectangle").detail, "Shape (width: float, height: float)");
    // A variable is not visible in its own initializer.
    assert_offers(&in_main("bind q = 1\n    bind r = @@"), &["q"], &["r"]);
    // A shape is offered as a literal with its fields.
    assert_eq!(item(&in_main("bind q = @@"), "Point").snippet.as_deref(), Some("Point { x: ${x}, y: ${y} }"));
}

#[test]
fn nothing_is_offered_inside_text_ending_in_an_escaped_quote() {
    assert_offers(&in_main("show(\"say \\\"@@"), &[], &["bind", "when", "p"]);
    assert_offers(&in_main("show(\"say \\\"hi@@"), &[], &["bind", "when", "p"]);
}

#[test]
fn statement_items_start_a_line_even_after_invalid_text() {
    assert_offers(&in_main("bind s = 1 @ @@"), &[], &["bind", "when"]);
}

#[test]
fn types_are_offered_where_a_type_is_written() {
    assert_offers(&in_main("bind q: @@"), &["int", "text", "Point", "Shape"], &["unit", "p", "bind"]);
    assert_offers(&in_main("bind q: ^@@"), &["int", "Point"], &["p"]);
    assert_offers(&in_main("bind q = new @@"), &["Point"], &["p"]);
    assert_offers("task f(a: @@", &["int", "float"], &["unit"]);
    assert_offers("task f() -> @@", &["unit", "int"], &[]);
    assert_offers("shape S\n    field: @@", &["float", "S"], &[]);
}

#[test]
fn fields_follow_the_type_of_the_value() {
    assert_offers(&in_main("show(p.@@)"), &["x", "y"], &["start", "p"]);
    assert_eq!(item(&in_main("show(p.@@)"), "x").detail, "int");
    assert_offers(&in_main("show(line.start.@@)"), &["x", "y"], &["finish"]);
    assert_offers(&in_main("show(origin().@@)"), &["x", "y"], &[]);
    assert_offers(&in_main("show(line.finish^.@@)"), &["x", "y"], &[]);
    // A kind value has no fields, only its type, which every value has.
    assert_eq!(labels(&in_main("show(figure.@@)")), ["type"]);

    // Through a pointer, the field is reached with `^.`, replacing the dot.
    let marked = in_main("show(line.finish.@@)");
    let (completions, source) = complete(&marked);
    assert_eq!(completions.from, LineIndex::new(&source).utf16_of(source.find("finish.").unwrap() + "finish".len()));
    assert_eq!(item(&marked, "x").apply.as_deref(), Some("^.x"));
}

#[test]
fn fields_still_complete_when_the_rest_of_the_file_is_broken() {
    // The line after the cursor doesn't parse, so no compiler types are available.
    let broken = |code: &str| PROGRAM.replace("    @@\n", &format!("    {}\n    bind broken = (\n", code));
    assert_offers(&broken("show(p.@@)"), &["x", "y"], &[]);
    assert_offers(&broken("show(line.start.@@)"), &["x", "y"], &[]);
    assert_eq!(item(&broken("show(line.finish.@@)"), "y").apply.as_deref(), Some("^.y"));
    assert_offers(&broken("show(origin().@@)"), &["x"], &[]);
}

#[test]
fn case_offers_the_variants_not_yet_matched() {
    let marked = in_main("match figure\n    case Circle r\n        show(r)\n    case @@\n    fi");
    assert_offers(&marked, &["Rectangle", "Dot", "all remaining cases"], &["Circle", "p", "bind"]);
    assert_eq!(item(&marked, "Rectangle").snippet.as_deref(), Some("Rectangle ${width} ${height}"));
    assert_eq!(item(&marked, "all remaining cases").snippet.as_deref(), Some("Rectangle ${width} ${height}\n\t\ncase Dot\n\t${}"));
    // The bindings after a variant are new names.
    assert_eq!(labels(&in_main("match figure\n    case Rectangle @@\n    fi")), Vec::<String>::new());
    // A case written through the kind covers its variant too.
    let through_kind = in_main("match figure\n    case Shape.Circle r\n        show(r)\n    case @@\n    fi");
    assert_offers(&through_kind, &["Rectangle", "Dot", "all remaining cases"], &["Circle"]);
    assert_eq!(item(&through_kind, "all remaining cases").snippet.as_deref(), Some("Rectangle ${width} ${height}\n\t\ncase Dot\n\t${}"));
    // After `case Shape.`, the variants not matched yet, however they were written.
    let after_kind = in_main("match figure\n    case Circle r\n        show(r)\n    case Shape.Dot\n        show(0)\n    case Shape.@@\n    fi");
    assert_eq!(labels(&after_kind), ["Rectangle"]);
    // A `_` written doesn't hide the variants still named.
    assert_eq!(labels(&in_main("match figure\n    case _\n        show(0)\n    case Shape.@@\n    fi")), ["Circle", "Rectangle", "Dot"]);
}

#[test]
fn case_snippets_never_link_fields_of_different_variants() {
    // Snippet fields with the same name are edited together, so a payload
    // field name shared by two variants must get two field names.
    let source = "kind Reading\n    Exact(value: int)\n    Between(value: int, high: int)\n    Missing\nfi\ntask main()\n    bind r = Exact(1)\n    @@\nfi\n";
    assert_eq!(
        item(source, "match r").snippet.as_deref(),
        Some("match r\ncase Exact ${value}\n\t\ncase Between ${value2} ${high}\n\t\ncase Missing\n\t${}\nfi")
    );
    let marked = source.replace("    @@\n", "    match r\n    case @@\n    fi\n");
    assert_eq!(item(&marked, "all remaining cases").snippet.as_deref(), Some("Exact ${value}\n\t\ncase Between ${value2} ${high}\n\t\ncase Missing\n\t${}"));
    // A single case needs no renaming.
    assert_eq!(item(&marked, "Between").snippet.as_deref(), Some("Between ${value} ${high}"));
}

#[test]
fn shape_literals_offer_the_missing_fields() {
    assert_offers(&in_main("bind q = Point { x: 1, @@ }"), &["y"], &["x", "p"]);
    assert_eq!(item(&in_main("bind q = Point { @@"), "x").snippet.as_deref(), Some("x: ${}"));
    // A value after the colon is an expression.
    assert_offers(&in_main("bind q = Point { x: @@ }"), &["p", "total"], &["y"]);
}

#[test]
fn nothing_is_offered_in_comments_text_or_new_names() {
    for marked in [
        in_main("; a comm@@ent"),
        in_main("show(\"hel@@lo\")"),
        in_main("show(\"unfinished @@"),
        in_main("bind @@"),
        "task @@".to_string(),
        "task f(hold @@".to_string(),
        "kind K\n    V(@@".to_string(),
        in_main("each @@"),
    ] {
        assert_eq!(labels(&marked), Vec::<String>::new(), "at {:?}", marked);
    }
    // A parameter's name is new, but it may be one the task changes.
    for marked in ["task f(@@", "task f(a: int, @@"] {
        assert_eq!(labels(marked), ["hold", "hold&"], "at {:?}", marked);
    }
    // A field's or a method's name is new, but it may be private, a shape's
    // field may be read-only or shared, and a method may change its value. A
    // kind has no fields, so nothing there is read-only or shared.
    assert_eq!(labels("shape S\n    @@"), ["priv", "read", "shared", "alters"]);
    assert_eq!(labels("kind K\n    V\n    @@"), ["priv", "alters"]);
}

/// The hover at the `nth` occurrence of `needle` (offset `shift` into it).
fn hover(source: &str, needle: &str, nth: usize) -> Hover {
    let byte = source.match_indices(needle).nth(nth).unwrap_or_else(|| panic!("no {:?}", needle)).0;
    let offset = LineIndex::new(source).utf16_of(byte);
    Analysis::new(source).hover(offset).unwrap_or_else(|| panic!("no hover at {:?}", needle))
}

fn code_and_doc(h: Hover) -> (String, String) {
    (h.code, h.doc)
}

#[test]
fn hover_describes_declarations() {
    let source = PROGRAM.replace(
        "    @@\n",
        "    show(area(figure))\n    match figure\n    case Circle r\n        show(r)\n    case Rectangle w h\n        show(w)\n    case Dot\n        show(0)\n    fi\n    each i in 0..3\n        show(i)\n    fi\n",
    );
    let pair = |code: &str, doc: &str| (code.to_string(), doc.to_string());
    assert_eq!(code_and_doc(hover(&source, "area(figure)", 0)), pair("task area(s: Shape) -> float", ""));
    assert_eq!(code_and_doc(hover(&source, "p,", 0)), pair("bind p: Point", "An immutable variable."));
    assert_eq!(code_and_doc(hover(&source, "total >", 0)), pair("hold total: int", "A mutable variable."));
    assert_eq!(code_and_doc(hover(&source, "s: Shape", 0)), pair("s: Shape", "A parameter of task area."));
    assert_eq!(code_and_doc(hover(&source, "r)", 0)), pair("r: float", "Bound by case Circle."));
    assert_eq!(code_and_doc(hover(&source, "i)", 0)), pair("each i: int", "The counter of an each loop."));
    assert_eq!(code_and_doc(hover(&source, "Circle(1.0)", 0)), pair("Circle(radius: float)", "A variant of kind Shape."));
    assert_eq!(code_and_doc(hover(&source, "radius", 0)), pair("radius: float", "A payload field of variant Circle."));
    assert_eq!(code_and_doc(hover(&source, "start: p", 0)), pair("start: Point", "A field of shape Line."));
    assert_eq!(code_and_doc(hover(&source, "int\n    y: int", 0)), pair("int", "A 64-bit signed integer. int(x) turns a value into one: a float becomes its whole part, toward zero (a float with none to give, such as nan, stops the program), and a bool becomes 1 or 0."));
    assert_eq!(hover(&source, "show", 0).code, "task show(value: anything with str, \u{2026})");
    assert_eq!(hover(&source, "Point {", 0).code, "shape Point\n    x: int\n    y: int\nfi");
    assert_eq!(hover(&source, "Shape\n", 0).code, "kind Shape\n    Circle(radius: float)\n    Rectangle(width: float, height: float)\n    Dot\nfi");
    // The hovered range is the name.
    let h = hover(&source, "total >", 0);
    assert_eq!(h.to - h.from, "total".len());
}

#[test]
fn show_takes_several_values() {
    // A value after a comma completes like the first one, and the file checks.
    assert_offers(&in_main("show(\"total: \", @@)"), &["total", "p", "area", "true"], &["fi", "task"]);
    assert_offers(&in_main("show(total, @@)"), &["p", "line", "show"], &["else"]);
    let checked = "task main()\n    bind n = 1\n    show(\"n = \", n, \"!\")\n    show()\nfi\n";
    assert_eq!(Analysis::new(checked).diagnostics(), Vec::new());
}

#[test]
fn a_value_shown_by_its_own_str_keeps_its_type_in_the_editor() {
    // The checker turns such a value into the call of its `str`, which stands
    // where the value is written. The editor asks about the value there, so a
    // member of it completes as it does anywhere else, and not as a member of
    // the text the call gives.
    let source = "shape Card\n    name: text\n\n    str(r: region) -> text@r\n        <- \"{.name}\" at r\n    fi\nfi\n\ntask main()\n    region r\n    hold cards = new [Card]() at r\n    cards.add(Card { name: \"Ada\" })\n    show(cards[0])\nfi\n";
    assert_eq!(Analysis::new(source).diagnostics(), Vec::new());
    assert_offers(&source.replace("show(cards[0])", "show(cards[0].@@)"), &["name", "str", "copy"], &["len"]);
    assert_eq!(hover(source, "cards[0])", 0).code, "hold cards: [Card]@r");
}

#[test]
fn show_takes_a_value_with_its_own_str() {
    // A shape with a `str` of its own is a value `show` writes, and the name in
    // the call still hovers as what it is, although the checker has turned the
    // value into the call of that method.
    let source = "shape Card\n    name: text\n\n    str(r: region) -> text@r\n        <- \"{.name}\" at r\n    fi\nfi\n\ntask main()\n    bind card = Card { name: \"Ada\" }\n    show(\"top: \", card)\nfi\n";
    assert_eq!(Analysis::new(source).diagnostics(), Vec::new());
    assert_eq!(hover(source, "card)", 0).code, "bind card: Card");
    assert_eq!(hover(source, "str(r", 0).code, "str(r: region) -> text@r");
}

#[test]
fn the_kind_reading_a_number_sends_is_known_to_the_editor() {
    // `ParseError` and its variants are declared by no file, so the editor knows
    // them as it knows the builtin types.
    let source = "task main()\n    match int(\"42\")\n    case {value}\n        show(value)\n    case failure why\n        match why\n        case NotANumber\n            show(\"no\")\n        case TooLarge\n            show(\"big\")\n        fi\n    fi\nfi\n";
    assert_eq!(Analysis::new(source).diagnostics(), Vec::new());
    assert!(hover(source, "int(\"42\")", 0).doc.contains("turns a value into one"), "{:?}", hover(source, "int(\"42\")", 0).doc);
    assert!(hover(source, "NotANumber", 0).doc.contains("no number at all"), "{:?}", hover(source, "NotANumber", 0).doc);
    assert!(hover(source, "TooLarge", 0).doc.contains("not one the type holds"), "{:?}", hover(source, "TooLarge", 0).doc);
    // The type a reading gives, and the kind where an error type goes.
    let bound = "task main()\n    bind n = int(\"42\")\n    show(n { 0 })\nfi\n";
    assert_eq!(Analysis::new(bound).diagnostics(), Vec::new());
    assert_eq!(hover(bound, "n =", 0).code, "bind n: int ~ ParseError");
    assert_offers("task f() -> int ~ @@", &["ParseError", "text"], &["unit"]);
    // Hovering the kind itself shows what it is.
    let named = "task age(line: text) -> int ~ ParseError\n    <- int(line) {\n        failure e\n        <- fail e\n    }\nfi\n\ntask main()\n    show(age(\"1\") { 0 })\nfi\n";
    assert_eq!(Analysis::new(named).diagnostics(), Vec::new());
    assert!(hover(named, "ParseError", 0).code.starts_with("kind ParseError"), "{:?}", hover(named, "ParseError", 0).code);
}

#[test]
fn the_kind_reading_a_number_from_input_sends_is_known_to_the_editor() {
    // `InputError`, `Ended` and `NoNumber` are declared by no file either, and a
    // `scan` that reads a number names no region.
    let source = "task main()\n    match scan(\"Age: \", int)\n    case {age}\n        show(age)\n    case failure e\n        match e\n        case Ended\n            show(\"nothing\")\n        case NoNumber why\n            match why\n            case NotANumber\n                show(\"no number\")\n            case TooLarge\n                show(\"too large\")\n            fi\n        fi\n    fi\nfi\n";
    assert_eq!(Analysis::new(source).diagnostics(), Vec::new());
    assert!(hover(source, "Ended", 0).doc.contains("input has ended"), "{:?}", hover(source, "Ended", 0).doc);
    assert!(hover(source, "NoNumber why", 0).code.starts_with("NoNumber(why: ParseError)"), "{:?}", hover(source, "NoNumber why", 0).code);
    assert!(hover(source, "scan", 0).doc.contains("names no region"), "{:?}", hover(source, "scan", 0).doc);
    // The type it gives, with the compiler's types and with the types read from
    // the source while the file doesn't check.
    for checks in [true, false] {
        let shown = if checks { "show(age { 0 })" } else { "show(unknown)" };
        let bound = format!("task main()\n    bind age = scan(\"Age: \", int)\n    bind size = scan(\"Size: \", float)\n    {}\nfi\n", shown);
        assert_eq!(Analysis::new(&bound).diagnostics().is_empty(), checks);
        assert_eq!(hover(&bound, "age =", 0).code, "bind age: int ~ InputError", "checks: {}", checks);
        assert_eq!(hover(&bound, "size =", 0).code, "bind size: float ~ InputError", "checks: {}", checks);
    }
    // The two types this form reads, offered after the prompt, and the kind
    // where an error type goes.
    assert_eq!(labels(&in_main("bind age = scan(\"Age: \", @@)")), ["int", "float"]);
    assert_offers("task f() -> int ~ @@", &["InputError", "ParseError"], &["unit"]);
    // A region follows a line read, and never a number read.
    assert_offers(&in_main("bind line = scan(\"Name: \") @@"), &["at"], &[]);
    assert_offers(&in_main("bind age = scan(\"Age: \", int) @@"), &[], &["at"]);
}

#[test]
fn the_cases_of_a_kind_no_file_declares_are_offered() {
    // Its variants are in no file, so they come from the language itself; a kind
    // the file does declare must not be offered in their place.
    let of = |body: &str| format!("kind Light\n    Red\n    Dim(n: int)\nfi\n\ntask why(e: InputError) -> int\n    {}\n    <- 0\nfi\n", body);
    assert_eq!(labels(&of("match e\n    case @@\n    fi")), ["Ended", "NoNumber", "_", "all remaining cases"]);
    assert_eq!(labels(&of("when e is @@\n    fi")), ["Ended", "NoNumber"]);
    assert_eq!(labels(&of("match e\n    case InputError.@@\n    fi")), ["Ended", "NoNumber"]);
    // A case already written is not offered again, and `is` takes one case only.
    assert_eq!(labels(&of("match e\n    case Ended\n        show(0)\n    case @@\n    fi")), ["NoNumber", "_"]);
    // The variant that holds a reason is taken apart by a pattern, and written
    // as a value with parentheses.
    let pattern = item(&of("match e\n    case @@\n    fi"), "NoNumber");
    assert_eq!(pattern.snippet.as_deref(), Some("NoNumber ${why}"));
    assert_eq!(pattern.detail, "InputError (why: ParseError)");
    let value = item("task f() -> int ~ ParseError\n    <- fail ParseError.@@\nfi\n", "NotANumber");
    assert_eq!(value.detail, "ParseError");
    assert_eq!(item(&of("bind e2 = InputError.@@"), "NoNumber").snippet.as_deref(), Some("NoNumber(${why})"));
    // The match with every case, offered for a value of one.
    assert_eq!(
        item(&of("@@"), "match e").snippet.as_deref(),
        Some("match e\ncase Ended\n\t\ncase NoNumber ${why}\n\t${}\nfi")
    );
}

#[test]
fn hover_reads_declared_types_when_the_file_doesnt_check() {
    let source = PROGRAM.replace(
        "    @@\n",
        "    show(missing)\n    bind q = Point { x: 1, y: 2 }\n    bind far = line.finish\n    bind made = origin()\n    bind c = Circle(2.0)\n    show(q)\n",
    );
    assert_eq!(hover(&source, "q =", 0).code, "bind q: Point");
    assert_eq!(hover(&source, "far", 0).code, "bind far: ^Point@points");
    assert_eq!(hover(&source, "made", 0).code, "bind made: Point");
    assert_eq!(hover(&source, "c =", 0).code, "bind c: Shape");
}

const REGIONAL: &str = "\
shape Node
    value: int
    next: maybe ^Node
fi

kind Tree
    Leaf
    Branch(left: ^Tree, right: ^Tree)
fi

task main()
    CHECK
    region r
    bind leaf = new Tree(Tree.Leaf) at r
    bind branch = Tree.Branch(leaf, leaf)
    bind bare = Branch(leaf, leaf)
    bind lone = new Node(Node { value: 1, next: none }) at r
    bind linked = Node { value: 2, next: lone }
    bind following = linked.next
    bind count = linked.value
    match branch
    case Branch left right
        show(1)
    case Leaf
        show(2)
    fi
fi
";

#[test]
fn values_holding_pointers_are_in_the_region_of_their_parts_whether_or_not_the_file_checks() {
    for checks in [true, false] {
        let source = REGIONAL.replace("CHECK", if checks { "show(0)" } else { "show(missing)" });
        assert_eq!(Analysis::new(&source).diagnostics().is_empty(), checks);
        let code = |needle: &str| hover(&source, needle, 0).code;
        let when = if checks { "checked" } else { "declared" };
        assert_eq!(code("branch ="), "bind branch: Tree@r", "{}", when);
        assert_eq!(code("bare ="), "bind bare: Tree@r", "{}", when);
        assert_eq!(code("lone ="), "bind lone: ^Node@r", "{}", when);
        assert_eq!(code("linked ="), "bind linked: Node@r", "{}", when);
        assert_eq!(code("following ="), "bind following: maybe ^Node@r", "{}", when);
        assert_eq!(code("count ="), "bind count: int", "{}", when);
        assert_eq!(code("left right"), "left: ^Tree@r", "{}", when);
    }
}

#[test]
fn a_value_turned_into_a_number_reads_as_that_number() {
    // `int(x)` and `float(x)` are calls named by the type they give, so the
    // editor reads them as that number, whether or not the file checks.
    let source = "task main()\n    bind n = 7\n    bind f = float(n)\n    bind w = int(2.9)\n    bind c = int(n > 3)\nfi\n";
    assert_eq!(Analysis::new(source).diagnostics(), Vec::new());
    assert_eq!(hover(source, "f =", 0).code, "bind f: float");
    assert_eq!(hover(source, "w =", 0).code, "bind w: int");
    assert_eq!(hover(source, "c =", 0).code, "bind c: int");
    let broken = source.replace("    bind c =", "    show(missing)\n    bind c =");
    assert_eq!(hover(&broken, "f =", 0).code, "bind f: float");
    assert_eq!(hover(&broken, "w =", 0).code, "bind w: int");
    // Hovering the name of the call says what it does.
    assert!(hover(source, "float(n)", 0).doc.contains("float(n) turns an int"), "{:?}", hover(source, "float(n)", 0).doc);
}

#[test]
fn hover_reads_operators_in_initializers_when_the_file_doesnt_check() {
    let source = PROGRAM.replace(
        "    @@\n",
        "    show(missing)\n    bind less = total < 2\n    bind wrapped = (total == 1)\n    bind both = not less and wrapped\n    \
         bind mixed = float(1) + 2.5\n    bind whole = 2 * total - -(total % 3)\n    bind inner = (total)\n    bind unknown = total + missing\n    bind crossed = 1 + 2.5\n",
    );
    for (needle, code) in [
        ("less =", "bind less: bool"),
        ("wrapped =", "bind wrapped: bool"),
        ("both =", "bind both: bool"),
        ("mixed =", "bind mixed: float"),
        // An int and a float don't mix, so the arithmetic has no type to read.
        ("crossed =", "bind crossed"),
        ("whole =", "bind whole: int"),
        ("inner =", "bind inner: int"),
        // An operand of unknown type leaves the arithmetic's type unknown.
        ("unknown =", "bind unknown"),
    ] {
        assert_eq!(hover(&source, needle, 0).code, code, "at {:?}", needle);
    }
}

const OUTCOMES: &str = "\
kind Problem
    Empty
    Bad(at: int)
fi

task parse(t: text) -> int ~ Problem
    <- 1
fi

task main()
    bind m: maybe int = none
    bind r = parse(\"1\")
    hold total = 0
    @@
fi
";

fn in_outcomes(code: &str) -> String {
    OUTCOMES.replace("    @@\n", &format!("    {}\n", code))
}

#[test]
fn types_offer_maybe_and_error_types() {
    assert_offers(&in_outcomes("bind x: @@"), &["maybe", "int", "Problem"], &["unit", "m"]);
    // After `maybe`, another type, but not `maybe` or `unit` again.
    assert_offers(&in_outcomes("bind x: maybe @@"), &["int", "Problem"], &["maybe", "unit"]);
    assert_offers(&in_outcomes("bind x: maybe ^@@"), &["int"], &["m"]);
    // After `or` in a type, the error type, which is never unit or a maybe.
    assert_offers("task f() -> int ~ @@\nfi\n", &["text"], &["unit", "maybe"]);
    assert_offers(&in_outcomes("bind x: int ~ @@"), &["Problem"], &["m", "total"]);
    // `or` between values is still an expression.
    assert_offers(&in_outcomes("show(total == 1 or @@)"), &["total", "m"], &["Problem"]);
}

#[test]
fn case_and_is_offer_the_patterns_of_maybe_and_error_values() {
    let marked = in_outcomes("match m\n    case @@\n    fi");
    assert_offers(&marked, &["{value}", "none", "all remaining cases"], &["failure e", "Empty"]);
    assert_eq!(item(&marked, "{value}").snippet.as_deref(), Some("{${value}}"));
    assert_eq!(item(&marked, "all remaining cases").snippet.as_deref(), Some("{${value}}\n\t\ncase none\n\t${}"));
    let marked = in_outcomes("match r\n    case {n}\n        show(n)\n    case @@\n    fi");
    assert_offers(&marked, &["failure e"], &["{value}", "none", "all remaining cases"]);
    assert_eq!(item(&marked, "failure e").snippet.as_deref(), Some("failure ${e}"));

    assert_offers(&in_outcomes("when m is @@"), &["{value}", "none"], &["failure e", "total"]);
    assert_offers(&in_outcomes("when r is @@"), &["{value}", "failure e"], &["none"]);
    assert_offers(&in_outcomes("when parse(\"x\") { failure _\n        0\n    } is @@"), &[], &[]);
    // The names a pattern or plan B declares are new.
    for marked in [
        in_outcomes("when m is {@@"),
        in_outcomes("when r is failure @@"),
        in_outcomes("when parse(\"2\") is Bad @@"),
        in_outcomes("bind n = r { failure @@"),
        in_outcomes("bind n = r {\n        failure @@"),
    ] {
        assert_eq!(labels(&marked), Vec::<String>::new(), "at {:?}", marked);
    }
    // A kind's `_` case.
    let marked = PROGRAM.replace("    @@\n", "    match figure\n    case Dot\n        show(1)\n    case @@\n    fi\n");
    assert_offers(&marked, &["_", "Circle"], &["Dot"]);
}

#[test]
fn plan_b_blocks_offer_values_and_statements() {
    assert_offers(&in_outcomes("bind n = m { @@"), &["total", "m", "fail", "none"], &[]);
    assert_offers(&in_outcomes("bind n = m {\n        @@"), &["bind", "total", "when"], &[]);
    // `}` closes a plan B, not `fi`, and only its first line may name the failure.
    let marked = in_outcomes("when total > 0\n        bind n = r {\n            @@");
    assert_offers(&marked, &["failure", "bind"], &["fi", "else"]);
    assert_eq!(item(&marked, "failure").snippet.as_deref(), Some("failure ${e}"));
    assert_offers(&in_outcomes("bind n = r {\n        failure e\n        @@"), &["bind"], &["failure", "fi"]);
    // After the plan B closes, the blocks around it continue.
    assert_offers(&in_outcomes("when total > 0\n        bind n = r {\n            0\n        }\n        @@"), &["fi", "else"], &["failure"]);
    assert_offers(&in_outcomes("@@"), &["match m", "match r", "when … is", "loop … is"], &[]);
    assert_eq!(item(&in_outcomes("@@"), "match r").snippet.as_deref(), Some("match r\ncase {${value}}\n\t\ncase failure ${e}\n\t${}\nfi"));
}

#[test]
fn hover_describes_maybe_and_error_names() {
    let source = OUTCOMES.replace(
        "    @@\n",
        "    bind n = r {\n        failure problem\n        show(0)\n        0\n    }\n    when m is {value}\n        show(value + n)\n    fi\n",
    );
    assert_eq!(code_and_doc(hover(&source, "problem", 0)), ("failure problem: Problem".to_string(), "The failure this plan B handles.".to_string()));
    assert_eq!(code_and_doc(hover(&source, "value + n", 0)), ("value: int".to_string(), "Bound by an is condition.".to_string()));
    assert_eq!(hover(&source, "m:", 0).code, "bind m: maybe int");
    assert_eq!(hover(&source, "r =", 0).code, "bind r: int ~ Problem");
    assert_eq!(hover(&source, "maybe int", 0).code, "maybe T");
    assert_eq!(hover(&source, "none", 0).code, "none");
    // `~` explains itself (its class is an operator, as the snapshots show).
    assert_eq!(hover(&source, "~ Problem", 0).code, "T ~ E");
    // The two words say which is which.
    assert_eq!(hover(&source, "failure problem", 0).code, "failure e");
    assert!(hover(&source, "failure problem", 0).doc.contains("Names the failure"), "{:?}", hover(&source, "failure problem", 0).doc);
    let sending = "task f() -> int ~ text\n    <- fail \"x\"\nfi\n";
    assert!(hover(sending, "fail \"x\"", 0).doc.contains("Makes a failure"), "{:?}", hover(sending, "fail \"x\"", 0).doc);
}

#[test]
fn hover_reads_maybe_and_error_names_when_the_file_doesnt_check() {
    let source = OUTCOMES.replace(
        "    @@\n",
        "    show(missing)\n    bind a = m { 0 }\n    bind b = r {\n        failure problem\n        0\n    }\n    \
         when m is {value}\n        show(value)\n    fi\n    when r is failure wrong\n        show(1)\n    fi\n    \
         match r\n    case {got}\n        show(got)\n    case failure bad\n        show(1)\n    fi\n",
    );
    for (needle, code) in [
        ("a =", "bind a: int"),
        ("b =", "bind b: int"),
        ("problem", "failure problem: Problem"),
        ("value}", "value: int"),
        ("wrong", "wrong: Problem"),
        ("got}", "got: int"),
        ("bad", "bad: Problem"),
    ] {
        assert_eq!(hover(&source, needle, 0).code, code, "at {:?}", needle);
    }
}

const REGIONS: &str = "\
shape Node
    value: int
    next: maybe ^Node
fi

task push(r: region, list: maybe ^Node@r) -> ^Node@r
    <- new Node(Node { value: 1, next: list }) at r
fi

task sum(list: maybe ^Node@s) -> int
    @@
fi

task main()
    region nodes
    bind first = push(nodes, none)
fi
";

#[test]
fn regions_are_offered_where_a_region_is_named() {
    let in_main = |code: &str| REGIONS.replace("    @@\n", "    <- 0\n").replace("    bind first = push(nodes, none)\n", &format!("    bind first = push(nodes, none)\n    {}\n", code));
    // After `@` in a type, every visible region.
    assert_offers(&in_main("hold later: maybe ^Node@ @@"), &["nodes"], &["first", "int"]);
    // `new` allocates, so a region a task only names in its types isn't offered.
    assert_offers(&in_main("bind two = new int(2) at @@"), &["nodes"], &["first"]);
    assert_offers(&in_main("bind two = new@ @@"), &["nodes"], &["first"]);
    let in_sum = |code: &str| REGIONS.replace("    @@\n", &format!("    {}\n", code));
    assert_offers(&in_sum("bind p: maybe ^int@ @@"), &["s"], &[]);
    assert_offers(&in_sum("bind p = new int(1) at @@"), &[], &["s"]);
    assert_eq!(item(&in_sum("bind p: maybe ^int@ @@"), "s").detail, "region, named in the task's types");
    // In a header, the regions named so far on its line.
    assert_offers("task link(a: ^int@left, b: ^int@ @@", &["left"], &[]);
    // A parameter's type can be `region`; a statement can declare one.
    assert_offers("task make(r: @@", &["region", "int"], &[]);
    assert_offers(&in_main("bind x: @@"), &["int"], &["region"]);
    assert_offers(&in_main("@@"), &["region"], &["nodes", "free", "new … at"]);
    // A region is a value only as the argument for a region parameter, and there
    // only the regions the caller can allocate in are offered.
    assert_offers(&in_main("show(sum(push(@@"), &["nodes"], &["first"]);
    assert_offers(&in_main("show(sum(push(nod@@"), &["nodes"], &["first"]);
    assert_offers(&in_main("show(sum(push(nodes, @@"), &["first"], &["nodes"]);
    assert_offers(&in_main("show(@@"), &["first"], &["nodes"]);
    assert_offers(&in_main("show(sum(@@"), &["first"], &["nodes"]);
    assert_offers(&in_sum("bind p = push(@@"), &[], &["s", "list"]);
    // A task the file doesn't declare may take one.
    assert_offers(&in_main("later(@@"), &["first", "nodes"], &[]);
    assert_offers(&in_main("bind y = @@"), &["first", "new … at"], &["nodes"]);
    assert_eq!(item(&in_main("bind y = @@"), "new … at").snippet.as_deref(), Some("new ${T}(${value}) at ${r}"));
    assert_eq!(labels(&in_main("region @@")), Vec::<String>::new());
}

#[test]
fn hover_describes_regions() {
    let source = REGIONS.replace("    @@\n", "    <- 0\n");
    assert_eq!(code_and_doc(hover(&source, "r: region", 0)).0, "r: region");
    assert!(hover(&source, "r: region", 0).doc.contains("can allocate in"));
    assert_eq!(hover(&source, "s) -> int", 0).code, "region s");
    assert!(hover(&source, "s) -> int", 0).doc.contains("can't allocate in it"));
    assert_eq!(hover(&source, "nodes\n", 0).code, "region nodes");
    assert_eq!(hover(&source, "region nodes", 0).code, "region r");
    assert_eq!(hover(&source, "new Node", 0).code, "new T(value) at r");
    assert_eq!(hover(&source, "first =", 0).code, "bind first: ^Node@nodes");
    // While the file doesn't check, written types still show their regions.
    let broken = source.replace("    <- 0\n", "    <- missing\n").replace("bind first = push(nodes, none)", "bind first: maybe ^Node@nodes = none\n    bind two = new@nodes int(2)");
    assert_eq!(hover(&broken, "first:", 0).code, "bind first: maybe ^Node@nodes");
    assert_eq!(hover(&broken, "two =", 0).code, "bind two: ^int@nodes");
    assert_eq!(hover(&broken, "list: maybe", 1).code, "list: maybe ^Node@s");
    // `at` names a region, and `in` counts through a range.
    assert!(hover(&source, "at r", 0).doc.contains("Allocates in region r"));
    assert!(hover("task main()\n    each i in 0..3\n        show(i)\n    fi\nfi\n", "in 0", 0).doc.contains("Counts from a"));
}

#[test]
fn call_results_are_in_the_callers_regions() {
    let source = "\
shape Node
    value: int
    next: maybe ^Node
fi

task push(r: region, list: maybe ^Node@r) -> ^Node@r
    <- new Node(Node { value: 1, next: list }) at r
fi

task first(list: maybe ^Node@q) -> maybe ^Node@q
    <- list
fi

task main()
    region nodes
    bind a = push(nodes, none)
    bind b = push(nodes, first(a) { a })
    bind c = first(none) { a }
    when first(none) is {n}
        show(n^.value)
    fi
    @@
fi
";
    let checks = source.replace("    @@\n", "");
    let broken = source.replace("    @@\n", "    show(missing)\n");
    for source in [&checks, &broken] {
        // The task's `r` and `q` are the caller's `nodes`: from the region
        // argument, from an argument's type, or from a plan B's fallback.
        assert_eq!(hover(source, "a =", 0).code, "bind a: ^Node@nodes");
        assert_eq!(hover(source, "b =", 0).code, "bind b: ^Node@nodes");
        assert_eq!(hover(source, "c =", 0).code, "bind c: ^Node@nodes");
        // A result no argument gives a region to is in none, which hover explains.
        let n = hover(source, "n}", 0);
        assert_eq!(n.code, "n: ^Node@_");
        assert!(n.doc.contains("@_ means no region"), "{:?}", n.doc);
    }
}

#[test]
fn results_serialize_to_the_json_the_editor_reads() {
    let marked = in_main("show(line.finish.@@)");
    let (completions, _) = complete(&marked);
    let json = serde_json::to_value(&completions.items[0]).unwrap();
    assert_eq!(json, serde_json::json!({ "label": "x", "kind": "field", "detail": "int", "apply": "^.x", "boost": 3 }));
    let h = hover(PROGRAM, "origin()", 0);
    let json = serde_json::to_value(&h).unwrap();
    assert_eq!(json["code"], "task origin() -> Point");
    assert!(json.get("from").is_some() && json.get("doc").is_some());
}

#[test]
fn types_follow_a_region_and_error_types_follow_a_regional_type() {
    let regions = "shape Node\n    value: int\n    next: maybe ^Node\nfi\n\ntask main()\n    region nodes\n    @@\nfi\n";
    let in_regions = |code: &str| regions.replace("    @@\n", &format!("    {}\n", code));
    assert_offers(&in_regions("bind two = new@nodes @@"), &["int", "Node", "maybe"], &["main", "show", "true"]);
    assert_offers("shape Node\n    value: int\nfi\ntask f(r: region) -> ^Node@r ~ @@", &["text", "Node"], &["unit", "maybe", "f"]);
}

#[test]
fn a_plan_b_naming_its_error_is_a_block_of_its_own() {
    let source = "task parse(t: text) -> int ~ text\n    <- 1\nfi\ntask main()\n    when true\n        bind r = parse(\"x\") { failure e\n            @@\n        }\n    fi\nfi\n";
    assert_offers(source, &["show", "e"], &["fi", "else"]);
    // While the file doesn't check, the binding still has the value's type, not the error's.
    let broken = "kind Problem\n    Empty\nfi\ntask parse(t: text) -> int ~ Problem\n    <- 1\nfi\ntask main()\n    show(missing)\n    bind r = parse(\"x\") { failure e\n        0\n    }\n    show(r)\nfi\n";
    assert_eq!(hover(broken, "r = parse", 0).code, "bind r: int");
}

#[test]
fn only_a_value_that_can_fail_has_an_error_to_name() {
    let source = "task main()\n    bind m: maybe int = none\n    bind n = m {\n        @@\n    }\nfi\n";
    assert_offers(source, &["show"], &["fail", "failure"]);
    let failing = "task get() -> int ~ text\n    <- 1\nfi\ntask main()\n    bind n = get() {\n        @@\n    }\nfi\n";
    // The noun names the failure a plan B handles; the verb belongs where a value goes.
    assert_offers(failing, &["failure"], &["fail"]);
    // `fail` as a value only where some type can hold an error.
    assert_offers("task main()\n    bind x = @@\nfi\n", &["true"], &["fail"]);
}

#[test]
fn a_case_binding_of_a_kind_in_a_region_keeps_the_region() {
    let source = "kind Tree\n    Leaf(value: int)\n    Branch(left: ^Tree, right: ^Tree)\nfi\ntask count(t: Tree@r) -> int\n    show(missing)\n    match t\n    case Leaf value\n        <- value\n    case Branch left right\n        <- count(left^) + count(right^)\n    fi\nfi\n";
    assert_eq!(hover(source, "left right", 0).code, "left: ^Tree@r");
}

#[test]
fn block_words_fit_where_they_are() {
    // Right after `match`, only a case.
    let matching = "kind K\n    A(v: int)\n    B\nfi\ntask main()\n    bind k = A(1)\n    match k\n    @@\nfi\n";
    assert_eq!(labels(matching), vec!["case".to_string()]);
    // A `when` has one `else`.
    let second_else = "task main()\n    when true\n        show(1)\n    else\n        @@\n    fi\nfi\n";
    assert_offers(second_else, &["fi", "show"], &["else"]);
    // A maybe's cases include `_`.
    let maybe = "task main()\n    bind m: maybe int = 1\n    match m\n    case {v}\n        show(v)\n    case @@\n    fi\nfi\n";
    assert_offers(maybe, &["none", "_"], &["{value}"]);
}

#[test]
fn after_a_finished_value_only_words_that_continue_it() {
    let in_task = |code: &str| format!("task main()\n    region r\n    bind m: maybe int = 1\n    {}\nfi\n", code);
    assert_offers(&in_task("when m @@"), &["is", "and", "or"], &["m", "show", "new"]);
    assert_offers(&in_task("each i @@"), &["in"], &["and", "m"]);
    assert_offers(&in_task("bind p = new int(1) @@"), &["at", "and"], &["m", "in"]);
    // After a written type, only the `~` that gives it an error side.
    assert_offers("task f() -> int @@", &["~"], &["and", "is", "or"]);
    // Right after an operator, values again.
    assert_offers(&in_task("bind x = 1 + @@"), &["m"], &["and"]);
}

#[test]
fn new_is_offered_only_where_a_region_can_be_allocated_in() {
    assert_offers("task main()\n    bind x = @@\nfi\n", &["true"], &["new", "new … at"]);
    assert_offers("task main()\n    region r\n    bind x = @@\nfi\n", &["new", "new … at"], &[]);
    assert_offers("task f(list: maybe ^int@r)\n    bind x = @@\nfi\n", &["list"], &["new", "new … at"]);
}

#[test]
fn keyword_hovers_describe_fail_is_and_underscore() {
    let source = "task get() -> int ~ text\n    <- fail \"x\"\nfi\ntask main()\n    when get() is {_}\n        show(1)\n    fi\nfi\n";
    assert!(hover(source, "fail", 0).code.starts_with("fail"));
    assert!(hover(source, "is {", 0).code.contains("is"));
    assert_eq!(hover(source, "_}", 0).code, "_");
}

const RESULTS: &str = "\
task divmod(a: int, b: int) -> int, int
    <- a / b, a % b
fi

task main()
    bind q, r = divmod(17, 5)
    hold x = 0
    hold y = 0
    bind first, second = 1.5, true
    @@
fi
";

/// `RESULTS` with `@@` replaced by `code` (which has its own `@@`).
fn in_results(code: &str) -> String {
    RESULTS.replace("    @@\n", &format!("    {}\n", code))
}

#[test]
fn several_results_are_typed_and_completed() {
    assert_eq!(Analysis::new(&RESULTS.replace("    @@\n", "")).diagnostics(), Vec::new());

    // Each name in a list takes the type of the value in its place.
    assert_eq!(hover(RESULTS, "q,", 0).code, "bind q: int");
    assert_eq!(hover(RESULTS, "r =", 0).code, "bind r: int");
    assert_eq!(hover(RESULTS, "first", 0).code, "bind first: float");
    assert_eq!(hover(RESULTS, "second", 0).code, "bind second: bool");
    assert_eq!(hover(RESULTS, "divmod", 0).code, "task divmod(a: int, b: int) -> int, int");

    // The same while the file doesn't check, from the declarations alone.
    let broken = in_results("show(missing)");
    assert_eq!(hover(&broken, "q,", 0).code, "bind q: int");
    assert_eq!(hover(&broken, "second", 0).code, "bind second: bool");
    assert_eq!(hover(&broken, "r =", 0).code, "bind r: int");

    // Another result type after a comma, where `unit` is not one.
    assert_offers("task f() -> int, @@", &["int", "text", "maybe"], &["unit", "f"]);
    assert_offers("task f() -> int, ^@@", &["int"], &["unit"]);
    assert_offers("task f() -> int, int, @@", &["float"], &["unit"]);
    // A parameter list's comma still declares a name.
    assert_offers("task f(a: int, @@", &[], &["int", "a"]);

    // Another name after a comma in a `bind`, then values after the `=`.
    assert_offers(&in_results("bind a, @@"), &[], &["int", "q", "divmod"]);
    assert_offers(&in_results("hold a, b = 1, @@"), &["q", "divmod", "true"], &["bind"]);
    assert_offers(&in_results("x, y = 1, @@"), &["q", "divmod"], &["bind"]);
    assert_offers(&in_results("<- 1, @@"), &["q", "divmod"], &["bind"]);
}

const NAMED: &str = "\
task score(hits: int, misses: int) -> total: int
    total <- hits * 10
    when misses == 0
        <- total + 50
    fi
    total <- total - misses * 5
fi

task main()
    show(score(3, 1))
    @@
fi
";

/// `NAMED` with `@@` replaced by `code` (which has its own `@@`).
fn in_named(code: &str) -> String {
    NAMED.replace("    @@\n", &format!("    {}\n", code))
}

#[test]
fn named_results_are_names_of_the_task() {
    assert_eq!(Analysis::new(&NAMED.replace("    @@\n", "")).diagnostics(), Vec::new());

    // The name in the header is the result, not a type, and it keeps its type
    // where it is set and read.
    let hover_total = hover(NAMED, "total", 0);
    assert_eq!(hover_total.code, "result total: int");
    assert!(hover_total.doc.contains("task score") && hover_total.doc.contains("total <- …"), "{}", hover_total.doc);
    assert_eq!(hover(NAMED, "total <- total", 0).code, "result total: int");
    assert_eq!(hover(NAMED, "total + 50", 0).code, "result total: int");
    assert_eq!(hover(NAMED, "score(3, 1)", 0).code, "task score(hits: int, misses: int) -> total: int");

    // A result's type comes from the header, regions and all.
    let regional = "shape Node\n    value: int\nfi\ntask build(r: region, n: int) -> list: maybe ^Node@r, count: int\n    list, count <- none, 0\nfi\n";
    assert_eq!(hover(regional, "list, count", 0).code, "result list: maybe ^Node@r");
    assert_eq!(hover(regional, "count <- none", 0).code, "result count: int");

    // A named result's type is the task's result type, as a call gives it.
    let broken = "task pair() -> steps: int, peak: int\n    steps, peak <- 0, 1\nfi\ntask main()\n    bind s, p = pair()\n    show(missing)\nfi\n";
    assert_eq!(hover(broken, "s, p = pair", 0).code, "bind s: int");
    assert_eq!(hover(broken, "p = pair", 0).code, "bind p: int");

    // Setting a result is offered as a statement, inside its task only.
    assert_offers(&in_named("@@"), &["show", "when"], &["total <- "]);
    let inside = "task score(hits: int) -> total: int\n    @@\nfi\n";
    assert_offers(inside, &["total <- ", "when", "hits"], &[]);
    assert_eq!(item(inside, "total <- ").snippet.as_deref(), Some("total <- ${value}"));
    assert_eq!(item(inside, "total <- ").detail, "set result int");

    // A result's type is completed after its `:`, where `unit` and `region`
    // are not types a task can send; types after `->` still are offered.
    assert_offers("task f() -> total: @@", &["int", "text", "maybe"], &["total", "region"]);
    assert_offers("task f(r: @@", &["int", "region"], &[]);
    assert_offers("task f() -> @@", &["int", "unit"], &["region"]);

    // Either every result has a name or none does, so a name follows the comma
    // of a named list, as in a parameter list, and nothing is completed there.
    assert_offers("task f() -> total: int, @@", &[], &["int", "maybe", "total", "f", "show"]);
    assert_offers("task f() -> total: int, p@@", &[], &["int", "maybe", "total", "show"]);
    assert_offers("task f() -> total: int, ^@@", &[], &["int", "total", "show"]);
    assert_offers("task f() -> total: int, maybe @@", &[], &["int", "total", "show"]);
    assert_offers("task f() -> total: int, peak: int, @@", &[], &["int", "peak", "show"]);
    // An unnamed list still takes another type.
    assert_offers("task f() -> maybe int, @@", &["int", "maybe"], &["unit"]);
}


