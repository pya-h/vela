//! Diagnostics, outline, go-to-definition, and highlighting of half-typed
//! code, plus a tolerance test that runs every request on every prefix of the
//! example programs.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

use vela_analysis::{Analysis, Diagnostic, LineIndex, Location, OutlineItem, Severity, SymbolKind, TokenClass};

/// The UTF-16 offset of the `nth` (0-based) occurrence of `needle`.
fn at(source: &str, needle: &str, nth: usize) -> usize {
    let byte = source.match_indices(needle).nth(nth).unwrap_or_else(|| panic!("no {:?} #{}", needle, nth)).0;
    LineIndex::new(source).utf16_of(byte)
}

/// The text of a UTF-16 location.
fn text(source: &str, location: Location) -> String {
    let index = LineIndex::new(source);
    source[index.byte_of_utf16(location.from)..index.byte_of_utf16(location.to)].to_string()
}

/// Asserts that the name starting at `(needle, nth)` (the `nth` occurrence
/// of `needle`) is declared at the name starting at `declaration`.
fn assert_definition(source: &str, name_use: (&str, usize), declaration: (&str, usize)) {
    let name: String = declaration.0.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
    let found = Analysis::new(source).definition(at(source, name_use.0, name_use.1));
    let expected = at(source, declaration.0, declaration.1);
    let expected = Location { from: expected, to: expected + name.len(), path: None };
    assert_eq!(found, Some(expected), "{:?} should go to {:?}", name_use, declaration);
}

fn class_at(source: &str, needle: &str, nth: usize) -> TokenClass {
    let offset = at(source, needle, nth);
    Analysis::new(source).tokens().into_iter().find(|t| t.from == offset).unwrap_or_else(|| panic!("no token at {:?}", needle)).class
}

const SHAPES: &str = "\
shape Point
    x: int
    y: int
fi

shape Size
    x: int
fi

kind Light
    Red
    Dimmed(level: int)
fi

task main()
    bind p = Point { y: 2, x: 1 }
    bind s = Size { x: 3 }
    show(p.x + s.x)
    show(total(p))
    match Dimmed(4)
    case Dimmed level
        show(level)
    case Red
        show(0)
    fi
fi

task total(q: Point) -> int
    <- q.x + q.y
fi
";

#[test]
fn definitions_of_declarations_and_uses() {
    // A task used before its declaration.
    assert_definition(SHAPES, ("total", 0), ("total", 1));
    // A declaration goes to itself.
    assert_definition(SHAPES, ("total", 1), ("total", 1));
    // Variables, parameters, variants, types, and case bindings.
    assert_definition(SHAPES, ("p.x", 0), ("p =", 0));
    assert_definition(SHAPES, ("q.x", 0), ("q:", 0));
    assert_definition(SHAPES, ("Dimmed(4)", 0), ("Dimmed(level", 0));
    assert_definition(SHAPES, ("Point {", 0), ("Point\n", 0));
    assert_definition(SHAPES, ("level)", 0), ("level\n", 0));
}

#[test]
fn field_definitions_use_the_type_of_the_value() {
    // Two shapes have a field `x`: the checker's types pick the right one.
    let analysis = Analysis::new(SHAPES);
    let point_x = at(SHAPES, "x: int", 0);
    let size_x = at(SHAPES, "x: int", 1);
    let go = |needle: &str| analysis.definition(at(SHAPES, needle, 0) + needle.len() - 1).map(|l| l.from);
    assert_eq!(go("p.x"), Some(point_x));
    assert_eq!(go("s.x"), Some(size_x));
    // Shape literal fields, in any order.
    assert_eq!(go("{ y"), Some(at(SHAPES, "y: int", 0)));
    assert_eq!(go("Size { x"), Some(size_x));
}

#[test]
fn field_definitions_before_an_error_are_exact_and_after_it_fall_back_to_unique_names() {
    let source = format!(
        "{}task broken()\n    bind q = Point {{ x: 1, y: 2 }}\n    show(q.x)\n    show(missing)\n    show(q.y)\n    show(q.x)\nfi\n",
        SHAPES
    );
    let analysis = Analysis::new(&source);
    assert_eq!(analysis.diagnostics().len(), 1);
    let go = |nth: usize, needle: &str| analysis.definition(at(&source, needle, nth) + 2).map(|l| l.from);
    // `SHAPES` itself uses `q.x` and `q.y` once, in `total`.
    // Checked before the error: the checker's type picks `Point.x`.
    assert_eq!(go(1, "q.x"), Some(at(&source, "x: int", 0)));
    // After it, only `Point` has `y`; both shapes have `x`, so that one is unknown.
    assert_eq!(go(1, "q.y"), Some(at(&source, "y: int", 0)));
    assert_eq!(go(2, "q.x"), None);
}

