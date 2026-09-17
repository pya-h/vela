//! Arrays and slices in the editor: hover and completion with the compiler's
//! types, and the same types read from the declarations while the file doesn't
//! check. In the sources below, `@@` marks the cursor.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, CompletionItem, LineIndex};

const PROGRAM: &str = "\
shape Point
    x: int
    y: int
fi

task sum(xs: [int]@r) -> int
    hold total = 0
    each x in xs
        total = total + x
    fi
    <- total
fi

task main()
    CHECK
    bind scores = [90, 72, 85]
    region r
    bind points = new [Point]([Point { x: 1, y: 2 }; 3]) at r
    bind boxed = new [int; 3](scores) at r
    bind part = points[1..3]
    bind first = part[0]
    bind count = scores.len
    each point in points
        show(point.x)
    fi
    show(sum(new [int](scores) at r) + count + first.y)
    @@
fi
";

/// `PROGRAM` with `code` at the cursor, and a first line in `main` that
/// either checks or stops the checker before anything else in `main`.
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

/// The code and doc of the hover at the first occurrence of `needle`.
fn hover(source: &str, needle: &str) -> (String, String) {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    let hover = Analysis::new(source).hover(LineIndex::new(source).utf16_of(byte)).unwrap_or_else(|| panic!("no hover at {:?}", needle));
    (hover.code, hover.doc)
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    let source = program(true, "show(0)");
    assert_eq!(Analysis::new(&source).diagnostics(), Vec::new());
}

#[test]
fn hover_shows_array_and_slice_types_whether_or_not_the_file_checks() {
    for checks in [true, false] {
        let source = program(checks, "show(0)");
        let code = |needle: &str| hover(&source, needle).0;
        let when = if checks { "checked" } else { "declared" };
        assert_eq!(code("scores ="), "bind scores: [int; 3]", "{}", when);
        assert_eq!(code("points ="), "bind points: [Point]@r", "{}", when);
        assert_eq!(code("boxed ="), "bind boxed: ^[int; 3]@r", "{}", when);
        assert_eq!(code("part ="), "bind part: [Point]@r", "{}", when);
        assert_eq!(code("first ="), "bind first: Point", "{}", when);
        assert_eq!(code("count ="), "bind count: int", "{}", when);
        assert_eq!(code("xs:"), "xs: [int]@r", "{}", when);
        assert_eq!(hover(&source, "x in"), ("each x: int".to_string(), "An item of the array or slice an each loop goes through.".to_string()), "{}", when);
        assert_eq!(code("point in"), "each point: Point", "{}", when);
    }
    let source = program(true, "show(0)");
    assert_eq!(hover(&source, "len"), ("len: int".to_string(), "The number of items of an array or slice.".to_string()));
}

#[test]
fn a_counter_and_an_item_are_described_apart() {
    let source = "task main()\n    each i in 0..3\n        show(i)\n    fi\n    each n in [1, 2]\n        show(n)\n    fi\nfi\n";
    assert_eq!(hover(source, "i in").1, "The counter of an each loop.");
    assert_eq!(hover(source, "n in").1, "An item of the array or slice an each loop goes through.");
}

#[test]
fn completion_offers_len_on_arrays_and_slices() {
    for checks in [true, false] {
        assert_eq!(labels(&program(checks, "show(scores.@@)")), ["len"], "checks: {}", checks);
        assert_eq!(labels(&program(checks, "show(points.@@)")), ["len"], "checks: {}", checks);
        let fields = labels(&program(checks, "show(points[0].@@)"));
        assert!(fields.contains(&"x".to_string()) && fields.contains(&"y".to_string()), "{:?}", fields);
    }
    // An array behind a pointer is reached through `^`.
    let through = completions(&program(true, "show(boxed.@@)"));
    assert_eq!(through.iter().map(|item| (item.label.as_str(), item.apply.as_deref())).collect::<Vec<_>>(), [("len", Some("^.len"))]);
}

#[test]
fn completion_offers_types_in_brackets_and_nothing_for_a_count() {
    let types = labels(&program(true, "bind grid: [@@"));
    for expected in ["int", "Point", "maybe"] {
        assert!(types.contains(&expected.to_string()), "{:?} in {:?}", expected, types);
    }
    // An array can't hold unit.
    assert!(!types.contains(&"unit".to_string()), "{:?}", types);
    assert!(labels(&program(true, "bind grid: [[@@")).contains(&"float".to_string()));
    assert_eq!(labels(&program(true, "bind grid: [int; @@")), Vec::<String>::new());
    // The count of a repeated value is a value.
    assert!(labels(&program(true, "bind zeros = [0; @@")).contains(&"count".to_string()));
    // `or` goes on after an array type.
    assert!(labels(&program(true, "hold pair: [int; 2] @@")).contains(&"or".to_string()));
}

#[test]
fn values_in_an_array_inside_a_shape_literal_are_not_its_fields() {
    let items = labels(&program(true, "bind p = Point { x: [1, @@"));
    assert!(!items.contains(&"y".to_string()), "{:?}", items);
    assert!(items.contains(&"scores".to_string()), "{:?}", items);
}
