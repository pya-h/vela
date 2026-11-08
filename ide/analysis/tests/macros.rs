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
    fixed area = P * 2.0
    fixed twice = SIZE * 2
    fixed limit = consts.LIMIT
    @@
fi
";

const CONSTS: &str = "\
#LIMIT = 10
#MASK = 0xfe
#DOWN = -0x1e
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
        assert_eq!(hover(&source, "SIZE * 2").0, "#SIZE = 4", "{}", when);
        let (code, doc) = hover(&source, "LIMIT\n");
        assert_eq!(code, "#LIMIT = 10", "{}", when);
        assert!(doc.contains("of type int"), "{}: {}", when, doc);
        assert_eq!(hover(&source, "twice =").0, "fixed twice: int", "{}", when);
        assert_eq!(hover(&source, "limit =").0, "fixed limit: int", "{}", when);
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
    let counted = "task main()\n    fixed xs = [0#3]\n    show(xs[#1])\nfi\n";
    let hashes: Vec<TokenClass> = Analysis::new(counted).tokens().into_iter().filter(|t| &counted[t.from..t.to] == "#").map(|t| t.class).collect();
    assert_eq!(hashes, [TokenClass::Punctuation, TokenClass::Punctuation]);
    // Nor is a comment written as other languages write one a macro.
    let noted = "task main()\nfi\n# the count\n";
    assert!(Analysis::new(noted).outline().iter().all(|item| item.kind != SymbolKind::Macro));
    assert_eq!(class_at("P = 3"), Some(TokenClass::Variant));
    assert_eq!(class_at("P * 2.0\n    fixed"), Some(TokenClass::Variant));
    assert_eq!(class_at("LIMIT\n"), Some(TokenClass::Variant));
    let plain = "task main()\n    fixed macro = 1\n    show(macro)\nfi\n";
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
    assert_eq!(labels("task main()\nfi\n\n#@@"), Vec::<String>::new());
}

#[test]
fn a_mistake_in_a_macro_is_shown_in_the_editor() {
    let source = "#A = 1 + 2.0\n\ntask main()\n    show(A)\nfi\n";
    let diagnostics = Analysis::new(source).diagnostics();
    assert_eq!(diagnostics.len(), 1, "{:?}", diagnostics);
    assert!(diagnostics[0].message.starts_with("an int and a float don't mix"), "{:?}", diagnostics);
}

#[test]
fn a_macro_in_another_base_is_an_int_though_it_holds_an_e() {
    let source = program(true, "show(consts.MASK, consts.DOWN)");
    for needle in ["MASK,", "DOWN)"] {
        let (code, doc) = hover(&source, needle);
        assert!(code.starts_with('#'), "{}", code);
        assert!(doc.contains("of type int"), "{}: {}", needle, doc);
    }
}

#[test]
fn an_indented_macro_outside_every_task_is_still_a_macro() {
    let source = "  #P = 3\ntask main()\n    show(P)\nfi\n";
    let analysis = Analysis::new(source);
    assert_eq!(analysis.diagnostics(), Vec::new());
    let outline: Vec<(String, SymbolKind)> = analysis.outline().into_iter().map(|item| (item.name, item.kind)).collect();
    assert_eq!(outline, [("P".to_string(), SymbolKind::Macro), ("main".to_string(), SymbolKind::Task)]);
    let byte = source.find("P)").unwrap();
    assert_eq!(analysis.hover(LineIndex::new(source).utf16_of(byte)).map(|h| h.code).as_deref(), Some("#P = 3"));
}

#[test]
fn values_made_of_macros_have_types_when_the_file_does_not_parse() {
    let source = "#N = 2\n#TAU = N * 3\ntask main()\n    fixed z = N\n    fixed w = N + 1\n    show(z, w)\nfi\ntask g(\n";
    let analysis = Analysis::new(source);
    let hover = |needle: &str| {
        let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
        analysis.hover(LineIndex::new(source).utf16_of(byte)).unwrap_or_else(|| panic!("no hover at {:?}", needle))
    };
    assert_eq!(hover("z =").code, "fixed z: int");
    assert_eq!(hover("w =").code, "fixed w: int");
    assert!(hover("TAU").doc.contains("of type int"), "{}", hover("TAU").doc);
}
