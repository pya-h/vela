//! How parameters take their values, in the editor: `flex n: int`, a parameter
//! the task changes, and `flex& n: int`, the caller's place, which a call gives
//! as `&a`, are parameters whose hover says so, the words before them are
//! keywords, a parameter list offers them, and all of it holds while the file
//! doesn't check. In the sources below, `@@` marks the cursor.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, CompletionItem, LineIndex, TokenClass};

const PROGRAM: &str = "\
task countdown(flex n: int, step: int) -> int
    CHECK
    loop n > 0
        n = n - step
    fi
    <- n
fi

task swap(flex& a: int, flex& b: int)
    a, b = b, a
fi

task main()
    show(countdown(10, 3))
    flex x = 1
    flex y = 2
    swap(&x, &y)
    show(x)
fi
";

fn program(checks: bool) -> String {
    let first = if checks { "show(0)" } else { "show(unknown)" };
    PROGRAM.replace("CHECK", first)
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
    assert_eq!(Analysis::new(&program(true)).diagnostics(), Vec::new());
}

#[test]
fn a_flex_parameter_is_a_parameter_whose_hover_says_it_changes() {
    for checks in [true, false] {
        let source = program(checks);
        let when = if checks { "checked" } else { "declared" };
        let (code, doc) = hover(&source, "n: int");
        assert_eq!(code, "flex n: int", "{}", when);
        assert!(doc.starts_with("A parameter of task countdown."), "{}: {}", when, doc);
        assert!(doc.contains("the caller's variable never does"), "{}: {}", when, doc);
        // Where it is used, too, and a plain one beside it says nothing of the kind.
        assert_eq!(hover(&source, "n > 0").0, "flex n: int", "{}", when);
        assert_eq!(hover(&source, "step: int").0, "step: int", "{}", when);
        let analysis = Analysis::new(&source);
        let class_at = |needle: &str| {
            let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
            analysis.tokens().into_iter().find(|t| t.from <= byte && byte < t.to).map(|t| t.class)
        };
        assert_eq!(class_at("flex n"), Some(TokenClass::Keyword), "{}", when);
        assert_eq!(class_at("n: int"), Some(TokenClass::Parameter), "{}", when);
        assert_eq!(class_at("n - step"), Some(TokenClass::Parameter), "{}", when);
    }
}

#[test]
fn a_parameter_list_offers_flex() {
    for marked in ["task f(@@)\nfi\n", "task f(a: int, @@)\nfi\n", "task f(ho@@)\nfi\n"] {
        let items = completions(marked);
        let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(labels, ["flex", "flex&", "bind", "bind&", "fixed", "fixed&"], "{:?}", marked);
    }
    // A method's too, in a shape, and in a kind once the line says it is one.
    for marked in ["shape S\n    n: int\n\n    alters add(@@)\n    fi\nfi\n", "kind K\n    A\n\n    alters m(@@)\n    fi\nfi\n", "kind K\n    A\n\n    m(@@) -> int\n    fi\nfi\n"] {
        let labels: Vec<String> = completions(marked).into_iter().map(|item| item.label).collect();
        assert_eq!(labels, ["flex", "flex&", "bind", "bind&", "fixed", "fixed&"], "{:?}", marked);
    }
    // Not after the word, where the parameter's name goes.
    assert!(completions("task f(flex @@)\nfi\n").is_empty());
    assert!(completions("task f(flex& @@)\nfi\n").is_empty());
    assert!(completions("task f(bind @@)\nfi\n").is_empty());
    assert!(completions("task f(bind& @@)\nfi\n").is_empty());
    // At a call, what follows '&' is a place: the variables.
    let at_call: Vec<String> = completions(&program(true).replace("swap(&x, &y)", "swap(&x, &@@)")).into_iter().map(|item| item.label).collect();
    assert!(at_call.contains(&"y".to_string()), "{:?}", at_call);
}

#[test]
fn changing_a_parameter_that_never_changes_says_how_to_write_one_that_does() {
    let source = "\
task f(n: int)
    n = 2
fi

task main()
    f(1)
fi
";
    let diagnostics = Analysis::new(source).diagnostics();
    let [diagnostic] = diagnostics.as_slice() else { panic!("{:?}", diagnostics) };
    assert_eq!(
        diagnostic.message,
        "cannot assign to immutable variable 'n'; a parameter that changes is written 'flex n: int', and is the task's own copy"
    );
}

#[test]
fn a_flex_ref_parameter_is_the_callers_place() {
    for checks in [true, false] {
        let source = program(checks);
        let when = if checks { "checked" } else { "declared" };
        let (code, doc) = hover(&source, "a: int");
        assert_eq!(code, "flex& a: int", "{}", when);
        assert!(doc.starts_with("A parameter of task swap."), "{}: {}", when, doc);
        assert!(doc.contains("The caller's place"), "{}: {}", when, doc);
        assert_eq!(hover(&source, "a, b = b").0, "flex& a: int", "{}", when);
        // The place given at the call is the variable itself.
        assert_eq!(hover(&source, "x, &y").0, "flex x: int", "{}", when);
        let analysis = Analysis::new(&source);
        let class_at = |needle: &str| {
            let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
            analysis.tokens().into_iter().find(|t| t.from <= byte && byte < t.to).map(|t| t.class)
        };
        assert_eq!(class_at("flex& a"), Some(TokenClass::Keyword), "{}", when);
        assert_eq!(class_at("& a"), Some(TokenClass::Keyword), "{}", when);
        assert_eq!(class_at("&x"), Some(TokenClass::Operator), "{}", when);
        assert_eq!(class_at("x, &y"), Some(TokenClass::Variable), "{}", when);
    }
}

