use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;

use vela::ast::{
    Applied, Block, Expr, ExprKind, Item, KindDef, Pattern, PatternKind, SHOWN, START_VALUES, Span,
    Stmt, StmtKind, THIS, Target, Type, TypedForm, method_parts,
};
use vela::check::{in_region, regional_types, textual_types};
use vela::driver;
use vela::error::Error;
use vela::modules::{self, SourceFile};

#[derive(Debug, Clone, Copy)]
pub(crate) struct Context<'a> {
    pub path: &'a str,
    pub sources: &'a HashMap<String, String>,
}

/// Restores compiler-specialized names (`Box__of__int` -> `Box<int>`); Vela identifiers cannot hold `__`.
fn as_written(ty: &Type, made: &HashMap<&str, &[Type]>) -> Type {
    match ty {
        Type::Named(name, region) => match made.get(name.as_str()) {
            Some(given) => Type::Applied(Box::new(Applied {
                name: as_written_name(name, made),
                types: given.iter().map(|ty| as_written(ty, made)).collect(),
                region: region.clone(),
            })),
            None => ty.clone(),
        },
        Type::Ptr(inner, region) => Type::Ptr(Box::new(as_written(inner, made)), region.clone()),
        Type::Slice(inner, region) => {
            Type::Slice(Box::new(as_written(inner, made)), region.clone())
        }
        Type::Array(inner, count) => Type::Array(Box::new(as_written(inner, made)), *count),
        Type::Range(inner) => Type::Range(Box::new(as_written(inner, made))),
        Type::Maybe(inner) => Type::Maybe(Box::new(as_written(inner, made))),
        Type::Fail(inner) => Type::Fail(Box::new(as_written(inner, made))),
        Type::Or(value, error) => Type::Or(
            Box::new(as_written(value, made)),
            Box::new(as_written(error, made)),
        ),
        Type::Tuple(items) => Type::Tuple(items.iter().map(|ty| as_written(ty, made)).collect()),
        Type::Results(types) => {
            Type::Results(types.iter().map(|ty| as_written(ty, made)).collect())
        }
        Type::Applied(applied) => Type::Applied(Box::new(Applied {
            name: as_written_name(&applied.name, made),
            types: applied
                .types
                .iter()
                .map(|ty| as_written(ty, made))
                .collect(),
            region: applied.region.clone(),
        })),
        other => other.clone(),
    }
}

fn as_written_name(name: &str, made: &HashMap<&str, &[Type]>) -> String {
    match made.contains_key(name) {
        true => name.split("__of__").next().unwrap_or(name).to_string(),
        false => name.to_string(),
    }
}

pub(crate) fn slashed(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

struct Declarations<'a> {
    kinds: HashMap<&'a str, &'a KindDef>,
    takes_region: HashSet<String>,
}

pub(crate) type Pos = (usize, usize);

fn pos(span: Span) -> Pos {
    (span.line, span.col)
}

#[derive(Default)]
pub(crate) struct Checked {
    pub errors: Vec<Error>,
    pub warnings: Vec<Error>,
    pub files: Vec<SourceFile>,
    pub missing: Vec<String>,
    /// Keyed by identifying token (line, col): identifier, operator, `^`, `new`, or shape literal name.
    pub exprs: HashMap<Pos, Type>,
    pub decls: HashMap<Pos, Type>,
    /// Keyed by member token (line, col); `""` denotes built-in members such as `len`.
    pub field_accesses: HashMap<Pos, String>,
}

