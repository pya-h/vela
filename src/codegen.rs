use std::collections::HashMap;
use std::fmt::Write;

use crate::ast::*;
use crate::error::{Error, Result};

pub struct Generator {
    out: String,
    temp_counter: u64,
    shapes: HashMap<String, ShapeDef>,
    kinds: HashMap<String, KindDef>,
    tasks: HashMap<String, (Vec<Type>, Type)>,
    scopes: Vec<HashMap<String, Type>>,
}

impl Generator {
    pub fn new() -> Self {
        Generator {
            out: String::new(),
            temp_counter: 0,
            shapes: HashMap::new(),
            kinds: HashMap::new(),
            tasks: HashMap::new(),
            scopes: Vec::new(),
        }
    }

    pub fn generate(&mut self, items: &[Item]) -> Result<String> {
        self.shapes.clear();
        self.kinds.clear();
        self.tasks.clear();
        for item in items {
            match item {
                Item::Shape(s) => { self.shapes.insert(s.name.clone(), s.clone()); }
                Item::Kind(k) => { self.kinds.insert(k.name.clone(), k.clone()); }
                Item::Task(t) => {
                    let ptypes: Vec<Type> = t.params.iter().map(|p| p.ty.clone()).collect();
                    self.tasks.insert(t.name.clone(), (ptypes, t.ret.clone()));
                }
            }
        }

        self.out.clear();
        writeln!(self.out, "#include \"vela_rt.h\"").unwrap();
        writeln!(self.out).unwrap();

        // Type definitions first
        for item in items {
            match item {
                Item::Shape(s) => self.gen_shape(s)?,
                Item::Kind(k) => self.gen_kind(k)?,
                _ => {}
            }
        }
        writeln!(self.out).unwrap();

        // Forward declarations for tasks
        for item in items {
            if let Item::Task(t) = item {
                write!(self.out, "{} {}(", c_type(&t.ret), mangle(&t.name)).unwrap();
                for (i, param) in t.params.iter().enumerate() {
                    if i > 0 { write!(self.out, ", ").unwrap(); }
                    write!(self.out, "{} {}", c_type(&param.ty), param.name).unwrap();
                }
                writeln!(self.out, ");").unwrap();
            }
        }
        writeln!(self.out).unwrap();

        // Task definitions
        let mut has_main = false;
        for item in items {
            if let Item::Task(t) = item {
                if t.name == "main" {
                    has_main = true;
                }
                self.gen_task(t)?;
            }
        }

        if has_main {
            writeln!(self.out).unwrap();
            writeln!(self.out, "int main(void) {{").unwrap();
            writeln!(self.out, "    vela_main();").unwrap();
            writeln!(self.out, "    return 0;").unwrap();
            writeln!(self.out, "}}").unwrap();
        }

        Ok(self.out.clone())
    }

