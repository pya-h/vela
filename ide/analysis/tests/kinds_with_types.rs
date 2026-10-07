//! Kinds that take types, `kind Option<T>`: the types they take, the values
//! of the kinds made for them, named as the program writes them, and the
//! variants given their types, whether or not the file checks.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, LineIndex, SymbolKind, TokenClass};

const PROGRAM: &str = "\
kind Option<T: shown>
    Some(value: T)
    None

    isSome() -> bool
        <- match .this
        case Some _ = true
        case None = false
        fi
    fi
fi

kind Either<A, B>
    Left(a: A)
    Right(b: B)
fi

task main()
    bind some = Option.Some(1)
    bind empty: Option<text> = Option.None
    bind written = Option<float>.None
    bind either = Either<int, text>.Right(\"r\")
    match some
    case Some v
        show(v)
    case None
        show(0)
    fi
    CHECK
fi
";

/// The program as it checks, as it parses but doesn't check, and as it
/// doesn't parse.
const WHEN: [&str; 3] = ["checked", "declared", "unparsed"];

fn program(when: &str) -> String {
    let source = PROGRAM.replace("CHECK", if when == "checked" { "show(some.isSome())" } else { "show(unknown)" });
    if when == "unparsed" { source + "task g(\n" } else { source }
}

fn hover(source: &str, needle: &str) -> String {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    Analysis::new(source).hover(LineIndex::new(source).utf16_of(byte)).unwrap_or_else(|| panic!("no hover at {:?}", needle)).code
}

fn class(source: &str, needle: &str) -> TokenClass {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    let at = LineIndex::new(source).utf16_of(byte);
    Analysis::new(source).tokens().into_iter().find(|token| token.from <= at && at < token.to).unwrap_or_else(|| panic!("no token at {:?}", needle)).class
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    assert_eq!(Analysis::new(&program("checked")).diagnostics(), Vec::new());
}

#[test]
fn a_value_of_a_kind_given_types_hovers_as_the_program_writes_it() {
    let source = program("checked");
    assert_eq!(hover(&source, "some ="), "bind some: Option<int>");
    assert_eq!(hover(&source, "empty:"), "bind empty: Option<text>");
    assert_eq!(hover(&source, "written ="), "bind written: Option<float>");
    assert_eq!(hover(&source, "either ="), "bind either: Either<int, text>");
    assert_eq!(hover(&source, "v\n"), "v: int");
}

#[test]
fn a_written_type_reads_the_same_whether_or_not_the_file_checks() {
    for when in WHEN {
        let source = program(when);
        assert_eq!(hover(&source, "empty:"), "bind empty: Option<text>", "{}", when);
    }
}

#[test]
fn a_kind_and_the_types_it_takes_are_outlined_together() {
    let outline = Analysis::new(&program("checked")).outline();
    let option = outline.first().expect("the first kind");
    assert_eq!((option.name.as_str(), option.kind), ("Option", SymbolKind::Kind));
    let members: Vec<(&str, SymbolKind)> = option.children.iter().map(|c| (c.name.as_str(), c.kind)).collect();
    assert_eq!(members, [("T", SymbolKind::TypeParam), ("Some", SymbolKind::Variant), ("None", SymbolKind::Variant), ("isSome", SymbolKind::Method)]);
}

#[test]
fn the_types_a_kind_takes_and_is_given_are_colored_as_types() {
    for when in WHEN {
        let source = program(when);
        assert_eq!(class(&source, "T: shown"), TokenClass::Type, "{}", when);
        assert_eq!(class(&source, "T)"), TokenClass::Type, "{}", when);
        assert_eq!(class(&source, "float>.None"), TokenClass::TypeBuiltin, "{}", when);
        assert_eq!(class(&source, "Option<float>"), TokenClass::Type, "{}", when);
        assert_eq!(class(&source, "None\n    bind either"), TokenClass::Variant, "{}", when);
        assert_eq!(class(&source, "Right("), TokenClass::Variant, "{}", when);
    }
}

#[test]
fn a_variant_whose_types_nothing_tells_is_reported() {
    let source = program("checked").replace("show(some.isSome())", "bind lost = Option.None");
    let diagnostics = Analysis::new(&source).diagnostics();
    let [diagnostic] = diagnostics.as_slice() else { panic!("{:?}", diagnostics) };
    assert!(diagnostic.message.starts_with("cannot infer the type of 'lost'"), "{}", diagnostic.message);
}
