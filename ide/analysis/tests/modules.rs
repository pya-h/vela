#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::collections::HashMap;

use vela_analysis::{Analysis, CompletionItem, LineIndex, Location, Severity, TokenClass};

const MAIN: &str = "\
include shapes/geometry
include palette (Color, describe)

task main()
    fixed square = geometry.Square(3.0)
    show(geometry.area(square))
    fixed corner: geometry.Point = geometry.Point { x: 3.0, y: 4.0 }
    show(corner.x)
    match square
    case Square side
        show(side)
    case Circle radius
        show(radius)
    fi
    fixed red: Color = palette.Red
    show(describe(red))
    fixed size = geometry.measure(square)
    @@
fi
";

const GEOMETRY: &str = "\
include ../numbers

shape Point
    x: float
    y: float
fi

shape Measure
    size: float
    priv cached: float
fi

task measure(figure: Figure) -> Measure
    <- Measure { size: area(figure), cached: 0.0 }
fi

kind Figure
    Circle(radius: float)
    Square(side: float)
fi

task area(figure: Figure) -> float
    match figure
    case Circle radius
        <- pi() * radius * radius
    case Square side
        <- numbers.square(side)
    fi
fi

priv task pi() -> float
    <- 3.14159
fi

priv kind Secret
    Hidden
fi
";

const NUMBERS: &str = "task square(x: float) -> float\n    <- x * x\nfi\n";

const PALETTE: &str = "\
kind Color
    Red
    Blue
fi

task describe(color: Color) -> text
    match color
    case Red
        <- \"red\"
    case Blue
        <- \"blue\"
    fi
fi
";

fn sources() -> HashMap<String, String> {
    [("app/shapes/geometry.vela", GEOMETRY), ("app/numbers.vela", NUMBERS), ("app/palette.vela", PALETTE)]
        .into_iter()
        .map(|(path, source)| (path.to_string(), source.to_string()))
        .collect()
}

fn main_with(code: &str) -> String {
    MAIN.replace("    @@\n", &format!("    {}\n", code))
}

fn at(source: &str, needle: &str, nth: usize) -> usize {
    let byte = source.match_indices(needle).nth(nth).unwrap_or_else(|| panic!("no {:?}", needle)).0;
    LineIndex::new(source).utf16_of(byte)
}

fn class_at(analysis: &Analysis, source: &str, needle: &str, nth: usize) -> TokenClass {
    let offset = at(source, needle, nth);
    analysis.tokens().into_iter().find(|t| t.from == offset).unwrap_or_else(|| panic!("no token at {:?}", needle)).class
}

fn items(marked: &str) -> Vec<CompletionItem> {
    let offset = marked.find("@@").unwrap();
    let source = marked.replacen("@@", "", 1);
    let sources = sources();
    let analysis = Analysis::with_modules(&source, "app/main.vela", &sources);
    analysis.complete(LineIndex::new(&source).utf16_of(offset)).items
}

fn labels(marked: &str) -> Vec<String> {
    items(marked).into_iter().map(|item| item.label).collect()
}

#[test]
fn the_modules_a_file_needs_are_listed_until_given() {
    let source = main_with("");
    let none = HashMap::new();
    let alone = Analysis::with_modules(&source, "app/main.vela", &none);
    assert_eq!(alone.missing_modules(), ["app/shapes/geometry.vela", "app/palette.vela"]);
    assert_eq!(alone.diagnostics()[0].message, "can't find module 'shapes/geometry': there is no file 'shapes/geometry.vela'");

    let mut some = HashMap::new();
    some.insert("app/shapes/geometry.vela".to_string(), GEOMETRY.to_string());
    some.insert("app/palette.vela".to_string(), PALETTE.to_string());
    assert_eq!(Analysis::with_modules(&source, "app/main.vela", &some).missing_modules(), ["app/numbers.vela"]);

    let sources = sources();
    let complete = Analysis::with_modules(&source, "app/main.vela", &sources);
    assert_eq!(complete.missing_modules(), Vec::<String>::new());
    assert_eq!(complete.diagnostics(), Vec::new());
}

