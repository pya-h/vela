#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::collections::HashMap;

use vela_analysis::{Analysis, CompletionItem, LineIndex, SymbolKind, TokenClass};

const PROGRAM: &str = "\
include shapes

alias SampleTask => (int) -> int
alias Tasks => [SampleTask#2]
priv alias Cents => int

task add_one(n: Cents) -> Cents
    <- n + 1
fi

task main()
    region r default
    fixed tasks: Tasks = [add_one, add_one]
    fixed corner: shapes.Corner = shapes.Point { x: 1, y: 2 }
    @@
fi
";

const SHAPES: &str = "\
shape Point
    x: int
    y: int
fi

alias Corner => Point
priv alias Hidden => int
";

fn sources() -> HashMap<String, String> {
    [("app/shapes.vela", SHAPES)].into_iter().map(|(path, source)| (path.to_string(), source.to_string())).collect()
}

fn program(code: &str) -> String {
    PROGRAM.replace("    @@\n", &format!("    {}\n", code))
}

fn analysis(source: &str) -> Analysis {
    Analysis::with_modules(source, "app/main.vela", &sources())
}

fn completions(marked: &str) -> Vec<CompletionItem> {
    let at = marked.find("@@").expect("a @@ marker");
    let source = marked.replacen("@@", "", 1);
    analysis(&source).complete(LineIndex::new(&source).utf16_of(at)).items
}

fn labels(marked: &str) -> Vec<String> {
    completions(marked).into_iter().map(|item| item.label).collect()
}

fn hover(source: &str, needle: &str) -> (String, String) {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    let hover = analysis(source).hover(LineIndex::new(source).utf16_of(byte)).unwrap_or_else(|| panic!("no hover at {:?}", needle));
    (hover.code, hover.doc)
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    let source = program("show(tasks[1](2), corner.x)");
    assert_eq!(analysis(&source).diagnostics(), Vec::new());
}

#[test]
fn a_type_alias_hovers_with_the_type_it_stands_for() {
    let source = program("show(0)");
    let (code, doc) = hover(&source, "SampleTask =>");
    assert_eq!(code, "alias SampleTask => (int) -> int");
    assert!(doc.contains("another name for (int) -> int"), "{}", doc);
    assert_eq!(hover(&source, "Cents =>").0, "priv alias Cents => int");
    assert_eq!(hover(&source, "SampleTask#2").0, "alias SampleTask => (int) -> int");
    let (code, doc) = hover(&source, "Corner =");
    assert_eq!(code, "alias Corner => shapes.Point");
    assert!(doc.contains("shapes"), "{}", doc);
    assert_eq!(hover(&source, "tasks:").0, "fixed tasks: [(int) -> int#2]");
}

#[test]
fn a_type_alias_is_colored_as_a_type() {
    let source = program("show(0)");
    let analysis = analysis(&source);
    let class_at = |needle: &str| {
        let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
        analysis.tokens().into_iter().find(|t| t.from <= byte && byte < t.to).map(|t| t.class)
    };
    assert_eq!(class_at("alias SampleTask"), Some(TokenClass::Keyword));
    assert_eq!(class_at("SampleTask =>"), Some(TokenClass::Type));
    assert_eq!(class_at("SampleTask#2"), Some(TokenClass::Type));
    assert_eq!(class_at("Cents) ->"), Some(TokenClass::Type));
    assert_eq!(class_at("Corner ="), Some(TokenClass::Type));
    let plain = "task main()\n    fixed alias = 1\n    show(alias)\nfi\n";
    let classes: Vec<TokenClass> = Analysis::new(plain).tokens().into_iter().filter(|t| &plain[t.from..t.to] == "alias").map(|t| t.class).collect();
    assert_eq!(classes, [TokenClass::Variable, TokenClass::Variable]);
}

#[test]
fn type_aliases_are_in_the_outline() {
    let source = program("show(0)");
    let outline = analysis(&source).outline();
    let items: Vec<(&str, SymbolKind, &str)> = outline.iter().map(|item| (item.name.as_str(), item.kind, item.detail.as_str())).collect();
    assert_eq!(
        items,
        [
            ("SampleTask", SymbolKind::Alias, "(int) -> int"),
            ("Tasks", SymbolKind::Alias, "[SampleTask#2]"),
            ("Cents", SymbolKind::Alias, "int"),
            ("add_one", SymbolKind::Task, "(n: Cents) -> Cents"),
            ("main", SymbolKind::Task, "()"),
        ]
    );
}

#[test]
fn type_aliases_are_offered_where_a_type_goes_and_as_a_word() {
    let found = labels(&program("fixed later: @@"));
    for name in ["SampleTask", "Tasks", "Cents"] {
        assert!(found.contains(&name.to_string()), "{:?}", found);
    }
    let found = labels(&program("fixed later: shapes.@@"));
    assert!(found.contains(&"Corner".to_string()) && found.contains(&"Point".to_string()), "{:?}", found);
    assert!(!found.contains(&"Hidden".to_string()), "{:?}", found);
    let found = completions("task main()\nfi\n\n@@");
    let snippet = found.iter().find(|item| item.label == "alias").expect("an alias snippet");
    assert_eq!(snippet.snippet.as_deref(), Some("alias ${Name} => ${type}"));
}

#[test]
fn a_mistake_in_a_type_alias_is_shown_in_the_editor() {
    let source = "alias Missing => Nowhere\n\ntask main()\nfi\n";
    let diagnostics = Analysis::new(source).diagnostics();
    assert_eq!(diagnostics.len(), 1, "{:?}", diagnostics);
    assert_eq!(diagnostics[0].message, "unknown type 'Nowhere'");
}
