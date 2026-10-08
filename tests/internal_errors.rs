//! Code generation trusts the checker's annotations and guarantees. When they
//! are missing or broken (a compiler bug), it must return an internal compiler
//! error instead of panicking. These tests run code generation on programs that
//! skipped the checker.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela::ast::{ExprKind, Item, StmtKind};
use vela::check::Checker;
use vela::codegen::Generator;
use vela::lexer::Lexer;
use vela::parser::Parser;

fn parse(source: &str) -> Vec<Item> {
    let (tokens, spans, ends) = Lexer::new(source).tokenize().unwrap();
    Parser::new(tokens, spans, ends).parse_file().unwrap()
}

/// Generates C from `source` without checking it, expecting an internal error
/// with exactly `message`.
fn assert_internal_error(source: &str, message: &str) {
    let err = Generator::new().generate(&parse(source)).unwrap_err();
    assert_eq!(err.span, None, "internal errors have no diagnostic location");
    assert_eq!(err.message, format!("internal compiler error: {}", message));
}

#[test]
fn checked_program_generates_c() {
    let mut items = parse("task main()\n    show(1)\nfi\n");
    let mut checker = Checker::new();
    checker.check_file(&mut items);
    assert!(checker.errors().is_empty(), "{:?}", checker.errors());
    let c = Generator::new().generate(&items).unwrap();
    assert!(c.contains("vela__rt__show_int(1);"), "{}", c);
}

#[test]
fn untyped_expression() {
    assert_internal_error(
        "task main()\n    show(1)\nfi\n",
        "expression has not been type-checked (at 2:10)",
    );
}

#[test]
fn untyped_binding() {
    assert_internal_error(
        "task main()\n    fixed x = 1\nfi\n",
        "binding 'x' has no type (at 2:11)",
    );
}

#[test]
fn show_with_a_type_it_cannot_write() {
    // The checker refuses a value that can fail given to `show`; give a
    // checked call one afterwards, as a checker bug would.
    let mut items = parse("task main()\n    region r\n    fixed xs = int(\"1\")\n    show(1, 2)\nfi\n");
    let mut checker = Checker::new();
    checker.check_file(&mut items);
    assert!(checker.errors().is_empty(), "{:?}", checker.errors());
    let Item::Task(main) = &mut items[0] else { panic!("expected task main") };
    let StmtKind::Binding { init, .. } = &main.body[1].kind else { panic!("expected the binding of xs") };
    let slice = init[0].clone();
    let StmtKind::Expr(call) = &mut main.body[2].kind else { panic!("expected the call of show") };
    let ExprKind::Call { args, .. } = &mut call.kind else { panic!("expected a call, got {:?}", call.kind) };
    args[1] = slice;

    let err = Generator::new().generate(&items).unwrap_err();
    assert_eq!(err.span, None);
    assert_eq!(err.message, "internal compiler error: show called with type int ~ ParseError (at 4:5)");
}

#[test]
fn the_region_of_a_show_read_outside_one() {
    // A value with its own `str` becomes the call of that method in the region
    // `show` makes for its line; read that call on its own, where there is no
    // such region, as a checker bug would.
    let mut items = parse(
        "shape Point\n    x: int\n\n    str(r: region) -> text@r\n        <- \"{.x}\" at r\n    fi\nfi\n\ntask main()\n    show(Point { x: 1 })\nfi\n",
    );
    let mut checker = Checker::new();
    checker.check_file(&mut items);
    assert!(checker.errors().is_empty(), "{:?}", checker.errors());
    let Some(Item::Task(main)) = items.iter_mut().find(|item| matches!(item, Item::Task(task) if task.name == "main")) else {
        panic!("expected task main")
    };
    let StmtKind::Expr(call) = &mut main.body[0].kind else { panic!("expected the call of show") };
    let ExprKind::Call { args, .. } = &mut call.kind else { panic!("expected a call, got {:?}", call.kind) };
    *call = args[0].clone();

    let err = Generator::new().generate(&items).unwrap_err();
    assert_eq!(err.span, None);
    assert_eq!(err.message, "internal compiler error: the region of a 'show' read outside one (at 10:10)");
}

#[test]
fn variant_with_wrong_argument_count() {
    // The checker resolves `Circle(1.0)` into a constructor; give that node an
    // extra argument afterwards, as a checker bug would.
    let mut items = parse("kind Shape\n    Circle(radius: float)\nfi\n\ntask main()\n    fixed c = Circle(1.0)\nfi\n");
    let mut checker = Checker::new();
    checker.check_file(&mut items);
    assert!(checker.errors().is_empty(), "{:?}", checker.errors());
    let Item::Task(main) = &mut items[1] else { panic!("expected task main") };
    let StmtKind::Binding { init, .. } = &mut main.body[0].kind else { panic!("expected a binding") };
    let expr = &mut init[0];
    let ExprKind::Construct { args, .. } = &mut expr.kind else { panic!("expected a constructor, got {:?}", expr.kind) };
    args.push(args[0].clone());

    let err = Generator::new().generate(&items).unwrap_err();
    assert_eq!(err.span, None);
    assert_eq!(
        err.message,
        "internal compiler error: variant 'Circle' called with 2 arguments (expected 1) (at 6:15)"
    );
}

#[test]
fn match_on_unchecked_value() {
    assert_internal_error(
        "kind Light\n    Red(level: int)\nfi\n\ntask main()\n    match Red(1)\n    case Red a\n        show(a)\n    fi\nfi\n",
        "expression has not been type-checked (at 6:11)",
    );
}

#[test]
fn value_list_that_does_not_fit_its_names() {
    // The checker counts the values a list of names receives; only one call
    // may give several of them.
    assert_internal_error("task main()\n    fixed a, b = 1, 2, 3\nfi\n", "3 values received by 2 names (at 2:11)");
}

#[test]
fn send_with_the_wrong_number_of_values() {
    assert_internal_error("task one() -> int\n    <- 1, 2\nfi\n", "2 values sent by a task of type int (at 2:5)");
}
