//! What the compiler's checker knows about a program, collected into lookup
//! tables keyed by source position. The checker annotates the program in
//! place; when it stops at an error, the tables still hold everything it
//! checked before that error.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;

use vela::ast::{Block, Expr, ExprKind, Item, KindDef, Pattern, PatternKind, Span, Stmt, StmtKind, Target, Type};
use vela::check::{in_region, regional_types, textual_types};
use vela::driver;
use vela::error::Error;
use vela::modules::{self, SourceFile};

/// Where the analyzed file is, and the text of the other files of its program
/// that the editor has, by path (`/`-separated, as `include` paths resolve).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Context<'a> {
    pub path: &'a str,
    pub sources: &'a HashMap<String, String>,
}

/// A path as the editor names files: `/`-separated.
pub(crate) fn slashed(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// What recording types needs to know about the program's declarations.
struct Declarations<'a> {
    kinds: HashMap<&'a str, &'a KindDef>,
    /// The shapes and kinds that can be given a region: those holding
    /// pointers or text.
    takes_region: HashSet<String>,
}

/// A (line, column) position, as the compiler reports it.
pub(crate) type Pos = (usize, usize);

fn pos(span: Span) -> Pos {
    (span.line, span.col)
}

#[derive(Default)]
pub(crate) struct Checked {
    /// The compiler's first error, if any. Its position may be in another file
    /// of the program (see `files`).
    pub error: Option<Error>,
    /// The program's files, the analyzed file first, as far as they loaded.
    pub files: Vec<SourceFile>,
    /// The files the program includes, directly or through others, that the
    /// editor didn't give.
    pub missing: Vec<String>,
    /// The type of each checked expression, by the position of the token that
    /// identifies it: the name of a variable, call, or field access; the
    /// operator of a unary or binary operation or of `^`; `new`; or a shape
    /// literal's shape name.
    pub exprs: HashMap<Pos, Type>,
    /// The type of each parameter, variable, and region (`Type::Region`), by
    /// the position of its name.
    pub decls: HashMap<Pos, Type>,
    /// For each field access `a.b`, the shape of `a`, by the position of `b`;
    /// an empty name for `xs.len` on an array or slice.
    pub field_accesses: HashMap<Pos, String>,
}

impl Checked {
    pub fn new(source: &str, context: Context) -> Checked {
        let path = Path::new(context.path);
        // Every file reachable through the files the editor has, reading the
        // ones it doesn't as empty, so it can fetch them all at once.
        let mut missing = Vec::new();
        let mut discover = |file: &Path| -> io::Result<String> {
            let file = slashed(file);
            if let Some(source) = context.sources.get(&file) {
                return Ok(source.clone());
            }
            if !missing.contains(&file) {
                missing.push(file);
            }
            Ok(String::new())
        };
        let _ = modules::load(path, source, &mut discover);
        let mut loader = |file: &Path| context.sources.get(&slashed(file)).cloned().ok_or_else(|| io::Error::from(io::ErrorKind::NotFound));
        let partial = driver::front_end_program(path, source, &mut loader);
        let spelling = partial.spelling;
        let mut checked = Checked { error: partial.error, files: partial.files, missing, ..Checked::default() };
        let Some(items) = partial.items else { return checked };
        let kinds: HashMap<&str, &KindDef> = items
            .iter()
            .filter_map(|item| match item {
                Item::Kind(k) => Some((k.name.as_str(), k)),
                _ => None,
            })
            .collect();
        let declarations = Declarations { kinds, takes_region: regional_types(&items).union(&textual_types(&items)).cloned().collect() };
        // Positions are recorded for the analyzed file only.
        for item in &items {
            if let Item::Task(task) = item
                && task.span.file == 0
            {
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
                checked.block(&task.body, &declarations);
            }
        }
        // The program names another module's shapes and kinds by the module's
        // path (`shapes/geometry.Point`); this file names them as it writes them.
        for ty in checked.exprs.values_mut().chain(checked.decls.values_mut()) {
            *ty = spelling.ty(ty);
        }
        for shape in checked.field_accesses.values_mut() {
            *shape = spelling.name(shape);
        }
        checked
    }

    fn block(&mut self, stmts: &Block, kinds: &Declarations) {
        for stmt in stmts {
            self.statement(stmt, kinds);
        }
    }

    fn statement(&mut self, stmt: &Stmt, kinds: &Declarations) {
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
                // A counter, or an item of an array or slice, once the checker
                // knows which.
                let item = match (&range.kind, &range.ty) {
                    (ExprKind::Range(..), _) => Some(Type::Int),
                    (_, Some(Type::Array(items, _) | Type::Slice(items, _))) => Some((**items).clone()),
                    _ => None,
                };
                if let Some(item) = item {
                    self.decls.insert(pos(stmt.span), item);
                }
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
    fn pattern(&mut self, subject: Option<&Type>, pattern: &Pattern, kinds: &Declarations) {
        let Some(subject) = subject else { return };
        match (&pattern.kind, subject) {
            (PatternKind::Variant { tag, binds, .. }, Type::Named(name, region)) => {
                let payload = kinds.kinds.get(name.as_str()).and_then(|k| k.variants.iter().find(|v| v.tag == *tag)).map(|v| &v.payload);
                for ((_, span), field) in binds.iter().zip(payload.into_iter().flatten()) {
                    let ty = match region {
                        Some(region) => in_region(&field.ty, region, &kinds.takes_region),
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

    fn expr(&mut self, e: &Expr, kinds: &Declarations) {
        // A wrapped value keeps its own type at its position.
        if let ExprKind::Wrap(inner) = &e.kind {
            return self.expr(inner, kinds);
        }
        if let Some(ty) = &e.ty {
            self.exprs.entry(pos(e.span)).or_insert_with(|| ty.clone());
        }
        match &e.kind {
            ExprKind::Field(base, _) => {
                match &base.ty {
                    Some(Type::Named(shape, _)) => {
                        self.field_accesses.insert(pos(e.span), shape.clone());
                    }
                    Some(Type::Array(..) | Type::Slice(..)) => {
                        self.field_accesses.insert(pos(e.span), String::new());
                    }
                    _ => {}
                }
                self.expr(base, kinds);
            }
            ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) | ExprKind::NoneLit
            | ExprKind::Var(_) => {}
            ExprKind::Unary(_, inner) | ExprKind::Deref(inner) | ExprKind::New { value: inner, .. } => self.expr(inner, kinds),
            ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Range(a, b) | ExprKind::Repeat { value: a, count: b } => {
                self.expr(a, kinds);
                self.expr(b, kinds);
            }
            ExprKind::Call(_, args) | ExprKind::Construct { args, .. } | ExprKind::Array(args) => {
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
            ExprKind::Scan { args, .. } => {
                for arg in args {
                    self.expr(arg, kinds);
                }
            }
            // `x.copy()`: the word is a member of every value, as `len` is of a slice.
            ExprKind::Copy { value, .. } => {
                self.field_accesses.insert(pos(e.span), String::new());
                self.expr(value, kinds);
            }
            // `xs.add(…)` and `xs.del(…)`: the word is a slice's member, as `len` is.
            ExprKind::Edit { items, args, .. } => {
                if matches!(items.ty, Some(Type::Slice(..))) {
                    self.field_accesses.insert(pos(e.span), String::new());
                }
                self.expr(items, kinds);
                for arg in args {
                    self.expr(arg, kinds);
                }
            }
        }
    }
}
