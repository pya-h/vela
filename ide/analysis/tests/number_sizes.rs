#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, CompletionItem, LineIndex, TokenClass};

const PROGRAM: &str = "\
task main()
    flex small: int8 = 100
    fixed wide: uint64 = 7
    fixed f: float32 = 1.5
    small += 1
    show(small, wide, f, int32(small))
    @@
fi
";

fn program(code: &str) -> String {
    PROGRAM.replace("    @@\n", &format!("    {}\n", code))
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

fn class_at(source: &str, needle: &str) -> Option<TokenClass> {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    Analysis::new(source).tokens().into_iter().find(|t| t.from <= byte && byte < t.to).map(|t| t.class)
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    let source = program("show(1)");
    assert_eq!(Analysis::new(&source).diagnostics(), Vec::new());
}

#[test]
fn sized_number_types_are_colored_and_hovered() {
    let source = program("show(1)");
    assert_eq!(class_at(&source, "int8 ="), Some(TokenClass::TypeBuiltin));
    assert_eq!(class_at(&source, "uint64 ="), Some(TokenClass::TypeBuiltin));
    assert_eq!(class_at(&source, "int32(small)"), Some(TokenClass::TypeBuiltin));
    let (code, doc) = hover(&source, "int8 =");
    assert_eq!(code, "int8");
    assert!(doc.contains("-128 to 127"), "{}", doc);
    let (_, doc) = hover(&source, "float32 =");
    assert!(doc.contains("32-bit"), "{}", doc);
}

#[test]
fn sized_number_types_are_offered_after_a_colon() {
    let labels: Vec<String> = completions(&program("fixed n: ui@@")).into_iter().map(|item| item.label).collect();
    for word in ["uint8", "uint16", "uint32", "uint64"] {
        assert!(labels.contains(&word.to_string()), "{} in {:?}", word, labels);
    }
}

#[test]
fn a_number_that_does_not_fit_is_shown_in_the_editor() {
    let source = program("fixed tiny: int8 = 300");
    let diagnostics = Analysis::new(&source).diagnostics();
    assert_eq!(diagnostics.len(), 1, "{:?}", diagnostics);
    assert_eq!(diagnostics[0].message, "300 doesn't fit in an int8, which holds -128 to 127");
    let source = program("show(small + wide)");
    let diagnostics = Analysis::new(&source).diagnostics();
    assert_eq!(diagnostics.len(), 1, "{:?}", diagnostics);
    assert!(diagnostics[0].message.starts_with("an int8 and a uint64 don't mix in arithmetic"), "{}", diagnostics[0].message);
}
