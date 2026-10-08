//! Fields marked `read` in the editor (`read path: text`): the field is still a
//! field, with the word before it colored as one, hover says who assigns it, a
//! literal completes only what it may give, and all of it holds while the file
//! doesn't check. In the sources below, `@@` marks the cursor.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, CompletionItem, LineIndex, TokenClass};

const PROGRAM: &str = "\
shape Meter
    read label: text
    read count: int = 0
    note: text = \"\"

    alters tick()
        .count = .count + 1
    fi
fi

task main()
    CHECK
    flex meter = Meter { label: \"door\" }
    meter.tick()
    fixed counted = meter.count
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

fn hover(source: &str, needle: &str) -> (String, String) {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    let hover = Analysis::new(source).hover(LineIndex::new(source).utf16_of(byte)).unwrap_or_else(|| panic!("no hover at {:?}", needle));
    (hover.code, hover.doc)
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    assert_eq!(Analysis::new(&program(true, "show(counted)")).diagnostics(), Vec::new());
}

#[test]
fn a_read_field_is_a_field_whose_hover_says_who_assigns_it() {
    for checks in [true, false] {
        let source = program(checks, "show(counted)");
        let when = if checks { "checked" } else { "declared" };
        let (code, doc) = hover(&source, "label: text");
        assert_eq!(code, "read label: text", "{}", when);
        assert!(doc.contains("only the alters methods of its shape change it"), "{}: {}", when, doc);
        assert_eq!(hover(&source, "count: int =").0, "read count: int", "{}", when);
        // An ordinary field beside them says nothing of the kind.
        assert_eq!(hover(&source, "note: text").0, "note: text", "{}", when);
        assert_eq!(hover(&source, "counted =").0, "bind counted: int", "{}", when);
        // The shape lists its members with their markers.
        let shape = hover(&source, "Meter\n").0;
        assert!(shape.contains("    read label: text\n") && shape.contains("    read count: int\n"), "{}: {}", when, shape);
    }
}

#[test]
fn the_word_is_colored_as_a_keyword_only_where_it_marks_a_field() {
    let source = "\
shape Book
    read title: text
    read: int = 0

    read_it() -> int
        <- .read
    fi
fi

task read(n: int) -> int
    <- n
fi

task main()
    fixed b = Book { title: \"x\" }
    show(read(b.read))
fi
";
    assert_eq!(Analysis::new(source).diagnostics(), Vec::new());
    let analysis = Analysis::new(source);
    let class_at = |needle: &str| {
        let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
        analysis.tokens().into_iter().find(|t| t.from <= byte && byte < t.to).map(|t| t.class)
    };
    assert_eq!(class_at("read title"), Some(TokenClass::Keyword));
    assert_eq!(class_at("title: text"), Some(TokenClass::Field));
    // A field, and a task, that are called `read`.
    assert_eq!(class_at("read: int"), Some(TokenClass::Field));
    assert_eq!(class_at("read(n"), Some(TokenClass::Task));
}

#[test]
fn a_literal_completes_what_it_may_give() {
    for checks in [true, false] {
        let items = completions(&program(checks, "flex other = Me@@"));
        let meter = items.iter().find(|item| item.label == "Meter").unwrap_or_else(|| panic!("no Meter: {:?}", items));
        assert_eq!(meter.snippet.as_deref(), Some("Meter { label: ${label} }"), "checks: {}", checks);
        // Inside one, a read field with no value to fall back on is offered, and
        // one that falls back on a value is given by no literal.
        let inside = completions(&program(checks, "flex other = Meter { @@ }"));
        let offered: Vec<&str> = inside.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(offered, ["label", "note"], "checks: {}", checks);
    }
}

#[test]
fn assigning_a_read_field_outside_its_methods_is_reported() {
    let source = program(true, "meter.count = 5");
    let diagnostics = Analysis::new(&source).diagnostics();
    let [diagnostic] = diagnostics.as_slice() else { panic!("{:?}", diagnostics) };
    assert_eq!(diagnostic.message, "'count' is a read-only field of 'Meter', which only the alters methods of 'Meter' change");
}
