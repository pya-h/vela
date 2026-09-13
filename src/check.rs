use std::collections::HashMap;

use crate::ast::*;
use crate::error::{Error, Result};

#[derive(Clone)]
struct VarInfo {
    ty: Type,
    mutable: bool,
}

pub struct Checker {
    shapes: HashMap<String, ShapeDef>,
    kinds: HashMap<String, KindDef>,
    tasks: HashMap<String, (Vec<Type>, Type)>,
    scopes: Vec<HashMap<String, VarInfo>>,
    current_ret: Option<Type>,
}

impl Checker {
    pub fn new() -> Self {
        Checker {
            shapes: HashMap::new(),
            kinds: HashMap::new(),
            tasks: HashMap::new(),
            scopes: Vec::new(),
            current_ret: None,
        }
    }

    pub fn check_file(&mut self, items: &[Item]) -> Result<()> {
        for item in items {
            match item {
                Item::Shape(s) => {
                    if self.shapes.contains_key(&s.name) {
                        return err(1, 1, format!("shape '{}' defined twice", s.name));
                    }
                    self.shapes.insert(s.name.clone(), s.clone());
                }
                Item::Kind(k) => {
                    if self.kinds.contains_key(&k.name) {
                        return err(1, 1, format!("kind '{}' defined twice", k.name));
                    }
                    self.kinds.insert(k.name.clone(), k.clone());
                }
                Item::Task(t) => {
                    if self.tasks.contains_key(&t.name) {
                        return err(1, 1, format!("task '{}' defined twice", t.name));
                    }
                    let ptypes: Vec<Type> = t.params.iter().map(|p| p.1.clone()).collect();
                    self.tasks.insert(t.name.clone(), (ptypes, t.ret.clone()));
                }
            }
        }

        for item in items {
            if let Item::Task(t) = item {
                self.check_task(t)?;
            }
        }
        Ok(())
    }

