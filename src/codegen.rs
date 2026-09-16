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
///
/// An expression is normally one C expression. One that contains a plan B
/// (`x { … }`) needs statements, so generating it writes those statements
/// ("the prelude") before the line that uses its value; see `gen_operands`.
///
/// A `region r` is an arena, `vela__rt__region r[1]`: as an array, `r` is a
/// pointer to it, like a region parameter. Every block frees the regions it
/// declared when it closes, and a `<-` frees every region the task declared
/// before it returns.
pub struct Generator {
    w: CWriter,
    temp_counter: u64,
    kinds: HashMap<String, KindDef>,
    /// The return type of the task being generated.
    ret: Type,
    /// The regions declared in each block being generated, outermost first.
    regions: Vec<Vec<String>>,
    /// The names of the task's results, in order, when it names them.
    results: Vec<String>,
    /// The name of the task being generated, for runtime messages.
    task: String,
}

impl Default for Generator {
    fn default() -> Self {
        Generator { w: CWriter::default(), temp_counter: 0, kinds: HashMap::new(), ret: Type::Unit, regions: Vec::new(), results: Vec::new(), task: String::new() }
    }
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
        let used = UsedTypes::collect(items);

        self.w = CWriter::default();
        self.w.line("#include \"vela_rt.h\"");
        self.w.blank();