#[test]
fn variables_are_scoped_to_their_blocks() {
    let source = "\
task main()
    when true
        bind label = 1
        show(label)
    else
        show(label)
        bind label = 2
        show(label)
    fi
    each i in 0..3
        show(i)
    fi
fi
";
    assert_definition(source, ("label", 1), ("label", 0));
    // The `else` branch doesn't see the `when` branch's variable...
    assert_eq!(Analysis::new(source).definition(at(source, "label", 2) + 1), None);
    assert_eq!(class_at(source, "label", 2), TokenClass::Variable);
    // ...and has its own.
    assert_definition(source, ("label", 4), ("label", 3));
    assert_definition(source, ("i)", 0), ("i in", 0));
}

#[test]
fn plan_b_and_is_names_are_scoped_to_their_blocks() {
    let source = "\
task parse(t: text) -> int or text
    <- 1
fi

task main()
    bind m: maybe int = 2
    bind n = parse(\"x\") {
        fail problem
        bind fallback = 0
        show(problem)
        fallback
    }
    show(fallback)
    when m is {value}
        show(value + n)
    else
        show(value)
    fi
    loop m is {again}
        show(again)
    fi
fi
";
    assert_definition(source, ("problem)", 0), ("problem", 0));
    assert_definition(source, ("fallback\n    }", 0), ("fallback =", 0));
    // A plan B's names end with its block, and an `is` pattern's with its branch.
    assert_eq!(Analysis::new(source).definition(at(source, "fallback)", 0)), None);
    assert_definition(source, ("value + n", 0), ("value}", 0));
    assert_eq!(Analysis::new(source).definition(at(source, "value)", 0)), None);
    assert_definition(source, ("again)", 0), ("again}", 0));
    assert_eq!(class_at(source, "is {value}", 0), TokenClass::Keyword);
    assert_eq!(class_at(source, "maybe int", 0), TokenClass::Keyword);
    assert_eq!(class_at(source, "text\n    <- 1", 0), TokenClass::TypeBuiltin);
    assert_eq!(class_at(source, "fail problem", 0), TokenClass::Keyword);

    // Blocks inside a plan B don't end it early.
    let source = "\
kind Light
    Red
    Green
fi

task main()
    bind m: maybe int = none
    bind n = m {
        bind light = Red
        match light
        case Red
            show(1)
        case Green
            show(2)
        fi
        when true
            show(3)
        else
            show(4)
        fi
        bind inside = 5
        inside
    }
    show(inside)
    bind own = m {
        own
    }
    show(own)
fi
";
    assert_definition(source, ("inside\n    }", 0), ("inside =", 0));
    assert_eq!(Analysis::new(source).definition(at(source, "inside)", 0)), None);
    // A binding isn't visible in its own plan B, only after it.
    assert_eq!(Analysis::new(source).definition(at(source, "own\n    }", 0)), None);
    assert_definition(source, ("own)", 0), ("own =", 0));
}

#[test]
fn regions_resolve_to_their_declarations() {
    let source = "\
shape Node
    value: int
    next: maybe ^Node
fi

task push(r: region, list: maybe ^Node@r) -> ^Node@r
    <- new Node(Node { value: 1, next: list }) at r
fi

task sum(list: maybe ^Node@s) -> int
    <- 0
fi

task main()
    region nodes
    bind first = push(nodes, none)
    bind second = new@nodes int(2)
    when true
        region inner
        show(sum(first))
    fi
    show(sum(push(nodes, first)))
fi
";
    assert_eq!(class_at(source, "r: region", 0), TokenClass::Region);
    assert_eq!(class_at(source, "region,", 0), TokenClass::Keyword);
    assert_eq!(class_at(source, "r)", 0), TokenClass::Region);
    // A header's `@r` names the region parameter; `in r` refers to it.
    assert_definition(source, ("r) ->", 0), ("r: region", 0));
    assert_definition(source, ("r\n", 0), ("r: region", 0));
    // A region the header only names is declared where it first appears.
    assert_definition(source, ("s) -> int", 0), ("s) -> int", 0));
    assert_eq!(class_at(source, "s) -> int", 0), TokenClass::Region);
    // Region statements, used as arguments and after `new@`.
    assert_definition(source, ("nodes, none", 0), ("nodes\n", 0));
    assert_definition(source, ("nodes int", 0), ("nodes\n", 0));
    assert_eq!(class_at(source, "nodes, first", 0), TokenClass::Region);
    // A region declared in a block ends with it.
    assert_definition(source, ("inner", 0), ("inner", 0));
    let analysis = Analysis::new(source);
    assert_eq!(analysis.diagnostics(), []);
    assert_eq!(class_at(source, "int(2)", 0), TokenClass::TypeBuiltin);
}

