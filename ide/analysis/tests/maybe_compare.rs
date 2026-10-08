//! Comparing a maybe, in the editor: a plan B that gives `none` makes the
//! whole a `maybe`, and the name bound to it is one, whether or not the file
//! checks.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, LineIndex};

const PROGRAM: &str = "\
task half(n: int) -> maybe int
    when n % 2 == 0
        <- n / 2
    fi
    <- none
fi

task main()
    CHECK
    fixed b = scan(\"enter: \", int) { none }
    fixed m = half(8) { none }
    fixed n = half(8) { 0 }
    fixed k = half(8) {
        show(\"odd\")
        none
    }
    fixed j = half(8) {
        when n > 0
            show(\"none\")
        fi
        n
    }
    when b ~= 10
        show(m, n, k, j)
    fi
fi
";

/// The program as it is, with a mistake the checker reads past, or with one
/// the parser stops at, so that nothing the checker found is there.
fn programs() -> [String; 3] {
    [PROGRAM.replace("CHECK", "show(0)"), PROGRAM.replace("CHECK", "show(unknown)"), PROGRAM.replace("CHECK", "show(") + "task ("]
}

fn hover(source: &str, needle: &str) -> String {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    Analysis::new(source).hover(LineIndex::new(source).utf16_of(byte)).unwrap_or_else(|| panic!("no hover at {:?}", needle)).code
}

#[test]
fn the_program_used_by_these_tests_compiles() {
    assert_eq!(Analysis::new(&programs()[0]).diagnostics(), Vec::new());
}

#[test]
fn a_plan_b_giving_none_is_a_maybe_whether_or_not_the_file_checks() {
    for source in programs() {
        assert_eq!(hover(&source, "b = scan"), "bind b: maybe int", "{}", source);
        assert_eq!(hover(&source, "m = half"), "bind m: maybe int", "{}", source);
        assert_eq!(hover(&source, "n = half"), "bind n: int", "{}", source);
        // Over several lines, when the last line is `none`, and not otherwise.
        assert_eq!(hover(&source, "k = half"), "bind k: maybe int", "{}", source);
        assert_eq!(hover(&source, "j = half"), "bind j: int", "{}", source);
    }
}

/// `is` joined with `and`: the name the pattern binds is seen after its `and`
/// and in the block, and not after the `when`.
const JOINED: &str = "\
task half(n: int) -> maybe int
    when n % 2 == 0
        <- n / 2
    fi
    <- none
fi

task main()
    CHECK
    fixed b = half(8)
    when b is { inside } and inside < 0 and half(inside) is { quarter }
        show(inside, quarter)
    fi
    @@
fi
";

fn joined(first: &str, code: &str) -> String {
    JOINED.replace("CHECK", first).replace("    @@\n", &format!("    {}\n", code))
}

fn completion_labels(marked: &str) -> Vec<String> {
    let at = marked.find("@@").expect("a @@ marker");
    let source = marked.replacen("@@", "", 1);
    Analysis::new(&source).complete(LineIndex::new(&source).utf16_of(at)).items.into_iter().map(|item| item.label).collect()
}

#[test]
fn the_joined_program_compiles() {
    assert_eq!(Analysis::new(&joined("show(0)", "show(b)")).diagnostics(), Vec::new());
}

#[test]
fn a_name_an_is_binds_is_seen_after_its_and_and_in_the_block() {
    for first in ["show(0)", "show(unknown)", "show("] {
        let source = joined(first, "show(b)");
        let analysis = Analysis::new(&source);
        let index = LineIndex::new(&source);
        let bound = index.utf16_of(source.find("inside }").unwrap());
        for used in ["inside < 0", "inside) is", "inside, quarter"] {
            let at = index.utf16_of(source.find(used).unwrap());
            assert_eq!(analysis.definition(at).map(|l| l.from), Some(bound), "{} in {:?}", used, first);
        }
        assert_eq!(hover(&source, "inside < 0"), "inside: int", "{:?}", first);
        assert_eq!(hover(&source, "quarter)"), "quarter: int", "{:?}", first);
    }
}

#[test]
fn a_name_an_is_binds_completes_after_its_and_and_not_after_the_when() {
    let inside = |code: &str| completion_labels(&JOINED.replace("CHECK", "show(0)").replace("inside < 0 and", code));
    assert!(inside("ins@@ < 0 and").contains(&"inside".to_string()));
    let after = completion_labels(&joined("show(0)", "show(ins@@)"));
    assert!(!after.contains(&"inside".to_string()), "{:?}", after);
}

#[test]
fn a_condition_holding_an_is_goes_on_with_and_and_another_is() {
    let after = |condition: &str| completion_labels(&joined("show(0)", &format!("when {} @@\n        show(b)\n    fi", condition)));
    // After a value in a part, `is` may match it; `or` never joins an `is`.
    let next_part = after("b is { x } and half(x)");
    assert!(next_part.contains(&"is".to_string()) && next_part.contains(&"and".to_string()) && !next_part.contains(&"or".to_string()), "{:?}", next_part);
    let test = after("b is { x } and x > 0");
    assert!(test.contains(&"and".to_string()) && !test.contains(&"or".to_string()), "{:?}", test);
    // Without an `is`, both still join.
    let plain = after("b == none");
    assert!(plain.contains(&"is".to_string()) && plain.contains(&"or".to_string()), "{:?}", plain);
}
