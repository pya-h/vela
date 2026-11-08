#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, CompletionItem, LineIndex, TokenClass};

const PROGRAM: &str = "\
kind StepError
    TooBig(n: int)
fi

shape Till
    total: int

    plus(n: int) -> int
        <- .total + n
    fi
fi

task double(n: int) -> int
    <- n * 2
fi

task scale(by: int, n: int) -> int
    <- n * by
fi

task checked(n: int) -> int ~ StepError
    when n > 100
        <- fail TooBig(n)
    fi
    <- n
fi

task main()
    fixed till = Till { total: 5 }
    fixed a = 3 |> double() |>> scale(10)
    fixed b = 60
        |> double()
        |> checked() { 0 }
    show(a, b, 4 |> till.plus())
    @@
fi
";

fn program(code: &str) -> String {
    PROGRAM.replace("    @@\n", &format!("    {}\n", code))
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

fn class_at(source: &str, needle: &str) -> Option<TokenClass> {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    Analysis::new(source).tokens().into_iter().find(|t| t.from <= byte && byte < t.to).map(|t| t.class)
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    let source = program("show(1)");
    assert_eq!(Analysis::new(&source).diagnostics(), Vec::new());
}

#[test]
fn pipes_and_their_steps_are_colored() {
    let source = program("show(1)");
    assert_eq!(class_at(&source, "|> double() |>>"), Some(TokenClass::Operator));
    assert_eq!(class_at(&source, "|>> scale"), Some(TokenClass::Operator));
    assert_eq!(class_at(&source, "double() |>>"), Some(TokenClass::Task));
    assert_eq!(class_at(&source, "scale(10)"), Some(TokenClass::Task));
    assert_eq!(class_at(&source, "|> double()\n"), Some(TokenClass::Operator));
    assert_eq!(class_at(&source, "checked() { 0 }"), Some(TokenClass::Task));
    assert_eq!(class_at(&source, "plus())"), Some(TokenClass::Method));
    assert_eq!(class_at(&source, "till.plus"), Some(TokenClass::Variable));
}

#[test]
fn a_step_hovers_as_the_task_it_calls() {
    let source = program("show(1)");
    let (code, _) = hover(&source, "scale(10)");
    assert!(code.starts_with("task scale(by: int, n: int) -> int"), "{}", code);
    let (code, _) = hover(&source, "plus())");
    assert!(code.contains("plus(n: int) -> int"), "{}", code);
}

#[test]
fn tasks_are_offered_after_a_pipe() {
    let labels: Vec<String> = completions(&program("show(2 |> do@@)")).into_iter().map(|item| item.label).collect();
    assert!(labels.contains(&"double".to_string()), "{:?}", labels);
    let labels: Vec<String> = completions(&program("show(2 |> till.@@)")).into_iter().map(|item| item.label).collect();
    assert!(labels.contains(&"plus".to_string()), "{:?}", labels);
}

#[test]
fn a_mistake_in_a_pipe_is_shown_in_the_editor() {
    let source = program("show(2 |>>> scale(10))");
    let diagnostics = Analysis::new(&source).diagnostics();
    assert_eq!(diagnostics.len(), 1, "{:?}", diagnostics);
    assert_eq!(
        diagnostics[0].message,
        "'|>>>' gives the value before it to 'scale' as argument 3, but the call writes only 1, so argument 2 would be missing; write the arguments before it, or fewer '>'"
    );
    let source = program("2 |> checked()");
    let diagnostics = Analysis::new(&source).diagnostics();
    assert_eq!(diagnostics.len(), 1, "{:?}", diagnostics);
    assert!(diagnostics[0].message.starts_with("the error this pipe may fail with is ignored"), "{}", diagnostics[0].message);
}