#[test]
fn half_typed_code_still_resolves_and_highlights() {
    // The first task is never closed and a string is unterminated.
    let source = "\
task first(n: int) -> int
    bind twice = n * 2
    show(\"not done
    <- twice

task second()
    show(first(1) $)
fi
";
    let analysis = Analysis::new(source);
    assert_definition(source, ("twice", 1), ("twice", 0));
    assert_definition(source, ("n * 2", 0), ("n:", 0));
    assert_definition(source, ("first(1)", 0), ("first(n", 0));
    assert_eq!(class_at(source, "\"not done", 0), TokenClass::String);
    assert_eq!(class_at(source, "<- twice", 0), TokenClass::Keyword);
    assert_eq!(class_at(source, "second", 0), TokenClass::Task);
    assert_eq!(class_at(source, "$", 0), TokenClass::Invalid);
    let outline: Vec<(String, usize)> = analysis.outline().into_iter().map(|item| (item.name, item.to)).collect();
    // The unclosed task ends at its last token.
    let first_end = at(source, "twice\n\ntask", 0) + "twice".len();
    assert_eq!(outline, [("first".to_string(), first_end), ("second".to_string(), at(source, "\nfi", 0) + 3)]);
}

#[test]
fn diagnostics_cover_the_token_the_error_points_at() {
    assert_eq!(Analysis::new(SHAPES).diagnostics(), []);

    // Non-ASCII text before the error: offsets are UTF-16.
    let source = "task main()\n    show(\"é𝄞\")\n    show(missing)\nfi\n";
    let from = at(source, "missing", 0);
    assert_eq!(
        Analysis::new(source).diagnostics(),
        [Diagnostic {
            from,
            to: from + 7,
            line: Some(3),
            col: Some(10),
            message: "unknown variable 'missing'".to_string(),
            severity: Severity::Error,
        }]
    );

    // An error inside a token covers one character; one at the end of a line covers nothing.
    let escape = "task main()\n    show(\"a\\qb\")\nfi\n";
    let d = &Analysis::new(escape).diagnostics()[0];
    assert_eq!((d.from, d.to), (at(escape, "\\q", 0), at(escape, "\\q", 0) + 1));
    let eol = "task main()\n    bind x =\nfi\n";
    let d = &Analysis::new(eol).diagnostics()[0];
    assert_eq!((d.from, d.to, d.message.as_str()), (at(eol, "=\n", 0) + 1, at(eol, "=\n", 0) + 1, "expected an expression, got end of line"));
}

#[test]
fn outline_lists_items_with_their_members() {
    let analysis = Analysis::new(SHAPES);
    let summary = |item: &OutlineItem| {
        let children: Vec<String> = item.children.iter().map(|c| format!("{} {}", c.name, c.detail)).collect();
        (item.name.clone(), item.kind, item.detail.clone(), children)
    };
    let outline: Vec<_> = analysis.outline().iter().map(summary).collect();
    assert_eq!(
        outline,
        [
            ("Point".into(), SymbolKind::Shape, "".into(), vec!["x int".to_string(), "y int".to_string()]),
            ("Size".into(), SymbolKind::Shape, "".into(), vec!["x int".to_string()]),
            ("Light".into(), SymbolKind::Kind, "".into(), vec!["Red ".to_string(), "Dimmed (level: int)".to_string()]),
            ("main".into(), SymbolKind::Task, "()".into(), vec![]),
            ("total".into(), SymbolKind::Task, "(q: Point) -> int".into(), vec![]),
        ]
    );
    let light = &analysis.outline()[2];
    assert_eq!(light.children[1].children[0].name, "level");
    assert_eq!(text(SHAPES, Location { from: light.from, to: light.to, path: None }), "kind Light\n    Red\n    Dimmed(level: int)\nfi");
    assert_eq!(text(SHAPES, Location { from: light.name_from, to: light.name_to, path: None }), "Light");
}

#[test]
fn results_serialize_to_the_json_the_editor_reads() {
    let source = "task main()\n    show(x)\nfi\n";
    let analysis = Analysis::new(source);
    let json = serde_json::to_value((analysis.tokens()[0].clone(), &analysis.diagnostics(), &analysis.outline())).unwrap();
    assert_eq!(
        json,
        serde_json::json!([
            { "from": 0, "to": 4, "class": "keyword" },
            [{ "from": 21, "to": 22, "line": 2, "col": 10, "message": "unknown variable 'x'", "severity": "error" }],
            [{ "name": "main", "kind": "task", "detail": "()", "from": 0, "to": 26, "nameFrom": 5, "nameTo": 9, "children": [] }]
        ])
    );
}

fn vela_files(dir: &str) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(dir))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "vela"))
        .collect();
    files.sort();
    files
}