        // Types. A typedef for every shape, kind, `maybe`, and `or` comes first,
        // so any type can point to any other (or to itself); then the kinds' tag
        // enums; then the struct bodies, each after the types it contains by
        // value.
        for item in items {
            if let Some(name) = type_name(item) {
                self.w.line(&format!("typedef struct {0} {0};", mangle(name)));
            }
        }
        for ty in &used.compounds {
            self.w.line(&format!("typedef struct {0} {0};", c_type(ty)));
        }
        for item in items {
            if let Item::Kind(k) = item {
                self.gen_tag_enum(k);
            }
        }
        for def in type_order(items, &used.compounds)? {
            match def {
                TypeDef::Item(Item::Shape(s)) => self.gen_shape(s),
                TypeDef::Item(Item::Kind(k)) => self.gen_kind(k),
                TypeDef::Item(Item::Task(_)) => {}
                TypeDef::Compound(ty) => self.gen_compound(ty),
            }
        }
        for ty in &used.shown {
            self.gen_show_maybe(ty);
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
            // Line by line even into a pipe, so output isn't lost if the
            // program then crashes.
            self.w.line("setvbuf(stdout, NULL, _IOLBF, BUFSIZ);");
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

    /// The struct of a `maybe T` (`has`, `value`), a `T or E` (`ok`, `value`,
    /// `error`; no `value` for `unit or E`), or several results (`r0`, `r1`,
    /// …). A `maybe`'s and an `or`'s members are always initialized, so either
    /// can be read.
    fn gen_compound(&mut self, ty: &Type) {
        self.w.open(&format!("struct {}", c_type(ty)));
        match ty {
            Type::Results(types) => {
                for (i, result) in types.iter().enumerate() {
                    self.w.line(&format!("{} {};", c_type(result), result_member(i)));
                }
            }
            Type::Maybe(inner) => {
                self.w.line("bool has;");
                self.w.line(&format!("{} value;", c_type(inner)));
            }
            Type::Or(value, error) => {
                self.w.line("bool ok;");
                if **value != Type::Unit {
                    self.w.line(&format!("{} value;", c_type(value)));
                }
                self.w.line(&format!("{} error;", c_type(error)));
            }
            _ => {}
        }
        self.w.close_with(";");
    }

    /// `show` for a `maybe` of a type `show` prints: the value, or `none`.
    fn gen_show_maybe(&mut self, ty: &Type) {
        let Type::Maybe(inner) = ty else { return };
        let helper = show_helper(inner);
        self.w.open(&format!("static void {}({} m)", show_maybe_helper(ty), c_type(ty)));
        self.w.open("if (m.has)");
        self.w.line(&format!("{}(m.value);", helper));
        self.w.reopen("else");
        self.w.line("vela__rt__show_none();");
        self.w.close();
        self.w.close();
    }

    fn gen_task(&mut self, t: &TaskDef) -> Result<()> {
        self.ret = t.ret.clone();
        self.regions.clear();
        self.results = t.result_names().map(|(name, _)| name.clone()).collect();
        self.task = t.name.clone();
        self.w.open(&signature(t));
        for param in &t.params {
            self.use_if_unread(&param.name, &t.body);
        }
        // Named results are locals of the task. The checker has made sure none
        // is read before it is set; the zero is there so that a checker of
        // mine that ever missed one would still read a defined value.
        for (result, ty) in t.results.iter().zip(t.ret.results()) {
            if let Some((name, _)) = &result.name {
                self.w.line(&format!("{} {} = {{0}};", c_type(ty), c_ident(name)));
            }
        }
        self.gen_block(&t.body)?;
        match &t.ret {
            Type::Unit => self.w.line("return;"),
            Type::Or(value, _) if **value == Type::Unit => {
                self.w.line(&format!("return {};", success(&t.ret)));
            }
            // Reaching the end of a task sends its results as they are.
            _ if !self.results.is_empty() => {
                let results = self.current_results();
                self.w.line(&format!("return {};", results));
            }
            _ => self.w.line(&format!("vela__rt__missing_send(\"{}\");", t.name)),
        }
        self.w.close();
        self.results.clear();
        Ok(())
    }

    /// The C expression for the task's named results as they are now: the one
    /// result, or a result struct holding them all.
    fn current_results(&self) -> String {
        let names: Vec<String> = self.results.iter().map(|name| c_ident(name)).collect();
        match names.as_slice() {
            [only] => only.clone(),
            _ => {
                let members: Vec<String> = names.iter().enumerate().map(|(i, name)| format!(".{} = {}", result_member(i), name)).collect();
                format!("(({}){{ {} }})", c_type(&self.ret), members.join(", "))
            }
        }
    }

    fn gen_block(&mut self, block: &[Stmt]) -> Result<()> {
        self.enter_block();
        self.gen_statements(block, block)?;
        self.leave_block(block);
        Ok(())
    }

    /// Starts a block that may declare regions.
    fn enter_block(&mut self) {
        self.regions.push(Vec::new());
    }

    /// Ends the block `block`, freeing the regions it declared, newest first.
    /// (After a final `<-`, which frees them itself, nothing more runs.)
    fn leave_block(&mut self, block: &[Stmt]) {
        let declared = self.regions.pop().unwrap_or_default();
        if matches!(block.last().map(|stmt| &stmt.kind), Some(StmtKind::Send(_))) {
            return;
        }
        for region in declared.iter().rev() {
            self.w.line(&format!("vela__rt__region_free({});", c_ident(region)));
        }
    }

    /// Frees every region the task has declared so far, newest first, before
    /// a `<-` leaves it. Whether any exist.
    fn free_all_regions(&mut self) -> bool {
        let all: Vec<String> = self.regions.iter().flatten().cloned().collect();
        for region in all.iter().rev() {
            self.w.line(&format!("vela__rt__region_free({});", c_ident(region)));
        }
        !all.is_empty()
    }

    /// Generates `stmts`, the first statements of `scope`: a variable declared
    /// in them counts as read if anything after it in `scope` reads it.
    fn gen_statements(&mut self, stmts: &[Stmt], scope: &[Stmt]) -> Result<()> {
        for (i, stmt) in stmts.iter().enumerate() {
            self.gen_stmt(stmt)?;
            if let StmtKind::Binding { names, .. } = &stmt.kind {
                for bound in names.iter().filter(|bound| bound.name != DISCARD) {
                    self.use_if_unread(&bound.name, scope.get(i + 1..).unwrap_or_default());
                }
            }
        }
        Ok(())
    }

    /// Vela allows variables that are never read, but C compilers warn about
    /// them (`-Wunused-variable`, `-Wunused-but-set-variable`,
    /// `-Wunused-parameter`). If nothing in `scope` reads `name`, this writes
    /// `(void)name;`, which counts as a use in C.
    fn use_if_unread(&mut self, name: &str, scope: &[Stmt]) {
        if !block_reads(scope, name) {
            self.w.line(&format!("(void){};", c_ident(name)));
        }
    }

    fn gen_stmt(&mut self, stmt: &Stmt) -> Result<()> {
        match &stmt.kind {
            StmtKind::Expr(e) => {
                let expr = self.gen_expr(e)?;
                // A plan B that gives no value is all prelude, and one that
                // does gives a temporary, which the statement drops.
                if matches!(e.kind, ExprKind::PlanB { .. }) && !expr.is_empty() {
                    self.w.line(&format!("(void){};", expr));
                } else if !expr.is_empty() {
                    self.w.line(&format!("{};", expr));
                }
            }
            StmtKind::Binding { names, init, .. } => {
                let values = self.gen_value_list(names.len(), init, stmt.span)?;
                for (bound, value) in names.iter().zip(values) {
                    let ty = bound
                        .ty
                        .as_ref()
                        .ok_or_else(|| Error::internal_at(bound.span, format!("binding '{}' has no type", bound.name)))?;
                    if bound.name == DISCARD {
                        // Nothing to declare: the value is computed and dropped.
                        self.drop_value(&value);
                    } else {
                        self.w.line(&format!("{} {} = {};", c_type(ty), c_ident(&bound.name), value));
                    }
                }
            }
            StmtKind::Assign(targets, values) => {
                // The checker guarantees every place is a variable, field, or
                // deref, all of which generate valid C lvalues.
                if let ([Target::Place(lhs)], [rhs]) = (targets.as_slice(), values.as_slice()) {
                    if has_plan_b(rhs) && !matches!(lhs.kind, ExprKind::Var(_)) {
                        // Find the place before the value's prelude runs, which
                        // might change what the place refers to.
                        let place = self.gen_expr(lhs)?;
                        let pointer = self.fresh_temp("place");
                        self.w.line(&format!("{}* {} = &{};", c_type(lhs.ty()?), pointer, place));
                        let value = self.gen_expr(rhs)?;
                        self.w.line(&format!("*{} = {};", pointer, value));
                    } else {
                        let l = self.gen_expr(lhs)?;
                        let r = self.gen_expr(rhs)?;
                        self.w.line(&format!("{} = {};", l, r));
                    }
                } else {
                    self.gen_multi_assign(targets, values, stmt.span)?;
                }
            }
            StmtKind::When(cond, then_b, else_b) => {
                match &cond.pattern {
                    None => {
                        let c = self.gen_condition(&cond.value)?;
                        self.w.open(&format!("if ({})", c));
                        self.gen_block(then_b)?;
                    }
                    Some(pattern) => {
                        let subject = self.gen_subject(&cond.value)?;
                        let test = self.pattern_test(cond.value.ty()?, &subject, pattern)?;
                        self.w.open(&format!("if ({})", test));
                        self.gen_pattern_bindings(cond.value.ty()?, &subject, pattern, then_b)?;
                        self.gen_block(then_b)?;
                    }
                }
                if let Some(else_b) = else_b {
                    self.w.reopen("else");
                    self.gen_block(else_b)?;
                }
                self.w.close();
            }
            StmtKind::Loop(cond, body) => match &cond.pattern {
                None if !has_plan_b(&cond.value) => {
                    let c = self.gen_condition(&cond.value)?;
                    self.w.open(&format!("while ({})", c));
                    self.gen_block(body)?;
                    self.w.close();
                }
                None => {
                    // The condition's prelude runs before every test.
                    self.w.open("while (1)");
                    let c = self.gen_expr(&cond.value)?;
                    self.w.open(&format!("if (!{})", parenthesized(&c)));
                    self.w.line("break;");
                    self.w.close();
                    self.gen_block(body)?;
                    self.w.close();
                }
                Some(pattern) => {
                    self.w.open("while (1)");
                    let subject = self.gen_subject(&cond.value)?;
                    let test = self.pattern_test(cond.value.ty()?, &subject, pattern)?;
                    self.w.open(&format!("if (!({}))", test));
                    self.w.line("break;");
                    self.w.close();
                    self.gen_pattern_bindings(cond.value.ty()?, &subject, pattern, body)?;
                    self.gen_block(body)?;
                    self.w.close();
                }
            },
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
            StmtKind::Send(values) if values.is_empty() => {
                // A task's own regions are freed first; the results are locals,
                // and can't point into one of them.
                let results = (!self.results.is_empty()).then(|| self.current_results());
                self.free_all_regions();
                match (results, &self.ret) {
                    (Some(results), _) => self.w.line(&format!("return {};", results)),
                    (None, Type::Or(..)) => self.w.line(&format!("return {};", success(&self.ret))),
                    (None, _) => self.w.line("return;"),
                }
            }
            StmtKind::SetResults(names, values) => {
                let given = self.gen_value_list(names.len(), values, stmt.span)?;
                for ((name, _), value) in names.iter().zip(given) {
                    self.w.line(&format!("{} = {};", c_ident(name), value));
                }
            }
            StmtKind::Send(values) => {
                let expr = self.gen_sent_value(values, stmt.span)?;
                if self.regions.iter().all(Vec::is_empty) {
                    self.w.line(&format!("return {};", expr));
                } else {
                    // The value may read memory in the regions, so it is
                    // computed before they are freed.
                    let result = self.fresh_temp("result");
                    self.w.line(&format!("{} {} = {};", c_type(&self.ret), result, expr));
                    self.free_all_regions();
                    self.w.line(&format!("return {};", result));
                }
            }
            StmtKind::Region(name) => {
                self.w.line(&format!("vela__rt__region {}[1] = {{ VELA__RT__REGION_EMPTY }};", c_ident(name)));
                if let Some(block) = self.regions.last_mut() {
                    block.push(name.clone());
                }
            }
        }
        Ok(())
    }

    /// The C expressions for `wanted` values received by a list of names or
    /// places: one per value, or the members of the result struct of a single
    /// call that sends them all. Values are evaluated in order, before the
    /// first of them is stored, so `a, b = b, a` swaps.
    fn gen_value_list(&mut self, wanted: usize, values: &[Expr], span: Span) -> Result<Vec<String>> {
        if values.len() == wanted {
            let operands: Vec<&Expr> = values.iter().collect();
            let mut given = self.gen_operands(&operands)?;
            if wanted > 1 {
                // Each value is stored before any of them is, since a place
                // assigned here may be read by a later value.
                for (value, expr) in given.iter_mut().zip(values) {
                    let temp = self.fresh_temp("value");
                    self.w.line(&format!("{} {} = {};", c_type(expr.ty()?), temp, value));
                    *value = temp;
                }
            }
            return Ok(given);
        }
        // One call, giving every value.
        let [call] = values else {
            return Err(Error::internal_at(span, format!("{} values received by {} names", values.len(), wanted)));
        };
        let Type::Results(types) = call.ty()?.clone() else {
            return Err(Error::internal_at(span, format!("a value of type {} received by {} names", call.ty()?, wanted)));
        };
        let value = self.gen_expr(call)?;
        let results = self.fresh_temp("results");
        self.w.line(&format!("{} {} = {};", c_type(&Type::Results(types.clone())), results, value));
        Ok((0..types.len()).map(|i| format!("{}.{}", results, result_member(i))).collect())
    }

    /// Writes `value`, which is received by a `_`, as a statement that drops it.
    fn drop_value(&mut self, value: &str) {
        if !value.is_empty() {
            self.w.line(&format!("(void){};", parenthesized(value)));
        }
    }

    /// An assignment to several places, or to a single `_`.
    fn gen_multi_assign(&mut self, targets: &[Target], values: &[Expr], span: Span) -> Result<()> {
        // Every place is found before the values run, since a value may change
        // what a later place refers to, and a place assigned here may be read
        // by a later value.
        let mut places = Vec::new();
        for target in targets {
            match target {
                Target::Discard(_) => places.push(None),
                Target::Place(place) => {
                    let lvalue = self.gen_expr(place)?;
                    let pointer = self.fresh_temp("place");
                    self.w.line(&format!("{}* {} = &{};", c_type(place.ty()?), pointer, lvalue));
                    places.push(Some(pointer));
                }
            }
        }
        let given = self.gen_value_list(targets.len(), values, span)?;
        for (place, value) in places.into_iter().zip(given) {
            match place {
                Some(pointer) => self.w.line(&format!("*{} = {};", pointer, value)),
                None => self.drop_value(&value),
            }
        }
        Ok(())
    }

    /// The one C expression a `<-` sends: the value, or a result struct holding
    /// every value.
    fn gen_sent_value(&mut self, values: &[Expr], span: Span) -> Result<String> {
        if let [only] = values {
            return self.gen_expr(only);
        }
        let Type::Results(types) = self.ret.clone() else {
            return Err(Error::internal_at(span, format!("{} values sent by a task of type {}", values.len(), self.ret)));
        };
        let operands: Vec<&Expr> = values.iter().collect();
        let given = self.gen_operands(&operands)?;
        let members: Vec<String> = given.iter().enumerate().map(|(i, value)| format!(".{} = {}", result_member(i), value)).collect();
        Ok(format!("(({}){{ {} }})", c_type(&Type::Results(types)), members.join(", ")))
    }

    /// The value a `match` or `is` tests, as a C expression that can be
    /// read several times: a variable is used directly, anything else is
    /// stored in a temporary first.
    fn gen_subject(&mut self, expr: &Expr) -> Result<String> {
        let value = self.gen_expr(expr)?;
        if matches!(expr.kind, ExprKind::Var(_)) {
            return Ok(value);
        }
        let subject = self.fresh_temp("subject");
        self.w.line(&format!("{} {} = {};", c_type(expr.ty()?), subject, value));
        Ok(subject)
    }

    /// The C condition under which `pattern` matches `subject`, of type `ty`.
    fn pattern_test(&self, ty: &Type, subject: &str, pattern: &Pattern) -> Result<String> {
        let test = match (&pattern.kind, ty) {
            (PatternKind::Any, _) => "1".to_string(),
            (PatternKind::Value(_), Type::Maybe(inner)) if is_pointer(inner) => format!("{} != NULL", subject),
            (PatternKind::None, Type::Maybe(inner)) if is_pointer(inner) => format!("{} == NULL", subject),
            (PatternKind::Value(_), Type::Maybe(_)) => format!("{}.has", subject),
            (PatternKind::None, Type::Maybe(_)) => format!("!{}.has", subject),
            (PatternKind::Value(_), Type::Or(..)) => format!("{}.ok", subject),
            (PatternKind::Fail(_), Type::Or(..)) => format!("!{}.ok", subject),
            (PatternKind::Variant { tag, .. }, Type::Named(kind, _)) => format!("{}.tag == {}", subject, tag_value(kind, tag)),
            _ => return Err(Error::internal_at(pattern.span, format!("pattern '{}' on type {}", pattern.label(), ty))),
        };
        Ok(test)
    }

    /// Declares the names `pattern` binds from `subject`, of type `ty`, for the
    /// block `body`.
    fn gen_pattern_bindings(&mut self, ty: &Type, subject: &str, pattern: &Pattern, body: &[Stmt]) -> Result<()> {
        let mut bindings: Vec<(&str, Type, String)> = Vec::new();
        match (&pattern.kind, ty) {
            (PatternKind::Value((name, _)), Type::Maybe(inner)) if is_pointer(inner) => {
                bindings.push((name, (**inner).clone(), subject.to_string()));
            }
            (PatternKind::Value((name, _)), Type::Maybe(inner) | Type::Or(inner, _)) => {
                bindings.push((name, (**inner).clone(), format!("{}.value", subject)));
            }
            (PatternKind::Fail((name, _)), Type::Or(_, error)) => {
                bindings.push((name, (**error).clone(), format!("{}.error", subject)));
            }
            (PatternKind::Variant { tag, binds }, Type::Named(kind_name, _)) => {
                let variant = self
                    .kinds
                    .get(kind_name)
                    .and_then(|k| k.variants.iter().find(|v| v.tag == *tag))
                    .ok_or_else(|| Error::internal_at(pattern.span, format!("unknown variant '{}'", tag)))?;
                if binds.len() != variant.payload.len() {
                    return Err(Error::internal_at(
                        pattern.span,
                        format!("case '{}' has {} bindings (expected {})", tag, binds.len(), variant.payload.len()),
                    ));
                }
                for ((name, _), field) in binds.iter().zip(&variant.payload) {
                    let value = format!("{}.data.{}.{}", subject, c_ident(tag), c_ident(&field.name));
                    bindings.push((name, field.ty.clone(), value));
                }
            }
            _ => {}
        }
        for (name, ty, value) in bindings {
            if name == DISCARD {
                continue;
            }
            self.w.line(&format!("{} {} = {};", c_type(&ty), c_ident(name), value));
            // A pattern must name every value, even ones its block never reads.
            self.use_if_unread(name, body);
        }
        Ok(())
    }

    /// Generates a `match`. On a kind, it is a `switch` on the tag, with `_` as
    /// `default`; on a `maybe` or `T or E`, an `if`/`else` chain whose last case
    /// needs no test. Each case body gets its own C block, so different cases
    /// may bind the same names. The matched value is evaluated exactly once.
    fn gen_match(&mut self, expr: &Expr, cases: &[Case]) -> Result<()> {
        let ty = expr.ty()?.clone();
        let needs_temp = !matches!(expr.kind, ExprKind::Var(_));
        let subject = if needs_temp {
            let value = self.gen_expr(expr)?;
            let tmp = self.fresh_temp("match");
            self.w.open("");
            self.w.line(&format!("{} {} = {};", c_type(&ty), tmp, value));
            tmp
        } else {
            self.gen_expr(expr)?
        };

        if let Type::Named(kind_name, _) = &ty {
            if !self.kinds.contains_key(kind_name) {
                return Err(Error::internal_at(expr.span, format!("match on unknown kind '{}'", kind_name)));
            }
            // Case labels line up with their `switch`, so the switch's own braces
            // do not indent.
            self.w.line(&format!("switch ({}.tag) {{", subject));
            for case in cases {
                match &case.pattern.kind {
                    PatternKind::Variant { tag, .. } => self.w.open(&format!("case {}:", tag_value(kind_name, tag))),
                    PatternKind::Any => self.w.open("default:"),
                    _ => {
                        return Err(Error::internal_at(case.pattern.span, format!("case '{}' on kind '{}'", case.pattern.label(), kind_name)));
                    }
                }
                self.gen_pattern_bindings(&ty, &subject, &case.pattern, &case.body)?;
                self.gen_block(&case.body)?;
                self.w.line("break;");
                self.w.close();
            }
            self.w.line("}");
        } else if matches!(ty, Type::Maybe(_) | Type::Or(..)) {
            for (i, case) in cases.iter().enumerate() {
                let last = i + 1 == cases.len();
                let test = if last { None } else { Some(self.pattern_test(&ty, &subject, &case.pattern)?) };
                match (i, test) {
                    (0, Some(test)) => self.w.open(&format!("if ({})", test)),
                    // A lone `_`, which never reads the subject.
                    (0, None) => {
                        self.w.open("");
                        self.w.line(&format!("(void){};", subject));
                    }
                    (_, Some(test)) => self.w.reopen(&format!("else if ({})", test)),
                    (_, None) => self.w.reopen("else"),
                }
                self.gen_pattern_bindings(&ty, &subject, &case.pattern, &case.body)?;
                self.gen_block(&case.body)?;
            }
            if !cases.is_empty() {
                self.w.close();
            }
        } else {
            return Err(Error::internal_at(expr.span, format!("match on type {}", ty)));
        }

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

    /// Generates operands that are evaluated in order, as a call's arguments or
    /// an operator's two sides. When one of them has a prelude, they are
    /// evaluated left to right: every operand before the last prelude is stored
    /// in a temporary first, so that it is evaluated before that prelude runs
    /// (the prelude may call tasks or change variables), and so is every other
    /// operand but the last, since C evaluates the rest in no fixed order.
    fn gen_operands(&mut self, operands: &[&Expr]) -> Result<Vec<String>> {
        let last_prelude = operands.iter().rposition(|e| has_plan_b(e));
        let mut values = Vec::new();
        for (i, operand) in operands.iter().enumerate() {
            let value = self.gen_expr(operand)?;
            let constant = matches!(
                operand.kind,
                ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) | ExprKind::NoneLit
            );
            // A variable after the last prelude reads the same value whenever C reads it.
            let variable = matches!(operand.kind, ExprKind::Var(_));
            let in_order = last_prelude.is_some_and(|last| i < last || (i + 1 < operands.len() && !variable));
            if in_order && !constant {
                let tmp = self.fresh_temp("arg");
                self.w.line(&format!("{} {} = {};", c_type(operand.ty()?), tmp, value));
                values.push(tmp);
            } else {
                values.push(value);
            }
        }
        Ok(values)
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
                        Type::Maybe(_) => show_maybe_helper(arg.ty()?),
                        Type::Int | Type::Float | Type::Bool | Type::Text => show_helper(arg.ty()?),
                        other => return Err(Error::internal_at(span, format!("show called with type {}", other))),
                    };
                    let value = self.gen_expr(arg)?;
                    return Ok(format!("{}({})", helper, value));
                }
                let args: Vec<&Expr> = args.iter().collect();
                let values = self.gen_operands(&args)?;
                Ok(format!("{}({})", mangle(name), values.join(", ")))
            }
            ExprKind::Unary(op, operand) => {
                let expr = self.gen_expr(operand)?;
                // `-x` on an int overflows only for the smallest int; a
                // negated literal can't be it.
                if *op == UnaryOp::Neg && *operand.ty()? == Type::Int && int_constant(operand).is_none() {
                    return Ok(format!("vela__rt__neg({}, {})", expr, self.location(span)));
                }
                let op_s = match op {
                    UnaryOp::Neg => "-",
                    UnaryOp::Not => "!",
                };
                Ok(format!("({}{})", op_s, expr))
            }
            ExprKind::Binary(op, l, r) => {
                let binary = self.gen_binary(e, *op, l, r)?;
                Ok(parenthesized(&binary))
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
                let operands: Vec<&Expr> = args.iter().collect();
                let values = self.gen_operands(&operands)?;
                let fields: Vec<String> = variant
                    .payload
                    .iter()
                    .zip(values)
                    .map(|(field, value)| format!(".{} = {}", c_ident(&field.name), value))
                    .collect();
                Ok(format!(
                    "(({}){{ .tag = {}, .data = {{ .{} = {{ {} }} }} }})",
                    mangle(kind), tag_value, c_ident(tag), fields.join(", ")
                ))
            }
            ExprKind::New { ty, value, region, .. } => {
                // Copies the value out of a one-element array compound literal,
                // which works for every type in standard C.
                let val = self.gen_expr(value)?;
                let ctype = c_type(ty);
                Ok(format!(
                    "(({0}*)vela__rt__new({1}, ({0}[1]){{ {2} }}, sizeof({0}), _Alignof({0})))",
                    ctype,
                    c_ident(region),
                    val
                ))
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
                let operands: Vec<&Expr> = fields.iter().map(|field| &field.value).collect();
                let values = self.gen_operands(&operands)?;
                let fields: Vec<String> = fields
                    .iter()
                    .zip(values)
                    .map(|(field, value)| format!(".{} = {}", c_ident(&field.name), value))
                    .collect();
                Ok(format!("(({}){{ {} }})", mangle(name), fields.join(", ")))
            }
            ExprKind::Range(_, _) => Err(Error::internal_at(span, "range outside 'each' reached code generation")),
            // The error value itself; `Wrap` makes it part of a `T or E`.
            ExprKind::Fail(error) => self.gen_expr(error),
            ExprKind::Wrap(inner) => self.gen_wrap(e, inner),
            ExprKind::PlanB { value, error, body } => self.gen_plan_b(e, value, error.as_ref(), body),
        }
    }

    /// A value becoming part of a `maybe` or `T or E`, the type of `e`.
    fn gen_wrap(&mut self, e: &Expr, inner: &Expr) -> Result<String> {
        let target = e.ty()?;
        let ctype = c_type(target);
        match target {
            Type::Maybe(pointer) if is_pointer(pointer) => match inner.ty()? {
                Type::None => Ok("NULL".to_string()),
                _ => self.gen_expr(inner),
            },
            Type::Maybe(_) => match inner.ty()? {
                Type::None => Ok(format!("(({}){{ .has = false }})", ctype)),
                _ => Ok(format!("(({}){{ .has = true, .value = {} }})", ctype, self.gen_expr(inner)?)),
            },
            Type::Or(value, _) => {
                if let ExprKind::Fail(error) = &inner.kind {
                    return Ok(format!("(({}){{ .ok = false, .error = {} }})", ctype, self.gen_expr(error)?));
                }
                if **value == Type::Unit {
                    return Err(Error::internal_at(e.span, "a value wrapped as the success of 'unit or E'"));
                }
                Ok(format!("(({}){{ .ok = true, .value = {} }})", ctype, self.gen_expr(inner)?))
            }
            other => Err(Error::internal_at(e.span, format!("a value wrapped as {}", other))),
        }
    }

    /// A plan B, `value { body }`, the plan B `e`. Its prelude stores `value`,
    /// then runs `body` if there is nothing to extract; its C expression is the
    /// temporary holding the result (empty when it gives no value).
    fn gen_plan_b(&mut self, e: &Expr, value: &Expr, error: Option<&Binding>, body: &[Stmt]) -> Result<String> {
        let value_ty = value.ty()?.clone();
        let result_ty = e.ty()?.clone();
        let value_c = self.gen_expr(value)?;
        let subject = self.fresh_temp("subject");
        self.w.line(&format!("{} {} = {};", c_type(&value_ty), subject, value_c));
        let (present, extracted) = match &value_ty {
            Type::Maybe(inner) if is_pointer(inner) => (format!("{} != NULL", subject), subject.clone()),
            Type::Maybe(_) => (format!("{}.has", subject), format!("{}.value", subject)),
            Type::Or(..) => (format!("{}.ok", subject), format!("{}.value", subject)),
            other => return Err(Error::internal_at(e.span, format!("plan B on type {}", other))),
        };
        let result = if result_ty == Type::Unit {
            String::new()
        } else {
            let result = self.fresh_temp("value");
            self.w.line(&format!("{} {} = {};", c_type(&result_ty), result, extracted));
            result
        };
        self.w.open(&format!("if (!({}))", present));
        if let (Some((name, _)), Type::Or(_, error_ty)) = (error, &value_ty)
            && name != DISCARD
        {
            self.w.line(&format!("{} {} = {}.error;", c_type(error_ty), c_ident(name), subject));
            self.use_if_unread(name, body);
        }
        match body.split_last() {
            Some((last, rest)) if !result.is_empty() && matches!(last.kind, StmtKind::Expr(_)) => {
                // The value is stored before the block's regions are freed.
                self.enter_block();
                self.gen_statements(rest, body)?;
                if let StmtKind::Expr(fallback) = &last.kind {
                    let fallback = self.gen_expr(fallback)?;
                    self.w.line(&format!("{} = {};", result, fallback));
                }
                self.leave_block(body);
            }
            _ => self.gen_block(body)?,
        }
        self.w.close();
        Ok(result)
    }

    /// A binary operation without surrounding parentheses.
    fn gen_binary(&mut self, e: &Expr, op: BinaryOp, l: &Expr, r: &Expr) -> Result<String> {
        if matches!(op, BinaryOp::And | BinaryOp::Or) && has_plan_b(r) {
            // The right side's prelude must only run when the left side
            // doesn't decide the result.
            let left = self.gen_expr(l)?;
            let result = self.fresh_temp("cond");
            self.w.line(&format!("bool {} = {};", result, left));
            let test = if op == BinaryOp::And { result.clone() } else { format!("!{}", result) };
            self.w.open(&format!("if ({})", test));
            let right = self.gen_expr(r)?;
            self.w.line(&format!("{} = {};", result, right));
            self.w.close();
            return Ok(result);
        }
        let [left, right] = <[String; 2]>::try_from(self.gen_operands(&[l, r])?)
            .map_err(|_| Error::internal_at(e.span, "a binary operation without two operands"))?;
        if matches!(op, BinaryOp::Eq | BinaryOp::Ne) {
            // `m == none` tests a maybe.
            let maybe = match (l.ty()?, r.ty()?) {
                (Type::Maybe(inner), Type::None) => Some((inner, &left)),
                (Type::None, Type::Maybe(inner)) => Some((inner, &right)),
                _ => None,
            };
            if let Some((inner, value)) = maybe {
                let empty = op == BinaryOp::Eq;
                return Ok(match (is_pointer(inner), empty) {
                    (true, true) => format!("{} == NULL", value),
                    (true, false) => format!("{} != NULL", value),
                    (false, true) => format!("!{}.has", value),
                    (false, false) => format!("{}.has", value),
                });
            }
        }
        if matches!(op, BinaryOp::Eq | BinaryOp::Ne) && *l.ty()? == Type::Text {
            // Text compares by contents, not by address.
            let not = if op == BinaryOp::Ne { "!" } else { "" };
            return Ok(format!("{}vela__rt__text_eq({}, {})", not, left, right));
        }
        if op == BinaryOp::Mod && *e.ty()? == Type::Float {
            // C's `%` is integer-only.
            return Ok(format!("vela__rt__fmod({}, {})", left, right));
        }
        // Two integer literals would be computed in C's `int`, which is often 32
        // bits: `1000000 * 1000000` must be computed in 64 bits, as Vela's `int`.
        // The checker has made sure the result fits.
        let (left, right) = match (int_constant(l), int_constant(r)) {
            (Some(left), Some(right)) if *e.ty()? == Type::Int => (left, right),
            _ => {
                if let Some(helper) = checked_int_helper(op, e, r)? {
                    return Ok(format!("vela__rt__{}({}, {}, {})", helper, left, right, self.location(e.span)));
                }
                (left, right)
            }
        };
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

    /// Where an operation is, for a runtime message: a C string literal
    /// `"LINE:COL in task 'NAME'"`.
    fn location(&self, span: Span) -> String {
        format!("\"{}:{} in task '{}'\"", span.line, span.col, escape_c_string(&self.task))
    }

    fn gen_range(&mut self, e: &Expr) -> Result<(String, String)> {
        match &e.kind {
            ExprKind::Range(a, b) => {
                let [av, bv] = <[String; 2]>::try_from(self.gen_operands(&[a, b])?)
                    .map_err(|_| Error::internal_at(e.span, "a range without two ends"))?;
                Ok((av, bv))
            }
            _ => Err(Error::internal_at(e.span, "each over a non-range expression")),
        }
    }
}

