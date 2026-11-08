#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, LineIndex, TokenClass};

const PROGRAM: &str = "\
task log(f: (int) -> int)
    show(f(1))
fi

task tag(f: (int) -> int, name: text)
    show(name)
fi

$log $tag(\"sq\")
task sq(n: int) -> int
    <- n * n
fi

task main()
    CHECK
fi
";

fn program(checked: bool) -> String {
    PROGRAM.replace("CHECK", if checked { "show(sq(2))" } else { "show(unknown)" })
}

fn hover(source: &str, needle: &str) -> String {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    Analysis::new(source).hover(LineIndex::new(source).utf16_of(byte)).unwrap_or_else(|| panic!("no hover at {:?}", needle)).code
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    assert_eq!(Analysis::new(&program(true)).diagnostics(), Vec::new());
}

#[test]
fn a_decorator_names_its_task_and_its_mark_explains_itself() {
    for checked in [true, false] {
        let source = program(checked);
        assert_eq!(hover(&source, "log $"), "task log(f: (int) -> int)");
        assert_eq!(hover(&source, "tag(\"sq"), "task tag(f: (int) -> int, name: text)");
        assert_eq!(hover(&source, "$log"), "$d, $d(values), $d?, or $(w1, w2(values), .m)");
        let at = LineIndex::new(&source).utf16_of(source.find("$tag").unwrap());
        let class = Analysis::new(&source).tokens().into_iter().find(|t| t.from == at).map(|t| t.class);
        assert_eq!(class, Some(TokenClass::Keyword));
        let names: Vec<String> = Analysis::new(&source).outline().into_iter().map(|item| item.name).collect();
        assert_eq!(names, ["log", "tag", "sq", "main"]);
    }
}

#[test]
fn a_decorator_that_does_not_fit_is_reported_where_it_is_written() {
    let source = program(true).replace("$log $tag(\"sq\")", "$tag");
    let diagnostics = Analysis::new(&source).diagnostics();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!((diagnostics[0].line, diagnostics[0].col), (Some(9), Some(2)));
    assert!(diagnostics[0].message.starts_with("decorator 'tag' takes the task and 1 value"), "{}", diagnostics[0].message);
}

#[test]
fn a_wrapped_task_is_called_as_its_wrappers_make_it() {
    let source = "\
task worded(next: (int) -> int, n: int) -> text
    <- \"x\"
fi

$(worded)
task sq(n: int) -> int
    <- n * n
fi

task main()
    fixed said = sq(2)
    show(said)
fi
";
    assert_eq!(Analysis::new(source).diagnostics(), Vec::new());
    assert_eq!(hover(source, "said ="), "fixed said: text");
    assert_eq!(hover(source, "worded)"), "task worded(next: (int) -> int, n: int) -> text");
    let names: Vec<String> = Analysis::new(source).outline().into_iter().map(|item| item.name).collect();
    assert_eq!(names, ["worded", "sq", "main"]);
}

#[test]
fn a_wrapper_value_worked_out_at_start_hovers_as_written() {
    // The value is read where the program starts, from a hidden variable, and
    // the editor still answers about what the file writes.
    let source = "\
shape Config
    shared flex depth: int = 2
fi

task logged(next: (int) -> int, times: int, n: int) -> int
    <- next(n)
fi

$(logged(Config.depth))
task cube(n: int) -> int
    <- n * n * n
fi

task main()
    show(cube(3))
fi
";
    assert_eq!(Analysis::new(source).diagnostics(), Vec::new());
    assert_eq!(hover(source, "depth))"), "shared flex depth: int");
    let at = LineIndex::new(source).utf16_of(source.find("depth))").unwrap());
    let declared = LineIndex::new(source).utf16_of(source.find("depth:").unwrap());
    assert_eq!(Analysis::new(source).definition(at).map(|found| found.from), Some(declared));
}

#[test]
fn decorators_on_and_of_shared_methods_hover_as_written() {
    let source = "\
shape Registry
    shared read count: int = 0

    shared enroll(f: (int) -> int)
        Registry.count = Registry.count + 1
    fi
fi

shape Timer
    shared timed(next: (int) -> int, n: int) -> int
        <- next(n)
    fi
fi

shape Math
    $Registry.enroll
    $(Timer.timed)
    shared cube(n: int) -> int
        <- n * n * n
    fi
fi

task main()
    fixed f = Math.cube
    show(f(2))
fi
";
    assert_eq!(Analysis::new(source).diagnostics(), Vec::new());
    assert_eq!(hover(source, "$Registry"), "$d, $d(values), $d?, or $(w1, w2(values), .m)");
    assert_eq!(hover(source, "enroll\n    $"), "shared enroll(f: (int) -> int)");
    assert_eq!(hover(source, "timed)"), "shared timed(next: (int) -> int, n: int) -> int");
    assert_eq!(hover(source, "cube\n    show"), "shared cube(n: int) -> int");
    assert_eq!(hover(source, "f = "), "fixed f: (int) -> int");
    let at = LineIndex::new(source).utf16_of(source.find("enroll\n    $").unwrap());
    let declared = LineIndex::new(source).utf16_of(source.find("enroll(f").unwrap());
    assert_eq!(Analysis::new(source).definition(at).map(|found| found.from), Some(declared));
    let broken = source.replace("show(f(2))", "show(unknown)");
    assert_eq!(hover(&broken, "enroll\n    $"), "shared enroll(f: (int) -> int)");
    assert_eq!(hover(&broken, "cube\n    show"), "shared cube(n: int) -> int");
}

const ON_METHODS: &str = "\
task audited<P..., R>(next: (P...) -> R, args: P...) -> R
    <- next(args...)
fi

shape Account
    read balance: int

    alters guarded(next: (flex& Account, int) -> bool, amount: int) -> bool
        when amount > .balance
            <- false
        fi
        <- next(&.this, amount)
    fi

    $(audited)
    alters deposit(amount: int)
        .balance += amount
    fi

    $(.guarded)
    alters withdraw(amount: int) -> bool
        .balance -= amount
        <- true
    fi
fi

task main()
    CHECK
    flex a = Account { balance: 0 }
    a.deposit(5)
    show(a.withdraw(3))
fi
";

fn class_at(source: &str, needle: &str) -> Option<TokenClass> {
    let at = LineIndex::new(source).utf16_of(source.find(needle).unwrap());
    Analysis::new(source).tokens().into_iter().find(|t| t.from == at).map(|t| t.class)
}

#[test]
fn a_method_of_every_value_takes_wrappers_and_a_method_of_its_type_wraps_it() {
    for checked in [true, false] {
        let source = ON_METHODS.replace("CHECK", if checked { "show(0)" } else { "show(unknown)" });
        if checked {
            assert_eq!(Analysis::new(&source).diagnostics(), Vec::new());
        }
        assert_eq!(class_at(&source, "audited)"), Some(TokenClass::Task));
        assert_eq!(class_at(&source, "guarded)"), Some(TokenClass::Method));
        assert!(hover(&source, "guarded)").starts_with("alters guarded(next: (flex& Account, int) -> bool"), "{}", hover(&source, "guarded)"));
    }
    let source = ON_METHODS.replace("CHECK", "show(0)").replace("alters guarded(", "guarded(").replace("<- next(&.this, amount)", "<- false");
    let diagnostics = Analysis::new(&source).diagnostics();
    assert!(diagnostics.iter().any(|d| d.message.starts_with("method 'withdraw' alters the value")), "{:?}", diagnostics);
}