/// Whether every `${` in a snippet template is closed by a `}`.
fn balanced_snippet(template: &str) -> bool {
    let mut rest = template;
    while let Some(open) = rest.find("${") {
        match rest[open..].find('}') {
            Some(close) => rest = &rest[open + close + 1..],
            None => return false,
        }
    }
    true
}

/// Runs completion and hover at `offset` and checks the results.
fn check_cursor(analysis: &Analysis, offset: usize, length: usize, what: &str) {
    let completions = analysis.complete(offset);
    assert!(completions.from <= completions.to && completions.to == offset, "{} at {}: {:?}", what, offset, (completions.from, completions.to));
    for item in &completions.items {
        assert!(!item.label.is_empty(), "{} at {}: {:?}", what, offset, item);
        assert!(item.snippet.as_deref().is_none_or(balanced_snippet), "{} at {}: {:?}", what, offset, item);
    }
    if let Some(hover) = analysis.hover(offset) {
        assert!(hover.from <= offset && offset <= hover.to && hover.to <= length && !hover.code.is_empty(), "{} at {}: {:?}", what, offset, hover);
    }
}

/// Runs every request on `source` and checks the results are well formed:
/// completion and hover at the end, or, if `everywhere`, at every token.
fn check_well_formed(source: &str, what: &str, everywhere: bool) {
    let analysis = Analysis::new(source);
    let length = source.encode_utf16().count();
    let tokens = analysis.tokens();
    let mut end = 0;
    for token in &tokens {
        assert!(token.from >= end && token.from < token.to && token.to <= length, "{}: {:?}", what, token);
        end = token.to;
    }
    for d in analysis.diagnostics() {
        assert!(d.from <= d.to && d.to <= length, "{}: {:?}", what, d);
    }
    fn check_outline(items: &[OutlineItem], length: usize, what: &str) {
        for item in items {
            assert!(item.from <= item.name_from && item.name_to <= item.to && item.to <= length, "{}: {:?}", what, item);
            check_outline(&item.children, length, what);
        }
    }
    check_outline(&analysis.outline(), length, what);
    check_cursor(&analysis, length, length, what);
    if everywhere {
        for token in &tokens {
            check_cursor(&analysis, token.from, length, what);
            check_cursor(&analysis, token.to, length, what);
        }
    }
    // A definition always names the same identifier as the name it came from.
    let names = [TokenClass::Task, TokenClass::Type, TokenClass::Variant, TokenClass::Field, TokenClass::Parameter, TokenClass::Variable, TokenClass::Region];
    for token in tokens.iter().filter(|t| names.contains(&t.class)) {
        if let Some(target) = analysis.definition(token.from) {
            let name = text(source, Location { from: token.from, to: token.to, path: None });
            assert_eq!(text(source, target), name, "{}: definition of {:?}", what, name);
        }
    }
}

#[test]
fn every_request_works_on_every_prefix_of_the_examples() {
    // Every half-typed state of a real program, cut at each token boundary.
    for file in vela_files("examples") {
        let source = fs::read_to_string(&file).unwrap();
        let mut cuts: Vec<usize> = vela::lexer::highlight(&source).iter().flat_map(|t| [t.range.start, t.range.end]).collect();
        cuts.dedup();
        for offset in cuts {
            check_well_formed(&source[..offset], &format!("{} cut at byte {}", file.display(), offset), false);
        }
        check_well_formed(&source, &file.display().to_string(), true);
    }
    for dir in ["tests/run", "tests/fail"] {
        for file in vela_files(dir) {
            check_well_formed(&fs::read_to_string(&file).unwrap(), &file.display().to_string(), true);
        }
    }
}