/// `text` in parentheses, unless it already is one parenthesized expression
/// or a plain name.
fn parenthesized(text: &str) -> String {
    let simple = text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if simple { text.to_string() } else { format!("({})", text) }
}

fn is_pointer(ty: &Type) -> bool {
    matches!(ty, Type::Ptr(..))
}

/// The member of a result struct holding result `i`.
fn result_member(i: usize) -> String {
    format!("r{}", i)
}

/// The success of a `unit or E` task, as a C compound literal.
fn success(ret: &Type) -> String {
    format!("(({}){{ .ok = true }})", c_type(ret))
}

/// The runtime task that shows a value of builtin type `ty`.
fn show_helper(ty: &Type) -> String {
    let name = match ty {
        Type::Int => "int",
        Type::Float => "float",
        Type::Bool => "bool",
        _ => "text",
    };
    format!("vela__rt__show_{}", name)
}

/// The generated task that shows a `maybe` of type `ty`.
fn show_maybe_helper(ty: &Type) -> String {
    format!("vela__show__{}", c_type(ty).trim_start_matches("vela__"))
}

/// Whether the expression contains a plan B, so generating it writes a prelude.
fn has_plan_b(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::PlanB { .. } => true,
        ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) | ExprKind::NoneLit
        | ExprKind::Var(_) => false,
        ExprKind::Field(inner, _) | ExprKind::Unary(_, inner) | ExprKind::Deref(inner) | ExprKind::New { value: inner, .. }
        | ExprKind::Fail(inner) | ExprKind::Wrap(inner) => has_plan_b(inner),
        ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Range(a, b) => has_plan_b(a) || has_plan_b(b),
        ExprKind::Call(_, args) | ExprKind::Construct { args, .. } => args.iter().any(has_plan_b),
        ExprKind::StructInit(_, fields) => fields.iter().any(|field| has_plan_b(&field.value)),
    }
}

