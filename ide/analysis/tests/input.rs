//! Reading input in the editor: `scan(prompt) at r`, with the compiler's types
//! and with the types read from the source while the file doesn't check. In the
//! sources below, `@@` marks the cursor.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, CompletionItem, LineIndex};

const PROGRAM: &str = "\
task main()
    CHECK
    region lines
    bind asked = scan(\"Name: \") at lines
    bind short = scan(\"\")@lines
    bind name = scan(\"Name: \") at lines { \"nobody\" }
    show(name)
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
    assert_eq!(Analysis::new(&program(true, "show(1)")).diagnostics(), Vec::new());
}

#[test]
fn a_line_read_is_a_maybe_text_in_its_region_whether_or_not_the_file_checks() {
    for checks in [true, false] {
        let source = program(checks, "show(1)");
        let when = if checks { "checked" } else { "declared" };
        assert_eq!(hover(&source, "asked =").0, "bind asked: maybe text@lines", "{}", when);
        assert_eq!(hover(&source, "short =").0, "bind short: maybe text@lines", "{}", when);
        assert_eq!(hover(&source, "name =").0, "bind name: text@lines", "{}", when);
    }
}

#[test]
fn scan_has_a_hover_and_a_snippet_that_names_the_region() {
    let source = program(true, "show(1)");
    let (code, doc) = hover(&source, "scan(");
    assert_eq!(code, "task scan(prompt: text) at r -> maybe text@r");
    assert!(doc.starts_with("Shows the prompt, then reads one line"), "{}", doc);
    let items = completions(&program(true, "bind again = sc@@"));
    let scan = items.iter().find(|item| item.label == "scan").expect("scan is offered");
    assert_eq!(scan.snippet.as_deref(), Some("scan(${prompt}) at ${region}"));
}

#[test]
fn completion_continues_a_scan_with_its_region() {
    for checks in [true, false] {
        assert!(labels(&program(checks, "bind again = scan(\"x\") @@")).contains(&"at".to_string()), "checks: {}", checks);
        assert!(labels(&program(checks, "bind again = scan(\"x\") at @@")).contains(&"lines".to_string()), "checks: {}", checks);
        assert!(labels(&program(checks, "bind again = scan(\"x\")@ @@")).contains(&"lines".to_string()), "checks: {}", checks);
    }
}

#[test]
fn at_is_offered_only_where_a_region_can_follow() {
    for checks in [true, false] {
        let offered = |code: &str| labels(&program(checks, code)).contains(&"at".to_string());
        assert!(offered("bind p = new int(1) @@"), "checks: {}", checks);
        assert!(offered("bind p = new [int]([1, 2]) @@"), "checks: {}", checks);
        assert!(!offered("show(scan(\"x\")@lines) @@"), "checks: {}", checks);
        assert!(!offered("bind same = scan(\"x\")@lines == scan(\"y\")@lines @@"), "checks: {}", checks);
        assert!(!offered("bind p = new@lines int(1) @@"), "checks: {}", checks);
        assert!(!offered("show(named(1)) @@"), "checks: {}", checks);
    }
}

#[test]
fn scan_called_is_the_builtin_even_beside_a_variable_named_scan() {
    let source = program(true, "hold scan = 3\n    show(scan)\n    bind again = scan(\"x\") at lines");
    let byte = source.rfind("scan(").expect("a call");
    let hover = Analysis::new(&source).hover(LineIndex::new(&source).utf16_of(byte)).expect("a hover");
    assert_eq!(hover.code, "task scan(prompt: text) at r -> maybe text@r");
}

#[test]
fn a_scan_without_a_region_is_reported() {
    let source = program(true, "bind lost = scan(\"x\")");
    let diagnostics = Analysis::new(&source).diagnostics();
    let [diagnostic] = diagnostics.as_slice() else { panic!("{:?}", diagnostics) };
    assert!(diagnostic.message.starts_with("'scan' keeps the line it reads in a region"), "{}", diagnostic.message);
}
