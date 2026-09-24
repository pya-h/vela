//! `else when` chains in the editor: the words, what each branch's names reach,
//! and what Studio offers inside one.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, LineIndex, TokenClass};

/// The UTF-16 offset of the `nth` (0-based) occurrence of `needle`.
fn at(source: &str, needle: &str, nth: usize) -> usize {
    let byte = source.match_indices(needle).nth(nth).unwrap_or_else(|| panic!("no {:?} #{}", needle, nth)).0;
    LineIndex::new(source).utf16_of(byte)
}

fn class_at(source: &str, needle: &str, nth: usize) -> TokenClass {
    let offset = at(source, needle, nth);
    Analysis::new(source).tokens().into_iter().find(|t| t.from == offset).unwrap_or_else(|| panic!("no token at {:?}", needle)).class
}

fn labels(marked: &str) -> Vec<String> {
    let at = marked.find("@@").expect("a @@ marker");
    let source = marked.replacen("@@", "", 1);
    Analysis::new(&source).complete(LineIndex::new(&source).utf16_of(at)).items.into_iter().map(|item| item.label).collect()
}

const CHAIN: &str = "\
kind Light
    Off
    Dimmed(level: int)
fi

task main()
    hold light: Light = Dimmed(4)
    bind score = 70
    when score >= 90
        show(\"top\")
    else when light is Dimmed level
        show(level)
    else when score >= 60
        show(\"pass\")
    else
        show(\"none\")
    fi
    show(score)
fi
";

#[test]
fn a_chain_of_else_whens_checks() {
    assert_eq!(Analysis::new(CHAIN).diagnostics(), Vec::new());
}

#[test]
fn both_words_of_an_else_when_are_keywords() {
    assert_eq!(class_at(CHAIN, "else when light", 0), TokenClass::Keyword);
    assert_eq!(class_at(CHAIN, "when light", 0), TokenClass::Keyword);
    assert_eq!(class_at(CHAIN, "score >= 60", 0), TokenClass::Variable);
}

#[test]
fn a_name_a_branch_binds_stays_in_that_branch() {
    // `level` is the pattern's name in the branch that matched it.
    assert_eq!(class_at(CHAIN, "level)", 0), TokenClass::Variable);
    // One `fi` closes the whole chain, so what follows is the enclosing block
    // again: a name from a branch is gone, and the block's own names are back.
    assert_eq!(class_at(CHAIN, "score)", 0), TokenClass::Variable);
    let after = CHAIN.replace("    show(score)\n", "    show(level)\n");
    let complaints: Vec<String> = Analysis::new(&after).diagnostics().into_iter().map(|d| d.message).collect();
    assert!(complaints.iter().any(|m| m.contains("'level'")), "{:?}", complaints);
}

#[test]
fn completion_offers_when_after_else() {
    let items = labels("task main()\n    when 1 > 2\n        show(1)\n    else @@\nfi\n");
    assert!(items.contains(&"when".to_string()), "{:?}", items);
}
