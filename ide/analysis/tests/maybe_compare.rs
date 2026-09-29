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
    bind b = scan(\"enter: \", int) { none }
    bind m = half(8) { none }
    bind n = half(8) { 0 }
    bind k = half(8) {
        show(\"odd\")
        none
    }
    bind j = half(8) {
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
