//! Text in the editor: its length, `t.len`, and its parts, `t[a..b]`, with
//! the compiler's types and with the types read from the declarations while
//! the file doesn't check. In the sources below, `@@` marks the cursor.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, CompletionItem, LineIndex};

const PROGRAM: &str = "\
shape Person
    name: text
fi

task main()
    CHECK
    region r
    bind name = \"Ada\"
    bind person = Person { name: name }
    bind line = scan(\"\") at r { \"\" }
    bind count = name.len
    bind inner = person.name.len
    bind read = line.len
    bind part = name[0..2]
    bind kept = line[1..line.len]
    bind inside = person.name[0..1][0..1]
    bind before = name < \"Bob\"
    bind joined = name + \" \" + line at r
    bind wrapped = (\"<\" + name + \">\")@r
    bind sum = count + 1
    @@
fi
";

/// `PROGRAM` with `code` at the cursor, and a first line in `main` that
/// either checks or stops the checker before anything else in `main`.
fn program(checks: bool, code: &str) -> String {
    let first = if checks { "show(0)" } else { "show(unknown)" };
    PROGRAM.replace("CHECK", first).replace("    @@\n", &format!("    {}\n", code))
}

fn completions(marked: &str) -> Vec<CompletionItem> {
    let at = marked.find("@@").expect("a @@ marker");
    let source = marked.replacen("@@", "", 1);
    Analysis::new(&source).complete(LineIndex::new(&source).utf16_of(at)).items
}

/// The code and doc of the hover at the first occurrence of `needle`.
fn hover(source: &str, needle: &str) -> (String, String) {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    let hover = Analysis::new(source).hover(LineIndex::new(source).utf16_of(byte)).unwrap_or_else(|| panic!("no hover at {:?}", needle));
    (hover.code, hover.doc)
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    assert_eq!(Analysis::new(&program(true, "show(0)")).diagnostics(), Vec::new());
}

#[test]
fn the_length_of_text_is_an_int_whether_or_not_the_file_checks() {
    for checks in [true, false] {
        let source = program(checks, "show(0)");
        let when = if checks { "checked" } else { "declared" };
        assert_eq!(hover(&source, "count =").0, "bind count: int", "{}", when);
        assert_eq!(hover(&source, "inner =").0, "bind inner: int", "{}", when);
        assert_eq!(hover(&source, "read =").0, "bind read: int", "{}", when);
    }
    let source = program(true, "show(0)");
    let len = source.find("name.len").unwrap() + "name.".len();
    let hover = Analysis::new(&source).hover(LineIndex::new(&source).utf16_of(len)).unwrap();
    assert_eq!(hover.code, "len: int");
    assert_eq!(hover.doc, "The number of items of an array or slice, or the number of bytes of text.");
}

#[test]
fn a_part_of_text_is_text_where_the_text_is_whether_or_not_the_file_checks() {
    for checks in [true, false] {
        let source = program(checks, "show(0)");
        let when = if checks { "checked" } else { "declared" };
        assert_eq!(hover(&source, "part =").0, "bind part: text", "{}", when);
        assert_eq!(hover(&source, "kept =").0, "bind kept: text@r", "{}", when);
        assert_eq!(hover(&source, "inside =").0, "bind inside: text", "{}", when);
    }
    // A comparison's type comes from the checker.
    assert_eq!(hover(&program(true, "show(0)"), "before =").0, "bind before: bool");
}

#[test]
fn text_completes_its_length_and_its_copy() {
    for checks in [true, false] {
        let when = if checks { "checked" } else { "declared" };
        for value in ["name", "person.name", "line"] {
            let items = completions(&program(checks, &format!("show({}.@@)", value)));
            let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
            assert_eq!(labels, ["len", "copy"], "{} {}", value, when);
            assert_eq!(items[0].detail, "int, the number of bytes", "{} {}", value, when);
        }
    }
}

#[test]
fn a_misspelled_length_says_what_text_has() {
    let source = "task main()\n    bind name = \"Ada\"\n    show(name.size)\n    show(name.len())\nfi\n";
    let messages: Vec<String> = Analysis::new(source).diagnostics().into_iter().map(|d| d.message).collect();
    assert_eq!(messages, ["type text has no field 'size'; how many bytes it has is '.len'"]);
}

#[test]
fn joined_text_is_text_of_its_region_whether_or_not_the_file_checks() {
    for checks in [true, false] {
        let source = program(checks, "show(0)");
        let when = if checks { "checked" } else { "declared" };
        assert_eq!(hover(&source, "joined =").0, "bind joined: text@r", "{}", when);
        assert_eq!(hover(&source, "wrapped =").0, "bind wrapped: text@r", "{}", when);
        assert_eq!(hover(&source, "sum =").0, "bind sum: int", "{}", when);
    }
}

#[test]
fn joining_without_a_region_says_how_to_name_one() {
    let source = "task main()\n    bind name = \"Ada\" + \"!\"\n    show(name)\nfi\n";
    let messages: Vec<String> = Analysis::new(source).diagnostics().into_iter().map(|d| d.message).collect();
    assert_eq!(
        messages,
        ["joining text makes new text, which goes in a region; name it after the last part, as in 'a + b at r', or after parentheses, as in '(a + b)@r'"]
    );
}
