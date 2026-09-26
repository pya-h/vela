//! A region written after a call in the editor: `f(x) at r` and `f(x)@r` give
//! a task or a method the region it takes last, as `x.str() at r` always did,
//! with the compiler's types and with the types read from the source while the
//! file doesn't check. In the sources below, `@@` marks the cursor.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, CompletionItem, LineIndex};

const PROGRAM: &str = "\
kind Why
    Empty
fi

shape Tag
    name: text
fi

task label(n: int, into: region) -> text@into
    <- \"#{n}\" at into
fi

task tagged(name: text, into: region) -> Tag@into
    <- Tag { name: \"{name}\" at into }
fi

task checked(t: text, into: region) -> text@into ~ Why
    when t.len == 0
        <- fail Empty
    fi
    <- \"[{t}]\" at into
fi

shape Counter
    n: int

    shown(into: region) -> text@into
        <- \"{.n}\" at into
    fi
fi

task main()
    CHECK
    region work
    bind after = label(1) at work
    bind tight = label(2)@work
    bind inside = label(3, work)
    bind tag = tagged(\"x\") at work
    bind planned = checked(\"ok\") at work { \"none\" }
    bind counter = Counter { n: 4 }
    bind method = counter.shown() at work
    bind chained = \"a\" + label(5)@work at work
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

fn hover(source: &str, needle: &str) -> String {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    Analysis::new(source).hover(LineIndex::new(source).utf16_of(byte)).unwrap_or_else(|| panic!("no hover at {:?}", needle)).code
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    assert_eq!(Analysis::new(&program(true, "show(after)")).diagnostics(), Vec::new());
}

#[test]
fn a_region_after_a_call_is_the_one_it_takes_last_whether_or_not_the_file_checks() {
    for checks in [true, false] {
        let source = program(checks, "show(after)");
        let when = if checks { "checked" } else { "declared" };
        assert_eq!(hover(&source, "after ="), "bind after: text@work", "{}", when);
        assert_eq!(hover(&source, "tight ="), "bind tight: text@work", "{}", when);
        assert_eq!(hover(&source, "inside ="), "bind inside: text@work", "{}", when);
        // A result that is neither text nor a pointer keeps its own type.
        assert_eq!(hover(&source, "tag ="), "bind tag: Tag@work", "{}", when);
        assert_eq!(hover(&source, "planned ="), "bind planned: text@work", "{}", when);
        assert_eq!(hover(&source, "method ="), "bind method: text@work", "{}", when);
        assert_eq!(hover(&source, "chained ="), "bind chained: text@work", "{}", when);
    }
}

#[test]
fn the_region_after_a_call_completes_as_a_region() {
    for checks in [true, false] {
        let labels: Vec<String> = completions(&program(checks, "bind more = label(6) at @@")).into_iter().map(|item| item.label).collect();
        assert!(labels.contains(&"work".to_string()), "checks: {}: {:?}", checks, labels);
    }
}

#[test]
fn an_at_that_is_not_last_in_a_chain_is_reported() {
    let source = program(true, "show(label(7) at work + \"x\")");
    let diagnostics = Analysis::new(&source).diagnostics();
    let [diagnostic] = diagnostics.as_slice() else { panic!("{:?}", diagnostics) };
    assert!(diagnostic.message.starts_with("'at r' names the region of everything before it, so it comes after the last part"), "{}", diagnostic.message);
}

#[test]
fn a_region_given_twice_is_reported() {
    let source = program(true, "show(\"x\".len)\n    bind n = checked(\"\", work) at work { \"\" }");
    let diagnostics = Analysis::new(&source).diagnostics();
    let [diagnostic] = diagnostics.as_slice() else { panic!("{:?}", diagnostics) };
    assert_eq!(diagnostic.message, "task 'checked' is given its region in the parentheses already, so this is a second one; name it once");
}
