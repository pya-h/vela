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
        Ok(())
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
    /// `unit` is only valid as a return type, which is checked separately.
    fn check_type(&self, ty: &Type, span: Span) -> Result<()> {
        match ty {
            Type::Unit => err(span, "unit is only allowed as a return type"),
            Type::Ptr(inner) => self.check_type(inner, span),
            Type::Named(name) if !self.shapes.contains_key(name) && !self.kinds.contains_key(name) => {
                err(span, format!("unknown type '{}'", name))
            }
            _ => Ok(()),
        }
    }

    /// Rejects a shape or kind that contains itself by value, directly or
    /// through other types: its values would have infinite size. Containing a
    /// pointer to itself (`^T`) is fine. `path` holds the types being entered;
    /// `acyclic` the types already known to be free of cycles.
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
            let Type::Named(target) = &field.ty else { continue };
            if let Some(start) = path.iter().position(|n| n == target) {
                let chain: Vec<&str> = path.iter().skip(start).map(String::as_str).chain([target.as_str()]).collect();
                return err(
                    field.ty_span,
                    format!("'{}' contains itself by value ({}); use a pointer such as '^{}'", target, chain.join(" -> "), target),
                );
            }
            self.check_value_cycle(target, path, acyclic)?;
        }
        path.pop();
        acyclic.insert(name.to_string());
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
        if task.ret != Type::Unit && !always_sends(&task.body) {
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

    fn check_block(&mut self, block: &mut Block) -> Result<()> {
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
                // Of all expressions, only a call can have an effect.
                match &e.kind {
                    ExprKind::Call(..) => Ok(()),
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
                        self.expect_type(&init_ty, annotated, init.span)?;
                        annotated.clone()
                    }
                    None => match init_ty {
                        Type::None => {
                            return err(init.span, format!("cannot infer the type of '{}' from none; give it a pointer type, such as '{}: ^int'", name, name));
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
                let rhs_ty = self.check_expr(rhs)?;
                self.expect_type(&rhs_ty, &lhs_ty, rhs.span)?;
                Ok(())
            }
            StmtKind::When(cond, then_block, else_block) => {
                let cty = self.check_expr(cond)?;
                self.expect_type(&cty, &Type::Bool, cond.span)?;
                self.check_scoped_block(then_block)?;
                if let Some(else_b) = else_block {
                    self.check_scoped_block(else_b)?;
                }
                Ok(())
            }
            StmtKind::Loop(cond, body) => {
                let cty = self.check_expr(cond)?;
                self.expect_type(&cty, &Type::Bool, cond.span)?;
                self.check_scoped_block(body)
            }
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
                    Some(e) if ret == Type::Unit => err(e.span, "this task has no return type, so send cannot take a value"),
                    Some(e) => {
                        let ety = self.check_expr(e)?;
                        self.expect_type(&ety, &ret, e.span)
                    }
                    None if ret != Type::Unit => err(span, "expected a value after send"),
                    None => Ok(()),
                }
            }
            StmtKind::Free(e) => {
                let ety = self.check_expr(e)?;
                if ety.ptr_inner().is_none() {
                    return err(e.span, "free requires a pointer");
                }
                Ok(())
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
                if !info.mutable {
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
        let kind_name = match &ety {
            Type::Named(n) => n.clone(),
            _ => return err(expr.span, "match requires a kind value"),
        };
        let kind = self.kinds.get(&kind_name)
            .ok_or_else(|| Error::at(expr.span, format!("'{}' is not a kind", kind_name)))?
            .clone();
        let mut seen = HashSet::new();
        for case in cases {
            let variant = kind.variants.iter().find(|v| v.tag == case.tag)
                .ok_or_else(|| Error::at(case.span, format!("kind '{}' has no variant '{}'", kind_name, case.tag)))?;
            if case.binds.len() != variant.payload.len() {
                return err(case.span, format!("case '{}' expects {} bindings, got {}", case.tag, variant.payload.len(), case.binds.len()));
            }
            if seen.contains(&case.tag) {
                return err(case.span, format!("duplicate case '{}'", case.tag));
            }
            seen.insert(case.tag.clone());
            // The case's bindings and its body share one scope.
            self.push_scope();
            let result = case.binds.iter().zip(variant.payload.iter())
                .try_for_each(|((bind, bind_span), field)| {
                    self.define(bind, field.ty.clone(), false, *bind_span)
                })
                .and_then(|()| self.check_block(&mut case.body));
            self.pop_scope();
            result?;
        }
        for variant in &kind.variants {
            if !seen.contains(&variant.tag) {
                return err(span, format!("missing case '{}' in match", variant.tag));
            }
        }
        Ok(())
    }

    /// Checks an expression, records its type on the node, and returns it.
    fn check_expr(&mut self, expr: &mut Expr) -> Result<Type> {
        self.resolve_constructor(expr);
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
                    if !matches!(ty, Type::Int | Type::Float | Type::Bool | Type::Text) {
                        return err(arg.span, "show expects int, float, bool, or text");
                    }
                    return Ok(Type::Unit);
                }
                let (params, ret) = self.tasks.get(name.as_str())
                    .ok_or_else(|| Error::at(span, format!("unknown task '{}'", name)))?
                    .clone();
                if args.len() != params.len() {
                    return err(span, format!("task '{}' expects {} args, got {}", name, params.len(), args.len()));
                }
                for (arg, param) in args.iter_mut().zip(params.iter()) {
                    let aty = self.check_expr(arg)?;
                    self.expect_type(&aty, param, arg.span)?;
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
                    return err(span, format!("variant '{}' expects {} args, got {}", tag, variant.payload.len(), args.len()));
                }
                for (arg, field) in args.iter_mut().zip(variant.payload.iter()) {
                    let aty = self.check_expr(arg)?;
                    self.expect_type(&aty, &field.ty, arg.span)?;
                }
                Ok(Type::Named(kind.clone()))
            }
            ExprKind::New { ty, ty_span, value } => {
                self.check_type(ty, *ty_span)?;
                let val_ty = self.check_expr(value)?;
                self.expect_type(&val_ty, ty, value.span)?;
                Ok(Type::Ptr(Box::new(ty.clone())))
            }
            ExprKind::Deref(e) => {
                let ty = self.check_expr(e)?;
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
                    let fty = self.check_expr(&mut field.value)?;
                    self.expect_type(&fty, &declared.ty, field.value.span)?;
                }
                if let Some(missing) = shape.fields.iter().find(|f| !given.contains_key(&f.name)) {
                    return err(span, format!("missing field '{}' in '{}'", missing.name, name));
                }
                Ok(Type::Named(name.clone()))
            }
            ExprKind::Range(..) => err(span, "a range can only be used in 'each'"),
        }
    }

    fn check_unary(&mut self, op: UnaryOp, e: &mut Expr, span: Span) -> Result<Type> {
        let ty = self.check_expr(e)?;
        match op {
            UnaryOp::Neg => {
                if !ty.is_numeric() {
                    return err(span, "negation requires numeric type");
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
                    return err(span, "arithmetic requires numeric operands");
                }
                // Mixing int and float promotes the result to float.
                if lt == Type::Float || rt == Type::Float {
                    Ok(Type::Float)
                } else {
                    Ok(Type::Int)
                }
            }
            BinaryOp::Eq | BinaryOp::Ne => {
                let none_and_pointer = matches!(
                    (&lt, &rt),
                    (Type::None, Type::Ptr(_) | Type::None) | (Type::Ptr(_), Type::None)
                );
                if !none_and_pointer {
                    self.expect_type(&rt, &lt, r.span)?;
                }
                if !lt.is_numeric() && !matches!(lt, Type::Bool | Type::Text | Type::Ptr(_) | Type::None) {
                    return err(span, "cannot compare this type");
                }
                Ok(Type::Bool)
            }
            BinaryOp::Lt | BinaryOp::Gt | BinaryOp::Le | BinaryOp::Ge => {
                if !lt.is_numeric() {
                    return err(span, "comparison requires numeric operands");
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
            _ => err(span, format!("type {} has no fields", base)),
        }
    }

    /// Checks that a value of type `actual` fits where `expected` is required:
    /// the types are equal, or the value is `none` and a pointer is expected.
    fn expect_type(&self, actual: &Type, expected: &Type, span: Span) -> Result<()> {
        let none_for_pointer = *actual == Type::None && matches!(expected, Type::Ptr(_));
        if actual != expected && !none_for_pointer {
            err(span, format!("expected type {}, got {}", expected, actual))
        } else {
            Ok(())
        }
    }
}

/// Whether running `block` always ends in `send`: it contains a `send`, a
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

fn err<T>(span: Span, msg: impl Into<String>) -> Result<T> {
    Err(Error::at(span, msg))
}
