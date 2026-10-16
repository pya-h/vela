use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::check;
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

/// The most arms a `when` of plain conditions is written as an `else if`
/// chain in C, which a C compiler nests (see `Generator::gen_when`).
const ELSE_IF_ARMS: usize = 64;

/// Generates C from a program that has already been checked by
/// `check::Checker`. Types come from the checker's annotations; the generator
/// does no type inference of its own. A missing annotation or other broken
/// checker guarantee is reported as an internal compiler error.
///
/// An expression is normally one C expression. One that contains a plan B
/// (`x { … }`), a change to a slice (`xs.del()`), or a `scan` needs statements, so
/// generating it writes those statements ("the prelude") before the line that
/// uses its value; see `gen_operands`.
///
/// A `region r` is an arena, `vela__rt__region r[1]`: as an array, `r` is a
/// pointer to it, like a region parameter. Every block frees the regions it
/// declared when it closes, and a `<-` frees every region the task declared
/// before it returns.
pub struct Generator {
    /// Whether the program writes no notices to standard error, as of a
    /// startup decorator whose failure it passes over (`--no-notices`).
    quiet: bool,
    w: CWriter,
    temp_counter: u64,
    /// An expression written as other C than its own while a value is made:
    /// the item a step reads, `m[k]` in `m[k] += 1`, read through the
    /// address it was found at (see the `Assign` arm of `gen_stmt`).
    replaced: Option<(*const Expr, String)>,
    /// The C of the value each pipe being written gives its call, the
    /// innermost last, which its `Piped` is written as (see `gen_pipe`).
    piped: Vec<String>,
    kinds: HashMap<String, KindDef>,
    shapes: HashMap<String, ShapeDef>,
    /// The shapes and kinds holding text or pointers, whose values a copy
    /// goes through (see `check::regional_types` and `check::textual_types`).
    copied_through: HashSet<String>,
    /// The return type of the task being generated.
    ret: Type,
    /// The regions declared in each block being generated, outermost first.
    regions: Vec<Vec<String>>,
    /// The names of the task's results, in order, when it names them.
    results: Vec<String>,
    /// The name of the task being generated, for runtime messages.
    task: String,
    /// The methods that change the value they are called on, which take it by
    /// pointer (see `signature`), by the name of the task each is.
    altering: HashSet<String>,
    /// The names in the task being generated that are pointers to a place,
    /// read through them: the value a method that alters it is called on, every
    /// `flex&` parameter, and the references of the blocks being generated.
    pointers: HashSet<String>,
    /// The parameters of the task being generated that are passed by a hidden
    /// pointer (see `HIDDEN_ABOVE`), which are in `pointers` too.
    hidden: HashSet<String>,
    /// For each task, by the name it is generated under, which of its
    /// parameters are passed by a hidden pointer, in order.
    hidden_params: HashMap<String, Vec<bool>>,
    /// The C names of the regions the `show` calls being generated make for the
    /// values written by their own `str`, innermost last (see `gen_show`). A
    /// `show` nests inside another only through a plan B of one of its values.
    shown: Vec<String>,
    /// The tasks named as values whose C takes its values otherwise than
    /// their type does, each given a function that does (see `gen_adapter`).
    adapted: HashSet<String>,
    /// Where the code being generated is, for a runtime message, when it is
    /// no task's own: a value worked out where the program starts, as
    /// `decorator 'route' on task 'home'` (see `location`).
    place: Option<String>,
    /// The loops around the code being generated, innermost last.
    loops: Vec<LoopLabels>,
    /// The name `outer =>` gave the loop about to be generated.
    loop_name: Option<String>,
    /// What the loop about to be generated gives, when it is a value.
    loop_giving: Option<Giving>,
}

#[derive(Clone)]
struct MapAcc {
    acc_name: String,
    coll_ty: Type,
    item_ty: Type,
    region: Option<String>,
    idx_name: String,
}

/// A loop that gives a value (see `ValueLoop`): the temporary holding it, of
/// type `ty`, `V ~ LoopResult<B, M>`, and whether its passes give it, each on
/// its last line, rather than nothing.
#[derive(Clone)]
struct Giving {
    result: String,
    ty: Type,
    gives: bool,
    map_acc: Option<MapAcc>,
}

/// A loop being generated, which `break` and `continue` leave with a `goto`:
/// its name, if it has one, how many blocks of regions were open around it,
/// and its labels, the one after it and the one at the end of a pass, each
/// written only where something goes to it, since C warns about a label
/// nothing goes to.
#[derive(Clone)]
struct LoopLabels {
    name: Option<String>,
    giving: Option<Giving>,
    depth: usize,
    stop: String,
    next: String,
    ran: Option<String>,
    pass_ok: Option<String>,
    stopped: bool,
    continued: bool,
}

impl Default for Generator {
    fn default() -> Self {
        Generator {
            quiet: false,
            w: CWriter::default(),
            temp_counter: 0,
            kinds: HashMap::new(),
            shapes: HashMap::new(),
            copied_through: HashSet::new(),
            ret: Type::Nothing,
            regions: Vec::new(),
            results: Vec::new(),
            task: String::new(),
            altering: HashSet::new(),
            pointers: HashSet::new(),
            replaced: None,
            piped: Vec::new(),
            hidden: HashSet::new(),
            hidden_params: HashMap::new(),
            shown: Vec::new(),
            adapted: HashSet::new(),
            place: None,
            loops: Vec::new(),
            loop_name: None,
            loop_giving: None,
        }
    }
}

impl Generator {
    pub fn new() -> Self {
        Self::default()
    }

    /// The generator for a program that writes notices to standard error, or
    /// none (see `quiet`).
    pub fn with_notices(mut self, notices: bool) -> Self {
        self.quiet = !notices;
        self
    }

    pub fn generate(&mut self, items: &[Item]) -> Result<String> {
        self.kinds.clear();
        self.shapes.clear();
        self.altering.clear();
        for item in items {
            if let Item::Task(t) = item
                && t.owner.as_ref().is_some_and(|owner| owner.alters)
            {
                // One task is generated for each set of types a method of a
                // shape that takes them is used with, and each is an item of
                // its own, so every one of them is listed here.
                self.altering.insert(t.name.clone());
            }
            match item {
                // A shape or kind that takes types generates nothing: the one
                // made for each set of types it is given is generated in its
                // place (see the checker's `shape_instance` and `kind_instance`).
                Item::Kind(k) if k.types.is_empty() => {
                    self.kinds.insert(k.name.clone(), k.clone());
                }
                Item::Kind(_) => {}
                Item::Shape(s) if s.types.is_empty() => {
                    self.shapes.insert(s.name.clone(), s.clone());
                }
                Item::Shape(_) => {}
                Item::Task(_) | Item::Macro(_) => {}
            }
        }
        // Which parameters each task takes by a hidden pointer, known before
        // any call or declaration is written.
        self.hidden_params.clear();
        for item in items {
            if let Item::Task(t) = item {
                let alters = self.altering.contains(&t.name);
                let hidden = t.params.iter().map(|p| self.is_hidden(t, p, alters)).collect();
                self.hidden_params.insert(t.name.clone(), hidden);
            }
        }
        self.copied_through = check::regional_types(items);
        self.copied_through.extend(check::textual_types(items));
        let used = UsedTypes::collect(items);

        self.w = CWriter::default();
        self.w.line("#include \"vela_rt.h\"");
        self.w.blank();

        // Macros, each the value the checker computed for it.
        let mut defined = false;
        for item in items {
            if let Item::Macro(m) = item {
                let value = self.macro_value(&m.value)?;
                self.w.line(&format!("#define {} {}", macro_name(&m.name), value));
                defined = true;
            }
        }
        if defined {
            self.w.blank();
        }

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
            self.w.line(&format!("typedef struct {0} {0};", compound_name(ty)));
            if matches!(ty, Type::Map(..)) {
                self.w.line(&format!("typedef struct {0} {0};", map_entry(ty)));
            }
        }
        // A task type is a pointer to a function, whose types need only be
        // declared, as the lines above do.
        for ty in &used.tasks {
            self.gen_task_type(ty);
        }
        for item in items {
            if let Item::Kind(k) = item
                && k.types.is_empty()
            {
                self.gen_tag_enum(k);
            }
        }
        for def in type_order(items, &used.compounds.list)? {
            match def {
                TypeDef::Item(Item::Shape(s)) => self.gen_shape(s),
                TypeDef::Item(Item::Kind(k)) => self.gen_kind(k),
                TypeDef::Item(Item::Task(_) | Item::Macro(_)) => {}
                TypeDef::Compound(ty) => self.gen_compound(ty),
            }
        }
        for ty in &used.read {
            self.gen_read_number(ty);
        }
        for ty in &used.scanned {
            self.gen_scan_number(ty);
        }
        self.gen_item_helpers(&used);
        self.gen_map_helpers(&used);
        self.gen_compares(&used)?;
        self.gen_widens(&used)?;
        // A slice made of another whose items hold anything in a region is
        // copied as `copy()` copies (see `gen_new`).
        let mut copied = used.copied.list.clone();
        for slice in used.slice_copied.list.iter().filter(|slice| self.copies_deep(slice)) {
            if !copied.contains(slice) {
                copied.push(slice.clone());
            }
        }
        self.gen_copies(&copied);
        self.gen_closure_helpers(&used);
        self.w.blank();

        // Shared fields: one variable each for the whole program, given its
        // first value where the program starts.
        let shared: Vec<(String, &SharedField)> = items
            .iter()
            .filter_map(|item| match item {
                Item::Shape(s) if s.types.is_empty() => Some(s),
                _ => None,
            })
            .flat_map(|s| s.shared.iter().map(move |member| (shared_name(&s.name, &member.field.name), member)))
            .collect();
        for (name, member) in &shared {
            self.w.line(&format!("static {} {};", c_type(&member.field.ty), name));
        }
        if !shared.is_empty() {
            self.w.blank();
        }

        // Forward declarations for tasks. A generic task generates nothing: one
        // task per set of types a call gives it is generated in its place (see
        // the checker's `expand_instances`).
        for item in items {
            if let Item::Task(t) = item
                && t.types.is_empty()
            {
                self.w.line(&format!("{};", signature(t, self.altering.contains(&t.name), self.hidden_of(&t.name), &|name| self.shapes.contains_key(name))));
            }
        }
        self.w.blank();
        self.adapted.clear();
        for name in &used.task_values {
            self.gen_adapter(items, name);
        }
        for (name, ty) in &used.kept_tasks {
            self.w.line(&format!("static {} {} = {};", ty, kept_task_name(name), self.task_value(name)));
        }
        self.gen_writers(&used)?;

        // Task definitions
        let mut has_main = false;
        for item in items {
            if let Item::Task(t) = item
                && t.types.is_empty()
            {
                if t.name == "main" {
                    has_main = true;
                }
                self.gen_task(t)?;
            }
        }

        if has_main {
            self.w.blank();
            // Output is C's usual: line by line to a terminal, in blocks to a
            // pipe or a file. `scan` flushes before it reads, and the runtime
            // flushes before it stops a program.
            self.w.open("int main(void)");
            for (name, member) in &shared {
                let Some(value) = &member.field.default else {
                    return Err(Error::internal_at(member.field.span, "a shared field without its first value"));
                };
                // A wrapper's value worked out here names where it is written.
                self.place = member.start.clone();
                let value = self.gen_expr(value)?;
                self.place = None;
                self.w.line(&format!("{} = {};", name, value));
            }
            // The startup decorators, after the shared fields they may fill.
            for item in items {
                if let Item::Task(t) = item
                    && is_startup(&t.name)
                {
                    self.w.line(&format!("{}();", mangle(&t.name)));
                }
            }
            self.w.line("vela_main();");
            self.w.line("return 0;");
            self.w.close();
        }

