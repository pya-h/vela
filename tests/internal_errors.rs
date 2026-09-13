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
    let (tokens, spans) = Lexer::new(source).tokenize().unwrap();
    Parser::new(tokens, spans).parse_file().unwrap()
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
    Checker::new().check_file(&mut items).unwrap();
    let c = Generator::new().generate(&items).unwrap();
    assert!(c.contains("vela_show_int(1);"), "{}", c);
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
        "task main()\n    bind x = 1\nfi\n",
        "binding 'x' has no type (at 2:10)",
    );
}

#[test]
fn show_with_wrong_argument_count() {
    assert_internal_error(
        "task main()\n    show(1, 2)\nfi\n",
        "show called with 2 arguments (at 2:5)",
    );
}

#[test]
fn variant_with_wrong_argument_count() {
    // The checker resolves `Circle(1.0)` into a constructor; give that node an
    // extra argument afterwards, as a checker bug would.
    let mut items = parse("kind Shape\n    Circle(radius: float)\nfi\n\ntask main()\n    Circle(1.0)\nfi\n");
    Checker::new().check_file(&mut items).unwrap();
    let Item::Task(main) = &mut items[1] else { panic!("expected task main") };
    let StmtKind::Expr(expr) = &mut main.body[0].kind else { panic!("expected an expression statement") };
    let ExprKind::Construct { args, .. } = &mut expr.kind else { panic!("expected a constructor, got {:?}", expr.kind) };
    args.push(args[0].clone());

    let err = Generator::new().generate(&items).unwrap_err();
    assert_eq!(err.span, None);
    assert_eq!(
        err.message,
        "internal compiler error: variant 'Circle' called with 2 arguments (expected 1) (at 6:5)"
    );
}

#[test]
fn match_on_unchecked_value() {
    assert_internal_error(
        "kind Light\n    Red(level: int)\nfi\n\ntask main()\n    match Red(1)\n    case Red a\n        show(a)\n    fi\nfi\n",
        "expression has not been type-checked (at 6:11)",
    );
}