impl Checked {
    pub fn new(source: &str, context: Context) -> Checked {
        let path = Path::new(context.path);
        let mut absent = false;
        let mut loader = |file: &Path| {
            let found = context.sources.get(&slashed(file)).cloned();
            absent |= found.is_none();
            found.ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        };
        let partial = driver::front_end_program(path, source, &mut loader);
        // Re-run discovery with empty stubs when anything is missing so the editor can fetch all paths at once.
        let mut missing = Vec::new();
        if absent {
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
        }
        let spelling = partial.spelling;
        let mut checked = Checked {
            errors: partial.errors,
            warnings: partial.warnings,
            files: partial.files,
            missing,
            ..Checked::default()
        };
        let Some(items) = partial.items else {
            return checked;
        };
        let kinds: HashMap<&str, &KindDef> = items
            .iter()
            .filter_map(|item| match item {
                Item::Kind(k) => Some((k.name.as_str(), k)),
                _ => None,
            })
            .collect();
        let declarations = Declarations {
            kinds,
            takes_region: regional_types(&items)
                .union(&textual_types(&items))
                .cloned()
                .collect(),
        };
        // Record main-file positions only; skip monomorphized tasks whose spans echo the generic body.
        for item in &items {
            if let Item::Macro(m) = item
                && m.span.file == 0
                && let Some(ty) = &m.value.ty
            {
                checked.declare(m.span, ty.clone());
            }
            if let Item::Task(task) = item
                && task.span.file == 0
                && task.given.is_empty()
            {
                // Receiver `this` sits on the method itself rather than a written parameter.
                for param in task.params.iter().filter(|param| param.name != THIS) {
                    checked.declare(param.span, param.ty.clone());
                }
                for (result, ty) in task.results.iter().zip(task.ret.results()) {
                    if let Some((_, at)) = &result.name {
                        checked.declare(*at, ty.clone());
                    }
                }
                checked.block(&task.body, &declarations);
            }
            // Wrapper defaults were hoisted into `START_VALUES`.
            if let Item::Shape(values) = item
                && values.name == START_VALUES
            {
                for member in values
                    .shared
                    .iter()
                    .filter(|member| member.field.span.file == 0)
                {
                    if let Some(value) = &member.field.default {
                        checked.expr(value, &declarations);
                    }
                }
            }
        }
        let made: HashMap<&str, &[Type]> = items
            .iter()
            .filter_map(|item| match item {
                Item::Shape(s) if !s.given.is_empty() => {
                    Some((s.name.as_str(), s.given.as_slice()))
                }
                Item::Kind(k) if !k.given.is_empty() => Some((k.name.as_str(), k.given.as_slice())),
                _ => None,
            })
            .collect();
        // Unmangle compiler-specialized types and foreign module prefixes back to source spelling.
        for ty in checked.exprs.values_mut().chain(checked.decls.values_mut()) {
            *ty = spelling.ty(&as_written(ty, &made));
        }
        for shape in checked.field_accesses.values_mut() {
            *shape = spelling.name(&as_written_name(shape, &made));
        }
        checked
    }