#[test]
fn an_error_in_a_module_is_shown_at_its_include() {
    let source = main_with("");
    let mut sources = sources();
    sources.insert("app/numbers.vela".to_string(), "task square(x: float) -> float\n    <- x * \"x\"\nfi\n".to_string());
    let analysis = Analysis::with_modules(&source, "app/main.vela", &sources);
    let diagnostics = analysis.diagnostics();
    assert_eq!((diagnostics[0].line, diagnostics[0].col), (Some(1), Some(9)));
    assert_eq!(diagnostics[0].message, "app/numbers.vela:2:10: arithmetic requires numeric operands");
}

#[test]
fn a_module_keeps_its_own_warnings() {
    let source = format!("{}\ntask same(a: Pair, b: Pair) -> bool\n    <- a == b\nfi\n\nshape Pair\n    x: int\nfi\n", main_with(""));
    let mut sources = sources();
    let numbers = sources.get_mut("app/numbers.vela").unwrap();
    numbers.push_str("\nshape Two\n    x: int\nfi\n\ntask alike(a: Two, b: Two) -> bool\n    <- a == b\nfi\n");
    let analysis = Analysis::with_modules(&source, "app/main.vela", &sources);
    let diagnostics = analysis.diagnostics();
    let line = source.lines().position(|line| line == "    <- a == b").unwrap() + 1;
    assert_eq!(
        diagnostics.iter().map(|d| (d.line, d.col, d.severity)).collect::<Vec<_>>(),
        [(Some(line), Some(8), Severity::Warning), (Some(line), Some(13), Severity::Warning)]
    );
    assert!(diagnostics[0].message.starts_with("'==' compares Pair by where it is, and 'a' is a parameter"), "{}", diagnostics[0].message);
}

#[test]
fn names_of_modules_are_highlighted_as_what_they_are() {
    let source = main_with("");
    let sources = sources();
    let analysis = Analysis::with_modules(&source, "app/main.vela", &sources);
    assert_eq!(class_at(&analysis, &source, "shapes", 0), TokenClass::Module);
    assert_eq!(class_at(&analysis, &source, "geometry\n", 0), TokenClass::Module);
    assert_eq!(class_at(&analysis, &source, "Color, describe", 0), TokenClass::Type);
    assert_eq!(class_at(&analysis, &source, "describe)", 0), TokenClass::Task);
    assert_eq!(class_at(&analysis, &source, "geometry.Square", 0), TokenClass::Module);
    assert_eq!(class_at(&analysis, &source, "Square(3.0)", 0), TokenClass::Variant);
    assert_eq!(class_at(&analysis, &source, "area(square)", 0), TokenClass::Task);
    assert_eq!(class_at(&analysis, &source, "Point =", 0), TokenClass::Type);
    assert_eq!(class_at(&analysis, &source, "Point {", 0), TokenClass::Type);
    assert_eq!(class_at(&analysis, &source, "Color =", 0), TokenClass::Type);
    assert_eq!(class_at(&analysis, &source, "Red", 0), TokenClass::Variant);
    assert_eq!(class_at(&analysis, &source, "describe(red)", 0), TokenClass::Task);
    assert_eq!(class_at(&analysis, &source, "x)", 0), TokenClass::Field);

    let alone = Analysis::new(&source);
    assert_eq!(class_at(&alone, &source, "geometry.Square", 0), TokenClass::Module);
}

