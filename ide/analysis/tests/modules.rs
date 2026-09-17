//! Analysis of a file that includes other files: the editor gives their text by
//! path, and highlighting, diagnostics, hover, definitions, and completion
//! follow the names into them. In the sources below, `@@` marks the cursor.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::collections::HashMap;

use vela_analysis::{Analysis, LineIndex, Location, TokenClass};

const MAIN: &str = "\
include shapes/geometry
include palette (Color, describe)

task main()
    bind square = geometry.Square(3.0)
    show(geometry.area(square))
    bind corner: geometry.Point = geometry.Point { x: 3.0, y: 4.0 }
    show(corner.x)
    match square
    case Square side
        show(side)
    case Circle radius
        show(radius)
    fi
    bind red: Color = palette.Red
    show(describe(red))
    bind size = geometry.measure(square)
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

/// `MAIN` with its `@@` line replaced by `code` (which may have its own `@@`).
fn main_with(code: &str) -> String {
    MAIN.replace("    @@\n", &format!("    {}\n", code))
}

/// The UTF-16 offset of the `nth` occurrence of `needle` in `source`.
fn at(source: &str, needle: &str, nth: usize) -> usize {
    let byte = source.match_indices(needle).nth(nth).unwrap_or_else(|| panic!("no {:?}", needle)).0;
    LineIndex::new(source).utf16_of(byte)
}

fn class_at(analysis: &Analysis, source: &str, needle: &str, nth: usize) -> TokenClass {
    let offset = at(source, needle, nth);
    analysis.tokens().into_iter().find(|t| t.from == offset).unwrap_or_else(|| panic!("no token at {:?}", needle)).class
}

/// The labels completed at `@@` in `marked`, with every module given.
fn labels(marked: &str) -> Vec<String> {
    let offset = marked.find("@@").unwrap();
    let source = marked.replacen("@@", "", 1);
    let sources = sources();
    let analysis = Analysis::with_modules(&source, "app/main.vela", &sources);
    analysis.complete(LineIndex::new(&source).utf16_of(offset)).items.into_iter().map(|item| item.label).collect()
}

#[test]
fn the_modules_a_file_needs_are_listed_until_given() {
    let source = main_with("");
    let none = HashMap::new();
    let alone = Analysis::with_modules(&source, "app/main.vela", &none);
    assert_eq!(alone.missing_modules(), ["app/shapes/geometry.vela", "app/palette.vela"]);
    assert_eq!(alone.diagnostics()[0].message, "can't find module 'shapes/geometry': there is no file 'shapes/geometry.vela'");

    // A module's own `include` is found once the module is given, relative to it.
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
    // numbers is included by geometry, which this file includes on its first line.
    assert_eq!((diagnostics[0].line, diagnostics[0].col), (Some(1), Some(9)));
    assert_eq!(diagnostics[0].message, "app/numbers.vela:2:10: arithmetic requires numeric operands");
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
    // A field of another module's shape.
    assert_eq!(class_at(&analysis, &source, "x)", 0), TokenClass::Field);

    // Without the modules, the names still read as modules.
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
    // The checker's types name other modules' shapes as the file does.
    assert_eq!(analysis.hover(at(&source, "corner:", 0)).unwrap().code, "bind corner: geometry.Point");
    assert_eq!(analysis.hover(at(&source, "square =", 0)).unwrap().code, "bind square: geometry.Figure");

    let definition = analysis.definition(at(&source, "area(square)", 0)).unwrap();
    let from = LineIndex::new(GEOMETRY).utf16_of(GEOMETRY.find("task area(").unwrap() + "task ".len());
    assert_eq!(definition, Location { from, to: from + 4, path: Some("app/shapes/geometry.vela".to_string()) });
    let file = analysis.definition(at(&source, "geometry.Square", 0)).unwrap();
    assert_eq!(file, Location { from: 0, to: 0, path: Some("app/shapes/geometry.vela".to_string()) });
    // A name brought in leads to its module too.
    let describe = analysis.definition(at(&source, "describe(red)", 0)).unwrap();
    assert_eq!(describe.path.as_deref(), Some("app/palette.vela"));
    // The file's own names stay in the file.
    assert_eq!(analysis.definition(at(&source, "square)", 0)).unwrap().path, None);
    // The outline is the file's own.
    assert_eq!(analysis.outline().iter().map(|item| item.name.as_str()).collect::<Vec<_>>(), ["main"]);
}

#[test]
fn completion_offers_what_modules_declare() {
    // After a module's name: what it declares, except what is private.
    let members = labels(&main_with("show(geometry.@@)"));
    for name in ["area", "Point", "Figure", "Circle", "Square", "Measure", "measure"] {
        assert!(members.contains(&name.to_string()), "{} in {:?}", name, members);
    }
    assert!(!members.iter().any(|name| name == "pi" || name == "square" || name == "x"), "{:?}", members);
    // Where a type is written, only its shapes and kinds.
    assert_eq!(labels(&main_with("bind p: geometry.@@ = corner")), ["Point", "Measure", "Figure"]);
    // In an `include` line's parentheses, the names a module declares.
    let listed = labels(&MAIN.replace("include palette (Color, describe)", "include palette (Color, @@)"));
    assert_eq!(listed, ["Color", "Red", "Blue", "describe"]);

    // A value names the modules and what `include` brings in, not another module's
    // names on their own.
    let values = labels(&main_with("show(@@)"));
    for name in ["geometry", "palette", "describe", "square", "corner"] {
        assert!(values.contains(&name.to_string()), "{} in {:?}", name, values);
    }
    assert!(!values.iter().any(|name| name == "area" || name == "Point"), "{:?}", values);
    let types = labels(&main_with("bind c: @@ = red"));
    assert!(types.contains(&"Color".to_string()) && types.contains(&"geometry".to_string()) && !types.contains(&"Point".to_string()), "{:?}", types);

    // Fields of another module's shape, except private ones.
    assert_eq!(labels(&main_with("show(corner.@@)")), ["x", "y"]);
    assert_eq!(labels(&main_with("show(size.@@)")), ["size"]);
    // A match on another module's kind names its variants alone.
    let cases = labels(&MAIN.replace("    case Circle radius\n        show(radius)\n", "    case @@\n"));
    assert!(cases.contains(&"Circle".to_string()) && !cases.iter().any(|c| c.contains('.')), "{:?}", cases);

    // `include` is offered only before the file's items; `priv` before an item.
    assert!(labels("@@").contains(&"include".to_string()));
    let after_items = labels("task a()\nfi\n@@");
    assert!(after_items.contains(&"priv".to_string()) && !after_items.contains(&"include".to_string()), "{:?}", after_items);
}

#[test]
fn every_request_works_on_every_prefix_of_a_program_and_its_modules() {
    // Half-typed text in the file or in a module it includes, cut at every character.
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
