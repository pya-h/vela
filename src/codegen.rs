use std::collections::HashMap;

use crate::ast::*;
use crate::error::{Error, Result};

/// Accumulates C source line by line and tracks indentation. Appending to a
/// `String` cannot fail, so no method returns a `Result`.
#[derive(Default)]
struct CWriter {
    out: String,
    depth: usize,
}

impl CWriter {
    /// Writes `text` as a full line at the current indentation.
    fn line(&mut self, text: &str) {
        for _ in 0..self.depth {
            self.out.push_str("    ");
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn blank(&mut self) {
        self.out.push('\n');
    }

    /// Writes `header {` (or just `{` for an empty header) and indents the
    /// lines that follow.
    fn open(&mut self, header: &str) {
        if header.is_empty() {
            self.line("{");
        } else {
            self.line(&format!("{} {{", header));
        }
        self.depth += 1;
    }

    /// Ends the innermost block with `}` followed by `trailer`, as in `} Point;`.
    fn close_with(&mut self, trailer: &str) {
        self.depth = self.depth.saturating_sub(1);
        self.line(&format!("}}{}", trailer));
    }

    fn close(&mut self) {
        self.close_with("");
    }

    /// Ends the innermost block and opens the next one, as in `} else {`.
    fn reopen(&mut self, keyword: &str) {
        self.close_with(&format!(" {} {{", keyword));
        self.depth += 1;
    }
}

/// Generates C from a program that has already been checked by
/// `check::Checker`. Types come from the checker's annotations; the generator
/// does no type inference of its own. A missing annotation or other broken
/// checker guarantee is reported as an internal compiler error.
#[derive(Default)]
pub struct Generator {
    w: CWriter,
    temp_counter: u64,
    kinds: HashMap<String, KindDef>,
}

impl Generator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn generate(&mut self, items: &[Item]) -> Result<String> {
        self.kinds.clear();
        for item in items {
            if let Item::Kind(k) = item {
                self.kinds.insert(k.name.clone(), k.clone());
            }
        }

        self.w = CWriter::default();
        self.w.line("#include \"vela_rt.h\"");
        self.w.blank();

        // Type definitions first
        for item in items {
            match item {
                Item::Shape(s) => self.gen_shape(s),
                Item::Kind(k) => self.gen_kind(k),
                Item::Task(_) => {}
            }
        }
        self.w.blank();

        // Forward declarations for tasks
        for item in items {
            if let Item::Task(t) = item {
                self.w.line(&format!("{};", signature(t)));
            }
        }
        self.w.blank();

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
            self.w.blank();
            self.w.open("int main(void)");
            self.w.line("vela_main();");
            self.w.line("return 0;");
            self.w.close();
        }

        Ok(std::mem::take(&mut self.w.out))
    }

    fn fresh_temp(&mut self, prefix: &str) -> String {
        let n = self.temp_counter;
        self.temp_counter += 1;
        format!("{}__{}", prefix, n)
    }

    fn gen_shape(&mut self, s: &ShapeDef) {
        self.w.open(&format!("typedef struct {}", mangle(&s.name)));
        for field in &s.fields {
            self.w.line(&format!("{} {};", c_type(&field.ty), field.name));
        }
        self.w.close_with(&format!(" {};", mangle(&s.name)));
    }

    fn gen_kind(&mut self, k: &KindDef) {
        let tag_name = format!("{}_Tag", mangle(&k.name));
        self.w.open(&format!("typedef enum {}", tag_name));
        for v in &k.variants {
            self.w.line(&format!("{}_{},", tag_name, v.tag));
        }
        self.w.close_with(&format!(" {};", tag_name));

        self.w.open(&format!("typedef struct {}", mangle(&k.name)));
        self.w.line(&format!("{} tag;", tag_name));
        self.w.open("union");
        for v in &k.variants {
            if !v.payload.is_empty() {
                self.w.open("struct");
                for field in &v.payload {
                    self.w.line(&format!("{} {};", c_type(&field.ty), field.name));
                }
                self.w.close_with(&format!(" {};", v.tag));
            }
        }
        self.w.close_with(" data;");
        self.w.close_with(&format!(" {};", mangle(&k.name)));
    }

    fn gen_task(&mut self, t: &TaskDef) -> Result<()> {
        self.w.open(&signature(t));
        self.gen_block(&t.body)?;
        if t.ret == Type::Unit {
            self.w.line("return;");
        }
        self.w.close();
        Ok(())
    }

    fn gen_block(&mut self, block: &Block) -> Result<()> {
        for stmt in block {
            self.gen_stmt(stmt)?;
        }
        Ok(())
    }

