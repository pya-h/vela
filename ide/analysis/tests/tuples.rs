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
        assert_eq!(pairs, [("0", "text"), ("1", "int")], "checks: {}", checks);
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
