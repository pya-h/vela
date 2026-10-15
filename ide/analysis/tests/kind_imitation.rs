//! Kind imitation in the editor: `kind AppError::ParseErr & errs.Missing` has
//! the variants of the kinds it imitates, so the editor hovers, colors and
//! completes them through it as it does its own, here and from another module.
//! In the sources below, `@@` marks the cursor.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::collections::HashMap;

use vela_analysis::{Analysis, CompletionItem, LineIndex, SymbolKind, TokenClass};

const PROGRAM: &str = "\
include errs

kind ParseErr
    Empty
    Bad(place: int)
fi

kind AppError::ParseErr & errs.Missing
    Io(code: int)
fi

task load(n: int) -> int ~ AppError
    when n == 0
        <- fail Empty
    fi
    <- n
fi

task main()
    fixed e: AppError = AppError.Bad(3)
    @@
fi
";

const ERRS: &str = "\
kind Missing
    NoFile(name: text)
fi
";

fn sources() -> HashMap<String, String> {
    [("app/errs.vela", ERRS)].into_iter().map(|(path, source)| (path.to_string(), source.to_string())).collect()
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
    let source = program("show(e, load(0) { 0 })");
    assert_eq!(analysis(&source).diagnostics(), Vec::new());
}

#[test]
fn a_kind_hovers_with_the_variants_it_imitates() {
    let source = program("show(e)");
    let (code, _) = hover(&source, "AppError::");
    assert_eq!(code, "kind AppError::ParseErr & errs.Missing\n    Empty ; from ParseErr\n    Bad(place: int) ; from ParseErr\n    NoFile(name: text) ; from Missing\n    Io(code: int)\nfi");
    // A variant written through the imitating kind is the variant itself.
    let (code, doc) = hover(&source, "Bad(3)");
    assert_eq!(code, "Bad(place: int)");
    assert!(doc.contains("ParseErr"), "{}", doc);
}

#[test]
fn the_kinds_a_kind_imitates_are_colored_as_types() {
    let source = program("show(e)");
    let analysis = analysis(&source);
    let class_at = |needle: &str| {
        let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
        analysis.tokens().into_iter().find(|t| t.from <= byte && byte < t.to).map(|t| t.class)
    };
    assert_eq!(class_at("ParseErr &"), Some(TokenClass::Type));
    assert_eq!(class_at("Missing\n"), Some(TokenClass::Type));
    assert_eq!(class_at("Bad(3)"), Some(TokenClass::Variant));
}

#[test]
fn an_imitating_kind_is_in_the_outline_with_what_it_imitates() {
    let source = program("show(e)");
    let outline = analysis(&source).outline();
    let kind = outline.iter().find(|item| item.name == "AppError").expect("AppError in the outline");
    assert_eq!((kind.kind, kind.detail.as_str()), (SymbolKind::Kind, "::ParseErr & errs.Missing"));
}

#[test]
fn the_variants_a_kind_imitates_are_offered_through_it() {
    let found = labels(&program("fixed f = AppError.@@"));
    for name in ["Empty", "Bad", "NoFile", "Io"] {
        assert!(found.contains(&name.to_string()), "{:?}", found);
    }
    // A match on a value of it is offered with a case for each.
    let found = completions(&program("ma@@"));
    let matched = found.iter().find(|item| item.label == "match e").expect("a match on e");
    let snippet = matched.snippet.as_deref().unwrap_or_default();
    for case in ["case Empty", "case Bad", "case NoFile", "case Io"] {
        assert!(snippet.contains(case), "{}", snippet);
    }
}

#[test]
fn a_mistake_in_an_imitation_is_shown_in_the_editor() {
    let source = "kind A::Nowhere\n    One\nfi\n\ntask main()\nfi\n";
    let diagnostics = Analysis::new(source).diagnostics();
    assert_eq!(diagnostics.len(), 1, "{:?}", diagnostics);
    assert_eq!(diagnostics[0].message, "unknown kind 'Nowhere'");
}
