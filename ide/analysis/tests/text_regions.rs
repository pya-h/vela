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
    fixed boxed = new text(name) at r
    <- boxed^
fi

task longer(a: text@r, b: text@r) -> text@r
    <- a
fi

task main()
    CHECK
    region r
    fixed kept = named(r, \"Ada\")
    fixed lasting = \"Bob\"
    fixed plain = Person { name: lasting, age: 3 }
    fixed held = Person { name: kept, age: 36 }
    fixed names = new [text]([lasting#2]) at r
    fixed first = names[0]
    fixed node = new Node(Node { label: lasting, next: none }) at r
    fixed label = node^.label
    fixed either = longer(\"a\", \"b\")
    fixed both = [lasting, kept]
    show(kept)
fi
";

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
        assert_eq!(hover_code(&source, "kept ="), "fixed kept: text@r", "{}", when);
        assert_eq!(hover_code(&source, "lasting ="), "fixed lasting: text", "{}", when);
        assert_eq!(hover_code(&source, "plain ="), "fixed plain: Person", "{}", when);
        assert_eq!(hover_code(&source, "held ="), "fixed held: Person@r", "{}", when);
        assert_eq!(hover_code(&source, "first ="), "fixed first: text@r", "{}", when);
        assert_eq!(hover_code(&source, "label ="), "fixed label: text@r", "{}", when);
        assert_eq!(hover_code(&source, "either ="), "fixed either: text", "{}", when);
        assert_eq!(hover_code(&source, "both ="), "fixed both: [text@r#2]", "{}", when);
    }
}

#[test]
fn a_region_mismatch_of_text_is_reported_where_the_value_is() {
    let source = program(true).replace("    show(kept)\n", "    flex only: text = lasting\n    only = kept\n");
    let diagnostics = Analysis::new(&source).diagnostics();
    let [diagnostic] = diagnostics.as_slice() else { panic!("{:?}", diagnostics) };
    assert!(diagnostic.message.starts_with("expected type text, got text@r; this text is in region 'r'"), "{}", diagnostic.message);
}
