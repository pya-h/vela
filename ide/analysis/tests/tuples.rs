//! Tuples in the editor: `(int, text)` types and values, items read as `t.0`,
//! and names that unpack a tuple, with the compiler's types and with the types
//! read from the source while the file doesn't check. In the sources below,
//! `@@` marks the cursor.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, CompletionItem, LineIndex, TokenClass};

const PROGRAM: &str = "\
task divide(a: int, b: int) -> (int, int)
    <- (a / b, a % b)
fi

task named(r: region, line: text@r) -> (text@r, int)
    <- (line, 1)
fi

task find(id: int) -> (int, text) ~ (text, int)
    when id < 0
        <- fail (\"unknown\", id)
    fi
    <- (id, \"found\")
fi

task main()
    CHECK
    region r
    bind pair = (\"Ada\", 36)
    bind nested: (int, (float, text)) = (1, (2.5, \"deep\"))
    bind quotient, remainder = divide(17, 5)
    bind (whole, (part, word)) = nested
    bind line = scan(\"\") at r { \"\" }
    bind kept = named(r, line)
    bind age = pair.1
    bind deep = nested.1.1
    show(age + quotient + remainder + whole)
    hold found: maybe (text, int) = none
    when found is {(who, years)}
        show(who)
        show(years)
    fi
    match find(1)
        case {(code, label)}
            show(code)
            show(label)
        case failure (why, bad)
            show(why)
            show(bad)
    fi
    when pair is hold (person, count)
        count = count + 1
        show(person)
        show(count)
    fi
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

fn hover(source: &str, needle: &str) -> (String, String) {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    let hover = Analysis::new(source).hover(LineIndex::new(source).utf16_of(byte)).unwrap_or_else(|| panic!("no hover at {:?}", needle));
    (hover.code, hover.doc)
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    assert_eq!(Analysis::new(&program(true, "show(part)")).diagnostics(), Vec::new());
}

#[test]
fn tuples_and_the_names_unpacking_them_have_types_whether_or_not_the_file_checks() {
    for checks in [true, false] {
        let source = program(checks, "show(part)\n    show(word)");
        let when = if checks { "checked" } else { "declared" };
        assert_eq!(hover(&source, "pair =").0, "bind pair: (text, int)", "{}", when);
        assert_eq!(hover(&source, "nested:").0, "bind nested: (int, (float, text))", "{}", when);
        assert_eq!(hover(&source, "quotient,").0, "bind quotient: int", "{}", when);
        assert_eq!(hover(&source, "remainder =").0, "bind remainder: int", "{}", when);
        assert_eq!(hover(&source, "whole,").0, "bind whole: int", "{}", when);
        assert_eq!(hover(&source, "part,").0, "bind part: float", "{}", when);
        assert_eq!(hover(&source, "word)").0, "bind word: text", "{}", when);
        assert_eq!(hover(&source, "kept =").0, "bind kept: (text@r, int)", "{}", when);
        assert_eq!(hover(&source, "age =").0, "bind age: int", "{}", when);
        assert_eq!(hover(&source, "deep =").0, "bind deep: text", "{}", when);
    }
}

#[test]
fn an_item_hovers_with_its_type_and_completes_by_position() {
    for checks in [true, false] {
        let source = program(checks, "show(part)");
        let (code, doc) = hover(&source, "1.1\n");
        assert_eq!(code, "item 1: (float, text)", "checks: {}", checks);
        assert!(doc.starts_with("An item of a tuple"), "{}", doc);
        let items = completions(&program(checks, "show(pair.@@)"));
        let pairs: Vec<(&str, &str)> = items.iter().map(|item| (item.label.as_str(), item.detail.as_str())).collect();
        assert_eq!(pairs, [("0", "text"), ("1", "int"), ("str", "str() at r, the value as text in a region, as show writes it"), ("type", "text, the value's type as code writes it")], "checks: {}", checks);
    }
}

#[test]
fn items_and_tuple_types_are_colored() {
    let source = program(true, "show(part)");
    let analysis = Analysis::new(&source);
    let class_at = |needle: &str| {
        let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
        analysis.tokens().into_iter().find(|t| t.from <= byte && byte < t.to).map(|t| t.class)
    };
    assert_eq!(class_at("1\n    bind deep"), Some(TokenClass::Field));
    assert_eq!(class_at("float, text)) ="), Some(TokenClass::TypeBuiltin));
    assert_eq!(class_at("text)) ="), Some(TokenClass::TypeBuiltin));
    assert_eq!(class_at("word)) = nested"), Some(TokenClass::Variable));
}

#[test]
fn changing_an_item_is_reported() {
    let source = program(true, "hold changing = (1, 2)\n    changing.0 = 5");
    let diagnostics = Analysis::new(&source).diagnostics();
    let [diagnostic] = diagnostics.as_slice() else { panic!("{:?}", diagnostics) };
    assert!(diagnostic.message.starts_with("a tuple's items never change"), "{}", diagnostic.message);
}

#[test]
fn names_a_pattern_takes_from_a_tuple_have_types() {
    for checks in [true, false] {
        let source = program(checks, "show(part)");
        let when = if checks { "checked" } else { "declared" };
        assert_eq!(hover(&source, "who)").0, "who: text", "{}", when);
        assert_eq!(hover(&source, "years)").0, "years: int", "{}", when);
        assert_eq!(hover(&source, "code, label").0, "code: int", "{}", when);
        assert_eq!(hover(&source, "label)").0, "label: text", "{}", when);
        assert_eq!(hover(&source, "why)").0, "why: text", "{}", when);
        assert_eq!(hover(&source, "person, count").0, "hold person: text", "{}", when);
        assert_eq!(hover(&source, "count)").0, "hold count: int", "{}", when);
    }
}

#[test]
fn completion_offers_the_patterns_that_take_a_tuple_apart() {
    for checks in [true, false] {
        let items = completions(&program(checks, "when found is @@"));
        let templates: Vec<(&str, Option<&str>)> = items.iter().map(|item| (item.label.as_str(), item.snippet.as_deref())).collect();
        assert_eq!(templates, [("{value}", Some("{(${item0}, ${item1})}")), ("none", None)], "checks: {}", checks);
        let plain = completions(&program(checks, "when pair is @@"));
        assert_eq!(plain.iter().map(|item| item.snippet.as_deref()).collect::<Vec<_>>(), [Some("(${item0}, ${item1})")], "checks: {}", checks);
    }
}

#[test]
fn completion_offers_types_inside_a_tuple_type() {
    for checks in [true, false] {
        let labels = |marked: &str| completions(marked).into_iter().map(|item| item.label).collect::<Vec<_>>();
        let types = ["int".to_string(), "float".to_string(), "bool".to_string(), "text".to_string(), "maybe".to_string()];
        assert_eq!(labels(&program(checks, "bind typed: (int, @@) = (1, 2)")), types, "checks: {}", checks);
        assert_eq!(labels(&program(checks, "bind typed: (@@) = (1, 2)")), types, "checks: {}", checks);
        // A parameter, a field, and a result take items of their own.
        assert_eq!(labels("task f(t: (int, @@))\nfi\n"), types);
        assert_eq!(labels("shape Holder\n    t: (int, @@)\nfi\n"), ["int", "float", "bool", "text", "Holder", "maybe"]);
        assert_eq!(labels("task f() -> (int, @@)\nfi\n"), types);
        // A name of a binding, and one of a parameter, are still being declared,
        // and a parameter may be one the task changes.
        assert!(labels(&program(checks, "bind one, @@ = (1, 2)")).is_empty(), "checks: {}", checks);
        assert_eq!(labels("task f(a: int, @@)\nfi\n"), ["hold", "hold&", "bind", "bind&"]);
    }
}

#[test]
fn a_pattern_that_always_matches_is_reported_where_it_cannot_run() {
    let source = program(true, "when pair is (a, b)\n        show(a)\n    else\n        show(0)\n    fi");
    let diagnostics = Analysis::new(&source).diagnostics();
    let [diagnostic] = diagnostics.as_slice() else { panic!("{:?}", diagnostics) };
    assert!(diagnostic.message.starts_with("a tuple's items always match"), "{}", diagnostic.message);
}

#[test]
fn naming_a_tuples_items_without_parentheses_is_reported() {
    let source = program(true, "when found is {who, years}\n        show(who)\n    fi");
    let diagnostics = Analysis::new(&source).diagnostics();
    let [diagnostic] = diagnostics.as_slice() else { panic!("{:?}", diagnostics) };
    assert!(diagnostic.message.contains("{(name, age)}"), "{}", diagnostic.message);
}

#[test]
fn one_type_in_parentheses_is_grouped_rather_than_a_tuple() {
    // `~` holds the whole type before it, so parentheses are how the other
    // nesting is written, and Studio shows each as it is written.
    let source = "\
task risky(n: int) -> int ~ text
    <- n
fi

task kept(v: maybe (int ~ text)) -> int
    bind inner = v { <- -1 }
    <- inner { -2 }
fi

task main()
    bind held: maybe (int ~ text) = risky(1)
    bind plain: (int) = 2
    show(kept(held), plain)
fi
";
    assert_eq!(Analysis::new(source).diagnostics(), Vec::new());
    assert_eq!(hover(source, "held: maybe").0, "bind held: maybe (int ~ text)");
    assert_eq!(hover(source, "plain: (int)").0, "bind plain: int");
    assert_eq!(hover(source, "inner = v").0, "bind inner: int ~ text");
    // And while the file does not check, where types are read as they are written.
    let typing = source.replace("    show(kept(held), plain)", "    show(unknown)\n    show(kept(held), plain)");
    assert!(!Analysis::new(&typing).diagnostics().is_empty());
    assert_eq!(hover(&typing, "v: maybe").0, "v: maybe (int ~ text)");
    assert_eq!(hover(&typing, "plain: (int)").0, "bind plain: int");
}