/// The `maybe` and `or` types a program uses, which each need a C struct, and
/// the `maybe` types it shows, which each need a helper.
struct UsedTypes {
    compounds: Vec<Type>,
    shown: Vec<Type>,
}

impl UsedTypes {
    fn collect(items: &[Item]) -> UsedTypes {
        let mut used = UsedTypes { compounds: Vec::new(), shown: Vec::new() };
        for item in items {
            match item {
                Item::Shape(s) => s.fields.iter().for_each(|f| used.add(&f.ty)),
                Item::Kind(k) => k.variants.iter().flat_map(|v| &v.payload).for_each(|f| used.add(&f.ty)),
                Item::Task(t) => {
                    t.params.iter().for_each(|p| used.add(&p.ty));
                    used.add(&t.ret);
                    used.block(&t.body);
                }
            }
        }
        used
    }

    /// Records `ty` and every type inside it. Regions don't change a type's C
    /// struct, so they are left out.
    fn add(&mut self, ty: &Type) {
        let ty = &ty.without_regions();
        match ty {
            Type::Ptr(inner, _) | Type::Range(inner) | Type::Fail(inner) => self.add(inner),
            Type::Maybe(inner) => {
                self.add(inner);
                if !is_pointer(inner) && !self.compounds.contains(ty) {
                    self.compounds.push(ty.clone());
                }
            }
            Type::Or(value, error) => {
                self.add(value);
                self.add(error);
                if !self.compounds.contains(ty) {
                    self.compounds.push(ty.clone());
                }
            }
            Type::Results(types) => {
                types.iter().for_each(|result| self.add(result));
                if !self.compounds.contains(ty) {
                    self.compounds.push(ty.clone());
                }
            }
            _ => {}
        }
    }

