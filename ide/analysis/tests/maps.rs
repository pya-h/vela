//! Maps in the editor: `[K: V]@r` hovered and completed with the compiler's
//! types, and with the same types read from the declarations and values while
//! the file doesn't check. In the sources below, `@@` marks the cursor.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, CompletionItem, LineIndex};

const PROGRAM: &str = "\
task total(m: [text: int]@r) -> int
    hold sum = 0
    each name, count in m
        sum = sum + count
    fi
    <- sum
fi

task main()
    CHECK
    region r
    hold ages = new [text: int]([\"ana\": 31]) at r
    hold sizes = [\"w\": 1.5, \"h\": 2.0] at r
    hold named = new [:int]([one: 1, two: 2]) at r
    ages[\"bo\"] = 25
    bind age = ages[\"ana\"]
    bind maybe_age = ages[#\"cy\"]
    bind known = ages.has(\"bo\")
    bind taken = ages.del(\"bo\")
    bind names = ages.keys at r
    bind counts = ages.values at r
    bind how_many = ages.len
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

fn hover(source: &str, needle: &str) -> (String, String) {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    let hover = Analysis::new(source).hover(LineIndex::new(source).utf16_of(byte)).unwrap_or_else(|| panic!("no hover at {:?}", needle));
    (hover.code, hover.doc)
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    let source = program(true, "show(total(ages), sizes, named, age, maybe_age, known, taken, names, counts, how_many)");
    assert_eq!(Analysis::new(&source).diagnostics(), Vec::new());
}

#[test]
fn maps_hover_with_their_types_whether_or_not_the_file_checks() {
    for checks in [true, false] {
        let source = program(checks, "show(0)");
        let code = |needle: &str| hover(&source, needle).0;
        let when = if checks { "checked" } else { "declared" };
        assert_eq!(code("ages ="), "hold ages: [text: int]@r", "{}", when);
        assert_eq!(code("sizes ="), "hold sizes: [text: float]@r", "{}", when);
        assert_eq!(code("named ="), "hold named: [text: int]@r", "{}", when);
        assert_eq!(code("age ="), "bind age: int", "{}", when);
        assert_eq!(code("maybe_age ="), "bind maybe_age: maybe int", "{}", when);
        assert_eq!(code("known ="), "bind known: bool", "{}", when);
        assert_eq!(code("taken ="), "bind taken: maybe int", "{}", when);
        assert_eq!(code("names ="), "bind names: [text]@r", "{}", when);
        assert_eq!(code("counts ="), "bind counts: [int]@r", "{}", when);
        assert_eq!(code("how_many ="), "bind how_many: int", "{}", when);
        assert_eq!(code("name, count"), "each name: text@r", "{}", when);
        assert_eq!(code("count in"), "each count: int", "{}", when);
    }
}

#[test]
fn a_map_offers_its_members_after_a_dot() {
    for checks in [true, false] {
        let found = labels(&program(checks, "ages.@@"));
        for wanted in ["len", "has", "keys", "values", "del", "copy", "str"] {
            assert!(found.contains(&wanted.to_string()), "{} missing from {:?} (checks: {})", wanted, found, checks);
        }
        assert!(!found.contains(&"add".to_string()), "{:?}", found);
    }
}
