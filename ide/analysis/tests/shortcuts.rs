//! The short forms of slices (and maps) in the editor: `[1, 2, ...] at r` is
//! a slice in `r`, hovered with the compiler's types, and with the same types
//! read from the values while the file doesn't check. In the sources below,
//! `@@` marks the cursor.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, LineIndex};

const PROGRAM: &str = "\
task main()
    CHECK
    region r
    flex xs = [1, 2, ...] at r
    fixed names = [\"ada\", ...]@r
    fixed firsts = xs[0]
    region kept default
    fixed kept_values = [1.5, ...]
    @@
fi
";

fn program(checks: bool, code: &str) -> String {
    let first = if checks { "show(0)" } else { "show(unknown)" };
    PROGRAM.replace("CHECK", first).replace("    @@\n", &format!("    {}\n", code))
}

fn hover(source: &str, needle: &str) -> (String, String) {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    let hover = Analysis::new(source).hover(LineIndex::new(source).utf16_of(byte)).unwrap_or_else(|| panic!("no hover at {:?}", needle));
    (hover.code, hover.doc)
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    let source = program(true, "show(xs, names, firsts, kept_values)");
    assert_eq!(Analysis::new(&source).diagnostics(), Vec::new());
}

#[test]
fn a_slice_shortcut_hovers_as_a_slice_whether_or_not_the_file_checks() {
    for checks in [true, false] {
        let source = program(checks, "show(0)");
        let code = |needle: &str| hover(&source, needle).0;
        let when = if checks { "checked" } else { "declared" };
        assert_eq!(code("xs ="), "flex xs: [int]@r", "{}", when);
        assert_eq!(code("names ="), "fixed names: [text]@r", "{}", when);
        assert_eq!(code("firsts ="), "fixed firsts: int", "{}", when);
        assert_eq!(code("kept_values ="), "fixed kept_values: [float]@kept", "{}", when);
    }
}