    fn block(&mut self, block: &[Stmt]) {
        for stmt in block {
            match &stmt.kind {
                StmtKind::Expr(e) => self.expr(e),
                StmtKind::Send(values) | StmtKind::SetResults(_, values) => values.iter().for_each(|value| self.expr(value)),
                StmtKind::Region(_) => {}
                StmtKind::Binding { names, init, .. } => {
                    for bound in names {
                        if let Some(ty) = &bound.ty {
                            self.add(ty);
                        }
                    }
                    init.iter().for_each(|value| self.expr(value));
                }
                StmtKind::Assign(targets, values) => {
                    for target in targets {
                        if let Target::Place(place) = target {
                            self.expr(place);
                        }
                    }
                    values.iter().for_each(|value| self.expr(value));
                }
                StmtKind::When(cond, then_b, else_b) => {
                    self.expr(&cond.value);
                    self.block(then_b);
                    if let Some(else_b) = else_b {
                        self.block(else_b);
                    }
                }
                StmtKind::Loop(cond, body) => {
                    self.expr(&cond.value);
                    self.block(body);
                }
                StmtKind::Each(_, range, body) => {
                    self.expr(range);
                    self.block(body);
                }
                StmtKind::Match(subject, cases) => {
                    self.expr(subject);
                    cases.iter().for_each(|case| self.block(&case.body));
                }
            }
        }
    }

