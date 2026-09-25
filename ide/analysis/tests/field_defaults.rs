//! A field that names the value a literal leaving it out gives it
//! (`size: int = 0`) in the editor: the field's type is the type alone, a
//! literal completes only what it must give, and both hold while the file
//! doesn't check. In the sources below, `@@` marks the cursor.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, CompletionItem, LineIndex};

const PROGRAM: &str = "\
kind State
    Idle
    Busy
fi

shape Job
    name: text
    tries: int = 0
    note: maybe text = none
    state: State = State.Idle
fi

task main()
    CHECK
    hold job = Job { name: \"sweep\" }
    bind tries = job.tries
    bind note = job.note
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
    assert_eq!(Analysis::new(&program(true, "show(tries)")).diagnostics(), Vec::new());
}

#[test]
fn a_field_that_falls_back_on_a_value_has_its_type_alone() {
    for checks in [true, false] {
        let source = program(checks, "show(tries)");
        let when = if checks { "checked" } else { "declared" };
        // The value the field falls back on is no part of its type.
        assert_eq!(hover(&source, "tries: int =").0, "tries: int", "{}", when);
        assert_eq!(hover(&source, "note: maybe").0, "note: maybe text", "{}", when);
        assert_eq!(hover(&source, "state: State =").0, "state: State", "{}", when);
        // And a name bound to one reads the same type, left out or not.
        assert_eq!(hover(&source, "tries = job").0, "bind tries: int", "{}", when);
        assert_eq!(hover(&source, "note = job").0, "bind note: maybe text", "{}", when);
    }
}

#[test]
fn a_literal_completes_the_fields_it_must_give() {
    for checks in [true, false] {
        // The whole literal offers only the field with no value to fall back on.
        let items = completions(&program(checks, "hold other = Jo@@"));
        let job = items.iter().find(|item| item.label == "Job").unwrap_or_else(|| panic!("no Job: {:?}", items));
        assert_eq!(job.snippet.as_deref(), Some("Job { name: ${name} }"), "checks: {}", checks);
        // Inside one, every field not given yet is still offered, since a
        // literal may write what it would otherwise fall back on.
        let inside = completions(&program(checks, "hold other = Job { @@ }"));
        let offered: Vec<(&str, &str)> = inside.iter().map(|item| (item.label.as_str(), item.detail.as_str())).collect();
        assert_eq!(offered, [("name", "text"), ("tries", "int"), ("note", "maybe text"), ("state", "State")], "checks: {}", checks);
    }
}

#[test]
fn a_shape_whose_every_field_falls_back_completes_as_an_empty_literal() {
    let source = "\
shape Counters
    hits: int = 0
    misses: int = 0
fi

task main()
    bind counted = Coun@@
fi
";
    let items = completions(source);
    let counters = items.iter().find(|item| item.label == "Counters").unwrap_or_else(|| panic!("no Counters: {:?}", items));
    assert_eq!(counters.snippet.as_deref(), Some("Counters {}"));
}

#[test]
fn a_shape_whose_value_builds_itself_is_reported_rather_than_filled_forever() {
    // A literal's left-out fields are filled in to check it, so a shape holding
    // itself would fill itself without end. The editor must survive it.
    let source = "\
shape Loop
    inner: Loop = Loop {}
fi

task main()
    show(1)
fi
";
    let diagnostics = Analysis::new(source).diagnostics();
    let [diagnostic] = diagnostics.as_slice() else { panic!("{:?}", diagnostics) };
    assert!(diagnostic.message.starts_with("'Loop' contains itself by value"), "{}", diagnostic.message);
}

#[test]
fn a_value_the_field_cannot_fall_back_on_is_reported_where_it_is_written() {
    let source = program(true, "show(tries)").replace("tries: int = 0", "tries: int = \"none\"");
    let diagnostics = Analysis::new(&source).diagnostics();
    let [diagnostic] = diagnostics.as_slice() else { panic!("{:?}", diagnostics) };
    assert!(diagnostic.message.starts_with("expected type int, got text"), "{}", diagnostic.message);
}
