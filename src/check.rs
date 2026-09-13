use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::error::{Error, Result};

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
    scopes: Vec<HashMap<String, VarInfo>>,
    current_ret: Option<Type>,
}

impl Checker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn check_file(&mut self, items: &mut [Item]) -> Result<()> {
        for item in items.iter() {
            match item {
                Item::Shape(s) => {
                    if self.shapes.contains_key(&s.name) {
                        return err(s.span, format!("shape '{}' defined twice", s.name));
                    }
                    self.shapes.insert(s.name.clone(), s.clone());
                }
                Item::Kind(k) => {
                    if self.kinds.contains_key(&k.name) {
                        return err(k.span, format!("kind '{}' defined twice", k.name));
                    }
                    self.kinds.insert(k.name.clone(), k.clone());
                }
                Item::Task(t) => {
                    if self.tasks.contains_key(&t.name) {
                        return err(t.span, format!("task '{}' defined twice", t.name));
                    }
                    let ptypes: Vec<Type> = t.params.iter().map(|p| p.ty.clone()).collect();
                    self.tasks.insert(t.name.clone(), (ptypes, t.ret.clone()));
                }
            }
        }

        for item in items.iter_mut() {
            if let Item::Task(t) = item {
                self.check_task(t)?;
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
        self.lookup(name)
            .ok_or_else(|| Error::at(span, format!("unknown variable '{}'", name)))
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
            StmtKind::Expr(e) => { self.check_expr(e)?; Ok(()) }
            StmtKind::Binding { name, mutable, ty, init } => {
                let init_ty = self.check_expr(init)?;
                if let Some(annotated) = ty {
                    self.expect_type(&init_ty, annotated, init.span)?;
                }
                // Equal to the annotation, if there is one.
                *ty = Some(init_ty.clone());
                self.define(name, init_ty, *mutable, span)
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
                let rty = self.check_expr(range)?;
                let inner = rty.range_inner()
                    .ok_or_else(|| Error::at(range.span, "each requires a range"))?
                    .clone();
                // The loop variable and the body share one scope.
                self.push_scope();
                let result = self.define(name, inner, false, span)
                    .and_then(|()| self.check_block(body));
                self.pop_scope();
                result
            }
            StmtKind::Match(expr, cases) => self.check_match(span, expr, cases),
            StmtKind::Send(expr) => {
                let ret = self.current_ret.clone().unwrap_or(Type::Unit);
                if let Some(e) = expr {
                    let ety = self.check_expr(e)?;
                    self.expect_type(&ety, &ret, e.span)?;
                } else if ret != Type::Unit {
                    return err(span, "expected a value after send");
                }
                Ok(())
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
        let ty = self.infer_expr(expr)?;
        expr.ty = Some(ty.clone());
        Ok(ty)
    }

    fn infer_expr(&mut self, expr: &mut Expr) -> Result<Type> {
        let span = expr.span;
        match &mut expr.kind {
            ExprKind::IntLit(_) => Ok(Type::Int),
            ExprKind::FloatLit(_) => Ok(Type::Float),
            ExprKind::BoolLit(_) => Ok(Type::Bool),
            ExprKind::TextLit(_) => Ok(Type::Text),
            ExprKind::NoneLit => Ok(Type::Ptr(Box::new(Type::Unit))),
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
                // Check if it's a kind variant constructor
                let mut found_kind = None;
                for kind in self.kinds.values() {
                    if let Some(variant) = kind.variants.iter().find(|v| v.tag == *name) {
                        found_kind = Some((kind.name.clone(), variant.clone()));
                        break;
                    }
                }
                if let Some((kind_name, variant)) = found_kind {
                    if args.len() != variant.payload.len() {
                        return err(span, format!("variant '{}' expects {} args, got {}", name, variant.payload.len(), args.len()));
                    }
                    for (arg, field) in args.iter_mut().zip(variant.payload.iter()) {
                        let aty = self.check_expr(arg)?;
                        self.expect_type(&aty, &field.ty, arg.span)?;
                    }
                    return Ok(Type::Named(kind_name));
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
            ExprKind::New(ty, e) => {
                let val_ty = self.check_expr(e)?;
                self.expect_type(&val_ty, ty, e.span)?;
                Ok(Type::Ptr(Box::new((**ty).clone())))
            }
            ExprKind::Deref(e) => {
                let ty = self.check_expr(e)?;
                ty.ptr_inner().cloned()
                    .ok_or_else(|| Error::at(span, "cannot dereference non-pointer"))
            }
            ExprKind::StructInit(name, fields) => {
                let shape = self.shapes.get(name.as_str())
                    .ok_or_else(|| Error::at(span, format!("unknown shape '{}'", name)))?
                    .clone();
                if fields.len() != shape.fields.len() {
                    return err(span, format!("shape '{}' expects {} fields, got {}", name, shape.fields.len(), fields.len()));
                }
                for ((fname, fexpr), sfield) in fields.iter_mut().zip(shape.fields.iter()) {
                    if *fname != sfield.name {
                        return err(fexpr.span, format!("expected field '{}', got '{}'", sfield.name, fname));
                    }
                    let fty = self.check_expr(fexpr)?;
                    self.expect_type(&fty, &sfield.ty, fexpr.span)?;
                }
                Ok(Type::Named(name.clone()))
            }
            ExprKind::Range(a, b) => {
                let aty = self.check_expr(a)?;
                let bty = self.check_expr(b)?;
                self.expect_type(&aty, &Type::Int, a.span)?;
                self.expect_type(&bty, &Type::Int, b.span)?;
                Ok(Type::Range(Box::new(Type::Int)))
            }
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
                self.expect_type(&rt, &lt, r.span)?;
                if !lt.is_numeric() && !matches!(lt, Type::Bool | Type::Text | Type::Ptr(_)) {
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
                let shape = self.shapes.get(name)
                    .ok_or_else(|| Error::at(span, format!("unknown shape '{}'", name)))?;
                for f in &shape.fields {
                    if f.name == field {
                        return Ok(f.ty.clone());
                    }
                }
                err(span, format!("shape '{}' has no field '{}'", name, field))
            }
            _ => err(span, "field access on non-shape"),
        }
    }

    fn expect_type(&self, actual: &Type, expected: &Type, span: Span) -> Result<()> {
        if actual != expected {
            err(span, format!("expected type {}, got {}", expected, actual))
        } else {
            Ok(())
        }
    }
}

fn err<T>(span: Span, msg: impl Into<String>) -> Result<T> {
    Err(Error::at(span, msg))
}
