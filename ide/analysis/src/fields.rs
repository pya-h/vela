//! Exact field references from the checked program. A field access `a.b`
//! needs the type of `a`, which only the checker knows; when the file checks,
//! this maps every field name in an access to the shape it belongs to.

use std::collections::HashMap;

use vela::ast::{Block, Expr, ExprKind, Item, Stmt, StmtKind, Type};

/// The shape of each field access, keyed by the (line, column) of the field
/// name.
pub(crate) fn field_accesses(items: &[Item]) -> HashMap<(usize, usize), String> {
    let mut found = HashMap::new();
    for item in items {
        if let Item::Task(task) = item {
            block(&task.body, &mut found);
        }
    }
    found
}

fn block(stmts: &Block, found: &mut HashMap<(usize, usize), String>) {
    for stmt in stmts {
        statement(stmt, found);
    }
}

fn statement(stmt: &Stmt, found: &mut HashMap<(usize, usize), String>) {
    match &stmt.kind {
        StmtKind::Expr(e) | StmtKind::Free(e) | StmtKind::Send(Some(e)) => expr(e, found),
        StmtKind::Send(None) => {}
        StmtKind::Binding { init, .. } => expr(init, found),
        StmtKind::Assign(target, value) => {
            expr(target, found);
            expr(value, found);
        }
        StmtKind::When(cond, then_block, else_block) => {
            expr(cond, found);
            block(then_block, found);
            if let Some(else_block) = else_block {
                block(else_block, found);
            }
        }
        StmtKind::Loop(cond, body) | StmtKind::Each(_, cond, body) => {
            expr(cond, found);
            block(body, found);
        }
        StmtKind::Match(subject, cases) => {
            expr(subject, found);
            for case in cases {
                block(&case.body, found);
            }
        }
    }
}

fn expr(e: &Expr, found: &mut HashMap<(usize, usize), String>) {
    match &e.kind {
        ExprKind::Field(base, _) => {
            if let Some(Type::Named(shape)) = &base.ty {
                found.insert((e.span.line, e.span.col), shape.clone());
            }
            expr(base, found);
        }
        ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) | ExprKind::NoneLit
        | ExprKind::Var(_) => {}
        ExprKind::Unary(_, inner) | ExprKind::Deref(inner) | ExprKind::New { value: inner, .. } => expr(inner, found),
        ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Range(a, b) => {
            expr(a, found);
            expr(b, found);
        }
        ExprKind::Call(_, args) | ExprKind::Construct { args, .. } => {
            for arg in args {
                expr(arg, found);
            }
        }
        ExprKind::StructInit(_, fields) => {
            for field in fields {
                expr(&field.value, found);
            }
        }
    }
}
