#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, LineIndex, TokenClass};

fn at(source: &str, needle: &str, nth: usize) -> usize {
    let byte = source.match_indices(needle).nth(nth).unwrap_or_else(|| panic!("no {:?} #{}", needle, nth)).0;
    LineIndex::new(source).utf16_of(byte)
}

fn hover(source: &str, needle: &str) -> (String, String) {
    let hover = Analysis::new(source).hover(at(source, needle, 0)).unwrap_or_else(|| panic!("no hover at {:?}", needle));
    (hover.code, hover.doc)
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
    flex light: Light = Dimmed(4)
    fixed score = 70
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
    assert_eq!(class_at(CHAIN, "level)", 0), TokenClass::Variable);
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

#[test]
fn a_when_on_a_line_of_its_own_inside_an_else_is_a_block_of_its_own() {
    // Only `else when` on one line is a chain: a `when` on the line below an
    // `else` opens a block, which the `else` block's own `fi` closes, so the
    // name it binds is gone after it and the enclosing names are back.
    let source = "\
task main()
    fixed a: maybe int = none
    fixed b: maybe int = 7
    when a is {x}
        show(x)
    else
        when b is {y}
            show(y)
        fi
        show(0)
    fi
    show(1)
fi
";
    assert_eq!(Analysis::new(source).diagnostics(), Vec::new());
    assert_eq!(class_at(source, "y)", 0), TokenClass::Variable);
    let after = source.replace("        show(0)\n", "        show(y)\n");
    let complaints: Vec<String> = Analysis::new(&after).diagnostics().into_iter().map(|d| d.message).collect();
    assert!(complaints.iter().any(|m| m.contains("'y'")), "{:?}", complaints);
}

const VALUES: &str = "\
kind Reading
    Ok(value: int)
    Missing
fi

task main()
    fixed r = Ok(8)
    fixed half = match r
    case Ok value = value / 2
    case Missing = none
    fi
    fixed label = match r
    case Ok value
        when value > 5
            \"high\"
        else
            \"low\"
        fi
    case Missing = \"none\"
    fi
    show(half, label)
fi
";

#[test]
fn a_match_and_a_when_used_as_values_check() {
    assert_eq!(Analysis::new(VALUES).diagnostics(), Vec::new());
}

#[test]
fn hover_shows_the_type_the_branches_settle_on() {
    assert_eq!(hover(VALUES, "half = match").0, "fixed half: maybe int");
    assert_eq!(hover(VALUES, "label = match").0, "fixed label: text");
    assert_eq!(hover(VALUES, "value / 2").0, "value: int");
}

#[test]
fn a_value_a_case_gives_is_read_as_a_value_not_a_pattern() {
    assert_eq!(class_at(VALUES, "Ok value = value", 0), TokenClass::Variant);
    assert_eq!(class_at(VALUES, "value = value", 0), TokenClass::Variable);
    assert_eq!(class_at(VALUES, "value / 2", 0), TokenClass::Variable);
    assert_eq!(class_at(VALUES, "match r", 0), TokenClass::Keyword);
    assert_eq!(class_at(VALUES, "when value > 5", 0), TokenClass::Keyword);
}

#[test]
fn completion_offers_a_match_and_a_when_where_a_value_goes() {
    let items = labels("task main()\n    fixed x = @@\nfi\n");
    assert!(items.contains(&"match".to_string()), "{:?}", items);
    assert!(items.contains(&"when".to_string()), "{:?}", items);
    assert!(items.contains(&"match … = value".to_string()), "{:?}", items);
    let inside = labels("kind K\n    Ok(v: int)\nfi\n\ntask main()\n    fixed k = Ok(1)\n    fixed n = match k\n    case Ok v = @@\n    fi\n    show(n)\nfi\n");
    assert!(inside.contains(&"v".to_string()), "{:?}", inside);
}

#[test]
fn a_loop_is_not_offered_a_when() {
    // A `loop` takes its condition directly, which the parser insists on, so the
    // word that would open a value there is left out; a `match` still fits.
    let items = labels("task main()\n    flex k = 0\n    loop @@\nfi\n");
    assert!(!items.contains(&"when".to_string()), "{:?}", items);
    assert!(!items.contains(&"when … else".to_string()), "{:?}", items);
    assert!(items.contains(&"match".to_string()), "{:?}", items);
}

#[test]
fn a_half_typed_value_still_highlights_and_says_what_it_can() {
    let source = "task main()\n    fixed n = match missing\n    case Ok v = v\n    fi\n    show(n)\nfi\n";
    assert!(!Analysis::new(source).diagnostics().is_empty());
    assert_eq!(class_at(source, "match missing", 0), TokenClass::Keyword);
    assert_eq!(class_at(source, "case", 0), TokenClass::Keyword);
    assert_eq!(class_at(source, "v = v", 0), TokenClass::Variable);
}