        Ok(std::mem::take(&mut self.w.out))
    }

    /// Which parameters a task of type `task` takes by a hidden pointer: each
    /// that is larger than `HIDDEN_ABOVE`, as a task's plain parameter is. A
    /// value the task changes through, `flex`, is one C type with a value, since
    /// a task taking it as a value goes where it is expected.
    fn hidden_of_type(&self, task: &TaskType) -> Vec<bool> {
        task.params.iter().zip(&task.taken).map(|(param, how)| matches!(how, Taken::Value | Taken::Flex | Taken::Bind) && self.c_layout(param).0 > HIDDEN_ABOVE).collect()
    }

    /// The C type of task type `ty`: a pointer to a function taking its values
    /// as a task takes them, `typedef int64_t (*vela__task__1__int__int)(int64_t);`.
    fn gen_task_type(&mut self, ty: &Type) {
        // `[@r](int) -> int`: the function, which takes what the lambda keeps
        // first, and what it keeps, a block of its region.
        if let Type::Closure(closure) = ty {
            let task = &closure.task;
            let hidden = self.hidden_of_type(task);
            let params: Vec<String> = std::iter::once("void*".to_string()).chain(task.params.iter().zip(&task.taken).zip(&hidden).map(|((param, how), hidden)| task_param_c(param, how, *hidden))).collect();
            self.w.line(&format!("typedef struct {{ {} (*call)({}); void* kept; }} {};", c_type(&task.ret), params.join(", "), c_type(ty)));
            return;
        }
        let Type::Task(task) = ty else { return };
        let hidden = self.hidden_of_type(task);
        let params: Vec<String> = task.params.iter().zip(&task.taken).zip(&hidden).map(|((param, how), hidden)| task_param_c(param, how, *hidden)).collect();
        let params = if params.is_empty() { "void".to_string() } else { params.join(", ") };
        self.w.line(&format!("typedef {} (*{})({});", c_type(&task.ret), c_type(ty), params));
    }

    /// For each type of a lambda that keeps what it uses, once every type is
    /// complete: the call of a value of it, `vela__call__closure__…(f, …)`,
    /// which reads the value once, and the function a plain task value given
    /// where one is expected runs as, given the task where it keeps its
    /// copies (see `ExprKind::Kept`).
    fn gen_closure_helpers(&mut self, used: &UsedTypes) {
        for ty in &used.tasks {
            let Type::Closure(closure) = ty else { continue };
            let task = &closure.task;
            let hidden = self.hidden_of_type(task);
            let params: Vec<String> = task.params.iter().zip(&task.taken).zip(&hidden).enumerate().map(|(i, ((param, how), hidden))| format!("{} v{}", task_param_c(param, how, *hidden), i)).collect();
            let args: Vec<String> = (0..task.params.len()).map(|i| format!("v{}", i)).collect();
            let send = if task.ret == Type::Nothing { "" } else { "return " };
            let ret = c_type(&task.ret);
            let word = type_word(ty);
            let given = std::iter::once(format!("{} f", c_type(ty))).chain(params.iter().cloned()).collect::<Vec<_>>().join(", ");
            let passed = std::iter::once("f.kept".to_string()).chain(args.iter().cloned()).collect::<Vec<_>>().join(", ");
            self.w.line(&format!("static inline {} vela__call__{}({}) {{ {}f.call({}); }}", ret, word, given, send, passed));
            let plain = c_type(&Type::Task(Box::new(task.clone())));
            let kept = std::iter::once("void* kept".to_string()).chain(params).collect::<Vec<_>>().join(", ");
            self.w.line(&format!("static inline {} vela__plain__{}({}) {{ {}(*({}*)kept)({}); }}", ret, word, kept, send, plain, args.join(", ")));
        }
    }

    /// For task `name`, named as a value, a function taking its values as its
    /// type does, when the task's own C takes them otherwise: a large `flex`
    /// or `fixed` parameter is copied into the task, where a task value reads
    /// it where the caller keeps it, and a native task takes every value as it
    /// is (see `is_hidden`).
    fn gen_adapter(&mut self, items: &[Item], name: &str) {
        let Some(t) = items.iter().find_map(|item| match item {
            Item::Task(t) if t.name == name => Some(t),
            _ => None,
        }) else {
            return;
        };
        // `.this` of an `alters` method is the caller's place, as `flex&` is.
        let alters = self.altering.contains(name);
        let taken = t
            .params
            .iter()
            .map(|param| {
                if param.ty == Type::Region {
                    Taken::Region(param.name.clone())
                } else if param.passing.is_ref() {
                    Taken::Place(param.passing)
                } else if alters && param.name == THIS {
                    Taken::Place(Passing::FlexRef)
                } else {
                    Taken::Value
                }
            })
            .collect();
        let task = TaskType { params: t.params.iter().map(|param| param.ty.clone()).collect(), taken, ret: t.ret.clone() };
        let wanted = self.hidden_of_type(&task);
        let own = self.hidden_of(name).to_vec();
        if wanted == own {
            return;
        }
        let params: Vec<String> = task
            .params
            .iter()
            .zip(&task.taken)
            .zip(&wanted)
            .enumerate()
            .map(|(i, ((param, how), hidden))| format!("{} v{}", task_param_c(param, how, *hidden), i))
            .collect();
        let args: Vec<String> = wanted
            .iter()
            .zip(own.iter().chain(std::iter::repeat(&false)))
            .enumerate()
            .map(|(i, (wanted, own))| match (wanted, own) {
                (true, false) => format!("*v{}", i),
                (false, true) => format!("&v{}", i),
                _ => format!("v{}", i),
            })
            .collect();
        // A lambda that keeps what it uses is given what it keeps first.
        let (params, args): (Vec<String>, Vec<String>) = match t.kept {
            Some(_) => (std::iter::once(format!("void* {}", KEPT_PARAM)).chain(params).collect(), std::iter::once(KEPT_PARAM.to_string()).chain(args).collect()),
            None => (params, args),
        };
        let params = if params.is_empty() { "void".to_string() } else { params.join(", ") };
        let call = format!("{}({})", mangle(name), args.join(", "));
        self.w.open(&format!("static {} {}({})", c_type(&t.ret), adapter_name(name), params));
        self.w.line(&if t.ret == Type::Nothing { format!("{};", call) } else { format!("return {};", call) });
        self.w.close();
        self.adapted.insert(name.to_string());
    }

    /// The C value of task `name` named as a value: its function, or the one
    /// that takes its values as its type does (see `gen_adapter`).
    fn task_value(&self, name: &str) -> String {
        if self.adapted.contains(name) { adapter_name(name) } else { mangle(name) }
    }

    /// `[@r](…) … fi`, the value of lambda `lambda`, which keeps what it uses:
    /// its function, and a block of region `region` holding the region and a
    /// copy of each variable it uses (see `check::kept_name`).
    fn gen_closure(&mut self, e: &Expr, lambda: &str, region: &str) -> Result<String> {
        let kept = kept_name(lambda);
        let fields: Vec<Field> = self.shapes.get(&kept).map(|shape| shape.fields.clone()).ok_or_else(|| Error::internal_at(e.span, format!("no block for what lambda '{}' keeps", lambda)))?;
        let mut values = Vec::new();
        for field in &fields {
            let value = if field.ty == Type::Region {
                c_ident(&field.name)
            } else if field.passing.is_ref() {
                // A place: the variable's address, as a `flex&` parameter is given.
                let mut var = Expr::new(ExprKind::Var(field.name.clone()), e.span);
                var.ty = field.ty.ptr_inner().cloned();
                format!("({})({})", c_type(&field.ty), self.gen_address(&var)?)
            } else {
                let mut var = Expr::new(ExprKind::Var(field.name.clone()), e.span);
                var.ty = Some(field.ty.clone());
                self.gen_expr(&var)?
            };
            values.push(format!(".{} = {}", c_ident(&field.name), value));
        }
        Ok(format!(
            "(({0}){{ {1}, vela__rt__new({2}, &({3}){{ {4} }}, sizeof({3}), _Alignof({3})) }})",
            c_type(e.ty()?),
            self.task_value(lambda),
            c_ident(region),
            mangle(&kept),
            values.join(", ")
        ))
    }

    /// `f(x)`, the call of the task value `callee` holds: the value is read
    /// first, then the arguments, left to right, each given as the task's type
    /// takes it.
    fn gen_invoke(&mut self, e: &Expr, callee: &Expr, args: &[Expr]) -> Result<String> {
        // A lambda that keeps what it uses is called with what it keeps.
        let (task, call) = match callee.ty()? {
            Type::Task(task) => (task.as_ref(), None),
            Type::Closure(closure) => (&closure.task, Some(format!("vela__call__{}", type_word(callee.ty()?)))),
            _ => return Err(Error::internal_at(e.span, "a call of a value that holds no task")),
        };
        let task = &task.clone();
        let mut hidden = vec![false];
        hidden.extend(self.hidden_of_type(task));
        let operands: Vec<&Expr> = std::iter::once(callee).chain(args).collect();
        let values = if hidden.contains(&true) { self.gen_hidden_operands(&operands, &hidden, true)? } else { self.gen_operands(&operands)? };
        let Some((function, values)) = values.split_first() else { return Err(Error::internal_at(e.span, "a call without the value it calls")) };
        if let Some(call) = call {
            let given: Vec<String> = std::iter::once(function.clone()).chain(values.iter().cloned()).collect();
            return Ok(format!("{}({})", call, given.join(", ")));
        }
        Ok(format!("({})({})", function, values.join(", ")))
    }

    /// Which parameters of the task generated as `task` are passed by a
    /// hidden pointer, in order.
    fn hidden_of(&self, task: &str) -> &[bool] {
        self.hidden_params.get(task).map_or(&[], Vec::as_slice)
    }

    /// Whether parameter `p` of `t` is passed by a hidden pointer: one that
    /// never changes, of a type larger than `HIDDEN_ABOVE` bytes. A native
    /// task's C is the runtime's, which takes its values as they are.
    fn is_hidden(&self, t: &TaskDef, p: &Field, alters: bool) -> bool {
        !t.native && p.passing == Passing::Copy && p.ty != Type::Region && !(alters && p.name == THIS) && self.c_layout(&p.ty).0 > HIDDEN_ABOVE
    }

    /// The size and alignment, in bytes, of a value of `ty` in the C this
    /// writes (see `Layout`).
    fn c_layout(&self, ty: &Type) -> (usize, usize) {
        Layout { shapes: &self.shapes, kinds: &self.kinds }.of(ty)
    }

    fn fresh_temp(&mut self, prefix: &str) -> String {
        let n = self.temp_counter;
        self.temp_counter += 1;
        format!("vela__{}__{}", prefix, n)
    }

    /// A name for the region a `show` makes for its line (see `gen_show`). It
    /// goes where the regions a block declares go, and those are written through
    /// `c_ident`, which leaves this name as it is: no Vela name holds `__`, so
    /// no region of a program is called this.
    fn shown_region(&mut self) -> String {
        let n = self.temp_counter;
        self.temp_counter += 1;
        format!("shown__{}", n)
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

    /// The C a macro's `#define` holds: its value, as a literal of its type.
    fn macro_value(&mut self, value: &Expr) -> Result<String> {
        Ok(match (&value.kind, value.ty()?) {
            (ExprKind::IntLit(i64::MIN), Type::Int) => "(-INT64_C(9223372036854775807) - 1)".to_string(),
            (ExprKind::IntLit(n), Type::Int) => format!("INT64_C({})", n),
            (ExprKind::IntLit(n), _) => format!("((uint8_t){})", n),
            _ => self.gen_expr(value)?,
        })
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

    /// The struct of a `maybe T` (`has`, `value`), a `T ~ E` (`ok`, `value`,
    /// `error`; no `value` for `nothing ~ E`), several results (`r0`, `r1`, …),
    /// an array (`items`), or a slice (`items`, a pointer, `len`, and `room`,
    /// where `add` puts more items; see `vela__rt__items_insert`). A
    /// `maybe`'s and an `or`'s members are always initialized, so either can be
    /// read.
    fn gen_compound(&mut self, ty: &Type) {
        // A map: its entries, and the header its values point to (see the
        // runtime's notes on maps, and `gen_map_helpers`).
        if let Type::Map(key, value, _) = ty {
            self.w.open(&format!("struct {}", map_entry(ty)));
            self.w.line("uint64_t hash;");
            self.w.line(&format!("{} key;", c_type(key)));
            self.w.line(&format!("{} value;", c_type(value)));
            self.w.close_with(";");
            self.w.open(&format!("struct {}", compound_name(ty)));
            self.w.line(&format!("{}* entries;", map_entry(ty)));
            self.w.line("uint32_t* slots;");
            self.w.line("int64_t used;");
            self.w.line("int64_t live;");
            self.w.line("int64_t cap;");
            self.w.line("uint64_t mask;");
            self.w.line("vela__rt__region* region;");
            self.w.close_with(";");
            return;
        }
        self.w.open(&format!("struct {}", c_type(ty)));
        match ty {
            Type::Results(types) => {
                for (i, result) in types.iter().enumerate() {
                    self.w.line(&format!("{} {};", c_type(result), result_member(i)));
                }
            }
            Type::Tuple(items) => {
                for (i, item) in items.iter().enumerate() {
                    self.w.line(&format!("{} {};", c_type(item), tuple_member(i)));
                }
            }
            Type::Maybe(inner) => {
                self.w.line("bool has;");
                self.w.line(&format!("{} value;", c_type(inner)));
            }
            Type::Array(items, count) => self.w.line(&format!("{} items[{}];", c_type(items), count)),
            Type::Slice(items, _) => {
                self.w.line(&format!("{}* items;", c_type(items)));
                self.w.line("int64_t len;");
                self.w.line("vela__rt__room* room;");
            }
            Type::Or(value, error) => {
                self.w.line("bool ok;");
                if **value != Type::Nothing {
                    self.w.line(&format!("{} value;", c_type(value)));
                }
                self.w.line(&format!("{} error;", c_type(error)));
            }
            _ => {}
        }
        self.w.close_with(";");
    }

    /// The hash and the equality of each type of key a map has, then each map
    /// type's table: made, searched, added to, remade, read, taken from, and
    /// read out as slices. They are `static inline` and marked as maybe unused,
    /// so a program that uses only some of them builds without a warning for
    /// the rest (clang warns of an unused `static inline` function of the
    /// program itself, not only of a header's).
    fn gen_map_helpers(&mut self, used: &UsedTypes) {
        for key in &used.map_keys {
            let (ctype, word) = (c_type(key), type_word(key));
            let hash = match key {
                Type::Text(_) => "vela__rt__hash_bytes(vela__rt__hash_seed(), k.bytes, k.len)".to_string(),
                Type::Ptr(..) => "vela__rt__mix((uint64_t)(uintptr_t)k ^ vela__rt__hash_seed())".to_string(),
                Type::Tuple(items) => {
                    let mut h = "vela__rt__hash_seed()".to_string();
                    for (i, item) in items.iter().enumerate() {
                        h = format!("vela__rt__mix({} ^ vela__hash__{}(k.{}))", h, type_word(item), tuple_member(i));
                    }
                    h
                }
                _ => "vela__rt__mix((uint64_t)k ^ vela__rt__hash_seed())".to_string(),
            };
            let same = match key {
                Type::Text(_) => "vela__rt__text_eq(a, b)".to_string(),
                Type::Tuple(items) => items
                    .iter()
                    .enumerate()
                    .map(|(i, item)| format!("vela__keyeq__{}(a.{1}, b.{1})", type_word(item), tuple_member(i)))
                    .collect::<Vec<_>>()
                    .join(" && "),
                _ => "a == b".to_string(),
            };
            self.w.open(&format!("VELA__RT__MAYBE_UNUSED static inline uint64_t vela__hash__{}({} k)", word, ctype));
            self.w.line(&format!("return {};", hash));
            self.w.close();
            self.w.open(&format!("VELA__RT__MAYBE_UNUSED static inline bool vela__keyeq__{0}({1} a, {1} b)", word, ctype));
            self.w.line(&format!("return {};", same));
            self.w.close();
        }
        for map in used.compounds.iter().filter(|ty| matches!(ty, Type::Map(..))) {
            let Type::Map(key, value, _) = map else { continue };
            let (m, e) = (compound_name(map), map_entry(map));
            let (k, v) = (c_type(key), c_type(value));
            let maybe = c_type(&Type::Maybe(value.clone()));
            // A maybe of a pointer is the pointer, NULL for none.
            let pointer = is_pointer(value);
            let none = if pointer { "NULL".to_string() } else { format!("({}){{ .has = false }}", maybe) };
            let some = |x: &str| if pointer { x.to_string() } else { format!("({}){{ .has = true, .value = {} }}", maybe, x) };
            let hash = format!("vela__rt__kept_hash(vela__hash__{}(key))", type_word(key));
            let same = |a: &str| format!("vela__keyeq__{}({}, key)", type_word(key), a);
            let name = |what: &str| item_helper(what, map);
            // Made, with room for `hint` keys.
            self.w.open(&format!("VELA__RT__MAYBE_UNUSED static inline {}* {}(vela__rt__region* r, int64_t hint)", m, name("map_new")));
            self.w.line(&format!("{0}* m = vela__rt__region_alloc(r, sizeof({0}), _Alignof({0}));", m));
            self.w.line("int64_t cap = 8;");
            self.w.open("while (cap < hint)");
            self.w.line("cap *= 2;");
            self.w.close();
            self.w.line(&format!("m->entries = vela__rt__map_room(r, cap, sizeof({0}), _Alignof({0}), false);", e));
            self.w.line("m->slots = vela__rt__map_room(r, cap * 2, sizeof(uint32_t), _Alignof(uint32_t), true);");
            self.w.line("m->used = 0;");
            self.w.line("m->live = 0;");
            self.w.line("m->cap = cap;");
            self.w.line("m->mask = (uint64_t)(cap * 2 - 1);");
            self.w.line("m->region = r;");
            self.w.line("return m;");
            self.w.close();
            // The slot holding `key`, or -1.
            self.w.open(&format!("VELA__RT__MAYBE_UNUSED static inline int64_t {}(const {}* m, {} key, uint64_t h)", name("map_slot"), m, k));
            self.w.line("uint64_t i = h & m->mask;");
            self.w.open("for (;;)");
            self.w.line("uint32_t s = m->slots[i];");
            self.w.open("if (s == 0)");
            self.w.line("return -1;");
            self.w.close();
            self.w.open(&format!("if (s >= 2 && m->entries[s - 2].hash == h && {})", same("m->entries[s - 2].key")));
            self.w.line("return (int64_t)i;");
            self.w.close();
            self.w.line("i = (i + 1) & m->mask;");
            self.w.close();
            self.w.close();
            // Entry `entry` in the first slot free from its hash on.
            self.w.open(&format!("VELA__RT__MAYBE_UNUSED static inline void {}({}* m, uint64_t h, int64_t entry)", name("map_place"), m));
            self.w.line("uint64_t i = h & m->mask;");
            self.w.open("while (m->slots[i] >= 2)");
            self.w.line("i = (i + 1) & m->mask;");
            self.w.close();
            self.w.line("m->slots[i] = (uint32_t)(entry + 2);");
            self.w.close();
            // Made again when its entries are full: twice as large when more
            // than half of them are kept, and in place when half are gone.
            self.w.open(&format!("VELA__RT__MAYBE_UNUSED static inline void {}({}* m)", name("map_remake"), m));
            self.w.line("int64_t kept = 0;");
            self.w.open("if (m->live * 2 > m->cap || m->live == m->cap)");
            self.w.line("int64_t cap = m->cap < 8 ? 8 : m->cap * 2;");
            self.w.line(&format!("{0}* entries = vela__rt__map_room(m->region, cap, sizeof({0}), _Alignof({0}), false);", e));
            self.w.open("for (int64_t i = 0; i < m->used; i++)");
            self.w.open("if (m->entries[i].hash != 0)");
            self.w.line("entries[kept++] = m->entries[i];");
            self.w.close();
            self.w.close();
            self.w.line("m->entries = entries;");
            self.w.line("m->cap = cap;");
            self.w.line("m->slots = vela__rt__map_room(m->region, cap * 2, sizeof(uint32_t), _Alignof(uint32_t), true);");
            self.w.line("m->mask = (uint64_t)(cap * 2 - 1);");
            self.w.reopen("else");
            self.w.open("for (int64_t i = 0; i < m->used; i++)");
            self.w.open("if (m->entries[i].hash != 0)");
            self.w.line("m->entries[kept++] = m->entries[i];");
            self.w.close();
            self.w.close();
            self.w.line("memset(m->slots, 0, (size_t)(m->mask + 1) * sizeof(uint32_t));");
            self.w.close();
            self.w.line("m->used = kept;");
            self.w.open("for (int64_t i = 0; i < kept; i++)");
            self.w.line(&format!("{}(m, m->entries[i].hash, i);", name("map_place")));
            self.w.close();
            self.w.close();
            // `m[key] = value`: the key's value replaced, or the key added last.
            self.w.open(&format!("VELA__RT__MAYBE_UNUSED static inline void {}({}* m, {} key, {} value)", name("map_set"), m, k, v));
            self.w.line(&format!("uint64_t h = {};", hash));
            self.w.line(&format!("int64_t s = {}(m, key, h);", name("map_slot")));
            self.w.open("if (s >= 0)");
            self.w.line("m->entries[m->slots[s] - 2].value = value;");
            self.w.line("return;");
            self.w.close();
            self.w.open("if (m->used == m->cap)");
            self.w.line(&format!("{}(m);", name("map_remake")));
            self.w.close();
            self.w.line(&format!("m->entries[m->used] = ({}){{ .hash = h, .key = key, .value = value }};", e));
            self.w.line(&format!("{}(m, h, m->used);", name("map_place")));
            self.w.line("m->used++;");
            self.w.line("m->live++;");
            self.w.close();
            // `m[key]`, where the value is: the program stops without the key.
            self.w.open(&format!("VELA__RT__MAYBE_UNUSED static inline {}* {}({}* m, {} key, const char* at)", v, name("map_at"), m, k));
            self.w.line(&format!("int64_t s = {}(m, key, {});", name("map_slot"), hash));
            self.w.open("if (s < 0)");
            self.w.line("vela__rt__no_key(at);");
            self.w.close();
            self.w.line("return &m->entries[m->slots[s] - 2].value;");
            self.w.close();
            // `m[#key]`: the value, or none.
            self.w.open(&format!("VELA__RT__MAYBE_UNUSED static inline {} {}(const {}* m, {} key)", maybe, name("lookup"), m, k));
            self.w.line(&format!("int64_t s = {}(m, key, {});", name("map_slot"), hash));
            self.w.open("if (s < 0)");
            self.w.line(&format!("return {};", none));
            self.w.close();
            self.w.line(&format!("return {};", some("m->entries[m->slots[s] - 2].value")));
            self.w.close();
            // `m.del(key)`: the key taken out, and its value given, or none.
            self.w.open(&format!("VELA__RT__MAYBE_UNUSED static inline {} {}({}* m, {} key)", maybe, name("map_del"), m, k));
            self.w.line(&format!("int64_t s = {}(m, key, {});", name("map_slot"), hash));
            self.w.open("if (s < 0)");
            self.w.line(&format!("return {};", none));
            self.w.close();
            self.w.line(&format!("{}* entry = &m->entries[m->slots[s] - 2];", e));
            self.w.line("entry->hash = 0;");
            self.w.line("m->slots[s] = 1;");
            self.w.line("m->live--;");
            self.w.line(&format!("return {};", some("entry->value")));
            self.w.close();
            // `m.has(key)`.
            self.w.open(&format!("VELA__RT__MAYBE_UNUSED static inline bool {}(const {}* m, {} key)", name("map_has"), m, k));
            self.w.line(&format!("return {}(m, key, {}) >= 0;", name("map_slot"), hash));
            self.w.close();
            // `new [K: V]([k: v, …])`: the pairs added in order, so a key
            // given twice keeps its last value.
            self.w.open(&format!("VELA__RT__MAYBE_UNUSED static inline {0}* {1}(vela__rt__region* r, int64_t n, {2} const* keys, {3} const* values)", m, name("map_of"), k, v));
            self.w.line(&format!("{}* m = {}(r, n * 2);", m, name("map_new")));
            self.w.open("for (int64_t i = 0; i < n; i++)");
            self.w.line(&format!("{}(m, keys[i], values[i]);", name("map_set")));
            self.w.close();
            self.w.line("return m;");
            self.w.close();
            // `m.keys at r` and `m.values at r`: a slice in `r`, in order.
            for (what, ty, member) in [("map_keys", &**key, "key"), ("map_values", &**value, "value")] {
                let slice = c_type(&Type::Slice(Box::new(ty.clone()), None));
                let item = c_type(ty);
                self.w.open(&format!("VELA__RT__MAYBE_UNUSED static inline {} {}(vela__rt__region* r, const {}* m)", slice, name(what), m));
                self.w.open("if (m->live == 0)");
                self.w.line(&format!("return ({}){{ .items = vela__rt__no_items(), .len = 0 }};", slice));
                self.w.close();
                self.w.line(&format!("{0}* items = vela__rt__region_items(r, m->live, sizeof({0}), _Alignof({0}), \"a map's {1}s\");", item, member));
                self.w.line("int64_t n = 0;");
                self.w.open("for (int64_t i = 0; i < m->used; i++)");
                self.w.open("if (m->entries[i].hash != 0)");
                self.w.line(&format!("items[n++] = m->entries[i].{};", member));
                self.w.close();
                self.w.close();
                self.w.line(&format!("return ({}){{ .items = items, .len = n }};", slice));
                self.w.close();
            }
        }
    }

    /// Whether `each` goes through a map, `m` or `&m`, or one side of one,
    /// `m.keys` or `m.values`.
    fn walks_map(&self, items: &Expr) -> bool {
        match &items.kind {
            ExprKind::MapOp { region: None, .. } => true,
            ExprKind::Ref(place) => matches!(place.ty, Some(Type::Map(..))),
            _ => matches!(items.ty, Some(Type::Map(..))),
        }
    }

    /// `each` through a map's entries: `name` gets the key (or, through
    /// `m.values`, the value) and `second` the value. Through `m` or one side
    /// of it, the entries are those there when the loop started, as many as
    /// there were, as a slice's items are; through `&m`, the map as it is at
    /// each pass. An entry taken out is skipped when its turn comes.
    fn gen_each_map(&mut self, name: &str, second: Option<&str>, items: &Expr, body: &Block, finally: Option<&[Stmt]>, named: (Option<String>, Option<Giving>)) -> Result<()> {
        let (map, live, only) = match &items.kind {
            ExprKind::MapOp { map, op, .. } => (map.as_ref(), false, Some(*op)),
            ExprKind::Ref(place) => (place.as_ref(), true, None),
            _ => (items, false, None),
        };
        let ty = map.ty()?.clone();
        let Type::Map(key, value, _) = &ty else { return Err(Error::internal_at(items.span, "each over no map")) };
        let (walked, entries, count, index) = (self.fresh_temp("map"), self.fresh_temp("entries"), self.fresh_temp("count"), self.fresh_temp("index"));
        self.w.open("");
        let (entries_now, count_now) = if live {
            let address = self.gen_address(map)?;
            self.w.line(&format!("{} const* {} = {};", c_type(&ty), walked, address));
            (format!("(*{})->entries", walked), format!("(*{})->used", walked))
        } else {
            let value = self.gen_expr(map)?;
            self.w.line(&format!("{}* {} = ({})->entries;", map_entry(&ty), entries, value));
            self.w.line(&format!("int64_t {} = ({})->used;", count, value));
            (entries.clone(), count.clone())
        };
        self.enter_loop(named, finally.is_some());
        self.w.open(&format!("for (int64_t {0} = 0; {0} < {1}; {0}++)", index, count_now));
        self.w.open(&format!("if ({}[{}].hash != 0)", entries_now, index));
        let (first, first_ty, member) = match only {
            Some(MapOp::Values) => (name, &**value, "value"),
            _ => (name, &**key, "key"),
        };
        if first != DISCARD {
            self.w.line(&format!("{} {} = {}[{}].{};", c_type(first_ty), c_ident(first), entries_now, index, member));
            self.use_if_unread(first, body);
        }
        if let Some(second) = second.filter(|second| *second != DISCARD) {
            self.w.line(&format!("{} {} = {}[{}].value;", c_type(value), c_ident(second), entries_now, index));
            self.use_if_unread(second, body);
        }
        self.gen_pass(body)?;
        self.w.close();
        self.end_pass();
        self.w.close();
        self.finish_loop(finally)?;
        self.leave_loop();
        self.w.close();
        Ok(())
    }

    /// The helpers for arrays and slices that the program uses, each a C
    /// function so that an array or slice given to it is evaluated once.
    fn gen_item_helpers(&mut self, used: &UsedTypes) {
        for ty in &used.lookups {
            let maybe = c_type(&Type::Maybe(Box::new(lookup_item(ty))));
            // A maybe of a pointer is the pointer, NULL for none.
            let item = lookup_item(ty);
            let (none, some) = if is_pointer(&item) {
                ("NULL".to_string(), "items[i]".to_string())
            } else {
                (format!("({}){{ .has = false }}", maybe), format!("({}){{ .has = true, .value = items[i] }}", maybe))
            };
            let name = item_helper("lookup", ty);
            match ty {
                Type::Text(_) => {
                    self.w.open(&format!("static {} {}(vela__rt__text t, int64_t i)", maybe, name));
                    self.w.line("const uint8_t* items = (const uint8_t*)t.bytes;");
                    self.w.line("int64_t len = t.len;");
                }
                Type::Slice(..) => {
                    self.w.open(&format!("static {} {}({} s, int64_t i)", maybe, name, c_type(ty)));
                    self.w.line(&format!("{} const* items = s.items;", c_type(&item)));
                    self.w.line("int64_t len = s.len;");
                }
                // `T const*`: the items don't change, whatever they are (a
                // pointer item stays a pointer to what can change).
                _ => self.w.open(&format!("static {} {}({} const* items, int64_t len, int64_t i)", maybe, name, c_type(&item))),
            }
            self.w.open("if (i < 0 || i >= len)");
            self.w.line(&format!("return {};", none));
            self.w.close();
            self.w.line(&format!("return {};", some));
            self.w.close();
        }
        for slice in &used.slice_at {
            let (ctype, items) = (c_type(slice), c_type(item_type(slice)));
            self.w.open(&format!("static {}* {}({} s, int64_t i, const char* at)", items, item_helper("at", slice), ctype));
            self.w.line("return s.items + vela__rt__index(i, s.len, at);");
            self.w.close();
        }
        for slice in &used.slice_part {
            let ctype = c_type(slice);
            self.w.open(&format!("static {0} {1}({0} s, int64_t from, int64_t to, const char* at)", ctype, item_helper("part", slice)));
            self.w.line("vela__rt__part(from, to, s.len, \"slice\", at);");
            // A part ending where the slice's room is used up can grow into it.
            self.w.line(&format!("return ({}){{ .items = s.items + from, .len = to - from, .room = s.room }};", ctype));
            self.w.close();
        }
        let mut shallow: Vec<&Type> = used.slice_copy.list.iter().collect();
        for slice in used.slice_copied.list.iter().filter(|slice| !self.copies_deep(slice)) {
            if !shallow.contains(&slice) {
                shallow.push(slice);
            }
        }
        for slice in shallow {
            let (ctype, items) = (c_type(slice), c_type(item_type(slice)));
            self.w.open(&format!("static {0} {1}(vela__rt__region* r, {0} s)", ctype, item_helper("copy", slice)));
            // The source exists, so its size in bytes can't overflow.
            self.w.line(&format!("{0}* items = vela__rt__region_alloc(r, (size_t)s.len * sizeof({0}), _Alignof({0}));", items));
            self.w.line(&format!("memcpy(items, s.items, (size_t)s.len * sizeof({}));", items));
            self.w.line(&format!("return ({}){{ .items = items, .len = s.len }};", ctype));
            self.w.close();
        }
        for slice in &used.slice_fill {
            let (ctype, items) = (c_type(slice), c_type(item_type(slice)));
            self.w.open(&format!("static {0} {1}(vela__rt__region* r, int64_t len, {2} value, const char* at)", ctype, item_helper("fill", slice), items));
            self.w.line(&format!("{0}* items = vela__rt__region_items(r, len, sizeof({0}), _Alignof({0}), at);", items));
            self.w.open("for (int64_t i = 0; i < len; i++)");
            self.w.line("items[i] = value;");
            self.w.close();
            self.w.line(&format!("return ({}){{ .items = items, .len = len }};", ctype));
            self.w.close();
        }
        for slice in &used.slice_add {
            let (ctype, items) = (c_type(slice), c_type(item_type(slice)));
            self.w.open(&format!("static void {}(vela__rt__region* r, {}* s, {} value)", item_helper("add", slice), ctype, items));
            self.w.line(&format!("{0}* items = vela__rt__items_insert(r, s->items, s->len, &s->room, s->len, sizeof({0}), _Alignof({0}));", items));
            self.w.line("items[s->len] = value;");
            self.w.line("s->items = items;");
            self.w.line("s->len += 1;");
            self.w.close();
        }
        for slice in &used.slice_insert {
            let (ctype, items) = (c_type(slice), c_type(item_type(slice)));
            self.w.open(&format!("static void {}(vela__rt__region* r, {}* s, int64_t i, {} value, const char* at)", item_helper("insert", slice), ctype, items));
            // Any index from 0 to the length: the item goes before item `i`, or last.
            self.w.line("vela__rt__index(i, s->len + 1, at);");
            self.w.line(&format!("{0}* items = vela__rt__items_insert(r, s->items, s->len, &s->room, i, sizeof({0}), _Alignof({0}));", items));
            self.w.line("items[i] = value;");
            self.w.line("s->items = items;");
            self.w.line("s->len += 1;");
            self.w.close();
        }
        for slice in &used.slice_del {
            let (ctype, items) = (c_type(slice), c_type(item_type(slice)));
            self.w.open(&format!("static {0} {1}({2}* s, int64_t i, const char* at)", items, item_helper("del", slice), ctype));
            self.w.line(&format!("{} item = s->items[vela__rt__index(i, s->len, at)];", items));
            self.w.line(&format!("vela__rt__items_remove(s->items, s->len, s->room, i, sizeof({}));", items));
            self.w.line("s->len -= 1;");
            self.w.line("return item;");
            self.w.close();
        }
        for slice in &used.slice_pop {
            let (ctype, items) = (c_type(slice), c_type(item_type(slice)));
            self.w.open(&format!("static {0} {1}({2}* s, const char* at)", items, item_helper("pop", slice), ctype));
            self.w.line("vela__rt__not_empty(s->len, at);");
            self.w.line(&format!("{} item = s->items[s->len - 1];", items));
            self.w.line(&format!("vela__rt__items_remove(s->items, s->len, s->room, s->len - 1, sizeof({}));", items));
            self.w.line("s->len -= 1;");
            self.w.line("return item;");
            self.w.close();
        }
        if used.scans {
            let maybe = c_type(&Type::Maybe(Box::new(Type::Text(None))));
            self.w.open(&format!("static {} vela__scan(vela__rt__region* r, vela__rt__text prompt)", maybe));
            self.w.line(&format!("{} line = {{ .has = false }};", maybe));
            self.w.line("line.has = vela__rt__scan(r, prompt, &line.value);");
            self.w.line("return line;");
            self.w.close();
        }
        for array in &used.array_fill {
            let Type::Array(items, count) = array else { continue };
            let ctype = c_type(array);
            self.w.open(&format!("static {0} {1}({2} value)", ctype, item_helper("fill", array), c_type(items)));
            self.w.line(&format!("{} array;", ctype));
            self.w.open(&format!("for (int64_t i = 0; i < INT64_C({}); i++)", count));
            self.w.line("array.items[i] = value;");
            self.w.close();
            self.w.line("return array;");
            self.w.close();
        }
    }

    /// The comparisons a program makes that need a function of their own:
    /// `vela__same__K` for `==` on a kind (its variant, then what it carries,
    /// each by its own rule) and on a slice (its view), and `vela__similar__K`
    /// for `~=`, item by item. Each takes the two values' addresses, and all
    /// are declared before any is defined, since a kind's calls another's.
    fn gen_compares(&mut self, used: &UsedTypes) -> Result<()> {
        for (what, types) in [("same", &used.same), ("similar", &used.similar)] {
            for ty in types {
                self.w.line(&format!("static bool {}({1} const* a, {1} const* b);", copy_helper(what, ty), c_type(ty)));
            }
        }
        for ty in &used.same {
            self.w.open(&format!("static bool {}({1} const* a, {1} const* b)", copy_helper("same", ty), c_type(ty)));
            match ty {
                Type::Slice(..) => self.w.line("return a->items == b->items && a->len == b->len;"),
                Type::Named(name, _) => self.gen_kind_compare(name, "same")?,
                other => return Err(Error::internal(format!("'==' by a function of its own on {}", other))),
            }
            self.w.close();
        }
        for ty in &used.similar {
            self.w.open(&format!("static bool {}({1} const* a, {1} const* b)", copy_helper("similar", ty), c_type(ty)));
            match ty {
                // The same keys, each with similar values, in any order.
                Type::Map(_, value, _) => {
                    let pointer = is_pointer(value);
                    self.w.line(&format!("const {0}* m = *a;", compound_name(ty)));
                    self.w.line(&format!("const {0}* n = *b;", compound_name(ty)));
                    self.w.open("if (m == n)");
                    self.w.line("return true;");
                    self.w.close();
                    self.w.open("if (m->live != n->live)");
                    self.w.line("return false;");
                    self.w.close();
                    self.w.open("for (int64_t i = 0; i < m->used; i++)");
                    self.w.open("if (m->entries[i].hash == 0)");
                    self.w.line("continue;");
                    self.w.close();
                    self.w.line(&format!("{} found = {}(n, m->entries[i].key);", c_type(&Type::Maybe(value.clone())), item_helper("lookup", ty)));
                    let (missing, found) = if pointer { ("found == NULL", "found") } else { ("!found.has", "found.value") };
                    let test = self.compare_item("similar", value, "m->entries[i].value", found);
                    self.w.open(&format!("if ({} || !({}))", missing, test));
                    self.w.line("return false;");
                    self.w.close();
                    self.w.close();
                    self.w.line("return true;");
                }
                Type::Array(items, count) => {
                    self.w.open(&format!("for (int64_t i = 0; i < {}; i++)", count));
                    self.w.open(&format!("if (!({}))", self.compare_item("similar", items, "a->items[i]", "b->items[i]")));
                    self.w.line("return false;");
                    self.w.close();
                    self.w.close();
                    self.w.line("return true;");
                }
                Type::Slice(items, _) => {
                    self.w.open("if (a->len != b->len)");
                    self.w.line("return false;");
                    self.w.close();
                    self.w.open("for (int64_t i = 0; i < a->len; i++)");
                    self.w.open(&format!("if (!({}))", self.compare_item("similar", items, "a->items[i]", "b->items[i]")));
                    self.w.line("return false;");
                    self.w.close();
                    self.w.close();
                    self.w.line("return true;");
                }
                Type::Tuple(items) => {
                    let tests: Vec<String> = items
                        .iter()
                        .enumerate()
                        .map(|(i, item)| self.compare_item("similar", item, &format!("a->{}", tuple_member(i)), &format!("b->{}", tuple_member(i))))
                        .collect();
                    self.w.line(&format!("return {};", tests.join(" && ")));
                }
                Type::Maybe(inner) => {
                    self.w.open("if (a->has != b->has)");
                    self.w.line("return false;");
                    self.w.close();
                    self.w.line(&format!("return !a->has || ({});", self.compare_item("similar", inner, "a->value", "b->value")));
                }
                // Every field but shared ones, `priv` ones too: they are all
                // part of the value.
                Type::Named(name, _) if self.shapes.contains_key(name) => {
                    let fields: Vec<Field> = self.shapes.get(name).map(|shape| shape.fields.clone()).unwrap_or_default();
                    if fields.is_empty() {
                        self.w.line("(void)a;");
                        self.w.line("(void)b;");
                        self.w.line("return true;");
                    } else {
                        let tests: Vec<String> = fields
                            .iter()
                            .map(|field| self.compare_item("similar", &field.ty, &format!("a->{}", c_ident(&field.name)), &format!("b->{}", c_ident(&field.name))))
                            .collect();
                        self.w.line(&format!("return {};", tests.join(" && ")));
                    }
                }
                Type::Named(name, _) => self.gen_kind_compare(name, "similar")?,
                other => return Err(Error::internal(format!("'~=' on {}", other))),
            }
            self.w.close();
        }
        if !used.same.is_empty() || !used.similar.is_empty() {
            self.w.blank();
        }
        Ok(())
    }

    /// Whether two values `a` and `b` of type `ty`, both places, compare, as a
    /// C test, by `how`: `same` (`==`, each by its own rule) or `similar`
    /// (`~=`, item by item). A number, a bool, and a pointer compare as C does
    /// either way, and text by its bytes.
    fn compare_item(&self, how: &str, ty: &Type, a: &str, b: &str) -> String {
        match ty {
            Type::Int | Type::Unit | Type::Char | Type::Float | Type::Bool | Type::Ptr(..) | Type::Task(_) => format!("{} == {}", a, b),
            // `==` on maps is which map: the handle.
            Type::Map(..) if how == "same" => format!("{} == {}", a, b),
            Type::Text(_) => format!("vela__rt__text_eq({}, {})", a, b),
            // A `maybe` of a pointer is the pointer, `NULL` for none.
            Type::Maybe(inner) if is_pointer(inner) => format!("{} == {}", a, b),
            Type::Maybe(inner) if how == "same" => {
                format!("({0}.has == {1}.has && (!{0}.has || ({2})))", a, b, self.compare_item(how, inner, &format!("{}.value", a), &format!("{}.value", b)))
            }
            // By where they are: two places are one only when both values are.
            _ if how == "same" && !matches!(ty, Type::Slice(..)) && !self.is_kind(ty) => format!("(const void*)&{} == (const void*)&{}", a, b),
            _ => format!("{}(&{}, &{})", copy_helper(how, ty), a, b),
        }
    }

    /// Whether `ty` is a kind.
    fn is_kind(&self, ty: &Type) -> bool {
        matches!(ty, Type::Named(name, _) if self.kinds.contains_key(name))
    }

    /// Whether `==` compares values of type `ty` by where they are (see the
    /// checker's `by_place`): an array, a shape, a tuple, and a kind that can
    /// carry one.
    fn by_place(&self, ty: &Type) -> bool {
        self.by_place_in(ty, &mut Vec::new())
    }

    fn by_place_in(&self, ty: &Type, seen: &mut Vec<String>) -> bool {
        match ty {
            Type::Array(..) | Type::Tuple(_) => true,
            Type::Named(name, _) if self.shapes.contains_key(name) => true,
            Type::Named(name, _) => {
                let Some(kind) = self.kinds.get(name) else { return false };
                if seen.contains(name) {
                    return false;
                }
                seen.push(name.clone());
                let found = kind.variants.iter().flat_map(|variant| &variant.payload).any(|field| {
                    // A `maybe` a variant carries compares what it holds (see
                    // `kind_comparable`), so by that value's own rule.
                    let carried = match &field.ty {
                        Type::Maybe(inner) => inner.as_ref(),
                        other => other,
                    };
                    self.by_place_in(carried, seen)
                });
                seen.pop();
                found
            }
            _ => false,
        }
    }

    /// For each two kinds a value goes from and to (see `ExprKind::Widen`),
    /// `vela__widen__A__into__B`, which gives the variant of the second that
    /// has the name of the first's, carrying the same values.
    fn gen_widens(&mut self, used: &UsedTypes) -> Result<()> {
        for (from, to) in &used.widened {
            let (Type::Named(source, _), Type::Named(target, _)) = (from, to) else {
                return Err(Error::internal(format!("a value of {} made one of {}", from, to)));
            };
            let Some(kind) = self.kinds.get(source).cloned() else { return Err(Error::internal(format!("no kind '{}' to widen", source))) };
            self.w.open(&format!("static {} {}({} v)", c_type(to), widen_helper(from, to), c_type(from)));
            self.w.open("switch (v.tag)");
            let last = kind.variants.len().saturating_sub(1);
            for (i, variant) in kind.variants.iter().enumerate() {
                // The last one is the rest, so every way through gives a value.
                let label = if i == last { "default:".to_string() } else { format!("case {}:", tag_value(source, &variant.tag)) };
                let value = if variant.payload.is_empty() {
                    format!("({}){{ .tag = {} }}", mangle(target), tag_value(target, &variant.tag))
                } else {
                    let tag = c_ident(&variant.tag);
                    let fields: Vec<String> = variant.payload.iter().map(|field| format!(".{0} = v.data.{1}.{0}", c_ident(&field.name), tag)).collect();
                    format!("({}){{ .tag = {}, .data = {{ .{} = {{ {} }} }} }}", mangle(target), tag_value(target, &variant.tag), tag, fields.join(", "))
                };
                self.w.line(&label);
                self.w.line(&format!("    return {};", value));
            }
            self.w.close();
            self.w.close();
        }
        Ok(())
    }

    /// The body of `vela__same__K` or `vela__similar__K` (by `how`) for the
    /// kind `name`: the same variant, and what it carries compared.
    fn gen_kind_compare(&mut self, name: &str, how: &str) -> Result<()> {
        let Some(kind) = self.kinds.get(name).cloned() else { return Err(Error::internal(format!("no kind '{}' to compare", name))) };
        self.w.open("if (a->tag != b->tag)");
        self.w.line("return false;");
        self.w.close();
        if kind.variants.iter().all(|variant| variant.payload.is_empty()) {
            self.w.line("return true;");
            return Ok(());
        }
        self.w.open("switch (a->tag)");
        for variant in kind.variants.iter().filter(|variant| !variant.payload.is_empty()) {
            let tests: Vec<String> = variant
                .payload
                .iter()
                .map(|field| {
                    let at = |side: &str| format!("{}->data.{}.{}", side, c_ident(&variant.tag), c_ident(&field.name));
                    self.compare_item(how, &field.ty, &at("a"), &at("b"))
                })
                .collect();
            self.w.line(&format!("case {}:", tag_value(&kind.name, &variant.tag)));
            self.w.line(&format!("    return {};", tests.join(" && ")));
        }
        self.w.line("default:");
        self.w.line("    return true;");
        self.w.close();
        Ok(())
    }

    /// For each type a program copies (`x.copy() at r`), `vela__copied__K`, which
    /// copies a value into a region; and for each type a copy goes through,
    /// `vela__fix__K`, which replaces what a value copied byte for byte holds
    /// in regions with copies (see `vela__rt__copy` in the runtime).
    fn gen_copies(&mut self, copied: &[Type]) {
        let mut fixed: Vec<Type> = Vec::new();
        let mut waiting: Vec<Type> = copied.iter().map(fix_type).collect();
        while let Some(ty) = waiting.pop() {
            if fixed.contains(&ty) || !self.needs_fix(&ty) {
                continue;
            }
            waiting.extend(self.fix_parts(&ty).into_iter().map(|part| fix_type(&part)));
            fixed.push(ty);
        }
        for ty in &fixed {
            self.w.line(&format!("static void {}(vela__rt__copy* c, void* at);", copy_helper("fix", ty)));
        }
        for ty in &fixed {
            if matches!(ty, Type::Map(..)) {
                self.gen_fix_table(ty);
            }
            self.gen_fix(ty);
        }
        for ty in copied {
            if !self.needs_fix(&fix_type(ty)) {
                continue;
            }
            let ctype = c_type(ty);
            self.w.open(&format!("static {0} {1}(vela__rt__region* r, {0} value)", ctype, copy_helper("copied", ty)));
            self.w.line("vela__rt__copy c = { .r = r };");
            self.w.line(&format!("{}(&c, &value);", copy_helper("fix", &fix_type(ty))));
            self.w.line("vela__rt__copy_finish(&c);");
            self.w.line("return value;");
            self.w.close();
        }
    }

    /// Whether `new [T](xs)` making a `slice` of another copies at every
    /// depth, as `copy()` does: where its items hold text, a slice, a map or a
    /// pointer, so the new slice shares nothing with the one it is made of.
    fn copies_deep(&self, slice: &Type) -> bool {
        self.needs_fix(&fix_type(item_type(slice)))
    }

    /// Whether a value of type `ty` holds anything in a region that a copy
    /// replaces: text, a slice, or a pointer, directly or by value.
    fn needs_fix(&self, ty: &Type) -> bool {
        match ty {
            Type::Text(_) | Type::Ptr(..) | Type::Slice(..) | Type::Map(..) => true,
            Type::Named(name, _) => self.copied_through.contains(name),
            Type::Maybe(inner) | Type::Array(inner, _) => self.needs_fix(inner),
            Type::Or(value, error) => self.needs_fix(value) || self.needs_fix(error),
            Type::Tuple(items) => items.iter().any(|item| self.needs_fix(item)),
            _ => false,
        }
    }

    /// The types whose values a value of type `ty` holds, directly or behind
    /// a pointer or slice, which its fix may need to fix too.
    fn fix_parts(&self, ty: &Type) -> Vec<Type> {
        match ty {
            Type::Ptr(inner, _) | Type::Slice(inner, _) | Type::Maybe(inner) | Type::Array(inner, _) => vec![(**inner).clone()],
            Type::Or(value, error) | Type::Map(value, error, _) => vec![(**value).clone(), (**error).clone()],
            Type::Tuple(items) => items.clone(),
            Type::Named(name, _) => match (self.shapes.get(name), self.kinds.get(name)) {
                (Some(shape), _) => shape.fields.iter().map(|f| f.ty.without_regions()).collect(),
                (_, Some(kind)) => kind.variants.iter().flat_map(|v| &v.payload).map(|f| f.ty.without_regions()).collect(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    /// The fix of a map's header just copied into the copy's region: its
    /// entries up to the last one used, and its slots, copied, and each key and
    /// value kept copied in turn. Its room ends at its entries, so the next key
    /// added remakes the table in the new region (see `gen_map_helpers`).
    fn gen_fix_table(&mut self, ty: &Type) {
        let Type::Map(key, value, _) = ty else { return };
        let (header, entry) = (compound_name(ty), map_entry(ty));
        self.w.open(&format!("static void {}(vela__rt__copy* c, void* at)", copy_helper("fixtable", ty)));
        self.w.line(&format!("{}* m = at;", header));
        self.w.line("m->region = c->r;");
        self.w.line(&format!("m->entries = vela__rt__copy_items(c, m->entries, m->used, sizeof({0}), _Alignof({0}));", entry));
        self.w.line("m->slots = vela__rt__copy_items(c, m->slots, (int64_t)(m->mask + 1), sizeof(uint32_t), _Alignof(uint32_t));");
        self.w.line("m->cap = m->used;");
        let fixes: Vec<(&str, String)> = [("key", &**key), ("value", &**value)]
            .into_iter()
            .filter(|(_, ty)| self.needs_fix(&fix_type(ty)))
            .map(|(member, ty)| (member, copy_helper("fix", &fix_type(ty))))
            .collect();
        if !fixes.is_empty() {
            self.w.open("for (int64_t i = 0; i < m->used; i++)");
            self.w.open("if (m->entries[i].hash != 0)");
            for (member, fix) in fixes {
                self.w.line(&format!("{}(c, &m->entries[i].{});", fix, member));
            }
            self.w.close();
            self.w.close();
        }
        self.w.close();
    }

    /// `vela__fix__K` for `ty`, which `needs_fix`.
    fn gen_fix(&mut self, ty: &Type) {
        let ctype = c_type(ty);
        self.w.open(&format!("static void {}(vela__rt__copy* c, void* at)", copy_helper("fix", ty)));
        self.w.line(&format!("{0}* v = at;", ctype));
        match ty {
            Type::Text(_) => self.w.line("*v = vela__rt__copy_text(c, *v);"),
            Type::Ptr(inner, _) => {
                let fix = if self.needs_fix(&fix_type(inner)) { copy_helper("fix", &fix_type(inner)) } else { "NULL".to_string() };
                let pointee = c_type(inner);
                self.w.line(&format!("*v = vela__rt__copy_pointer(c, *v, sizeof({0}), _Alignof({0}), {1});", pointee, fix));
            }
            Type::Slice(inner, _) => {
                let items = c_type(inner);
                self.w.line(&format!("v->items = vela__rt__copy_items(c, v->items, v->len, sizeof({0}), _Alignof({0}));", items));
                self.w.line("v->room = NULL;");
                if self.needs_fix(&fix_type(inner)) {
                    self.w.open("for (int64_t i = 0; i < v->len; i++)");
                    self.w.line(&format!("{}(c, &v->items[i]);", copy_helper("fix", &fix_type(inner))));
                    self.w.close();
                }
            }
            Type::Array(inner, count) => {
                self.w.open(&format!("for (int64_t i = 0; i < INT64_C({}); i++)", count));
                self.w.line(&format!("{}(c, &v->items[i]);", copy_helper("fix", &fix_type(inner))));
                self.w.close();
            }
            // The header is copied once however many values point to it, so
            // two names for one map stay one map; its table is copied after.
            Type::Map(..) => {
                let header = compound_name(ty);
                self.w.line(&format!("*v = vela__rt__copy_pointer(c, *v, sizeof({0}), _Alignof({0}), {1});", header, copy_helper("fixtable", ty)));
            }
            Type::Maybe(inner) => {
                self.w.open("if (v->has)");
                self.w.line(&format!("{}(c, &v->value);", copy_helper("fix", &fix_type(inner))));
                self.w.close();
            }
            Type::Or(value, error) => {
                let fix_value = **value != Type::Nothing && self.needs_fix(value);
                if fix_value {
                    self.w.open("if (v->ok)");
                    self.w.line(&format!("{}(c, &v->value);", copy_helper("fix", &fix_type(value))));
                }
                if self.needs_fix(error) {
                    if fix_value {
                        self.w.reopen("else");
                    } else {
                        self.w.open("if (!v->ok)");
                    }
                    self.w.line(&format!("{}(c, &v->error);", copy_helper("fix", &fix_type(error))));
                }
                self.w.close();
            }
            Type::Tuple(items) => {
                let fixed: Vec<(usize, &Type)> = items.iter().enumerate().filter(|(_, item)| self.needs_fix(item)).collect();
                for (i, item) in fixed {
                    self.w.line(&format!("{}(c, &v->{});", copy_helper("fix", &fix_type(item)), tuple_member(i)));
                }
            }
            Type::Named(name, _) => {
                if let Some(shape) = self.shapes.get(name).cloned() {
                    let fields: Vec<&Field> = shape.fields.iter().filter(|f| self.needs_fix(&fix_type(&f.ty))).collect();
                    for field in fields {
                        self.w.line(&format!("{}(c, &v->{});", copy_helper("fix", &fix_type(&field.ty)), c_ident(&field.name)));
                    }
                } else if let Some(kind) = self.kinds.get(name).cloned() {
                    self.w.open("switch (v->tag)");
                    for variant in &kind.variants {
                        let fields: Vec<&Field> = variant.payload.iter().filter(|f| self.needs_fix(&fix_type(&f.ty))).collect();
                        if fields.is_empty() {
                            continue;
                        }
                        self.w.open(&format!("case {}:", tag_value(name, &variant.tag)));
                        for field in fields {
                            self.w.line(&format!("{}(c, &v->data.{}.{});", copy_helper("fix", &fix_type(&field.ty)), c_ident(&variant.tag), c_ident(&field.name)));
                        }
                        self.w.line("break;");
                        self.w.close();
                    }
                    self.w.line("default:");
                    self.w.line("    break;");
                    self.w.close();
                }
            }
            _ => {}
        }
        self.w.close();
    }

    /// Reading a number of type `ty` from text: the number, or the reason the
    /// text was none, as a `ParseError` (see `ast::parse_error_kind`). The
    /// runtime reads the text and says which; this turns that into the
    /// `ty ~ ParseError` the program holds.
    fn gen_read_number(&mut self, ty: &Type) {
        let (from, value) = (format!("vela__rt__{}_of_text", type_word(ty)), c_type(ty));
        let or = Type::Or(Box::new(ty.clone()), Box::new(Type::Named(PARSE_ERROR.to_string(), None)));
        let (or_type, tag) = (c_type(&or), tag_type(PARSE_ERROR));
        self.w.open(&format!("static {} {}(vela__rt__text text)", or_type, read_helper(ty)));
        self.w.line(&format!("{} value = 0;", value));
        self.w.line(&format!("int read = {}(text, &value);", from));
        self.w.open("if (read == VELA__RT__READ_OK)");
        self.w.line(&format!("return ({}){{ .ok = true, .value = value }};", or_type));
        self.w.close();
        self.w.line(&format!(
            "{} why = read == VELA__RT__READ_TOO_LARGE ? {} : {};",
            tag,
            tag_value(PARSE_ERROR, TOO_LARGE),
            tag_value(PARSE_ERROR, NOT_A_NUMBER)
        ));
        self.w.line(&format!("return ({}){{ .ok = false, .error = {{ .tag = why }} }};", or_type));
        self.w.close();
    }

    /// Reading a number of type `ty` from input: the number, the input having
    /// ended, or the line being no number, as an `InputError` (see
    /// `ast::input_error_kind`). The runtime shows the prompt, reads the line,
    /// and frees it, so nothing here needs a region.
    fn gen_scan_number(&mut self, ty: &Type) {
        let (from, value) = (format!("vela__rt__{}_of_input", type_word(ty)), c_type(ty));
        let or = Type::Or(Box::new(ty.clone()), Box::new(Type::Named(INPUT_ERROR.to_string(), None)));
        let or_type = c_type(&or);
        self.w.open(&format!("static {} {}(vela__rt__text prompt)", or_type, scan_helper(ty)));
        self.w.line(&format!("{} value = 0;", value));
        self.w.line(&format!("int read = {}(prompt, &value);", from));
        self.w.open("if (read == VELA__RT__READ_OK)");
        self.w.line(&format!("return ({}){{ .ok = true, .value = value }};", or_type));
        self.w.close();
        self.w.open("if (read == VELA__RT__READ_ENDED)");
        self.w.line(&format!(
            "return ({}){{ .ok = false, .error = {{ .tag = {} }} }};",
            or_type,
            tag_value(INPUT_ERROR, ENDED)
        ));
        self.w.close();
        // The line was no number, and the reason is the one reading text gives.
        self.w.line(&format!(
            "{} why = read == VELA__RT__READ_TOO_LARGE ? {} : {};",
            tag_type(PARSE_ERROR),
            tag_value(PARSE_ERROR, TOO_LARGE),
            tag_value(PARSE_ERROR, NOT_A_NUMBER)
        ));
        self.w.line(&format!(
            "return ({}){{ .ok = false, .error = {{ .tag = {}, .data = {{ .{} = {{ .{} = ({}){{ .tag = why }} }} }} }} }};",
            or_type,
            tag_value(INPUT_ERROR, NO_NUMBER),
            c_ident(NO_NUMBER),
            c_ident(WHY),
            mangle(PARSE_ERROR)
        ));
        self.w.close();
    }

    /// The functions that write values as text, for `show` and `str` (see
    /// `UsedTypes::note_write`): each type's `write` puts a value in a writer,
    /// and its `text` makes one text of it in a region. All are declared before
    /// any is defined, since an array's writes its items'.
    fn gen_writers(&mut self, used: &UsedTypes) -> Result<()> {
        for ty in &used.writes {
            self.w.line(&format!("static void {}(vela__rt__writer* w, {} v);", write_helper(ty), written_c_type(ty)));
        }
        for ty in &used.texts {
            self.w.line(&format!("static vela__rt__text {}(vela__rt__region* r, {} v);", text_helper(ty), written_c_type(ty)));
        }
        for ty in &used.writes {
            self.gen_writer(ty)?;
        }
        for ty in &used.texts {
            self.w.open(&format!("static vela__rt__text {}(vela__rt__region* r, {} v)", text_helper(ty), written_c_type(ty)));
            match ty {
                // A shape's or kind's own `str` makes the text itself.
                Type::Named(owner, _) if self.has_own_str(owner) => self.w.line(&format!("return {};", self.own_str_call(owner, "r")?)),
                _ => {
                    self.w.line("vela__rt__writer w = VELA__RT__BUILDER;");
                    match ty {
                        Type::Ptr(..) => self.w.line("vela__rt__put_address(&w, v);"),
                        Type::Task(_) | Type::Closure(_) => self.w.line(&format!("(void)v; VELA__RT__PUT(&w, \"{}\");", self.task_text(ty))),
                        _ => self.w.line(&format!("{}(&w, v);", write_helper(ty))),
                    }
                    self.w.line("return vela__rt__built(&w, r);");
                }
            }
            self.w.close();
        }
        if !used.writes.is_empty() || !used.texts.is_empty() {
            self.w.blank();
        }
        Ok(())
    }

    /// Whether the shape or kind `owner` gives its own method `str`.
    fn has_own_str(&self, owner: &str) -> bool {
        self.str_task(owner).is_some()
    }

    /// The task the method `str` of the shape or kind `owner` is (see
    /// `str_task`).
    fn str_task(&self, owner: &str) -> Option<String> {
        let given = match (self.shapes.get(owner), self.kinds.get(owner)) {
            (Some(shape), _) => shape.given.as_slice(),
            (None, Some(kind)) => kind.given.as_slice(),
            (None, None) => &[],
        };
        str_task(owner, given, |task| self.hidden_params.contains_key(task))
    }

    /// A shape's or kind's name as its default text writes it: its own name,
    /// wherever it is read, and a shape made for types written as given them,
    /// `Box<int>` (see `ShapeDef::given`).
    fn written_name(&self, name: &str) -> String {
        let given = match (self.shapes.get(name), self.kinds.get(name)) {
            (Some(shape), _) => shape.given.as_slice(),
            (None, Some(kind)) => kind.given.as_slice(),
            (None, None) => &[],
        };
        match given {
            given if !given.is_empty() => {
                let base = name.split("__of__").next().unwrap_or(name);
                let given: Vec<String> =
                    given.iter().map(|ty| ty.without_regions().map_names(&|inner| self.written_name(inner)).to_string()).collect();
                format!("{}<{}>", unqualified(base), given.join(", "))
            }
            _ => unqualified(name).to_string(),
        }
    }

    /// The call of the method `str` a shape or kind `owner` gives, on the value
    /// `v`, into the region `region`.
    fn own_str_call(&self, owner: &str, region: &str) -> Result<String> {
        let method = self.str_task(owner).ok_or_else(|| Error::internal(format!("no method 'str' of '{}' to call", owner)))?;
        let value = if self.hidden_of(&method).first().copied().unwrap_or(false) { "&v" } else { "v" };
        Ok(format!("{}({}, {})", mangle(&method), value, region))
    }

    /// `write` for a value of type `ty`: `[1, 2]` for an array or slice,
    /// `(1, "a")` for a tuple, `none` or `{ 4 }` for a `maybe`, and what its own
    /// `str` gives for a shape or kind, made in a region of its own.
    fn gen_writer(&mut self, ty: &Type) -> Result<()> {
        self.w.open(&format!("static void {}(vela__rt__writer* w, {} v)", write_helper(ty), written_c_type(ty)));
        match ty {
            Type::Array(items, _) | Type::Slice(items, _) => {
                let count = match ty {
                    Type::Array(_, count) => count.to_string(),
                    _ => "v.len".to_string(),
                };
                self.w.line("VELA__RT__PUT(w, \"[\");");
                self.w.open(&format!("for (int64_t i = 0; i < {}; i++)", count));
                self.w.open("if (i > 0)");
                self.w.line("VELA__RT__PUT(w, \", \");");
                self.w.close();
                self.w.line(&self.put_item(items, "v.items[i]"));
                self.w.close();
                self.w.line("VELA__RT__PUT(w, \"]\");");
            }
            // `["a": 1, "b": 2]`, in the order the keys were added; `[:]` empty.
            Type::Map(key, value, _) => {
                self.w.open("if (v->live == 0)");
                self.w.line("VELA__RT__PUT(w, \"[:]\");");
                self.w.line("return;");
                self.w.close();
                self.w.line("VELA__RT__PUT(w, \"[\");");
                self.w.line("bool first = true;");
                self.w.open("for (int64_t i = 0; i < v->used; i++)");
                self.w.open("if (v->entries[i].hash == 0)");
                self.w.line("continue;");
                self.w.close();
                self.w.open("if (!first)");
                self.w.line("VELA__RT__PUT(w, \", \");");
                self.w.close();
                self.w.line("first = false;");
                self.w.line(&self.put_item(key, "v->entries[i].key"));
                self.w.line("VELA__RT__PUT(w, \": \");");
                self.w.line(&self.put_item(value, "v->entries[i].value"));
                self.w.close();
                self.w.line("VELA__RT__PUT(w, \"]\");");
            }
            Type::Tuple(items) => {
                self.w.line("VELA__RT__PUT(w, \"(\");");
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        self.w.line("VELA__RT__PUT(w, \", \");");
                    }
                    self.w.line(&self.put_item(item, &format!("v.{}", tuple_member(i))));
                }
                self.w.line("VELA__RT__PUT(w, \")\");");
            }
            Type::Maybe(inner) => {
                // A `maybe` of a pointer is the pointer, and `none` is NULL.
                let (empty, value) = if is_pointer(inner) { ("v == NULL", "v".to_string()) } else { ("!v.has", "v.value".to_string()) };
                self.w.open(&format!("if ({})", empty));
                self.w.line("VELA__RT__PUT(w, \"none\");");
                self.w.line("return;");
                self.w.close();
                self.w.line("VELA__RT__PUT(w, \"{ \");");
                self.w.line(&self.put_item(inner, &value));
                self.w.line("VELA__RT__PUT(w, \" }\");");
            }
            Type::Named(owner, _) if self.has_own_str(owner) => {
                self.w.line("vela__rt__region r[1] = { VELA__RT__REGION_EMPTY };");
                self.w.line(&format!("vela__rt__put_text(w, {});", self.own_str_call(owner, "r")?));
                self.w.line("vela__rt__region_free(r);");
            }
            // Without one, a shape is written as a literal of its fields that
            // any file reads, `Point { x: 1, y: 2 }`.
            Type::Named(name, _) if self.shapes.contains_key(name) => {
                let fields: Vec<Field> = self.shapes.get(name).map(|shape| shape.fields.iter().filter(|field| !field.private).cloned().collect()).unwrap_or_default();
                let written = self.written_name(name);
                if fields.is_empty() {
                    self.w.line("(void)v;");
                    self.w.line(&format!("VELA__RT__PUT(w, \"{} {{}}\");", written));
                } else {
                    for (i, field) in fields.iter().enumerate() {
                        let before = if i == 0 { format!("{} {{ ", written) } else { ", ".to_string() };
                        self.w.line(&format!("VELA__RT__PUT(w, \"{}{}: \");", before, field.name));
                        self.w.line(&self.put_item(&field.ty, &format!("v.{}", c_ident(&field.name))));
                    }
                    self.w.line("VELA__RT__PUT(w, \" }\");");
                }
            }
            // And a kind as the variant it holds, with the values it carries as
            // a call gives them, `Circle(1.5)`.
            Type::Named(name, _) if self.kinds.contains_key(name) => {
                let Some(kind) = self.kinds.get(name).cloned() else { return Err(Error::internal(format!("no kind '{}' to write", name))) };
                self.w.open("switch (v.tag)");
                for variant in &kind.variants {
                    self.w.line(&format!("case {}:", tag_value(&kind.name, &variant.tag)));
                    self.w.line(&format!("    VELA__RT__PUT(w, \"{}{}\");", variant.tag, if variant.payload.is_empty() { "" } else { "(" }));
                    for (i, field) in variant.payload.iter().enumerate() {
                        if i > 0 {
                            self.w.line("    VELA__RT__PUT(w, \", \");");
                        }
                        self.w.line(&format!("    {}", self.put_item(&field.ty, &format!("v.data.{}.{}", c_ident(&variant.tag), c_ident(&field.name)))));
                    }
                    if !variant.payload.is_empty() {
                        self.w.line("    VELA__RT__PUT(w, \")\");");
                    }
                    self.w.line("    break;");
                }
                self.w.line("default:");
                self.w.line("    break;");
                self.w.close();
            }
            other => return Err(Error::internal(format!("writing a value of type {} as text", other))),
        }
        self.w.close();
        Ok(())
    }

    fn gen_task(&mut self, t: &TaskDef) -> Result<()> {
        self.ret = t.ret.clone();
        let alters = self.altering.contains(&t.name);
        let hidden = self.hidden_of(&t.name).to_vec();
        self.hidden = t.params.iter().zip(&hidden).filter(|(_, hidden)| **hidden).map(|(p, _)| p.name.clone()).collect();
        self.pointers = t.params.iter().filter(|p| is_pointer_param(p, alters)).map(|p| p.name.clone()).collect();
        self.pointers.extend(self.hidden.iter().cloned());
        self.regions.clear();
        self.loops.clear();
        self.results = t.result_names().map(|(name, _)| name.clone()).collect();
        self.task = shown_name(&t.name);
        self.w.open(&signature(t, alters, &hidden, &|name| self.shapes.contains_key(name)));
        // A native task's body is the runtime function it stands for, given
        // its parameters as they are (see `ast::NATIVE`).
        if t.native {
            let function = crate::library::native(&t.name).ok_or_else(|| Error::internal_at(t.span, format!("the runtime has no body for the native task '{}'", t.name)))?;
            let args: Vec<String> = t.params.iter().map(|param| c_ident(&param.name)).collect();
            let call = format!("{}({})", function, args.join(", "));
            self.w.line(&if t.ret == Type::Nothing { format!("{};", call) } else { format!("return {};", call) });
            self.w.close();
            return Ok(());
        }
        // What a lambda keeps: its region, and a pointer to each copy, which
        // it reads and changes where it is kept.
        if t.kept.is_some() {
            let kept = kept_name(&t.name);
            let fields: Vec<Field> = self.shapes.get(&kept).map(|shape| shape.fields.clone()).unwrap_or_default();
            self.w.line(&format!("{0}* vela__block = {1};", mangle(&kept), KEPT_PARAM));
            for field in &fields {
                if field.ty == Type::Region {
                    self.w.line(&format!("vela__rt__region* {0} = vela__block->{0};", c_ident(&field.name)));
                    self.w.line(&format!("(void){};", c_ident(&field.name)));
                } else if field.passing.is_ref() {
                    // A place the lambda keeps: the variable itself.
                    self.w.line(&format!("{0} {1} = vela__block->{1};", c_type(&field.ty), c_ident(&field.name)));
                    self.w.line(&format!("(void){};", c_ident(&field.name)));
                    self.pointers.insert(field.name.clone());
                } else {
                    self.w.line(&format!("{0}* {1} = &vela__block->{1};", c_type(&field.ty), c_ident(&field.name)));
                    self.w.line(&format!("(void){};", c_ident(&field.name)));
                    self.pointers.insert(field.name.clone());
                }
            }
        }
        for (param, hidden) in t.params.iter().zip(hidden.iter().copied().chain(std::iter::repeat(false))) {
            if !owns_copy(t, param, alters, hidden) {
                continue;
            }
            self.w.line(&format!("{} {} = {};", c_type(&param.ty), c_ident(&param.name), given_name(&param.name)));
        }
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
        if is_startup(&t.name) {
            self.gen_startup(t)?;
        } else {
            self.gen_block(&t.body)?;
        }
        match &t.ret {
            Type::Nothing => self.w.line("return;"),
            Type::Or(value, _) if **value == Type::Nothing => {
                self.w.line(&format!("return {};", success(&t.ret)));
            }
            // Reaching the end of a task sends its results as they are.
            _ if !self.results.is_empty() => {
                let results = self.current_results();
                self.w.line(&format!("return {};", results));
            }
            _ => self.w.line(&format!("vela__rt__missing_send(\"{}\");", escape_c_string(&self.task))),
        }
        self.w.close();
        self.results.clear();
        Ok(())
    }

    /// The task running a file's startup decorators, `t`: each call in turn,
    /// and one that can fail tested: a failure stops the program, or, for one
    /// written `$d?` (see `TaskDef::passes`), is a notice and the program goes
    /// on. Either way standard error says which decorator failed on which task,
    /// and why, unless the program writes no notices and goes on.
    fn gen_startup(&mut self, t: &TaskDef) -> Result<()> {
        self.enter_block();
        for (stmt, passes) in t.body.iter().zip(t.passes.iter().chain(std::iter::repeat(&false))) {
            let StmtKind::Expr(call) = &stmt.kind else { return Err(Error::internal_at(stmt.span, "a startup decorator that is no call")) };
            // A runtime error in its values names the decorator and its task,
            // not the task the decorators are gathered into.
            if let ExprKind::Call { name, args, .. } = &call.kind {
                let decorated = match args.first().map(|arg| &arg.kind) {
                    Some(ExprKind::Var(task) | ExprKind::TaskValue(task)) => notice_name(task),
                    _ => String::new(),
                };
                self.place = Some(format!("decorator '{}' on task '{}'", notice_name(name), decorated));
            }
            let (Some(Type::Or(_, error)), ExprKind::Call { name, args, .. }) = (&call.ty, &call.kind) else {
                self.gen_stmt(stmt)?;
                self.place = None;
                continue;
            };
            let result = self.gen_expr(call)?;
            self.place = None;
            if *passes && self.quiet {
                self.w.line(&format!("(void)({});", result));
                continue;
            }
            let decorated = match args.first().map(|arg| &arg.kind) {
                Some(ExprKind::Var(task) | ExprKind::TaskValue(task)) => notice_name(task),
                _ => String::new(),
            };
            let (failed, notice) = (self.fresh_temp("failed"), self.fresh_temp("notice"));
            self.w.open("");
            self.w.line(&format!("{} {} = {};", c_type(call.ty()?), failed, result));
            self.w.open(&format!("if (!{}.ok)", failed));
            self.w.line(&format!("vela__rt__region {}[1] = {{ VELA__RT__REGION_EMPTY }};", notice));
            let why = format!("{}({}, {}.error)", text_function(error), notice, failed);
            self.w.line(&format!(
                "vela__rt__decorator_failed(\"{}\", \"{}\", {}, {});",
                escape_c_string(&notice_name(name)),
                escape_c_string(&decorated),
                why,
                if *passes { "false" } else { "true" }
            ));
            self.w.line(&format!("vela__rt__region_free({});", notice));
            self.w.close();
            self.w.close();
        }
        self.leave_block(&t.body);
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

    /// A block that gives a value, assigned to `into`: its statements, then its
    /// last line, which is the value. The assignment happens before the block's
    /// regions are freed, as a plan B's does. Where the last line is a `when` or
    /// a `match`, its own branches assign it; where every path sends, nothing is
    /// assigned and the block is generated as it is.
    fn gen_value_block(&mut self, body: &[Stmt], into: &str) -> Result<()> {
        let Some((last, rest)) = body.split_last() else { return self.gen_block(body) };
        if !matches!(last.kind, StmtKind::Expr(_)) && !check::gives_value(last) {
            return self.gen_block(body);
        }
        self.enter_block();
        self.gen_statements(rest, body)?;
        match &last.kind {
            StmtKind::Expr(value) => {
                let value = self.gen_expr(value)?;
                self.w.line(&format!("{} = {};", into, value));
            }
            _ => self.gen_branching_value(last, Some(into))?,
        }
        self.leave_block(body);
        Ok(())
    }

    /// A `match` or a `when` used as a value: a temporary holds it, every branch
    /// assigns it, and the temporary is what the expression reads. C has no
    /// statement that is also a value, so this is the shape `gen_plan_b` uses.
    fn gen_choice(&mut self, e: &Expr, stmt: &Stmt) -> Result<String> {
        let ty = e.ty()?.clone();
        let result = self.fresh_temp("chosen");
        // Zeroed: a branch that sends assigns nothing, and C compilers warn
        // about a value that may be read before it is written.
        self.w.line(&format!("{} {} = {{ 0 }};", c_type(&ty), result));
        self.gen_branching_value(stmt, Some(&result))?;
        Ok(result)
    }

    /// The statement a `match` or a `when` used as a value is, generated with
    /// each of its blocks giving the value into `into`.
    fn gen_branching_value(&mut self, stmt: &Stmt, into: Option<&str>) -> Result<()> {
        match &stmt.kind {
            StmtKind::When(arms, else_b) => self.gen_when(arms, else_b.as_deref(), into),
            StmtKind::Match(subject, cases) => self.gen_match(subject, cases, into),
            _ => Err(Error::internal_at(stmt.span, "a statement that is not a 'when' or a 'match' used as a value")),
        }
    }

    /// Starts a loop, named `name` (which `outer =>` gave it) or not: what a
    /// `break` or a `continue` in it goes to. Called once what is found before
    /// the loop starts is found, since a `break` there is not this loop's.
    fn enter_loop(&mut self, (name, giving): (Option<String>, Option<Giving>), has_finally: bool) {
        let (stop, next) = (self.fresh_temp("stop"), self.fresh_temp("next"));
        let ran = if giving.as_ref().is_some_and(|g| g.map_acc.is_some()) {
            let r = self.fresh_temp("ran");
            self.w.line(&format!("bool {} = false;", r));
            Some(r)
        } else {
            None
        };
        let pass_ok = if has_finally {
            let p = self.fresh_temp("pass_ok");
            self.w.line(&format!("bool {} = false;", p));
            Some(p)
        } else {
            None
        };
        if let Some(giving) = &giving
            && let Some(map) = &giving.map_acc
        {
            match &map.coll_ty {
                Type::Array(..) => {
                    self.w.line(&format!("{} {} = {{ 0 }};", c_type(&map.coll_ty), map.acc_name));
                    self.w.line(&format!("int64_t {} = 0;", map.idx_name));
                }
                Type::Slice(..) => {
                    self.w.line(&format!("{} {} = (({}){{ .items = vela__rt__no_items(), .len = 0 }});", c_type(&map.coll_ty), map.acc_name, c_type(&map.coll_ty)));
                }
                _ => {}
            }
        }
        self.loops.push(LoopLabels { name, giving, depth: self.regions.len(), stop, next, ran, pass_ok, stopped: false, continued: false });
    }

    /// The name and what it gives of the loop about to be generated, taken
    /// before what is found before it starts, which may hold loops of its own.
    fn take_loop(&mut self) -> (Option<String>, Option<Giving>) {
        (self.loop_name.take(), self.loop_giving.take())
    }

    /// The body of a pass of the innermost loop: one that gives a value
    /// gives it on its last line, or, giving nothing, ends the pass with a
    /// success.
    fn gen_pass(&mut self, body: &[Stmt]) -> Result<()> {
        let innermost = self.loops.last().cloned();
        if let Some(ran) = innermost.as_ref().and_then(|l| l.ran.as_ref()) {
            self.w.line(&format!("{} = true;", ran));
        }
        if let Some(pass_ok) = innermost.as_ref().and_then(|l| l.pass_ok.as_ref()) {
            self.w.line(&format!("{} = false;", pass_ok));
        }
        let res = match innermost.as_ref().and_then(|l| l.giving.as_ref()) {
            Some(giving) if giving.gives => {
                if let Some(map) = &giving.map_acc {
                    let item_temp = self.fresh_temp("item");
                    self.w.line(&format!("{} {};", c_type(&map.item_ty), item_temp));
                    self.gen_value_block(body, &item_temp)?;
                    match &map.coll_ty {
                        Type::Array(..) => {
                            self.w.line(&format!("{}.items[{}++] = {};", map.acc_name, map.idx_name, item_temp));
                        }
                        Type::Slice(..) => {
                            let reg = map.region.as_deref().unwrap_or("r");
                            self.w.line(&format!("{}({}, &{}, {});", item_helper("add", &map.coll_ty), c_ident(reg), map.acc_name, item_temp));
                        }
                        _ => {}
                    }
                    Ok(())
                } else {
                    self.gen_block(body)
                }
            }
            Some(giving) => {
                self.gen_block(body)?;
                self.w.line(&format!("{} = (({}){{ .ok = true }});", giving.result, c_type(&giving.ty)));
                Ok(())
            }
            None => self.gen_block(body),
        };
        res?;
        if let Some(pass_ok) = innermost.as_ref().and_then(|l| l.pass_ok.as_ref()) {
            self.w.line(&format!("{} = true;", pass_ok));
        }
        Ok(())
    }

    fn finish_loop(&mut self, finally: Option<&[Stmt]>) -> Result<()> {
        if let Some(finally) = finally {
            self.gen_finally(finally)?;
        } else if let Some(innermost) = self.loops.last()
            && let Some(giving) = &innermost.giving
            && let Some(map) = &giving.map_acc
            && let Some(ran) = &innermost.ran
        {
            self.w.open(&format!("if ({})", ran));
            self.w.line(&format!("{} = (({}){{ .ok = true, .value = {} }});", giving.result, c_type(&giving.ty), map.acc_name));
            self.w.close();
        }
        Ok(())
    }

    fn gen_finally(&mut self, finally: &[Stmt]) -> Result<()> {
        let innermost = self.loops.last().cloned();
        let giving = innermost.as_ref().and_then(|l| l.giving.clone());
        let pass_ok = innermost.as_ref().and_then(|l| l.pass_ok.clone());
        if let Some(pass_ok) = &pass_ok {
            self.w.open(&format!("if ({})", pass_ok));
        }
        match giving {
            Some(giving) if giving.gives => self.gen_value_block(finally, &giving.result)?,
            Some(giving) => {
                self.gen_block(finally)?;
                self.w.line(&format!("{} = (({}){{ .ok = true }});", giving.result, c_type(&giving.ty)));
            }
            None => self.gen_block(finally)?,
        }
        if pass_ok.is_some() {
            self.w.close();
        }
        Ok(())
    }

    /// A loop that gives a value, the expression `e`: a temporary holds it,
    /// the failure `Skipped` until a pass gives one.
    fn gen_loop_value(&mut self, e: &Expr, value: &ValueLoop) -> Result<String> {
        let ty = e.ty()?.clone();
        let result = self.fresh_temp("looped");
        let skipped = self.loop_failure(&ty, "Skipped", None, e.span)?;
        self.w.line(&format!("{} {} = {};", c_type(&ty), result, skipped));
        self.loop_name = value.name.as_ref().map(|(name, _)| name.clone());
        let map_acc = if value.finally().is_none() && value.gives {
            if let Type::Or(coll_ty, _) = &ty {
                let (item_ty, region) = match coll_ty.as_ref() {
                    Type::Array(item, _) => ((**item).clone(), None),
                    Type::Slice(item, region) => ((**item).clone(), region.clone()),
                    _ => (Type::Nothing, None),
                };
                Some(MapAcc {
                    acc_name: self.fresh_temp("acc"),
                    coll_ty: (**coll_ty).clone(),
                    item_ty,
                    region,
                    idx_name: self.fresh_temp("idx"),
                })
            } else {
                None
            }
        } else {
            None
        };
        self.loop_giving = Some(Giving { result: result.clone(), ty, gives: value.gives, map_acc });
        self.gen_stmt(&value.stmt)?;
        Ok(result)
    }

    /// The failure `tag` of `LoopResult` as a value of `ty`, the type of a loop
    /// that gives one: `Broke` holding `value`, or `none`.
    fn loop_failure(&mut self, ty: &Type, tag: &str, value: Option<&Expr>, span: Span) -> Result<String> {
        let Type::Or(_, result) = ty else { return Err(Error::internal_at(span, format!("a loop that gives {}", ty))) };
        let Type::Named(kind, _) = result.as_ref() else { return Err(Error::internal_at(span, format!("a loop that fails with {}", result))) };
        let typed = |kind: ExprKind, ty: Type| {
            let mut e = Expr::new(kind, span);
            e.ty = Some(ty);
            e
        };
        let args = if tag == "Broke" {
            let payload = self
                .kinds
                .get(kind)
                .and_then(|made| made.variants.iter().find(|variant| variant.tag == tag))
                .and_then(|variant| variant.payload.first())
                .map(|field| field.ty.clone())
                .ok_or_else(|| Error::internal_at(span, "a LoopResult with no Broke"))?;
            let given = match value {
                Some(value) if matches!(value.ty, Some(Type::Maybe(_))) => value.clone(),
                Some(value) => typed(ExprKind::Wrap(Box::new(value.clone())), payload),
                None => typed(ExprKind::Wrap(Box::new(typed(ExprKind::NoneLit, Type::None))), payload),
            };
            vec![given]
        } else {
            Vec::new()
        };
        let construct = typed(ExprKind::Construct { kind: kind.clone(), tag: tag.to_string(), args, types: Vec::new() }, (**result).clone());
        let failure = typed(ExprKind::Fail(Box::new(construct)), Type::Fail(result.clone()));
        self.gen_expr(&typed(ExprKind::Wrap(Box::new(failure)), ty.clone()))
    }

    /// Inside an open `while (1)`: the test of `cond`, which stops the loop
    /// when it fails, and `body` where it holds, the names its patterns bind
    /// seen there.
    fn gen_tested_pass(&mut self, cond: &Condition, body: &Block) -> Result<()> {
        if !cond.single() {
            let matched = self.fresh_temp("matched");
            self.w.line(&format!("bool {} = false;", matched));
            self.gen_parts(cond, body, &mut |g| {
                g.w.line(&format!("{} = true;", matched));
                g.gen_pass(body)
            })?;
            self.w.open(&format!("if (!{})", matched));
            self.w.line("break;");
            self.w.close();
            return Ok(());
        }
        match &cond.pattern {
            None => {
                let c = self.gen_expr(&cond.value)?;
                self.w.open(&format!("if (!{})", parenthesized(&c)));
                self.w.line("break;");
                self.w.close();
                self.gen_pass(body)
            }
            Some(pattern) => {
                let subject = self.gen_subject(&cond.value)?;
                let test = self.pattern_test(cond.value.ty()?, &subject, pattern)?;
                self.w.open(&format!("if (!({}))", test));
                self.w.line("break;");
                self.w.close();
                self.gen_pattern_bindings(cond.value.ty()?, &subject, pattern, body)?;
                self.gen_pass(body)
            }
        }
    }

    /// The end of a pass of the innermost loop, inside its C body after the
    /// pass's regions are freed: where a `continue` goes. A loop written out
    /// for a group's values ends several passes, each with a label of its own.
    fn end_pass(&mut self) {
        let Some(innermost) = self.loops.last_mut().filter(|innermost| innermost.continued) else { return };
        innermost.continued = false;
        let label = std::mem::take(&mut innermost.next);
        self.w.line(&format!("{}: ;", label));
        let next = self.fresh_temp("next");
        if let Some(innermost) = self.loops.last_mut() {
            innermost.next = next;
        }
    }

    /// Ends the innermost loop, after its C statement: where a `break` goes.
    fn leave_loop(&mut self) {
        if let Some(ended) = self.loops.pop().filter(|ended| ended.stopped) {
            self.w.line(&format!("{}: ;", ended.stop));
        }
    }

    /// `break` (`stop`) or `continue`, of the loop named `name` or the
    /// innermost: the regions declared inside that loop so far are freed,
    /// newest first, as a `<-` frees the task's, and a `goto` leaves the pass.
    /// In a loop that gives a value, a `break` makes it fail with `Broke`,
    /// holding `value`, and a `continue` with `EndedByContinue`, which a later
    /// pass replaces.
    fn gen_jump(&mut self, stop: bool, name: Option<&str>, value: Option<&Expr>, span: Span) -> Result<()> {
        let found = match name {
            Some(name) => self.loops.iter().rposition(|around| around.name.as_deref() == Some(name)),
            None => self.loops.len().checked_sub(1),
        };
        if let Some(giving) = found.and_then(|at| self.loops.get(at)).and_then(|target| target.giving.clone()) {
            let failure = self.loop_failure(&giving.ty, if stop { "Broke" } else { "EndedByContinue" }, value, span)?;
            self.w.line(&format!("{} = {};", giving.result, failure));
        }
        let Some(depth) = found.and_then(|at| self.loops.get(at)).map(|target| target.depth) else {
            return Err(Error::internal_at(span, "a 'break' or a 'continue' with no loop around it"));
        };
        let open: Vec<String> = self.regions.iter().skip(depth).flatten().cloned().collect();
        for region in open.iter().rev() {
            self.w.line(&format!("vela__rt__region_free({});", c_ident(region)));
        }
        let Some(target) = found.and_then(|at| self.loops.get_mut(at)) else { return Err(Error::internal_at(span, "a loop gone")) };
        let label = if stop {
            target.stopped = true;
            target.stop.clone()
        } else {
            target.continued = true;
            target.next.clone()
        };
        self.w.line(&format!("goto {};", label));
        Ok(())
    }

    /// Starts a block that may declare regions.
    fn enter_block(&mut self) {
        self.regions.push(Vec::new());
    }

    /// Ends the block `block`, freeing the regions it declared, newest first.
    /// (After a final `<-`, `break`, or `continue`, which free them themselves,
    /// nothing more runs.)
    fn leave_block(&mut self, block: &[Stmt]) {
        // A reference is read through its pointer in its own block only: a
        // block beside it may use the name for a variable of its own.
        for stmt in block {
            if let StmtKind::Binding { names, reference: true, .. } = &stmt.kind {
                for bound in names {
                    self.pointers.remove(&bound.name);
                }
            }
        }
        let declared = self.regions.pop().unwrap_or_default();
        if matches!(block.last().map(|stmt| &stmt.kind), Some(StmtKind::Send(_) | StmtKind::Break(..) | StmtKind::Continue(_))) {
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
                for bound in names.iter().flat_map(Bound::leaves).filter(|bound| bound.name != DISCARD) {
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
            // The checker makes every one the assignment it is.
            StmtKind::Step { .. } => return Err(Error::internal_at(stmt.span, "a step the checker left as it was")),
            // A change to a slice drops the item `del` gives, and a `scan` the line.
            StmtKind::Expr(Expr { kind: ExprKind::Edit { items, op, args }, span, .. }) => {
                self.gen_edit(*span, items, *op, args, true)?;
            }
            StmtKind::Expr(e @ Expr { kind: ExprKind::Scan { args, region, .. }, .. }) => {
                self.gen_scan(e, args, region.as_deref(), true)?;
            }
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
            // `flex& y = &x`: a pointer to the place, read through it; one that
            // only reads, `fixed& y = &x`, or a `bind&` of a type with no
            // contents, may name a value passed by a hidden pointer, which is
            // `const`.
            StmtKind::Binding { names, mode, reference: true, init } => {
                if let ([bound], [Expr { kind: ExprKind::Ref(place), .. }]) = (names.as_slice(), init.as_slice()) {
                    let read_only = !mode.can_change_contents(has_contents(place.ty()?, &|name| self.shapes.contains_key(name)));
                    let address = match &place.kind {
                        ExprKind::Var(_) => self.gen_receiver(place)?,
                        _ if read_only => format!("&{}", self.gen_place(place)?),
                        _ => self.gen_receiver(place)?,
                    };
                    // `T const*`, which for a pointer type is a pointer to a
                    // constant pointer, not to what it points to.
                    let constant = if read_only { " const" } else { "" };
                    self.w.line(&format!("{}{}* {} = {};", c_type(place.ty()?), constant, c_ident(&bound.name), address));
                    self.pointers.insert(bound.name.clone());
                } else {
                    return Err(Error::internal_at(stmt.span, "a reference that isn't one name given one place"));
                }
            }
            StmtKind::Binding { names, init, .. } => {
                let values = self.gen_value_list(names.len(), init, stmt.span)?;
                for (bound, value) in names.iter().zip(values) {
                    let ty = bound
                        .ty
                        .as_ref()
                        .ok_or_else(|| Error::internal_at(bound.span, format!("binding '{}' has no type", bound.name)))?;
                    if !bound.parts.is_empty() && bound.leaves().iter().all(|leaf| leaf.name == DISCARD) {
                        // `(_, _)`: nothing to declare.
                        self.drop_value(&value);
                    } else if !bound.parts.is_empty() {
                        // The tuple is kept once, and each name takes its item.
                        let tuple = self.fresh_temp("tuple");
                        self.w.line(&format!("{} {} = {};", c_type(ty), tuple, value));
                        self.gen_unpacked(bound, &tuple)?;
                    } else if bound.name == DISCARD {
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
                if let ([Target::Place(lhs)], [rhs]) = (targets.as_slice(), values.as_slice())
                    && let ExprKind::Index(_, index) = &lhs.kind
                    && !matches!(index.kind, ExprKind::Range(..))
                    && let ExprKind::Binary(_, read, by) = &rhs.kind
                    && read.span == lhs.span
                    && matches!(read.kind, ExprKind::Index(..))
                    && changes_nothing(by)
                    && !has_prelude(by)
                {
                    // A step, `m[k] += v` or `xs[i]++`, which the checker made
                    // `m[k] = m[k] + v` (the item read has the place's own
                    // position): the item is found once, its key hashed or its
                    // index checked once, and read and set through its address.
                    // What is added changes nothing, so the address stays good.
                    let place = self.gen_place(lhs)?;
                    let pointer = self.fresh_temp("place");
                    self.w.line(&format!("{}* {} = &{};", c_type(lhs.ty()?), pointer, place));
                    self.replaced = Some((&**read as *const Expr, format!("(*{})", pointer)));
                    let value = self.gen_expr(rhs);
                    self.replaced = None;
                    self.w.line(&format!("*{} = {};", pointer, value?));
                } else if let ([Target::Place(lhs)], [rhs]) = (targets.as_slice(), values.as_slice())
                    && let ExprKind::Index(map, key) = &lhs.kind
                    && let Some(map_ty @ Type::Map(..)) = &map.ty
                {
                    // `m[k] = v`: the key's value replaced, or the key added.
                    let helper = item_helper("map_set", &map_ty.without_regions());
                    let [map, key, value] = <[String; 3]>::try_from(self.gen_operands(&[map, key, rhs])?)
                        .map_err(|_| Error::internal_at(stmt.span, "a map's value set without its map"))?;
                    self.w.line(&format!("{}({}, {}, {});", helper, map, key, value));
                } else if let ([Target::Place(lhs)], [rhs]) = (targets.as_slice(), values.as_slice()) {
                    if (has_prelude(rhs) || !changes_nothing(rhs))
                        && let ExprKind::Index(base, index) = &lhs.kind
                        && let Some(slice_ty @ Type::Slice(..)) = &base.ty
                        && !matches!(index.kind, ExprKind::Range(..))
                        && matches!(base.kind, ExprKind::Var(_) | ExprKind::Field(..) | ExprKind::Deref(_))
                    {
                        // `xs[i] = grow(&xs)`: the value may move the slice's
                        // items (`xs.add(…)`), so the slice's place and the
                        // index are found first, and the item after the value,
                        // in the slice as it is then.
                        let slice = self.gen_place(base)?;
                        let pointer = self.fresh_temp("slice");
                        self.w.line(&format!("{} const* {} = &{};", c_type(slice_ty), pointer, slice));
                        let at = self.gen_expr(index)?;
                        let position = self.fresh_temp("index");
                        self.w.line(&format!("int64_t {} = {};", position, at));
                        // C sequences neither side of `=` first, so the value
                        // is kept before the item is found.
                        let value = self.gen_expr(rhs)?;
                        let kept = self.fresh_temp("value");
                        self.w.line(&format!("{} {} = {};", c_type(lhs.ty()?), kept, value));
                        let helper = item_helper("at", slice_ty);
                        let location = self.location(lhs.span);
                        self.w.line(&format!("*{}(*{}, {}, {}) = {};", helper, pointer, position, location, kept));
                    } else if (has_prelude(rhs) || !changes_nothing(rhs)) && !matches!(lhs.kind, ExprKind::Var(_)) {
                        // Find the place before the value's prelude runs, which
                        // might change what the place refers to.
                        let place = self.gen_place(lhs)?;
                        let pointer = self.fresh_temp("place");
                        self.w.line(&format!("{}* {} = &{};", c_type(lhs.ty()?), pointer, place));
                        let value = self.gen_expr(rhs)?;
                        self.w.line(&format!("*{} = {};", pointer, value));
                    } else {
                        let l = self.gen_place(lhs)?;
                        let r = self.gen_expr(rhs)?;
                        self.w.line(&format!("{} = {};", l, r));
                    }
                } else {
                    self.gen_multi_assign(targets, values, stmt.span)?;
                }
            }
            StmtKind::When(arms, else_b) => self.gen_when(arms, else_b.as_deref(), None)?,
            // A C block of its own: its regions are freed, and the files opened
            // in them closed, at its end (see `leave_block`).
            StmtKind::Block(body) => {
                self.w.open("");
                self.gen_block(body)?;
                self.w.close();
            }
            StmtKind::Named(name, inner) => {
                self.loop_name = Some(name.clone());
                self.gen_stmt(inner)?;
            }
            StmtKind::Break(name, value) => self.gen_jump(true, name.as_ref().map(|(name, _)| name.as_str()), value.as_deref(), stmt.span)?,
            StmtKind::Continue(name) => self.gen_jump(false, name.as_ref().map(|(name, _)| name.as_str()), None, stmt.span)?,
            // `each v in args` written out: a block for each value, and a
            // `continue` goes to the end of the one it is in.
            StmtKind::Turns(turns) => {
                let name = self.take_loop();
                self.enter_loop(name, false);
                for turn in turns {
                    self.w.open("");
                    self.gen_block(turn)?;
                    self.end_pass();
                    self.w.close();
                }
                self.leave_loop();
            }
            StmtKind::Parted(parted) => {
                // `{ start; while (1) { test; body; next: step; } stop: }`: the
                // step runs after a `continue` too, and never after a `break`.
                let name = self.take_loop();
                self.w.open("");
                for (i, item) in parted.start.iter().enumerate() {
                    self.gen_stmt(item)?;
                    // A name of the loop's own that it only sets is unread in C.
                    if let StmtKind::Binding { names, .. } = &item.kind {
                        for bound in names.iter().flat_map(Bound::leaves).filter(|bound| bound.name != DISCARD) {
                            let read = block_reads(parted.start.get(i + 1..).unwrap_or_default(), &bound.name)
                                || parted.cond.iter().flat_map(Condition::parts).any(|part| expr_reads(&part.value, &bound.name))
                                || block_reads(&parted.step, &bound.name)
                                || block_reads(&parted.body, &bound.name);
                            if !read {
                                self.w.line(&format!("(void){};", c_ident(&bound.name)));
                            }
                        }
                    }
                }
                self.enter_loop(name, parted.finally.is_some());
                self.w.open("while (1)");
                match &parted.cond {
                    Some(cond) => self.gen_tested_pass(cond, &parted.body)?,
                    None => self.gen_pass(&parted.body)?,
                }
                self.end_pass();
                for item in &parted.step {
                    self.gen_stmt(item)?;
                }
                self.w.close();
                self.finish_loop(parted.finally.as_deref())?;
                self.leave_loop();
                self.w.close();
            }
            StmtKind::Loop(cond, body, finally) if !cond.single() => {
                // `loop m is {x} and x > 0`: each part tested inside the one
                // before, the body inside them all, and a failed part ends it.
                let name = self.take_loop();
                self.enter_loop(name, finally.is_some());
                let matched = self.fresh_temp("matched");
                self.w.open("while (1)");
                self.w.line(&format!("bool {} = false;", matched));
                self.gen_parts(cond, body, &mut |g| {
                    g.w.line(&format!("{} = true;", matched));
                    g.gen_pass(body)
                })?;
                self.w.open(&format!("if (!{})", matched));
                self.w.line("break;");
                self.w.close();
                self.end_pass();
                self.w.close();
                self.finish_loop(finally.as_deref())?;
                self.leave_loop();
            }
            StmtKind::Loop(cond, body, finally) => {
                // The condition is tested inside the loop, so a `break` in
                // its plan B stops this one.
                let name = self.take_loop();
                self.enter_loop(name, finally.is_some());
                match &cond.pattern {
                    None if !has_prelude(&cond.value) => {
                        let c = self.gen_condition(&cond.value)?;
                        self.w.open(&format!("while ({})", c));
                        self.gen_pass(body)?;
                    }
                    None => {
                        // The condition's prelude runs before every test.
                        self.w.open("while (1)");
                        let c = self.gen_expr(&cond.value)?;
                        self.w.open(&format!("if (!{})", parenthesized(&c)));
                        self.w.line("break;");
                        self.w.close();
                        self.gen_pass(body)?;
                    }
                    Some(pattern) => {
                        self.w.open("while (1)");
                        let subject = self.gen_subject(&cond.value)?;
                        let test = self.pattern_test(cond.value.ty()?, &subject, pattern)?;
                        self.w.open(&format!("if (!({}))", test));
                        self.w.line("break;");
                        self.w.close();
                        self.gen_pattern_bindings(cond.value.ty()?, &subject, pattern, body)?;
                        self.gen_pass(body)?;
                    }
                }
                self.end_pass();
                self.w.close();
                self.finish_loop(finally.as_deref())?;
                self.leave_loop();
            }
            // `each v in &xs`: the place is found once, and its length and
            // items are read through it at each pass, as they are then.
            // `each k, v in m`, `each k in m.keys`, and `each k, v in &m`: the
            // entries in order, skipping those taken out.
            StmtKind::Each(name, second, items, body, finally) if self.walks_map(items) => {
                let named = self.take_loop();
                self.gen_each_map(name, second.as_ref().map(|(name, _)| name.as_str()), items, body, finally.as_deref(), named)?;
            }
            StmtKind::Each(name, _, items, body, finally) if matches!(items.kind, ExprKind::Ref(_)) => {
                let ExprKind::Ref(place) = &items.kind else { return Err(Error::internal_at(items.span, "each over no place")) };
                let named = self.take_loop();
                let ty = items.ty()?.clone();
                let walked = self.fresh_temp("items");
                let index = self.fresh_temp("index");
                let (item, len) = match &ty {
                    Type::Array(item, count) => (item, format!("INT64_C({})", count)),
                    Type::Slice(item, _) => (item, format!("{}->len", walked)),
                    other => return Err(Error::internal_at(items.span, format!("each over a place of type {}", other))),
                };
                let address = self.gen_address(place.as_ref())?;
                self.w.open("");
                self.w.line(&format!("const {} *{} = {};", c_type(&ty), walked, address));
                self.enter_loop(named, finally.is_some());
                self.w.open(&format!("for (int64_t {0} = 0; {0} < {1}; {0}++)", index, len));
                self.w.line(&format!("{} {} = {}->items[{}];", c_type(item.as_ref()), c_ident(name), walked, index));
                self.use_if_unread(name, body);
                self.gen_pass(body)?;
                self.end_pass();
                self.w.close();
                self.finish_loop(finally.as_deref())?;
                self.leave_loop();
                self.w.close();
            }
            StmtKind::Each(name, _, items, body, finally) if !matches!(items.kind, ExprKind::Range(..)) => {
                // What it goes through is found once: an array is copied, and a
                // slice is a view found then, so the loop goes through the items
                // where they were when it started, as many as there were.
                let named = self.take_loop();
                let ty = items.ty()?.clone();
                let (item, len) = match &ty {
                    Type::Array(item, count) => (item, format!("INT64_C({})", count)),
                    Type::Slice(item, _) => (item, String::new()),
                    other => return Err(Error::internal_at(items.span, format!("each over a value of type {}", other))),
                };
                let value = self.gen_expr(items)?;
                let walked = self.fresh_temp("items");
                let index = self.fresh_temp("index");
                let len = if len.is_empty() { format!("{}.len", walked) } else { len };
                self.w.open("");
                self.w.line(&format!("{} {} = {};", c_type(&ty), walked, value));
                self.enter_loop(named, finally.is_some());
                self.w.open(&format!("for (int64_t {0} = 0; {0} < {1}; {0}++)", index, len));
                self.w.line(&format!("{} {} = {}.items[{}];", c_type(item.as_ref()), c_ident(name), walked, index));
                self.use_if_unread(name, body);
                self.gen_pass(body)?;
                self.end_pass();
                self.w.close();
                self.finish_loop(finally.as_deref())?;
                self.leave_loop();
                self.w.close();
            }
            StmtKind::Each(name, _, range, body, finally) => {
                let named = self.take_loop();
                let (a, b) = self.gen_range(range)?;
                // The end of the range is evaluated once, before the first
                // iteration. (The checker forbids shadowing, so `b` cannot
                // refer to a different variable called `name`.)
                let end = self.fresh_temp("end");
                let name = c_ident(name);
                self.enter_loop(named, finally.is_some());
                self.w.open(&format!(
                    "for (int64_t {} = {}, {} = {}; {} < {}; {}++)",
                    name, a, end, b, name, end, name
                ));
                self.gen_pass(body)?;
                self.end_pass();
                self.w.close();
                self.finish_loop(finally.as_deref())?;
                self.leave_loop();
            }
            StmtKind::Match(expr, cases) => self.gen_match(expr, cases, None)?,
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
                // `<- next(n)` in a copy whose `R` is `nothing`: the call runs,
                // and the task sends nothing.
                if self.ret == Type::Nothing {
                    if !expr.is_empty() {
                        self.w.line(&format!("{};", expr));
                    }
                    self.free_all_regions();
                    self.w.line("return;");
                    return Ok(());
                }
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
            StmtKind::Region { name, .. } => {
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
                    if matches!(expr.ty()?, Type::Nothing) {
                        // A call that sends nothing, received by `_`: it runs here
                        // (or already has, as a prelude, when it wrote nothing).
                        if !value.is_empty() {
                            self.w.line(&format!("{};", value));
                        }
                        value.clear();
                        continue;
                    }
                    let temp = self.fresh_temp("value");
                    self.w.line(&format!("{} {} = {};", c_type(expr.ty()?), temp, value));
                    *value = temp;
                }
            }
            return Ok(given);
        }
        // One call giving every value, or one tuple giving its items.
        let [call] = values else {
            return Err(Error::internal_at(span, format!("{} values received by {} names", values.len(), wanted)));
        };
        let ty = call.ty()?.clone();
        let (count, member): (usize, fn(usize) -> String) = match &ty {
            Type::Results(types) => (types.len(), result_member),
            Type::Tuple(items) => (items.len(), tuple_member),
            other => return Err(Error::internal_at(span, format!("a value of type {} received by {} names", other, wanted))),
        };
        let value = self.gen_expr(call)?;
        let results = self.fresh_temp("results");
        self.w.line(&format!("{} {} = {};", c_type(&ty), results, value));
        Ok((0..count).map(|i| format!("{}.{}", results, member(i))).collect())
    }

    /// Declares the names `( … )` takes from the tuple `tuple`, a C expression
    /// read without effects.
    fn gen_unpacked(&mut self, group: &Bound, tuple: &str) -> Result<()> {
        for (i, part) in group.parts.iter().enumerate() {
            let item = format!("{}.{}", tuple, tuple_member(i));
            if !part.parts.is_empty() {
                self.gen_unpacked(part, &item)?;
            } else if part.name != DISCARD {
                let ty = part.ty.as_ref().ok_or_else(|| Error::internal_at(part.span, format!("binding '{}' has no type", part.name)))?;
                self.w.line(&format!("{} {} = {};", c_type(ty), c_ident(&part.name), item));
            }
        }
        Ok(())
    }

    /// The pointers to the places `target` writes to, with how each is read from
    /// the value given (a member path of tuples, empty for the value itself),
    /// found before any value is computed.
    fn gen_target_places(&mut self, target: &Target, path: String, places: &mut Vec<(Option<String>, String)>) -> Result<()> {
        match target {
            Target::Discard(_) => places.push((None, path)),
            Target::Place(place) => {
                let lvalue = self.gen_place(place)?;
                let pointer = self.fresh_temp("place");
                self.w.line(&format!("{}* {} = &{};", c_type(place.ty()?), pointer, lvalue));
                places.push((Some(pointer), path));
            }
            Target::Tuple(parts, _) => {
                for (i, part) in parts.iter().enumerate() {
                    self.gen_target_places(part, format!("{}.{}", path, tuple_member(i)), places)?;
                }
            }
        }
        Ok(())
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
            let mut parts = Vec::new();
            self.gen_target_places(target, String::new(), &mut parts)?;
            places.push(parts);
        }
        let types = given_types(targets.len(), values, span)?;
        let given = self.gen_value_list(targets.len(), values, span)?;
        for (((parts, target), value), ty) in places.into_iter().zip(targets).zip(given).zip(types) {
            // A tuple taken apart is kept once, and each place takes its item.
            let value = match target {
                Target::Tuple(..) if parts.iter().all(|(place, _)| place.is_none()) => {
                    // `(_, _)`: nothing is written.
                    self.drop_value(&value);
                    continue;
                }
                Target::Tuple(..) => {
                    let tuple = self.fresh_temp("tuple");
                    self.w.line(&format!("{} {} = {};", c_type(&ty), tuple, value));
                    tuple
                }
                _ => value,
            };
            for (place, path) in parts {
                match place {
                    Some(pointer) => self.w.line(&format!("*{} = {}{};", pointer, value, path)),
                    None if path.is_empty() => self.drop_value(&value),
                    None => {}
                }
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
            // A tuple's items always match; the names take them.
            (PatternKind::Tuple(_), _) => "1".to_string(),
            _ => return Err(Error::internal_at(pattern.span, format!("pattern '{}' on type {}", pattern.label(), ty))),
        };
        Ok(test)
    }

    /// Declares the names `pattern` binds from `subject`, of type `ty`, for the
    /// block `body`.
    fn gen_pattern_bindings(&mut self, ty: &Type, subject: &str, pattern: &Pattern, body: &[Stmt]) -> Result<()> {
        let mut bindings: Vec<(&str, Type, String)> = Vec::new();
        let mut named = false;
        match (&pattern.kind, ty) {
            (PatternKind::Value(names), Type::Maybe(inner)) if is_pointer(inner) => {
                bound_names(names, inner, subject.to_string(), &mut bindings);
            }
            (PatternKind::Value(names), Type::Maybe(inner) | Type::Or(inner, _)) => {
                bound_names(names, inner, format!("{}.value", subject), &mut bindings);
            }
            (PatternKind::Fail(names), Type::Or(_, error)) => {
                bound_names(names, error, format!("{}.error", subject), &mut bindings);
            }
            // `(x, y)`: the items of the tuple itself.
            (PatternKind::Tuple(names), _) => bound_names(names, ty, subject.to_string(), &mut bindings),
            (PatternKind::Variant { tag, binds, .. }, Type::Named(kind_name, _)) => {
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
                for (names, field) in binds.iter().zip(&variant.payload) {
                    let value = format!("{}.data.{}.{}", subject, c_ident(tag), c_ident(&field.name));
                    bound_names(names, &field.ty, value, &mut bindings);
                }
            }
            _ => {}
        }
        for (name, ty, value) in bindings {
            if name == DISCARD {
                continue;
            }
            named = true;
            self.w.line(&format!("{} {} = {};", c_type(&ty), c_ident(name), value));
            // A pattern must name every value, even ones its block never reads.
            self.use_if_unread(name, body);
        }
        // A tuple's items always match, so its test reads nothing; with every
        // item dropped (`is (_, _)`), nothing would read the value at all.
        if !named && matches!(pattern.kind, PatternKind::Tuple(_)) {
            self.w.line(&format!("(void){};", subject));
        }
        Ok(())
    }

    /// Generates a `when`, and the `else when`s after it: as C's `else if`
    /// where each arm after the first is one condition that needs nothing
    /// before it and there are at most `ELSE_IF_ARMS`, and otherwise each arm
    /// in turn, the one that runs going to the end past the rest, so the C
    /// stays flat however long the chain is.
    fn gen_when(&mut self, arms: &[WhenArm], else_b: Option<&[Stmt]>, into: Option<&str>) -> Result<()> {
        let Some((first, rest)) = arms.split_first() else { return Err(Error::internal("a 'when' without a condition")) };
        let plain = |cond: &Condition| cond.pattern.is_none() && cond.single() && !has_prelude(&cond.value);
        if rest.is_empty() {
            return self.gen_one_when(&first.cond, &first.body, else_b, into);
        }
        if arms.len() <= ELSE_IF_ARMS && first.cond.pattern.is_none() && first.cond.single() && rest.iter().all(|arm| plain(&arm.cond)) {
            let c = self.gen_condition(&first.cond.value)?;
            self.w.open(&format!("if ({})", c));
            self.gen_branch_block(&first.body, into)?;
            for arm in rest {
                let c = self.gen_condition(&arm.cond.value)?;
                self.w.reopen(&format!("else if ({})", c));
                self.gen_branch_block(&arm.body, into)?;
            }
            if let Some(else_b) = else_b {
                self.w.reopen("else");
                self.gen_branch_block(else_b, into)?;
            }
            self.w.close();
            return Ok(());
        }
        // Each arm in turn at one depth, however many there are, and the arm
        // that runs leaves for the end: a C compiler nests an `else if` chain,
        // and a long one is more than it takes.
        let end = self.fresh_temp("when_end");
        for arm in arms {
            self.gen_parts(&arm.cond, &arm.body, &mut |g| {
                g.gen_branch_block(&arm.body, into)?;
                g.w.line(&format!("goto {};", end));
                Ok(())
            })?;
        }
        if let Some(else_b) = else_b {
            self.w.open("");
            self.gen_branch_block(else_b, into)?;
            self.w.close();
        }
        self.w.line(&format!("{}: ;", end));
        Ok(())
    }

    /// A `when` of one arm, and its `else`.
    fn gen_one_when(&mut self, cond: &Condition, then_b: &[Stmt], else_b: Option<&[Stmt]>, into: Option<&str>) -> Result<()> {
        if !cond.single() {
            // `when m is {x} and x > 0`: each part tested inside the one before
            // and the block inside them all, so what a part binds is seen after
            // it; the `else` runs where any part failed.
            let matched = self.fresh_temp("matched");
            if else_b.is_some() {
                self.w.line(&format!("bool {} = false;", matched));
            }
            self.gen_parts(cond, then_b, &mut |g| {
                if else_b.is_some() {
                    g.w.line(&format!("{} = true;", matched));
                }
                g.gen_branch_block(then_b, into)
            })?;
            if let Some(else_b) = else_b {
                self.w.open(&format!("if (!{})", matched));
                self.gen_branch_block(else_b, into)?;
                self.w.close();
            }
            return Ok(());
        }
        match &cond.pattern {
            None => {
                let c = self.gen_condition(&cond.value)?;
                self.w.open(&format!("if ({})", c));
                self.gen_branch_block(then_b, into)?;
            }
            Some(pattern) => {
                let subject = self.gen_subject(&cond.value)?;
                let test = self.pattern_test(cond.value.ty()?, &subject, pattern)?;
                self.w.open(&format!("if ({})", test));
                self.gen_pattern_bindings(cond.value.ty()?, &subject, pattern, then_b)?;
                self.gen_branch_block(then_b, into)?;
            }
        }
        if let Some(else_b) = else_b {
            self.w.reopen("else");
            self.gen_branch_block(else_b, into)?;
        }
        self.w.close();
        Ok(())
    }

    /// The tests of a condition joined with `and`, each inside the one before,
    /// with what an `is` binds declared in its own; `inner` writes what runs
    /// when every part holds, inside them all. A part's statements, such as a
    /// call's with a plan B, run only when the parts before it held.
    fn gen_parts(&mut self, cond: &Condition, body: &[Stmt], inner: &mut dyn FnMut(&mut Self) -> Result<()>) -> Result<()> {
        let mut opened = 0;
        for part in cond.parts() {
            match &part.pattern {
                None => {
                    let c = self.gen_condition(&part.value)?;
                    self.w.open(&format!("if ({})", c));
                }
                Some(pattern) => {
                    let subject = self.gen_subject(&part.value)?;
                    let test = self.pattern_test(part.value.ty()?, &subject, pattern)?;
                    self.w.open(&format!("if ({})", test));
                    self.gen_pattern_bindings(part.value.ty()?, &subject, pattern, body)?;
                }
            }
            opened += 1;
        }
        inner(self)?;
        for _ in 0..opened {
            self.w.close();
        }
        Ok(())
    }

    /// One branch's block: giving a value into `into`, or plain statements.
    fn gen_branch_block(&mut self, body: &[Stmt], into: Option<&str>) -> Result<()> {
        match into {
            Some(into) => self.gen_value_block(body, into),
            None => self.gen_block(body),
        }
    }

    /// Generates a `match`. On a kind, it is a `switch` on the tag, with `_` as
    /// `default`; on a `maybe` or `T ~ E`, an `if`/`else` chain whose last case
    /// needs no test. Each case body gets its own C block, so different cases
    /// may bind the same names. The matched value is evaluated exactly once.
    fn gen_match(&mut self, expr: &Expr, cases: &[Case], into: Option<&str>) -> Result<()> {
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
                self.gen_branch_block(&case.body, into)?;
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
                self.gen_branch_block(&case.body, into)?;
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
    /// an operator's two sides. When one of them has a prelude, or may change
    /// something (a call, which may change a variable given as `&a`), they are
    /// evaluated left to right: every operand before the last such one is
    /// stored in a temporary first, so that it is evaluated before that one
    /// runs, and so is every other operand but the last, since C evaluates the
    /// rest in no fixed order.
    fn gen_operands(&mut self, operands: &[&Expr]) -> Result<Vec<String>> {
        let last_prelude = operands.iter().rposition(|e| has_prelude(e) || !changes_nothing(e));
        let mut values = Vec::new();
        for (i, operand) in operands.iter().enumerate() {
            let value = self.gen_expr(operand)?;
            // A variable after the last prelude reads the same value whenever C reads it.
            let variable = matches!(operand.kind, ExprKind::Var(_));
            let in_order = last_prelude.is_some_and(|last| i < last || (i + 1 < operands.len() && !variable));
            values.push(self.operand_value(operand, value, in_order)?);
        }
        Ok(values)
    }

    /// What C reads of `operand`, whose C is `value`: kept in a variable of its
    /// own when `in_order`, so it is worked out before a later operand's
    /// prelude runs (see `gen_operands`).
    fn operand_value(&mut self, operand: &Expr, value: String, in_order: bool) -> Result<String> {
        let constant = matches!(
            operand.kind,
            ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::CharLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) | ExprKind::NoneLit | ExprKind::Macro { .. }
        );
        // A value-less operand (`xs.add(v)`, or a call of a task sending
        // nothing, in a list of values) runs in order as a statement.
        if in_order && *operand.ty()? == Type::Nothing {
            if !value.is_empty() {
                self.w.line(&format!("{};", value));
            }
            Ok(String::new())
        } else if in_order && !constant {
            let tmp = self.fresh_temp("arg");
            let ty = operand.ty()?;
            match &operand.kind {
                // An address read (`&a == &b`), typed `^T`: only compared, so
                // kept as one, which a place that never changes gives too.
                ExprKind::Ref(place) if place.ty.as_ref().is_some_and(|place| *ty == Type::Ptr(Box::new(place.clone()), None)) => {
                    self.w.line(&format!("const void* {} = {};", tmp, value));
                }
                // A place given to a `flex&` parameter is its address.
                ExprKind::Ref(_) => self.w.line(&format!("{}* {} = {};", c_type(operand.ty()?), tmp, value)),
                _ => self.w.line(&format!("{} {} = {};", c_type(operand.ty()?), tmp, value)),
            }
            Ok(tmp)
        } else {
            Ok(value)
        }
    }

    /// The arguments of a call some of whose parameters are passed by a hidden
    /// pointer (`hidden`, by position), read left to right as `gen_operands`
    /// reads them. Such a parameter is given the address of where the value is
    /// kept, a local variable or a part of one, when nothing the call does
    /// before it starts can change it: no argument after it may change
    /// anything. A value anywhere else (behind a pointer, in a slice, named by
    /// a reference) might change while the task runs, through memory the task
    /// reaches, so it is copied first, as any other value is; and so is every
    /// one given to a method that changes the value it is called on (not
    /// `addressable`), which may hold it.
    fn gen_hidden_operands(&mut self, operands: &[&Expr], hidden: &[bool], addressable: bool) -> Result<Vec<String>> {
        let last_effect = operands.iter().rposition(|e| !changes_nothing(e));
        let mut values = Vec::new();
        for (i, operand) in operands.iter().enumerate() {
            let effect_after = last_effect.is_some_and(|last| i < last);
            // As in `gen_operands`: before the last operand that may change
            // something, or that one itself when others follow it.
            let variable = matches!(operand.kind, ExprKind::Var(_));
            let in_order = last_effect.is_some_and(|last| i < last || (i + 1 < operands.len() && !variable));
            if hidden.get(i).copied().unwrap_or(false) {
                if addressable && !effect_after && self.kept_place(operand) {
                    values.push(self.address_of(operand)?);
                    continue;
                }
                let value = self.gen_expr(operand)?;
                if in_order {
                    let tmp = self.fresh_temp("arg");
                    self.w.line(&format!("{} {} = {};", c_type(operand.ty()?), tmp, value));
                    values.push(format!("&{}", tmp));
                } else {
                    // A copy made where the argument is read, which lasts as long
                    // as the block, so the call writes no statement of its own.
                    values.push(format!("&((struct {{ {} value; }}){{ {} }}).value", c_type(operand.ty()?), value));
                }
                continue;
            }
            let value = self.gen_expr(operand)?;
            let constant = matches!(
                operand.kind,
                ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::CharLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) | ExprKind::NoneLit | ExprKind::Macro { .. }
            );
            if in_order && !constant {
                let tmp = self.fresh_temp("arg");
                let pointer = if matches!(operand.kind, ExprKind::Ref(_)) { "*" } else { "" };
                self.w.line(&format!("{}{} {} = {};", c_type(operand.ty()?), pointer, tmp, value));
                values.push(tmp);
            } else {
                values.push(value);
            }
        }
        Ok(values)
    }

    /// Whether `e` is a place in the task's own memory, which only the task
    /// changes: a local variable (not a reference or a `flex&` parameter, which
    /// may name memory in a region), a parameter passed by a hidden pointer,
    /// or a field or an array item of one.
    fn kept_place(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Var(name) => !self.pointers.contains(name) || self.hidden.contains(name),
            ExprKind::Field(base, _) if matches!(base.ty, Some(Type::Named(..))) => self.kept_place(base),
            ExprKind::Index(base, index) if matches!(base.ty, Some(Type::Array(..))) && !matches!(index.kind, ExprKind::Range(..)) => {
                changes_nothing(index) && self.kept_place(base)
            }
            _ => false,
        }
    }

    /// The address of a place `kept_place` accepts.
    fn address_of(&mut self, e: &Expr) -> Result<String> {
        match &e.kind {
            ExprKind::Var(name) if self.hidden.contains(name) => Ok(c_ident(name)),
            ExprKind::Var(name) => Ok(format!("&{}", c_ident(name))),
            _ => Ok(format!("&{}", self.gen_place(e)?)),
        }
    }

    /// The C expression for `e`. Each kind of expression with more to do than
    /// a word has a function of its own: this function's frame is on the
    /// stack once per nesting level of an expression.
    fn gen_expr(&mut self, e: &Expr) -> Result<String> {
        if let Some((replaced, text)) = &self.replaced
            && std::ptr::eq(*replaced, e)
        {
            return Ok(text.clone());
        }
        match &e.kind {
            ExprKind::IntLit(n) => Ok(n.to_string()),
            ExprKind::FloatLit(n) => Ok(c_float_literal(*n)),
            ExprKind::CharLit(c) => Ok(format!("((uint8_t){})", c)),
            ExprKind::BoolLit(b) => Ok(if *b { "true".to_string() } else { "false".to_string() }),
            ExprKind::TextLit(s) => Ok(format!("VELA__RT__TEXT(\"{}\")", escape_c_string(s))),
            ExprKind::NoneLit => Ok("NULL".to_string()),
            ExprKind::Macro { name, .. } => Ok(macro_name(name)),
            ExprKind::TaskValue(name) => Ok(self.task_value(name)),
            ExprKind::Closure { lambda, region, .. } => self.gen_closure(e, lambda, region),
            ExprKind::Kept { value, region } => {
                let Some(Type::Closure(closure)) = &e.ty else { return Err(Error::internal_at(e.span, "a task value kept as no lambda")) };
                let plain = c_type(&Type::Task(Box::new(closure.task.clone())));
                let kept = match (region, &value.kind) {
                    // A task named here: where the program keeps it.
                    (None, ExprKind::TaskValue(name)) => format!("(void*)&{}", kept_task_name(name)),
                    (Some(region), _) => {
                        let value = self.gen_expr(value)?;
                        format!("vela__rt__new({0}, &({1}){{ {2} }}, sizeof({1}), _Alignof({1}))", c_ident(region), plain, value)
                    }
                    _ => return Err(Error::internal_at(e.span, "a task value kept in no region")),
                };
                Ok(format!("(({}){{ vela__plain__{}, {} }})", c_type(e.ty()?), type_word(e.ty()?), kept))
            }
            ExprKind::Invoke { callee, args } => self.gen_invoke(e, callee, args),
            // The checker makes every one the `New` it stands for.
            ExprKind::Shortcut { .. } => Err(Error::internal_at(e.span, "a shortcut the checker left as it was")),
            ExprKind::Pairs(_) => Err(Error::internal_at(e.span, "a map's pairs outside the 'new' they are given to")),
            ExprKind::MapOp { map, op, key, region, .. } => {
                let ty = map.ty()?.without_regions();
                match (op, key) {
                    (MapOp::Has, Some(key)) => {
                        let [map, key] = <[String; 2]>::try_from(self.gen_operands(&[map, key])?)
                            .map_err(|_| Error::internal_at(e.span, "'has' without its map"))?;
                        Ok(format!("{}({}, {})", item_helper("map_has", &ty), map, key))
                    }
                    (MapOp::Keys | MapOp::Values, None) => {
                        let region = region.as_deref().ok_or_else(|| Error::internal_at(e.span, "a map's keys with no region"))?;
                        let what = if *op == MapOp::Keys { "map_keys" } else { "map_values" };
                        Ok(format!("{}({}, {})", item_helper(what, &ty), c_ident(region), self.gen_expr(map)?))
                    }
                    _ => Err(Error::internal_at(e.span, format!("'{}' on a map, as written", op.word()))),
                }
            }
            // The checker makes every one the `Var` it stands for.
            ExprKind::RegionAfter(_) => Err(Error::internal_at(e.span, "a region written after a call that the checker left as it was")),
            ExprKind::TypeOf(_) => Err(Error::internal_at(e.span, "a '.type' left for types a task was never made for")),
            ExprKind::Var(name) if self.pointers.contains(name) => Ok(format!("(*{})", c_ident(name))),
            // The place given to a `flex&` parameter: its address, worked out
            // where the argument is, as its value would be (`gen_operands`
            // keeps it in order with the others).
            ExprKind::Ref(place) => self.gen_address(place),
            // The region the `show` being generated makes for the values written
            // by their own `str`, which has a name of its own in C (see
            // `gen_show`), so that one `show` inside another keeps its own.
            ExprKind::Var(name) if name == SHOWN => self
                .shown
                .last()
                .cloned()
                .ok_or_else(|| Error::internal_at(e.span, "the region of a 'show' read outside one")),
            ExprKind::Var(name) => Ok(c_ident(name)),
            ExprKind::Shared { shape, field } => Ok(shared_name(shape, field)),
            ExprKind::Field(base, field) => self.gen_field(base, field),
            ExprKind::Index(base, index) => self.gen_index(e, base, index, None),
            ExprKind::Lookup(base, index) => {
                let [value, index] = <[String; 2]>::try_from(self.gen_operands(&[base, index])?)
                    .map_err(|_| Error::internal_at(e.span, "a lookup without its value"))?;
                let key = lookup_key(base.ty()?);
                Ok(match base.ty()? {
                    Type::Map(..) => format!("{}({}, {})", item_helper("lookup", &key), value, index),
                    // An array's items are read where they are, even in a value
                    // made for the expression, which lasts as long as it does.
                    Type::Array(_, count) => format!("{}(({}).items, INT64_C({}), {})", item_helper("lookup", &key), value, count, index),
                    _ => format!("{}({}, {})", item_helper("lookup", &key), value, index),
                })
            }
            ExprKind::Array(values) => self.gen_array(e, values),
            ExprKind::Tuple(values) => self.gen_tuple(e, values),
            ExprKind::Repeat { value, .. } => self.gen_repeat(e, value),
            ExprKind::Call { name, types, args } => self.gen_call(e, name, types, args),
            ExprKind::Unary(op, operand) => self.gen_unary(e, *op, operand),
            ExprKind::Binary(op, l, r) => self.gen_binary(e, *op, l, r).map(|binary| parenthesized(&binary)),
            // A value of a kind made for its types is of that kind, which its type
            // names (see `ExprKind::Construct`).
            ExprKind::Construct { kind, tag, args, .. } => {
                let kind = match &e.ty {
                    Some(Type::Named(made, _)) => made.clone(),
                    _ => kind.clone(),
                };
                self.gen_construct(e, &kind, tag, args)
            }
            ExprKind::New { ty, value, region, .. } => {
                // The checker settles the region of a `new` that names none
                // (see `Checker::region_or_default`).
                let region = region.as_deref().ok_or_else(|| Error::internal_at(e.span, "a 'new' with no region"))?;
                self.gen_new(e, ty, value, region)
            }
            ExprKind::Deref(inner) => self.gen_expr(inner).map(|expr| format!("(*({}))", expr)),
            // Each use of a task taking a group gives its values one by one
            // (see `Checker::substitute_expr`).
            ExprKind::Spread(_) => Err(Error::internal_at(e.span, "a group's values given on whole outside a task that takes a group")),
            ExprKind::StructInit { name, fields, .. } => self.gen_struct_init(name, fields),
            ExprKind::Range(_, _) => Err(Error::internal_at(e.span, "range outside 'each' reached code generation")),
            // The error value itself; `Wrap` makes it part of a `T ~ E`.
            ExprKind::Fail(error) => self.gen_expr(error),
            ExprKind::Wrap(inner) => self.gen_wrap(e, inner),
            ExprKind::Widen(inner) => self.gen_widen(e, inner),
            ExprKind::Pipe(call) => self.gen_pipe(e, call),
            ExprKind::Piped(_) => self.piped.last().cloned().ok_or_else(|| Error::internal_at(e.span, "a piped value outside its pipe")),
            ExprKind::PlanB { value, error, body } => self.gen_plan_b(e, value, error.as_ref(), body),
            ExprKind::Choice(stmt) => self.gen_choice(e, stmt),
            ExprKind::LoopValue(value) => self.gen_loop_value(e, value),
            ExprKind::Edit { items, op, args } => self.gen_edit(e.span, items, *op, args, false),
            ExprKind::Scan { args, region, .. } => self.gen_scan(e, args, region.as_deref(), false),
            ExprKind::Join { parts, region, .. } => self.gen_join(parts, region),
            ExprKind::ToText { value, region, .. } => self.gen_to_text(e, value, region),
            ExprKind::Copy { value, region, .. } => self.gen_copy(e, value, region.as_deref()),
            // The checker turns every method call into the call of the task the
            // method is (see `Checker::check_method_call`).
            ExprKind::MethodCall { name, .. } => Err(Error::internal_at(e.span, format!("the method call '{}' reached code generation", name))),
        }
    }

    /// `a + b at region`: the parts, evaluated in order, joined into new text.
    ///
    /// Each part is read as it is, so the text is allocated once: text joins as
    /// the text it is, and a number is written into a buffer on the stack that
    /// lasts until the join has read it. Other values are made into text first.
    fn gen_join(&mut self, parts: &[Expr], region: &str) -> Result<String> {
        // What each part reads, and how it becomes text.
        let read: Vec<(&Expr, Option<&str>)> = parts
            .iter()
            .map(|part| match &part.kind {
                ExprKind::ToText { value, .. } => match value.ty {
                    Some(Type::Int | Type::Unit) => (value.as_ref(), Some("vela__rt__int_view((char[VELA__RT__NUMBER_ROOM]){ 0 }, ")),
                    Some(Type::Float) => (value.as_ref(), Some("vela__rt__float_view((char[VELA__RT__NUMBER_ROOM]){ 0 }, ")),
                    Some(Type::Char) => (value.as_ref(), Some("vela__rt__char_view((char[VELA__RT__NUMBER_ROOM]){ 0 }, ")),
                    Some(Type::Text(_)) => (value.as_ref(), None),
                    _ => (part, None),
                },
                _ => (part, None),
            })
            .collect();
        let values = self.gen_operands(&read.iter().map(|(value, _)| *value).collect::<Vec<_>>())?;
        let values: Vec<String> =
            values.into_iter().zip(&read).map(|(value, (_, view))| view.map_or(value.clone(), |view| format!("{}{})", view, value))).collect();
        Ok(format!("vela__rt__join({}, (vela__rt__text[]){{ {} }}, {})", c_ident(region), values.join(", "), values.len()))
    }

    /// `value.str() at region`, the expression `e`, on a value with `str` built in.
    fn gen_to_text(&mut self, e: &Expr, value: &Expr, region: &str) -> Result<String> {
        let _ = e;
        let helper = text_function(value.ty()?);
        let value = self.gen_expr(value)?;
        Ok(format!("{}({}, {})", helper, c_ident(region), value))
    }

    /// `value.copy() at region`, the expression `e`.
    fn gen_copy(&mut self, e: &Expr, value: &Expr, region: Option<&str>) -> Result<String> {
        let region = region.ok_or_else(|| Error::internal_at(e.span, "a copy whose region isn't settled reached code generation"))?;
        if let ExprKind::Edit { items, op, args } = &value.kind
            && op.makes_copy()
        {
            return self.gen_changed(e.span, items, *op, args, region);
        }
        let copied = self.gen_expr(value)?;
        // A value that holds nothing in a region is its own copy, as `int` is
        // for a task made from a generic one that copies its values. The region
        // is still read, so C doesn't warn about a parameter this leaves unused.
        if !self.needs_fix(&fix_type(value.ty()?)) {
            return Ok(format!("((void){}, {})", c_ident(region), parenthesized(&copied)));
        }
        Ok(format!("{}({}, {})", copy_helper("copied", value.ty()?), c_ident(region), copied))
    }

    /// `items.added(…)` or `items.without(…)` at `span`, a copy in `region` made
    /// with one change: a complete copy of `items`, as `copy()` makes, then the
    /// change made to it with the helper `add` or `del` uses, or `m[k] = v` and
    /// `m.del(k)` for a map. A value added is copied into `region` too, so the
    /// new value shares nothing with what it was made of. The C expression for
    /// the copy, made in statements before it.
    fn gen_changed(&mut self, span: Span, items: &Expr, op: EditOp, args: &[Expr], region: &str) -> Result<String> {
        let ty = items.ty()?.clone();
        let operands: Vec<&Expr> = std::iter::once(items).chain(args).collect();
        let values = self.gen_operands(&operands)?;
        let Some((source, rest)) = values.split_first() else { return Err(Error::internal_at(span, format!("'{}' without its value", op.word()))) };
        let r = c_ident(region);
        let copy = self.fresh_temp("changed");
        self.w.line(&format!("{} {} = {}({}, {});", c_type(&ty), copy, copy_helper("copied", &ty), r, source));
        let at = self.location(span);
        let call = match (&ty, op, rest) {
            (Type::Slice(item, _), EditOp::Added, [value]) => {
                let value = self.copied_into(item, &r, value);
                format!("{}({}, &{}, {})", item_helper("add", &ty), r, copy, value)
            }
            (Type::Slice(item, _), EditOp::Added, [index, value]) => {
                let value = self.copied_into(item, &r, value);
                format!("{}({}, &{}, {}, {}, {})", item_helper("insert", &ty), r, copy, index, value, at)
            }
            (Type::Slice(..), EditOp::Without, []) => {
                self.w.line(&format!("vela__rt__not_empty_for({}.len, \"without()\", {});", copy, at));
                format!("(void){}(&{}, {})", item_helper("pop", &ty), copy, at)
            }
            (Type::Slice(..), EditOp::Without, [index]) => format!("(void){}(&{}, {}, {})", item_helper("del", &ty), copy, index, at),
            (Type::Map(key_ty, value_ty, _), EditOp::Added, [key, value]) => {
                let (key, value) = (self.copied_into(key_ty, &r, key), self.copied_into(value_ty, &r, value));
                format!("{}({}, {}, {})", item_helper("map_set", &ty.without_regions()), copy, key, value)
            }
            (Type::Map(..), EditOp::Without, [key]) => format!("(void){}({}, {})", item_helper("map_del", &ty.without_regions()), copy, key),
            _ => return Err(Error::internal_at(span, format!("'{}' with {} arguments on a value of type {}", op.word(), rest.len(), ty))),
        };
        self.w.line(&format!("{};", call));
        Ok(copy)
    }

    /// `value`, a C expression of type `ty`, copied into the region `r` names
    /// when it holds anything in a region, or as it is.
    fn copied_into(&self, ty: &Type, r: &str, value: &str) -> String {
        if self.needs_fix(&fix_type(ty)) {
            format!("{}({}, {})", copy_helper("copied", ty), r, value)
        } else {
            value.to_string()
        }
    }

    /// `scan(prompt) at region`, at `span`, which it writes as a prelude, so it
    /// reads in order with the rest of the expression around it: the C
    /// expression for the `maybe text` it gives, or nothing when it is a
    /// `statement` of its own.
    fn gen_scan(&mut self, e: &Expr, args: &[Expr], region: Option<&str>, statement: bool) -> Result<String> {
        let span = e.span;
        let (prompt, typed) = match (args, region) {
            ([prompt], Some(region)) => (prompt, Err(region)),
            // A typed read names no region, and the type it reads is the value
            // of the `T ~ InputError` it gives.
            ([prompt, _], None) => match e.ty()? {
                Type::Or(number, _) => (prompt, Ok((**number).clone())),
                other => return Err(Error::internal_at(span, format!("a typed scan of type {}", other))),
            },
            _ => return Err(Error::internal_at(span, format!("scan with {} arguments", args.len()))),
        };
        let prompt = self.gen_expr(prompt)?;
        let call = match &typed {
            Ok(number) => format!("{}({})", scan_helper(number), prompt),
            Err(region) => format!("vela__scan({}, {})", c_ident(region), prompt),
        };
        if statement {
            self.w.line(&format!("{};", call));
            return Ok(String::new());
        }
        let (line, ty) = match &typed {
            Ok(_) => (self.fresh_temp("number"), e.ty()?.clone()),
            Err(_) => (self.fresh_temp("line"), Type::Maybe(Box::new(Type::Text(None)))),
        };
        self.w.line(&format!("{} {} = {};", c_type(&ty), line, call));
        Ok(line)
    }

    /// `items.add(…)` or `items.del(…)`, at `span`, which it writes as a
    /// prelude: the C expression for the item `del` gives, or nothing for
    /// `add`, or when the change is a `statement` of its own. The slice's
    /// place is found before the arguments are evaluated, as an assignment's.
    fn gen_edit(&mut self, span: Span, items: &Expr, op: EditOp, args: &[Expr], statement: bool) -> Result<String> {
        let slice = items.ty()?.clone();
        // `m.del(k)`: the map is its handle, read as any value is.
        if let (Type::Map(_, value, _), [key]) = (&slice, args) {
            let [map, key] = <[String; 2]>::try_from(self.gen_operands(&[items, key])?)
                .map_err(|_| Error::internal_at(span, "'del' without its map"))?;
            let call = format!("{}({}, {})", item_helper("map_del", &slice.without_regions()), map, key);
            if statement {
                self.w.line(&format!("{};", call));
                return Ok(String::new());
            }
            let taken = self.fresh_temp("taken");
            self.w.line(&format!("{} {} = {};", c_type(&Type::Maybe(value.clone())), taken, call));
            return Ok(taken);
        }
        let place = match &items.kind {
            ExprKind::Var(name) if self.pointers.contains(name) => c_ident(name),
            ExprKind::Var(name) => format!("&{}", c_ident(name)),
            _ => {
                let place = self.gen_place(items)?;
                let pointer = self.fresh_temp("place");
                self.w.line(&format!("{}* {} = &{};", c_type(&slice), pointer, place));
                pointer
            }
        };
        let operands: Vec<&Expr> = args.iter().collect();
        let values = self.gen_operands(&operands)?;
        let at = self.location(span);
        let call = match (op, values.as_slice(), &slice) {
            (EditOp::Add, [value], Type::Slice(_, Some(region))) => {
                format!("{}({}, {}, {})", item_helper("add", &slice), c_ident(region), place, value)
            }
            (EditOp::Add, [index, value], Type::Slice(_, Some(region))) => {
                format!("{}({}, {}, {}, {}, {})", item_helper("insert", &slice), c_ident(region), place, index, value, at)
            }
            (EditOp::Del, [], _) => format!("{}({}, {})", item_helper("pop", &slice), place, at),
            (EditOp::Del, [index], _) => format!("{}({}, {}, {})", item_helper("del", &slice), place, index, at),
            _ => return Err(Error::internal_at(span, format!("'{}' with {} arguments on a value of type {}", op.word(), values.len(), slice))),
        };
        if statement || op == EditOp::Add {
            self.w.line(&format!("{};", call));
            return Ok(String::new());
        }
        let item = self.fresh_temp("item");
        self.w.line(&format!("{} {} = {};", c_type(item_type(&slice)), item, call));
        Ok(item)
    }

    /// `base.field`, or an array's or slice's `len`.
    fn gen_field(&mut self, base: &Expr, field: &str) -> Result<String> {
        let b = self.gen_expr(base)?;
        match base.ty()? {
            // An array's length is its type's; the array is still read.
            Type::Array(_, count) => Ok(format!("((void){}, INT64_C({}))", parenthesized(&b), count)),
            Type::Slice(..) | Type::Text(_) => Ok(format!("({}).len", b)),
            Type::Map(..) => Ok(format!("({})->live", b)),
            Type::Tuple(_) => {
                let i = field.parse::<usize>().map_err(|_| Error::internal_at(base.span, format!("a tuple's item '{}'", field)))?;
                Ok(format!("({}).{}", b, tuple_member(i)))
            }
            _ => Ok(format!("({}).{}", b, c_ident(field))),
        }
    }

    /// `[a, b, c]`, the expression `e`.
    fn gen_array(&mut self, e: &Expr, values: &[Expr]) -> Result<String> {
        let operands: Vec<&Expr> = values.iter().collect();
        let given = self.gen_operands(&operands)?;
        Ok(format!("(({}){{ .items = {{ {} }} }})", c_type(e.ty()?), given.join(", ")))
    }

    /// `(a, b)`, the expression `e`.
    fn gen_tuple(&mut self, e: &Expr, values: &[Expr]) -> Result<String> {
        let operands: Vec<&Expr> = values.iter().collect();
        let given = self.gen_operands(&operands)?;
        let items: Vec<String> = given.iter().enumerate().map(|(i, value)| format!(".{} = {}", tuple_member(i), value)).collect();
        Ok(format!("(({}){{ {} }})", c_type(e.ty()?), items.join(", ")))
    }

    /// `[value#count]`, the expression `e`.
    fn gen_repeat(&mut self, e: &Expr, value: &Expr) -> Result<String> {
        let value = self.gen_expr(value)?;
        Ok(format!("{}({})", item_helper("fill", e.ty()?), value))
    }

    /// A call of a task, or of `show`, the expression `e`.
    fn gen_call(&mut self, e: &Expr, name: &str, types: &[TypeArg], args: &[Expr]) -> Result<String> {
        if name == "show" {
            return self.gen_show(e, args);
        }
        if let Some(into) = number_cast(name) {
            return self.gen_number_cast(e, &into, args);
        }
        // A call of a generic task calls the one made for the types it gives.
        let given: Vec<Type> = types.iter().map(|(ty, _)| ty.clone()).collect();
        let called = instance_name(name, &given);
        // A method that changes the value it is called on is given the place it
        // is in, found before the arguments are read, as an assignment's is.
        // The place is found before the arguments are read, unless none of them
        // can change anything: then its address is an expression, which writes
        // no statement, so the call is no prelude (see `has_prelude`).
        let (first, rest) = match args.split_first() {
            Some((value, rest)) if self.altering.contains(&called) => {
                let quiet = rest.iter().all(|arg| changes_nothing(arg) && !has_prelude(arg));
                (Some(if quiet { self.gen_address(value)? } else { self.gen_receiver(value)? }), rest)
            }
            _ => (None, args),
        };
        let rest: Vec<&Expr> = rest.iter().collect();
        let offset = usize::from(first.is_some());
        let hidden: Vec<bool> = (0..rest.len()).map(|i| self.hidden_of(&called).get(i + offset).copied().unwrap_or(false)).collect();
        // A method that changes the value it is called on may be given a part
        // of it to copy, `c.grow(c.big)`, which it must not see change.
        let mut values = if hidden.contains(&true) { self.gen_hidden_operands(&rest, &hidden, first.is_none())? } else { self.gen_operands(&rest)? };
        if let Some(first) = first {
            values.insert(0, first);
        }
        Ok(format!("{}({})", mangle(&called), values.join(", ")))
    }

    /// `show(a, b, …)`, the expression `e`: each value is written where it is
    /// read, so the values are written, and anything they do happens, left to
    /// right. Every value but the last is written without ending the line,
    /// which the last one does, so `show` of one value is the one call it
    /// always was; `show()` is the end of a line alone.
    ///
    /// A value written by its own `str` makes text, which needs a region: the
    /// line gets one of its own, freed once it ends, so the text lasts exactly
    /// as long as writing it takes (see `check::show_with_str`). Every value is
    /// written as a statement, so the free comes after the last of them.
    fn gen_show(&mut self, e: &Expr, args: &[Expr]) -> Result<String> {
        let shown = args.iter().any(|arg| expr_reads(arg, SHOWN)).then(|| self.shown_region());
        if let Some(region) = &shown {
            self.w.line(&format!("vela__rt__region {}[1] = {{ VELA__RT__REGION_EMPTY }};", c_ident(region)));
            self.shown.push(region.clone());
            // A `<-` inside one of the values leaves the task before the line
            // ends, and frees every region open then, this one with them, so it
            // counts as declared by the block until the line is written.
            if let Some(block) = self.regions.last_mut() {
                block.push(region.clone());
            }
        }
        let written = self.gen_shown(e, args);
        if let Some(region) = &shown {
            self.shown.pop();
            if let Some(block) = self.regions.last_mut() {
                block.pop();
            }
            self.w.line(&format!("vela__rt__region_free({});", c_ident(region)));
        }
        written?;
        Ok(String::new())
    }

    /// The values of a `show`, each written where it is read (see `gen_show`).
    fn gen_shown(&mut self, e: &Expr, args: &[Expr]) -> Result<()> {
        let Some((last, before)) = args.split_last() else {
            self.w.line("vela__rt__show_line();");
            return Ok(());
        };
        for arg in before {
            let value = self.gen_expr(arg)?;
            self.w.line(&self.shown(e, arg.ty()?, &value, false)?);
        }
        let value = self.gen_expr(last)?;
        self.w.line(&self.shown(e, last.ty()?, &value, true)?);
        Ok(())
    }

    /// `int(x)`, `float(x)`, or `unit(x)`, the expression `e`: a value turned
    /// into a number. A number turned into its own type is the value itself, a
    /// bool is 1 or 0 as C makes it, and an int or a unit is a float exactly, as
    /// a unit is an int. A float turned into an int, and a number turned into a
    /// unit, can fail, so only they are checked at runtime.
    fn gen_number_cast(&mut self, e: &Expr, into: &Type, args: &[Expr]) -> Result<String> {
        let [value] = args else {
            return Err(Error::internal_at(e.span, format!("'{}' called with {}", into, args.len())));
        };
        let from = value.ty()?.clone();
        let expr = self.gen_expr(value)?;
        // Text is read by the helper generated for the type it gives, which
        // carries the reason when the text is no number.
        if matches!(from, Type::Text(_)) {
            return Ok(format!("{}({})", read_helper(into), expr));
        }
        Ok(match (into, &from) {
            (Type::Int, Type::Int) | (Type::Float, Type::Float) | (Type::Unit, Type::Unit) => expr,
            (Type::Int, Type::Bool | Type::Unit) => format!("((int64_t){})", parenthesized(&expr)),
            (Type::Unit, Type::Bool) => format!("((uint8_t){})", parenthesized(&expr)),
            (Type::Float, Type::Int | Type::Unit) => format!("((double){})", parenthesized(&expr)),
            (Type::Int, Type::Float) => format!("vela__rt__int_of_float({}, {})", expr, self.location(e.span)),
            (Type::Unit, Type::Int) => format!("vela__rt__unit_of({}, {})", expr, self.location(e.span)),
            (Type::Char, Type::Char) | (Type::Char, Type::Unit) | (Type::Unit, Type::Char) => expr,
            (Type::Int, Type::Char) => format!("((int64_t){})", parenthesized(&expr)),
            (Type::Char, Type::Int) => format!("vela__rt__char_of({}, {})", expr, self.location(e.span)),
            (Type::Unit, Type::Float) => {
                let at = self.location(e.span);
                format!("vela__rt__unit_of(vela__rt__int_of_float({}, {}), {})", expr, at, at)
            }
            _ => return Err(Error::internal_at(e.span, format!("'{}' of a value of type {}", into, from))),
        })
    }

    /// The address of the place a method that changes the value it is called on
    /// is called on, or of one given to a `flex&` parameter, which the checker
    /// has made sure can change.
    fn gen_receiver(&mut self, value: &Expr) -> Result<String> {
        if let ExprKind::Var(name) = &value.kind {
            // The value a method is called on is already a pointer in a method
            // that changes it, and so is a `flex&` parameter, so giving either
            // on passes the pointer.
            if self.pointers.contains(name) {
                return Ok(c_ident(name));
            }
            return Ok(format!("&{}", c_ident(name)));
        }
        let place = self.gen_place(value)?;
        let pointer = self.fresh_temp("place");
        self.w.line(&format!("{}* {} = &{};", c_type(value.ty()?), pointer, place));
        Ok(pointer)
    }

    /// The address of a place that can change, as an expression that writes no
    /// statement of its own unless the place's index has a prelude.
    fn gen_address(&mut self, place: &Expr) -> Result<String> {
        match &place.kind {
            ExprKind::Var(_) => self.gen_receiver(place),
            _ => Ok(format!("&{}", self.gen_place(place)?)),
        }
    }

    /// `-operand` or `not operand`, the expression `e`.
    fn gen_unary(&mut self, e: &Expr, op: UnaryOp, operand: &Expr) -> Result<String> {
        let expr = self.gen_expr(operand)?;
        // `-x` on an int overflows only for the smallest int; a
        // negated literal can't be it.
        if op == UnaryOp::Neg && *operand.ty()? == Type::Int && int_constant(operand).is_none() {
            return Ok(format!("vela__rt__neg({}, {})", expr, self.location(e.span)));
        }
        let op_s = match op {
            UnaryOp::Neg => "-",
            UnaryOp::Not => "!",
        };
        Ok(format!("({}{})", op_s, expr))
    }

    /// The variant `tag` of `kind` with its payload `args`, the expression `e`.
    fn gen_construct(&mut self, e: &Expr, kind: &str, tag: &str, args: &[Expr]) -> Result<String> {
        let span = e.span;
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

    /// `new T(value) at region`, the expression `e`: a pointer, or a slice.
    fn gen_new(&mut self, e: &Expr, ty: &Type, value: &Expr, region: &str) -> Result<String> {
        let span = e.span;
        if let Type::Map(key, item, _) = ty {
            let map = e.ty()?.without_regions();
            let ExprKind::Pairs(pairs) = &value.kind else {
                // Another map, copied into the region, its keys and values too.
                let source = self.gen_expr(value)?;
                return Ok(format!("{}({}, {})", copy_helper("copied", value.ty()?), c_ident(region), source));
            };
            if pairs.is_empty() {
                return Ok(format!("{}({}, 0)", item_helper("map_new", &map), c_ident(region)));
            }
            let all: Vec<&Expr> = pairs.iter().flat_map(|(key, value)| [key, value]).collect();
            let written = self.gen_operands(&all)?;
            let keys: Vec<&str> = written.iter().step_by(2).map(String::as_str).collect();
            let values: Vec<&str> = written.iter().skip(1).step_by(2).map(String::as_str).collect();
            return Ok(format!(
                "{}({}, {}, ({}[]){{ {} }}, ({}[]){{ {} }})",
                item_helper("map_of", &map),
                c_ident(region),
                pairs.len(),
                c_type(key),
                keys.join(", "),
                c_type(item),
                values.join(", ")
            ));
        }
        if matches!(ty, Type::Slice(..)) {
            let slice = e.ty()?;
            if matches!(&value.kind, ExprKind::Array(values) if values.is_empty()) {
                // No items, and no room until the first `add`. The region is
                // still named, so a region parameter counts as used.
                return Ok(format!("((void){}, ({}){{ .items = vela__rt__no_items(), .len = 0 }})", c_ident(region), c_type(slice)));
            }
            if let ExprKind::Repeat { value: item, count } = &value.kind {
                let [item, count] = <[String; 2]>::try_from(self.gen_operands(&[item, count])?)
                    .map_err(|_| Error::internal_at(span, "a repeated value without its count"))?;
                return Ok(format!("{}({}, {}, {}, {})", item_helper("fill", slice), c_ident(region), count, item, self.location(span)));
            }
            let source = self.gen_expr(value)?;
            let source = match value.ty()? {
                // A view of the array, which lives until the copy is made.
                // The array may be read through a hidden `const` pointer; the
                // copy only reads the view.
                Type::Array(_, count) => format!(
                    "(({}){{ .items = ({}*)({}).items, .len = INT64_C({}) }})",
                    c_type(slice),
                    c_type(item_type(slice)),
                    source,
                    count
                ),
                _ => source,
            };
            // A slice made of another shares nothing with it, at any depth;
            // one made of the values written holds them as they are.
            if !matches!(value.kind, ExprKind::Array(_)) && self.copies_deep(slice) {
                return Ok(format!("{}({}, {})", copy_helper("copied", slice), c_ident(region), source));
            }
            return Ok(format!("{}({}, {})", item_helper("copy", slice), c_ident(region), source));
        }
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

    /// A shape literal of `name`.
    fn gen_struct_init(&mut self, name: &str, fields: &[FieldInit]) -> Result<String> {
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

    /// `xs[i]` or `xs[a..b]`, the expression `e`. `place` is the array `base`
    /// names as a place, already generated, when `e` is assigned to.
    fn gen_index(&mut self, e: &Expr, base: &Expr, index: &Expr, place: Option<String>) -> Result<String> {
        if let ExprKind::Range(from, to) = &index.kind {
            return self.gen_slice(e, base, from, to);
        }
        let at = self.location(e.span);
        match base.ty()? {
            Type::Array(_, count) => {
                let constant = int_constant(index);
                let (array, index) = match place {
                    Some(array) => (array, self.gen_expr(index)?),
                    None => {
                        let [array, index] = <[String; 2]>::try_from(self.gen_operands(&[base, index])?)
                            .map_err(|_| Error::internal_at(e.span, "an index without its array"))?;
                        (array, index)
                    }
                };
                // The checker has made sure a constant index is in range.
                let index = match constant {
                    Some(constant) => constant,
                    None => format!("vela__rt__index({}, INT64_C({}), {})", index, count, at),
                };
                Ok(format!("({}).items[{}]", array, index))
            }
            Type::Slice(..) => {
                let [slice, index] = <[String; 2]>::try_from(self.gen_operands(&[base, index])?)
                    .map_err(|_| Error::internal_at(e.span, "an index without its slice"))?;
                Ok(format!("(*{}({}, {}, {}))", item_helper("at", base.ty()?), slice, index, at))
            }
            Type::Text(_) => {
                let [text, index] = <[String; 2]>::try_from(self.gen_operands(&[base, index])?)
                    .map_err(|_| Error::internal_at(e.span, "an index without its text"))?;
                Ok(format!("vela__rt__text_at({}, {}, {})", text, index, at))
            }
            // Where the key's value is in the map's table: a place too.
            Type::Map(..) => {
                let helper = item_helper("map_at", &base.ty()?.without_regions());
                let [map, key] = <[String; 2]>::try_from(self.gen_operands(&[base, index])?)
                    .map_err(|_| Error::internal_at(e.span, "a key without its map"))?;
                Ok(format!("(*{}({}, {}, {}))", helper, map, key, at))
            }
            other => Err(Error::internal_at(e.span, format!("an index into a value of type {}", other))),
        }
    }

    /// `base[from..to]`, the expression `e`: a slice, or a part of text.
    fn gen_slice(&mut self, e: &Expr, base: &Expr, from: &Expr, to: &Expr) -> Result<String> {
        let at = self.location(e.span);
        if matches!(base.ty()?, Type::Text(_)) {
            let [text, from, to] = <[String; 3]>::try_from(self.gen_operands(&[base, from, to])?)
                .map_err(|_| Error::internal_at(e.span, "a part of text without two bounds"))?;
            return Ok(format!("vela__rt__text_part({}, {}, {}, {})", text, from, to, at));
        }
        let slice = e.ty()?;
        let source = match base.ty()? {
            Type::Array(_, count) => {
                // The array is in a region: the slice points at its items there.
                let array = self.gen_place(base)?;
                let items = if has_prelude(from) || has_prelude(to) {
                    let items = self.fresh_temp("items");
                    self.w.line(&format!("{}* {} = ({}).items;", c_type(item_type(slice)), items, array));
                    items
                } else {
                    format!("({}).items", array)
                };
                let [from, to] = <[String; 2]>::try_from(self.gen_operands(&[from, to])?)
                    .map_err(|_| Error::internal_at(e.span, "a slice without two bounds"))?;
                let view = format!("(({}){{ .items = {}, .len = INT64_C({}) }})", c_type(slice), items, count);
                return Ok(format!("{}({}, {}, {}, {})", item_helper("part", slice), view, from, to, at));
            }
            _ => base,
        };
        let [source, from, to] = <[String; 3]>::try_from(self.gen_operands(&[source, from, to])?)
            .map_err(|_| Error::internal_at(e.span, "a slice without two bounds"))?;
        Ok(format!("{}({}, {}, {}, {})", item_helper("part", slice), source, from, to, at))
    }

    /// A place an assignment writes to, as a C lvalue. An item of an array is
    /// part of the array's own place, which is found before the index's
    /// prelude runs, if it has one.
    fn gen_place(&mut self, e: &Expr) -> Result<String> {
        match &e.kind {
            ExprKind::Field(base, field) if matches!(base.ty, Some(Type::Named(..))) => {
                Ok(format!("({}).{}", self.gen_place(base)?, c_ident(field)))
            }
            ExprKind::Index(base, index) if matches!(base.ty, Some(Type::Array(..))) && !matches!(index.kind, ExprKind::Range(..)) => {
                let mut array = self.gen_place(base)?;
                if has_prelude(index) {
                    let pointer = self.fresh_temp("place");
                    self.w.line(&format!("{}* {} = &{};", c_type(base.ty()?), pointer, array));
                    array = format!("*{}", pointer);
                }
                self.gen_index(e, base, index, Some(array))
            }
            _ => self.gen_expr(e),
        }
    }

    /// A value becoming part of a `maybe` or `T ~ E`, the type of `e`.
    /// A value of a kind made a value of a kind imitating it (see
    /// `ExprKind::Widen`), by the function that does so for the two (see
    /// `gen_widens`).
    fn gen_widen(&mut self, e: &Expr, inner: &Expr) -> Result<String> {
        let helper = widen_helper(inner.ty()?, e.ty()?);
        Ok(format!("{}({})", helper, self.gen_expr(inner)?))
    }

    /// `a |> f(b)` (see `ExprKind::Pipe`): the value first, in a temporary
    /// the call's `Piped` is written as, then the call. Where the value can
    /// fail, the call runs only when it succeeded, and the pipe fails with its
    /// error otherwise, each error made a value of the one the pipe fails with
    /// where they differ (see `pipe_widens`).
    fn gen_pipe(&mut self, e: &Expr, call: &Expr) -> Result<String> {
        let value = piped_value(call).ok_or_else(|| Error::internal_at(e.span, "a pipe giving its value to no call"))?;
        let value_ty = value.ty()?.clone();
        let value_c = self.gen_expr(value)?;
        let piped = self.fresh_temp("piped");
        self.w.line(&format!("{} {} = {};", c_type(&value_ty), piped, value_c));
        let Type::Or(_, before) = &value_ty else {
            self.piped.push(piped);
            let called = self.gen_expr(call);
            self.piped.pop();
            return called;
        };
        let result_ty = e.ty()?.clone();
        let Type::Or(_, error_ty) = &result_ty else { return Err(Error::internal_at(e.span, format!("a pipe that can fail of type {}", result_ty))) };
        let ctype = c_type(&result_ty);
        let result = self.fresh_temp("value");
        self.w.line(&format!("{} {};", ctype, result));
        self.w.open(&format!("if ({}.ok)", piped));
        self.piped.push(format!("{}.value", piped));
        let called = self.gen_expr(call);
        self.piped.pop();
        let called = called?;
        match call.ty()? {
            Type::Or(_, after) if after.without_regions() == error_ty.without_regions() => self.w.line(&format!("{} = {};", result, called)),
            Type::Or(sent, after) => {
                let step = self.fresh_temp("step");
                self.w.line(&format!("{} {} = {};", c_type(call.ty()?), step, called));
                self.w.open(&format!("if ({}.ok)", step));
                match **sent {
                    Type::Nothing => self.w.line(&format!("{} = ({}){{ .ok = true }};", result, ctype)),
                    _ => self.w.line(&format!("{} = ({}){{ .ok = true, .value = {}.value }};", result, ctype, step)),
                }
                self.w.reopen("else");
                self.w.line(&format!("{} = ({}){{ .ok = false, .error = {}({}.error) }};", result, ctype, widen_helper(after, error_ty), step));
                self.w.close();
            }
            Type::Nothing => {
                // A call giving nothing is a statement of its own.
                self.w.line(&format!("{};", called));
                self.w.line(&format!("{} = ({}){{ .ok = true }};", result, ctype));
            }
            _ => self.w.line(&format!("{} = ({}){{ .ok = true, .value = {} }};", result, ctype, called)),
        }
        self.w.reopen("else");
        let error = match before.without_regions() == error_ty.without_regions() {
            true => format!("{}.error", piped),
            false => format!("{}({}.error)", widen_helper(before, error_ty), piped),
        };
        self.w.line(&format!("{} = ({}){{ .ok = false, .error = {} }};", result, ctype, error));
        self.w.close();
        Ok(result)
    }

    fn gen_wrap(&mut self, e: &Expr, inner: &Expr) -> Result<String> {
        let target = e.ty()?;
        let ctype = c_type(target);
        match target {
            Type::Maybe(pointer) if is_pointer(pointer) => match inner.ty()? {
                Type::None => {
                    self.work_out_none(inner)?;
                    Ok("NULL".to_string())
                }
                _ => self.gen_expr(inner),
            },
            Type::Maybe(_) => match inner.ty()? {
                Type::None => {
                    self.work_out_none(inner)?;
                    Ok(format!("(({}){{ .has = false }})", ctype))
                }
                _ => Ok(format!("(({}){{ .has = true, .value = {} }})", ctype, self.gen_expr(inner)?)),
            },
            Type::Or(value, _) => {
                if let ExprKind::Fail(error) = &inner.kind {
                    return Ok(format!("(({}){{ .ok = false, .error = {} }})", ctype, self.gen_expr(error)?));
                }
                if **value == Type::Nothing {
                    return Err(Error::internal_at(e.span, "a value wrapped as the success of 'nothing ~ E'"));
                }
                Ok(format!("(({}){{ .ok = true, .value = {} }})", ctype, self.gen_expr(inner)?))
            }
            other => Err(Error::internal_at(e.span, format!("a value wrapped as {}", other))),
        }
    }

    /// Works out, for what it does, the tuple of `none` read past its end as
    /// `t.#k` (see `Checker::check_safe_item`), a stop included; `none` itself
    /// is nothing to.
    fn work_out_none(&mut self, none: &Expr) -> Result<()> {
        if let ExprKind::Field(tuple, _) = &none.kind
            && !without_effects(tuple)
        {
            let value = self.gen_expr(tuple)?;
            self.w.line(&format!("(void)({});", value));
        }
        Ok(())
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
        // A plan B giving `none` made the whole a `maybe` of what the value
        // holds: the value when it is there, wrapped.
        let held = match &value_ty {
            Type::Maybe(inner) | Type::Or(inner, _) => &**inner,
            _ => &value_ty,
        };
        let extracted = match &result_ty {
            // What the value holds may be a maybe itself, when a generic filled
            // one in (`maybe T` with `T` a `maybe int`).
            Type::Maybe(inner) if inner.without_regions() == held.without_regions() => match (&value_ty, is_pointer(inner)) {
                (Type::Maybe(_), _) => subject.clone(),
                (_, true) => format!("{} ? {} : NULL", present, extracted),
                (_, false) => {
                    let ctype = c_type(&result_ty);
                    format!("{} ? ({}){{ .has = true, .value = {} }} : ({}){{ .has = false }}", present, ctype, extracted, ctype)
                }
            },
            _ => extracted,
        };
        let result = if result_ty == Type::Nothing {
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
        // The value is stored before the block's regions are freed.
        match result.is_empty() {
            false => self.gen_value_block(body, &result)?,
            true => self.gen_block(body)?,
        }
        self.w.close();
        Ok(result)
    }

    /// A binary operation without surrounding parentheses.
    fn gen_binary(&mut self, e: &Expr, op: BinaryOp, l: &Expr, r: &Expr) -> Result<String> {
        // The code generator recurses through operands, so what each kind of
        // operation writes is in a function of its own, keeping little on the
        // stack here.
        if matches!(op, BinaryOp::And | BinaryOp::Or) && has_prelude(r) {
            return self.gen_short_circuit(op, l, r);
        }
        let refs = [l, r].into_iter().any(|side| matches!(side.kind, ExprKind::Ref(_)));
        if matches!(op, BinaryOp::Eq | BinaryOp::Ne) && !refs && self.by_place(l.ty()?) {
            return self.gen_place_equality(op, l, r);
        }
        let [left, right] = <[String; 2]>::try_from(self.gen_operands(&[l, r])?)
            .map_err(|_| Error::internal_at(e.span, "a binary operation without two operands"))?;
        self.binary_of(e, op, l, r, left, right)
    }

    /// `l and r` or `l or r`, where `r` has a prelude, which must only run
    /// when `l` doesn't decide the result.
    fn gen_short_circuit(&mut self, op: BinaryOp, l: &Expr, r: &Expr) -> Result<String> {
        let left = self.gen_expr(l)?;
        let result = self.fresh_temp("cond");
        self.w.line(&format!("bool {} = {};", result, left));
        let test = if op == BinaryOp::And { result.clone() } else { format!("!{}", result) };
        self.w.open(&format!("if ({})", test));
        let right = self.gen_expr(r)?;
        self.w.line(&format!("{} = {};", result, right));
        self.w.close();
        Ok(result)
    }

    /// `l == r` or `l != r` of an array, a shape, a tuple, or a kind carrying
    /// one, compared by where it is: both sides are places (the checker made
    /// sure), read as their addresses rather than as values, which could be
    /// copies.
    fn gen_place_equality(&mut self, op: BinaryOp, l: &Expr, r: &Expr) -> Result<String> {
        let not = if op == BinaryOp::Ne { "!" } else { "" };
        let left = self.gen_address(l)?;
        let right = self.gen_address(r)?;
        if self.is_kind(l.ty()?) {
            return Ok(format!("{}{}({}, {})", not, copy_helper("same", l.ty()?), left, right));
        }
        let symbol = if op == BinaryOp::Eq { "==" } else { "!=" };
        let (left, right) = (format!("(const void*)({})", left), format!("(const void*)({})", right));
        Ok(format!("({} {} {})", left, symbol, other_side(&left, right, "const void*")))
    }

    /// `l op r` in C, given `left` and `right`, the C of its operands.
    fn binary_of(&mut self, e: &Expr, op: BinaryOp, l: &Expr, r: &Expr, left: String, right: String) -> Result<String> {
        let not = if matches!(op, BinaryOp::Ne | BinaryOp::NotSimilar) { "!" } else { "" };
        if matches!(op, BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Similar | BinaryOp::NotSimilar) {
            // `m == none` tests a maybe.
            let maybe = match (l.ty()?, r.ty()?) {
                (Type::Maybe(inner), Type::None) => Some((inner, &left)),
                (Type::None, Type::Maybe(inner)) => Some((inner, &right)),
                _ => None,
            };
            if let Some((inner, value)) = maybe {
                let empty = matches!(op, BinaryOp::Eq | BinaryOp::Similar);
                return Ok(match (is_pointer(inner), empty) {
                    (true, true) => format!("{} == NULL", value),
                    (true, false) => format!("{} != NULL", value),
                    (false, true) => format!("!{}.has", value),
                    (false, false) => format!("{}.has", value),
                });
            }
        }
        // `&a == &b` and `&a == p`: where the places are. A place that never
        // changes is `const` in C, so both sides are read as addresses alone.
        if matches!(op, BinaryOp::Eq | BinaryOp::Ne) && [l, r].into_iter().any(|side| matches!(side.kind, ExprKind::Ref(_))) {
            let symbol = if op == BinaryOp::Eq { "==" } else { "!=" };
            let (left, right) = (format!("(const void*)({})", left), format!("(const void*)({})", right));
            return Ok(format!("({} {} {})", left, symbol, other_side(&left, right, "const void*")));
        }
        // A value compared by a function of its own is given to it by address,
        // a copy in a compound literal that lasts as long as the expression.
        let by_address = |value: &str, ty: &Type| format!("({} const[1]){{ {} }}", c_type(ty), value);
        if matches!(op, BinaryOp::Eq | BinaryOp::Ne) && (self.is_kind(l.ty()?) || matches!(l.ty()?, Type::Slice(..))) {
            let ty = l.ty()?;
            return Ok(format!("{}{}({}, {})", not, copy_helper("same", ty), by_address(&left, ty), by_address(&right, ty)));
        }
        // Text compares by contents, not by address, alike or similar.
        if matches!(l.ty()?, Type::Text(_)) && !matches!(op, BinaryOp::Lt | BinaryOp::Gt | BinaryOp::Le | BinaryOp::Ge) {
            return Ok(format!("{}vela__rt__text_eq({}, {})", not, left, right));
        }
        if matches!(op, BinaryOp::Similar | BinaryOp::NotSimilar) {
            let ty = l.ty()?;
            let symbol = if not.is_empty() { "==" } else { "!=" };
            return Ok(match ty {
                // A number, a bool, and a pointer are similar when equal.
                Type::Int | Type::Unit | Type::Char | Type::Float | Type::Bool | Type::Ptr(..) | Type::Task(_) => format!("({} {} {})", left, symbol, other_side(&left, right, &c_type(ty))),
                Type::Maybe(inner) if is_pointer(inner) => format!("({} {} {})", left, symbol, other_side(&left, right, &c_type(ty))),
                _ => format!("{}{}({}, {})", not, copy_helper("similar", ty), by_address(&left, ty), by_address(&right, ty)),
            });
        }
        if matches!(l.ty()?, Type::Text(_)) {
            // Text is ordered by its bytes.
            let symbol = match op {
                BinaryOp::Lt => "<",
                BinaryOp::Gt => ">",
                BinaryOp::Le => "<=",
                BinaryOp::Ge => ">=",
                other => return Err(Error::internal_at(e.span, format!("text operator {:?}", other))),
            };
            return Ok(format!("vela__rt__text_order({}, {}) {} 0", left, right, symbol));
        }
        // `**`: an int or unit power is exact and checked, and any other is C's `pow`.
        if op == BinaryOp::Pow {
            // `math.e ** x` is `exp(x)`, which C computes more closely than `pow`.
            if matches!(&l.kind, ExprKind::Macro { name, .. } if name == crate::library::MATH_E) {
                return Ok(format!("exp((double){})", parenthesized(&right)));
            }
            let at = self.location(e.span);
            return Ok(match e.ty()? {
                Type::Int => format!("vela__rt__pow({}, (int64_t){}, {})", left, parenthesized(&right), at),
                Type::Unit => format!("vela__rt__unit_pow({}, {}, {})", left, right, at),
                _ => format!("pow((double){}, (double){})", parenthesized(&left), parenthesized(&right)),
            });
        }
        // A char moved by an int is computed as an int, and checked to be a
        // char; the distance between two chars is an int, computed below.
        if *e.ty()? == Type::Char {
            let at = self.location(e.span);
            let helper = if op == BinaryOp::Add { "add" } else { "sub" };
            return Ok(format!("vela__rt__char_of(vela__rt__{}({}, {}, {}), {})", helper, left, right, at, at));
        }
        // Units are computed as ints, where no result of two units overflows,
        // and the result is checked to fit in a unit.
        if *e.ty()? == Type::Unit {
            let at = self.location(e.span);
            let whole = match op {
                BinaryOp::Div => format!("vela__rt__div({}, {}, {})", left, right, at),
                BinaryOp::Mod => format!("vela__rt__mod({}, {}, {})", left, right, at),
                BinaryOp::Add => format!("(int64_t){} + (int64_t){}", parenthesized(&left), parenthesized(&right)),
                BinaryOp::Sub => format!("(int64_t){} - (int64_t){}", parenthesized(&left), parenthesized(&right)),
                BinaryOp::Mul => format!("(int64_t){} * (int64_t){}", parenthesized(&left), parenthesized(&right)),
                other => return Err(Error::internal_at(e.span, format!("unit operator {:?}", other))),
            };
            return Ok(format!("vela__rt__unit_of({}, {})", whole, at));
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
            BinaryOp::Pow => return Err(Error::internal_at(e.span, "a power that is no function")),
            BinaryOp::Eq => "==",
            BinaryOp::Similar | BinaryOp::NotSimilar => return Err(Error::internal_at(e.span, "a similarity that is no function")),
            BinaryOp::Ne => "!=",
            BinaryOp::Lt => "<",
            BinaryOp::Gt => ">",
            BinaryOp::Le => "<=",
            BinaryOp::Ge => ">=",
            BinaryOp::And => "&&",
            BinaryOp::Or => "||",
        };
        let right = match op {
            BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Gt | BinaryOp::Le | BinaryOp::Ge => other_side(&left, right, &c_type(r.ty()?)),
            _ => right,
        };
        Ok(format!("{} {} {}", left, op_s, right))
    }

    /// Where an operation is, for a runtime message: a C string literal
    /// `"LINE:COL in task 'NAME'"`.
    fn location(&self, span: Span) -> String {
        match &self.place {
            Some(place) => format!("\"{}:{} in {}\"", span.line, span.col, escape_c_string(place)),
            // A lambda has no name a program writes; its place says which.
            None if is_lambda(&self.task) => format!("\"{}:{} in a lambda\"", span.line, span.col),
            None => format!("\"{}:{} in task '{}'\"", span.line, span.col, escape_c_string(&self.task)),
        }
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

/// The right side of a comparison, `right`, of C type `c_ty`: as it is, or,
/// when it is written as the left side is (`x == x`), as a compound literal
/// of it. A value compared with itself is a question Vela may ask (a float
/// that isn't a number is unequal to itself), and C's warning about a
/// comparison that is always true doesn't read through a compound literal.
fn other_side(left: &str, right: String, c_ty: &str) -> String {
    if left == right { format!("({}){{ {} }}", c_ty, right) } else { right }
}

/// `text` in parentheses, unless it is a plain name or number.
fn parenthesized(text: &str) -> String {
    let simple = text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if simple { text.to_string() } else { format!("({})", text) }
}

fn is_pointer(ty: &Type) -> bool {
    matches!(ty, Type::Ptr(..))
}

/// The types of the `wanted` values `values` give: one each, or the several a
/// call sends, or a tuple's items.
fn given_types(wanted: usize, values: &[Expr], span: Span) -> Result<Vec<Type>> {
    if values.len() == wanted {
        return values.iter().map(|value| value.ty().cloned()).collect();
    }
    match values {
        [one] => match one.ty()? {
            Type::Results(types) | Type::Tuple(types) => Ok(types.clone()),
            other => Err(Error::internal_at(span, format!("a value of type {} received by {} names", other, wanted))),
        },
        _ => Err(Error::internal_at(span, format!("{} values received by {} names", values.len(), wanted))),
    }
}

/// The names `names` binds from the value `value` of type `ty`, each with the
/// C expression that reads it: one name takes the value, and `(a, b)` reads a
/// tuple's items.
fn bound_names<'p>(names: &'p Names, ty: &Type, value: String, bindings: &mut Vec<(&'p str, Type, String)>) {
    let Type::Tuple(items) = ty else {
        bindings.push((&names.name, ty.clone(), value));
        return;
    };
    if names.parts.is_empty() {
        bindings.push((&names.name, ty.clone(), value));
        return;
    }
    for (i, (part, item)) in names.parts.iter().zip(items).enumerate() {
        bound_names(part, item, format!("{}.{}", value, tuple_member(i)), bindings);
    }
}

/// The C member holding item `i` of a tuple: `i0`, `i1`, ….
fn tuple_member(i: usize) -> String {
    format!("i{}", i)
}

fn result_member(i: usize) -> String {
    format!("r{}", i)
}

/// The success of a `nothing ~ E` task, as a C compound literal.
fn success(ret: &Type) -> String {
    format!("(({}){{ .ok = true }})", c_type(ret))
}

/// The runtime task that writes a value of builtin type `ty`: the one that
/// ends the line when `ends`, which the last value `show` is given does.
fn written_helper(ty: &Type, ends: bool) -> String {
    let name = match ty {
        Type::Int | Type::Unit => "int",
        Type::Char => "char",
        Type::Float => "float",
        Type::Bool => "bool",
        _ => "text",
    };
    format!("vela__rt__{}_{}", if ends { "show" } else { "write" }, name)
}

impl Generator {
    /// The statement `show` writes a value `value` of type `ty` with, ending the
    /// line when `ends`: a number, a bool, or text by the runtime's own helper, and
    /// anything else through a writer to the output (see `Generator::gen_writer`).
    fn shown(&self, e: &Expr, ty: &Type, value: &str, ends: bool) -> Result<String> {
        let written = match ty {
            Type::Int | Type::Unit | Type::Char | Type::Float | Type::Bool | Type::Text(_) => return Ok(format!("{}({});", written_helper(ty, ends), value)),
            Type::Ptr(..) => format!("vela__rt__put_address(&VELA__RT__OUTPUT, (const void*){});", value),
            Type::Task(_) | Type::Closure(_) => format!("(void)({}); VELA__RT__PUT(&VELA__RT__OUTPUT, \"{}\");", value, self.task_text(ty)),
            Type::Array(..) | Type::Slice(..) | Type::Map(..) | Type::Tuple(_) | Type::Maybe(_) | Type::Named(..) => {
                format!("{}(&VELA__RT__OUTPUT, {});", write_helper(&ty.without_regions()), value)
            }
            other => return Err(Error::internal_at(e.span, format!("show called with type {}", other))),
        };
        Ok(if ends { format!("{} vela__rt__show_line();", written) } else { written })
    }

    /// What a task value is written as: its type, `task (int) -> int`, with each
    /// shape or kind by its own name. A value can't say which task it holds.
    fn task_text(&self, ty: &Type) -> String {
        format!("task {}", ty.map_names(&|name| self.written_name(name)))
    }

    /// The statement that writes `value`, of type `ty`, inside another value: text
    /// in quotes, a pointer as its address, and anything but a number or a bool
    /// by its type's `write`.
    fn put_item(&self, ty: &Type, value: &str) -> String {
        match ty {
            Type::Int | Type::Unit => format!("vela__rt__put_int(w, {});", value),
            Type::Char => format!("vela__rt__put_char(w, {});", value),
            Type::Float => format!("vela__rt__put_float(w, {});", value),
            Type::Bool => format!("vela__rt__put_bool(w, {});", value),
            Type::Text(_) => format!("vela__rt__put_quoted(w, {});", value),
            Type::Ptr(..) => format!("vela__rt__put_address(w, (const void*){});", value),
            Type::Task(_) | Type::Closure(_) => format!("(void)({}); VELA__RT__PUT(w, \"{}\");", value, self.task_text(ty)),
            _ => format!("{}(w, {});", write_helper(&ty.without_regions()), value),
        }
    }
}

/// The task the method `str` of the shape or kind `owner` is, when it gives
/// one, of the tasks `exists` knows: a shape made for the types `given` has
/// the `str` of the shape it was made from, made for them (`Box__of__int`
/// has `Box__on__str__of__int`).
fn str_task(owner: &str, given: &[Type], exists: impl Fn(&str) -> bool) -> Option<String> {
    let own = method_name(owner, STR);
    if exists(&own) {
        return Some(own);
    }
    let source = owner.split("__of__").next().filter(|_| !given.is_empty())?;
    let made = instance_name(&method_name(source, STR), given);
    exists(&made).then_some(made)
}

/// How values are laid out in the C the generator writes, from the shapes and
/// kinds a program declares: shared with the checker, which says in a warning
/// how a parameter is given (see `hidden_by_size`).
pub struct Layout<'a> {
    pub shapes: &'a HashMap<String, ShapeDef>,
    pub kinds: &'a HashMap<String, KindDef>,
}

impl Layout<'_> {
    /// The size and alignment, in bytes, of a value of `ty` in the C the
    /// generator writes, as a 64-bit target lays it out.
    pub fn of(&self, ty: &Type) -> (usize, usize) {
        match ty {
            Type::Int | Type::Float | Type::Ptr(..) | Type::None | Type::Region | Type::Task(_) => (8, 8),
            // Its function and what it keeps.
            Type::Closure(_) => (16, 8),
            Type::Maybe(inner) if is_pointer(inner) => (8, 8),
            Type::Bool | Type::Unit | Type::Char => (1, 1),
            Type::Nothing => (0, 1),
            Type::Text(_) => (16, 8),
            Type::Slice(..) => (24, 8),
            Type::Fail(error) => self.of(error),
            Type::Array(items, count) => {
                let (size, align) = self.of(items);
                (size.saturating_mul(usize::try_from(*count).unwrap_or(usize::MAX)), align)
            }
            Type::Maybe(inner) => self.struct_layout([&Type::Bool, inner.as_ref()]),
            Type::Or(value, error) if **value == Type::Nothing => self.struct_layout([&Type::Bool, error.as_ref()]),
            Type::Or(value, error) => self.struct_layout([&Type::Bool, value.as_ref(), error.as_ref()]),
            Type::Results(types) | Type::Tuple(types) => self.struct_layout(types.iter()),
            Type::Named(name, _) => {
                if let Some(shape) = self.shapes.get(name) {
                    if shape.fields.is_empty() {
                        return (1, 1);
                    }
                    return self.struct_layout(shape.fields.iter().map(|field| &field.ty));
                }
                let Some(kind) = self.kinds.get(name) else { return (8, 8) };
                // The tag, an enum, and the union of the variants' values.
                let (mut size, mut align) = (0, 1);
                for variant in &kind.variants {
                    if !variant.payload.is_empty() {
                        let (s, a) = self.struct_layout(variant.payload.iter().map(|field| &field.ty));
                        size = size.max(s);
                        align = align.max(a);
                    }
                }
                let tag: usize = 4;
                let align = align.max(4);
                let data = tag.div_ceil(align) * align;
                ((data + size).div_ceil(align) * align, align)
            }
            _ => (8, 8),
        }
    }

    /// The size and alignment of a C struct of members of the `types`.
    fn struct_layout<'a>(&self, types: impl IntoIterator<Item = &'a Type>) -> (usize, usize) {
        let (mut size, mut align) = (0usize, 1usize);
        for ty in types {
            let (s, a) = self.of(ty);
            size = size.div_ceil(a) * a + s;
            align = align.max(a);
        }
        (size.div_ceil(align) * align, align)
    }
}

/// Whether a parameter the compiler chooses how to give (`x: T`) is given by
/// where the caller keeps it, for a value of `size` bytes: one larger than
/// `HIDDEN_ABOVE`.
pub fn hidden_by_size(size: usize) -> bool {
    size > HIDDEN_ABOVE
}

/// The generated function that puts a value of type `ty` in a writer.
fn write_helper(ty: &Type) -> String {
    format!("vela__write__{}", type_word(ty))
}

/// The generated function that makes a value of type `ty` into text in a
/// region, `x.str() at r`.
/// The C type a value of `ty` is given to its `write` and `text` helpers as:
/// its own, but an address for a pointer, which is only written, and may be
/// the address of a place that never changes (`"{&x}"`).
fn written_c_type(ty: &Type) -> String {
    match ty {
        Type::Ptr(..) => "const void*".to_string(),
        _ => c_type(ty),
    }
}

/// A task as a notice names it: alone when the program's own file has it, and
/// after its module's name otherwise (`web.route`, for `lib/web`), so the same
/// task name in two modules can be told apart, however a file includes them.
pub(crate) fn notice_name(name: &str) -> String {
    let name = shown_name(name);
    match name.rsplit_once(MODULE_SEPARATOR) {
        Some((module, alone)) => format!("{}{}{}", module.rsplit_once('/').map_or(module, |(_, last)| last), MODULE_SEPARATOR, alone),
        None => name,
    }
}

/// The C type a task value's function takes parameter `ty` as, given `how`
/// (see `Taken`): a place as a pointer, one only read to a `const` place, a
/// region as the region, and a value as itself, or a pointer to it where it is
/// `hidden`.
fn task_param_c(ty: &Type, how: &Taken, hidden: bool) -> String {
    match how {
        Taken::Place(Passing::FlexRef | Passing::BindRef) => format!("{}*", c_type(ty)),
        Taken::Place(_) => format!("{} const*", c_type(ty)),
        _ if hidden => format!("const {}*", c_type(ty)),
        _ => c_type(ty),
    }
}

/// A task named in a runtime message as the program writes it: a method
/// through the shape or kind it belongs to, and a wrapper's layer as the task
/// it wraps (see `ast::layer_name`).
fn shown_name(name: &str) -> String {
    let name = name.split(LAYER).next().unwrap_or(name);
    // A copy made for the types a use gives is named as the task it copies.
    let name = name.split("__of__").next().unwrap_or(name);
    match method_parts(name) {
        Some((owner, method)) => format!("{}{}{}", owner, MODULE_SEPARATOR, method),
        None => name.to_string(),
    }
}

/// The C function making a value of type `ty` text in a region:
/// `f(region, value)`.
fn text_function(ty: &Type) -> String {
    match ty {
        Type::Int | Type::Unit => "vela__rt__int_str".to_string(),
        Type::Char => "vela__rt__char_str".to_string(),
        Type::Float => "vela__rt__float_str".to_string(),
        Type::Bool => "vela__rt__bool_str".to_string(),
        Type::Text(_) => "vela__rt__text_str".to_string(),
        // A pointer is its address, written as a writer does.
        other => text_helper(&other.without_regions()),
    }
}

fn text_helper(ty: &Type) -> String {
    format!("vela__text__{}", type_word(ty))
}

/// The generated task that reads a number of type `ty` from text: `int("42")`,
/// `float("2.5")`, and `unit("7")`.
fn read_helper(ty: &Type) -> String {
    format!("vela__read__{}", type_word(ty))
}

/// The generated task that reads a number of type `ty` from input:
/// `scan("Age: ", int)`.
fn scan_helper(ty: &Type) -> String {
    format!("vela__scan__{}", type_word(ty))
}


/// Whether the expression contains a plan B, a change to a slice, or a `scan`,
/// so generating it writes a prelude.
fn has_prelude(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Spread(inner) => has_prelude(inner),
        ExprKind::PlanB { .. } | ExprKind::Choice(_) | ExprKind::LoopValue(_) | ExprKind::Edit { .. } | ExprKind::Scan { .. } | ExprKind::Pipe(_) => true,
        // A temporary its pipe wrote before the call.
        ExprKind::Piped(_) => false,
        ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::CharLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) | ExprKind::NoneLit | ExprKind::Macro { .. }
        | ExprKind::Var(_) | ExprKind::RegionAfter(_) | ExprKind::Shared { .. } | ExprKind::TypeOf(_) | ExprKind::TaskValue(_) | ExprKind::Closure { .. } => false,
        ExprKind::Kept { value, .. } => has_prelude(value),
        ExprKind::Invoke { callee, args } => {
            let all: Vec<&Expr> = std::iter::once(callee.as_ref()).chain(args).collect();
            all.iter().any(|e| has_prelude(e)) || ordered(&all)
        }
        // `t.#5` past the end of a tuple that changes something to work out:
        // a statement works it out (see `work_out_none`).
        ExprKind::Field(inner, item) if item.starts_with(check::SAFE_ITEM) && e.ty == Some(Type::None) && !without_effects(inner) => true,
        ExprKind::Field(inner, _) | ExprKind::Unary(_, inner) | ExprKind::Deref(inner) | ExprKind::Ref(inner) | ExprKind::New { value: inner, .. }
        | ExprKind::Fail(inner) | ExprKind::Wrap(inner) | ExprKind::Widen(inner) | ExprKind::Copy { value: inner, .. } | ExprKind::Shortcut { value: inner, .. } | ExprKind::ToText { value: inner, .. } => {
            has_prelude(inner)
        }
        ExprKind::MapOp { map, key: None, .. } => has_prelude(map),
        ExprKind::MapOp { map, key: Some(key), .. } => has_prelude(map) || has_prelude(key) || ordered(&[map, key]),
        ExprKind::Pairs(pairs) => {
            let all: Vec<&Expr> = pairs.iter().flat_map(|(key, value)| [key, value]).collect();
            all.iter().any(|e| has_prelude(e)) || ordered(&all)
        }
        ExprKind::Index(a, b) | ExprKind::Lookup(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Range(a, b) | ExprKind::Repeat { value: a, count: b } => {
            has_prelude(a) || has_prelude(b) || ordered(&[a, b])
        }
        ExprKind::Call { args, .. }
        | ExprKind::Construct { args, .. }
        | ExprKind::Array(args)
        | ExprKind::Tuple(args)
        | ExprKind::Join { parts: args, .. } => args.iter().any(has_prelude) || ordered(&args.iter().collect::<Vec<_>>()),
        ExprKind::StructInit { fields, .. } => {
            fields.iter().any(|field| has_prelude(&field.value)) || ordered(&fields.iter().map(|field| &field.value).collect::<Vec<_>>())
        }
        // The checker rewrites a method call into a call; read as one anyway,
        // so nothing here depends on that having happened.
        ExprKind::MethodCall { value, args, .. } => {
            has_prelude(value) || args.iter().any(has_prelude) || ordered(&std::iter::once(value.as_ref()).chain(args).collect::<Vec<_>>())
        }
    }
}

/// Whether `gen_operands` stores some of `operands` in temporaries to read
/// them left to right, which is a prelude: one of several may change
/// something.
fn ordered(operands: &[&Expr]) -> bool {
    operands.len() > 1 && operands.iter().any(|e| !changes_nothing(e))
}

/// The `maybe`, `or`, array, and slice types a program uses, which each need a
/// C struct, and the helpers it needs for them: `show` for a `maybe`, and
/// writing one before another value of the same `show`; for a
/// slice, reading an item, slicing, copying into a region, and filling one; for
/// an array, filling it. Types are kept without regions.
struct UsedTypes {
    compounds: TypeList,
    /// The types whose values are written as text through a generated
    /// `write` (see `note_write`), each after the types inside it.
    writes: TypeList,
    /// The types whose values `str` makes into text through a generated
    /// `text`: those with a `write`, and shapes and kinds with their own `str`.
    texts: TypeList,
    /// For each shape or kind without its own `str`, the types its default
    /// text writes: a shape's fields that aren't `priv`, a kind's payloads.
    parts: HashMap<String, Vec<Type>>,
    /// The numbers read from text, `int("42")` and `float("2.5")`, each of which
    /// needs the helper that turns what the runtime reads into a value or a
    /// `ParseError`.
    read: TypeList,
    /// The numbers read from input, `scan("Age: ", int)`, each of which needs the
    /// helper that turns what the runtime reads into a value or an `InputError`.
    scanned: TypeList,
    slice_at: TypeList,
    /// The types of the maps' keys, each tuple after its items.
    map_keys: TypeList,
    /// The values read with a lookup that gives none out of range, `t.at(i)`.
    lookups: TypeList,
    slice_part: TypeList,
    slice_copy: TypeList,
    /// The slices made of another slice or array, `new [T](xs)`, which copy
    /// at every depth where their items hold anything in a region.
    slice_copied: TypeList,
    slice_fill: TypeList,
    slice_add: TypeList,
    slice_insert: TypeList,
    slice_del: TypeList,
    slice_pop: TypeList,
    array_fill: TypeList,
    /// The types of the values the program copies with `copy()`.
    copied: TypeList,
    /// The kinds and slices `==` compares by a function of its own (see
    /// `Generator::gen_compares`), and the kinds those kinds carry.
    same: TypeList,
    /// The types `~=` compares item by item, and the types inside them.
    similar: TypeList,
    /// Each shape's fields, every one, and each kind's carried values, for
    /// what `~=` and `==` compare inside them.
    fields: HashMap<String, Vec<Type>>,
    /// The kinds, which `==` compares by variant rather than by place.
    kinds: HashSet<String>,
    /// Whether the program calls `scan`.
    scans: bool,
    /// The task types values have, each after the task types inside it, by
    /// the C type each is (see `Generator::gen_task_type`).
    tasks: Vec<Type>,
    task_words: HashSet<String>,
    /// The tasks named as values, in the order first named.
    task_values: Vec<String>,
    /// The tasks named where a lambda that keeps what it uses is expected,
    /// each with the C type of its value (see `ExprKind::Kept`).
    kept_tasks: Vec<(String, String)>,
    /// Each kind a value goes from, with the kind imitating it that it goes
    /// to (see `ExprKind::Widen`), without regions, in the order first noted.
    widened: Vec<(Type, Type)>,
}

/// Types in the order they were first noted, with a set beside them so that
/// noting one again, and asking whether one is there, takes no search.
#[derive(Default)]
struct TypeList {
    list: Vec<Type>,
    set: HashSet<Type>,
}

impl TypeList {
    fn contains(&self, ty: &Type) -> bool {
        self.set.contains(ty)
    }

    fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Adds `ty` as it is, unless it is there.
    fn insert(&mut self, ty: Type) {
        if !self.set.contains(&ty) {
            self.set.insert(ty.clone());
            self.list.push(ty);
        }
    }

    fn iter(&self) -> std::slice::Iter<'_, Type> {
        self.list.iter()
    }
}

impl<'a> IntoIterator for &'a TypeList {
    type Item = &'a Type;
    type IntoIter = std::slice::Iter<'a, Type>;

    fn into_iter(self) -> Self::IntoIter {
        self.list.iter()
    }
}

/// Adds `ty`, without regions, to `list` unless it is there.
fn note(list: &mut TypeList, ty: &Type) {
    list.insert(ty.without_regions());
}

impl UsedTypes {
    /// Notes that values of type `ty` are made text in a region (see
    /// `text_function`).
    fn note_text(&mut self, ty: &Type) {
        if matches!(ty, Type::Int | Type::Unit | Type::Char | Type::Float | Type::Bool | Type::Text(_)) {
            return;
        }
        // A shape's or kind's own `str` needs no `write`.
        if !matches!(ty, Type::Named(name, _) if !self.parts.contains_key(name)) {
            self.note_write(ty);
        }
        note(&mut self.texts, ty);
    }

    fn collect(items: &[Item]) -> UsedTypes {
        let mut used = UsedTypes {
            compounds: TypeList::default(),
            writes: TypeList::default(),
            texts: TypeList::default(),
            parts: HashMap::new(),
            read: TypeList::default(),
            scanned: TypeList::default(),
            slice_at: TypeList::default(),
            map_keys: TypeList::default(),
            lookups: TypeList::default(),
            slice_part: TypeList::default(),
            slice_copy: TypeList::default(),
            slice_copied: TypeList::default(),
            slice_fill: TypeList::default(),
            slice_add: TypeList::default(),
            slice_insert: TypeList::default(),
            slice_del: TypeList::default(),
            slice_pop: TypeList::default(),
            array_fill: TypeList::default(),
            copied: TypeList::default(),
            same: TypeList::default(),
            similar: TypeList::default(),
            fields: HashMap::new(),
            kinds: HashSet::new(),
            scans: false,
            tasks: Vec::new(),
            task_words: HashSet::new(),
            task_values: Vec::new(),
            kept_tasks: Vec::new(),
            widened: Vec::new(),
        };
        let tasks: HashSet<&str> = items
            .iter()
            .filter_map(|item| match item {
                Item::Task(t) => Some(t.name.as_str()),
                _ => None,
            })
            .collect();
        let exists = |task: &str| tasks.contains(task);
        for item in items {
            match item {
                Item::Shape(s) if s.types.is_empty() => {
                    used.fields.insert(s.name.clone(), s.fields.iter().map(|f| f.ty.clone()).collect());
                }
                Item::Kind(k) if k.types.is_empty() => {
                    used.kinds.insert(k.name.clone());
                    used.fields.insert(k.name.clone(), k.variants.iter().flat_map(|v| v.payload.iter().map(|f| f.ty.clone())).collect());
                }
                _ => {}
            }
        }
        for item in items {
            match item {
                Item::Shape(s) if s.types.is_empty() && str_task(&s.name, &s.given, exists).is_none() => {
                    used.parts.insert(s.name.clone(), s.fields.iter().filter(|f| !f.private).map(|f| f.ty.clone()).collect());
                }
                Item::Kind(k) if k.types.is_empty() && str_task(&k.name, &k.given, exists).is_none() => {
                    used.parts.insert(k.name.clone(), k.variants.iter().flat_map(|v| v.payload.iter().map(|f| f.ty.clone())).collect());
                }
                _ => {}
            }
        }
        for item in items {
            match item {
                // The fields of a shape that takes types hold `T`, which no C
                // type answers; the shapes made from it hold the types given.
                Item::Shape(s) if s.types.is_empty() => {
                    s.fields.iter().for_each(|f| used.add(&f.ty));
                    // A shared field's first value is written where the program starts.
                    for member in &s.shared {
                        used.add(&member.field.ty);
                        if let Some(value) = &member.field.default {
                            used.expr(value);
                        }
                    }
                }
                Item::Shape(_) => {}
                Item::Kind(k) if k.types.is_empty() => k.variants.iter().flat_map(|v| &v.payload).for_each(|f| used.add(&f.ty)),
                Item::Kind(_) => {}
                // A generic task's own types hold `T`, which no C type
                // answers; the tasks made from it hold the types it is given.
                Item::Task(t) if t.types.is_empty() => {
                    t.params.iter().for_each(|p| used.add(&p.ty));
                    used.add(&t.ret);
                    used.block(&t.body);
                    // A startup decorator's error is made text where it fails
                    // (see `Generator::gen_startup`).
                    if is_startup(&t.name) {
                        for stmt in &t.body {
                            if let StmtKind::Expr(Expr { ty: Some(Type::Or(_, error)), .. }) = &stmt.kind {
                                used.note_text(error);
                            }
                        }
                    }
                }
                Item::Task(_) | Item::Macro(_) => {}
            }
        }
        used
    }

    /// Records `ty` and every type inside it. Regions don't change a type's C
    /// struct, so they are left out.
    fn add(&mut self, ty: &Type) {
        let ty = &ty.without_regions();
        // One recorded already had every type inside it recorded first.
        if self.compounds.contains(ty) {
            return;
        }
        match ty {
            Type::Ptr(inner, _) | Type::Range(inner) | Type::Fail(inner) => self.add(inner),
            // Every type it takes and sends has a C type first, which its own
            // names; its regions are its own, so they are left as they are.
            Type::Task(task) => {
                if self.task_words.contains(&type_word(ty)) {
                    return;
                }
                task.params.iter().for_each(|param| self.add(param));
                self.add(&task.ret);
                if self.task_words.insert(type_word(ty)) {
                    self.tasks.push(ty.clone());
                }
            }
            // Its function's type too, which a plain task value it is made of
            // is (see `gen_closure_helpers`).
            Type::Closure(closure) => {
                if self.task_words.contains(&type_word(ty)) {
                    return;
                }
                self.add(&Type::Task(Box::new(closure.task.clone())));
                if self.task_words.insert(type_word(ty)) {
                    self.tasks.push(ty.clone());
                }
            }
            Type::Maybe(inner) => {
                self.add(inner);
                if !is_pointer(inner) && !self.compounds.contains(ty) {
                    self.compounds.insert(ty.clone());
                }
            }
            Type::Or(value, error) => {
                self.add(value);
                self.add(error);
                if !self.compounds.contains(ty) {
                    self.compounds.insert(ty.clone());
                }
            }
            Type::Results(types) | Type::Tuple(types) => {
                types.iter().for_each(|item| self.add(item));
                if !self.compounds.contains(ty) {
                    self.compounds.insert(ty.clone());
                }
            }
            Type::Array(inner, _) | Type::Slice(inner, _) => {
                self.add(inner);
                if !self.compounds.contains(ty) {
                    self.compounds.insert(ty.clone());
                }
            }
            // A map's helpers give a maybe of a value and slices of its keys
            // and values, so those types are made with it.
            Type::Map(key, value, _) => {
                if self.compounds.contains(ty) {
                    return;
                }
                self.add(key);
                self.add(value);
                self.add(&Type::Maybe(value.clone()));
                self.add(&Type::Slice(key.clone(), None));
                self.add(&Type::Slice(value.clone(), None));
                self.note_key(key);
                self.compounds.insert(ty.clone());
            }
            _ => {}
        }
    }

    /// Records that `key` is a map's key, which needs a hash and an equality
    /// of its own, and so do a tuple's items, first.
    fn note_key(&mut self, key: &Type) {
        let key = key.without_regions();
        if self.map_keys.contains(&key) {
            return;
        }
        if let Type::Tuple(items) = &key {
            items.iter().for_each(|item| self.note_key(item));
        }
        self.map_keys.insert(key);
    }

    /// Records that values of type `ty` are written as text, and so are the
    /// values inside them: a number, a bool, text, and a pointer are put by the
    /// runtime, and anything else by a `write` of its own.
    fn note_write(&mut self, ty: &Type) {
        let ty = ty.without_regions();
        match &ty {
            Type::Int | Type::Unit | Type::Char | Type::Float | Type::Bool | Type::Text(_) | Type::Ptr(..) | Type::Task(_) => return,
            Type::Maybe(inner) | Type::Array(inner, _) | Type::Slice(inner, _) => self.note_write(inner),
            Type::Map(key, value, _) => {
                self.note_write(key);
                self.note_write(value);
            }
            Type::Tuple(items) => items.iter().for_each(|item| self.note_write(item)),
            // A shape or kind written by default writes its parts; one that
            // holds itself (through a slice, say) is noted once.
            Type::Named(name, _) => {
                if self.writes.contains(&ty) {
                    return;
                }
                note(&mut self.writes, &ty);
                for part in self.parts.get(name).cloned().unwrap_or_default() {
                    self.note_write(&part);
                }
                return;
            }
            // Nothing else is written as text; the checker made sure of it.
            _ => return,
        }
        note(&mut self.writes, &ty);
    }

    /// Records that `==` compares values of type `ty`: a kind, and a slice,
    /// by a function of its own, and so the kinds and slices a kind carries.
    fn note_same(&mut self, ty: &Type) {
        let ty = ty.without_regions();
        match &ty {
            Type::Slice(..) => note(&mut self.same, &ty),
            Type::Maybe(inner) => self.note_same(inner),
            Type::Named(name, _) if self.kinds.contains(name) => {
                if self.same.contains(&ty) {
                    return;
                }
                note(&mut self.same, &ty);
                for part in self.fields.get(name).cloned().unwrap_or_default() {
                    self.note_same(&part);
                }
            }
            _ => {}
        }
    }

    /// Records that `~=` compares values of type `ty`, and so the values
    /// inside them: a number, a bool, text, and a pointer are compared where
    /// they are, and anything else by a function of its own.
    fn note_similar(&mut self, ty: &Type) {
        let ty = ty.without_regions();
        match &ty {
            Type::Int | Type::Unit | Type::Char | Type::Float | Type::Bool | Type::Text(_) | Type::Ptr(..) | Type::Task(_) => {}
            Type::Maybe(inner) if matches!(inner.as_ref(), Type::Ptr(..)) => {}
            _ if self.similar.contains(&ty) => {}
            Type::Maybe(inner) | Type::Array(inner, _) | Type::Slice(inner, _) => {
                note(&mut self.similar, &ty);
                self.note_similar(inner);
            }
            Type::Tuple(items) => {
                note(&mut self.similar, &ty);
                items.iter().for_each(|item| self.note_similar(item));
            }
            Type::Map(_, value, _) => {
                note(&mut self.similar, &ty);
                self.note_similar(value);
            }
            Type::Named(name, _) => {
                note(&mut self.similar, &ty);
                for part in self.fields.get(name).cloned().unwrap_or_default() {
                    self.note_similar(&part);
                }
            }
            _ => {}
        }
    }

    /// Records the types of the names a binding declares, and of the tuples it unpacks.
    fn bound(&mut self, bound: &Bound) {
        if let Some(ty) = &bound.ty {
            self.add(ty);
        }
        bound.parts.iter().for_each(|part| self.bound(part));
    }

    /// Records what the places of an assignment read.
    fn target(&mut self, target: &Target) {
        match target {
            Target::Discard(_) => {}
            Target::Place(place) => self.expr(place),
            Target::Tuple(parts, _) => parts.iter().for_each(|part| self.target(part)),
        }
    }

    fn block(&mut self, block: &[Stmt]) {
        for stmt in block {
            match &stmt.kind {
                StmtKind::Expr(e) => self.expr(e),
                StmtKind::Step { target, value, .. } => {
                    self.expr(target);
                    self.expr(value);
                }
                StmtKind::Send(values) | StmtKind::SetResults(_, values) => values.iter().for_each(|value| self.expr(value)),
                StmtKind::Region { .. } => {}
                StmtKind::Binding { names, init, .. } => {
                    names.iter().for_each(|bound| self.bound(bound));
                    init.iter().for_each(|value| self.expr(value));
                }
                StmtKind::Assign(targets, values) => {
                    targets.iter().for_each(|target| self.target(target));
                    values.iter().for_each(|value| self.expr(value));
                }
                StmtKind::Block(body) => self.block(body),
                StmtKind::Turns(turns) => turns.iter().for_each(|turn| self.block(turn)),
                StmtKind::Named(_, inner) => self.block(std::slice::from_ref(&**inner)),
                StmtKind::Break(_, value) => value.iter().for_each(|value| self.expr(value)),
                StmtKind::Continue(_) => {}
                StmtKind::When(arms, else_b) => {
                    for arm in arms {
                        arm.cond.parts().for_each(|part| self.expr(&part.value));
                        self.block(&arm.body);
                    }
                    if let Some(else_b) = else_b {
                        self.block(else_b);
                    }
                }
                StmtKind::Loop(cond, body, finally) => {
                    cond.parts().for_each(|part| self.expr(&part.value));
                    self.block(body);
                    if let Some(finally) = finally {
                        self.block(finally);
                    }
                }
                StmtKind::Parted(parted) => {
                    self.block(&parted.start);
                    parted.cond.iter().flat_map(Condition::parts).for_each(|part| self.expr(&part.value));
                    self.block(&parted.step);
                    self.block(&parted.body);
                    if let Some(finally) = &parted.finally {
                        self.block(finally);
                    }
                }
                StmtKind::Each(_, _, range, body, finally) => {
                    self.expr(range);
                    self.block(body);
                    if let Some(finally) = finally {
                        self.block(finally);
                    }
                }
                StmtKind::Match(subject, cases) => {
                    self.expr(subject);
                    cases.iter().for_each(|case| self.block(&case.body));
                }
            }
        }
    }

    /// Notes that a value of kind `from` is made one of `to`, the kind
    /// imitating it (see `widened`).
    fn widen(&mut self, from: &Type, to: &Type) {
        let pair = (from.without_regions(), to.without_regions());
        if !self.widened.contains(&pair) {
            self.widened.push(pair);
        }
    }

    fn expr(&mut self, e: &Expr) {
        if let Some(ty) = &e.ty {
            self.add(ty);
        }
        match &e.kind {
            ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::CharLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) | ExprKind::NoneLit | ExprKind::Macro { .. }
            | ExprKind::Var(_) | ExprKind::RegionAfter(_) | ExprKind::Shared { .. } | ExprKind::TypeOf(_) => {}
            ExprKind::Spread(inner) => self.expr(inner),
            ExprKind::TaskValue(name) | ExprKind::Closure { lambda: name, .. } => {
                if !self.task_values.contains(name) {
                    self.task_values.push(name.clone());
                }
            }
            ExprKind::Kept { value, region } => {
                if let (None, ExprKind::TaskValue(name), Some(ty)) = (region, &value.kind, &value.ty)
                    && !self.kept_tasks.iter().any(|(kept, _)| kept == name)
                {
                    self.kept_tasks.push((name.clone(), c_type(ty)));
                }
                self.expr(value)
            }
            ExprKind::Invoke { callee, args } => {
                self.expr(callee);
                args.iter().for_each(|arg| self.expr(arg));
            }
            ExprKind::Shortcut { value, .. } => self.expr(value),
            ExprKind::Pairs(pairs) => pairs.iter().for_each(|(key, value)| {
                self.expr(key);
                self.expr(value);
            }),
            ExprKind::MapOp { map, key, .. } => {
                self.expr(map);
                if let Some(key) = key {
                    self.expr(key);
                }
            }
            // A map made from another map copies it.
            ExprKind::New { ty, value, .. } if matches!(**ty, Type::Map(..)) => {
                if !matches!(value.kind, ExprKind::Pairs(_))
                    && let Some(ty) = &value.ty
                {
                    note(&mut self.copied, ty);
                }
                self.expr(value);
            }
            ExprKind::New { ty, value, .. } if matches!(**ty, Type::Slice(..)) => {
                let Some(slice) = &e.ty else { return };
                match &value.kind {
                    // The repeated value fills the slice; it makes no array.
                    ExprKind::Repeat { value: item, count } => {
                        note(&mut self.slice_fill, slice);
                        self.expr(item);
                        self.expr(count);
                    }
                    // `new [T]()` copies nothing.
                    ExprKind::Array(values) if values.is_empty() => {}
                    // The values written, copied item by item.
                    ExprKind::Array(_) => {
                        note(&mut self.slice_copy, slice);
                        self.expr(value);
                    }
                    // Another slice or array, copied at every depth where the
                    // items hold anything in a region (see `Generator::gen_new`).
                    _ => {
                        note(&mut self.slice_copied, slice);
                        self.expr(value);
                    }
                }
            }
            ExprKind::Field(inner, _) | ExprKind::Unary(_, inner) | ExprKind::Deref(inner) | ExprKind::Ref(inner) | ExprKind::New { value: inner, .. }
            | ExprKind::Fail(inner) | ExprKind::Wrap(inner) => self.expr(inner),
            ExprKind::Widen(inner) => {
                if let (Some(from), Some(to)) = (&inner.ty, &e.ty) {
                    self.widen(from, to);
                }
                self.expr(inner)
            }
            // Each error of a pipe's steps that is not the one it fails with
            // is made a value of that one (see `gen_pipe`).
            ExprKind::Pipe(call) => {
                if let Some(Type::Or(_, error)) = &e.ty {
                    let before = piped_value(call).and_then(|value| value.ty.as_ref());
                    for step in [before, call.ty.as_ref()] {
                        if let Some(Type::Or(_, from)) = step
                            && from.without_regions() != error.without_regions()
                        {
                            self.widen(from, error);
                        }
                    }
                }
                self.expr(call)
            }
            ExprKind::Piped(inner) => self.expr(inner),
            // The same statement a `match` or a `when` on its own is.
            ExprKind::Choice(stmt) => self.block(std::slice::from_ref(stmt)),
            ExprKind::LoopValue(value) => {
                self.block(std::slice::from_ref(&value.stmt));
                if let Some(Type::Or(coll, _)) = &e.ty
                    && let Type::Slice(..) = coll.as_ref()
                {
                    note(&mut self.slice_add, coll);
                }
            }
            ExprKind::Lookup(base, index) => {
                // A map's lookup is one of its own helpers.
                if let Some(ty) = base.ty.as_ref().filter(|ty| !matches!(ty, Type::Map(..))) {
                    note(&mut self.lookups, &lookup_key(ty));
                }
                self.expr(base);
                self.expr(index);
            }
            ExprKind::Index(base, index) => {
                match (&index.kind, &base.ty, &e.ty) {
                    // A part of text needs no helper of its own.
                    (ExprKind::Range(..), _, Some(slice @ Type::Slice(..))) => note(&mut self.slice_part, slice),
                    (_, Some(slice @ Type::Slice(..)), _) => note(&mut self.slice_at, slice),
                    _ => {}
                }
                self.expr(base);
                self.expr(index);
            }
            ExprKind::Repeat { value, count } => {
                if let Some(array) = &e.ty {
                    note(&mut self.array_fill, array);
                }
                self.expr(value);
                self.expr(count);
            }
            ExprKind::Array(values) | ExprKind::Tuple(values) => values.iter().for_each(|value| self.expr(value)),
            ExprKind::Binary(op, a, b) => {
                let refs = [a, b].into_iter().any(|side| matches!(side.kind, ExprKind::Ref(_)));
                match (op, &a.ty) {
                    // `m == none` tests the maybe, and compares no values.
                    (BinaryOp::Eq | BinaryOp::Ne, Some(ty)) if !refs && ![&a.ty, &b.ty].contains(&&Some(Type::None)) => self.note_same(ty),
                    // `none ~= m` is typed by its other side.
                    (BinaryOp::Similar | BinaryOp::NotSimilar, Some(Type::None)) => {}
                    (BinaryOp::Similar | BinaryOp::NotSimilar, Some(ty)) if !matches!(b.ty, Some(Type::None)) => self.note_similar(ty),
                    _ => {}
                }
                self.expr(a);
                self.expr(b);
            }
            ExprKind::Range(a, b) => {
                self.expr(a);
                self.expr(b);
            }
            ExprKind::Call { name, args, .. } => {
                // `int(t)` and `float(t)` on text need the helper that reads it.
                if let Some(into) = number_cast(name)
                    && args.first().and_then(|arg| arg.ty.as_ref()).is_some_and(|ty| matches!(ty, Type::Text(_)))
                {
                    note(&mut self.read, &into);
                }
                if name == "show" {
                    for ty in args.iter().filter_map(|arg| arg.ty.as_ref()) {
                        self.note_write(ty);
                    }
                }
                args.iter().for_each(|arg| self.expr(arg));
            }
            ExprKind::MethodCall { value, args, .. } => {
                self.expr(value);
                args.iter().for_each(|arg| self.expr(arg));
            }
            ExprKind::Construct { args, .. } => args.iter().for_each(|arg| self.expr(arg)),
            ExprKind::StructInit { fields, .. } => fields.iter().for_each(|field| self.expr(&field.value)),
            ExprKind::PlanB { value, body, .. } => {
                self.expr(value);
                self.block(body);
            }
            ExprKind::Copy { value, .. } => {
                if let Some(ty) = &value.ty {
                    note(&mut self.copied, ty);
                }
                self.expr(value);
            }
            ExprKind::Join { parts, .. } => parts.iter().for_each(|part| self.expr(part)),
            ExprKind::ToText { value, .. } => {
                if let Some(ty) = &value.ty {
                    self.note_text(ty);
                }
                self.expr(value)
            }
            ExprKind::Scan { args, .. } => {
                // A typed read needs the helper that reads a number and the
                // struct of the `T ~ InputError` it gives; a line read needs
                // the one that reads text, whose `maybe text` has a struct even
                // when it isn't kept.
                match (&e.ty, args.len()) {
                    (Some(ty @ Type::Or(number, _)), 2) => {
                        note(&mut self.scanned, number);
                        self.add(ty);
                    }
                    _ => {
                        self.scans = true;
                        self.add(&Type::Maybe(Box::new(Type::Text(None))));
                    }
                }
                // The type a typed read names is a type, not a value.
                args.iter().take(1).for_each(|arg| self.expr(arg));
            }
            ExprKind::Edit { items, op, args } => {
                if let Some(slice @ Type::Slice(..)) = &items.ty {
                    let list = match (op, args.len()) {
                        (EditOp::Add | EditOp::Added, 1) => &mut self.slice_add,
                        (EditOp::Add | EditOp::Added, _) => &mut self.slice_insert,
                        (EditOp::Del | EditOp::Without, 0) => &mut self.slice_pop,
                        (EditOp::Del | EditOp::Without, _) => &mut self.slice_del,
                    };
                    note(list, slice);
                }
                // What `added` puts in its copy is copied there with it.
                if *op == EditOp::Added {
                    match &items.ty {
                        Some(Type::Slice(item, _)) => note(&mut self.copied, item),
                        Some(Type::Map(key, value, _)) => {
                            note(&mut self.copied, key);
                            note(&mut self.copied, value);
                        }
                        _ => {}
                    }
                }
                self.expr(items);
                args.iter().for_each(|arg| self.expr(arg));
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
        StmtKind::Step { target, value, .. } => expr(target) || expr(value),
        StmtKind::Send(values) | StmtKind::SetResults(_, values) => values.iter().any(expr),
        StmtKind::Break(_, value) => value.as_deref().is_some_and(expr),
        StmtKind::Region { .. } | StmtKind::Continue(_) => false,
        StmtKind::Named(_, inner) => stmt_reads(inner, name),
        StmtKind::Turns(turns) => turns.iter().any(body),
        StmtKind::Binding { init, .. } => init.iter().any(expr),
        StmtKind::Assign(targets, values) => {
            targets.iter().any(|target| target_reads(target, name)) || values.iter().any(expr)
        }
        StmtKind::When(arms, else_b) => arms.iter().any(|arm| arm.cond.parts().any(|part| expr(&part.value)) || body(&arm.body)) || else_b.as_ref().is_some_and(body),
        StmtKind::Loop(cond, b, finally) => cond.parts().any(|part| expr(&part.value)) || body(b) || finally.as_ref().is_some_and(body),
        StmtKind::Parted(parted) => body(&parted.start) || parted.cond.iter().flat_map(Condition::parts).any(|part| expr(&part.value)) || body(&parted.step) || body(&parted.body) || parted.finally.as_ref().is_some_and(body),
        StmtKind::Block(b) => body(b),
        StmtKind::Each(_, _, range, b, finally) => expr(range) || body(b) || finally.as_ref().is_some_and(body),
        StmtKind::Match(subject, cases) => expr(subject) || cases.iter().any(|case| body(&case.body)),
    }
}

/// Whether assigning to `target` reads `name`. Writing to a variable or its
/// fields doesn't read it; writing through a pointer (`p^ = …`) reads `p`.
fn target_reads(target: &Target, name: &str) -> bool {
    match target {
        Target::Discard(_) => false,
        Target::Place(place) => place_reads(place, name),
        Target::Tuple(parts, _) => parts.iter().any(|part| target_reads(part, name)),
    }
}

/// Whether assigning to the place `target` reads `name`.
fn place_reads(target: &Expr, name: &str) -> bool {
    match &target.kind {
        ExprKind::Var(_) => false,
        ExprKind::Field(base, _) => place_reads(base, name),
        // Writing an item of an array writes the array; a slice is read to
        // find its item.
        ExprKind::Index(base, index) if matches!(base.ty, Some(Type::Array(..))) => place_reads(base, name) || expr_reads(index, name),
        _ => expr_reads(target, name),
    }
}

fn expr_reads(e: &Expr, name: &str) -> bool {
    match &e.kind {
        ExprKind::Spread(inner) => expr_reads(inner, name),
        ExprKind::Var(var) | ExprKind::RegionAfter(var) => var == name,
        ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::CharLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) | ExprKind::NoneLit | ExprKind::Macro { .. } | ExprKind::Shared { .. } | ExprKind::TypeOf(_) | ExprKind::TaskValue(_) => false,
        // It reads the variables it keeps too, each of which a `(void)` marks
        // read where it is declared (see `use_if_unread`).
        ExprKind::Closure { region, .. } if region == name => true,
        ExprKind::Kept { region: Some(region), .. } if region == name => true,
        ExprKind::Closure { .. } => false,
        ExprKind::Kept { value, .. } => expr_reads(value, name),
        ExprKind::Invoke { callee, args } => expr_reads(callee, name) || args.iter().any(|arg| expr_reads(arg, name)),
        // `new` reads the region it allocates in.
        ExprKind::New { value, region, .. } => region.as_deref() == Some(name) || expr_reads(value, name),
        // `t.#5` past the tuple's end is none, and reads the tuple only for what
        // working it out does (see `work_out_none`).
        ExprKind::Field(inner, item) if item.starts_with(check::SAFE_ITEM) && e.ty == Some(Type::None) && without_effects(inner) => false,
        ExprKind::Field(inner, _) | ExprKind::Unary(_, inner) | ExprKind::Deref(inner) | ExprKind::Ref(inner)
        | ExprKind::Fail(inner) | ExprKind::Wrap(inner) | ExprKind::Widen(inner) | ExprKind::Pipe(inner) | ExprKind::Piped(inner) => expr_reads(inner, name),
        ExprKind::Index(a, b) | ExprKind::Lookup(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Range(a, b) | ExprKind::Repeat { value: a, count: b } => {
            expr_reads(a, name) || expr_reads(b, name)
        }
        ExprKind::Call { args, .. } | ExprKind::Construct { args, .. } | ExprKind::Array(args) | ExprKind::Tuple(args) => {
            args.iter().any(|arg| expr_reads(arg, name))
        }
        ExprKind::MethodCall { value, args, .. } => expr_reads(value, name) || args.iter().any(|arg| expr_reads(arg, name)),
        ExprKind::StructInit { fields, .. } => fields.iter().any(|field| expr_reads(&field.value, name)),
        ExprKind::Pairs(pairs) => pairs.iter().any(|(key, value)| expr_reads(key, name) || expr_reads(value, name)),
        ExprKind::MapOp { map, key, region, .. } => {
            region.as_deref() == Some(name) || expr_reads(map, name) || key.as_deref().is_some_and(|key| expr_reads(key, name))
        }
        ExprKind::PlanB { value, body, .. } => expr_reads(value, name) || block_reads(body, name),
        // The same statement a `match` or a `when` on its own is.
        ExprKind::Choice(stmt) => block_reads(std::slice::from_ref(stmt), name),
        ExprKind::LoopValue(value) => block_reads(std::slice::from_ref(&value.stmt), name),
        // `scan` reads the region it keeps the line in, where it keeps one, and
        // the prompt. The type a typed read names is a type, not a value, so a
        // variable of that name is not read by it.
        ExprKind::Scan { args, region, .. } => region.as_deref() == Some(name) || args.iter().take(1).any(|arg| expr_reads(arg, name)),
        // Joining text, and a value turned into text, read the region the text goes in.
        ExprKind::Join { parts, region, .. } => region == name || parts.iter().any(|part| expr_reads(part, name)),
        ExprKind::ToText { value, region, .. } => region == name || expr_reads(value, name),
        // `copy` reads the region it copies into.
        ExprKind::Copy { value, region, .. } | ExprKind::Shortcut { value, region, .. } => region.as_deref() == Some(name) || expr_reads(value, name),
        // `add` reads the region it may allocate in.
        ExprKind::Edit { items, op, args } => {
            let region = match &items.ty {
                Some(Type::Slice(_, Some(region))) if *op == EditOp::Add => region == name,
                _ => false,
            };
            region || expr_reads(items, name) || args.iter().any(|arg| expr_reads(arg, name))
        }
    }
}

fn type_name(item: &Item) -> Option<&str> {
    match item {
        // A shape that takes types is not a C type: see `generate`.
        Item::Shape(s) if !s.types.is_empty() => None,
        // The values of wrappers worked out where the program starts are
        // shared fields of a shape no value of which is ever made.
        Item::Shape(s) if s.name == START_VALUES => None,
        Item::Shape(s) => Some(&s.name),
        Item::Kind(k) if !k.types.is_empty() => None,
        Item::Kind(k) => Some(&k.name),
        Item::Task(_) | Item::Macro(_) => None,
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
        Type::Maybe(_) | Type::Or(..) | Type::Results(_) | Type::Tuple(_) | Type::Array(..) | Type::Slice(..) => vec![c_type(ty)],
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
        names.push(compound_name(ty));
        by_name.insert(compound_name(ty), TypeDef::Compound(ty));
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
        TypeDef::Item(Item::Task(_) | Item::Macro(_)) => Vec::new(),
        TypeDef::Compound(Type::Maybe(inner)) => value_dependencies(inner),
        TypeDef::Compound(Type::Or(value, error)) => value_dependencies(value).into_iter().chain(value_dependencies(error)).collect(),
        TypeDef::Compound(Type::Results(types) | Type::Tuple(types)) => types.iter().flat_map(value_dependencies).collect(),
        TypeDef::Compound(Type::Array(items, _)) => value_dependencies(items),
        // Its entries hold a key and a value each.
        TypeDef::Compound(Type::Map(key, value, _)) => value_dependencies(key).into_iter().chain(value_dependencies(value)).collect(),
        TypeDef::Compound(_) => Vec::new(),
    }
}

/// Whether evaluating `e` changes nothing a later value could read: no call,
/// no change to a slice, no line read. (It may still stop the program.)
fn changes_nothing(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::CharLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) | ExprKind::NoneLit | ExprKind::Macro { .. } | ExprKind::Var(_) | ExprKind::Shared { .. } | ExprKind::TaskValue(_) => true,
        // A temporary its pipe wrote before the call.
        ExprKind::Piped(_) => true,
        ExprKind::Field(inner, _) | ExprKind::Deref(inner) | ExprKind::Ref(inner) | ExprKind::Unary(_, inner) | ExprKind::Wrap(inner) | ExprKind::Widen(inner) => changes_nothing(inner),
        ExprKind::Index(a, b) | ExprKind::Lookup(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Range(a, b) => changes_nothing(a) && changes_nothing(b),
        ExprKind::Array(items) | ExprKind::Tuple(items) => items.iter().all(changes_nothing),
        _ => false,
    }
}

/// A parameter that never changes, of a type larger than this many bytes, is
/// passed by a hidden pointer, `const T*`, to where the caller keeps the value
/// (or to a copy the call makes), rather than copied into the task: the task
/// can't change it, and the call makes sure nothing else does while the task
/// runs (see `Generator::gen_call`). The user chose 64 bytes: eight words.
pub const HIDDEN_ABOVE: usize = 64;

/// Whether parameter `p` of a task is a pointer to the caller's place in C: a
/// `flex&` one, or the value a method that `alters` it is called on.
fn is_pointer_param(p: &Field, alters: bool) -> bool {
    p.passing.is_ref() || (alters && p.name == THIS)
}

/// Whether parameter `p` of `t` is a copy of the task's own, which the task
/// makes from the value it is given, under the name `given_name`: one given as
/// a value (`x: T`, `fixed x`, `flex x`, and `.this` of a method that changes
/// nothing), not one the task reads where the caller keeps it (`hidden`) or
/// through a pointer. A parameter given as a value is a place of the task's own
/// in C too, but an optimizing C compiler may read it where the caller keeps
/// the value when the task never changes it, so its address, which a strict
/// copy's `&x` and `==` read, could be the caller's. A local's never is. A
/// native task's C is the runtime's, which takes its values as they are.
fn owns_copy(t: &TaskDef, p: &Field, alters: bool, hidden: bool) -> bool {
    !t.native && !hidden && !is_pointer_param(p, alters) && p.ty != Type::Region
}

/// The C name of the value given for `name`, a copy the task makes its own
/// (see `owns_copy`): `vela__given__this` for `.this`. No Vela name holds
/// `__`, so it is no other name.
fn given_name(name: &str) -> String {
    format!("vela__given__{}", name.trim_start_matches('.'))
}

/// The C variable holding task `name`, kept where a lambda that keeps what it
/// uses is expected (see `ExprKind::Kept`).
fn kept_task_name(name: &str) -> String {
    format!("vela__kept_task__{}", mangle(name))
}

/// The C name of what a lambda that keeps what it uses is given first: the
/// block of its region holding what it keeps (see `Generator::gen_closure`).
const KEPT_PARAM: &str = "vela__kept";

/// The C declarator of a task, e.g. `int64_t vela_add(int64_t a, int64_t b)`.
fn signature(t: &TaskDef, alters: bool, hidden: &[bool], is_shape: &dyn Fn(&str) -> bool) -> String {
    let params: Vec<String> = t
        .params
        .iter()
        .enumerate()
        .map(|(i, p)| {
            // A large value that never changes is read where the caller keeps
            // it (see `HIDDEN_ABOVE`).
            if hidden.get(i).copied().unwrap_or(false) {
                return format!("const {}* {}", c_type(&p.ty), c_ident(&p.name));
            }
            // A method that changes the value it is called on is given the
            // place it is in, so what it writes is what the caller holds, and
            // so is a `flex&` parameter.
            // A `fixed&` one is only read there, so it is given a place that
            // never changes as well: `T const*`, which for a pointer is a
            // pointer that stays, not what it points to (`T* const*`). So is a
            // `bind&` one of a type with no contents, which acts as `fixed&`.
            if p.passing == Passing::FixedRef || (p.passing == Passing::BindRef && !has_contents(&p.ty, is_shape)) {
                return format!("{} const* {}", c_type(&p.ty), c_ident(&p.name));
            }
            if is_pointer_param(p, alters) {
                return format!("{}* {}", c_type(&p.ty), c_ident(&p.name));
            }
            if owns_copy(t, p, alters, false) {
                return format!("{} {}", c_type(&p.ty), given_name(&p.name));
            }
            format!("{} {}", c_type(&p.ty), c_ident(&p.name))
        })
        .collect();
    // A lambda that keeps what it uses is given what it keeps first.
    let params: Vec<String> = t.kept.as_ref().map(|_| format!("void* {}", KEPT_PARAM)).into_iter().chain(params).collect();
    // `()` in C declares a function with unspecified parameters; `(void)` means none.
    let params = if params.is_empty() { "void".to_string() } else { params.join(", ") };
    format!("{} {}({})", c_type(&t.ret), mangle(&t.name), params)
}

// C names. Vela names start with an ASCII letter and never contain `__`, so
// the generated C names below have distinct shapes and can't collide with each
// other, with the runtime (`vela__rt__*`), or with C:
//
//   vela_Point, vela_area         tasks, shapes, kinds          (`mangle`)
//   vela_shapes__1__geometry__0__area   another module's          (`c_name`)
//   vela___2____1__common__1__text__0__pad   one above the main file's folder
//   vela_Shape__Tag               a kind's tag enum type        (`tag_type`)
//   vela_Shape__tag_Circle        a variant's tag value         (`tag_value`)
//   vela__int                     a local or field named like C (`c_ident`)
//   vela__maybe__int              a `maybe` struct              (`c_type`)
//   vela__or__int__n_ParseError   an `or` struct                (`c_type`)
//   vela__write__maybe__int       a value put in a writer       (`write_helper`)
//   vela__text__array__4__int     a value made into text        (`text_helper`)
//   vela__array__4__int           an array struct               (`c_type`)
//   vela__slice__int              a slice struct                (`c_type`)
//   vela__at__slice__int          a helper for a slice or array (`item_helper`)
//   vela__end__0, vela__empty__   compiler temporaries and members
//
// A region is a local or parameter like any other (`c_ident`); regions don't
// change C types, so `^int@a` and `^int@b` are both `int64_t*`.

/// The C name of the function that takes task `name`'s values as its type
/// does (see `Generator::gen_adapter`). No Vela name holds `__`.
fn adapter_name(name: &str) -> String {
    format!("{}__as_value", mangle(name))
}

/// The C name of a task, shape, or kind.
fn mangle(name: &str) -> String {
    format!("vela_{}", c_name(name))
}

fn tag_type(kind: &str) -> String {
    format!("vela_{}__Tag", c_name(kind))
}

/// The C name of shared field `field` of shape `shape`: a variable of its own,
/// one for the whole program. No Vela name contains `__`, so it is no other
/// name. (Kept apart from `gen_expr`, whose frame is on the stack once per
/// nesting level.)
#[inline(never)]
fn shared_name(shape: &str, field: &str) -> String {
    format!("vela_{}__shared_{}", c_name(shape), field)
}

/// The C name of a macro, a `#define`.
fn macro_name(name: &str) -> String {
    format!("VELA_MACRO_{}", c_name(name))
}

fn tag_value(kind: &str, tag: &str) -> String {
    format!("vela_{}__tag_{}", c_name(kind), tag)
}

/// A task, shape, or kind name as part of a C name: the name with its module's
/// path folded into it (see `flat_name`). No Vela name contains `__`, and none
/// starts with a digit, so the result can't be any other name, or read as a
/// word the generated names use.
fn c_name(name: &str) -> String {
    flat_name(name)
}

/// The C name of a local, parameter, field, or variant union member: the Vela
/// name itself, unless C could read it as something else, such as a keyword
/// or a macro from a standard header.
fn c_ident(name: &str) -> String {
    // The value a method is called on is named `.this`, which no Vela name can
    // be (see `ast::THIS`), so C needs a name for it.
    if name == THIS {
        return "vela__this".to_string();
    }
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
        // A generic declaration is never generated: each use is generated with
        // the type it gives, so these would be compiler bugs (see `instances`
        // and `shape_instance`).
        Type::Param(name) => format!("/*type {}*/", name),
        Type::Group(name) => format!("/*group {}*/", name),
        Type::Applied(applied) => format!("/*shape {}*/", applied.name),
        // Nor is a program with a type the checker couldn't work out, since that
        // program has an error (see `Type::Unknown`).
        Type::Unknown => "/*unknown type*/".to_string(),
        Type::Int => "int64_t".to_string(),
        Type::Unit | Type::Char => "uint8_t".to_string(),
        Type::Float => "double".to_string(),
        Type::Bool => "bool".to_string(),
        Type::Text(_) => "vela__rt__text".to_string(),
        Type::Nothing => "void".to_string(),
        Type::None => "void*".to_string(),
        Type::Ptr(t, _) => format!("{}*", c_type(t)),
        Type::Named(n, _) => mangle(n),
        Type::Region => "vela__rt__region*".to_string(),
        Type::Range(t) => format!("/*range<{}>*/", c_type(t)),
        // A `maybe` pointer is the pointer itself, with `NULL` for none.
        Type::Maybe(inner) if is_pointer(inner) => c_type(inner),
        Type::Maybe(inner) => format!("vela__maybe__{}", type_word(inner)),
        Type::Or(value, error) => format!("vela__or__{}__{}", type_word(value), type_word(error)),
        Type::Fail(error) => c_type(error),
        Type::Results(_) | Type::Tuple(_) | Type::Array(..) | Type::Slice(..) => format!("vela__{}", type_word(ty)),
        // A map is a pointer to its table's header, in its region.
        Type::Map(..) => format!("vela__{}*", type_word(ty)),
        // A task is a pointer to its function (see `Generator::gen_task_type`),
        // and a lambda that keeps what it uses that and what it keeps.
        Type::Task(_) | Type::Closure(_) => format!("vela__{}", type_word(ty)),
    }
}

/// The C name of the struct a compound type is: its C type, or for a map the
/// header its values point to.
fn compound_name(ty: &Type) -> String {
    match ty {
        Type::Map(..) => format!("vela__{}", type_word(ty)),
        other => c_type(other),
    }
}

/// The C struct of one entry of a map's table: its hash, key, and value.
fn map_entry(ty: &Type) -> String {
    format!("vela__entry__{}", type_word(ty))
}

/// The type of the items of an array or slice type.
fn item_type(ty: &Type) -> &Type {
    match ty {
        Type::Array(items, _) | Type::Slice(items, _) => items,
        other => other,
    }
}

/// The C name of a generated helper for an array or slice type:
/// `vela__at__slice__int`, `vela__fill__array__4__int`.
fn item_helper(what: &str, ty: &Type) -> String {
    format!("vela__{}__{}", what, type_word(ty))
}

/// What a lookup (`xs[#i]`, `t.at(i)`) into a value of type `ty` gives when
/// it finds an item: the item, or a char of text.
fn lookup_item(ty: &Type) -> Type {
    match ty {
        Type::Text(_) => Type::Char,
        Type::Array(items, _) | Type::Slice(items, _) => items.without_regions(),
        other => other.clone(),
    }
}

/// The type a lookup's helper is made for, one per kind of value and item: an
/// array of any count is read through its items (`[T#0]` stands for them all).
fn lookup_key(ty: &Type) -> Type {
    match ty.without_regions() {
        Type::Array(items, _) => Type::Array(items, 0),
        other => other,
    }
}

/// The C name of a generated helper for copies of values of type `ty`:
/// `vela__copied__ptr__n_Node`, `vela__fix__text`.
/// The C name of the function making a value of kind `from` one of kind `to`,
/// which imitates it (see `Generator::gen_widens`).
/// The value a pipe's call holds as its `Piped` argument (see
/// `ExprKind::Pipe`), where fitting the parameter may have wrapped it.
fn piped_value(call: &Expr) -> Option<&Expr> {
    let (ExprKind::Call { args, .. } | ExprKind::Invoke { args, .. } | ExprKind::Construct { args, .. }) = &call.kind else { return None };
    args.iter().find_map(|arg| {
        let mut arg = arg;
        loop {
            match &arg.kind {
                ExprKind::Piped(value) => return Some(value.as_ref()),
                ExprKind::Wrap(inner) | ExprKind::Widen(inner) | ExprKind::Kept { value: inner, .. } => arg = inner,
                _ => return None,
            }
        }
    })
}

fn widen_helper(from: &Type, to: &Type) -> String {
    format!("vela__widen__{}__into__{}", type_word(&from.without_regions()), type_word(&to.without_regions()))
}

fn copy_helper(what: &str, ty: &Type) -> String {
    format!("vela__{}__{}", what, type_word(&ty.without_regions()))
}

/// The type whose fix a value of type `ty` takes: a `maybe` pointer is the
/// pointer itself in C, `NULL` for none.
fn fix_type(ty: &Type) -> Type {
    match ty.without_regions() {
        Type::Maybe(inner) if is_pointer(&inner) => *inner,
        other => other,
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
