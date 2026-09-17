//! Copies into a region in the editor: `x.copy()`, `x.copy() at r`, and
//! `x.copy()@r`, with the compiler's types and with the types read from the
//! source while the file doesn't check. In the sources below, `@@` marks the
//! cursor.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, CompletionItem, LineIndex};

const PROGRAM: &str = "\
shape Person
    name: text
    age: int
fi

shape Node
    label: text
    next: maybe ^Node
fi

task main()
    CHECK
    region kept
    region work
    bind line = scan(\"\") at work { \"\" }
    bind written = line.copy() at kept
    bind short = line.copy()@kept
    hold place: text@kept = \"\"
    place = line.copy()
    bind worker: Person@work = Person { name: line, age: 3 }
    bind person = worker.copy() at kept
    bind node = new Node(Node { label: line, next: none }) at work
    bind nodes = node.copy()@kept
    show(place)
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
fn a_copy_has_the_type_of_its_value_in_the_region_named_whether_or_not_the_file_checks() {
    for checks in [true, false] {
        let source = program(checks, "show(1)");
        let when = if checks { "checked" } else { "declared" };
        assert_eq!(hover(&source, "written =").0, "bind written: text@kept", "{}", when);
        assert_eq!(hover(&source, "short =").0, "bind short: text@kept", "{}", when);
        assert_eq!(hover(&source, "person =").0, "bind person: Person@kept", "{}", when);
        assert_eq!(hover(&source, "nodes =").0, "bind nodes: ^Node@kept", "{}", when);
    }
}

#[test]
fn copy_has_a_hover() {
    let source = program(true, "show(1)");
    let (code, doc) = hover(&source, "copy() at");
    assert_eq!(code, "copy() at r or copy()@r");
    assert!(doc.starts_with("Copies a value into a region"), "{}", doc);
}

#[test]
fn completion_offers_copy_on_values_holding_text_slices_or_pointers() {
    for checks in [true, false] {
        assert!(labels(&program(checks, "show(line.@@)")).contains(&"copy".to_string()), "text, checks: {}", checks);
        let fields = labels(&program(checks, "show(worker.@@)"));
        assert!(fields.contains(&"name".to_string()) && fields.contains(&"copy".to_string()), "{:?}", fields);
        let counted = labels(&program(checks, "bind n = 3\n    show(n.@@)"));
        assert!(!counted.contains(&"copy".to_string()), "an int, checks: {}: {:?}", checks, counted);
        assert!(labels(&program(checks, "bind again = line.copy() @@")).contains(&"at".to_string()), "checks: {}", checks);
        assert!(labels(&program(checks, "bind again = line.copy()@ @@")).contains(&"kept".to_string()), "checks: {}", checks);
    }
    // A pointer's fields are reached through `^`; the pointer itself is copied.
    let through = completions(&program(true, "show(node.@@)"));
    let copy = through.iter().find(|item| item.label == "copy").expect("copy is offered");
    assert_eq!(copy.snippet.as_deref(), Some(".copy()"));
}

#[test]
fn a_copy_without_a_region_is_reported() {
    let source = program(true, "bind lost = line.copy()");
    let diagnostics = Analysis::new(&source).diagnostics();
    let [diagnostic] = diagnostics.as_slice() else { panic!("{:?}", diagnostics) };
    assert!(diagnostic.message.starts_with("can't tell which region to copy into"), "{}", diagnostic.message);
}