    fn expr(&mut self, e: &Expr) {
        if let Some(ty) = &e.ty {
            self.add(ty);
        }
        match &e.kind {
            ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) | ExprKind::NoneLit
            | ExprKind::Var(_) => {}
            ExprKind::Field(inner, _) | ExprKind::Unary(_, inner) | ExprKind::Deref(inner) | ExprKind::New { value: inner, .. }
            | ExprKind::Fail(inner) | ExprKind::Wrap(inner) => self.expr(inner),
            ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Range(a, b) => {
                self.expr(a);
                self.expr(b);
            }
            ExprKind::Call(name, args) => {
                if name == "show"
                    && let [arg] = args.as_slice()
                    && let Some(ty @ Type::Maybe(_)) = &arg.ty
                    && !self.shown.contains(ty)
                {
                    // `show` takes no pointers, so a shown maybe has no regions.
                    self.shown.push(ty.clone());
                }
                args.iter().for_each(|arg| self.expr(arg));
            }
            ExprKind::Construct { args, .. } => args.iter().for_each(|arg| self.expr(arg)),
            ExprKind::StructInit(_, fields) => fields.iter().for_each(|field| self.expr(&field.value)),
            ExprKind::PlanB { value, body, .. } => {
                self.expr(value);
                self.block(body);
            }
        }
    }
}

