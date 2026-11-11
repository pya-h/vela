#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, LineIndex, TokenClass};

const PROGRAM: &str = "\
task bump(flex& n: int)
    n = n + 1
fi

task total(fixed& p: int) -> int
    <- p
fi

task countdown(flex n: int, step: int) -> int
    loop n > 0
        n = n - step
    fi
    <- n
fi

task tick_twice(flex& f: [flex @r]() -> int) -> int
    <- f() + f()
fi

task main()
    fixed bumped = bump
    fixed totalled = total
    flex count = 0
    fixed big = [1, 2, 3]
    region r
    fixed add = [flex& count, fixed& big, @r](n: int) -> nothing count = count + n + big[0] fi
    flex seen = 0
    flex next = [flex @r]() -> int
        seen = seen + 1
        <- seen
    fi
    fixed g: [@r](flex& int) -> nothing = [@r](flex& j: int) -> nothing j = j + 1 fi
    add(2)
    g(&count)
    show(bumped, totalled, countdown(count, 1), tick_twice(&next))
fi
";

fn class_at(source: &str, needle: &str) -> Option<TokenClass> {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    Analysis::new(source).tokens().into_iter().find(|t| t.from <= byte && byte < t.to).map(|t| t.class)
}

fn hover(source: &str, needle: &str) -> Option<String> {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    Analysis::new(source).hover(LineIndex::new(source).utf16_of(byte)).map(|hover| hover.code)
}

fn labels(marked: &str) -> Vec<String> {
    let at = marked.find("@@").unwrap();
    let source = marked.replacen("@@", "", 1);
    Analysis::new(&source).complete(LineIndex::new(&source).utf16_of(at)).items.into_iter().map(|item| item.label).collect()
}

fn only_error(source: &str) -> (usize, usize, String) {
    let diagnostics = Analysis::new(source).diagnostics();
    assert_eq!(diagnostics.len(), 1, "{:?}", diagnostics);
    let d = &diagnostics[0];
    (d.line.unwrap(), d.col.unwrap(), d.message.clone())
}

fn checks(source: &str) {
    assert_eq!(Analysis::new(source).diagnostics(), Vec::new(), "{:?}", source);
}

#[test]
fn a_program_written_with_the_words_checks() {
    assert_eq!(Analysis::new(PROGRAM).diagnostics(), Vec::new());
}

#[test]
fn the_words_are_keywords() {
    for needle in ["flex& n", "fixed& p", "flex n", "fixed bumped", "flex count", "flex& count", "fixed& big", "flex @r]() -> int\n", "flex& int"] {
        assert_eq!(class_at(PROGRAM, needle), Some(TokenClass::Keyword), "{:?}", needle);
    }
}

#[test]
fn a_hover_says_the_words() {
    for (needle, code) in [
        ("n: int)", "flex& n: int"),
        ("p: int)", "fixed& p: int"),
        ("n: int, step", "flex n: int"),
        ("bumped =", "fixed bumped: (flex& int) -> nothing"),
        ("totalled =", "fixed totalled: (fixed& int) -> int"),
        ("count = 0", "flex count: int"),
    ] {
        assert_eq!(hover(PROGRAM, needle).as_deref(), Some(code), "{:?}", needle);
    }
}

#[test]
fn the_old_words_are_keywords_still() {
    let source = "task main()\n    bind x = 1\n    hold y = 2\nfi\n";
    assert_eq!(class_at(source, "bind"), Some(TokenClass::Keyword));
    assert_eq!(class_at(source, "hold"), Some(TokenClass::Keyword));
}

#[test]
fn hold_is_a_variable_word_now() {
    checks("task main()\n    hold x = 1\nfi\n");
    checks("task main()\n    fixed t = (1, 2)\n    hold (a, b) = t\nfi\n");
    checks("task main()\n    fixed t = (1, 2)\n    hold(a, b) = t\nfi\n");
}

#[test]
fn bind_is_a_variable_word_now() {
    checks("task main()\n    bind x = 1\nfi\n");
    checks("task main()\n    fixed t = (1, 2)\n    bind (a, b) = t\nfi\n");
    checks("task main()\n    fixed t = (1, 2)\n    bind(a, b) = t\nfi\n");
}

#[test]
fn hold_written_as_a_name_is_an_error() {
    assert_eq!(
        only_error("task main()\n    fixed hold = 1\nfi\n"),
        (2, 11, "expected a name, got 'hold', which is a reserved word".to_string())
    );
    assert_eq!(
        only_error("task hold(n: int)\nfi\n"),
        (1, 6, "expected a name, got 'hold', which is a reserved word".to_string())
    );
}

#[test]
fn bind_written_as_a_name_is_an_error() {
    assert_eq!(
        only_error("task main()\n    fixed bind = 1\nfi\n"),
        (2, 11, "expected a name, got 'bind', which is a reserved word".to_string())
    );
}

#[test]
fn variable_words_are_offered() {
    let offered = labels("task main()\n    @@\nfi\n");
    assert!(offered.iter().any(|label| label == "fixed"));
    assert!(offered.iter().any(|label| label == "flex"));
    for marked in ["task main()\n    fixed x = 1\n    bind @@\nfi\n", "task main()\n    fixed x = 1\n    hold @@\nfi\n"] {
        assert!(labels(marked).is_empty(), "{:?}: {:?}", marked, labels(marked));
    }
}

#[test]
fn a_type_that_never_changes_is_asked_for_with_frozen() {
    assert_eq!(labels("shape Box<T: @@>\n    value: T\nfi\n"), ["frozen", "same", "similar", "shown", "ordered"]);
    let source = "shape Box<T: frozen>\n    value: T\nfi\n";
    assert_eq!(class_at(source, "frozen>"), Some(TokenClass::Keyword));
    let wrong = "shape Box<T: fixed>\n    value: T\nfi\n";
    let diagnostics = Analysis::new(wrong).diagnostics();
    assert_eq!(diagnostics.len(), 1, "{:?}", diagnostics);
    assert_eq!(diagnostics[0].message, "expected what the type must allow: 'frozen', 'same', 'similar', 'shown', 'ordered', or a pattern it follows");
}
