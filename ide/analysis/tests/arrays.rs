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
    flex total = 0
    each x in xs
        total = total + x
    fi
    <- total
fi

task main()
    CHECK
    fixed scores = [90, 72, 85]
    region r
    fixed points = new [Point]([Point { x: 1, y: 2 }#3]) at r
    fixed boxed = new [int#3](scores) at r
    fixed part = points[1..3]
    fixed first = part[0]
    fixed count = scores.len
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
        assert_eq!(code("scores ="), "fixed scores: [int#3]", "{}", when);
        assert_eq!(code("points ="), "fixed points: [Point]@r", "{}", when);
        assert_eq!(code("boxed ="), "fixed boxed: ^[int#3]@r", "{}", when);
        assert_eq!(code("part ="), "fixed part: [Point]@r", "{}", when);
        assert_eq!(code("first ="), "fixed first: Point", "{}", when);
        assert_eq!(code("count ="), "fixed count: int", "{}", when);
        assert_eq!(code("xs:"), "xs: [int]@r", "{}", when);
        assert_eq!(hover(&source, "x in"), ("each x: int".to_string(), "An item of the array or slice an each loop goes through.".to_string()), "{}", when);
        assert_eq!(code("point in"), "each point: Point", "{}", when);
    }
    let source = program(true, "show(0)");
    assert_eq!(hover(&source, "len"), ("len: int".to_string(), "The number of items of an array or slice, or the number of bytes of text.".to_string()));
}

#[test]
fn a_counter_and_an_item_are_described_apart() {
    let source = "task main()\n    each i in 0..3\n        show(i)\n    fi\n    each n in [1, 2]\n        show(n)\n    fi\nfi\n";
    assert_eq!(hover(source, "i in").1, "The counter of an each loop.");
    assert_eq!(hover(source, "n in").1, "An item of the array or slice an each loop goes through.");
}

#[test]
fn completion_offers_len_on_arrays_and_len_add_del_and_copy_on_slices() {
    for checks in [true, false] {
        assert_eq!(labels(&program(checks, "show(scores.@@)")), ["len", "str", "type"], "checks: {}", checks);
        // A slice of values in a region can be copied too.
        assert_eq!(labels(&program(checks, "show(points.@@)")), ["len", "add", "del", "str", "copy", "type"], "checks: {}", checks);
        let fields = labels(&program(checks, "show(points[0].@@)"));
        assert!(fields.contains(&"x".to_string()) && fields.contains(&"y".to_string()), "{:?}", fields);
    }
    // An array behind a pointer is reached through `^`.
    let through = completions(&program(true, "show(boxed.@@)"));
    // An array behind a pointer is reached through `^`, and the pointer is copied as it is.
    let pairs: Vec<(&str, Option<&str>, Option<&str>)> = through.iter().map(|item| (item.label.as_str(), item.apply.as_deref(), item.snippet.as_deref())).collect();
    assert_eq!(pairs, [("len", Some("^.len"), None), ("str", None, Some(".str() at ${r}")), ("copy", None, Some(".copy()")), ("type", None, Some(".type"))]);
}

#[test]
fn completion_offers_types_in_brackets_and_nothing_for_a_count() {
    let types = labels(&program(true, "fixed grid: [@@"));
    for expected in ["int", "Point", "maybe"] {
        assert!(types.contains(&expected.to_string()), "{:?} in {:?}", expected, types);
    }
    // An array can't hold nothing.
    assert!(!types.contains(&"nothing".to_string()), "{:?}", types);
    assert!(labels(&program(true, "fixed grid: [[@@")).contains(&"float".to_string()));
    assert_eq!(labels(&program(true, "fixed grid: [int#@@")), Vec::<String>::new());
    // The count of a repeated value is a value.
    assert!(labels(&program(true, "fixed zeros = [0#@@")).contains(&"count".to_string()));
    // An error type goes on after an array type, with `~`.
    assert!(labels(&program(true, "flex pair: [int#2] @@")).contains(&"~".to_string()));
}

#[test]
fn values_in_an_array_inside_a_shape_literal_are_not_its_fields() {
    let items = labels(&program(true, "fixed p = Point { x: [1, @@"));
    assert!(!items.contains(&"y".to_string()), "{:?}", items);
    assert!(items.contains(&"scores".to_string()), "{:?}", items);
}

#[test]
fn completion_continues_array_and_slice_expressions() {
    for checks in [true, false] {
        let when = if checks { "checked" } else { "declared" };
        // After a slice made with `new`, the region it goes in.
        assert!(labels(&program(checks, "fixed copy = new [int](scores) @@")).contains(&"at".to_string()), "{}", when);
        assert!(labels(&program(checks, "fixed copy = new [int#3](scores) @@")).contains(&"at".to_string()), "{}", when);
        assert_eq!(labels(&program(checks, "fixed copy = new [int](scores) at @@")), ["r"], "{}", when);
        // Once it has one, no second `at`.
        assert!(!labels(&program(checks, "fixed copy = new [int](scores) at r @@")).contains(&"at".to_string()), "{}", when);
        // Nor after an array literal, which `new` doesn't make.
        assert!(!labels(&program(checks, "fixed copy = [1, 2] @@")).contains(&"at".to_string()), "{}", when);
        // What `each` goes through is a value: arrays, slices, and counts.
        let values = labels(&program(checks, "each v in @@"));
        for name in ["scores", "points", "part", "count", "sum"] {
            assert!(values.contains(&name.to_string()), "{} in {:?} ({})", name, values, when);
        }
        assert!(!values.iter().any(|name| name == "fixed" || name == "each" || name == "fi"), "{:?} ({})", values, when);
        // After its name, only `in`.
        assert_eq!(labels(&program(checks, "each v @@")), ["in"], "{}", when);
    }
}

#[test]
fn changes_to_a_slice_have_types_hovers_and_snippets() {
    for checks in [true, false] {
        let when = if checks { "checked" } else { "declared" };
        let code = "flex names = new [text]() at r\n    names.add(\"a\")\n    names.add(0, \"b\")\n    fixed taken = names.del(1)\n    flex moving = points.copy() at r\n    fixed last = moving.del()\n    show(taken)\n    show(last.x)";
        let source = program(checks, code);
        if checks {
            assert_eq!(Analysis::new(&source).diagnostics(), Vec::new());
        }
        assert_eq!(hover(&source, "names =").0, "flex names: [text]@r", "{}", when);
        assert_eq!(hover(&source, "taken =").0, "fixed taken: text@r", "{}", when);
        assert_eq!(hover(&source, "last =").0, "fixed last: Point", "{}", when);
    }
    let source = program(true, "flex names = new [text]() at r\n    names.add(\"a\")\n    show(names.del())");
    let (code, doc) = hover(&source, "add(");
    assert_eq!(code, "add(value) or add(index, value)");
    assert!(doc.starts_with("Adds a value to a slice"), "{}", doc);
    let (code, doc) = hover(&source, "del(");
    assert_eq!(code, "del(index) or del()");
    assert!(doc.starts_with("Takes the item at the index out"), "{}", doc);
    let items = completions(&program(true, "points.@@"));
    let snippets: Vec<(&str, Option<&str>)> = items.iter().map(|item| (item.label.as_str(), item.snippet.as_deref())).collect();
    assert_eq!(snippets, [("len", None), ("add", Some("add(${value})")), ("del", Some("del(${index})")), ("str", Some("str() at ${r}")), ("copy", Some("copy()")), ("type", None)]);
    // An array can't change its length.
    assert_eq!(labels(&program(true, "scores.@@")), ["len", "str", "type"]);
}

#[test]
fn a_variant_named_like_a_change_is_still_a_variant() {
    let source = "kind Step\n    add(amount: int)\n    del\nfi\n\ntask main()\n    fixed a = Step.add(2)\n    fixed b = Step.del\n    match a\n        case add n\n            show(n)\n        case del\n            show(0)\n    fi\nfi\n";
    assert_eq!(Analysis::new(source).diagnostics(), Vec::new());
    assert_eq!(hover(source, "a =").0, "fixed a: Step");
}

#[test]
fn an_array_type_written_in_a_header_is_read_while_the_file_does_not_check() {
    // The count after `#` makes it an array, read from the parameter's type
    // as written, since the checker stops at the first line.
    let source = "task top(xs: [int#3], grid: [[float#2]#2]) -> int\n    show(unknown)\n    show(xs.@@, grid[0].@@)\n    <- 0\nfi\n";
    let first = source.replacen("@@", "", 1);
    assert_eq!(labels(&first), ["len", "str", "type"]);
    let second = source.replacen("@@", "", 1).replacen("grid[0].", "grid[0].@@", 1);
    assert_eq!(labels(&second.replacen("xs.@@", "xs.", 1)), ["len", "str", "type"]);
    let (code, _) = hover(&source.replace("@@", ""), "grid[0]");
    assert!(code.contains("[[float#2]#2]"), "{}", code);
}

#[test]
fn an_each_through_a_place_has_the_items_type_whether_or_not_the_file_checks() {
    let code = "each live in &points\n        show(live.x)\n    fi";
    // As it checks, with a mistake the checker reads past, and with one the
    // parser stops at, so that nothing the checker found is there.
    let parse_stops = PROGRAM.replace("CHECK", "show(").replace("    @@\n", &format!("    {}\n", code)) + "task (";
    for source in [program(true, code), program(false, code), parse_stops] {
        assert_eq!(hover(&source, "live in").0, "each live: Point", "{}", source);
        assert!(labels(&source.replacen("live.x", "live.@@", 1)).contains(&"x".to_string()), "{}", source);
    }
}
