//! The new spellings of the words for variables, `fixed` and `flex`, which
//! replace `bind` and `hold` (Part 22): until the old ones go, Studio reads a
//! program written with either the same way. The words are keywords, and a
//! parameter list, a task's type, and the list of what a lambda keeps, which
//! Studio also reads as text, mean what they mean with the old spellings,
//! whether the file checks or not. `@@` marks the cursor.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, LineIndex, TokenClass};

/// Written with the old spellings; `new_spellings` gives the same program with
/// the new ones.
const PROGRAM: &str = "\
task bump(hold& n: int)
    n = n + 1
fi

task total(bind& p: int) -> int
    <- p
fi

task countdown(hold n: int, step: int) -> int
    loop n > 0
        n = n - step
    fi
    <- n
fi

task tick_twice(hold& f: [hold @r]() -> int) -> int
    <- f() + f()
fi

task main()
    CHECK
    bind bumped = bump
    bind totalled = total
    hold count = 0
    bind big = [1, 2, 3]
    region r
    bind add = [hold& count, bind& big, @r](n: int) -> nothing count = count + n + big[0] fi
    hold seen = 0
    hold next = [hold @r]() -> int
        seen = seen + 1
        <- seen
    fi
    bind g: [@r](hold& int) -> nothing = [@r](hold& j: int) -> nothing j = j + 1 fi
    add(2)
    g(&count)
    show(bumped, totalled, countdown(count, 1), tick_twice(&next))
fi
";

/// The same program, written with `flex` and `fixed`.
fn new_spellings(source: &str) -> String {
    source.replace("hold", "flex").replace("bind", "fixed")
}

/// The program as checked, as declared (a name it doesn't know), and as
/// unparsed (a header left open at the end).
fn sources(program: &str) -> Vec<(&'static str, String)> {
    let checked = program.replace("CHECK", "show(0)");
    let declared = program.replace("CHECK", "show(unknown)");
    let unparsed = format!("{}task h(\n", declared);
    vec![("checked", checked), ("declared", declared), ("unparsed", unparsed)]
}

fn hover(source: &str, needle: &str) -> Option<String> {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    Analysis::new(source).hover(LineIndex::new(source).utf16_of(byte)).map(|hover| hover.code)
}

fn class_at(source: &str, needle: &str) -> Option<TokenClass> {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    Analysis::new(source).tokens().into_iter().find(|t| t.from <= byte && byte < t.to).map(|t| t.class)
}

#[test]
fn a_program_written_with_the_new_spellings_checks() {
    let source = new_spellings(PROGRAM).replace("CHECK", "show(0)");
    assert_eq!(Analysis::new(&source).diagnostics(), Vec::new());
}

#[test]
fn the_new_spellings_are_keywords() {
    let source = new_spellings(PROGRAM).replace("CHECK", "show(0)");
    for needle in ["flex& n", "fixed& p", "flex n", "fixed bumped", "flex count", "flex& count", "fixed& big", "flex @r]() -> int\n", "flex& int"] {
        assert_eq!(class_at(&source, needle), Some(TokenClass::Keyword), "{:?}", needle);
    }
}

#[test]
fn a_hover_reads_the_new_spellings_as_the_old_ones() {
    let needles = [
        "n: int)",          // a place, flex& n
        "p: int)",          // a place only read, fixed& p
        "n: int, step",     // the task's own copy, flex n
        "f: [",             // a place holding a lambda that changes its copies
        "bumped =",         // a task's type with a place, (flex& int) -> nothing
        "totalled =",       // and one only read, (fixed& int) -> int
        "count, ",          // a place a lambda keeps, [flex& count, …]
        "big, @r",          // and one it reads, [… fixed& big, …]
        "add =",            // the lambda keeping them
        "next =",           // a lambda that changes its copies, [flex @r]
        "g:",               // a lambda's type with a place
        "j: int",           // a lambda's own parameter that is a place
    ];
    for (when, old) in sources(PROGRAM) {
        let new = new_spellings(&old);
        for needle in needles {
            // The needles are the same in both spellings.
            assert_eq!(hover(&new, needle), hover(&old, needle), "{}: {:?}", when, needle);
            assert!(hover(&old, needle).is_some(), "{}: {:?} has no hover", when, needle);
        }
    }
}

#[test]
fn a_type_that_never_changes_is_asked_for_with_frozen() {
    let marked = "shape Box<T: @@>\n    value: T\nfi\n";
    let at = marked.find("@@").unwrap();
    let source = marked.replacen("@@", "", 1);
    let labels: Vec<String> = Analysis::new(&source).complete(LineIndex::new(&source).utf16_of(at)).items.into_iter().map(|item| item.label).collect();
    assert_eq!(labels, ["frozen", "same", "similar", "shown", "ordered"]);
    let source = "shape Box<T: frozen>\n    value: T\nfi\n";
    assert_eq!(class_at(source, "frozen>"), Some(TokenClass::Keyword));
    // 'fixed' is the word for a variable now, so a type can't be asked for it.
    let wrong = "shape Box<T: fixed>\n    value: T\nfi\n";
    let diagnostics = Analysis::new(wrong).diagnostics();
    assert_eq!(diagnostics.len(), 1, "{:?}", diagnostics);
    assert_eq!(diagnostics[0].message, "expected what the type must allow: 'frozen', 'same', 'shown', 'ordered'");
}
