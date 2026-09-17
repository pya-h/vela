//! Text in regions in the editor: `text@r`, and shapes and kinds holding text
//! that name a region, with the compiler's types and with the types read from
//! the declarations while the file doesn't check. In the sources below, `@@`
//! marks the cursor.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, LineIndex};

const PROGRAM: &str = "\
shape Person
    name: text
    age: int
fi

shape Node
    label: text
    next: maybe ^Node
fi

task named(r: region, name: text) -> text@r
    bind boxed = new text(name) at r
    <- boxed^
fi

task main()
    CHECK
    region r
    bind kept = named(r, \"Ada\")
    bind lasting = \"Bob\"
    bind plain = Person { name: lasting, age: 3 }
    bind held = Person { name: kept, age: 36 }
    bind names = new [text]([lasting; 2]) at r
    bind first = names[0]
    bind node = new Node(Node { label: lasting, next: none }) at r
    bind label = node^.label
    show(kept)
fi
";

/// `PROGRAM` with a first line in `main` that either checks or stops the
/// checker before anything else in `main`.
fn program(checks: bool) -> String {
    PROGRAM.replace("CHECK", if checks { "show(0)" } else { "show(unknown)" })
}

fn hover_code(source: &str, needle: &str) -> String {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    Analysis::new(source).hover(LineIndex::new(source).utf16_of(byte)).unwrap_or_else(|| panic!("no hover at {:?}", needle)).code
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    assert_eq!(Analysis::new(&program(true)).diagnostics(), Vec::new());
}

#[test]
fn hover_shows_text_and_shapes_in_regions_whether_or_not_the_file_checks() {
    for checks in [true, false] {
        let source = program(checks);
        let when = if checks { "checked" } else { "declared" };
        assert_eq!(hover_code(&source, "kept ="), "bind kept: text@r", "{}", when);
        assert_eq!(hover_code(&source, "lasting ="), "bind lasting: text", "{}", when);
        assert_eq!(hover_code(&source, "plain ="), "bind plain: Person", "{}", when);
        assert_eq!(hover_code(&source, "held ="), "bind held: Person@r", "{}", when);
        assert_eq!(hover_code(&source, "first ="), "bind first: text@r", "{}", when);
        assert_eq!(hover_code(&source, "label ="), "bind label: text@r", "{}", when);
    }
}

#[test]
fn a_region_mismatch_of_text_is_reported_where_the_value_is() {
    let source = program(true).replace("    show(kept)\n", "    hold only: text = lasting\n    only = kept\n");
    let diagnostics = Analysis::new(&source).diagnostics();
    let [diagnostic] = diagnostics.as_slice() else { panic!("{:?}", diagnostics) };
    assert!(diagnostic.message.starts_with("expected type text, got text@r; this text is in region 'r'"), "{}", diagnostic.message);
}