    /// Omit `Unknown` so the editor falls back to source declarations.
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
            StmtKind::Step { target, value, .. } => {
                self.expr(target, kinds);
                self.expr(value, kinds);
            }
            StmtKind::Send(values) | StmtKind::SetResults(_, values) => {
                values.iter().for_each(|value| self.expr(value, kinds))
            }
            StmtKind::Region { names } => {
                for decl in names {
                    self.declare(decl.span, Type::Region);
                }
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
            StmtKind::When(arms, else_block) => {
                for arm in arms {
                    for part in arm.cond.parts() {
                        self.expr(&part.value, kinds);
                        if let Some(pattern) = &part.pattern {
                            self.pattern(part.value.ty.as_ref(), pattern, kinds);
                        }
                    }
                    self.block(&arm.body, kinds);
                }
                if let Some(else_block) = else_block {
                    self.block(else_block, kinds);
                }
            }
            StmtKind::Block(body) => self.block(body, kinds),
            StmtKind::Turns(turns) => turns.iter().for_each(|turn| self.block(turn, kinds)),
            StmtKind::Named(_, inner) => self.statement(inner, kinds),
            StmtKind::Break(_, value) => value.iter().for_each(|value| self.expr(value, kinds)),
            StmtKind::Continue(_) => {}
            StmtKind::Loop(cond, body, finally) => {
                for part in cond.parts() {
                    self.expr(&part.value, kinds);
                    if let Some(pattern) = &part.pattern {
                        self.pattern(part.value.ty.as_ref(), pattern, kinds);
                    }
                }
                self.block(body, kinds);
                if let Some(finally) = finally {
                    self.block(finally, kinds);
                }
            }
            StmtKind::Parted(parted) => {
                self.block(&parted.start, kinds);
                for part in parted.cond.iter().flat_map(|cond| cond.parts()) {
                    self.expr(&part.value, kinds);
                    if let Some(pattern) = &part.pattern {
                        self.pattern(part.value.ty.as_ref(), pattern, kinds);
                    }
                }
                self.block(&parted.step, kinds);
                self.block(&parted.body, kinds);
                if let Some(finally) = &parted.finally {
                    self.block(finally, kinds);
                }
            }
            StmtKind::Each(_, second, range, body, finally) => {
                let (item, value) = match (&range.kind, &range.ty) {
                    (ExprKind::Range(..), _) => (Some(Type::Int), None),
                    (_, Some(Type::Array(items, _) | Type::Slice(items, _))) => {
                        (Some((**items).clone()), None)
                    }
                    (_, Some(Type::Map(key, value, _))) => {
                        (Some((**key).clone()), Some((**value).clone()))
                    }
                    _ => (None, None),
                };
                if let Some(item) = item {
                    self.declare(stmt.span, item);
                }
                if let (Some((_, at)), Some(value)) = (second, value) {
                    self.declare(*at, value);
                }
                self.expr(range, kinds);
                self.block(body, kinds);
                if let Some(finally) = finally {
                    self.block(finally, kinds);
                }
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

    fn pattern(&mut self, subject: Option<&Type>, pattern: &Pattern, kinds: &Declarations) {
        let Some(subject) = subject else { return };
        match (&pattern.kind, subject) {
            (PatternKind::Variant { tag, binds, .. }, Type::Named(name, region)) => {
                let payload = kinds
                    .kinds
                    .get(name.as_str())
                    .and_then(|k| k.variants.iter().find(|v| v.tag == *tag))
                    .map(|v| &v.payload);
                for (names, field) in binds.iter().zip(payload.into_iter().flatten()) {
                    let ty = match region {
                        Some(region) => in_region(&field.ty, region, &kinds.takes_region),
                        None => field.ty.clone(),
                    };
                    self.pattern_names(names, &ty);
                }
            }
            (PatternKind::Value(names), Type::Maybe(inner) | Type::Or(inner, _)) => {
                self.pattern_names(names, inner)
            }
            (PatternKind::Fail(names), Type::Or(_, error)) => self.pattern_names(names, error),
            (PatternKind::Tuple(names), _) => self.pattern_names(names, subject),
            (
                PatternKind::Typed {
                    ty,
                    form: TypedForm::Named(names),
                    ..
                },
                _,
            ) => self.pattern_names(names, ty),
            (
                PatternKind::Typed {
                    ty: Type::Maybe(inner),
                    form: TypedForm::Value(names),
                    ..
                },
                _,
            ) => self.pattern_names(names, inner),
            _ => {}
        }
    }

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
        // Wrapper/widening/pipe nodes keep the inner expression's type at its position.
        if let ExprKind::Wrap(inner) | ExprKind::Widen(inner) | ExprKind::Piped(inner) = &e.kind {
            return self.expr(inner, kinds);
        }
        // `show(x)` lowers to `str(x, SHOWN)`; unwrap so hover inspects `x` rather than text.
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
                    Some(Type::Array(..) | Type::Slice(..) | Type::Map(..) | Type::Tuple(_)) => {
                        self.field_accesses.insert(pos(e.span), String::new());
                    }
                    _ => {}
                }
                self.expr(base, kinds);
            }
            ExprKind::Spread(inner) => self.expr(inner, kinds),
            ExprKind::Shared { shape, .. } if shape != START_VALUES => {
                self.field_accesses.insert(pos(e.span), shape.clone());
            }
            ExprKind::Shared { .. } => {}
            ExprKind::TaskValue(name) if let Some((owner, _)) = method_parts(name) => {
                self.field_accesses.insert(pos(e.span), owner.to_string());
            }
            ExprKind::IntLit(_)
            | ExprKind::FloatLit(_)
            | ExprKind::CharLit(_)
            | ExprKind::BoolLit(_)
            | ExprKind::TextLit(_)
            | ExprKind::NoneLit
            | ExprKind::Macro { .. }
            | ExprKind::Var(_)
            | ExprKind::RegionAfter(_)
            | ExprKind::TypeOf(_)
            | ExprKind::TaskValue(_)
            | ExprKind::Closure { .. } => {}
            ExprKind::Kept { value, .. } => self.expr(value, kinds),
            ExprKind::Invoke { callee, args } => {
                self.expr(callee, kinds);
                for arg in args {
                    self.expr(arg, kinds);
                }
            }
            ExprKind::Unary(_, inner)
            | ExprKind::Deref(inner)
            | ExprKind::Ref(inner)
            | ExprKind::New { value: inner, .. }
            | ExprKind::Shortcut { value: inner, .. } => self.expr(inner, kinds),
            ExprKind::Index(a, b)
            | ExprKind::Lookup(a, b)
            | ExprKind::Binary(_, a, b)
            | ExprKind::Range(a, b)
            | ExprKind::Repeat { value: a, count: b } => {
                self.expr(a, kinds);
                self.expr(b, kinds);
            }
            // Lowered method calls use `Owner__on__method`; strip `__of__` args from specialized types.
            ExprKind::Call { name, args, .. } if let Some((owner, _)) = method_parts(name) => {
                let owner = owner.split("__of__").next().unwrap_or(owner);
                self.field_accesses.insert(pos(e.span), owner.to_string());
                for arg in args {
                    self.expr(arg, kinds);
                }
            }
            ExprKind::Pairs(pairs) => {
                for (key, value) in pairs {
                    self.expr(key, kinds);
                    self.expr(value, kinds);
                }
            }
            ExprKind::MapOp { map, key, .. } => {
                self.expr(map, kinds);
                if let Some(key) = key {
                    self.expr(key, kinds);
                }
            }
            ExprKind::Call { args, .. }
            | ExprKind::Construct { args, .. }
            | ExprKind::Array(args)
            | ExprKind::Tuple(args) => {
                for arg in args {
                    self.expr(arg, kinds);
                }
            }
            ExprKind::StructInit { fields, .. } => {
                for field in fields {
                    self.expr(&field.value, kinds);
                }
            }
            ExprKind::Fail(inner)
            | ExprKind::Wrap(inner)
            | ExprKind::Widen(inner)
            | ExprKind::Pipe(inner)
            | ExprKind::Piped(inner) => self.expr(inner, kinds),
            ExprKind::PlanB { value, error, body } => {
                self.expr(value, kinds);
                if let (Some((_, span)), Some(Type::Or(_, error_ty))) = (error, &value.ty) {
                    self.declare(*span, (**error_ty).clone());
                }
                self.block(body, kinds);
            }
            ExprKind::Choice(stmt) => self.statement(stmt, kinds),
            ExprKind::LoopValue(value) => self.statement(&value.stmt, kinds),
            ExprKind::Scan { args, .. } | ExprKind::Join { parts: args, .. } => {
                for arg in args {
                    self.expr(arg, kinds);
                }
            }
            ExprKind::Copy { value, .. } => {
                self.field_accesses.insert(pos(e.span), String::new());
                self.expr(value, kinds);
            }
            // Only method-form `n.str()` precedes its receiver in column order; `text(n)` does not.
            ExprKind::ToText { value, .. } | ExprKind::ToJson { value, .. } => {
                if (value.span.line, value.span.col) < (e.span.line, e.span.col) {
                    self.field_accesses.insert(pos(e.span), String::new());
                }
                self.expr(value, kinds);
            }
            // Only un-checked files still contain `MethodCall`.
            ExprKind::MethodCall { value, args, .. } => {
                if let Some(Type::Named(owner, _)) = &value.ty {
                    self.field_accesses.insert(pos(e.span), owner.clone());
                }
                self.expr(value, kinds);
                for arg in args {
                    self.expr(arg, kinds);
                }
            }
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
