//! What the compiler's checker knows about a program, collected into lookup
//! tables keyed by source position. The checker annotates the program in
//! place; when it stops at an error, the tables still hold everything it
//! checked before that error.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;

use vela::ast::{Applied, Block, Expr, ExprKind, Item, KindDef, Pattern, PatternKind, SHOWN, ShapeDef, Span, Stmt, StmtKind, THIS, Target, Type, method_parts};
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

/// A type as the program writes it: a shape the compiler made for the types it
/// was given (`Box__of__int`) reads as those types (`Box<int>`), and so do the
/// types inside it.
fn as_written(ty: &Type, made: &HashMap<&str, &ShapeDef>) -> Type {
    match ty {
        Type::Named(name, region) => match made.get(name.as_str()) {
            Some(shape) => Type::Applied(Box::new(Applied {
                name: as_written_name(name, made),
                types: shape.given.iter().map(|ty| as_written(ty, made)).collect(),
                region: region.clone(),
            })),
            None => ty.clone(),
        },
        Type::Ptr(inner, region) => Type::Ptr(Box::new(as_written(inner, made)), region.clone()),
        Type::Slice(inner, region) => Type::Slice(Box::new(as_written(inner, made)), region.clone()),
        Type::Array(inner, count) => Type::Array(Box::new(as_written(inner, made)), *count),
        Type::Range(inner) => Type::Range(Box::new(as_written(inner, made))),
        Type::Maybe(inner) => Type::Maybe(Box::new(as_written(inner, made))),
        Type::Fail(inner) => Type::Fail(Box::new(as_written(inner, made))),
        Type::Or(value, error) => Type::Or(Box::new(as_written(value, made)), Box::new(as_written(error, made))),
        Type::Tuple(items) => Type::Tuple(items.iter().map(|ty| as_written(ty, made)).collect()),
        Type::Results(types) => Type::Results(types.iter().map(|ty| as_written(ty, made)).collect()),
        Type::Applied(applied) => Type::Applied(Box::new(Applied {
            name: as_written_name(&applied.name, made),
            types: applied.types.iter().map(|ty| as_written(ty, made)).collect(),
            region: applied.region.clone(),
        })),
        other => other.clone(),
    }
}