    fn gen_stmt(&mut self, stmt: &Stmt) -> Result<()> {
        match &stmt.kind {
            StmtKind::Expr(e) => {
                let expr = self.gen_expr(e)?;
                self.w.line(&format!("{};", expr));
            }
            StmtKind::Binding { name, ty, init, .. } => {
                let ty = ty
                    .as_ref()
                    .ok_or_else(|| Error::internal_at(stmt.span, format!("binding '{}' has no type", name)))?;
                let expr = self.gen_expr(init)?;
                self.w.line(&format!("{} {} = {};", c_type(ty), name, expr));
            }
            StmtKind::Assign(lhs, rhs) => {
                // The checker guarantees `lhs` is a variable, field, or deref,
                // all of which generate valid C lvalues.
                let l = self.gen_expr(lhs)?;
                let r = self.gen_expr(rhs)?;
                self.w.line(&format!("{} = {};", l, r));
            }
            StmtKind::When(cond, then_b, else_b) => {
                let c = self.gen_expr(cond)?;
                self.w.open(&format!("if ({})", c));
                self.gen_block(then_b)?;
                if let Some(else_b) = else_b {
                    self.w.reopen("else");
                    self.gen_block(else_b)?;
                }
                self.w.close();
            }
            StmtKind::Loop(cond, body) => {
                let c = self.gen_expr(cond)?;
                self.w.open(&format!("while ({})", c));
                self.gen_block(body)?;
                self.w.close();
            }
            StmtKind::Each(name, range, body) => {
                let (a, b) = self.gen_range(range)?;
                // The end of the range is evaluated once, before the first
                // iteration. (The checker forbids shadowing, so `b` cannot
                // refer to a different variable called `name`.)
                let end = self.fresh_temp("end");
                self.w.open(&format!(
                    "for (int64_t {} = {}, {} = {}; {} < {}; {}++)",
                    name, a, end, b, name, end, name
                ));
                self.gen_block(body)?;
                self.w.close();
            }
            StmtKind::Match(expr, cases) => self.gen_match(expr, cases)?,
            StmtKind::Send(None) => self.w.line("return;"),
            StmtKind::Send(Some(e)) => {
                let expr = self.gen_expr(e)?;
                self.w.line(&format!("return {};", expr));
            }
            StmtKind::Free(e) => {
                let expr = self.gen_expr(e)?;
                self.w.line(&format!("vela_free((void*){});", expr));
            }
        }
        Ok(())
    }

    /// Generates a `switch` on the kind's tag. Each case body gets its own C
    /// block, so different cases may bind the same names. The matched value
    /// is evaluated exactly once: a plain variable is used directly, anything
    /// else is first stored in a temporary.
    fn gen_match(&mut self, expr: &Expr, cases: &[Case]) -> Result<()> {
        let kind_name = match expr.ty()? {
            Type::Named(n) => n.clone(),
            other => return Err(Error::internal_at(expr.span, format!("match on non-kind type {}", other))),
        };
        let kind = self
            .kinds
            .get(&kind_name)
            .ok_or_else(|| Error::internal_at(expr.span, format!("match on unknown kind '{}'", kind_name)))?
            .clone();
        let tag_name = format!("{}_Tag", mangle(&kind_name));

        let value = self.gen_expr(expr)?;
        let needs_temp = !matches!(expr.kind, ExprKind::Var(_));
        let subject = if needs_temp {
            let tmp = self.fresh_temp("match");
            self.w.open("");
            self.w.line(&format!("{} {} = {};", mangle(&kind_name), tmp, value));
            tmp
        } else {
            value
        };

        // Case labels line up with their `switch`, so the switch's own braces
        // do not indent.
        self.w.line(&format!("switch ({}.tag) {{", subject));
        for case in cases {
            let variant = kind
                .variants
                .iter()
                .find(|v| v.tag == case.tag)
                .ok_or_else(|| Error::internal_at(case.span, format!("unknown variant '{}'", case.tag)))?;
            if case.binds.len() != variant.payload.len() {
                return Err(Error::internal_at(
                    case.span,
                    format!("case '{}' has {} bindings (expected {})", case.tag, case.binds.len(), variant.payload.len()),
                ));
            }
            self.w.open(&format!("case {}_{}:", tag_name, case.tag));
            for ((bind, _), field) in case.binds.iter().zip(&variant.payload) {
                self.w.line(&format!(
                    "{} {} = {}.data.{}.{};",
                    c_type(&field.ty), bind, subject, case.tag, field.name
                ));
            }
            self.gen_block(&case.body)?;
            self.w.line("break;");
            self.w.close();
        }
        self.w.line("}");

        if needs_temp {
            self.w.close();
        }
        Ok(())
    }