/// Whether any statement in `block` reads the variable `name`, as C's
/// unused-variable warnings see it: assigning to the variable or to one of its
/// fields is not a read. (Vela has no shadowing, so every `name` in the
/// variable's scope refers to it.)
fn block_reads(block: &[Stmt], name: &str) -> bool {
    block.iter().any(|stmt| stmt_reads(stmt, name))
}

fn stmt_reads(stmt: &Stmt, name: &str) -> bool {
    let expr = |e: &Expr| expr_reads(e, name);
    let body = |b: &Block| block_reads(b, name);
    match &stmt.kind {
        StmtKind::Expr(e) => expr(e),
        StmtKind::Send(values) | StmtKind::SetResults(_, values) => values.iter().any(expr),
        StmtKind::Region(_) => false,
        StmtKind::Binding { init, .. } => init.iter().any(expr),
        StmtKind::Assign(targets, values) => {
            targets.iter().any(|target| match target {
                Target::Discard(_) => false,
                Target::Place(place) => target_reads(place, name),
            }) || values.iter().any(expr)
        }
        StmtKind::When(cond, then_b, else_b) => expr(&cond.value) || body(then_b) || else_b.as_ref().is_some_and(body),
        StmtKind::Loop(cond, b) => expr(&cond.value) || body(b),
        StmtKind::Each(_, range, b) => expr(range) || body(b),
        StmtKind::Match(subject, cases) => expr(subject) || cases.iter().any(|case| body(&case.body)),
    }
}

/// Whether assigning to `target` reads `name`. Writing to a variable or its
/// fields doesn't read it; writing through a pointer (`p^ = …`) reads `p`.
fn target_reads(target: &Expr, name: &str) -> bool {
    match &target.kind {
        ExprKind::Var(_) => false,
        ExprKind::Field(base, _) => target_reads(base, name),
        _ => expr_reads(target, name),
    }
}

fn expr_reads(e: &Expr, name: &str) -> bool {
    match &e.kind {
        ExprKind::Var(var) => var == name,
        ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) | ExprKind::NoneLit => false,
        // `new` reads the region it allocates in.
        ExprKind::New { value, region, .. } => region == name || expr_reads(value, name),
        ExprKind::Field(inner, _) | ExprKind::Unary(_, inner) | ExprKind::Deref(inner)
        | ExprKind::Fail(inner) | ExprKind::Wrap(inner) => expr_reads(inner, name),
        ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Range(a, b) => {
            expr_reads(a, name) || expr_reads(b, name)
        }
        ExprKind::Call(_, args) | ExprKind::Construct { args, .. } => args.iter().any(|arg| expr_reads(arg, name)),
        ExprKind::StructInit(_, fields) => fields.iter().any(|field| expr_reads(&field.value, name)),
        ExprKind::PlanB { value, body, .. } => expr_reads(value, name) || block_reads(body, name),
    }
}

fn type_name(item: &Item) -> Option<&str> {
    match item {
        Item::Shape(s) => Some(&s.name),
        Item::Kind(k) => Some(&k.name),
        Item::Task(_) => None,
    }
}