#[test]
fn hover_and_definitions_follow_names_into_modules() {
    let source = main_with("");
    let sources = sources();
    let analysis = Analysis::with_modules(&source, "app/main.vela", &sources);

    let area = analysis.hover(at(&source, "area(square)", 0)).unwrap();
    assert_eq!(area.code, "task geometry.area(figure: geometry.Figure) -> float");
    assert_eq!(area.doc, "From module geometry (app/shapes/geometry.vela).");
    let module = analysis.hover(at(&source, "geometry.Square", 0)).unwrap();
    assert_eq!((module.code.as_str(), module.doc.as_str()), ("include shapes/geometry", "A module this file includes: the file app/shapes/geometry.vela."));
    let point = analysis.hover(at(&source, "Point =", 0)).unwrap();
    assert_eq!(point.code, "shape geometry.Point\n    x: float\n    y: float\nfi");
    let measure = analysis.hover(at(&source, "measure(square)", 0)).unwrap();
    assert_eq!(measure.code, "task geometry.measure(figure: geometry.Figure) -> geometry.Measure");
    let color = analysis.hover(at(&source, "Color =", 0)).unwrap();
    assert_eq!(color.code, "kind palette.Color\n    Red\n    Blue\nfi");
    assert_eq!(analysis.hover(at(&source, "corner:", 0)).unwrap().code, "fixed corner: geometry.Point");
    assert_eq!(analysis.hover(at(&source, "square =", 0)).unwrap().code, "fixed square: geometry.Figure");

    let definition = analysis.definition(at(&source, "area(square)", 0)).unwrap();
    let from = LineIndex::new(GEOMETRY).utf16_of(GEOMETRY.find("task area(").unwrap() + "task ".len());
    assert_eq!(definition, Location { from, to: from + 4, path: Some("app/shapes/geometry.vela".to_string()) });
    let file = analysis.definition(at(&source, "geometry.Square", 0)).unwrap();
    assert_eq!(file, Location { from: 0, to: 0, path: Some("app/shapes/geometry.vela".to_string()) });
    let describe = analysis.definition(at(&source, "describe(red)", 0)).unwrap();
    assert_eq!(describe.path.as_deref(), Some("app/palette.vela"));
    assert_eq!(analysis.definition(at(&source, "square)", 0)).unwrap().path, None);
    assert_eq!(analysis.outline().iter().map(|item| item.name.as_str()).collect::<Vec<_>>(), ["main"]);
}

#[test]
fn completion_offers_what_modules_declare() {
    let members = labels(&main_with("show(geometry.@@)"));
    for name in ["area", "Point", "Figure", "Circle", "Square", "Measure", "measure"] {
        assert!(members.contains(&name.to_string()), "{} in {:?}", name, members);
    }
    assert!(!members.iter().any(|name| name == "pi" || name == "square" || name == "x"), "{:?}", members);
    assert_eq!(labels(&main_with("fixed p: geometry.@@ = corner")), ["Point", "Measure", "Figure"]);
    let listed = labels(&MAIN.replace("include palette (Color, describe)", "include palette (Color, @@)"));
    assert_eq!(listed, ["Color", "Red", "Blue", "describe"]);

    let values = labels(&main_with("show(@@)"));
    for name in ["geometry", "palette", "describe", "square", "corner"] {
        assert!(values.contains(&name.to_string()), "{} in {:?}", name, values);
    }
    assert!(!values.iter().any(|name| name == "area" || name == "Point"), "{:?}", values);
    let types = labels(&main_with("fixed c: @@ = red"));
    assert!(types.contains(&"Color".to_string()) && types.contains(&"geometry".to_string()) && !types.contains(&"Point".to_string()), "{:?}", types);

    assert_eq!(labels(&main_with("show(corner.@@)")), ["x", "y", "str", "type"]);
    assert_eq!(labels(&main_with("show(size.@@)")), ["size", "str", "type"]);
    let cases = labels(&MAIN.replace("    case Circle radius\n        show(radius)\n", "    case @@\n"));
    assert!(cases.contains(&"Circle".to_string()) && !cases.iter().any(|c| c.contains('.')), "{:?}", cases);

    assert!(labels("@@").contains(&"include".to_string()));
    let after_items = labels("task a()\nfi\n@@");
    assert!(after_items.contains(&"priv".to_string()) && !after_items.contains(&"include".to_string()), "{:?}", after_items);
}

#[test]
fn every_request_works_on_every_prefix_of_a_program_and_its_modules() {
    let full = main_with("");
    let every = |source: &str, sources: &HashMap<String, String>| {
        let analysis = Analysis::with_modules(source, "app/main.vela", sources);
        let _ = (analysis.tokens(), analysis.diagnostics(), analysis.outline(), analysis.missing_modules());
        let length = LineIndex::new(source).utf16_of(source.len());
        for offset in (0..=length).step_by(7) {
            let _ = (analysis.complete(offset), analysis.hover(offset), analysis.definition(offset));
        }
    };
    let sources = sources();
    for cut in full.char_indices().map(|(i, _)| i) {
        every(&full[..cut], &sources);
    }
    for cut in GEOMETRY.char_indices().map(|(i, _)| i).step_by(3) {
        let mut cut_sources = sources.clone();
        cut_sources.insert("app/shapes/geometry.vela".to_string(), GEOMETRY[..cut].to_string());
        every(&full, &cut_sources);
    }
}

