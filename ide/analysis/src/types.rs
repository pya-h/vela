//! What the compiler's checker knows about a program, collected into lookup
//! tables keyed by source position. The checker annotates the program in
//! place; when it stops at an error, the tables still hold everything it
//! checked before that error.

use std::collections::{HashMap, HashSet};

use vela::ast::{Block, Expr, ExprKind, Item, KindDef, Pattern, PatternKind, Span, Stmt, StmtKind, Target, Type};
use vela::check::{in_region, regional_types};
use vela::driver;
use vela::error::Error;

/// What recording types needs to know about the program's declarations.
struct Context<'a> {
    kinds: HashMap<&'a str, &'a KindDef>,
    /// The shapes and kinds that hold pointers.
    regional: HashSet<String>,
}

/// A (line, column) position, as the compiler reports it.
pub(crate) type Pos = (usize, usize);

fn pos(span: Span) -> Pos {
    (span.line, span.col)
}

#[derive(Default)]
pub(crate) struct Checked {
    /// The compiler's first error, if any.
    pub error: Option<Error>,
    /// The type of each checked expression, by the position of the token that
    /// identifies it: the name of a variable, call, or field access; the
    /// operator of a unary or binary operation or of `^`; `new`; or a shape
    /// literal's shape name.
    pub exprs: HashMap<Pos, Type>,
    /// The type of each parameter, variable, and region (`Type::Region`), by
    /// the position of its name.
    pub decls: HashMap<Pos, Type>,
    /// For each field access `a.b`, the shape of `a`, by the position of `b`.
    pub field_accesses: HashMap<Pos, String>,
}

impl Checked {
    pub fn new(source: &str) -> Checked {
        let partial = driver::front_end_partial(source);
        let mut checked = Checked { error: partial.error, ..Checked::default() };
        let Some(items) = partial.items else { return checked };
        let kinds: HashMap<&str, &KindDef> = items
            .iter()
            .filter_map(|item| match item {
                Item::Kind(k) => Some((k.name.as_str(), k)),
                _ => None,
            })
            .collect();
        let context = Context { kinds, regional: regional_types(&items) };
        for item in &items {
            if let Item::Task(task) = item {
                for param in &task.params {
                    checked.decls.insert(pos(param.span), param.ty.clone());
                }
                // A named result is a variable of the task, with the type the
                // header gives it.
                for (result, ty) in task.results.iter().zip(task.ret.results()) {
                    if let Some((_, at)) = &result.name {
                        checked.decls.insert(pos(*at), ty.clone());
                    }
                }
                checked.block(&task.body, &context);
            }
        }
        checked
    }

    fn block(&mut self, stmts: &Block, kinds: &Context) {
        for stmt in stmts {
            self.statement(stmt, kinds);
        }
    }

    fn statement(&mut self, stmt: &Stmt, kinds: &Context) {
        match &stmt.kind {
            StmtKind::Expr(e) => self.expr(e, kinds),
            StmtKind::Send(values) | StmtKind::SetResults(_, values) => values.iter().for_each(|value| self.expr(value, kinds)),
            StmtKind::Region(_) => {
                self.decls.insert(pos(stmt.span), Type::Region);
            }
            StmtKind::Binding { names, init, .. } => {
                for bound in names {
                    if let Some(ty) = &bound.ty {
                        self.decls.insert(pos(bound.span), ty.clone());
                    }
                }
                init.iter().for_each(|value| self.expr(value, kinds));
            }
            StmtKind::Assign(targets, values) => {
                for target in targets {
                    if let Target::Place(place) = target {
                        self.expr(place, kinds);
                    }
                }
                values.iter().for_each(|value| self.expr(value, kinds));
            }
            StmtKind::When(cond, then_block, else_block) => {
                self.expr(&cond.value, kinds);
                if let Some(pattern) = &cond.pattern {
                    self.pattern(cond.value.ty.as_ref(), pattern, kinds);
                }
                self.block(then_block, kinds);
                if let Some(else_block) = else_block {
                    self.block(else_block, kinds);
                }
            }
            StmtKind::Loop(cond, body) => {
                self.expr(&cond.value, kinds);
                if let Some(pattern) = &cond.pattern {
                    self.pattern(cond.value.ty.as_ref(), pattern, kinds);
                }
                self.block(body, kinds);
            }
            StmtKind::Each(_, range, body) => {
                self.decls.insert(pos(stmt.span), Type::Int);
                self.expr(range, kinds);
                self.block(body, kinds);
            }
            StmtKind::Match(subject, cases) => {
                self.expr(subject, kinds);
                for case in cases {
                    self.pattern(subject.ty.as_ref(), &case.pattern, kinds);
                    self.block(&case.body, kinds);
                }
            }
        }
    }

    /// Records the types of the names `pattern` binds from a value of type
    /// `subject`.
    fn pattern(&mut self, subject: Option<&Type>, pattern: &Pattern, kinds: &Context) {
        let Some(subject) = subject else { return };
        match (&pattern.kind, subject) {
            (PatternKind::Variant { tag, binds }, Type::Named(name, region)) => {
                let payload = kinds.kinds.get(name.as_str()).and_then(|k| k.variants.iter().find(|v| v.tag == *tag)).map(|v| &v.payload);
                for ((_, span), field) in binds.iter().zip(payload.into_iter().flatten()) {
                    let ty = match region {
                        Some(region) => in_region(&field.ty, region, &kinds.regional),
                        None => field.ty.clone(),
                    };
                    self.decls.insert(pos(*span), ty);
                }
            }
            (PatternKind::Value((_, span)), Type::Maybe(inner) | Type::Or(inner, _)) => {
                self.decls.insert(pos(*span), (**inner).clone());
            }
            (PatternKind::Fail((_, span)), Type::Or(_, error)) => {
                self.decls.insert(pos(*span), (**error).clone());
            }
            _ => {}
        }
    }

    fn expr(&mut self, e: &Expr, kinds: &Context) {
        // A wrapped value keeps its own type at its position.
        if let ExprKind::Wrap(inner) = &e.kind {
            return self.expr(inner, kinds);
        }
        if let Some(ty) = &e.ty {
            self.exprs.entry(pos(e.span)).or_insert_with(|| ty.clone());
        }
        match &e.kind {
            ExprKind::Field(base, _) => {
                if let Some(Type::Named(shape, _)) = &base.ty {
                    self.field_accesses.insert(pos(e.span), shape.clone());
                }
                self.expr(base, kinds);
            }
            ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) | ExprKind::NoneLit
            | ExprKind::Var(_) => {}
            ExprKind::Unary(_, inner) | ExprKind::Deref(inner) | ExprKind::New { value: inner, .. } => self.expr(inner, kinds),
            ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Range(a, b) => {
                self.expr(a, kinds);
                self.expr(b, kinds);
            }
            ExprKind::Call(_, args) | ExprKind::Construct { args, .. } => {
                for arg in args {
                    self.expr(arg, kinds);
                }
            }
            ExprKind::StructInit(_, fields) => {
                for field in fields {
                    self.expr(&field.value, kinds);
                }
            }
            ExprKind::Fail(inner) | ExprKind::Wrap(inner) => self.expr(inner, kinds),
            ExprKind::PlanB { value, error, body } => {
                self.expr(value, kinds);
                if let (Some((_, span)), Some(Type::Or(_, error_ty))) = (error, &value.ty) {
                    self.decls.insert(pos(*span), (**error_ty).clone());
                }
                self.block(body, kinds);
            }
        }
    }
}
