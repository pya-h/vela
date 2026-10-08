//! A block's default region in the editor: `region r default` and
//! `r: region default`, the types of the values that go there, and what Studio
//! offers and says about the word. In the sources below, `@@` marks the cursor.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, LineIndex, TokenClass};

const PROGRAM: &str = "\
shape Card
    rank: int

    str(r: region default) -> text@r
        <- \"card \" + .rank.str()
    fi
fi

task grow(r: region default, upto: int) -> [int]@r
    flex made = new [int]()
    each n in 1..upto
        made.add(n)
    fi
    <- made
fi

task main()
    region kept default
    fixed squares = grow(kept, 3)
    fixed p = new int(7)
    fixed greeting = \"Hello, \" + \"Ada\"
    fixed shown = p^.str()
    each round in 0..2
        region pass default
        fixed line = \"pass \" + round.str()
        show(line)
    fi
    show(squares.len, greeting, shown)
    @@
";

fn program(code: &str) -> String {
    PROGRAM.replace("    @@\n", &format!("    {}\nfi\n", code))
}

fn hover(source: &str, needle: &str) -> (String, String) {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    let hover = Analysis::new(source).hover(LineIndex::new(source).utf16_of(byte)).unwrap_or_else(|| panic!("no hover at {:?}", needle));
    (hover.code, hover.doc)
}

fn labels(marked: &str) -> Vec<String> {
    let at = marked.find("@@").expect("a @@ marker");
    let source = marked.replacen("@@", "", 1);
    Analysis::new(&source).complete(LineIndex::new(&source).utf16_of(at)).items.into_iter().map(|item| item.label).collect()
}

fn class_at(source: &str, needle: &str) -> TokenClass {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    let at = LineIndex::new(source).utf16_of(byte);
    let analysis = Analysis::new(source);
    analysis.tokens().into_iter().find(|t| t.from <= at && at < t.to).unwrap_or_else(|| panic!("no token at {:?}", needle)).class
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    assert_eq!(Analysis::new(&program("show(1)")).diagnostics(), Vec::new());
}

#[test]
fn a_value_that_names_no_region_has_the_type_of_the_default() {
    let source = program("show(1)");
    // Hover says where a value lives, which is what a written `at r` would say.
    assert_eq!(hover(&source, "p = new").0, "bind p: ^int@kept");
    assert_eq!(hover(&source, "greeting =").0, "bind greeting: text@kept");
    assert_eq!(hover(&source, "shown =").0, "bind shown: text@kept");
    assert_eq!(hover(&source, "squares =").0, "bind squares: [int]@kept");
    // An inner block's own default: this text is freed when the loop's pass ends.
    assert_eq!(hover(&source, "line = ").0, "bind line: text@pass");
    // The region a task is given, which its header makes the default.
    assert_eq!(hover(&source, "made = new").0, "hold made: [int]@r");
}

#[test]
fn the_word_is_a_keyword_where_it_marks_a_default_region() {
    let source = program("show(1)");
    assert_eq!(class_at(&source, "default) -> text@r"), TokenClass::Keyword);
    assert_eq!(class_at(&source, "default, upto"), TokenClass::Keyword);
    assert_eq!(class_at(&source, "default\n    fixed squares"), TokenClass::Keyword);
    // The region it marks is still a region, and the name it declares is one too.
    assert_eq!(class_at(&source, "kept default"), TokenClass::Region);
    assert_eq!(class_at(&source, "pass default"), TokenClass::Region);
    // A parameter the word marks is still the region it takes, not an ordinary
    // parameter: `grow(r: region default, upto: int)`.
    assert_eq!(class_at(&source, "r: region default, upto"), TokenClass::Region);
    assert_eq!(class_at(&source, "upto: int"), TokenClass::Parameter);
}

#[test]
fn the_word_has_a_hover_of_its_own() {
    let source = program("show(1)");
    let (code, doc) = hover(&source, "default\n    fixed squares");
    assert_eq!(code, "region r default");
    assert!(doc.starts_with("Makes r the block's default region"), "{}", doc);
}

#[test]
fn completion_offers_the_word_after_a_region_name_and_after_a_region_type() {
    assert!(labels("task main()\n    region r @@\nfi\n").contains(&"default".to_string()));
    assert!(labels("task keep(r: region @@) -> int\n    <- 1\nfi\n").contains(&"default".to_string()));
    // Not where a region's name is being declared, nor anywhere else.
    assert!(!labels("task main()\n    region @@\nfi\n").contains(&"default".to_string()));
    assert!(!labels("task main()\n    region r\n    fixed x = @@\nfi\n").contains(&"default".to_string()));
}

#[test]
fn a_file_that_does_not_check_still_highlights_the_word() {
    // Studio reads a half-typed file from its tokens alone.
    let source = "task main()\n    region r default\n    fixed p = new int(missing)\nfi\n";
    assert!(!Analysis::new(source).diagnostics().is_empty());
    assert_eq!(class_at(source, "default"), TokenClass::Keyword);
    assert_eq!(class_at(source, "r default"), TokenClass::Region);
}
