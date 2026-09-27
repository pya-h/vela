use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::error::{Error, Result};
use crate::modules::{IncludeScope, ModuleScope, Program};

#[derive(Clone)]
struct VariantInfo {
    /// The kind the variant belongs to.
    kind: String,
    variant: Variant,
}

/// One block's names, and the region its allocations use when they name none:
/// what `region r default` or a `r: region default` parameter set (see
/// `ast::DEFAULT` and `Checker::default_region`), with where it was written.
#[derive(Default)]
struct Scope {
    names: HashMap<String, VarInfo>,
    default: Option<(String, Span)>,
}

#[derive(Clone)]
struct VarInfo {
    /// `Type::Region` for a region.
    ty: Type,
    mutable: bool,
    /// Where the variable was declared.
    span: Span,
    /// For a region: whether `new` can allocate in it. A region a task only
    /// names in its header's types (`p: ^int@r`) can't be.
    allocates: bool,
    /// Whether this is a named result, which only `<-` sets.
    result: bool,
    /// Whether the task's header declares it, which no code can change.
    param: bool,
    /// For a reference, `hold& y = &x` or `bind& y = &x`: the place it names,
    /// from a variable that isn't a reference itself (see `resolved_path`).
    refers: Option<Vec<Step>>,
}

/// What a place an assignment writes to takes: nothing (`_`), a value of a
/// type, or the items of a tuple, `( … )` at a position.
enum TargetType {
    Discard,
    Place(Type),
    Tuple(Vec<TargetType>, Span),
}

/// A name a pattern or a plan B declares: its name, where it is written, its
/// type, and whether it can be assigned (`hold` in a pattern).
type Declared = (String, Span, Type, bool);

/// Where a type is written, which decides the regions it may name.
#[derive(Clone, Copy, PartialEq)]
enum TypePlace {
    /// A shape's field or a variant's payload: no regions, since fields live in
    /// the region of the value holding them.
    Field,
    /// The type after `new`, which lives in the region `new` allocates in.
    New,
    /// A shape's shared field, which lives as long as the program, and so in no
    /// region (see `check_shared_type`).
    Shared,
    /// A task's header: any region names, which the task is given.
    Header,
    /// Inside a task: only regions visible there.
    Body,
}

/// Type-checks a program and annotates it for code generation: every
/// expression gets its `ty`, and every binding without an annotation gets its
/// inferred type. Code generation relies on these annotations.
#[derive(Default)]
pub struct Checker {
    shapes: HashMap<String, ShapeDef>,
    kinds: HashMap<String, KindDef>,
    /// Every task's type parameters, parameters (names and types), and return
    /// type.
    tasks: HashMap<String, TaskInfo>,
    /// The type parameters of the task being checked, with what each requires.
    /// Empty for an ordinary task.
    type_params: Vec<TypeParam>,
    /// What the task being checked is a method of, when it is one: its own name
    /// and whether it may change the value it is called on.
    current_method: Option<Owner>,
    /// The shape or kind whose `alters` method is being checked, as `owner_of`
    /// names it: the only code that may change its `read` fields.
    current_owner: Option<String>,
    /// The shapes and kinds that hold pointers, directly or in other shapes and
    /// kinds, so their values live in a region.
    regional: HashSet<String>,
    /// The shapes and kinds that can be given a region: those in `regional`, and
    /// those holding text (see `textual_types`), whose text is in the region
    /// when they name one (`Person@r`) and lasts as long as the program when
    /// they don't (`Person`).
    takes_region: HashSet<String>,
    /// The shapes and kinds holding something whose value can change: a
    /// pointer, a slice, or an array, directly or in other shapes and kinds by
    /// value. A tuple can't hold them.
    changeable: HashSet<String>,
    /// Every variant, by its name in the program: its tag, qualified with its
    /// kind's module outside the main file (`geometry.Circle`). Tags are unique
    /// across the kinds of a module.
    variants: HashMap<String, VariantInfo>,
    /// Each module of the program, by name; the main file's is empty.
    modules: HashMap<String, ModuleScope>,
    /// The tasks, shapes, kinds, and variants marked `priv` (a variant through
    /// its kind), by their names in the program.
    private: HashSet<String>,
    /// The module whose code is being checked.
    current_module: String,
    scopes: Vec<Scope>,
    current_ret: Option<Type>,
    /// The names of the current task's results, in order, when it names them.
    result_names: Vec<String>,
    /// Those of them not set on the path being checked. Vela has no zero
    /// values, so a result must be set before it is read, before a bare `<-`,
    /// and before the end of the task, on every path.
    unset: HashSet<String>,
    /// Where the parts of written types are, from the parser.
    type_places: TypePlaces,
    /// At most how many bytes a value of each shape and kind takes, once the
    /// declarations are checked (see `check_sizes`).
    sizes: HashMap<String, u128>,
    /// Where each `copy()` without a written region is, until the place it goes
    /// gives it one (see `settle_copies`). A statement must settle the ones it has.
    unsettled_copies: Vec<Span>,
    /// Where each call of a generic task whose types aren't all worked out yet
    /// is, with the task's name and the first type still unknown, until the
    /// place its value goes names it (see `settle_types`).
    unsettled_types: Vec<(Span, String, String)>,
    /// One task to make for each set of types a call gives a generic task: the
    /// name of the task made for them, the generic task's name, the types, and
    /// where the call that needs it is.
    instances: Vec<(String, String, Vec<Type>, Span)>,
    /// The shapes being made for their types, innermost last: a field of one may
    /// name a shape given types of its own, which needs that shape made first
    /// (see `shape_instance`).
    making_shapes: Vec<String>,
    /// For each shape made for a set of types, the shape and the types it was
    /// made for (`Box__of__int` is `Box` given `int`), so a use of a declaration
    /// that takes types can tell what a value's type gives it (see
    /// `unify_params`).
    made_of: HashMap<String, Applied>,
    /// The shapes made for one set of types each (see `shape_instance`), to add
    /// to the program's items.
    made_shapes: Vec<Item>,
    /// How each shape made for a set of types is written in a program
    /// (`lib/util.Box<int>` for `lib/util.Box__of__int`), so an error names it as
    /// the program writes it. Applied to every message (see `check_program`).
    written_names: HashMap<String, String>,
    /// For each generic task, the copies its body makes of values of the types
    /// it takes into a region its header doesn't keep them in: the type's name,
    /// the region the copy goes into, whether the header names that region at
    /// all, and where the copy is written. Such a copy is only the value itself,
    /// so every use must give a type that holds nothing in a region (see
    /// `check_copies_of`).
    loose_copies: HashMap<String, Vec<(String, String, bool, Span)>>,
    /// Every mistake found so far, in the order the checks found them. The
    /// checker goes on after one, so a file's mistakes are all reported
    /// together (see `record` and `Type::Unknown`).
    errors: Vec<Error>,
    /// The tasks, shapes, and kinds whose own declaration has a mistake. What
    /// they promise isn't known, so a use of one is checked as far as it can be
    /// and then left alone: reporting the declaration's mistake again at every
    /// use would bury the file's other mistakes (see `Type::Unknown`).
    broken: HashSet<String>,
    /// The shapes whose defaults are checked, or being checked (see
    /// `check_defaults_of`).
    defaults_checked: HashSet<String>,
    /// The field whose default is being checked, whose value is written with no
    /// task or method around it (see `check_field_default`).
    in_default: Option<String>,
    /// The shared field whose first value is being checked, as its shape and
    /// its index there, which may name a shared field above it (see
    /// `check_shared_value`).
    in_shared: Option<(String, usize)>,
    /// The first values of shared fields, checked, by `shared_key`: `None` for
    /// one with a mistake, which is reported once. Each is checked when it is
    /// first needed, by its own shape or by a shared field that names it.
    shared_values: HashMap<String, Option<Expr>>,
    /// The shared fields whose first value is being checked, innermost last,
    /// so a cycle of them is found (see `check_shared_value`).
    shared_checking: Vec<String>,
    /// The shape whose method is being checked, shared or not: the only code
    /// that may change its `shared read` fields.
    current_shape: Option<String>,
    /// Whether a call was refused for leaving out the region it takes last,
    /// which a chain joined `at r` reads (see `check_join`).
    region_wanted: bool,
}

/// What a call needs to know about a task: the types it takes, its parameters,
/// what it sends, and what it is a method of, when it is one.
#[derive(Clone)]
struct TaskInfo {
    types: Vec<TypeParam>,
    params: Vec<(String, Type)>,
    /// How each parameter takes its value, in the order of `params`.
    passing: Vec<Passing>,
    ret: Type,
    owner: Option<Owner>,
}

/// The error for `x.str()` or `text(x)` where neither the call nor the block
/// names a region for the new text to go in.
const STR_NEEDS_REGION: &str =
    "'str' makes new text, so it names the region the text goes in, as in 'x.str() at r', 'x.str()@r', 'x.str(r)', or 'text(x) at r'";

/// The error for joining text where neither the join nor the block names a
/// region for the new text to go in.
const JOIN_NEEDS_REGION: &str =
    "joining text makes new text, which goes in a region; name it after the last part, as in 'a + b at r', or after parentheses, as in '(a + b)@r'";

/// The error for a `copy()` whose region nothing names.
const UNSETTLED_COPY: &str =
    "can't tell which region to copy into, since nothing this copy goes into names one; name it, as in 'x.copy() at r' or 'x.copy()@r'";

/// The end of that error where the block has a default region: a copy goes where
/// the value it becomes goes, so a default is no answer for one.
const COPY_IGNORES_DEFAULT: &str = "; a copy goes where the value it becomes goes, so a block's default region doesn't settle one";

/// What a `match` or a `when` used as a value calls itself and its parts, so
/// that one set of checks words its messages for either (see
/// `Checker::check_branching_value`).
#[derive(Clone, Copy)]
struct Branches {
    /// The statement: `match` or `when`.
    of: &'static str,
    /// One of its parts: a `case`, or a `when`'s branch.
    each: &'static str,
}

const MATCH_BRANCHES: Branches = Branches { of: "match", each: "case" };
const WHEN_BRANCHES: Branches = Branches { of: "when", each: "branch" };

impl Branches {
    /// What a branch that gives no value is missing.
    fn needs_value(self) -> String {
        format!("a {} of a {} used as a value gives it on its last line, or sends with '<-' on every path", self.each, self.of)
    }
}

/// The name a shared field is known by in the checker's tables, and as a place:
/// its shape's name in the program, a dot, and its own (`Point.made`). No
/// variable is named with a dot.
fn shared_key(shape: &str, field: &str) -> String {
    format!("{}{}{}", shape, MODULE_SEPARATOR, field)
}

/// The `File` shape of `std/files`, which a shared field can't hold (see
/// `Checker::check_shared_type`).
const FILE_SHAPE: &str = "std/files.File";

/// At most how many tasks one generic task may be made into, and how deeply a
/// type given to one may be built up. A generic task whose call gives a type
/// built from its own (`f<(T, int)>` inside `f<T>`) would need an endless
/// number of them, so it stops at these.
const MAX_INSTANCES: usize = 500;
const MAX_TYPE_DEPTH: usize = 24;

/// The most memory a value may take, in bytes: 1 TiB. A larger type is an
/// error, so that no C compiler rejects the program for it.
const MAX_VALUE_BYTES: u128 = 1 << 40;

impl Checker {
    pub fn new() -> Self {
        Self::default()
    }

    /// A checker that points errors about a part of a written type (a region,
    /// a type name, the error type) at that part, using the parser's
    /// `Parser::type_places`.
    pub fn with_type_places(type_places: TypePlaces) -> Self {
        Checker { type_places, ..Self::default() }
    }

    /// The mistakes found so far, in the order they were found.
    pub fn errors(&self) -> &[Error] {
        &self.errors
    }

    /// Records a mistake and goes on checking. The check that found it stops,
    /// and the next declaration or statement is checked as if it had passed, so
    /// one mistake never hides the rest of the file. What the failed check was
    /// working out is left as `Type::Unknown`, which no later check complains
    /// about.
    fn record(&mut self, error: Error) {
        self.errors.push(error);
    }

    /// Checks each of `values` on its own, recording the mistakes they have.
    /// Used where nothing is known about the place they go — a call of a task
    /// whose header has a mistake, say — so that the values are still read for
    /// mistakes of their own (see `broken`).
    fn check_each(&mut self, values: &mut [Expr]) {
        for value in values {
            // A region argument is a region's name, not a value. Without the
            // task's parameters there is no telling which arguments are regions,
            // so a name that is one here is left alone.
            if matches!(&value.kind, ExprKind::Var(name) if self.lookup(name).is_some_and(|info| info.ty == Type::Region)) {
                continue;
            }
            // A region written after the call only needs to be one.
            if let ExprKind::RegionAfter(region) = &value.kind {
                let known = self.visible_region(region, value.span, false);
                self.keep_going(known);
                continue;
            }
            let checked = self.check_expr(value);
            self.keep_going(checked.map(|_| ()));
        }
    }

    /// Records the mistake a check that works out a type found, and stands
    /// `Type::Unknown` in for the type it couldn't work out. Used where what
    /// follows can be checked without that type: the block a condition guards,
    /// the cases of a match, the fallback of a plan B.
    fn or_unknown(&mut self, found: Result<Type>) -> Type {
        match found {
            Ok(ty) => ty,
            Err(error) => {
                self.record(error);
                Type::Unknown
            }
        }
    }

    /// Records the mistake in `result`, if it found one. `true` when it passed.
    fn keep_going(&mut self, result: Result<()>) -> bool {
        match result {
            Ok(()) => true,
            Err(error) => {
                self.record(error);
                false
            }
        }
    }

    /// Checks a program loaded with `modules::load`, and annotates its items.
    /// Returns every mistake the program has, in source order, and nothing when
    /// it checks. The items are annotated as far as they checked, so a tool can
    /// read the types around a mistake (see `driver::Partial`).
    pub fn check_program(program: &mut Program) -> Vec<Error> {
        let mut checker = Checker::with_type_places(std::mem::take(&mut program.type_places));
        checker.modules = program.modules.iter().map(|scope| (scope.name.clone(), scope.clone())).collect();
        checker.declare_builtin_kinds();
        checker.check_file(&mut program.items);
        // What a program with a mistake would be made into is never generated,
        // and the parts of it a mistake left unchecked would only cause more, so
        // the program is completed only once it checks.
        if checker.errors.is_empty() {
            let made = checker.expand_instances(&mut program.items);
            checker.keep_going(made);
        }
        if checker.errors.is_empty() {
            program.items.extend(std::mem::take(&mut checker.made_shapes));
            // No file declares the kinds that reading a number sends, so
            // they join the program's own declarations here, after every
            // check that reads them, for the code generator to write their C.
            program.items.extend(builtin_kinds().into_iter().map(Item::Kind));
        }
        program.type_places = checker.type_places;
        // Names in a message are written as the file with the error writes them.
        let written = std::mem::take(&mut checker.written_names);
        // A shape made for one set of types is named as the program gives
        // them: `Box<int>`, not the name the compiler made for it. The
        // longest name first, so one whose name starts another is not cut.
        let mut made: Vec<(&String, &String)> = written.iter().collect();
        made.sort_by_key(|(name, _)| std::cmp::Reverse(name.len()));
        let mut errors = std::mem::take(&mut checker.errors);
        for error in &mut errors {
            for (name, form) in &made {
                if error.message.contains(name.as_str()) {
                    error.message = error.message.replace(name.as_str(), form);
                }
            }
            if let Some(scope) = error.span.and_then(|span| program.modules.iter().find(|scope| scope.file == span.file)) {
                error.message = scope.spelling.text(&error.message);
            }
        }
        in_source_order(errors)
    }

    /// Whether a task, shape, kind, or variant name written in the current
    /// module can name the declaration called `name` in the program. The
    /// resolver has given every name a module's code can use its full name, so a
    /// name without a module is one of the main file's only in the main file —
    /// except for the builtin kinds, which belong to no file and so are written
    /// bare in every one (see `ast::builtin_kinds`).
    fn sees(&self, name: &str) -> bool {
        self.current_module.is_empty() || name.contains(MODULE_SEPARATOR) || BUILTIN_KIND_NAMES.contains(&name)
    }

    /// The program's name for `name` written unqualified in the current module:
    /// what `include` brings in under it, or else a name of the module itself.
    fn in_module(&self, name: &str) -> String {
        if let Some(full) = self.modules.get(&self.current_module).and_then(|scope| scope.imported(name)) {
            return full.to_string();
        }
        qualify(&self.current_module, name)
    }

    /// The program's name for a task, shape, kind, or variant name written in
    /// the current module: itself when qualified, and `in_module` otherwise.
    fn full_name(&self, name: &str) -> String {
        if name.contains(MODULE_SEPARATOR) { name.to_string() } else { self.in_module(name) }
    }

    /// The variant `name`, written unqualified in the current module, stands for.
    /// A variant of a builtin kind belongs to no module, so it is the same one in
    /// every file (see `ast::builtin_kinds`).
    fn variant_in_scope(&self, name: &str) -> Option<&VariantInfo> {
        if builtin_kind_of(name).is_some() {
            return self.variants.get(name);
        }
        self.variants.get(&self.in_module(name))
    }

    /// A hint for a name written in the current module that names nothing
    /// here, but that a module it includes declares: `; module 'm' has one: …`.
    /// Names of the program in it are written as the program names them, and
    /// the message is spelled for the file later (see `check_program`).
    fn elsewhere(&self, name: &str) -> String {
        let Some(scope) = self.modules.get(&self.current_module) else { return String::new() };
        let declared = |full: &str| {
            !self.private.contains(full) && (self.tasks.contains_key(full) || self.shapes.contains_key(full) || self.kinds.contains_key(full) || self.variants.contains_key(full))
        };
        let Some(include) = scope.includes.iter().find(|include| declared(&qualify(&include.id, name))) else { return String::new() };
        let full = qualify(&include.id, name);
        match scope.imports.iter().find(|(_, imported, _)| *imported == full) {
            // Brought in under its own name: the name is known, but not as what was written.
            Some((written, _, _)) if written == name => return String::new(),
            Some((alias, _, at)) => return format!("; it is brought in as '{}' at {}:{}", alias, at.line, at.col),
            None => {}
        }
        match &include.local {
            Some(module) => format!("; module '{}' has one: write '{}', or bring it in with 'include … ({})'", module, full, name),
            None => format!("; module '{}' has one: list it in 'include {} {{…}}'", include.written, include.written),
        }
    }

    /// The error for a shape literal of `name`, which names no shape.
    fn unknown_shape(&self, span: Span, name: &str) -> Error {
        match self.variants.get(name).filter(|_| self.sees(name)) {
            Some(info) => Error::at(
                span,
                format!("'{}' is a variant of kind '{}', not a shape; its values are written '{}(…)'", info.variant.tag, info.kind, name),
            ),
            None => Error::at(span, format!("unknown shape '{}'{}", name, self.elsewhere(name))),
        }
    }

    /// A hint for a variable name that names nothing, but that a module the
    /// current module includes would be named by, without its alias or braces.
    fn module_elsewhere(&self, name: &str) -> String {
        let Some(scope) = self.modules.get(&self.current_module) else { return String::new() };
        match scope.includes.iter().find(|include| include.file_name == name && include.local.as_deref() != Some(name)) {
            Some(IncludeScope { local: Some(alias), written, .. }) => format!("; module '{}' is included as '{}'", written, alias),
            Some(include) => format!("; module '{}' is included with only its parts, which are named alone", include.written),
            None => String::new(),
        }
    }

    /// Where the current module includes the module it names `name`, if it does.
    fn module_used(&self, name: &str) -> Option<Span> {
        let scope = self.modules.get(&self.current_module)?;
        scope.included(name).map(|include| include.span)
    }

    /// Where `part` (see `TypePlaces`) is in the type written at `span`, or
    /// `span` itself if it isn't known.
    fn place(&self, span: Span, part: &str) -> Span {
        let parts = self.type_places.get(&span.place_key());
        // A shape or kind of another module may be written without its module,
        // as in its own file or after `include m (Name)`.
        let found = |part: &str| parts.and_then(|parts| parts.iter().find(|(name, _)| name == part)).map(|(_, at)| *at);
        let spelled = self.modules.get(&self.current_module).map(|scope| scope.spelling.name(part));
        found(part).or_else(|| spelled.and_then(|spelled| found(&spelled))).or_else(|| found(unqualified(part))).unwrap_or(span)
    }

    /// Checks the items of a program, recording every mistake they have (see
    /// `errors` and `check_program`). Each declaration and each task body is
    /// checked whatever the ones before it found.
    pub fn check_file(&mut self, items: &mut [Item]) {
        self.collect_declarations(items);
        self.regional = regional_types(items);
        self.takes_region = textual_types(items).union(&self.regional).cloned().collect();
        self.changeable = types_holding(items, holds_changeable);
        for item in items.iter_mut() {
            if let Item::Task(t) = item {
                self.settle_receiver(t);
            }
        }
        self.check_declarations(items);
        for item in items.iter_mut() {
            if let Item::Task(t) = item {
                self.current_module = module_of(&t.name).to_string();
                // A mistake in a body is the body's own: the task's header was
                // checked with the other declarations, so calls of it elsewhere
                // are checked as usual.
                let checked = self.check_task(t);
                self.keep_going(checked);
            }
        }
        self.current_module.clear();
    }

    /// Checks the types one use gives a generic task against the copies its body
    /// makes of the values of those types (see `loose_copies`): a copy is a
    /// value of the region it is made in, and nothing in the header keeps a
    /// value of that type there, so the copy is only the value itself and the
    /// type given must hold nothing in a region. `at` is the use.
    fn check_copies_of(&self, task: &str, types: &[Type], at: Span) -> Result<()> {
        let Some(copies) = self.loose_copies.get(task) else { return Ok(()) };
        let Some(info) = self.tasks.get(task) else { return Ok(()) };
        let given: HashMap<&str, &Type> = info.types.iter().map(|param| param.name.as_str()).zip(types).collect();
        for (param, region, in_header, copy_at) in copies {
            let Some(ty) = given.get(param.as_str()) else { continue };
            let Some(lives) = ty.regions().into_iter().find(|region| *region != ANY_REGION) else { continue };
            let what = if *in_header {
                format!("nothing in its header keeps a '{}' in '{}', so that copy is only the value itself", param, region)
            } else {
                format!("'{}' is declared inside '{}' and freed when it ends, so that copy is only the value itself", region, unqualified(task))
            };
            let fix = if *in_header {
                format!("name the region in a parameter's type, as in 'xs: [{}]@{}', or write '{}' for {} itself", param, region, unqualified(task), ty)
            } else {
                format!("copy the value before the call, or write '{}' for {} itself", unqualified(task), ty)
            };
            return err(
                at,
                format!(
                    "'{}' copies a '{}' into region '{}' at {}:{}, and {}; this use gives {}, which is in region '{}'. So {}",
                    unqualified(task),
                    param,
                    region,
                    copy_at.line,
                    copy_at.col,
                    what,
                    ty,
                    lives,
                    fix
                ),
            );
        }
        Ok(())
    }

    /// Makes one task for each set of types a call gives a generic task, with
    /// the types in place of its type parameters, and points every such call at
    /// the one made for it. A generic task itself generates nothing: its body is
    /// checked once, with `T` standing for the type each use gives.
    ///
    /// Making one can need another (a generic task calling a generic task), so
    /// they are made from a list until it is empty. A task whose call gives a
    /// type built from its own (`f<(T, int)>` inside `f<T>`) would never stop,
    /// so a type nested too deeply, or too many tasks, is an error.
    fn expand_instances(&mut self, items: &mut Vec<Item>) -> Result<()> {
        let generic: HashMap<String, TaskDef> = items
            .iter()
            .filter_map(|item| match item {
                Item::Task(t) if !t.types.is_empty() => Some((t.name.clone(), t.clone())),
                _ => None,
            })
            .collect();
        let mut made: HashSet<String> = HashSet::new();
        let mut queue: Vec<(String, String, Vec<Type>, Span)> = self.instances.drain(..).collect();
        let mut tasks: Vec<Item> = Vec::new();
        while let Some((key, name, types, at)) = queue.pop() {
            if !made.insert(key.clone()) {
                continue;
            }
            if made.len() > MAX_INSTANCES {
                return err(at, format!("'{}' is used with more than {} different types, which no program needs; a generic task whose call gives a type built from its own never stops", unqualified(&name), MAX_INSTANCES));
            }
            let Some(source) = generic.get(&name) else {
                return Err(Error::internal_at(at, format!("no generic task '{}' to make for {} types", name, types.len())));
            };
            self.check_copies_of(&name, &types, at)?;
            if types.iter().any(|ty| type_depth(ty) > MAX_TYPE_DEPTH) {
                return err(at, format!("this use of '{}' gives a type built up more than {} types deep; a generic task whose call gives a type built from its own never stops", unqualified(&name), MAX_TYPE_DEPTH));
            }
            let mut task = source.clone();
            task.name = key;
            task.types = Vec::new();
            task.given = types.clone();
            let given: HashMap<String, Type> = source.types.iter().map(|param| param.name.clone()).zip(types).collect();
            let mut made = Made::default();
            self.substitute_task(&mut task, &given, &mut made)?;
            for (name, types, at) in made.needed {
                queue.push((instance_name(&name, &types), name, types, at));
            }
            tasks.push(Item::Task(task));
        }
        items.extend(tasks);
        Ok(())
    }

    /// Makes `ParseError`, `InputError` and their variants known, as if a file
    /// had declared them: reading a number from text sends the first (see
    /// `check_number_cast`) and reading one from input the second (see
    /// `check_scan`), and `match`, `is`, and a plan B read either as they read
    /// any kind. No program may take these names (see `collect_declarations`).
    fn declare_builtin_kinds(&mut self) {
        for kind in builtin_kinds() {
            for variant in &kind.variants {
                let info = VariantInfo { kind: kind.name.clone(), variant: variant.clone() };
                self.variants.insert(variant.tag.clone(), info);
            }
            self.kinds.insert(kind.name.clone(), kind);
        }
    }

    /// Registers every task, shape, kind, and variant. They share one
    /// namespace: no two may have the same name, and none may use the name of
    /// a builtin type or task.
    fn collect_declarations(&mut self, items: &[Item]) {
        let mut names: HashMap<String, Span> = HashMap::new();
        // The mistakes found here, kept aside while `names` is borrowed, and
        // recorded once the items are all read. A name declared twice keeps the
        // first declaration, so the checks that follow have one of each name.
        let mut found: Vec<Error> = Vec::new();
        let mut declare = |name: &str, span: Span| -> Result<()> {
            // No declaration may take the name of a builtin type or task.
            let written = unqualified(name);
            if Type::builtin(written).is_some() {
                return err(span, format!("'{}' is a builtin type and cannot be redefined", written));
            }
            if BUILTIN_TASKS.contains(&written) {
                return err(span, format!("'{}' is a builtin task and cannot be redefined", written));
            }
            // The kinds reading a number sends, and their variants: a program
            // gets them without declaring them, and writes them as it writes
            // any kind's, so those names are their own.
            if BUILTIN_KIND_NAMES.contains(&written) {
                let what = match builtin_kind_of(written) {
                    Some(kind) => format!("a variant of the builtin kind '{}'", kind),
                    None => "a builtin kind".to_string(),
                };
                return err(span, format!("'{}' is {} and cannot be redefined", written, what));
            }
            if let Some(at) = names.get(name) {
                return err(span, format!("'{}' is already defined at {}:{}", written, at.line, at.col));
            }
            names.insert(name.to_string(), span);
            Ok(())
        };
        for item in items {
            let private = match item {
                Item::Task(t) => t.private,
                Item::Shape(s) => s.private,
                Item::Kind(k) => k.private,
            };
            if private {
                self.private.insert(item.name().to_string());
                if let Item::Kind(k) = item {
                    self.private.extend(k.variants.iter().map(|v| qualify(module_of(&k.name), &v.tag)));
                }
            }
            // A name already taken is reported, and the declaration taking it
            // is left out: every later check reads the first declaration of the
            // name, as it would have before the second was written.
            let mut taken = |name: &str, span: Span| -> bool {
                match declare(name, span) {
                    Ok(()) => false,
                    Err(error) => {
                        found.push(error);
                        true
                    }
                }
            };
            match item {
                Item::Shape(s) => {
                    if taken(&s.name, s.span) {
                        continue;
                    }
                    self.shapes.insert(s.name.clone(), s.clone());
                }
                Item::Kind(k) => {
                    if taken(&k.name, k.span) {
                        continue;
                    }
                    for v in &k.variants {
                        let full = qualify(module_of(&k.name), &v.tag);
                        if taken(&full, v.span) {
                            continue;
                        }
                        let info = VariantInfo { kind: k.name.clone(), variant: v.clone() };
                        self.variants.insert(full, info);
                    }
                    self.kinds.insert(k.name.clone(), k.clone());
                }
                Item::Task(t) => {
                    if taken(&t.name, t.span) {
                        continue;
                    }
                    // A method is written as a member of its shape or kind, so
                    // every message names it that way (see `written_names`).
                    if let Some(owner) = &t.owner
                        && let Some(ty) = t.owner_type()
                    {
                        let written = format!("{}{}{}", ty, MODULE_SEPARATOR, owner.name);
                        self.written_names.insert(t.name.clone(), written);
                        let short = method_name(unqualified(ty), &owner.name);
                        self.written_names.insert(short, format!("{}{}{}", unqualified(ty), MODULE_SEPARATOR, owner.name));
                    }
                    let params = t.params.iter().map(|p| (p.name.clone(), p.ty.clone())).collect();
                    self.tasks.insert(t.name.clone(), TaskInfo { types: t.types.clone(), params, passing: t.params.iter().map(|p| p.passing).collect(), ret: t.ret.clone(), owner: t.owner.clone() });
                }
            }
        }
        for error in found {
            self.record(error);
        }
    }

    /// Checks the declarations themselves: every written type exists, field
    /// names are distinct, kinds have variants, and no type contains itself.
    /// A task's header gets its types with regions settled (see `resolve_type`).
    fn check_declarations(&mut self, items: &mut [Item]) {
        for item in items.iter_mut() {
            let name = item.name().to_string();
            self.current_module = module_of(&name).to_string();
            let mut declared = self.check_private_types(item);
            if declared.is_ok() {
                declared = self.check_declaration(item);
            }
            if !self.keep_going(declared) {
                // What the declaration promises isn't settled, so every use of
                // it is left alone (see `broken`).
                self.broken.insert(name);
            }
        }
        let mut acyclic = HashSet::new();
        let mut order = Vec::new();
        for item in items.iter() {
            let name = match item {
                Item::Shape(s) => s.name.clone(),
                Item::Kind(k) => k.name.clone(),
                Item::Task(_) => continue,
            };
            if let Err((error, cycle)) = self.check_value_cycle(&name, &mut acyclic, &mut order) {
                self.record(error);
                for found in cycle {
                    acyclic.insert(found.clone());
                    self.broken.insert(found);
                }
                self.broken.insert(name);
            }
        }
        self.current_module.clear();
        self.check_sizes(&order);
        self.check_constructible(items);
        self.check_field_defaults(items);
        self.check_shared_fields(items);
    }

    /// Checks the value each field falls back on, after every declaration is
    /// settled. It comes last because such a value can be a literal of another
    /// shape, whose own left-out fields are filled in to check it: a shape that
    /// held itself would fill itself forever, and by now it is one whose fields
    /// aren't settled, which no literal reads (see `broken`).
    fn check_field_defaults(&mut self, items: &[Item]) {
        for item in items {
            if let Item::Shape(shape) = item {
                self.check_defaults_of(&shape.name);
            }
        }
    }

    /// Checks the defaults of the shape `name` once, as its own module reads
    /// them, and marks it broken when one has a mistake. A default that is a
    /// literal of a shape declared later checks that shape's first, so each
    /// mistake is told once and as its own shape says it. Nothing loops: a
    /// default holds no pointer, and a shape that holds itself by value is
    /// already broken (see `check_field_defaults`).
    #[inline(never)]
    fn check_defaults_of(&mut self, name: &str) {
        if self.broken.contains(name) || !self.defaults_checked.insert(name.to_string()) {
            return;
        }
        let Some(shape) = self.shapes.get(name).cloned() else { return };
        if shape.fields.iter().all(|field| field.default.is_none()) {
            return;
        }
        let module = std::mem::replace(&mut self.current_module, module_of(name).to_string());
        let types = std::mem::take(&mut self.type_params);
        self.enter_type_params(&shape.types);
        let mut checked = Ok(());
        for field in &shape.fields {
            if let Some(value) = &field.default {
                checked = self.check_field_default(&mut value.clone(), &field.ty, &field.name);
                if checked.is_err() {
                    break;
                }
            }
        }
        self.type_params = types;
        self.current_module = module;
        if !self.keep_going(checked) {
            self.broken.insert(name.to_string());
        }
    }

    /// Checks one declaration: a shape's or kind's fields, or a task's header.
    fn check_declaration(&mut self, item: &mut Item) -> Result<()> {
        match item {
            Item::Shape(s) => {
                // A shape's own types are in scope while its fields are
                // read, as a task's are while its header and body are.
                self.enter_type_params(&s.types);
                let fields = self.check_fields(&mut s.fields);
                self.type_params.clear();
                fields?;
                for member in &mut s.shared {
                    let field = &mut member.field;
                    field.ty = self.resolve_type(&field.ty, field.ty_span, TypePlace::Shared)?;
                    self.check_shared_type(&field.ty, field.ty_span, &field.name)?;
                }
                if let Some(shape) = self.shapes.get_mut(&s.name) {
                    shape.shared = s.shared.clone();
                }
                // The fields as they were read, with their regions settled,
                // so the shape made for one set of types (see
                // `shape_instance`) substitutes into them.
                if let Some(shape) = self.shapes.get_mut(&s.name) {
                    shape.fields = s.fields.clone();
                }
            }
            Item::Kind(k) => {
                if k.variants.is_empty() {
                    return err(k.span, format!("kind '{}' has no variants", k.name));
                }
                for v in &mut k.variants {
                    self.check_fields(&mut v.payload)?;
                }
                if let Some(kind) = self.kinds.get_mut(&k.name) {
                    kind.variants = k.variants.clone();
                }
            }
            Item::Task(t) => {
                self.check_header(t)?;
                if t.name == "main" {
                    if let Some(param) = t.params.first() {
                        return err(param.span, "task 'main' cannot take parameters");
                    }
                    if t.ret != Type::Unit {
                        return err(t.ret_span(), "task 'main' cannot return a value");
                    }
                }
            }
        }
        Ok(())
    }

    /// Settles how large a value of each shape and kind can be, in `order`,
    /// where each comes after the types it holds by value, and rejects one too
    /// large for memory (see `MAX_VALUE_BYTES`).
    fn check_sizes(&mut self, order: &[String]) {
        for name in order {
            let (bytes, span) = match (self.shapes.get(name), self.kinds.get(name)) {
                (Some(shape), _) => (fields_bytes(&shape.fields, &self.sizes), shape.span),
                (None, Some(kind)) => {
                    let largest = kind.variants.iter().map(|v| fields_bytes(&v.payload, &self.sizes)).max().unwrap_or(0);
                    (largest.saturating_add(8), kind.span)
                }
                (None, None) => continue,
            };
            if bytes > MAX_VALUE_BYTES {
                self.record(Error::at(span, format!("'{}' is too large: a value of it would take more than 1 TiB of memory", unqualified(name))));
                self.broken.insert(name.clone());
                continue;
            }
            self.sizes.insert(name.clone(), bytes);
        }
    }

    /// Rejects a type written at `span` whose values would take more than
    /// `MAX_VALUE_BYTES`, such as `[[int; 1000000]; 1000000]`.
    fn check_size(&self, ty: &Type, span: Span) -> Result<()> {
        if value_bytes(ty, &self.sizes) > MAX_VALUE_BYTES {
            return err(span, format!("{} is too large: a value of it would take more than 1 TiB of memory; a slice in a region can be as long as memory allows", ty));
        }
        Ok(())
    }

    /// Rejects a private type in what other files see of a public item: a
    /// task's parameters and results, a shape's public fields, and a kind's
    /// payloads. Code using the item couldn't name the type.
    fn check_private_types(&self, item: &Item) -> Result<()> {
        let (private, what, name, fields): (bool, &str, &str, Vec<&Field>) = match item {
            // A method is reached only through a value of its shape or kind, so
            // a private one keeps its methods private, and the type of the value
            // they are called on gives nothing away.
            Item::Task(t) if t.owner.is_some() => {
                let hidden = t.owner_type().is_some_and(|ty| self.private.contains(ty));
                (t.private || hidden, "method", &t.name, t.params.iter().skip(1).collect())
            }
            Item::Task(t) => (t.private, "task", &t.name, t.params.iter().collect()),
            Item::Shape(s) => (s.private, "shape", &s.name, s.fields.iter().filter(|f| !f.private).collect()),
            Item::Kind(k) => (k.private, "kind", &k.name, k.variants.iter().flat_map(|v| &v.payload).collect()),
        };
        if private {
            return Ok(());
        }
        let mut written: Vec<(&Type, Span)> = fields.iter().map(|f| (&f.ty, f.ty_span)).collect();
        if let Item::Task(t) = item {
            written.extend(t.ret.results().iter().enumerate().map(|(i, ty)| (ty, t.result_span(i))));
        }
        for (ty, span) in written {
            let hidden = named_types(ty).into_iter().find(|n| self.shapes.get(*n).is_some_and(|s| s.private) || self.kinds.get(*n).is_some_and(|k| k.private));
            if let Some(hidden) = hidden {
                let fix = if matches!(item, Item::Shape(_)) { "make the field 'priv' too, or the type public" } else { "make it 'priv' too, or the type public" };
                return err(
                    self.place(span, hidden),
                    format!("{} '{}' is public, but '{}' is private to its file, so other files couldn't name it; {}", what, unqualified(name), unqualified(hidden), fix),
                );
            }
        }
        Ok(())
    }

    /// Checks a shape's fields or a variant's payload.
    /// A shape literal, `Point { x: 1 }` or `Box<int> { value: none }`: every
    /// field given exactly once, and the region of the value they make.
    fn check_struct_init(&mut self, span: Span, name: &mut String, written: &mut Vec<TypeArg>, fields: &mut Vec<FieldInit>) -> Result<Type> {
        if self.sees(name) && self.kinds.contains_key(name.as_str()) {
            return refuse(|| Error::at(span, format!("'{}' is a kind, not a shape; create its values with its variants", name)));
        }
        let mut shape = self.shapes.get(name.as_str()).filter(|_| self.sees(name)).ok_or_else(|| self.unknown_shape(span, name))?.clone();
        self.check_defaults_of(name);
        // A shape whose own fields have a mistake takes nothing known, so the
        // values are read on their own (see `broken`).
        if self.broken.contains(name.as_str()) {
            let mut values: Vec<&mut Expr> = fields.iter_mut().map(|field| &mut field.value).collect();
            for value in &mut values {
                let checked = self.check_expr(value);
                self.keep_going(checked.map(|_| ()));
            }
            return Ok(Type::Unknown);
        }
        // A literal of a shape that takes types is a literal of the shape made
        // for them, so the rest reads as any other literal. Inside a declaration
        // that takes types, where a type given isn't known yet, it stays a
        // literal of the shape with the types it is given (see `substitute_expr`).
        let mut given_types: Option<Vec<Type>> = None;
        if !shape.types.is_empty() {
            let types = self.literal_types(span, name, written, &shape, fields)?;
            if types.iter().any(|ty| !ty.params().is_empty()) {
                *written = types.iter().map(|ty| (ty.clone(), span)).collect();
                given_types = Some(types);
            } else {
                let made = self.shape_instance(name, &types, span)?;
                *name = made.clone();
                written.clear();
                shape = self.shapes.get(&made).cloned().unwrap_or(shape);
            }
        } else if let Some((ty, at)) = written.first() {
            return refuse(|| Error::at(*at, format!("shape '{}' takes no types, so '< … >' has nothing to name; write '{}' alone", unqualified(name), ty)));
        }
        // A private field another file can't give is one only its own module can
        // build the shape at all — unless the shape gives it a value to fall back
        // on, which is what lets a shape keep a field of its own and still be
        // built anywhere.
        let outside = module_of(name) != self.current_module;
        if outside
            && let Some(hidden) = shape.fields.iter().find(|f| f.private && f.default.is_none())
        {
            return refuse(|| Error::at(
                span,
                format!(
                    "'{}' can't be built outside its file, since its field '{}' is private and falls back on no value; a task of its module can build it, or that field can name the value it falls back on",
                    name, hidden.name
                ),
            ));
        }
        // Fields may be given in any order, each exactly once.
        let mut seen: HashMap<String, Span> = HashMap::new();
        let mut declared = Vec::new();
        for field in fields.iter() {
            let Some(field_def) = shape.fields.iter().find(|f| f.name == field.name) else {
                if shape.shared.iter().any(|member| member.field.name == field.name) {
                    return refuse(|| Error::at(
                        field.span,
                        format!("'{0}' is a shared field of '{1}', one value for the whole program rather than part of each value, so no literal gives it; it is '{1}.{0}'", field.name, name),
                    ));
                }
                return refuse(|| Error::at(field.span, format!("shape '{}' has no field '{}'", name, field.name)));
            };
            if let Some(at) = seen.get(&field.name) {
                return refuse(|| Error::at(field.span, format!("field '{}' is already given at {}:{}", field.name, at.line, at.col)));
            }
            if field_def.private && outside && !field.filled {
                return refuse(|| Error::at(
                    field.span,
                    format!("'{}' is a private field of '{}', which only its own file can give; leave it out and it falls back on its own value", field.name, name),
                ));
            }
            // A `read` field that falls back on a value is the shape's own
            // business: no literal writes it, so nothing can make it say what
            // isn't so.
            if field_def.read && field_def.default.is_some() && !field.filled {
                return refuse(|| Error::at(
                    field.span,
                    format!(
                        "'{}' is a read-only field of '{1}' that falls back on its own value, so no literal gives it; only the alters methods of '{1}' change it",
                        field.name, name
                    ),
                ));
            }
            seen.insert(field.name.clone(), field.span);
            declared.push(field_def.clone());
        }
        // A field left out takes the value its shape falls back on, written into
        // the literal here so that nothing after this sees a literal without it.
        for field_def in &shape.fields {
            if seen.contains_key(&field_def.name) {
                continue;
            }
            let Some(default) = &field_def.default else {
                return refuse(|| Error::at(span, format!("missing field '{}' in '{}'", field_def.name, name)));
            };
            fields.push(FieldInit { name: field_def.name.clone(), span, value: default.clone(), filled: true });
            declared.push(field_def.clone());
        }
        // The types given in place of the names the shape takes, for a literal
        // whose types aren't all known yet.
        if let Some(types) = &given_types {
            let given: HashMap<&str, &Type> = shape.types.iter().map(|param| param.name.as_str()).zip(types).collect();
            for field in &mut declared {
                field.ty = field.ty.map_params(&mut |name| given.get(name).map(|ty| (*ty).clone()));
            }
        }
        // Filled in last, they stay last however often the literal is checked.
        let written = fields.iter().filter(|field| !field.filled).count();
        let mut values: Vec<&mut Expr> = fields.iter_mut().map(|field| &mut field.value).collect();
        let region = self.check_fields_of_value(&mut values, &declared, Some((written, module_of(name))))?;
        match given_types {
            Some(types) => {
                let region = self.region_of_applied(name, &shape, region);
                Ok(Type::Applied(Box::new(Applied { name: name.clone(), types, region })))
            }
            None => Ok(Type::Named(name.clone(), self.region_of_value(name, region))),
        }
    }

    /// The region of a value of a shape given types not all known yet: the one
    /// its values hold something in, when any type it may be given could.
    fn region_of_applied(&self, name: &str, shape: &ShapeDef, region: String) -> Option<String> {
        let may_hold = self.takes_region.contains(name) || shape.fields.iter().any(|field| !field.ty.params().is_empty());
        may_hold.then_some(region)
    }

    /// A written `Box<int>`: the shape made for those types, in the region the
    /// type names. Inside a declaration that takes types, where a type given
    /// isn't known yet (`Box<T>`), the application is kept as it is until the
    /// declaration is used.
    fn resolve_applied(&mut self, applied: &Applied, span: Span, place: TypePlace, inside: Option<&str>) -> Result<Type> {

        let name = &applied.name;
        let at = self.place(span, name);
        if self.type_param(name).is_some() {
            return err(at, format!("'{}' is a type this declaration takes, so it takes no types of its own", name));
        }
        if self.sees(name) && self.kinds.contains_key(name.as_str()) {
            return err(at, format!("'{}' is a kind, and a kind takes no types yet, so it can't be given any", unqualified(name)));
        }
        let Some(def) = self.shapes.get(name.as_str()).filter(|_| self.sees(name)).cloned() else {
            return Err(self.unknown_shape(at, name));
        };
        if def.types.is_empty() {
            return err(at, format!("shape '{}' takes no types, so '< … >' has nothing to name", unqualified(name)));
        }
        if def.types.len() != applied.types.len() {
            let names: Vec<&str> = def.types.iter().map(|param| param.name.as_str()).collect();
            return err(
                at,
                format!(
                    "shape '{}' takes {}, so name {}, as in '{}<{}>'",
                    unqualified(name),
                    count(def.types.len(), "type"),
                    if def.types.len() == 1 { "it" } else { "all of them" },
                    unqualified(name),
                    names.join(", ")
                ),
            );
        }
        // The types it is given are in the region of the value holding them, as
        // a shape's fields are, so they take the region written on the whole
        // type (or the one around it).
        let region = applied.region.as_deref().or(inside);
        let mut types = Vec::new();
        for ty in &applied.types {
            types.push(self.resolve_regions(ty, span, place, region)?);
        }
        for (param, ty) in def.types.iter().zip(&types) {
            self.check_need("shape", name, param, ty, at)?;
        }
        if types.iter().any(|ty| !ty.params().is_empty()) {
            // A shape given a type this declaration takes: whether its values
            // live in a region depends on the types given, so it names one
            // wherever any of them could.
            let may_hold = self.takes_region.contains(name.as_str()) || def.fields.iter().any(|field| !field.ty.params().is_empty());
            if may_hold && region.is_none() && matches!(place, TypePlace::Header | TypePlace::Body) {
                return err(
                    at,
                    format!(
                        "'{0}<{1}>' may hold text or pointers, since the types given may, so it names the region its values live in: write '{0}<{1}>@r', with 'r' a region the header names (such as an 'r: region' parameter)",
                        unqualified(name),
                        types.iter().map(Type::to_string).collect::<Vec<_>>().join(", ")
                    ),
                );
            }
            return Ok(Type::Applied(Box::new(applied.with_types(types))));
        }
        let key = self.shape_instance(name, &types, at)?;
        self.resolve_regions(&Type::Named(key, applied.region.clone()), span, place, inside)
    }

    /// The shape made for the types `types` give the shape `name`, made on the
    /// first use. The types name no region, as a shape's fields don't, so one
    /// shape serves every region: the field of `Box<text>` is `text`, which is
    /// in the region of the value holding it, so `Box<text>@r` and
    /// `Box<text>@s` are that one shape in two regions.
    fn shape_instance(&mut self, name: &str, types: &[Type], at: Span) -> Result<String> {
        let types: Vec<Type> = types.iter().map(Type::without_regions).collect();
        let key = instance_name(name, &types);
        if self.shapes.contains_key(&key) {
            return Ok(key);
        }
        let Some(source) = self.shapes.get(name).cloned() else {
            return Err(Error::internal_at(at, format!("no shape '{}' to make for {} types", name, types.len())));
        };
        if types.iter().any(|ty| type_depth(ty) > MAX_TYPE_DEPTH) {
            return err(
                at,
                format!(
                    "this use of '{}' gives a type built up more than {} types deep; a shape whose field gives it a type built from its own never stops",
                    unqualified(name),
                    MAX_TYPE_DEPTH
                ),
            );
        }
        if self.made_shapes.len() >= MAX_INSTANCES {
            return err(
                at,
                format!(
                    "more than {} shapes are made for the types they are given, which no program needs; a shape whose field gives it a type built from its own never stops",
                    MAX_INSTANCES
                ),
            );
        }
        // A field naming the same shape with the same types is the shape itself,
        // however deep the pointers go; one that gives it a type built from its
        // own (`Grow<Grow<T>>` in `Grow<T>`) would need an endless line of them.
        if self.making_shapes.len() >= MAX_TYPE_DEPTH {
            let names = source.types.iter().map(|param| param.name.clone()).collect::<Vec<_>>().join(", ");
            return err(
                at,
                format!(
                    "'{0}' would need more than {1} shapes made for it, one inside the next: a field of it gives it a type built from its own, which never stops. A field can name the shape with the types it takes, as in 'maybe ^{0}<{2}>'",
                    unqualified(name),
                    MAX_TYPE_DEPTH,
                    names
                ),
            );
        }
        // The name is registered before the fields are read, so a field naming
        // the same shape again (behind a pointer) is this shape rather than an
        // endless line of them.
        let written = format!("{}<{}>", name, types.iter().map(Type::to_string).collect::<Vec<_>>().join(", "));
        self.written_names.insert(key.clone(), written);
        self.made_of.insert(key.clone(), Applied { name: name.to_string(), types: types.clone(), region: None });
        let made = ShapeDef { name: key.clone(), types: Vec::new(), given: types.clone(), fields: Vec::new(), shared: Vec::new(), span: source.span, private: source.private };
        self.shapes.insert(key.clone(), made);
        self.making_shapes.push(key.clone());
        let fields = self.instance_fields(&source, types);
        self.making_shapes.pop();
        let fields = fields?;
        if let Some(shape) = self.shapes.get_mut(&key) {
            shape.fields = fields.clone();
        }
        // What its values hold, and how large they are, as for a shape the
        // program declares (see `check_file` and `check_sizes`).
        if fields.iter().any(|field| holds_pointers(&field.ty, &self.regional)) {
            self.regional.insert(key.clone());
        }
        if self.regional.contains(&key) || fields.iter().any(|field| holds_text(&field.ty, &self.takes_region)) {
            self.takes_region.insert(key.clone());
        }
        if fields.iter().any(|field| holds_changeable(&field.ty, &self.changeable)) {
            self.changeable.insert(key.clone());
        }
        let bytes = fields_bytes(&fields, &self.sizes);
        if bytes > MAX_VALUE_BYTES {
            return err(at, format!("'{}' is too large: a value of it would take more than 1 TiB of memory", key));
        }
        self.sizes.insert(key.clone(), bytes);
        if let Err((error, _)) = self.check_value_cycle(&key, &mut HashSet::new(), &mut Vec::new()) {
            return Err(error);
        }
        if let Some(shape) = self.shapes.get(&key) {
            self.made_shapes.push(Item::Shape(shape.clone()));
        }
        Ok(key)
    }

    /// The shape a literal of a shape that takes types is of: the types the
    /// literal names (`Box<int> { … }`), or the ones the values it gives show
    /// (`Box { value: 1 }`).
    fn literal_types(&mut self, span: Span, name: &str, written: &mut [TypeArg], shape: &ShapeDef, fields: &mut [FieldInit]) -> Result<Vec<Type>> {
        let mut given: HashMap<String, Type> = HashMap::new();
        if let Some((_, at)) = written.first() {
            if written.len() != shape.types.len() {
                let names: Vec<&str> = shape.types.iter().map(|param| param.name.as_str()).collect();
                return err(
                    *at,
                    format!(
                        "shape '{}' takes {}, so name all of them, as in '{}<{}> {{ … }}'",
                        unqualified(name),
                        count(shape.types.len(), "type"),
                        unqualified(name),
                        names.join(", ")
                    ),
                );
            }
            for ((ty, at), param) in written.iter_mut().zip(&shape.types) {
                // The types a shape is given name no region, as its fields
                // don't: the region comes from the values, as for any literal.
                *ty = self.resolve_type(ty, *at, TypePlace::Field)?;
                self.check_need("shape", name, param, ty, *at)?;
                given.insert(param.name.clone(), ty.clone());
            }
            return Ok(shape.types.iter().filter_map(|param| given.get(&param.name).cloned()).collect());
        }
        // Nothing written: the values say which types they are. They are checked
        // again with the shape made for them, so what this check left to settle
        // is left to that one.
        let copies = self.unsettled_copies.len();
        let untold = self.unsettled_types.len();
        let mut found = Ok(());
        for field in fields.iter_mut() {
            let Some(declared) = shape.fields.iter().find(|f| f.name == field.name) else { continue };
            match self.check_expr(&mut field.value) {
                Ok(ty) => {
                    self.match_params(&declared.ty, &ty, &mut given);
                }
                Err(error) => {
                    found = Err(error);
                    break;
                }
            }
        }
        self.unsettled_copies.truncate(copies);
        self.unsettled_types.truncate(untold);
        found?;
        let mut types = Vec::new();
        for param in &shape.types {
            let Some(ty) = given.get(&param.name) else {
                return err(
                    span,
                    format!(
                        "can't tell which type '{}' is for this '{}', since no field it is given names one; write it, as in '{}<int> {{ … }}'",
                        param.name,
                        unqualified(name),
                        unqualified(name)
                    ),
                );
            };
            self.check_need("shape", name, param, ty, span)?;
            types.push(ty.clone());
        }
        Ok(types)
    }

    /// The fields of the shape made for `types`: the shape's own, with those
    /// types in place of the names it takes, and each shape they give types to
    /// made in turn.
    fn instance_fields(&mut self, source: &ShapeDef, types: Vec<Type>) -> Result<Vec<Field>> {
        let given: HashMap<String, Type> = source.types.iter().map(|param| param.name.clone()).zip(types).collect();
        let mut fields = Vec::new();
        for field in &source.fields {
            let ty = field.ty.map_params(&mut |param| given.get(param).cloned());
            let ty = self.resolve_type(&ty, field.ty_span, TypePlace::Field)?;
            fields.push(Field { ty, ..field.clone() });
        }
        Ok(fields)
    }

    /// The type with each shape made for a set of types written as that shape
    /// given them (`Box__of__int` as `Box<int>`), so that a header naming
    /// `Box<T>` and a value of `Box<int>` can be matched up (see `unify_params`
    /// and `unify_regions`). It is the checker's own reading; the types in the
    /// program stay as the shapes made for them.
    fn unmade(&self, ty: &Type) -> Type {
        let boxed = |ty: &Type| Box::new(self.unmade(ty));
        match ty {
            Type::Named(name, region) => match self.made_of.get(name) {
                Some(made) => Type::Applied(Box::new(Applied {
                    name: made.name.clone(),
                    // One shape serves every region, so the types it was made
                    // for hold none (see `shape_instance`). Everything a value
                    // in a region holds is in that region, its types given
                    // included, so reading the shape back gives them the region
                    // of the value, as a field takes it (see `field_in`).
                    types: made
                        .types
                        .iter()
                        .map(|ty| {
                            let ty = self.unmade(ty);
                            match region {
                                Some(region) => self.in_region(&ty, region),
                                None => ty,
                            }
                        })
                        .collect(),
                    region: region.clone(),
                })),
                None => ty.clone(),
            },
            Type::Applied(applied) => Type::Applied(Box::new(Applied {
                name: applied.name.clone(),
                types: applied.types.iter().map(|ty| self.unmade(ty)).collect(),
                region: applied.region.clone(),
            })),
            Type::Ptr(inner, region) => Type::Ptr(boxed(inner), region.clone()),
            Type::Slice(inner, region) => Type::Slice(boxed(inner), region.clone()),
            Type::Array(inner, count) => Type::Array(boxed(inner), *count),
            Type::Range(inner) => Type::Range(boxed(inner)),
            Type::Maybe(inner) => Type::Maybe(boxed(inner)),
            Type::Fail(inner) => Type::Fail(boxed(inner)),
            Type::Or(value, error) => Type::Or(boxed(value), boxed(error)),
            Type::Tuple(items) => Type::Tuple(items.iter().map(|ty| self.unmade(ty)).collect()),
            Type::Results(types) => Type::Results(types.iter().map(|ty| self.unmade(ty)).collect()),
            other => other.clone(),
        }
    }

    /// Matches a type a declaration names against the type a value is, with
    /// both read the same way (see `unmade`), and records which region of the
    /// caller each region the declaration names stands for. A shape made for
    /// its types is one type with the shape given them, so a header naming
    /// `Box<text>@r` is matched by a value of the shape made for `Box<text>`.
    fn match_regions(&self, named: &Type, value: &Type, regions: &mut HashMap<String, String>) {
        unify_regions(&self.unmade(named), &self.unmade(value), regions);
    }

    /// Matches as `match_regions` does, and records which type each name the
    /// declaration takes stands for.
    fn match_params(&self, named: &Type, value: &Type, given: &mut HashMap<String, Type>) {
        unify_params(&self.unmade(named), &self.unmade(value), given);
    }

    /// Puts the shape made for its types in place of every application in `ty`
    /// whose types are all known, which is what a use of a declaration that
    /// takes types leaves behind (see `expand_instances`).
    fn settle_applications(&mut self, ty: &mut Type, at: Span) -> Result<()> {
        match ty {
            Type::Applied(applied) => {
                for ty in &mut applied.types {
                    self.settle_applications(ty, at)?;
                }
                if applied.types.iter().any(|ty| !ty.params().is_empty()) {
                    return Ok(());
                }
                let applied = applied.clone();
                let key = self.shape_instance(&applied.name, &applied.types, at)?;
                let region = applied.region.filter(|_| self.takes_region.contains(&key));
                *ty = Type::Named(key, region);
                Ok(())
            }
            Type::Ptr(inner, _) | Type::Slice(inner, _) | Type::Array(inner, _) | Type::Range(inner) | Type::Maybe(inner) | Type::Fail(inner) => {
                self.settle_applications(inner, at)
            }
            Type::Or(value, error) => {
                self.settle_applications(value, at)?;
                self.settle_applications(error, at)
            }
            Type::Tuple(types) | Type::Results(types) => types.iter_mut().try_for_each(|ty| self.settle_applications(ty, at)),
            _ => Ok(()),
        }
    }

    /// Refuses a shared field's type that holds what lives in a region, which
    /// no region outlasts: a pointer, a slice, or a value of a shape or kind
    /// that holds one; and a `File`, which a region closes (see
    /// `docs/future.html#static-region` for a region that would last).
    fn check_shared_type(&self, ty: &Type, span: Span, field: &str) -> Result<()> {
        if holds_pointers(ty, &self.regional) {
            return err(
                span,
                format!(
                    "shared field '{}' can't hold a pointer or a slice, or a value holding one: those live in a region, and a shared field lives as long as the program, which no region does",
                    field
                ),
            );
        }
        if self.holds_file(ty) {
            return err(span, format!("shared field '{}' can't hold a 'File', which the region it is opened in closes, and a shared field lives as long as the program", field));
        }
        Ok(())
    }

    /// Whether a value of type `ty` holds a `File` of `std/files`, directly or
    /// in a shape, kind, or tuple it holds by value.
    fn holds_file(&self, ty: &Type) -> bool {
        match ty {
            Type::Named(name, _) if name == FILE_SHAPE => true,
            Type::Named(name, _) => {
                let fields = self.shapes.get(name).map(|shape| shape.fields.iter().collect::<Vec<_>>()).unwrap_or_default();
                let payloads = self.kinds.get(name).map(|kind| kind.variants.iter().flat_map(|variant| &variant.payload).collect::<Vec<_>>()).unwrap_or_default();
                fields.into_iter().chain(payloads).any(|field| self.holds_file(&field.ty))
            }
            Type::Maybe(inner) | Type::Array(inner, _) => self.holds_file(inner),
            Type::Or(value, error) => self.holds_file(value) || self.holds_file(error),
            Type::Tuple(items) => items.iter().any(|item| self.holds_file(item)),
            _ => false,
        }
    }

    fn check_fields(&mut self, fields: &mut [Field]) -> Result<()> {
        let mut seen: HashMap<String, Span> = HashMap::new();
        for field in fields.iter_mut() {
            if let Some(at) = seen.get(&field.name) {
                return err(field.span, format!("field '{}' is already defined at {}:{}", field.name, at.line, at.col));
            }
            seen.insert(field.name.clone(), field.span);
            field.ty = self.resolve_type(&field.ty, field.ty_span, TypePlace::Field)?;
        }
        Ok(())
    }

    /// Checks a field's default, `size: int = 0`: a value the compiler works out
    /// without running the program, which fits the field's type. A copy is
    /// checked, so the one the shape keeps is the value as it is written, and
    /// every literal that leaves the field out gets a copy of that (see
    /// `check_struct_init`). A mistake here makes the shape one whose fields
    /// aren't settled, so no literal of it says the same thing again (see
    /// `broken`).
    fn check_field_default(&mut self, value: &mut Expr, ty: &Type, field: &str) -> Result<()> {
        let at = value.span;
        // `none` is the one value that fits a type the shape takes, in a maybe,
        // whatever type that is.
        let fits_any = matches!(ty, Type::Maybe(_)) && matches!(value.kind, ExprKind::NoneLit);
        if !ty.params().is_empty() && !fits_any {
            return err(
                at,
                format!("field '{}' is of a type the shape takes, which is a different type in every value, so it has no value to fall back on", field),
            );
        }
        let outer = self.in_default.replace(field.to_string());
        let checked = self.check_expr(value);
        self.in_default = outer;
        checked?;
        if !constant_value(value) {
            return err(
                at,
                format!(
                    "the value field '{}' falls back on must be one the compiler works out without running the program: a number, text, 'true', 'false', 'none', a variant, or a tuple, an array, or a literal of those",
                    field
                ),
            );
        }
        self.coerce(value, ty)?;
        Ok(())
    }

    /// Drops the region named on the value a method is called on where its shape
    /// or kind lives in none: the parser writes `@this` on every receiver, since
    /// only the whole file says which types hold something in a region. A shape
    /// given types keeps it, and the shape made for each set of types settles it
    /// (see `settle_applications`).
    fn settle_receiver(&self, task: &mut TaskDef) {
        if task.owner.is_none() {
            return;
        }
        let Some(Type::Named(name, region)) = task.params.first_mut().map(|param| &mut param.ty) else { return };
        if !self.takes_region.contains(name.as_str()) {
            *region = None;
        }
    }

    /// Checks a task's parameters and result types, and settles their regions.
    /// A region the header names without a `region` parameter is one the task
    /// only reads or links pointers in (it can't allocate there), and each
    /// result's regions must be regions the task is given.
    fn check_header(&mut self, t: &mut TaskDef) -> Result<()> {
        self.enter_type_params(&t.types);
        // `r: region default` makes the region the task is given its body's
        // default (see `ast::DEFAULT`): only a region can be one, and a task
        // takes one at most, as a block declares one at most.
        let mut marked: Option<&Field> = None;
        for param in t.params.iter().filter(|param| param.default_region) {
            if param.ty != Type::Region {
                return err(
                    param.span,
                    format!("only a region can be a task's default region, as in 'r: region {}'; '{}' is a parameter of type {}", DEFAULT, param.name, param.ty),
                );
            }
            if let Some(first) = marked {
                return err(
                    param.span,
                    format!("this task already has a default region, '{}', at {}:{}", first.name, first.span.line, first.span.col),
                );
            }
            marked = Some(param);
        }
        if t.name == "main" && !t.types.is_empty() {
            return err(t.span, "'main' is where the program starts, so it takes no types");
        }
        let region_params: Vec<&str> = t.params.iter().filter(|p| p.ty == Type::Region).map(|p| p.name.as_str()).collect();
        let mut named: Vec<String> = region_params.iter().map(|name| name.to_string()).collect();
        let mut resolved = Vec::new();
        for param in &t.params {
            if param.ty == Type::Region {
                resolved.push(Type::Region);
                continue;
            }
            let ty = self.resolve_type(&param.ty, param.ty_span, TypePlace::Header)?;
            for region in ty.regions() {
                if let Some(other) = t.params.iter().find(|p| p.name == region && p.ty != Type::Region) {
                    return err(self.place(param.ty_span, &format!("@{}", region)), format!("'{}' is a parameter, not a region", other.name));
                }
                if !named.iter().any(|n| n == region) {
                    named.push(region.to_string());
                }
            }
            resolved.push(ty);
        }
        let several = matches!(t.ret, Type::Results(_));
        let written: Vec<(Type, Span)> = t.ret.results().iter().cloned().zip((0..).map(|i| t.result_span(i))).collect();
        let mut results = Vec::new();
        for (i, (result, span)) in written.into_iter().enumerate() {
            if result == Type::Region {
                return err(span, "a task can't send a region; regions are only parameters, as in 'task build(r: region)'");
            }
            if result == Type::Unit {
                return err(span, "unit is only allowed as a task's whole return type, so it can't be one of several results");
            }
            let ret = self.resolve_type(&result, span, TypePlace::Header)?;
            if let Some(region) = ret.regions().into_iter().find(|r| !named.iter().any(|n| n == r)) {
                let at = self.place(span, &format!("@{}", region));
                if t.params.iter().any(|p| p.name == region) {
                    return err(at, format!("'{}' is a parameter, not a region", region));
                }
                // A named result is easier to point at by its name than by its
                // place in the list.
                let which = match t.results.get(i).and_then(|r| r.name.as_ref()) {
                    Some((name, _)) => format!("result '{}'", name),
                    None if several => format!("result {}", i + 1),
                    None => "the result".to_string(),
                };
                return err(
                    at,
                    format!(
                        "{1} lives in region '{0}', which the task isn't given; take it as a parameter, as in '{0}: region'",
                        region, which
                    ),
                );
            }
            results.push(ret);
        }
        t.ret = match results.len() {
            0 => Type::Unit,
            1 => results.into_iter().next().unwrap_or(Type::Unit),
            _ => Type::Results(results),
        };
        // A `unit ~ E` task succeeds by reaching its end or a bare `<-`, so a
        // name for its result would have nothing to hold.
        if !sends_value(&t.ret)
            && let Some((name, at)) = t.result_names().next()
        {
            return err(*at, format!("a '{}' task succeeds by reaching its end or a bare '<-', so '{}' has no value to name", t.ret, name));
        }
        for (param, ty) in t.params.iter_mut().zip(resolved) {
            param.ty = ty;
        }
        if t.owner.as_ref().is_some_and(|owner| owner.name == STR) {
            check_str_method(t)?;
        }
        let params = t.params.iter().map(|p| (p.name.clone(), p.ty.clone())).collect();
        self.tasks.insert(t.name.clone(), TaskInfo { types: t.types.clone(), params, passing: t.params.iter().map(|p| p.passing).collect(), ret: t.ret.clone(), owner: t.owner.clone() });
        self.type_params.clear();
        Ok(())
    }

    /// Makes the types a declaration takes visible while its header and body are
    /// checked: `T` in them means the type each use gives. A name that is
    /// already a type is reported when the file's names are resolved (see
    /// `Resolver::enter_type_params`).
    fn enter_type_params(&mut self, types: &[TypeParam]) {
        self.type_params = types.to_vec();
    }

    /// What the type parameter called `name` requires, if it is one here.
    fn type_param(&self, name: &str) -> Option<&TypeParam> {
        self.type_params.iter().find(|param| param.name == name)
    }

    /// Checks a written type and settles its regions for `place`:
    /// - in a field or after `new`, no type names a region;
    /// - elsewhere, every pointer and every regional shape or kind names one,
    ///   which inside a task must be a region visible there;
    /// - a shape or kind without pointers lives in no region, so `Point@r` is
    ///   an error, while the `Point` in `^Point@r` just has its region dropped.
    fn resolve_type(&mut self, ty: &Type, span: Span, place: TypePlace) -> Result<Type> {
        self.check_type(ty, span)?;
        // The sizes of shapes and kinds are known inside tasks; fields and
        // headers are checked for size once they are (see `check_task`).
        if matches!(place, TypePlace::Body | TypePlace::New) {
            self.check_size(ty, span)?;
        }
        self.resolve_regions(ty, span, place, None)
    }

    /// `resolve_type` for `ty` inside a pointer of region `inside`.
    fn resolve_regions(&mut self, ty: &Type, span: Span, place: TypePlace, inside: Option<&str>) -> Result<Type> {
        let given = |region: &Option<String>| -> Result<()> {
            match (place, region) {
                (TypePlace::Field, Some(region)) => err(
                    self.place(span, &format!("@{}", region)),
                    format!("a field lives in the region of the value holding it, so its type names no region; remove '@{}'", region),
                ),
                (TypePlace::Shared, Some(region)) => err(
                    self.place(span, &format!("@{}", region)),
                    format!("a shared field lives as long as the program, which no region does, so its type names none; remove '@{}'", region),
                ),
                (TypePlace::New, Some(region)) => err(
                    self.place(span, &format!("@{}", region)),
                    format!("the value 'new' makes lives in the region it allocates in, so its type names no region; remove '@{}'", region),
                ),
                (TypePlace::Body, Some(region)) => self.visible_region(region, self.place(span, &format!("@{}", region)), false),
                _ => Ok(()),
            }
        };
        let needs_region = matches!(place, TypePlace::Header | TypePlace::Body);
        match ty {
            // `Box<int>`: the shape made for those types (see `resolve_applied`).
            Type::Applied(applied) => {
                let applied = applied.clone();
                self.resolve_applied(&applied, span, place, inside)
            }
            Type::Ptr(inner, region) => {
                given(region)?;
                if needs_region && region.is_none() {
                    return err(span, format!("a pointer lives in a region, so its type names one, as in '{}@r'", ty));
                }
                if let (Some(region), Some(inside)) = (region, inside)
                    && region != inside
                {
                    return err(
                        self.place(span, &format!("@{}", region)),
                        format!("everything a value in region '{0}' holds is in '{0}', its pointers too; write '@{0}' for '@{1}'", inside, region),
                    );
                }
                let inner = self.resolve_regions(inner, span, place, region.as_deref())?;
                Ok(Type::Ptr(Box::new(inner), region.clone()))
            }
            Type::Slice(inner, region) => {
                given(region)?;
                if needs_region && region.is_none() {
                    return err(span, format!("a slice lives in a region, so its type names one, as in '{}@r'", ty));
                }
                if let (Some(region), Some(inside)) = (region, inside)
                    && region != inside
                {
                    return err(
                        self.place(span, &format!("@{}", region)),
                        format!("everything a value in region '{0}' holds is in '{0}', its slices too; write '@{0}' for '@{1}'", inside, region),
                    );
                }
                let inner = self.resolve_regions(inner, span, place, region.as_deref())?;
                Ok(Type::Slice(Box::new(inner), region.clone()))
            }
            Type::Array(inner, count) => Ok(Type::Array(Box::new(self.resolve_regions(inner, span, place, inside)?), *count)),
            // Text inside a value of a region is in that region.
            Type::Text(region) => {
                given(region)?;
                if let (Some(region), Some(inside)) = (region, inside)
                    && region != inside
                {
                    return err(
                        self.place(span, &format!("@{}", region)),
                        format!("everything a value in region '{0}' holds is in '{0}', its text too; remove '@{1}'", inside, region),
                    );
                }
                Ok(Type::Text(region.clone().or_else(|| inside.map(str::to_string))))
            }
            // A type parameter's regions are the given type's, so it names none
            // of its own.
            Type::Named(name, region) if self.type_param(name).is_some() => {
                // A slice or pointer around it gives it their region (`[T]@r`),
                // which a parameter simply takes; a region written on the
                // parameter itself says nothing, since what a `T` holds is in
                // the region of the type given for it.
                if let Some(region) = region.as_deref().filter(|region| inside != Some(region)) {
                    return err(
                        self.place(span, &format!("@{}", region)),
                        format!("'{0}' is a type this task takes, and what a '{0}' holds is in the region of the type given for it, so it names none; remove '@{1}'", name, region),
                    );
                }
                Ok(Type::Param(name.clone()))
            }
            Type::Named(name, region) => {
                if let Some(shape) = self.shapes.get(name).filter(|shape| !shape.types.is_empty()) {
                    let names: Vec<&str> = shape.types.iter().map(|param| param.name.as_str()).collect();
                    return err(
                        self.place(span, name),
                        format!(
                            "shape '{}' takes {}, so name {}, as in '{}<{}>'",
                            unqualified(name),
                            count(shape.types.len(), "type"),
                            if shape.types.len() == 1 { "it" } else { "all of them" },
                            unqualified(name),
                            names.join(", ")
                        ),
                    );
                }
                given(region)?;
                if !self.regional.contains(name) {
                    // A shape or kind holding text may name the region its text
                    // is in, and is in the region of a value holding it.
                    if self.takes_region.contains(name) {
                        if let (Some(region), Some(inside)) = (region, inside)
                            && region != inside
                        {
                            return err(
                                self.place(span, &format!("@{}", region)),
                                format!("everything a value in region '{0}' holds is in '{0}', '{2}' too; remove '@{1}'", inside, region, name),
                            );
                        }
                        return Ok(Type::Named(name.clone(), region.clone().or_else(|| inside.map(str::to_string))));
                    }
                    if let Some(region) = region
                        && inside != Some(region.as_str())
                    {
                        return err(
                            self.place(span, &format!("@{}", region)),
                            format!("'{}' holds no pointers or text, so it lives in no region; remove '@{}'", name, region),
                        );
                    }
                    return Ok(Type::named(name));
                }
                if needs_region && region.is_none() {
                    return err(self.place(span, name), format!("'{0}' holds pointers, so it lives in a region; name it, as in '{0}@r'", name));
                }
                Ok(ty.clone())
            }
            Type::Maybe(inner) => Ok(Type::Maybe(Box::new(self.resolve_regions(inner, span, place, inside)?))),
            Type::Tuple(items) => Ok(Type::Tuple(items.iter().map(|item| self.resolve_regions(item, span, place, inside)).collect::<Result<_>>()?)),
            Type::Or(value, error) => Ok(Type::Or(
                Box::new(self.resolve_regions(value, span, place, inside)?),
                Box::new(self.resolve_regions(error, span, place, inside)?),
            )),
            other => Ok(other.clone()),
        }
    }

    /// Checks that `name` is a region visible here; `allocating` also requires
    /// that `new` can allocate in it.
    fn visible_region(&self, name: &str, span: Span, allocating: bool) -> Result<()> {
        match self.lookup(name) {
            Some(info) if info.ty == Type::Region => {
                if allocating && !info.allocates {
                    if name == THIS_WORD && self.current_method.is_some() {
                        return err(span, self.cannot_allocate_in_this("allocate where the value is kept, and give the method what you made"));
                    }
                    return err(
                        span,
                        format!("this task can't allocate in '{0}', which it only names in its types; take it as a parameter, '{0}: region'", name),
                    );
                }
                Ok(())
            }
            Some(_) => err(span, format!("'{}' is a variable, not a region", name)),
            None => err(span, format!("unknown region '{0}'; declare it with 'region {0}'", name)),
        }
    }

    /// `ty`, whose regions are unsettled (a field's type, or the type after
    /// `new`), as it is in a value of region `region`: its pointers and
    /// regional shapes and kinds get `region`.
    fn in_region(&self, ty: &Type, region: &str) -> Type {
        in_region(ty, region, &self.takes_region)
    }

    /// A type written in the source must be a builtin type, a shape, or a kind.
    /// `unit` is only valid as a return type, which is checked separately, and
    /// as the value side of `unit ~ E`.
    fn check_type(&self, ty: &Type, span: Span) -> Result<()> {
        match ty {
            Type::Unit => err(self.place(span, "unit"), "unit is only allowed as a return type"),
            Type::Region => err(self.place(span, "region"), "'region' is only the type of a task's parameter, as in 'task build(r: region)'"),
            // Only a task's header lists several types, and `check_header`
            // checks each of them on its own.
            Type::Results(_) => err(span, "several types, separated by commas, are only a task's results"),
            Type::Ptr(inner, _) => self.check_type(inner, span),
            Type::Array(inner, _) | Type::Slice(inner, _) => match inner.as_ref() {
                Type::Unit => {
                    let what = if matches!(ty, Type::Array(..)) { "an array" } else { "a slice" };
                    err(self.place(span, "unit"), format!("{} can't hold unit, which has no value", what))
                }
                _ => self.check_type(inner, span),
            },
            Type::Maybe(inner) => match inner.as_ref() {
                Type::Maybe(_) => err(span, "a maybe can't hold another maybe"),
                Type::Unit => err(self.place(span, "unit"), "a maybe can't hold unit, which has no value"),
                _ => self.check_type(inner, span),
            },
            Type::Or(value, error) => {
                // One error side, whichever side of the `~` the other is written
                // on: `(int ~ text) ~ text` and `int ~ (text ~ int)` name two, as
                // `maybe maybe int` names two emptinesses.
                if matches!(value.as_ref(), Type::Or(..)) || matches!(error.as_ref(), Type::Or(..)) {
                    return err(self.place(span, "~"), "a type has one error type: '~' can't appear twice");
                }
                if **value != Type::Unit {
                    self.check_type(value, span)?;
                }
                match error.as_ref() {
                    Type::Unit => return err(self.place(span, "~"), "the error type can't be unit"),
                    Type::Maybe(_) => return err(self.place(span, "~"), "the error type can't be a maybe"),
                    _ => self.check_type(error, span)?,
                }
                // Text in two regions is still text on both sides, and a value
                // of no region in particular (a literal) would fit both.
                if value == error || (value.without_regions() == error.without_regions() && self.fits_any_region(value)) {
                    return err(self.place(span, "~"), format!("the error type must differ from the value type, as in '{} ~ ParseError'", value));
                }
                Ok(())
            }
            Type::Tuple(items) => {
                for item in items {
                    self.check_type(item, span)?;
                    if let Some(what) = self.changeable_item(item) {
                        return err(span, format!("a tuple's items never change, so it can't hold {}", what));
                    }
                }
                Ok(())
            }
            // `T`: the type each use of a generic declaration gives it.
            Type::Named(name, _) if self.type_param(name).is_some() => Ok(()),
            // `Box<int>`: what it is given is checked with it (see
            // `resolve_applied`), which also reports a name that is no shape.
            Type::Applied(applied) => applied.types.iter().try_for_each(|ty| self.check_type(ty, span)),
            Type::Named(name, _) if !self.sees(name) || (!self.shapes.contains_key(name) && !self.kinds.contains_key(name)) => {
                match self.variants.get(name).filter(|_| self.sees(name)) {
                    Some(info) => err(
                        self.place(span, name),
                        format!("'{}' is a variant, not a type; its type is its kind, '{}'", info.variant.tag, info.kind),
                    ),
                    None => err(self.place(span, name), format!("unknown type '{}'{}", name, self.elsewhere(name))),
                }
            }
            _ => Ok(()),
        }
    }

    /// What a tuple's item of type `ty` holds whose value can change, for a
    /// message: a pointer, a slice, an array, or a shape or kind holding one.
    /// `None` when nothing in it can change.
    fn changeable_item(&self, ty: &Type) -> Option<String> {
        match ty {
            // A shape given types: what its fields hold, with those types in
            // place of the names it takes.
            Type::Applied(applied) => {
                let shape = self.shapes.get(&applied.name)?;
                let given: HashMap<&str, &Type> = shape.types.iter().map(|param| param.name.as_str()).zip(&applied.types).collect();
                let fields: Vec<Type> = shape.fields.iter().map(|field| field.ty.map_params(&mut |name| given.get(name).map(|ty| (*ty).clone()))).collect();
                let found = fields.iter().find_map(|ty| self.changeable_item(ty))?;
                Some(format!("'{}', which holds {}", ty, found))
            }
            // Only a type required to be `fixed` is known never to change.
            Type::Param(name) if !self.allows(Need::Fixed, ty) => {
                Some(format!("'{0}', which may be any type here; require it, as in '<{0}: {1}>'", name, Need::Fixed.word()))
            }
            Type::Ptr(..) => Some(format!("a pointer ({}), whose value can change", ty)),
            Type::Slice(..) => Some(format!("a slice ({}), whose items can change", ty)),
            Type::Array(..) => Some(format!("an array ({})", ty)),
            Type::Unit => Some("unit, which has no value".to_string()),
            Type::Named(name, _) if self.changeable.contains(name) => {
                Some(format!("'{}', which holds a pointer, a slice, or an array", unqualified(name)))
            }
            Type::Maybe(inner) => self.changeable_item(inner),
            Type::Or(value, error) => self.changeable_item(value).filter(|_| **value != Type::Unit).or_else(|| self.changeable_item(error)),
            Type::Tuple(items) => items.iter().find_map(|item| self.changeable_item(item)),
            _ => None,
        }
    }

    /// Whether a value of type `ty` can be of no region in particular, as a
    /// literal is, and so fit the same type in any region: text, and a shape or
    /// kind that can be given a region, held by value.
    fn fits_any_region(&self, ty: &Type) -> bool {
        match ty {
            Type::Text(_) => true,
            Type::Named(name, _) => self.takes_region.contains(name),
            Type::Maybe(inner) | Type::Array(inner, _) => self.fits_any_region(inner),
            Type::Or(value, error) => self.fits_any_region(value) || self.fits_any_region(error),
            Type::Tuple(items) => items.iter().any(|item| self.fits_any_region(item)),
            _ => false,
        }
    }

    /// Rejects a shape or kind that contains itself by value, directly or
    /// through other types: its values would have infinite size. Containing a
    /// pointer to itself (`maybe ^T`) is fine. `acyclic` holds the types
    /// already known to be free of cycles. The search keeps its own stack, so
    /// a long chain of types can't overflow the compiler's.
    ///
    /// Each type found free of cycles is added to `order` after the types it
    /// contains by value.
    fn check_value_cycle(&self, root: &str, acyclic: &mut HashSet<String>, order: &mut Vec<String>) -> std::result::Result<(), (Error, Vec<String>)> {
        if acyclic.contains(root) {
            return Ok(());
        }
        // Each type being entered, with the types its fields contain by value
        // and how many of those have been followed.
        type Entered<'a> = (&'a str, Vec<(&'a str, Span)>, usize);
        let mut path: Vec<Entered> = vec![(root, self.contained_types(root), 0)];
        let mut on_path: HashSet<&str> = HashSet::from([root]);
        while let Some((_, targets, next)) = path.last_mut() {
            let Some(&(target, span)) = targets.get(*next) else {
                if let Some((name, _, _)) = path.pop() {
                    on_path.remove(name);
                    acyclic.insert(name.to_string());
                    order.push(name.to_string());
                }
                continue;
            };
            *next += 1;
            if on_path.contains(target) {
                let start = path.iter().position(|(name, _, _)| *name == target).unwrap_or_default();
                let chain: Vec<&str> = path.iter().skip(start).map(|(name, _, _)| *name).chain([target]).collect();
                // A shape that takes types keeps them in the pointer that
                // breaks the cycle: `maybe ^Node<T>`.
                let takes = self.shapes.get(target).map_or(String::new(), |shape| match shape.types.is_empty() {
                    true => String::new(),
                    false => format!("<{}>", shape.types.iter().map(|param| param.name.clone()).collect::<Vec<_>>().join(", ")),
                });
                let error = Error::at(
                    span,
                    format!("'{}' contains itself by value ({}); use a pointer such as 'maybe ^{}{}'", target, chain.join(" -> "), target, takes),
                );
                // Every type in the cycle contains itself through the others, so
                // the cycle is one mistake: it is reported at the field that
                // closes it, and the types in it are not walked again.
                return Err((error, chain.into_iter().map(str::to_string).collect()));
            }
            if !acyclic.contains(target) {
                on_path.insert(target);
                path.push((target, self.contained_types(target), 0));
            }
        }
        Ok(())
    }

    /// The types a shape's fields or a kind's payloads contain by value, each
    /// with the position of the field's type.
    fn contained_types(&self, name: &str) -> Vec<(&str, Span)> {
        let fields: Vec<&Field> = match (self.shapes.get(name), self.kinds.get(name)) {
            (Some(shape), _) => shape.fields.iter().collect(),
            (None, Some(kind)) => kind.variants.iter().flat_map(|v| &v.payload).collect(),
            (None, None) => Vec::new(),
        };
        fields.into_iter().flat_map(|field| contained_by_value(&field.ty).into_iter().map(|target| (target, field.ty_span))).collect()
    }

    /// Rejects a shape or kind that could never be created. A pointer can't be
    /// `none`, so a shape whose field points to its own type (directly or
    /// through other types) would need one to exist before the first is made.
    /// A kind can be created if any of its variants can.
    fn check_constructible(&mut self, items: &[Item]) {
        // Each type that could be created, with the types each way of making it
        // holds: a shape's fields, or each variant's values. A shape made for a
        // set of types (`Out<int>`) is one of them, with the fields it was made
        // with, since what it holds says whether it can be (see `shape_instance`).
        let mut types: Vec<(&str, Vec<Vec<Type>>)> = Vec::new();
        for item in items {
            match item {
                Item::Shape(s) => types.push((s.name.as_str(), vec![s.fields.iter().map(|f| f.ty.clone()).collect()])),
                Item::Kind(k) => types.push((k.name.as_str(), k.variants.iter().map(|v| v.payload.iter().map(|f| f.ty.clone()).collect()).collect())),
                Item::Task(_) => {}
            }
        }
        for made in self.made_of.keys() {
            if let Some(shape) = self.shapes.get(made) {
                types.push((made.as_str(), vec![shape.fields.iter().map(|f| f.ty.clone()).collect()]));
            }
        }
        // The types whose fields name each type: when a type turns out to be
        // creatable, only those need another look.
        let mut users: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, (_, ways)) in types.iter().enumerate() {
            for ty in ways.iter().flatten() {
                for name in named_types(ty) {
                    users.entry(name).or_default().push(i);
                }
            }
        }
        // The kinds no file declares hold a variant that holds nothing, so they
        // can always be created; no item declares them, so they are known before
        // the first look rather than found by it (see `ast::builtin_kinds`).
        let mut can: HashSet<&str> = BUILTIN_KIND_NAMES.iter().copied().filter(|name| builtin_kind_of(name).is_none()).collect();
        let mut pending: Vec<usize> = (0..types.len()).collect();
        while let Some(i) = pending.pop() {
            let Some((name, ways)) = types.get(i) else { continue };
            if ways.iter().any(|holds| holds.iter().all(|ty| constructible(ty, &can))) && can.insert(name) {
                pending.extend(users.get(name).into_iter().flatten().copied());
            }
        }
        // Each type that can never be created is reported, since one that holds
        // another says nothing about the one it holds.
        let mut found = Vec::new();
        for item in items {
            // A type that already has a mistake of its own says nothing more:
            // one that contains itself by value, say, can never be created
            // either, which is the same mistake told twice (see `broken`).
            if self.broken.contains(item.name()) {
                continue;
            }
            match item {
                Item::Shape(s) if !can.contains(s.name.as_str()) => {
                    if let Some(field) = s.fields.iter().find(|f| !constructible(&f.ty, &can)) {
                        found.push((
                            s.name.clone(),
                            Error::at(
                                field.ty_span,
                                format!(
                                    "'{}' can never be created: field '{}' needs an existing {}; make it 'maybe {}'",
                                    s.name, field.name, field.ty, field.ty
                                ),
                            ),
                        ));
                    }
                }
                Item::Kind(k) if !can.contains(k.name.as_str()) => {
                    found.push((
                        k.name.clone(),
                        Error::at(
                            k.span,
                            format!("'{}' can never be created: every variant needs an existing value that can't be created either; use 'maybe' for a value that can be missing", k.name),
                        ),
                    ));
                }
                _ => {}
            }
        }
        for (name, error) in found {
            self.record(error);
            self.broken.insert(name);
        }
    }

    fn check_task(&mut self, task: &mut TaskDef) -> Result<()> {
        self.enter_type_params(&task.types);
        // A header with a mistake never settled its types, so the body is
        // checked with nothing known about what the task takes or sends: its own
        // mistakes are still reported, and the header's isn't reported again at
        // every line that uses a parameter (see `broken`).
        let unsettled = self.broken.contains(&task.name);
        let known = |ty: &Type| if unsettled { Type::Unknown } else { ty.clone() };
        for param in &task.params {
            self.check_size(&param.ty, param.ty_span)?;
        }
        for (i, result) in task.ret.results().iter().enumerate() {
            self.check_size(result, task.result_span(i))?;
        }
        self.current_ret = Some(known(&task.ret));
        self.current_method = task.owner.clone();
        self.current_owner = task.owner.as_ref().filter(|owner| owner.alters).and(task.params.first()).and_then(|this| self.owner_of(&this.ty));
        self.current_shape = task.params.first().filter(|_| task.owner.is_some()).and_then(|this| self.owner_of(&this.ty));
        // Parameters and the task body share one scope, as in C.
        self.scopes = vec![Scope::default()];
        for param in &task.params {
            if param.ty == Type::Region && param.passing != Passing::Copy {
                self.record(Error::at(param.span, format!("a region never changes, so '{}' can't be '{}'", param.name, if param.passing == Passing::HoldRef { "hold&" } else { "hold" })));
            }
            if param.ty == Type::Region {
                self.define_region(&param.name, param.span, true)?;
            } else {
                // Only the value a method marked `alters` is called on changes,
                // and a parameter the task holds a copy of (`hold x: T`) or the
                // caller's place (`hold& x: T`).
                let alters = param.name == THIS && task.owner.as_ref().is_some_and(|owner| owner.alters);
                self.define_param(&param.name, known(&param.ty), alters || param.passing != Passing::Copy, param.span)?;
            }
        }
        // The regions the header only names in its types.
        for param in &task.params {
            for region in param.ty.regions() {
                if self.lookup(region).is_none() {
                    self.define_region(region, param.ty_span, false)?;
                }
            }
        }
        // A region the header marks is the default of the body, which shares the
        // parameters' scope, so a second default in the body is refused as a
        // second in any block is (see `set_default_region`).
        if let Some((name, at)) = task.params.iter().find(|param| param.default_region).map(|param| (param.name.clone(), param.span)) {
            self.set_default_region(&name, at)?;
        }
        // Named results are variables of the task, declared by its header.
        self.result_names = task.result_names().map(|(name, _)| name.clone()).collect();
        self.unset = self.result_names.iter().cloned().collect();
        for (result, ty) in task.results.iter().zip(task.ret.results()) {
            if let Some((name, at)) = &result.name {
                self.define_result(name, known(ty), *at)?;
            }
        }
        // A native task's body is the runtime's (see `ast::NATIVE`), so its
        // header is all there is to check.
        if task.native {
            self.scopes.clear();
            self.current_ret = None;
            self.current_method = None;
            self.current_owner = None;
            self.current_shape = None;
            self.result_names.clear();
            self.unset.clear();
            return Ok(());
        }
        let before = self.errors.len();
        self.check_block(&mut task.body)?;
        // Whether every path sends a value can only be told of a body that
        // checked: a line with a mistake may well be the one that sends, so a
        // task with a mistake anywhere is not told this as well (see `record`).
        let whole = self.errors.len() == before && !unsettled;
        // The copies the body makes of the values of the types the task takes
        // (substituting no types changes nothing and only reads the body). A
        // copy into a region the header keeps that type in is the value's own
        // region, since a use must give a type of that region (see
        // `region_conflict` at a call); any other is only the value itself, and
        // every use must fit that (see `check_copies_of`).
        // Only a body that checked, since reading it again relies on every call
        // and method call having been settled (see `substitute_expr`).
        if !task.types.is_empty() && whole {
            let mut made = Made::default();
            self.substitute_task(task, &HashMap::new(), &mut made)?;
            let kept = keeps_params(task);
            let named = header_regions(task);
            let loose: Vec<(String, String, bool, Span)> = made
                .copies
                .into_iter()
                .filter(|(param, region, _)| !kept.get(param).is_some_and(|regions| regions.contains(region)))
                .map(|(param, region, at)| {
                    let in_header = named.contains(&region);
                    (param, region, in_header, at)
                })
                .collect();
            if !loose.is_empty() {
                self.loose_copies.insert(task.name.clone(), loose);
            }
        }
        self.scopes.clear();
        self.current_ret = None;
        self.current_method = None;
        self.current_owner = None;
        self.current_shape = None;
        let ends_well = if self.result_names.is_empty() {
            if whole && sends_value(&task.ret) && !always_sends(&task.body) {
                err(task.span, format!("task '{}' can finish without sending a value", task.name))
            } else {
                Ok(())
            }
        } else if whole
            && !always_sends(&task.body)
            && let Some(name) = self.first_unset()
        {
            // Reaching the end sends the results, so they must all be set.
            err(task.span, format!("task '{}' can finish without setting '{}'", task.name, name))
        } else {
            Ok(())
        };
        self.result_names.clear();
        self.unset.clear();
        ends_well
    }

    /// Checks `block` as a path of its own, and returns the results it leaves
    /// unset, or `None` when every path through it leaves the task, so nothing
    /// after it runs on this path. `entry` is the state the block starts from.
    fn branch_state(&mut self, block: &Block, entry: &HashSet<String>) -> Option<HashSet<String>> {
        let after = std::mem::replace(&mut self.unset, entry.clone());
        (!always_sends(block)).then_some(after)
    }

    /// The state after branches that each start from `entry`: a result is set
    /// only where every branch that runs on sets it. Where no branch runs on,
    /// what follows is unreachable, and `entry` is kept for checking it.
    fn join_branches(&mut self, entry: HashSet<String>, branches: Vec<Option<HashSet<String>>>) {
        let mut after: Option<HashSet<String>> = None;
        for branch in branches.into_iter().flatten() {
            after.get_or_insert_with(HashSet::new).extend(branch);
        }
        self.unset = after.unwrap_or(entry);
    }

    fn push_scope(&mut self) {
        self.scopes.push(Scope::default());
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
    fn check_block_with(&mut self, bindings: Vec<Declared>, block: &mut Block) -> Result<()> {
        self.push_scope();
        let result = bindings
            .into_iter()
            .filter(|(name, _, _, _)| name != DISCARD)
            .try_for_each(|(name, span, ty, mutable)| self.define(&name, ty, mutable, span))
            .and_then(|()| self.check_block(block));
        self.pop_scope();
        result
    }

    /// Declares a variable in the innermost scope. Vela has no shadowing: a
    /// name cannot be declared while another variable of that name is visible.
    /// (Sibling blocks may reuse names, since neither is visible to the other.)
    fn define(&mut self, name: &str, ty: Type, mutable: bool, span: Span) -> Result<()> {
        // A variable named like a variant would make a bare `Empty` ambiguous.
        if let Some(info) = self.variant_in_scope(name) {
            let at = info.variant.span;
            if at.file != span.file {
                return err(span, format!("'{}' is already the name of a variant of kind '{}'", name, info.kind));
            }
            return err(span, format!("'{}' is already defined at {}:{}", name, at.line, at.col));
        }
        // Nor like a kind, which `Kind.Variant` would read as.
        if self.kinds.contains_key(&self.in_module(name)) {
            return err(span, format!("'{0}' is the name of a kind, so '{0}.x' would name its variant; choose another name", name));
        }
        // Nor like a shape, which `Shape.f` would read as, for its shared field.
        if self.shapes.contains_key(&self.in_module(name)) {
            return err(span, format!("'{0}' is the name of a shape, so '{0}.x' would name its shared field; choose another name", name));
        }
        // Nor may it take a module's name, which `name.x` would read as.
        if let Some(at) = self.module_used(name) {
            return err(span, format!("'{}' is the name of a module this file includes, at {}:{}", name, at.line, at.col));
        }
        if let Some(existing) = self.lookup(name) {
            let at = existing.span;
            let what = match existing {
                _ if existing.ty == Type::Region => "the name of a region",
                _ if existing.result => "a result of this task, declared",
                _ => "defined",
            };
            return err(span, format!("'{}' is already {} at {}:{}", name, what, at.line, at.col));
        }
        let Some(scope) = self.scopes.last_mut() else {
            return Err(Error::internal(format!("'{}' defined outside of a task", name)));
        };
        let allocates = ty == Type::Region;
        scope.names.insert(name.to_string(), VarInfo { ty, mutable, span, allocates, result: false, param: false, refers: None });
        Ok(())
    }

    /// Declares a parameter, which the task's header gives it. Only the value a
    /// method marked `alters` is called on can change.
    fn define_param(&mut self, name: &str, ty: Type, mutable: bool, span: Span) -> Result<()> {
        self.define(name, ty, mutable, span)?;
        if let Some(info) = self.scopes.last_mut().and_then(|scope| scope.names.get_mut(name)) {
            info.param = true;
        }
        Ok(())
    }

    /// Declares a named result, which reads like a variable and is set only by
    /// `total <- value`.
    fn define_result(&mut self, name: &str, ty: Type, span: Span) -> Result<()> {
        self.define(name, ty, false, span)?;
        if let Some(info) = self.scopes.last_mut().and_then(|scope| scope.names.get_mut(name)) {
            info.result = true;
        }
        Ok(())
    }

    /// The first result, in the order the header names them, that isn't set on
    /// this path.
    fn first_unset(&self) -> Option<&str> {
        self.result_names.iter().find(|name| self.unset.contains(*name)).map(String::as_str)
    }

    /// Declares a region in the innermost scope; `allocates` tells whether
    /// `new` can allocate in it.
    fn define_region(&mut self, name: &str, span: Span, allocates: bool) -> Result<()> {
        self.define(name, Type::Region, false, span)?;
        if let Some(info) = self.scopes.last_mut().and_then(|scope| scope.names.get_mut(name)) {
            info.allocates = allocates;
        }
        Ok(())
    }

    fn lookup(&self, name: &str) -> Option<&VarInfo> {
        for scope in self.scopes.iter().rev() {
            if let Some(info) = scope.names.get(name) {
                return Some(info);
            }
        }
        None
    }

    /// The region an allocation that names none goes in: the default region of
    /// the innermost block that has one, which a block sets with
    /// `region r default` or a task with an `r: region default` parameter.
    /// `None` where no block in scope has named one, which is the error each
    /// allocating form reports in its own words.
    fn default_region(&self) -> Option<String> {
        self.scopes.iter().rev().find_map(|scope| scope.default.as_ref()).map(|(name, _)| name.clone())
    }

    /// Makes `name` the default region of the innermost block. A block names one
    /// default at most: a second is a mistake, since two lines that look alike
    /// would allocate in regions of different lifetimes.
    fn set_default_region(&mut self, name: &str, span: Span) -> Result<()> {
        if let Some((already, at)) = self.scopes.last().and_then(|scope| scope.default.clone()) {
            return err(
                span,
                format!(
                    "this block already has a default region, '{}', named at {}:{}; write 'at {}' on the lines that go there, or declare it in an inner block",
                    already, at.line, at.col, name
                ),
            );
        }
        if let Some(scope) = self.scopes.last_mut() {
            scope.default = Some((name.to_string(), span));
        }
        Ok(())
    }

    /// The region an allocation uses, given the one it writes: that region, or
    /// the block's default. `needed` is what to say when there is no default,
    /// which each form words for itself, and a region visible here is named as
    /// the one to mark.
    /// Settles the region of a call to `what` (`task 'f'`, `method 'M.m'`), whose
    /// written arguments are `args` and whose parameters, a method's without the
    /// value it is called on, are `params`. A region written after the call
    /// (`f(x) at r`, which the parser adds last; see `ExprKind::RegionAfter`)
    /// becomes the region argument it stands for, where the task takes one last
    /// that the parentheses leave out; and where the parentheses leave out only
    /// that region, it is the block's default, as for any allocation that names
    /// none. Anything else is left to the call's own checks.
    fn settle_call_region(&mut self, span: Span, what: &str, args: &mut Vec<Expr>, params: &[(String, Type)]) -> Result<()> {
        let last = params.last().filter(|(_, ty)| *ty == Type::Region);
        if let Some(at) = region_after(args) {
            if last.is_none() {
                return err(at, format!("{} takes no region last, so no region is named after its call", what));
            }
            if args.len() > params.len() {
                return err(at, format!("{} is given its region in the parentheses already, so this is a second one; name it once", what));
            }
            if let Some(arg) = args.last_mut()
                && let ExprKind::RegionAfter(region) = &mut arg.kind
            {
                arg.kind = ExprKind::Var(std::mem::take(region));
            }
            return Ok(());
        }
        // A call whose last argument is a region, where the task takes none,
        // gave its region and left out something else, which the count of its
        // arguments says. Where the task takes a region there too, as in
        // `join(t, a: region, into: region)`, that region is its own.
        let takes_one_there = args.len().checked_sub(1).and_then(|at| params.get(at)).is_some_and(|(_, ty)| *ty == Type::Region);
        let gives_region = !takes_one_there
            && args.last().is_some_and(|arg| matches!(&arg.kind, ExprKind::Var(name) if self.lookup(name).is_some_and(|info| info.ty == Type::Region)));
        if let Some((param, _)) = last
            && args.len() + 1 == params.len()
            && !gives_region
        {
            let region = self.region_or_default(None, span, || {
                format!("{} takes the region it makes memory in last, as '{}'; name it after the call, as in '… at r' or '…@r', or in the parentheses", what, param)
            });
            self.region_wanted |= region.is_err();
            let region = region?;
            args.push(Expr::new(ExprKind::Var(region), span));
        }
        Ok(())
    }

    fn region_or_default(&mut self, written: Option<&String>, span: Span, needed: impl FnOnce() -> String) -> Result<String> {
        if let Some(region) = written {
            return Ok(region.clone());
        }
        match self.default_region() {
            Some(region) => Ok(region),
            None => err(span, format!("{}{}", needed(), self.or_make_it_default())),
        }
    }

    /// The error for a `copy()` that nothing gives a region: a copy is the one
    /// thing that allocates without taking a block's default, since the place it
    /// goes says where it belongs, so the message says that where a block has one.
    fn unsettled_copy(&self) -> String {
        match self.default_region() {
            Some(_) => format!("{}{}", UNSETTLED_COPY, COPY_IGNORES_DEFAULT),
            None => UNSETTLED_COPY.to_string(),
        }
    }

    /// The end of the message for an allocation that names no region: when a
    /// region is visible here, the shortest fix is to make it the default.
    fn or_make_it_default(&self) -> String {
        match self.innermost_region() {
            Some(name) => format!("; or make a region the block's default, as in 'region {} {}'", name, DEFAULT),
            None => String::new(),
        }
    }

    /// The name of the innermost region this code can allocate in, if any.
    fn innermost_region(&self) -> Option<&str> {
        self.scopes
            .iter()
            .rev()
            .flat_map(|scope| scope.names.iter())
            .find(|(_, info)| info.ty == Type::Region && info.allocates)
            .map(|(name, _)| name.as_str())
    }

    /// The variable called `name`, which must not be a region.
    fn lookup_or_err(&self, name: &str, span: Span) -> Result<&VarInfo> {
        if let Some(info) = self.lookup(name)
            && info.ty == Type::Region
        {
            return err(span, format!("'{0}' is a region, not a value; allocate in it with 'new T(…) at {0}'", name));
        }
        self.lookup(name).ok_or_else(|| {
            if name == THIS {
                Error::at(span, self.not_a_method(THIS))
            } else if self.sees(name) && self.variants.contains_key(name) {
                Error::at(span, format!("'{}' is a variant, not a variable", name))
            } else if self.module_used(name).is_some() {
                Error::at(span, format!("'{0}' is a module, not a variable; name something in it, as in '{0}.name'", name))
            } else if self.kinds.contains_key(&self.full_name(name)) {
                Error::at(span, format!("'{0}' is a kind, not a variable; name one of its variants, as in '{0}.Variant'", name))
            } else if let Some(shape) = self.shapes.get(&self.full_name(name)) {
                // Most likely a shape literal the parser read as a plan B.
                let example = match shape.fields.first() {
                    Some(field) => format!("{} {{ {}: … }}", name, field.name),
                    None => format!("{} {{}}", name),
                };
                Error::at(span, format!("'{}' is a shape, not a variable; a literal names its fields on one line, as in '{}'", name, example))
            } else {
                let hint = match self.module_elsewhere(name) {
                    hint if hint.is_empty() => self.elsewhere(name),
                    hint => hint,
                };
                Error::at(span, format!("unknown variable '{}'{}", name, hint))
            }
        })
    }

    /// Checks the statements of a block, each whatever the ones before it
    /// found: a statement with a mistake is reported and the next one is
    /// checked, so a block's mistakes are all reported together. The names a
    /// statement that didn't check meant to declare are declared without a type,
    /// so that using them is no second mistake (see `Type::Unknown`).
    fn check_block(&mut self, block: &mut [Stmt]) -> Result<()> {
        for stmt in block {
            let checked = self.check_stmt(stmt);
            if !self.keep_going(checked) {
                self.declare_unchecked(stmt);
            }
        }
        Ok(())
    }

    /// Declares, without a type, the names a statement that didn't check would
    /// have declared. Only `bind` declares a name the rest of the block sees;
    /// what a pattern or a loop declares is visible in its own block only, which
    /// the statement checks itself.
    fn declare_unchecked(&mut self, stmt: &Stmt) {
        let StmtKind::Binding { names, mutable, .. } = &stmt.kind else { return };
        for bound in names.iter().flat_map(Bound::leaves) {
            if bound.name != DISCARD && self.lookup(&bound.name).is_none() {
                // A name that can't be declared at all (one already taken by a
                // variant, say) is the statement's own mistake, already reported.
                let _ = self.define(&bound.name, Type::Unknown, *mutable, bound.span);
            }
        }
    }

    /// Checks a statement, whose copies must all know their regions when it ends.
    fn check_stmt(&mut self, stmt: &mut Stmt) -> Result<()> {
        let before = self.unsettled_copies.len();
        let types_before = self.unsettled_types.len();
        let result = self.check_stmt_kind(stmt);
        let unsettled = self.unsettled_copies.get(before).copied();
        let untold = self.unsettled_types.get(types_before).cloned();
        self.unsettled_copies.truncate(before);
        self.unsettled_types.truncate(types_before);
        result?;
        if let Some(at) = unsettled {
            return err(at, self.unsettled_copy());
        }
        match untold {
            Some((at, task, param)) => err(at, self.untold_type(&task, &param)),
            None => Ok(()),
        }
    }

    fn check_stmt_kind(&mut self, stmt: &mut Stmt) -> Result<()> {
        let span = stmt.span;
        match &mut stmt.kind {
            StmtKind::Expr(e) => {
                // `fail e` names a plan B's error only on its first line.
                if let ExprKind::Fail(inner) = &e.kind
                    && let ExprKind::Var(name) = &inner.kind
                    && self.lookup(name).is_none()
                {
                    return err(e.span, format!("'fail {0}' makes a failure, but nothing here is named '{0}'; a plan B names its failure with 'failure {0}' on its first line, and '<- fail {0}' sends one", name));
                }
                self.check_expr(e)?;
                // Of all expressions, only a call, a change to a slice, or a
                // `scan` can have an effect, and so can a plan B on a call or a
                // `scan`, or one that gives no value.
                match &e.kind {
                    ExprKind::Call { name, .. } if matches!(e.ty, Some(Type::Or(..))) => err(e.span, ignored_error(name)),
                    // `int(x)` and `float(x)` read like calls, but they only give
                    // a number back: on its own, the line does nothing.
                    ExprKind::Call { name, .. } if number_cast(name).is_some() => {
                        err(e.span, format!("the number '{}' gives is unused; only a task call can be used as a statement", name))
                    }
                    // A line read and dropped is a line dropped on purpose, but
                    // a number read and dropped drops the error with it.
                    ExprKind::Scan { .. } if matches!(e.ty, Some(Type::Or(..))) => err(e.span, ignored_error(SCAN)),
                    ExprKind::Call { .. } | ExprKind::Edit { .. } | ExprKind::Scan { .. } => Ok(()),
                    // A method called on a value whose type is unknown stays
                    // unread, and whatever made the type unknown was reported.
                    ExprKind::MethodCall { .. } => Ok(()),
                    // A method call left unread, whose mistake is reported, counts as one.
                    ExprKind::PlanB { value, .. }
                        if e.ty == Some(Type::Unit) || matches!(value.kind, ExprKind::Call { .. } | ExprKind::Scan { .. } | ExprKind::MethodCall { .. }) =>
                    {
                        Ok(())
                    }
                    ExprKind::Binary(BinaryOp::Eq, ..) => err(e.span, "the result of '==' is unused; use '=' to assign"),
                    _ => err(e.span, "this value is unused; only a task call can be used as a statement"),
                }
            }
            StmtKind::Binding { names, mutable, reference: true, init } => self.check_reference(names, *mutable, init),
            StmtKind::Binding { names, mutable, init, .. } => {
                for bound in names.iter_mut() {
                    self.resolve_bound_types(bound)?;
                }
                // `hold y = &x`: a copy, where a reference was maybe meant.
                if let Some(marked) = init.iter().find(|value| matches!(value.kind, ExprKind::Ref(_))) {
                    let word = if *mutable { "hold" } else { "bind" };
                    let name = names.first().map_or("y", |bound| bound.name.as_str());
                    return err(marked.span, format!("'{0} {1}' takes a copy, so leave out '&'; '{0}& {1} = &…' is another name for the place", word, name));
                }
                let given = self.value_types(init, names.len(), span, true)?;
                // A single call sending several values gives them all, and
                // those values are taken as they are.
                let forwarded = init.len() < names.len();
                let list_span = init.first().map_or(span, |value| value.span);
                let mut declared = Vec::new();
                for i in 0..names.len() {
                    let Some(actual) = given.get(i).cloned() else { continue };
                    let value_span = if forwarded { list_span } else { init.get(i).map_or(span, |value| value.span) };
                    // `(a, b)`: the names of a tuple's items.
                    if let Some(group) = names.get_mut(i).filter(|bound| !bound.parts.is_empty()) {
                        self.unpack_bound(group, &actual, value_span, &mut declared)?;
                        continue;
                    }
                    let Some((name, at, annotated)) = names.get(i).map(|b| (b.name.clone(), b.span, b.ty.clone())) else { continue };
                    let ty = match annotated {
                        Some(annotated) => {
                            match init.get_mut(i).filter(|_| !forwarded) {
                                Some(value) => self.coerce(value, &annotated)?,
                                None => fits_or_err(&actual, &annotated, value_span, given_by(init))?,
                            }
                            annotated
                        }
                        None if name == DISCARD => actual,
                        // A variable without a type names no region for a copy.
                        None if matches!(init.get(i).filter(|_| !forwarded).map(|value| &value.kind), Some(ExprKind::Copy { region: None, .. })) => {
                            return err(value_span, self.unsettled_copy());
                        }
                        None => self.inferred_type(&name, &actual, value_span)?,
                    };
                    if name == DISCARD && matches!(ty, Type::Or(..)) {
                        return err(value_span, dropped_error(&ty));
                    }
                    if let Some(bound) = names.get_mut(i) {
                        bound.ty = Some(ty.clone());
                    }
                    declared.push((name, at, ty));
                }
                for (name, at, ty) in declared {
                    if name != DISCARD {
                        self.define(&name, ty, *mutable, at)?;
                    }
                }
                Ok(())
            }
            StmtKind::Assign(targets, values) => {
                let mut places = Vec::new();
                let mut assigned: HashMap<String, Span> = HashMap::new();
                for target in targets.iter_mut() {
                    places.push(self.check_target(target, &mut assigned)?);
                }
                // `y = &x`: '=' writes a value; nothing points a name elsewhere.
                if let Some(marked) = values.iter().find(|value| matches!(value.kind, ExprKind::Ref(_))) {
                    let reference = targets.iter().find_map(|target| match target {
                        Target::Place(Expr { kind: ExprKind::Var(name), .. }) if self.lookup(name).is_some_and(|info| info.refers.is_some()) => Some(name.clone()),
                        _ => None,
                    });
                    return err(
                        marked.span,
                        match reference {
                            Some(name) => format!("'{0}' names one place for as long as it lasts, and '{0} = …' writes a value there, so leave out '&'", name),
                            None => "'=' writes a value, so leave out '&'; a name for a place is declared, as in 'hold& y = &x'".to_string(),
                        },
                    );
                }
                let given = self.value_types(values, targets.len(), span, true)?;
                let forwarded = values.len() < targets.len();
                let list_span = values.first().map_or(span, |value| value.span);
                for (i, place) in places.into_iter().enumerate() {
                    let Some(actual) = given.get(i).cloned() else { continue };
                    let value_span = if forwarded { list_span } else { values.get(i).map_or(span, |value| value.span) };
                    let place = match place {
                        TargetType::Tuple(parts, at) => {
                            self.unpack_target(&parts, at, &actual, value_span)?;
                            continue;
                        }
                        TargetType::Discard => None,
                        TargetType::Place(ty) => Some(ty),
                    };
                    let Some(expected) = place else {
                        if matches!(actual, Type::Or(..)) {
                            return err(value_span, dropped_error(&actual));
                        }
                        continue;
                    };
                    match values.get_mut(i).filter(|_| !forwarded) {
                        Some(value) => self.coerce(value, &expected)?,
                        None => fits_or_err(&actual, &expected, value_span, given_by(values))?,
                    }
                }
                Ok(())
            }
            StmtKind::SetResults(names, values) => self.check_set_results(span, names, values),
            StmtKind::When(cond, then_block, else_block) => self.check_when(span, cond, then_block, else_block, None).map(|_| ()),
            // Its statements run once, as the block around it does, so what they
            // set is set after it; what they declare ends at its `fi`.
            StmtKind::Block(body) => self.check_scoped_block(body),
            StmtKind::Loop(cond, body) => {
                if let Some(pattern) = &cond.pattern
                    && matches!(pattern.kind, PatternKind::Tuple(_))
                {
                    return err(pattern.span, "a tuple's items always match, so this loop would never end; take the tuple apart with 'bind' instead");
                }
                // A loop's body might not run, so it sets nothing for certain.
                let entry = self.unset.clone();
                self.check_condition(cond, body, None)?;
                self.unset = entry;
                Ok(())
            }
            StmtKind::Each(name, range, body) => {
                // A range is only valid here, so it is checked here rather than
                // as an ordinary expression.
                let item = if let ExprKind::Range(start, end) = &mut range.kind {
                    for bound in [start, end] {
                        let bty = self.check_expr(bound)?;
                        self.expect_type(&bty, &Type::Int, bound.span)?;
                    }
                    range.ty = Some(Type::Range(Box::new(Type::Int)));
                    Type::Int
                } else {
                    match self.check_expr(range)? {
                        Type::Array(items, _) | Type::Slice(items, _) if unsettled(&items) => {
                            return err(range.span, format!("cannot tell the type of '{}' from none or 'fail …'; give the array a type first", name));
                        }
                        Type::Array(items, _) | Type::Slice(items, _) => *items,
                        // What it goes through couldn't be worked out, so what
                        // each item is can't be either; the body is still checked
                        // with the name declared (see `Type::Unknown`).
                        other if other.unknown() => Type::Unknown,
                        other => {
                            return err(
                                range.span,
                                format!("each goes through a range, such as 0..n, or the items of an array or slice, but this value is {}{}", other, extract_hint(&[&other])),
                            );
                        }
                    }
                };
                // The loop variable and the body share one scope.
                let entry = self.unset.clone();
                self.push_scope();
                let result = self.define(name, item, false, span)
                    .and_then(|()| self.check_block(body));
                self.pop_scope();
                self.unset = entry;
                result
            }
            StmtKind::Match(expr, cases) => self.check_match(span, expr, cases, None).map(|_| ()),
            StmtKind::Send(values) => {
                let ret = self.current_ret.clone().unwrap_or(Type::Unit);
                let expected = ret.results();
                if values.is_empty() {
                    if !self.result_names.is_empty() {
                        // A bare `<-` sends the results as they are.
                        return match self.first_unset() {
                            Some(name) => err(span, format!("'{}' might not be set here", name)),
                            None => Ok(()),
                        };
                    }
                    return match (sends_value(&ret), expected.len()) {
                        (false, _) => Ok(()),
                        (true, 1) => err(span, "expected a value after '<-'"),
                        (true, n) => err(span, format!("expected {} after '<-', one for each result", count(n, "value"))),
                    };
                }
                let first = values.first().map_or(span, |value| value.span);
                if expected.is_empty() {
                    return err(first, "this task has no return type, so '<-' cannot take a value");
                }
                // One value for one result, which may become a `maybe`, a
                // `T ~ E`, or the error of one.
                if let ([e], [ret]) = (values.as_mut_slice(), expected) {
                    let ret = ret.clone();
                    let ety = self.check_expr(e)?;
                    if !sends_value(&ret) && !matches!(ety, Type::Fail(_)) && !fits_regions(&ety, &ret) {
                        return err(e.span, "this task sends no value when it succeeds; write a bare '<-', or '<- fail …' to send an error");
                    }
                    return self.coerce(e, &ret);
                }
                let expected = expected.to_vec();
                let given = self.value_types(values, expected.len(), span, false).map_err(|mut error| {
                    // `-> (int, int)` sends one tuple, and `-> int, int` two values.
                    if let [Type::Tuple(_)] = expected.as_slice() {
                        error.message.push_str("; this task sends one tuple, so write its values in parentheses, as in '<- (a, b)'");
                    }
                    error
                })?;
                let forwarded = values.len() < expected.len();
                for (i, want) in expected.iter().enumerate() {
                    let Some(actual) = given.get(i).cloned() else { continue };
                    match values.get_mut(i).filter(|_| !forwarded) {
                        Some(value) => self.coerce(value, want)?,
                        None => fits_or_err(&actual, want, first, Given::Sent)?,
                    }
                }
                Ok(())
            }
            StmtKind::Region { name, default } => {
                self.define_region(name, span, true)?;
                match default {
                    true => self.set_default_region(name, span),
                    false => Ok(()),
                }
            }
        }
    }

    /// Checks a `when`, its `else`, and the `else when`s in between (a chain is
    /// the `when` its `else` holds). In `gives`, every branch gives the whole a
    /// value, which the branches settle between them; a chain gives it through
    /// the `when` on the last line of its `else`, which `value_block_tail`
    /// reads. A `when` that gives a value needs an `else`, since there is no
    /// value without one.
    fn check_when(
        &mut self,
        span: Span,
        cond: &mut Condition,
        then_block: &mut Block,
        else_block: &mut Option<Block>,
        gives: Option<Branches>,
    ) -> Result<Option<Type>> {
        // A tuple's items always match, so an `else` could never run.
        if let (Some(pattern), Some(_)) = (&cond.pattern, else_block.as_ref())
            && matches!(pattern.kind, PatternKind::Tuple(_))
        {
            return err(pattern.span, "a tuple's items always match, so this 'when' always holds and its 'else' would never run; take the tuple apart with 'bind' instead");
        }
        if gives.is_some() && else_block.is_none() {
            return err(span, "a 'when' used as a value needs an 'else': without one, nothing gives the value when the condition doesn't hold");
        }
        let entry = self.unset.clone();
        let mut given = Vec::new();
        let then_value = match gives {
            None => {
                self.check_condition(cond, then_block, None)?;
                None
            }
            Some(_) => {
                let found = self.check_condition(cond, then_block, gives);
                self.note_branch(found, span)
            }
        };
        given.extend(then_value.map(|(ty, at)| (0usize, ty, at)));
        let after_then = self.branch_state(then_block, &entry);
        let after_else = match else_block {
            Some(else_b) => {
                let value = match gives {
                    Some(branches) => {
                        let found = self.check_value_block(Vec::new(), else_b, span, branches);
                        self.note_branch(found, span)
                    }
                    None => self.check_scoped_block(else_b).map(|()| None)?,
                };
                given.extend(value.map(|(ty, at)| (1usize, ty, at)));
                self.branch_state(else_b, &entry)
            }
            // A condition that always holds leaves no other path.
            None if always_holds(cond) => None,
            // Without an `else`, not running the block is a path too.
            None => Some(entry.clone()),
        };
        self.join_branches(entry, vec![after_then, after_else]);
        let Some(branches) = gives else { return Ok(None) };
        let settled = self.settle_branches(span, &given, branches)?;
        let Some(settled) = settled else { return Ok(None) };
        for (i, _, _) in given.iter().filter(|(_, ty, _)| !ty.unknown() && *ty != settled) {
            match (i, else_block.as_mut()) {
                (0, _) => self.settle_value(then_block, &settled)?,
                (_, Some(else_b)) => self.settle_value(else_b, &settled)?,
                (_, None) => {}
            }
        }
        self.escaping_region(span, &settled, branches)?;
        Ok(Some(settled))
    }

    /// Records the mistake a branch's value had, if any, and says what it gives:
    /// nothing where every path through it sends, and an unknown type where it
    /// didn't check, so that the other branches still settle theirs and nothing
    /// complains about it twice (see `Type::Unknown`).
    fn note_branch(&mut self, found: Result<Option<(Type, Span)>>, at: Span) -> Option<(Type, Span)> {
        match found {
            Ok(value) => value,
            Err(error) => {
                self.record(error);
                Some((Type::Unknown, at))
            }
        }
    }

    /// Coerces the value a block gives to `settled`, the type the branches
    /// settled on. It runs once the block's own scope has closed, so a value in a
    /// region the block declared can't become the whole's. A `when` or a `match`
    /// on the last line is coerced through its own branches, since it settled a
    /// type among them that may be narrower than the whole: an `int` where a case
    /// beside it gives `none`, and so the whole is a `maybe int`.
    fn settle_value(&mut self, block: &mut Block, settled: &Type) -> Result<()> {
        let Some(last) = block.last_mut() else { return Ok(()) };
        match &mut last.kind {
            // A value with no type never checked, which is already reported: there
            // is nothing to coerce (see `Type::Unknown`).
            StmtKind::Expr(e) if e.ty.is_none() => Ok(()),
            StmtKind::Expr(e) => self.coerce(e, settled),
            // A `when` with no `else`, or a block every path sends from, gives no
            // value of its own to coerce (see `gives_value`).
            _ => self.settle_branch_values(last, settled),
        }
    }

    /// Coerces the value each branch of a `when` or a `match` gives to `settled`,
    /// the type the place it goes, or the branches around it, settled on. A
    /// branching value becomes what is expected branch by branch, rather than as a
    /// whole: each branch's own `none` or `fail e` then becomes part of the
    /// `maybe` or the `T ~ E`, which is what the branch gives back.
    fn settle_branch_values(&mut self, stmt: &mut Stmt, settled: &Type) -> Result<()> {
        match &mut stmt.kind {
            StmtKind::When(_, then_block, Some(else_block)) => {
                self.settle_value(then_block, settled)?;
                self.settle_value(else_block, settled)
            }
            StmtKind::Match(_, cases) => cases.iter_mut().try_for_each(|case| self.settle_value(&mut case.body, settled)),
            _ => Ok(()),
        }
    }

    /// Refuses a value in a region that is freed before the `match` or `when`
    /// giving it ends: the region was declared inside a branch, so nothing
    /// outside can name it.
    fn escaping_region(&mut self, at: Span, settled: &Type, branches: Branches) -> Result<()> {
        let escaping = settled.regions().into_iter().find(|region| *region != ANY_REGION && self.lookup(region).is_none());
        match escaping {
            Some(region) => err(
                at,
                format!("this value is in region '{}', which is freed when the {} ends, so it can't be the {}'s value", region, branches.each, branches.of),
            ),
            None => Ok(()),
        }
    }

    /// Checks a `when` or `loop` condition and the block it guards, where the
    /// names an `is` pattern binds are visible. In `gives`, the block gives a
    /// value on its last line, which comes back with its position (see
    /// `check_value_block`); otherwise the block holds statements and nothing
    /// comes back.
    fn check_condition(&mut self, cond: &mut Condition, body: &mut Block, gives: Option<Branches>) -> Result<Option<(Type, Span)>> {
        let found = self.check_expr(&mut cond.value);
        let ty = self.or_unknown(found);
        let Some(pattern) = &cond.pattern else {
            self.expect_type(&ty, &Type::Bool, cond.value.span)?;
            return match gives {
                Some(branches) => self.check_value_block(Vec::new(), body, cond.value.span, branches),
                None => self.check_scoped_block(body).map(|()| None),
            };
        };
        if matches!(pattern.kind, PatternKind::Any) {
            return err(pattern.span, "'_' matches anything, so this condition would always hold");
        }
        let bindings = self.pattern_bindings(&ty, pattern, false)?;
        match gives {
            Some(branches) => self.check_value_block(bindings, body, pattern.span, branches),
            None => self.check_block_with(bindings, body).map(|()| None),
        }
    }

    /// Checks a block that gives a value, in a new scope that first declares
    /// `bindings` (from a pattern). Its statements are checked as usual, and its
    /// last line gives the value: an expression, a `when` or a `match` whose own
    /// branches give it, or nothing at all when every path through the block
    /// sends with `<-`. `at` is where to blame a block that gives no value.
    fn check_value_block(&mut self, bindings: Vec<Declared>, body: &mut Block, at: Span, branches: Branches) -> Result<Option<(Type, Span)>> {
        self.push_scope();
        let result = bindings
            .into_iter()
            .filter(|(name, _, _, _)| name != DISCARD)
            .try_for_each(|(name, span, ty, mutable)| self.define(&name, ty, mutable, span))
            .and_then(|()| self.value_block_tail(body, at, branches));
        self.pop_scope();
        result
    }

    /// The statements of a block that gives a value, checked in the scope
    /// `check_value_block` opened: everything but the last line as usual, and the
    /// last line as the value.
    fn value_block_tail(&mut self, body: &mut Block, at: Span, branches: Branches) -> Result<Option<(Type, Span)>> {
        let Some((last, rest)) = body.split_last_mut() else {
            return err(at, branches.needs_value());
        };
        self.check_block(rest)?;
        // A `when` or a `match` on the last line gives the value through its own
        // branches, which is what makes a chain of conditions give one.
        if gives_value(last) {
            let span = last.span;
            return Ok(self.check_branching_value(last)?.map(|ty| (ty, span)));
        }
        if let StmtKind::Expr(e) = &mut last.kind {
            let ty = self.check_expr(e)?;
            if ty == Type::Unit {
                return err(e.span, format!("the last line of a {} gives its value, but this has none", branches.each));
            }
            return Ok(Some((ty, e.span)));
        }
        self.check_stmt(last)?;
        if !always_sends(body) {
            return err(at, branches.needs_value());
        }
        Ok(None)
    }

    /// A `when` or a `match` used as a value: the type its branches settle on,
    /// and nothing where every branch sends. The statement is the same one it is
    /// on its own, so its patterns, its cases and its bookkeeping are checked as
    /// they always are.
    fn check_branching_value(&mut self, stmt: &mut Stmt) -> Result<Option<Type>> {
        let span = stmt.span;
        match &mut stmt.kind {
            StmtKind::When(cond, then_block, else_block) => self.check_when(span, cond, then_block, else_block, Some(WHEN_BRANCHES)),
            StmtKind::Match(subject, cases) => self.check_match(span, subject, cases, Some(MATCH_BRANCHES)),
            _ => Err(Error::internal_at(span, "a statement that is not a 'when' or a 'match' used as a value")),
        }
    }

    /// The type the branches of a `match` or a `when` used as a value settle on:
    /// the types they give, joined one after another by the rule the items of an
    /// array follow (`join_types`), so that a branch giving `none` or a failure
    /// makes the whole a `maybe` or a `T ~ E`, as a value becomes one anywhere
    /// else. A branch whose value had a mistake of its own settles nothing, and
    /// `None` comes back where nothing settles it.
    fn settle_branches(&self, at: Span, given: &[(usize, Type, Span)], branches: Branches) -> Result<Option<Type>> {
        let mut settled: Option<Type> = None;
        for (_, ty, _) in given.iter().filter(|(_, ty, _)| !ty.unknown()) {
            let Some(sofar) = &settled else {
                settled = Some(ty.clone());
                continue;
            };
            match join_types(sofar, ty) {
                Some(joined) => settled = Some(joined),
                // The branches disagree, which the loop below reports against
                // what they had settled on until here.
                None => break,
            }
        }
        // A branch that didn't check may have been the one that told `none` or a
        // failure what it is part of, so nothing more is said about it here.
        if let Some(ty) = &settled
            && unsettled(ty)
            && given.iter().any(|(_, ty, _)| ty.unknown())
        {
            return Ok(None);
        }
        let Some(settled) = settled else {
            if given.is_empty() {
                return err(at, format!("every {} of this {} sends with '<-', so nothing gives it a value", branches.each, branches.of));
            }
            // Every branch that gives a value had a mistake of its own, which is
            // already reported (see `Type::Unknown`).
            return Ok(None);
        };
        // Every branch must fit what they settled on, which the fold above only
        // promises for the ones it read before a widening or a disagreement.
        for (_, ty, span) in given.iter().filter(|(_, ty, _)| !ty.unknown()) {
            if !fits(ty, &settled) {
                return err(
                    *span,
                    format!("this {} gives {}, and another gives {}; every {} of a {} used as a value gives one type", branches.each, ty, settled, branches.each, branches.of),
                );
            }
        }
        Ok(Some(settled))
    }

    /// Checks that `pattern` can match a value of type `subject`, and returns
    /// the names it binds with their types. `in_case` words messages for a
    /// `case` rather than an `is`.
    fn pattern_bindings(&self, subject: &Type, pattern: &Pattern, in_case: bool) -> Result<Vec<Declared>> {
        let span = pattern.span;
        // Nothing is known about the value, so nothing about what matches it
        // either; the names the pattern writes are declared without a type, so
        // the block it guards still reads (see `Type::Unknown`).
        if subject.unknown() {
            return Ok(pattern.bindings().into_iter().map(|names| (names.name.clone(), names.span, Type::Unknown, names.mutable)).collect());
        }
        // `fail TooBig` or `{Red}`: a pattern names the whole error or value.
        if let PatternKind::Value(names) | PatternKind::Fail(names) = &pattern.kind
            && names.parts.is_empty()
            && self.variant_in_scope(&names.name).is_some()
        {
            let (keyword, whole) = if matches!(pattern.kind, PatternKind::Fail(_)) { ("failure e", "error") } else { ("{value}", "value") };
            return err(names.span, format!("'{}' is a variant, but '{}' names the whole {}; give it a name, then match on it", names.name, keyword, whole));
        }
        match (&pattern.kind, subject) {
            (PatternKind::Any | PatternKind::None, Type::Maybe(_)) => Ok(Vec::new()),
            (PatternKind::Any, _) => Ok(Vec::new()),
            (PatternKind::Variant { kind, tag, binds }, Type::Named(kind_name, region)) if self.kinds.contains_key(kind_name) => {
                if let Some((written, at)) = kind.as_deref()
                    && written != kind_name
                {
                    return err(*at, format!("this pattern names kind '{}', but the value's kind is '{}'", written, kind_name));
                }
                let variant = self.kinds.get(kind_name)
                    .and_then(|kind| kind.variants.iter().find(|v| v.tag == *tag))
                    .ok_or_else(|| Error::at(span, format!("kind '{}' has no variant '{}'", kind_name, tag)))?;
                if binds.len() != variant.payload.len() {
                    let what = if in_case { format!("case '{}'", tag) } else { format!("'{}'", tag) };
                    return err(span, format!("{} expects {}, got {}", what, count(variant.payload.len(), "binding"), binds.len()));
                }
                binds
                    .iter()
                    .zip(&variant.payload)
                    .map(|(names, field)| self.bound_names(names, &self.field_in(&field.ty, region), false))
                    .collect::<Result<Vec<_>>>()
                    .map(|bound| bound.concat())
            }
            (PatternKind::Variant { kind: Some(written), tag, .. }, other) => {
                let (kind, at) = &**written;
                match self.kinds.get(kind) {
                    Some(declared) if declared.variants.iter().any(|v| v.tag == *tag) => {
                        err(span, format!("'{}' is a variant of kind '{}', but this value is {}", tag, kind, other))
                    }
                    _ => err(*at, format!("kind '{}' has no variant '{}'", kind, tag)),
                }
            }
            (PatternKind::Variant { tag, .. }, other) => {
                if self.variant_in_scope(tag).is_some() {
                    err(span, format!("'{}' is a variant, but this value is {}", tag, other))
                } else {
                    err(span, format!("unknown variant '{}'", tag))
                }
            }
            (PatternKind::Value(names), Type::Maybe(inner)) => self.bound_names(names, inner, false),
            (PatternKind::Value(names), Type::Or(value, _)) => {
                if **value == Type::Unit && names.name != DISCARD {
                    return err(names.span, "this success has no value to name; write '{_}'");
                }
                self.bound_names(names, value, false)
            }
            (PatternKind::Fail(names), Type::Or(_, error)) => self.bound_names(names, error, false),
            // `(x, y)`: the items of a tuple, which always match.
            (PatternKind::Tuple(names), _) => self.bound_names(names, subject, false),
            (PatternKind::Value(_), other) => {
                err(span, format!("'{{…}}' matches the value in a maybe or a 'T ~ E', but this value is {}", other))
            }
            (PatternKind::None, Type::None) => err(span, "this value is always none, so there is nothing to match"),
            (PatternKind::None, other) => err(span, format!("'none' matches an empty maybe, but this value is {}", other)),
            (PatternKind::Fail(_), other) => {
                err(span, format!("'failure …' matches the error of a 'T ~ E', but this value is {}", other))
            }
        }
    }

    /// The names `names` binds from a value of type `ty`, with their types:
    /// one name takes the value, and `(a, b)` the items of a tuple. `mutable`
    /// is a `hold` written around them.
    fn bound_names(&self, names: &Names, ty: &Type, mutable: bool) -> Result<Vec<Declared>> {
        let mutable = mutable || names.mutable;
        if names.parts.is_empty() {
            return Ok(vec![(names.name.clone(), names.span, ty.clone(), mutable)]);
        }
        let items = self.tuple_items(ty, names.parts.len(), names.span)?;
        names
            .parts
            .iter()
            .zip(&items)
            .map(|(part, item)| self.bound_names(part, item, mutable))
            .collect::<Result<Vec<_>>>()
            .map(|bound| bound.concat())
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
                if info.result {
                    return err(span, format!("'{0}' is a result; set it with '{0} <- …'", name));
                }
                if !info.mutable {
                    if name == THIS {
                        return Err(self.this_cannot_change(span, &format!("assigning '{}'", THIS)));
                    }
                    return err(span, format!("cannot assign to immutable variable '{}'{}", name, param_hint(name, info)));
                }
                let ty = info.ty.clone();
                expr.ty = Some(ty.clone());
                Ok(ty)
            }
            ExprKind::Field(base, field) => {
                // A member of the value a method is called on, which only a
                // method marked `alters` may change.
                if matches!(&base.kind, ExprKind::Var(name) if name == THIS) && !self.lookup(THIS).is_some_and(|info| info.mutable) {
                    return Err(self.this_cannot_change(span, &format!("assigning '{}{}'", MODULE_SEPARATOR, field)));
                }
                let base_ty = self.check_field_base(base)?;
                let ty = self.field_type(&base_ty, field, span)?;
                self.read_only_field(&base_ty, field, span)?;
                match base_ty {
                    Type::Tuple(_) => {
                        return err(span, "a tuple's items never change; give the whole tuple a new value instead, as in 't = (1, 2)'");
                    }
                    Type::Array(..) => return err(span, "'len' can't be assigned: an array's length is part of its type"),
                    Type::Slice(..) => return err(span, "'len' can't be assigned: a slice's length changes with 'xs.add(…)' and 'xs.del(…)', or by slicing it, as in 'xs = xs[0..2]'"),
                    Type::Text(_) => return err(span, "'len' can't be assigned: it is how many bytes the text has, and text never changes; give the variable other text instead"),
                    _ => {}
                }
                expr.ty = Some(ty.clone());
                Ok(ty)
            }
            ExprKind::Deref(_) => self.check_expr(expr),
            ExprKind::Shared { shape, field } => {
                let (shape, field) = (shape.clone(), field.clone());
                let ty = self.check_expr(expr)?;
                self.changeable_shared(&shape, &field, span, "")?;
                Ok(ty)
            }
            ExprKind::Index(base, index) => {
                if matches!(index.kind, ExprKind::Range(..)) {
                    if matches!(self.check_expr(base)?, Type::Text(_)) {
                        return err(span, "a part of text can't be assigned, since text never changes; give the variable other text instead, as in 't = t[0..2]'");
                    }
                    return err(span, "a slice of values can't be assigned; assign its items one at a time, as in 'xs[i] = v'");
                }
                let base_ty = self.check_item_base(base)?;
                let ty = self.check_index(base, &base_ty, index, span)?;
                expr.ty = Some(ty.clone());
                Ok(ty)
            }
            _ => err(span, "only a variable, a field, an item of an array or slice, or the value a pointer points to (p^) can be assigned"),
        }
    }

    /// Refuses to change `field` of a value of type `base` where it is a `read`
    /// field, outside the alters methods of its own shape: it is read anywhere, so what
    /// a shape keeps true of itself stays its own, in the file that declares it
    /// as much as in another. Assigning it, an item of it, or a part of it, or
    /// calling a method that alters it, all change it.
    fn read_only_field(&self, base: &Type, field: &str, span: Span) -> Result<()> {
        if let Some(owner) = self.owner_of(base).filter(|owner| self.current_owner.as_ref() != Some(owner))
            && self.declared_field(&owner, field).is_some_and(|declared| declared.read)
        {
            // The full name, which each file's message spells as it writes it.
            return err(span, format!("'{}' is a read-only field of '{1}', which only the alters methods of '{1}' change", field, owner));
        }
        Ok(())
    }

    /// Checks the array or slice whose item is assigned, and returns its type.
    /// An array holds its items, so it must be an assignable place itself; a
    /// slice's items are in a region, behind a pointer, so they can always be
    /// written, as through `p^`.
    fn check_item_base(&mut self, base: &mut Expr) -> Result<Type> {
        if let ExprKind::Var(name) = &base.kind {
            let info = self.lookup_or_err(name, base.span)?;
            if matches!(info.ty, Type::Array(..)) {
                if info.result {
                    return err(base.span, format!("'{0}' is a result; set it with '{0} <- …'", name));
                }
                if !info.mutable {
                    return err(base.span, format!("cannot assign to an item of immutable variable '{}'{}", name, param_hint(name, info)));
                }
            }
            return self.check_expr(base);
        }
        let ty = self.check_expr(base)?;
        if matches!(ty, Type::Array(..)) {
            self.assignable_array(base)?;
        }
        Ok(ty)
    }

    /// Rejects an array, already checked, whose item can't be assigned: one in a
    /// `bind` variable, a result, or a temporary value.
    fn assignable_array(&self, place: &Expr) -> Result<()> {
        match &place.kind {
            ExprKind::Var(name) => {
                let info = self.lookup_or_err(name, place.span)?;
                if info.result {
                    return err(place.span, format!("'{0}' is a result; set it with '{0} <- …'", name));
                }
                if !info.mutable {
                    return err(place.span, format!("cannot assign to an item of immutable variable '{}'{}", name, param_hint(name, info)));
                }
                Ok(())
            }
            ExprKind::Deref(_) => Ok(()),
            ExprKind::Shared { shape, field } => self.changeable_shared(shape, field, place.span, ""),
            ExprKind::Field(base, field) => {
                if let Some(ty) = &base.ty {
                    self.read_only_field(ty, field, place.span)?;
                }
                self.assignable_array(base)
            }
            ExprKind::Index(base, _) if matches!(base.ty, Some(Type::Slice(..))) => Ok(()),
            ExprKind::Index(base, _) => self.assignable_array(base),
            _ => err(place.span, "cannot assign to an item of a temporary value"),
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
                // a kind) the field error from `field_type` is the useful one. A
                // shape given types counts, whether it has been made for them
                // yet or not.
                let is_shape = match &info.ty {
                    Type::Named(ty, _) => self.shapes.contains_key(ty),
                    Type::Applied(applied) => self.shapes.contains_key(&applied.name),
                    _ => false,
                };
                if is_shape && info.result {
                    return err(span, format!("'{0}' is a result; set it with '{0} <- …'", name));
                }
                if is_shape && !info.mutable {
                    return err(span, format!("cannot assign to a field of immutable variable '{}'{}", name, param_hint(name, info)));
                }
                self.check_expr(base)
            }
            ExprKind::Field(..) | ExprKind::Deref(_) | ExprKind::Index(..) | ExprKind::Shared { .. } => self.check_assign_target(base),
            _ => err(span, "cannot assign to a field of a temporary value"),
        }
    }

    /// Checks a `match` and its cases. In `gives`, every case gives the match a
    /// value, which the cases settle between them (see `settle_branches`);
    /// otherwise the cases hold statements and nothing comes back.
    fn check_match(&mut self, span: Span, expr: &mut Expr, cases: &mut [Case], gives: Option<Branches>) -> Result<Option<Type>> {
        let found = self.check_expr(expr);
        let ety = self.or_unknown(found);
        // The value's type couldn't be worked out, so which cases cover it
        // isn't known; each case is still checked, and none is missing or
        // covered twice as far as this match can tell (see `Type::Unknown`).
        let unknown = ety.unknown();
        // The cases that together cover every value.
        let needed: Vec<String> = match &ety {
            _ if unknown => Vec::new(),
            Type::Named(name, _) => match self.kinds.get(name) {
                Some(kind) => kind.variants.iter().map(|v| v.tag.clone()).collect(),
                None => return err(expr.span, format!("'match' needs a kind, a maybe, or a 'T ~ E', but '{}' is a shape", name)),
            },
            Type::Maybe(_) => vec!["{…}".to_string(), "none".to_string()],
            Type::Or(..) => vec!["{…}".to_string(), "failure …".to_string()],
            // A tuple has one case, so it is taken apart rather than matched.
            Type::Tuple(_) => {
                return err(expr.span, format!("match requires a kind, a maybe, or a 'T ~ E' value, got {}; take a tuple apart with 'bind', as in 'bind (a, b) = t'", ety));
            }
            other => return err(expr.span, format!("match requires a kind, a maybe, or a 'T ~ E' value, got {}", other)),
        };
        let mut seen: Vec<String> = Vec::new();
        let mut matched_all = false;
        // Every case is a path of its own, and a result is set after the match
        // only where every case that runs on sets it.
        let entry = self.unset.clone();
        let mut branches = Vec::new();
        // The type each case gives, where the match is a value and the case
        // gives one: a case that sends gives none.
        let mut given = Vec::new();
        for (i, case) in cases.iter_mut().enumerate() {
            if matched_all {
                return err(case.pattern.span, "this case comes after '_', which already matches everything");
            }
            let bindings = self.pattern_bindings(&ety, &case.pattern, true)?;
            let label = case.pattern.label();
            if matches!(case.pattern.kind, PatternKind::Any) {
                if !unknown && needed.iter().all(|n| seen.contains(n)) {
                    return err(case.pattern.span, "every case is already covered, so '_' never matches");
                }
                matched_all = true;
            } else if seen.contains(&label) {
                return err(case.pattern.span, format!("duplicate case '{}'", label));
            }
            seen.push(label);
            match gives {
                Some(words) => {
                    let found = self.check_value_block(bindings, &mut case.body, case.pattern.span, words);
                    let value = self.note_branch(found, case.pattern.span);
                    given.extend(value.map(|(ty, at)| (i, ty, at)));
                }
                None => {
                    // `case p = value` where the match gives none: its body is
                    // that one value, written on the pattern's own line, which
                    // nothing would read.
                    if let [Stmt { kind: StmtKind::Expr(value), .. }] = case.body.as_slice()
                        && value.span.line == case.pattern.span.line
                    {
                        return err(value.span, "a case gives a value with '=' only where the match is one itself, as in 'bind name = match …'; the cases of a match on its own hold statements, written on the lines below it");
                    }
                    self.check_block_with(bindings, &mut case.body)?;
                }
            }
            branches.push(self.branch_state(&case.body, &entry));
        }
        // A value the cases don't cover would leave the match without setting
        // anything, but then a case is missing, which is the error below.
        self.join_branches(entry, branches);
        if !matched_all
            && let Some(missing) = needed.iter().find(|n| !seen.contains(n))
        {
            return err(span, format!("missing case '{}' in match", missing));
        }
        let Some(words) = gives else { return Ok(None) };
        let Some(settled) = self.settle_branches(span, &given, words)? else { return Ok(None) };
        for (i, _, _) in given.iter().filter(|(_, ty, _)| !ty.unknown() && *ty != settled) {
            if let Some(case) = cases.get_mut(*i) {
                self.settle_value(&mut case.body, &settled)?;
            }
        }
        self.escaping_region(span, &settled, words)?;
        Ok(Some(settled))
    }

    /// Checks an expression, records its type on the node, and returns it. A
    /// call that sends several values is rejected here: its values are
    /// separate, so only a list can receive them (see `value_types`).
    fn check_expr(&mut self, expr: &mut Expr) -> Result<Type> {
        let ty = self.check_expr_multi(expr)?;
        let Type::Results(types) = &ty else { return Ok(ty) };
        let what = match &expr.kind {
            ExprKind::Call { name, .. } => format!("'{}'", name),
            _ => "this call".to_string(),
        };
        err(
            expr.span,
            format!("{} sends {} values; receive them with 'bind {} = …'", what, types.len(), example_names(types.len())),
        )
    }

    /// `check_expr`, which also allows a call that sends several values.
    fn check_expr_multi(&mut self, expr: &mut Expr) -> Result<Type> {
        self.resolve_constructor(expr);
        self.resolve_empty_plan_b(expr);
        let ty = self.infer_expr(expr)?;
        expr.ty = Some(ty.clone());
        Ok(ty)
    }

    /// Checks the values of a list — a send, or the right side of a binding or
    /// an assignment — and returns the type of each. A single call that sends
    /// `wanted` values gives them all; otherwise every value gives its own, and
    /// there must be one per name.
    fn value_types(&mut self, values: &mut [Expr], wanted: usize, span: Span, unpacks: bool) -> Result<Vec<Type>> {
        let mut given = Vec::new();
        if let ([only], false) = (&mut *values, wanted == 1) {
            let ty = self.check_expr_multi(only)?;
            match ty {
                Type::Results(types) => given = types,
                // `bind a, b = pair`: the items of one tuple.
                Type::Tuple(items) if unpacks => {
                    if items.len() != wanted {
                        return err(only.span, format!("this tuple has {} items, but {} take them", items.len(), count(wanted, "name")));
                    }
                    given = items;
                }
                Type::Tuple(_) => {
                    return err(only.span, format!("expected {}, got one tuple; a tuple is one value, so send its items, as in '<- t.0, t.1'", count(wanted, "value")));
                }
                other => given.push(other),
            }
        } else {
            for value in values.iter_mut() {
                given.push(self.check_expr(value)?);
            }
        }
        if given.len() != wanted {
            let at = values.get(wanted).map_or(span, |extra| extra.span);
            return err(at, format!("expected {}, got {}", count(wanted, "value"), given.len()));
        }
        Ok(given)
    }

    /// Resolves the types written for the names a binding declares, through the
    /// parts of the tuples it unpacks.
    fn resolve_bound_types(&mut self, bound: &mut Bound) -> Result<()> {
        if let Some(annotated) = &mut bound.ty {
            *annotated = self.resolve_type(annotated, bound.ty_span.unwrap_or(bound.span), TypePlace::Body)?;
        }
        bound.parts.iter_mut().try_for_each(|part| self.resolve_bound_types(part))
    }

    /// Checks `(a, b)` in a binding, which takes the items of `actual`, the
    /// value at `at`, and adds the names it declares, with their types, to
    /// `declared`.
    fn unpack_bound(&mut self, bound: &mut Bound, actual: &Type, at: Span, declared: &mut Vec<(String, Span, Type)>) -> Result<()> {
        if bound.parts.is_empty() {
            let ty = match &bound.ty {
                Some(annotated) => {
                    if !fits_regions(actual, annotated) {
                        return err(at, format!("expected type {}, got {} for '{}'{}", annotated, actual, bound.name, mismatch_hint(actual, annotated)));
                    }
                    annotated.clone()
                }
                None if bound.name == DISCARD => actual.clone(),
                None => self.inferred_type(&bound.name, actual, at)?,
            };
            if bound.name == DISCARD && matches!(ty, Type::Or(..)) {
                return err(at, dropped_error(&ty));
            }
            bound.ty = Some(ty.clone());
            declared.push((bound.name.clone(), bound.span, ty));
            return Ok(());
        }
        let items = self.tuple_items(actual, bound.parts.len(), bound.span)?;
        bound.ty = Some(self.lasting(actual));
        for (part, item) in bound.parts.iter_mut().zip(&items) {
            self.unpack_bound(part, item, at, declared)?;
        }
        Ok(())
    }

    /// The items of a value of type `actual` that `( … )` at `at`, with `names`
    /// parts, unpacks.
    fn tuple_items(&self, actual: &Type, names: usize, at: Span) -> Result<Vec<Type>> {
        match actual {
            _ if actual.unknown() => Ok(vec![Type::Unknown; names]),
            Type::Tuple(items) if items.len() == names => Ok(items.clone()),
            Type::Tuple(items) => err(at, format!("this tuple has {} items, but '( … )' takes {}", items.len(), names)),
            other => err(at, format!("'( … )' takes the items of a tuple, but this value is {}{}", other, extract_hint(&[other]))),
        }
    }

    /// Checks one place an assignment writes to, recording the variables it
    /// assigns in `assigned`, and returns what it takes.
    fn check_target(&mut self, target: &mut Target, assigned: &mut HashMap<String, Span>) -> Result<TargetType> {
        match target {
            Target::Discard(_) => Ok(TargetType::Discard),
            Target::Place(place) => {
                // Each place takes one value, so a variable assigned twice
                // would quietly keep the last.
                if let ExprKind::Var(name) = &place.kind
                    && let Some(at) = assigned.insert(name.clone(), place.span)
                {
                    return err(place.span, format!("'{}' is already assigned at {}:{}", name, at.line, at.col));
                }
                Ok(TargetType::Place(self.check_assign_target(place)?))
            }
            Target::Tuple(parts, at) => {
                let at = *at;
                let parts = parts.iter_mut().map(|part| self.check_target(part, assigned)).collect::<Result<_>>()?;
                Ok(TargetType::Tuple(parts, at))
            }
        }
    }

    /// Checks that the items of `actual`, the value at `value_span`, fit the
    /// places `( … )` at `at` writes them to.
    fn unpack_target(&self, parts: &[TargetType], at: Span, actual: &Type, value_span: Span) -> Result<()> {
        let items = self.tuple_items(actual, parts.len(), at)?;
        for (part, item) in parts.iter().zip(&items) {
            match part {
                TargetType::Discard if matches!(item, Type::Or(..)) => return err(value_span, dropped_error(item)),
                TargetType::Discard => {}
                TargetType::Place(expected) => {
                    if !fits_regions(item, expected) {
                        return err(value_span, format!("expected type {}, got {}{}", expected, item, mismatch_hint(item, expected)));
                    }
                }
                TargetType::Tuple(inner, inner_at) => self.unpack_target(inner, *inner_at, item, value_span)?,
            }
        }
        Ok(())
    }

    /// Checks `total <- value`, which sets named results and keeps going. The
    /// names may be any of the task's results, in any order, each once.
    fn check_set_results(&mut self, span: Span, names: &[Binding], values: &mut [Expr]) -> Result<()> {
        let mut expected = Vec::new();
        let mut seen: HashMap<&str, Span> = HashMap::new();
        for (name, at) in names {
            if let Some(before) = seen.insert(name, *at) {
                return err(*at, format!("'{}' is already set at {}:{}", name, before.line, before.col));
            }
            let Some(info) = self.lookup(name).filter(|info| info.result) else {
                return err(*at, self.not_a_result(name));
            };
            expected.push(info.ty.clone());
        }
        let given = self.value_types(values, expected.len(), span, false)?;
        let forwarded = values.len() < expected.len();
        let list_span = values.first().map_or(span, |value| value.span);
        for (i, want) in expected.iter().enumerate() {
            let Some(actual) = given.get(i).cloned() else { continue };
            match values.get_mut(i).filter(|_| !forwarded) {
                Some(value) => self.coerce(value, want)?,
                None => fits_or_err(&actual, want, list_span, Given::Sent)?,
            }
        }
        // Set, so the results can be read from here on.
        for (name, _) in names {
            self.unset.remove(name);
        }
        Ok(())
    }

    /// Why `name <- value` doesn't name a result of this task.
    fn not_a_result(&self, name: &str) -> String {
        if self.lookup(name).is_some() {
            return format!("'{0}' isn't a result of this task; assign it with '{0} = …'", name);
        }
        match self.result_names.as_slice() {
            [] => format!("'{}' isn't a result of this task, which names none; a header names them, as in '-> total: int'", name),
            names => format!("'{}' isn't a result of this task; its results are {}", name, names.join(", ")),
        }
    }

    /// The type a `bind` or `hold` without an annotation gives `name`, from the
    /// type of its value.
    fn inferred_type(&self, name: &str, actual: &Type, at: Span) -> Result<Type> {
        // Text of no region in particular, and a shape or kind holding only such
        // text, lasts as long as the program: a value made of literals.
        let actual = &self.lasting(actual);
        match actual {
            Type::None => err(at, format!("cannot infer the type of '{}' from none; give it a type, such as '{}: maybe int'", name, name)),
            Type::Fail(error) => err(
                at,
                format!("cannot infer the type of '{}' from 'fail …'; give it a type, such as '{}: int ~ {}'", name, name, error),
            ),
            Type::Unit => err(at, "this expression has no value (its type is unit)"),
            other if unsettled(other) => err(
                at,
                format!("cannot infer the type of '{0}' from none or 'fail …' in its value; give it a type, such as '{0}: {1}'", name, settled_example(other)),
            ),
            other if other.regions().contains(&ANY_REGION) => {
                let example = other.map_regions(&mut |region| Some(if region == ANY_REGION { "r" } else { region }.to_string()));
                err(
                    at,
                    format!(
                        "cannot tell which region '{0}' lives in, since its value holds no pointers yet; give it a type, such as '{0}: {1}'",
                        name, example
                    ),
                )
            }
            other => Ok(other.clone()),
        }
    }

    /// `ty` with `ANY_REGION` removed from its text and from the shapes and
    /// kinds that hold text but no pointers: a value in no region holds only
    /// text that lasts as long as the program. (One holding pointers keeps it,
    /// since a variable of it must name a region.)
    fn lasting(&self, ty: &Type) -> Type {
        lasting(ty, &self.regional)
    }

    /// Rewrites a variant used as a value, `Empty`, or called, `Circle(1.0)`,
    /// into a `Construct` node. Variables can't share a variant's name, so a
    /// bare name is never ambiguous.
    fn resolve_constructor(&self, expr: &mut Expr) {
        let variant = |name: &str| {
            self.variants.get(name).filter(|_| self.sees(name)).map(|info| (info.kind.clone(), info.variant.tag.clone()))
        };
        let resolved = match &mut expr.kind {
            ExprKind::Var(name) if self.lookup(name).is_none() => variant(name).map(|(kind, tag)| (kind, tag, Vec::new())),
            ExprKind::Call { name, args, .. } => variant(name).map(|(kind, tag)| (kind, tag, std::mem::take(args))),
            _ => None,
        };
        if let Some((kind, tag, args)) = resolved {
            expr.kind = ExprKind::Construct { kind, tag, args };
        }
    }

    /// Rewrites `name {}` into an empty plan B when `name` is a variable rather
    /// than a shape: the parser reads both as a shape literal.
    fn resolve_empty_plan_b(&self, expr: &mut Expr) {
        let ExprKind::StructInit { name, fields, .. } = &mut expr.kind else { return };
        if !fields.is_empty() || (self.sees(name) && self.shapes.contains_key(name.as_str())) || self.lookup(name).is_none() {
            return;
        }
        let value = Expr::new(ExprKind::Var(std::mem::take(name)), expr.span);
        expr.kind = ExprKind::PlanB { value: Box::new(value), error: None, body: Vec::new() };
    }

    fn infer_expr(&mut self, expr: &mut Expr) -> Result<Type> {
        let span = expr.span;
        // A method call becomes the call of the task the method is, which the
        // rest of this checks as it checks any call.
        if matches!(expr.kind, ExprKind::MethodCall { .. }) {
            self.check_method_call(expr)?;
        }
        if matches!(expr.kind, ExprKind::Shared { .. }) {
            return self.check_shared_use(expr);
        }
        match &mut expr.kind {
            ExprKind::IntLit(_) => Ok(Type::Int),
            ExprKind::FloatLit(_) => Ok(Type::Float),
            ExprKind::BoolLit(_) => Ok(Type::Bool),
            ExprKind::TextLit(_) => Ok(Type::Text(None)),
            ExprKind::NoneLit => Ok(Type::None),
            // One the call it is written after doesn't take (a variant's, which
            // `settle_call_region` never sees).
            ExprKind::RegionAfter(_) => err(span, "a region is named after a call only when the call takes one last"),
            // One a call gives a `hold&` parameter is checked with the call's
            // arguments (see `check_argument`).
            ExprKind::Ref(_) => err(span, "'&' marks a place given to a 'hold&' parameter, as in 'f(&a)', or named by a reference, as in 'hold& y = &a'; anywhere else, write the value itself"),
            ExprKind::Shared { .. } => Err(Error::internal_at(span, "a shared field read as another value")),
            ExprKind::Var(name) => {
                let ty = self.lookup_or_err(name, span)?.ty.clone();
                if self.unset.contains(name.as_str()) {
                    return err(span, format!("'{}' might not be set here", name));
                }
                Ok(ty)
            }
            ExprKind::Field(base, field) => {
                // `.x` outside a method, named as it is written.
                if self.current_method.is_none() && matches!(&base.kind, ExprKind::Var(name) if name == THIS) {
                    return err(span, self.not_a_method(&format!("{}{}", MODULE_SEPARATOR, field)));
                }
                let base_ty = self.check_expr(base)?;
                self.field_type(&base_ty, field, span)
            }
            ExprKind::Index(base, index) => {
                let base_ty = self.check_expr(base)?;
                self.check_index(base, &base_ty, index, span)
            }
            ExprKind::Array(values) => self.check_array(values, span),
            ExprKind::Tuple(values) => self.check_tuple(values),
            ExprKind::Repeat { value, count } => self.check_repeat(value, count, span),
            ExprKind::Call { name, types, args } => {
                // `show`, `int(…)` and `float(…)` make no memory a program sees,
                // so nothing names a region after them.
                if let Some(at) = region_after(args)
                    && (name == "show" || number_cast(name).is_some())
                {
                    let what = if name == "show" { "'show' writes each value in a region of its own".to_string() } else { format!("'{}(…)' makes a number, which holds no memory", name) };
                    return err(at, format!("{}, so no region is named after it", what));
                }
                if name == "show" {
                    // `show` takes any number of values, written one after
                    // another with nothing between them; `show()` is a line of
                    // its own.
                    for arg in args.iter_mut() {
                        self.check_shown(arg)?;
                    }
                    return Ok(Type::Unit);
                }
                // `int(x)` and `float(x)`: a value turned into a number, as
                // `text(x) at r` turns one into text.
                if let Some(into) = number_cast(name) {
                    return self.check_number_cast(&into, types, args, span);
                }
                let info = self.tasks.get(name.as_str()).filter(|_| self.sees(name))
                    .ok_or_else(|| {
                        // `send (x)` written the old way reads as a call.
                        let hint = match name.as_str() {
                            "send" => "; a task sends its result with '<-'".to_string(),
                            "free" => "; Vela has no 'free': memory in a region is freed when the block declaring the region closes".to_string(),
                            _ if self.module_used(name).is_some() => format!("; '{0}' is a module, so name a task in it, as in '{0}.name(…)'", name),
                            _ => self.elsewhere(name),
                        };
                        Error::at(span, format!("unknown task '{}'{}", name, hint))
                    })?
                    .clone();
                // A task whose header has a mistake takes and sends nothing
                // known, so the call is only checked as far as its arguments
                // are values of their own (see `broken`).
                if self.broken.contains(name.as_str()) {
                    self.check_each(args);
                    return Ok(Type::Unknown);
                }
                self.settle_call_region(span, &format!("task '{}'", name), args, &info.params)?;
                if args.len() != info.params.len() {
                    return err(span, format!("task '{}' expects {}, got {}", name, count(info.params.len(), "argument"), args.len()));
                }
                self.check_call_args(span, name, types, args, &info)
            }
            ExprKind::Unary(op, e) => self.check_unary(*op, e, span),
            ExprKind::Construct { kind, tag, args } if self.broken.contains(kind.as_str()) => {
                // A kind whose own variants have a mistake holds nothing known
                // (see `broken`).
                self.check_each(args);
                Ok(Type::Unknown)
            }
            ExprKind::Construct { kind, tag, args } => {
                let variant = self.kinds.get(kind.as_str())
                    .and_then(|k| k.variants.iter().find(|v| v.tag == *tag))
                    .cloned()
                    .ok_or_else(|| Error::internal_at(span, format!("unknown variant '{}'", tag)))?;
                if let Some(at) = region_after(args) {
                    return err(at, format!("variant '{}' makes a value, not memory, so no region is named after it", tag));
                }
                if args.len() != variant.payload.len() {
                    return err(span, format!("variant '{}' expects {}, got {}", tag, count(variant.payload.len(), "argument"), args.len()));
                }
                let mut values: Vec<&mut Expr> = args.iter_mut().collect();
                let region = self.check_fields_of_value(&mut values, &variant.payload, None)?;
                Ok(Type::Named(kind.clone(), self.region_of_value(kind, region)))
            }
            ExprKind::New { ty, ty_span, value, region, region_span } => {
                **ty = self.resolve_type(ty, *ty_span, TypePlace::New)?;
                // `^int ~ E` means `(^int) ~ E`, so no written type could hold the pointer.
                if matches!(**ty, Type::Or(..)) {
                    return err(*ty_span, format!("'new' can't make a pointer to '{}'; extract the value first, or keep it in a shape's field", ty));
                }
                // The region written, or the block's default, which is kept on
                // the node so the code generator sees the region either way.
                let written = ty.to_string();
                let allocates = self.region_or_default(region.as_ref(), *region_span, || {
                    format!("'new' needs a region to allocate in, as in 'new {0}(…) at r' or 'new@r {0}(…)'", written)
                })?;
                *region = Some(allocates.clone());
                self.visible_region(&allocates, *region_span, true)?;
                // `new [T](…) at r` makes the slice itself, with its items in `r`.
                if let Type::Slice(items, _) = &**ty {
                    let items = self.in_region(items, &allocates);
                    let slice = Type::Slice(Box::new(items.clone()), Some(allocates));
                    self.check_slice_source(value, &items, &slice)?;
                    return Ok(slice);
                }
                let pointee = self.in_region(ty, &allocates);
                self.check_expr(value)?;
                self.coerce(value, &pointee)?;
                Ok(Type::Ptr(Box::new(pointee), Some(allocates)))
            }
            ExprKind::Deref(e) => {
                let ty = self.check_expr(e)?;
                if ty.unknown() {
                    return Ok(Type::Unknown);
                }
                if let Type::Maybe(inner) = &ty
                    && inner.ptr_inner().is_some()
                {
                    return err(span, format!("this pointer is a {}, which may be none; extract it first, as in 'when p is {{q}}' or 'p {{ … }}'", ty));
                }
                if let Type::Or(value, _) = &ty
                    && value.ptr_inner().is_some()
                {
                    return err(span, format!("this pointer is a {}, which may be an error; extract it first, as in 'p {{ … }}' or 'match'", ty));
                }
                let hint = if matches!(ty, Type::Slice(..)) { "; read an item of a slice with 'xs[i]'" } else { "" };
                ty.ptr_inner().cloned()
                    .ok_or_else(|| Error::at(span, format!("'^' reads the value a pointer points to, but this value is {}{}", ty, hint)))
            }
            ExprKind::StructInit { name, types, fields } => self.check_struct_init(span, name, types, fields),
            ExprKind::Range(..) => err(span, "a range can only be used in 'each' or to take part of an array or slice, as in 'xs[1..3]'"),
            ExprKind::Fail(error) => {
                let ty = self.check_expr(error)?;
                if matches!(ty, Type::Unit | Type::None | Type::Fail(_) | Type::Maybe(_) | Type::Or(..)) {
                    return err(error.span, format!("'fail' needs an error value, got {}", ty));
                }
                Ok(Type::Fail(Box::new(ty)))
            }
            ExprKind::PlanB { value, error, body } => self.check_plan_b(span, value, error.as_ref(), body),
            // A `match` or a `when` used as a value: its branches give it, and
            // one whose branches had a mistake of their own gives an unknown
            // type, so nothing after it complains twice (see `Type::Unknown`).
            ExprKind::Choice(stmt) => Ok(self.check_branching_value(stmt)?.unwrap_or(Type::Unknown)),
            ExprKind::Edit { items, op, args } => self.check_edit(span, items, *op, args),
            ExprKind::Scan { args, region, region_span } => self.check_scan(span, args, region, *region_span),
            ExprKind::Join { parts, region, region_span } => self.check_join(parts, region, *region_span),
            ExprKind::Binary(..) => self.check_binary_expr(expr),
            ExprKind::ToText { value, region, region_span } => self.check_to_text(value, region, *region_span),
            ExprKind::Copy { value, region, region_span } => self.check_copy(span, value, region.as_deref(), *region_span),
            // Only the checker creates `Wrap`, with its type already set.
            ExprKind::Wrap(_) => expr.ty.clone().ok_or_else(|| Error::internal_at(span, "a wrapped value has no type")),
            // `check_method_call` above has made this the call it is, unless the
            // value it is called on has no type it could work out, which is
            // what says which method this is (see `Type::Unknown`).
            ExprKind::MethodCall { value, args, .. } if value.ty.as_ref().is_some_and(Type::unknown) => {
                self.check_each(args);
                Ok(Type::Unknown)
            }
            ExprKind::MethodCall { name, .. } => Err(Error::internal_at(span, format!("the method call '{}' was not rewritten", name))),
        }
    }

    /// Checks a tuple literal, `(a, b)`, and returns its type: its items' types.
    fn check_tuple(&mut self, values: &mut [Expr]) -> Result<Type> {
        let mut items = Vec::new();
        for value in values.iter_mut() {
            let ty = self.check_expr(value)?;
            if let Some(what) = self.changeable_item(&ty) {
                return err(value.span, format!("a tuple's items never change, so it can't hold {}", what));
            }
            items.push(ty);
        }
        Ok(Type::Tuple(items))
    }

    /// Checks an array literal, `[a, b, c]`, and returns its type. Kept out of
    /// `infer_expr`, whose stack frame every level of nested code pays for.
    fn check_array(&mut self, values: &mut [Expr], span: Span) -> Result<Type> {
        let mut joined: Option<Type> = None;
        for value in values.iter_mut() {
            let ty = self.check_expr(value)?;
            joined = Some(match joined {
                None => ty,
                Some(before) => join_types(&before, &ty).ok_or_else(|| {
                    Error::at(value.span, format!("an array's values have one type, but this one is {} and the ones before it are {}", ty, before))
                })?,
            });
        }
        let items = joined.unwrap_or(Type::Unit);
        if items == Type::Unit {
            return err(span, "an array can't hold unit, which has no value");
        }
        // `none` alone, or `fail e`, becomes part of a type an annotation gives.
        if !unsettled(&items) {
            for value in values.iter_mut() {
                self.coerce(value, &items)?;
            }
        }
        let ty = Type::Array(Box::new(items), u64::try_from(values.len()).unwrap_or(u64::MAX));
        self.check_size(&ty, span)?;
        Ok(ty)
    }

    /// Checks `[value; count]` outside `new`, an array, and returns its type.
    fn check_repeat(&mut self, value: &mut Expr, count: &mut Expr, span: Span) -> Result<Type> {
        let items = self.check_expr(value)?;
        if items == Type::Unit {
            return err(value.span, "an array can't hold unit, which has no value");
        }
        let count_ty = self.check_expr(count)?;
        self.expect_type(&count_ty, &Type::Int, count.span)?;
        let Some(n) = constant_int(count) else {
            // The region of a slice's items is the one it is made in.
            let written = if unsettled(&items) { "T".to_string() } else { items.without_regions().to_string() };
            return err(
                count.span,
                format!("an array's count is known before the program runs, as in '[0; 8]'; for a count known only as it runs, make a slice in a region, as in 'new [{}]([…; n]) at r'", written),
            );
        };
        let Some(n) = u64::try_from(n).ok().filter(|n| *n > 0) else {
            return err(count.span, "an array holds at least one value, so its count is 1 or more");
        };
        let ty = Type::Array(Box::new(items), n);
        self.check_size(&ty, span)?;
        Ok(ty)
    }

    /// Checks `t[a..b]`, a part of text of `region`: the same bytes, so it is
    /// text of that region too. Text has no items to read one at a time.
    fn check_text_part(&mut self, region: &Option<String>, index: &mut Expr, span: Span) -> Result<Type> {
        let ExprKind::Range(from, to) = &mut index.kind else {
            return err(span, "text has no items to read one at a time; a part of it is 't[a..b]', as in 't[i..i + 1]' for the byte at 'i'");
        };
        for bound in [&mut **from, &mut **to] {
            let ty = self.check_expr(bound)?;
            self.expect_type(&ty, &Type::Int, bound.span)?;
        }
        let (start, end) = (constant_int(from), constant_int(to));
        for (value, at) in [(start, from.span), (end, to.span)] {
            if let Some(value) = value.filter(|value| *value < 0) {
                return err(at, format!("text bound {} is out of range: it is less than 0", value));
            }
        }
        if let (Some(start), Some(end)) = (start, end)
            && end < start
        {
            return err(index.span, format!("text {}..{} ends before it starts", start, end));
        }
        index.ty = Some(Type::Range(Box::new(Type::Int)));
        Ok(Type::Text(region.clone()))
    }

    /// Checks `xs[i]`, an item, or `xs[a..b]`, a slice, on `base` of type
    /// `base_ty`, already checked, and returns its type. A constant index or
    /// bound that is out of range for an array is an error here; the program
    /// checks the rest as it runs.
    fn check_index(&mut self, base: &Expr, base_ty: &Type, index: &mut Expr, span: Span) -> Result<Type> {
        if base_ty.unknown() {
            // Whether this reads an item or takes a slice depends on the type
            // the checker couldn't work out (see `Type::Unknown`).
            let checked = self.check_expr(index);
            self.keep_going(checked.map(|_| ()));
            return Ok(Type::Unknown);
        }
        if let Type::Text(region) = base_ty {
            return self.check_text_part(region, index, span);
        }
        let items = match base_ty {
            // `[none, none][i]`: an item that is only a literal can be wrapped
            // where a `maybe` or `T ~ E` is expected, but not one read out.
            Type::Array(items, _) if matches!(**items, Type::None | Type::Fail(_)) => {
                return err(span, "cannot tell the type of this item from none or 'fail …'; give the array a type first");
            }
            Type::Array(items, _) | Type::Slice(items, _) => (**items).clone(),
            Type::Ptr(inner, _) if matches!(**inner, Type::Array(..)) => {
                return err(span, format!("this value is a pointer, {}; index the array it points to, as in 'p^[i]'", base_ty));
            }
            other => {
                return err(span, format!("'[…]' reads an item of an array or slice, but this value is {}{}", other, extract_hint(&[other])));
            }
        };
        let count = match base_ty {
            Type::Array(_, count) => Some(*count),
            _ => None,
        };
        let out_of_range = |value: i64, at: Span, what: &str| -> Result<()> {
            let beyond = u64::try_from(value).map_or(true, |value| count.is_some_and(|count| value > count || (what == "index" && value == count)));
            if !beyond {
                return Ok(());
            }
            match count {
                Some(count) => err(at, format!("{} {} is out of range 0..{} for {}", what, value, count, base_ty)),
                None => err(at, format!("{} {} is out of range: it is less than 0", what, value)),
            }
        };
        let ExprKind::Range(from, to) = &mut index.kind else {
            let ty = self.check_expr(index)?;
            self.expect_type(&ty, &Type::Int, index.span)?;
            if let Some(value) = constant_int(index) {
                out_of_range(value, index.span, "index")?;
            }
            return Ok(items);
        };
        for bound in [&mut **from, &mut **to] {
            let ty = self.check_expr(bound)?;
            self.expect_type(&ty, &Type::Int, bound.span)?;
        }
        let (start, end) = (constant_int(from), constant_int(to));
        for (value, at) in [(start, from.span), (end, to.span)] {
            if let Some(value) = value {
                out_of_range(value, at, "slice bound")?;
            }
        }
        if let (Some(start), Some(end)) = (start, end)
            && end < start
        {
            return err(index.span, format!("slice {}..{} ends before it starts", start, end));
        }
        index.ty = Some(Type::Range(Box::new(Type::Int)));
        let region = match base_ty {
            Type::Slice(_, region) => region.clone(),
            _ => match place_region(base) {
                Some(region) => Some(region),
                None => {
                    return err(
                        span,
                        format!(
                            "a slice points into a region, but this array isn't in one; copy it into a slice, as in 'new [{}](xs) at r', or keep the array in a region with 'new {}(…) at r'",
                            items.without_regions(),
                            base_ty.without_regions()
                        ),
                    );
                }
            },
        };
        Ok(Type::Slice(Box::new(items), region))
    }

    /// Checks `items.add(…)` or `items.del(…)`, a change to a slice at `span`,
    /// and returns its type: `unit` for `add`, the item taken out for `del`.
    /// The slice must be in a place that can be assigned, since its length
    /// changes, and `add` needs to allocate in the slice's region, which may
    /// need more room.
    fn check_edit(&mut self, span: Span, items: &mut Expr, op: EditOp, args: &mut [Expr]) -> Result<Type> {
        let word = op.word();
        let ty = match &items.kind {
            ExprKind::Var(name) => {
                let name = name.clone();
                let info = self.lookup_or_err(&name, items.span)?;
                if info.result {
                    return err(items.span, format!("'{0}' is a result; set it with '{0} <- …'", name));
                }
                if !info.mutable && matches!(info.ty, Type::Slice(..)) {
                    if info.param || info.refers.is_some() {
                        let what = if info.param { "a parameter, which never changes" } else { "a 'bind&'" };
                        return err(items.span, format!("'{}' changes '{}', {}{}", word, name, what, param_hint(&name, info)));
                    }
                    return err(items.span, format!("'{}' changes '{}', which is immutable; keep the slice in a 'hold' variable to change it", word, name));
                }
                self.check_expr(items)?
            }
            ExprKind::Index(_, index) if matches!(index.kind, ExprKind::Range(..)) => {
                self.check_expr(items)?;
                return err(
                    items.span,
                    format!("'{}' changes the slice where it is kept, so it can't change a part taken of one; keep the part in a variable first, as in 'hold part = xs[a..b]'", word),
                );
            }
            // The place is checked as an assignment's is: the slice is written.
            ExprKind::Field(..) | ExprKind::Deref(_) | ExprKind::Index(..) => self.check_assign_target(items).map_err(|mut error| {
                // Said of this change rather than of an assignment.
                if let Some(rest) = error.message.strip_prefix("cannot assign to ") {
                    error.message = format!("'{}' can't change {}", word, rest);
                } else if error.message.starts_with("'len' can't be assigned") {
                    error.message = format!("'{}' changes a slice, but this value is int", word);
                }
                error
            })?,
            _ => {
                let ty = self.check_expr(items)?;
                if matches!(ty, Type::Slice(..)) {
                    return err(
                        items.span,
                        format!("'{}' changes the slice where it is kept, so it needs a variable, a field, or an item holding the slice, not a temporary value", word),
                    );
                }
                ty
            }
        };
        let (item, region) = match &ty {
            Type::Slice(item, region) => ((**item).clone(), region.clone()),
            // Whether this is a slice at all depends on a type the checker
            // couldn't work out; the arguments are still read (`Type::Unknown`).
            _ if ty.unknown() => {
                self.check_each(args);
                return Ok(Type::Unit);
            }
            Type::Array(..) => {
                return err(
                    span,
                    format!("an array's length is part of its type, so '{}' can't change it; make a slice in a region, as in 'new [{}](xs) at r'", word, item_type_example(&ty)),
                );
            }
            other => {
                return err(span, format!("'{}' changes a slice, but this value is {}{}", word, other, extract_hint(&[other])));
            }
        };
        let (index, value) = match (op, &mut *args) {
            (EditOp::Add, [value]) => (None, Some(value)),
            (EditOp::Add, [index, value]) => (Some(index), Some(value)),
            (EditOp::Del, []) => (None, None),
            (EditOp::Del, [index]) => (Some(index), None),
            (EditOp::Add, _) => {
                return err(span, format!("'add' takes the value, or an index and the value, as in 'xs.add(v)' or 'xs.add(i, v)', got {}", count(args.len(), "argument")));
            }
            (EditOp::Del, _) => {
                return err(span, format!("'del' takes an index, or nothing for the last item, as in 'xs.del(i)' or 'xs.del()', got {}", count(args.len(), "argument")));
            }
        };
        if let Some(index) = index {
            let index_ty = self.check_expr(index)?;
            self.expect_type(&index_ty, &Type::Int, index.span)?;
            if let Some(n) = constant_int(index)
                && n < 0
            {
                return err(index.span, format!("index {} is out of range: it is less than 0", n));
            }
        }
        let Some(value) = value else { return Ok(item) };
        self.check_expr(value)?;
        self.coerce(value, &item)?;
        // Adding may need more room, which comes from the slice's region.
        match region.as_deref() {
            Some(region) if region != ANY_REGION => match self.lookup(region) {
                Some(info) if info.ty == Type::Region && !info.allocates && region == THIS_WORD && self.current_method.is_some() => {
                    err(span, format!("adding to a slice may need more room in its region, and {}", self.cannot_allocate_in_this("add to it where the value is kept, outside the method")))
                }
                Some(info) if info.ty == Type::Region && !info.allocates => err(
                    span,
                    format!("adding to a slice may need more room in its region '{0}', which this task only names in its types; take it as a parameter, '{0}: region'", region),
                ),
                _ => self.visible_region(region, span, true).map(|()| Type::Unit),
            },
            _ => err(
                span,
                format!(
                    "adding to a slice may need more room in its region, but which region this slice is in isn't known here; keep it in a variable whose type names the region, as in 'hold xs: {}@r = …'",
                    Type::Slice(Box::new(item.without_regions()), None)
                ),
            ),
        }
    }

    /// Checks a read of input, at `span`, and returns its type:
    /// `scan(prompt) at r`, the line as `maybe text@r`, or `scan(prompt, int)`,
    /// a number read from that line as `int ~ InputError`. The type a typed
    /// read names is a type, not a value, so it is never checked as one.
    fn check_scan(&mut self, span: Span, args: &mut [Expr], region: &mut Option<String>, region_span: Span) -> Result<Type> {
        let (prompt, rest) = match args {
            [] => return err(span, SCAN_TAKES),
            [prompt, rest @ ..] => (prompt, rest),
        };
        // A type where the prompt goes, as in `scan(int)`: the prompt comes
        // first. A variable may be named after a builtin type, so one that is in
        // scope is read as the value it is, as anywhere else.
        if let ExprKind::Var(name) = &prompt.kind
            && Type::builtin(name).is_some()
            && self.lookup(name).is_none()
        {
            return err(prompt.span, format!("'scan' shows a text prompt first, as in 'scan(\"Age: \", {})'", name));
        }
        let shows = self.check_expr(prompt)?;
        if !matches!(shows, Type::Text(_)) && !shows.unknown() {
            return err(
                prompt.span,
                format!("'scan' shows a text prompt, as in 'scan(\"Name: \") at r', but this value is {}{}", shows, extract_hint(&[&shows])),
            );
        }
        let number = match rest {
            [] => None,
            [named] => Some(scanned_type(named)?),
            _ => return err(span, format!("{}, got {}", SCAN_TAKES, count(args.len(), "argument"))),
        };
        match number {
            // A line is text, which needs memory, so it goes in the region the
            // call names, or else the block's default.
            None => {
                let keeps = self.region_or_default(region.as_ref(), region_span, || {
                    "'scan' keeps the line it reads in a region, so it names one, as in 'scan(\"Name: \") at r' or 'scan(\"Name: \")@r'".to_string()
                })?;
                *region = Some(keeps.clone());
                self.visible_region(&keeps, region_span, true)?;
                Ok(Type::Maybe(Box::new(Type::Text(Some(keeps)))))
            }
            // A number needs no memory, so this form names no region and takes
            // no default either.
            Some(number) if region.is_some() => err(
                region_span,
                format!("a number needs no memory, so 'scan' reading one names no region: write 'scan(\"Age: \", {})'", number),
            ),
            Some(number) => Ok(Type::Or(Box::new(number), Box::new(Type::Named(INPUT_ERROR.to_string(), None)))),
        }
    }

    /// Checks one value given to `show`: a number, a bool, text, a `maybe` of
    /// one, a shape or kind with its own `str`, or a type a declaration takes
    /// that is `shown`.
    fn check_shown(&mut self, arg: &mut Expr) -> Result<()> {
        let copies = self.unsettled_copies.len();
        let types = self.unsettled_types.len();
        let ty = self.check_expr(arg)?;
        if ty.unknown() {
            return Ok(());
        }
        let shown = match &ty {
            Type::Maybe(inner) => inner.as_ref(),
            other => other,
        };
        if matches!(shown, Type::Int | Type::Float | Type::Bool | Type::Text(_)) || self.allows(Need::Shown, shown) {
            return Ok(());
        }
        // A shape or kind says how its values are written with its own `str`,
        // which `show` calls in a region of its own. The value is checked again
        // as part of that call, as a method call checks it twice.
        if self.has_own_str(&ty) {
            self.unsettled_copies.truncate(copies);
            self.unsettled_types.truncate(types);
            return self.show_with_str(arg);
        }
        if let Some(untold) = self.untold_param(&ty) {
            return err(arg.span, untold);
        }
        let hint = match &ty {
            Type::Param(_) => format!("; {} {}", ty, self.why_not(Need::Shown, &ty)),
            Type::Ptr(..) => "; to show the value it points to, write 'p^'".to_string(),
            Type::Array(..) | Type::Slice(..) => "; show its items one at a time, as in 'each v in xs'".to_string(),
            Type::Tuple(_) => "; show its items one at a time, as in 'show(t.0)'".to_string(),
            // A `maybe` of a shape or kind: its `str` needs a value, and there
            // is none to call it on until the `maybe` is opened.
            Type::Maybe(inner) if self.has_own_str(inner) => {
                format!("; a shape or kind is written by its own 'str', which needs a value, so open the {} first, as in 'when m is {{value}}'", ty)
            }
            // A `maybe` of a value that can fail: two to open, the `maybe` first.
            Type::Maybe(inner) if matches!(inner.as_ref(), Type::Or(..)) => extract_hint(&[&ty]),
            Type::Or(..) => extract_hint(&[&ty]),
            Type::Unit => "; this has no value".to_string(),
            _ => self.no_own_str(&ty),
        };
        err(arg.span, format!("show expects int, float, bool, text, or a shape or kind with its own 'str'{}", hint))
    }

    /// Whether values of type `ty` are written by a method `str` of their own.
    fn has_own_str(&self, ty: &Type) -> bool {
        self.owner_of(ty).is_some_and(|owner| self.tasks.contains_key(&method_name(&owner, STR)))
    }

    /// The end of a message about a shape or kind that has no `str` of its own,
    /// and nothing for any other type.
    fn no_own_str(&self, ty: &Type) -> String {
        let Some(owner) = self.owner_of(ty).filter(|owner| self.shapes.contains_key(owner) || self.kinds.contains_key(owner)) else {
            return String::new();
        };
        let what = if self.kinds.contains_key(&owner) { "kind" } else { "shape" };
        format!(
            "; {} '{}' has no method 'str', which says how its values are written; give it one, written 'str(r: region) -> text@r'",
            what,
            unqualified(&owner)
        )
    }

    /// Rewrites a value given to `show` that has its own `str` into the call of
    /// that method, which writes the value into the region `show` makes for the
    /// line and frees when it ends (see `codegen::gen_show`).
    fn show_with_str(&mut self, arg: &mut Expr) -> Result<()> {
        let span = arg.span;
        if self.lookup(SHOWN).is_none() {
            self.define_region(SHOWN, span, true)?;
        }
        let value = std::mem::replace(arg, Expr::new(ExprKind::NoneLit, span));
        arg.kind = ExprKind::MethodCall {
            value: Box::new(value),
            name: STR.to_string(),
            args: vec![Expr::new(ExprKind::Var(SHOWN.to_string()), span)],
            dotted: None,
        };
        self.check_expr(arg)?;
        Ok(())
    }

    /// Checks `a + b at region` or `"n = {n}" at region`, text joined into
    /// `region`, and returns its type,
    /// `text@region`. Every part is text, of any region or lasting.
    #[inline(never)]
    fn check_join(&mut self, parts: &mut [Expr], region: &mut String, region_span: Span) -> Result<Type> {
        // Text holding values that no `at r` followed (see
        // `Parser::parse_interpolated`) goes in the block's default region.
        if let Some(field) = &self.in_default {
            return err(
                parts.first().map_or(region_span, |part| part.span),
                format!("joined text, or text holding values, is made while the program runs, so field '{}' can't fall back on it; its default is text written whole, as in '\"n\"'", field),
            );
        }
        if region.is_empty() {
            *region = self.region_or_default(None, region_span, || {
                "text holding values is new text, so it names the region it goes in, as in '\"n = {n}\" at r' or '\"n = {n}\"@r'".to_string()
            })?;
        }
        let region = region.as_str();
        let mut numbers = true;
        let mut other = None;
        // A part that is a call wanting a region fails, and when every other
        // part is a number, the `at r` it was written for is a number chain's.
        let wanted = std::mem::take(&mut self.region_wanted);
        let mut failed = None;
        for part in parts.iter_mut() {
            let ty = match self.check_expr(part) {
                Ok(ty) => ty,
                Err(error) if self.region_wanted && failed.is_none() => {
                    failed = Some(error);
                    continue;
                }
                Err(error) => {
                    self.region_wanted = wanted;
                    return Err(error);
                }
            };
            numbers &= ty.is_numeric();
            if !matches!(ty, Type::Text(_)) && other.is_none() && !ty.unknown() {
                other = Some((ty, part.span));
            }
        }
        self.region_wanted = wanted;
        if let Some(error) = failed {
            if numbers && other.is_some() {
                return err(
                    region_span,
                    format!("numbers are added without a region, so 'at {0}' after '+' names the region of none of them; a call that takes its region last names it with '@' right after it, as in 'f(x)@{0}'", region),
                );
            }
            return Err(error);
        }
        if numbers {
            return err(region_span, format!("numbers are added without a region; 'at {}' after '+' is for joining text", region));
        }
        if let Some((ty, at)) = other {
            return err(at, format!("'+' joins text, but this value is {}{}", ty, self.to_text_hint(&ty)));
        }
        self.visible_region(region, region_span, true)?;
        // A part that is text turned into text ('"{name}"', or 't.str() at r'
        // written in a chain) is joined as it is: the join copies its bytes into
        // the region, so copying them there first would be for nothing.
        for part in parts.iter_mut() {
            let ExprKind::ToText { value, .. } = &part.kind else { continue };
            if matches!(value.ty, Some(Type::Text(_))) {
                let ExprKind::ToText { value, .. } = std::mem::replace(&mut part.kind, ExprKind::NoneLit) else { continue };
                *part = *value;
            }
        }
        Ok(Type::Text(Some(region.to_string())))
    }

    /// The end of a message about a value of type `ty` where text is wanted: how
    /// to turn it into text, when it can be.
    fn to_text_hint(&self, ty: &Type) -> String {
        // This hint is for text being joined, which goes in a region, so the
        // parentheses `fail` needs are followed by one.
        if matches!(ty, Type::Fail(_)) {
            return "; 'fail' takes only the value right after it, so write 'fail (a + b)@r'".to_string();
        }
        if self.has_builtin_str(ty) || self.has_own_str(ty) {
            return "; turn it into text first, as in 'n.str() at r'".to_string();
        }
        extract_hint(&[ty])
    }

    /// Checks `value.copy()`, at `span`, into `region` when one is written, and
    /// returns its type: the value's, in that region. Without one, the copy is
    /// of `ANY_REGION` and waits for the place it goes to settle its region.
    fn check_copy(&mut self, span: Span, value: &mut Expr, region: Option<&str>, region_span: Span) -> Result<Type> {
        let ty = self.check_expr(value)?;
        if ty.unknown() {
            return Ok(Type::Unknown);
        }
        if unsettled(&ty) {
            return err(value.span, format!("cannot tell the type of this value from none or 'fail …', so it can't be copied; give it a type first, as in 'x: {}'", settled_example(&ty)));
        }
        if !holds_pointers(&ty, &self.regional) && !holds_text(&ty, &self.takes_region) && ty.params().is_empty() {
            let what = match ty {
                Type::Unit => "this has no value".to_string(),
                _ => format!("this value is {}, which holds no text, slice, or pointer; use it as it is", ty),
            };
            return err(span, format!("'copy' copies what a value holds in a region, but {}", what));
        }
        let bare = ty.without_regions();
        match region {
            Some(region) => {
                self.visible_region(region, region_span, true)?;
                Ok(self.in_region(&bare, region))
            }
            None => {
                self.unsettled_copies.push(span);
                Ok(self.in_region(&bare, ANY_REGION))
            }
        }
    }

    /// Gives a `copy()` that `expr` is the region of `expected`, the type of the
    /// place it goes, when that type names one region; otherwise the copy stays
    /// unsettled, which its statement reports. Copies in an array, shape, or
    /// variant literal, and in a value becoming a `maybe` or a `T ~ E`, go
    /// where their part of `expected` does.
    fn settle_copies(&mut self, expr: &mut Expr, expected: &Type) -> Result<()> {
        match (&mut expr.kind, expected) {
            (ExprKind::Copy { region: region @ None, .. }, _) => {
                let mut regions = expected.regions();
                regions.sort_unstable();
                regions.dedup();
                match regions.as_slice() {
                    [] => err(
                        expr.span,
                        format!("'copy' makes its copy in a region, but a {} here lasts as long as the program, as a literal does; give the place a region, as in '{}'", expected, expected.map_regions_of_names(&mut |_| Some("r".to_string()))),
                    ),
                    [one] if *one != ANY_REGION => {
                        let one = one.to_string();
                        self.visible_region(&one, expr.span, true)?;
                        if let Some(at) = self.unsettled_copies.iter().rposition(|at| *at == expr.span) {
                            self.unsettled_copies.remove(at);
                        }
                        *region = Some(one);
                        expr.ty = Some(expected.clone());
                        Ok(())
                    }
                    _ => Ok(()),
                }
            }
            (ExprKind::Array(values), Type::Array(items, _)) => values.iter_mut().try_for_each(|value| self.settle_copies(value, items)),
            (ExprKind::Repeat { value, .. }, Type::Array(items, _)) => self.settle_copies(value, items),
            (ExprKind::Tuple(values), Type::Tuple(items)) => values.iter_mut().zip(items).try_for_each(|(value, item)| self.settle_copies(value, item)),
            (ExprKind::Wrap(inner), Type::Maybe(value) | Type::Or(value, _)) => self.settle_copies(inner, value),
            (ExprKind::StructInit { name, fields, .. }, Type::Named(_, Some(region))) => {
                let Some(shape) = self.shapes.get(name.as_str()).cloned() else { return Ok(()) };
                fields.iter_mut().try_for_each(|field| match shape.fields.iter().find(|f| f.name == field.name) {
                    Some(declared) => self.settle_copies(&mut field.value, &self.in_region(&declared.ty, region)),
                    None => Ok(()),
                })
            }
            (ExprKind::Construct { kind, tag, args }, Type::Named(_, Some(region))) => {
                let Some(variant) = self.kinds.get(kind.as_str()).and_then(|k| k.variants.iter().find(|v| v.tag == *tag)).cloned() else { return Ok(()) };
                args.iter_mut().zip(&variant.payload).try_for_each(|(arg, field)| self.settle_copies(arg, &self.in_region(&field.ty, region)))
            }
            _ => Ok(()),
        }
    }

    /// Checks the value of `new [T](value) at r`, which gives the slice its
    /// items, of type `items` in `r`: `[v; n]`, whose count may be known only as
    /// the program runs, or an array or slice whose items are copied.
    fn check_slice_source(&mut self, value: &mut Expr, items: &Type, slice: &Type) -> Result<()> {
        if let ExprKind::Repeat { value: item, count } = &mut value.kind {
            self.check_expr(item)?;
            self.coerce(item, items)?;
            let count_ty = self.check_expr(count)?;
            self.expect_type(&count_ty, &Type::Int, count.span)?;
            if let Some(n) = constant_int(count)
                && n < 0
            {
                return err(count.span, format!("count {} is negative", n));
            }
            value.ty = Some(slice.clone());
            return Ok(());
        }
        // `new [T]()`: no items yet.
        if matches!(&value.kind, ExprKind::Array(values) if values.is_empty()) {
            value.ty = Some(slice.clone());
            return Ok(());
        }
        let ty = self.check_expr(value)?;
        match &ty {
            Type::Array(_, count) => self.coerce(value, &Type::Array(Box::new(items.clone()), *count)),
            Type::Slice(source, _) if fits_regions(source, items) => Ok(()),
            Type::Slice(source, _) => {
                err(value.span, format!("expected type {}, got {}{}", slice, ty, mismatch_hint(source, items)))
            }
            _ if ty.unknown() => Ok(()),
            other => err(
                value.span,
                format!("'new [{}](…)' makes a slice from '[value; count]', an array, or a slice, but this value is {}", items, other),
            ),
        }
    }

    /// Checks a plan B, `value { body }`, and returns the type it gives: the
    /// type inside `value`.
    fn check_plan_b(&mut self, span: Span, value: &mut Expr, error: Option<&Binding>, body: &mut Block) -> Result<Type> {
        let found = self.check_expr(value);
        let value_ty = self.or_unknown(found);
        // Whether the value needs a plan B at all depends on the type the
        // checker couldn't work out, so the fallback is read for mistakes of
        // its own and nothing more (see `Type::Unknown`).
        let (extracted, error_ty) = match &value_ty {
            Type::Maybe(inner) => ((**inner).clone(), None),
            Type::Or(ok, fail) => ((**ok).clone(), Some((**fail).clone())),
            _ if value_ty.unknown() => (Type::Unknown, Some(Type::Unknown)),
            other => {
                return err(value.span, format!("this value is {}, not a maybe or a 'T ~ E', so it needs no plan B", other));
            }
        };
        let mut bindings = Vec::new();
        if let Some((name, at)) = error {
            let Some(error_ty) = error_ty else {
                return err(*at, format!("a {} has no error to name; 'failure e' names the error of a 'T ~ E'", value_ty));
            };
            if self.variant_in_scope(name).is_some() {
                return err(*at, format!("'{}' is a variant; to send an error, write '<- fail {}'", name, name));
            }
            bindings.push((name.clone(), *at, error_ty, false));
        }
        // The block runs only when there is nothing to extract, so a result it
        // sets isn't set for certain.
        let entry = self.unset.clone();
        self.push_scope();
        let result = bindings
            .into_iter()
            .filter(|(name, _, _, _)| name != DISCARD)
            .try_for_each(|(name, at, ty, mutable)| self.define(&name, ty, mutable, at))
            .and_then(|()| self.check_plan_b_body(span, &extracted, body));
        self.pop_scope();
        self.unset = entry;
        let Some((settled, at)) = result? else { return Ok(extracted) };
        // The fallback's regions must outlive the plan B: a region declared in
        // its block is freed when it ends.
        if let Some(region) = settled.regions().into_iter().find(|region| *region != ANY_REGION && self.lookup(region).is_none()) {
            return err(at, format!("this value is in region '{}', which is freed when the plan B ends, so it can't be the plan B's value", region));
        }
        // A plan B is a place the value goes, so a call whose types it settled
        // (see `check_plan_b_body`) learns them from it.
        if !value_ty.params().is_empty() {
            let told = match &value_ty {
                Type::Maybe(_) => Type::Maybe(Box::new(settled.clone())),
                Type::Or(_, error) => Type::Or(Box::new(settled.clone()), error.clone()),
                other => other.clone(),
            };
            self.settle_types(value, &told)?;
        }
        Ok(settled)
    }

    /// Checks a plan B's statements: the last line gives the value, unless every
    /// path through the block sends, or there is no value to give (`unit ~ E`).
    ///
    /// Returns the plan B's type, and the last line's position, when the last line
    /// settles it: a value holding no pointers (of `ANY_REGION`, such as
    /// `first(none)`) has nothing to extract that lives anywhere, so the plan B
    /// takes the fallback's regions.
    fn check_plan_b_body(&mut self, span: Span, extracted: &Type, body: &mut Block) -> Result<Option<(Type, Span)>> {
        if *extracted == Type::Unit {
            return self.check_block(body).map(|()| None);
        }
        // Whether the plan B has to give a value depends on the type the checker
        // couldn't work out, so its last line is read as a value and nothing is
        // asked of it (see `Type::Unknown`).
        if extracted.unknown() {
            let Some((last, rest)) = body.split_last_mut() else { return Ok(None) };
            self.check_block(rest)?;
            if let StmtKind::Expr(e) = &mut last.kind {
                self.check_expr(e)?;
                return Ok(None);
            }
            self.check_stmt(last)?;
            return Ok(None);
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
            // A plan B on a call that isn't told which type it is: the
            // fallback's type is the one to use, since it is where the value
            // goes (see `settle_types`).
            if extracted.params().iter().any(|name| self.type_param(name).is_none()) {
                let mut given = HashMap::new();
                self.match_params(extracted, &ty, &mut given);
                let settled = extracted.map_params(&mut |name| given.get(name).cloned());
                if settled != *extracted {
                    return self.coerce(e, &settled).map(|()| Some((settled, e.span)));
                }
            }
            if extracted.regions().contains(&ANY_REGION) {
                let mut regions = HashMap::new();
                self.match_regions(extracted, &ty, &mut regions);
                let settled = extracted.map_regions(&mut |region| Some(regions.get(region).cloned().unwrap_or_else(|| region.to_string())));
                if settled != *extracted {
                    return self.coerce(e, &settled).map(|()| Some((settled, e.span)));
                }
            }
            return self.coerce(e, extracted).map(|()| None);
        }
        // A `when` or a `match` on the last line gives the value through its own
        // branches, as it does in the block of a case (see `value_block_tail`).
        if gives_value(last) {
            self.check_branching_value(last)?;
            return self.settle_value(body, extracted).map(|()| None);
        }
        self.check_stmt(last)?;
        if !always_sends(body) {
            return err(span, format!("a plan B must end with a value of type {}, or send with '<-' on every path", extracted));
        }
        Ok(None)
    }

    fn check_unary(&mut self, op: UnaryOp, e: &mut Expr, span: Span) -> Result<Type> {
        let ty = self.check_expr(e)?;
        if ty.unknown() {
            return Ok(Type::Unknown);
        }
        match op {
            UnaryOp::Neg => {
                if !ty.is_numeric() {
                    return err(span, format!("negation requires a number, got {}{}", ty, extract_hint(&[&ty])));
                }
                if let Some(value) = constant_int(e) {
                    fits_int(-i128::from(value), span)?;
                }
                Ok(ty)
            }
            UnaryOp::Not => {
                self.expect_type(&ty, &Type::Bool, e.span)?;
                Ok(Type::Bool)
            }
        }
    }

    /// Checks a binary operation, and returns its type. `a + b` on text is a
    /// join, which makes new text and so needs a region: the block's default,
    /// since a written one is a `Join` from the parser. That replaces the
    /// expression, so this takes the whole node.
    fn check_binary_expr(&mut self, expr: &mut Expr) -> Result<Type> {
        let span = expr.span;
        let ExprKind::Binary(op, l, r) = &mut expr.kind else {
            return Err(Error::internal_at(span, "a binary operation that is not one"));
        };
        let op = *op;
        let lt = self.check_expr(l)?;
        let rt = self.check_expr(r)?;
        // With one operand's type unknown, the operation's own type is unknown
        // too: a comparison is still a bool, but what it compares isn't checked
        // (see `Type::Unknown`).
        if lt.unknown() || rt.unknown() {
            return Ok(match op {
                BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge | BinaryOp::And | BinaryOp::Or => Type::Bool,
                _ => Type::Unknown,
            });
        }
        if op == BinaryOp::Add && (matches!(lt, Type::Text(_)) || matches!(rt, Type::Text(_))) {
            if let Some(other) = [&lt, &rt].into_iter().find(|ty| !matches!(ty, Type::Text(_))) {
                return err(span, format!("'+' joins text to text, but the other value is {}{}", other, self.to_text_hint(other)));
            }
            let region = self.region_or_default(None, span, || JOIN_NEEDS_REGION.to_string())?;
            return self.join_into(expr, region, span);
        }
        self.check_binary(op, l, r, span, &lt, &rt)
    }

    /// Replaces `a + b` on text with the join it is, in `region`, and returns its
    /// type. The parts are checked already, so this only settles the region and
    /// splices the parts of a join of the same region, which a chain of `+` makes
    /// one level at a time (see `check_join`).
    fn join_into(&mut self, expr: &mut Expr, region: String, span: Span) -> Result<Type> {
        self.visible_region(&region, span, true)?;
        let taken = std::mem::replace(&mut expr.kind, ExprKind::NoneLit);
        let ExprKind::Binary(_, l, r) = taken else {
            return Err(Error::internal_at(span, "a join that is no binary operation"));
        };
        let mut parts = Vec::new();
        for side in [*l, *r] {
            match side.kind {
                ExprKind::Join { parts: inner, region: theirs, .. } if theirs == region => parts.extend(inner),
                // Text turned into text is joined as it is, as `check_join` does
                // with the parts of a written join.
                ExprKind::ToText { value, .. } if matches!(value.ty, Some(Type::Text(_))) => parts.push(*value),
                _ => parts.push(side),
            }
        }
        expr.kind = ExprKind::Join { parts, region: region.clone(), region_span: span };
        Ok(Type::Text(Some(region)))
    }

    fn check_binary(&mut self, op: BinaryOp, l: &mut Expr, r: &mut Expr, span: Span, lt: &Type, rt: &Type) -> Result<Type> {
        let (lt, rt) = (lt.clone(), rt.clone());
        match op {
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => {
                if !lt.is_numeric() || !rt.is_numeric() {
                    if let Some(untold) = self.untold_param(&lt).or_else(|| self.untold_param(&rt)) {
                        return err(span, untold);
                    }
                    // No word asks a type for arithmetic (see `Need`), so a
                    // task that adds its values is written for numbers.
                    if let Some(name) = [&lt, &rt].iter().find_map(|ty| match ty {
                        Type::Param(name) => Some(name),
                        _ => None,
                    }) {
                        return err(
                            span,
                            format!(
                                "arithmetic requires numeric operands; '{}' is a type this task takes, and no word asks a type for arithmetic, so a task that does it takes numbers ('int' or 'float') rather than a type it is given",
                                name
                            ),
                        );
                    }
                    return err(span, format!("arithmetic requires numeric operands{}", extract_hint(&[&lt, &rt])));
                }
                // An int doesn't become a float on its own: the writer says so
                // with `float(n)`, as every other place a number goes (a
                // binding, an argument, a field, an item) already asks.
                if lt != rt {
                    let at = if lt == Type::Int { l.span } else { r.span };
                    return err(
                        at,
                        "an int and a float don't mix in arithmetic: turn one into the other first, as in 'float(n) + 2.5' (or 'n + int(2.5)', which truncates)",
                    );
                }
                if lt == Type::Float {
                    return Ok(Type::Float);
                }
                check_constant_arithmetic(op, l, r, span)?;
                Ok(Type::Int)
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
                // Comparing stores nothing, so pointers into different regions
                // compare (and are never equal), and so do texts in any regions.
                let pointers = matches!((&lt, &rt), (Type::Ptr(..), Type::Ptr(..))) && lt.without_regions() == rt.without_regions();
                let texts = matches!((&lt, &rt), (Type::Text(_), Type::Text(_)));
                let tuples = matches!((&lt, &rt), (Type::Tuple(_), Type::Tuple(_))) && lt.without_regions() == rt.without_regions();
                if !pointers && !texts && !tuples {
                    self.expect_type(&rt, &lt, r.span)?;
                }
                if !comparable(&lt) && !self.allows(Need::Same, &lt) {
                    if let Some(untold) = self.untold_param(&lt).or_else(|| self.untold_param(&rt)) {
                        return err(span, untold);
                    }
                    let symbol = if op == BinaryOp::Eq { "==" } else { "!=" };
                    let hint = match &lt {
                        Type::Tuple(_) => "; tuples compare item by item when every item is a number, a bool, text, or a tuple of them".to_string(),
                        Type::Param(_) => format!("; it {}", self.why_not(Need::Same, &lt)),
                        _ => String::new(),
                    };
                    return err(span, format!("'{}' can't compare values of type {}{}", symbol, lt, hint));
                }
                Ok(Type::Bool)
            }
            BinaryOp::Lt | BinaryOp::Gt | BinaryOp::Le | BinaryOp::Ge => {
                let symbol = match op {
                    BinaryOp::Lt => "<",
                    BinaryOp::Gt => ">",
                    BinaryOp::Le => "<=",
                    _ => ">=",
                };
                if !self.allows(Need::Ordered, &lt) {
                    if let Some(untold) = self.untold_param(&lt).or_else(|| self.untold_param(&rt)) {
                        return err(span, untold);
                    }
                    if let Type::Param(_) = &lt {
                        return err(span, format!("'{}' compares numbers or text; {} {}", symbol, lt, self.why_not(Need::Ordered, &lt)));
                    }
                    if matches!(lt, Type::Tuple(_)) {
                        return err(span, format!("'{}' compares numbers or text; a tuple is only compared with '==' and '!=', or item by item, as in 't.0 < u.0'", symbol));
                    }
                    return err(span, format!("'{}' compares numbers or text, not {}{}", symbol, lt, extract_hint(&[&lt])));
                }
                // Comparing stores nothing, so texts in any regions compare.
                if !matches!((&lt, &rt), (Type::Text(_), Type::Text(_))) {
                    self.expect_type(&rt, &lt, r.span)?;
                }
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
        // Nothing is known about what the value holds: its type couldn't be
        // worked out, or the shape it is of has a mistake in its own fields
        // (see `Type::Unknown` and `broken`).
        if base.unknown() || self.owner_of(base).is_some_and(|owner| self.broken.contains(&owner)) {
            return Ok(Type::Unknown);
        }
        match base {
            // A shape given a type this declaration takes: its field's type is
            // the shape's with the types given in place of the names it takes.
            Type::Applied(applied) => {
                let Some(shape) = self.shapes.get(&applied.name) else {
                    return err(span, format!("type {} has no fields", base));
                };
                let Some(found) = shape.fields.iter().find(|f| f.name == field) else {
                    return err(span, format!("shape '{}' has no field '{}'", unqualified(&applied.name), field));
                };
                if found.private && module_of(&applied.name) != self.current_module {
                    return err(span, format!("'{}' is a private field of '{}', which only its own file can use", field, unqualified(&applied.name)));
                }
                let given: HashMap<&str, &Type> = shape.types.iter().map(|param| param.name.as_str()).zip(&applied.types).collect();
                let ty = found.ty.map_params(&mut |name| given.get(name).map(|ty| (*ty).clone()));
                Ok(self.field_in(&ty, &applied.region))
            }
            Type::Named(name, region) => {
                let Some(shape) = self.shapes.get(name) else {
                    return err(span, format!("'{}' is a kind and has no fields; use match to read its values", name));
                };
                for f in &shape.fields {
                    if f.name == field {
                        if f.private && module_of(name) != self.current_module {
                            return err(span, format!("'{}' is a private field of '{}', which only its own file can use", field, name));
                        }
                        return Ok(self.field_in(&f.ty, region));
                    }
                }
                if shape.shared.iter().any(|member| member.field.name == field) {
                    return err(
                        span,
                        format!("'{0}' is a shared field of '{1}', one value for the whole program rather than part of each value, so it is read as '{1}.{0}'", field, name),
                    );
                }
                err(span, format!("shape '{}' has no field '{}'", name, field))
            }
            Type::Tuple(items) => match field.parse::<usize>() {
                Ok(n) => items.get(n).cloned().ok_or_else(|| {
                    Error::at(span, format!("this tuple has {} items, 't.0' to 't.{}', so it has no item {}", items.len(), items.len() - 1, n))
                }),
                Err(_) => err(span, format!("type {} has no field '{}'; a tuple's items are read by position, as in 't.0'", base, field)),
            },
            _ if field.parse::<u64>().is_ok() => {
                err(span, format!("'.{}' reads an item of a tuple, but this value is {}{}", field, base, extract_hint(&[base])))
            }
            Type::Array(..) | Type::Slice(..) | Type::Text(_) if field == LEN => Ok(Type::Int),
            Type::Text(_) => err(span, format!("type {} has no field '{}'; how many bytes it has is '.len'", base, field)),
            Type::Array(..) | Type::Slice(..) => {
                err(span, format!("type {} has no field '{}'; its number of items is '.len'", base, field))
            }
            Type::Ptr(..) => err(span, format!("type {} has no fields; dereference it first, as in 'p^.{}'", base, field)),
            Type::Maybe(_) | Type::Or(..) => {
                err(span, format!("type {} has no fields; extract its value first, as in 'when x is {{value}}'", base))
            }
            _ => err(span, format!("type {} has no fields", base)),
        }
    }

    /// A field's or payload's declared type `ty`, in a value of region `region`
    /// (none for a value that holds no pointers).
    fn field_in(&self, ty: &Type, region: &Option<String>) -> Type {
        match region {
            Some(region) => self.in_region(ty, region),
            None => ty.clone(),
        }
    }

    /// The region of a value of shape or kind `name` whose fields' values are
    /// in `region` (see `check_fields_of_value`): none for a type that lives in
    /// no region, and none for a value holding only text that lasts as long as
    /// the program, which is how a type holding text is written without a
    /// region.
    fn region_of_value(&self, name: &str, region: String) -> Option<String> {
        if self.regional.contains(name) {
            return Some(region);
        }
        (self.takes_region.contains(name) && region != ANY_REGION).then_some(region)
    }

    /// Checks the values given for a shape's fields or a variant's payload,
    /// `declared` in the same order, and returns the region of the value they
    /// make: the region of the pointers among them, or `ANY_REGION` when they
    /// hold none. Every value must then fit its field in that region. With
    /// `filled`, the values from its index on are what a shape's fields fall
    /// back on, written in its module, `filled`'s other half, and read there,
    /// which may give what the literal's own can't, such as a private field.
    fn check_fields_of_value(&mut self, values: &mut [&mut Expr], declared: &[Field], filled: Option<(usize, &str)>) -> Result<String> {
        const OWN: &str = "";
        let mut regions = HashMap::new();
        // A literal nests in a literal, so this loop is kept small, as the
        // stack each level takes (see `check_in_module`).
        let from = filled.map_or(usize::MAX, |(from, _)| from);
        for (at, (value, field)) in values.iter_mut().zip(declared).enumerate() {
            let ty = if at < from { self.check_expr(value) } else { self.check_in_module(value, filled) }?;
            self.match_regions(&self.in_region(&field.ty, OWN), &ty, &mut regions);
        }
        let region = regions.remove(OWN).unwrap_or_else(|| ANY_REGION.to_string());
        for (value, field) in values.iter_mut().zip(declared) {
            self.coerce(value, &self.in_region(&field.ty, &region))?;
        }
        Ok(region)
    }

    /// Checks a value filled into a literal, as the module `filled` names reads
    /// it (see `check_fields_of_value`).
    #[inline(never)]
    fn check_in_module(&mut self, value: &mut Expr, filled: Option<(usize, &str)>) -> Result<Type> {
        let Some((_, module)) = filled else { return self.check_expr(value) };
        let outer = std::mem::replace(&mut self.current_module, module.to_string());
        let checked = self.check_expr(value);
        self.current_module = outer;
        checked
    }

    /// The shape or kind whose methods a value of type `ty` has: its name, with
    /// a shape made for a set of types read as the shape itself (see `unmade`),
    /// since a made shape has the methods the shape it was made from has.
    fn owner_of(&self, ty: &Type) -> Option<String> {
        match self.unmade(ty) {
            Type::Named(name, _) => Some(name),
            Type::Applied(applied) => Some(applied.name.clone()),
            _ => None,
        }
    }

    /// The field `field` as the shape `owner` declares it, for what its markers
    /// say (`priv`, `read`, and the value it falls back on). A shape made for a
    /// set of types is read as the shape it was made from, whose fields carry
    /// them (see `owner_of`).
    fn declared_field(&self, owner: &str, field: &str) -> Option<&Field> {
        self.shapes.get(owner)?.fields.iter().find(|declared| declared.name == field)
    }

    /// The methods of the shape or kind `owner`, in no particular order.
    fn methods_of(&self, owner: &str) -> Vec<&str> {
        let prefix = method_name(owner, "");
        self.tasks.keys().filter_map(|name| name.strip_prefix(prefix.as_str())).collect()
    }

    /// The error for `value.name(…)` where a value of type `base` has no method
    /// `name`.
    fn no_method(&self, base: &Type, name: &str, span: Span) -> Error {
        // A slice's built-in changes are what these usually mean.
        let instead = match name {
            "push" | "append" | "insert" => "; a slice adds an item with 'add', as in 'xs.add(v)' or 'xs.add(i, v)'",
            "pop" | "remove" | "delete" => "; a slice takes an item out with 'del', as in 'xs.del(i)' or 'xs.del()'",
            LEN if matches!(base, Type::Text(_)) => "; how many bytes text has is read without '(…)', as in 't.len'",
            LEN => "; how many items a slice holds is read without '(…)', as in 'xs.len'",
            _ => "",
        };
        if let Some(owner) = self.owner_of(base).filter(|owner| self.shapes.contains_key(owner) || self.kinds.contains_key(owner)) {
            let what = if self.kinds.contains_key(&owner) { "kind" } else { "shape" };
            let written = unqualified(&owner);
            if name == STR {
                return Error::at(
                    span,
                    format!("{} '{}' has no method 'str', which turns its values into text; give it one, written 'str(r: region) -> text@r'", what, written),
                );
            }
            if self.shapes.get(&owner).is_some_and(|shape| shape.fields.iter().any(|field| field.name == name)) {
                return Error::at(span, format!("'{}' is a field of {} '{}', not a method, so it is read without '(…)'", name, what, written));
            }
            if self.methods_of(&owner).is_empty() {
                return Error::at(span, format!("{} '{}' has no methods; they are written inside it, below its {}", what, written, if what == "kind" { "variants" } else { "fields" }));
            }
            return Error::at(span, format!("{} '{}' has no method '{}'", what, written, name));
        }
        match base {
            Type::Param(_) if name == STR => Error::at(span, format!("'str' turns a value into text, and {} {}", base, self.why_not(Need::Shown, base))),
            Type::Ptr(..) if name == STR => Error::at(span, format!("type {} can't be turned into text; turn the value it points to into text, as in 'p^.str() at r'", base)),
            Type::Maybe(_) | Type::Or(..) if name == STR => {
                Error::at(span, format!("type {} can't be turned into text; extract its value first, as in 'when x is {{value}}'", base))
            }
            _ if name == STR => Error::at(
                span,
                format!("type {} can't be turned into text; numbers, bools, text, and shapes and kinds with a method 'str' can", base),
            ),
            Type::Ptr(..) => Error::at(span, format!("type {} has no methods; call it on the value it points to, as in 'p^.{}(…)'", base, name)),
            Type::Maybe(_) | Type::Or(..) => {
                Error::at(span, format!("type {} has no methods; extract its value first, as in 'when x is {{value}}'", base))
            }
            _ => Error::at(span, format!("type {} has no methods{}", base, instead)),
        }
    }

    /// Checks `value.name(…)` and rewrites it into the call of the task the
    /// method is (see `method_name`), with the value as its first argument. The
    /// value says which method it is, so it is checked first; the call checks it
    /// again, as a shape literal checks its values again (see `literal_types`).
    fn check_method_call(&mut self, expr: &mut Expr) -> Result<()> {
        let span = expr.span;
        let ExprKind::MethodCall { value, name, .. } = &mut expr.kind else { return Ok(()) };
        let name = name.clone();
        let copies = self.unsettled_copies.len();
        let untold = self.unsettled_types.len();
        let found = self.check_expr(value);
        self.unsettled_copies.truncate(copies);
        self.unsettled_types.truncate(untold);
        let ty = found?;
        // Which method this is comes from the value's type, so a value whose
        // type the checker couldn't work out leaves the call unread; the
        // expression it is part of is left as `Type::Unknown`.
        if ty.unknown() {
            return Ok(());
        }
        // `x.str()` and `text(x)` make new text, so they name the region it goes
        // in; with none written, that is the block's default (see `parse_str`).
        // It reads as the region argument the written forms give, so a shape's
        // own `str` takes it as its parameter, as it always did.
        if name == STR && matches!(&expr.kind, ExprKind::MethodCall { args, .. } if args.is_empty()) {
            let region = self.region_or_default(None, span, || STR_NEEDS_REGION.to_string())?;
            if let ExprKind::MethodCall { args, .. } = &mut expr.kind {
                args.push(Expr::new(ExprKind::Var(region), span));
            }
        }
        if name == STR && self.has_builtin_str(&ty) {
            // The region written after it is the one `str` takes, and the only one.
            if let ExprKind::MethodCall { args, .. } = &mut expr.kind
                && let Some(at) = region_after(args)
            {
                if args.len() > 1 {
                    return err(at, "'str' is given its region in the parentheses already, so this is a second one; name it once");
                }
                if let Some(arg) = args.last_mut()
                    && let ExprKind::RegionAfter(region) = &mut arg.kind
                {
                    arg.kind = ExprKind::Var(std::mem::take(region));
                }
            }
            return self.builtin_str(expr);
        }
        let owner = self.owner_of(&ty).ok_or_else(|| self.no_method(&ty, &name, span))?;
        let full = method_name(&owner, &name);
        let info = self.tasks.get(&full).filter(|_| self.sees(&full)).cloned().ok_or_else(|| self.no_method(&ty, &name, span))?;
        let written = format!("{}{}{}", unqualified(&owner), MODULE_SEPARATOR, name);
        if self.private.contains(&full) && module_of(&owner) != self.current_module {
            return err(span, format!("'{}' is a private method of '{}', which only its own file can use", name, unqualified(&owner)));
        }
        if let ExprKind::MethodCall { args, .. } = &mut expr.kind {
            self.settle_call_region(span, &format!("method '{}'", written), args, info.params.get(1..).unwrap_or_default())?;
        }
        let given = match &expr.kind {
            ExprKind::MethodCall { args, .. } => args.len(),
            _ => 0,
        };
        // The value it is called on is the first parameter, not an argument.
        if given + 1 != info.params.len() {
            return err(span, format!("method '{}' expects {}, got {}", written, count(info.params.len() - 1, "argument"), given));
        }
        if info.owner.as_ref().is_some_and(|owner| owner.alters) {
            let ExprKind::MethodCall { value, .. } = &expr.kind else { return Ok(()) };
            self.altered_place(value, &written)?;
        }
        let taken = std::mem::replace(&mut expr.kind, ExprKind::NoneLit);
        let ExprKind::MethodCall { value, args, .. } = taken else {
            return Err(Error::internal_at(span, "a method call that is no longer one"));
        };
        let mut all = Vec::with_capacity(args.len() + 1);
        all.push(*value);
        all.extend(args);
        expr.kind = ExprKind::Call { name: full, types: Vec::new(), args: all };
        Ok(())
    }

    /// Whether values of type `ty` have `str` built in: numbers, bools, text,
    /// and a type a declaration takes that is `shown`, which is one of those.
    fn has_builtin_str(&self, ty: &Type) -> bool {
        matches!(ty, Type::Int | Type::Float | Type::Bool | Type::Text(_)) || (matches!(ty, Type::Param(_)) && self.allows(Need::Shown, ty))
    }

    /// Rewrites `value.str(r)`, a method call on a value with `str` built in,
    /// into the `ToText` it is, which the rest of the checker checks.
    fn builtin_str(&mut self, expr: &mut Expr) -> Result<()> {
        let span = expr.span;
        let taken = std::mem::replace(&mut expr.kind, ExprKind::NoneLit);
        let ExprKind::MethodCall { value, args, .. } = taken else {
            return Err(Error::internal_at(span, "a 'str' that is no longer a method call"));
        };
        let region = match <[Expr; 1]>::try_from(args) {
            Ok([Expr { kind: ExprKind::Var(region), span: region_span, .. }]) => (region, region_span),
            Ok([other]) => return err(other.span, "'str' takes the region the text goes in, as in 'x.str(r)' or 'x.str() at r'"),
            Err(args) => {
                return err(span, format!("'str' takes the region the text goes in, as in 'x.str(r)' or 'x.str() at r', got {}", count(args.len(), "argument")));
            }
        };
        expr.kind = ExprKind::ToText { value, region: region.0, region_span: region.1 };
        Ok(())
    }

    /// Checks `value.str() at region` on a value with `str` built in, and
    /// returns its type, `text@region`.
    fn check_to_text(&mut self, value: &mut Expr, region: &str, region_span: Span) -> Result<Type> {
        self.check_expr(value)?;
        self.visible_region(region, region_span, true)?;
        Ok(Type::Text(Some(region.to_string())))
    }

    /// Checks `int(x)` or `float(x)`, a value turned into a number, and returns
    /// that number's type. A `float` from an int always works; an `int` from a
    /// float is its whole part, toward zero, which stops the program when there
    /// is none to give, and an `int` from a bool is 1 or 0. A number turned into
    /// its own type is the value itself.
    fn check_number_cast(&mut self, into: &Type, types: &[TypeArg], args: &mut [Expr], span: Span) -> Result<Type> {
        if let Some((_, at)) = types.first() {
            return err(*at, format!("'{0}' takes no types, only the value to turn into {0}, as in '{0}(x)'", into));
        }
        let takes = match into {
            Type::Int => "'int' takes an int, a float, a bool, or text",
            _ => "'float' takes an int, a float, or text",
        };
        let [value] = args else {
            return err(span, format!("'{0}(…)' turns one value into {0}, as in '{0}(x)', got {1}", into, count(args.len(), "value")));
        };
        let ty = self.check_expr(value)?;
        let fits = matches!((into, &ty), (Type::Int, Type::Int | Type::Float | Type::Bool) | (Type::Float, Type::Int | Type::Float));
        if fits || ty.unknown() {
            return Ok(into.clone());
        }
        // Text may hold no number at all, which is a mistake in the input rather
        // than in the program, so reading it sends a value or a reason why not.
        if matches!(ty, Type::Text(_)) {
            return Ok(Type::Or(Box::new(into.clone()), Box::new(Type::Named(PARSE_ERROR.to_string(), None))));
        }
        let hint = match &ty {
            Type::Bool => format!("; a bool becomes a number with 'int', as in '{}(int(ok))'", into),
            Type::Ptr(..) => format!("; to turn the value it points to into a number, write '{}(p^)'", into),
            Type::Maybe(_) | Type::Or(..) => extract_hint(&[&ty]),
            Type::Unit => "; this has no value".to_string(),
            Type::Named(..) | Type::Applied(_) => "; a shape or kind becomes a number only through a task that reads it".to_string(),
            Type::Param(_) => format!("; {} is any type here, and no word requires a number", ty),
            _ => String::new(),
        };
        err(value.span, format!("{}, but this value is {}{}", takes, ty, hint))
    }

    /// Checks that the value a method marked `alters` is called on can change:
    /// the same places an assignment writes to. Its type is already checked, so
    /// this only says why a place can't.
    fn altered_place(&self, value: &Expr, method: &str) -> Result<()> {
        let refuse = |what: String| err(value.span, format!("'{}' changes the value it is called on, so {}", method, what));
        match &value.kind {
            ExprKind::Var(name) => {
                let info = self.lookup_or_err(name, value.span)?;
                if info.mutable {
                    return Ok(());
                }
                if name == THIS {
                    return Err(self.this_cannot_change(value.span, &format!("calling '{}'", method)));
                }
                if info.result {
                    return refuse(format!("it needs a place that can change, and '{}' is a result, which only '<-' sets", name));
                }
                if info.refers.is_some() {
                    return refuse(format!("it needs a place that can change, and '{0}' only reads the place it names; a reference that changes it is written 'hold& {0} = &…'", name));
                }
                if info.param {
                    return refuse(format!(
                        "it needs a place that can change, and '{0}' is a parameter, which never does; write 'hold {0}: {1}' to change the task's own copy, or 'hold& {0}: {1}' to change the caller's",
                        name,
                        info.ty
                    ));
                }
                refuse(format!(
                    "it needs a place that can change, and '{}' never does (declared at {}:{}); a value declared with 'hold' can",
                    name, info.span.line, info.span.col
                ))
            }
            ExprKind::Field(base, field) => {
                if let Some(ty) = &base.ty {
                    self.read_only_field(ty, field, value.span)?;
                }
                self.altered_place(base, method)
            }
            // A slice's items are in a region, behind a pointer, so they can
            // always be written, as what a pointer points to can.
            ExprKind::Index(base, _) if matches!(base.ty, Some(Type::Slice(..))) => Ok(()),
            ExprKind::Index(base, _) => self.altered_place(base, method),
            ExprKind::Deref(_) => Ok(()),
            ExprKind::Shared { shape, field } => self.changeable_shared(shape, field, value.span, &format!("'{}' changes the value it is called on, and ", method)),
            _ => refuse("it needs a place that can change, such as a 'hold' variable, a field or item of one, or what a pointer points to, and not a value made here".to_string()),
        }
    }

    /// The error for changing the value a method is called on where the method
    /// doesn't say it may: `what` is what changes it.
    fn needs_alters(&self, span: Span, what: &str, owner: &Owner) -> Error {
        Error::at(
            span,
            format!(
                "{} changes the value this method is called on, so '{1}' goes before its name, as in '{1} {2}(…)'",
                what, ALTERS, owner.name
            ),
        )
    }

    /// The error for assigning what a method is called on, or a member of it,
    /// where the method doesn't say it may change it.
    fn this_cannot_change(&self, span: Span, what: &str) -> Error {
        match &self.current_method {
            Some(owner) => self.needs_alters(span, what, owner),
            None => Error::at(span, self.not_a_method(THIS)),
        }
    }

    /// Why a method can't allocate in the region the value it is called on lives
    /// in: nothing gives a method that region, only the type of the value. A
    /// region it takes is another one, which the value can't point into, so the
    /// fix, `what_to_do`, is always outside the method.
    fn cannot_allocate_in_this(&self, what_to_do: &str) -> String {
        format!(
            "'@{0}' is the region the value this method is called on lives in, which a method isn't given, so it can't allocate there, and a region it takes is another one, which the value can't point into; {1}",
            THIS_WORD, what_to_do
        )
    }

    /// Checks `X.f`, a shared field of a shape, which `expr` is, and returns its
    /// type. Where a value is worked out before the program runs (the value a
    /// field falls back on, or a shared field's first value), `expr` becomes the
    /// first value of the field it names, which is what it is there. (A
    /// function of its own: `infer_expr`'s frame is on the stack once per
    /// nesting level.)
    #[inline(never)]
    fn check_shared_use(&mut self, expr: &mut Expr) -> Result<Type> {
        let span = expr.span;
        let ExprKind::Shared { shape, field } = &expr.kind else { return Err(Error::internal_at(span, "a shared field without its name")) };
        let (shape, field) = (shape.clone(), field.clone());
        if self.broken.contains(&shape) {
            return Ok(Type::Unknown);
        }
        let Some(def) = self.shapes.get(&shape) else { return Err(Error::internal_at(span, format!("shared field of unknown shape '{}'", shape))) };
        let Some(index) = def.shared.iter().position(|member| member.field.name == field) else {
            if def.fields.iter().any(|declared| declared.name == field) {
                return err(span, format!("'{0}' is a field of every value of '{1}', not a shared one, so it is read on a value, as in 'p.{0}'", field, shape));
            }
            if self.tasks.contains_key(&method_name(&shape, &field)) {
                return err(span, format!("'{0}' is a method of every value of '{1}', so it is called on a value, as in 'p.{0}(…)'", field, shape));
            }
            // `Card.Hearts`, written as if the shape were a kind.
            if self.variants.values().any(|info| info.variant.tag == field) {
                return err(span, format!("'{}' is a shape, not a kind, so it has no variant '{}'", shape, field));
            }
            return err(span, format!("shape '{}' has no shared field '{}'", shape, field));
        };
        let Some(member) = def.shared.get(index).cloned() else { return Ok(Type::Unknown) };
        if member.field.private && module_of(&shape) != self.current_module {
            return err(span, format!("'{}' is a private shared field of '{}', which only its own file can use", field, shape));
        }
        // Worked out before the program runs: the field's first value.
        if let Some((within, above)) = self.in_shared.clone() {
            if within == shape && index >= above {
                let what = if index == above { "its own".to_string() } else { format!("'{}', declared below it at {}:{}", shared_key(&shape, &field), member.field.span.line, member.field.span.col) };
                return err(span, format!("a shared field's first value can name the shared fields above it in its shape, not {}", what));
            }
        } else if let Some(falls_back) = self.in_default.clone() {
            if member.changes() {
                return err(
                    span,
                    format!(
                        "the value field '{}' falls back on is worked out before the program runs, and shared field '{}' can change, so it can't be named there; one that never changes can",
                        falls_back, shared_key(&shape, &field)
                    ),
                );
            }
        } else {
            expr.ty = Some(member.field.ty.clone());
            return Ok(member.field.ty);
        }
        let value = self.shared_value(&shape, index, span)?;
        let ty = value.ty.clone().unwrap_or(Type::Unknown);
        *expr = Expr { span, ..value };
        Ok(ty)
    }

    /// Refuses to change the shared field `field` of `shape` at `span` where
    /// it can't be: one that is neither `hold` nor `read`, and a `read` one
    /// outside the methods of its shape. `lead` starts the message, for a
    /// change that is a call's.
    fn changeable_shared(&self, shape: &str, field: &str, span: Span, lead: &str) -> Result<()> {
        let Some(member) = self.shapes.get(shape).and_then(|def| def.shared.iter().find(|member| member.field.name == field)) else { return Ok(()) };
        let written = shared_key(shape, field);
        if !member.changes() {
            return err(
                span,
                format!(
                    "{}'{}' never changes (declared at {}:{}); a shared field that changes is 'shared hold {}', or 'shared read {}', which only the methods of '{}' change",
                    lead, written, member.field.span.line, member.field.span.col, field, field, shape
                ),
            );
        }
        if !member.hold && self.current_shape.as_deref() != Some(shape) {
            return err(span, format!("{}'{}' is a read-only shared field, which only the methods of '{}' change", lead, written, shape));
        }
        Ok(())
    }

    /// The first value of shared field `index` of `shape`, checked, which a use
    /// at `span` names (or the field's own declaration). Each is checked once,
    /// where its own module reads it: a mistake found in it is sent on, to be
    /// reported once by whoever asked first, and a later use of it only says it
    /// has one.
    fn shared_value(&mut self, shape: &str, index: usize, span: Span) -> Result<Expr> {
        let Some(member) = self.shapes.get(shape).and_then(|def| def.shared.get(index)).cloned() else {
            return Err(Error::internal_at(span, "a shared field that isn't declared"));
        };
        let key = shared_key(shape, &member.field.name);
        let written = shared_key(shape, &member.field.name);
        match self.shared_values.get(&key) {
            Some(Some(value)) => return Ok(value.clone()),
            Some(None) => return err(span, format!("the first value of '{}' has a mistake of its own", written)),
            None => {}
        }
        if let Some(at) = self.shared_checking.iter().position(|checking| *checking == key) {
            let mut circle: Vec<String> = self.shared_checking.get(at..).unwrap_or_default().to_vec();
            circle.push(key);
            return err(span, format!("the first values of shared fields name one another in a circle, {}, so none of them has one", circle.join(" → ")));
        }
        let Some(mut value) = member.field.default.clone() else { return Err(Error::internal_at(span, "a shared field with no first value")) };
        self.shared_checking.push(key.clone());
        let module = std::mem::replace(&mut self.current_module, module_of(shape).to_string());
        let outer_shared = self.in_shared.replace((shape.to_string(), index));
        let outer_default = self.in_default.take();
        let checked = self.check_first_value(&mut value, &member.field);
        self.in_default = outer_default;
        self.in_shared = outer_shared;
        self.current_module = module;
        self.shared_checking.pop();
        let kept = checked.map(|()| value);
        self.shared_values.insert(key, kept.as_ref().ok().cloned());
        kept
    }

    /// Checks a shared field's first value, written after its type: a value the
    /// compiler works out without running the program, as a field falls back
    /// on (see `check_field_default`), which may name a shared field above it.
    fn check_first_value(&mut self, value: &mut Expr, field: &Field) -> Result<()> {
        self.check_expr(value)?;
        if !constant_value(value) {
            return err(
                value.span,
                format!(
                    "the first value of shared field '{}' must be one the compiler works out without running the program: a number, text, 'true', 'false', 'none', a variant, a shared field above it, or a tuple, an array, or a literal of those",
                    field.name
                ),
            );
        }
        self.coerce(value, &field.ty)
    }

    /// Checks the first value of every shared field, and writes it, checked,
    /// into its shape's declaration for the code generator. A field named by
    /// another's first value has been checked already, and is checked once.
    fn check_shared_fields(&mut self, items: &mut [Item]) {
        for item in items.iter_mut() {
            let Item::Shape(shape) = item else { continue };
            if self.broken.contains(&shape.name) {
                continue;
            }
            for index in 0..shape.shared.len() {
                let Some(member) = shape.shared.get(index) else { continue };
                let key = shared_key(&shape.name, &member.field.name);
                if !self.shared_values.contains_key(&key) {
                    let span = member.field.default.as_ref().map_or(member.field.span, |value| value.span);
                    let checked = self.shared_value(&shape.name, index, span).map(|_| ());
                    self.keep_going(checked);
                }
                if let (Some(Some(value)), Some(member)) = (self.shared_values.get(&key), shape.shared.get_mut(index)) {
                    member.field.default = Some(value.clone());
                }
            }
        }
    }

    /// The error for `.this` or `.x` in a task that is no method.
    fn not_a_method(&self, written: &str) -> String {
        if self.in_shared.is_some() {
            return format!("'{}' reads the value a method is called on, and a shared field belongs to no value", written);
        }
        if let Some(field) = &self.in_default {
            return format!("'{}' reads the value a method is called on, and the value field '{}' falls back on is worked out before there is one, so it reads no other field", written, field);
        }
        let names = if written == THIS { "is the value a method is called on" } else { "names a member of the value a method is called on" };
        format!("'{}' {}, and this is a task, which is called on no value", written, names)
    }

    /// Checks a call's arguments against the task's `params`, and returns the
    /// type the call gives. A region argument must be a region the caller can
    /// allocate in. The task's other regions stand for the caller's regions
    /// that the arguments' types show; one that no argument shows (as for
    /// `none`) becomes `ANY_REGION`.
    fn check_call_args(&mut self, span: Span, task: &str, written: &mut Vec<TypeArg>, args: &mut [Expr], info: &TaskInfo) -> Result<Type> {
        let params = &info.params;
        let ret = &info.ret;
        // The types the call gives: written after the name, or worked out from
        // the arguments below (and from the place the value goes, later).
        let mut given = self.written_types(span, task, written, info)?;
        // What the arguments show each type to be, kept apart from the types
        // written at the call so one written without its region is reported as
        // that, rather than as an argument of the wrong type.
        let mut shown: HashMap<String, Type> = HashMap::new();
        // Where each type the call gives is written: the call itself, for one
        // worked out rather than written.
        let places: Vec<Span> = info.types.iter().enumerate().map(|(i, _)| written.get(i).map_or(span, |(_, at)| *at)).collect();
        let mut regions: HashMap<String, String> = HashMap::new();
        let what = if info.owner.is_some() { "method" } else { "task" };
        for ((arg, (param, ty)), passing) in args.iter_mut().zip(params).zip(&info.passing) {
            if *ty != Type::Region {
                // A value of a shape made for its types is read as that shape
                // given them, so a header naming `Box<T>` matches it (see
                // `match_regions`).
                let actual = self.check_argument(arg, *passing, &format!("{} '{}'", what, task), param)?;
                self.match_regions(ty, &actual, &mut regions);
                self.match_params(ty, &actual, &mut given);
                self.match_params(ty, &actual, &mut shown);
                continue;
            }
            let ExprKind::Var(region) = &arg.kind else {
                return err(arg.span, format!("task '{}' takes a region as '{}', such as a 'region r' declared before the call", task, param));
            };
            self.visible_region(region, arg.span, true)?;
            if let Some(bound) = regions.get(param)
                && bound != region
            {
                return err(arg.span, format!("expected region '{}', got '{}'", bound, region));
            }
            regions.insert(param.clone(), region.clone());
            arg.ty = Some(Type::Region);
        }
        // A written type names its regions, as a written type does everywhere
        // else, so one that an argument shows to be of a region names it too.
        for ((param, at), (ty, _)) in info.types.iter().zip(&places).zip(written.iter()) {
            if let Some(held) = shown.get(&param.name)
                && held.without_regions() == ty.without_regions()
                && !fits_regions(held, ty)
            {
                return err(
                    *at,
                    format!(
                        "task '{0}' is given {1} as '{2}', but its argument holds {3}; a written type names its regions, so write '{0}<{3}>(…)'",
                        task, ty, param.name, held
                    ),
                );
            }
        }
        // Every type the call gives must allow what the task's header asks of
        // it, and the types it names are put in place of the parameters.
        for (param, at) in info.types.iter().zip(&places) {
            if let Some(ty) = given.get(&param.name) {
                self.check_need("task", task, param, ty, *at)?;
            }
        }
        // Everything a value in a region holds is in that region, so a type
        // given for a `T` must be of the region the header keeps it in.
        if !info.types.is_empty() {
            self.check_kept_regions(span, task, info, &regions, &given)?;
        }
        for ((arg, (param, ty)), passing) in args.iter_mut().zip(params).zip(&info.passing) {
            if *ty == Type::Region {
                continue;
            }
            let expected = self.settled_type(ty, &regions, &given, span)?;
            let actual = arg.ty()?.clone();
            // The place itself is given, so it holds exactly what the task
            // may write there: nothing becomes a wider type on the way.
            if *passing == Passing::HoldRef {
                // A region no argument names holds nothing the task could write
                // there but values that last as long as the program, as the
                // place's own do.
                let lasting = expected.map_regions(&mut |region| (region != ANY_REGION).then(|| region.to_string()));
                if actual != expected && actual != lasting && !actual.unknown() && !expected.unknown() {
                    if actual.without_regions() == expected.without_regions() {
                        return err(
                            arg.span,
                            format!("expected type {0}, got {1}; the {2} may write a {0} in the place given as '{3}', which a place of type {1} can't hold", expected, actual, what, param),
                        );
                    }
                    return err(
                        arg.span,
                        format!("expected type {}, got {}; '{}' is given the place itself, which the {} changes, so the place is of exactly its type", expected, actual, param, what),
                    );
                }
                continue;
            }
            let same_but_region = [Some(&expected), expected.extracted()].into_iter().flatten().any(|e| actual.without_regions() == e.without_regions());
            match self.coerce(arg, &expected) {
                // A parameter that holds text of no region, given text of one.
                Err(_) if same_but_region && expected.regions().is_empty() => {
                    let example = expected.map_regions_of_names(&mut |_| Some("r".to_string()));
                    return err(
                        arg.span,
                        format!(
                            "expected type {}, got {}; {} '{}' takes text that lasts as long as the program here, as a literal does; to take text of any region, its parameter can name one, as in '{}: {}'",
                            expected, actual, what, task, param, example
                        ),
                    );
                }
                // A method's parameter in `@this`, given a value of another region.
                Err(_) if same_but_region && info.owner.is_some() && ty.regions().contains(&THIS_WORD) => {
                    return err(
                        arg.span,
                        format!(
                            "expected type {}, got {}; method '{}' takes '{}' in '@{}', the region the value it is called on lives in, so it must be in that region too",
                            expected, actual, task, param, THIS_WORD
                        ),
                    );
                }
                Err(_) if same_but_region && !expected.regions().contains(&ANY_REGION) => {
                    return err(
                        arg.span,
                        format!("expected type {}, got {}; {} '{}' names one region for this and another argument, so they must be in the same region", expected, actual, what, task),
                    );
                }
                result => result?,
            }
        }
        self.check_aliasing(args, info)?;
        let sent = self.settled_type(ret, &regions, &given, span)?;
        // A type no argument names is worked out from the place the value goes
        // (see `settle_types`), or written at the call.
        let missing: Vec<&TypeParam> = info.types.iter().filter(|param| !given.contains_key(&param.name)).collect();
        if let Some(param) = missing.first() {
            if !sent.params().contains(&param.name.as_str()) {
                return err(span, self.untold_type(task, &param.name));
            }
            self.unsettled_types.push((span, task.to_string(), param.name.clone()));
        }
        *written = info
            .types
            .iter()
            .zip(&places)
            .map(|(param, at)| (given.get(&param.name).cloned().unwrap_or_else(|| Type::Param(param.name.clone())), *at))
            .collect();
        self.note_instance(span, task, written);
        Ok(sent)
    }

    /// Checks `hold& y = &x` or `bind& y = &x`, which the parser made one name
    /// and one value, and declares `y`, another name for the place `&x` marks:
    /// one that changes it (`hold&`), or only reads it. `y` is of the place's
    /// type exactly, since what either writes the other reads.
    fn check_reference(&mut self, names: &mut [Bound], mutable: bool, init: &mut [Expr]) -> Result<()> {
        let word = if mutable { "hold&" } else { "bind&" };
        let (Some(bound), Some(value)) = (names.first_mut(), init.first_mut()) else { return Ok(()) };
        self.resolve_bound_types(bound)?;
        let name = bound.name.clone();
        if name == DISCARD {
            return err(bound.span, format!("a reference names a place for as long as its block lasts, so '{}' takes a name, not '_'", word));
        }
        let ExprKind::Ref(place) = &mut value.kind else {
            self.check_expr(value)?;
            let example = place_path(value).map_or_else(|| "x".to_string(), |path| path_text(&path));
            return err(value.span, format!("'{} {}' is another name for a place, marked with '&', as in '&{}'", word, name, example));
        };
        let ty = self.check_expr(place)?;
        if mutable {
            if let Err(error) = self.given_place(place, &Taker::Reference(&name)) {
                // A place that only can't change may still be read through one.
                return match (self.read_place(place), self.resolved_path(place)) {
                    (Ok(()), Some(path)) => Err(Error { message: format!("{}; 'bind& {} = &{}' names it only to read it", error.message, name, path_text(&path)), ..error }),
                    _ => Err(error),
                };
            }
        } else {
            self.read_place(place)?;
        }
        let Some(path) = self.resolved_path(place) else {
            return err(place.span, format!("'{} {}' names a place: a variable, a field or an item of one, or what a pointer points to", word, name));
        };
        if let Some(annotated) = &bound.ty
            && *annotated != ty
            && !ty.unknown()
        {
            return err(
                bound.ty_span.unwrap_or(bound.span),
                format!("'{}' names a place of type {}, and a reference is of its place's type exactly, since what either writes the other reads", name, ty),
            );
        }
        bound.ty = Some(ty.clone());
        value.ty = Some(ty.clone());
        let at = bound.span;
        self.define(&name, ty, mutable, at)?;
        if let Some(info) = self.scopes.last_mut().and_then(|scope| scope.names.get_mut(&name)) {
            info.refers = Some(path);
        }
        Ok(())
    }

    /// Checks that `place`, already checked, can be read through `bind& y`: a
    /// place, as `given_place` takes, of any variable, other than an item of a
    /// slice, which moves when the slice grows, and a result, which only its
    /// name reads.
    fn read_place(&self, place: &Expr) -> Result<()> {
        let refuse = |why: &str| err(place.span, format!("a reference names a place for as long as its block lasts, so {}", why));
        match &place.kind {
            ExprKind::Var(name) => {
                if self.lookup_or_err(name, place.span)?.result {
                    return refuse(&format!("it can't name '{}', a result, which is read by its name", name));
                }
                Ok(())
            }
            ExprKind::Field(base, _) => self.read_place(base),
            ExprKind::Index(_, index) if matches!(index.kind, ExprKind::Range(..)) => refuse("it can't name a part taken of a value, which is a new value, not a place"),
            ExprKind::Index(base, _) if matches!(base.ty, Some(Type::Slice(..))) => refuse(
                "it can't name an item of a slice, which moves when the slice grows; hold the item's index, or copy the item into a variable",
            ),
            ExprKind::Index(base, _) => self.read_place(base),
            ExprKind::Deref(_) | ExprKind::Shared { .. } => Ok(()),
            _ => refuse("it names a variable, a field or an item of one, or what a pointer points to, and not a value made here"),
        }
    }

    /// The place `e` names, as `place_path` gives it, with a reference it
    /// starts at replaced by the place that reference names.
    fn resolved_path(&self, e: &Expr) -> Option<Vec<Step>> {
        let mut path = place_path(e)?;
        if let Some(Step::Field(root)) = path.first()
            && let Some(refers) = self.lookup(root).and_then(|info| info.refers.clone())
        {
            path.splice(0..1, refers);
        }
        Some(path)
    }

    /// Checks one argument of a call, given to `param` of `callee` (`task 'f'`),
    /// and returns its type: a place marked `&` for a `hold&` parameter, and a
    /// value otherwise.
    fn check_argument(&mut self, arg: &mut Expr, passing: Passing, callee: &str, param: &str) -> Result<Type> {
        match (&mut arg.kind, passing) {
            (ExprKind::Ref(place), Passing::HoldRef) => {
                let ty = self.check_expr(place)?;
                self.given_place(place, &Taker::Param(param))?;
                arg.ty = Some(ty.clone());
                Ok(ty)
            }
            (ExprKind::Ref(_), _) => err(
                arg.span,
                format!("{} takes a copy as '{1}', so leave out '&'; a parameter written 'hold& {1}: …' is given the place itself", callee, param),
            ),
            (_, Passing::HoldRef) => {
                self.check_expr(arg)?;
                match place_path(arg) {
                    Some(path) => err(
                        arg.span,
                        format!("{} changes '{}', the place it is given, so the call marks that place with '&', as in '&{}'", callee, param, path_text(&path)),
                    ),
                    None => err(
                        arg.span,
                        format!("{} changes '{}', the place it is given, so it takes a place that can change, marked with '&', as in '&a', and this is a value made here", callee, param),
                    ),
                }
            }
            _ => self.check_expr(arg),
        }
    }

    /// Checks that `place`, already checked, can be given to a `hold&`
    /// parameter or named by a `hold&` reference (see `Taker`):
    /// a place that can change, as an assignment writes to, other than an item
    /// of a slice, which may move when the slice grows.
    fn given_place(&self, place: &Expr, taker: &Taker) -> Result<()> {
        let refuse = |why: String| err(place.span, format!("{}, so {}", taker.lead(), why));
        match &place.kind {
            ExprKind::Var(name) => {
                let info = self.lookup_or_err(name, place.span)?;
                if info.result {
                    return refuse(format!("it needs a place that can change, and '{}' is a result, which only '<-' sets", name));
                }
                if info.mutable {
                    return Ok(());
                }
                if name == THIS {
                    return Err(self.this_cannot_change(place.span, &taker.using(THIS)));
                }
                if info.refers.is_some() {
                    return refuse(format!("it needs a place that can change, and '{}' is a 'bind&', which only reads the place it names", name));
                }
                if info.param {
                    return refuse(format!(
                        "it needs a place that can change, and '{0}' is a parameter, which never does; to pass on the caller's place, write 'hold& {0}: {1}'",
                        name, info.ty
                    ));
                }
                refuse(format!("it needs a place that can change, and '{}' never does (declared at {}:{}); a value declared with 'hold' can", name, info.span.line, info.span.col))
            }
            ExprKind::Field(base, field) => {
                if matches!(&base.kind, ExprKind::Var(name) if name == THIS) && !self.lookup(THIS).is_some_and(|info| info.mutable) {
                    return Err(self.this_cannot_change(place.span, &taker.using(&format!("{}{}", MODULE_SEPARATOR, field))));
                }
                match &base.ty {
                    Some(Type::Tuple(_)) => return refuse("it needs a place that can change, and a tuple's items never do".to_string()),
                    Some(Type::Array(..) | Type::Slice(..) | Type::Text(_)) => {
                        return refuse(format!("it needs a place that can change, and '{}' is worked out from the value, not kept in it", field));
                    }
                    Some(ty) => self.read_only_field(ty, field, place.span)?,
                    None => {}
                }
                self.given_place(base, taker)
            }
            ExprKind::Index(_, index) if matches!(index.kind, ExprKind::Range(..)) => {
                refuse("it needs a place that can change, and a part taken of a value is a new value, not a place".to_string())
            }
            ExprKind::Index(base, _) => match &base.ty {
                Some(Type::Slice(..)) => refuse(
                    "it can't be an item of a slice, which moves when the slice grows; give the slice and the item's index, or copy the item into a 'hold' variable, change it, and put it back"
                        .to_string(),
                ),
                Some(Type::Text(_)) => refuse("it needs a place that can change, and text never does".to_string()),
                _ => self.given_place(base, taker),
            },
            ExprKind::Deref(_) => Ok(()),
            ExprKind::Shared { shape, field } => self.changeable_shared(shape, field, place.span, &format!("{}, and ", taker.lead())),
            _ => refuse("it needs a place that can change: a 'hold' variable, a field or array item of one, or what a pointer points to (p^), not a value made here".to_string()),
        }
    }

    /// Rejects a call that gives one place twice where the task changes it:
    /// `swap(&a, &a)`, `f(a, &a)`, or `f(&a, &a.x)`, and a method that alters
    /// the value it is called on given a part of it as a `hold&`,
    /// `c.give(&c.n)`. A copy of a part of it, `c.grow(c.n)`, is read before
    /// the method runs, as it always was. The task would see a value
    /// change under it that it never wrote. Two places through pointers, `&p^`
    /// and `&q^`, can't be told apart before the program runs, and are allowed,
    /// as writing through two pointers is.
    fn check_aliasing(&self, args: &[Expr], info: &TaskInfo) -> Result<()> {
        let alters = info.owner.as_ref().is_some_and(|owner| owner.alters);
        // Each argument that names a place, whether the task changes it, and
        // the parameter it is given to.
        let mut places: Vec<(Vec<Step>, bool, &str, Span)> = Vec::new();
        for (i, ((arg, (param, _)), passing)) in args.iter().zip(&info.params).zip(&info.passing).enumerate() {
            let (place, changed) = match (&arg.kind, passing) {
                (ExprKind::Ref(place), Passing::HoldRef) => (place.as_ref(), true),
                (_, Passing::Hold) => continue,
                _ => (arg, i == 0 && alters),
            };
            let Some(path) = self.resolved_path(place) else { continue };
            for (before, before_changed, before_param, _) in &places {
                // A value given to a copy is read before the call starts, so a
                // method may change the value it is called on and be given a
                // part of it, `c.grow(c.n)`; only a `hold&` beside it names it.
                let receiver_and_copy = alters && ((i != 0 && !changed && *before_param == THIS) || (i == 0 && !*before_changed));
                if (changed || *before_changed) && !receiver_and_copy && overlaps(before, &path) {
                    let name = |param: &str| if param == THIS { "the value it is called on".to_string() } else { format!("'{}'", param) };
                    let relation = if path.len() > before.len() {
                        "is part of the place given as"
                    } else if path.len() < before.len() {
                        "holds the place given as"
                    } else if path.contains(&Step::Item(None)) || before.contains(&Step::Item(None)) {
                        "may be the place given as"
                    } else {
                        "is given as"
                    };
                    let changing = if changed { param.as_str() } else { *before_param };
                    let how = if changing == THIS { "the method changes the value it is called on".to_string() } else { format!("the call changes it through '{}'", changing) };
                    // A reference is named as written, with the place it names.
                    let written = place_path(place).map(|written| path_text(&written)).unwrap_or_default();
                    let subject = if written != path_text(&path) { format!("'{}' names '{}', which", written, path_text(&path)) } else { format!("'{}'", written) };
                    return err(
                        arg.span,
                        format!("{} {} {} too, and {}; a place the call changes is given once, so keep a copy of it in a variable first, and give that as the other", subject, relation, name(before_param), how),
                    );
                }
            }
            places.push((path, changed, param, arg.span));
        }
        Ok(())
    }

    /// One type of a task's header as the call sees it: the caller's regions in
    /// place of the header's own names, the types the call gives in place of the
    /// names it takes, and the shape made for those types in place of a shape
    /// given them (see `settle_applications`).
    fn settled_type(&mut self, ty: &Type, regions: &HashMap<String, String>, given: &HashMap<String, Type>, at: Span) -> Result<Type> {
        let ty = ty.map_regions(&mut |region| Some(regions.get(region).cloned().unwrap_or_else(|| ANY_REGION.to_string())));
        let mut ty = ty.map_params(&mut |name| given.get(name).cloned());
        self.settle_applications(&mut ty, at)?;
        Ok(ty)
    }

    /// Rejects a call whose types would make the task keep a value of one region
    /// inside a value of another, which no value can do.
    fn check_kept_regions(&mut self, span: Span, task: &str, info: &TaskInfo, regions: &HashMap<String, String>, given: &HashMap<String, Type>) -> Result<()> {
        for ty in info.params.iter().map(|(_, ty)| ty.clone()).chain([info.ret.clone()]) {
            let ty = self.settled_type(&ty, regions, given, span)?;
            if let Some((outer, inner)) = region_conflict(&ty, None) {
                return err(
                    span,
                    format!(
                        "'{0}' would keep this in region '{1}' and in region '{2}', but everything a value in a region holds is in that region; copy it into '{1}' first, as in 'value.copy() at {1}'",
                        unqualified(task), outer, inner
                    ),
                );
            }
        }
        Ok(())
    }

    /// The types written after a call's name, by the name of the type each one
    /// is given for. Writing them is all or nothing.
    fn written_types(&mut self, span: Span, task: &str, written: &mut [TypeArg], info: &TaskInfo) -> Result<HashMap<String, Type>> {
        if written.is_empty() {
            return Ok(HashMap::new());
        }
        if info.types.is_empty() {
            return err(span, format!("task '{}' takes no types, so '< … >' has nothing to name", task));
        }
        if written.len() != info.types.len() {
            let names: Vec<&str> = info.types.iter().map(|param| param.name.as_str()).collect();
            return err(
                span,
                format!("task '{}' takes {}, so name all of them, as in '{}<{}>(…)'", task, count(info.types.len(), "type"), unqualified(task), names.join(", ")),
            );
        }
        let mut given = HashMap::new();
        for ((ty, at), param) in written.iter_mut().zip(&info.types) {
            *ty = self.resolve_type(ty, *at, TypePlace::Body)?;
            given.insert(param.name.clone(), ty.clone());
        }
        Ok(given)
    }

    /// Checks that `ty`, given for the type parameter `param`, allows what the
    /// task's header asks of it.
    fn check_need(&self, what: &str, task: &str, param: &TypeParam, ty: &Type, at: Span) -> Result<()> {
        let Some((need, _)) = param.need else { return Ok(()) };
        if self.allows(need, ty) {
            return Ok(());
        }
        err(
            at,
            format!(
                "{} '{}' takes a type that is '{}' as '{}', and {} {}",
                what,
                unqualified(task),
                need.word(),
                param.name,
                ty,
                self.why_not(need, ty)
            ),
        )
    }

    /// Whether values of type `ty` allow what `need` asks: a type parameter
    /// allows it when its own word already does.
    fn allows(&self, need: Need, ty: &Type) -> bool {
        if let Type::Param(name) = ty {
            return self.type_param(name).and_then(|param| param.need).is_some_and(|(own, _)| own.means(need));
        }
        match need {
            Need::Ordered => ty.is_numeric() || matches!(ty, Type::Text(_)),
            Need::Shown => matches!(ty, Type::Int | Type::Float | Type::Bool | Type::Text(_)),
            Need::Same => comparable(ty),
            Need::Fixed => self.changeable_item(ty).is_none(),
        }
    }

    /// Why a type doesn't allow what `need` asks, for the end of a message.
    fn why_not(&self, need: Need, ty: &Type) -> String {
        if let Type::Param(name) = ty {
            let own = self.type_param(name).and_then(|param| param.need);
            return match own {
                Some((own, _)) => format!("is only '{}' here; require it, as in '<{}: {}>'", own.word(), name, need.word()),
                None => format!("is any type here; require it, as in '<{}: {}>'", name, need.word()),
            };
        }
        match need {
            Need::Ordered => "has no order, so '<' can't compare it".to_string(),
            Need::Shown => "can't be shown".to_string(),
            Need::Same => "can't be compared with '=='".to_string(),
            Need::Fixed => match self.changeable_item(ty) {
                Some(what) => format!("holds {}", what),
                None => "can change".to_string(),
            },
        }
    }

    /// The error for a type a call neither names nor shows in its arguments or
    /// in the place its value goes.
    fn untold_type(&self, task: &str, param: &str) -> String {
        format!(
            "can't tell which type '{}' is for this call of '{}', since no argument and nothing this value goes into names one; write it, as in '{}<int>(…)'",
            param,
            unqualified(task),
            unqualified(task)
        )
    }

    /// The error for a value whose type names a type no task here takes: it is
    /// a generic task's, and the call it came from was never told which type to
    /// use, so that is what to say rather than anything about the type itself.
    fn untold_param(&self, ty: &Type) -> Option<String> {
        let name = ty.params().into_iter().find(|name| self.type_param(name).is_none())?;
        let (_, task, _) = self
            .unsettled_types
            .iter()
            .rev()
            .find(|(_, task, param)| param == name || self.tasks.get(task).is_some_and(|info| info.types.iter().any(|p| p.name == name)))?;
        Some(self.untold_type(task, name))
    }

    /// Records that a use of the generic task `task` with the types `given` is
    /// needed, so one made for them is generated (see `instances`). `at` is the
    /// call, for the errors about a task used with too many types.
    fn note_instance(&mut self, at: Span, task: &str, given: &[TypeArg]) {
        if given.is_empty() || given.iter().any(|(ty, _)| !ty.params().is_empty()) {
            return;
        }
        let types: Vec<Type> = given.iter().map(|(ty, _)| ty.clone()).collect();
        let key = instance_name(task, &types);
        if !self.instances.iter().any(|(name, ..)| *name == key) {
            self.instances.push((key, task.to_string(), types, at));
        }
    }

    /// Checks that the value of `expr` fits where `expected` is required, and
    /// marks where it becomes part of a `maybe` or `T ~ E` with a `Wrap` node:
    /// a plain value or `none` in a `maybe`, a plain value or `fail e` in a
    /// `T ~ E`.
    ///
    /// A value built without pointers (of `ANY_REGION`) fits the same type in
    /// any region, and takes that region; any other value must be of exactly
    /// the expected regions.
    fn coerce(&mut self, expr: &mut Expr, expected: &Type) -> Result<()> {
        // A value the checker couldn't work out the type of goes anywhere, and
        // anything goes into a place whose type it couldn't work out: what went
        // wrong is already reported (see `Type::Unknown`).
        if expected.unknown() || expr.ty.as_ref().is_some_and(Type::unknown) {
            return Ok(());
        }
        self.settle_types(expr, expected)?;
        let actual = expr.ty()?.clone();
        if fits_regions(&actual, expected) {
            expr.ty = Some(expected.clone());
            return self.settle_copies(expr, expected);
        }
        // A `match` or a `when` used as a value becomes what is expected in each
        // of its branches, since that is where its values are made: a branch
        // giving `none` beside one giving an `int` becomes part of the `maybe`
        // itself, rather than the whole being wrapped once, which would leave the
        // branches to run for nothing. Only what the branches can become is
        // pushed into them: where the type they settled on doesn't go where this
        // value does at all, the mistake is the whole one's, and a branch has
        // already become that settled type, so blaming its value would name a
        // type the branch doesn't say (`bind n: int = when … 1 … none … fi`).
        if let ExprKind::Choice(stmt) = &mut expr.kind
            && fits(&actual, expected)
        {
            self.settle_branch_values(stmt, expected)?;
            expr.ty = Some(expected.clone());
            return Ok(());
        }
        if self.coerce_items(expr, expected)? {
            return Ok(());
        }
        match expected {
            Type::Maybe(inner) if actual == Type::None || fits_regions(&actual, inner) || self.coerce_items(expr, inner)? => {
                if fits_regions(&actual, inner) {
                    expr.ty = Some((**inner).clone());
                    self.settle_copies(expr, inner)?;
                }
                wrap(expr, expected);
                return Ok(());
            }
            Type::Or(value, error) => {
                if let Type::Fail(given) = &actual {
                    if !fits_regions(given, error) {
                        return err(expr.span, format!("expected an error of type {}, got {}{}", error, given, mismatch_hint(given, error)));
                    }
                    if let ExprKind::Fail(inner) = &mut expr.kind {
                        self.settle_copies(inner, error)?;
                    }
                    wrap(expr, expected);
                    return Ok(());
                }
                if **value != Type::Unit {
                    let fits_value = fits_regions(&actual, value)
                        || matches!(value.as_ref(), Type::Maybe(inner) if actual == Type::None || fits_regions(&actual, inner))
                        || self.coerce_items(expr, value)?;
                    if fits_value {
                        self.coerce(expr, value)?;
                        wrap(expr, expected);
                        return Ok(());
                    }
                }
            }
            _ => {}
        }
        if let Some(untold) = self.untold_param(expected) {
            return err(expr.span, untold);
        }
        err(expr.span, format!("expected type {}, got {}{}", expected, actual, mismatch_hint(&actual, expected)))
    }

    /// Gives a call of a generic task the types no argument named, from the type
    /// of the place its value goes: `bind a: maybe int = nothing()` says that
    /// `nothing`'s `T` is `int`. A call whose types are all known already, and
    /// any other expression, is left as it is.
    fn settle_types(&mut self, expr: &mut Expr, expected: &Type) -> Result<()> {
        let ExprKind::Call { name, types, .. } = &expr.kind else { return Ok(()) };
        if types.iter().all(|(ty, _)| ty.params().is_empty()) {
            return Ok(());
        }
        let name = name.clone();
        let sent = expr.ty.clone().unwrap_or(Type::Unit);
        let mut given: HashMap<String, Type> = HashMap::new();
        self.match_params(&sent, expected, &mut given);
        if given.is_empty() {
            return Ok(());
        }
        let ExprKind::Call { types, .. } = &mut expr.kind else { return Ok(()) };
        for (ty, _) in types.iter_mut() {
            *ty = ty.map_params(&mut |param| given.get(param).cloned());
        }
        let types = types.clone();
        expr.ty = Some(sent.map_params(&mut |param| given.get(param).cloned()));
        if types.iter().any(|(ty, _)| !ty.params().is_empty()) {
            return Ok(());
        }
        if let Some(at) = self.unsettled_types.iter().rposition(|(at, _, _)| *at == expr.span) {
            self.unsettled_types.remove(at);
        }
        if let Some(info) = self.tasks.get(&name).cloned() {
            for (param, (ty, at)) in info.types.iter().zip(&types) {
                self.check_need("task", &name, param, ty, *at)?;
            }
        }
        self.note_instance(expr.span, &name, &types);
        Ok(())
    }

    /// Coerces an array written as a literal item by item, when its values can
    /// become the items of `expected`, as `[1, none]` or `[none; 4]` become
    /// arrays of `maybe int`. Whether `expected` is such an array.
    fn coerce_items(&mut self, expr: &mut Expr, expected: &Type) -> Result<bool> {
        // `(1, none)` as a `(int, maybe int)`: each value becomes its item.
        if let (Type::Tuple(items), ExprKind::Tuple(values)) = (expected, &mut expr.kind) {
            if items.len() != values.len() {
                return Ok(false);
            }
            for (value, item) in values.iter_mut().zip(items) {
                self.coerce(value, item)?;
            }
            expr.ty = Some(expected.clone());
            return Ok(true);
        }
        let Type::Array(items, count) = expected else { return Ok(false) };
        let written = match &expr.ty {
            Some(Type::Array(_, written)) => *written,
            _ => return Ok(false),
        };
        if written != *count {
            return Ok(false);
        }
        match &mut expr.kind {
            ExprKind::Array(values) => values.iter_mut().try_for_each(|value| self.coerce(value, items))?,
            ExprKind::Repeat { value, .. } => self.coerce(value, items)?,
            _ => return Ok(false),
        }
        expr.ty = Some(expected.clone());
        Ok(true)
    }

    /// Checks that a value of type `actual` is exactly `expected`, where no
    /// value changes type: conditions, operands, and ranges.
    fn expect_type(&self, actual: &Type, expected: &Type, span: Span) -> Result<()> {
        if actual.unknown() || expected.unknown() {
            // One of them is a type the checker couldn't work out, and what
            // went wrong is already reported (see `Type::Unknown`).
            return Ok(());
        }
        if actual != expected {
            err(span, format!("expected type {}, got {}{}", expected, actual, extract_hint(&[actual])))
        } else {
            Ok(())
        }
    }
}

/// The type of the items of an array type, as a message writes it for a
/// slice made from the array: without regions.
fn item_type_example(array: &Type) -> Type {
    match array {
        Type::Array(items, _) => items.without_regions(),
        other => other.without_regions(),
    }
}

/// Replaces `expr` with a `Wrap` of itself, of type `ty`.
fn wrap(expr: &mut Expr, ty: &Type) {
    let span = expr.span;
    let inner = std::mem::replace(expr, Expr::new(ExprKind::NoneLit, span));
    *expr = Expr { kind: ExprKind::Wrap(Box::new(inner)), span, ty: Some(ty.clone()) };
}

/// A hint for an operation on a `maybe` or `T ~ E`, whose value must be
/// extracted first; empty if none of `types` is one.
fn extract_hint(types: &[&Type]) -> String {
    match types.iter().find(|t| matches!(t, Type::Maybe(_) | Type::Or(..) | Type::Fail(_))) {
        Some(Type::Fail(_)) => "; 'fail' takes only the value right after it, so write 'fail (a + b)'".to_string(),
        Some(Type::Or(value, error)) if **value == Type::Unit => {
            format!("; a 'unit ~ {}' has no value, only an error to handle, as in 'x {{ failure e … }}'", error)
        }
        Some(ty) => format!("; the value in this {} must be extracted first, as in 'x {{ … }}' or 'when x is {{value}}'", ty),
        None => String::new(),
    }
}

/// `ty` with `ANY_REGION` removed from its text and from the shapes and kinds
/// that hold text but no pointers (not in `regional`), as a variable without a
/// written type takes it: see `Checker::lasting`.
pub fn lasting(ty: &Type, regional: &HashSet<String>) -> Type {
    let any = Some(ANY_REGION.to_string());
    match ty {
        Type::Text(region) if *region == any => Type::Text(None),
        Type::Named(name, region) if *region == any && !regional.contains(name) => Type::Named(name.clone(), None),
        Type::Applied(applied) if applied.region == any && !regional.contains(&applied.name) => {
            Type::Applied(Box::new(Applied { region: None, ..(**applied).clone() }))
        }
        Type::Maybe(inner) => Type::Maybe(Box::new(lasting(inner, regional))),
        Type::Array(inner, count) => Type::Array(Box::new(lasting(inner, regional)), *count),
        Type::Or(value, error) => Type::Or(Box::new(lasting(value, regional)), Box::new(lasting(error, regional))),
        Type::Tuple(items) => Type::Tuple(items.iter().map(|item| lasting(item, regional)).collect()),
        other => other.clone(),
    }
}

/// The shapes and kinds that hold pointers, directly or in other shapes and
/// kinds by value, so their values live in a region.
pub fn regional_types(items: &[Item]) -> HashSet<String> {
    types_holding(items, holds_pointers)
}

/// The shapes and kinds that hold text, directly or in other shapes and kinds
/// by value. One may name a region, in which its text is, or none, when its
/// text lasts as long as the program.
pub fn textual_types(items: &[Item]) -> HashSet<String> {
    types_holding(items, holds_text)
}

/// The shapes and kinds with a field or payload for which `holds` is true,
/// given the types already found, directly or through other shapes and kinds.
fn types_holding(items: &[Item], holds: fn(&Type, &HashSet<String>) -> bool) -> HashSet<String> {
    let fields_of = |item: &Item| -> Vec<Type> {
        match item {
            Item::Shape(s) => s.fields.iter().map(|f| f.ty.clone()).collect(),
            Item::Kind(k) => k.variants.iter().flat_map(|v| &v.payload).map(|f| f.ty.clone()).collect(),
            Item::Task(_) => Vec::new(),
        }
    };
    // The items whose fields name each type, to look at again when it turns
    // out to be regional.
    let mut users: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, item) in items.iter().enumerate() {
        for ty in fields_of(item) {
            for name in named_types(&ty) {
                users.entry(name.to_string()).or_default().push(i);
            }
        }
    }
    let mut regional = HashSet::new();
    let mut pending: Vec<usize> = (0..items.len()).collect();
    while let Some(i) = pending.pop() {
        let name = match items.get(i) {
            Some(Item::Shape(s)) => &s.name,
            Some(Item::Kind(k)) => &k.name,
            _ => continue,
        };
        if !regional.contains(name) && items.get(i).map(fields_of).unwrap_or_default().iter().any(|ty| holds(ty, &regional)) {
            regional.insert(name.clone());
            pending.extend(users.get(name).into_iter().flatten().copied());
        }
    }
    regional
}

/// A field's type, or the type after `new`, whose regions are unsettled, as it
/// is in a value of region `region`: its pointers, slices, and text, and its
/// shapes and kinds in `takes_region` (see `Checker::takes_region`) get
/// `region`.
pub fn in_region(ty: &Type, region: &str, takes_region: &HashSet<String>) -> Type {
    ty.map_regions_of_names(&mut |name| (name.is_empty() || takes_region.contains(name)).then(|| region.to_string()))
}

/// Whether a value of type `actual` fits where `expected` is required without
/// changing: the same type, where `actual` may be of `ANY_REGION` wherever
/// `expected` names a region. Text, and a shape or kind holding text, that
/// names no region lasts as long as the program, so it fits any region too. (A
/// shape or kind holding pointers always has a region once checked.)
fn fits_regions(actual: &Type, expected: &Type) -> bool {
    // A type the checker couldn't work out fits anywhere, and anything fits it:
    // what went wrong is reported where it was made (see `Type::Unknown`).
    if actual.unknown() || expected.unknown() {
        return true;
    }
    let region = |a: &Option<String>, e: &Option<String>| a == e || a.as_deref() == Some(ANY_REGION);
    let lasting = |a: &Option<String>, e: &Option<String>| region(a, e) || a.is_none();
    match (actual, expected) {
        (Type::Ptr(a, ar), Type::Ptr(e, er)) | (Type::Slice(a, ar), Type::Slice(e, er)) => region(ar, er) && fits_regions(a, e),
        (Type::Array(a, an), Type::Array(e, en)) => an == en && fits_regions(a, e),
        // A shape given types: the same shape and types, of a fitting region.
        (Type::Applied(a), Type::Applied(e)) if a.name == e.name && a.types.len() == e.types.len() => {
            lasting(&a.region, &e.region) && a.types.iter().zip(&e.types).all(|(a, e)| fits_regions(a, e))
        }
        (Type::Text(ar), Type::Text(er)) => lasting(ar, er),
        (Type::Named(a, ar), Type::Named(e, er)) => a == e && lasting(ar, er),
        (Type::Range(a), Type::Range(e)) | (Type::Maybe(a), Type::Maybe(e)) | (Type::Fail(a), Type::Fail(e)) => fits_regions(a, e),
        (Type::Or(av, ae), Type::Or(ev, ee)) => fits_regions(av, ev) && fits_regions(ae, ee),
        (Type::Tuple(a), Type::Tuple(e)) => a.len() == e.len() && a.iter().zip(e).all(|(a, e)| fits_regions(a, e)),
        _ => actual == expected,
    }
}

/// Whether a statement on the last line of a block gives that block its value: a
/// `match`, whose cases cover every value, or a `when` with an `else`, so that
/// every way through it gives one. A `when` without an `else` can't, so such a
/// block must send on every path, which is what its message says.
pub fn gives_value(stmt: &Stmt) -> bool {
    matches!(&stmt.kind, StmtKind::Match(..) | StmtKind::When(_, _, Some(_)))
}

/// Whether a value of type `from` goes where `to` is expected without being
/// remade: unchanged, or wrapped as part of a `maybe` or a `T ~ E`, which is all
/// `coerce` can do to a value whose parts it cannot see. Used where the branches
/// of a value `match` or `when` are checked against what they settled on
/// (`Checker::settle_branches`), and to tell a place that takes what they settled
/// on from one that would need each branch remade (`Checker::coerce`).
fn fits(from: &Type, to: &Type) -> bool {
    if fits_regions(from, to) {
        return true;
    }
    match to {
        Type::Maybe(inner) => *from == Type::None || fits_regions(from, inner),
        Type::Or(value, error) => match from {
            Type::Fail(given) => fits_regions(given, error),
            other => fits_regions(other, value),
        },
        _ => false,
    }
}

/// Whether a value of type `from` would have to be **remade inside** to be a
/// `to`: it is not one already and doesn't become one by being wrapped (`fits`),
/// but every part of it goes into the part beside it, wrapped or remade inside in
/// this same way. Nothing remakes a value already made (see `join_types`), so a
/// message says what to do instead. Two types that simply differ, like
/// `(int, text)` and `(int, maybe int)`, are not this.
fn remade_inside(from: &Type, to: &Type) -> bool {
    if fits(from, to) {
        return false;
    }
    match (from, to) {
        (Type::Maybe(a), Type::Maybe(b)) | (Type::Fail(a), Type::Fail(b)) => fits(a, b) || remade_inside(a, b),
        (Type::Or(value, error), Type::Or(other, known)) => fits_regions(error, known) && (fits(value, other) || remade_inside(value, other)),
        (Type::Array(a, an), Type::Array(b, bn)) => an == bn && (fits(a, b) || remade_inside(a, b)),
        (Type::Tuple(a), Type::Tuple(b)) => a.len() == b.len() && a.iter().zip(b).all(|(a, b)| fits(a, b) || remade_inside(a, b)),
        _ => false,
    }
}

/// Records in `regions` what each region of `pattern` stands for in `actual`,
/// following `actual` into the places a value can become part of `pattern`
/// (`maybe`, `T ~ E`, `fail`). A region already recorded, or one `actual`
/// doesn't show, is left as it is; checking the value against the settled type
/// then reports any mismatch.
/// The two regions of a type that keeps a value of one region inside a value of
/// another, which no value can do: the region around it and the one inside.
/// `None` when every part of `ty` is in one region, or lasts as long as the
/// program (a literal's text, which fits any region).
fn region_conflict(ty: &Type, inside: Option<&str>) -> Option<(String, String)> {
    let conflict = |region: &str| -> Option<(String, String)> {
        let inside = inside?;
        (region != inside && region != ANY_REGION && inside != ANY_REGION).then(|| (inside.to_string(), region.to_string()))
    };
    match ty {
        Type::Ptr(inner, region) | Type::Slice(inner, region) => match region.as_deref() {
            Some(region) => conflict(region).or_else(|| region_conflict(inner, Some(region))),
            None => region_conflict(inner, inside),
        },
        Type::Text(Some(region)) | Type::Named(_, Some(region)) => conflict(region),
        Type::Applied(applied) => match applied.region.as_deref() {
            Some(region) => conflict(region).or_else(|| applied.types.iter().find_map(|ty| region_conflict(ty, Some(region)))),
            None => applied.types.iter().find_map(|ty| region_conflict(ty, inside)),
        },
        Type::Maybe(inner) | Type::Array(inner, _) | Type::Fail(inner) | Type::Range(inner) => region_conflict(inner, inside),
        Type::Or(value, error) => region_conflict(value, inside).or_else(|| region_conflict(error, inside)),
        Type::Tuple(types) | Type::Results(types) => types.iter().find_map(|ty| region_conflict(ty, inside)),
        _ => None,
    }
}

/// How deeply a type is built up: `int` is 1, `maybe int` 2, `(int, maybe int)` 3.
fn type_depth(ty: &Type) -> usize {
    let inner = match ty {
        Type::Ptr(inner, _) | Type::Slice(inner, _) | Type::Array(inner, _) | Type::Range(inner) | Type::Maybe(inner) | Type::Fail(inner) => {
            type_depth(inner)
        }
        Type::Or(value, error) => type_depth(value).max(type_depth(error)),
        Type::Applied(applied) => applied.types.iter().map(type_depth).max().unwrap_or(0),
        Type::Tuple(types) | Type::Results(types) => types.iter().map(type_depth).max().unwrap_or(0),
        _ => 0,
    };
    inner + 1
}

/// The regions a task's header names: a `r: region` parameter, and the regions
/// written in its parameters' and results' types.
fn header_regions(task: &TaskDef) -> HashSet<String> {
    let mut regions: HashSet<String> = task.params.iter().filter(|param| param.ty == Type::Region).map(|param| param.name.clone()).collect();
    for ty in task.params.iter().map(|param| &param.ty).chain(task.ret.results()) {
        regions.extend(ty.regions().into_iter().map(str::to_string));
    }
    regions
}

/// For each type a task takes, the regions its header keeps values of that type
/// in: `xs: [T]@r` and `-> ^T@r` keep a `T` in `r`, and a use must give a type of
/// that region, since everything a value in a region holds is in that region.
fn keeps_params(task: &TaskDef) -> HashMap<String, HashSet<String>> {
    let mut kept: HashMap<String, HashSet<String>> = HashMap::new();
    for ty in task.params.iter().map(|param| &param.ty).chain(task.ret.results()) {
        keeps_params_of(ty, None, &mut kept);
    }
    kept
}

fn keeps_params_of(ty: &Type, inside: Option<&str>, kept: &mut HashMap<String, HashSet<String>>) {
    match ty {
        Type::Param(name) => {
            if let Some(region) = inside {
                kept.entry(name.clone()).or_default().insert(region.to_string());
            }
        }
        Type::Ptr(inner, region) | Type::Slice(inner, region) => keeps_params_of(inner, region.as_deref().or(inside), kept),
        Type::Array(inner, _) | Type::Range(inner) | Type::Maybe(inner) | Type::Fail(inner) => keeps_params_of(inner, inside, kept),
        Type::Or(value, error) => {
            keeps_params_of(value, inside, kept);
            keeps_params_of(error, inside, kept);
        }
        Type::Tuple(types) | Type::Results(types) => types.iter().for_each(|ty| keeps_params_of(ty, inside, kept)),
        _ => {}
    }
}

/// What walking a generic task's body turns up: the uses of generic tasks in it
/// that need tasks of their own, and the copies it makes of values whose types
/// name the types it takes (see `Checker::loose_copies`).
#[derive(Default)]
struct Made {
    needed: Vec<(String, Vec<Type>, Span)>,
    copies: Vec<(String, String, Span)>,
}

impl Checker {
    /// Puts the types `given` in place of the type parameters of `task`, a copy
    /// of a generic task made for one use, points its calls of generic tasks at
    /// the ones made for them, and puts the shape made for its types in place of
    /// every shape given them (see `Made` and `settle_applications`). With no
    /// types given it changes nothing, and only fills `made`, which is how the
    /// checker reads a generic task's copies.
    fn substitute_task(&mut self, task: &mut TaskDef, given: &HashMap<String, Type>, made: &mut Made) -> Result<()> {
        for param in &mut task.params {
            self.substitute_type(&mut param.ty, given, param.ty_span)?;
        }
        let at = task.span;
        self.substitute_type(&mut task.ret, given, at)?;
        self.substitute_block(&mut task.body, given, made)
    }

    /// One type of a task made for a set of types: the types given in place of
    /// the names it takes, and the shape made for them in place of a shape given
    /// them.
    fn substitute_type(&mut self, ty: &mut Type, given: &HashMap<String, Type>, at: Span) -> Result<()> {
        *ty = ty.map_params(&mut |name| given.get(name).cloned());
        self.settle_applications(ty, at)
    }

    fn substitute_block(&mut self, block: &mut Block, given: &HashMap<String, Type>, made: &mut Made) -> Result<()> {
        block.iter_mut().try_for_each(|stmt| self.substitute_stmt(stmt, given, made))
    }

    fn substitute_stmt(&mut self, stmt: &mut Stmt, given: &HashMap<String, Type>, made: &mut Made) -> Result<()> {
        let at = stmt.span;
        match &mut stmt.kind {
            StmtKind::Expr(e) => self.substitute_expr(e, given, made),
            StmtKind::Send(values) | StmtKind::SetResults(_, values) => {
                values.iter_mut().try_for_each(|value| self.substitute_expr(value, given, made))
            }
            StmtKind::Region { .. } => Ok(()),
            StmtKind::Binding { names, init, .. } => {
                for bound in names.iter_mut() {
                    self.substitute_bound(bound, given, at)?;
                }
                init.iter_mut().try_for_each(|value| self.substitute_expr(value, given, made))
            }
            StmtKind::Assign(targets, values) => {
                for target in targets.iter_mut() {
                    self.substitute_target(target, given, made)?;
                }
                values.iter_mut().try_for_each(|value| self.substitute_expr(value, given, made))
            }
            StmtKind::When(cond, then_block, else_block) => {
                self.substitute_expr(&mut cond.value, given, made)?;
                self.substitute_block(then_block, given, made)?;
                match else_block {
                    Some(else_block) => self.substitute_block(else_block, given, made),
                    None => Ok(()),
                }
            }
            StmtKind::Block(body) => self.substitute_block(body, given, made),
            StmtKind::Loop(cond, body) => {
                self.substitute_expr(&mut cond.value, given, made)?;
                self.substitute_block(body, given, made)
            }
            StmtKind::Each(_, range, body) => {
                self.substitute_expr(range, given, made)?;
                self.substitute_block(body, given, made)
            }
            StmtKind::Match(subject, cases) => {
                self.substitute_expr(subject, given, made)?;
                cases.iter_mut().try_for_each(|case| self.substitute_block(&mut case.body, given, made))
            }
        }
    }

    fn substitute_bound(&mut self, bound: &mut Bound, given: &HashMap<String, Type>, at: Span) -> Result<()> {
        if let Some(ty) = &mut bound.ty {
            *ty = ty.map_params(&mut |name| given.get(name).cloned());
            self.settle_applications(ty, at)?;
        }
        bound.parts.iter_mut().try_for_each(|part| self.substitute_bound(part, given, at))
    }

    fn substitute_target(&mut self, target: &mut Target, given: &HashMap<String, Type>, made: &mut Made) -> Result<()> {
        match target {
            Target::Discard(_) => Ok(()),
            Target::Place(place) => self.substitute_expr(place, given, made),
            Target::Tuple(parts, _) => parts.iter_mut().try_for_each(|part| self.substitute_target(part, given, made)),
        }
    }

    fn substitute_expr(&mut self, e: &mut Expr, given: &HashMap<String, Type>, made: &mut Made) -> Result<()> {
        // A copy of a value whose type names a type the task takes, read before
        // the types are put in place of the names: the copy is a value of the
        // region it goes into, so a use must give a type that lives there (see
        // `Checker::loose_copies`).
        if let ExprKind::Copy { region: Some(region), .. } = &e.kind
            && let Some(ty) = &e.ty
        {
            for param in ty.params() {
                made.copies.push((param.to_string(), region.clone(), e.span));
            }
        }
        let at = e.span;
        if let Some(ty) = &mut e.ty {
            *ty = ty.map_params(&mut |name| given.get(name).cloned());
            self.settle_applications(ty, at)?;
        }
        match &mut e.kind {
            ExprKind::Call { name, types, args } => {
                for (ty, _) in types.iter_mut() {
                    self.substitute_type(ty, given, at)?;
                }
                if !types.is_empty() {
                    made.needed.push((name.clone(), types.iter().map(|(ty, _)| ty.clone()).collect(), at));
                }
                args.iter_mut().try_for_each(|arg| self.substitute_expr(arg, given, made))
            }
            // A literal of a shape given the types this task takes becomes a
            // literal of the shape made for them (see `shape_instance`).
            ExprKind::StructInit { name, types, fields } => {
                for (ty, _) in types.iter_mut() {
                    self.substitute_type(ty, given, at)?;
                }
                if !types.is_empty() && types.iter().all(|(ty, _)| ty.params().is_empty()) {
                    let made_types: Vec<Type> = types.iter().map(|(ty, _)| ty.clone()).collect();
                    *name = self.shape_instance(&name.clone(), &made_types, at)?;
                    types.clear();
                }
                fields.iter_mut().try_for_each(|field| self.substitute_expr(&mut field.value, given, made))
            }
            ExprKind::New { ty, ty_span, value, .. } => {
                let ty_span = *ty_span;
                self.substitute_type(ty, given, ty_span)?;
                self.substitute_expr(value, given, made)
            }
            ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) | ExprKind::NoneLit | ExprKind::Var(_) | ExprKind::RegionAfter(_) | ExprKind::Shared { .. } => Ok(()),
            ExprKind::Unary(_, inner)
            | ExprKind::Deref(inner)
            | ExprKind::Ref(inner)
            | ExprKind::Field(inner, _)
            | ExprKind::Fail(inner)
            | ExprKind::Wrap(inner)
            | ExprKind::Copy { value: inner, .. }
            | ExprKind::ToText { value: inner, .. } => self.substitute_expr(inner, given, made),
            ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) | ExprKind::Range(a, b) | ExprKind::Repeat { value: a, count: b } => {
                self.substitute_expr(a, given, made)?;
                self.substitute_expr(b, given, made)
            }
            ExprKind::Construct { args, .. }
            | ExprKind::Array(args)
            | ExprKind::Tuple(args)
            | ExprKind::Scan { args, .. }
            | ExprKind::Join { parts: args, .. } => args.iter_mut().try_for_each(|arg| self.substitute_expr(arg, given, made)),
            ExprKind::PlanB { value, body, .. } => {
                self.substitute_expr(value, given, made)?;
                self.substitute_block(body, given, made)
            }
            // The same statement a `match` or a `when` on its own is.
            ExprKind::Choice(stmt) => self.substitute_stmt(stmt, given, made),
            ExprKind::Edit { items, args, .. } => {
                self.substitute_expr(items, given, made)?;
                args.iter_mut().try_for_each(|arg| self.substitute_expr(arg, given, made))
            }
            // Every method call became the call of the task the method is while
            // the body was checked (see `check_method_call`).
            ExprKind::MethodCall { name, .. } => Err(Error::internal_at(at, format!("the method call '{}' was never rewritten", name))),
        }
    }
}

/// Records what each type parameter of `pattern` stands for in `actual`, the
/// type of a value given where `pattern` is expected. A parameter takes the
/// first type found for it; `none`, which fits any `maybe`, names nothing.
fn unify_params(pattern: &Type, actual: &Type, given: &mut HashMap<String, Type>) {
    if *actual == Type::None || matches!(actual, Type::Fail(_)) {
        return;
    }
    match (pattern, actual) {
        (Type::Param(name), _) => {
            given.entry(name.clone()).or_insert_with(|| actual.clone());
        }
        (Type::Ptr(p, _), Type::Ptr(a, _)) | (Type::Slice(p, _), Type::Slice(a, _)) | (Type::Array(p, _), Type::Array(a, _)) => {
            unify_params(p, a, given)
        }
        // A shape given types: the same shape, whose types stand for each other
        // (a value of a shape made for its types is read as one, see
        // `Checker::unmade`).
        (Type::Applied(p), Type::Applied(a)) if p.name == a.name && p.types.len() == a.types.len() => {
            p.types.iter().zip(&a.types).for_each(|(p, a)| unify_params(p, a, given));
        }
        (Type::Tuple(p), Type::Tuple(a)) | (Type::Results(p), Type::Results(a)) if p.len() == a.len() => {
            p.iter().zip(a).for_each(|(p, a)| unify_params(p, a, given))
        }
        (Type::Maybe(p), Type::Maybe(a)) | (Type::Range(p), Type::Range(a)) | (Type::Fail(p), Type::Fail(a)) => unify_params(p, a, given),
        (Type::Or(pv, pe), Type::Or(av, ae)) => {
            unify_params(pv, av, given);
            unify_params(pe, ae, given);
        }
        // A plain value given where a `maybe` or a `T ~ E` is expected becomes
        // one, so it names the type inside.
        (Type::Maybe(p), a) | (Type::Or(p, _), a) => unify_params(p, a, given),
        _ => {}
    }
}

pub fn unify_regions(pattern: &Type, actual: &Type, regions: &mut HashMap<String, String>) {
    let mut record = |p: &Option<String>, a: &Option<String>| {
        if let (Some(p), Some(a)) = (p, a)
            && a != ANY_REGION
        {
            regions.entry(p.clone()).or_insert_with(|| a.clone());
        }
    };
    match (pattern, actual) {
        (Type::Ptr(p, pr), Type::Ptr(a, ar)) | (Type::Slice(p, pr), Type::Slice(a, ar)) => {
            record(pr, ar);
            unify_regions(p, a, regions);
        }
        (Type::Array(p, _), Type::Array(a, _)) => unify_regions(p, a, regions),
        (Type::Tuple(p), Type::Tuple(a)) => p.iter().zip(a).for_each(|(p, a)| unify_regions(p, a, regions)),
        (Type::Named(p, pr), Type::Named(a, ar)) if p == a => record(pr, ar),
        (Type::Applied(p), Type::Applied(a)) if p.name == a.name && p.types.len() == a.types.len() => {
            record(&p.region, &a.region);
            p.types.iter().zip(&a.types).for_each(|(p, a)| unify_regions(p, a, regions));
        }
        (Type::Text(pr), Type::Text(ar)) => record(pr, ar),
        (Type::Maybe(p), Type::Maybe(a)) | (Type::Range(p), Type::Range(a)) | (Type::Fail(p), Type::Fail(a)) => unify_regions(p, a, regions),
        (Type::Or(pv, pe), Type::Or(av, ae)) => {
            unify_regions(pv, av, regions);
            unify_regions(pe, ae, regions);
        }
        (Type::Or(_, pe), Type::Fail(a)) => unify_regions(pe, a, regions),
        (Type::Or(pv, _), a) => unify_regions(pv, a, regions),
        (Type::Maybe(p), a) => unify_regions(p, a, regions),
        _ => {}
    }
}

/// Whether a value of type `ty` holds a pointer, given the shapes and kinds
/// in `regional` that do.
fn holds_pointers(ty: &Type, regional: &HashSet<String>) -> bool {
    match ty {
        Type::Ptr(..) | Type::Slice(..) => true,
        Type::Named(name, _) => regional.contains(name),
        // A shape given types holds what its own fields hold and what the types
        // given hold, since a field may be one of them.
        Type::Applied(applied) => {
            regional.contains(&applied.name) || applied.types.iter().any(|ty| holds_pointers(ty, regional))
        }
        Type::Maybe(inner) | Type::Array(inner, _) => holds_pointers(inner, regional),
        Type::Or(value, error) => holds_pointers(value, regional) || holds_pointers(error, regional),
        Type::Tuple(items) => items.iter().any(|item| holds_pointers(item, regional)),
        _ => false,
    }
}

/// Whether `==` compares values of type `ty`: numbers, bools, text, pointers,
/// and tuples of them.
fn comparable(ty: &Type) -> bool {
    match ty {
        Type::Int | Type::Float | Type::Bool | Type::Text(_) | Type::Ptr(..) => true,
        Type::Tuple(items) => items.iter().all(comparable),
        _ => false,
    }
}

/// Whether a value of type `ty` holds a pointer, a slice, or an array, whose
/// value can change, given the shapes and kinds in `changeable` that do.
fn holds_changeable(ty: &Type, changeable: &HashSet<String>) -> bool {
    match ty {
        Type::Ptr(..) | Type::Slice(..) | Type::Array(..) => true,
        Type::Named(name, _) => changeable.contains(name),
        Type::Applied(applied) => {
            changeable.contains(&applied.name) || applied.types.iter().any(|ty| holds_changeable(ty, changeable))
        }
        Type::Maybe(inner) => holds_changeable(inner, changeable),
        Type::Or(value, error) => holds_changeable(value, changeable) || holds_changeable(error, changeable),
        Type::Tuple(items) => items.iter().any(|item| holds_changeable(item, changeable)),
        _ => false,
    }
}

/// Where the region written after a call is (`f(x) at r`), when the parser
/// added one as the call's last argument (see `ExprKind::RegionAfter`).
fn region_after(args: &[Expr]) -> Option<Span> {
    args.last().filter(|arg| matches!(arg.kind, ExprKind::RegionAfter(_))).map(|arg| arg.span)
}

/// Whether `expr` is a value the compiler works out without running the program,
/// which is what a field falls back on (see `check_field_default`): a literal,
/// `none`, a number with a sign, a variant, or a tuple, an array, or a shape's
/// literal whose parts are all of these. Checked after the expression is, so a
/// variant is already a `Construct` rather than a name. Nothing here reads a
/// variable, calls a task, or makes memory, so a literal that leaves a field out
/// costs no more than one that writes the value.
/// A refusal whose message `make` builds, apart from the function it is in:
/// a message's pieces then take no room on the stack of a function that nests,
/// as the checking of a literal in a literal does.
#[inline(never)]
fn refuse<T>(make: impl FnOnce() -> Error) -> Result<T> {
    Err(make())
}

fn constant_value(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) | ExprKind::NoneLit => true,
        ExprKind::Unary(_, inner) => constant_value(inner),
        ExprKind::Array(values) | ExprKind::Tuple(values) => values.iter().all(constant_value),
        ExprKind::Construct { args, .. } => args.iter().all(constant_value),
        ExprKind::Repeat { value, count } => constant_value(value) && constant_value(count),
        ExprKind::StructInit { fields, .. } => fields.iter().all(|field| constant_value(&field.value)),
        _ => false,
    }
}

/// Whether a value of type `ty` holds text by value, given the shapes and kinds
/// in `textual` that do.
fn holds_text(ty: &Type, textual: &HashSet<String>) -> bool {
    match ty {
        Type::Text(_) => true,
        Type::Named(name, _) => textual.contains(name),
        Type::Applied(applied) => {
            textual.contains(&applied.name) || applied.types.iter().any(|ty| holds_text(ty, textual))
        }
        Type::Maybe(inner) | Type::Array(inner, _) => holds_text(inner, textual),
        Type::Or(value, error) => holds_text(value, textual) || holds_text(error, textual),
        Type::Tuple(items) => items.iter().any(|item| holds_text(item, textual)),
        _ => false,
    }
}

/// Whether `ty` still holds `none` or `fail …` where a type belongs, as the
/// array `[none, none]` does until a type written for it settles what it holds.
fn unsettled(ty: &Type) -> bool {
    match ty {
        Type::None | Type::Fail(_) => true,
        Type::Array(items, _) => unsettled(items),
        Type::Tuple(items) => items.iter().any(unsettled),
        _ => false,
    }
}

/// `ty` with a type in place of each `none` and `fail e`, for a message
/// suggesting an annotation: `[maybe int; 2]` for `[none; 2]`.
fn settled_example(ty: &Type) -> Type {
    match ty {
        Type::None => Type::Maybe(Box::new(Type::Int)),
        Type::Fail(error) => Type::Or(Box::new(Type::Int), error.clone()),
        Type::Array(items, count) => Type::Array(Box::new(settled_example(items)), *count),
        Type::Tuple(items) => Type::Tuple(items.iter().map(settled_example).collect()),
        other => other.clone(),
    }
}

/// The type the values of types `a` and `b` can both become, where several
/// values must have one type: the items of an array, and the branches of a
/// `match` or a `when` used as a value. Either type, when the other fits it; a
/// `maybe` for `none` and a value; a `T ~ E` for `fail e` and a value. `None`
/// when there is none, which is the mistake of two values that disagree.
///
/// A value already made is never changed inside: a `maybe` or a `T ~ E` is
/// wrapped **around** it, which is all `coerce` can do to a value it cannot see
/// the parts of. So `int ~ text` beside `none` is `maybe (int ~ text)`, the
/// failure kept as it is inside a `maybe`, and not `(maybe int) ~ text`, which
/// would mean reaching into the value to make its `int` a `maybe int`. A `maybe`
/// or a `T ~ E` whose own inside would have to change joins with nothing, and the
/// two values are reported as disagreeing. An array or a tuple is different: its
/// items are values of their own, so joining them item by item asks nothing of a
/// value that is already made — a written one is built item by item (see
/// `Checker::coerce_items`), and one that is already made is refused by `coerce`,
/// which says to build a new one.
fn join_types(a: &Type, b: &Type) -> Option<Type> {
    if fits_regions(b, a) {
        return Some(a.clone());
    }
    if fits_regions(a, b) {
        return Some(b.clone());
    }
    let plain = |ty: &Type| !matches!(ty, Type::None | Type::Fail(_) | Type::Unit | Type::Maybe(_) | Type::Or(..));
    match (a, b) {
        (Type::Array(x, n), Type::Array(y, m)) if n == m => join_types(x, y).map(|items| Type::Array(Box::new(items), *n)),
        (Type::Tuple(x), Type::Tuple(y)) if x.len() == y.len() => x.iter().zip(y).map(|(x, y)| join_types(x, y)).collect::<Option<Vec<_>>>().map(Type::Tuple),
        (Type::None, Type::Maybe(_)) => Some(b.clone()),
        (Type::Maybe(_), Type::None) => Some(a.clone()),
        (Type::None, other) | (other, Type::None) if plain(other) => Some(Type::Maybe(Box::new(other.clone()))),
        // `none` beside a value that can fail: the `T ~ E` goes inside the
        // `maybe`, whole, since nothing can reach into it.
        (Type::None, Type::Or(..)) => Some(Type::Maybe(Box::new(b.clone()))),
        (Type::Or(..), Type::None) => Some(Type::Maybe(Box::new(a.clone()))),
        (Type::Maybe(inner), other) | (other, Type::Maybe(inner)) if plain(other) => join_types(inner, other)
            .filter(plain)
            // The maybe's own value stays as it is: only the other side is made
            // into it, or built as it.
            .filter(|joined| fits_regions(inner, joined))
            .map(|value| Type::Maybe(Box::new(value))),
        (Type::Fail(error), Type::Or(_, known)) | (Type::Or(_, known), Type::Fail(error)) if fits_regions(error, known) => {
            Some(if matches!(a, Type::Or(..)) { a.clone() } else { b.clone() })
        }
        (Type::Fail(error), other) | (other, Type::Fail(error)) if plain(other) || matches!(other, Type::Maybe(_)) => {
            (!fits_regions(other, error) && !fits_regions(error, other)).then(|| Type::Or(Box::new(other.clone()), error.clone()))
        }
        (Type::Or(value, error), other) | (other, Type::Or(value, error)) if !matches!(other, Type::Fail(_)) => join_types(value, other)
            .filter(|joined| !matches!(joined, Type::Or(..)))
            // As above: the success it already holds is not remade.
            .filter(|joined| fits_regions(value, joined))
            .map(|joined| Type::Or(Box::new(joined), error.clone())),
        _ => None,
    }
}

/// The region of the array `place` names, when it is in one: behind a
/// pointer, or an item of a slice. `None` for an array in a variable or a
/// temporary value.
fn place_region(place: &Expr) -> Option<String> {
    match &place.kind {
        ExprKind::Deref(pointer) => match pointer.ty.as_ref()? {
            Type::Ptr(_, region) => region.clone(),
            _ => None,
        },
        ExprKind::Field(base, _) => place_region(base),
        ExprKind::Index(base, _) => match base.ty.as_ref()? {
            Type::Slice(_, region) => region.clone(),
            _ => place_region(base),
        },
        _ => None,
    }
}

/// At most how many bytes a value of type `ty` takes in C, given `sizes` for
/// shapes and kinds: each part counts at least 8 bytes, which covers padding.
fn value_bytes(ty: &Type, sizes: &HashMap<String, u128>) -> u128 {
    match ty {
        Type::Unit => 0,
        Type::Named(name, _) => sizes.get(name).copied().unwrap_or(8),
        // A shape given types is as large as the shape itself, whose fields of a
        // type it takes are counted as a word each (see `check_sizes`).
        Type::Applied(applied) => sizes.get(&applied.name).copied().unwrap_or(8),
        Type::Array(items, count) => value_bytes(items, sizes).saturating_mul(u128::from(*count)),
        Type::Slice(..) => 24,
        Type::Maybe(inner) => value_bytes(inner, sizes).saturating_add(8),
        Type::Or(value, error) => value_bytes(value, sizes).saturating_add(value_bytes(error, sizes)).saturating_add(8),
        Type::Fail(inner) | Type::Range(inner) => value_bytes(inner, sizes),
        Type::Results(types) | Type::Tuple(types) => types.iter().map(|ty| value_bytes(ty, sizes).max(8)).fold(0, u128::saturating_add),
        _ => 8,
    }
}

/// At most how many bytes a shape's fields or a variant's payload take.
fn fields_bytes(fields: &[Field], sizes: &HashMap<String, u128>) -> u128 {
    fields.iter().map(|field| value_bytes(&field.ty, sizes).max(8)).fold(8, u128::saturating_add)
}

/// Whether a task with return type `ret` must send a value: every return type
/// except `unit` and `unit ~ E`, which succeed by reaching their end.
fn sends_value(ret: &Type) -> bool {
    !matches!(ret, Type::Unit) && !matches!(ret, Type::Or(value, _) if **value == Type::Unit)
}

/// The shapes and kinds a value of type `ty` contains by value (not behind a
/// pointer).
fn contained_by_value(ty: &Type) -> Vec<&str> {
    match ty {
        Type::Named(name, _) => vec![name.as_str()],
        // A shape given types contains that shape by value: a field of it
        // naming the same shape again is a cycle either way.
        Type::Applied(applied) => {
            let mut names = vec![applied.name.as_str()];
            names.extend(applied.types.iter().flat_map(contained_by_value));
            names
        }
        Type::Maybe(inner) | Type::Array(inner, _) => contained_by_value(inner),
        Type::Tuple(items) => items.iter().flat_map(contained_by_value).collect(),
        Type::Or(value, error) => {
            let mut names = contained_by_value(value);
            names.extend(contained_by_value(error));
            names
        }
        _ => Vec::new(),
    }
}

/// Every shape or kind a type names, by value or through pointers.
fn named_types(ty: &Type) -> Vec<&str> {
    match ty {
        Type::Named(name, _) => vec![name.as_str()],
        Type::Applied(applied) => {
            let mut names = vec![applied.name.as_str()];
            names.extend(applied.types.iter().flat_map(named_types));
            names
        }
        Type::Ptr(inner, _) | Type::Maybe(inner) | Type::Fail(inner) | Type::Array(inner, _) | Type::Slice(inner, _) => named_types(inner),
        Type::Tuple(items) => items.iter().flat_map(named_types).collect(),
        Type::Or(value, error) => {
            let mut names = named_types(value);
            names.extend(named_types(error));
            names
        }
        _ => Vec::new(),
    }
}

/// Whether a value of type `ty` can be created, given the shapes and kinds in
/// `can` that can be.
fn constructible(ty: &Type, can: &HashSet<&str>) -> bool {
    match ty {
        Type::Named(name, _) => can.contains(name.as_str()),
        Type::Applied(applied) => can.contains(applied.name.as_str()),
        Type::Ptr(inner, _) | Type::Array(inner, _) | Type::Slice(inner, _) => constructible(inner, can),
        Type::Or(value, error) => **value == Type::Unit || constructible(value, can) || constructible(error, can),
        Type::Tuple(items) => items.iter().all(|item| constructible(item, can)),
        _ => true,
    }
}

/// Whether running `block` always ends in `<-`: it contains a `<-`, a
/// `when` whose two branches both always send, a `when` that always holds and
/// whose block sends, a `match` whose cases all send, or a `block` that sends. Loops don't count,
/// since their bodies might not run.
fn always_sends(block: &Block) -> bool {
    block.iter().any(|stmt| match &stmt.kind {
        StmtKind::Send(_) => true,
        StmtKind::When(_, then_block, Some(else_block)) => always_sends(then_block) && always_sends(else_block),
        StmtKind::When(cond, then_block, None) => always_holds(cond) && always_sends(then_block),
        StmtKind::Match(_, cases) => cases.iter().all(|case| always_sends(&case.body)),
        StmtKind::Block(body) => always_sends(body),
        _ => false,
    })
}

/// Whether a condition always holds: `is (x, y)`, whose items always match.
/// (Every other condition may be false, so its block may not run.)
fn always_holds(cond: &Condition) -> bool {
    matches!(&cond.pattern, Some(pattern) if matches!(pattern.kind, PatternKind::Tuple(_)))
}

/// The value of an int expression written with literals only (`-5`,
/// `60 * 60 * 24`), which the program computes the same way every time, or
/// `None` for any other expression. Its parts have been checked, so each of
/// them fits in an `int`.
fn constant_int(e: &Expr) -> Option<i64> {
    match &e.kind {
        ExprKind::IntLit(n) => Some(*n),
        ExprKind::Unary(UnaryOp::Neg, operand) => constant_int(operand)?.checked_neg(),
        ExprKind::Binary(op, l, r) => {
            let (a, b) = (constant_int(l)?, constant_int(r)?);
            match op {
                BinaryOp::Add => a.checked_add(b),
                BinaryOp::Sub => a.checked_sub(b),
                BinaryOp::Mul => a.checked_mul(b),
                BinaryOp::Div => a.checked_div(b),
                // The remainder of a division by -1 is 0, which Rust's
                // `checked_rem` leaves out for the smallest int.
                BinaryOp::Mod if b == -1 => Some(0),
                BinaryOp::Mod => a.checked_rem(b),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Rejects int arithmetic the program would stop on every time it runs: a
/// division or remainder by a constant zero, and a constant whose result
/// doesn't fit in an `int`. Anything else is checked as the program runs.
fn check_constant_arithmetic(op: BinaryOp, l: &Expr, r: &Expr, span: Span) -> Result<()> {
    let divisor = constant_int(r);
    if divisor == Some(0) {
        match op {
            BinaryOp::Div => return err(span, "division by zero"),
            BinaryOp::Mod => return err(span, "remainder of division by zero"),
            _ => {}
        }
    }
    let (Some(a), Some(b)) = (constant_int(l), divisor) else { return Ok(()) };
    let (a, b) = (i128::from(a), i128::from(b));
    let exact = match op {
        BinaryOp::Add => a + b,
        BinaryOp::Sub => a - b,
        BinaryOp::Mul => a * b,
        BinaryOp::Div => a / b,
        _ => return Ok(()),
    };
    fits_int(exact, span)
}

/// Rejects `value`, the exact result of a constant int operation at `span`,
/// when it doesn't fit in an `int`.
fn fits_int(value: i128, span: Span) -> Result<()> {
    if value > i128::from(i64::MAX) {
        return err(span, format!("integer overflow: the result, {}, is more than an int holds (at most {})", value, i64::MAX));
    }
    if value < i128::from(i64::MIN) {
        return err(span, format!("integer overflow: the result, {}, is less than an int holds (at least {})", value, i64::MIN));
    }
    Ok(())
}

/// Why a value of type `actual` doesn't fit where `expected` is required, or
/// nothing when there is nothing useful to say.
fn mismatch_hint(actual: &Type, expected: &Type) -> String {
    match (actual, expected) {
        (Type::None, Type::Ptr(..)) => format!("; a pointer can't be none, so use 'maybe {}'", expected),
        (Type::None, Type::Slice(..)) => format!("; a slice can't be none, so use 'maybe {}'", expected),
        (Type::None, Type::Or(value, _)) if value.ptr_inner().is_some() => {
            format!("; a pointer can't be none, so use 'maybe {}'", value)
        }
        (_, Type::Or(_, error)) if fits_regions(actual, error) => "; an error is given with 'fail', as in '<- fail e'".to_string(),
        (Type::Fail(_), _) => "; 'fail …' is an error, which only a 'T ~ E' can hold".to_string(),
        (Type::Maybe(inner) | Type::Or(inner, _), _) if fits_regions(inner, expected) => {
            "; extract its value first, as in 'x { … }' or 'when x is {value}'".to_string()
        }
        (Type::Text(Some(region)), Type::Text(None)) => {
            format!("; this text is in region '{}', and text here must last as long as the program, as a literal does", region)
        }
        (Type::Text(Some(region)), Type::Text(Some(other))) => format!("; this text is in region '{}', and text here is in region '{}'", region, other),
        // Pointers or text: what a shape or kind holds is in its region.
        (Type::Named(name, Some(region)), Type::Named(expected_name, Some(other))) if name == expected_name && region != ANY_REGION && other != ANY_REGION => {
            format!("; a value can't move to another region: what it holds in region '{}' is never stored in region '{}'", region, other)
        }
        (Type::Named(name, Some(region)), Type::Named(expected_name, None)) if name == expected_name => format!(
            "; this value holds text in region '{0}', and a '{1}' here holds text that lasts as long as the program; to keep text of a region, name it, as in '{1}@{0}'",
            region, name
        ),
        _ if [Some(expected), expected.extracted()].into_iter().flatten().any(|e| actual.without_regions() == e.without_regions()) => {
            // Text, or a shape or kind, inside what is given says it best.
            let inner = [Some(expected), expected.extracted()]
                .into_iter()
                .flatten()
                .find_map(|e| region_mismatch(actual, e))
                .filter(|(a, e)| !std::ptr::eq(*a, actual) || !std::ptr::eq(*e, expected))
                .map(|(a, e)| mismatch_hint(a, e))
                .filter(|hint| !hint.is_empty());
            if let Some(hint) = inner {
                return hint;
            }
            if expected.regions().contains(&ANY_REGION) {
                "; this place holds a value without pointers (of no region), so it can't take a pointer into one".to_string()
            } else {
                "; a value can't move to another region: a pointer into one region is never stored in another".to_string()
            }
        }
        // The same shape, differing in a part inside it: a value is wrapped as
        // part of a `maybe` or a `T ~ E`, but a value already made is never
        // reached into, so it is built again from its parts (see `join_types`).
        _ if remade_inside(actual, expected) => {
            let parts = match actual {
                Type::Tuple(items) => format!(", as in '({})'", (0..items.len()).map(|i| format!("v.{}", i)).collect::<Vec<_>>().join(", ")),
                Type::Maybe(_) | Type::Or(..) => ", taking its value out first, as in 'v { … }'".to_string(),
                _ => String::new(),
            };
            format!("; a value already made is never changed inside, so build one that is what this place holds{}", parts)
        }
        _ => String::new(),
    }
}

/// The first text, or shape or kind, in `actual` whose region differs from the
/// region of the one in the same place in `expected`, with that one.
fn region_mismatch<'a>(actual: &'a Type, expected: &'a Type) -> Option<(&'a Type, &'a Type)> {
    match (actual, expected) {
        (Type::Text(a), Type::Text(e)) | (Type::Named(_, a), Type::Named(_, e)) => (a != e).then_some((actual, expected)),
        (Type::Maybe(a), Type::Maybe(e)) | (Type::Array(a, _), Type::Array(e, _)) | (Type::Fail(a), Type::Fail(e)) => region_mismatch(a, e),
        (Type::Or(av, ae), Type::Or(ev, ee)) => region_mismatch(av, ev).or_else(|| region_mismatch(ae, ee)),
        (Type::Tuple(a), Type::Tuple(e)) => a.iter().zip(e).find_map(|(a, e)| region_mismatch(a, e)),
        _ => None,
    }
}

/// Where the values a list receives come from when one value gives them all:
/// the several a call sends, or the items of one tuple.
#[derive(Clone, Copy, PartialEq)]
enum Given {
    Sent,
    Items,
}

/// Whether one value gives them all by being a tuple taken apart, rather than
/// a call sending several.
fn given_by(values: &[Expr]) -> Given {
    match values {
        [only] if matches!(only.ty, Some(Type::Tuple(_))) => Given::Items,
        _ => Given::Sent,
    }
}

/// Checks that a value of type `actual`, one of the several a call sends or the
/// items of a tuple (`given`), fits where `expected` is required. Nothing can
/// change it on the way, so unlike a single value it never becomes a `maybe` or
/// a `T ~ E` here.
fn fits_or_err(actual: &Type, expected: &Type, at: Span, given: Given) -> Result<()> {
    if fits_regions(actual, expected) {
        return Ok(());
    }
    let wrapping = match expected {
        Type::Maybe(inner) => *actual == Type::None || fits_regions(actual, inner),
        Type::Or(value, error) => match actual {
            Type::Fail(given) => fits_regions(given, error),
            _ => fits_regions(actual, value),
        },
        _ => false,
    };
    let hint = if wrapping && given == Given::Items {
        format!("; a tuple's items are taken as they are, so an item doesn't become a {} on the way", expected)
    } else if wrapping {
        format!("; a task sends each of its values as it is, so it doesn't become a {} on the way", expected)
    } else {
        mismatch_hint(actual, expected)
    };
    err(at, format!("expected type {}, got {}{}", expected, actual, hint))
}

/// What `scan` takes, for a call that takes something else.
const SCAN_TAKES: &str = "'scan' takes the prompt it shows, and the type of a number to read, as in 'scan(\"Name: \") at r' or 'scan(\"Age: \", int)'";

/// The type a typed `scan` names, `int` or `float`: the number it reads. It is a
/// type, not a value, so it is never checked as one.
fn scanned_type(arg: &Expr) -> Result<Type> {
    if let ExprKind::Var(name) = &arg.kind {
        if let Some(number) = number_cast(name) {
            return Ok(number);
        }
        // Text is what `scan` reads by itself, so naming it says to leave it out.
        if matches!(Type::builtin(name), Some(Type::Text(_))) {
            return err(arg.span, "'scan' reads text on its own, so 'text' is left out: write 'scan(\"Name: \") at r'");
        }
    }
    err(arg.span, "'scan' reads a number, so it names 'int' or 'float', as in 'scan(\"Age: \", int)'")
}

/// The error for a call whose `T ~ E` result is thrown away.
fn ignored_error(task: &str) -> String {
    // A method is called on a value, and written so.
    if let Some((owner, method)) = method_parts(task) {
        return format!(
            "the error '{0}{1}{2}' may send is ignored; handle it with a plan B, as in 'value.{2}(…) {{ failure e … }}', or with match",
            owner, MODULE_SEPARATOR, method
        );
    }
    format!("the error '{0}' may send is ignored; handle it with a plan B, as in '{0}(…) {{ failure e … }}', or with match", task)
}

/// The error for a `_` that would drop a value holding an error.
fn dropped_error(ty: &Type) -> String {
    format!("'_' would drop the error in a value of type {}; give it a name, then handle it with a plan B or match", ty)
}

/// Example names for a list of `n` values, as in `bind a, b = …`.
fn example_names(n: usize) -> String {
    let names: Vec<String> = (0..n.min(3)).filter_map(|i| u8::try_from(i).ok()).map(|i| char::from(b'a' + i).to_string()).collect();
    if n > 3 { format!("{}, …", names.join(", ")) } else { names.join(", ") }
}

/// Checks the header of a method named `str`, which turns the value it is
/// called on into text for `x.str() at r` (and `text(x) at r`), so it has the
/// one header those can call.
fn check_str_method(t: &TaskDef) -> Result<()> {
    let written = "a method named 'str' turns the value it is called on into text, so it is written 'str(r: region) -> text@r'";
    if let Some(at) = t.owner.as_ref().and_then(|owner| owner.alters_span) {
        return err(at, "a method named 'str' turns the value it is called on into text without changing it, so it isn't marked 'alters'");
    }
    let region = match t.params.get(1..) {
        Some([param]) if param.ty == Type::Region => &param.name,
        _ => return err(t.span, written),
    };
    if t.ret != Type::Text(Some(region.clone())) {
        return err(t.span, written);
    }
    Ok(())
}

/// `1 argument`, `2 arguments`.
fn count(n: usize, noun: &str) -> String {
    if n == 1 { format!("1 {}", noun) } else { format!("{} {}s", n, noun) }
}

/// The program's name for `name` declared in module `module`: `module.name`,
/// or just `name` in the main file.
fn qualify(module: &str, name: &str) -> String {
    if module.is_empty() { name.to_string() } else { format!("{}{}{}", module, MODULE_SEPARATOR, name) }
}

fn err<T>(span: Span, msg: impl Into<String>) -> Result<T> {
    Err(Error::at(span, msg))
}

/// The mistakes as they are reported: each once, in the order a reader meets
/// them, which is by file (the program's main file first), then line, then
/// column. The checker finds them in the order it checks — every declaration
/// before any task's body — and the same mistake can be found twice, by two
/// checks that each had to work the type out. A mistake about no position at
/// all (a compiler bug) comes last.
fn in_source_order(errors: Vec<Error>) -> Vec<Error> {
    let mut seen = HashSet::new();
    let mut errors: Vec<Error> =
        errors.into_iter().filter(|error| seen.insert((error.span.map(Span::place_key), error.message.clone()))).collect();
    // A stable sort, so two mistakes at one position stay in the order they
    // were found.
    errors.sort_by_key(|error| match error.span {
        Some(span) => (0, span.file, span.line, span.col),
        None => (1, 0, 0, 0),
    });
    errors
}

/// The end of the message for changing a parameter that never changes: how to
/// write one the task may change. Empty for anything else.
fn param_hint(name: &str, info: &VarInfo) -> String {
    if info.refers.is_some() {
        return format!("; '{0}' only reads the place it names, and a reference that changes it is written 'hold& {0} = &…'", name);
    }
    if !info.param || name == THIS {
        return String::new();
    }
    format!("; a parameter that changes is written 'hold {}: {}', and is the task's own copy", name, info.ty)
}

/// A step from a variable to a place in it: a field, an item, or what a
/// pointer points to (see `place_path`).
#[derive(Debug, Clone, PartialEq)]
enum Step {
    Field(String),
    /// An item at a number known before the program runs, or at one that isn't.
    Item(Option<i64>),
    Deref,
}

/// The place `e` names, as its variable and the steps from it, or `None` for a
/// value that isn't a place.
fn place_path(e: &Expr) -> Option<Vec<Step>> {
    let mut steps = Vec::new();
    let mut at = e;
    loop {
        match &at.kind {
            ExprKind::Var(name) => {
                steps.push(Step::Field(name.clone()));
                steps.reverse();
                return Some(steps);
            }
            // A shared field is one place for the whole program, named as it is
            // written, which no variable's name is.
            ExprKind::Shared { shape, field } => {
                steps.push(Step::Field(shared_key(shape, field)));
                steps.reverse();
                return Some(steps);
            }
            ExprKind::Field(base, field) => {
                steps.push(Step::Field(field.clone()));
                at = base;
            }
            ExprKind::Index(base, index) if !matches!(index.kind, ExprKind::Range(..)) => {
                steps.push(Step::Item(match index.kind {
                    ExprKind::IntLit(n) => Some(n),
                    _ => None,
                }));
                at = base;
            }
            ExprKind::Deref(pointer) => {
                steps.push(Step::Deref);
                at = pointer;
            }
            _ => return None,
        }
    }
}

/// Whether two places may be the same, or one part of the other: they start at
/// the same variable and no step tells them apart. A pointer is not what it
/// points to, so `p` and `p^` are two places.
fn overlaps(a: &[Step], b: &[Step]) -> bool {
    let (shorter, longer) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    if longer.get(shorter.len()) == Some(&Step::Deref) {
        return false;
    }
    a.iter().zip(b).all(|pair| match pair {
        (Step::Item(Some(x)), Step::Item(Some(y))) => x == y,
        (Step::Item(_), Step::Item(_)) => true,
        (x, y) => x == y,
    })
}

/// A place as a program writes it: `a.b[2]`, `p^.x`.
fn path_text(path: &[Step]) -> String {
    let mut text = String::new();
    for (i, step) in path.iter().enumerate() {
        match step {
            // `.n` inside a method is a field of the value it is called on.
            Step::Field(name) if i == 0 && name == THIS && path.len() > 1 => {}
            Step::Field(name) if i == 0 => text.push_str(name),
            Step::Field(name) => {
                text.push('.');
                text.push_str(name);
            }
            Step::Item(Some(n)) => text.push_str(&format!("[{}]", n)),
            Step::Item(None) => text.push_str("[…]"),
            Step::Deref => text.push('^'),
        }
    }
    text
}

/// What takes a place that changes: a `hold&` parameter, or a `hold&` local
/// reference, by its name, which the errors about the place name.
enum Taker<'a> {
    Param(&'a str),
    Reference(&'a str),
}

impl Taker<'_> {
    /// How an error about the place starts.
    fn lead(&self) -> String {
        match self {
            Taker::Param(name) => format!("'{}' is given the place itself, to change", name),
            Taker::Reference(name) => format!("'hold& {}' names a place to change it", name),
        }
    }

    /// What giving it `place` is, for the error that it changes `.this`.
    fn using(&self, place: &str) -> String {
        match self {
            Taker::Param(name) => format!("giving '{}' to '{}'", place, name),
            Taker::Reference(name) => format!("naming '{}' with 'hold& {}'", place, name),
        }
    }
}
