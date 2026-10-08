//! What a fixed name owns, a slice, a map or a pointer, is never given to what
//! may change it (Part 22, phase 2.3): Studio shows the compiler's error where
//! the value is given, for a call that sends back what it was given too, and
//! none where a task only reads what it is given to make something new; a
//! lambda is followed to what it sends back. A slice made of another is a
//! complete copy, owned by no one (phase 2.4).

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::Analysis;

const TASKS: &str = "\
task first_two(xs: [int]@r) -> [int]@r
    <- xs[0..2]
fi

task lengths(r: region, names: [text]@r) -> [int]@r
    flex out = new [int]() at r
    each name in names
        out.add(name.len)
    fi
    <- out
fi
";

fn diagnostics(main: &str) -> Vec<(usize, usize, String)> {
    let source = format!("{}\ntask main()\n    region r default\n{}fi\n", TASKS, main);
    Analysis::new(&source).diagnostics().into_iter().map(|d| (d.line.unwrap(), d.col.unwrap(), d.message)).collect()
}

#[test]
fn a_flex_name_given_what_a_fixed_one_owns_is_an_error_where_it_is_given() {
    assert_eq!(
        diagnostics("    fixed xs = [1, 2, ...]\n    flex ys = xs\n"),
        [(16, 15, "'ys' is given what fixed 'xs' owns, and nothing changes through what a fixed name owns; give it a copy: 'xs.copy() at r'".to_string())]
    );
}

#[test]
fn a_call_sending_back_what_it_was_given_gives_what_the_owner_owns() {
    assert_eq!(
        diagnostics("    fixed nums = [1, 2, 3, ...]\n    flex part = first_two(nums)\n"),
        [(16, 17, "'part' is given what fixed 'nums' owns, and nothing changes through what a fixed name owns; give it a copy: 'first_two(…).copy() at r'".to_string())]
    );
}

#[test]
fn a_call_making_something_new_gives_nothing_a_fixed_name_owns() {
    assert_eq!(diagnostics("    fixed names = [\"ada\", ...]\n    flex sizes = lengths(r, names)\n    sizes[0] = 0\n"), []);
}

#[test]
fn a_lambda_is_followed_to_what_it_sends_back() {
    assert_eq!(
        diagnostics("    fixed xs = [1, 2, ...]\n    flex f = [@r]() -> [int]@r <- xs fi\n    show(f())\n"),
        [(16, 14, "'f' is given a lambda that may send back what fixed 'xs' owns, and nothing changes through what a fixed name owns; make 'f' fixed, or have the lambda send back a copy, as in '<- xs.copy() at r'".to_string())]
    );
    // One that only reads what it keeps to make something new gives nothing owned.
    assert_eq!(diagnostics("    fixed xs = [1, 2, ...]\n    flex f = [@r]() -> [int]@r <- xs.copy() at r fi\n    flex ys = f()\n    ys[0] = 0\n"), []);
}

#[test]
fn a_slice_made_of_another_is_a_copy_of_its_own() {
    assert_eq!(diagnostics("    fixed rows = [[1, 2, ...], ...]\n    flex mine = new [[int]](rows) at r\n    mine[0][0] = 9\n"), []);
}