#[test]
fn a_call_names_a_task_even_when_a_variable_has_its_name() {
    let source = "task double(n: int) -> int\n    <- n * 2\nfi\n\ntask main()\n    bind double = double(4)\n    show(double)\nfi\n";
    assert_eq!(class_at(source, "double(4)", 0), TokenClass::Task);
    assert_definition(source, ("double(4)", 0), ("double(n", 0));
    assert_definition(source, ("double)", 0), ("double =", 0));
    // A variable isn't visible in its own initializer.
    let source = "task main()\n    bind y = y + 1\nfi\n";
    assert_eq!(Analysis::new(source).definition(at(source, "y + 1", 0)), None);
}

#[test]
fn hover_works_at_the_end_of_a_name() {
    let source = "task main()\n    bind t = 1\n    show(t)\nfi\n";
    let end_of_t = at(source, "t)", 0) + 1;
    let hover = Analysis::new(source).hover(end_of_t).expect("a hover for t");
    assert!(hover.code.contains("t: int"), "{}", hover.code);
}

#[test]
fn an_unclosed_brace_ends_with_its_line() {
    let source = "shape P\n    x: int\nfi\n\ntask main()\n    bind p = P { x: 1\n    bind q: int = 2\nfi\n";
    assert_eq!(class_at(source, "int = 2", 0), TokenClass::TypeBuiltin);
}

#[test]
fn a_header_region_named_before_its_parameter_is_that_parameter() {
    let source = "task f(a: ^int@r, r: region) -> ^int@r\n    <- new int(a^) at r\nfi\n\ntask main()\nfi\n";
    assert_eq!(Analysis::new(source).diagnostics(), Vec::<Diagnostic>::new());
    for nth in 0..2 {
        assert_definition(source, ("r,", 0), ("r: region", 0));
        assert_definition(source, ("r\n", nth), ("r: region", 0));
    }
    let hover = Analysis::new(source).hover(at(source, "r,", 0)).unwrap();
    assert_eq!((hover.code.as_str(), hover.doc.contains("can allocate in")), ("r: region", true));
    check_well_formed(source, "header region", true);
}

#[test]
fn very_deep_types_are_analyzed_without_a_crash() {
    // The compiler stops at its nesting limit; the analysis must not recurse
    // through the text of a type written far deeper. A 1 MiB stack, smaller
    // than Vela Studio's, shows a missing limit as a crash.
    let run = || {
        let deep = "^".repeat(20_000);
        for source in [
            format!("task f(a: {}int)\n    show(1)\nfi\n", deep),
            format!("task main()\n    bind a: {}int = 1\n    show(a)\nfi\n", deep),
            format!("task main()\n    bind a: {}int = 1\n    show(a)\nfi\n", "maybe ".repeat(20_000)),
            format!("task f(a: {}int{})\n    show(1)\nfi\n", "[".repeat(20_000), "; 2]".repeat(20_000)),
            format!("task main()\n    bind a: {}int{}@r = 1\n    show(a)\nfi\n", "[".repeat(20_000), "]".repeat(20_000)),
        ] {
            let analysis = Analysis::new(&source);
            let length = source.encode_utf16().count();
            analysis.tokens();
            analysis.outline();
            assert!(!analysis.diagnostics().is_empty());
            for offset in [8, length.saturating_sub(12), length.saturating_sub(5), length] {
                analysis.hover(offset);
                analysis.complete(offset);
                analysis.definition(offset);
            }
        }
    };
    std::thread::Builder::new().stack_size(1 << 20).spawn(run).unwrap().join().unwrap();
}

#[test]
fn a_variable_on_a_line_with_an_unclosed_brace_is_visible_on_the_next() {
    let source = "shape P\n    x: int\nfi\n\ntask main()\n    bind p = P { x: 1\n    show(p)\nfi\n";
    assert_definition(source, ("p)", 0), ("p =", 0));
    // A plan B spanning lines still hides the name inside it.
    let plan_b = "task main()\n    bind m: maybe int = none\n    bind n = m { fail e\n        n\n    }\n    show(n)\nfi\n";
    assert_eq!(Analysis::new(plan_b).definition(at(plan_b, "n\n    }", 0)), None);
    assert_definition(plan_b, ("n)", 0), ("n =", 0));
}

#[test]
fn positions_convert_quickly_on_very_long_lines() {
    let source = format!("task main()\n    show(1{})\nfi\n", " + 1".repeat(20_000));
    let started = std::time::Instant::now();
    let analysis = Analysis::new(&source);
    analysis.tokens();
    analysis.diagnostics();
    analysis.outline();
    assert!(started.elapsed() < std::time::Duration::from_secs(2), "took {:?}", started.elapsed());
}