#[test]
fn names_on_an_include_line_are_inserted_alone() {
    let listed = items("include shapes/geometry (@@)\n\ntask main()\nfi\n");
    let names: Vec<&str> = listed.iter().map(|item| item.label.as_str()).collect();
    for name in ["area", "measure", "Point", "Figure", "Circle"] {
        assert!(names.contains(&name), "{} in {:?}", name, names);
    }
    assert!(!names.contains(&"pi") && !names.contains(&"Secret") && !names.contains(&"Hidden"), "{:?}", names);
    for item in listed.iter().chain(&items("include shapes/geometry {Point, @@}\n\ntask main()\nfi\n")) {
        assert_eq!((&item.snippet, &item.apply), (&None, &None), "{:?}", item);
    }
    let call = items(&main_with("show(geometry.@@)")).into_iter().find(|item| item.label == "area").unwrap();
    assert_eq!(call.snippet.as_deref(), Some("area(${figure})"));
}

#[test]
fn a_private_kind_and_its_variants_are_hidden_from_other_files() {
    let members = labels(&main_with("show(geometry.@@)"));
    assert!(members.contains(&"Figure".to_string()) && members.contains(&"Circle".to_string()), "{:?}", members);
    assert!(!members.iter().any(|name| name == "Secret" || name == "Hidden"), "{:?}", members);
    assert!(!labels(&main_with("fixed s: geometry.@@ = square")).contains(&"Secret".to_string()));
}

#[test]
fn a_shape_with_a_private_field_is_not_offered_as_a_literal_in_another_file() {
    let members = items(&main_with("show(geometry.@@)"));
    let measure = members.iter().find(|item| item.label == "Measure").unwrap();
    assert_eq!(measure.snippet, None);
    let point = members.iter().find(|item| item.label == "Point").unwrap();
    assert_eq!(point.snippet.as_deref(), Some("Point { x: ${x}, y: ${y} }"));
    let brought = MAIN.replace("include palette (Color, describe)", "include palette (Color, describe)\ninclude shapes/geometry (Measure => Size)");
    let brought = brought.replace("include shapes/geometry\n", "");
    let values = items(&brought.replace("    @@\n", "    show(@@)\n"));
    assert_eq!(values.iter().find(|item| item.label == "Size").map(|item| item.snippet.clone()), Some(None));
    assert_eq!(labels(&main_with("fixed m = geometry.Measure { @@ }")), ["size"]);
}

fn with_parse(code: &str) -> String {
    main_with("").replace("task main()", &format!("{}\n    <- fail s\nfi\n\ntask main()", code))
}

#[test]
fn another_modules_type_goes_on_with_its_error_type() {
    let source = with_parse("task parse(s: text) -> geometry.Point ~ text");
    let sources = sources();
    let analysis = Analysis::with_modules(&source, "app/main.vela", &sources);
    assert_eq!(analysis.diagnostics(), Vec::new());
    assert_eq!(class_at(&analysis, &source, "Point ~", 0), TokenClass::Type);
    assert_eq!(class_at(&analysis, &source, "text\n", 0), TokenClass::TypeBuiltin);
    assert_eq!(analysis.hover(at(&source, "text\n", 0)).unwrap().code, "text");

    let error_types = labels(&with_parse("task parse(s: text) -> geometry.Point ~ @@"));
    assert!(error_types.contains(&"int".to_string()) && !error_types.contains(&"main".to_string()), "{:?}", error_types);
    assert_eq!(labels(&with_parse("task parse(s: text) -> geometry.Point @@")), ["~"]);
    assert_eq!(labels(&with_parse("task parse(s: text) -> ^geometry.Point@r @@")), ["~"]);
    assert_eq!(labels(&main_with("show(geometry.area(square) @@)")), ["and", "or"]);
}

fn braces_program(checks: bool, code: &str) -> String {
    let first = if checks { "show(0)" } else { "show(unknown)" };
    format!("include shapes/geometry {{measure, Circle}}\n\ntask main()\n    {}\n    fixed size = measure(Circle(1.0))\n    {}\nfi\n", first, code)
}

