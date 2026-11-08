#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, CompletionItem, LineIndex, TokenClass};

const PROGRAM: &str = "\
task main()
    CHECK
    flex total = 0
    block
        flex inner = 2
        total = total + inner
    fi
    block
        fixed inner = \"text\"
        show(inner)
    fi
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

fn hover(source: &str, needle: &str) -> String {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    Analysis::new(source).hover(LineIndex::new(source).utf16_of(byte)).unwrap_or_else(|| panic!("no hover at {:?}", needle)).code
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    assert_eq!(Analysis::new(&program(true, "show(total)")).diagnostics(), Vec::new());
}

#[test]
fn each_block_has_its_own_names() {
    for checks in [true, false] {
        let source = program(checks, "show(total)");
        let when = if checks { "checked" } else { "declared" };
        assert_eq!(hover(&source, "inner = 2"), "flex inner: int", "{}", when);
        assert_eq!(hover(&source, "inner\n    fi\n    block"), "flex inner: int", "{}", when);
        assert_eq!(hover(&source, "inner = \"text\""), "fixed inner: text", "{}", when);
        assert_eq!(hover(&source, "inner)\n    fi\n    show"), "fixed inner: text", "{}", when);
        let analysis = Analysis::new(&source);
        let byte = source.find("block").unwrap_or_default();
        let class = analysis.tokens().into_iter().find(|t| t.from <= byte && byte < t.to).map(|t| t.class);
        assert_eq!(class, Some(TokenClass::Keyword), "{}", when);
    }
}

#[test]
fn a_name_of_a_block_is_not_offered_after_it() {
    for checks in [true, false] {
        let offered = labels(&program(checks, "show(@@)"));
        assert!(offered.contains(&"total".to_string()), "{:?}", offered);
        assert!(!offered.contains(&"inner".to_string()), "{:?}", offered);
    }
}

#[test]
fn completion_offers_the_word_and_nothing_after_it() {
    let offered = labels(&program(true, "bl@@"));
    assert!(offered.iter().filter(|label| *label == "block").count() >= 1, "{:?}", offered);
    let snippet = completions(&program(true, "bl@@")).into_iter().find(|item| item.snippet.as_deref() == Some("block\n\t${}\nfi"));
    assert!(snippet.is_some());
    assert!(labels(&program(true, "block @@")).is_empty());
}

#[test]
fn a_name_used_after_its_block_is_reported() {
    let diagnostics = Analysis::new(&program(true, "show(inner)")).diagnostics();
    let [diagnostic] = diagnostics.as_slice() else { panic!("{:?}", diagnostics) };
    assert_eq!(diagnostic.message, "unknown variable 'inner'");
}