    fn gen_expr(&mut self, e: &Expr) -> Result<String> {
        let span = e.span;
        match &e.kind {
            ExprKind::IntLit(n) => Ok(n.to_string()),
            ExprKind::FloatLit(n) => Ok(c_float_literal(*n)),
            ExprKind::BoolLit(b) => Ok(if *b { "true".to_string() } else { "false".to_string() }),
            ExprKind::TextLit(s) => Ok(format!("\"{}\"", escape_c_string(s))),
            ExprKind::NoneLit => Ok("NULL".to_string()),
            ExprKind::Var(name) => Ok(name.clone()),
            ExprKind::Field(base, field) => {
                let b = self.gen_expr(base)?;
                Ok(format!("({}).{}", b, field))
            }
            ExprKind::Index(_base, _index) => Err(Error::internal_at(span, "indexing reached code generation")),
            ExprKind::Call(name, args) => {
                if name == "show" {
                    let [arg] = args.as_slice() else {
                        return Err(Error::internal_at(span, format!("show called with {} arguments", args.len())));
                    };
                    let helper = match arg.ty()? {
                        Type::Int => "vela_show_int",
                        Type::Float => "vela_show_float",
                        Type::Bool => "vela_show_bool",
                        Type::Text => "vela_show_text",
                        other => return Err(Error::internal_at(span, format!("show called with type {}", other))),
                    };
                    let value = self.gen_expr(arg)?;
                    return Ok(format!("{}({})", helper, value));
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
                    if args.len() != variant.payload.len() {
                        return Err(Error::internal_at(
                            span,
                            format!("variant '{}' called with {} arguments (expected {})", name, args.len(), variant.payload.len()),
                        ));
                    }
                    let mut fields = Vec::new();
                    for (field, arg) in variant.payload.iter().zip(args) {
                        fields.push(format!(".{} = {}", field.name, self.gen_expr(arg)?));
                    }
                    let tag_name = format!("{}_Tag", mangle(&kind_name));
                    return Ok(format!(
                        "(({}){{ .tag = {}_{}, .data = {{ .{} = {{ {} }} }} }})",
                        mangle(&kind_name), tag_name, variant.tag, variant.tag, fields.join(", ")
                    ));
                }
                let values = args.iter().map(|arg| self.gen_expr(arg)).collect::<Result<Vec<_>>>()?;
                Ok(format!("{}({})", mangle(name), values.join(", ")))
            }
            ExprKind::Unary(op, operand) => {
                let expr = self.gen_expr(operand)?;
                let op_s = match op {
                    UnaryOp::Neg => "-",
                    UnaryOp::Not => "!",
                };
                Ok(format!("({}{})", op_s, expr))
            }
            ExprKind::Binary(op, l, r) => {
                let left = self.gen_expr(l)?;
                let right = self.gen_expr(r)?;
                if *op == BinaryOp::Mod && *e.ty()? == Type::Float {
                    // C's `%` is integer-only.
                    return Ok(format!("vela_fmod({}, {})", left, right));
                }
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
            ExprKind::New(ty, value) => {
                let val = self.gen_expr(value)?;
                let tmp = self.fresh_temp("p");
                let ctype = c_type(ty);
                Ok(format!("({{ {}* {} = ({}*)vela_new(sizeof({})); *{} = {}; {}; }})", ctype, tmp, ctype, ctype, tmp, val, tmp))
            }
            ExprKind::Deref(inner) => {
                let expr = self.gen_expr(inner)?;
                Ok(format!("(*({}))", expr))
            }
            ExprKind::StructInit(name, fields) => {
                let mut values = Vec::new();
                for (fname, fexpr) in fields {
                    values.push(format!(".{} = {}", fname, self.gen_expr(fexpr)?));
                }
                Ok(format!("((struct {}){{ {} }})", mangle(name), values.join(", ")))
            }
            ExprKind::Range(_, _) => Err(Error::at(span, "range cannot be used as a value")),
        }
    }

    fn gen_range(&mut self, e: &Expr) -> Result<(String, String)> {
        match &e.kind {
            ExprKind::Range(a, b) => {
                let av = self.gen_expr(a)?;
                let bv = self.gen_expr(b)?;
                Ok((av, bv))
            }
            _ => Err(Error::internal_at(e.span, "each over a non-range expression")),
        }
    }
}

/// The C declarator of a task, e.g. `int64_t vela_add(int64_t a, int64_t b)`.
fn signature(t: &TaskDef) -> String {
    let params: Vec<String> = t
        .params
        .iter()
        .map(|p| format!("{} {}", c_type(&p.ty), p.name))
        .collect();
    format!("{} {}({})", c_type(&t.ret), mangle(&t.name), params.join(", "))
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

/// Formats a float so C always reads it as a `double` literal: `3.0` rather
/// than `3` (which C would treat as an integer), and exponent form such as
/// `1e300` for very large or small magnitudes. The lexer rejects non-finite
/// literals, so `inf`/`NaN` never reach here.
fn c_float_literal(n: f64) -> String {
    // Debug formatting always includes a decimal point or an exponent.
    format!("{:?}", n)
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