    fn fresh_temp(&mut self, prefix: &str) -> String {
        let n = self.temp_counter;
        self.temp_counter += 1;
        format!("{}__{}", prefix, n)
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn define_var(&mut self, name: String, ty: Type) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name, ty);
        }
    }

    fn lookup_var(&self, name: &str) -> Option<&Type> {
        for scope in self.scopes.iter().rev() {
            if let Some(ty) = scope.get(name) {
                return Some(ty);
            }
        }
        None
    }

    fn gen_shape(&mut self, s: &ShapeDef) -> Result<()> {
        writeln!(self.out, "typedef struct {} {{", mangle(&s.name)).unwrap();
        for field in &s.fields {
            writeln!(self.out, "    {} {};", c_type(&field.ty), field.name).unwrap();
        }
        writeln!(self.out, "}} {};", mangle(&s.name)).unwrap();
        Ok(())
    }

    fn gen_kind(&mut self, k: &KindDef) -> Result<()> {
        let tag_name = format!("{}_Tag", mangle(&k.name));
        writeln!(self.out, "typedef enum {0} {{", tag_name).unwrap();
        for v in &k.variants {
            writeln!(self.out, "    {}_{},", tag_name, v.tag).unwrap();
        }
        writeln!(self.out, "}} {0};", tag_name).unwrap();

        writeln!(self.out, "typedef struct {} {{", mangle(&k.name)).unwrap();
        writeln!(self.out, "    {} tag;", tag_name).unwrap();
        writeln!(self.out, "    union {{").unwrap();
        for v in &k.variants {
            if !v.payload.is_empty() {
                writeln!(self.out, "        struct {{").unwrap();
                for field in &v.payload {
                    writeln!(self.out, "            {} {};", c_type(&field.ty), field.name).unwrap();
                }
                writeln!(self.out, "        }} {};", v.tag).unwrap();
            }
        }
        writeln!(self.out, "    }} data;").unwrap();
        writeln!(self.out, "}} {};", mangle(&k.name)).unwrap();
        Ok(())
    }

    fn gen_task(&mut self, t: &TaskDef) -> Result<()> {
        write!(self.out, "{} {}(", c_type(&t.ret), mangle(&t.name)).unwrap();
        for (i, param) in t.params.iter().enumerate() {
            if i > 0 { write!(self.out, ", ").unwrap(); }
            write!(self.out, "{} {}", c_type(&param.ty), param.name).unwrap();
        }
        writeln!(self.out, ") {{").unwrap();
        self.push_scope();
        for param in &t.params {
            self.define_var(param.name.clone(), param.ty.clone());
        }
        self.gen_block(&t.body, 1)?;
        self.pop_scope();
        if t.ret == Type::Unit {
            writeln!(self.out, "    return;").unwrap();
        }
        writeln!(self.out, "}}").unwrap();
        Ok(())
    }

    fn gen_block(&mut self, block: &Block, indent: usize) -> Result<()> {
        let pad = "    ".repeat(indent);
        for stmt in block {
            self.gen_stmt(stmt, &pad)?;
        }
        Ok(())
    }

    fn gen_stmt(&mut self, stmt: &Stmt, pad: &str) -> Result<()> {
        match &stmt.kind {
            StmtKind::Expr(e) => {
                let expr = self.gen_expr(e)?;
                if !expr.is_empty() {
                    writeln!(self.out, "{}{};", pad, expr).unwrap();
                }
            }
            StmtKind::Bind(name, ty_hint, Some(e)) | StmtKind::Hold(name, ty_hint, Some(e)) => {
                let ty = if ty_hint == &Type::Unit {
                    self.expr_type(e).map_err(|_| Error::at(stmt.span, "cannot infer type; add ': type' annotation"))?
                } else {
                    ty_hint.clone()
                };
                let expr = self.gen_expr(e)?;
                self.define_var(name.clone(), ty.clone());
                writeln!(self.out, "{}{} {} = {};", pad, c_type(&ty), name, expr).unwrap();
            }
            StmtKind::Bind(_, _, None) | StmtKind::Hold(_, _, None) => {
                return err(stmt.span, "bind/hold without initializer");
            }
            StmtKind::Assign(lhs, rhs) => {
                let l = self.gen_lvalue(lhs)?;
                let r = self.gen_expr(rhs)?;
                writeln!(self.out, "{}{} = {};", pad, l, r).unwrap();
            }
            StmtKind::When(cond, then_b, else_b) => {
                let c = self.gen_expr(cond)?;
                writeln!(self.out, "{}if ({}) {{", pad, c).unwrap();
                self.gen_block(then_b, indent_level(pad) + 1)?;
                if let Some(else_b) = else_b {
                    writeln!(self.out, "{}}} else {{", pad).unwrap();
                    self.gen_block(else_b, indent_level(pad) + 1)?;
                }
                writeln!(self.out, "{}}}", pad).unwrap();
            }
            StmtKind::Loop(cond, body) => {
                let c = self.gen_expr(cond)?;
                writeln!(self.out, "{}while ({}) {{", pad, c).unwrap();
                self.gen_block(body, indent_level(pad) + 1)?;
                writeln!(self.out, "{}}}", pad).unwrap();
            }
            StmtKind::Each(name, range, body) => {
                let (a, b) = self.gen_range(range)?;
                writeln!(self.out, "{}for (int64_t {} = {}; {} < {}; {}++) {{", pad, name, a, name, b, name).unwrap();
                self.push_scope();
                self.define_var(name.clone(), Type::Int);
                self.gen_block(body, indent_level(pad) + 1)?;
                self.pop_scope();
                writeln!(self.out, "{}}}", pad).unwrap();
            }
            StmtKind::Match(expr, cases) => {
                let e = self.gen_expr(expr)?;
                let ty = self.expr_type(expr)?;
                let kind_name = match &ty {
                    Type::Named(n) => n.clone(),
                    _ => return err(expr.span, "match on non-kind"),
                };
                let kind = self.kinds.get(&kind_name).ok_or_else(|| Error::at(expr.span, format!("unknown kind '{}'", kind_name)))?.clone();
                let tag_name = format!("{}_Tag", mangle(&kind_name));
                writeln!(self.out, "{}switch ({}.tag) {{", pad, e).unwrap();
                self.push_scope();
                for case in cases {
                    let variant = kind.variants.iter().find(|v| v.tag == case.tag)
                        .ok_or_else(|| Error::at(case.span, format!("unknown variant '{}'", case.tag)))?;
                    writeln!(self.out, "{}case {}_{}:", pad, tag_name, case.tag).unwrap();
                    let inner_pad = "    ".repeat(indent_level(pad) + 1);
                    for (bind, field) in case.binds.iter().zip(variant.payload.iter()) {
                        self.define_var(bind.clone(), field.ty.clone());
                        writeln!(self.out, "{}{} {} = {}.data.{}.{};", inner_pad, c_type(&field.ty), bind, e, case.tag, field.name).unwrap();
                    }
                    self.gen_block(&case.body, indent_level(pad) + 1)?;
                    writeln!(self.out, "{}    break;", pad).unwrap();
                }
                self.pop_scope();
                writeln!(self.out, "{}}}", pad).unwrap();
            }
            StmtKind::Send(None) => {
                writeln!(self.out, "{}return;", pad).unwrap();
            }
            StmtKind::Send(Some(e)) => {
                let expr = self.gen_expr(e)?;
                writeln!(self.out, "{}return {};", pad, expr).unwrap();
            }
            StmtKind::Free(e) => {
                let expr = self.gen_expr(e)?;
                writeln!(self.out, "{}vela_free((void*){});", pad, expr).unwrap();
            }
        }
        Ok(())
    }

    fn gen_expr(&mut self, e: &Expr) -> Result<String> {
        let span = e.span;
        match &e.kind {
            ExprKind::IntLit(n) => Ok(n.to_string()),
            ExprKind::FloatLit(n) => Ok(n.to_string()),
            ExprKind::BoolLit(b) => Ok(if *b { "true".to_string() } else { "false".to_string() }),
            ExprKind::TextLit(s) => Ok(format!("\"{}\"", escape_c_string(s))),
            ExprKind::NoneLit => Ok("NULL".to_string()),
            ExprKind::Var(name) => Ok(name.clone()),
            ExprKind::Field(base, field) => {
                let b = self.gen_expr(base)?;
                Ok(format!("({}).{}", b, field))
            }
            ExprKind::Index(_base, _index) => err(span, "indexing not supported"),
            ExprKind::Call(name, args) => {
                if name == "show" {
                    let arg = self.gen_expr(&args[0])?;
                    let ty = self.expr_type(&args[0])?;
                    let helper = match ty {
                        Type::Int => "vela_show_int",
                        Type::Float => "vela_show_float",
                        Type::Bool => "vela_show_bool",
                        Type::Text => "vela_show_text",
                        _ => return err(span, "show unsupported for this type"),
                    };
                    return Ok(format!("{}({})", helper, arg));
                }
                // Kind variant constructor?
                let mut found = None;
                for kind in self.kinds.values() {
                    if let Some(variant) = kind.variants.iter().find(|v| v.tag == *name) {
                        found = Some((kind.name.clone(), variant.clone()));
                        break;
                    }
                }
                if let Some((kind_name, variant)) = found {
                    let tag_name = format!("{}_Tag", mangle(&kind_name));
                    let mut s = format!("(({}){{ .tag = {}_{}, .data = {{ .{} = {{ ", mangle(&kind_name), tag_name, variant.tag, variant.tag);
                    for (i, field) in variant.payload.iter().enumerate() {
                        if i > 0 { s.push_str(", "); }
                        s.push_str(&format!(".{0} = {1}", field.name, self.gen_expr(&args[i])?));
                    }
                    s.push_str(" } } })");
                    return Ok(s);
                }
                let mut s = format!("{}(", mangle(name));
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 { s.push_str(", "); }
                    s.push_str(&self.gen_expr(arg)?);
                }
                s.push(')');
                Ok(s)
            }
            ExprKind::Unary(op, e) => {
                let expr = self.gen_expr(e)?;
                let op_s = match op {
                    UnaryOp::Neg => "-",
                    UnaryOp::Not => "!",
                };
                Ok(format!("({}{})", op_s, expr))
            }
            ExprKind::Binary(op, l, r) => {
                let left = self.gen_expr(l)?;
                let right = self.gen_expr(r)?;
                let op_s = match op {
                    BinaryOp::Add => "+",
                    BinaryOp::Sub => "-",
                    BinaryOp::Mul => "*",
                    BinaryOp::Div => "/",
                    BinaryOp::Mod => "%",
                    BinaryOp::Eq => "==",
                    BinaryOp::Ne => "!=",
                    BinaryOp::Lt => "<",
                    BinaryOp::Gt => ">",
                    BinaryOp::Le => "<=",
                    BinaryOp::Ge => ">=",
                    BinaryOp::And => "&&",
                    BinaryOp::Or => "||",
                };
                Ok(format!("({} {} {})", left, op_s, right))
            }
            ExprKind::New(ty, e) => {
                let val = self.gen_expr(e)?;
                let tmp = self.fresh_temp("p");
                let ctype = c_type(ty);
                Ok(format!("({{ {}* {} = ({}*)vela_new(sizeof({})); *{} = {}; {}; }})", ctype, tmp, ctype, ctype, tmp, val, tmp))
            }
            ExprKind::Deref(e) => {
                let expr = self.gen_expr(e)?;
                Ok(format!("(*({}))", expr))
            }
            ExprKind::StructInit(name, fields) => {
                let mut s = format!("((struct {}){{ ", mangle(name));
                for (i, (fname, fexpr)) in fields.iter().enumerate() {
                    if i > 0 { s.push_str(", "); }
                    s.push_str(&format!(".{0} = {1}", fname, self.gen_expr(fexpr)?));
                }
                s.push_str(" })");
                Ok(s)
            }
            ExprKind::Range(_, _) => err(span, "range cannot be used as a value"),
        }
    }

    fn gen_lvalue(&mut self, e: &Expr) -> Result<String> {
        let span = e.span;
        match &e.kind {
            ExprKind::Var(name) => Ok(name.clone()),
            ExprKind::Field(base, field) => {
                let b = self.gen_expr(base)?;
                Ok(format!("({}).{}", b, field))
            }
            ExprKind::Deref(e) => {
                let expr = self.gen_expr(e)?;
                Ok(format!("(*({}))", expr))
            }
            _ => err(span, "invalid assignment target"),
        }
    }

    fn gen_range(&mut self, e: &Expr) -> Result<(String, String)> {
        let span = e.span;
        match &e.kind {
            ExprKind::Range(a, b) => {
                let av = self.gen_expr(a)?;
                let bv = self.gen_expr(b)?;
                Ok((av, bv))
            }
            _ => err(span, "expected range expression"),
        }
    }

    fn expr_type(&self, e: &Expr) -> Result<Type> {
        let span = e.span;
        match &e.kind {
            ExprKind::IntLit(_) => Ok(Type::Int),
            ExprKind::FloatLit(_) => Ok(Type::Float),
            ExprKind::BoolLit(_) => Ok(Type::Bool),
            ExprKind::TextLit(_) => Ok(Type::Text),
            ExprKind::NoneLit => Ok(Type::Ptr(Box::new(Type::Unit))),
            ExprKind::New(ty, _) => Ok(Type::Ptr(Box::new((**ty).clone()))),
            ExprKind::StructInit(name, _) => Ok(Type::Named(name.clone())),
            ExprKind::Call(name, args) if name == "show" => {
                if args.is_empty() {
                    return err(span, "show expects 1 argument");
                }
                Ok(Type::Unit)
            }
            ExprKind::Call(name, args) => {
                // Kind variant constructor?
                for kind in self.kinds.values() {
                    if let Some(variant) = kind.variants.iter().find(|v| v.tag == *name) {
                        let _ = variant;
                        if args.len() != variant.payload.len() {
                            return err(span, "wrong number of arguments");
                        }
                        return Ok(Type::Named(kind.name.clone()));
                    }
                }
                let (params, ret) = self.tasks.get(name)
                    .ok_or_else(|| Error::at(span, format!("unknown task '{}'", name)))?
                    .clone();
                if args.len() != params.len() {
                    return err(span, "wrong number of arguments");
                }
                Ok(ret)
            }
            ExprKind::Field(base, field) => {
                let base_ty = self.expr_type(base)?;
                let shape_name = match &base_ty {
                    Type::Named(n) => n.clone(),
                    _ => return err(span, "field access on non-shape"),
                };
                let shape = self.shapes.get(&shape_name).ok_or_else(|| Error::at(span, format!("unknown shape '{}'", shape_name)))?;
                for f in &shape.fields {
                    if f.name == *field {
                        return Ok(f.ty.clone());
                    }
                }
                err(span, format!("shape '{}' has no field '{}'", shape_name, field))
            }
            ExprKind::Deref(e) => {
                let ty = self.expr_type(e)?;
                ty.ptr_inner().cloned().ok_or_else(|| Error::at(span, "cannot dereference non-pointer"))
            }
            ExprKind::Unary(op, e) => {
                let ty = self.expr_type(e)?;
                match op {
                    UnaryOp::Neg => Ok(ty),
                    UnaryOp::Not => Ok(Type::Bool),
                }
            }
            ExprKind::Binary(op, l, _r) => {
                let lt = self.expr_type(l)?;
                match op {
                    BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => Ok(lt),
                    BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Gt | BinaryOp::Le | BinaryOp::Ge | BinaryOp::And | BinaryOp::Or => Ok(Type::Bool),
                }
            }
            ExprKind::Var(name) => {
                self.lookup_var(name).cloned().ok_or_else(|| Error::at(span, format!("unknown variable '{}'", name)))
            }
            _ => err(span, "cannot infer expression type for codegen"),
        }
    }
}

fn mangle(name: &str) -> String {
    // Prefix to avoid collisions with C keywords/runtime
    format!("vela_{}", name)
}

fn c_type(ty: &Type) -> String {
    match ty {
        Type::Int => "int64_t".to_string(),
        Type::Float => "double".to_string(),
        Type::Bool => "bool".to_string(),
        Type::Text => "const char*".to_string(),
        Type::Unit => "void".to_string(),
        Type::Ptr(t) => format!("{}*", c_type(t)),
        Type::Named(n) => mangle(n),
        Type::Range(t) => format!("/*range<{}>*/", c_type(t)),
    }
}

fn indent_level(pad: &str) -> usize {
    pad.len() / 4
}

fn escape_c_string(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

fn err<T>(span: Span, msg: impl Into<String>) -> Result<T> {
    Err(Error::at(span, msg))
}