/// The name of the shape a made one was made from: everything before the
/// `__of__` the compiler joins its types with (no Vela name holds `__`).
fn as_written_name(name: &str, made: &HashMap<&str, &ShapeDef>) -> String {
    match made.contains_key(name) {
        true => name.split("__of__").next().unwrap_or(name).to_string(),
        false => name.to_string(),
    }
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
    /// Every mistake the compiler found, in source order. A position may be in
    /// another file of the program (see `files`).
    pub errors: Vec<Error>,
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
        let mut checked = Checked { errors: partial.errors, files: partial.files, missing, ..Checked::default() };
        let Some(items) = partial.items else { return checked };
        let kinds: HashMap<&str, &KindDef> = items
            .iter()
            .filter_map(|item| match item {
                Item::Kind(k) => Some((k.name.as_str(), k)),
                _ => None,
            })
            .collect();
        let declarations = Declarations { kinds, takes_region: regional_types(&items).union(&textual_types(&items)).cloned().collect() };
        // Positions are recorded for the analyzed file only. A task made from a
        // generic one holds the positions of the generic task's body with the
        // types of one use in place of its own, so the editor reads the generic
        // task itself, where `T` is a type it takes.
        for item in &items {
            if let Item::Task(task) = item
                && task.span.file == 0
                && task.given.is_empty()
            {
                // A method's value is no parameter the file writes: it sits at
                // the method's name.
                for param in task.params.iter().filter(|param| param.name != THIS) {
                    checked.declare(param.span, param.ty.clone());
                }
                // A named result is a variable of the task, with the type the
                // header gives it.
                for (result, ty) in task.results.iter().zip(task.ret.results()) {
                    if let Some((_, at)) = &result.name {
                        checked.declare(*at, ty.clone());
                    }
                }
                checked.block(&task.body, &declarations);
            }
        }
        // A shape the compiler made for the types it was given is written as
        // those types (`Box<int>`), the way the program writes it.
        let made: HashMap<&str, &ShapeDef> = items
            .iter()
            .filter_map(|item| match item {
                Item::Shape(s) if !s.given.is_empty() => Some((s.name.as_str(), s)),
                _ => None,
            })
            .collect();
        // The program names another module's shapes and kinds by the module's
        // path (`shapes/geometry.Point`); this file names them as it writes them.
        for ty in checked.exprs.values_mut().chain(checked.decls.values_mut()) {
            *ty = spelling.ty(&as_written(ty, &made));
        }
        for shape in checked.field_accesses.values_mut() {
            *shape = spelling.name(&as_written_name(shape, &made));
        }
        checked
    }

    /// Records the type of a name written at `at`. A type the checker couldn't
    /// work out is no type to show, so it is left out and the editor falls back
    /// to what the source itself says (see `ast::Type::Unknown`).
    fn declare(&mut self, at: Span, ty: Type) {
        if !ty.unknown() {
            self.decls.insert(pos(at), ty);
        }
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
                self.declare(stmt.span, Type::Region);
            }
            StmtKind::Binding { names, init, .. } => {
                for bound in names.iter().flat_map(vela::ast::Bound::leaves) {
                    if let Some(ty) = &bound.ty {
                        self.declare(bound.span, ty.clone());
                    }
                }
                init.iter().for_each(|value| self.expr(value, kinds));
            }
            StmtKind::Assign(targets, values) => {
                for target in targets {
                    self.target(target, kinds);
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
                    self.declare(stmt.span, item);
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
                for (names, field) in binds.iter().zip(payload.into_iter().flatten()) {
                    let ty = match region {
                        Some(region) => in_region(&field.ty, region, &kinds.takes_region),
                        None => field.ty.clone(),
                    };
                    self.pattern_names(names, &ty);
                }
            }
            (PatternKind::Value(names), Type::Maybe(inner) | Type::Or(inner, _)) => self.pattern_names(names, inner),
            (PatternKind::Fail(names), Type::Or(_, error)) => self.pattern_names(names, error),
            // `(x, y)`: the items of the tuple itself.
            (PatternKind::Tuple(names), _) => self.pattern_names(names, subject),
            _ => {}
        }
    }

    /// Records the type of each name `names` binds from a value of type `ty`.
    fn pattern_names(&mut self, names: &vela::ast::Names, ty: &Type) {
        if names.parts.is_empty() {
            self.declare(names.span, ty.clone());
            return;
        }
        let Type::Tuple(items) = ty else { return };
        for (part, item) in names.parts.iter().zip(items) {
            self.pattern_names(part, item);
        }
    }

    fn target(&mut self, target: &Target, kinds: &Declarations) {
        match target {
            Target::Discard(_) => {}
            Target::Place(place) => self.expr(place, kinds),
            Target::Tuple(parts, _) => parts.iter().for_each(|part| self.target(part, kinds)),
        }
    }

    fn expr(&mut self, e: &Expr, kinds: &Declarations) {
        // A wrapped value keeps its own type at its position.
        if let ExprKind::Wrap(inner) = &e.kind {
            return self.expr(inner, kinds);
        }
        // So does a value given to `show` that the checker turned into the call
        // of its own `str` (see `check::show_with_str`): the call stands where
        // the value is written, and the editor asks about the value there, not
        // about the text it becomes. No program can name that region, so no
        // call a program writes looks like this one.
        if let ExprKind::Call { args, .. } = &e.kind
            && let [value, region] = args.as_slice()
            && matches!(&region.kind, ExprKind::Var(name) if name == SHOWN)
        {
            return self.expr(value, kinds);
        }
        if let Some(ty) = &e.ty
            && !ty.unknown()
        {
            self.exprs.entry(pos(e.span)).or_insert_with(|| ty.clone());
        }
        match &e.kind {
            ExprKind::Field(base, _) => {
                match &base.ty {
                    Some(Type::Named(shape, _)) => {
                        self.field_accesses.insert(pos(e.span), shape.clone());
                    }
                    // `len`, and a tuple's items: members no declaration names.
                    Some(Type::Array(..) | Type::Slice(..) | Type::Tuple(_)) => {
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
            // `c.step()`, which the checker made a call of the task the method
            // is, at the method's name: the name after the dot is a member of
            // the shape or kind before `__on__` (no Vela name holds `__`), as
            // written without the types a made one was given.
            ExprKind::Call { name, args, .. } if let Some((owner, _)) = method_parts(name) => {
                let owner = owner.split("__of__").next().unwrap_or(owner);
                self.field_accesses.insert(pos(e.span), owner.to_string());
                for arg in args {
                    self.expr(arg, kinds);
                }
            }
            ExprKind::Call { args, .. } | ExprKind::Construct { args, .. } | ExprKind::Array(args) | ExprKind::Tuple(args) => {
                for arg in args {
                    self.expr(arg, kinds);
                }
            }
            ExprKind::StructInit { fields, .. } => {
                for field in fields {
                    self.expr(&field.value, kinds);
                }
            }
            ExprKind::Fail(inner) | ExprKind::Wrap(inner) => self.expr(inner, kinds),
            ExprKind::PlanB { value, error, body } => {
                self.expr(value, kinds);
                if let (Some((_, span)), Some(Type::Or(_, error_ty))) = (error, &value.ty) {
                    self.declare(*span, (**error_ty).clone());
                }
                self.block(body, kinds);
            }
            ExprKind::Scan { args, .. } | ExprKind::Join { parts: args, .. } => {
                for arg in args {
                    self.expr(arg, kinds);
                }
            }
            // `x.copy()`: the word is a member of every value, as `len` is of a slice.
            ExprKind::Copy { value, .. } => {
                self.field_accesses.insert(pos(e.span), String::new());
                self.expr(value, kinds);
            }
            // `n.str() at r` is a member too, written after its value; `text(n)
            // at r` is written before it, and its word is the type.
            ExprKind::ToText { value, .. } => {
                if (value.span.line, value.span.col) < (e.span.line, e.span.col) {
                    self.field_accesses.insert(pos(e.span), String::new());
                }
                self.expr(value, kinds);
            }
            // `p.area()`: the checker turns a method call into the call of the
            // task the method is, so one is left only where a file doesn't
            // check. The name after the dot is a member of the value's type.
            ExprKind::MethodCall { value, args, .. } => {
                if let Some(Type::Named(owner, _)) = &value.ty {
                    self.field_accesses.insert(pos(e.span), owner.clone());
                }
                self.expr(value, kinds);
                for arg in args {
                    self.expr(arg, kinds);
                }
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