    fn check_task(&mut self, task: &TaskDef) -> Result<()> {
        self.current_ret = Some(task.ret.clone());
        self.push_scope();
        for (name, ty) in &task.params {
            self.define(name.clone(), ty.clone(), false);
        }
        self.check_block(&task.body)?;
        self.pop_scope();
        self.current_ret = None;
        Ok(())
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn define(&mut self, name: String, ty: Type, mutable: bool) {
        let scope = self.scopes.last_mut().unwrap();
        scope.insert(name, VarInfo { ty, mutable });
    }

    fn lookup(&self, name: &str) -> Option<&VarInfo> {
        for scope in self.scopes.iter().rev() {
            if let Some(info) = scope.get(name) {
                return Some(info);
            }
        }
        None
    }

    fn check_block(&mut self, block: &Block) -> Result<()> {
        for stmt in block {
            self.check_stmt(stmt)?;
        }
        Ok(())
    }

    fn check_stmt(&mut self, stmt: &Stmt) -> Result<()> {
        match stmt {
            Stmt::Expr(e) => { self.check_expr(e)?; Ok(()) }
            Stmt::Bind(name, ty_hint, expr) => {
                let expr_ty = self.check_expr(expr.as_ref().unwrap())?;
                let final_ty = if ty_hint == &Type::Unit {
                    expr_ty
                } else {
                    self.expect_type(&expr_ty, ty_hint)?;
                    ty_hint.clone()
                };
                self.define(name.clone(), final_ty, false);
                Ok(())
            }
            Stmt::Hold(name, ty_hint, expr) => {
                let expr_ty = self.check_expr(expr.as_ref().unwrap())?;
                let final_ty = if ty_hint == &Type::Unit {
                    expr_ty
                } else {
                    self.expect_type(&expr_ty, ty_hint)?;
                    ty_hint.clone()
                };
                self.define(name.clone(), final_ty, true);
                Ok(())
            }
            Stmt::Assign(lhs, rhs) => {
                let lhs_ty = self.check_lvalue(lhs)?;
                let rhs_ty = self.check_expr(rhs)?;
                self.expect_type(&rhs_ty, &lhs_ty)?;
                Ok(())
            }
            Stmt::When(cond, then_block, else_block) => {
                let cty = self.check_expr(cond)?;
                self.expect_type(&cty, &Type::Bool)?;
                self.check_block(then_block)?;
                if let Some(else_b) = else_block {
                    self.check_block(else_b)?;
                }
                Ok(())
            }
            Stmt::Loop(cond, body) => {
                let cty = self.check_expr(cond)?;
                self.expect_type(&cty, &Type::Bool)?;
                self.check_block(body)?;
                Ok(())
            }
            Stmt::Each(name, range, body) => {
                let rty = self.check_expr(range)?;
                let inner = rty.range_inner().ok_or_else(|| Error::new(1, 1, "each requires a range"))?.clone();
                self.push_scope();
                self.define(name.clone(), inner, false);
                self.check_block(body)?;
                self.pop_scope();
                Ok(())
            }
            Stmt::Match(expr, cases) => self.check_match(expr, cases),
            Stmt::Send(expr) => {
                let ret = self.current_ret.clone().unwrap_or(Type::Unit);
                if let Some(e) = expr {
                    let ety = self.check_expr(e)?;
                    self.expect_type(&ety, &ret)?;
                } else if ret != Type::Unit {
                    return err(1, 1, "expected a value after send");
                }
                Ok(())
            }
            Stmt::Free(e) => {
                let ety = self.check_expr(e)?;
                if ety.ptr_inner().is_none() {
                    return err(1, 1, "free requires a pointer");
                }
                Ok(())
            }
        }
    }

    fn check_lvalue(&mut self, expr: &Expr) -> Result<Type> {
        match expr {
            Expr::Var(name) => {
                let info = self.lookup(name).ok_or_else(|| Error::new(1, 1, format!("unknown variable '{}'", name)))?;
                if !info.mutable {
                    return err(1, 1, format!("cannot assign to immutable variable '{}'", name));
                }
                Ok(info.ty.clone())
            }
            Expr::Field(base, field) => {
                let base_ty = self.check_expr(base)?;
                self.field_type(&base_ty, field)
            }
            Expr::Deref(base) => {
                let base_ty = self.check_expr(base)?;
                base_ty.ptr_inner().cloned().ok_or_else(|| Error::new(1, 1, "cannot deref non-pointer"))
            }
            _ => err(1, 1, "invalid assignment target"),
        }
    }

    fn check_match(&mut self, expr: &Expr, cases: &[Case]) -> Result<()> {
        let ety = self.check_expr(expr)?;
        let kind_name = match &ety {
            Type::Named(n) => n.clone(),
            _ => return err(1, 1, "match requires a kind value"),
        };
        let kind = self.kinds.get(&kind_name).ok_or_else(|| Error::new(1, 1, format!("'{}' is not a kind", kind_name)))?.clone();
        let mut seen = std::collections::HashSet::new();
        for case in cases {
            let variant = kind.variants.iter().find(|v| v.tag == case.tag)
                .ok_or_else(|| Error::new(1, 1, format!("kind '{}' has no variant '{}'", kind_name, case.tag)))?;
            if case.binds.len() != variant.payload.len() {
                return err(1, 1, format!("case '{}' expects {} bindings, got {}", case.tag, variant.payload.len(), case.binds.len()));
            }
            if seen.contains(&case.tag) {
                return err(1, 1, format!("duplicate case '{}'", case.tag));
            }
            seen.insert(case.tag.clone());
            self.push_scope();
            for (bind, (field, ty)) in case.binds.iter().zip(variant.payload.iter()) {
                let _ = field;
                self.define(bind.clone(), ty.clone(), false);
            }
            self.check_block(&case.body)?;
            self.pop_scope();
        }
        for variant in &kind.variants {
            if !seen.contains(&variant.tag) {
                return err(1, 1, format!("missing case '{}' in match", variant.tag));
            }
        }
        Ok(())
    }

    fn check_expr(&mut self, expr: &Expr) -> Result<Type> {
        match expr {
            Expr::IntLit(_) => Ok(Type::Int),
            Expr::FloatLit(_) => Ok(Type::Float),
            Expr::BoolLit(_) => Ok(Type::Bool),
            Expr::TextLit(_) => Ok(Type::Text),
            Expr::NoneLit => Ok(Type::Ptr(Box::new(Type::Unit))),
            Expr::Var(name) => {
                let info = self.lookup(name).ok_or_else(|| Error::new(1, 1, format!("unknown variable '{}'", name)))?;
                Ok(info.ty.clone())
            }
            Expr::Field(base, field) => {
                let base_ty = self.check_expr(base)?;
                self.field_type(&base_ty, field)
            }
            Expr::Index(_base, _index) => {
                // Arrays not supported yet
                err(1, 1, "indexing not supported")
            }
            Expr::Call(name, args) => {
                if name == "show" {
                    if args.len() != 1 {
                        return err(1, 1, "show expects 1 argument");
                    }
                    let ty = self.check_expr(&args[0])?;
                    if !matches!(ty, Type::Int | Type::Float | Type::Bool | Type::Text) {
                        return err(1, 1, "show expects int, float, bool, or text");
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
                        return err(1, 1, format!("variant '{}' expects {} args, got {}", name, variant.payload.len(), args.len()));
                    }
                    for (arg, (_field, pty)) in args.iter().zip(variant.payload.iter()) {
                        let aty = self.check_expr(arg)?;
                        self.expect_type(&aty, pty)?;
                    }
                    return Ok(Type::Named(kind_name));
                }
                let (params, ret) = self.tasks.get(name)
                    .ok_or_else(|| Error::new(1, 1, format!("unknown task '{}'", name)))?
                    .clone();
                if args.len() != params.len() {
                    return err(1, 1, format!("task '{}' expects {} args, got {}", name, params.len(), args.len()));
                }
                for (arg, param) in args.iter().zip(params.iter()) {
                    let aty = self.check_expr(arg)?;
                    self.expect_type(&aty, param)?;
                }
                Ok(ret)
            }
            Expr::Unary(op, e) => self.check_unary(*op, e),
            Expr::Binary(op, l, r) => self.check_binary(*op, l, r),
            Expr::New(ty, e) => {
                let val_ty = self.check_expr(e)?;
                self.expect_type(&val_ty, ty)?;
                Ok(Type::Ptr(Box::new((**ty).clone())))
            }
            Expr::Deref(e) => {
                let ty = self.check_expr(e)?;
                ty.ptr_inner().cloned().ok_or_else(|| Error::new(1, 1, "cannot dereference non-pointer"))
            }
            Expr::StructInit(name, fields) => {
                let shape = self.shapes.get(name).ok_or_else(|| Error::new(1, 1, format!("unknown shape '{}'", name)))?.clone();
                if fields.len() != shape.fields.len() {
                    return err(1, 1, format!("shape '{}' expects {} fields, got {}", name, shape.fields.len(), fields.len()));
                }
                for ((fname, fexpr), (sname, sty)) in fields.iter().zip(shape.fields.iter()) {
                    if fname != sname {
                        return err(1, 1, format!("expected field '{}', got '{}'", sname, fname));
                    }
                    let fty = self.check_expr(fexpr)?;
                    self.expect_type(&fty, sty)?;
                }
                Ok(Type::Named(name.clone()))
            }
            Expr::Range(a, b) => {
                let aty = self.check_expr(a)?;
                let bty = self.check_expr(b)?;
                self.expect_type(&aty, &Type::Int)?;
                self.expect_type(&bty, &Type::Int)?;
                Ok(Type::Range(Box::new(Type::Int)))
            }
        }
    }

    fn check_unary(&mut self, op: UnaryOp, e: &Expr) -> Result<Type> {
        let ty = self.check_expr(e)?;
        match op {
            UnaryOp::Neg => {
                if !ty.is_numeric() {
                    return err(1, 1, "negation requires numeric type");
                }
                Ok(ty)
            }
            UnaryOp::Not => {
                self.expect_type(&ty, &Type::Bool)?;
                Ok(Type::Bool)
            }
        }
    }

    fn check_binary(&mut self, op: BinaryOp, l: &Expr, r: &Expr) -> Result<Type> {
        let lt = self.check_expr(l)?;
        let rt = self.check_expr(r)?;
        match op {
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => {
                if !lt.is_numeric() || !rt.is_numeric() {
                    return err(1, 1, "arithmetic requires numeric operands");
                }
                if lt == Type::Float || rt == Type::Float {
                    Ok(Type::Float)
                } else {
                    Ok(Type::Int)
                }
            }
            BinaryOp::Eq | BinaryOp::Ne => {
                self.expect_type(&lt, &rt)?;
                if !lt.is_numeric() && !matches!(lt, Type::Bool | Type::Text | Type::Ptr(_)) {
                    return err(1, 1, "cannot compare this type");
                }
                Ok(Type::Bool)
            }
            BinaryOp::Lt | BinaryOp::Gt | BinaryOp::Le | BinaryOp::Ge => {
                if !lt.is_numeric() {
                    return err(1, 1, "comparison requires numeric operands");
                }
                self.expect_type(&lt, &rt)?;
                Ok(Type::Bool)
            }
            BinaryOp::And | BinaryOp::Or => {
                self.expect_type(&lt, &Type::Bool)?;
                self.expect_type(&rt, &Type::Bool)?;
                Ok(Type::Bool)
            }
        }
    }

    fn field_type(&self, base: &Type, field: &str) -> Result<Type> {
        match base {
            Type::Named(name) => {
                let shape = self.shapes.get(name).ok_or_else(|| Error::new(1, 1, format!("unknown shape '{}'", name)))?;
                for (fname, fty) in &shape.fields {
                    if fname == field {
                        return Ok(fty.clone());
                    }
                }
                err(1, 1, format!("shape '{}' has no field '{}'", name, field))
            }
            _ => err(1, 1, "field access on non-shape"),
        }
    }

    fn expect_type(&self, actual: &Type, expected: &Type) -> Result<()> {
        if actual != expected {
            err(1, 1, format!("expected type {:?}, got {:?}", expected, actual))
        } else {
            Ok(())
        }
    }
}

fn err<T>(line: usize, col: usize, msg: impl Into<String>) -> Result<T> {
    Err(Error::new(line, col, msg))
}