#[test]
fn a_call_that_leaves_out_the_mark_is_reported() {
    let source = program(true).replace("swap(&x, &y)", "swap(x, &y)");
    let diagnostics = Analysis::new(&source).diagnostics();
    let [diagnostic] = diagnostics.as_slice() else { panic!("{:?}", diagnostics) };
    assert_eq!(diagnostic.message, "task 'swap' changes 'a', the place it is given, so the call marks that place with '&', as in '&x'");
}

const REFERENCES: &str = "\
shape Counter
    n: int = 0
fi

task main()
    CHECK
    flex x = 1
    flex& y = &x
    y = 5
    fixed& seen = &x
    flex c = Counter {}
    flex& n = &c.n
    n = 2
    show(seen, n)
fi
";

#[test]
fn a_local_reference_hovers_with_its_word_and_type() {
    for checks in [true, false] {
        let source = REFERENCES.replace("CHECK", if checks { "show(0)" } else { "show(unknown)" });
        let when = if checks { "checked" } else { "declared" };
        let (code, doc) = hover(&source, "y = &x");
        assert_eq!(code, "flex& y: int", "{}", when);
        assert!(doc.contains("a change to either is a change to both"), "{}: {}", when, doc);
        assert_eq!(hover(&source, "y = 5").0, "flex& y: int", "{}", when);
        assert_eq!(hover(&source, "seen = &x").0, "fixed& seen: int", "{}", when);
        assert_eq!(hover(&source, "n = &c.n").0, "flex& n: int", "{}", when);
        let analysis = Analysis::new(&source);
        let class_at = |needle: &str| {
            let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
            analysis.tokens().into_iter().find(|t| t.from <= byte && byte < t.to).map(|t| t.class)
        };
        assert_eq!(class_at("& y"), Some(TokenClass::Keyword), "{}", when);
        assert_eq!(class_at("& seen"), Some(TokenClass::Keyword), "{}", when);
        assert_eq!(class_at("&x\n    y"), Some(TokenClass::Operator), "{}", when);
        assert_eq!(class_at("y = 5"), Some(TokenClass::Variable), "{}", when);
    }
    // Nothing is offered for the name being declared.
    assert!(completions("task main()\n    flex x = 1\n    flex& @@\nfi\n").is_empty());
    assert!(completions("task main()\n    flex x = 1\n    fixed& @@\nfi\n").is_empty());
}

#[test]
fn a_reference_that_only_reads_is_reported_where_it_is_changed() {
    let source = REFERENCES.replace("CHECK", "show(0)").replace("    show(seen, n)", "    seen = 1");
    let diagnostics = Analysis::new(&source).diagnostics();
    let [diagnostic] = diagnostics.as_slice() else { panic!("{:?}", diagnostics) };
    assert_eq!(diagnostic.message, "cannot assign to immutable variable 'seen'; 'seen' only reads the place it names, and a reference that changes it is written 'flex& seen = &…'");
}

#[test]
fn fixed_and_fixed_ref_parameters_are_parameters_whose_hover_says_how_they_are_given() {
    let base = "shape Point\n    x: int\nfi\n\ntask look(fixed& p: Point, fixed q: Point) -> bool\n    CHECK\n    <- p == q\nfi\n\ntask main()\n    fixed a = Point { x: 1 }\n    show(look(&a, a))\nfi\n";
    for checks in [true, false] {
        let source = base.replace("CHECK", if checks { "show(0)" } else { "show(unknown)" });
        if checks {
            assert_eq!(Analysis::new(&source).diagnostics(), Vec::new());
        }
        let (code, doc) = hover(&source, "p: Point");
        assert_eq!(code, "fixed& p: Point", "checks: {}", checks);
        assert!(doc.contains("read where it is and never changed"), "{}", doc);
        let (code, doc) = hover(&source, "q: Point");
        assert_eq!(code, "fixed q: Point", "checks: {}", checks);
        assert!(doc.contains("Always a copy"), "{}", doc);
        // The words are keywords, and a use of the name is the parameter.
        let analysis = Analysis::new(&source);
        let class_at = |needle: &str| {
            let byte = source.find(needle).unwrap();
            analysis.tokens().into_iter().find(|t| t.from <= byte && byte < t.to).map(|t| t.class)
        };
        assert_eq!(class_at("fixed& p"), Some(TokenClass::Keyword));
        assert_eq!(class_at("& p"), Some(TokenClass::Keyword));
        assert_eq!(class_at("fixed q"), Some(TokenClass::Keyword));
        assert_eq!(class_at("p == q"), Some(TokenClass::Parameter));
    }
    // A value given without '&' is reported.
    let diagnostics = Analysis::new(&base.replace("CHECK", "show(0)").replace("look(&a, a)", "look(a, a)")).diagnostics();
    assert!(diagnostics.iter().any(|d| d.message.contains("reads 'p' where the caller keeps it, so the call marks that place with '&'")), "{:?}", diagnostics);
}