/// A type with a C struct definition: a shape or kind, or a `maybe` or `or`.
enum TypeDef<'a> {
    Item(&'a Item),
    Compound(&'a Type),
}

/// The C names of the struct definitions a value of type `ty` contains by
/// value, which must come before any struct embedding it.
fn value_dependencies(ty: &Type) -> Vec<String> {
    match ty {
        Type::Named(name, _) => vec![mangle(name)],
        Type::Maybe(inner) if is_pointer(inner) => Vec::new(),
        Type::Maybe(_) | Type::Or(..) | Type::Results(_) => vec![c_type(ty)],
        _ => Vec::new(),
    }
}

/// Shapes, kinds, and compound types in source order, except that each comes
/// after every type it contains by value, since C needs a struct's complete
/// definition before it can be embedded. (The checker has rejected types that
/// contain themselves by value.)
fn type_order<'a>(items: &'a [Item], compounds: &'a [Type]) -> Result<Vec<TypeDef<'a>>> {
    let mut by_name: HashMap<String, TypeDef<'a>> = HashMap::new();
    let mut names = Vec::new();
    for item in items {
        if let Some(name) = type_name(item) {
            names.push(mangle(name));
            by_name.insert(mangle(name), TypeDef::Item(item));
        }
    }
    for ty in compounds {
        names.push(c_type(ty));
        by_name.insert(c_type(ty), TypeDef::Compound(ty));
    }
    let mut finished = HashMap::new();
    let mut order = Vec::new();
    for name in &names {
        visit_type(name, &mut by_name, &mut finished, &mut order)?;
    }
    Ok(order)
}

/// Depth-first step of `type_order`, from `root`. `finished` maps a type to
/// `false` while its dependencies are being emitted and `true` once it is in
/// `order`. The search keeps its own stack, so a long chain of types can't
/// overflow the compiler's.
fn visit_type<'a>(
    root: &str,
    by_name: &mut HashMap<String, TypeDef<'a>>,
    finished: &mut HashMap<String, bool>,
    order: &mut Vec<TypeDef<'a>>,
) -> Result<()> {
    // Each type being entered, with its dependencies, reversed so the next is last.
    let mut stack: Vec<(String, TypeDef<'a>, Vec<String>)> = Vec::new();
    let mut next = Some(root.to_string());
    loop {
        if let Some(name) = next.take() {
            match finished.get(&name) {
                Some(true) => {}
                Some(false) => return Err(Error::internal(format!("type '{}' contains itself by value", name))),
                // A builtin type has no definition.
                None => {
                    if let Some(def) = by_name.remove(&name) {
                        finished.insert(name.clone(), false);
                        let mut dependencies = dependencies_of(&def);
                        dependencies.reverse();
                        stack.push((name, def, dependencies));
                    }
                }
            }
        }
        let Some((_, _, dependencies)) = stack.last_mut() else { return Ok(()) };
        match dependencies.pop() {
            Some(dependency) => next = Some(dependency),
            None => {
                if let Some((name, def, _)) = stack.pop() {
                    finished.insert(name, true);
                    order.push(def);
                }
            }
        }
    }
}

/// The types a definition contains by value.
fn dependencies_of(def: &TypeDef) -> Vec<String> {
    match def {
        TypeDef::Item(Item::Shape(s)) => s.fields.iter().flat_map(|f| value_dependencies(&f.ty)).collect(),
        TypeDef::Item(Item::Kind(k)) => k.variants.iter().flat_map(|v| &v.payload).flat_map(|f| value_dependencies(&f.ty)).collect(),
        TypeDef::Item(Item::Task(_)) => Vec::new(),
        TypeDef::Compound(Type::Maybe(inner)) => value_dependencies(inner),
        TypeDef::Compound(Type::Or(value, error)) => value_dependencies(value).into_iter().chain(value_dependencies(error)).collect(),
        TypeDef::Compound(Type::Results(types)) => types.iter().flat_map(value_dependencies).collect(),
        TypeDef::Compound(_) => Vec::new(),
    }
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
//   vela__maybe__int              a `maybe` struct              (`c_type`)
//   vela__or__int__n_ParseError   an `or` struct                (`c_type`)
//   vela__show__maybe__int        `show` for a `maybe`          (`show_maybe_helper`)
//   vela__end__0, vela__empty__   compiler temporaries and members
//
// A region is a local or parameter like any other (`c_ident`); regions don't
// change C types, so `^int@a` and `^int@b` are both `int64_t*`.

/// The C name of a task, shape, or kind.
fn mangle(name: &str) -> String {
    format!("vela_{}", c_name(name))
}

fn tag_type(kind: &str) -> String {
    format!("vela_{}__Tag", c_name(kind))
}

fn tag_value(kind: &str, tag: &str) -> String {
    format!("vela_{}__tag_{}", c_name(kind), tag)
}

/// A task, shape, or kind name as part of a C name. A name from a module other
/// than the main file's has the module before a `.`, which becomes `__0__`:
/// no Vela name contains `__`, and none starts with a digit, so the result
/// can't be any other name, or read as a word the generated names use.
fn c_name(name: &str) -> String {
    name.replace(MODULE_SEPARATOR, "__0__")
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
        Type::Ptr(t, _) => format!("{}*", c_type(t)),
        Type::Named(n, _) => mangle(n),
        Type::Region => "vela__rt__region*".to_string(),
        Type::Range(t) => format!("/*range<{}>*/", c_type(t)),
        // A `maybe` pointer is the pointer itself, with `NULL` for none.
        Type::Maybe(inner) if is_pointer(inner) => c_type(inner),
        Type::Maybe(inner) => format!("vela__maybe__{}", type_key(inner)),
        Type::Or(value, error) => format!("vela__or__{}__{}", type_key(value), type_key(error)),
        Type::Fail(error) => c_type(error),
        Type::Results(_) => format!("vela__{}", type_key(ty)),
    }
}

/// A type as part of a C name, distinct for distinct types: words separated by
/// `__`, which no Vela name contains, with a shape or kind name marked `n_` so
/// it can't read as a builtin or as `ptr`, `maybe`, or `or`.
fn type_key(ty: &Type) -> String {
    match ty {
        Type::Int => "int".to_string(),
        Type::Float => "float".to_string(),
        Type::Bool => "bool".to_string(),
        Type::Text => "text".to_string(),
        Type::Unit => "unit".to_string(),
        Type::None => "none".to_string(),
        Type::Named(name, _) => format!("n_{}", c_name(name)),
        Type::Region => "region".to_string(),
        Type::Ptr(inner, _) => format!("ptr__{}", type_key(inner)),
        Type::Range(inner) => format!("range__{}", type_key(inner)),
        Type::Maybe(inner) => format!("maybe__{}", type_key(inner)),
        Type::Or(value, error) => format!("or__{}__{}", type_key(value), type_key(error)),
        Type::Fail(error) => format!("fail__{}", type_key(error)),
        // The count keeps two lists of results apart when the types of one
        // read as the start of the other.
        Type::Results(types) => {
            let keys: Vec<String> = types.iter().map(type_key).collect();
            format!("results__{}__{}", types.len(), keys.join("__"))
        }
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

/// The runtime helper that computes the int operation `e` (`op` on the
/// operands, `r` the right one), stopping the program when its result doesn't
/// fit or it divides by zero; `None` when C's operator can't go wrong: `e`
/// isn't int arithmetic, or it divides by a literal other than 0 and -1.
fn checked_int_helper(op: BinaryOp, e: &Expr, r: &Expr) -> Result<Option<&'static str>> {
    if *e.ty()? != Type::Int {
        return Ok(None);
    }
    let safe_divisor = int_value(r).is_some_and(|divisor| divisor != 0 && divisor != -1);
    Ok(match op {
        BinaryOp::Add => Some("add"),
        BinaryOp::Sub => Some("sub"),
        BinaryOp::Mul => Some("mul"),
        BinaryOp::Div if !safe_divisor => Some("div"),
        BinaryOp::Mod if !safe_divisor => Some("mod"),
        _ => None,
    })
}

/// The value of an integer literal, possibly negated.
fn int_value(e: &Expr) -> Option<i64> {
    match &e.kind {
        ExprKind::IntLit(n) => Some(*n),
        ExprKind::Unary(UnaryOp::Neg, operand) => int_value(operand).and_then(i64::checked_neg),
        _ => None,
    }
}

/// An integer literal, possibly negated, as a 64-bit C constant: `INT64_C(5)`,
/// `(-INT64_C(5))`. `None` for any other expression.
fn int_constant(e: &Expr) -> Option<String> {
    match &e.kind {
        ExprKind::IntLit(n) => Some(format!("INT64_C({})", n)),
        ExprKind::Unary(UnaryOp::Neg, operand) => int_constant(operand).map(|inner| format!("(-{})", inner)),
        _ => None,
    }
}