#[test]
fn types_a_file_cant_write_are_spelled_with_their_module_path() {
    let sources = sources();
    for checks in [true, false] {
        let source = braces_program(checks, "show(size.size)");
        let analysis = Analysis::with_modules(&source, "app/main.vela", &sources);
        assert_eq!(analysis.diagnostics().is_empty(), checks, "{:?}", analysis.diagnostics());
        assert_eq!(analysis.hover(at(&source, "size =", 0)).unwrap().code, "fixed size: shapes/geometry.Measure", "checks: {}", checks);
        assert_eq!(labels(&braces_program(checks, "show(size.@@)")), ["size", "str", "type"], "checks: {}", checks);
        // A variant listed alone belongs to a kind the file can't name, spelled
        // as the compiler spells it.
        let circle = analysis.hover(at(&source, "Circle(1.0)", 0)).unwrap();
        assert_eq!(circle.code, "Circle(radius: float)");
        assert_eq!(circle.doc, "A variant of kind shapes/geometry.Figure. From module shapes/geometry (app/shapes/geometry.vela).");
    }
    let wrong = braces_program(true, "fixed n: int = Circle(2.0)");
    let messages: Vec<String> = Analysis::with_modules(&wrong, "app/main.vela", &sources).diagnostics().into_iter().map(|d| d.message).collect();
    assert_eq!(messages, ["expected type int, got shapes/geometry.Figure"]);
}

const CLASH: &str = "\
include shapes/geometry

kind Mark
    Circle(label: text)
fi

task main()
    CHECK
    fixed figure = geometry.Square(2.0)
    match figure
    case Circle radius
        show(radius)
    case Square side
        show(side)
    fi
    when figure is Square edge
        show(edge)
    fi
    match Circle(\"m\")
    case Circle label
        show(label)
    fi
fi
";

#[test]
fn a_variant_named_alone_in_a_pattern_is_the_matched_kinds() {
    let sources = sources();
    for checks in [true, false] {
        let source = CLASH.replace("CHECK", if checks { "show(0)" } else { "show(unknown)" });
        let analysis = Analysis::with_modules(&source, "app/main.vela", &sources);
        let path = |needle: &str| analysis.definition(at(&source, needle, 0)).map(|location| location.path);
        let geometry = Some(Some("app/shapes/geometry.vela".to_string()));
        assert_eq!(path("Circle radius"), geometry, "checks: {}", checks);
        assert_eq!(path("Square side"), geometry, "checks: {}", checks);
        assert_eq!(path("Square edge"), geometry, "checks: {}", checks);
        assert_eq!(analysis.hover(at(&source, "Circle radius", 0)).unwrap().code, "geometry.Circle(radius: float)");
        assert_eq!(analysis.hover(at(&source, "radius)", 0)).unwrap().code, "radius: float");
        assert_eq!(path("Circle label"), Some(None), "checks: {}", checks);
        assert_eq!(analysis.hover(at(&source, "Circle label", 0)).unwrap().code, "Circle(label: text)");
    }
}

#[test]
fn patterns_through_a_kind_that_isnt_known_still_name_a_variant() {
    let source = main_with("match square\n    case geometry.Figure.Circle r\n        show(r)\n    fi\n    match square\n    case Unknown.Thing t\n        show(t)\n    fi");
    let alone = Analysis::new(&source);
    assert_eq!(class_at(&alone, &source, "Figure.Circle", 0), TokenClass::Type);
    assert_eq!(class_at(&alone, &source, "Circle r", 0), TokenClass::Variant);
    assert_eq!(class_at(&alone, &source, "r)", 0), TokenClass::Variable);
    assert_eq!(class_at(&alone, &source, "Unknown", 0), TokenClass::Type);
    assert_eq!(class_at(&alone, &source, "Thing", 0), TokenClass::Variant);
    assert_eq!(alone.hover(at(&source, "Thing", 0)), None);
    let offset = at(&source, "show(r)", 0) + "show(".len();
    let names: Vec<String> = alone.complete(offset).items.into_iter().map(|item| item.label).collect();
    assert!(names.contains(&"r".to_string()) && !names.contains(&"Circle".to_string()), "{:?}", names);
}
