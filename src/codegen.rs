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

        // Types. A typedef for every shape and kind comes first, so any type can
        // point to any other (or to itself); then the kinds' tag enums; then the
        // struct bodies, each after the types it contains by value.
        for item in items {
            if let Some(name) = type_name(item) {
                self.w.line(&format!("typedef struct {0} {0};", mangle(name)));
            }
        }
        for item in items {
            if let Item::Kind(k) = item {
                self.gen_tag_enum(k);
            }
        }
        for item in type_order(items)? {
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
        format!("vela__{}__{}", prefix, n)
    }

    fn gen_shape(&mut self, s: &ShapeDef) {
        self.w.open(&format!("struct {}", mangle(&s.name)));
        if s.fields.is_empty() {
            // ISO C has no empty structs.
            self.w.line("char vela__empty__;");
        }
        for field in &s.fields {
            self.w.line(&format!("{} {};", c_type(&field.ty), c_ident(&field.name)));
        }
        self.w.close_with(";");
    }

    fn gen_tag_enum(&mut self, k: &KindDef) {
        let tag_type = tag_type(&k.name);
        self.w.open(&format!("typedef enum {}", tag_type));
        for v in &k.variants {
            self.w.line(&format!("{},", tag_value(&k.name, &v.tag)));
        }
        self.w.close_with(&format!(" {};", tag_type));
    }

    fn gen_kind(&mut self, k: &KindDef) {
        self.w.open(&format!("struct {}", mangle(&k.name)));
        self.w.line(&format!("{} tag;", tag_type(&k.name)));
        // Variants without a payload have no union member, and a kind with no
        // payloads at all has no union (ISO C has no empty unions).
        if k.variants.iter().any(|v| !v.payload.is_empty()) {
            self.w.open("union");
            for v in &k.variants {
                if !v.payload.is_empty() {
                    self.w.open("struct");
                    for field in &v.payload {
                        self.w.line(&format!("{} {};", c_type(&field.ty), c_ident(&field.name)));
                    }
                    self.w.close_with(&format!(" {};", c_ident(&v.tag)));
                }
            }
            self.w.close_with(" data;");
        }
        self.w.close_with(";");
    }

    fn gen_task(&mut self, t: &TaskDef) -> Result<()> {
        self.w.open(&signature(t));
        self.gen_block(&t.body)?;
        if t.ret == Type::Unit {
            self.w.line("return;");
        } else {
            self.w.line(&format!("vela__rt__missing_send(\"{}\");", t.name));
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
                self.w.line(&format!("{} {} = {};", c_type(ty), c_ident(name), expr));
            }
            StmtKind::Assign(lhs, rhs) => {
                // The checker guarantees `lhs` is a variable, field, or deref,
                // all of which generate valid C lvalues.
                let l = self.gen_expr(lhs)?;
                let r = self.gen_expr(rhs)?;
                self.w.line(&format!("{} = {};", l, r));
            }
            StmtKind::When(cond, then_b, else_b) => {
                let c = self.gen_condition(cond)?;
                self.w.open(&format!("if ({})", c));
                self.gen_block(then_b)?;
                if let Some(else_b) = else_b {
                    self.w.reopen("else");
                    self.gen_block(else_b)?;
                }
                self.w.close();
            }
            StmtKind::Loop(cond, body) => {
                let c = self.gen_condition(cond)?;
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
                let name = c_ident(name);
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
                self.w.line(&format!("vela__rt__free((void*){});", expr));
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
            self.w.open(&format!("case {}:", tag_value(&kind_name, &case.tag)));
            for ((bind, _), field) in case.binds.iter().zip(&variant.payload) {
                let bind = c_ident(bind);
                self.w.line(&format!(
                    "{} {} = {}.data.{}.{};",
                    c_type(&field.ty), bind, subject, c_ident(&case.tag), c_ident(&field.name)
                ));
                // A case must bind every field, used or not; don't let C warn.
                self.w.line(&format!("(void){};", bind));
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

    /// An `if`/`while` condition. The statement supplies the parentheses, so a
    /// binary operation drops its own (C compilers warn about `if ((a == b))`).
    fn gen_condition(&mut self, e: &Expr) -> Result<String> {
        match &e.kind {
            ExprKind::Binary(op, l, r) => self.gen_binary(e, *op, l, r),
            _ => self.gen_expr(e),
        }
    }

    fn gen_expr(&mut self, e: &Expr) -> Result<String> {
        let span = e.span;
        match &e.kind {
            ExprKind::IntLit(n) => Ok(n.to_string()),
            ExprKind::FloatLit(n) => Ok(c_float_literal(*n)),
            ExprKind::BoolLit(b) => Ok(if *b { "true".to_string() } else { "false".to_string() }),
            ExprKind::TextLit(s) => Ok(format!("\"{}\"", escape_c_string(s))),
            ExprKind::NoneLit => Ok("NULL".to_string()),
            ExprKind::Var(name) => Ok(c_ident(name)),
            ExprKind::Field(base, field) => {
                let b = self.gen_expr(base)?;
                Ok(format!("({}).{}", b, c_ident(field)))
            }
            ExprKind::Index(_base, _index) => Err(Error::internal_at(span, "indexing reached code generation")),
            ExprKind::Call(name, args) => {
                if name == "show" {
                    let [arg] = args.as_slice() else {
                        return Err(Error::internal_at(span, format!("show called with {} arguments", args.len())));
                    };
                    let helper = match arg.ty()? {
                        Type::Int => "vela__rt__show_int",
                        Type::Float => "vela__rt__show_float",
                        Type::Bool => "vela__rt__show_bool",
                        Type::Text => "vela__rt__show_text",
                        other => return Err(Error::internal_at(span, format!("show called with type {}", other))),
                    };
                    let value = self.gen_expr(arg)?;
                    return Ok(format!("{}({})", helper, value));
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
                let binary = self.gen_binary(e, *op, l, r)?;
                Ok(format!("({})", binary))
            }
            ExprKind::Construct { kind, tag, args } => {
                let variant = self
                    .kinds
                    .get(kind)
                    .and_then(|k| k.variants.iter().find(|v| v.tag == *tag))
                    .cloned()
                    .ok_or_else(|| Error::internal_at(span, format!("unknown variant '{}'", tag)))?;
                if args.len() != variant.payload.len() {
                    return Err(Error::internal_at(
                        span,
                        format!("variant '{}' called with {} arguments (expected {})", tag, args.len(), variant.payload.len()),
                    ));
                }
                let tag_value = tag_value(kind, tag);
                if variant.payload.is_empty() {
                    return Ok(format!("(({}){{ .tag = {} }})", mangle(kind), tag_value));
                }
                let mut fields = Vec::new();
                for (field, arg) in variant.payload.iter().zip(args) {
                    fields.push(format!(".{} = {}", c_ident(&field.name), self.gen_expr(arg)?));
                }
                Ok(format!(
                    "(({}){{ .tag = {}, .data = {{ .{} = {{ {} }} }} }})",
                    mangle(kind), tag_value, c_ident(tag), fields.join(", ")
                ))
            }
            ExprKind::New { ty, value, .. } => {
                // Copies the value out of a one-element array compound literal,
                // which works for every type in standard C.
                let val = self.gen_expr(value)?;
                let ctype = c_type(ty);
                Ok(format!("(({0}*)vela__rt__box(({0}[1]){{ {1} }}, sizeof({0})))", ctype, val))
            }
            ExprKind::Deref(inner) => {
                let expr = self.gen_expr(inner)?;
                Ok(format!("(*({}))", expr))
            }
            ExprKind::StructInit(name, fields) => {
                if fields.is_empty() {
                    // Initializes the placeholder member of an empty shape.
                    return Ok(format!("(({}){{ 0 }})", mangle(name)));
                }
                let mut values = Vec::new();
                for field in fields {
                    values.push(format!(".{} = {}", c_ident(&field.name), self.gen_expr(&field.value)?));
                }
                Ok(format!("(({}){{ {} }})", mangle(name), values.join(", ")))
            }
            ExprKind::Range(_, _) => Err(Error::internal_at(span, "range outside 'each' reached code generation")),
        }
    }

    /// A binary operation without surrounding parentheses.
    fn gen_binary(&mut self, e: &Expr, op: BinaryOp, l: &Expr, r: &Expr) -> Result<String> {
        let left = self.gen_expr(l)?;
        let right = self.gen_expr(r)?;
        if matches!(op, BinaryOp::Eq | BinaryOp::Ne) && *l.ty()? == Type::Text {
            // Text compares by contents, not by address.
            let not = if op == BinaryOp::Ne { "!" } else { "" };
            return Ok(format!("{}vela__rt__text_eq({}, {})", not, left, right));
        }
        if op == BinaryOp::Mod && *e.ty()? == Type::Float {
            // C's `%` is integer-only.
            return Ok(format!("vela__rt__fmod({}, {})", left, right));
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
        Ok(format!("{} {} {}", left, op_s, right))
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

fn type_name(item: &Item) -> Option<&str> {
    match item {
        Item::Shape(s) => Some(&s.name),
        Item::Kind(k) => Some(&k.name),
        Item::Task(_) => None,
    }
}

/// Shapes and kinds in source order, except that each comes after every type
/// it contains by value, since C needs a struct's complete definition before
/// it can be embedded. (The checker has rejected types that contain
/// themselves by value.)
fn type_order(items: &[Item]) -> Result<Vec<&Item>> {
    let by_name: HashMap<&str, &Item> = items
        .iter()
        .filter_map(|item| type_name(item).map(|name| (name, item)))
        .collect();
    let mut finished = HashMap::new();
    let mut order = Vec::new();
    for item in items {
        if let Some(name) = type_name(item) {
            visit_type(name, &by_name, &mut finished, &mut order)?;
        }
    }
    Ok(order)
}

/// Depth-first step of `type_order`. `finished` maps a type to `false` while
/// its dependencies are being emitted and `true` once it is in `order`.
fn visit_type<'a>(
    name: &'a str,
    by_name: &HashMap<&'a str, &'a Item>,
    finished: &mut HashMap<&'a str, bool>,
    order: &mut Vec<&'a Item>,
) -> Result<()> {
    match finished.get(name) {
        Some(true) => return Ok(()),
        Some(false) => return Err(Error::internal(format!("type '{}' contains itself by value", name))),
        None => {}
    }
    let Some(&item) = by_name.get(name) else {
        return Ok(()); // a builtin type
    };
    finished.insert(name, false);
    let fields: Vec<&Field> = match item {
        Item::Shape(s) => s.fields.iter().collect(),
        Item::Kind(k) => k.variants.iter().flat_map(|v| &v.payload).collect(),
        Item::Task(_) => Vec::new(),
    };
    for field in fields {
        if let Type::Named(dep) = &field.ty {
            visit_type(dep, by_name, finished, order)?;
        }
    }
    finished.insert(name, true);
    order.push(item);
    Ok(())
}

/// The C declarator of a task, e.g. `int64_t vela_add(int64_t a, int64_t b)`.
fn signature(t: &TaskDef) -> String {
    let params: Vec<String> = t
        .params
        .iter()
        .map(|p| format!("{} {}", c_type(&p.ty), c_ident(&p.name)))
        .collect();
    // `()` in C declares a function with unspecified parameters; `(void)` means none.
    let params = if params.is_empty() { "void".to_string() } else { params.join(", ") };
    format!("{} {}({})", c_type(&t.ret), mangle(&t.name), params)
}

// C names. Vela names start with an ASCII letter and never contain `__`, so
// the generated C names below have distinct shapes and can't collide with each
// other, with the runtime (`vela__rt__*`), or with C:
//
//   vela_Point, vela_area         tasks, shapes, kinds          (`mangle`)
//   vela_Shape__Tag               a kind's tag enum type        (`tag_type`)
//   vela_Shape__tag_Circle        a variant's tag value         (`tag_value`)
//   vela__int                     a local or field named like C (`c_ident`)
//   vela__end__0, vela__empty__   compiler temporaries and members

/// The C name of a task, shape, or kind.
fn mangle(name: &str) -> String {
    format!("vela_{}", name)
}

fn tag_type(kind: &str) -> String {
    format!("vela_{}__Tag", kind)
}

fn tag_value(kind: &str, tag: &str) -> String {
    format!("vela_{}__tag_{}", kind, tag)
}

/// The C name of a local, parameter, field, or variant union member: the Vela
/// name itself, unless C could read it as something else, such as a keyword
/// or a macro from a standard header.
fn c_ident(name: &str) -> String {
    if collides_with_c(name) {
        format!("vela__{}", name)
    } else {
        name.to_string()
    }
}

fn collides_with_c(name: &str) -> bool {
    C_RESERVED.contains(&name)
        // Our own prefix, and POSIX's reserved `_t` type suffix.
        || name.starts_with("vela_")
        || name.ends_with("_t")
        // Object-like macros such as EOF, NULL, and INT64_MAX.
        || name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// C11 and C23 keywords, and lowercase object-like macros that the runtime's
/// headers (or common compilers) may define.
const C_RESERVED: &[&str] = &[
    "alignas", "alignof", "auto", "bool", "break", "case", "char", "const", "constexpr",
    "continue", "default", "do", "double", "else", "enum", "extern", "false", "float", "for",
    "goto", "if", "inline", "int", "long", "nullptr", "register", "restrict", "return", "short",
    "signed", "sizeof", "static", "static_assert", "struct", "switch", "thread_local", "true",
    "typedef", "typeof", "typeof_unqual", "union", "unsigned", "void", "volatile", "while",
    "asm", "complex", "errno", "imaginary", "linux", "math_errhandling", "noreturn", "stderr",
    "stdin", "stdout", "unix", "L_tmpnam",
];

fn c_type(ty: &Type) -> String {
    match ty {
        Type::Int => "int64_t".to_string(),
        Type::Float => "double".to_string(),
        Type::Bool => "bool".to_string(),
        Type::Text => "const char*".to_string(),
        Type::Unit => "void".to_string(),
        Type::None => "void*".to_string(),
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

/// Escapes text for a C string literal. Everything outside printable ASCII
/// (control characters, NUL, and each byte of a non-ASCII character) becomes a
/// three-digit octal escape, which, unlike `\x`, can't absorb the characters
/// after it; `?` is escaped so C never sees a trigraph such as `??!`.
fn escape_c_string(s: &str) -> String {
    let mut out = String::new();
    for byte in s.bytes() {
        match byte {
            b'\\' => out.push_str("\\\\"),
            b'"' => out.push_str("\\\""),
            b'?' => out.push_str("\\?"),
            b'\n' => out.push_str("\\n"),
            b'\t' => out.push_str("\\t"),
            b'\r' => out.push_str("\\r"),
            b' '..=b'~' => out.push(char::from(byte)),
            _ => out.push_str(&format!("\\{:03o}", byte)),
        }
    }
    out
}
