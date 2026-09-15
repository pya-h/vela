use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::error::{Error, Result};

#[derive(Clone)]
struct VariantInfo {
    /// The kind the variant belongs to.
    kind: String,
    variant: Variant,
}

#[derive(Clone)]
struct VarInfo {
    ty: Type,
    mutable: bool,
    /// Where the variable was declared.
    span: Span,
}

/// Type-checks a program and annotates it for code generation: every
/// expression gets its `ty`, and every binding without an annotation gets its
/// inferred type. Code generation relies on these annotations.
#[derive(Default)]
pub struct Checker {
    shapes: HashMap<String, ShapeDef>,
    kinds: HashMap<String, KindDef>,
    tasks: HashMap<String, (Vec<Type>, Type)>,
    /// Every variant, by tag. Tags are unique across all kinds.
    variants: HashMap<String, VariantInfo>,
    scopes: Vec<HashMap<String, VarInfo>>,
    current_ret: Option<Type>,
}

impl Checker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn check_file(&mut self, items: &mut [Item]) -> Result<()> {
        self.collect_declarations(items)?;
        self.check_declarations(items)?;
        for item in items.iter_mut() {
            if let Item::Task(t) = item {
                self.check_task(t)?;
            }
        }
        Ok(())
    }

    /// Registers every task, shape, kind, and variant. They share one
    /// namespace: no two may have the same name, and none may use the name of
    /// a builtin type or task.
    fn collect_declarations(&mut self, items: &[Item]) -> Result<()> {
        let mut names: HashMap<String, Span> = HashMap::new();
        let mut declare = |name: &str, span: Span| -> Result<()> {
            // No declaration may take the name of a builtin type or task.
            if Type::builtin(name).is_some() {
                return err(span, format!("'{}' is a builtin type and cannot be redefined", name));
            }
            if BUILTIN_TASKS.contains(&name) {
                return err(span, format!("'{}' is a builtin task and cannot be redefined", name));
            }
            if let Some(at) = names.get(name) {
                return err(span, format!("'{}' is already defined at {}:{}", name, at.line, at.col));
            }
            names.insert(name.to_string(), span);
            Ok(())
        };
        for item in items {
            match item {
                Item::Shape(s) => {
                    declare(&s.name, s.span)?;
                    self.shapes.insert(s.name.clone(), s.clone());
                }
                Item::Kind(k) => {
                    declare(&k.name, k.span)?;
                    for v in &k.variants {
                        declare(&v.tag, v.span)?;
                        let info = VariantInfo { kind: k.name.clone(), variant: v.clone() };
                        self.variants.insert(v.tag.clone(), info);
                    }
                    self.kinds.insert(k.name.clone(), k.clone());
                }
                Item::Task(t) => {
                    declare(&t.name, t.span)?;
                    let ptypes: Vec<Type> = t.params.iter().map(|p| p.ty.clone()).collect();
                    self.tasks.insert(t.name.clone(), (ptypes, t.ret.clone()));
                }
            }
        }
        Ok(())
    }

    /// Checks the declarations themselves: every written type exists, field
    /// names are distinct, kinds have variants, and no type contains itself.
    fn check_declarations(&self, items: &[Item]) -> Result<()> {
        for item in items {
            match item {
                Item::Shape(s) => self.check_fields(&s.fields)?,
                Item::Kind(k) => {
                    if k.variants.is_empty() {
                        return err(k.span, format!("kind '{}' has no variants", k.name));
                    }
                    for v in &k.variants {
                        self.check_fields(&v.payload)?;
                    }
                }
                Item::Task(t) => {
                    for param in &t.params {
                        self.check_type(&param.ty, param.ty_span)?;
                    }
                    if t.ret != Type::Unit {
                        self.check_type(&t.ret, t.ret_span)?;
                    }
                    if t.name == "main" {
                        if let Some(param) = t.params.first() {
                            return err(param.span, "task 'main' cannot take parameters");
                        }
                        if t.ret != Type::Unit {
                            return err(t.ret_span, "task 'main' cannot return a value");
                        }
                    }
                }
            }
        }
        let mut acyclic = HashSet::new();
        for item in items {
            let name = match item {
                Item::Shape(s) => &s.name,
                Item::Kind(k) => &k.name,
                Item::Task(_) => continue,
            };
            self.check_value_cycle(name, &mut Vec::new(), &mut acyclic)?;
        }
        self.check_constructible(items)
    }

    /// Checks a shape's fields or a variant's payload.
    fn check_fields(&self, fields: &[Field]) -> Result<()> {
        let mut seen: HashMap<&str, Span> = HashMap::new();
        for field in fields {
            if let Some(at) = seen.get(field.name.as_str()) {
                return err(field.span, format!("field '{}' is already defined at {}:{}", field.name, at.line, at.col));
            }
            seen.insert(&field.name, field.span);
            self.check_type(&field.ty, field.ty_span)?;
        }
        Ok(())
    }

    /// A type written in the source must be a builtin type, a shape, or a kind.
    /// `unit` is only valid as a return type, which is checked separately, and
    /// as the value side of `unit or E`.
    fn check_type(&self, ty: &Type, span: Span) -> Result<()> {
        match ty {
            Type::Unit => err(span, "unit is only allowed as a return type"),
            Type::Ptr(inner) => self.check_type(inner, span),
            Type::Maybe(inner) => match inner.as_ref() {
                Type::Maybe(_) => err(span, "a maybe can't hold another maybe"),
                Type::Unit => err(span, "a maybe can't hold unit, which has no value"),
                _ => self.check_type(inner, span),
            },
            Type::Or(value, error) => {
                if **value != Type::Unit {
                    self.check_type(value, span)?;
                }
                match error.as_ref() {
                    Type::Unit => return err(span, "the error type can't be unit"),
                    Type::Maybe(_) => return err(span, "the error type can't be a maybe"),
                    _ => self.check_type(error, span)?,
                }
                if value == error {
                    return err(span, format!("the error type must differ from the value type, as in '{} or ParseError'", value));
                }
                Ok(())
            }
            Type::Named(name) if !self.shapes.contains_key(name) && !self.kinds.contains_key(name) => {
                err(span, format!("unknown type '{}'", name))
            }
            _ => Ok(()),
        }
    }

    /// Rejects a shape or kind that contains itself by value, directly or
    /// through other types: its values would have infinite size. Containing a
    /// pointer to itself (`maybe ^T`) is fine. `path` holds the types being
    /// entered; `acyclic` the types already known to be free of cycles.
    fn check_value_cycle(&self, name: &str, path: &mut Vec<String>, acyclic: &mut HashSet<String>) -> Result<()> {
        if acyclic.contains(name) {
            return Ok(());
        }
        path.push(name.to_string());
        let fields: Vec<&Field> = match (self.shapes.get(name), self.kinds.get(name)) {
            (Some(shape), _) => shape.fields.iter().collect(),
            (None, Some(kind)) => kind.variants.iter().flat_map(|v| &v.payload).collect(),
            (None, None) => Vec::new(),
        };
        for field in fields {
            for target in contained_by_value(&field.ty) {
                if let Some(start) = path.iter().position(|n| n == target) {
                    let chain: Vec<&str> = path.iter().skip(start).map(String::as_str).chain([target]).collect();
                    return err(
                        field.ty_span,
                        format!("'{}' contains itself by value ({}); use a pointer such as 'maybe ^{}'", target, chain.join(" -> "), target),
                    );
                }
                self.check_value_cycle(target, path, acyclic)?;
            }
        }
        path.pop();
        acyclic.insert(name.to_string());
        Ok(())
    }

    /// Rejects a shape or kind that could never be created. A pointer can't be
    /// `none`, so a shape whose field points to its own type (directly or
    /// through other types) would need one to exist before the first is made.
    /// A kind can be created if any of its variants can.
    fn check_constructible(&self, items: &[Item]) -> Result<()> {
        let mut can: HashSet<&str> = HashSet::new();
        loop {
            let mut changed = false;
            for item in items {
                let (name, ok) = match item {
                    Item::Shape(s) => (s.name.as_str(), s.fields.iter().all(|f| constructible(&f.ty, &can))),
                    Item::Kind(k) => (k.name.as_str(), k.variants.iter().any(|v| v.payload.iter().all(|f| constructible(&f.ty, &can)))),
                    Item::Task(_) => continue,
                };
                if ok && can.insert(name) {
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        for item in items {
            match item {
                Item::Shape(s) if !can.contains(s.name.as_str()) => {
                    if let Some(field) = s.fields.iter().find(|f| !constructible(&f.ty, &can)) {
                        return err(
                            field.ty_span,
                            format!(
                                "'{}' can never be created: field '{}' needs an existing {}; make it 'maybe {}'",
                                s.name, field.name, field.ty, field.ty
                            ),
                        );
                    }
                }
                Item::Kind(k) if !can.contains(k.name.as_str()) => {
                    return err(
                        k.span,
                        format!("'{}' can never be created: every variant needs an existing value that can't be created either; use 'maybe' for a value that can be missing", k.name),
                    );
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn check_task(&mut self, task: &mut TaskDef) -> Result<()> {
        self.current_ret = Some(task.ret.clone());
        // Parameters and the task body share one scope, as in C.
        self.scopes = vec![HashMap::new()];
        for param in &task.params {
            self.define(&param.name, param.ty.clone(), false, param.span)?;
        }
        self.check_block(&mut task.body)?;
        self.scopes.clear();
        self.current_ret = None;
        if sends_value(&task.ret) && !always_sends(&task.body) {
            return err(task.span, format!("task '{}' can finish without sending a value", task.name));
        }
        Ok(())
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// Checks `block` in a new nested scope.
    fn check_scoped_block(&mut self, block: &mut Block) -> Result<()> {
        self.push_scope();
        let result = self.check_block(block);
        self.pop_scope();
        result
    }

    /// Checks `block` in a new nested scope that first declares `bindings`
    /// (from a pattern), each immutable. `_` declares nothing.
    fn check_block_with(&mut self, bindings: Vec<(String, Span, Type)>, block: &mut Block) -> Result<()> {
        self.push_scope();
        let result = bindings
            .into_iter()
            .filter(|(name, _, _)| name != DISCARD)
            .try_for_each(|(name, span, ty)| self.define(&name, ty, false, span))
            .and_then(|()| self.check_block(block));
        self.pop_scope();
        result
    }

    /// Declares a variable in the innermost scope. Vela has no shadowing: a
    /// name cannot be declared while another variable of that name is visible.
    /// (Sibling blocks may reuse names, since neither is visible to the other.)
    fn define(&mut self, name: &str, ty: Type, mutable: bool, span: Span) -> Result<()> {
        // A variable named like a variant would make a bare `Empty` ambiguous.
        if let Some(info) = self.variants.get(name) {
            let at = info.variant.span;
            return err(span, format!("'{}' is already defined at {}:{}", name, at.line, at.col));
        }
        if let Some(existing) = self.lookup(name) {
            let at = existing.span;
            return err(span, format!("'{}' is already defined at {}:{}", name, at.line, at.col));
        }
        let Some(scope) = self.scopes.last_mut() else {
            return Err(Error::internal(format!("'{}' defined outside of a task", name)));
        };
        scope.insert(name.to_string(), VarInfo { ty, mutable, span });
        Ok(())
    }

    fn lookup(&self, name: &str) -> Option<&VarInfo> {
        for scope in self.scopes.iter().rev() {
            if let Some(info) = scope.get(name) {
                return Some(info);
            }
        }
        None
    }

    fn lookup_or_err(&self, name: &str, span: Span) -> Result<&VarInfo> {
        self.lookup(name).ok_or_else(|| {
            if self.variants.contains_key(name) {
                Error::at(span, format!("'{}' is a variant, not a variable", name))
            } else {
                Error::at(span, format!("unknown variable '{}'", name))
            }
        })
    }

    fn check_block(&mut self, block: &mut [Stmt]) -> Result<()> {
        for stmt in block {
            self.check_stmt(stmt)?;
        }
        Ok(())
    }

    fn check_stmt(&mut self, stmt: &mut Stmt) -> Result<()> {
        let span = stmt.span;
        match &mut stmt.kind {
            StmtKind::Expr(e) => {
                self.check_expr(e)?;
                // Of all expressions, only a call can have an effect, and a
                // plan B that gives no value only runs for its effect.
                match &e.kind {
                    ExprKind::Call(name, _) if matches!(e.ty, Some(Type::Or(..))) => err(
                        e.span,
                        format!("the error '{}' may send is ignored; handle it with a plan B, as in '{}(…) {{ fail e … }}', or with match", name, name),
                    ),
                    ExprKind::Call(..) => Ok(()),
                    ExprKind::PlanB { .. } if e.ty == Some(Type::Unit) => Ok(()),
                    ExprKind::Binary(BinaryOp::Eq, ..) => err(e.span, "the result of '==' is unused; use '=' to assign"),
                    _ => err(e.span, "this value is unused; only a task call can be used as a statement"),
                }
            }
            StmtKind::Binding { name, mutable, ty, ty_span, init } => {
                if let Some(annotated) = ty {
                    self.check_type(annotated, ty_span.unwrap_or(span))?;
                }
                let init_ty = self.check_expr(init)?;
                let binding_ty = match ty {
                    Some(annotated) => {
                        self.coerce(init, annotated)?;
                        annotated.clone()
                    }
                    None => match init_ty {
                        Type::None => {
                            return err(init.span, format!("cannot infer the type of '{}' from none; give it a type, such as '{}: maybe int'", name, name));
                        }
                        Type::Fail(error) => {
                            return err(init.span, format!("cannot infer the type of '{}' from 'fail …'; give it a type, such as '{}: int or {}'", name, name, error));
                        }
                        Type::Unit => return err(init.span, "this expression has no value (its type is unit)"),
                        other => other,
                    },
                };
                *ty = Some(binding_ty.clone());
                self.define(name, binding_ty, *mutable, span)
            }
            StmtKind::Assign(lhs, rhs) => {
                let lhs_ty = self.check_assign_target(lhs)?;
                self.check_expr(rhs)?;
                self.coerce(rhs, &lhs_ty)
            }
            StmtKind::When(cond, then_block, else_block) => {
                self.check_condition(cond, then_block)?;
                if let Some(else_b) = else_block {
                    self.check_scoped_block(else_b)?;
                }
                Ok(())
            }
            StmtKind::Loop(cond, body) => self.check_condition(cond, body),
            StmtKind::Each(name, range, body) => {
                // A range is only valid here, so it is checked here rather than
                // as an ordinary expression.
                let ExprKind::Range(start, end) = &mut range.kind else {
                    return err(range.span, "each requires a range, such as 0..n");
                };
                for bound in [start, end] {
                    let bty = self.check_expr(bound)?;
                    self.expect_type(&bty, &Type::Int, bound.span)?;
                }
                range.ty = Some(Type::Range(Box::new(Type::Int)));
                // The loop variable and the body share one scope.
                self.push_scope();
                let result = self.define(name, Type::Int, false, span)
                    .and_then(|()| self.check_block(body));
                self.pop_scope();
                result
            }
            StmtKind::Match(expr, cases) => self.check_match(span, expr, cases),
            StmtKind::Send(expr) => {
                let ret = self.current_ret.clone().unwrap_or(Type::Unit);
                match expr {
                    Some(e) if ret == Type::Unit => err(e.span, "this task has no return type, so '<-' cannot take a value"),
                    Some(e) => {
                        let ety = self.check_expr(e)?;
                        if !sends_value(&ret) && !matches!(ety, Type::Fail(_)) && ety != ret {
                            return err(e.span, "this task sends no value when it succeeds; write a bare '<-', or '<- fail …' to send an error");
                        }
                        self.coerce(e, &ret)
                    }
                    None if sends_value(&ret) => err(span, "expected a value after '<-'"),
                    None => Ok(()),
                }
            }
            StmtKind::Free(e) => {
                let ety = self.check_expr(e)?;
                let pointer = match &ety {
                    Type::Maybe(inner) => inner.as_ref(),
                    other => other,
                };
                if pointer.ptr_inner().is_none() {
                    return err(e.span, "free requires a pointer");
                }
                Ok(())
            }
        }
    }

    /// Checks a `when` or `loop` condition and the block it guards, where the
    /// names an `is` pattern binds are visible.
    fn check_condition(&mut self, cond: &mut Condition, body: &mut Block) -> Result<()> {
        let ty = self.check_expr(&mut cond.value)?;
        let Some(pattern) = &cond.pattern else {
            self.expect_type(&ty, &Type::Bool, cond.value.span)?;
            return self.check_scoped_block(body);
        };
        if matches!(pattern.kind, PatternKind::Any) {
            return err(pattern.span, "'_' matches anything, so this condition would always hold");
        }
        let bindings = self.pattern_bindings(&ty, pattern, false)?;
        self.check_block_with(bindings, body)
    }

    /// Checks that `pattern` can match a value of type `subject`, and returns
    /// the names it binds with their types. `in_case` words messages for a
    /// `case` rather than an `is`.
    fn pattern_bindings(&self, subject: &Type, pattern: &Pattern, in_case: bool) -> Result<Vec<(String, Span, Type)>> {
        let span = pattern.span;
        let bound = |(name, at): &Binding, ty: &Type| vec![(name.clone(), *at, ty.clone())];
        match (&pattern.kind, subject) {
            (PatternKind::Any | PatternKind::None, Type::Maybe(_)) => Ok(Vec::new()),
            (PatternKind::Any, _) => Ok(Vec::new()),
            (PatternKind::Variant { tag, binds }, Type::Named(kind_name)) if self.kinds.contains_key(kind_name) => {
                let variant = self.kinds.get(kind_name)
                    .and_then(|kind| kind.variants.iter().find(|v| v.tag == *tag))
                    .ok_or_else(|| Error::at(span, format!("kind '{}' has no variant '{}'", kind_name, tag)))?;
                if binds.len() != variant.payload.len() {
                    let what = if in_case { format!("case '{}'", tag) } else { format!("'{}'", tag) };
                    return err(span, format!("{} expects {}, got {}", what, count(variant.payload.len(), "binding"), binds.len()));
                }
                Ok(binds.iter().zip(&variant.payload).map(|((name, at), field)| (name.clone(), *at, field.ty.clone())).collect())
            }
            (PatternKind::Variant { tag, .. }, other) => {
                if self.variants.contains_key(tag) {
                    err(span, format!("'{}' is a variant, but this value is {}", tag, other))
                } else {
                    err(span, format!("unknown variant '{}'", tag))
                }
            }
            (PatternKind::Value(bind), Type::Maybe(inner)) => Ok(bound(bind, inner)),
            (PatternKind::Value(bind), Type::Or(value, _)) => {
                if **value == Type::Unit && bind.0 != DISCARD {
                    return err(bind.1, "this success has no value to name; write '{_}'");
                }
                Ok(bound(bind, value))
            }
            (PatternKind::Fail(bind), Type::Or(_, error)) => Ok(bound(bind, error)),
            (PatternKind::Value(_), other) => {
                err(span, format!("'{{…}}' matches the value in a maybe or a 'T or E', but this value is {}", other))
            }
            (PatternKind::None, other) => err(span, format!("'none' matches an empty maybe, but this value is {}", other)),
            (PatternKind::Fail(_), other) => {
                err(span, format!("'fail …' matches the error of a 'T or E', but this value is {}", other))
            }
        }
    }

    /// Checks the left side of an assignment and returns its type.
    ///
    /// Assignable places are: a `hold` variable, a field of an assignable
    /// place, and anything behind a pointer (`p^`, `p^.x`). Writing through a
    /// pointer is allowed even when the pointer itself is a `bind`, because
    /// the binding holds the address, not the pointed-to value.
    fn check_assign_target(&mut self, expr: &mut Expr) -> Result<Type> {
        let span = expr.span;
        match &mut expr.kind {
            ExprKind::Var(name) => {
                let info = self.lookup_or_err(name, span)?;
                if !info.mutable {
                    return err(span, format!("cannot assign to immutable variable '{}'", name));
                }
                let ty = info.ty.clone();
                expr.ty = Some(ty.clone());
                Ok(ty)
            }
            ExprKind::Field(base, field) => {
                let base_ty = self.check_field_base(base)?;
                let ty = self.field_type(&base_ty, field, span)?;
                expr.ty = Some(ty.clone());
                Ok(ty)
            }
            ExprKind::Deref(_) => self.check_expr(expr),
            _ => err(span, "invalid assignment target"),
        }
    }

    /// Checks the base of an assigned field, which must itself be an
    /// assignable place.
    fn check_field_base(&mut self, base: &mut Expr) -> Result<Type> {
        let span = base.span;
        match &base.kind {
            ExprKind::Var(name) => {
                let info = self.lookup_or_err(name, span)?;
                // Only a shape has fields to assign; for anything else (a pointer,
                // a kind) the field error from `field_type` is the useful one.
                let is_shape = matches!(&info.ty, Type::Named(ty) if self.shapes.contains_key(ty));
                if is_shape && !info.mutable {
                    return err(span, format!("cannot assign to a field of immutable variable '{}'", name));
                }
                self.check_expr(base)
            }
            ExprKind::Field(..) | ExprKind::Deref(_) => self.check_assign_target(base),
            _ => err(span, "cannot assign to a field of a temporary value"),
        }
    }

    fn check_match(&mut self, span: Span, expr: &mut Expr, cases: &mut [Case]) -> Result<()> {
        let ety = self.check_expr(expr)?;
        // The cases that together cover every value.
        let needed: Vec<String> = match &ety {
            Type::Named(name) => match self.kinds.get(name) {
                Some(kind) => kind.variants.iter().map(|v| v.tag.clone()).collect(),
                None => return err(expr.span, format!("'{}' is not a kind", name)),
            },
            Type::Maybe(_) => vec!["{…}".to_string(), "none".to_string()],
            Type::Or(..) => vec!["{…}".to_string(), "fail …".to_string()],
            other => return err(expr.span, format!("match requires a kind, a maybe, or a 'T or E' value, got {}", other)),
        };
        let mut seen: Vec<String> = Vec::new();
        let mut matched_all = false;
        for case in cases {
            if matched_all {
                return err(case.pattern.span, "this case comes after '_', which already matches everything");
            }
            let bindings = self.pattern_bindings(&ety, &case.pattern, true)?;
            let label = case.pattern.label();
            if matches!(case.pattern.kind, PatternKind::Any) {
                if needed.iter().all(|n| seen.contains(n)) {
                    return err(case.pattern.span, "every case is already covered, so '_' never matches");
                }
                matched_all = true;
            } else if seen.contains(&label) {
                return err(case.pattern.span, format!("duplicate case '{}'", label));
            }
            seen.push(label);
            self.check_block_with(bindings, &mut case.body)?;
        }
        if !matched_all
            && let Some(missing) = needed.iter().find(|n| !seen.contains(n))
        {
            return err(span, format!("missing case '{}' in match", missing));
        }
        Ok(())
    }

    /// Checks an expression, records its type on the node, and returns it.
    fn check_expr(&mut self, expr: &mut Expr) -> Result<Type> {
        self.resolve_constructor(expr);
        self.resolve_empty_plan_b(expr);
        let ty = self.infer_expr(expr)?;
        expr.ty = Some(ty.clone());
        Ok(ty)
    }

    /// Rewrites a variant used as a value, `Empty`, or called, `Circle(1.0)`,
    /// into a `Construct` node. Variables can't share a variant's name, so a
    /// bare name is never ambiguous.
    fn resolve_constructor(&self, expr: &mut Expr) {
        let variant_kind = |tag: &str| self.variants.get(tag).map(|info| info.kind.clone());
        let resolved = match &mut expr.kind {
            ExprKind::Var(name) if self.lookup(name).is_none() => {
                variant_kind(name).map(|kind| (kind, std::mem::take(name), Vec::new()))
            }
            ExprKind::Call(name, args) => {
                variant_kind(name).map(|kind| (kind, std::mem::take(name), std::mem::take(args)))
            }
            _ => None,
        };
        if let Some((kind, tag, args)) = resolved {
            expr.kind = ExprKind::Construct { kind, tag, args };
        }
    }

    /// Rewrites `name {}` into an empty plan B when `name` is a variable rather
    /// than a shape: the parser reads both as a shape literal.
    fn resolve_empty_plan_b(&self, expr: &mut Expr) {
        let ExprKind::StructInit(name, fields) = &mut expr.kind else { return };
        if !fields.is_empty() || self.shapes.contains_key(name.as_str()) || self.lookup(name).is_none() {
            return;
        }
        let value = Expr::new(ExprKind::Var(std::mem::take(name)), expr.span);
        expr.kind = ExprKind::PlanB { value: Box::new(value), error: None, body: Vec::new() };
    }

    fn infer_expr(&mut self, expr: &mut Expr) -> Result<Type> {
        let span = expr.span;
        match &mut expr.kind {
            ExprKind::IntLit(_) => Ok(Type::Int),
            ExprKind::FloatLit(_) => Ok(Type::Float),
            ExprKind::BoolLit(_) => Ok(Type::Bool),
            ExprKind::TextLit(_) => Ok(Type::Text),
            ExprKind::NoneLit => Ok(Type::None),
            ExprKind::Var(name) => Ok(self.lookup_or_err(name, span)?.ty.clone()),
            ExprKind::Field(base, field) => {
                let base_ty = self.check_expr(base)?;
                self.field_type(&base_ty, field, span)
            }
            ExprKind::Index(_base, _index) => {
                // Arrays not supported yet
                err(span, "indexing not supported")
            }
            ExprKind::Call(name, args) => {
                if name == "show" {
                    let [arg] = args.as_mut_slice() else {
                        return err(span, "show expects 1 argument");
                    };
                    let ty = self.check_expr(arg)?;
                    let shown = match &ty {
                        Type::Maybe(inner) => inner.as_ref(),
                        other => other,
                    };
                    if !matches!(shown, Type::Int | Type::Float | Type::Bool | Type::Text) {
                        return err(arg.span, "show expects int, float, bool, or text");
                    }
                    return Ok(Type::Unit);
                }
                let (params, ret) = self.tasks.get(name.as_str())
                    .ok_or_else(|| {
                        // `send (x)` written the old way reads as a call.
                        let hint = if name == "send" { "; a task sends its result with '<-'" } else { "" };
                        Error::at(span, format!("unknown task '{}'{}", name, hint))
                    })?
                    .clone();
                if args.len() != params.len() {
                    return err(span, format!("task '{}' expects {}, got {}", name, count(params.len(), "argument"), args.len()));
                }
                for (arg, param) in args.iter_mut().zip(params.iter()) {
                    self.check_expr(arg)?;
                    self.coerce(arg, param)?;
                }
                Ok(ret)
            }
            ExprKind::Unary(op, e) => self.check_unary(*op, e, span),
            ExprKind::Binary(op, l, r) => self.check_binary(*op, l, r, span),
            ExprKind::Construct { kind, tag, args } => {
                let variant = self.variants.get(tag.as_str())
                    .map(|info| info.variant.clone())
                    .ok_or_else(|| Error::internal_at(span, format!("unknown variant '{}'", tag)))?;
                if args.len() != variant.payload.len() {
                    return err(span, format!("variant '{}' expects {}, got {}", tag, count(variant.payload.len(), "argument"), args.len()));
                }
                for (arg, field) in args.iter_mut().zip(variant.payload.iter()) {
                    self.check_expr(arg)?;
                    self.coerce(arg, &field.ty)?;
                }
                Ok(Type::Named(kind.clone()))
            }
            ExprKind::New { ty, ty_span, value } => {
                self.check_type(ty, *ty_span)?;
                self.check_expr(value)?;
                self.coerce(value, ty)?;
                Ok(Type::Ptr(Box::new(ty.clone())))
            }
            ExprKind::Deref(e) => {
                let ty = self.check_expr(e)?;
                if let Type::Maybe(inner) = &ty
                    && inner.ptr_inner().is_some()
                {
                    return err(span, format!("this pointer is a {}, which may be none; extract it first, as in 'when p is {{q}}' or 'p {{ … }}'", ty));
                }
                ty.ptr_inner().cloned()
                    .ok_or_else(|| Error::at(span, "cannot dereference non-pointer"))
            }
            ExprKind::StructInit(name, fields) => {
                if self.kinds.contains_key(name.as_str()) {
                    return err(span, format!("'{}' is a kind, not a shape; create its values with its variants", name));
                }
                let shape = self.shapes.get(name.as_str())
                    .ok_or_else(|| Error::at(span, format!("unknown shape '{}'", name)))?
                    .clone();
                // Fields may be given in any order, each exactly once.
                let mut given: HashMap<String, Span> = HashMap::new();
                for field in fields.iter_mut() {
                    let Some(declared) = shape.fields.iter().find(|f| f.name == field.name) else {
                        return err(field.span, format!("shape '{}' has no field '{}'", name, field.name));
                    };
                    if let Some(at) = given.get(&field.name) {
                        return err(field.span, format!("field '{}' is already given at {}:{}", field.name, at.line, at.col));
                    }
                    given.insert(field.name.clone(), field.span);
                    self.check_expr(&mut field.value)?;
                    self.coerce(&mut field.value, &declared.ty)?;
                }
                if let Some(missing) = shape.fields.iter().find(|f| !given.contains_key(&f.name)) {
                    return err(span, format!("missing field '{}' in '{}'", missing.name, name));
                }
                Ok(Type::Named(name.clone()))
            }
            ExprKind::Range(..) => err(span, "a range can only be used in 'each'"),
            ExprKind::Fail(error) => {
                let ty = self.check_expr(error)?;
                if matches!(ty, Type::Unit | Type::None | Type::Fail(_) | Type::Maybe(_) | Type::Or(..)) {
                    return err(error.span, format!("'fail' needs an error value, got {}", ty));
                }
                Ok(Type::Fail(Box::new(ty)))
            }
            ExprKind::PlanB { value, error, body } => self.check_plan_b(span, value, error.as_ref(), body),
            // Only the checker creates `Wrap`, with its type already set.
            ExprKind::Wrap(_) => expr.ty.clone().ok_or_else(|| Error::internal_at(span, "a wrapped value has no type")),
        }
    }

    /// Checks a plan B, `value { body }`, and returns the type it gives: the
    /// type inside `value`.
    fn check_plan_b(&mut self, span: Span, value: &mut Expr, error: Option<&Binding>, body: &mut Block) -> Result<Type> {
        let value_ty = self.check_expr(value)?;
        let (extracted, error_ty) = match &value_ty {
            Type::Maybe(inner) => ((**inner).clone(), None),
            Type::Or(ok, fail) => ((**ok).clone(), Some((**fail).clone())),
            other => {
                return err(value.span, format!("this value is {}, not a maybe or a 'T or E', so it needs no plan B", other));
            }
        };
        let mut bindings = Vec::new();
        if let Some((name, at)) = error {
            let Some(error_ty) = error_ty else {
                return err(*at, format!("a {} has no error to name; 'fail e' names the error of a 'T or E'", value_ty));
            };
            if self.variants.contains_key(name) {
                return err(*at, format!("'{}' is a variant; to send an error, write '<- fail {}'", name, name));
            }
            bindings.push((name.clone(), *at, error_ty));
        }
        self.push_scope();
        let result = bindings
            .into_iter()
            .filter(|(name, _, _)| name != DISCARD)
            .try_for_each(|(name, at, ty)| self.define(&name, ty, false, at))
            .and_then(|()| self.check_plan_b_body(span, &extracted, body));
        self.pop_scope();
        result?;
        Ok(extracted)
    }

    /// Checks a plan B's statements: the last line gives the value, unless every
    /// path through the block sends, or there is no value to give (`unit or E`).
    fn check_plan_b_body(&mut self, span: Span, extracted: &Type, body: &mut Block) -> Result<()> {
        if *extracted == Type::Unit {
            return self.check_block(body);
        }
        let Some((last, rest)) = body.split_last_mut() else {
            return err(span, format!("a plan B needs a value of type {} on its last line, or a '<-'", extracted));
        };
        self.check_block(rest)?;
        if let StmtKind::Expr(e) = &mut last.kind {
            let ty = self.check_expr(e)?;
            if ty == Type::Unit {
                return err(e.span, format!("the last line of a plan B gives its value, but this has none; expected a value of type {}", extracted));
            }
            return self.coerce(e, extracted);
        }
        self.check_stmt(last)?;
        if !always_sends(body) {
            return err(span, format!("a plan B must end with a value of type {}, or send with '<-' on every path", extracted));
        }
        Ok(())
    }

    fn check_unary(&mut self, op: UnaryOp, e: &mut Expr, span: Span) -> Result<Type> {
        let ty = self.check_expr(e)?;
        match op {
            UnaryOp::Neg => {
                if !ty.is_numeric() {
                    return err(span, format!("negation requires numeric type{}", extract_hint(&[&ty])));
                }
                Ok(ty)
            }
            UnaryOp::Not => {
                self.expect_type(&ty, &Type::Bool, e.span)?;
                Ok(Type::Bool)
            }
        }
    }

    fn check_binary(&mut self, op: BinaryOp, l: &mut Expr, r: &mut Expr, span: Span) -> Result<Type> {
        let lt = self.check_expr(l)?;
        let rt = self.check_expr(r)?;
        match op {
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => {
                if !lt.is_numeric() || !rt.is_numeric() {
                    return err(span, format!("arithmetic requires numeric operands{}", extract_hint(&[&lt, &rt])));
                }
                // Mixing int and float promotes the result to float.
                if lt == Type::Float || rt == Type::Float {
                    Ok(Type::Float)
                } else {
                    Ok(Type::Int)
                }
            }
            BinaryOp::Eq | BinaryOp::Ne => {
                let none_and_maybe = matches!((&lt, &rt), (Type::None, Type::Maybe(_)) | (Type::Maybe(_), Type::None));
                if none_and_maybe {
                    return Ok(Type::Bool);
                }
                if lt == Type::None || rt == Type::None {
                    return err(span, "'none' can only be compared with a maybe value");
                }
                if let Some(maybe) = [&lt, &rt].into_iter().find(|t| matches!(t, Type::Maybe(_))) {
                    return err(span, format!("a {} can only be compared with none; extract its value first, as in 'x {{ … }}'", maybe));
                }
                self.expect_type(&rt, &lt, r.span)?;
                if !lt.is_numeric() && !matches!(lt, Type::Bool | Type::Text | Type::Ptr(_)) {
                    return err(span, "cannot compare this type");
                }
                Ok(Type::Bool)
            }
            BinaryOp::Lt | BinaryOp::Gt | BinaryOp::Le | BinaryOp::Ge => {
                if !lt.is_numeric() {
                    return err(span, format!("comparison requires numeric operands{}", extract_hint(&[&lt])));
                }
                self.expect_type(&rt, &lt, r.span)?;
                Ok(Type::Bool)
            }
            BinaryOp::And | BinaryOp::Or => {
                self.expect_type(&lt, &Type::Bool, l.span)?;
                self.expect_type(&rt, &Type::Bool, r.span)?;
                Ok(Type::Bool)
            }
        }
    }

    fn field_type(&self, base: &Type, field: &str, span: Span) -> Result<Type> {
        match base {
            Type::Named(name) => {
                let Some(shape) = self.shapes.get(name) else {
                    return err(span, format!("'{}' is a kind and has no fields; use match to read its values", name));
                };
                for f in &shape.fields {
                    if f.name == field {
                        return Ok(f.ty.clone());
                    }
                }
                err(span, format!("shape '{}' has no field '{}'", name, field))
            }
            Type::Ptr(_) => err(span, format!("type {} has no fields; dereference it first, as in 'p^.{}'", base, field)),
            Type::Maybe(_) | Type::Or(..) => {
                err(span, format!("type {} has no fields; extract its value first, as in 'when x is {{value}}'", base))
            }
            _ => err(span, format!("type {} has no fields", base)),
        }
    }

    /// Checks that the value of `expr` fits where `expected` is required, and
    /// marks where it becomes part of a `maybe` or `T or E` with a `Wrap` node:
    /// a plain value or `none` in a `maybe`, a plain value or `fail e` in a
    /// `T or E`.
    fn coerce(&self, expr: &mut Expr, expected: &Type) -> Result<()> {
        let actual = expr.ty()?.clone();
        if actual == *expected {
            return Ok(());
        }
        match expected {
            Type::Maybe(inner) if actual == Type::None || actual == **inner => {
                wrap(expr, expected);
                return Ok(());
            }
            Type::Or(value, error) => {
                if let Type::Fail(given) = &actual {
                    if given != error {
                        return err(expr.span, format!("expected an error of type {}, got {}", error, given));
                    }
                    wrap(expr, expected);
                    return Ok(());
                }
                if **value != Type::Unit {
                    let fits_value = actual == **value
                        || matches!(value.as_ref(), Type::Maybe(inner) if actual == Type::None || actual == **inner);
                    if fits_value {
                        self.coerce(expr, value)?;
                        wrap(expr, expected);
                        return Ok(());
                    }
                }
            }
            _ => {}
        }
        let hint = match (&actual, expected) {
            (Type::None, Type::Ptr(_)) => format!("; a pointer can't be none, so use 'maybe {}'", expected),
            (Type::Fail(_), _) => "; 'fail …' is an error, which only a 'T or E' can hold".to_string(),
            (Type::Maybe(inner) | Type::Or(inner, _), _) if **inner == *expected => {
                "; extract its value first, as in 'x { … }' or 'when x is {value}'".to_string()
            }
            _ => String::new(),
        };
        err(expr.span, format!("expected type {}, got {}{}", expected, actual, hint))
    }

    /// Checks that a value of type `actual` is exactly `expected`, where no
    /// value changes type: conditions, operands, and ranges.
    fn expect_type(&self, actual: &Type, expected: &Type, span: Span) -> Result<()> {
        if actual != expected {
            err(span, format!("expected type {}, got {}{}", expected, actual, extract_hint(&[actual])))
        } else {
            Ok(())
        }
    }
}

/// Replaces `expr` with a `Wrap` of itself, of type `ty`.
fn wrap(expr: &mut Expr, ty: &Type) {
    let span = expr.span;
    let inner = std::mem::replace(expr, Expr::new(ExprKind::NoneLit, span));
    *expr = Expr { kind: ExprKind::Wrap(Box::new(inner)), span, ty: Some(ty.clone()) };
}

/// A hint for an operation on a `maybe` or `T or E`, whose value must be
/// extracted first; empty if none of `types` is one.
fn extract_hint(types: &[&Type]) -> String {
    match types.iter().find(|t| matches!(t, Type::Maybe(_) | Type::Or(..))) {
        Some(ty) => format!("; a {} must be extracted first, as in 'x {{ … }}' or 'when x is {{value}}'", ty),
        None => String::new(),
    }
}

/// Whether a task with return type `ret` must send a value: every return type
/// except `unit` and `unit or E`, which succeed by reaching their end.
fn sends_value(ret: &Type) -> bool {
    !matches!(ret, Type::Unit) && !matches!(ret, Type::Or(value, _) if **value == Type::Unit)
}

/// The shapes and kinds a value of type `ty` contains by value (not behind a
/// pointer).
fn contained_by_value(ty: &Type) -> Vec<&str> {
    match ty {
        Type::Named(name) => vec![name.as_str()],
        Type::Maybe(inner) => contained_by_value(inner),
        Type::Or(value, error) => {
            let mut names = contained_by_value(value);
            names.extend(contained_by_value(error));
            names
        }
        _ => Vec::new(),
    }
}

/// Whether a value of type `ty` can be created, given the shapes and kinds in
/// `can` that can be.
fn constructible(ty: &Type, can: &HashSet<&str>) -> bool {
    match ty {
        Type::Named(name) => can.contains(name.as_str()),
        Type::Ptr(inner) => constructible(inner, can),
        Type::Or(value, error) => **value == Type::Unit || constructible(value, can) || constructible(error, can),
        _ => true,
    }
}

/// Whether running `block` always ends in `<-`: it contains a `<-`, a
/// `when` whose two branches both always send, or a `match` whose cases all do.
/// Loops don't count, since their bodies might not run.
fn always_sends(block: &Block) -> bool {
    block.iter().any(|stmt| match &stmt.kind {
        StmtKind::Send(_) => true,
        StmtKind::When(_, then_block, Some(else_block)) => always_sends(then_block) && always_sends(else_block),
        StmtKind::Match(_, cases) => cases.iter().all(|case| always_sends(&case.body)),
        _ => false,
    })
}

/// `1 argument`, `2 arguments`.
fn count(n: usize, noun: &str) -> String {
    if n == 1 { format!("1 {}", noun) } else { format!("{} {}s", n, noun) }
}

fn err<T>(span: Span, msg: impl Into<String>) -> Result<T> {
    Err(Error::at(span, msg))
}
