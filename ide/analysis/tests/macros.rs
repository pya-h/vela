//! Macros in the editor: `#P = 3.14159` outside every task, colored,
//! hovered with the type of its value, listed in the outline, and completed by
//! name, through its module, and as a word at the top level. In the sources
//! below, `@@` marks the cursor.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::collections::HashMap;

use vela_analysis::{Analysis, CompletionItem, LineIndex, SymbolKind, TokenClass};

const PROGRAM: &str = "\
include consts

#P = 3.14159
#TAU = P * 2.0
#SIZE = 4
priv #NAME = \"vela\"

task main()
    CHECK
    bind area = P * 2.0
    bind twice = SIZE * 2
    bind limit = consts.LIMIT
    @@
fi
";

const CONSTS: &str = "\
#LIMIT = 10
#GREETING = \"hello\"
priv #SECRET = 42
";

fn sources() -> HashMap<String, String> {
    [("app/consts.vela", CONSTS)].into_iter().map(|(path, source)| (path.to_string(), source.to_string())).collect()
}

fn program(checks: bool, code: &str) -> String {
    let first = if checks { "show(0)" } else { "show(unknown)" };
    PROGRAM.replace("CHECK", first).replace("    @@\n", &format!("    {}\n", code))
}

fn completions(marked: &str) -> Vec<CompletionItem> {
    let at = marked.find("@@").expect("a @@ marker");
    let source = marked.replacen("@@", "", 1);
    let sources = sources();
    Analysis::with_modules(&source, "app/main.vela", &sources).complete(LineIndex::new(&source).utf16_of(at)).items
}

fn labels(marked: &str) -> Vec<String> {
    completions(marked).into_iter().map(|item| item.label).collect()
}

fn hover(source: &str, needle: &str) -> (String, String) {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    let sources = sources();
    let hover = Analysis::with_modules(source, "app/main.vela", &sources).hover(LineIndex::new(source).utf16_of(byte)).unwrap_or_else(|| panic!("no hover at {:?}", needle));
    (hover.code, hover.doc)
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    let source = program(true, "show(area, twice, limit, TAU, NAME, consts.GREETING)");
    let sources = sources();
    assert_eq!(Analysis::with_modules(&source, "app/main.vela", &sources).diagnostics(), Vec::new());
}

#[test]
fn a_macro_hovers_with_its_value_and_type_whether_or_not_the_file_checks() {
    for checks in [true, false] {
        let source = program(checks, "show(0)");
        let when = if checks { "checked" } else { "declared" };
        let (code, doc) = hover(&source, "TAU =");
        assert_eq!(code, "#TAU = P * 2.0", "{}", when);
        assert!(doc.contains("of type float"), "{}: {}", when, doc);
        assert_eq!(hover(&source, "NAME =").0, "priv #NAME = \"vela\"", "{}", when);
        // A use hovers as the macro it names, here and in another module.
        assert_eq!(hover(&source, "SIZE * 2").0, "#SIZE = 4", "{}", when);
        let (code, doc) = hover(&source, "LIMIT\n");
        assert_eq!(code, "#LIMIT = 10", "{}", when);
        assert!(doc.contains("of type int"), "{}: {}", when, doc);
        assert_eq!(hover(&source, "twice =").0, "bind twice: int", "{}", when);
        assert_eq!(hover(&source, "limit =").0, "bind limit: int", "{}", when);
    }
}

#[test]
fn a_macro_is_colored_as_a_value_that_never_changes() {
    let source = program(true, "show(0)");
    let sources = sources();
    let analysis = Analysis::with_modules(&source, "app/main.vela", &sources);
    let class_at = |needle: &str| {
        let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
        analysis.tokens().into_iter().find(|t| t.from <= byte && byte < t.to).map(|t| t.class)
    };
    assert_eq!(class_at("#P"), Some(TokenClass::Keyword));
    // `#` in an array's count is punctuation.
    let counted = "task main()\n    bind xs = [0#3]\n    show(xs[#1])\nfi\n";
    let hashes: Vec<TokenClass> = Analysis::new(counted).tokens().into_iter().filter(|t| &counted[t.from..t.to] == "#").map(|t| t.class).collect();
    assert_eq!(hashes, [TokenClass::Punctuation, TokenClass::Punctuation]);
    // Nor is a comment written as other languages write one a macro.
    let noted = "task main()\nfi\n# the count\n";
    assert!(Analysis::new(noted).outline().iter().all(|item| item.kind != SymbolKind::Macro));
    assert_eq!(class_at("P = 3"), Some(TokenClass::Variant));
    assert_eq!(class_at("P * 2.0\n    bind"), Some(TokenClass::Variant));
    assert_eq!(class_at("LIMIT\n"), Some(TokenClass::Variant));
    // A variable named `macro` is only a variable.
    let plain = "task main()\n    bind macro = 1\n    show(macro)\nfi\n";
    let classes: Vec<TokenClass> = Analysis::new(plain).tokens().into_iter().filter(|t| &plain[t.from..t.to] == "macro").map(|t| t.class).collect();
    assert_eq!(classes, [TokenClass::Variable, TokenClass::Variable]);
}

#[test]
fn macros_are_in_the_outline() {
    let source = program(true, "show(0)");
    let sources = sources();
    let outline = Analysis::with_modules(&source, "app/main.vela", &sources).outline();
    let items: Vec<(&str, SymbolKind)> = outline.iter().map(|item| (item.name.as_str(), item.kind)).collect();
    assert_eq!(items, [("P", SymbolKind::Macro), ("TAU", SymbolKind::Macro), ("SIZE", SymbolKind::Macro), ("NAME", SymbolKind::Macro), ("main", SymbolKind::Task)]);
}

#[test]
fn macros_are_offered_by_name_through_their_module_and_as_a_word() {
    for checks in [true, false] {
        let found = labels(&program(checks, "show(@@)"));
        for name in ["P", "TAU", "SIZE", "NAME"] {
            assert!(found.contains(&name.to_string()), "{:?}", found);
        }
        let found = labels(&program(checks, "show(consts.@@)"));
        assert!(found.contains(&"LIMIT".to_string()) && found.contains(&"GREETING".to_string()), "{:?}", found);
        assert!(!found.contains(&"SECRET".to_string()), "{:?}", found);
    }
    let found = completions("task main()\nfi\n\n@@");
    let snippet = found.iter().find(|item| item.label == "macro").expect("a macro snippet");
    assert_eq!(snippet.snippet.as_deref(), Some("#${NAME} = ${value}"));
    // A new macro's name is its own.
    assert_eq!(labels("task main()\nfi\n\n#@@"), Vec::<String>::new());
}

#[test]
fn a_mistake_in_a_macro_is_shown_in_the_editor() {
    let source = "#A = 1 + 2.0\n\ntask main()\n    show(A)\nfi\n";
    let diagnostics = Analysis::new(source).diagnostics();
    assert_eq!(diagnostics.len(), 1, "{:?}", diagnostics);
    assert!(diagnostics[0].message.starts_with("an int and a float don't mix"), "{:?}", diagnostics);
}
