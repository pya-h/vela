use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::codegen;
use crate::error::{Error, Result};
use crate::library;
use crate::modules::{IncludeScope, ModuleScope, Program};
use crate::parser::MAX_NESTING;

/// A macro: its value as written, and the literal it is once computed (see
/// `Checker::macro_value`).
#[derive(Debug, Clone)]
struct MacroInfo {
    written: Expr,
    value: Option<Expr>,
    /// Why its value couldn't be computed, once tried: each macro computed
    /// from it, and each use, gets that again without computing it again.
    failed: Option<Error>,
    /// Where its name is written.
    span: Span,
}

/// A value the compiler computes, as a macro's is.
#[derive(Debug, Clone, PartialEq)]
enum Constant {
    Int(i64),
    Unit(u8),
    Float(f64),
    Char(u8),
    Bool(bool),
    Text(String),
}

impl Constant {
    fn of(e: &Expr) -> Option<Constant> {
        match (&e.kind, &e.ty) {
            (ExprKind::IntLit(n), Some(Type::Unit)) => u8::try_from(*n).ok().map(Constant::Unit),
            (ExprKind::IntLit(n), _) => Some(Constant::Int(*n)),
            (ExprKind::FloatLit(x), _) => Some(Constant::Float(*x)),
            (ExprKind::CharLit(c), _) => Some(Constant::Char(*c)),
            (ExprKind::BoolLit(b), _) => Some(Constant::Bool(*b)),
            (ExprKind::TextLit(t), _) => Some(Constant::Text(t.clone())),
            (ExprKind::Macro { value, .. }, _) => Constant::of(value),
            _ => None,
        }
    }

    fn ty(&self) -> Type {
        match self {
            Constant::Int(_) => Type::Int,
            Constant::Unit(_) => Type::Unit,
            Constant::Float(_) => Type::Float,
            Constant::Char(_) => Type::Char,
            Constant::Bool(_) => Type::Bool,
            Constant::Text(_) => Type::Text(None),
        }
    }

    /// The literal this is, checked, at `span`.
    fn literal(self, span: Span) -> Expr {
        let ty = self.ty();
        let kind = match self {
            Constant::Int(n) => ExprKind::IntLit(n),
            Constant::Unit(n) => ExprKind::IntLit(i64::from(n)),
            Constant::Float(x) => ExprKind::FloatLit(x),
            Constant::Char(c) => ExprKind::CharLit(c),
            Constant::Bool(b) => ExprKind::BoolLit(b),
            Constant::Text(t) => ExprKind::TextLit(t),
        };
        Expr { kind, span, ty: Some(ty) }
    }
}

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

/// Where a value that holds a slice, a map or a pointer may come from, for the
/// rule that what a fixed name owns is never given to what may change it (see
/// `Checker::owners`). A call gives back what the task's body may send, which
/// is known once every task is checked, so the rule is settled then (see
/// `Checker::settle_owners`).
#[derive(Clone, Debug, PartialEq)]
enum Own {
    /// A name of this task that never changes: a `fixed` variable, a
    /// `fixed&`, a pattern's or loop's name without `flex`, a lambda's copy.
    Root(String),
    /// A parameter of this task that never changes (`x: T`, `fixed x`,
    /// `fixed& x`, `.this` outside `alters`), by its place in the header.
    Param(usize, String),
    /// What `task` may send back of what its parameter `param` is given,
    /// which is given `from`.
    Sent { task: String, param: usize, from: Vec<Own> },
    /// What a lambda that keeps what it uses keeps of the code around it,
    /// which a call of it may send back.
    Kept(String),
}

/// The types one copy of a generic task was given: as the program writes them,
/// and by the name each fills in.
type GivenTypes = (Vec<Type>, HashMap<String, Type>);

/// Where a value is given to what may change it (see `Checker::hand`).
#[derive(Clone, Debug)]
enum HandedTo {
    /// A `flex` variable or name, or one assigned.
    Variable(String),
    /// A `flex` parameter of a task.
    Param { task: String, param: String },
    /// A place that changes: a field, an item, a map's value, a slice added to.
    Place(String),
    /// A task's named result.
    Result(String),
    /// A place written, or changed by an `alters` method, that is reached from
    /// a value made where it is written: the words before the owner.
    Written(String),
}

/// A value given to what may change it, with what owns it, settled once every
/// task is checked (see `Checker::settle_owners`).
#[derive(Clone, Debug)]
struct Handed {
    span: Span,
    owners: Vec<Own>,
    to: HandedTo,
    /// The copy the message suggests instead, as in `xs.copy() at r`.
    copy: String,
    /// Whether the value is a lambda, which holds no slice, map or pointer of
    /// its own but may send back what it keeps when called.
    lambda: bool,
    /// Where no region is here to copy into: the one the copy names, which
    /// the task would take.
    no_region: Option<String>,
    /// In a generic task, where the value holds a slice, a map or a pointer
    /// only for some types it may be given: the task and the value's type,
    /// which each copy of the task fills in (see `Checker::copies`).
    generic: Option<(String, Type)>,
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
    /// How a parameter is given (see `Passing`); `Copy` for anything else.
    passing: Passing,
    /// For a reference, `flex& y = &x` or `fixed& y = &x`: the place it names,
    /// from a variable that isn't a reference itself (see `resolved_path`).
    refers: Option<Vec<Step>>,
    /// Whether this is a variable of the code around a lambda that keeps what
    /// it uses, `[@r](…)`, which the lambda keeps a copy of if it uses it.
    kept: bool,
    /// For a name that never changes: where its value came from (see `Own`).
    owners: Vec<Own>,
}

/// A lambda that keeps what it uses, `[@r](…)` (see `Checker::kept_lambdas`).
struct KeptLambda {
    region: String,
    flex: bool,
    /// Where it is written.
    span: Span,
    /// The variables and region of the code around it where it is written,
    /// which it may keep: the region it keeps them in, and no other.
    outer: Vec<(String, VarInfo)>,
    /// The lambda that keeps what it uses it is written in, if any, which
    /// keeps what this one keeps of the code around both.
    within: Option<String>,
    /// The variables it uses, which it keeps, in the order first used.
    uses: Vec<String>,
    /// The variables it keeps as places, `[flex& count, @r]`.
    places: Vec<KeptPlace>,
    /// What owns each of `outer`, where it is written (see `Checker::owners`).
    owned: HashMap<String, Vec<Own>>,
}

/// What a place an assignment writes to takes: nothing (`_`), a value of a
/// type, or the items of a tuple, `( … )` at a position.
enum TargetType {
    Discard,
    Place(Type),
    Tuple(Vec<TargetType>, Span),
}

/// A name a pattern or a plan B declares: its name, where it is written, its
/// type, and whether it can be assigned (`flex` in a pattern).
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
    /// Every macro, by its name in the program.
    macros: HashMap<String, MacroInfo>,
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
    /// The names of the tasks in `instances`, and of those taken from it, so
    /// each set of types is noted once.
    noted_instances: HashSet<String>,
    /// The variants of kinds that take types made in the statement being
    /// checked whose types neither their values nor anything else has told
    /// yet: where each is, its kind, its variant, the first type untold, and
    /// its types as a message writes them, `int` for each untold. The place
    /// the value goes tells them (see `settle_variant`); one still untold at
    /// the statement's end is a mistake.
    untold_kinds: Vec<(Span, String, String, String, String)>,
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
    /// What the program does that it may not mean, found so far: code that
    /// builds, but whose meaning rests on a choice the compiler made (see
    /// `warn_chosen`).
    warnings: Vec<Error>,
    /// The files already warned about an array or slice of chars, which is
    /// said once per file (see `warn_char_array`).
    char_arrays_warned: HashSet<usize>,
    /// The tasks, shapes, and kinds whose own declaration has a mistake. What
    /// they promise isn't known, so a use of one is checked as far as it can be
    /// and then left alone: reporting the declaration's mistake again at every
    /// use would bury the file's other mistakes (see `Type::Unknown`).
    broken: HashSet<String>,
    /// The shapes whose defaults are checked, or being checked (see
    /// `check_defaults_of`).
    defaults_checked: HashSet<String>,
    /// For each task with parameters that fall back on values, the value of
    /// each, checked as its own module reads it (`None` for one without), in
    /// the order of its parameters, a method's value first. A call that leaves
    /// out the last ones is given these (see `fill_defaults`).
    param_defaults: HashMap<String, Vec<Option<Expr>>>,
    /// The field or parameter whose default is being checked, written as a
    /// message names it, `field 'x'` or `parameter 'n'`: its value is written
    /// with no task or method around it (see `check_field_default`).
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
    /// The shape or kind whose method is being checked, shared or not, for
    /// the hint when one of its methods is called by name alone.
    current_type: Option<String>,
    /// Where each call written on a value (`x.m(…)`) is, once it is the call
    /// of its method's task, which may be checked again as a value of another
    /// call: any other call of a method is written on its shape, `X.m(…)`.
    on_values: HashSet<(usize, usize, usize)>,
    /// Whether a call was refused for leaving out the region it takes last,
    /// which a chain joined `at r` reads (see `check_join`).
    region_wanted: bool,
    /// The task a lambda became, while its body is checked (see
    /// `ast::lambda_name`).
    current_lambda: Option<String>,
    /// The program's name for the task whose body is checked.
    current_task: String,
    /// The place of each parameter of that task in its header, by name.
    param_index: HashMap<String, usize>,
    /// Every value given to what may change it, settled once every task is
    /// checked (see `settle_owners`).
    handed: Vec<Handed>,
    /// For each task, where what its `<-` sends may come from.
    sends: HashMap<String, Vec<Own>>,
    /// The tasks whose body is the runtime's, which may send back anything.
    natives: HashSet<String>,
    /// What the names about to be declared own, by where each is written,
    /// which `define` gives them (see `bind_from`).
    bound_owners: Vec<(Span, Vec<Own>)>,
    /// What owns each variable read, by where it is read (its file, line and
    /// column), noted while its name is known: a value block's last line may
    /// read a name of its own, which is gone when the block's value is used.
    read_owners: HashMap<(usize, usize, usize), Vec<Own>>,
    /// For each generic task, the types each copy made of it was given: as
    /// the program writes them, and by the name each fills in.
    copies: HashMap<String, Vec<GivenTypes>>,
    /// The tuple items a type test has narrowed, `t.#5? is maybe int`, for the
    /// rest of its condition and its block: the tuple's name, the item's place,
    /// and the type its reads give in a `maybe` there.
    narrowed: Vec<(String, usize, Type)>,
    /// The groups whose `each v in args` the statements checked are inside,
    /// where a group's name alone, `P`, is the type of the turn's value (see
    /// `check_type`), each with how many loops are around it.
    group_turns: Vec<(String, usize)>,
    /// The loops around the statements checked, innermost last: the name each
    /// is given, `outer => loop …`, with where it is written, if any.
    loops: Vec<Option<(String, Span)>>,
    /// The name given to the loop about to be checked, `outer =>`.
    loop_name: Option<(String, Span)>,
    /// The loops that give a value around the code being checked (see
    /// `ValueLoop`): each one's place in `loops`, counting from one, and the
    /// values its `break`s give, with where.
    value_loops: Vec<(usize, Vec<(Type, Span)>)>,
    /// Whether the loop about to be checked gives a value on the last lines of
    /// its passes, and, once checked, what they give.
    loop_gives: bool,
    loop_given: Option<(Type, Span)>,
    /// While a loop's step is checked, how many loops are around it, that
    /// loop the innermost (see `PartedLoop`).
    stepping: Option<usize>,
    /// For each lambda, the names its value's place sees: the variables and
    /// regions of the code around it, which it takes nothing from, for the
    /// message when its body names one.
    lambda_outer: HashMap<String, HashSet<String>>,
    /// For each lambda, the named loops around its value's place, outside
    /// lambdas around it too: no name inside the lambda takes theirs, and no
    /// jump in it reaches them.
    lambda_loops: HashMap<String, Vec<(String, Span)>>,
    /// For each lambda that keeps what it uses (see `ExprKind::Closure`): what
    /// it may keep and what it does.
    kept_lambdas: HashMap<String, KeptLambda>,
    /// The order the lambdas that keep what they use were written in, inner
    /// ones after the ones around them.
    kept_order: Vec<String>,
    /// The variables of the code around it that the lambda being checked
    /// uses, which it keeps (see `lookup`).
    kept_used: std::cell::RefCell<Vec<String>>,
    /// Whether the variable being checked is read to be called or given as a
    /// place, the only ways a `[flex @r]` lambda's value is read.
    calling: bool,
    /// The region the lambda called through a value keeps what it uses in,
    /// which the call's types name as it is (see `check_invoke`).
    kept_call: Option<String>,
    /// Whether the value being checked is given on whole as a group's values,
    /// `args...`, the only way a group's values are read.
    spreading: bool,
    /// Each task with wrappers, and its wrappers' calls as written, until
    /// `settle_wrappers` makes its layers' bodies (see `add_wrappers`).
    wrapped: Vec<(String, Vec<Expr>)>,
    /// Each task whose body calls a wrapper, with how many values the task it
    /// wraps takes, for `check_start_values`.
    wrapper_layers: HashMap<String, usize>,
    /// Each task with wrappers whose layers are settled, and the tasks that
    /// call its wrappers, in order: the task itself, then `task__layer__1`, …
    /// (see `settle_wrapped`).
    wrapped_chains: Vec<(String, Vec<String>)>,
    /// The values of wrappers worked out where the program starts, each a
    /// shared field of `START_VALUES`, in the order they are written (see
    /// `check_start_values`, and `order_start_values` for the order they are
    /// worked out in).
    start_values: Vec<StartValue>,
    /// What the value being checked is, when it is worked out where the program
    /// starts, outside any task: a decorator's or a wrapper's (see `AT_START`).
    /// A `<-` there would leave the task it is worked out in.
    at_start: Option<&'static str>,
}

/// A value of a wrapper worked out where the program starts (see
/// `Checker::start_values`).
struct StartValue {
    /// The task whose wrapper takes it.
    task: String,
    /// The wrapper, as its call is written.
    wrapper: String,
    /// Its field of `START_VALUES`.
    member: String,
    span: Span,
}

/// What the values worked out where the program starts are, in messages (see
/// `Checker::at_start`).
const DECORATOR_VALUES: &str = "a decorator's values";
const WRAPPER_VALUES: &str = "a wrapper's own values";

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
const LOOP_BRANCHES: Branches = Branches { of: "loop", each: "pass" };

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

/// The most types one type given to a generic may be built of (see
/// `type_size`): far more than a program writes, and small enough to handle.
const MAX_TYPE_SIZE: usize = 1000;

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
            // `&a`, a place given to a parameter that may take one: the place
            // only needs to be one this code sees.
            let checked = match &mut value.kind {
                ExprKind::Ref(place) => self.check_expr(place),
                _ => self.check_expr(value),
            };
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
        Checker::check_program_warned(program).0
    }

    /// `check_program`, with the warnings too: what the program does that it
    /// may not mean, in source order. A program with warnings still builds.
    pub fn check_program_warned(program: &mut Program) -> (Vec<Error>, Vec<Error>) {
        let mut checker = Checker::with_type_places(std::mem::take(&mut program.type_places));
        checker.modules = program.modules.iter().map(|scope| (scope.name.clone(), scope.clone())).collect();
        checker.declare_builtin_kinds();
        add_startups(&mut program.items);
        checker.add_wrappers(&mut program.items);
        checker.check_file(&mut program.items);
        // What a program with a mistake would be made into is never generated,
        // and the parts of it a mistake left unchecked would only cause more, so
        // the program is completed only once it checks.
        if checker.errors.is_empty() {
            let made = checker.expand_instances(&mut program.items);
            checker.keep_going(made);
        }
        // What a fixed name owns is given to what may change it only where
        // every task's sends are known.
        if checker.errors.is_empty() {
            checker.settle_owners();
        }
        // The shapes and kinds made for their types join the program, so a
        // tool reading a program with a mistake names them as written too.
        program.items.extend(std::mem::take(&mut checker.made_shapes));
        if checker.errors.is_empty() {
            // The values of wrappers worked out where the program starts, after
            // every shared field of the program, which they may read.
            if let Some(values) = checker.shapes.remove(START_VALUES) {
                program.items.push(Item::Shape(values));
            }
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
        let warnings_at = errors.len();
        errors.append(&mut checker.warnings);
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
        let warnings = errors.split_off(warnings_at);
        (in_source_order(errors), in_source_order(warnings))
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
        if name == unqualified(library::LOOP_RESULT) {
            return "; it is in Vela's library, and 'include std/loops {LoopResult}' brings it in".to_string();
        }
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
        self.settle_wrappers(items);
        // A field's value may name a wrapped task, whose type its wrappers
        // settle (see `settle_wrapped`).
        self.check_field_defaults(items);
        self.check_param_defaults(items);
        self.check_shared_fields(items);
        self.check_start_values(items);
        // A lambda is checked after the code it is written in, which notes
        // the names around it (see `lambda_outer`): the parser adds lambdas
        // last, each after the ones inside it, so they are checked from the
        // last back.
        let (lambdas, others): (Vec<&mut Item>, Vec<&mut Item>) = items.iter_mut().partition(|item| matches!(item, Item::Task(t) if is_lambda(&t.name)));
        for item in others.into_iter().chain(lambdas.into_iter().rev()) {
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
        self.finish_kept();
        self.order_start_values(items);
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
                return err(at, format!("'{}' is used with more than {} different types, which no program needs; a generic task whose call gives a type built from its own, or a group with values added to it, never stops", unqualified(&name), MAX_INSTANCES));
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
            let mut given: HashMap<String, Type> = source.types.iter().map(|param| param.name.clone()).zip(types).collect();
            for param in source.types.iter().filter(|param| param.group) {
                let types = group_types(&given, &param.name).unwrap_or_default();
                for (place, ty) in types.iter().enumerate() {
                    given.insert(group_item(&param.name, place), ty.clone());
                }
                given.insert(format!("{}{}", param.name, GROUP_MARK), Type::Tuple(types));
            }
            // A kind or shape given types, `Option<int>`, is the one made for
            // them, which a group's values and a turn over them take as given.
            for ty in given.values_mut() {
                self.settle_applications(ty, at)?;
            }
            self.copies.entry(name.clone()).or_default().push((task.given.clone(), given.clone()));
            let mut made = Made::default();
            self.substitute_task(&mut task, &given, &mut made)?;
            if let Some((read, place, given_count)) = made.past_items.first() {
                return err(
                    at,
                    format!(
                        "this use gives '{}' {}, and its tuple of them is read at item {} at {}:{}{}; read it there with '.#{}', a maybe, or inside 'when args.len > {}'",
                        unqualified(&name),
                        count(*given_count, "value"),
                        place,
                        read.line,
                        read.col,
                        self.file_named(read.file, at.file),
                        place,
                        place
                    ),
                );
            }
            for (span, what, param) in &made.reads_chosen {
                let Some((ty, declared)) = made.chosen.get(param) else { continue };
                let types: Vec<String> = task.given.iter().map(|ty| ty.without_regions().to_string()).collect();
                let given = format!("where '{}' is given {}, at {}:{}{}", unqualified(&name), types.join(", "), at.line, at.col, self.file_named(at.file, span.file));
                let message = chosen_message(what, param, ty, declared, self.size_of(ty), Some(given));
                self.warnings.push(Error::at(*span, message));
            }
            if let Some((compare, ty)) = made.placeless.first() {
                return err(
                    at,
                    format!(
                        "this use gives '{}' {}, which the '==' or '!=' at {}:{}{} compares by where it is, as it does arrays, shapes, and tuples, and a side there is a value made on the spot, with no place of its own; compare them item by item with '~=' there, and ask for '<T: similar>'",
                        unqualified(&name),
                        ty.without_regions(),
                        compare.line,
                        compare.col,
                        self.file_named(compare.file, at.file)
                    ),
                );
            }
            for (name, types, at) in made.needed {
                // `f((v, v))` in `f`: each use doubles the type, which would
                // grow too large to handle long before it grew too deep. A
                // group's values given on, `f(args...)`, are a tuple of them,
                // each a value of its own, as a call writing them gives them.
                let parts = |ty: &Type| match ty {
                    Type::Tuple(items) => items.iter().map(type_size).max().unwrap_or(0),
                    other => type_size(other),
                };
                if types.iter().any(|ty| parts(ty) > MAX_TYPE_SIZE) {
                    return err(at, format!("this use of '{}' gives a type of more than {} parts; a generic task whose call gives a type built from its own never stops", unqualified(&name), MAX_TYPE_SIZE));
                }
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
                Item::Macro(m) => m.private,
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
                Item::Macro(m) => {
                    if taken(&m.name, m.span) {
                        continue;
                    }
                    self.macros.insert(m.name.clone(), MacroInfo { written: m.value.clone(), value: None, failed: None, span: m.span });
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
                Item::Task(_) | Item::Macro(_) => continue,
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

    /// Checks the values the tasks' parameters fall back on, each as its own
    /// module reads it, and keeps them for the calls that leave them out (see
    /// `param_defaults`). A task with a mistake in one is broken, so its calls
    /// say nothing more (see `broken`).
    fn check_param_defaults(&mut self, items: &mut [Item]) {
        for item in items.iter_mut() {
            let Item::Task(t) = item else { continue };
            if t.params.iter().all(|param| param.default.is_none()) || self.broken.contains(&t.name) {
                continue;
            }
            let module = std::mem::replace(&mut self.current_module, module_of(&t.name).to_string());
            self.enter_type_params(&t.types);
            let checked = self.check_defaults_of_task(t);
            self.type_params.clear();
            self.current_module = module;
            match checked {
                Ok(values) => {
                    self.param_defaults.insert(t.name.clone(), values);
                }
                Err(error) => {
                    self.keep_going(Err(error));
                    self.broken.insert(t.name.clone());
                }
            }
        }
    }

    /// The values `t`'s parameters fall back on, checked (see
    /// `check_param_defaults`).
    #[inline(never)]
    fn check_defaults_of_task(&mut self, t: &mut TaskDef) -> Result<Vec<Option<Expr>>> {
        // The value a method is called on is told by `not_a_method`, which
        // names it as written.
        let names: Vec<String> = t.params.iter().map(|param| param.name.clone()).filter(|name| name != THIS).collect();
        let mut values = Vec::with_capacity(t.params.len());
        for param in &mut t.params {
            let Some(value) = &mut param.default else {
                values.push(None);
                continue;
            };
            // Another parameter has no value before the call gives it.
            let mut other = None;
            walk_expr(value, &mut |expr| {
                if let ExprKind::Var(name) = &expr.kind
                    && other.is_none()
                    && names.contains(name)
                {
                    other = Some((name.clone(), expr.span));
                }
            });
            if matches!(value.kind, ExprKind::Closure { .. }) {
                return err(
                    value.span,
                    format!(
                        "a lambda that keeps what it uses keeps it in a region, which the value parameter '{}' falls back on can't name, since it is worked out before the call; give a lambda that keeps nothing, or a task",
                        param.name
                    ),
                );
            }
            if let Some((other, at)) = other {
                return err(
                    at,
                    format!(
                        "the value parameter '{}' falls back on is worked out before the program runs, so it can't read parameter '{}', which a call gives",
                        param.name, other
                    ),
                );
            }
            self.check_default_value(value, &param.ty, &format!("parameter '{}'", param.name), "the task takes, which is a different type in every use")?;
            // `none` for a `maybe T` is the `none` of each use's own type, so
            // each call checks it as written.
            if !param.ty.params().is_empty() {
                values.push(Some(Expr::new(ExprKind::NoneLit, value.span)));
                continue;
            }
            values.push(Some(value.clone()));
        }
        Ok(values)
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
        for (i, field) in shape.fields.iter().enumerate() {
            if let Some(value) = &field.default {
                let mut value = value.clone();
                // Arithmetic worked out once is the number each literal is
                // given (see `check_default_value`).
                if is_arithmetic(Some(&value))
                    && let Ok(folded) = self.fold_constant(&value, &mut Vec::new())
                {
                    value = folded.literal(value.span);
                    if let Some(kept) = self.shapes.get_mut(name).and_then(|shape| shape.fields.get_mut(i)) {
                        kept.default = Some(value.clone());
                    }
                }
                checked = self.check_field_default(&mut value, &field.ty, &field.name);
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
                // A kind's own types are in scope while its variants are read,
                // as a shape's are while its fields are.
                self.enter_type_params(&k.types);
                let payloads = k.variants.iter_mut().try_for_each(|v| self.check_fields(&mut v.payload));
                self.type_params.clear();
                payloads?;
                if let Some(kind) = self.kinds.get_mut(&k.name) {
                    kind.variants = k.variants.clone();
                }
            }
            // A macro whose name was refused has no value to compute.
            Item::Macro(m) if self.macros.contains_key(&m.name) => m.value = self.macro_value(&m.name, &mut Vec::new())?,
            Item::Macro(_) => {}
            Item::Task(t) => {
                self.check_header(t)?;
                if t.name == "main" {
                    if let Some(param) = t.params.first() {
                        return err(param.span, "task 'main' cannot take parameters");
                    }
                    if t.ret != Type::Nothing {
                        return err(t.ret_span(), "task 'main' cannot return a value");
                    }
                }
            }
        }
        Ok(())
    }

    /// The literal macro `name` stands for, computed once. `through` holds the
    /// macros whose values are being computed, which the value can't name.
    fn macro_value(&mut self, name: &str, through: &mut Vec<String>) -> Result<Expr> {
        let Some(info) = self.macros.get(name) else { return Err(Error::internal(format!("no macro '{}'", name))) };
        if let Some(value) = &info.value {
            return Ok(value.clone());
        }
        if let Some(error) = &info.failed {
            return Err(error.clone());
        }
        let (written, span) = (info.written.clone(), info.span);
        if let Some(first) = through.iter().position(|macro_name| macro_name == name) {
            let mut chain: Vec<&str> = through.iter().skip(first).map(|m| unqualified(m)).collect();
            chain.push(unqualified(name));
            return err(span, format!("macro '{}' is computed from itself: {}", unqualified(name), chain.join(" from ")));
        }
        // Each macro computed from another waits for it, so a chain as deep
        // as the parser's nesting is as far as they go.
        if through.len() >= MAX_NESTING {
            return err(span, format!("a chain of macros, each computed from the next, is at most {} long, and '{}' is the {}st", MAX_NESTING, unqualified(name), MAX_NESTING + 1));
        }
        through.push(name.to_string());
        let module = std::mem::replace(&mut self.current_module, module_of(name).to_string());
        let value = self.fold_constant(&written, through);
        self.current_module = module;
        through.pop();
        let value = value.map(|value| value.literal(span));
        if let Some(info) = self.macros.get_mut(name) {
            match &value {
                Ok(value) => info.value = Some(value.clone()),
                Err(error) => info.failed = Some(error.clone()),
            }
        }
        value
    }

    /// What `e`, a macro's value, is: literals, other macros, and the
    /// operators and conversions of numbers, chars, bools and text on them.
    fn fold_constant(&mut self, e: &Expr, through: &mut Vec<String>) -> Result<Constant> {
        let span = e.span;
        let not_constant = |what: &str| -> Result<Constant> {
            err(span, format!("a macro's value is computed when compiling, from literals, other macros, and operators on them, so it can't be {}", what))
        };
        match &e.kind {
            ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::CharLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) | ExprKind::Macro { .. } => {
                Constant::of(e).ok_or_else(|| Error::internal_at(span, "a literal that is no constant"))
            }
            ExprKind::Var(name) if self.macros.contains_key(name.as_str()) => {
                let value = self.macro_value(name, through)?;
                Constant::of(&value).ok_or_else(|| Error::internal_at(span, "a macro that is no constant"))
            }
            ExprKind::Var(name) if is_lambda(name) => not_constant("a lambda, which is a task; write it where its value goes"),
            ExprKind::Var(name) => not_constant(&format!("'{}', which is no macro", unqualified(name))),
            ExprKind::Unary(op, operand) => {
                let value = self.fold_constant(operand, through)?;
                match (op, value) {
                    (UnaryOp::Neg, Constant::Int(n)) => n.checked_neg().map(Constant::Int).ok_or_else(|| Error::at(span, "the value doesn't fit in an int")),
                    (UnaryOp::Neg, Constant::Float(x)) => Ok(Constant::Float(-x)),
                    (UnaryOp::Not, Constant::Bool(b)) => Ok(Constant::Bool(!b)),
                    (UnaryOp::Neg, other) => err(span, format!("'-' makes an int or a float negative, not a {}", other.ty())),
                    (UnaryOp::Not, other) => err(span, format!("'not' is for a bool, not a {}", other.ty())),
                }
            }
            ExprKind::Binary(op, l, r) => {
                let (a, b) = (self.fold_constant(l, through)?, self.fold_constant(r, through)?);
                fold_binary(*op, a, b, span)
            }
            ExprKind::Call { name, args, .. } if [INT_CAST, FLOAT_CAST, CHAR_CAST, UNIT_CAST].contains(&name.as_str()) => {
                let [arg] = args.as_slice() else { return err(span, format!("'{}(…)' takes one value", name)) };
                let value = self.fold_constant(arg, through)?;
                fold_cast(name, value, span)
            }
            ExprKind::Call { name, .. } => not_constant(&format!("a call, '{}(…)'", unqualified(name))),
            ExprKind::Join { .. } => not_constant("text holding values; join text with '+', as in '#TITLE = NAME + \"!\"'"),
            ExprKind::NoneLit => not_constant("'none'"),
            ExprKind::Array(_) | ExprKind::Repeat { .. } => not_constant("an array"),
            ExprKind::Tuple(_) => not_constant("a tuple"),
            _ => not_constant("anything that makes a value when the program runs"),
        }
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
    /// `MAX_VALUE_BYTES`, such as `[[int#1000000]#1000000]`.
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
                let receiver = usize::from(t.owner.as_ref().is_some_and(|owner| !owner.shared));
                (t.private || hidden, "method", &t.name, t.params.iter().skip(receiver).collect())
            }
            Item::Task(t) => (t.private, "task", &t.name, t.params.iter().collect()),
            // Its shared fields are named on it, `X.f`, so theirs count too.
            Item::Shape(s) => (s.private, "shape", &s.name, s.fields.iter().chain(s.shared.iter().map(|m| &m.field)).filter(|f| !f.private).collect()),
            Item::Kind(k) => (k.private, "kind", &k.name, k.variants.iter().flat_map(|v| &v.payload).collect()),
            // A macro's type is a builtin one.
            Item::Macro(_) => return Ok(()),
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
        // A shape or a kind that takes types: what it takes, and what it holds,
        // which the types given may be.
        let Some((word, params, parts)) = self.takes_types(name).filter(|_| self.sees(name)) else {
            return Err(self.unknown_shape(at, name));
        };
        if params.is_empty() {
            return err(at, format!("{} '{}' takes no types, so '< … >' has nothing to name", word, unqualified(name)));
        }
        if params.len() != applied.types.len() {
            let names: Vec<&str> = params.iter().map(|param| param.name.as_str()).collect();
            return err(
                at,
                format!(
                    "{} '{}' takes {}, so name {}, as in '{}<{}>'",
                    word,
                    unqualified(name),
                    count(params.len(), "type"),
                    if params.len() == 1 { "it" } else { "all of them" },
                    unqualified(name),
                    names.join(", ")
                ),
            );
        }
        // The types it is given are in the region of the value holding them, as
        // a shape's fields are, so they take the region written on the whole
        // type (or the one around it), and name none of their own (`Box<text>@r`
        // fills in its `text@r` as it is read, which says the same).
        if let Some(named) = applied.types.iter().flat_map(Type::regions).find(|named| applied.region.as_deref() != Some(*named)) {
            let given: Vec<String> = applied.types.iter().map(|ty| ty.without_regions().to_string()).collect();
            return err(
                self.place(span, &format!("@{}", named)),
                format!(
                    "a type given lives in the region of the value holding it, so it names no region; write '{}<{}>@{}'",
                    unqualified(name),
                    given.join(", "),
                    named
                ),
            );
        }
        let region = applied.region.as_deref().or(inside);
        let mut types = Vec::new();
        for ty in &applied.types {
            types.push(self.resolve_regions(ty, span, place, region)?);
        }
        for (param, ty) in params.iter().zip(&types) {
            self.check_need(word, name, param, ty, at)?;
        }
        if types.iter().any(|ty| !ty.params().is_empty()) {
            // A shape given a type this declaration takes: whether its values
            // live in a region depends on the types given, so it names one
            // wherever any of them could.
            let may_hold = self.takes_region.contains(name.as_str()) || parts.iter().any(|ty| !ty.params().is_empty());
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
        let key = self.instance(name, &types, at)?;
        self.resolve_regions(&Type::Named(key, applied.region.clone()), span, place, inside)
    }

    /// For a shape or a kind called `name`: which it is, the types it takes,
    /// and the types of what its values hold (a shape's fields, a kind's
    /// payloads), which may name them.
    fn takes_types(&self, name: &str) -> Option<(&'static str, Vec<TypeParam>, Vec<Type>)> {
        if let Some(shape) = self.shapes.get(name) {
            return Some(("shape", shape.types.clone(), shape.fields.iter().map(|field| field.ty.clone()).collect()));
        }
        let kind = self.kinds.get(name)?;
        Some(("kind", kind.types.clone(), kind.variants.iter().flat_map(|variant| variant.payload.iter().map(|field| field.ty.clone())).collect()))
    }

    /// The shape or kind made for the types `types` give the shape or kind
    /// `name` (see `shape_instance` and `kind_instance`).
    fn instance(&mut self, name: &str, types: &[Type], at: Span) -> Result<String> {
        if self.kinds.contains_key(name) {
            return self.kind_instance(name, types, at);
        }
        self.shape_instance(name, types, at)
    }

    /// The kind made for the types `types` give the kind `name`, made on the
    /// first use, as a shape is (see `shape_instance`): its variants carry
    /// those types in place of the names it takes, in the region of the value
    /// holding them, so one kind serves every region.
    fn kind_instance(&mut self, name: &str, types: &[Type], at: Span) -> Result<String> {
        let types: Vec<Type> = types.iter().map(Type::without_regions).collect();
        let key = instance_name(name, &types);
        if self.kinds.contains_key(&key) {
            return Ok(key);
        }
        let Some(source) = self.kinds.get(name).cloned() else {
            return Err(Error::internal_at(at, format!("no kind '{}' to make for {} types", name, types.len())));
        };
        if types.iter().any(|ty| type_depth(ty) > MAX_TYPE_DEPTH || type_size(ty) > MAX_TYPE_SIZE) {
            return err(
                at,
                format!(
                    "this use of '{}' gives a type built up more than {} types deep; a kind whose variant gives it a type built from its own never stops",
                    unqualified(name),
                    MAX_TYPE_DEPTH
                ),
            );
        }
        if self.made_shapes.len() >= MAX_INSTANCES {
            return err(
                at,
                format!(
                    "more than {} shapes and kinds are made for the types they are given, which no program needs; a kind whose variant gives it a type built from its own never stops",
                    MAX_INSTANCES
                ),
            );
        }
        if self.making_shapes.len() >= MAX_TYPE_DEPTH {
            let names = source.types.iter().map(|param| param.name.clone()).collect::<Vec<_>>().join(", ");
            return err(
                at,
                format!(
                    "'{0}' would need more than {1} kinds made for it, one inside the next: a variant of it gives it a type built from its own, which never stops. A variant can name the kind with the types it takes, as in 'maybe ^{0}<{2}>'",
                    unqualified(name),
                    MAX_TYPE_DEPTH,
                    names
                ),
            );
        }
        // Registered before the variants are read, so one naming the same kind
        // again (behind a pointer) is this kind.
        let written = format!("{}<{}>", name, types.iter().map(Type::to_string).collect::<Vec<_>>().join(", "));
        self.written_names.insert(key.clone(), written);
        self.made_of.insert(key.clone(), Applied { name: name.to_string(), types: types.clone(), region: None });
        let made = KindDef { name: key.clone(), types: Vec::new(), given: types.clone(), variants: Vec::new(), span: source.span, private: source.private };
        self.kinds.insert(key.clone(), made);
        self.making_shapes.push(key.clone());
        let given: HashMap<String, Type> = source.types.iter().map(|param| param.name.clone()).zip(types).collect();
        let mut variants = Vec::new();
        let mut found = Ok(());
        for variant in &source.variants {
            let mut payload = Vec::new();
            for field in &variant.payload {
                // A kind used before its declaration is checked has its
                // payloads as written, where `T` is still a name: read with
                // its types in scope, `T` is the type it takes (as
                // `instance_fields` has it).
                let outer = std::mem::replace(&mut self.type_params, source.types.clone());
                let read = self.resolve_type(&field.ty, field.ty_span, TypePlace::Field);
                self.type_params = outer;
                let ty = read.map(|ty| ty.map_params(&mut |param| given.get(param).cloned()));
                match ty.and_then(|ty| self.resolve_type(&ty, field.ty_span, TypePlace::Field)) {
                    Ok(ty) => payload.push(Field { ty, ..field.clone() }),
                    Err(error) => found = Err(error),
                }
            }
            variants.push(Variant { payload, ..variant.clone() });
        }
        self.making_shapes.pop();
        if let Err(error) = found {
            self.kinds.remove(&key);
            self.written_names.remove(&key);
            self.made_of.remove(&key);
            return Err(error);
        }
        if let Some(kind) = self.kinds.get_mut(&key) {
            kind.variants = variants.clone();
        }
        let payloads: Vec<&Field> = variants.iter().flat_map(|variant| &variant.payload).collect();
        if payloads.iter().any(|field| holds_pointers(&field.ty, &self.regional)) {
            self.regional.insert(key.clone());
        }
        if self.regional.contains(&key) || payloads.iter().any(|field| holds_text(&field.ty, &self.takes_region)) {
            self.takes_region.insert(key.clone());
        }
        if payloads.iter().any(|field| holds_changeable(&field.ty, &self.changeable)) {
            self.changeable.insert(key.clone());
        }
        let largest = variants.iter().map(|variant| fields_bytes(&variant.payload, &self.sizes)).max().unwrap_or(0);
        let bytes = largest.saturating_add(8);
        if bytes > MAX_VALUE_BYTES {
            return err(at, format!("'{}' is too large: a value of it would take more than 1 TiB of memory", key));
        }
        self.sizes.insert(key.clone(), bytes);
        if let Err((error, _)) = self.check_value_cycle(&key, &mut HashSet::new(), &mut Vec::new()) {
            return Err(error);
        }
        if let Some(kind) = self.kinds.get(&key) {
            self.made_shapes.push(Item::Kind(kind.clone()));
        }
        Ok(key)
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
        if types.iter().any(|ty| type_depth(ty) > MAX_TYPE_DEPTH || type_size(ty) > MAX_TYPE_SIZE) {
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
        let fields = match fields {
            Ok(fields) => fields,
            // Not made after all: a later use gets the same error, not one
            // about a shape with no fields.
            Err(error) => {
                self.shapes.remove(&key);
                self.written_names.remove(&key);
                self.made_of.remove(&key);
                return Err(error);
            }
        };
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
        let kinds = self.untold_kinds.len();
        let mut found = Ok(());
        for field in fields.iter_mut() {
            let Some(declared) = shape.fields.iter().find(|f| f.name == field.name) else { continue };
            match self.check_expr(&mut field.value) {
                Ok(ty) => {
                    // A variant whose types it didn't tell, `Option.None`,
                    // tells nothing, and another field may.
                    let mut told = HashMap::new();
                    self.match_params(&declared.ty, &ty, &mut told);
                    for (param, ty) in told {
                        if !ty.params().iter().any(|name| name.starts_with(UNTOLD)) {
                            given.entry(param).or_insert(ty);
                        }
                    }
                }
                Err(error) => {
                    found = Err(error);
                    break;
                }
            }
        }
        self.unsettled_copies.truncate(copies);
        self.unsettled_types.truncate(untold);
        self.untold_kinds.truncate(kinds);
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
            // A shape used before its declaration is checked has its fields
            // as written, where `T` is still a name: read with its types in
            // scope, `T` is the type it takes.
            let outer = std::mem::replace(&mut self.type_params, source.types.clone());
            let read = self.resolve_type(&field.ty, field.ty_span, TypePlace::Field);
            self.type_params = outer;
            let ty = read?.map_params(&mut |param| given.get(param).cloned());
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
            Type::Map(key, value, region) => Type::Map(boxed(key), boxed(value), region.clone()),
            Type::Array(inner, count) => Type::Array(boxed(inner), *count),
            Type::Range(inner) => Type::Range(boxed(inner)),
            Type::Maybe(inner) => Type::Maybe(boxed(inner)),
            Type::Fail(inner) => Type::Fail(boxed(inner)),
            Type::Or(value, error) => Type::Or(boxed(value), boxed(error)),
            Type::Tuple(items) => Type::Tuple(items.iter().map(|ty| self.unmade(ty)).collect()),
            Type::Results(types) => Type::Results(types.iter().map(|ty| self.unmade(ty)).collect()),
            Type::Task(task) => Type::Task(Box::new(TaskType { params: task.params.iter().map(|ty| self.unmade(ty)).collect(), taken: task.taken.clone(), ret: self.unmade(&task.ret) })),
            Type::Closure(closure) => {
                let task = TaskType { params: closure.task.params.iter().map(|ty| self.unmade(ty)).collect(), taken: closure.task.taken.clone(), ret: self.unmade(&closure.task.ret) };
                Type::Closure(Box::new(ClosureType { task, region: closure.region.clone(), flex: closure.flex }))
            }
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
                let key = self.instance(&applied.name, &applied.types, at)?;
                let region = applied.region.filter(|_| self.takes_region.contains(&key));
                *ty = Type::Named(key, region);
                Ok(())
            }
            Type::Ptr(inner, _) | Type::Slice(inner, _) | Type::Array(inner, _) | Type::Range(inner) | Type::Maybe(inner) | Type::Fail(inner) => {
                self.settle_applications(inner, at)
            }
            // A map's key given by a use of a generic declaration: one that is
            // compared and hashed, as a written key must be.
            Type::Map(key, value, _) => {
                if let Some(why) = key_problem(key, &|_| false) {
                    return err(at, format!("a map's key is compared and hashed, so it is an int, a unit, a char, a bool, text, a pointer, or a tuple of them, not {}, which this use gives", why));
                }
                self.settle_applications(key, at)?;
                self.settle_applications(value, at)
            }
            Type::Or(value, error) => {
                self.settle_applications(value, at)?;
                self.settle_applications(error, at)
            }
            Type::Tuple(types) | Type::Results(types) => types.iter_mut().try_for_each(|ty| self.settle_applications(ty, at)),
            Type::Task(task) => {
                task.params.iter_mut().try_for_each(|ty| self.settle_applications(ty, at))?;
                self.settle_applications(&mut task.ret, at)
            }
            Type::Closure(closure) => {
                closure.task.params.iter_mut().try_for_each(|ty| self.settle_applications(ty, at))?;
                self.settle_applications(&mut closure.task.ret, at)
            }
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
        self.holds_file_in(ty, &mut Vec::new())
    }

    /// `holds_file`, having looked into the shapes and kinds in `seen` already,
    /// so a type holding itself, which is refused later, is looked into once.
    fn holds_file_in<'t>(&'t self, ty: &'t Type, seen: &mut Vec<&'t str>) -> bool {
        match ty {
            Type::Named(name, _) if name == FILE_SHAPE => true,
            Type::Named(name, _) => {
                if seen.contains(&name.as_str()) {
                    return false;
                }
                seen.push(name);
                let fields = self.shapes.get(name).into_iter().flat_map(|shape| &shape.fields);
                let payloads = self.kinds.get(name).into_iter().flat_map(|kind| kind.variants.iter().flat_map(|variant| &variant.payload));
                fields.chain(payloads).any(|field| self.holds_file_in(&field.ty, seen))
            }
            Type::Maybe(inner) | Type::Array(inner, _) => self.holds_file_in(inner, seen),
            Type::Or(value, error) => self.holds_file_in(value, seen) || self.holds_file_in(error, seen),
            Type::Tuple(items) => items.iter().any(|item| self.holds_file_in(item, seen)),
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
        self.check_default_value(value, ty, &format!("field '{}'", field), "the shape takes, which is a different type in every value")
    }

    /// Checks `value`, which `what` (`field 'x'` or `parameter 'n'`) of type
    /// `ty` falls back on: a value the compiler works out, of that type, which
    /// a type the declaration takes (`which` says why) can't have.
    fn check_default_value(&mut self, value: &mut Expr, ty: &Type, what: &str, which: &str) -> Result<()> {
        let at = value.span;
        // `none` is the one value that fits a type the declaration takes, in a
        // maybe, whatever type that is.
        let fits_any = matches!(ty, Type::Maybe(_)) && matches!(value.kind, ExprKind::NoneLit);
        if !ty.params().is_empty() && !fits_any {
            return err(at, format!("{} is of a type {}, so it has no value to fall back on", what, which));
        }
        // Arithmetic on literals and macros is worked out here, once, so what
        // each literal or call is given is the number it makes.
        if is_arithmetic(Some(value)) && let Ok(folded) = self.fold_constant(value, &mut Vec::new()) {
            *value = folded.literal(at);
        }
        let outer = self.in_default.replace(what.to_string());
        let checked = self.check_expr(value);
        self.in_default = outer;
        checked?;
        if !constant_value(value) {
            let tasks = if what.starts_with("parameter") { ", a task, a lambda that keeps nothing" } else { "" };
            return err(
                at,
                format!(
                    "the value {} falls back on must be one the compiler works out without running the program: a number, text, 'true', 'false', 'none', a variant{}, arithmetic on numbers and macros, or a tuple, an array, or a literal of those",
                    what, tasks
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
        if task.owner.as_ref().is_none_or(|owner| owner.shared) {
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
        // The task's types are out of scope after its header, whether or not
        // it checks, so a declaration after it can't name them.
        let checked = self.check_header_in_scope(t);
        self.type_params.clear();
        checked
    }

    /// `check_header`, with the task's types in scope.
    fn check_header_in_scope(&mut self, t: &mut TaskDef) -> Result<()> {
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
        // `args: P...`: the values a call gives between the ones named before
        // it and after it.
        let mut grouped = false;
        for param in &t.params {
            let Type::Group(group) = &param.ty else { continue };
            if std::mem::replace(&mut grouped, true) {
                return err(param.span, format!("a task takes one group of values, and '{}: {}...' is a second; name the others one by one", param.name, group));
            }
            if param.passing != Passing::Copy {
                return err(param.span, format!("a group's values are read as copies the task can't change, so '{}' can't be '{}'; a task value given them takes them as places where it takes places, as 'next: (P...) -> R' given a task taking 'flex& n: int'", param.name, param.passing.word()));
            }
        }
        let region_params: Vec<&str> = t.params.iter().filter(|p| p.ty == Type::Region).map(|p| p.name.as_str()).collect();
        let mut named: Vec<String> = region_params.iter().map(|name| name.to_string()).collect();
        // A lambda that keeps what it uses in `r` may name `r` in its types,
        // where what it keeps lives.
        if let Some(kept) = &t.kept {
            if let Some(param) = t.params.iter().find(|param| param.name == kept.region) {
                return err(param.span, format!("'{0}' is the region this lambda keeps what it uses in, '[@{0}]', so a parameter of its own is named otherwise", kept.region));
            }
            named.push(kept.region.clone());
        }
        let mut resolved = Vec::new();
        for param in &t.params {
            if param.ty == Type::Region {
                resolved.push(Type::Region);
                continue;
            }
            let ty = self.resolve_type(&param.ty, param.ty_span, TypePlace::Header)?;
            // A `[flex @r]` lambda changes what it keeps, so it is never copied.
            if held_lambda(&ty) && param.passing != Passing::FlexRef {
                return err(
                    param.span,
                    format!("'{0}' is a '[flex @r]' lambda, which changes what it keeps and is never copied, so it is given as a place, 'flex& {0}: {1}'", param.name, ty),
                );
            }
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
            if result == Type::Nothing {
                return err(span, "'nothing' is only allowed as a task's whole return type, so it can't be one of several results");
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
                // `@this` is the region of the value a method is called on.
                if region == THIS_WORD && t.owner.as_ref().is_some_and(|owner| owner.shared) {
                    return err(
                        at,
                        format!("{} lives in '@{}', the region of the value a method is called on, and a shared method has none; take a region, as in 'r: region', and name it", which, THIS_WORD),
                    );
                }
                if is_lambda(&t.name) {
                    return err(at, format!("{1} lives in region '{0}', which the lambda isn't given; take it as a parameter, as in '({0}: region) -> …'", region, which));
                }
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
            0 => Type::Nothing,
            1 => results.into_iter().next().unwrap_or(Type::Nothing),
            _ => Type::Results(results),
        };
        // A `nothing ~ E` task succeeds by reaching its end or a bare `<-`, so a
        // name for its result would have nothing to hold.
        if !sends_value(&t.ret)
            && let Some((name, at)) = t.result_names().next()
        {
            return err(*at, format!("a '{}' task succeeds by reaching its end or a bare '<-', so '{}' has no value to name", t.ret, name));
        }
        for (param, ty) in t.params.iter_mut().zip(resolved) {
            param.ty = ty;
        }
        // A wrapper's layers of `str` are its body, kept out of sight.
        if t.owner.as_ref().is_some_and(|owner| owner.name == STR) && !t.name.contains(LAYER) {
            check_str_method(t)?;
        }
        let params = t.params.iter().map(|p| (p.name.clone(), p.ty.clone())).collect();
        self.tasks.insert(t.name.clone(), TaskInfo { types: t.types.clone(), params, passing: t.params.iter().map(|p| p.passing).collect(), ret: t.ret.clone(), owner: t.owner.clone() });
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
        // The type of one item of a group's tuple, `P#0`, asks what its group does.
        let name = name.split(GROUP_ITEM).next().unwrap_or(name);
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
    /// A task's type written at `span`, its regions resolved as a header's
    /// are, its own; `kept` is one it may also name, the region a lambda
    /// keeps what it uses in (see `ClosureType`).
    fn resolve_task_type(&mut self, task: &TaskType, span: Span, kept: Option<&str>) -> Result<TaskType> {
        let params = task.params.iter().map(|param| self.resolve_regions(param, span, TypePlace::Header, None)).collect::<Result<Vec<_>>>()?;
        let ret = self.resolve_regions(&task.ret, span, TypePlace::Header, None)?;
        let mut regions: Vec<&str> = Vec::new();
        for (i, _) in task.params.iter().enumerate() {
            let Some(region) = task.region(i) else { continue };
            if regions.contains(&region) {
                return err(self.place(span, &format!("@{}", region)), format!("this task's type takes region '{}' twice; each region it takes has a name of its own", region));
            }
            regions.push(region);
        }
        let named: Vec<&str> = params.iter().flat_map(Type::regions).chain(regions.iter().copied()).chain(kept).collect();
        if let Some(region) = ret.regions().into_iter().find(|region| !named.contains(region)) {
            return err(
                self.place(span, &format!("@{}", region)),
                format!("what this task sends lives in region '{}', which none of its parameters names, so no call could say which region that is", region),
            );
        }
        Ok(TaskType { params, taken: task.taken.clone(), ret })
    }

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
            Type::Map(key, value, region) => {
                given(region)?;
                if needs_region && region.is_none() {
                    return err(span, format!("a map lives in a region, so its type names one, as in '{}@r'", ty));
                }
                if let (Some(region), Some(inside)) = (region, inside)
                    && region != inside
                {
                    return err(
                        self.place(span, &format!("@{}", region)),
                        format!("everything a value in region '{0}' holds is in '{0}', its maps too; write '@{0}' for '@{1}'", inside, region),
                    );
                }
                let key = self.resolve_regions(key, span, place, region.as_deref())?;
                let value = self.resolve_regions(value, span, place, region.as_deref())?;
                Ok(Type::Map(Box::new(key), Box::new(value), region.clone()))
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
                if let Some((word, params, _)) = self.takes_types(name).filter(|(_, params, _)| !params.is_empty()) {
                    let names: Vec<&str> = params.iter().map(|param| param.name.as_str()).collect();
                    return err(
                        self.place(span, name),
                        format!(
                            "{} '{}' takes {}, so name {}, as in '{}<{}>'",
                            word,
                            unqualified(name),
                            count(params.len(), "type"),
                            if params.len() == 1 { "it" } else { "all of them" },
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
            // Its regions are its own, as a header's are, so it names them as a
            // header does, and what it sends is in regions its parameters name.
            Type::Task(task) => Ok(Type::Task(Box::new(self.resolve_task_type(task, span, None)?.normalized()))),
            // `[@r](int) -> int`: a lambda keeping what it uses in `r`, which
            // its task's type may name too.
            Type::Closure(closure) => {
                if matches!(place, TypePlace::Field | TypePlace::Shared) {
                    return err(
                        self.place(span, &format!("@{}", closure.region)),
                        format!("a field can't hold a lambda that keeps what it uses yet, '{}'; keep it in a variable, or hold a task value, '{}'", ty, Type::Task(Box::new(closure.task.clone()))),
                    );
                }
                // After `new`, its region is the one `new` allocates in, which
                // `check_new` sees.
                if place != TypePlace::New {
                    given(&Some(closure.region.clone()))?;
                }
                if let Some(inside) = inside.filter(|inside| *inside != closure.region) {
                    return err(
                        self.place(span, &format!("@{}", closure.region)),
                        format!("everything a value in region '{0}' holds is in '{0}', what a lambda keeps too; write '@{0}' for '@{1}'", inside, closure.region),
                    );
                }
                let task = self.resolve_task_type(&closure.task, span, Some(&closure.region))?;
                Ok(Type::Closure(Box::new(ClosureType { task, region: closure.region.clone(), flex: closure.flex }.normalized())))
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
                    if name == THIS_WORD && self.called_on_a_value() {
                        return err(span, self.cannot_allocate_in_this("allocate where the value is kept, and give the method what you made"));
                    }
                    if self.current_lambda.is_some() {
                        return err(span, format!("this lambda can't allocate in '{0}', which it only names in its types; take it as a parameter, '{0}: region'", name));
                    }
                    return err(
                        span,
                        format!("this task can't allocate in '{0}', which it only names in its types; take it as a parameter, '{0}: region'", name),
                    );
                }
                Ok(())
            }
            Some(_) => err(span, format!("'{}' is a variable, not a region", name)),
            None if self.current_lambda.as_ref().and_then(|lambda| self.lambda_outer.get(lambda)).is_some_and(|outer| outer.contains(name)) => {
                if let Some(kept) = self.current_lambda.as_ref().and_then(|lambda| self.kept_lambdas.get(lambda)) {
                    return err(
                        span,
                        format!("region '{0}' belongs to the code around this lambda, which keeps what it uses in '{1}' and sees no other region of it; use '{1}', declare a region of its own inside it, or take one as a parameter, '{0}: region'", name, kept.region),
                    );
                }
                err(
                    span,
                    format!("region '{0}' belongs to the code around this lambda, and a lambda takes nothing from it; declare a region of its own inside it, or take one as a parameter, '{0}: region'; a lambda that keeps what it uses names the region it keeps it in, '[@{0}](…)'", name),
                )
            }
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
    /// `nothing` is only valid as a return type, which is checked separately, and
    /// as the value side of `nothing ~ E`.
    fn check_type(&self, ty: &Type, span: Span) -> Result<()> {
        if holds_held_lambda(ty) {
            return err(span, format!("{} holds a '[flex @r]' lambda, which changes what it keeps and is never copied, so it is held alone, in a 'flex' variable", ty));
        }
        match ty {
            Type::Nothing => err(self.place(span, "nothing"), "'nothing' is only allowed as a return type"),
            Type::Closure(closure) => self.check_type(&Type::Task(Box::new(closure.task.clone())), span),
            Type::Region => err(self.place(span, "region"), "'region' is only the type of a task's parameter, as in 'task build(r: region)'"),
            // Only a task's header lists several types, and `check_header`
            // checks each of them on its own.
            Type::Results(_) => err(span, "several types, separated by commas, are only a task's results"),
            Type::Ptr(inner, _) => self.check_type(inner, span),
            Type::Map(key, value, _) => {
                if let Some(why) = key_problem(key, &|name| self.type_param(name).is_some()) {
                    return err(span, format!("a map's key is compared and hashed, so it is an int, a unit, a char, a bool, text, a pointer, or a tuple of them, not {}", why));
                }
                if **value == Type::Nothing {
                    return err(self.place(span, "nothing"), "a map can't hold 'nothing', which has no value");
                }
                self.check_type(key, span)?;
                self.check_type(value, span)
            }
            Type::Array(inner, _) | Type::Slice(inner, _) => match inner.as_ref() {
                Type::Nothing => {
                    let what = if matches!(ty, Type::Array(..)) { "an array" } else { "a slice" };
                    err(self.place(span, "nothing"), format!("{} can't hold 'nothing', which has no value", what))
                }
                _ => self.check_type(inner, span),
            },
            Type::Maybe(inner) => match inner.as_ref() {
                Type::Maybe(_) => err(span, "a maybe can't hold another maybe"),
                Type::Nothing => err(self.place(span, "nothing"), "a maybe can't hold 'nothing', which has no value"),
                _ => self.check_type(inner, span),
            },
            Type::Or(value, error) => {
                // One error side, whichever side of the `~` the other is written
                // on: `(int ~ text) ~ text` and `int ~ (text ~ int)` name two, as
                // `maybe maybe int` names two emptinesses.
                if matches!(value.as_ref(), Type::Or(..)) || matches!(error.as_ref(), Type::Or(..)) {
                    return err(self.place(span, "~"), "a type has one error type: '~' can't appear twice");
                }
                if **value != Type::Nothing {
                    self.check_type(value, span)?;
                }
                match error.as_ref() {
                    Type::Nothing => return err(self.place(span, "~"), "the error type can't be 'nothing'"),
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
            // `(int) -> int`: its parameters are values, places, and regions
            // written `@r`, and it sends one value or nothing, as a header does.
            Type::Task(task) => {
                for (i, param) in task.params.iter().enumerate() {
                    match param {
                        _ if task.region(i).is_some() => {}
                        Type::Nothing => return err(self.place(span, "nothing"), "a task takes no 'nothing', which has no value; '()' takes no values, as in '() -> int'"),
                        Type::Region => {
                            return err(self.place(span, "region"), "a task's type writes a region it takes as '@r', named so that what it sends can live there, as in '(@r, int) -> [int]@r'");
                        }
                        _ => self.check_type(param, span)?,
                    }
                }
                match &task.ret {
                    Type::Nothing => Ok(()),
                    Type::Region => err(self.place(span, "region"), "a task can't send a region; regions are only parameters, as in 'task build(r: region)'"),
                    Type::Or(value, _) if **value == Type::Nothing => self.check_type(&task.ret, span),
                    ret => self.check_type(ret, span),
                }
            }
            // `P...`: a group of types the task takes, which the parser lets
            // only a parameter's whole type, and all a task's type takes, be.
            Type::Group(name) => match self.type_param(name) {
                Some(param) if param.group => Ok(()),
                Some(_) => err(self.place(span, name), format!("'{0}' is one type, not a group; a task takes a group of types as '<{0}...>'", name)),
                None => err(self.place(span, name), format!("unknown group of types '{0}'; a task takes one as '<{0}...>'", name)),
            },
            // `P` alone, inside a task: one value's type, as `v` has in `each v in args`.
            Type::Named(name, _) if self.group_turns.iter().any(|(group, _)| group == name) && self.type_param(name).is_some_and(|param| param.group) => Ok(()),
            Type::Named(name, _) if self.type_param(name).is_some_and(|param| param.group) => err(
                self.place(span, name),
                format!("'{0}' is a group of types, so a header writes it '{0}...': a parameter's whole type, as in 'args: {0}...', or in a task's type, as in '({0}...) -> R' or '(int, {0}...) -> R'; '{0}' alone is one value's type, written only inside 'each v in args', as the type 'v' has", name),
            ),
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
                let fields = self.applied_parts(applied)?;
                let found = fields.iter().find_map(|ty| self.changeable_item(ty))?;
                Some(format!("'{}', which holds {}", ty, found))
            }
            // Only a type required to be `frozen` is known never to change.
            Type::Param(name) if !self.allows(Need::Frozen, ty) => {
                Some(format!("'{}', which {}", name, self.why_not(Need::Frozen, ty)))
            }
            Type::Group(name) if !self.allows(Need::Frozen, ty) => {
                Some(format!("'{}...', which {}", name, self.why_not(Need::Frozen, ty)))
            }
            Type::Ptr(..) => Some(format!("a pointer ({}), whose value can change", ty)),
            Type::Slice(..) => Some(format!("a slice ({}), whose items can change", ty)),
            Type::Map(..) => Some(format!("a map ({}), whose values can change", ty)),
            Type::Array(..) => Some(format!("an array ({})", ty)),
            Type::Nothing => Some("'nothing', which has no value".to_string()),
            Type::Named(name, _) if self.changeable.contains(name) => {
                Some(format!("'{}', which holds a pointer, a slice, or an array", unqualified(name)))
            }
            Type::Maybe(inner) => self.changeable_item(inner),
            Type::Or(value, error) => self.changeable_item(value).filter(|_| **value != Type::Nothing).or_else(|| self.changeable_item(error)),
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
        // holds: a shape's fields, or each variant's values. A shape or kind
        // made for a set of types (`Out<int>`, `Option<int>`) is one of them,
        // with what it was made with, since what it holds says whether it can
        // be (see `shape_instance` and `kind_instance`).
        let mut types: Vec<(&str, Vec<Vec<Type>>)> = Vec::new();
        for item in items {
            match item {
                Item::Shape(s) => types.push((s.name.as_str(), vec![s.fields.iter().map(|f| f.ty.clone()).collect()])),
                Item::Kind(k) => types.push((k.name.as_str(), k.variants.iter().map(|v| v.payload.iter().map(|f| f.ty.clone()).collect()).collect())),
                Item::Task(_) | Item::Macro(_) => {}
            }
        }
        for made in self.made_of.keys() {
            if let Some(shape) = self.shapes.get(made) {
                types.push((made.as_str(), vec![shape.fields.iter().map(|f| f.ty.clone()).collect()]));
            } else if let Some(kind) = self.kinds.get(made) {
                types.push((made.as_str(), kind.variants.iter().map(|v| v.payload.iter().map(|f| f.ty.clone()).collect()).collect()));
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
        self.current_lambda = is_lambda(&task.name).then(|| task.name.clone());
        self.current_task = task.name.clone();
        self.param_index = task.params.iter().enumerate().map(|(i, param)| (param.name.clone(), i)).collect();
        self.current_method = task.owner.clone();
        self.current_owner = task.owner.as_ref().filter(|owner| owner.alters).and(task.params.first()).and_then(|this| self.owner_of(&this.ty));
        self.current_type = task.owner_type().map(str::to_string);
        self.current_shape = match &task.owner {
            // A shared method is its shape's own, with no value it is called on.
            Some(owner) if owner.shared => task.owner_type().filter(|ty| self.shapes.contains_key(*ty)).map(str::to_string),
            Some(_) => task.params.first().and_then(|this| self.owner_of(&this.ty)),
            None => None,
        };
        // Parameters and the task body share one scope, as in C.
        self.scopes = vec![Scope::default()];
        // A lambda's body is a task of its own, inside no loop of the code around it.
        self.loops.clear();
        self.loop_name = None;
        self.stepping = None;
        // A lambda that keeps what it uses sees the variables of the code
        // around it, and the region it keeps them in, under its own (see
        // `kept_scope`); one whose value was never checked has none to see.
        if task.kept.is_some() {
            // A header with a mistake, told already, leaves nothing to keep.
            match self.kept_scope(&task.name).filter(|_| !unsettled) {
                Some(around) => self.scopes.insert(0, around),
                None => {
                    self.scopes.clear();
                    self.current_ret = None;
                    return Ok(());
                }
            }
        }
        for param in &task.params {
            if param.ty == Type::Region && param.passing != Passing::Copy {
                let why = if param.passing.changes() { "never changes" } else { "is given as it is" };
                self.record(Error::at(param.span, format!("a region {}, so '{}' can't be '{}'", why, param.name, param.passing.word())));
            }
            if param.ty == Type::Region {
                self.define_region(&param.name, param.span, true)?;
                // Given to the task, so it outlives the task's variables.
                if let Some(info) = self.scopes.last_mut().and_then(|scope| scope.names.get_mut(&param.name)) {
                    info.param = true;
                }
            } else {
                // Only the value a method marked `alters` is called on changes,
                // and a parameter the task holds a copy of (`flex x: T`) or the
                // caller's place (`flex& x: T`).
                let alters = param.name == THIS && task.owner.as_ref().is_some_and(|owner| owner.alters);
                self.define_param(&param.name, known(&param.ty), alters || param.passing.changes(), param.passing, param.span)?;
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
            self.natives.insert(task.name.clone());
            self.scopes.clear();
            self.current_ret = None;
            self.current_method = None;
            self.current_owner = None;
            self.current_shape = None;
            self.current_type = None;
            self.result_names.clear();
            self.unset.clear();
            return Ok(());
        }
        let before = self.errors.len();
        if is_startup(&task.name) {
            self.check_startup(&mut task.body, &task.passes);
        } else {
            let checked = self.check_block(&mut task.body);
            // What a lambda that keeps what it uses keeps: what it used.
            let used = std::mem::take(&mut *self.kept_used.borrow_mut());
            if let Some(kept) = self.kept_lambdas.get_mut(&task.name) {
                for place in kept.places.iter().filter(|place| !used.contains(&place.name)) {
                    self.warnings.push(Error::at(place.span, format!("this lambda keeps '{0}' as a place, '{1} {0}', and never uses it; leave it out of the list", place.name, place.passing.word())));
                }
                kept.uses = used;
            }
            checked?;
        }
        // Whether every path sends a value can only be told of a body that
        // checked: a line with a mistake may well be the one that sends, so a
        // task with a mistake anywhere is not told this as well (see `record`).
        // A method of a shape with a mistake reads its fields as unknown, so its
        // body settles nothing that reads them (see `broken`).
        let owner_broken = task.owner.is_some() && method_parts(&task.name).is_some_and(|(of, _)| self.broken.contains(of));
        let whole = self.errors.len() == before && !unsettled && !owner_broken;
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
        self.current_lambda = None;
        self.current_task.clear();
        self.current_ret = None;
        self.current_method = None;
        self.current_owner = None;
        self.current_shape = None;
        self.current_type = None;
        let ends_well = if self.result_names.is_empty() {
            if whole && sends_value(&task.ret) && !always_sends(&task.body) {
                err(task.span, format!("{} can finish without sending a value", shown_task(&task.name)))
            } else {
                Ok(())
            }
        } else if whole
            && !always_sends(&task.body)
            && let Some(name) = self.first_unset()
        {
            // Reaching the end sends the results, so they must all be set.
            err(task.span, format!("{} can finish without setting '{}'", shown_task(&task.name), name))
        } else {
            Ok(())
        };
        self.result_names.clear();
        self.unset.clear();
        ends_well
    }

    /// Checks `block` as a path of its own, and returns the results it leaves
    /// unset, or `None` when every path through it leaves it (sending, or with
    /// `break` or `continue`), so nothing after it runs on this path. `entry` is the state the block starts from.
    fn branch_state(&mut self, block: &Block, entry: &HashSet<String>) -> Option<HashSet<String>> {
        let after = std::mem::replace(&mut self.unset, entry.clone());
        (!always_leaves(block)).then_some(after)
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

    /// Starts a loop, with the name `outer =>` gave it, if any.
    /// Hides the loop giving a value about to be entered, where it `gives`,
    /// from what runs before its passes, an `each`'s source and a three-part
    /// loop's start: a loop there, one deep as it will be, gives it nothing
    /// with its `break`. Gives where it was, for `show_value_loop`.
    fn hide_value_loop(&mut self, gives: bool) -> Option<usize> {
        let depth = self.loops.len() + 1;
        match self.value_loops.last_mut() {
            Some((at, _)) if gives && *at == depth => Some(std::mem::replace(at, usize::MAX)),
            _ => None,
        }
    }

    fn show_value_loop(&mut self, hidden: Option<usize>) {
        if let (Some(depth), Some((at, _))) = (hidden, self.value_loops.last_mut()) {
            *at = depth;
        }
    }

    fn enter_loop(&mut self) {
        let name = self.loop_name.take();
        self.loops.push(name);
    }

    /// `loop start || condition || step` (see `PartedLoop`), named `named`, in
    /// the scope its start's names live in.
    /// Its passes give a value where it `gives` (see `ValueLoop`).
    fn check_parted(&mut self, parted: &mut PartedLoop, named: Option<(String, Span)>, gives: bool, span: Span) -> Result<()> {
        if let Some((name, at)) = unused_in_loop(parted) {
            return err(at, format!("'{}' is declared for this loop, and no part of it uses it; leave it out", name));
        }
        // A region lives until its block closes, and the start and the step
        // are no blocks.
        if let Some(region) = parted.start.iter().chain(&parted.step).find(|item| matches!(item.kind, StmtKind::Region { .. })) {
            return err(region.span, "a loop's start and step declare no region, as they are no block of their own; declare it before the loop, or in its body for each pass");
        }
        // The start runs once, before the loop: what it sets is set after it,
        // and a jump there leaves a loop around this one. A name without
        // `flex` or `fixed` is one already declared, which it changes.
        for item in parted.start.iter_mut() {
            let mut places = Vec::new();
            match &item.kind {
                StmtKind::Assign(targets, _) => targets.iter().for_each(|target| target_places(target, &mut places)),
                StmtKind::Step { target, .. } => places.push(target),
                _ => {}
            }
            if let Some((name, at)) = places.iter().find_map(|place| match &place.kind {
                ExprKind::Var(name) if self.lookup(name).is_none() => Some((name.clone(), place.span)),
                _ => None,
            }) {
                return err(
                    at,
                    format!("unknown variable '{0}'; a loop's start sets names already declared, and declares its own after 'flex' or 'fixed', as in 'loop flex {0} = 0 || …'", name),
                );
            }
            // The loop's name is no name of its start, whose names live in the
            // loop, though a jump there leaves the loop around it.
            if let (StmtKind::Binding { names, .. }, Some((loop_name, at))) = (&item.kind, &named) {
                let mut declared = Vec::new();
                names.iter().for_each(|bound| bound_names(bound, &mut declared));
                if let Some((name, span)) = declared.into_iter().find(|(name, _)| name == loop_name) {
                    return err(span, format!("'{}' is already the name of a loop around this, at {}:{}", name, at.line, at.col));
                }
            }
            let hidden = self.hide_value_loop(gives);
            let checked = self.check_stmt(item);
            self.show_value_loop(hidden);
            checked?;
        }
        // Its body might not run, so it sets nothing for certain; its condition
        // is tested inside it, so a plan B there may stop it.
        let unchanging = parted.cond.as_ref().and_then(|cond| unchanging_reads(cond, &parted.body, &parted.step, named.as_ref().map(|(name, _)| name.as_str())));
        let entry = self.unset.clone();
        let stepping = self.stepping;
        self.loops.push(named);
        let result = self.check_parted_passes(parted, gives);
        self.loops.pop();
        self.stepping = stepping;
        self.unset = entry;
        result?;
        match &parted.cond {
            None if !parted.endless && parted.start.is_empty() && parted.step.is_empty() => self.warnings.push(Error::at(span, ENDLESS_UNSAID)),
            None => {}
            Some(cond) if says_true(cond) => {
                if parted.start.is_empty() && parted.step.is_empty() {
                    self.warnings.push(Error::at(span, ENDLESS_UNSAID));
                }
            }
            Some(cond) => {
                if let Some(names) = unchanging {
                    self.warn_unchanging(&names, cond.value.span);
                }
            }
        }
        Ok(())
    }

    /// The condition, the body and the step of a loop in three parts, inside it.
    fn check_parted_passes(&mut self, parted: &mut PartedLoop, gives: bool) -> Result<()> {
        let given = match &mut parted.cond {
            Some(cond) => self.check_condition(cond, &mut parted.body, gives.then_some(LOOP_BRANCHES))?,
            None => self.check_scoped_block(&mut parted.body).map(|()| None)?,
        };
        // The step runs after each pass, where a `continue` of this loop leads,
        // so it can't have one; what it declares ends with it.
        self.stepping = Some(self.loops.len());
        for item in parted.step.iter_mut() {
            if let StmtKind::Binding { names, .. } = &item.kind {
                let at = names.first().map_or(item.span, |bound| bound.span);
                return err(at, "a loop's step runs to its end at once, so a name declared there ends before anything can use it; declare it in the start, after 'flex'");
            }
            self.check_stmt(item)?;
        }
        self.loop_given = given;
        Ok(())
    }

    /// Warns, at `at`, about a loop whose condition reads only the variables
    /// `names`, which nothing in it changes and nothing in it leaves (see
    /// `unchanging_reads`), so it never ends once it starts: where each is a
    /// plain value of this task, which no reference or lambda may change
    /// elsewhere.
    fn warn_unchanging(&mut self, names: &[String], at: Span) {
        let plain = |info: &VarInfo| {
            info.refers.is_none()
                && !info.kept
                && matches!(info.passing, Passing::Copy | Passing::Flex | Passing::Fixed)
                && !matches!(info.ty, Type::Ptr(..) | Type::Slice(..) | Type::Map(..) | Type::Task(_) | Type::Closure(_) | Type::Unknown)
        };
        if !names.iter().all(|name| self.lookup(name).is_some_and(plain)) {
            return;
        }
        // A reference or a lambda that keeps a place may change it anywhere.
        if self.scopes.iter().flat_map(|scope| scope.names.values()).any(|info| info.refers.is_some())
            || self.kept_lambdas.values().any(|kept| kept.places.iter().any(|place| names.contains(&place.name)))
        {
            return;
        }
        let read: Vec<String> = names.iter().map(|name| format!("'{}'", name)).collect();
        self.warnings.push(Error::at(
            at,
            format!("this loop never ends: its condition reads only {}, which nothing in the loop changes, and nothing in it leaves the loop", read.join(" and ")),
        ));
    }

    /// A loop that gives a value (see `ValueLoop`), and its type: `V ~
    /// LoopResult<B, M>`, `V` what the last lines of its passes give, `B` what
    /// its `break`s give and `M` what its last lines fail with, each `bool`
    /// where none is given. As a statement with a plan B, it gives nothing.
    fn check_loop_value(&mut self, value: &mut ValueLoop, span: Span) -> Result<Type> {
        let never_ends = match &value.stmt.kind {
            StmtKind::Parted(parted) => parted.cond.as_ref().is_none_or(says_true),
            StmtKind::Loop(cond, _) => says_true(cond),
            _ => false,
        };
        if never_ends && !value.gives {
            return err(span, "a loop that never ends by itself ends only by a 'break' or a '<-', so a plan B after it would run every time it ends; do what it does before the 'break' instead");
        }
        if never_ends {
            return err(
                span,
                "a loop that never ends by itself gives no value, since no pass of it is the final one; give it a condition, as in 'loop i < n', and stop it early with 'break = v'",
            );
        }
        // A name written with `=>` follows the rule of names, as a statement's does.
        if let Some((name, at)) = value.name.as_ref().filter(|_| value.arrow) {
            self.push_scope();
            let free = self.define(name, Type::Unknown, false, *at);
            self.pop_scope();
            free?;
        }
        self.loop_name = value.name.clone();
        self.value_loops.push((self.loops.len() + 1, Vec::new()));
        self.loop_gives = value.gives;
        self.loop_given = None;
        // The statement is the loop the value is, whose last lines the place
        // the value goes may tell more of (see `settle_variant`).
        let result = self.check_stmt_kind(&mut value.stmt);
        self.loop_gives = false;
        self.loop_name = None;
        let breaks = self.value_loops.pop().map(|(_, breaks)| breaks).unwrap_or_default();
        let given = self.loop_given.take();
        // `each v in args` is written out once per value, each pass its own:
        // said first, as what its passes give is no matter then.
        if let StmtKind::Each(_, _, range, _) = &value.stmt.kind
            && matches!(range.ty, Some(Type::Group(_)))
        {
            return err(range.span, "a loop through the values of a group gives no value, since each pass is written out for a value of its own; give the values on to a task, or gather what the passes give in a variable");
        }
        result?;
        // What the last lines give: a value, and what they may fail with.
        let (given, at) = match (value.gives, given) {
            (false, _) => (Type::Nothing, span),
            (true, Some(given)) => given,
            (true, None) => return err(span, "every pass of this loop leaves, with '<-', 'break' or 'continue', so its last line never gives its value"),
        };
        let (given, manual) = match given {
            Type::Or(value, error) => (*value, Some(*error)),
            Type::Fail(_) => return err(at, "the last line of every pass of this loop fails, so it never gives a value; give one on a path of its own"),
            other => (other, None),
        };
        if matches!(given, Type::None) {
            return err(at, "the last line of this loop gives only none, which doesn't say its type; give a value, or 'none' beside one in a 'when'");
        }
        value.open = (!breaks.iter().any(|(ty, _)| *ty != Type::None), manual.is_none());
        let manual = match manual {
            None => Type::Bool,
            Some(error) => self.manual_type(&error, value, at)?,
        };
        // What the `break`s give, or `bool` where none does.
        let mut broke: Option<Type> = None;
        for (ty, at) in &breaks {
            let ty = match ty {
                Type::None => continue,
                Type::Maybe(inner) => (**inner).clone(),
                other => other.clone(),
            };
            broke = Some(match broke {
                None => ty,
                Some(before) => self.join(&before, &ty).filter(|joined| !matches!(joined, Type::Maybe(_) | Type::Or(..))).ok_or_else(|| {
                    Error::at(*at, format!("this 'break' gives {}, and another gives {}; the breaks of a loop give one type", ty, before))
                })?,
            });
        }
        let broke = broke.unwrap_or(Type::Bool);
        if holds_untold(&broke)
            && let Some((ty, at)) = breaks.iter().find(|(ty, _)| holds_untold(ty))
        {
            // Its statement has said so already, as of a variant nothing told.
            if self.errors.iter().any(|error| error.span == Some(*at)) {
                return Ok(Type::Unknown);
            }
            return err(
                *at,
                format!("this 'break' gives a variant whose values don't say all of its types, and nothing tells a loop's breaks more; write them, as in '{}'", untold_written(ty)),
            );
        }
        // `LoopResult<B, M>`, in the region of what it holds, which is one, and
        // which outlives every pass, so no pass declares it.
        let mut failures = Vec::new();
        if let Some(body) = value.body() {
            last_values(body, &mut failures);
        }
        let manual_at = failures.iter().find(|e| may_fail(e)).map_or(at, |e| e.span);
        let held: Vec<(String, Span)> = breaks
            .iter()
            .chain([&(manual.clone(), manual_at)])
            .flat_map(|(ty, at)| ty.regions().into_iter().map(|region| (region.to_string(), *at)))
            .filter(|(region, _)| region != ANY_REGION)
            .collect();
        for (region, at) in &held {
            if !unnamed_region(region) && self.lookup(region).is_none() {
                return err(*at, format!("this value is in region '{}', which is freed when the pass ends, so it can't be what the loop fails with", region));
            }
        }
        if let Some((first, _)) = held.first()
            && let Some((other, other_at)) = held.iter().find(|(region, _)| region != first)
        {
            return err(
                *other_at,
                format!("this value is in region '{}', and another the loop fails with is in region '{}'; what a loop fails with is in one region, so give both in the same one", other, first),
            );
        }
        let region = held.first().map_or(ANY_REGION.to_string(), |(region, _)| region.clone());
        let result = Applied { name: library::LOOP_RESULT.to_string(), types: vec![broke, manual], region: Some(region) };
        let result = self.made_kinds(Type::Applied(Box::new(result)), span)?;
        let ty = Type::Or(Box::new(given), Box::new(result));
        if value.gives
            && let Some(body) = value.body_mut()
        {
            self.settle_value(body, &ty)?;
        }
        // Its value outlives every pass, so it is in no region one declares.
        if let Some(region) = ty.regions().into_iter().find(|region| !unnamed_region(region) && *region != ANY_REGION && self.lookup(region).is_none()) {
            let at = breaks.iter().find(|(ty, _)| ty.regions().contains(&region)).map_or(at, |(_, at)| *at);
            return err(at, format!("this value is in region '{}', which is freed when the pass ends, so it can't be the loop's value", region));
        }
        Ok(ty)
    }

    /// `M`, what the last lines of the loop `value` fail with, from `error`,
    /// the error they give: only `LoopResult.Manual(m)`, since the other
    /// variants say what the loop did.
    fn manual_type(&self, error: &Type, value: &ValueLoop, at: Span) -> Result<Type> {
        let wrong = || {
            err(
                at,
                "the last line of a loop that gives a value fails only with 'LoopResult.Manual(…)', which 'include std/loops {LoopResult}' brings in; 'break = v' stops it with a value",
            )
        };
        let Type::Applied(result) = self.unmade(error) else { return wrong() };
        if result.name != library::LOOP_RESULT {
            return wrong();
        }
        let mut failures = Vec::new();
        if let Some(body) = value.body() {
            last_values(body, &mut failures);
        }
        for failure in failures {
            let fails = may_fail(failure);
            let failure = match &failure.kind {
                ExprKind::Wrap(inner) => inner,
                _ => failure,
            };
            match &failure.kind {
                ExprKind::Fail(inner) => match &inner.kind {
                    ExprKind::Construct { tag, .. } if tag == "Manual" => {}
                    ExprKind::Construct { tag, .. } => {
                        return err(
                            inner.span,
                            format!("'{}' says what the loop did, so its last line fails only with 'LoopResult.Manual(…)'; 'break = v' stops it with a value", tag),
                        );
                    }
                    _ => return err(inner.span, "the last line of a loop that gives a value fails only with 'fail LoopResult.Manual(…)' written there, which says the loop failed by its own choice"),
                },
                // A value that may fail, as another loop's: what it fails with
                // says what that did, not this loop.
                _ if fails => {
                    return err(
                        failure.span,
                        "this may fail, and the last line of a loop that gives a value fails only with 'fail LoopResult.Manual(…)' written there; give it a plan B, as in 'x { 0 }'",
                    );
                }
                _ => {}
            }
        }
        Ok(result.types.get(1).cloned().filter(|ty| !is_untold(ty)).unwrap_or(Type::Bool))
    }

    /// The value of a `break` that stops the loop named `name`, or the
    /// innermost: one that gives a value takes it, as what it fails with.
    fn check_break_value(&mut self, name: Option<&(String, Span)>, value: Option<&mut Expr>) -> Result<()> {
        let target = match name {
            None => Some(self.loops.len()),
            Some((name, _)) => self.loops.iter().rposition(|named| named.as_ref().is_some_and(|(given, _)| given == name)).map(|i| i + 1),
        };
        let Some(value) = value else { return Ok(()) };
        let Some(at) = self.value_loops.iter().rposition(|(at, _)| Some(*at) == target) else {
            return err(
                value.span,
                "this loop gives no value, so its 'break' gives none; a loop gives one where it is a value, as in 'fixed r = loop … fi'",
            );
        };
        let ty = self.check_expr(value)?;
        if matches!(ty, Type::Nothing | Type::Fail(_) | Type::Or(..)) {
            return err(value.span, format!("a 'break' gives a value the loop fails with, and this is {}", ty));
        }
        // A turn of `each v in args` is written out for each value, so a value
        // of the turn's type would be of another type in each.
        let params = ty.params();
        if let Some((group, _)) = self.group_turns.iter().find(|(group, around)| target.is_some_and(|target| target <= *around) && params.contains(&group.as_str())) {
            return err(
                value.span,
                format!(
                    "this 'break' gives the loop around the loop through the group's values a value of type {0}, which each value of the group makes another type; give it a value of one type, as in 'v.str() at r'",
                    group
                ),
            );
        }
        if let Some((_, breaks)) = self.value_loops.get_mut(at) {
            breaks.push((ty, value.span));
        }
        Ok(())
    }

    /// `break` or `continue` (`word`), given the name of the loop it stops or
    /// goes on with, or none for the innermost: there must be such a loop
    /// around it, in this task.
    fn check_jump(&mut self, word: &str, name: Option<&(String, Span)>, span: Span) -> Result<()> {
        let (what, verb) = if word == "break" { ("stops a loop", "stop") } else { ("goes on with a loop's next pass", "go on with") };
        if self.loops.is_empty() {
            if self.current_lambda.is_some() {
                return err(span, format!("'{}' {}, and there is none around it in this lambda; a lambda's body is a task of its own, so it can't {} a loop around the lambda", word, what, verb));
            }
            return err(span, format!("'{}' {}, and there is none around it here", word, what));
        }
        // `continue` in a step of the loop it goes on with would run the step again.
        let target = match name {
            None => Some(self.loops.len()),
            Some((name, _)) => self.loops.iter().rposition(|named| named.as_ref().is_some_and(|(given, _)| given == name)).map(|i| i + 1),
        };
        if word == "continue" && target.is_some() && target == self.stepping {
            return err(span, "'continue' goes to this loop's step, and this is its step; leave it out, or 'break' to stop the loop");
        }
        let Some((name, at)) = name else { return Ok(()) };
        if self.loops.iter().flatten().any(|(given, _)| given == name) {
            return Ok(());
        }
        if self.loop_around_lambda(name).is_some() {
            return err(*at, format!("'{0}' names a loop around this lambda, and a lambda's body is a task of its own, so it can't {1} that loop; only the loops inside the lambda can be named here", name, verb));
        }
        if self.lookup(name).is_some() {
            return err(*at, format!("'{}' is a variable, not the name of a loop around this; a loop is named before its word, as in 'outer => each x in xs'", name));
        }
        let named: Vec<String> = self.loops.iter().flatten().map(|(given, _)| format!("'{}'", given)).collect();
        let around = match named.as_slice() {
            [] => format!("none of the loops around it has a name; a loop is named before its word, as in '{} => each x in xs'", name),
            [one] => format!("the one named loop around it is {}", one),
            many => format!("the named loops around it are {}", many.join(", ")),
        };
        err(*at, format!("no loop around this '{}' is named '{}': {}", word, name, around))
    }

    /// Notes the named loops around the lambda `lambda`'s value (see
    /// `lambda_loops`).
    fn note_lambda_loops(&mut self, lambda: &str) {
        let mut named: Vec<(String, Span)> = self.current_lambda.as_ref().and_then(|current| self.lambda_loops.get(current)).cloned().unwrap_or_default();
        named.extend(self.loops.iter().flatten().cloned());
        self.lambda_loops.insert(lambda.to_string(), named);
    }

    /// The named loop `name` around the lambda being checked, with where it
    /// is named.
    fn loop_around_lambda(&self, name: &str) -> Option<Span> {
        let lambda = self.current_lambda.as_ref()?;
        self.lambda_loops.get(lambda)?.iter().find(|(given, _)| given == name).map(|(_, at)| *at)
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
        let full = self.in_module(name);
        // Nor like a macro, which a bare name would read as.
        if let Some(info) = self.macros.get(&full) {
            let at = info.span;
            if at.file != span.file {
                return err(span, format!("'{}' is already the name of a macro this file includes", name));
            }
            return err(span, format!("'{}' is already defined at {}:{}, as a macro", name, at.line, at.col));
        }
        // Nor like a kind, which `Kind.Variant` would read as.
        if self.kinds.contains_key(&full) {
            return err(span, format!("'{0}' is the name of a kind, so '{0}.x' would name its variant; choose another name", name));
        }
        // Nor like a shape, which `Shape.f` would read as, for its shared field.
        if self.shapes.contains_key(&full) {
            return err(span, format!("'{0}' is the name of a shape, so '{0}.x' would name its shared field; choose another name", name));
        }
        // Nor may a task value take a task's name, which `name(…)` would call.
        if (is_task(&ty) || matches!(ty, Type::Closure(_)) || matches!(&ty, Type::Maybe(inner) if is_task(inner))) && self.tasks.contains_key(&full) {
            return err(span, format!("'{0}' is the name of a task, so '{0}(…)' would call it rather than this task value; choose another name", name));
        }
        // Nor may it take a module's name, which `name.x` would read as.
        if let Some(at) = self.module_used(name) {
            return err(span, format!("'{}' is the name of a module this file includes, at {}:{}", name, at.line, at.col));
        }
        // A lambda that keeps what it uses names nothing as the code around it
        // does, so a region of it, which it doesn't see, is no name of its own.
        if self.lookup(name).is_none()
            && let Some(lambda) = self.current_lambda.as_ref().filter(|lambda| self.kept_lambdas.contains_key(*lambda))
            && self.lambda_outer.get(lambda).is_some_and(|outer| outer.contains(name))
        {
            return err(span, format!("'{}' is already a name in the code around this lambda, and a lambda that keeps what it uses reuses none of those names, even ones it doesn't keep; name this one otherwise", name));
        }
        // Nor like a loop around it, which `break` and `continue` name. The
        // name is still declared, so its uses say nothing more.
        let clash = match (self.loops.iter().flatten().find(|(given, _)| given == name), self.loop_around_lambda(name)) {
            (Some((_, at)), _) => Some(format!("'{}' is already the name of a loop around this, at {}:{}", name, at.line, at.col)),
            (None, Some(at)) => Some(format!("'{}' is already the name of a loop around this lambda, at {}:{}", name, at.line, at.col)),
            (None, None) => None,
        };
        if let Some(existing) = self.lookup(name) {
            let at = existing.span;
            let what = match existing {
                _ if existing.kept => "a variable of the code around this lambda, which the lambda may keep, declared",
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
        // What it owns, where `bind_from` or a binding said so.
        let owners = match self.bound_owners.iter().position(|(at, _)| *at == span) {
            Some(i) => self.bound_owners.swap_remove(i).1,
            None => Vec::new(),
        };
        scope.names.insert(name.to_string(), VarInfo { ty, mutable, span, allocates, result: false, param: false, passing: Passing::Copy, refers: None, kept: false, owners });
        match clash {
            Some(clash) => err(span, clash),
            None => Ok(()),
        }
    }

    /// Declares a parameter, which the task's header gives it. Only the value a
    /// method marked `alters` is called on can change.
    fn define_param(&mut self, name: &str, ty: Type, mutable: bool, passing: Passing, span: Span) -> Result<()> {
        self.define(name, ty, mutable, span)?;
        if let Some(info) = self.scopes.last_mut().and_then(|scope| scope.names.get_mut(name)) {
            info.param = true;
            info.passing = passing;
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
                // A variable of the code around a lambda, which it keeps.
                if info.kept && !self.kept_used.borrow().iter().any(|used| used == name) {
                    self.kept_used.borrow_mut().push(name.to_string());
                }
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
        // A scope's names are in no order, so of its regions the one
        // declared last is taken.
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| {
                scope.names.iter().filter(|(_, info)| info.ty == Type::Region && info.allocates).max_by_key(|(_, info)| (info.span.line, info.span.col))
            })
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
            } else if self.sees(name) && self.macros.contains_key(name) {
                Error::at(span, format!("'{}' is a macro, a value that never changes, not a variable", unqualified(name)))
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
                if let Some(outer) = self.current_lambda.as_ref().and_then(|lambda| self.lambda_outer.get(lambda))
                    && outer.contains(name)
                {
                    // A lambda that keeps what it uses keeps variables, not the
                    // results of the task around it, which only `<-` sets.
                    if self.current_lambda.as_ref().is_some_and(|lambda| self.kept_lambdas.contains_key(lambda)) {
                        return Error::at(span, format!("'{0}' is a result of the task around this lambda, which a lambda doesn't keep; keep a variable of it first, as in 'fixed kept = {0}', and use that", name));
                    }
                    return Error::at(span, format!("'{0}' belongs to the code around this lambda, and a lambda takes nothing from it; give '{0}' as a parameter, or call a task that takes it; a lambda that keeps what it uses names the region it keeps its copies in, as in '[@r](…)'", name));
                }
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
    /// have declared. Only `fixed` declares a name the rest of the block sees;
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
        let kinds_before = self.untold_kinds.len();
        let result = self.check_stmt_kind(stmt);
        let unsettled = self.unsettled_copies.get(before).copied();
        let untold = self.unsettled_types.get(types_before).cloned();
        let untold_kind = self.untold_kinds.get(kinds_before).cloned();
        self.unsettled_copies.truncate(before);
        self.unsettled_types.truncate(types_before);
        self.untold_kinds.truncate(kinds_before);
        // A mistake about a value of a type nothing told, `Option<…>`, is
        // that nothing told it: `Option.None == Option.None`. A place that
        // tells other types is a mistake of its own (`Either<int, text>`
        // given `Either.Right(5)`).
        if let (Err(error), Some((at, kind, tag, param, example))) = (&result, &untold_kind)
            && ((!error.message.starts_with("expected type")
                && !error.message.starts_with("cannot infer")
                && error.message.contains(&format!("{}<", unqualified(kind)))
                && ["<…", "…>", ", …"].iter().any(|untold| error.message.contains(untold)))
                // `each x in [Option.None]`: no item tells its types.
                || error.message.starts_with("cannot tell the type of"))
        {
            return err(*at, untold_kind_message(kind, tag, param, example));
        }
        result?;
        if let Some((at, kind, tag, param, example)) = untold_kind {
            return err(at, untold_kind_message(&kind, &tag, &param, &example));
        }
        if let Some(at) = unsettled {
            return err(at, self.unsettled_copy());
        }
        match untold {
            Some((at, task, param)) => err(at, self.untold_type(&task, &param)),
            None => Ok(()),
        }
    }

    /// Makes `x++`, `x--`, or `x op= v` the assignment it is, `x = x op v`,
    /// which the rest checks as it checks any. The place is written twice, so
    /// nothing in it may call a task or change anything. `++` and `--` count
    /// whole numbers and chars; `t += u` on text joins into the region `t` is
    /// in.
    fn step_to_assignment(&mut self, stmt: &mut Stmt) -> Result<()> {
        let StmtKind::Step { target, op, value, symbol, at } = &mut stmt.kind else { return Ok(()) };
        let (op, symbol, at) = (*op, *symbol, *at);
        if let Some(inside) = changing_part(target) {
            let example = if matches!(symbol, "++" | "--") { format!("xs[i]{}", symbol) } else { format!("xs[i] {} …", symbol) };
            return err(
                inside,
                format!("'{}' names its place twice, reading and then setting it, so the place can't call a task or change anything; keep that part in a variable first, as in 'fixed i = next()' and then '{}'", symbol, example),
            );
        }
        let ty = self.check_expr(&mut target.clone())?;
        let counts = matches!(symbol, "++" | "--");
        if counts && !matches!(ty, Type::Int | Type::Unit | Type::Char | Type::Unknown) {
            let hint = match ty {
                Type::Float => format!("; for a float, write 'x {} 1.0'", if symbol == "++" { "+=" } else { "-=" }),
                _ => String::new(),
            };
            return err(at, format!("'{}' counts an int, a unit, or a char by one, and this is {}{}", symbol, a_type(&ty), hint));
        }
        // `f += 1`: a whole number written out doesn't become a float (a
        // power's may be an int, `f **= 2`).
        if ty == Type::Float
            && op != BinaryOp::Pow
            && let ExprKind::IntLit(n) = value.kind
        {
            return err(value.span, format!("'{}' on a float takes a float, and {} is an int; write it '{}.0', as in 'x {} {}.0'", symbol, n, n, symbol, n));
        }
        let place = target.clone();
        let value = std::mem::replace(value, Expr::new(ExprKind::NoneLit, at));
        let changed = match &ty {
            Type::Text(region) if op == BinaryOp::Add => {
                let Some(region) = region.clone() else {
                    return err(
                        at,
                        "'+=' joins text into the region the text is in, and this text lasts as long as the program, in no region; keep it in a region, as in 'flex t: text@r = …', or write 't = t + u at r'",
                    );
                };
                // `t += "{n}!"` and `t += a + b`: text holding values, and
                // text joined, go into the same region, part by part.
                let mut parts = vec![place];
                step_parts(value, &region, at, &mut parts);
                Expr::new(ExprKind::Join { parts, region, region_span: at }, at)
            }
            Type::Text(_) => return err(at, format!("'{}' works on numbers; text is only joined, with '+='", symbol)),
            _ => Expr::new(ExprKind::Binary(op, Box::new(place), Box::new(value)), at),
        };
        let target = std::mem::replace(target, Expr::new(ExprKind::NoneLit, at));
        stmt.kind = StmtKind::Assign(vec![Target::Place(target)], vec![changed]);
        Ok(())
    }

    fn check_stmt_kind(&mut self, stmt: &mut Stmt) -> Result<()> {
        if matches!(stmt.kind, StmtKind::Step { .. }) {
            self.step_to_assignment(stmt)?;
        }
        let span = stmt.span;
        match &mut stmt.kind {
            StmtKind::Step { .. } => Err(Error::internal_at(span, "a step left as it was")),
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
                    ExprKind::Invoke { callee, .. } if matches!(e.ty, Some(Type::Or(..))) => {
                        let name = match &callee.kind {
                            ExprKind::Var(name) => name.clone(),
                            _ => "this task".to_string(),
                        };
                        err(e.span, ignored_error(&name))
                    }
                    ExprKind::Invoke { .. } => Ok(()),
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
                        if e.ty == Some(Type::Nothing) || matches!(value.kind, ExprKind::Call { .. } | ExprKind::Scan { .. } | ExprKind::MethodCall { .. } | ExprKind::LoopValue(_)) =>
                    {
                        Ok(())
                    }
                    ExprKind::Binary(BinaryOp::Eq, ..) => err(e.span, "the result of '==' is unused; use '=' to assign"),
                    ExprKind::Fail(_) => err(
                        e.span,
                        "'fail …' makes a failure, which a line of its own drops; send it with '<- fail …', or stop a loop that gives a value with 'break = …'",
                    ),
                    _ => err(e.span, "this value is unused; only a task call can be used as a statement"),
                }
            }
            StmtKind::Binding { names, mutable, reference: true, init } => self.check_reference(names, *mutable, init),
            StmtKind::Binding { names, mutable, init, .. } => {
                // `fixed r = loop …`: the variable names the loop it gives, and a
                // `LoopResult` written with no types there takes the loop's,
                // `fixed r: int ~ LoopResult = loop …`.
                let mut from_loop = HashSet::new();
                let each_given = init.len() == names.len();
                for (i, bound) in names.iter_mut().enumerate() {
                    let Some(value) = init.get_mut(i).filter(|_| each_given) else { continue };
                    // `fixed r = loop … fi { 0 }` names it too.
                    if let ExprKind::PlanB { value, .. } = &mut value.kind
                        && let ExprKind::LoopValue(value) = &mut value.kind
                    {
                        if bound.parts.is_empty() && bound.name != DISCARD {
                            name_value_loop(value, &bound.name, bound.span)?;
                        }
                        continue;
                    }
                    let ExprKind::LoopValue(value) = &mut value.kind else { continue };
                    if bound.parts.is_empty() && bound.name != DISCARD {
                        name_value_loop(value, &bound.name, bound.span)?;
                    }
                    if let Some(Type::Or(given, error)) = &bound.ty
                        && matches!(error.as_ref(), Type::Named(name, None) if name == library::LOOP_RESULT)
                    {
                        bound.ty = Some((**given).clone());
                        from_loop.insert(i);
                    }
                }
                for bound in names.iter_mut() {
                    self.resolve_bound_types(bound)?;
                }
                // `flex y = &x`: a copy, where a reference was maybe meant.
                if let Some(marked) = init.iter().find(|value| matches!(value.kind, ExprKind::Ref(_))) {
                    let word = if *mutable { "flex" } else { "fixed" };
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
                    let annotated = match (annotated, &actual) {
                        (Some(given), Type::Or(_, result)) if from_loop.contains(&i) => Some(Type::Or(Box::new(given), result.clone())),
                        (annotated, _) => annotated,
                    };
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
                // What the names are given: a `flex` one may not take what a
                // fixed name owns, and one that never changes owns it too.
                for (i, bound) in names.iter().enumerate() {
                    let Some(value) = (if forwarded { init.first() } else { init.get(i) }) else { continue };
                    if *mutable {
                        if let Some(ty) = given.get(i).filter(|_| bound.name != DISCARD || !bound.parts.is_empty()) {
                            self.hand_as(value, ty, HandedTo::Variable(bound_text(bound)));
                        }
                        continue;
                    }
                    let owners = self.owners(value);
                    if !owners.is_empty() {
                        let mut spans = Vec::new();
                        bound_spans(bound, &mut spans);
                        self.bound_owners.extend(spans.into_iter().map(|at| (at, owners.clone())));
                    }
                }
                for (name, at, ty) in declared {
                    // A `[flex @r]` lambda changes what it keeps.
                    if held_lambda(&ty) && !*mutable {
                        return err(at, format!("'{}' is a '[flex @r]' lambda, which changes what it keeps, so it is held in a 'flex' variable: 'flex {} = …'", name, name));
                    }
                    if holds_held_lambda(&ty) {
                        return err(at, format!("'{}' would hold a '[flex @r]' lambda inside a value, which is copied with it; a '[flex @r]' lambda is held alone, in a 'flex' variable", name));
                    }
                    if name != DISCARD {
                        self.define(&name, ty, *mutable, at)?;
                    }
                }
                Ok(())
            }
            StmtKind::Assign(targets, values) => {
                // `total = loop …`: the variable names the loop it gives.
                for (target, value) in targets.iter().zip(values.iter_mut()) {
                    let value = match &mut value.kind {
                        ExprKind::PlanB { value, .. } => value,
                        _ => value,
                    };
                    if let (Target::Place(Expr { kind: ExprKind::Var(name), span, .. }), ExprKind::LoopValue(value)) = (target, &mut value.kind) {
                        name_value_loop(value, name, *span)?;
                    }
                }
                let mut places = Vec::new();
                let mut assigned: HashMap<String, Span> = HashMap::new();
                for target in targets.iter_mut() {
                    places.push(self.check_target(target, &mut assigned)?);
                }
                // `m[a], m[b] = 1, 2`: a key's value may be added, which only
                // the line setting it alone does.
                if targets.len() > 1
                    && let Some(at) = targets.iter().find_map(|target| match target {
                        Target::Place(Expr { kind: ExprKind::Index(map, _), span, .. }) if matches!(map.ty, Some(Type::Map(..))) => Some(*span),
                        _ => None,
                    })
                {
                    return err(at, "a map's value is set on a line of its own, as in 'm[k] = v', since it may add the key");
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
                            None => "'=' writes a value, so leave out '&'; a name for a place is declared, as in 'flex& y = &x'".to_string(),
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
                    // A place that changes may not take what a fixed name owns.
                    let to = match targets.get(i) {
                        Some(Target::Place(Expr { kind: ExprKind::Var(name), .. })) => HandedTo::Variable(name.clone()),
                        Some(Target::Place(place)) => HandedTo::Place(self.value_text(place)),
                        _ => continue,
                    };
                    if let Some(value) = if forwarded { values.first() } else { values.get(i) } {
                        self.hand_as(value, &expected, to);
                    }
                }
                Ok(())
            }
            StmtKind::SetResults(names, values) => self.check_set_results(span, names, values),
            StmtKind::When(arms, else_block) => self.check_when(span, arms, else_block, None).map(|_| ()),
            // Its statements run once, as the block around it does, so what they
            // set is set after it; what they declare ends at its `fi`.
            StmtKind::Block(body) => self.check_scoped_block(body),
            StmtKind::Loop(cond, body) => {
                let gives = std::mem::take(&mut self.loop_gives);
                if always_holds(cond)
                    && let Some(pattern) = &cond.pattern
                {
                    return err(pattern.span, "a tuple's items always match, so this loop would never end; take the tuple apart with 'fixed' instead");
                }
                // A loop's body might not run, so it sets nothing for certain.
                // Its condition is tested inside it, so a plan B there may stop it.
                // What the loop may do is read as written, before checking
                // makes its calls what they are.
                let unchanging = unchanging_reads(cond, body, &[], self.loop_name.as_ref().map(|(name, _)| name.as_str()));
                let entry = self.unset.clone();
                self.enter_loop();
                let result = self.check_condition(cond, body, gives.then_some(LOOP_BRANCHES));
                self.loops.pop();
                self.unset = entry;
                self.loop_given = result?;
                // `loop true`: one that never ends by itself says so in a word.
                if says_true(cond) {
                    self.warnings.push(Error::at(span, ENDLESS_UNSAID));
                } else if let Some(names) = unchanging {
                    self.warn_unchanging(&names, cond.value.span);
                }
                Ok(())
            }
            StmtKind::Parted(parted) => {
                // Its start's names live in it, which a scope of its own ends.
                let named = self.loop_name.take();
                let gives = std::mem::take(&mut self.loop_gives);
                self.push_scope();
                let result = self.check_parted(parted, named, gives, span);
                self.pop_scope();
                result
            }
            StmtKind::Named(name, inner) => {
                // The name is one in scope as any other: it may match no name
                // declared where it is seen, and nothing inside it may take it.
                self.push_scope();
                let free = self.define(name, Type::Unknown, false, span);
                self.pop_scope();
                // The loop is still checked, as named, for mistakes of its own.
                if let Err(error) = free {
                    self.record(error);
                }
                self.loop_name = Some((name.clone(), span));
                let result = self.check_stmt(inner);
                self.loop_name = None;
                result
            }
            StmtKind::Break(name, value) => {
                let name = name.clone();
                self.check_jump("break", name.as_ref(), span)?;
                self.check_break_value(name.as_ref(), value.as_deref_mut())
            }
            StmtKind::Continue(name) => {
                let name = name.clone();
                self.check_jump("continue", name.as_ref(), span)
            }
            StmtKind::Turns(_) => Err(Error::internal_at(span, "a loop written out for a group's values, checked")),
            StmtKind::Each(name, second, range, body) => {
                // What it goes through is found before it starts, outside it,
                // so a loop in a plan B there takes neither its name nor a `break`.
                let named = self.loop_name.take();
                let gives = std::mem::take(&mut self.loop_gives);
                let hidden = self.hide_value_loop(gives);
                let source = self.check_each_source(name, second.as_ref(), range);
                self.show_value_loop(hidden);
                let (item, value) = source?;
                if name == DISCARD && value.is_none() {
                    return err(span, "'_' drops a map's key or value, as in 'each _, v in m'; a loop over this gives one item each pass, so name it");
                }
                // `each v in args`: `P` alone is the type of `v` inside it.
                let turn = match &range.ty {
                    Some(Type::Group(group)) => Some(group.clone()),
                    _ => None,
                };
                if let Some(group) = &turn {
                    if self.group_turns.iter().any(|(turned, _)| turned == group) {
                        return err(
                            range.span,
                            format!("this goes through the values of the group '{0}...' inside a loop through them, where '{0}' alone would be the type of two values at once; give the turn's value and the group on to a task of its own that goes through them, as in 'each_with(v, args...)'", group),
                        );
                    }
                    self.group_turns.push((group.clone(), self.loops.len()));
                }
                // The loop variables and the body share one scope; `_` names none.
                let entry = self.unset.clone();
                self.loops.push(named);
                self.push_scope();
                // Its names own what the items are owned by.
                let mut each_names: Vec<Declared> = vec![(name.clone(), span, item.clone(), false)];
                if let (Some((second, at)), Some(value)) = (second.as_ref(), value.as_ref()) {
                    each_names.push((second.clone(), *at, value.clone(), false));
                }
                self.bind_from(&each_names, range);
                let mut result = if name == DISCARD { Ok(()) } else { self.define(name, item, false, span) };
                if let (Ok(()), Some((second, at)), Some(value)) = (&result, second.as_ref(), value)
                    && second != DISCARD
                {
                    result = self.define(second, value, false, *at);
                }
                let result = result.and_then(|()| {
                    if !gives {
                        return self.check_block(body);
                    }
                    self.loop_given = self.value_block_tail(body, span, LOOP_BRANCHES)?;
                    Ok(())
                });
                self.pop_scope();
                self.loops.pop();
                self.unset = entry;
                if turn.is_some() {
                    self.group_turns.pop();
                }
                result
            }
            StmtKind::Match(expr, cases) => self.check_match(span, expr, cases, None).map(|_| ()),
            StmtKind::Send(values) => {
                if let Some(what) = self.at_start {
                    return err(
                        span,
                        format!("'<-' leaves a task, and {} are worked out where the program starts, outside every task; give a value instead, as in 'int(t) {{ 0 }}'", what),
                    );
                }
                let ret = self.current_ret.clone().unwrap_or(Type::Nothing);
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
                    self.coerce(e, &ret)?;
                    self.note_sent(values);
                    return Ok(());
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
                self.note_sent(values);
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

    /// Checks a `when`: its arms, the `else when`s of a chain, one after
    /// another, and its `else`. In `gives`, every branch gives the whole a
    /// value, which the branches settle between them. A `when` that gives a
    /// value needs an `else`, since there is no value without one.
    fn check_when(&mut self, span: Span, arms: &mut [WhenArm], else_block: &mut Option<Block>, gives: Option<Branches>) -> Result<Option<Type>> {
        // A tuple's items always match, so nothing after such an arm could run.
        let last = arms.len().saturating_sub(1);
        for (i, arm) in arms.iter().enumerate() {
            if let Some(pattern) = &arm.cond.pattern
                && always_holds(&arm.cond)
                && (i < last || else_block.is_some())
            {
                let after = if i < last { "the arms after it" } else { "its 'else'" };
                return err(pattern.span, format!("a tuple's items always match, so this 'when' always holds and {} would never run; take the tuple apart with 'fixed' instead", after));
            }
        }
        if gives.is_some() && else_block.is_none() {
            return err(span, "a 'when' used as a value needs an 'else': without one, nothing gives the value when the condition doesn't hold");
        }
        let entry = self.unset.clone();
        let mut given = Vec::new();
        let mut states = Vec::new();
        for (i, arm) in arms.iter_mut().enumerate() {
            let value = match gives {
                None => self.check_condition(&mut arm.cond, &mut arm.body, None).map(|_| None)?,
                Some(_) => {
                    let found = self.check_condition(&mut arm.cond, &mut arm.body, gives);
                    self.note_branch(found, arm.span)
                }
            };
            given.extend(value.map(|(ty, at)| (i, ty, at)));
            // An arm known never to hold is no path.
            let state = self.branch_state(&arm.body, &entry);
            states.push(state.filter(|_| known_condition(&arm.cond) != Some(false)));
        }
        match else_block {
            Some(else_b) => {
                let value = match gives {
                    Some(branches) => {
                        let found = self.check_value_block(Vec::new(), else_b, span, branches);
                        self.note_branch(found, span)
                    }
                    None => self.check_scoped_block(else_b).map(|()| None)?,
                };
                given.extend(value.map(|(ty, at)| (arms.len(), ty, at)));
                states.push(self.branch_state(else_b, &entry));
            }
            // A condition that always holds leaves no other path.
            None if arms.iter().any(|arm| known_condition(&arm.cond) == Some(true)) => {}
            // Without an `else`, running no block is a path too.
            None => states.push(Some(entry.clone())),
        }
        self.join_branches(entry, states);
        let Some(branches) = gives else { return Ok(None) };
        let settled = self.settle_branches(span, &given, branches)?;
        let Some(settled) = settled else { return Ok(None) };
        let settled = self.made_kinds(settled, span)?;
        for (i, _, _) in given.iter().filter(|(_, ty, _)| !ty.unknown() && *ty != settled) {
            match (arms.get_mut(*i), else_block.as_mut()) {
                (Some(arm), _) => self.settle_value(&mut arm.body, &settled)?,
                (None, Some(else_b)) => self.settle_value(else_b, &settled)?,
                (None, None) => {}
            }
        }
        self.escaping_region(span, &given, &settled, branches)?;
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
            StmtKind::When(arms, Some(else_block)) => {
                for arm in arms.iter_mut() {
                    self.settle_value(&mut arm.body, settled)?;
                }
                self.settle_value(else_block, settled)
            }
            StmtKind::Match(_, cases) => cases.iter_mut().try_for_each(|case| self.settle_value(&mut case.body, settled)),
            _ => Ok(()),
        }
    }

    /// Refuses a value in a region that is freed before the `match` or `when`
    /// giving it ends: the region was declared inside a branch, so nothing
    /// outside can name it. The error is at the value of the first branch
    /// in it, of those `given`, or else at `at`.
    fn escaping_region(&mut self, at: Span, given: &[(usize, Type, Span)], settled: &Type, branches: Branches) -> Result<()> {
        let escaping = settled.regions().into_iter().find(|region| !unnamed_region(region) && self.lookup(region).is_none());
        match escaping {
            Some(region) => err(
                given.iter().find(|(_, ty, _)| ty.regions().contains(&region)).map_or(at, |(_, _, span)| *span),
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
        let narrowed = self.narrowed.len();
        let result = self.check_condition_parts(cond, body, gives);
        self.narrowed.truncate(narrowed);
        let result = result?;
        self.settle_type_tests(cond, body);
        Ok(result)
    }

    /// `check_condition`, before the type tests in it are settled.
    fn check_condition_parts(&mut self, cond: &mut Condition, body: &mut Block, gives: Option<Branches>) -> Result<Option<(Type, Span)>> {
        if !cond.single() {
            // `m is {x} and x > 0`: the names each part binds are seen by the
            // parts after it and by the block, all in one scope around them.
            let at = cond.value.span;
            self.push_scope();
            let result = self.check_parts(cond).and_then(|()| match gives {
                Some(branches) => self.check_value_block(Vec::new(), body, at, branches),
                None => self.check_scoped_block(body).map(|()| None),
            });
            self.pop_scope();
            return result;
        }
        let found = self.check_expr(&mut cond.value);
        let ty = self.or_unknown(found);
        let Some(pattern) = &mut cond.pattern else {
            self.expect_type(&ty, &Type::Bool, cond.value.span)?;
            return match gives {
                Some(branches) => self.check_value_block(Vec::new(), body, cond.value.span, branches),
                None => self.check_scoped_block(body).map(|()| None),
            };
        };
        if matches!(pattern.kind, PatternKind::Any) {
            return err(pattern.span, "'_' matches anything, so this condition would always hold");
        }
        let bindings = self.part_bindings(&ty, &mut cond.value, pattern)?;
        self.bind_from(&bindings, &cond.value);
        match gives {
            Some(branches) => self.check_value_block(bindings, body, pattern.span, branches),
            None => self.check_block_with(bindings, body).map(|()| None),
        }
    }

    /// Checks the parts of a condition joined with `and`, in the scope
    /// `check_condition` opened: each a `bool`, or an `is` whose names are
    /// declared there for the parts after it.
    fn check_parts(&mut self, cond: &mut Condition) -> Result<()> {
        let mut part = Some(cond);
        while let Some(current) = part {
            let found = self.check_expr(&mut current.value);
            let ty = self.or_unknown(found);
            match &mut current.pattern {
                None => self.expect_type(&ty, &Type::Bool, current.value.span)?,
                Some(pattern) if matches!(pattern.kind, PatternKind::Any) => {
                    return err(pattern.span, "'_' matches anything, so this condition would always hold");
                }
                Some(pattern) => {
                    let bindings = self.part_bindings(&ty, &mut current.value, pattern)?;
                    self.bind_from(&bindings, &current.value);
                    for (name, span, ty, mutable) in bindings {
                        if name != DISCARD {
                            self.define(&name, ty, mutable, span)?;
                        }
                    }
                }
            }
            part = current.and.as_deref_mut();
        }
        Ok(())
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
        last_loop_gives(last);
        self.check_block(rest)?;
        // A `when` or a `match` on the last line gives the value through its own
        // branches, which is what makes a chain of conditions give one.
        // One whose every branch leaves (`<-`, `break`, `continue`) leaves the
        // block too, which then gives nothing, as a last `<-` does.
        if gives_value(last) && always_leaves(std::slice::from_ref(last)) {
            self.check_stmt(last)?;
            return Ok(None);
        }
        if gives_value(last) {
            let span = last.span;
            return Ok(self.check_branching_value(last)?.map(|ty| (ty, span)));
        }
        if let StmtKind::Expr(e) = &mut last.kind {
            let ty = self.check_expr(e)?;
            if ty == Type::Nothing {
                return err(e.span, format!("the last line of a {} gives its value, but this has none", branches.each));
            }
            return Ok(Some((ty, e.span)));
        }
        self.check_stmt(last)?;
        if !always_leaves(body) {
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
            StmtKind::When(arms, else_block) => self.check_when(span, arms, else_block, Some(WHEN_BRANCHES)),
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
            match self.join(sofar, ty) {
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
                return err(at, format!("every {} of this {} leaves, with '<-', 'break' or 'continue', so nothing gives it a value", branches.each, branches.of));
            }
            // Every branch that gives a value had a mistake of its own, which is
            // already reported (see `Type::Unknown`).
            return Ok(None);
        };
        // Every branch must fit what they settled on, which the fold above only
        // promises for the ones it read before a widening or a disagreement.
        for (_, ty, span) in given.iter().filter(|(_, ty, _)| !ty.unknown()) {
            // A variant whose types it didn't tell is told them by the others.
            if !fits(ty, &settled) && self.join(ty, &settled).as_ref() != Some(&settled) {
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
    /// The names a part of a condition, `value is pattern`, binds: as any
    /// pattern does, except for a test of a tuple item's type, and `none`
    /// matched on a read past the tuple's end, which is always none.
    fn part_bindings(&mut self, ty: &Type, value: &mut Expr, pattern: &mut Pattern) -> Result<Vec<Declared>> {
        let at = pattern.span;
        if let PatternKind::Typed { ty: written, form, .. } = &mut pattern.kind {
            // The type as resolved, which `settle_type_tests` compares.
            let (resolved, bindings) = self.type_test_bindings(value, written, form, at)?;
            *written = resolved;
            return Ok(bindings);
        }
        if matches!(pattern.kind, PatternKind::None) && past_end(value) {
            value.ty = Some(Type::None);
            self.coerce(value, &Type::Maybe(Box::new(Type::Bool)))?;
            return Ok(Vec::new());
        }
        self.pattern_bindings(ty, pattern, false)
    }

    /// Checks a test of the type of a tuple's item, `t.#5 is maybe int {x}`,
    /// and returns the names it binds: the item for `{x}`, the read itself, a
    /// `maybe`, for `x`. The reads of the item after it, in its condition and
    /// its block, give the type it names (see `check_safe_item`).
    fn type_test_bindings(&mut self, value: &Expr, written: &Type, form: &TypedForm, at: Span) -> Result<(Type, Vec<Declared>)> {
        let Some((tuple, place)) = safe_read_of(value) else { return err(value.span, TYPE_TEST_ON_A_READ) };
        let Type::Maybe(wanted) = self.resolve_type(written, at, TypePlace::Body)? else {
            // `maybe int ~ text` is an `int ~ text` whose value is a maybe.
            let held = match written {
                Type::Or(value, error) => match &**value {
                    Type::Maybe(inner) => format!("({} ~ {})", inner, error),
                    _ => format!("({})", written),
                },
                _ => written.to_string(),
            };
            return err(at, format!("a tuple's item read with '.#' is a maybe, so its type is written as one, as in 'maybe {}'", held));
        };
        if !wanted.params().is_empty() {
            return err(at, format!("a type test names a type the program knows, such as 'maybe int'; '{}' is given by each use", wanted));
        }
        // The item's own type where it has the type tested, so a value of a
        // region keeps it. Where the item's type is not known yet (a group's,
        // or one holding a `T`), its regions are one the task can't name, so
        // nothing taken from it outlives the call (see `UNNAMED_REGION`).
        let held = match item_state(value) {
            Some(Some(found)) if same_ignoring_regions(&found, &wanted) => found,
            Some(_) => (*wanted).clone(),
            None => self.in_region(&wanted.without_regions(), UNNAMED_REGION),
        };
        self.narrowed.push((tuple, place, held.clone()));
        let bindings = match form {
            TypedForm::Only => Vec::new(),
            TypedForm::Named(names) if !names.parts.is_empty() => {
                return err(names.span, "the read is a maybe, so one name takes it; to take the item apart, write it in braces, as in 'is maybe (int, text) {(a, b)}'");
            }
            TypedForm::Named(names) => vec![(names.name.clone(), names.span, Type::Maybe(Box::new(held)), names.mutable)],
            TypedForm::Value(names) => self.bound_names(names, &held, false)?,
        };
        Ok(((Type::Maybe(wanted)), bindings))
    }

    /// Settles the tests of the types of tuple items in `cond`, guarding
    /// `body`, where each item's type is known: always, but in a task taking a
    /// group, where each copy knows it. A test that fails empties the block and
    /// makes the condition false there; one that holds becomes what it then is:
    /// `true`, a name for the read, or the item in a `maybe`. Nothing is left to
    /// test when the program runs.
    fn settle_type_tests(&mut self, cond: &mut Condition, body: &mut Block) {
        let mut part = Some(cond);
        while let Some(current) = part {
            if let Some(Pattern { kind: PatternKind::Typed { ty, form, .. }, .. }) = &current.pattern {
                let Type::Maybe(wanted) = ty else { return };
                let Some(state) = item_state(&current.value) else { return };
                let holds = match (&state, form) {
                    (None, TypedForm::Value(_)) => false,
                    (None, _) => true,
                    (Some(found), _) => same_ignoring_regions(found, wanted),
                };
                let span = current.value.span;
                if !holds {
                    let mut never = Expr::new(ExprKind::BoolLit(false), span);
                    never.ty = Some(Type::Bool);
                    current.value = never;
                    current.pattern = None;
                    current.and = None;
                    body.clear();
                    return;
                }
                match form.clone() {
                    TypedForm::Only => {
                        let mut always = Expr::new(ExprKind::BoolLit(true), span);
                        always.ty = Some(Type::Bool);
                        current.value = always;
                        current.pattern = None;
                    }
                    TypedForm::Named(names) => {
                        let held = state.clone().unwrap_or_else(|| (**wanted).clone());
                        let read = std::mem::replace(&mut current.value, Expr::new(ExprKind::NoneLit, span));
                        current.value = match read.kind {
                            ExprKind::Wrap(_) => read,
                            _ => Expr { kind: ExprKind::Wrap(Box::new(Expr { ty: Some(Type::None), ..read })), span, ty: None },
                        };
                        current.value.ty = Some(Type::Maybe(Box::new(held)));
                        if let Some(pattern) = &mut current.pattern {
                            pattern.kind = PatternKind::Tuple(names);
                        }
                    }
                    TypedForm::Value(names) => {
                        if let Some(pattern) = &mut current.pattern {
                            pattern.kind = PatternKind::Value(names);
                        }
                    }
                }
            }
            part = current.and.as_deref_mut();
        }
    }

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
                // A kind made for its types is written as the kind it was made from.
                let from = self.made_of.get(kind_name).map_or(kind_name.as_str(), |made| made.name.as_str());
                if let Some((written, at)) = kind.as_deref()
                    && written != from
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
            // A kind given a type this declaration takes: what its variant
            // carries, with the types given in place of the names it takes.
            (PatternKind::Variant { kind, tag, binds }, Type::Applied(applied)) if self.kinds.contains_key(&applied.name) => {
                if let Some((written, at)) = kind.as_deref()
                    && *written != applied.name
                {
                    return err(*at, format!("this pattern names kind '{}', but the value's kind is '{}'", written, subject));
                }
                let def = self.kinds.get(&applied.name).cloned().unwrap_or_else(|| KindDef { name: String::new(), types: Vec::new(), given: Vec::new(), variants: Vec::new(), span, private: false });
                let variant = def.variants.iter().find(|v| v.tag == *tag).ok_or_else(|| Error::at(span, format!("kind '{}' has no variant '{}'", applied.name, tag)))?;
                if binds.len() != variant.payload.len() {
                    let what = if in_case { format!("case '{}'", tag) } else { format!("'{}'", tag) };
                    return err(span, format!("{} expects {}, got {}", what, count(variant.payload.len(), "binding"), binds.len()));
                }
                let names: HashMap<&str, &Type> = def.types.iter().map(|param| param.name.as_str()).zip(&applied.types).collect();
                binds
                    .iter()
                    .zip(&variant.payload)
                    .map(|(names_bound, field)| {
                        let ty = field.ty.map_params(&mut |param| names.get(param).map(|ty| (*ty).clone()));
                        self.bound_names(names_bound, &self.field_in(&ty, &applied.region), false)
                    })
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
                } else if BUILTIN_TYPE_WORDS.contains(&tag.as_str()) {
                    err(span, format!("unknown variant '{0}'; a type is tested on a tuple's item read with '.#', which is a maybe, as in 'is maybe {0} {{x}}'", tag))
                } else {
                    err(span, format!("unknown variant '{}'", tag))
                }
            }
            (PatternKind::Value(names), Type::Maybe(inner)) => self.bound_names(names, inner, false),
            (PatternKind::Value(names), Type::Or(value, _)) => {
                if **value == Type::Nothing && names.name != DISCARD {
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
            (PatternKind::Typed { .. }, _) => err(span, TYPE_TEST_ON_A_READ),
            (PatternKind::None, Type::None) => err(span, "this value is always none, so there is nothing to match"),
            (PatternKind::None, other) => err(span, format!("'none' matches an empty maybe, but this value is {}", other)),
            (PatternKind::Fail(_), other) => {
                err(span, format!("'failure …' matches the error of a 'T ~ E', but this value is {}", other))
            }
        }
    }

    /// The names `names` binds from a value of type `ty`, with their types:
    /// one name takes the value, and `(a, b)` the items of a tuple. `mutable`
    /// is a `flex` written around them.
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
    /// Assignable places are: a `flex` variable, a field or item of an
    /// assignable place, and what a pointer points to or a slice's or map's
    /// item, reached from a name that changes (see `written_through`).
    fn check_assign_target(&mut self, expr: &mut Expr) -> Result<Type> {
        self.check_place_target(expr, true)
    }

    /// `check_assign_target`, where `sets_key` says a map's item written is
    /// set as `m[k] = v` sets it, which may add the key; `add` and `del` on a
    /// slice kept in a map change the value already there.
    fn check_place_target(&mut self, expr: &mut Expr, sets_key: bool) -> Result<Type> {
        // The place as written, for the error about a name it is reached from.
        let written = self.written_text(expr);
        let ty = self.check_written(expr, &written, sets_key)?;
        self.hand_written(expr, None);
        Ok(ty)
    }

    /// `check_assign_target` of `expr`, part of the place `written`, as written.
    fn check_written(&mut self, expr: &mut Expr, written: &str, sets_key: bool) -> Result<Type> {
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
                    return err(span, format!("cannot assign to immutable variable '{}'{}", name, self.param_hint(name, info)));
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
                let base_ty = self.check_field_base(base, written)?;
                let ty = self.field_type(&base_ty, field, span)?;
                self.read_only_field(&base_ty, field, span)?;
                match base_ty {
                    Type::Tuple(_) => {
                        return err(span, "a tuple's items never change; give the whole tuple a new value instead, as in 't = (1, 2)'");
                    }
                    Type::Array(..) => return err(span, "'len' can't be assigned: an array's length is part of its type"),
                    Type::Slice(..) => return err(span, "'len' can't be assigned: a slice's length changes with 'xs.add(…)' and 'xs.del(…)', or by slicing it, as in 'xs = xs[0..2]'"),
                    Type::Map(..) => return err(span, "'len' can't be assigned: it is how many keys the map holds, which 'm[k] = v' and 'm.del(k)' change"),
                    Type::Text(_) => return err(span, "'len' can't be assigned: it is how many bytes the text has, and text never changes; give the variable other text instead"),
                    _ => {}
                }
                expr.ty = Some(ty.clone());
                Ok(ty)
            }
            ExprKind::Deref(pointer) => {
                self.written_through(pointer, written, None)?;
                self.check_expr(expr)
            }
            ExprKind::Shared { shape, field } => {
                let (shape, field) = (shape.clone(), field.clone());
                let ty = self.check_expr(expr)?;
                self.changeable_shared(&shape, &field, span, "")?;
                Ok(ty)
            }
            ExprKind::Index(base, index) => {
                if matches!(index.kind, ExprKind::Range(..)) {
                    match self.check_expr(base)? {
                        Type::Text(_) => {
                            return err(span, "a part of text can't be assigned, since text never changes; give the variable other text instead, as in 't = t[0..2]'");
                        }
                        Type::Map(..) => return err(span, "a map has no parts: '[…]' takes one key, as in 'm[k] = v'"),
                        _ => {}
                    }
                    return err(span, "a slice of values can't be assigned; assign its items one at a time, as in 'xs[i] = v'");
                }
                let base_ty = self.check_item_base(base, written, sets_key)?;
                if matches!(base_ty, Type::Text(_)) {
                    return err(span, "a byte of text can't be assigned, since text never changes; build other text instead, as in 't = t[0..2] + \"x\" at r'");
                }
                let ty = self.check_index(base, &base_ty, index, span)?;
                // A key added is kept in the map, so it lives where the map does.
                if let Type::Map(key, _, _) = &base_ty {
                    self.coerce(index, key)?;
                }
                expr.ty = Some(ty.clone());
                Ok(ty)
            }
            ExprKind::Lookup(..) => err(span, "'[#…]' gives the item in a maybe, a value made here, not the place it is in; assign the item with 'xs[i] = v'"),
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

    /// Checks the array, slice or map whose item is assigned, and returns its
    /// type. An array holds its items, so it must be an assignable place
    /// itself; a slice's items are in a region, behind a pointer, and are
    /// written where the slice is reached from a name that changes, as what a
    /// pointer points to is. `written` is the place assigned, as written, and
    /// `sets_key` says the item is set as `m[k] = v` sets it, which may add the
    /// key; a write into a value already in a map changes what the map holds,
    /// as a write into a slice's item does.
    fn check_item_base(&mut self, base: &mut Expr, written: &str, sets_key: bool) -> Result<Type> {
        if let ExprKind::Var(name) = &base.kind {
            let info = self.lookup_or_err(name, base.span)?;
            if matches!(info.ty, Type::Array(..)) || (sets_key && matches!(info.ty, Type::Map(..))) {
                if info.result {
                    return err(base.span, format!("'{0}' is a result; set it with '{0} <- …'", name));
                }
                if !info.mutable && matches!(info.ty, Type::Map(..)) {
                    // A parameter's hint says how to write one that changes.
                    let hint = match self.param_hint(name, info) {
                        hint if hint.is_empty() => "; keep the map in a 'flex' variable to change it".to_string(),
                        hint => hint,
                    };
                    return err(base.span, format!("'m[k] = v' changes '{}', which is immutable{}", name, hint));
                }
                if !info.mutable {
                    return err(base.span, format!("cannot assign to an item of immutable variable '{}'{}", name, self.param_hint(name, info)));
                }
            }
            let ty = self.check_expr(base)?;
            if matches!(ty, Type::Slice(..) | Type::Map(..)) {
                self.written_through(base, written, None)?;
            }
            return Ok(ty);
        }
        let ty = self.check_expr(base)?;
        // `m[k] = v` may add the key, which changes the map as `add` changes a
        // slice, so the map is in a place that can change.
        if matches!(ty, Type::Array(..) | Type::Map(..)) {
            self.assignable_array(base, written)?;
        }
        if matches!(ty, Type::Slice(..)) {
            self.written_through(base, written, None)?;
        }
        Ok(ty)
    }

    /// Rejects an array, already checked, whose item can't be assigned: one in a
    /// `fixed` variable, a result, or a temporary value, or one reached from a
    /// name that never changes (see `written_through`). `written` is the place
    /// assigned, as written.
    fn assignable_array(&self, place: &Expr, written: &str) -> Result<()> {
        match &place.kind {
            ExprKind::Var(name) => {
                let info = self.lookup_or_err(name, place.span)?;
                if info.result {
                    return err(place.span, format!("'{0}' is a result; set it with '{0} <- …'", name));
                }
                if !info.mutable {
                    return err(place.span, format!("cannot assign to an item of immutable variable '{}'{}", name, self.param_hint(name, info)));
                }
                Ok(())
            }
            ExprKind::Deref(pointer) => self.written_through(pointer, written, None),
            ExprKind::Shared { shape, field } => self.changeable_shared(shape, field, place.span, ""),
            ExprKind::Field(base, field) => {
                if let Some(ty) = &base.ty {
                    self.read_only_field(ty, field, place.span)?;
                }
                self.assignable_array(base, written)
            }
            // A value in a map is in its table in a region, as a slice's item is.
            ExprKind::Index(base, _) if matches!(base.ty, Some(Type::Slice(..) | Type::Map(..))) => self.written_through(base, written, None),
            ExprKind::Index(base, _) => self.assignable_array(base, written),
            _ => err(place.span, "cannot assign to an item of a temporary value"),
        }
    }

    /// Refuses a write into `written` (the place as written) through `place`,
    /// what a pointer points to or a slice's or map's items are reached from,
    /// where its root is a name that never changes: nothing changes through a
    /// `fixed` variable, a parameter that isn't `flex`, a `fixed&`, a lambda's
    /// copy, or `.this` outside an `alters` method, at any depth. A result is
    /// the task's own value, being built, so what it holds may be written. A
    /// value made where it is written (a call's, `new`'s) is no name's. With
    /// `method`, the write is that `alters` method's, called on `written`.
    fn written_through(&self, place: &Expr, written: &str, method: Option<&str>) -> Result<()> {
        match &place.kind {
            ExprKind::Var(name) => {
                let info = self.lookup_or_err(name, place.span)?;
                if info.mutable || info.result {
                    return Ok(());
                }
                if name == THIS {
                    let what = match method {
                        Some(method) => format!("calling '{}' on '{}'", method, written),
                        None => format!("writing '{}'", written),
                    };
                    return Err(self.this_cannot_change(place.span, &what));
                }
                let change = match method {
                    Some(method) => format!("'{}' changes '{}', the value it is called on, and", method, written),
                    None => format!("cannot change '{}':", written),
                };
                err(place.span, format!("{} nothing changes through immutable variable '{}', at any depth{}", change, name, self.param_hint(name, info)))
            }
            ExprKind::Field(base, _) | ExprKind::Index(base, _) | ExprKind::Deref(base) => self.written_through(base, written, method),
            ExprKind::Shared { shape, field } => self.changeable_shared(shape, field, place.span, ""),
            _ => Ok(()),
        }
    }

    /// Where the value `e`, already checked, may come from, where it holds a
    /// slice, a map or a pointer: the names that never change it is reached
    /// from, and the calls that may give back what such a name holds (see
    /// `Own`). Empty for a value that holds none, which is copied whole, and
    /// for one made here or reached from a name that changes. What a fixed
    /// name owns is never given to what may change it (see `hand`).
    fn owners(&self, e: &Expr) -> Vec<Own> {
        let holds = e.ty.as_ref().is_some_and(|ty| self.holds_handle(ty));
        // A lambda's value keeps what it uses, which a call of it may give back.
        let lambda = matches!(e.ty, Some(Type::Closure(_)));
        if !holds && !lambda {
            return Vec::new();
        }
        let all = |values: &mut dyn Iterator<Item = &Expr>| values.flat_map(|value| self.owners(value)).collect::<Vec<Own>>();
        match &e.kind {
            ExprKind::Var(name) => match self.read_owners.get(&(e.span.file, e.span.line, e.span.col)) {
                Some(owners) => owners.clone(),
                None => self.lookup(name).map_or_else(Vec::new, |info| self.name_owners(name, info)),
            },
            // A `fixed&` argument is the place it names.
            ExprKind::Ref(place) => self.owners(place),
            ExprKind::Field(base, _) | ExprKind::Index(base, _) | ExprKind::Lookup(base, _) | ExprKind::Deref(base) => self.owners(base),
            ExprKind::Call { name, args, .. } => self.call_owners(name, args),
            // A task value or a lambda's: what it was given, and what it keeps.
            ExprKind::Invoke { callee, args } => all(&mut std::iter::once(callee.as_ref()).chain(args.iter())),
            ExprKind::Closure { lambda, .. } => vec![Own::Kept(lambda.clone())],
            ExprKind::Array(items) | ExprKind::Tuple(items) => all(&mut items.iter()),
            ExprKind::Construct { args, .. } => all(&mut args.iter()),
            ExprKind::StructInit { fields, .. } => all(&mut fields.iter().map(|field| &field.value)),
            ExprKind::Pairs(pairs) => all(&mut pairs.iter().flat_map(|(key, value)| [key, value])),
            ExprKind::Repeat { value, .. }
            | ExprKind::Wrap(value)
            | ExprKind::Kept { value, .. }
            | ExprKind::Shortcut { value, .. }
            | ExprKind::Spread(value)
            | ExprKind::Fail(value) => self.owners(value),
            // Keys and values are copied into a slice of their own, which
            // shares only what each of them holds.
            ExprKind::MapOp { map, op, .. } => match (op, &map.ty) {
                (MapOp::Keys, Some(Type::Map(key, _, _))) if self.holds_handle(key) => self.owners(map),
                (MapOp::Values, Some(Type::Map(_, value, _))) if self.holds_handle(value) => self.owners(map),
                _ => Vec::new(),
            },
            // A copy of a map or of a slice is its own, at every depth, but what
            // a lambda keeps stays shared; a slice of the values written, or of
            // one value repeated, holds them as they are.
            ExprKind::New { ty, value, .. } => match ty.as_ref() {
                map @ Type::Map(..) if !matches!(value.kind, ExprKind::Pairs(_)) && !has_closure(map) => Vec::new(),
                Type::Slice(item, _) if !matches!(value.kind, ExprKind::Array(_) | ExprKind::Repeat { .. }) && !has_closure(item) => Vec::new(),
                _ => self.owners(value),
            },
            ExprKind::Copy { .. } => Vec::new(),
            ExprKind::PlanB { value, body, .. } => {
                let mut given = Vec::new();
                last_values(body, &mut given);
                all(&mut std::iter::once(value.as_ref()).chain(given))
            }
            ExprKind::Choice(stmt) => {
                let mut given = Vec::new();
                last_values(std::slice::from_ref(stmt.as_ref()), &mut given);
                all(&mut given.into_iter())
            }
            ExprKind::LoopValue(value) => {
                let mut given = Vec::new();
                loop_values(&value.stmt, &mut given);
                all(&mut given.into_iter())
            }
            ExprKind::MethodCall { value, args, .. } => all(&mut std::iter::once(value.as_ref()).chain(args.iter())),
            _ => Vec::new(),
        }
    }

    /// What owns the value of the variable `name`, with `info`, where it never
    /// changes: what it was given, and the name itself where its value holds a
    /// slice, a map or a pointer. A lambda kept in a variable is followed to
    /// what it keeps, from what it was given; one that is a parameter may
    /// send back anything the caller's lambda keeps.
    fn name_owners(&self, name: &str, info: &VarInfo) -> Vec<Own> {
        if info.mutable || info.result {
            return Vec::new();
        }
        let mut owners = info.owners.clone();
        let ty = info.ty.clone();
        let index = self.param_index.get(name).filter(|_| info.param);
        if self.holds_handle(&ty) && (index.is_some() || !self.only_lambdas(&ty)) {
            owners.push(match index {
                Some(i) => Own::Param(*i, name.to_string()),
                None => Own::Root(name.to_string()),
            });
        }
        owners
    }

    /// Where the value a call of `task` given `args` gives back may come
    /// from: what the task may send back of each argument (see `Own::Sent`).
    /// A call whose arguments aren't one for each parameter, as one giving a
    /// group's values is, may give back any of them.
    fn call_owners(&self, task: &str, args: &[Expr]) -> Vec<Own> {
        let one_each = self.tasks.get(task).is_some_and(|info| info.params.len() == args.len()) && !args.iter().any(|arg| matches!(arg.kind, ExprKind::Spread(_)));
        args.iter()
            .enumerate()
            .flat_map(|(param, arg)| {
                let from = self.owners(arg);
                match (from.is_empty(), one_each) {
                    (true, _) => Vec::new(),
                    (false, true) => vec![Own::Sent { task: task.to_string(), param, from }],
                    (false, false) => from,
                }
            })
            .collect()
    }

    /// The hint for a variable that never changes, `name` with `info`, where a
    /// change is refused (see `param_hint_for`).
    fn param_hint(&self, name: &str, info: &VarInfo) -> String {
        param_hint_for(name, info, holds_pointers(&info.ty, &self.regional))
    }

    /// The value a message suggests copying: `value` as written, or its owner
    /// (`OWNER_MARK`) where it has no short text, as `[a, b]` hasn't, or is a
    /// plan B, which may give another value.
    fn copied_text(&self, value: &Expr) -> String {
        match self.value_text(value) {
            // A plan B's own value may be what its block gives.
            _ if matches!(value.kind, ExprKind::PlanB { .. }) => OWNER_MARK.to_string(),
            text if text == "…" => OWNER_MARK.to_string(),
            text => text,
        }
    }

    /// A value as a message names it: a place as written, a call by its task's
    /// name (a method by what it is called on, or its shape's name if shared),
    /// or a plan B by its value.
    fn value_text(&self, e: &Expr) -> String {
        if let Some(path) = place_path(e) {
            return path_text(&path);
        }
        match &e.kind {
            ExprKind::Call { name, args, .. } => {
                let shared = self.tasks.get(name).and_then(|info| info.owner.as_ref()).is_some_and(|owner| owner.shared);
                match (method_parts(name), args.first()) {
                    (Some(_), _) if shared => format!("{}(…)", called_name(name)),
                    (Some((_, method)), Some(value)) => format!("{}.{}(…)", self.value_text(value), method),
                    _ => format!("{}(…)", name),
                }
            }
            ExprKind::Invoke { callee, .. } => format!("{}(…)", self.value_text(callee)),
            ExprKind::MethodCall { name, .. } => format!("….{}(…)", name),
            ExprKind::PlanB { value, .. } | ExprKind::Wrap(value) | ExprKind::Kept { value, .. } => self.value_text(value),
            ExprKind::Lookup(base, index) => match index.kind {
                ExprKind::IntLit(n) => format!("{}[#{}]", self.value_text(base), n),
                _ => format!("{}[#…]", self.value_text(base)),
            },
            _ => "…".to_string(),
        }
    }

    /// A place as a message quotes it, as `path_text` writes it, where a part
    /// taken of a value on the way (`xs[0..2][0]`) is an item too.
    fn written_text(&self, e: &Expr) -> String {
        if let Some(path) = place_path(e) {
            return path_text(&path);
        }
        match &e.kind {
            ExprKind::Field(base, field) => format!("{}.{}", self.written_text(base), field),
            ExprKind::Index(base, index) => match index.kind {
                ExprKind::IntLit(n) => format!("{}[{}]", self.written_text(base), n),
                _ => format!("{}[…]", self.written_text(base)),
            },
            ExprKind::Deref(base) => format!("{}^", self.written_text(base)),
            _ => self.value_text(e),
        }
    }

    /// Notes that `value`, already checked, is given to `to`, which may change
    /// what it holds: an error, once every task is checked, if a fixed name
    /// owns it (see `settle_owners`).
    fn hand(&mut self, value: &Expr, to: HandedTo) {
        let ty = value.ty.clone().unwrap_or(Type::Unknown);
        self.hand_as(value, &ty, to);
    }

    /// `hand`, where what is given is of type `ty`: one of the several values
    /// `value` sends, or the whole of it.
    fn hand_as(&mut self, value: &Expr, ty: &Type, to: HandedTo) {
        // A lambda's value holds none: what it keeps counts where it is called.
        if !self.holds_handle(ty) {
            return;
        }
        let owners = self.owners(value);
        if !owners.is_empty() {
            // Several values sent by one call are no one value to copy.
            let copied = if matches!(value.ty, Some(Type::Results(_))) { OWNER_MARK.to_string() } else { self.copied_text(value) };
            let handed = self.handed(value.span, owners, to, &copied, ty);
            self.handed.push(handed);
        }
    }

    /// A value of type `ty` given at `span` to `to`, with what owns it; the
    /// message suggests a copy of `copied`, or of the owner (`OWNER_MARK`)
    /// for a lambda, whose value can't be copied.
    fn handed(&self, span: Span, owners: Vec<Own>, to: HandedTo, copied: &str, ty: &Type) -> Handed {
        let lambda = self.only_lambdas(ty);
        let copied = if lambda { OWNER_MARK } else { copied };
        let (region, none_here) = match ty {
            Type::Closure(closure) => self.copy_region_here(&closure.task.ret),
            _ => self.copy_region_here(ty),
        };
        let copy = format!("{}.copy() at {}", copied, region);
        let no_region = none_here.then_some(region);
        Handed { span, owners, to, copy, lambda, no_region, generic: self.generic_part(ty) }
    }

    /// The region a suggested copy of a value of type `ty` goes in: the one
    /// its type names, where the code here may allocate in it, or else the
    /// innermost region here; and whether there is none here, so the region
    /// the copy names is one the task would take.
    fn copy_region_here(&self, ty: &Type) -> (String, bool) {
        let usable = |name: &str| self.lookup(name).is_some_and(|info| info.ty == Type::Region && info.allocates);
        if let Some(region) = ty.regions().into_iter().find(|region| !unnamed_region(region) && *region != THIS_WORD && usable(region)) {
            return (region.to_string(), false);
        }
        let here = self.scopes.iter().rev().flat_map(|scope| scope.names.iter()).find(|(name, info)| info.ty == Type::Region && info.allocates && !unnamed_region(name) && *name != THIS_WORD);
        here.map_or_else(|| (copy_region(ty).to_string(), true), |(name, _)| (name.clone(), false))
    }

    /// Whether a value of type `ty` holds a slice, a map or a pointer, which
    /// another name may share, or is a lambda that may send one back; for
    /// several values, whether any of them does.
    fn holds_handle(&self, ty: &Type) -> bool {
        shares_handle(ty, &self.regional, true) || self.may_hold(ty, true)
    }

    /// Whether a value of type `ty` holds a slice, a map or a pointer only
    /// through lambdas that may send one back.
    fn only_lambdas(&self, ty: &Type) -> bool {
        self.holds_handle(ty) && !shares_handle(ty, &self.regional, false) && !self.may_hold(ty, false)
    }

    /// Whether `ty` holds a type the generic task being checked takes, which
    /// may be given one that holds a slice, a map or a pointer: any but one
    /// that must be `frozen`. With `lambdas`, also one a lambda sends back.
    fn may_hold(&self, ty: &Type, lambdas: bool) -> bool {
        match ty {
            Type::Param(name) | Type::Named(name, _) => {
                self.type_params.iter().any(|param| param.name == *name && !param.needs.iter().any(|(need, _)| *need == Need::Frozen))
            }
            Type::Applied(applied) => applied.types.iter().any(|ty| self.may_hold(ty, lambdas)),
            Type::Maybe(inner) | Type::Array(inner, _) | Type::Fail(inner) => self.may_hold(inner, lambdas),
            Type::Or(value, error) => self.may_hold(value, lambdas) || self.may_hold(error, lambdas),
            Type::Tuple(items) | Type::Results(items) => items.iter().any(|item| self.may_hold(item, lambdas)),
            Type::Closure(closure) => lambdas && self.may_hold(&closure.task.ret, lambdas),
            _ => false,
        }
    }

    /// Where a value of type `ty` holds a slice, a map or a pointer only for
    /// some types the generic task being checked may be given: the task, and
    /// the type its copies fill in.
    fn generic_part(&self, ty: &Type) -> Option<(String, Type)> {
        (!shares_handle(ty, &self.regional, true) && self.may_hold(ty, true)).then(|| (self.current_task.clone(), ty.clone()))
    }

    /// The names a pattern binds, `bindings`, from `source`, already checked:
    /// a `flex` one may not be given what a fixed name owns (see `hand`), and
    /// one that never changes owns what the value is owned by.
    fn bind_from(&mut self, bindings: &[Declared], source: &Expr) {
        let owners = self.owners(source);
        if owners.is_empty() {
            return;
        }
        for (name, at, ty, mutable) in bindings {
            if name == DISCARD || !self.holds_handle(ty) {
                continue;
            }
            if *mutable {
                let handed = self.handed(*at, owners.clone(), HandedTo::Variable(name.clone()), &self.copied_text(source), ty);
                self.handed.push(handed);
            } else {
                self.bound_owners.push((*at, owners.clone()));
            }
        }
    }

    /// Notes what this task's `<-` sends may come from, which a call of it
    /// gives back (see `Own::Sent`); for a lambda, what a call of its value
    /// gives back (see `Own::Kept`).
    fn note_sent(&mut self, values: &[Expr]) {
        let sent: Vec<Own> = values.iter().flat_map(|value| self.owners(value)).collect();
        if !sent.is_empty() {
            self.sends.entry(self.current_task.clone()).or_default().extend(sent);
        }
    }

    /// Notes a write, or a change by an `alters` method (`method`), of
    /// `place`, already checked, where it is reached from a value made where
    /// it is written rather than from a name, as in `first(xs)^.x = 1`: an
    /// error if a fixed name owns that value (see `settle_owners`).
    fn hand_written(&mut self, place: &Expr, method: Option<&str>) {
        let mut root = place;
        while let ExprKind::Field(base, _) | ExprKind::Index(base, _) | ExprKind::Lookup(base, _) | ExprKind::Deref(base) = &root.kind {
            root = base;
        }
        if matches!(root.kind, ExprKind::Var(_) | ExprKind::Shared { .. }) || std::ptr::eq(root, place) {
            return;
        }
        let made = self.value_text(root);
        let lead = match method {
            Some(method) => format!("'{}' changes what '{}' gives back", method, made),
            None => format!("cannot change what '{}' gives back", made),
        };
        let owners = self.owners(root);
        if !owners.is_empty() {
            let generic = root.ty.as_ref().and_then(|ty| self.generic_part(ty));
            self.handed.push(Handed { span: root.span, owners, to: HandedTo::Written(lead), copy: String::new(), lambda: false, no_region: None, generic });
        }
    }

    /// Settles every value given to what may change it (see `hand`), once
    /// every task is checked: which parameters each task may send back is
    /// worked out from its sends, through the calls in them, until nothing
    /// more is found, and a value a fixed name owns, directly or through what
    /// a call gives back, is an error where it is given. A `[flex @r]` lambda
    /// may not keep a copy of what a fixed name owns either.
    fn settle_owners(&mut self) {
        // Which parameters each task may send back, by their place.
        let mut sends_back: HashMap<String, HashSet<usize>> = HashMap::new();
        loop {
            let mut grown = false;
            for (task, sent) in &self.sends {
                let params = self.tasks.get(task).map_or(0, |info| info.params.len());
                for param in 0..params {
                    if sends_back.get(task).is_some_and(|back| back.contains(&param)) {
                        continue;
                    }
                    if self.reaches_param(sent, param, &sends_back) {
                        sends_back.entry(task.clone()).or_default().insert(param);
                        grown = true;
                    }
                }
            }
            if !grown {
                break;
            }
        }
        let mut errors = Vec::new();
        for handed in &self.handed {
            let Some((owner, of_param)) = self.owner_of_handed(&handed.owners, &sends_back) else { continue };
            // In a generic task, only a copy given a type that holds one.
            let given = match &handed.generic {
                None => String::new(),
                Some((task, ty)) => {
                    let copy = self.copies.get(task).and_then(|copies| copies.iter().find(|(_, given)| shares_handle(&in_copy(ty, given), &self.regional, true)));
                    let Some((types, _)) = copy else { continue };
                    let types: Vec<String> = types.iter().map(|ty| ty.without_regions().to_string()).collect();
                    format!(", where '{}' is given {}", unqualified(task), types.join(", "))
                }
            };
            let owner_text = owner.clone();
            let copy = handed.copy.replace(OWNER_MARK, &owner_text);
            // Where no region is here, the copy goes in one the task would take.
            let region = handed.no_region.as_ref().map_or_else(String::new, |region| format!(", in a region the task takes, as in '{}: region'", region));
            let rule = "nothing changes through what a fixed name owns";
            // A lambda's value can't be copied, but what it sends back can.
            let lambda = format!("a lambda that may send back what fixed '{}' owns", owner_text);
            let send_copy = format!("have the lambda send back a copy, as in '<- {}'{}", copy, region);
            let message = match &handed.to {
                HandedTo::Variable(name) if handed.lambda => format!("'{}' is given {}, and {}; make '{}' fixed, or {}", name, lambda, rule, name, send_copy),
                HandedTo::Param { task, param } if handed.lambda => {
                    format!("'{}' may change what its parameter '{}' sends back, so it can't be given {}; take it as '{}' without 'flex', or {}", task, param, lambda, param, send_copy)
                }
                HandedTo::Place(place) if handed.lambda => format!("'{}' changes, so it can't be given {}: {}; {}", place, lambda, rule, send_copy),
                HandedTo::Result(name) if handed.lambda => format!("'{}' is a result, which the task may change, so it can't be given {}; {}", name, lambda, send_copy),
                HandedTo::Variable(name) => format!("'{}' is given what fixed '{}' owns, and {}; give it a copy: '{}'{}", name, owner_text, rule, copy, region),
                HandedTo::Param { task, param } => {
                    let keep = match of_param {
                        // `.this` changes in an `alters` method.
                        true if owner == THIS => "make the method 'alters'".to_string(),
                        true => format!("take '{0}' as 'flex {0}'", owner_text),
                        false => format!("keep '{}' in a flex variable", owner_text),
                    };
                    format!("'{}' changes what its parameter '{}' reaches, so it can't be given what fixed '{}' owns; give it a copy, '{}'{}, or {}", task, param, owner_text, copy, region, keep)
                }
                HandedTo::Place(place) => format!("'{}' changes, so it can't be given what fixed '{}' owns: {}; give it a copy: '{}'{}", place, owner_text, rule, copy, region),
                HandedTo::Result(name) => format!("'{}' is a result, which the task may change, so it can't be given what fixed '{}' owns; give it a copy: '{}'{}", name, owner_text, copy, region),
                HandedTo::Written(lead) => format!("{}: it is what fixed '{}' owns, and {}, at any depth", lead, owner_text, rule),
            };
            // The types a generic task is given that make it so, after its owner.
            let message = message.replacen(" owns", &format!(" owns{}", given), 1);
            errors.push(Error::at(handed.span, message));
        }
        // A `[flex @r]` lambda's copies are its own to change.
        for kept in self.kept_lambdas.values().filter(|kept| kept.flex) {
            for name in &kept.uses {
                let Some((_, info)) = kept.outer.iter().find(|(outer, _)| outer == name) else { continue };
                if info.mutable || info.result || !shares_handle(&info.ty, &self.regional, true) {
                    continue;
                }
                let rule = "nothing changes through what a fixed name owns";
                // A lambda kept may send back what it keeps, and can't be copied.
                let message = if self.only_lambdas(&info.ty) {
                    format!(
                        "a '[flex @{1}]' lambda may change what its copies hold, so it can't keep a copy of fixed '{0}', a lambda that may send back what a fixed name owns, and {2}; make it a '[@{1}]' lambda",
                        name, kept.region, rule
                    )
                } else {
                    let copy = format!("{}.copy() at {}", name, copy_region(&info.ty));
                    format!(
                        "a '[flex @{1}]' lambda may change what its copies hold, so it can't keep a copy of fixed '{0}', and {3}; make it a '[@{1}]' lambda, or keep a copy of its own, as in 'flex mine = {2}', and use that",
                        name, kept.region, copy, rule
                    )
                };
                errors.push(Error::at(kept.span, message));
            }
        }
        // One place may be checked more than once, in a generic task's copies.
        let mut seen = HashSet::new();
        errors.retain(|error| seen.insert((error.span.map(|span| (span.file, span.line, span.col)), error.message.clone())));
        self.errors.extend(errors);
    }

    /// Whether `owners` reach parameter `param` of the task they are in,
    /// through what the calls in them send back.
    fn reaches_param(&self, owners: &[Own], param: usize, sends_back: &HashMap<String, HashSet<usize>>) -> bool {
        owners.iter().any(|own| match own {
            Own::Param(i, _) => *i == param,
            Own::Sent { task, param: given, from } => self.may_send_back(task, *given, sends_back) && self.reaches_param(from, param, sends_back),
            Own::Kept(lambda) => self.reaches_param(&self.lambda_sends(lambda), param, sends_back),
            Own::Root(_) => false,
        })
    }

    /// Whether `task` may send back what its parameter `param` is given.
    fn may_send_back(&self, task: &str, param: usize, sends_back: &HashMap<String, HashSet<usize>>) -> bool {
        self.natives.contains(task) || sends_back.get(task).is_some_and(|back| back.contains(&param))
    }

    /// The fixed name that owns a value with `owners`, if one does, and
    /// whether it is a parameter.
    fn owner_of_handed(&self, owners: &[Own], sends_back: &HashMap<String, HashSet<usize>>) -> Option<(String, bool)> {
        owners.iter().find_map(|own| match own {
            Own::Root(name) => Some((name.clone(), false)),
            Own::Param(_, name) => Some((name.clone(), true)),
            Own::Sent { task, param, from } => self.may_send_back(task, *param, sends_back).then(|| self.owner_of_handed(from, sends_back)).flatten(),
            Own::Kept(lambda) => self.owner_of_handed(&self.lambda_sends(lambda), sends_back),
        })
    }

    /// What a call of `lambda`'s value may give back, as the code where it is
    /// written owns it: its sends, where each copy it keeps is what owns that
    /// variable there. What the lambda makes, and its parameters, which its
    /// caller gives, are no one's there.
    fn lambda_sends(&self, lambda: &str) -> Vec<Own> {
        let Some(kept) = self.kept_lambdas.get(lambda) else { return Vec::new() };
        let sent = self.sends.get(lambda).map_or(&[][..], Vec::as_slice);
        self.around_lambda(kept, lambda, sent)
    }

    /// `owners`, noted in `lambda` (kept as `kept`), as the code where it is
    /// written owns them: a copy by what owns its variable there, a lambda
    /// written inside this one by what it sends, and what the copies were
    /// given there as it is.
    fn around_lambda(&self, kept: &KeptLambda, lambda: &str, owners: &[Own]) -> Vec<Own> {
        owners
            .iter()
            .flat_map(|own| match own {
                Own::Root(name) => kept.owned.get(name).cloned().unwrap_or_default(),
                Own::Param(..) => Vec::new(),
                Own::Sent { task, param, from } => {
                    let from = self.around_lambda(kept, lambda, from);
                    if from.is_empty() { Vec::new() } else { vec![Own::Sent { task: task.clone(), param: *param, from }] }
                }
                Own::Kept(inner) if self.kept_lambdas.get(inner).is_some_and(|inner| inner.within.as_deref() == Some(lambda)) => {
                    self.around_lambda(kept, lambda, &self.lambda_sends(inner))
                }
                Own::Kept(_) => vec![own.clone()],
            })
            .collect()
    }

    /// Checks the base of an assigned field, which must itself be an
    /// assignable place. `written` is the place assigned, as written.
    fn check_field_base(&mut self, base: &mut Expr, written: &str) -> Result<Type> {
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
                    return err(span, format!("cannot assign to a field of immutable variable '{}'{}", name, self.param_hint(name, info)));
                }
                self.check_expr(base)
            }
            ExprKind::Field(..) | ExprKind::Deref(_) | ExprKind::Index(..) | ExprKind::Shared { .. } => self.check_written(base, written, false),
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
            // A kind given a type this declaration takes.
            Type::Applied(applied) if self.kinds.contains_key(&applied.name) => {
                self.kinds.get(&applied.name).map(|kind| kind.variants.iter().map(|v| v.tag.clone()).collect()).unwrap_or_default()
            }
            Type::Maybe(_) => vec!["{…}".to_string(), "none".to_string()],
            Type::Or(..) => vec!["{…}".to_string(), "failure …".to_string()],
            // A tuple has one case, so it is taken apart rather than matched.
            Type::Tuple(_) => {
                return err(expr.span, format!("match requires a kind, a maybe, or a 'T ~ E' value, got {}; take a tuple apart with 'fixed', as in 'fixed (a, b) = t'", ety));
            }
            other => return err(expr.span, format!("match requires a kind, a maybe, or a 'T ~ E' value, got {}", other)),
        };
        let mut seen: HashSet<String> = HashSet::new();
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
            self.bind_from(&bindings, expr);
            let label = case.pattern.label();
            if matches!(case.pattern.kind, PatternKind::Any) {
                if !unknown && needed.iter().all(|n| seen.contains(n)) {
                    return err(case.pattern.span, "every case is already covered, so '_' never matches");
                }
                matched_all = true;
            } else if seen.contains(&label) {
                return err(case.pattern.span, format!("duplicate case '{}'", label));
            }
            seen.insert(label);
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
                        return err(value.span, "a case gives a value with '=' only where the match is one itself, as in 'fixed name = match …'; the cases of a match on its own hold statements, written on the lines below it");
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
            && let Some(missing) = needed.iter().find(|n| !seen.contains(*n))
        {
            return err(span, format!("missing case '{}' in match", missing));
        }
        let Some(words) = gives else { return Ok(None) };
        let Some(settled) = self.settle_branches(span, &given, words)? else { return Ok(None) };
        let settled = self.made_kinds(settled, span)?;
        for (i, _, _) in given.iter().filter(|(_, ty, _)| !ty.unknown() && *ty != settled) {
            if let Some(case) = cases.get_mut(*i) {
                self.settle_value(&mut case.body, &settled)?;
            }
        }
        self.escaping_region(span, &given, &settled, words)?;
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
            format!("{} sends {} values; receive them with 'fixed {} = …'", what, types.len(), example_names(types.len())),
        )
    }

    /// `check_expr`, which also allows a call that sends several values.
    fn check_expr_multi(&mut self, expr: &mut Expr) -> Result<Type> {
        self.resolve_constructor(expr);
        self.resolve_empty_plan_b(expr);
        self.resolve_task_use(expr);
        if matches!(&expr.kind, ExprKind::Field(_, field) if field == TYPE_WORD) {
            return self.check_type_of(expr);
        }
        let ty = self.infer_expr(expr)?;
        if matches!(expr.kind, ExprKind::Array(_) | ExprKind::Repeat { .. } | ExprKind::New { .. }) {
            self.warn_char_array(&ty, expr.span);
        }
        if let ExprKind::Var(name) = &expr.kind
            && let Some(info) = self.lookup(name)
        {
            let owners = self.name_owners(name, info);
            if !owners.is_empty() {
                self.read_owners.insert((expr.span.file, expr.span.line, expr.span.col), owners);
            }
        }
        expr.ty = Some(ty.clone());
        Ok(ty)
    }

    /// Warns, once per file, about an array or slice of chars made at `span`:
    /// text built by hand, which `text` does already.
    fn warn_char_array(&mut self, ty: &Type, span: Span) {
        let chars = match ty {
            Type::Array(items, _) | Type::Slice(items, _) | Type::Ptr(items, _) => match items.as_ref() {
                Type::Array(inner, _) => **inner == Type::Char,
                other => *other == Type::Char,
            },
            _ => false,
        };
        if chars && self.char_arrays_warned.insert(span.file) {
            self.warnings.push(Error::at(
                span,
                "this builds text by hand as chars; 'text' holds characters already, with its length, parts, 't[i]', joining, and '==', so prefer it, as in 'fixed word = \"abc\"' (said once in each file)",
            ));
        }
    }

    /// Checks `value.type`, the type of the value as code writes it, and makes
    /// it the text it is: the compiler knows it, so the value is never worked
    /// out, as C's `sizeof` doesn't. Regions are left out (`[float]`, `^Point`),
    /// a shape or kind is its own name wherever it is read, and a shape made for
    /// types is written as given them (`Box<int>`).
    fn check_type_of(&mut self, expr: &mut Expr) -> Result<Type> {
        let ExprKind::Field(base, _) = &mut expr.kind else { return Err(Error::internal_at(expr.span, "a '.type' that isn't one")) };
        // An address is no value of its own, so it has no type to read.
        if let ExprKind::Ref(place) = &base.kind {
            let example = place_path(place).map_or_else(|| "x".to_string(), |path| path_text(&path));
            return err(base.span, format!("an address, '&{0}', is only shown, turned into text with 'str', or compared with '=='; the type of the place is '{0}.type'", example));
        }
        let ty = self.check_expr(base)?;
        if ty.unknown() {
            return Ok(Type::Unknown);
        }
        if matches!(ty, Type::Nothing) {
            return err(base.span, "this has no value, so no type to read");
        }
        expr.kind = match ty.params().is_empty() {
            true => ExprKind::TextLit(self.written_type(&ty)),
            false => ExprKind::TypeOf(Box::new(ty)),
        };
        expr.ty = Some(Type::Text(None));
        Ok(Type::Text(None))
    }

    /// A type as code writes it, for `x.type`: no regions, each shape or kind by
    /// its own name, and a shape made for types as given them.
    fn written_type(&self, ty: &Type) -> String {
        self.unmade(&ty.without_regions()).map_names(&|name| unqualified(name).to_string()).to_string()
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
                // A call of a task whose mistake is told already says nothing
                // of how many values it sends (see `Type::Unknown`).
                Type::Unknown => given = vec![Type::Unknown; wanted],
                // `fixed a, b = pair`: the items of one tuple.
                Type::Tuple(items) if unpacks && group_in_tuple(&items).is_some() => return err(only.span, GROUP_TUPLE_UNPACKED),
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
            Type::Tuple(items) if group_in_tuple(items).is_some() => err(at, GROUP_TUPLE_UNPACKED),
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
            // What a result holds may be written, so it may not take what a
            // fixed name owns.
            if let (Some((name, _)), Some(value)) = (names.get(i), if forwarded { values.first() } else { values.get(i) }) {
                self.hand_as(value, want, HandedTo::Result(name.clone()));
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

    /// How `ty`, a kind whose variant didn't tell all of its types, is written
    /// with them, as the variant's message suggests (see `untold_kinds`):
    /// `Option<Option<int>>` for `Option.Some(Option.None)`.
    fn untold_example(&self, ty: &Type) -> Option<String> {
        let Type::Applied(applied) = ty else { return None };
        let (_, kind, _, _, example) = self.untold_kinds.iter().rev().find(|(_, kind, ..)| *kind == applied.name)?;
        Some(format!("{}<{}>", unqualified(kind), example))
    }

    /// The type a `fixed` or `flex` without an annotation gives `name`, from the
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
            Type::Nothing => err(at, "this expression has no value (its type is 'nothing')"),
            other if holds_untold(other) => err(
                at,
                format!(
                    "cannot infer the type of '{0}', since its value doesn't say every type its kind takes; give it a type, such as '{0}: {1}', or write them, as in '{2}'",
                    name,
                    self.untold_example(other).unwrap_or_else(|| settled_example(other).to_string()),
                    self.untold_example(other).map_or_else(|| untold_written(other), |example| format!("{}.…", example))
                ),
            ),
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
        // A macro named as a value is the literal it stands for, which the C
        // names by the macro (see `ExprKind::Macro`). A broken one stays a name.
        if let ExprKind::Var(name) = &expr.kind
            && self.lookup(name).is_none()
            && self.sees(name)
            && let Some(value) = self.macros.get(name.as_str()).and_then(|info| info.value.clone())
        {
            expr.kind = ExprKind::Macro { name: name.clone(), value: Box::new(value) };
            return;
        }
        let variant = |name: &str| {
            self.variants.get(name).filter(|_| self.sees(name)).map(|info| (info.kind.clone(), info.variant.tag.clone()))
        };
        let resolved = match &mut expr.kind {
            ExprKind::Var(name) if self.lookup(name).is_none() => variant(name).map(|(kind, tag)| (kind, tag, Vec::new())),
            ExprKind::Call { name, args, .. } => variant(name).map(|(kind, tag)| (kind, tag, std::mem::take(args))),
            _ => None,
        };
        if let Some((kind, tag, args)) = resolved {
            expr.kind = ExprKind::Construct { kind, tag, args, types: Vec::new() };
        }
    }

    /// Rewrites a name that names a task and no variable into the task's value,
    /// and `f(x)`, where `f` is a variable of a task's type, into the call of
    /// the task it holds. A variable named like a task that holds no task leaves
    /// `f(x)` the task's call, as it always was; one named like no task makes it
    /// a call of the variable, which says what the variable holds.
    fn resolve_task_use(&self, expr: &mut Expr) {
        match &mut expr.kind {
            ExprKind::Var(name) if self.lookup(name).is_none() => {
                let full = self.full_name(name);
                // A shared method is named so only by its decorators (see
                // `Parser::decorate`); a program names it `X.m`.
                // The layers of a wrapped method of every value are named so too,
                // by the layer before each (see `add_wrappers`).
                if self.tasks.get(&full).is_some_and(|info| info.owner.as_ref().is_none_or(|owner| owner.shared) || full.contains(LAYER)) {
                    expr.kind = ExprKind::TaskValue(full);
                }
            }
            // `X.f(…)`: a shared field that holds a task, called, where the
            // shape has no method of that name.
            ExprKind::Call { name, types, args }
                if types.is_empty()
                    && !self.tasks.contains_key(name.as_str())
                    && method_parts(name).is_some_and(|(owner, field)| {
                        self.shapes.get(owner).is_some_and(|shape| shape.shared.iter().any(|member| member.field.name == field && (is_task(&member.field.ty) || matches!(&member.field.ty, Type::Maybe(inner) if is_task(inner)))))
                    }) =>
            {
                let Some((owner, field)) = method_parts(name) else { return };
                let callee = Expr::new(ExprKind::Shared { shape: owner.to_string(), field: field.to_string() }, expr.span);
                let args = std::mem::take(args);
                expr.kind = ExprKind::Invoke { callee: Box::new(callee), args };
            }
            ExprKind::Call { name, types, args } if types.is_empty() => {
                let Some(info) = self.lookup(name) else { return };
                let holds_task = is_task(&info.ty) || matches!(&info.ty, Type::Maybe(inner) if is_task(inner));
                let calls_other = self.tasks.contains_key(&self.full_name(name)) || BUILTIN_TASKS.contains(&name.as_str()) || number_cast(name).is_some();
                if info.ty == Type::Region || (!holds_task && calls_other) {
                    return;
                }
                let callee = Expr::new(ExprKind::Var(std::mem::take(name)), expr.span);
                let args = std::mem::take(args);
                expr.kind = ExprKind::Invoke { callee: Box::new(callee), args };
            }
            _ => {}
        }
    }

    /// Checks the startup decorators in `body` (see `add_startups`), each a
    /// call `d(task, args)`: that `d` takes a task of the task's type first,
    /// and that it sends nothing, since what it sent would go nowhere.
    fn check_startup(&mut self, body: &mut Block, passes: &[bool]) {
        for stmt in body.iter_mut() {
            let fits = self.decorator_fits(stmt);
            if self.keep_going(fits) {
                // The call is checked as a value, since a failure it sends is
                // handled where the program starts (see `startup_failure`).
                let StmtKind::Expr(call) = &mut stmt.kind else { continue };
                // No region exists where the program starts. One sending several
                // values is told what a decorator sends, below.
                let recorded = self.errors.len();
                self.at_start = Some(DECORATOR_VALUES);
                let checked = self.check_expr_multi(call).map(|_| ()).map_err(|error| at_start_error(error, DECORATOR_VALUES));
                self.at_start = None;
                self.said_at_start(recorded, DECORATOR_VALUES);
                self.keep_going(checked);
            }
        }
        for (stmt, passes) in body.iter().zip(passes.iter().chain(std::iter::repeat(&false))) {
            let StmtKind::Expr(call) = &stmt.kind else { continue };
            let ExprKind::Call { name, .. } = &call.kind else { continue };
            match call.ty.as_ref() {
                // A decorator that can fail: its error is written out where the
                // program starts, so its type must have a text form.
                Some(Type::Or(value, error)) if **value == Type::Nothing => self.note_written_strs(&error.clone(), call.span),
                Some(Type::Nothing) if *passes => self.warnings.push(Error::at(
                    call.span,
                    format!("'?' lets a failure of '{}' pass, but it sends nothing, so it never fails; leave the '?' out", unqualified(name)),
                )),
                Some(Type::Nothing | Type::Unknown) | None => {}
                Some(ty) => {
                    let message = format!(
                        "a startup decorator sends nothing, or 'nothing ~ E' when it can fail, since it runs where the program starts and nothing takes what it sends; '{}' sends {}",
                        unqualified(name),
                        ty
                    );
                    self.record(Error::at(call.span, message));
                }
            }
        }
    }

    /// For a call of a task whose last parameter takes a group of types,
    /// `args: P...`, the task as this call sees it: the group made into one type
    /// for each value given after the others (`P#0`, `P#1`, …), worked out from
    /// them as any type is, then kept together again (see `packed_groups`).
    /// `None` for any other task.
    fn group_call_info(&self, info: &TaskInfo, args: &[Expr], written: &[TypeArg], span: Span, callee: &str) -> Result<Option<TaskInfo>> {
        let Some(at) = info.params.iter().position(|(_, ty)| matches!(ty, Type::Group(_))) else { return Ok(None) };
        let Some((value, Type::Group(group))) = info.params.get(at) else { return Ok(None) };
        if !written.is_empty() {
            return err(span, format!("{} takes a group of types, '{}...', which a call doesn't write yet; leave the types out, and the values say them", callee, group));
        }
        let fixed = info.params.len() - 1;
        if args.len() < fixed {
            let after = fixed - at;
            let expects = match (at, after) {
                (_, 0) => format!("{}, then any number of values for '{}'", count(at, "argument"), value),
                (0, _) => format!("any number of values for '{}', then {}", value, count(after, "argument")),
                _ => format!("{}, any number of values for '{}', then {}", count(at, "argument"), value, count(after, "argument")),
            };
            // `f(args...)`, the values of the group of the task it is in.
            let got = match args.iter().filter(|arg| !matches!(arg.kind, ExprKind::Spread(_))).count() {
                alone if alone < args.len() => format!("{} and a group's values", alone),
                all => all.to_string(),
            };
            return err(span, format!("{} expects {}, got {}", callee, expects, got));
        }
        let names: Vec<String> = (0..args.len() - fixed).map(|i| format!("{}{}{}", group, GROUP_ITEM, i)).collect();
        let items = |ty: &Type| ty.map_groups(&mut |name| (name == group).then(|| names.iter().map(|name| Type::Param(name.clone())).collect()));
        let mut types = Vec::new();
        for param in &info.types {
            // A group given no values stays, given no types (see `check_call_args`).
            if param.name == *group && names.is_empty() {
                types.push(param.clone());
            } else if param.name == *group {
                types.extend(names.iter().map(|name| TypeParam { name: name.clone(), span: param.span, needs: param.needs.clone(), group: false }));
            } else {
                types.push(param.clone());
            }
        }
        let mut params: Vec<(String, Type)> = info.params.iter().take(at).map(|(name, ty)| (name.clone(), items(ty))).collect();
        params.extend(names.iter().enumerate().map(|(i, name)| (format!("{} {}", value, i + 1), Type::Param(name.clone()))));
        params.extend(info.params.iter().skip(at + 1).map(|(name, ty)| (name.clone(), items(ty))));
        let mut passing: Vec<Passing> = info.passing.iter().take(at).copied().collect();
        passing.extend(names.iter().map(|_| Passing::Copy));
        passing.extend(info.passing.iter().skip(at + 1).copied());
        Ok(Some(TaskInfo { types, params, passing, ret: items(&info.ret), owner: info.owner.clone() }))
    }

    /// Checks that a call of the task `name`, as `info` is for the call, can be
    /// given the group's values `args[spread]`, `args...`: only as values for the
    /// group the task takes. Values written beside them join that group,
    /// but not in a call of the task by itself, whose group would be one value
    /// longer at each call, and so would never be filled in.
    fn check_spread_given(&self, name: &str, callee: &str, info: &TaskInfo, args: &[Expr], spread: usize) -> Result<()> {
        let Some(at) = args.get(spread).map(|arg| arg.span) else { return Ok(()) };
        let Some(group) = info.types.iter().find(|param| param.group || param.name.contains(GROUP_ITEM)) else {
            return err(
                at,
                format!(
                    "{} takes a set number of values, and how many a group holds is known only where it is filled in; give a group's values to a task taking a group, as in 'next(args...)', or name them one by one",
                    callee
                ),
            );
        };
        let group = group.name.split(GROUP_ITEM).next().unwrap_or(&group.name);
        let fixed = info.params.iter().take_while(|(_, ty)| !matches!(ty, Type::Param(param) if param.starts_with(&format!("{}{}", group, GROUP_ITEM)))).count();
        let item = format!("{}{}", group, GROUP_ITEM);
        let after = info.params.iter().rev().take_while(|(_, ty)| !matches!(ty, Type::Param(param) if param.starts_with(&item))).count();
        if let Some((param, _)) = info.params.get(spread).filter(|_| spread < fixed || spread >= info.params.len() - after) {
            return err(at, format!("a group's values go to the group {} takes, between the values it names one by one, and they would be given as '{}' here", callee, param));
        }
        // The values written beside them in the group, before and after.
        let grown = (spread - fixed) + (info.params.len() - after - spread - 1);
        if grown > 0 && self.full_name(name) == self.current_task {
            return err(
                at,
                format!(
                    "{} gives its own group {} more than it was given, so its group would grow at each call and never be filled in; give it 'args...' alone",
                    callee,
                    count(grown, "value")
                ),
            );
        }
        Ok(())
    }

    /// Checks `t.#k`, the tuple `t`'s item `k` in a maybe, which is `none` past
    /// the tuple's end: an item there becomes the item read as `t.k` and made
    /// a maybe (see `ExprKind::Wrap`), and one past it stays, read as `none`,
    /// which takes its maybe type from where it goes.
    fn check_safe_item(&mut self, expr: &mut Expr) -> Result<Type> {
        let span = expr.span;
        let ExprKind::Field(base, item) = &mut expr.kind else { return Err(Error::internal_at(span, "a tuple's item read as no field")) };
        let base_ty = self.check_expr(base)?;
        let Ok(place) = item.trim_start_matches(SAFE_ITEM).parse::<usize>() else {
            return Err(Error::internal_at(span, "a tuple's item read in a maybe without its place"));
        };
        // After a test of its type, `t.#5? is maybe int`, the item read is of
        // the type tested, or none (see `type_test_bindings`).
        let narrowed = match &base.kind {
            ExprKind::Var(tuple) => self.narrowed.iter().rev().find(|(name, at, _)| name == tuple && *at == place).map(|(_, _, ty)| ty.clone()),
            _ => None,
        };
        let items = match &base_ty {
            Type::Unknown => return Ok(Type::Unknown),
            // A group's tuple: each copy of the task knows its length, so the
            // read stays until then (see `substitute_expr`).
            Type::Tuple(items) if group_in_tuple(items).is_some() => {
                let group = group_in_tuple(items).unwrap_or_default();
                let item = narrowed.unwrap_or_else(|| Type::Param(group_item(&group, place)));
                return Ok(Type::Maybe(Box::new(item)));
            }
            Type::Tuple(items) => items.clone(),
            other => return err(span, format!("'.#{}' reads an item of a tuple in a maybe, but this value is {}{}", place, other, extract_hint(&[other]))),
        };
        let Some(found) = items.get(place).cloned() else {
            let Some(narrowed) = narrowed else { return Ok(Type::None) };
            let told = Type::Maybe(Box::new(narrowed));
            expr.ty = Some(Type::None);
            self.coerce(expr, &told)?;
            return Ok(told);
        };
        // An item of another type than the one tested is read where the test
        // failed, in a block that is dropped, or one holding a `T`, which each
        // copy settles: either way it reads as the type tested.
        let read_as = match narrowed {
            Some(narrowed) if !same_ignoring_regions(&found, &narrowed) => Type::Maybe(Box::new(narrowed)),
            _ => Type::Maybe(Box::new(found.clone())),
        };
        let taken = std::mem::replace(&mut expr.kind, ExprKind::NoneLit);
        if let ExprKind::Field(base, _) = taken {
            let mut read = Expr::new(ExprKind::Field(base, place.to_string()), span);
            read.ty = Some(found);
            expr.kind = ExprKind::Wrap(Box::new(read));
        }
        Ok(read_as)
    }

    /// The group whose values `e` names, when it is a name of them, `args`.
    fn group_of(&self, e: &Expr) -> Option<String> {
        let ExprKind::Var(name) = &e.kind else { return None };
        match &self.lookup(name)?.ty {
            Type::Group(group) => Some(group.clone()),
            _ => None,
        }
    }

    /// Checks `args.len`, read from `base`, the values of a group: how many
    /// there are, a number each copy of the task knows (see `substitute_expr`).
    fn check_group_member(&mut self, base: &mut Expr, member: &str, span: Span) -> Result<Type> {
        let Some(group) = self.group_of(base) else { return Err(Error::internal_at(span, "a group's member read from no group")) };
        base.ty = Some(Type::Group(group.clone()));
        match member {
            LEN => Ok(Type::Int),
            // The tuple for two or more values; for one or none, a failure
            // holding the one value, or none (see `group_tuple_type`).
            GROUP_TUPLE if !self.allows(Need::Frozen, &Type::Group(group.clone())) => err(
                span,
                format!("a tuple's items never change, so making one of the values of the group '{0}...' asks each of its types to be frozen: '<{0}...: frozen>'", group),
            ),
            GROUP_TUPLE => Ok(group_tuple_type(&group)),
            _ => err(
                span,
                format!("the values of the group '{}...' have 'len', how many they are, and 'tuple', and nothing else to read with '.'; give them on whole, as in 'next(args...)'", group),
            ),
        }
    }

    /// Checks `args...`, the values of a group parameter given on whole as the
    /// last values of a call, and returns the group's type.
    fn check_spread(&mut self, inner: &mut Expr, span: Span) -> Result<Type> {
        let outer = std::mem::replace(&mut self.spreading, true);
        let ty = self.check_expr(inner);
        self.spreading = outer;
        match ty? {
            ty @ (Type::Group(_) | Type::Unknown) => Ok(ty),
            ty => err(span, format!("'...' gives the values of a group on whole, as in 'next(args...)', and this is a value of type {}", ty)),
        }
    }

    /// Moves the body of each task with wrappers, `$(w1, w2(a))`, into a task of
    /// its own, the innermost layer, and makes a task for each other layer: the
    /// task itself calls `w1`, giving it the next layer, its own values, and the
    /// task's values, and so on inward (see `ast::layer_name`). Every call of the
    /// task, and every value of it, goes through the whole chain, the task
    /// calling itself too. What each layer sends is worked out once the headers
    /// are checked (see `settle_wrappers`), so each layer's header sends what the
    /// task does until then.
    fn add_wrappers(&mut self, items: &mut Vec<Item>) {
        let mut layers = Vec::new();
        for item in items.iter_mut() {
            let Item::Task(task) = item else { continue };
            if task.wrappers.is_empty() {
                continue;
            }
            let calls = std::mem::take(&mut task.wrappers);
            let at = calls.first().map_or(task.span, |call| call.span);
            if !task.types.is_empty() {
                let message = match method_parts(&task.name) {
                    Some((owner, method)) if task.owner.is_some() => format!(
                        "'{}' is a method of '{}', which takes types, so it can't have wrappers yet: each wrapper is given it as a value, and only a task that takes none can be one",
                        method,
                        unqualified(owner)
                    ),
                    _ => format!("task '{}' takes types, so it can't have wrappers yet: each wrapper is given it as a value, and only a task that takes none can be one", unqualified(&task.name)),
                };
                self.record(Error::at(at, message));
                continue;
            }
            if task.native {
                self.record(Error::at(at, format!("native task '{}' has no body of Vela's to wrap", unqualified(&task.name))));
                continue;
            }
            let count = calls.len();
            let unnamed = |task: &TaskDef| task.ret.results().iter().enumerate().map(|(i, _)| ResultDef { name: None, ty_span: task.result_span(i) }).collect::<Vec<_>>();
            let mut inner = task.clone();
            inner.name = layer_name(&task.name, count);
            inner.private = true;
            inner.decorators = Vec::new();
            inner.passes = Vec::new();
            for k in 1..count {
                let mut layer = task.clone();
                layer.name = layer_name(&task.name, k);
                layer.private = true;
                layer.decorators = Vec::new();
                layer.passes = Vec::new();
                layer.body = Vec::new();
                layer.results = unnamed(task);
                layers.push(layer);
            }
            // Each layer is the task in a message.
            for k in 1..=count {
                let layer = layer_name(&task.name, k);
                self.written_names.insert(unqualified(&layer).to_string(), unqualified(&task.name).to_string());
            }
            task.results = unnamed(task);
            task.body = Vec::new();
            layers.push(inner);
            self.wrapped.push((task.name.clone(), calls));
        }
        items.extend(layers.into_iter().map(Item::Task));
    }

    /// Works out what each layer of each task with wrappers sends, from the
    /// innermost out, and writes its body: the call of its wrapper.
    fn settle_wrappers(&mut self, items: &mut [Item]) {
        let places: HashMap<String, usize> = items.iter().enumerate().filter(|(_, item)| matches!(item, Item::Task(_))).map(|(i, item)| (item.name().to_string(), i)).collect();
        for (task, calls) in self.wrappers_first() {
            self.current_module = module_of(&task).to_string();
            let settled = self.settle_wrapped(items, &places, &task, &calls).and_then(|()| self.wrapped_privately(items, &places, &task, &calls));
            if !self.keep_going(settled) {
                // What the layers send isn't known, so neither is what the task does.
                self.broken.insert(task.clone());
                for k in 1..calls.len() {
                    self.broken.insert(layer_name(&task, k));
                }
            }
        }
        self.current_module.clear();
    }

    /// Refuses a public task that its wrappers make send a type private to
    /// its file, which other files couldn't name, as its header couldn't.
    fn wrapped_privately(&self, items: &[Item], places: &HashMap<String, usize>, task: &str, calls: &[Expr]) -> Result<()> {
        let Some(Item::Task(t)) = places.get(task).and_then(|&at| items.get(at)) else { return Ok(()) };
        if t.private || t.owner_type().is_some_and(|ty| self.private.contains(ty)) {
            return Ok(());
        }
        let Some(info) = self.tasks.get(task) else { return Ok(()) };
        let hidden = info.ret.results().iter().flat_map(named_types).find(|n| self.shapes.get(*n).is_some_and(|s| s.private) || self.kinds.get(*n).is_some_and(|k| k.private));
        match (hidden, calls.first()) {
            (Some(hidden), Some(call)) => err(
                call.span,
                format!(
                    "task '{}' is public, and its wrappers make it send '{}', which is private to its file, so other files couldn't name it; make the task 'priv' too, or the type public",
                    unqualified(task),
                    unqualified(hidden)
                ),
            ),
            _ => Ok(()),
        }
    }

    /// The tasks with wrappers (see `wrapped`), each after its wrappers that
    /// have wrappers of their own: what a wrapper sends is settled before a
    /// task it wraps reads it, wherever the two are written.
    fn wrappers_first(&mut self) -> Vec<(String, Vec<Expr>)> {
        let wrapped = std::mem::take(&mut self.wrapped);
        let order: Vec<String> = wrapped.iter().map(|(task, _)| task.clone()).collect();
        let mut pending: HashMap<String, Vec<Expr>> = wrapped.into_iter().collect();
        let mut settled = Vec::new();
        for task in order {
            self.wrapper_first(&task, &mut pending, &mut settled);
        }
        settled
    }

    /// Adds `task`, if it is still `pending`, to `settled` after its wrappers.
    fn wrapper_first(&self, task: &str, pending: &mut HashMap<String, Vec<Expr>>, settled: &mut Vec<(String, Vec<Expr>)>) {
        // Taken out first, so a wrapper is never waited on twice.
        let Some(calls) = pending.remove(task) else { return };
        let module = module_of(task);
        for call in &calls {
            if let ExprKind::Call { name, .. } = &call.kind {
                // `full_name`, for the module `task` is in.
                let full = match self.modules.get(module).and_then(|scope| scope.imported(name)) {
                    // `$(.checked)`: a method of the wrapped method's type.
                    _ if name.starts_with(MEMBER_WRAPPER) => match method_parts(task) {
                        Some((owner, _)) => method_name(owner, name.trim_start_matches(MEMBER_WRAPPER)),
                        None => continue,
                    },
                    _ if name.contains(MODULE_SEPARATOR) => name.clone(),
                    Some(imported) => imported.to_string(),
                    None => qualify(module, name),
                };
                self.wrapper_first(&full, pending, settled);
            }
        }
        settled.push((task.to_string(), calls));
    }

    fn settle_wrapped(&mut self, items: &mut [Item], places: &HashMap<String, usize>, task: &str, calls: &[Expr]) -> Result<()> {
        if self.broken.contains(task) {
            return Ok(());
        }
        let Some(info) = self.tasks.get(task) else { return Ok(()) };
        if let (Type::Results(_), Some(call)) = (&info.ret, calls.first()) {
            let what = match (method_parts(task), &info.owner) {
                (Some((_, method)), Some(owner)) if !owner.shared => format!("method '{}'", method),
                _ => format!("task '{}'", unqualified(task)),
            };
            return err(call.span, format!("{} sends several values, so it can't have wrappers yet: each wrapper is given it as a task value, which sends one value or nothing", what));
        }
        // How the task takes each value, which the rest of the chain takes too:
        // a method of every value its value first (see `task_params_taken`).
        let (types, taken) = self.task_params_taken(info);
        let params: Vec<(String, Type)> = info.params.iter().zip(&types).map(|((name, _), ty)| (name.clone(), ty.clone())).collect();
        let layers = calls.len();
        let mut sends = self.tasks.get(&layer_name(task, layers)).map_or(Type::Unknown, |inner| inner.ret.clone());
        let written_task = unqualified(task);
        for (k, call) in calls.iter().enumerate().rev() {
            let ExprKind::Call { name, args, .. } = &call.kind else { return Err(Error::internal_at(call.span, "a wrapper that is no call")) };
            let written = unqualified(name);
            // `$(.checked)`: a method of the value the wrapped method is called
            // on, which it is called on too (see `member_wrapper`).
            let member = name.strip_prefix(MEMBER_WRAPPER);
            let full = match member {
                Some(method) => self.member_wrapper(task, method, call.span)?,
                None => self.full_name(name),
            };
            // A wrapper whose header has a mistake, told already, promises
            // nothing to check the chain against.
            if self.broken.contains(&full) {
                self.broken.insert(task.to_string());
                for k in 1..layers {
                    self.broken.insert(layer_name(task, k));
                }
                return Ok(());
            }
            let Some(wrapper) = self.tasks.get(&full).cloned() else {
                if BUILTIN_TASKS.contains(&name.as_str()) || number_cast(name).is_some() {
                    return err(call.span, format!("'{}' is built in, so it can't be a wrapper: a wrapper is a task or a shared method the program declares", name));
                }
                // `X.m`, naming no method of `X`.
                if let Some((owner, field)) = self.shared_field(&full) {
                    return err(call.span, format!("'{}.{}' is a shared field, not a method, so it can't be a wrapper: a wrapper is a task or a shared method", owner, field));
                }
                if method_parts(&full).is_some() {
                    return Err(self.unknown_task(&full, &[], call.span));
                }
                if let Some((says, written)) = self.own_shared_method(name, task) {
                    return err(call.span, format!("unknown task '{}'; {}, so it is written '$({})'", name, says, written));
                }
                return err(call.span, format!("unknown task '{}'; a wrapper is a task that takes the task it wraps first", name));
            };
            // A shared method is a task of its type's; one of every value has
            // a value to be called on, which no layer gives it.
            if member.is_none() && wrapper.owner.as_ref().is_some_and(|owner| !owner.shared) {
                return err(
                    call.span,
                    format!("'{}' is a method of every value, called on one, so it can't be a wrapper of a task; it wraps a method of its own type, written '$(.{})' above it", name, wrapper.owner.as_ref().map_or("", |owner| owner.name.as_str())),
                );
            }
            // A member wrapper is called on the value, which it takes as its
            // method does; the rest of its parameters are a wrapper's.
            let skip = usize::from(member.is_some());
            // The values the wrapper takes of the task's: a member wrapper is
            // called on the value instead.
            let values = types.len() - skip;
            let (whose, besides) = match (member, method_parts(task)) {
                (Some(_), Some((_, method))) => (format!("method '{}'", method), " besides the value it is called on"),
                _ => (format!("task '{}'", written_task), ""),
            };
            // The value a member wrapper is called on, whose region is the one
            // the wrapped method's value is in.
            let this = wrapper.params.first().map(|(_, ty)| ty.clone()).filter(|_| member.is_some());
            let wrapper = match member {
                Some(_) => TaskInfo { params: wrapper.params.iter().skip(1).cloned().collect(), passing: wrapper.passing.iter().skip(1).copied().collect(), ..wrapper },
                None => wrapper,
            };
            match wrapper.params.first() {
                Some((_, ty)) if is_task(ty) => {}
                Some((_, Type::Closure(closure))) => {
                    return err(
                        call.span,
                        format!(
                            "wrapper '{}' takes {} first, the type of a lambda that keeps what it uses, and the rest of the chain is a task, which keeps nothing; take it as '{}'",
                            written,
                            Type::Closure(closure.clone()),
                            Type::Task(Box::new(closure.task.clone()))
                        ),
                    );
                }
                other => {
                    let takes = other.map_or("nothing".to_string(), |(_, ty)| format!("{} first", ty));
                    let message = match member {
                        Some(method) => format!(
                            "method '{0}' takes {1} after its value, so it can't be a wrapper: a method wrapping another takes the rest of the chain first, a task taking the value first, as in '{0}(next: (flex& T@a, int) -> int, n: int) -> int'",
                            method, takes
                        ),
                        None => format!("task '{}' takes {}, so it can't be a wrapper: a wrapper takes the rest of the chain first, a task of the type of the task it wraps, as in 'task {}(next: (int) -> int, n: int) -> int'", written, takes, written),
                    };
                    return err(call.span, message);
                }
            }
            // Which of the wrapper's parameters take the task's values, and which
            // of the task's each takes: all of them, after its own values, or
            // for a wrapper taking a group, the ones the next task's type names
            // around its group, and the rest in the group (see `wrapper_tail`).
            let tail = match self.wrapper_tail(&wrapper, params.get(skip..).unwrap_or_default(), taken.get(skip..).unwrap_or_default(), written, written_task, call.span)? {
                Some(tail) => tail,
                None => {
                    let message = match wrapper.params.len().checked_sub(1 + values) {
                        None => format!(
                            "wrapper '{}' takes {} after the next task, and {} takes {}{}, which a wrapper takes after its own values",
                            written,
                            count(wrapper.params.len() - 1, "value"),
                            whose,
                            count(values, "value"),
                            besides
                        ),
                        Some(own) => format!("wrapper '{}' takes the next task, {} of its own, then the {} of {}{}; {} given", written, count(own, "value"), count(values, "value"), whose, besides, count(args.len(), "value")),
                    };
                    return err(call.span, message);
                }
            };
            let grouped = tail.group.is_some();
            if tail.own != args.len() {
                let message = if grouped {
                    format!("wrapper '{}' takes the next task, {} of its own, then the task's values; {} given", written, count(tail.own, "value"), count(args.len(), "value"))
                } else {
                    format!("wrapper '{}' takes the next task, {} of its own, then the {} of {}{}; {} given", written, count(tail.own, "value"), count(values, "value"), whose, besides, count(args.len(), "value"))
                };
                return err(call.span, message);
            }
            if matches!(wrapper.ret, Type::Results(_)) {
                return err(call.span, format!("wrapper '{}' sends several values, and a wrapper sends one value or nothing, as a task value does", written));
            }
            // The wrapper's types and regions, from the next task and the task's
            // values, which are known here; its own values are the call's to say.
            // Its types and regions are found in the task's own names, which
            // its values and what it sends are in; the type is compared as every
            // task type is, with its regions named in order.
            let named_next = Type::Task(Box::new(TaskType { params: types.clone(), taken: taken.clone(), ret: sends.clone() }));
            let next = named_next.normalized_tasks();
            // A wrapper that takes no types takes these as they are, which is
            // said as the wrapper's mistake rather than its call's.
            if wrapper.types.is_empty()
                && !sends.unknown()
                && let Some((_, first)) = wrapper.params.first()
                && *first != next
            {
                let chain = if k + 1 == layers { format!("task '{}'", written_task) } else { format!("what comes after wrapper '{}'", written) };
                // A value the wrapper names in no region, where the task's is in
                // one, as a method's value may be.
                let unnamed = match (first, &next) {
                    (Type::Task(mine), Type::Task(theirs)) => mine.params.iter().zip(&theirs.params).any(|(mine, theirs)| mine.regions().is_empty() && !theirs.regions().is_empty()),
                    _ => false,
                };
                let regions = match &next {
                    Type::Task(task) if unnamed && strip_all_regions(first) == strip_all_regions(&next) => {
                        let example = task.params.iter().find_map(|ty| match ty {
                            Type::Named(name, Some(_)) => Some(format!("{}@a", unqualified(name))),
                            _ => None,
                        });
                        format!(
                            "; they differ in their regions only: a value of a type holding text or pointers, as the value a method is called on, may be in any region the caller gives, so the wrapper names one, as in '{}'",
                            example.unwrap_or_else(|| "text@a".to_string())
                        )
                    }
                    _ => String::new(),
                };
                return err(call.span, format!("{} is a {}, and wrapper '{}' takes a {} first{}", chain, next, written, first, regions));
            }
            // How it takes the task's values is its own to say, types or none;
            // what they are, where its own type for one names no type it takes.
            if !sends.unknown() {
                for &(mine, theirs) in &tail.pairs {
                    let theirs = theirs + skip;
                    let (Some((param, ty)), Some(passing), Some((value, given)), Some(how)) = (wrapper.params.get(mine), wrapper.passing.get(mine), params.get(theirs), taken.get(theirs)) else { continue };
                    let same_way = match (taken_by(param, ty, *passing), how) {
                        (Taken::Region(_), Taken::Region(_)) => true,
                        (mine, theirs) => mine == *theirs,
                    };
                    let differs = (wrapper.types.is_empty() || ty.params().is_empty()) && !ty.unknown() && !given.unknown() && strip_all_regions(ty) != strip_all_regions(given);
                    if differs || !same_way {
                        let task_param = shown_param(value, given, task_passing(how));
                        let wrapper_param = shown_param(param, ty, *passing);
                        return err(call.span, format!("task '{}' takes '{}', and wrapper '{}' takes '{}' in its place", written_task, task_param, written, wrapper_param));
                    }
                }
            }
            let mut given = HashMap::new();
            let mut regions = HashMap::new();
            let mut pairs: Vec<(&Type, &Type)> = wrapper.params.first().map(|(_, ty)| (ty, &named_next)).into_iter().collect();
            pairs.extend(tail.pairs.iter().filter_map(|&(mine, theirs)| Some((&wrapper.params.get(mine)?.1, types.get(theirs + skip)?))));
            pairs.extend(this.as_ref().zip(types.first()));
            for (pattern, actual) in pairs.into_iter().filter(|(_, actual)| !actual.unknown()) {
                unify_params(pattern, actual, &mut given);
                unify_regions(pattern, actual, &mut regions);
            }
            // A region the task takes is the one the wrapper takes in its place,
            // which only their names tell.
            for &(mine, theirs) in &tail.pairs {
                if let (Some((param, Type::Region)), Some((given_name, Type::Region))) = (wrapper.params.get(mine), params.get(theirs + skip)) {
                    regions.insert(param.clone(), given_name.clone());
                }
            }
            let sent = self.settled_type(&wrapper.ret, &regions, &given, call.span)?;
            if let Some(param) = sent.params().first() {
                return err(call.span, format!("wrapper '{}' sends a {}, and '{}' is given by none of the next task and the task's values, so what it sends is not known here", written, sent, param));
            }
            if let Some((param, ty)) = wrapper.types.iter().find_map(|param| given.get(&param.name).filter(|ty| ty.regions().contains(&OWN_REGION)).map(|ty| (&param.name, ty))) {
                return err(call.span, own_region_type("wrapper", written, param, ty));
            }
            if sent.regions().contains(&ANY_REGION) {
                return err(call.span, format!("wrapper '{}' sends a value in a region none of the task's values is in, so the task can't send it", written));
            }
            let layer = if k == 0 { task.to_string() } else { layer_name(task, k) };
            if layer == "main" && sent != Type::Nothing {
                return err(call.span, format!("task 'main' cannot return a value, and wrapper '{}' sends {}", written, sent));
            }
            // The call of the wrapper, given the next layer, its own values, and
            // the task's.
            let mut given_args = vec![Expr::new(ExprKind::Var(layer_name(task, k + 1)), call.span)];
            given_args.extend(args.iter().cloned());
            // A place the task is given, it gives on as one; a member wrapper is
            // called on the value instead.
            given_args.extend(params.iter().zip(&taken).skip(skip).map(|((param, _), how)| {
                let value = Expr::new(ExprKind::Var(param.clone()), call.span);
                match how {
                    Taken::Place(_) => Expr::new(ExprKind::Ref(Box::new(value)), call.span),
                    _ => value,
                }
            }));
            let wrapped = match member {
                Some(method) => {
                    let value = Expr::new(ExprKind::Var(THIS.to_string()), call.span);
                    Expr::new(ExprKind::MethodCall { value: Box::new(value), name: method.to_string(), args: given_args, dotted: None }, call.span)
                }
                None => Expr::new(ExprKind::Call { name: name.clone(), types: Vec::new(), args: given_args }, call.span),
            };
            let body = match sent {
                Type::Nothing => Stmt::new(StmtKind::Expr(wrapped), call.span),
                _ => Stmt::new(StmtKind::Send(vec![wrapped]), call.span),
            };
            if let Some(info) = self.tasks.get_mut(&layer) {
                info.ret = sent.clone();
            }
            if let Some(Item::Task(def)) = places.get(&layer).and_then(|&i| items.get_mut(i)) {
                def.results = if sent == Type::Nothing { Vec::new() } else { vec![ResultDef { name: None, ty_span: call.span }] };
                def.ret = sent.clone();
                def.body = vec![body];
            }
            self.wrapper_layers.insert(layer, types.len());
            sends = sent;
        }
        let callers = (0..layers).map(|k| if k == 0 { task.to_string() } else { layer_name(task, k) }).collect();
        self.wrapped_chains.push((task.to_string(), callers));
        Ok(())
    }

    /// The task a member wrapper, `$(.method)`, names above `task`: the method
    /// of every value of `task`'s own type, which `task` must be a method of
    /// every value of; one wrapping a method that alters the value alters it
    /// too, and only such a one does, so neither sees the value change under
    /// it as a value that never changes.
    fn member_wrapper(&self, task: &str, method: &str, at: Span) -> Result<String> {
        let wrapped = self.tasks.get(task).and_then(|info| info.owner.clone());
        let (Some(wrapped), Some((owner, _))) = (wrapped.filter(|owner| !owner.shared), method_parts(task)) else {
            let what = if self.tasks.get(task).is_some_and(|info| info.owner.is_some()) { "a shared method, called on no value" } else { "no method" };
            return err(at, format!("'.{}' names a method of the value the wrapped method is called on, and '{}' is {}; name a task, as in '$({})'", method, unqualified(task), what, method));
        };
        let full = method_name(owner, method);
        let Some(found) = self.tasks.get(&full).and_then(|info| info.owner.clone()) else {
            let what = if self.kinds.contains_key(owner) { "kind" } else { "shape" };
            return err(at, format!("{} '{}' has no method '{}'", what, unqualified(owner), method));
        };
        if found.shared {
            return err(at, format!("'{0}' is a shared method of '{1}', called on no value, so it is named on its type, as in '$({1}.{0})'", method, unqualified(owner)));
        }
        match (wrapped.alters, found.alters) {
            (true, false) => err(
                at,
                format!("method '{}' alters the value it is called on, so a method wrapping it alters it too, 'alters {}(…)': with a value that never changes, it would see the value change under it", wrapped.name, method),
            ),
            (false, true) => err(
                at,
                format!("method '{0}' alters the value it is called on, and method '{1}' doesn't, so '{1}' may be called on a value that never changes, which '{0}' would change; a method wrapping '{1}' doesn't alter it", method, wrapped.name),
            ),
            _ => Ok(full),
        }
    }

    /// Which of `wrapper`'s parameters take the values of the task it wraps,
    /// which takes `params` as `taken` says: all of them, after the wrapper's
    /// own values (`None` where there are too few), or for a wrapper taking a
    /// group, `args: P...`, the values the next task's type names one by one
    /// around its group, `next: (@a, P...) -> R`, which the wrapper names so
    /// too, and the rest in the group, which holds no region.
    fn wrapper_tail(&self, wrapper: &TaskInfo, params: &[(String, Type)], taken: &[Taken], written: &str, written_task: &str, at: Span) -> Result<Option<WrapperTail>> {
        let Some(group) = wrapper.params.iter().position(|(_, ty)| matches!(ty, Type::Group(_))) else {
            let Some(own) = wrapper.params.len().checked_sub(1 + params.len()) else { return Ok(None) };
            return Ok(Some(WrapperTail { own, pairs: (0..params.len()).map(|i| (1 + own + i, i)).collect(), group: None }));
        };
        let Some((_, Type::Group(name))) = wrapper.params.get(group) else { return Ok(None) };
        let Some((next_name, Type::Task(next))) = wrapper.params.first() else { return Ok(None) };
        let Some(inner) = next.params.iter().position(|ty| *ty == Type::Group(name.clone())) else {
            return err(at, format!("wrapper '{0}' takes the task's values in its group '{1}...', so '{2}' takes that group too, as in '{2}: ({1}...) -> R'", written, name, next_name));
        };
        let (before, after) = (inner, next.params.len() - inner - 1);
        let named_after = wrapper.params.len() - group - 1;
        if named_after != after || group < 1 + before {
            return err(
                at,
                format!(
                    "wrapper '{}' names {} before its group and {} after it, and the type of '{}' names {} before the group and {} after it: the values named around the group are the task's, named as the next task's type names them, after the wrapper's own values",
                    written,
                    count(group.saturating_sub(1), "value"),
                    count(named_after, "value"),
                    next_name,
                    count(before, "value"),
                    count(after, "value")
                ),
            );
        }
        let own = group - 1 - before;
        if params.len() < before + after {
            return err(at, format!("task '{}' takes {}, and wrapper '{}' names {} around its group", written_task, count(params.len(), "value"), written, before + after));
        }
        let end = params.len() - after;
        if let Some(k) = (before..end).find(|&k| matches!(taken.get(k), Some(Taken::Region(_))))
            && let (Some(ahead), Some((region, _))) = (params.get(..k), params.get(k))
        {
            // The values the task takes before the region are named before it
            // too, a method's value first.
            // A place is named so in both.
            let how = |i: usize| match taken.get(i) {
                Some(Taken::Place(passing)) => format!("{} ", passing.word()),
                _ => String::new(),
            };
            let types: String = ahead.iter().enumerate().map(|(i, (_, ty))| format!("{}{}, ", how(i), ty)).collect();
            let names: String = ahead.iter().enumerate().map(|(i, (param, ty))| format!("{}{}: {}, ", how(i), if param == THIS { "v" } else { param }, ty)).collect();
            return err(
                at,
                format!(
                    "task '{0}' takes the region '{1}', which would be in the group of wrapper '{2}', and a group never holds a region; '{2}' names it as a parameter of its own, and '{3}' names it in its type, as in '{3}: ({5}@a, {4}...) -> R, {6}{1}: region, args: {4}...'",
                    written_task, region, written, next_name, name, types, names
                ),
            );
        }
        let mut pairs: Vec<(usize, usize)> = (0..before).map(|i| (1 + own + i, i)).collect();
        pairs.extend((0..after).map(|k| (group + 1 + k, end + k)));
        Ok(Some(WrapperTail { own, pairs, group: Some(group) }))
    }

    /// Checks the values of its own each wrapper is given in `$( … )`. Each is
    /// worked out where the program starts, where no variable and no region
    /// exist yet. One the compiler works out (a literal, a macro, a task's name)
    /// stays written into every call, which costs nothing; any other (a call, a
    /// shared field) becomes a shared field of `START_VALUES`, given its value
    /// once where the program starts, which every call of the task reads. The
    /// order they are worked out in is settled once every body is checked (see
    /// `order_start_values`).
    fn check_start_values(&mut self, items: &mut [Item]) {
        let places: HashMap<String, usize> = items.iter().enumerate().filter(|(_, item)| matches!(item, Item::Task(_))).map(|(i, item)| (item.name().to_string(), i)).collect();
        // In the order the program is written, its modules first, as its
        // startup decorators run.
        let mut chains = std::mem::take(&mut self.wrapped_chains);
        chains.sort_by_key(|(task, _)| places.get(task).copied().unwrap_or(usize::MAX));
        let mut members = Vec::new();
        for (task, callers) in &chains {
            self.current_module = module_of(task).to_string();
            for caller in callers {
                let Some(&takes) = self.wrapper_layers.get(caller) else { continue };
                let Some(Item::Task(def)) = places.get(caller).and_then(|&i| items.get_mut(i)) else { continue };
                let Some(call) = wrapper_call(&mut def.body) else { continue };
                // `$(.checked)`: a method of the value, called on it, which the
                // call's values leave out, as they do the value it takes first.
                let (wrapper, name, args, skip) = match &mut call.kind {
                    ExprKind::Call { name, args, .. } => (self.full_name(name), name.clone(), args, 0),
                    ExprKind::MethodCall { name, args, .. } => match method_parts(task) {
                        Some((owner, _)) => {
                            let full = method_name(owner, name);
                            let shown = codegen::notice_name(&full);
                            (full, shown, args, 1)
                        }
                        None => continue,
                    },
                    _ => continue,
                };
                let params: Vec<Type> = self.tasks.get(&wrapper).map(|info| info.params.iter().skip(skip).map(|(_, ty)| ty.clone()).collect()).unwrap_or_default();
                let passing: Vec<Passing> = self.tasks.get(&wrapper).map(|info| info.passing.iter().skip(skip).copied().collect()).unwrap_or_default();
                let own = args.len().saturating_sub(1 + takes - skip);
                // The types the wrapper takes, as the next task it is given tells
                // them, `T` of `next: (T) -> T` (see `settle_wrapped`).
                let mut known = HashMap::new();
                if let (Some(first), Some(Expr { kind: ExprKind::Var(next) | ExprKind::TaskValue(next), span, .. })) = (params.first(), args.first())
                    && let Ok(next) = self.task_value_type(&self.full_name(next), *span)
                {
                    self.match_params(first, &next, &mut known);
                }
                for (i, arg) in args.iter_mut().enumerate().skip(1).take(own) {
                    // A place the wrapper changes or reads where it is kept: no
                    // value worked out once is one.
                    if let Some(mark) = passing.get(i).filter(|passing| passing.is_ref()).map(|passing| if *passing == Passing::FlexRef { "flex&" } else { "fixed&" }) {
                        self.record(Error::at(
                            arg.span,
                            format!("wrapper '{}' takes this as a place, with '{}', but {} are worked out once, where the program starts, so none is a place; take it as a value, or name the place inside the wrapper", codegen::notice_name(&wrapper), mark, WRAPPER_VALUES),
                        ));
                        if !matches!(arg.kind, ExprKind::Ref(_)) {
                            // Marked, so the wrapper's call says nothing more about it.
                            let span = arg.span;
                            *arg = Expr::new(ExprKind::Ref(Box::new(arg.clone())), span);
                        }
                        continue;
                    }
                    // The type the wrapper takes it as, where that doesn't depend on
                    // the value itself; its regions are no region's, since none
                    // exists where the program starts.
                    let expected = params.get(i).map(|ty| ty.map_params(&mut |name| known.get(name).cloned())).filter(|ty| ty.params().is_empty() && !ty.unknown()).map(|ty| ty.without_regions());
                    let mut value = arg.clone();
                    let checked = self.check_start_value(&mut value, expected.as_ref());
                    if checked.is_ok() && constant_value(&value) {
                        // Written into every call as it was checked here, where no
                        // variable is seen: a task's name stays the task's, even
                        // where the task takes a parameter of that name.
                        *arg = value;
                        continue;
                    }
                    let span = arg.span;
                    let ty = match checked {
                        Ok(()) => value.ty.clone().unwrap_or(Type::Unknown),
                        // The call reads a value of no type, so nothing complains
                        // about it again (see `Type::unknown`).
                        Err(error) => {
                            self.record(error);
                            Type::Unknown
                        }
                    };
                    let member = format!("v{}", members.len() + 1);
                    let place = format!("wrapper '{}' on task '{}'", codegen::notice_name(&wrapper), codegen::notice_name(task));
                    let field = Field {
                        name: member.clone(),
                        ty,
                        span,
                        ty_span: span,
                        private: false,
                        read: false,
                        default_region: false,
                        passing: Passing::Copy,
                        default: Some(value),
                    };
                    members.push(SharedField { field, flex: false, start: Some(place) });
                    self.start_values.push(StartValue { task: task.clone(), wrapper: name.clone(), member: member.clone(), span });
                    *arg = Expr::new(ExprKind::Shared { shape: START_VALUES.to_string(), field: member }, span);
                }
            }
        }
        self.current_module.clear();
        if !members.is_empty() {
            let values = ShapeDef { name: START_VALUES.to_string(), types: Vec::new(), given: Vec::new(), fields: Vec::new(), shared: members, span: Span::default(), private: false };
            self.shapes.insert(START_VALUES.to_string(), values);
        }
    }

    /// Words the mistakes recorded since there were `recorded` of them, found in
    /// a value worked out where the program starts, as such (see
    /// `at_start_error`): a check that goes on after a mistake records it
    /// rather than sending it back.
    fn said_at_start(&mut self, recorded: usize, what: &str) {
        for error in self.errors.iter_mut().skip(recorded) {
            *error = at_start_error(error.clone(), what);
        }
    }

    /// Checks `value`, a wrapper's own value, where the program starts: with no
    /// variable and no region, as a startup decorator's values are checked, and
    /// as the type `expected` the wrapper takes it as, where that is known. It
    /// is kept for as long as the program runs, so it holds nothing in a region.
    fn check_start_value(&mut self, value: &mut Expr, expected: Option<&Type>) -> Result<()> {
        let copies = self.unsettled_copies.len();
        let types = self.unsettled_types.len();
        let recorded = self.errors.len();
        self.scopes = vec![Scope::default()];
        self.at_start = Some(WRAPPER_VALUES);
        let checked = self.check_expr(value).and_then(|_| match expected {
            Some(ty) => self.coerce(value, ty),
            None => Ok(()),
        });
        self.at_start = None;
        self.said_at_start(recorded, WRAPPER_VALUES);
        self.scopes.clear();
        let uncopied = self.unsettled_copies.get(copies).copied();
        let untold = self.unsettled_types.get(types).cloned();
        self.unsettled_copies.truncate(copies);
        self.unsettled_types.truncate(types);
        checked.map_err(|error| at_start_error(error, WRAPPER_VALUES))?;
        if let Some(at) = uncopied {
            return err(at, self.unsettled_copy());
        }
        if let Some((at, task, param)) = untold {
            return err(at, self.untold_type(&task, &param));
        }
        let ty = value.ty.clone().unwrap_or(Type::Unknown);
        if ty == Type::Nothing {
            return err(value.span, "this sends nothing, so it is no value a wrapper can be given");
        }
        if unsettled(&ty) {
            return err(value.span, format!("can't tell which type this value is: the wrapper takes it as whatever type the value says, and '{}' doesn't say all of it", ty));
        }
        if holds_pointers(&ty, &self.regional) || ty.regions().iter().any(|region| *region != ANY_REGION) || self.holds_file(&ty) {
            return err(
                value.span,
                format!(
                    "{} are worked out once, where the program starts, and kept as long as it runs, which no region lasts, so none can hold a pointer, a slice, text of a region, a 'File', or a value holding one; this one is {}",
                    WRAPPER_VALUES,
                    a_type(&ty)
                ),
            );
        }
        Ok(())
    }

    /// Works out the order the values of wrappers are given where the program
    /// starts (see `check_start_values`): a task's after those of every task
    /// they may call, so that none reads a value not worked out yet, and in the
    /// order the program is written otherwise. A value that may call its own
    /// task, or values that may call each other's, have no such order and are
    /// refused. It reads the calls every body makes, so it runs once they are
    /// all checked, and only on a program without other mistakes.
    fn order_start_values(&mut self, items: &[Item]) {
        if self.start_values.is_empty() || !self.errors.is_empty() {
            return;
        }
        let mut graph = CallGraph::of(items, self);
        let Some(def) = self.shapes.get(START_VALUES) else { return };
        let values: HashMap<&str, &Expr> = def.shared.iter().filter_map(|member| member.field.default.as_ref().map(|value| (member.field.name.as_str(), value))).collect();
        // What a call of a task held as a value may reach where the program
        // starts, from what every value worked out there may run.
        let mut all = Calls::default();
        let mut named = Vec::new();
        for value in values.values() {
            walk_expr(value, &mut |e| {
                self.note_calls(e, &mut all);
                note_value(e, &mut named);
            });
        }
        graph.settle_values(&all, named);
        let mut units: Vec<&str> = Vec::new();
        for start in &self.start_values {
            if !units.contains(&start.task.as_str()) {
                units.push(&start.task);
            }
        }
        // For each task, the tasks whose values its values need first, each with
        // the value that needs it.
        let mut needs: HashMap<&str, Vec<(&str, &StartValue)>> = HashMap::new();
        let mut mistakes = Vec::new();
        for start in &self.start_values {
            let Some(value) = values.get(start.member.as_str()) else { continue };
            let mut roots = Calls::default();
            walk_expr(value, &mut |e| self.note_calls(e, &mut roots));
            let reached = graph.reach(&roots);
            if reached.contains_key(&start.task) {
                let through = graph.path(&reached, &start.task);
                mistakes.push(Error::at(
                    start.span,
                    format!(
                        "this value of wrapper '{0}' is worked out where the program starts, before task '{1}' can run, and it calls '{1}'{2}, whose wrappers read it; give a value that doesn't call it, or work it out inside the wrapper",
                        unqualified(&start.wrapper),
                        start.task,
                        through
                    ),
                ));
                continue;
            }
            for unit in units.iter().filter(|unit| **unit != start.task && reached.contains_key(**unit)) {
                needs.entry(start.task.as_str()).or_default().push((unit, start));
            }
        }
        if mistakes.is_empty() {
            match start_order(&units, &needs) {
                Ok(order) => {
                    let rank: HashMap<&str, usize> = order.iter().enumerate().map(|(i, unit)| (*unit, i)).collect();
                    let mut ranked: Vec<(usize, usize, String)> = self.start_values.iter().enumerate().map(|(i, start)| (rank.get(start.task.as_str()).copied().unwrap_or(usize::MAX), i, start.member.clone())).collect();
                    ranked.sort();
                    let position: HashMap<String, usize> = ranked.into_iter().enumerate().map(|(at, (_, _, member))| (member, at)).collect();
                    if let Some(def) = self.shapes.get_mut(START_VALUES) {
                        def.shared.sort_by_key(|member| position.get(&member.field.name).copied().unwrap_or(usize::MAX));
                    }
                }
                Err(circle) => {
                    let parts: Vec<String> = circle.iter().map(|(start, called)| format!("a value of wrapper '{}' on task '{}' calls '{}'", unqualified(&start.wrapper), start.task, called)).collect();
                    let at = circle.first().map_or(Span::default(), |(start, _)| start.span);
                    mistakes.push(Error::at(
                        at,
                        format!(
                            "these wrapper values are worked out where the program starts, and each needs another worked out first, in a circle: {}; work one of them out inside its wrapper instead",
                            parts.join(", and ")
                        ),
                    ));
                }
            }
        }
        for mistake in mistakes {
            self.record(mistake);
        }
    }

    /// Notes what `e` calls, without what is inside it (see `walk_expr`): a
    /// task called or named as a value, a call of a task held as a value, and
    /// the `str` of each value written as text. A generic task's body writes
    /// values of the types it is given, so a call of one writes them too.
    fn note_calls(&self, e: &Expr, calls: &mut Calls) {
        let wrote = |ty: &Type, calls: &mut Calls| {
            let mut found = Vec::new();
            self.str_methods(ty, &mut Vec::new(), &mut found);
            calls.tasks.extend(found);
        };
        match &e.kind {
            ExprKind::Call { name, types, args } => {
                if self.tasks.contains_key(name) {
                    calls.tasks.push(name.clone());
                }
                for (ty, _) in types {
                    wrote(ty, calls);
                }
                if name == "show" {
                    for arg in args {
                        if let Some(ty) = &arg.ty {
                            wrote(ty, calls);
                        }
                    }
                }
            }
            ExprKind::TaskValue(name) | ExprKind::Closure { lambda: name, .. } => calls.tasks.push(name.clone()),
            ExprKind::Invoke { .. } => calls.invokes = true,
            ExprKind::ToText { value, .. } => {
                if let Some(ty) = &value.ty {
                    wrote(ty, calls);
                }
            }
            _ => {}
        }
    }

    /// The `str` methods writing a value of type `ty` as text may call: those of
    /// the shapes and kinds with one of their own, wherever they are in it.
    fn str_methods(&self, ty: &Type, seen: &mut Vec<String>, found: &mut Vec<String>) {
        match ty {
            Type::Maybe(inner) | Type::Array(inner, _) | Type::Slice(inner, _) => self.str_methods(inner, seen, found),
            Type::Map(key, value, _) => {
                self.str_methods(key, seen, found);
                self.str_methods(value, seen, found);
            }
            Type::Tuple(items) => items.iter().for_each(|item| self.str_methods(item, seen, found)),
            Type::Named(name, _) if !seen.contains(name) => {
                seen.push(name.clone());
                let own = self.made_of.get(name).map_or(name.as_str(), |made| made.name.as_str());
                let method = method_name(own, STR);
                if self.tasks.contains_key(&method) {
                    found.push(method);
                } else {
                    for part in self.written_parts(ty).unwrap_or_default() {
                        self.str_methods(&part, seen, found);
                    }
                }
            }
            _ => {}
        }
    }

    /// Whether the startup decorator `stmt`, the call `d(task, args)`, takes
    /// the task first and as many values as it is given, said as a decorator's
    /// mistake rather than a call's. A generic decorator is left to the call.
    fn decorator_fits(&self, stmt: &Stmt) -> Result<()> {
        let StmtKind::Expr(call) = &stmt.kind else { return Ok(()) };
        let ExprKind::Call { name, args, .. } = &call.kind else { return Ok(()) };
        // `main` is where the program starts, after its startup decorators
        // have run, so none of them can take it.
        if let Some(ExprKind::Var(decorated)) = args.first().map(|arg| &arg.kind)
            && self.full_name(decorated) == "main"
        {
            return err(
                call.span,
                "'main' takes no startup decorator: the decorators run before 'main', where the program starts, and nothing calls 'main' as a value; call what the decorator would do first thing in 'main' instead",
            );
        }
        if self.tasks.get(&self.full_name(name)).and_then(|info| info.owner.as_ref()).is_some_and(|owner| !owner.shared) {
            return err(call.span, format!("'{}' is a method of every value, called on one, so it can't be a decorator; a shared method can", name));
        }
        if let Some((owner, field)) = self.shared_field(&self.full_name(name)) {
            return err(call.span, format!("'{}.{}' is a shared field, not a method, so it can't be a decorator: a decorator is a task or a shared method", owner, field));
        }
        if BUILTIN_TASKS.contains(&name.as_str()) || number_cast(name).is_some() {
            return err(call.span, format!("'{}' is built in, so it can't be a decorator: a decorator is a task or a shared method the program declares", name));
        }
        if !self.tasks.contains_key(&self.full_name(name)) && self.macros.contains_key(name.as_str()) {
            return err(call.span, format!("'{}' is a macro, a value that never changes, so it can't be a decorator: a decorator is a task or a shared method", name));
        }
        if let Some(ExprKind::Var(decorated)) = args.first().map(|arg| &arg.kind)
            && let Some(hint) = self.own_shared_method(name, decorated)
        {
            if hint.1.starts_with(MEMBER_WRAPPER) {
                return err(call.span, format!("'{}' is a method of every value, called on one, so it can't be a startup decorator, which runs where no value is yet; a shared method can", name));
            }
            return err(call.span, format!("unknown task '{}'; {}, so it is written '${}'", name, hint.0, hint.1));
        }
        let Some(info) = self.tasks.get(&self.full_name(name)).filter(|info| info.types.is_empty() && info.owner.as_ref().is_none_or(|owner| owner.shared)) else { return Ok(()) };
        let Some(ExprKind::Var(decorated)) = args.first().map(|arg| &arg.kind) else { return Ok(()) };
        // As it is written: a decorator of another module keeps its module's name.
        let own = format!("{}{}", self.current_module, MODULE_SEPARATOR);
        let written = name.strip_prefix(own.as_str()).unwrap_or(name);
        let first = match info.params.first() {
            Some((_, ty)) if is_task(ty) => ty,
            Some((_, Type::Closure(closure))) => {
                return err(
                    call.span,
                    format!(
                        "decorator '{}' takes {} first, the type of a lambda that keeps what it uses, and the task below it is a task, which keeps nothing; take it as '{}'",
                        written,
                        Type::Closure(closure.clone()),
                        Type::Task(Box::new(closure.task.clone()))
                    ),
                );
            }
            other => {
                let takes = other.map_or("nothing".to_string(), |(_, ty)| format!("{} first", ty));
                return err(
                    call.span,
                    format!("task '{}' takes {}, so it can't be a decorator: a decorator takes the task written below it first, as in 'task {}(f: (int) -> int)'", written, takes, written),
                );
            }
        };
        // The last of its values may fall back on values of their own (see
        // `fill_defaults`), as `$d` leaves them out.
        let needed = self.param_defaults.get(&self.full_name(name)).and_then(|defaults| defaults.iter().position(Option::is_some)).unwrap_or(info.params.len());
        if args.len() > info.params.len() || args.len() < needed {
            let values = info.params.len() - 1;
            let takes = if values == 0 {
                "the task and nothing else, so it is written '$".to_string() + written + "'"
            } else if needed <= 1 {
                format!("the task and up to {}, written '${}' or '${}(…)'", count(values, "value"), written, written)
            } else if needed < info.params.len() {
                format!("the task and {} to {} values, written '${}(…)'", needed - 1, values, written)
            } else {
                format!("the task and {}, written '${}(…)'", count(values, "value"), written)
            };
            return err(call.span, format!("decorator '{}' takes {}, got {}", written, takes, count(args.len() - 1, "value")));
        }
        let value = self.task_value_type(&self.full_name(decorated), call.span)?;
        if !value.unknown() && value != *first {
            return err(call.span, format!("task '{}' is a {}, and decorator '{}' takes a {} first", unqualified(decorated), value, written, first));
        }
        Ok(())
    }

    /// The type of `square`, task `name` named as a value (see
    /// `ExprKind::TaskValue`): its header, without names, but a region's, and
    /// with how each parameter is given (see `Taken`). A task that takes types
    /// or sends several values is none.
    fn task_value_type(&self, name: &str, span: Span) -> Result<Type> {
        if self.broken.contains(name) {
            return Ok(Type::Unknown);
        }
        let Some(info) = self.tasks.get(name) else { return Err(Error::internal_at(span, format!("no task '{}' to name as a value", name))) };
        let written = unqualified(name);
        if !info.types.is_empty() {
            return err(span, format!("task '{}' takes types, so it can't be a value yet; only a task that takes none can", written));
        }
        if matches!(info.ret, Type::Results(_)) {
            // A wrapper's next task, a layer, is named as the task it wraps.
            let base = name.split(LAYER).next().unwrap_or(name);
            let what = match (method_parts(base), &info.owner) {
                (Some((_, method)), Some(owner)) if !owner.shared => format!("method '{}'", method),
                _ => format!("task '{}'", unqualified(base)),
            };
            return err(span, format!("{} sends several values, so it can't be a value yet; a task value sends one value or nothing", what));
        }
        let (params, taken) = self.task_params_taken(info);
        Ok(Type::Task(Box::new(TaskType { params, taken, ret: info.ret.clone() }.normalized())))
    }

    /// The types of the values a task takes and how it takes each, as its type
    /// says: a method of every value takes the value it is called on first, as
    /// a place where it alters it, and of the type of its shape or kind, with
    /// the region the value lives in only where the type holds one.
    fn task_params_taken(&self, info: &TaskInfo) -> (Vec<Type>, Vec<Taken>) {
        let alters = info.owner.as_ref().is_some_and(|owner| owner.alters && !owner.shared);
        info.params
            .iter()
            .zip(info.passing.iter().chain(std::iter::repeat(&Passing::Copy)))
            .map(|((param, ty), passing)| match ty {
                Type::Named(name, Some(_)) if param == THIS && !self.regional.contains(name) && !self.takes_region.contains(name) => {
                    (Type::Named(name.clone(), None), if alters { Taken::Place(Passing::FlexRef) } else { Taken::Value })
                }
                _ if param == THIS && alters => (ty.clone(), Taken::Place(Passing::FlexRef)),
                _ => (ty.clone(), taken_by(param, ty, *passing)),
            })
            .unzip()
    }

    /// Checks `f(x)`, the call of the task value `callee` holds, and returns
    /// what it sends: its arguments are checked as a named task's are, against
    /// the task's type, whose regions stand for the caller's regions its values
    /// show.
    /// `[@r](x: int) -> int … fi` at `span`: the value of a lambda that keeps
    /// what it uses of the code around it in region `region`, a copy of each
    /// variable it uses, made here (see `kept_scope`).
    fn check_closure(&mut self, lambda: &str, region: &str, flex: bool, places: &[KeptPlace], span: Span) -> Result<Type> {
        let written = if flex { format!("[flex @{}]", region) } else { format!("[@{}]", region) };
        // Where the program starts no region exists yet.
        if self.at_start.is_some() {
            return err(span, format!("'{}' keeps what the lambda uses in a region; give a task by its name, or a plain lambda", written));
        }
        match self.lookup(region) {
            Some(info) if info.ty == Type::Region => {}
            Some(_) => return err(span, format!("'{}' keeps what the lambda uses in a region, and '{}' is a variable", written, region)),
            None => return err(span, format!("'{}' keeps what the lambda uses in region '{}', which isn't one here; declare it with 'region {}', or take it as a parameter", written, region, region)),
        }
        self.visible_region(region, span, true)?;
        for place in places {
            self.check_kept_place(place, region)?;
        }
        // What the lambda may keep: every variable here, and the one region,
        // with those its places are in, which outlive them.
        let mut seen_regions: Vec<String> = vec![region.to_string()];
        for place in places {
            if let Some(info) = self.scopes.iter().rev().find_map(|scope| scope.names.get(&place.name)) {
                seen_regions.extend(info.ty.regions().into_iter().filter(|name| !unnamed_region(name)).map(str::to_string));
            }
        }
        let mut outer: Vec<(String, VarInfo)> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        // Innermost first, so a name is the one the lambda sees.
        for scope in self.scopes.iter().rev() {
            for (name, info) in &scope.names {
                if !seen.insert(name.as_str()) {
                    continue;
                }
                // `.this` stays the method's: a lambda in one takes it as a parameter.
                if info.result || (info.ty == Type::Region && !seen_regions.contains(name)) || name == SHOWN || name == THIS {
                    continue;
                }
                outer.push((name.clone(), info.clone()));
            }
        }
        outer.sort_by_key(|(_, info)| (info.span.line, info.span.col));
        let names = self.scopes.iter().flat_map(|scope| scope.names.keys().cloned()).collect();
        self.lambda_outer.insert(lambda.to_string(), names);
        self.note_lambda_loops(lambda);
        let within = self.current_lambda.clone().filter(|current| self.kept_lambdas.contains_key(current));
        let owned = outer.iter().map(|(name, info)| (name.clone(), self.name_owners(name, info))).collect();
        self.kept_lambdas.insert(lambda.to_string(), KeptLambda { region: region.to_string(), flex, span, outer, within, uses: Vec::new(), places: places.to_vec(), owned });
        self.kept_order.retain(|name| name != lambda);
        self.kept_order.push(lambda.to_string());
        if self.broken.contains(lambda) {
            return Ok(Type::Unknown);
        }
        let Some(info) = self.tasks.get(lambda) else { return Err(Error::internal_at(span, format!("no lambda '{}'", lambda))) };
        let (params, taken) = self.task_params_taken(info);
        let task = TaskType { params, taken, ret: info.ret.clone() };
        Ok(Type::Closure(Box::new(ClosureType { task, region: region.to_string(), flex }.normalized())))
    }

    /// Checks `place`, a variable a lambda kept in `region` keeps as a place:
    /// one that changes where it changes it, and that outlives the lambda,
    /// which ends with `region`, so `region` is declared after it, in its
    /// block or a block inside it.
    fn check_kept_place(&self, place: &KeptPlace, region: &str) -> Result<()> {
        let name = &place.name;
        let word = place.passing.word();
        // A use, which a lambda around this one keeps (see `lookup`).
        let _ = self.lookup(name);
        let at = |name: &str| self.scopes.iter().enumerate().rev().find_map(|(i, scope)| scope.names.get(name).map(|info| (i, info)));
        let Some((depth, info)) = at(name) else {
            return err(place.span, format!("unknown variable '{}'; a lambda keeps as a place a variable of the code around it", name));
        };
        if info.ty == Type::Region {
            return err(place.span, format!("'{}' is a region, which a lambda names alone, as in '[@{}]', and keeps nothing of as a place", name, name));
        }
        if info.result {
            return err(place.span, format!("'{}' is a result of this task, which only '<-' sets, so a lambda doesn't keep it as a place", name));
        }
        if place.passing == Passing::FlexRef {
            let mut var = Expr::new(ExprKind::Var(name.clone()), place.span);
            var.ty = Some(info.ty.clone());
            self.given_place(&var, &Taker::Kept(name)).map_err(|error| Error { span: Some(place.span), ..error })?;
        }
        let Some((region_depth, region_info)) = at(region) else { return Ok(()) };
        let refuse = |why: String| {
            err(
                place.span,
                format!(
                    "this lambda keeps '{0}' as a place, '{1} {0}', so it ends before '{0}' does, and it ends with region '{2}', which {3}; declare a region after '{0}', in its block, and keep the lambda there",
                    name, word, region, why
                ),
            )
        };
        if region_info.param {
            return refuse("is given to the task, so it outlives the task's variables".to_string());
        }
        if region_info.kept {
            return refuse("the lambda around this one keeps what it uses in, so it outlives what is declared inside that lambda".to_string());
        }
        let after = region_depth > depth || (region_depth == depth && (region_info.span.line, region_info.span.col) > (info.span.line, info.span.col));
        if !after {
            return refuse(format!("is declared before '{}', at {}:{}", name, region_info.span.line, region_info.span.col));
        }
        Ok(())
    }

    /// The scope a lambda that keeps what it uses sees under its own: a copy of
    /// each variable of the code around it, which changes only in a
    /// `[flex @r]` lambda, and the region it keeps them in. `None` for one whose
    /// value was never checked.
    fn kept_scope(&self, lambda: &str) -> Option<Scope> {
        let kept = self.kept_lambdas.get(lambda)?;
        let mut around = Scope::default();
        for (name, info) in &kept.outer {
            let mut info = info.clone();
            info.kept = true;
            info.param = false;
            info.passing = Passing::Copy;
            info.refers = None;
            if info.ty == Type::Region {
                // The region it keeps them in, which it checked allocates; a
                // region of a place may be one the task only names.
                info.allocates = info.allocates || *name == kept.region;
            } else if let Some(place) = kept.places.iter().find(|place| place.name == *name) {
                // A place: the variable itself, changed only through `flex&`.
                info.passing = place.passing;
                info.mutable = place.passing == Passing::FlexRef;
            } else {
                info.mutable = kept.flex;
            }
            around.names.insert(name.clone(), info);
        }
        Some(around)
    }

    /// Checks a read of `name`, a variable of the code around the lambda being
    /// checked, which keeps a copy of it, at `span`: a copy of a value of the
    /// region it keeps them in, or of none.
    fn check_kept_use(&self, name: &str, ty: &Type, span: Span) -> Result<()> {
        let Some(kept) = self.current_lambda.as_ref().and_then(|lambda| self.kept_lambdas.get(lambda)) else { return Ok(()) };
        // A place is the variable itself, which outlives the lambda.
        if *ty == Type::Region || kept.places.iter().any(|place| place.name == name) {
            return Ok(());
        }
        if let Some(other) = ty.regions().into_iter().find(|region| *region != kept.region && !unnamed_region(region)) {
            return err(
                span,
                format!(
                    "'{0}' is in region '{1}', and this lambda keeps what it uses in '{2}', where nothing points into '{1}'; copy it into '{2}' first, as in 'fixed mine = {0}.copy() at {2}', and use that, or keep '{0}' as a place, '[fixed& {0}, @{2}]', with '{2}' declared after it",
                    name, other, kept.region
                ),
            );
        }
        if held_lambda(ty) {
            return err(span, format!("'{0}' is a '[flex @r]' lambda, which is never copied, so a lambda can't keep a copy of it; give it as a parameter, 'flex& {0}', or keep it as a place, '[flex& {0}, @s]', in a region declared after it", name));
        }
        Ok(())
    }

    /// Once every body is checked: what each lambda that keeps what it uses
    /// keeps, with what the lambdas written in it keep of the code around
    /// both, as the shape of a block of its region (see `kept_name`).
    fn finish_kept(&mut self) {
        // The inner ones first, so the ones around them keep what they keep.
        for lambda in self.kept_order.clone().iter().rev() {
            let Some(kept) = self.kept_lambdas.get(lambda) else { continue };
            let Some(within) = kept.within.clone() else { continue };
            let passed: Vec<(String, Type)> = kept.uses.iter().filter_map(|name| kept.outer.iter().find(|(outer, info)| outer == name && info.kept).map(|(name, info)| (name.clone(), info.ty.clone()))).collect();
            let span = kept.span;
            let inner = kept.region.clone();
            let mut mistakes = Vec::new();
            if let Some(around) = self.kept_lambdas.get_mut(&within) {
                for (name, ty) in passed {
                    // What the lambda around keeps follows its own rules.
                    let place = around.places.iter().any(|place| place.name == name);
                    if !place && ty != Type::Region {
                        // One the lambda itself refused is told already.
                        let refused = ty.regions().into_iter().any(|region| region != inner && !unnamed_region(region));
                        if let Some(other) = ty.regions().into_iter().find(|region| *region != around.region && !unnamed_region(region)).filter(|_| !refused) {
                            mistakes.push(Error::at(span, format!("this lambda uses '{0}', which the lambda around it keeps for it, and '{0}' is in region '{1}', where that lambda, kept in '{2}', keeps nothing; copy it into '{2}' first, as in 'fixed mine = {0}.copy() at {2}'", name, other, around.region)));
                            continue;
                        }
                        if held_lambda(&ty) && !refused {
                            mistakes.push(Error::at(span, format!("this lambda uses '{}', a '[flex @r]' lambda, which the lambda around it would keep a copy of, and it is never copied", name)));
                            continue;
                        }
                    }
                    if !around.uses.contains(&name) {
                        around.uses.push(name);
                    }
                }
            }
            for mistake in mistakes {
                self.record(mistake);
            }
        }
        for lambda in &self.kept_order {
            let Some(kept) = self.kept_lambdas.get(lambda) else { continue };
            // The region first, which the lambda allocates in where it says so.
            let region_field = |name: &str| Field {
                name: name.to_string(),
                ty: Type::Region,
                span: kept.span,
                ty_span: kept.span,
                private: false,
                read: false,
                default_region: false,
                passing: Passing::Copy,
                default: None,
            };
            let mut fields = vec![region_field(&kept.region)];
            // The regions its places are in, which it may allocate in through them.
            for (name, info) in &kept.outer {
                if info.ty == Type::Region && *name != kept.region && info.allocates {
                    fields.push(region_field(name));
                }
            }
            for (name, info) in &kept.outer {
                if !kept.uses.contains(name) || info.ty == Type::Region {
                    continue;
                }
                // A place is kept as a pointer to the variable (see `Generator::gen_closure`).
                let place = kept.places.iter().find(|place| place.name == *name).map(|place| place.passing);
                fields.push(Field {
                    name: name.clone(),
                    ty: match place {
                        Some(_) => Type::Ptr(Box::new(info.ty.clone()), None),
                        None => info.ty.clone(),
                    },
                    passing: place.unwrap_or(Passing::Copy),
                    span: kept.span,
                    ty_span: kept.span,
                    private: false,
                    read: false,
                    default_region: false,
                    default: None,
                });
            }
            let shape = ShapeDef { name: kept_name(lambda), types: Vec::new(), given: Vec::new(), fields, shared: Vec::new(), span: kept.span, private: true };
            self.made_shapes.push(Item::Shape(shape));
        }
    }

    fn check_invoke(&mut self, callee: &mut Expr, args: &mut [Expr], span: Span) -> Result<Type> {
        self.calling = matches!(callee.kind, ExprKind::Var(_));
        let ty = self.check_expr(callee);
        self.calling = false;
        let ty = ty?;
        if ty.unknown() {
            self.check_each(args);
            return Ok(Type::Unknown);
        }
        let named = match &callee.kind {
            ExprKind::Var(name) => format!("'{}'", name),
            _ => "this value".to_string(),
        };
        // A lambda that keeps what it uses names the region it keeps them in,
        // which its task's type may name too: the call gives it as it is.
        let kept = match &ty {
            Type::Closure(closure) => Some(closure.region.clone()),
            _ => None,
        };
        let task = match &ty {
            Type::Task(task) => task.clone(),
            Type::Closure(closure) => Box::new(closure.task.clone()),
            Type::Maybe(inner) if is_task(inner) => {
                return err(callee.span, format!("{} is a {}, which may be none; extract the task first, as in 'when f is {{g}}' or 'f {{ … }}'", named, ty));
            }
            _ => return err(callee.span, format!("{} is of type {}, not a task, so it can't be called", named, ty)),
        };
        // `at r` after a call is the region of what a named task sends.
        if let Some(at) = region_after(args) {
            return err(at, "a call of a task value names every region it gives in its parentheses, as in 'g(r, 3)'; a region after a call gives a named task the region it takes last");
        }
        if args.len() != task.params.len() {
            if task.params.iter().any(|param| matches!(param, Type::Group(_))) {
                return err(
                    span,
                    format!(
                        "{} is a task of type {}, which takes {} named one by one and a group's values where its type writes the group, as in 'next(args...)', got {}",
                        named,
                        ty,
                        count(task.params.len() - 1, "value"),
                        match args.iter().filter(|arg| !matches!(arg.kind, ExprKind::Spread(_))).count() {
                            given if given == args.len() => count(given, "value"),
                            given => format!("{} and a group's values", count(given, "value")),
                        }
                    ),
                );
            }
            return err(span, format!("{} is a task of type {}, which takes {}, got {}", named, ty, count(task.params.len(), "argument"), args.len()));
        }
        // A region it takes is named as its type names it, which what it sends
        // names too; a place is given as a named task's is.
        let info = TaskInfo {
            types: Vec::new(),
            params: task.params.iter().enumerate().map(|(i, ty)| (task.region(i).map_or_else(|| format!("value {}", i + 1), str::to_string), ty.clone())).collect(),
            passing: (0..task.params.len()).map(|i| task.passing(i)).collect(),
            ret: task.ret.clone(),
            owner: None,
        };
        self.kept_call = kept;
        let sent = self.check_call_args(span, &callee_text(callee), &mut Vec::new(), args, &info);
        self.kept_call = None;
        sent
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
        // rest of this checks as it checks any call; any other call of a
        // method is written on its shape or kind, `X.m(…)`.
        if matches!(expr.kind, ExprKind::MethodCall { .. }) {
            self.check_method_call(expr)?;
            self.on_values.insert(span.place_key());
        }
        let on_a_value = self.on_values.contains(&span.place_key());
        if matches!(expr.kind, ExprKind::Shared { .. }) {
            return self.check_shared_use(expr);
        }
        // `["a": 1]` alone: a map of its own, in the block's default region.
        if matches!(expr.kind, ExprKind::Pairs(_)) {
            let value = std::mem::replace(&mut expr.kind, ExprKind::NoneLit);
            expr.kind = ExprKind::Shortcut { value: Box::new(Expr::new(value, span)), region: None, region_span: span };
        }
        if matches!(expr.kind, ExprKind::Shortcut { .. }) {
            self.expand_shortcut(expr)?;
        }
        if let Some(ty) = self.resolve_map_field(expr)? {
            return Ok(ty);
        }
        if matches!(&expr.kind, ExprKind::Field(_, item) if item.starts_with(SAFE_ITEM)) {
            return self.check_safe_item(expr);
        }
        match &mut expr.kind {
            ExprKind::Spread(inner) => self.check_spread(inner, span),
            ExprKind::IntLit(_) => Ok(Type::Int),
            ExprKind::FloatLit(_) => Ok(Type::Float),
            ExprKind::CharLit(_) => Ok(Type::Char),
            ExprKind::BoolLit(_) => Ok(Type::Bool),
            ExprKind::TextLit(_) => Ok(Type::Text(None)),
            ExprKind::NoneLit => Ok(Type::None),
            // One the call it is written after doesn't take (a variant's, which
            // `settle_call_region` never sees).
            ExprKind::RegionAfter(_) => err(span, "a region is named after a call only when the call takes one last"),
            // One a call gives a `flex&` or `fixed&` parameter is checked with the call's
            // arguments (see `check_argument`).
            ExprKind::Ref(_) => err(
                span,
                "'&' marks a place given to a 'flex&' or 'fixed&' parameter, as in 'f(&a)', named by a reference, as in 'flex& y = &a', or whose address is shown or compared, as in 'show(&a)' or '&a == &b'; anywhere else, write the value itself",
            ),
            ExprKind::Shared { .. } => Err(Error::internal_at(span, "a shared field read as another value")),
            // Checked again, as a method call's value is: it is text.
            ExprKind::TypeOf(_) => Ok(Type::Text(None)),
            ExprKind::Macro { value, .. } => Ok(value.ty.clone().unwrap_or(Type::Unknown)),
            ExprKind::Closure { lambda, region, flex, places } => {
                let (lambda, region, flex, places) = (lambda.clone(), region.clone(), *flex, places.clone());
                self.check_closure(&lambda, &region, flex, &places, span)
            }
            // Only `coerce` makes one, of a value it checked.
            ExprKind::Kept { value, .. } => Ok(value.ty.clone().unwrap_or(Type::Unknown)),
            ExprKind::TaskValue(name) => {
                if is_lambda(name) {
                    let seen = self.scopes.iter().flat_map(|scope| scope.names.keys().cloned()).collect();
                    self.lambda_outer.insert(name.clone(), seen);
                    self.note_lambda_loops(name);
                }
                self.task_value_type(name, span)
            }
            ExprKind::Invoke { callee, args } => self.check_invoke(callee, args, span),
            // A macro whose value has a mistake, which is told once, where it is written.
            ExprKind::Var(name) if self.lookup(name).is_none() && self.sees(name) && self.macros.contains_key(name.as_str()) => Ok(Type::Unknown),
            ExprKind::Var(name) => self.check_var(name, span),
            ExprKind::Field(base, field) if self.group_of(base).is_some() => {
                if field == GROUP_TUPLE_WRITTEN {
                    *field = GROUP_TUPLE.to_string();
                }
                self.check_group_member(base, field, span)
            }
            ExprKind::Field(base, field) if field == GROUP_TUPLE_WRITTEN => {
                let message = match &base.kind {
                    ExprKind::Var(name) => format!("'({0}...)' makes a tuple of a group's values, as '(args...)' does, and '{0}' is no group's values", name),
                    _ => "'(...)' around a value and three dots makes a tuple of a group's values, as '(args...)' does, and this is no group's values".to_string(),
                };
                err(base.span, message)
            }
            ExprKind::Field(base, field) => {
                // `.x` outside a method, named as it is written.
                if !self.called_on_a_value() && matches!(&base.kind, ExprKind::Var(name) if name == THIS) {
                    return err(span, self.not_a_method(&format!("{}{}", MODULE_SEPARATOR, field)));
                }
                let base_ty = self.check_expr(base)?;
                self.field_type(&base_ty, field, span)
            }
            ExprKind::Index(base, index) => {
                let base_ty = self.check_expr(base)?;
                self.check_index(base, &base_ty, index, span)
            }
            ExprKind::Lookup(base, index) => {
                let base_ty = self.check_expr(base)?;
                self.check_lookup(&base_ty, index, span)
            }
            ExprKind::Array(values) => self.check_array(values, span),
            ExprKind::Tuple(values) => self.check_tuple(values),
            ExprKind::Repeat { value, count } => self.check_repeat(value, count, span),
            ExprKind::Call { name, types, args } => self.check_task_call(span, on_a_value, name, types, args),
            ExprKind::Unary(op, e) => self.check_unary(*op, e, span),
            ExprKind::Construct { kind, args, .. } if self.broken.contains(kind.as_str()) => {
                // A kind whose own variants have a mistake holds nothing known
                // (see `broken`).
                self.check_each(args);
                Ok(Type::Unknown)
            }
            ExprKind::Construct { kind, tag, args, types } => self.check_construct(kind, tag, args, types, span),
            ExprKind::New { ty, ty_span, value, region, region_span } => self.check_new(ty, *ty_span, value, region, *region_span),
            // `expand_shortcut` above made it the `New` it stands for.
            ExprKind::Shortcut { .. } | ExprKind::Pairs(_) => Err(Error::internal_at(span, "a shortcut that was not expanded")),
            ExprKind::MapOp { map, op, key, region, region_span } => {
                let (op, region_span) = (*op, *region_span);
                self.check_map_op(map, op, key.as_deref_mut(), region, region_span)
            }
            ExprKind::Deref(e) => self.check_deref(e, span),
            ExprKind::StructInit { name, types, fields } => self.check_struct_init(span, name, types, fields),
            ExprKind::Range(..) => err(span, "a range can only be used in 'each' or to take part of an array or slice, as in 'xs[1..3]'"),
            ExprKind::Fail(error) => self.check_fail(error),
            ExprKind::PlanB { value, error, body } => self.check_plan_b(span, value, error.as_ref(), body),
            // A `match` or a `when` used as a value: its branches give it, and
            // one whose branches had a mistake of their own gives an unknown
            // type, so nothing after it complains twice (see `Type::Unknown`).
            ExprKind::Choice(stmt) => Ok(self.check_branching_value(stmt)?.unwrap_or(Type::Unknown)),
            ExprKind::LoopValue(value) => self.check_loop_value(value, span),
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

    /// Checks `name(args)`, a call of a task or of a shared method;
    /// `on_a_value` when it was written as a method call on a value. Kept
    /// apart from `task_call_setup`, as `check_call_args` is from what it
    /// calls: the checker recurses through calls, so the functions it goes
    /// through keep little on the stack.
    fn check_task_call(&mut self, span: Span, on_a_value: bool, name: &str, types: &mut Vec<TypeArg>, args: &mut Vec<Expr>) -> Result<Type> {
        let (info, group) = match self.task_call_setup(span, on_a_value, name, types, args)? {
            CallSetup::Checked(ty) => return Ok(ty),
            CallSetup::Call(info, group) => (info, group),
        };
        // The values a call gives a group decide how many types it holds.
        self.check_call_args(span, name, types, args, &info).map_err(|error| group_hint(error, group.as_ref()))
    }

    /// What a call of `name` needs before its arguments are checked against
    /// the task's header (see `check_task_call`).
    fn task_call_setup(&mut self, span: Span, on_a_value: bool, name: &str, types: &mut [TypeArg], args: &mut Vec<Expr>) -> Result<CallSetup> {
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
            return Ok(CallSetup::Checked(Type::Nothing));
        }
        // `int(x)` and `float(x)`: a value turned into a number, as
        // `text(x) at r` turns one into text.
        if let Some(into) = number_cast(name) {
            return self.check_number_cast(&into, types, args, span).map(CallSetup::Checked);
        }
        let info = self.tasks.get(name).filter(|_| self.sees(name))
            .ok_or_else(|| self.unknown_task(name, types, span))?
            .clone();
        // `X.m(…)`, a method named on its shape or kind: only a shared
        // one is called that way, and a private one only in its file.
        if let Some(owner) = info.owner.as_ref().filter(|_| !on_a_value)
            && let Some((of, _)) = method_parts(name)
        {
            let written = format!("{}{}{}", unqualified(of), MODULE_SEPARATOR, owner.name);
            if !owner.shared {
                return err(
                    span,
                    format!("'{0}' is called on a value of '{1}', as in 'x.{2}(…)'; only a shared method, written 'shared {2}(…)', is called on '{1}' itself", written, unqualified(of), owner.name),
                );
            }
            if self.private.contains(name) && module_of(of) != self.current_module {
                return err(span, format!("'{}' is a private method of '{}', which only its own file can use", owner.name, unqualified(of)));
            }
        }
        // A task whose header has a mistake takes and sends nothing
        // known, so the call is only checked as far as its arguments
        // are values of their own (see `broken`).
        if self.broken.contains(name) {
            self.check_each(args);
            return Ok(CallSetup::Checked(Type::Unknown));
        }
        // A shared method is named as it is written, `X.new`.
        let callee = match method_parts(name).filter(|_| !on_a_value) {
            Some((of, method)) => format!("method '{}{}{}'", unqualified(of), MODULE_SEPARATOR, method),
            None => format!("task '{}'", name),
        };
        // A task taking a group takes, for this call, one type for each
        // value the call gives it after the others; how many a spread,
        // `args...`, gives, only each copy of the caller knows.
        let spreads = args.iter().any(|arg| matches!(arg.kind, ExprKind::Spread(_)));
        let group = match info.params.last() {
            Some((_, Type::Group(group))) if !spreads => Some((group.clone(), args.len() + 1 - info.params.len().min(args.len() + 1))),
            _ => None,
        };
        let info = match self.group_call_info(&info, args, types, span, &callee)? {
            Some(expanded) => expanded,
            None => info,
        };
        if let Some(spread) = args.iter().position(|arg| matches!(arg.kind, ExprKind::Spread(_))) {
            self.check_spread_given(name, &callee, &info, args, spread)?;
        }
        self.settle_call_region(span, &callee, args, &info.params)?;
        self.fill_defaults(name, args);
        if args.len() != info.params.len() {
            // `f(a < b, c > (d))` reads as one argument, a call of `a`.
            if let Some((name, at)) = args.iter().find_map(|arg| match &arg.kind {
                ExprKind::Call { name, types, .. } if !types.is_empty() && self.lookup(name).is_some() && !self.tasks.contains_key(name.as_str()) => Some((name, arg.span)),
                _ => None,
            }) {
                return err(at, misread_comparison(name));
            }
            return err(span, format!("{} expects {}, got {}", callee, self.expected_args(name, info.params.len(), 0), args.len()));
        }
        Ok(CallSetup::Call(info, group))
    }

    /// Gives a call of `task` that leaves out the last of its parameters the
    /// values they fall back on, written in the task's module, which
    /// `check_args_of` reads them as (see `param_defaults`). A call with a
    /// spread gives as many values as only its copies know, and is filled in
    /// there.
    fn fill_defaults(&self, task: &str, args: &mut Vec<Expr>) {
        let Some(defaults) = self.param_defaults.get(task) else { return };
        if args.iter().any(|arg| matches!(arg.kind, ExprKind::Spread(_))) {
            return;
        }
        while let Some(Some(value)) = defaults.get(args.len()) {
            args.push(value.clone());
        }
    }

    /// The parameters of `task` a call gives, of `all`, without the `skip`
    /// first (a method's value), as an error message counts them: `2
    /// arguments`, or `2 to 4 arguments` where the last fall back on values.
    fn expected_args(&self, task: &str, all: usize, skip: usize) -> String {
        let all = all.saturating_sub(skip);
        let needed = self.param_defaults.get(task).and_then(|defaults| defaults.iter().position(Option::is_some)).map(|first| first.saturating_sub(skip));
        match needed {
            Some(0) => format!("up to {}", count(all, "argument")),
            Some(needed) if needed < all => format!("{} to {} arguments", needed, all),
            _ => count(all, "argument"),
        }
    }

    /// Whether `arg`, given for parameter `i` of `task`, is the value that
    /// parameter falls back on, filled in by `fill_defaults`, which only that
    /// value's own position names.
    fn filled_default(&self, task: &str, i: usize, arg: &Expr) -> bool {
        self.param_defaults.get(task).and_then(|defaults| defaults.get(i)).and_then(Option::as_ref).is_some_and(|value| value.span == arg.span)
    }

    /// Where `name`, a decorator naming no task, is a shared method of the
    /// shape or kind whose method `decorated` is: what to say of it, and how it
    /// is written.
    fn own_shared_method(&self, name: &str, decorated: &str) -> Option<(String, String)> {
        if name.contains(MODULE_SEPARATOR) || self.tasks.contains_key(&self.full_name(name)) {
            return None;
        }
        let (owner, _) = method_parts(decorated)?;
        let info = self.tasks.get(&method_name(owner, name))?;
        // A method of every value wraps another of its type, called on the value.
        if info.owner.as_ref().is_some_and(|owner| !owner.shared) {
            return Some((format!("'{}' is a method of every value of '{}', called on the value the wrapped method is", name, unqualified(owner)), format!(".{}", name)));
        }
        let written = format!("{}.{}", unqualified(owner), name);
        Some((format!("'{}' is a shared method of '{}', named on it everywhere", name, unqualified(owner)), written))
    }

    /// The shape and the field `name` (as `X__on__f`) names, when it is a
    /// shared field of a shape and no method of it.
    fn shared_field<'n>(&self, name: &'n str) -> Option<(&'n str, &'n str)> {
        if self.tasks.contains_key(name) {
            return None;
        }
        method_parts(name).filter(|(owner, field)| self.shapes.get(*owner).is_some_and(|shape| shape.shared.iter().any(|member| member.field.name == *field)))
    }

    /// The mistake in a call of `name`, which names no task this file sees.
    fn unknown_task(&self, name: &str, types: &[TypeArg], span: Span) -> Error {
        // `send (x)` written the old way reads as a call.
        // `X.make(…)` naming no method of X (see `Resolver::shared_method`).
        if let Some((owner, method)) = method_parts(name) {
            if self.shared_field(name).is_some() {
                return Error::at(span, format!("'{0}.{1}' is a shared field, not a method, so it is read as '{0}.{1}', without '(…)'", owner, method));
            }
            let what = if self.kinds.contains_key(owner) { "kind" } else { "shape" };
            let or_variant = if what == "kind" { "variant or " } else { "" };
            return Error::at(span, format!("{} '{}' has no {}shared method '{}'", what, owner, or_variant, method));
        }
        // `area()` inside a method of the shape that has it.
        if let Some(of) = &self.current_type
            && let Some(method) = self.tasks.get(&method_name(of, name)).and_then(|info| info.owner.as_ref())
        {
            let how = match (method.shared, self.called_on_a_value()) {
                (true, _) => format!("'{}.{}(…)'", unqualified(of), name),
                (false, true) => format!("'.{}(…)', or 'x.{}(…)' on another value", name, name),
                (false, false) => format!("'x.{}(…)', on a value of it", name),
            };
            return Error::at(span, format!("'{}' is a method of '{}', so a call names what it is called on, as in {}", name, unqualified(of), how));
        }
        if !types.is_empty() && self.lookup(name).is_some() {
            return Error::at(span, misread_comparison(name));
        }
        let hint = match name {
            "send" => "; a task sends its result with '<-'".to_string(),
            "free" => "; Vela has no 'free': memory in a region is freed when the block declaring the region closes".to_string(),
            _ if self.module_used(name).is_some() => format!("; '{0}' is a module, so name a task in it, as in '{0}.name(…)'", name),
            _ if self.macros.contains_key(name) => "; it is a macro, a value that never changes, so it is used without '()'".to_string(),
            // `(i < n, n > (m))`: a `<` after a name whose `>`
            // is followed by `(` gives types to a call.
            _ => self.elsewhere(name),
        };
        Error::at(span, format!("unknown task '{}'{}", name, hint))

    }

    /// Checks a read of the variable `name`.
    fn check_var(&mut self, name: &str, span: Span) -> Result<Type> {
        let calling = std::mem::take(&mut self.calling);
        let info = self.lookup_or_err(name, span)?;
        let (ty, kept, mutable) = (info.ty.clone(), info.kept, info.mutable);
        if kept {
            self.check_kept_use(name, &ty, span)?;
        }
        // A `[flex @r]` lambda changes what it keeps, so its value is never
        // copied: it is called, or given as a place.
        if held_lambda(&ty) && !calling {
            return err(
                span,
                format!("'{0}' is a '[flex @r]' lambda, which changes what it keeps, so it is never copied: call it, '{0}(…)', or give it as a place, '&{0}', to a 'flex& f: {1}' parameter", name, ty),
            );
        }
        // Calling it changes what it keeps, so it is called through a place
        // that changes.
        if held_lambda(&ty) && !mutable {
            return err(span, format!("'{0}' is a '[flex @r]' lambda, and calling it changes what it keeps, so it is called through a place that changes, a 'flex' variable or a 'flex&', which '{0}' isn't", name));
        }
        if self.unset.contains(name) {
            return err(span, format!("'{}' might not be set here", name));
        }
        // A group's values go on whole, and only to a task's call.
        if let Type::Group(group) = &ty
            && !self.spreading
        {
            return err(span, format!("'{0}' holds the values of the group '{1}...', which go on whole to a task that takes them, as in 'next({0}...)', or are counted, '{0}.len'", name, group));
        }
        Ok(ty)
    }

    /// Checks `Kind.Tag(args)`, a variant made with its values.
    fn check_construct(&mut self, kind: &str, tag: &str, args: &mut [Expr], written: &mut [TypeArg], span: Span) -> Result<Type> {
        let def = self.kinds.get(kind).cloned().ok_or_else(|| Error::internal_at(span, format!("unknown kind '{}'", kind)))?;
        let variant = def.variants.iter().find(|v| v.tag == *tag).cloned().ok_or_else(|| Error::internal_at(span, format!("unknown variant '{}'", tag)))?;
        if let Some(at) = region_after(args) {
            return err(at, format!("variant '{}' makes a value, not memory, so no region is named after it", tag));
        }
        if args.len() != variant.payload.len() {
            return err(span, format!("variant '{}' expects {}, got {}", tag, count(variant.payload.len(), "argument"), args.len()));
        }
        if def.types.is_empty() {
            if let Some((_, at)) = written.first() {
                return err(*at, format!("kind '{}' takes no types, so '< … >' has nothing to name; write '{}.{}'", unqualified(kind), unqualified(kind), tag));
            }
            let mut values: Vec<&mut Expr> = args.iter_mut().collect();
            let region = self.check_fields_of_value(&mut values, &variant.payload, None)?;
            return Ok(Type::Named(kind.to_string(), self.region_of_value(kind, region)));
        }
        // A kind that takes types: the ones written, or the ones the values
        // show, and the rest from the place the value goes (see `settle_variant`).
        let (given, hints) = self.variant_types(kind, &def, &variant, written, args)?;
        if def.types.iter().all(|param| given.contains_key(&param.name)) {
            let types: Vec<Type> = def.types.iter().filter_map(|param| given.get(&param.name).cloned()).collect();
            return self.made_variant(kind, tag, &types, args, false, span);
        }
        let untold = def.types.iter().find(|param| !given.contains_key(&param.name)).map(|param| param.name.clone()).unwrap_or_default();
        // The types to suggest: the ones told, and `int` in place of each untold.
        let example: Vec<String> = def
            .types
            .iter()
            .map(|param| given.get(&param.name).or_else(|| hints.get(&param.name)).map_or(Type::Int, |ty| settled_example(&ty.without_regions())).to_string())
            .collect();
        self.untold_kinds.push((span, kind.to_string(), tag.to_string(), untold, example.join(", ")));
        let types = def.types.iter().map(|param| given.get(&param.name).cloned().unwrap_or_else(|| Type::Param(format!("{}{}", UNTOLD, param.name)))).collect();
        Ok(Type::Applied(Box::new(Applied { name: kind.to_string(), types, region: None })))
    }

    /// The types the variant `variant` of `kind` (`def`, which takes types) is
    /// given: all the ones `written` names, which are all of them or none, or
    /// the ones its values `args` show, which may be fewer.
    /// Also the types its values show that hold one untold, which tell
    /// nothing, for a message to suggest.
    fn variant_types(&mut self, kind: &str, def: &KindDef, variant: &Variant, written: &mut [TypeArg], args: &mut [Expr]) -> Result<(HashMap<String, Type>, HashMap<String, Type>)> {
        let mut given: HashMap<String, Type> = HashMap::new();
        if let Some((_, at)) = written.first() {
            if written.len() != def.types.len() {
                let names: Vec<&str> = def.types.iter().map(|param| param.name.as_str()).collect();
                return err(
                    *at,
                    format!(
                        "kind '{}' takes {}, so name {}, as in '{}<{}>.{}'",
                        unqualified(kind),
                        count(def.types.len(), "type"),
                        if def.types.len() == 1 { "it" } else { "all of them" },
                        unqualified(kind),
                        names.join(", "),
                        variant.tag
                    ),
                );
            }
            for ((ty, at), param) in written.iter_mut().zip(&def.types) {
                // The types a kind is given name no region, as its payloads
                // don't: the region comes from the values.
                *ty = self.resolve_type(ty, *at, TypePlace::Field)?;
                self.check_need("kind", kind, param, ty, *at)?;
                given.insert(param.name.clone(), ty.clone());
            }
            return Ok((given, HashMap::new()));
        }
        // The values say which types they are. They are checked again with the
        // kind made for them, so what this check left to settle is left to that one.
        let copies = self.unsettled_copies.len();
        let untold = self.unsettled_types.len();
        let kinds = self.untold_kinds.len();
        let mut found = Ok(());
        // `none` tells no type either, but a message can suggest a maybe.
        let mut unsettled_values = Vec::new();
        for (arg, field) in args.iter_mut().zip(&variant.payload) {
            match self.check_expr(arg) {
                Ok(ty) if unsettled(&ty) && matches!(field.ty, Type::Param(_)) => unsettled_values.push((field.ty.to_string(), ty)),
                Ok(ty) => self.match_params(&field.ty, &ty, &mut given),
                Err(error) => {
                    found = Err(error);
                    break;
                }
            }
        }
        self.unsettled_copies.truncate(copies);
        self.unsettled_types.truncate(untold);
        self.untold_kinds.truncate(kinds);
        found?;
        // A value that is itself a variant with types untold, as in
        // `Option.Some(Option.None)`, tells nothing either.
        let mut hints: HashMap<String, Type> = unsettled_values.into_iter().collect();
        given.retain(|param, ty| {
            let told = !ty.params().iter().any(|name| name.starts_with(UNTOLD));
            if !told {
                hints.insert(param.clone(), ty.clone());
            }
            told
        });
        for param in &def.types {
            if let Some(ty) = given.get(&param.name) {
                self.check_need("kind", kind, param, ty, args.first().map_or(variant.span, |arg| arg.span))?;
            }
        }
        Ok((given, hints))
    }

    /// The value of the variant `tag` of `kind` given `types`, all known, with
    /// its values `args`: of the kind made for them, or, where a type this
    /// declaration takes is among them, of the kind given them, which each use
    /// makes (see `settle_applications`). Values already `checked` are not
    /// checked again.
    fn made_variant(&mut self, kind: &str, tag: &str, types: &[Type], args: &mut [Expr], checked: bool, span: Span) -> Result<Type> {
        let def = self.kinds.get(kind).cloned().ok_or_else(|| Error::internal_at(span, format!("unknown kind '{}'", kind)))?;
        let payload = |def: &KindDef| def.variants.iter().find(|v| v.tag == *tag).map(|variant| variant.payload.clone()).unwrap_or_default();
        if types.iter().any(|ty| !ty.params().is_empty()) {
            let names: HashMap<&str, &Type> = def.types.iter().map(|param| param.name.as_str()).zip(types).collect();
            let payload: Vec<Field> = payload(&def).into_iter().map(|field| Field { ty: field.ty.map_params(&mut |param| names.get(param).map(|ty| (*ty).clone())), ..field }).collect();
            let mut values: Vec<&mut Expr> = args.iter_mut().collect();
            let region = if checked { self.fields_region(&mut values, &payload)? } else { self.check_fields_of_value(&mut values, &payload, None)? };
            let may_hold = self.takes_region.contains(kind) || def.variants.iter().flat_map(|variant| &variant.payload).any(|field| !field.ty.params().is_empty());
            return Ok(Type::Applied(Box::new(Applied { name: kind.to_string(), types: types.to_vec(), region: may_hold.then_some(region) })));
        }
        // The kind made for the types, whose payloads name the kinds and shapes
        // made for theirs.
        let key = self.kind_instance(kind, types, span)?;
        let made = self.kinds.get(&key).cloned().ok_or_else(|| Error::internal_at(span, format!("no kind made for '{}'", kind)))?;
        let mut values: Vec<&mut Expr> = args.iter_mut().collect();
        let region = if checked { self.fields_region(&mut values, &payload(&made))? } else { self.check_fields_of_value(&mut values, &payload(&made), None)? };
        Ok(Type::Named(key.clone(), self.region_of_value(&key, region)))
    }

    /// A variant whose types its values didn't tell, `Option.None`, going where
    /// a value of `expected` is: the types the place names, and the value of
    /// the kind made for them. One whose place names none of its kind is left
    /// as it is, which the statement reports (see `untold_kinds`).
    fn settle_variant(&mut self, expr: &mut Expr, expected: &Type) -> Result<()> {
        // `fail Option.None` where a `T ~ Option<int>` goes: the failure's
        // value goes where the error does.
        if let (ExprKind::Fail(inner), Type::Or(_, error) | Type::Fail(error)) = (&mut expr.kind, expected)
            && inner.ty.as_ref().is_some_and(untold_kind)
        {
            self.settle_variant(inner, error)?;
            expr.ty = inner.ty.clone().map(|ty| Type::Fail(Box::new(ty)));
            return Ok(());
        }
        // A branch's `none` beside a branch's `Option.None`.
        let none = match &expr.kind {
            ExprKind::Wrap(inner) => matches!(inner.kind, ExprKind::NoneLit),
            kind => matches!(kind, ExprKind::NoneLit),
        };
        if none
            && expr.ty.as_ref().is_some_and(holds_untold)
            && matches!(expected, Type::Maybe(_))
            && !holds_untold(expected)
        {
            expr.ty = Some(expected.clone());
            return Ok(());
        }
        // A branch's value made part of the `T ~ E` the branches give, whose
        // error a branch's `fail Option.None` gave untold.
        if let ExprKind::Wrap(inner) = &mut expr.kind
            && let Some(Type::Or(value, error)) = &expr.ty
            && untold_kind(error)
            && let Type::Or(_, told) = expected
            && !holds_untold(told)
        {
            let own = Type::Or(value.clone(), told.clone());
            self.settle_variant(inner, &own)?;
            expr.ty = Some(own);
            return Ok(());
        }
        // A branch's `Option.None` made part of the maybe the branches give.
        if let ExprKind::Wrap(inner) = &mut expr.kind
            && inner.ty.as_ref().is_some_and(untold_kind)
        {
            self.settle_variant(inner, expected)?;
            if let Some(told) = inner.ty.clone() {
                expr.ty = match expr.ty.take() {
                    Some(Type::Maybe(_)) => Some(Type::Maybe(Box::new(told))),
                    Some(Type::Or(_, error)) => Some(Type::Or(Box::new(told), error)),
                    other => other,
                };
            }
            return Ok(());
        }
        // A loop going where an `int ~ LoopResult<int, text>` goes: the place
        // tells the `B` and `M` nothing in it told, and the variant its last
        // lines give whose types they didn't, `Option.None`.
        if let ExprKind::LoopValue(value) = &mut expr.kind
            && value.gives
            && let Some(Type::Or(given, result)) = expr.ty.clone()
            && let Type::Or(place, place_error) = expected
        {
            let told_given = holds_untold(&given) && !holds_untold(place);
            let told_result = match (self.unmade(&result), self.unmade(place_error)) {
                (Type::Applied(mine), Type::Applied(placed)) if value.open != (false, false) && mine.name == library::LOOP_RESULT && placed.name == mine.name => {
                    let pick = |i: usize, open: bool| if open { placed.types.get(i) } else { mine.types.get(i) }.cloned().unwrap_or(Type::Bool);
                    let types = vec![pick(0, value.open.0), pick(1, value.open.1)];
                    let region = mine.region.clone().filter(|region| region != ANY_REGION).or_else(|| placed.region.clone());
                    (types != mine.types).then(|| Type::Applied(Box::new(Applied { name: mine.name.clone(), types, region })))
                }
                _ => None,
            };
            if told_given || told_result.is_some() {
                let result = match told_result {
                    Some(told) => self.made_kinds(told, expr.span)?,
                    None => *result,
                };
                let own = Type::Or(if told_given { place.clone() } else { given }, Box::new(result.clone()));
                if let Some(body) = value.body_mut() {
                    // What the last lines were made part of fails with the
                    // loop's new result; then the place tells them the rest.
                    last_values_mut(body, &mut |e| {
                        if let (ExprKind::Wrap(_), Some(Type::Or(value, _))) = (&e.kind, &e.ty) {
                            e.ty = Some(Type::Or(value.clone(), Box::new(result.clone())));
                        }
                    });
                    self.settle_value(body, &own)?;
                }
                expr.ty = Some(own);
            }
            return Ok(());
        }
        // `(Option.None, 1)` where a `(Option<int>, int)` goes: each item is
        // told by its own place.
        if let (ExprKind::Tuple(items), Type::Tuple(places)) = (&mut expr.kind, expected)
            && expr.ty.as_ref().is_some_and(holds_untold)
            && items.len() == places.len()
        {
            for (item, place) in items.iter_mut().zip(places) {
                self.settle_variant(item, place)?;
            }
            if let Some(told) = items.iter().map(|item| item.ty.clone()).collect::<Option<Vec<Type>>>() {
                expr.ty = Some(Type::Tuple(told));
            }
            return Ok(());
        }
        // The variant whose types are untold, or the one a maybe of it holds,
        // as a `when` giving `none` or `Option.None` is, and the failure of one
        // giving `fail Option.None`, which the error of the place tells.
        // The whole then becomes `own`, its value's type kept.
        let choice = matches!(&expr.kind, ExprKind::Choice(_));
        let (sent, place, own) = match expr.ty.clone() {
            Some(Type::Applied(sent)) if sent.types.iter().any(|ty| is_untold(ty) || untold_inside(ty)) => (sent, expected.clone(), expected.clone()),
            Some(Type::Maybe(inner)) if choice && untold_kind(&inner) => match *inner {
                Type::Applied(sent) => (sent, expected.clone(), expected.clone()),
                _ => return Ok(()),
            },
            Some(Type::Or(value, error)) if choice && untold_kind(&error) => match (*error, expected) {
                (Type::Applied(sent), Type::Or(_, place)) => (sent, (**place).clone(), Type::Or(value, place.clone())),
                _ => return Ok(()),
            },
            // `when … none else when … fail Option.None else n fi`, a maybe of a
            // `T ~ E` going where a `maybe T ~ E` goes, branch by branch.
            Some(Type::Maybe(inner)) if choice && matches!(&*inner, Type::Or(_, error) if untold_kind(error)) => match (*inner, expected) {
                (Type::Or(_, error), Type::Or(_, place)) => match *error {
                    Type::Applied(sent) => (sent, (**place).clone(), expected.clone()),
                    _ => return Ok(()),
                },
                _ => return Ok(()),
            },
            _ => return Ok(()),
        };
        let span = expr.span;
        let Some(types) = self.told_types(&sent, &place) else { return Ok(()) };
        // `when c Option.None else Option.None fi`: each branch's value is
        // told by the place the whole goes.
        if let ExprKind::Choice(stmt) = &mut expr.kind {
            self.settle_branch_values(stmt, &own)?;
            expr.ty = Some(own);
            return Ok(());
        }
        if !untold_inside(expr.ty.as_ref().unwrap_or(&Type::Unknown)) {
            return Ok(());
        }
        {
            let def = self.kinds.get(&sent.name).cloned().ok_or_else(|| Error::internal_at(span, "a variant of no kind"))?;
            for (param, ty) in def.types.iter().zip(&types) {
                self.check_need("kind", &sent.name, param, ty, span)?;
            }
            let ExprKind::Construct { tag, args, .. } = &mut expr.kind else { return Ok(()) };
            let tag = tag.clone();
            // A value of it that is a variant nothing told, `Option.Some(Either.Left(1))`,
            // is told by the type of its field.
            if let Some(variant) = def.variants.iter().find(|variant| variant.tag == tag) {
                let names: HashMap<&str, &Type> = def.types.iter().map(|param| param.name.as_str()).zip(&types).collect();
                for (arg, field) in args.iter_mut().zip(&variant.payload) {
                    if arg.ty.as_ref().is_some_and(untold_inside) {
                        let place = field.ty.map_params(&mut |param| names.get(param).map(|ty| (*ty).clone()));
                        self.settle_variant(arg, &place)?;
                    }
                }
            }
            // Its values were checked where they are, whose names may be gone.
            let ty = self.made_variant(&sent.name, &tag, &types, args, true, span)?;
            expr.ty = Some(ty);
            if let Some(at) = self.untold_kinds.iter().rposition(|(at, ..)| *at == span) {
                self.untold_kinds.remove(at);
            }
        }
        Ok(())
    }

    /// The types `sent`, a variant whose types its values didn't all tell,
    /// takes from `expected`, the place it goes: the value itself, or what a
    /// maybe or a `T ~ E` holds, where that is its kind with types that fit.
    fn told_types(&self, sent: &Applied, expected: &Type) -> Option<Vec<Type>> {
        let mut places = vec![expected.clone()];
        match expected {
            Type::Maybe(inner) => places.push((**inner).clone()),
            Type::Or(value, _) => {
                places.push((**value).clone());
                if let Type::Maybe(inner) = value.as_ref() {
                    places.push((**inner).clone());
                }
            }
            _ => {}
        }
        places.into_iter().find_map(|place| {
            let Type::Applied(named) = self.unmade(&place) else { return None };
            if named.name != sent.name || named.types.len() != sent.types.len() {
                return None;
            }
            let told = sent.types.iter().zip(&named.types).all(|(sent, named)| {
                is_untold(sent)
                    || (untold_inside(sent) && self.join_told(sent, &named.without_regions()).is_some())
                    || fits(&sent.without_regions(), &named.without_regions())
                    || fits(&named.without_regions(), &sent.without_regions())
            });
            let types: Vec<Type> = sent.types.iter().zip(&named.types).map(|(sent, named)| if is_untold(sent) || untold_inside(sent) { named.without_regions() } else { sent.clone() }).collect();
            // A place no better told, as a branch of the same `when` is.
            (told && !types.iter().any(|ty| is_untold(ty) || holds_untold(ty))).then_some(types)
        })
    }

    /// The type values of types `a` and `b` can both become (see `join_types`),
    /// where a variant whose types its values didn't tell, `Option.None`, is
    /// told them by a value of the same kind beside it, as `none` is.
    fn join(&self, a: &Type, b: &Type) -> Option<Type> {
        if untold_inside(a) || untold_inside(b) {
            match (a, b) {
                // `none` beside `Option.None`, or a maybe of one beside a value.
                (Type::Maybe(x), Type::Maybe(y)) => return self.join(x, y).map(|joined| Type::Maybe(Box::new(joined))),
                (Type::Maybe(x), Type::None) | (Type::None, Type::Maybe(x)) => return Some(Type::Maybe(x.clone())),
                (Type::Maybe(x), other) | (other, Type::Maybe(x)) if !matches!(other, Type::Or(..) | Type::Fail(_)) => {
                    return self.join(x, other).map(|joined| Type::Maybe(Box::new(joined)));
                }
                _ => {}
            }
            // `Either.Left(1)` beside `Either.Right("x")`, or kinds of them:
            // each tells what it can, and what neither tells stays untold.
            if let (Type::Applied(left), Type::Applied(right)) = (self.unmade(a), self.unmade(b))
                && left.name == right.name
                && left.types.len() == right.types.len()
            {
                let types = left.types.iter().zip(&right.types).map(|(x, y)| self.join_told(x, y)).collect::<Option<Vec<Type>>>()?;
                // One that tells all of them is the type both are, as it is.
                for (side, applied) in [(b, &right), (a, &left)] {
                    if !untold_inside(side) && types.iter().zip(&applied.types).all(|(joined, own)| joined.without_regions() == own.without_regions()) {
                        return Some(side.clone());
                    }
                }
                // Everything a value in a region holds is in that region, so a
                // value holding one is in it.
                let region = left
                    .region
                    .clone()
                    .or_else(|| right.region.clone())
                    .or_else(|| types.iter().flat_map(Type::regions).find(|region| !unnamed_region(region)).map(str::to_string));
                return Some(Type::Applied(Box::new(Applied { name: left.name.clone(), types, region })));
            }
        }
        // A branch's `fail Option.None` beside a failure or a value of the
        // kind: the errors join as values do.
        let failing_untold = |ty: &Type| matches!(ty, Type::Fail(error) | Type::Or(_, error) if untold_kind(error));
        if failing_untold(a) || failing_untold(b) {
            match (a, b) {
                (Type::Fail(x), Type::Fail(y)) => return self.join(x, y).map(|error| Type::Fail(Box::new(error))),
                (Type::Or(value, x), Type::Fail(y)) | (Type::Fail(y), Type::Or(value, x)) => {
                    return self.join(x, y).map(|error| Type::Or(value.clone(), Box::new(error)));
                }
                (Type::Or(v, x), Type::Or(w, y)) => return Some(Type::Or(Box::new(self.join(v, w)?), Box::new(self.join(x, y)?))),
                _ => {}
            }
        }
        join_types(a, b)
    }

    /// The type a kind given types takes for one of them, `x` beside `y`
    /// (see `join`): the one that is told, or the one the other fits.
    fn join_told(&self, x: &Type, y: &Type) -> Option<Type> {
        match (is_untold(x), is_untold(y)) {
            (true, _) => Some(y.clone()),
            (false, true) => Some(x.clone()),
            _ if untold_inside(x) || untold_inside(y) => self.join(x, y),
            _ if fits(&x.without_regions(), &y.without_regions()) => Some(y.clone()),
            _ if fits(&y.without_regions(), &x.without_regions()) => Some(x.clone()),
            _ => None,
        }
    }

    /// The error of the variant at `at` whose types nothing told, if it is
    /// one (see `untold_kinds`).
    fn untold_kind_error(&self, at: Span) -> Option<Error> {
        let (at, kind, tag, param, example) = self.untold_kinds.iter().rev().find(|(span, ..)| *span == at)?;
        Some(Error::at(*at, untold_kind_message(kind, tag, param, example)))
    }

    /// `ty`, a type values beside each other settled on (see `join`), with the
    /// kind made for each kind given types its variants told between them,
    /// `Either<int, text>` for `Either.Left(1)` beside `Either.Right("x")`.
    fn made_kinds(&mut self, ty: Type, at: Span) -> Result<Type> {
        Ok(match ty {
            Type::Applied(applied)
                if applied.types.iter().all(|ty| ty.params().is_empty()) && self.kinds.get(&applied.name).is_some_and(|def| !def.types.is_empty()) =>
            {
                let key = self.kind_instance(&applied.name, &applied.types, at)?;
                let region = applied.region.and_then(|region| self.region_of_value(&key, region));
                Type::Named(key, region)
            }
            Type::Maybe(inner) => Type::Maybe(Box::new(self.made_kinds(*inner, at)?)),
            Type::Or(value, error) => Type::Or(Box::new(self.made_kinds(*value, at)?), Box::new(self.made_kinds(*error, at)?)),
            Type::Array(items, count) => Type::Array(Box::new(self.made_kinds(*items, at)?), count),
            Type::Tuple(items) => Type::Tuple(items.into_iter().map(|item| self.made_kinds(item, at)).collect::<Result<_>>()?),
            other => other,
        })
    }

    /// Checks `new T(value) at r` (see `ExprKind::New`).
    fn check_new(&mut self, ty: &mut Box<Type>, ty_span: Span, value: &mut Expr, region: &mut Option<String>, region_span: Span) -> Result<Type> {
        **ty = self.resolve_type(ty, ty_span, TypePlace::New)?;
        // `^int ~ E` means `(^int) ~ E`, so no written type could hold the pointer.
        if matches!(**ty, Type::Or(..)) {
            return err(ty_span, format!("'new' can't make a pointer to '{}'; extract the value first, or keep it in a shape's field", ty));
        }
        // `new Point` or `new Point()`: the parser wrote the literal of no
        // fields at the type, which only a shape whose fields all fall back on
        // values takes.
        if value.span == ty_span && matches!(&value.kind, ExprKind::StructInit { fields, .. } if fields.is_empty()) {
            self.check_empty_shape(ty, ty_span)?;
        }
        // The region written, or the block's default, which is kept on
        // the node so the code generator sees the region either way.
        let written = ty.to_string();
        let allocates = self.region_or_default(region.as_ref(), region_span, || {
            format!("'new' needs a region to allocate in, as in 'new {0}(…) at r' or 'new@r {0}(…)'", written)
        })?;
        *region = Some(allocates.clone());
        self.visible_region(&allocates, region_span, true)?;
        // What a lambda that keeps what it uses keeps is in its region, which
        // everything a value in `allocates` holds is in.
        if let Some(other) = ty.regions().into_iter().find(|kept| *kept != allocates) {
            return err(
                self.place(ty_span, &format!("@{}", other)),
                format!("everything a value in region '{0}' holds is in '{0}', what a lambda keeps too; write '@{0}' for '@{1}'", allocates, other),
            );
        }
        // `new [K: V](…) at r` makes the map, its table in `r`.
        if let Type::Map(key, value_ty, _) = &**ty {
            let key = self.in_region(key, &allocates);
            let value_ty = self.in_region(value_ty, &allocates);
            let map = Type::Map(Box::new(key.clone()), Box::new(value_ty.clone()), Some(allocates));
            self.check_map_source(value, &key, &value_ty, &map)?;
            return Ok(map);
        }
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

    /// Checks that `ty`, made by a `new` with no value at `span`, is a shape
    /// whose fields all fall back on values, which is the value it makes.
    fn check_empty_shape(&mut self, ty: &Type, span: Span) -> Result<()> {
        let name = match ty {
            Type::Named(name, _) => name.as_str(),
            Type::Applied(applied) => applied.name.as_str(),
            _ => "",
        };
        let Some(shape) = self.shapes.get(name).filter(|_| !self.kinds.contains_key(name)) else {
            return err(
                span,
                format!(
                    "'new' needs the value to put in the region, as in 'new {}(…) at r'; only a slice, a map, or a shape whose fields all fall back on values can be made empty, as in 'new [int] at r' or 'new [text: int] at r'",
                    ty
                ),
            );
        };
        // A private field from another file is the literal's to report: no
        // value written here could give it.
        let outside = module_of(name) != self.current_module;
        if let Some(field) = shape.fields.iter().find(|field| field.default.is_none()).filter(|field| !(field.private && outside)) {
            return err(
                span,
                format!(
                    "a 'new' with no value makes a '{0}' of the values its fields fall back on, and field '{1}' falls back on none; give the value, as in 'new {0}({0} {{ {1}: … }})'",
                    unqualified(name),
                    field.name
                ),
            );
        }
        Ok(())
    }

    /// Checks `p^`, the value a pointer points to.
    fn check_deref(&mut self, e: &mut Expr, span: Span) -> Result<Type> {
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

    /// Checks `fail error`.
    fn check_fail(&mut self, error: &mut Expr) -> Result<Type> {
        let ty = self.check_expr(error)?;
        if matches!(ty, Type::Nothing | Type::None | Type::Fail(_) | Type::Maybe(_) | Type::Or(..)) {
            return err(error.span, format!("'fail' needs an error value, got {}", ty));
        }
        Ok(Type::Fail(Box::new(ty)))
    }

    /// Makes `m.keys` or `m.values` the map operation it is (see `MapOp`).
    /// Gives the field's type when `expr` is a field of another value, read
    /// from its base, which is checked once here.
    fn resolve_map_field(&mut self, expr: &mut Expr) -> Result<Option<Type>> {
        let span = expr.span;
        // `m.keys`: a map's keys made into a slice in the block's default region.
        // The base is checked once: a field of that name on anything else is
        // read from the type found (checking it again would double the work
        // at each `.values` of a chain of them).
        if let ExprKind::Field(base, word) = &mut expr.kind
            && let Some(op @ (MapOp::Keys | MapOp::Values)) = MapOp::from_word(word)
            && (self.called_on_a_value() || !matches!(&base.kind, ExprKind::Var(name) if name == THIS))
        {
            let base_ty = self.check_expr(base)?;
            if !matches!(base_ty, Type::Map(..)) {
                return self.field_type(&base_ty, word, span).map(Some);
            }
            let taken = std::mem::replace(&mut expr.kind, ExprKind::NoneLit);
            if let ExprKind::Field(map, _) = taken {
                expr.kind = ExprKind::MapOp { map, op, key: None, region: None, region_span: span };
            }
        }
        Ok(None)
    }

    /// Checks a tuple literal, `(a, b)`, and returns its type: its items' types.
    fn check_tuple(&mut self, values: &mut [Expr]) -> Result<Type> {
        let mut items = Vec::new();
        for value in values.iter_mut() {
            let ty = self.check_expr(value)?;
            // A variant whose types are untold, `(Option.None, 1)`, gets them
            // from the place the tuple goes, whose type was checked so.
            if let Some(what) = self.changeable_item(&ty).filter(|_| !holds_untold(&ty)) {
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
                Some(before) => self.join(&before, &ty).ok_or_else(|| {
                    Error::at(value.span, format!("an array's values have one type, but this one is {} and the ones before it are {}", ty, before))
                })?,
            });
        }
        let items = self.made_kinds(joined.unwrap_or(Type::Nothing), span)?;
        if items == Type::Nothing {
            return err(span, "an array can't hold 'nothing', which has no value");
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

    /// Checks `[value#count]` outside `new`, an array, and returns its type.
    fn check_repeat(&mut self, value: &mut Expr, count: &mut Expr, span: Span) -> Result<Type> {
        let items = self.check_expr(value)?;
        if items == Type::Nothing {
            return err(value.span, "an array can't hold 'nothing', which has no value");
        }
        let count_ty = self.check_expr(count)?;
        self.expect_type(&count_ty, &Type::Int, count.span)?;
        let Some(n) = constant_int(count) else {
            // The region of a slice's items is the one it is made in.
            let written = if unsettled(&items) { "T".to_string() } else { items.without_regions().to_string() };
            return err(
                count.span,
                format!("an array's count is known before the program runs, as in '[0#8]'; for a count known only as it runs, make a slice in a region, as in 'new [{}]([…#n]) at r'", written),
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
            // `t[i]`: the byte at `i`, as a char.
            let ty = self.check_expr(index)?;
            self.expect_type(&ty, &Type::Int, index.span)?;
            if let Some(value) = constant_int(index).filter(|value| *value < 0) {
                return err(index.span, format!("index {} is out of range: it is less than 0", value));
            }
            let _ = span;
            return Ok(Type::Char);
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

    /// Makes `[1, 2, ...] at r` the `new [int]([1, 2]) at r` it stands for,
    /// with the items' type worked out from the values, which are checked here
    /// for it and again by the `New`.
    fn expand_shortcut(&mut self, expr: &mut Expr) -> Result<()> {
        let span = expr.span;
        if matches!(&expr.kind, ExprKind::Shortcut { value, .. } if matches!(value.kind, ExprKind::Pairs(_))) {
            return self.expand_map_shortcut(expr);
        }
        let ExprKind::Shortcut { value, .. } = &mut expr.kind else { return Ok(()) };
        let copies = self.unsettled_copies.len();
        let found = self.check_expr(value);
        self.unsettled_copies.truncate(copies);
        let items = match found? {
            Type::Array(items, _) => *items,
            // What went wrong is reported already (see `Type::Unknown`).
            _ => Type::Unknown,
        };
        if matches!(items, Type::None | Type::Fail(_)) {
            return err(span, "can't tell the type of a slice's items from none or 'fail …'; name it, as in 'new [maybe int]([none]) at r'");
        }
        let taken = std::mem::replace(&mut expr.kind, ExprKind::NoneLit);
        let ExprKind::Shortcut { value, region, region_span } = taken else {
            return Err(Error::internal_at(span, "a shortcut that is no longer one"));
        };
        let region = self.region_or_default(region.as_ref(), region_span, || {
            "a slice written by its values goes in a region, as in '[1, 2, ...] at r' or '[1, 2, ...]@r'".to_string()
        })?;
        let region = Some(region);
        let ty = Type::Slice(Box::new(items.without_regions()), None);
        expr.kind = ExprKind::New { ty: Box::new(ty), ty_span: span, value, region, region_span };
        Ok(())
    }

    /// Checks what `each` goes through, and returns the type of the name it
    /// gives each pass, and of the second name's value for a map: a range's
    /// numbers, an array's or a slice's items (`&xs` for the place as it is at
    /// each pass), a map's keys and values (`each k, v in m`), or one side of a
    /// map, `m.keys` or `m.values`, gone through without making a slice.
    fn check_each_source(&mut self, name: &str, second: Option<&(String, Span)>, range: &mut Expr) -> Result<(Type, Option<Type>)> {
        // `each v in args`: each value of a group, of its own type, `P` (see
        // `each_of_group`).
        if let Some(group) = self.group_of(range) {
            range.ty = Some(Type::Group(group.clone()));
            if let Some((second, at)) = second {
                return err(*at, format!("'each {}, {}' names a key and its value, which only a map gives; a group gives one value at a time, so name one", name, second));
            }
            return Ok((Type::Param(group), None));
        }
        let one = |ty: Type| -> Result<(Type, Option<Type>)> {
            match second {
                Some((second, at)) => err(
                    *at,
                    format!("'each {}, {}' names a key and its value, which only a map gives; this gives one item at a time, so name one", name, second),
                ),
                None => Ok((ty, None)),
            }
        };
        let map_pair = |key: Type, value: Type, span: Span| -> Result<(Type, Option<Type>)> {
            match second {
                Some(_) => Ok((key, Some(value))),
                None => err(
                    span,
                    format!("a map gives a key and its value each pass, so name both, as in 'each {}, value in …'; for its keys alone, go through '.keys', as in 'each {} in m.keys'", name, name),
                ),
            }
        };
        // A range is only valid here, so it is checked here rather than as an
        // ordinary expression.
        if let ExprKind::Range(start, end) = &mut range.kind {
            for bound in [start, end] {
                let bty = self.check_expr(bound)?;
                self.expect_type(&bty, &Type::Int, bound.span)?;
            }
            range.ty = Some(Type::Range(Box::new(Type::Int)));
            return one(Type::Int);
        }
        // `each k in m.keys`: one side of a map, gone through where it is.
        if let ExprKind::Field(base, word) = &mut range.kind
            && let Some(op @ (MapOp::Keys | MapOp::Values)) = MapOp::from_word(word)
            && let Type::Map(key, value, region) = self.check_expr(base)?
        {
            let item = if op == MapOp::Keys { *key } else { *value };
            let taken = std::mem::replace(&mut range.kind, ExprKind::NoneLit);
            if let ExprKind::Field(map, _) = taken {
                range.kind = ExprKind::MapOp { map, op, key: None, region: None, region_span: range.span };
            }
            range.ty = Some(Type::Slice(Box::new(item.clone()), region));
            return one(item);
        }
        if let ExprKind::Ref(place) = &mut range.kind {
            // `each v in &xs`: the items of the place as they are at each pass,
            // read through it, so the loop sees every change to it.
            let ty = self.check_expr(place)?;
            if !ty.unknown() && !self.is_place(place) {
                let what = no_place(place);
                return err(place.span, format!("'each v in &…' goes through a place as it is at each pass: a variable, a field or an item of one, or what a pointer points to, and this is {}; write 'each v in …' to go through a value", what));
            }
            range.ty = Some(ty.clone());
            return match ty {
                Type::Array(items, _) | Type::Slice(items, _) => one(*items),
                Type::Map(key, value, _) => map_pair(*key, *value, range.span),
                other if other.unknown() => Ok((Type::Unknown, second.map(|_| Type::Unknown))),
                other => err(range.span, format!("'each v in &…' goes through the items of an array, a slice, or a map as they are at each pass, but this value is {}", other)),
            };
        }
        match self.check_expr(range)? {
            Type::Array(items, _) | Type::Slice(items, _) if unsettled(&items) => {
                err(range.span, format!("cannot tell the type of '{}' from none or 'fail …'; give the array a type first", name))
            }
            Type::Array(items, _) | Type::Slice(items, _) => one(*items),
            Type::Map(key, value, _) => map_pair(*key, *value, range.span),
            // What it goes through couldn't be worked out, so what each item is
            // can't be either; the body is still checked with the names declared
            // (see `Type::Unknown`).
            other if other.unknown() => Ok((Type::Unknown, second.map(|_| Type::Unknown))),
            other => err(
                range.span,
                format!("each goes through a range, such as 0..n, the items of an array or slice, or a map's keys and values, but this value is {}{}", other, extract_hint(&[&other])),
            ),
        }
    }

    /// Makes `["a": 1] at r` the `new [text: int](["a": 1]) at r` it stands for,
    /// with the keys' and the values' types worked out from the pairs, as the
    /// items of two arrays would be.
    fn expand_map_shortcut(&mut self, expr: &mut Expr) -> Result<()> {
        let span = expr.span;
        let ExprKind::Shortcut { value, .. } = &mut expr.kind else { return Ok(()) };
        let ExprKind::Pairs(pairs) = &value.kind else { return Ok(()) };
        let side = |pick: fn(&(Expr, Expr)) -> &Expr| Expr::new(ExprKind::Array(pairs.iter().map(|pair| pick(pair).clone()).collect()), span);
        let (mut keys, mut values) = (side(|pair| &pair.0), side(|pair| &pair.1));
        let copies = self.unsettled_copies.len();
        let found = self.check_expr(&mut keys).and_then(|keys| Ok((keys, self.check_expr(&mut values)?)));
        self.unsettled_copies.truncate(copies);
        let (key_ty, value_ty) = match found? {
            (Type::Array(key, _), Type::Array(value, _)) => (*key, *value),
            _ => (Type::Unknown, Type::Unknown),
        };
        for (ty, what) in [(&key_ty, "keys"), (&value_ty, "values")] {
            if matches!(ty, Type::None | Type::Fail(_)) {
                return err(span, format!("can't tell the type of a map's {} from none or 'fail …'; name it, as in 'new [text: maybe int]([\"a\": none]) at r'", what));
            }
        }
        let taken = std::mem::replace(&mut expr.kind, ExprKind::NoneLit);
        let ExprKind::Shortcut { value, region, region_span } = taken else {
            return Err(Error::internal_at(span, "a shortcut that is no longer one"));
        };
        let region = self.region_or_default(region.as_ref(), region_span, || {
            "a map written by its pairs goes in a region, as in '[\"a\": 1] at r' or '[\"a\": 1]@r'".to_string()
        })?;
        let ty = Type::Map(Box::new(key_ty.without_regions()), Box::new(value_ty.without_regions()), None);
        expr.kind = ExprKind::New { ty: Box::new(ty), ty_span: span, value, region: Some(region), region_span };
        Ok(())
    }

    /// Checks what a new map is made from: its pairs, each key and value going
    /// into the map as a value goes into its place, or another map, copied.
    fn check_map_source(&mut self, value: &mut Expr, key: &Type, value_ty: &Type, map: &Type) -> Result<()> {
        let ExprKind::Pairs(pairs) = &mut value.kind else {
            let ty = self.check_expr(value)?;
            // Another map's keys and values are copied into the new one's
            // region, as `a.copy() at r` copies them, so theirs may be any.
            return match &ty {
                Type::Map(k, v, _) if k.without_regions() == key.without_regions() && v.without_regions() == value_ty.without_regions() => Ok(()),
                _ if ty.unknown() => Ok(()),
                Type::Map(..) => err(value.span, format!("a new {} copies the keys and values of a map of the same types, but this map is {}", map.without_regions(), ty.without_regions())),
                _ => err(value.span, format!("a new map is made from its pairs, as in 'new {}([k: v])', or from another map it copies, but this value is {}", map.without_regions(), ty)),
            };
        };
        let mut written: HashMap<String, Span> = HashMap::new();
        for (k, v) in pairs.iter_mut() {
            self.check_expr(k)?;
            self.coerce(k, key)?;
            self.check_expr(v)?;
            self.coerce(v, value_ty)?;
            // The same key written out twice: the second would replace the first.
            if let Some(text) = constant_key(k) {
                if let Some(at) = written.get(&text) {
                    return err(k.span, format!("the key {} is written twice, first at {}:{}; a map holds one value for each key", text, at.line, at.col));
                }
                written.insert(text, k.span);
            }
        }
        value.ty = Some(map.clone());
        Ok(())
    }

    /// Checks `m.has(k)`, and `m.keys` and `m.values` made into a slice in
    /// `region` (the block's default when none is written), and returns the
    /// type: a bool, or the slice.
    fn check_map_op(&mut self, map: &mut Expr, op: MapOp, key: Option<&mut Expr>, region: &mut Option<String>, region_span: Span) -> Result<Type> {
        let ty = self.check_expr(map)?;
        let Type::Map(key_ty, value_ty, _) = &ty else {
            if ty.unknown() {
                return Ok(Type::Unknown);
            }
            return err(map.span, format!("'{}' asks a map, but this value is {}{}", op.word(), ty, extract_hint(&[&ty])));
        };
        if op == MapOp::Has {
            let Some(key) = key else { return Err(Error::internal_at(map.span, "'has' without its key")) };
            self.check_key(key_ty, key)?;
            return Ok(Type::Bool);
        }
        let item = if op == MapOp::Keys { (**key_ty).clone() } else { (**value_ty).clone() };
        let word = op.word();
        let made = self.region_or_default(region.as_ref(), region_span, || {
            format!("'{0}' makes a slice, so it names the region it goes in, as in 'm.{0} at r' or 'm.{0}@r'; 'each k in m.{0}' goes through them without one", word)
        })?;
        self.visible_region(&made, region_span, true)?;
        *region = Some(made.clone());
        // The slice holds the map's keys or values themselves, not copies:
        // text in them is where the map's is, so the slice goes there too, as
        // a slice's items are in its region.
        if let Some(held) = item.regions().into_iter().find(|held| *held != made && *held != ANY_REGION) {
            let side = if op == MapOp::Keys { "keys" } else { "values" };
            return err(
                region_span,
                format!("the map's {0} hold what is in region '{1}', and 'm.{2}' holds them, not copies, so it goes there too, as in 'm.{2} at {1}'; to keep them in '{3}', copy that, as in '(m.{2} at {1}).copy() at {3}'", side, held, word, made),
            );
        }
        Ok(Type::Slice(Box::new(item), Some(made)))
    }

    /// Checks `key`, a key looked for in a map whose keys are of type `wanted`:
    /// any key of that type, in any region, since looking only compares (a
    /// number written out becomes a unit or the like, as elsewhere). A range
    /// takes a part, which a map has none of.
    fn check_key(&mut self, wanted: &Type, key: &mut Expr) -> Result<()> {
        if matches!(key.kind, ExprKind::Range(..)) {
            return err(key.span, "a map has no parts: '[…]' takes one key, as in 'm[k]'");
        }
        let ty = self.check_expr(key)?;
        if ty.unknown() || wanted.unknown() || ty.without_regions() == wanted.without_regions() {
            return Ok(());
        }
        self.coerce(key, wanted).map_err(|error| Error { message: error.message.replacen("expected type", "expected a key of type", 1), ..error })
    }

    /// Checks `xs[#i]` or `t.at(i)` on a value of type `base_ty`, already
    /// checked, and returns its type: a maybe of the item, none out of range.
    fn check_lookup(&mut self, base_ty: &Type, index: &mut Expr, span: Span) -> Result<Type> {
        if let Type::Map(key, value, _) = base_ty {
            self.check_key(key, index)?;
            return Ok(Type::Maybe(value.clone()));
        }
        let ty = self.check_expr(index)?;
        if base_ty.unknown() {
            return Ok(Type::Unknown);
        }
        self.expect_type(&ty, &Type::Int, index.span)?;
        let item = match base_ty {
            Type::Text(_) => Type::Char,
            Type::Array(items, _) if matches!(**items, Type::None | Type::Fail(_)) => {
                return err(span, "cannot tell the type of this item from none or 'fail …'; give the array a type first");
            }
            Type::Array(items, _) | Type::Slice(items, _) => (**items).clone(),
            Type::Ptr(inner, _) if matches!(**inner, Type::Array(..)) => {
                return err(span, format!("this value is a pointer, {}; look into the array it points to, as in 'p^[#i]'", base_ty));
            }
            Type::Map(..) => return Err(Error::internal_at(span, "a map's lookup read as an item's")),
            other => {
                return err(span, format!("'[#…]' reads an item of an array, a slice, or text, but this value is {}{}", other, extract_hint(&[other])));
            }
        };
        Ok(Type::Maybe(Box::new(item)))
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
        if let Type::Map(key, value, _) = base_ty {
            self.check_key(key, index)?;
            return Ok((**value).clone());
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
        // A map's table is packed again in place as keys are taken out, so a
        // part of an array in it would come to point at another key's value.
        if !matches!(base_ty, Type::Slice(..)) && in_map_value(base) {
            return err(
                span,
                format!(
                    "a slice can't point into a value in a map, which moves when the map grows or is packed; copy the array into a slice first, as in 'new [{}](m[k]) at r'",
                    items.without_regions()
                ),
            );
        }
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
    /// and returns its type: `nothing` for `add`, the item taken out for `del`.
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
                // The lambda's copy of a slice of the code around it shares its
                // items, not its length.
                if info.kept && info.passing == Passing::Copy && info.mutable && matches!(info.ty, Type::Slice(..)) {
                    return err(
                        items.span,
                        format!("'{0}' changes '{1}', the copy this lambda keeps of a slice of the code around it, whose length only the slice itself changes; make the slice inside the lambda, take it as a parameter, 'flex& {1}', or keep the slice as a place, '[flex& {1}, @s]', in a region declared after it", word, name),
                    );
                }
                if !info.mutable && matches!(info.ty, Type::Slice(..) | Type::Map(..)) {
                    if info.param && info.passing == Passing::FixedRef {
                        return err(
                            items.span,
                            format!("'{0}' changes '{1}', a 'fixed&' parameter, which only reads the caller's place; one that changes it is written 'flex& {1}: {2}'", word, name, info.ty),
                        );
                    }
                    if info.param || info.refers.is_some() {
                        let what = if info.param { "a parameter, which never changes" } else { "a 'fixed&'" };
                        return err(items.span, format!("'{}' changes '{}', {}{}", word, name, what, self.param_hint(&name, info)));
                    }
                    let what = if matches!(info.ty, Type::Map(..)) { "map" } else { "slice" };
                    return err(items.span, format!("'{}' changes '{}', which is immutable; keep the {} in a 'flex' variable to change it", word, name, what));
                }
                self.check_expr(items)?
            }
            ExprKind::Index(_, index) if matches!(index.kind, ExprKind::Range(..)) => {
                self.check_expr(items)?;
                return err(
                    items.span,
                    format!("'{}' changes the slice where it is kept, so it can't change a part taken of one; keep the part in a variable first, as in 'flex part = xs[a..b]'", word),
                );
            }
            // The place is checked as an assignment's is: the slice is written.
            ExprKind::Field(..) | ExprKind::Deref(_) | ExprKind::Index(..) => self.check_place_target(items, false).map_err(|mut error| {
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
        if let Type::Map(key, value, _) = &ty {
            return match (op, &mut *args) {
                (EditOp::Del, [k]) => {
                    self.check_key(key, k)?;
                    Ok(Type::Maybe(value.clone()))
                }
                (EditOp::Del, _) => err(span, format!("'del' takes the key to take out of the map, as in 'm.del(k)', got {}", count(args.len(), "argument"))),
                (EditOp::Add, _) => err(span, "a map adds a key, or gives it a new value, with 'm[k] = v'"),
            };
        }
        let (item, region) = match &ty {
            Type::Slice(item, region) => ((**item).clone(), region.clone()),
            // Whether this is a slice at all depends on a type the checker
            // couldn't work out; the arguments are still read (`Type::Unknown`).
            _ if ty.unknown() => {
                self.check_each(args);
                return Ok(Type::Nothing);
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
        // The slice changes, so it may not take what a fixed name owns.
        self.hand(value, HandedTo::Place(self.value_text(items)));
        // Adding may need more room, which comes from the slice's region.
        match region.as_deref() {
            Some(region) if region != ANY_REGION => match self.lookup(region) {
                Some(info) if info.ty == Type::Region && !info.allocates && region == THIS_WORD && self.called_on_a_value() => {
                    err(span, format!("adding to a slice may need more room in its region, and {}", self.cannot_allocate_in_this("add to it where the value is kept, outside the method")))
                }
                Some(info) if info.ty == Type::Region && !info.allocates => err(
                    span,
                    format!("adding to a slice may need more room in its region '{0}', which this task only names in its types; take it as a parameter, '{0}: region'", region),
                ),
                _ => self.visible_region(region, span, true).map(|()| Type::Nothing),
            },
            _ => err(
                span,
                format!(
                    "adding to a slice may need more room in its region, but which region this slice is in isn't known here; keep it in a variable whose type names the region, as in 'flex xs: {}@r = …'",
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
        // `show(&x)`: the address of the place.
        if matches!(arg.kind, ExprKind::Ref(_)) {
            return self.check_address(arg).map(|_| ());
        }
        // `show(args...)`: each value of a group, which asks its types to be shown.
        if matches!(arg.kind, ExprKind::Spread(_)) {
            return match self.check_expr(arg)? {
                Type::Group(group) if !self.allows(Need::Shown, &Type::Group(group.clone())) => err(
                    arg.span,
                    format!("show writes each value of the group '{0}...', so the group asks each of its types to be shown: '<{0}...: shown>'", group),
                ),
                _ => Ok(()),
            };
        }
        let copies = self.unsettled_copies.len();
        let types = self.unsettled_types.len();
        let ty = self.check_expr(arg)?;
        if ty.unknown() {
            return Ok(());
        }
        // `t.#5` past the tuple's end: none, written as any maybe's is.
        if past_end(arg) {
            return self.coerce(arg, &Type::Maybe(Box::new(Type::Bool)));
        }
        // A shape or kind says how its values are written with its own `str`,
        // which `show` calls in a region of its own. The value is checked again
        // as part of that call, as a method call checks it twice.
        if self.has_own_str(&ty) {
            self.unsettled_copies.truncate(copies);
            self.unsettled_types.truncate(types);
            return self.show_with_str(arg);
        }
        if self.writable(&ty) {
            self.note_written_strs(&ty, arg.span);
            return Ok(());
        }
        if let Some(untold) = self.untold_param(&ty) {
            return err(arg.span, untold);
        }
        // A shape or kind can say how it is written itself.
        let own = match self.owner_of(&ty).filter(|owner| self.shapes.contains_key(owner) || self.kinds.contains_key(owner)) {
            Some(owner) => format!(
                "; or give {} '{}' its own method 'str(r: region) -> text@r'",
                if self.kinds.contains_key(&owner) { "kind" } else { "shape" },
                unqualified(&owner)
            ),
            None => String::new(),
        };
        err(arg.span, format!("show can't write {}{}", self.why_unwritable(&ty), own))
    }

    /// Why values of type `ty` can't be written as text, for the end of a
    /// message: the type, the part of it that can't, and what to do instead.
    fn why_unwritable(&self, ty: &Type) -> String {
        let part = self.unwritable_part(ty).unwrap_or_else(|| ty.clone());
        let holds = if part == *ty { String::new() } else { format!(", since it holds {}", part) };
        let hint = match &part {
            Type::Param(_) => format!("; {} {}", part, self.why_not(Need::Shown, &part)),
            // A value that can fail is read first: a `maybe` of one has two to open.
            Type::Or(..) if part == *ty => extract_hint(&[ty]),
            Type::Or(..) => "; a value that can fail is read first, with a plan B, match, or is".to_string(),
            Type::Maybe(inner) if matches!(inner.as_ref(), Type::Or(..)) => extract_hint(&[&part]),
            Type::Nothing => "; this has no value".to_string(),
            _ => self.no_own_str(&part),
        };
        format!("{} as text{}{}", ty, holds, hint)
    }

    /// Whether values of type `ty` can be written as text, by `show`, `str`, and
    /// text holding values: numbers, bools, and text; a pointer, as its address;
    /// an array, a slice, a tuple, or a `maybe` whose values can; and a shape or
    /// kind with its own `str`. A value that can fail is read first, and a
    /// type a declaration takes can when its word says it is `shown`.
    fn writable(&self, ty: &Type) -> bool {
        self.unwritable_part(ty).is_none()
    }

    /// The first type in `ty`, itself included, whose values can't be written
    /// as text (see `writable`).
    fn unwritable_part(&self, ty: &Type) -> Option<Type> {
        self.unwritable_in(ty, &mut Vec::new())
    }

    /// `unwritable_part`, with the shapes and kinds already being looked
    /// through in `seen`: one that holds itself (through a slice, say) is
    /// written as far as its values go, so it is no reason to refuse.
    fn unwritable_in(&self, ty: &Type, seen: &mut Vec<String>) -> Option<Type> {
        match ty {
            Type::Int | Type::Unit | Type::Char | Type::Float | Type::Bool | Type::Text(_) | Type::Ptr(..) | Type::Task(_) | Type::Closure(_) => None,
            Type::Maybe(inner) | Type::Array(inner, _) | Type::Slice(inner, _) => self.unwritable_in(inner, seen),
            Type::Map(key, value, _) => self.unwritable_in(key, seen).or_else(|| self.unwritable_in(value, seen)),
            Type::Tuple(items) => items.iter().find_map(|item| self.unwritable_in(item, seen)),
            Type::Param(_) | Type::Group(_) if self.allows(Need::Shown, ty) => None,
            Type::Named(..) | Type::Applied(_) if self.has_own_str(ty) => None,
            // A variant whose types nothing told is the statement's mistake
            // (see `untold_kinds`), which says so.
            Type::Applied(_) if untold_kind(ty) => None,
            // Without its own `str`, a shape is written as its fields that any
            // file reads, and a kind as its variant with the values it carries.
            Type::Named(..) | Type::Applied(_) => {
                let name = match ty {
                    Type::Applied(applied) => &applied.name,
                    Type::Named(name, _) => name,
                    _ => return Some(ty.clone()),
                };
                if seen.contains(name) {
                    return None;
                }
                let Some(parts) = self.written_parts(ty) else { return Some(ty.clone()) };
                seen.push(name.clone());
                let found = parts.iter().find_map(|part| self.unwritable_in(part, seen));
                seen.pop();
                found
            }
            _ => Some(ty.clone()),
        }
    }

    /// The types a shape's or kind's default text writes: a shape's fields
    /// that aren't `priv` (shared ones are no part of a value), with the types
    /// a shape that takes them is given, or every value a kind's variants
    /// carry. `None` for no shape or kind.
    fn written_parts(&self, ty: &Type) -> Option<Vec<Type>> {
        let (name, given): (&str, &[Type]) = match ty {
            Type::Named(name, _) => (name, &[]),
            Type::Applied(applied) => (&applied.name, &applied.types),
            _ => return None,
        };
        if let Some(shape) = self.shapes.get(name) {
            let names: HashMap<&str, &Type> = shape.types.iter().map(|param| param.name.as_str()).zip(given).collect();
            return Some(
                shape
                    .fields
                    .iter()
                    .filter(|field| !field.private)
                    .map(|field| field.ty.map_params(&mut |param| names.get(param).map(|ty| (*ty).clone())))
                    .collect(),
            );
        }
        let kind = self.kinds.get(name)?;
        let names: HashMap<&str, &Type> = kind.types.iter().map(|param| param.name.as_str()).zip(given).collect();
        Some(kind.variants.iter().flat_map(|variant| variant.payload.iter().map(|field| field.ty.map_params(&mut |param| names.get(param).map(|ty| (*ty).clone())))).collect())
    }

    /// What a value of a shape or kind given types holds, a shape's fields or
    /// a kind's payloads, with the types given in place of the names it takes.
    fn applied_parts(&self, applied: &Applied) -> Option<Vec<Type>> {
        let (_, params, parts) = self.takes_types(&applied.name)?;
        let names: HashMap<&str, &Type> = params.iter().map(|param| param.name.as_str()).zip(&applied.types).collect();
        Some(parts.iter().map(|ty| ty.map_params(&mut |param| names.get(param).map(|ty| (*ty).clone()))).collect())
    }

    /// Whether values of type `ty` are written by a method `str` of their own.
    fn has_own_str(&self, ty: &Type) -> bool {
        self.owner_of(ty).is_some_and(|owner| self.tasks.contains_key(&method_name(&owner, STR)))
    }

    /// The `str`s that writing a value of type `ty` as text calls, of the
    /// shapes made for types it holds, itself included, that give their own:
    /// each is the method of the shape it was made from, made for the same
    /// types (`Box<int>` calls `Box`'s `str` made for `int`). One written only
    /// inside another value has no call of its own that makes it.
    fn written_strs(&self, ty: &Type, seen: &mut Vec<String>, found: &mut Vec<(String, Vec<Type>)>) {
        match ty {
            Type::Maybe(inner) | Type::Array(inner, _) | Type::Slice(inner, _) => self.written_strs(inner, seen, found),
            Type::Map(key, value, _) => {
                self.written_strs(key, seen, found);
                self.written_strs(value, seen, found);
            }
            Type::Tuple(items) => items.iter().for_each(|item| self.written_strs(item, seen, found)),
            Type::Named(name, _) if !seen.contains(name) => {
                seen.push(name.clone());
                if let Some(made) = self.made_of.get(name)
                    && self.tasks.contains_key(&method_name(&made.name, STR))
                {
                    found.push((method_name(&made.name, STR), made.types.clone()));
                } else if !self.has_own_str(ty) {
                    for part in self.written_parts(ty).unwrap_or_default() {
                        self.written_strs(&part, seen, found);
                    }
                }
            }
            _ => {}
        }
    }

    /// Makes sure each `str` writing a value of type `ty` calls is made (see
    /// `written_strs`), for a value written at `at`.
    fn note_written_strs(&mut self, ty: &Type, at: Span) {
        let mut found = Vec::new();
        self.written_strs(ty, &mut Vec::new(), &mut found);
        for (method, types) in found {
            let given: Vec<TypeArg> = types.into_iter().map(|ty| (ty, at)).collect();
            self.note_instance(at, &method, &given);
        }
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
    fn check_join(&mut self, parts: &mut Vec<Expr>, region: &mut String, region_span: Span) -> Result<Type> {
        // Text holding values that no `at r` followed (see
        // `Parser::parse_interpolated`) goes in the block's default region.
        if let Some(field) = &self.in_default {
            return err(
                parts.first().map_or(region_span, |part| part.span),
                format!("joined text, or text holding values, is made while the program runs, so {} can't fall back on it; its default is text written whole, as in '\"n\"'", field),
            );
        }
        if region.is_empty() {
            *region = self.region_or_default(None, region_span, || {
                "text holding values is new text, so it names the region it goes in, as in '\"n = {n}\" at r' or '\"n = {n}\"@r'".to_string()
            })?;
            // Each value turned into text in that region, as `at r` would.
            let pieces = std::mem::take(parts);
            crate::parser::interpolated_parts(pieces, region, region_span, parts);
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
        // What a lambda keeps stays in its region, which its type names.
        if has_closure(&ty) {
            return err(span, format!("'copy' can't copy {}: what a lambda that keeps what it uses keeps stays in its region, which its type names; make the lambda again in the other region", ty));
        }
        if !holds_pointers(&ty, &self.regional) && !holds_text(&ty, &self.takes_region) && ty.params().is_empty() {
            let what = match ty {
                Type::Nothing => "this has no value".to_string(),
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
            // A `when` or `match` giving a value: each branch's value goes
            // where the whole does.
            (ExprKind::Choice(stmt), _) => self.settle_branch_values(stmt, expected),
            (ExprKind::StructInit { name, fields, .. }, Type::Named(_, Some(region))) => {
                let Some(shape) = self.shapes.get(name.as_str()).cloned() else { return Ok(()) };
                fields.iter_mut().try_for_each(|field| match shape.fields.iter().find(|f| f.name == field.name) {
                    Some(declared) => self.settle_copies(&mut field.value, &self.in_region(&declared.ty, region)),
                    None => Ok(()),
                })
            }
            // The kind the value is of, made for its types where it takes them.
            (ExprKind::Construct { tag, args, .. }, Type::Named(kind, Some(region))) => {
                let Some(variant) = self.kinds.get(kind.as_str()).and_then(|k| k.variants.iter().find(|v| v.tag == *tag)).cloned() else { return Ok(()) };
                args.iter_mut().zip(&variant.payload).try_for_each(|(arg, field)| self.settle_copies(arg, &self.in_region(&field.ty, region)))
            }
            _ => Ok(()),
        }
    }

    /// Checks the value of `new [T](value) at r`, which gives the slice its
    /// items, of type `items` in `r`: `[v#n]`, whose count may be known only as
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
                format!("'new [{}](…)' makes a slice from '[value#count]', an array, or a slice, but this value is {}", items, other),
            ),
        }
    }

    /// Checks a plan B, `value { body }`, and returns the type it gives: the
    /// type inside `value`.
    fn check_plan_b(&mut self, span: Span, value: &mut Expr, error: Option<&Binding>, body: &mut Block) -> Result<Type> {
        let found = self.check_expr(value);
        let value_ty = self.or_unknown(found);
        if past_end(value) {
            return self.past_end_plan_b(span, value, error, body);
        }
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
        if let Some(region) = settled.regions().into_iter().find(|region| !unnamed_region(region) && self.lookup(region).is_none()) {
            return err(at, format!("this value is in region '{}', which is freed when the plan B ends, so it can't be the plan B's value", region));
        }
        // A plan B is a place the value goes, so a call whose types it settled
        // (see `check_plan_b_body`) learns them from it.
        if !value_ty.params().is_empty() {
            // What the value holds, where the plan B made the whole a `maybe`.
            let held = match &settled {
                Type::Maybe(inner) if !matches!(extracted, Type::Maybe(_)) => (**inner).clone(),
                _ => settled.clone(),
            };
            let told = match &value_ty {
                Type::Maybe(_) => Type::Maybe(Box::new(held)),
                Type::Or(_, error) => Type::Or(Box::new(held), error.clone()),
                other => other.clone(),
            };
            self.settle_types(value, &told)?;
        }
        Ok(settled)
    }

    /// A plan B on `t.#5` past the tuple's end, which is always `none`, so the
    /// block always runs: the item's type is the one the block gives, as in
    /// `t.#5 { 0 }`, a `maybe int`.
    fn past_end_plan_b(&mut self, span: Span, value: &mut Expr, error: Option<&Binding>, body: &mut Block) -> Result<Type> {
        if let Some((_, at)) = error {
            return err(*at, "a maybe has no error to name; 'failure e' names the error of a 'T ~ E'");
        }
        let open = Type::Param(PAST_END.to_string());
        let entry = self.unset.clone();
        self.push_scope();
        let result = self.check_plan_b_body(span, &open, body);
        self.pop_scope();
        self.unset = entry;
        let (settled, at) = result?.unwrap_or((Type::Nothing, span));
        if settled.params().contains(&PAST_END) {
            return err(at, "this tuple has no such item, so its plan B always runs, and gives the value: one of a type, not 'none'");
        }
        // A block that only sends gives no value; the item is then any maybe.
        let held = match &settled {
            Type::Nothing => Type::Bool,
            Type::Maybe(inner) => (**inner).clone(),
            other => other.clone(),
        };
        self.coerce(value, &Type::Maybe(Box::new(held)))?;
        Ok(settled)
    }

    /// Checks a plan B's statements: the last line gives the value, unless every
    /// path through the block sends, or there is no value to give (`nothing ~ E`).
    ///
    /// Returns the plan B's type, and the last line's position, when the last line
    /// settles it: a value holding no pointers (of `ANY_REGION`, such as
    /// `first(none)`) has nothing to extract that lives anywhere, so the plan B
    /// takes the fallback's regions.
    fn check_plan_b_body(&mut self, span: Span, extracted: &Type, body: &mut Block) -> Result<Option<(Type, Span)>> {
        if *extracted == Type::Nothing {
            return self.check_block(body).map(|()| None);
        }
        if let Some(last) = body.last_mut() {
            last_loop_gives(last);
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
            if ty == Type::Nothing {
                return err(e.span, format!("the last line of a plan B gives its value, but this has none; expected a value of type {}", extracted));
            }
            // `v { none }`: the whole is a `maybe`, the value when there is one
            // and empty otherwise, never a plain value, which would have none.
            if let Some(maybe) = gives_a_maybe(extracted, &ty) {
                return self.coerce(e, &maybe).map(|()| Some((maybe, e.span)));
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
        // One whose every branch leaves (`<-`, `break`, `continue`) leaves the
        // block too, which then gives nothing, as a last `<-` does.
        if gives_value(last) && always_leaves(std::slice::from_ref(last)) {
            self.check_stmt(last)?;
            return Ok(None);
        }
        if gives_value(last) {
            let span = last.span;
            let given = self.check_branching_value(last)?;
            if let Some(maybe) = given.and_then(|ty| gives_a_maybe(extracted, &ty)) {
                return self.settle_value(body, &maybe).map(|()| Some((maybe, span)));
            }
            return self.settle_value(body, extracted).map(|()| None);
        }
        self.check_stmt(last)?;
        if !always_leaves(body) {
            let or_loop = if self.loops.is_empty() { "" } else { ", or leave with 'break' or 'continue'" };
            return err(span, format!("a plan B must end with a value of type {}, or send with '<-' on every path{}", extracted, or_loop));
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
                if ty.is_byte() {
                    return err(span, format!("a {0} is never negative, so it can't be negated; turn it into an int first, as in '-int(x)'", ty));
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
        // `first<>(x)`: `<>` reads as 'not similar', so this is a use giving no
        // types, written where a value would be compared.
        if op == BinaryOp::NotSimilar
            && let ExprKind::Var(name) = &l.kind
            && self.lookup(name).is_none()
            && [name.clone(), self.in_module(name)].iter().any(|name| self.tasks.contains_key(name) || self.shapes.contains_key(name))
        {
            return err(span, format!("'< … >' names the types a use gives, as in '{}<int>'; leave it out to let the values say which they are", unqualified(name)));
        }
        // `&a == &b`: where two places are, not what they hold.
        if matches!(op, BinaryOp::Eq | BinaryOp::Ne) && [&**l, &**r].into_iter().any(|side| matches!(side.kind, ExprKind::Ref(_))) {
            return self.check_address_compare(l, r, span);
        }
        let mut lt = self.check_expr(l)?;
        let mut rt = self.check_expr(r)?;
        // `Option.None == k`: a variant whose types it didn't tell is told
        // them by the value beside it.
        if holds_untold(&lt) && !holds_untold(&rt) {
            self.settle_variant(l, &rt)?;
            lt = l.ty.clone().unwrap_or(lt);
        } else if holds_untold(&rt) && !holds_untold(&lt) {
            self.settle_variant(r, &lt)?;
            rt = r.ty.clone().unwrap_or(rt);
        } else if holds_untold(&lt)
            && holds_untold(&rt)
            && let Some(joined) = self.join(&lt, &rt).filter(|joined| !holds_untold(joined))
        {
            // `Either.Left(1) == Either.Right("x")`: each tells what the other
            // doesn't, as values side by side do.
            let joined = self.made_kinds(joined, span)?;
            self.settle_variant(l, &joined)?;
            self.settle_variant(r, &joined)?;
            lt = l.ty.clone().unwrap_or(lt);
            rt = r.ty.clone().unwrap_or(rt);
        }
        // With one operand's type unknown, the operation's own type is unknown
        // too: a comparison is still a bool, but what it compares isn't checked
        // (see `Type::Unknown`).
        if lt.unknown() || rt.unknown() {
            return Ok(match op {
                BinaryOp::Eq
                | BinaryOp::Ne
                | BinaryOp::Similar
                | BinaryOp::NotSimilar
                | BinaryOp::Lt
                | BinaryOp::Le
                | BinaryOp::Gt
                | BinaryOp::Ge
                | BinaryOp::And
                | BinaryOp::Or => Type::Bool,
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
        let (mut lt, mut rt) = (lt.clone(), rt.clone());
        if op == BinaryOp::Pow {
            // `b ** 2`: an exponent written out is a unit beside a unit base.
            // A base written out stays an int, so `2 ** b` is an int.
            if lt == Type::Unit && byte_literal(r, &Type::Unit)? {
                rt = Type::Unit;
            }
            return check_power(l, r, &lt, &rt, span);
        }
        // `b + 1`, `b < 200`, `m ~= 7`: a number written out beside a unit, or
        // a maybe of one, is one.
        let byte_in = |ty: &Type| match ty {
            Type::Maybe(inner) if inner.is_byte() => Some((**inner).clone()),
            other if other.is_byte() => Some(other.clone()),
            _ => None,
        };
        if let Some(byte) = byte_in(&lt)
            && byte_literal(r, &byte)?
        {
            rt = byte;
        } else if let Some(byte) = byte_in(&rt)
            && byte_literal(l, &byte)?
        {
            lt = byte;
        }
        match op {
            BinaryOp::Pow => Err(Error::internal_at(span, "a power checked as other arithmetic")),
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod if lt == Type::Char || rt == Type::Char => {
                char_arithmetic(op, l, r, &lt, &rt, span)
            }
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
                    if !lt.is_byte() && !rt.is_byte() {
                        return err(
                            at,
                            "an int and a float don't mix in arithmetic: turn one into the other first, as in 'float(n) + 2.5' (or 'n + int(2.5)', which truncates)",
                        );
                    }
                    let (byte, other) = if lt.is_byte() { (&lt, &rt) } else { (&rt, &lt) };
                    return err(
                        at,
                        format!(
                            "{0} and a {1} don't mix in arithmetic: turn one into the other first, as in '{2}(b) + x' or 'b + {1}(x)'",
                            if *other == Type::Int { "an int" } else { "a float" },
                            byte,
                            other
                        ),
                    );
                }
                if lt.is_byte() {
                    return Ok(lt);
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
                    return err(span, format!("'==' compares a {} only with none; extract its value first, as in 'x {{ … }} == v', or compare what it holds with '~=', as in 'x ~= v'", maybe));
                }
                // Comparing stores nothing, so values in different regions
                // compare (pointers and places into different regions are never
                // equal), and so do texts in any regions.
                if lt.without_regions() != rt.without_regions() {
                    self.expect_type(&rt, &lt, r.span)?;
                }
                let symbol = if op == BinaryOp::Eq { "==" } else { "!=" };
                if !self.comparable(&lt) {
                    if let Some(untold) = self.untold_param(&lt).or_else(|| self.untold_param(&rt)) {
                        return err(span, untold);
                    }
                    let hint = match &lt {
                        Type::Param(_) => format!("; it {}", self.why_not(Need::Same, &lt)),
                        Type::Named(name, _) if self.kinds.contains_key(name) => {
                            "; a kind is compared by its variant and what that carries, and one of its variants carries a value that can fail, which is read first".to_string()
                        }
                        _ => extract_hint(&[&lt]),
                    };
                    return err(span, format!("'{}' can't compare values of type {}{}", symbol, lt, hint));
                }
                // An array, a shape, or a tuple is compared by where it is, so
                // each side is a place; a value made on the spot is none.
                if self.by_place(&lt) {
                    let what = self.compared_by_place(symbol, &lt);
                    if !same_root(l, r) {
                        for side in [&*l, &*r] {
                            self.warn_chosen(side, &what);
                        }
                    }
                    for side in [&*l, &*r] {
                        if !self.is_place(side) {
                            return err(
                                side.span,
                                format!(
                                    "{}, as it does arrays, shapes, and tuples (and kinds carrying them), and this one is a value made here, with no place of its own; '~=' compares two values item by item",
                                    what
                                ),
                            );
                        }
                    }
                }
                Ok(Type::Bool)
            }
            BinaryOp::Similar | BinaryOp::NotSimilar => {
                let symbol = if op == BinaryOp::Similar { "~=" } else { "<>" };
                // `none` is similar to an empty `maybe`, as `==` has it.
                let none_and_maybe = matches!((&lt, &rt), (Type::None, Type::Maybe(_)) | (Type::Maybe(_), Type::None));
                if none_and_maybe {
                    return Ok(Type::Bool);
                }
                if lt == Type::None || rt == Type::None {
                    return err(span, "'none' can only be compared with a maybe value");
                }
                let (lt, rt) = self.widen_literal(l, r, lt, rt);
                // `m ~= v`, either side first: a maybe is similar to a plain
                // value when it holds one similar to it, so the plain side
                // becomes a maybe and the two compare as maybes do (empty is
                // similar to empty only).
                let (lt, wrapped) = match (&lt, &rt) {
                    (Type::Maybe(inner), plain) | (plain, Type::Maybe(inner)) if !matches!(plain, Type::Maybe(_)) => {
                        let maybe = Type::Maybe(inner.clone());
                        if inner.without_regions() != plain.without_regions() {
                            return err(span, format!("'{}' compares {} with {}, {}, or none, not with {}{}", symbol, maybe, inner, maybe, plain, extract_hint(&[plain])));
                        }
                        wrap(if matches!(lt, Type::Maybe(_)) { &mut *r } else { &mut *l }, &maybe);
                        (maybe, true)
                    }
                    // A maybe that a generic made of a maybe (`maybe maybe int`)
                    // beside the maybe it holds: the one it holds is wrapped.
                    (Type::Maybe(inner), other @ Type::Maybe(_)) | (other @ Type::Maybe(_), Type::Maybe(inner))
                        if inner.without_regions() == other.without_regions() =>
                    {
                        let maybe = Type::Maybe(inner.clone());
                        let lt_holds_rt = matches!(&lt, Type::Maybe(held) if held.without_regions() == rt.without_regions());
                        wrap(if lt_holds_rt { &mut *r } else { &mut *l }, &maybe);
                        (maybe, true)
                    }
                    (Type::Maybe(inner), other @ Type::Maybe(theirs)) if inner.without_regions() != theirs.without_regions() => {
                        return err(span, format!("'{}' compares {} with {}, {}, or none, not with {}", symbol, lt, inner, lt, other));
                    }
                    _ => (lt, false),
                };
                if !wrapped && lt.without_regions() != rt.without_regions() {
                    self.expect_type(&rt, &lt, r.span)?;
                }
                if let Some(part) = self.unsimilar_part(&lt) {
                    if let Some(untold) = self.untold_param(&lt).or_else(|| self.untold_param(&rt)) {
                        return err(span, untold);
                    }
                    let holds = if part == lt { String::new() } else { format!(", since it holds {}", part) };
                    let hint = match &part {
                        Type::Param(_) => format!("; {} {}", part, self.why_not(Need::Similar, &part)),
                        _ => extract_hint(&[&part]),
                    };
                    return err(span, format!("'{}' can't compare values of type {} item by item{}{}", symbol, lt, holds, hint));
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
                        return err(span, format!("'{}' compares numbers, chars, or text; {} {}", symbol, lt, self.why_not(Need::Ordered, &lt)));
                    }
                    if matches!(lt, Type::Tuple(_)) {
                        return err(span, format!("'{}' compares numbers, chars, or text; a tuple is only compared with '==' and '!=', or item by item, as in 't.0 < u.0'", symbol));
                    }
                    return err(span, format!("'{}' compares numbers, chars, or text, not {}{}", symbol, lt, extract_hint(&[&lt])));
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
        // Read, `.type` never reaches here (see `check_type_of`); changed, it does.
        if field == TYPE_WORD {
            return err(span, format!("'type' is the type of the value ({} here), which never changes", self.written_type(base)));
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
            // A group's tuple has two or more items, and each copy of the task
            // checks a read past those its call gives (see `substitute_expr`).
            Type::Tuple(items) if group_in_tuple(items).is_some() => match field.parse::<usize>() {
                Ok(n) => Ok(Type::Param(group_item(&group_in_tuple(items).unwrap_or_default(), n))),
                Err(_) => err(span, format!("type {} has no field '{}'; a tuple's items are read by position, as in 't.0'", base, field)),
            },
            Type::Tuple(items) => match field.parse::<usize>() {
                Ok(n) => items.get(n).cloned().ok_or_else(|| {
                    Error::at(span, format!("this tuple has {} items, 't.0' to 't.{}', so it has no item {}", items.len(), items.len() - 1, n))
                }),
                Err(_) => err(span, format!("type {} has no field '{}'; a tuple's items are read by position, as in 't.0'", base, field)),
            },
            _ if field.parse::<u64>().is_ok() => {
                err(span, format!("'.{}' reads an item of a tuple, but this value is {}{}", field, base, extract_hint(&[base])))
            }
            Type::Array(..) | Type::Slice(..) | Type::Map(..) | Type::Text(_) if field == LEN => Ok(Type::Int),
            // `m.keys` alone is read only by `each`, so it is no field either.
            Type::Map(..) if field == "keys" || field == "values" => {
                let one = if field == "keys" { "k" } else { "v" };
                err(span, format!("'m.{0}' alone is only gone through, as in 'each {1} in m.{0}'; 'm.{0} at r' makes a slice of them in a region", field, one))
            }
            Type::Map(..) => err(span, format!("type {} has no field '{}'; its number of keys is '.len', and its keys and values are '.keys at r' and '.values at r'", base, field)),
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
        // A literal nests in a literal, so this loop is kept small, as the
        // stack each level takes (see `check_in_module`).
        let from = filled.map_or(usize::MAX, |(from, _)| from);
        for (at, value) in values.iter_mut().enumerate().take(declared.len()) {
            if at < from { self.check_expr(value) } else { self.check_in_module(value, filled) }?;
        }
        self.fields_region(values, declared)
    }

    /// The region of the value `values`, already checked, make for the fields
    /// or payload `declared` (see `check_fields_of_value`), each value made to
    /// fit its field there.
    fn fields_region(&mut self, values: &mut [&mut Expr], declared: &[Field]) -> Result<String> {
        const OWN: &str = "";
        let mut regions = HashMap::new();
        for (value, field) in values.iter().zip(declared) {
            let ty = value.ty()?.clone();
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
        if name == TYPE_WORD {
            return Error::at(span, "'type' is the type of a value, read without '(…)', as in 'x.type'");
        }
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
            // Without a `str` of its own, one is written by its parts, and one
            // of them can't be.
            if name == STR {
                return Error::at(
                    span,
                    format!("'str' can't write {}; or give {} '{}' its own method 'str(r: region) -> text@r'", self.why_unwritable(base), what, written),
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
            _ if name == STR => Error::at(span, format!("'str' can't write {}", self.why_unwritable(base))),
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
        // `(&x).str()` and `"{&x}"`: the address of the place, as text. An
        // address is no value of its own, so it has nothing else.
        let found = match value.kind {
            ExprKind::Ref(_) if name != STR => {
                return err(value.span, format!("an address, '&x', is only shown, turned into text with 'str', or compared with '=='; it has no '{}'", name));
            }
            ExprKind::Ref(_) => self.check_address(value),
            _ => self.check_expr(value),
        };
        self.unsettled_copies.truncate(copies);
        self.unsettled_types.truncate(untold);
        let ty = found?;
        // `Option.None.isSome()`: a method's value tells it no types.
        if untold_kind(&ty)
            && let Some(error) = self.untold_kind_error(value.span)
        {
            return Err(error);
        }
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
        // `m.has(k)`: whether a map holds the key (see `ExprKind::MapOp`).
        if let Type::Map(..) = ty
            && let Some(op) = MapOp::from_word(&name)
        {
            if op != MapOp::Has {
                return err(span, format!("'{0}' is written without parentheses, with the region its slice goes in after it, as in 'm.{0} at r'", name));
            }
            let taken = std::mem::replace(&mut expr.kind, ExprKind::NoneLit);
            let ExprKind::MethodCall { value, mut args, .. } = taken else {
                return Err(Error::internal_at(span, "a method call that is no longer one"));
            };
            if args.len() != 1 {
                return err(span, format!("'has' takes the key to look for, as in 'm.has(k)', got {}", count(args.len(), "argument")));
            }
            let key = Box::new(args.remove(0));
            expr.kind = ExprKind::MapOp { map: value, op, key: Some(key), region: None, region_span: span };
            return Ok(());
        }
        // `t.at(i)`: a byte of text, or none (see `ExprKind::Lookup`).
        if name == AT && matches!(ty, Type::Text(_)) {
            let taken = std::mem::replace(&mut expr.kind, ExprKind::NoneLit);
            let ExprKind::MethodCall { value, mut args, .. } = taken else {
                return Err(Error::internal_at(span, "a method call that is no longer one"));
            };
            if args.len() != 1 {
                return err(span, format!("'at' takes the index of one byte, as in 't.at(i)', got {}", count(args.len(), "argument")));
            }
            expr.kind = ExprKind::Lookup(value, Box::new(args.remove(0)));
            return Ok(());
        }
        // `op.run(4, 5)`: a field that holds a task, called, where the value
        // has no method of that name.
        let method = self.owner_of(&ty).is_some_and(|owner| self.tasks.contains_key(&method_name(&owner, &name)));
        if !method
            && let Ok(field) = self.field_type(&ty, &name, span)
            && (is_task(&field) || matches!(&field, Type::Maybe(inner) if is_task(inner)))
        {
            let taken = std::mem::replace(&mut expr.kind, ExprKind::NoneLit);
            let ExprKind::MethodCall { value, args, .. } = taken else {
                return Err(Error::internal_at(span, "a method call that is no longer one"));
            };
            let callee = Expr::new(ExprKind::Field(value, name), span);
            expr.kind = ExprKind::Invoke { callee: Box::new(callee), args };
            return Ok(());
        }
        let owner = self.owner_of(&ty).ok_or_else(|| self.no_method(&ty, &name, span))?;
        let full = method_name(&owner, &name);
        let info = self.tasks.get(&full).filter(|_| self.sees(&full)).cloned().ok_or_else(|| self.no_method(&ty, &name, span))?;
        let written = format!("{}{}{}", unqualified(&owner), MODULE_SEPARATOR, name);
        if info.owner.as_ref().is_some_and(|owner| owner.shared) {
            return err(span, format!("'{0}' is a shared method, called on '{1}' itself, not on a value of it: write '{1}.{2}(…)'", written, unqualified(&owner), name));
        }
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
        // The value it is called on is the first parameter, not an argument;
        // the last may fall back on values (see `fill_defaults`).
        let needed = self.param_defaults.get(&full).and_then(|defaults| defaults.iter().position(Option::is_some)).unwrap_or(info.params.len());
        if given + 1 > info.params.len() || given + 1 < needed {
            return err(span, format!("method '{}' expects {}, got {}", written, self.expected_args(&full, info.params.len(), 1), given));
        }
        if info.owner.as_ref().is_some_and(|owner| owner.alters) {
            let ExprKind::MethodCall { value, .. } = &expr.kind else { return Ok(()) };
            self.altered_place(value, &written)?;
            self.hand_written(value, Some(&written));
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

    /// Whether values of type `ty` have `str` built in: every value that can
    /// be written as text (see `writable`) but a shape's or kind's with a
    /// method `str` of its own, which is called instead.
    fn has_builtin_str(&self, ty: &Type) -> bool {
        !self.has_own_str(ty) && self.writable(ty)
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
        let ty = match value.kind {
            ExprKind::Ref(_) => self.check_address(value)?,
            _ => self.check_expr(value)?,
        };
        self.note_written_strs(&ty, value.span);
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
            Type::Int => "'int' takes an int, a unit, a char, a float, a bool, or text",
            Type::Unit => "'unit' takes an int, a unit, a char, a float, a bool, or text",
            Type::Char => "'char' takes an int, a unit, or a char, the code of the character",
            _ => "'float' takes an int, a unit, a float, or text",
        };
        let [value] = args else {
            return err(span, format!("'{0}(…)' turns one value into {0}, as in '{0}(x)', got {1}", into, count(args.len(), "value")));
        };
        let ty = self.check_expr(value)?;
        let fits = matches!(
            (into, &ty),
            (Type::Int | Type::Unit, Type::Int | Type::Unit | Type::Char | Type::Float | Type::Bool)
                | (Type::Float, Type::Int | Type::Unit | Type::Float)
                | (Type::Char, Type::Int | Type::Unit | Type::Char)
        );
        // `unit(300)`: a number written out that a unit can't hold.
        if (into.is_byte() || *into == Type::Char)
            && ty == Type::Int
            && let Some(n) = constant_int(value)
        {
            fits_byte(n, into, value.span)?;
        }
        if fits || ty.unknown() {
            return Ok(into.clone());
        }
        // Text may hold no number at all, which is a mistake in the input rather
        // than in the program, so reading it sends a value or a reason why not.
        if matches!(ty, Type::Text(_)) && *into != Type::Char {
            return Ok(Type::Or(Box::new(into.clone()), Box::new(Type::Named(PARSE_ERROR.to_string(), None))));
        }
        let hint = match &ty {
            Type::Text(_) if *into == Type::Char => "; a byte of text is read with 't[i]', or 't.at(i)', which gives none out of range".to_string(),
            Type::Bool => format!("; a bool becomes a number with 'int', as in '{}(int(ok))'", into),
            Type::Ptr(..) => format!("; to turn the value it points to into a number, write '{}(p^)'", into),
            Type::Maybe(_) | Type::Or(..) => extract_hint(&[&ty]),
            Type::Nothing => "; this has no value".to_string(),
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
        self.altered_part(value, method, &self.written_text(value))
    }

    /// `altered_place` of `value`, part of the place `written`, as written.
    fn altered_part(&self, value: &Expr, method: &str, written: &str) -> Result<()> {
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
                    return refuse(format!("it needs a place that can change, and '{0}' only reads the place it names; a reference that changes it is written 'flex& {0} = &…'", name));
                }
                if info.param && info.passing == Passing::FixedRef {
                    return refuse(format!(
                        "it needs a place that can change, and '{0}' is a 'fixed&' parameter, which only reads the caller's place; write 'flex& {0}: {1}' to change it",
                        name, info.ty
                    ));
                }
                if info.param {
                    return refuse(format!(
                        "it needs a place that can change, and '{0}' is a parameter, which never does; write 'flex {0}: {1}' to change the task's own copy, or 'flex& {0}: {1}' to change the caller's",
                        name,
                        info.ty
                    ));
                }
                refuse(format!(
                    "it needs a place that can change, and '{}' never does (declared at {}:{}); a value declared with 'flex' can",
                    name, info.span.line, info.span.col
                ))
            }
            ExprKind::Field(base, field) => {
                if let Some(ty) = &base.ty {
                    if matches!(ty, Type::Tuple(_)) {
                        return refuse("it needs a place that can change, and a tuple's items never change; give the whole tuple a new value instead, as in 't = (1, 2)'".to_string());
                    }
                    self.read_only_field(ty, field, value.span)?;
                }
                self.altered_part(base, method, written)
            }
            // A slice's items are in a region, behind a pointer, and change as
            // what a pointer points to does: where it is reached from a name
            // that changes (see `written_through`).
            ExprKind::Index(base, _) if matches!(base.ty, Some(Type::Slice(..) | Type::Map(..))) => self.written_through(base, written, Some(method)),
            ExprKind::Index(base, _) => self.altered_part(base, method, written),
            ExprKind::Deref(pointer) => self.written_through(pointer, written, Some(method)),
            ExprKind::Shared { shape, field } => self.changeable_shared(shape, field, value.span, &format!("'{}' changes the value it is called on, and ", method)),
            _ => refuse("it needs a place that can change, such as a 'flex' variable, a field or item of one, or what a pointer points to, and not a value made here".to_string()),
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
            Some(owner) if !owner.shared => self.needs_alters(span, what, owner),
            _ => Error::at(span, self.not_a_method(THIS)),
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
        // `Point.new`, a shared method named without '(…)': its task, as a value.
        let task = method_name(&shape, &field);
        if let Some(owner) = self.tasks.get(&task).and_then(|info| info.owner.clone()) {
            let written = unqualified(&shape);
            if !owner.shared {
                let value = if self.kinds.contains_key(&shape) { "k" } else { "p" };
                return err(span, format!("'{0}' is a method of every value of '{1}', so it is called on a value, as in '{2}.{0}(…)'; only a shared method is a value, named on its type", field, written, value));
            }
            if self.private.contains(&task) && module_of(&shape) != self.current_module {
                return err(span, format!("'{}' is a private method of '{}', which only its own file can use", field, written));
            }
            expr.kind = ExprKind::TaskValue(task);
            return self.infer_expr(expr);
        }
        if self.kinds.contains_key(&shape) {
            if field == TYPE_WORD {
                return err(span, format!("'type' is read on a value, as in 'k.type', and '{}' is a kind, whose type is its name", unqualified(&shape)));
            }
            return err(span, format!("kind '{}' has no variant or shared method '{}'", unqualified(&shape), field));
        }
        let Some(def) = self.shapes.get(&shape) else { return Err(Error::internal_at(span, format!("shared field of unknown shape '{}'", shape))) };
        let Some(index) = def.shared.iter().position(|member| member.field.name == field) else {
            if def.fields.iter().any(|declared| declared.name == field) {
                return err(span, format!("'{0}' is a field of every value of '{1}', not a shared one, so it is read on a value, as in 'p.{0}'", field, shape));
            }
            if field == TYPE_WORD {
                return err(span, format!("'type' is read on a value, as in 'p.type', and '{}' is a shape, whose type is its name", unqualified(&shape)));
            }
            // `Card.Hearts`, written as if the shape were a kind.
            if self.variants.values().any(|info| info.variant.tag == field) {
                return err(span, format!("'{}' is a shape, not a kind, so it has no variant '{}'", shape, field));
            }
            return err(span, format!("shape '{}' has no shared field or shared method '{}'", shape, field));
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
                        "the value {} falls back on is worked out before the program runs, and shared field '{}' can change, so it can't be named there; one that never changes can",
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
    /// it can't be: one that is neither `flex` nor `read`, and a `read` one
    /// outside the methods of its shape. `lead` starts the message, for a
    /// change that is a call's.
    fn changeable_shared(&self, shape: &str, field: &str, span: Span, lead: &str) -> Result<()> {
        let Some(member) = self.shapes.get(shape).and_then(|def| def.shared.iter().find(|member| member.field.name == field)) else {
            // `Point.new`, a shared method, named as a value: no place.
            if self.tasks.get(&method_name(shape, field)).is_some_and(|info| info.owner.is_some()) {
                return err(span, format!("{}'{}.{}' is a method, whose value is the task itself, so it never changes", lead, unqualified(shape), field));
            }
            return Ok(());
        };
        let written = shared_key(shape, field);
        if !member.changes() {
            return err(
                span,
                format!(
                    "{}'{}' never changes (declared at {}:{}); a shared field that changes is 'shared flex {}', or 'shared read {}', which only the methods of '{}' change",
                    lead, written, member.field.span.line, member.field.span.col, field, field, shape
                ),
            );
        }
        if !member.flex && self.current_shape.as_deref() != Some(shape) {
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

    /// Whether the task being checked is a method called on a value, which it
    /// reads as `.this`: not a task, and not a shared method.
    fn called_on_a_value(&self) -> bool {
        self.current_method.as_ref().is_some_and(|owner| !owner.shared)
    }

    /// The error for `.this` or `.x` in a task that is no method, or in a
    /// shared method.
    fn not_a_method(&self, written: &str) -> String {
        if self.in_shared.is_some() {
            return format!("'{}' reads the value a method is called on, and a shared field belongs to no value", written);
        }
        if let Some(field) = &self.in_default {
            return format!("'{}' reads the value a method is called on, and the value {} falls back on is worked out before there is one, so it reads {}", written, field, if field.starts_with("field") { "no other field" } else { "none of its fields" });
        }
        let names = if written == THIS { "is the value a method is called on" } else { "names a member of the value a method is called on" };
        if self.current_lambda.as_ref().is_some_and(|lambda| self.kept_lambdas.contains_key(lambda)) {
            let field = written.trim_start_matches('.');
            let example = if written == THIS { "fixed value = .this".to_string() } else { format!("fixed {0} = .{0}", field) };
            return format!("'{}' {}, which a lambda doesn't keep; keep a variable of it in the method first, as in '{}', and use that", written, names, example);
        }
        if self.current_lambda.is_some() {
            return format!("'{}' {}, and a lambda takes nothing from the method around it; give the value as a parameter", written, names);
        }
        if let Some(owner) = self.current_method.as_ref().filter(|owner| owner.shared) {
            return format!("'{}' {}, and '{}' is a shared method, called on its shape or kind rather than on a value; take the value as a parameter, or make '{}' a method of the value", written, names, owner.name, owner.name);
        }
        format!("'{}' {}, and this is a task, which is called on no value", written, names)
    }

    /// Checks a call's arguments against the task's `params`, and returns the
    /// type the call gives. A region argument must be a region the caller can
    /// allocate in. The task's other regions stand for the caller's regions
    /// that the arguments' types show; one that no argument shows (as for
    /// `none`) becomes `ANY_REGION`.
    fn check_call_args(&mut self, span: Span, task: &str, written: &mut Vec<TypeArg>, args: &mut [Expr], info: &TaskInfo) -> Result<Type> {
        // The types the call gives: written after the name, or worked out from
        // the arguments below (and from the place the value goes, later).
        let given = self.written_types(span, task, written, info)?;
        // The region a lambda that keeps what it uses keeps it in, which its
        // type names as the caller does (see `check_invoke`).
        let regions = self.kept_call.take().map(|region| (region.clone(), region)).into_iter().collect();
        let mut found = CallTypes { given, shown: HashMap::new(), regions, taken: HashMap::new(), untold: false };
        // The arguments are checked in a function of their own, and what they
        // show is settled in another: the checker recurses through calls'
        // arguments, so this keeps little on the stack.
        let mut info = info.clone();
        self.check_args_of(task, args, &mut info, &mut found)?;
        self.settle_call_args(span, task, written, args, &info, found)
    }

    /// Notes in `found` how `actual`, the task value given at `at` for a
    /// parameter of type `ty`, takes each value of a group the call gives,
    /// and makes `ty` take them so too: a task value taking a place there is
    /// given the group's value as a place (see `group_place`). Two task values
    /// taking one value in different ways can't both be given it.
    fn note_group_taken(ty: &mut Type, actual: &Type, at: Span, found: &mut CallTypes) -> Result<()> {
        let (Type::Task(pattern), Type::Task(actual)) = (ty, actual) else { return Ok(()) };
        if pattern.params.len() != actual.params.len() {
            return Ok(());
        }
        for (i, param) in pattern.params.iter().enumerate() {
            let (Type::Param(name), Some(how)) = (param, actual.taken.get(i)) else { continue };
            if !name.contains(GROUP_ITEM) {
                continue;
            }
            match found.taken.get(name) {
                Some(before) if before != how => {
                    let item = name.rsplit(GROUP_ITEM).next().and_then(|k| k.parse::<usize>().ok()).map_or(0, |k| k + 1);
                    let way = |how: &Taken| match how {
                        Taken::Value => "as a value".to_string(),
                        Taken::Place(passing) => format!("as a place, '{}'", passing.word()),
                        Taken::Region(_) => "as a region".to_string(),
                    };
                    return err(
                        at,
                        format!("this task value takes value {} of the group {}, and another task value the call gives takes it {}, so the call can't give it to both; give task values taking it the same way", item, way(how), way(before)),
                    );
                }
                Some(_) => {}
                None => {
                    found.taken.insert(name.clone(), how.clone());
                }
            }
            if let Some(slot) = pattern.taken.get_mut(i) {
                *slot = how.clone();
            }
        }
        Ok(())
    }

    /// How a call gives `arg`, the value `param` of a group it gives, which a
    /// task value it gives takes as `found` notes: a place it takes there is
    /// given as a place, marked, and a region a group never holds.
    fn group_place(&self, task: &str, arg: &Expr, param: &str, name: &str, found: &CallTypes) -> Result<Option<Passing>> {
        match found.taken.get(name) {
            Some(Taken::Place(passing)) => Ok(Some(*passing)),
            Some(Taken::Region(region)) => err(
                arg.span,
                format!(
                    "task '{0}' gives a task value its group's values, and that task value takes a region there, '@{1}', which a group never holds; '{0}' takes the region as a parameter of its own and names it in the task value's type, as in 'next: (@a, P...) -> R, a: region, args: P...'",
                    unqualified(task),
                    region
                ),
            ),
            // A task value of a type not known, which a mistake in it left:
            // the place may be what it takes.
            None if found.untold && matches!(arg.kind, ExprKind::Ref(_)) => Ok(Some(Passing::FixedRef)),
            _ if matches!(arg.kind, ExprKind::Ref(_)) => err(
                arg.span,
                format!("'{}' is a value of a group, which holds a place only where the call gives it to a task value taking one there, as in 'next: (P...) -> R' given a task taking 'flex& n: int'; give the value itself", param),
            ),
            _ => Ok(None),
        }
    }

    /// Checks the arguments of a call of `task`, and notes in `found` what
    /// each shows the task's types and regions to be.
    fn check_args_of(&mut self, task: &str, args: &mut [Expr], info: &mut TaskInfo, found: &mut CallTypes) -> Result<()> {
        let what = if info.owner.is_some() { "method" } else { "task" };
        let mut deferred = Vec::new();
        // A group's values are checked after a task value given after them,
        // `apply(&n, 5, bump)`, which says how it takes each (see
        // `note_group_taken`).
        let group_item = |ty: &Type| matches!(ty, Type::Param(name) if name.contains(GROUP_ITEM));
        let first_item = info.params.iter().position(|(_, ty)| group_item(ty));
        let last_task = info.params.iter().rposition(|(_, ty)| matches!(ty, Type::Task(task) if task.params.iter().any(group_item)));
        let mut order: Vec<usize> = (0..args.len().min(info.params.len()).min(info.passing.len())).collect();
        if let (Some(first), Some(last)) = (first_item, last_task)
            && first < last
        {
            order.sort_by_key(|&i| info.params.get(i).is_some_and(|(_, ty)| group_item(ty)));
        }
        for i in order {
            let (Some(arg), Some((param, ty)), Some(passing)) = (args.get_mut(i), info.params.get_mut(i), info.passing.get_mut(i)) else { continue };
            if *ty != Type::Region {
                // A value of a group, which a task value given with it may take
                // as a place.
                if let Type::Param(name) = &*ty
                    && name.contains(GROUP_ITEM)
                    && let Some(place) = self.group_place(task, arg, param, name, found)?
                {
                    *passing = place;
                }
                // A value of a shape made for its types is read as that shape
                // given them, so a header naming `Box<T>` matches it (see
                // `match_regions`).
                // A value filled in is read as the task's module wrote it.
                let actual = if self.filled_default(task, i, arg) {
                    let outer = std::mem::replace(&mut self.current_module, module_of(task).to_string());
                    let actual = self.check_argument(arg, *passing, &format!("{} '{}'", what, task), param);
                    self.current_module = outer;
                    actual?
                } else {
                    self.check_argument(arg, *passing, &format!("{} '{}'", what, task), param)?
                };
                self.match_regions(ty, &actual, &mut found.regions);
                Self::note_group_taken(ty, &actual, arg.span, found)?;
                found.untold |= matches!(ty, Type::Task(_)) && actual.unknown();
                // A task value whose type names regions is matched after the
                // other values, whose types name the caller's regions its own
                // stand for.
                if let (Type::Task(_), Type::Task(task)) = (&*ty, &actual)
                    && (task.params.iter().chain([&task.ret]).any(|ty| !ty.regions().is_empty()) || task.taken.iter().any(|how| matches!(how, Taken::Region(_))))
                {
                    deferred.push((ty.clone(), actual));
                    continue;
                }
                self.match_params(ty, &actual, &mut found.given);
                self.match_params(ty, &actual, &mut found.shown);
                continue;
            }
            self.check_region_argument(task, arg, param, &mut found.regions)?;
        }
        for (ty, actual) in deferred {
            let actual = in_callers_regions(&ty, &actual, &found.given);
            self.match_params(&ty, &actual, &mut found.given);
            self.match_params(&ty, &actual, &mut found.shown);
        }
        Ok(())
    }

    /// Checks `arg`, given to a call of `task` as its region parameter `param`.
    fn check_region_argument(&mut self, task: &str, arg: &mut Expr, param: &str, regions: &mut HashMap<String, String>) -> Result<()> {
        let ExprKind::Var(region) = &arg.kind else {
            return err(arg.span, format!("task '{}' takes a region as '{}', such as a 'region r' declared before the call", task, param));
        };
        self.visible_region(region, arg.span, true)?;
        if let Some(bound) = regions.get(param)
            && bound != region
        {
            return err(arg.span, format!("expected region '{}', got '{}'", bound, region));
        }
        regions.insert(param.to_string(), region.clone());
        arg.ty = Some(Type::Region);
        Ok(())
    }

    /// Settles a call of `task` whose arguments are checked: the types it
    /// gives, what each argument becomes, and the type of what it sends.
    fn settle_call_args(&mut self, span: Span, task: &str, written: &mut Vec<TypeArg>, args: &mut [Expr], info: &TaskInfo, found: CallTypes) -> Result<Type> {
        let CallTypes { mut given, shown, regions, taken, untold } = found;
        let params = &info.params;
        let ret = &info.ret;
        // Where each type the call gives is written: the call itself, for one
        // worked out rather than written.
        let places: Vec<Span> = info.types.iter().enumerate().map(|(i, _)| written.get(i).map_or(span, |(_, at)| *at)).collect();
        let what = if info.owner.is_some() { "method" } else { "task" };
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
                // Its body may copy a value of the type, which a `[flex @r]`
                // lambda never is.
                if held_lambda(ty) || holds_held_lambda(ty) || group_items(&given, &param.name).is_some_and(|(types, _)| types.iter().any(|ty| held_lambda(ty) || holds_held_lambda(ty))) {
                    return err(
                        *at,
                        format!("task '{}' takes '{}' as a type, and is given a '[flex @r]' lambda for it, which changes what it keeps and is never copied, as its body may copy a value of that type; give it a '[@r]' lambda, or a task taking the lambda as 'flex& f: {}'", unqualified(task), param.name, ty),
                    );
                }
                self.check_need("task", task, param, ty, *at)?;
            }
        }
        // A type found in a region a task value takes is in the region the
        // task's body gives it there (see `regions_named_as`).
        for (param, at) in info.types.iter().zip(&places) {
            if let Some(ty) = given.get(&param.name)
                && ty.regions().contains(&OWN_REGION)
            {
                return err(*at, own_region_type(what, task, &param.name, ty));
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
            // may write there, or reads there: nothing becomes a wider type
            // on the way.
            if passing.is_ref() {
                // A region no argument names holds nothing the task could write
                // there but values that last as long as the program, as the
                // place's own do.
                let lasting = expected.map_regions(&mut |region| (region != ANY_REGION).then(|| region.to_string()));
                if actual != expected && actual != lasting && !actual.unknown() && !expected.unknown() {
                    let reads = *passing == Passing::FixedRef;
                    if actual.without_regions() == expected.without_regions() && !reads {
                        return err(
                            arg.span,
                            format!("expected type {0}, got {1}; the {2} may write a {0} in the place given as '{3}', which a place of type {1} can't hold", expected, actual, what, param),
                        );
                    }
                    let does = if reads { "reads where it is" } else { "changes" };
                    return err(
                        arg.span,
                        format!("expected type {}, got {}; '{}' is given the place itself, which the {} {}, so the place is of exactly its type", expected, actual, param, what, does),
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
        for ((arg, passing), (param, _)) in args.iter().zip(&info.passing).zip(&info.params) {
            match (passing, &arg.kind) {
                // A `flex` parameter may change what it holds.
                (Passing::Flex, _) => self.hand(arg, HandedTo::Param { task: task.to_string(), param: param.clone() }),
                (Passing::FlexRef, ExprKind::Ref(place)) => self.hand_written(place, None),
                _ => {}
            }
        }
        let sent = self.settled_type(ret, &regions, &given, span)?;
        // A type no argument names is worked out from the place the value goes
        // (see `settle_types`), or written at the call.
        // A group given no values, as a call of a task taking one sees it (see
        // `group_call_info`), is given no types.
        for param in info.types.iter().filter(|param| param.group) {
            given.entry(param.name.clone()).or_insert_with(|| Type::Tuple(Vec::new()));
        }
        let missing: Vec<&TypeParam> = info.types.iter().filter(|param| !given.contains_key(&param.name)).collect();
        // A task value whose mistake is told already may be what names them.
        if untold && !missing.is_empty() {
            return Ok(Type::Unknown);
        }
        let shown = sent.params();
        if let Some(param) = missing.iter().find(|param| !shown.contains(&param.name.as_str())) {
            return err(span, self.untold_type(task, &param.name));
        }
        if let Some(param) = missing.first() {
            self.unsettled_types.push((span, task.to_string(), param.name.clone()));
        }
        *written = packed_groups(
            info.types
                .iter()
                .zip(&places)
                .map(|(param, at)| (param.name.as_str(), (given.get(&param.name).cloned().unwrap_or_else(|| Type::Param(param.name.clone())), *at)))
                .collect(),
            &taken,
        );
        self.note_instance(span, task, written);
        Ok(sent)
    }

    /// Checks `flex& y = &x` or `fixed& y = &x`, which the parser made one name
    /// and one value, and declares `y`, another name for the place `&x` marks:
    /// one that changes it (`flex&`), or only reads it. `y` is of the place's
    /// type exactly, since what either writes the other reads.
    fn check_reference(&mut self, names: &mut [Bound], mutable: bool, init: &mut [Expr]) -> Result<()> {
        let word = if mutable { "flex&" } else { "fixed&" };
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
        // A `[flex @r]` lambda may be named by a reference, never copied.
        self.calling = matches!(place.kind, ExprKind::Var(_));
        let ty = self.check_expr(place);
        self.calling = false;
        let ty = ty?;
        if mutable {
            if let Err(error) = self.given_place(place, &Taker::Reference(&name)) {
                // A place that only can't change may still be read through one.
                return match (self.read_place(place), self.resolved_path(place)) {
                    (Ok(()), Some(path)) => Err(Error { message: format!("{}; 'fixed& {} = &{}' names it only to read it", error.message, name, path_text(&path)), ..error }),
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
        // A `fixed&` owns what its place is owned by; a `flex&` names a place
        // that changes, unless reached from a value made here.
        if mutable {
            self.hand_written(place, None);
        } else {
            let owners = self.owners(place);
            if !owners.is_empty() {
                self.bound_owners.push((at, owners));
            }
        }
        self.define(&name, ty, mutable, at)?;
        if let Some(info) = self.scopes.last_mut().and_then(|scope| scope.names.get_mut(&name)) {
            info.refers = Some(path);
        }
        Ok(())
    }

    /// Checks that `place`, already checked, can be read through `fixed& y`: a
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
            ExprKind::Field(base, field) if worked_out(base) => {
                refuse(&format!("it can't name '{}', which is worked out from the value, not kept in it", field))
            }
            ExprKind::Field(base, _) => self.read_place(base),
            ExprKind::Index(_, index) if matches!(index.kind, ExprKind::Range(..)) => refuse("it can't name a part taken of a value, which is a new value, not a place"),
            ExprKind::Index(base, _) if matches!(base.ty, Some(Type::Text(_))) => refuse("it can't name a byte of text, which never changes; copy the byte into a variable"),
            ExprKind::Index(base, _) if matches!(base.ty, Some(Type::Slice(..))) => refuse(
                "it can't name an item of a slice, which moves when the slice grows; hold the item's index, or copy the item into a variable",
            ),
            ExprKind::Index(base, _) if matches!(base.ty, Some(Type::Map(..))) => refuse(
                "it can't name a value in a map, which moves when the map grows; hold the key, or copy the value into a variable",
            ),
            ExprKind::Index(base, _) => self.read_place(base),
            ExprKind::Deref(_) | ExprKind::Shared { .. } => Ok(()),
            _ => refuse("it names a variable, a field or an item of one, or what a pointer points to, and not a value made here"),
        }
    }

    /// Checks `&x` where its address is read (`show(&x)`, `(&x).str()`,
    /// `"{&x}"`, `&a == &b`), and returns its type: a pointer to the place's
    /// type, which `show` and `str` write as an address. `x` is a place: a
    /// variable, a field or an item of one, what a pointer points to, or a
    /// shared field; a reference is the place it names.
    fn check_address(&mut self, e: &mut Expr) -> Result<Type> {
        let ExprKind::Ref(place) = &mut e.kind else { return Err(Error::internal_at(e.span, "an address of no place")) };
        let ty = self.check_expr(place)?;
        if ty.unknown() {
            // Read as unknown where it is used, as a method call's value is.
            e.ty = Some(Type::Unknown);
            return Ok(Type::Unknown);
        }
        if !self.is_place(place) {
            let what = no_place(place);
            return err(
                place.span,
                format!("'&' takes the address of a place: a variable, a field or an item of one, or what a pointer points to, and this is {}, with no place of its own", what),
            );
        }
        let written = place_path(place).map_or_else(|| "x".to_string(), |path| path_text(&path));
        self.warn_chosen(place, &format!("'&{}' is where the place is", written));
        let address = Type::Ptr(Box::new(ty), None);
        e.ty = Some(address.clone());
        Ok(address)
    }

    /// Whether `e` names a place `&` can take the address of (see
    /// `check_address`): a path from a variable or a shared field.
    fn is_place(&self, e: &Expr) -> bool {
        let mut at = e;
        loop {
            match &at.kind {
                ExprKind::Var(name) => return self.lookup(name).is_some(),
                ExprKind::Shared { .. } | ExprKind::Deref(_) => return true,
                // `xs.len` is worked out from the value, not kept in it.
                ExprKind::Field(base, _) if worked_out(base) => return false,
                ExprKind::Field(base, _) => at = base,
                ExprKind::Index(base, _) if matches!(base.ty, Some(Type::Text(_))) => return false,
                ExprKind::Index(base, index) if !matches!(index.kind, ExprKind::Range(..)) => at = base,
                _ => return false,
            }
        }
    }

    /// Checks `&a == &b`, or `&a == p` with `p` a pointer, at `span`: where
    /// two places are, or whether a pointer points at a place. Both are of one
    /// type, as a shape and its first field start at the same address and
    /// are still two places.
    fn check_address_compare(&mut self, l: &mut Expr, r: &mut Expr, span: Span) -> Result<Type> {
        let warned = self.warnings.len();
        let mut side_type = |side: &mut Expr| match side.kind {
            ExprKind::Ref(_) => self.check_address(side),
            _ => self.check_expr(side),
        };
        let (lt, rt) = (side_type(l)?, side_type(r)?);
        // Two parts of one parameter are where they are alike whichever way
        // it was given, so the answer doesn't rest on the choice.
        if let (ExprKind::Ref(a), ExprKind::Ref(b)) = (&l.kind, &r.kind)
            && same_root(a, b)
        {
            self.warnings.truncate(warned);
        }
        let (lt, rt) = (&lt, &rt);
        if lt.unknown() || rt.unknown() {
            return Ok(Type::Bool);
        }
        // The side without '&', which is a pointer or a mistake.
        for (side, ty) in [(&*l, lt), (&*r, rt)] {
            if matches!(side.kind, ExprKind::Ref(_)) {
                continue;
            }
            let hint = match ty {
                Type::Maybe(inner) if matches!(inner.as_ref(), Type::Ptr(..)) => "; a maybe is opened first, as in 'when m is {p}'".to_string(),
                Type::Ptr(..) => String::new(),
                _ => {
                    let example = place_path(side).map_or_else(|| "b".to_string(), |path| path_text(&path));
                    format!("; to compare where two places are, mark both, as in '&a == &{}'", example)
                }
            };
            if !hint.is_empty() {
                return err(side.span, format!("an address is compared with another place's address or with a pointer, but this value is {}{}", ty, hint));
            }
        }
        let place = |ty: &Type| match ty {
            Type::Ptr(inner, _) => inner.without_regions(),
            other => other.without_regions(),
        };
        let (left, right) = (place(lt), place(rt));
        if left != right {
            let pointed = match (&l.kind, &r.kind) {
                (ExprKind::Ref(_), ExprKind::Ref(_)) => None,
                (ExprKind::Ref(_), _) => Some((&right, &left)),
                _ => Some((&left, &right)),
            };
            return err(
                span,
                match pointed {
                    Some((to, place)) => format!("a pointer to {} never points at a place of type {}, so the two aren't compared", to, place),
                    None => format!("places of two types are never one place, even where they start at the same address (as a shape and its first field do): this compares a place of {} with one of {}", left, right),
                },
            );
        }
        Ok(Type::Bool)
    }

    /// The parameter `e` is, or is a field or an item of, when it is one the
    /// compiler chooses how to give (`x: T`): a copy, or, for a large value,
    /// where the caller keeps it (see `codegen::hidden_by_size`). Its name and
    /// type. A place a pointer points to is its own, however it was given.
    fn chosen_param(&self, e: &Expr) -> Option<(String, Type, Span)> {
        let (name, at) = param_root(e)?;
        let info = self.lookup(name)?;
        let chosen = info.param && info.passing == Passing::Copy && !info.mutable && info.ty != Type::Region;
        chosen.then(|| (name.to_string(), info.ty.clone(), at))
    }

    /// What `==` (or `!=`, as `symbol`) does with values of `ty`, compared by
    /// where they are, for a warning: a kind compares its variant by value and
    /// only what that carries by where it is.
    fn compared_by_place(&self, symbol: &str, ty: &Type) -> String {
        match ty {
            Type::Named(name, _) if self.kinds.contains_key(name) => format!("'{}' compares what {} carries by where it is", symbol, ty.without_regions()),
            _ => format!("'{}' compares {} by where it is", symbol, ty.without_regions()),
        }
    }

    /// `, in module 'm'` (or `, in the main file`) when `file`, where a use
    /// is, isn't `here`, the file a message about it is shown in; else nothing.
    fn file_named(&self, file: usize, here: usize) -> String {
        if file == here {
            return String::new();
        }
        match self.modules.values().find(|module| module.file == file) {
            Some(module) if !module.name.is_empty() => format!(" in module '{}'", module.name),
            _ => " in the main file".to_string(),
        }
    }

    /// Warns that `what`, at `at`, reads where a parameter the compiler chose
    /// how to give is (`chosen_param`), which the choice decides: a copy is a
    /// place of its own, and a large value is where the caller keeps it. The
    /// warning says which it is, and how to say which is meant.
    fn warn_chosen(&mut self, e: &Expr, what: &str) {
        let Some((name, ty, at)) = self.chosen_param(e) else { return };
        // A type a declaration takes has no size until a use gives it one,
        // where the task made for it warns (see `Made::reads_chosen`).
        if !ty.params().is_empty() {
            return;
        }
        let message = chosen_message(what, &name, &ty, &ty, self.size_of(&ty), None);
        self.warnings.push(Error::at(at, message));
    }

    /// How many bytes a value of `ty` takes in C (see `codegen::Layout`).
    fn size_of(&self, ty: &Type) -> usize {
        codegen::Layout { shapes: &self.shapes, kinds: &self.kinds }.of(ty).0
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
    /// and returns its type: a place marked `&` for a `flex&` parameter, and a
    /// value otherwise.
    fn check_argument(&mut self, arg: &mut Expr, passing: Passing, callee: &str, param: &str) -> Result<Type> {
        match (&mut arg.kind, passing) {
            (ExprKind::Ref(place), Passing::FlexRef | Passing::FixedRef) => {
                // A `[flex @r]` lambda is given as a place, never copied.
                self.calling = matches!(place.kind, ExprKind::Var(_));
                let ty = self.check_expr(place);
                self.calling = false;
                let ty = ty?;
                self.check_given_place(place, passing, callee, param)?;
                arg.ty = Some(ty.clone());
                Ok(ty)
            }
            (ExprKind::Ref(_), _) => err(
                arg.span,
                format!("{} takes a copy as '{1}', so leave out '&'; a parameter written 'flex& {1}: …' or 'fixed& {1}: …' is given the place itself", callee, param),
            ),
            (_, Passing::FixedRef | Passing::FlexRef) => {
                self.check_expr(arg)?;
                Err(unmarked_place(arg, passing, callee, param))
            }
            _ => self.check_expr(arg),
        }
    }

    /// Checks `place`, checked already, given with `&` to the `flex&` or
    /// `fixed&` parameter `param` of `callee`.
    fn check_given_place(&mut self, place: &Expr, passing: Passing, callee: &str, param: &str) -> Result<()> {
        match passing {
            Passing::FlexRef => self.given_place(place, &Taker::Param(param)),
            // A place only read: any place a `fixed&` reference may name.
            _ => {
                if !self.is_place(place) {
                    return err(
                        place.span,
                        format!("{} reads '{}' where the caller keeps it, so it takes a place: a variable, a field or an item of one, or what a pointer points to, and this is {}", callee, param, no_place(place)),
                    );
                }
                self.read_place(place).map_err(|error| Error { message: error.message.replacen("a reference names a place for as long as its block lasts", &format!("'{}' is read where the caller keeps it while the task runs", param), 1), ..error })
            }
        }
    }

    /// Checks that `place`, already checked, can be given to a `flex&`
    /// parameter or named by a `flex&` reference (see `Taker`):
    /// a place that can change, as an assignment writes to, other than an item
    /// of a slice, which may move when the slice grows.
    fn given_place(&self, place: &Expr, taker: &Taker) -> Result<()> {
        self.given_part(place, taker, &self.written_text(place))
    }

    /// `given_place` of `place`, part of the place `written`, as written.
    fn given_part(&self, place: &Expr, taker: &Taker, written: &str) -> Result<()> {
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
                    return refuse(format!("it needs a place that can change, and '{}' is a 'fixed&', which only reads the place it names", name));
                }
                if info.param && info.passing == Passing::FixedRef {
                    return refuse(format!(
                        "it needs a place that can change, and '{0}' is a 'fixed&' parameter, which only reads the caller's place; to pass it on to be changed, write 'flex& {0}: {1}'",
                        name, info.ty
                    ));
                }
                if info.param {
                    return refuse(format!(
                        "it needs a place that can change, and '{0}' is a parameter, which never does; to pass on the caller's place, write 'flex& {0}: {1}'",
                        name, info.ty
                    ));
                }
                refuse(format!("it needs a place that can change, and '{}' never does (declared at {}:{}); a value declared with 'flex' can", name, info.span.line, info.span.col))
            }
            ExprKind::Field(base, field) => {
                if matches!(&base.kind, ExprKind::Var(name) if name == THIS) && !self.lookup(THIS).is_some_and(|info| info.mutable) {
                    return Err(self.this_cannot_change(place.span, &taker.using(&format!("{}{}", MODULE_SEPARATOR, field))));
                }
                match &base.ty {
                    Some(Type::Tuple(_)) => return refuse("it needs a place that can change, and a tuple's items never do".to_string()),
                    Some(Type::Array(..) | Type::Slice(..) | Type::Map(..) | Type::Text(_)) => {
                        return refuse(format!("it needs a place that can change, and '{}' is worked out from the value, not kept in it", field));
                    }
                    Some(ty) => self.read_only_field(ty, field, place.span)?,
                    None => {}
                }
                self.given_part(base, taker, written)
            }
            ExprKind::Index(_, index) if matches!(index.kind, ExprKind::Range(..)) => {
                refuse("it needs a place that can change, and a part taken of a value is a new value, not a place".to_string())
            }
            ExprKind::Index(base, _) => match &base.ty {
                Some(Type::Slice(..)) => refuse(
                    "it can't be an item of a slice, which moves when the slice grows; give the slice and the item's index, or copy the item into a 'flex' variable, change it, and put it back"
                        .to_string(),
                ),
                Some(Type::Map(..)) => refuse(
                    "it can't be a value in a map, which moves when the map grows; give the map and the key, or copy the value into a 'flex' variable, change it, and put it back"
                        .to_string(),
                ),
                Some(Type::Text(_)) => refuse("it needs a place that can change, and text never does".to_string()),
                _ => self.given_part(base, taker, written),
            },
            ExprKind::Deref(pointer) => self.written_through(pointer, written, None),
            ExprKind::Shared { shape, field } => self.changeable_shared(shape, field, place.span, &format!("{}, and ", taker.lead())),
            _ => refuse("it needs a place that can change: a 'flex' variable, a field or array item of one, or what a pointer points to (p^), not a value made here".to_string()),
        }
    }

    /// `t ~= (1, none)`: a tuple or array written on one side becomes the
    /// other side's type where it can, as it becomes a written type in a
    /// binding (`(int, none)` a `(int, maybe int)`). The types of the two
    /// sides, after that.
    fn widen_literal(&mut self, l: &mut Expr, r: &mut Expr, lt: Type, rt: Type) -> (Type, Type) {
        let literal = |e: &Expr| matches!(e.kind, ExprKind::Tuple(_) | ExprKind::Array(_) | ExprKind::Repeat { .. });
        if lt.without_regions() == rt.without_regions() || lt.unknown() || rt.unknown() || literal(l) == literal(r) {
            return (lt, rt);
        }
        let (written, other) = if literal(r) { (r, &lt) } else { (l, &rt) };
        let before = written.clone();
        match self.coerce(written, other) {
            Ok(()) => (other.clone(), other.clone()),
            // Left as written, for the mismatch to be told as the two types.
            Err(_) => {
                *written = before;
                (lt, rt)
            }
        }
    }

    /// Rejects a call that gives one place twice where the task changes it:
    /// `swap(&a, &a)`, `f(a, &a)`, or `f(&a, &a.x)`, and a method that alters
    /// the value it is called on given a part of it as a `flex&`,
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
                (ExprKind::Ref(place), Passing::FlexRef) => (place.as_ref(), true),
                // Read where it is while the task runs, so no place given
                // beside it may change.
                (ExprKind::Ref(place), Passing::FixedRef) => (place.as_ref(), false),
                (_, Passing::Flex | Passing::Fixed) => continue,
                _ => (arg, i == 0 && alters),
            };
            let Some(path) = self.resolved_path(place) else { continue };
            for (before, before_changed, before_param, _) in &places {
                // A value given to a copy is read before the call starts, so a
                // method may change the value it is called on and be given a
                // part of it, `c.grow(c.n)`; only a `flex&` beside it names it.
                // A `fixed&` is read while the method runs, so it is no copy.
                let read_there = *passing == Passing::FixedRef;
                let receiver_and_copy = alters && !read_there && ((i != 0 && !changed && *before_param == THIS) || (i == 0 && !*before_changed));
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
                    let mut how = if changing == THIS { "the method changes the value it is called on".to_string() } else { format!("the call changes it through '{}'", changing) };
                    // The earlier one changes it: say where this one reads it.
                    if !changed {
                        how = format!("{} while it reads it through {}", how, name(param));
                    }
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
        let ty = ty.map_group_items(&mut |name| group_items(given, name));
        let mut ty = ty.map_params(&mut |name| given.get(name).cloned());
        self.settle_applications(&mut ty, at)?;
        // A task type given the caller's types names their regions its own way.
        if !given.is_empty() {
            ty = ty.normalized_tasks();
        }
        Ok(ty)
    }

    /// Rejects a call whose types would make the task keep a value of one region
    /// inside a value of another, which no value can do.
    fn check_kept_regions(&mut self, span: Span, task: &str, info: &TaskInfo, regions: &HashMap<String, String>, given: &HashMap<String, Type>) -> Result<()> {
        for ty in info.params.iter().map(|(_, ty)| ty.clone()).chain([info.ret.clone()]) {
            // Read before a kind or shape given types is made for them, which
            // keeps only its own region: `Option<text@s>@r` is in two.
            let ty = ty.map_regions(&mut |region| Some(regions.get(region).cloned().unwrap_or_else(|| ANY_REGION.to_string())));
            let ty = ty.map_group_items(&mut |name| group_items(given, name));
            let ty = ty.map_params(&mut |name| given.get(name).cloned());
            if let Some((outer, inner)) = region_conflict(&ty, None) {
                // A lambda that keeps what it uses is never copied.
                let fix = match has_closure(&ty) {
                    true => format!("make the lambda keep what it uses in '{0}', as in '[@{0}](…)'", outer),
                    false => format!("copy it into '{0}' first, as in 'value.copy() at {0}'", outer),
                };
                return err(
                    span,
                    format!("'{0}' would keep this in region '{1}' and in region '{2}', but everything a value in a region holds is in that region; {3}", unqualified(task), outer, inner, fix),
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
        let Some(&(need, _)) = param.needs.iter().find(|(need, _)| !self.allows(*need, ty)) else { return Ok(()) };
        // One of the types a call gives a group (see `group_call_info`), named
        // as the group is written and by the value it is for.
        if let Some((group, place)) = param.name.split_once(GROUP_ITEM) {
            let given = match ty {
                Type::Group(given) => format!("is given the group '{}...', which {}", given, self.why_not(need, ty)),
                _ => format!("value {} is {}, which {}", place.parse::<usize>().map_or(0, |i| i + 1), ty, self.why_not(need, ty)),
            };
            return err(at, format!("{} '{}' asks each value of its group '{}...' to be '{}', and {}", what, unqualified(task), group, need.word(), given));
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
        // A value of no type had a mistake of its own, already reported, as a
        // type nothing told is (see `untold_kinds`).
        if ty.unknown() || is_untold(ty) {
            return true;
        }
        // A group allows what it asks of each of its types.
        if let Type::Param(name) | Type::Group(name) = ty {
            return self.type_param(name).is_some_and(|param| param.needs.iter().any(|(own, _)| own.means(need)));
        }
        match need {
            Need::Ordered => ty.is_numeric() || matches!(ty, Type::Char | Type::Text(_)),
            Need::Shown => self.writable(ty),
            Need::Same => self.comparable(ty),
            Need::Similar => self.unsimilar_part(ty).is_none(),
            Need::Frozen => self.changeable_item(ty).is_none(),
        }
    }

    /// Whether `==` compares values of type `ty`, each by its own rule: a
    /// number, a bool, and a kind by value, text by its bytes, a pointer by the
    /// address it holds, a slice by its view (its first item and its length),
    /// and an array, a shape, and a tuple by where the value is (see
    /// `by_place`). A `maybe` is only compared with `none`, and a value that
    /// can fail is read first.
    fn comparable(&self, ty: &Type) -> bool {
        match ty {
            Type::Int | Type::Unit | Type::Char | Type::Float | Type::Bool | Type::Text(_) | Type::Ptr(..) | Type::Slice(..) | Type::Map(..) | Type::Array(..) | Type::Tuple(_) | Type::Task(_) => true,
            Type::Named(name, _) if self.kinds.contains_key(name) => self.kind_comparable(name, &mut Vec::new()),
            // `Option<T>` in a task that takes types: what its variants carry,
            // given those types, compares as a made kind's does.
            Type::Applied(applied) if self.kinds.contains_key(&applied.name) => self.applied_parts(applied).is_some_and(|parts| {
                parts.iter().all(|part| match part {
                    Type::Maybe(inner) => self.comparable(inner),
                    other => self.comparable(other),
                })
            }),
            Type::Named(..) | Type::Applied(_) => self.owner_of(ty).is_some_and(|owner| self.shapes.contains_key(&owner)),
            Type::Param(_) | Type::Group(_) => self.allows(Need::Same, ty),
            _ => false,
        }
    }

    /// Whether `==` compares values of the kind `name`: its variant, and each
    /// value a variant carries by that value's own rule, a `maybe` being both
    /// empty or both holding values that compare.
    fn kind_comparable(&self, name: &str, seen: &mut Vec<String>) -> bool {
        if seen.iter().any(|kind| kind == name) {
            return true;
        }
        let Some(kind) = self.kinds.get(name) else { return false };
        seen.push(name.to_string());
        let found = kind.variants.iter().flat_map(|variant| &variant.payload).all(|field| {
            let carried = match &field.ty {
                Type::Maybe(inner) => inner.as_ref(),
                other => other,
            };
            match carried {
                Type::Named(inner, _) if self.kinds.contains_key(inner) => self.kind_comparable(inner, seen),
                other => self.comparable(other),
            }
        });
        seen.pop();
        found
    }

    /// Whether `==` compares values of type `ty` by where they are, so both
    /// sides are places: an array, a shape, a tuple, and a kind that can carry
    /// one of those, whose variant is compared by value and what it carries by
    /// that value's own rule.
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
            Type::Applied(_) => true,
            _ => false,
        }
    }

    /// The first type in `ty`, itself included, whose values `~=` can't
    /// compare item by item: numbers, bools, text, pointers (by the address
    /// they hold, so nothing loops), and `maybe`s, arrays, slices, tuples,
    /// shapes (every field but shared ones), and kinds of those can.
    fn unsimilar_part(&self, ty: &Type) -> Option<Type> {
        self.unsimilar_in(ty, &mut Vec::new())
    }

    fn unsimilar_in(&self, ty: &Type, seen: &mut Vec<String>) -> Option<Type> {
        match ty {
            Type::Int | Type::Unit | Type::Char | Type::Float | Type::Bool | Type::Text(_) | Type::Ptr(..) | Type::Task(_) => None,
            Type::Maybe(inner) | Type::Array(inner, _) | Type::Slice(inner, _) => self.unsimilar_in(inner, seen),
            Type::Map(key, value, _) => self.unsimilar_in(key, seen).or_else(|| self.unsimilar_in(value, seen)),
            Type::Tuple(items) => items.iter().find_map(|item| self.unsimilar_in(item, seen)),
            Type::Param(_) | Type::Group(_) if self.allows(Need::Similar, ty) => None,
            // A shape or kind given types this declaration takes: its fields or
            // payloads with them.
            Type::Applied(applied) => self.applied_parts(applied)?.iter().find_map(|part| self.unsimilar_in(part, seen)),
            Type::Named(name, _) if seen.contains(name) => None,
            Type::Named(name, _) => {
                let parts: Vec<Type> = if let Some(shape) = self.shapes.get(name) {
                    shape.fields.iter().map(|field| field.ty.clone()).collect()
                } else if let Some(kind) = self.kinds.get(name) {
                    kind.variants.iter().flat_map(|variant| variant.payload.iter().map(|field| field.ty.clone())).collect()
                } else {
                    return Some(ty.clone());
                };
                seen.push(name.clone());
                let found = parts.iter().find_map(|part| self.unsimilar_in(part, seen));
                seen.pop();
                found
            }
            _ => Some(ty.clone()),
        }
    }

    /// Why a type doesn't allow what `need` asks, for the end of a message.
    fn why_not(&self, need: Need, ty: &Type) -> String {
        if let Type::Param(param) | Type::Group(param) = ty {
            // A group's needs are written after its dots, for `P...` and for
            // the type of one of its values, `P` alone or `P#0`.
            let declared = self.type_param(param);
            let group = matches!(ty, Type::Group(_)) || declared.is_some_and(|declared| declared.group);
            let name = match declared {
                Some(declared) if group => format!("{}...", declared.name),
                _ => param.clone(),
            };
            let owned: Vec<Need> = declared.map(|param| param.needs.iter().map(|(own, _)| *own).collect()).unwrap_or_default();
            let own: Vec<&str> = owned.iter().map(|own| own.word()).collect();
            // The word asked for replaces any it already means, as `ordered`
            // does `same`, since writing both is an error.
            let mut wanted: Vec<&str> = owned.iter().filter(|own| !need.means(**own)).map(|own| own.word()).collect();
            wanted.push(need.word());
            return match (own.is_empty(), group) {
                (false, false) => format!("is only '{}' here; require it too, as in '<{}: {}>'", own.join(" & "), name, wanted.join(" & ")),
                (true, false) => format!("is any type here; require it, as in '<{}: {}>'", name, need.word()),
                (false, true) => format!("asks only '{}' of its types here; require it too, as in '<{}: {}>'", own.join(" & "), name, wanted.join(" & ")),
                (true, true) => format!("asks nothing of its types here; require it, as in '<{}: {}>'", name, need.word()),
            };
        }
        match need {
            Need::Ordered => "has no order, so '<' can't compare it".to_string(),
            Need::Shown => "can't be shown".to_string(),
            Need::Same => "can't be compared with '=='".to_string(),
            Need::Similar => match self.unsimilar_part(ty) {
                Some(part) if part != *ty => format!("holds {}, which can't be compared item by item", part),
                _ => "can't be compared item by item".to_string(),
            },
            Need::Frozen => match self.changeable_item(ty) {
                Some(what) => format!("holds {}", what),
                None => "can change".to_string(),
            },
        }
    }

    /// The error for a type a call neither names nor shows in its arguments or
    /// in the place its value goes.
    fn untold_type(&self, task: &str, param: &str) -> String {
        // A call doesn't write the types of a task taking a group yet.
        if self.tasks.get(task).is_some_and(|info| info.types.iter().any(|param| param.group)) {
            return format!(
                "can't tell which type '{}' is for this call of '{}', since no argument and nothing this value goes into names one; a call of a task taking a group of types doesn't write its types yet, so give the value a place whose type names it, as in 'fixed x: int = {}(…)'",
                param,
                unqualified(task),
                unqualified(task)
            );
        }
        let count = self.tasks.get(task).map_or(1, |info| info.types.len().max(1));
        format!(
            "can't tell which type '{}' is for this call of '{}', since no argument and nothing this value goes into names one; write it, as in '{}<{}>(…)'",
            param,
            unqualified(task),
            unqualified(task),
            vec!["int"; count].join(", ")
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
        if self.noted_instances.insert(key.clone()) {
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
        self.settle_variant(expr, expected)?;
        // `flex b: unit = 200`: a number written out becomes a unit where one
        // is expected, alone, in a maybe, or as a success.
        let byte = match expected {
            Type::Maybe(inner) | Type::Or(inner, _) => inner,
            other => other,
        };
        if byte.is_byte() {
            byte_literal(expr, byte)?;
        }
        let actual = expr.ty()?.clone();
        if fits_regions(&actual, expected) {
            expr.ty = Some(expected.clone());
            return self.settle_copies(expr, expected);
        }
        // A plain task value where a lambda that keeps what it uses is
        // expected: it keeps only the task, in the region the type names.
        if let (Type::Task(task), Type::Closure(closure)) = (&actual, expected)
            && task.normalized_avoiding(&closure.region) == closure.task
        {
            // A task named here is kept where the program keeps it; a task
            // value, in the region.
            let region = match &expr.kind {
                ExprKind::TaskValue(_) => None,
                _ if closure.region == ANY_REGION => {
                    let shown = expected.map_regions(&mut |_| Some("r".to_string()));
                    return err(
                        expr.span,
                        format!(
                            "this task value goes where a lambda that keeps what it uses is expected, {0}, and keeps itself in a region, which no other value of this call names; give a task by its name, or keep this one first, as in 'fixed kept: {0} = …'",
                            shown
                        ),
                    );
                }
                _ => {
                    self.visible_region(&closure.region, expr.span, true)?;
                    Some(closure.region.clone())
                }
            };
            let value = std::mem::replace(expr, Expr::new(ExprKind::NoneLit, expr.span));
            *expr = Expr::new(ExprKind::Kept { value: Box::new(value), region }, expr.span);
            expr.ty = Some(expected.clone());
            return Ok(());
        }
        // A `match` or a `when` used as a value becomes what is expected in each
        // of its branches, since that is where its values are made: a branch
        // giving `none` beside one giving an `int` becomes part of the `maybe`
        // itself, rather than the whole being wrapped once, which would leave the
        // branches to run for nothing. Only what the branches can become is
        // pushed into them: where the type they settled on doesn't go where this
        // value does at all, the mistake is the whole one's, and a branch has
        // already become that settled type, so blaming its value would name a
        // type the branch doesn't say (`fixed n: int = when … 1 … none … fi`).
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
        // `n: int = v { none }`: the plan B made the whole a maybe, which has
        // no value where the plan B runs.
        if let (ExprKind::PlanB { value, .. }, Type::Maybe(held)) = (&expr.kind, &actual)
            && fits_regions(held, expected)
            && value.ty.as_ref().is_some_and(|ty| !matches!(ty, Type::Maybe(inner) | Type::Or(inner, _) if matches!(**inner, Type::Maybe(_))))
        {
            return err(
                expr.span,
                format!("this plan B can give none, so the whole is {}, with no value when the plan B runs, but {} is expected here; make the plan B give a value of type {}, or expect {}", actual, expected, expected, actual),
            );
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
            // `maybe (T ~ E)`: a value or a `fail e` becomes the `T ~ E`, which
            // the maybe then wraps, outside, as a join names it (see
            // `join_types`).
            Type::Maybe(inner) if matches!(**inner, Type::Or(..)) && fits(&actual, inner) => {
                self.coerce(expr, inner)?;
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
                if **value != Type::Nothing {
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
        // `2 ** n`: an int raised to an int that may be negative is a float.
        if actual == Type::Float
            && *expected == Type::Int
            && let ExprKind::Binary(BinaryOp::Pow, l, r) = &expr.kind
            && l.ty == Some(Type::Int)
            && r.ty == Some(Type::Int)
        {
            return err(
                expr.span,
                "expected type int, got float: an int raised to an int that may be negative is a float; raise it to a unit, as in 'n ** unit(k)', or to a constant 0 or more, as in 'n ** 2', for an int",
            );
        }
        err(expr.span, format!("expected type {}, got {}{}", expected, actual, mismatch_hint(&actual, expected)))
    }

    /// Gives a call of a generic task the types no argument named, from the type
    /// of the place its value goes: `fixed a: maybe int = empty()` says that
    /// `nothing`'s `T` is `int`. A call whose types are all known already, and
    /// any other expression, is left as it is.
    fn settle_types(&mut self, expr: &mut Expr, expected: &Type) -> Result<()> {
        let ExprKind::Call { name, types, .. } = &expr.kind else { return Ok(()) };
        if types.iter().all(|(ty, _)| ty.params().is_empty()) {
            return Ok(());
        }
        let name = name.clone();
        let sent = expr.ty.clone().unwrap_or(Type::Nothing);
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
        // Some are still untold: the call waits for them, under the name of
        // the first.
        if let Some(untold) = types.iter().find_map(|(ty, _)| ty.params().first().map(|param| param.to_string())) {
            if let Some(entry) = self.unsettled_types.iter_mut().rev().find(|(at, _, _)| *at == expr.span) {
                entry.2 = untold;
            }
            return Ok(());
        }
        if let Some(at) = self.unsettled_types.iter().rposition(|(at, _, _)| *at == expr.span) {
            self.unsettled_types.remove(at);
        }
        // Every type known now: a shape or kind given them is the one made
        // for them, as a call whose arguments told them gets (see `settled_type`).
        if let Some(ty) = &mut expr.ty {
            self.settle_applications(ty, expr.span)?;
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
    /// become the items of `expected`, as `[1, none]` or `[none#4]` become
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

/// The type of a plan B whose block gives `given` where a value of type
/// `extracted` was expected: a `maybe` of it when the block gives `none` or a
/// `maybe`, since the whole then has no value when both are empty. A plan B on
/// a value holding a `maybe` already gives one anyway, so nothing changes.
fn gives_a_maybe(extracted: &Type, given: &Type) -> Option<Type> {
    let plain = !matches!(extracted, Type::Maybe(_) | Type::Nothing) && !extracted.unknown();
    (plain && matches!(given, Type::None | Type::Maybe(_))).then(|| Type::Maybe(Box::new(extracted.clone())))
}

/// How a wrapper takes the values of a task it wraps (see
/// `Checker::wrapper_tail`).
struct WrapperTail {
    /// How many values of its own it takes, after the next task.
    own: usize,
    /// Each parameter of the wrapper that takes a value of the task alone,
    /// with the place of that value among the task's.
    pairs: Vec<(usize, usize)>,
    /// The place of its group among its parameters, for one taking a group.
    group: Option<usize>,
}

/// What the arguments of a call show its task's types and regions to be (see
/// `Checker::check_call_args`).
struct CallTypes {
    /// Each type the call gives: written after the task's name, or worked out
    /// from the arguments.
    given: HashMap<String, Type>,
    /// What the arguments show each type to be, kept apart from the types
    /// written at the call so one written without its region is reported as
    /// that, rather than as an argument of the wrong type.
    shown: HashMap<String, Type>,
    /// The region each region the header names is.
    regions: HashMap<String, String>,
    /// How a task value the call gives takes each value of a group the call
    /// gives (`P#0`, …): as a value, a place, or a region.
    taken: HashMap<String, Taken>,
    /// Whether a task value the call gives has a type not known, which a
    /// mistake in it left.
    untold: bool,
}

/// The mistake in `arg`, given without `&` to the `flex&` or `fixed&` parameter
/// `param` of `callee`.
fn unmarked_place(arg: &Expr, passing: Passing, callee: &str, param: &str) -> Error {
    let message = match (place_path(arg), passing) {
        (Some(path), Passing::FixedRef) => format!("{} reads '{}' where the caller keeps it, so the call marks that place with '&', as in '&{}'", callee, param, path_text(&path)),
        (None, Passing::FixedRef) => {
            format!("{} reads '{}' where the caller keeps it, so it takes a place marked with '&', as in '&a', and this is a value made here; a parameter written '{}: …' takes a copy", callee, param, param)
        }
        (Some(path), _) => format!("{} changes '{}', the place it is given, so the call marks that place with '&', as in '&{}'", callee, param, path_text(&path)),
        (None, _) => format!("{} changes '{}', the place it is given, so it takes a place that can change, marked with '&', as in '&a', and this is a value made here", callee, param),
    };
    Error::at(arg.span, message)
}

/// What `Checker::task_call_setup` found for a call.
enum CallSetup {
    /// A call checked already, of this type: one of `show` or a number cast,
    /// or of a task whose header has a mistake.
    Checked(Type),
    /// A call of a task, whose arguments are checked against its header, and
    /// the group it takes, with how many values the call gives it.
    Call(TaskInfo, Option<(String, usize)>),
}

/// `error`, in a call giving `group` its values, told how many types the
/// group is for that call when the error is of a value's type.
fn group_hint(mut error: Error, group: Option<&(String, usize)>) -> Error {
    if let Some((group, given)) = group
        && error.message.starts_with("expected type")
    {
        error.message.push_str(&format!("; this call gives the group '{}...' {}, so the group is {}", group, count(*given, "value"), count(*given, "type")));
    }
    error
}

/// The error for `(i < n, n > (m))`, which reads as a call of `i` given the
/// types `n, n`: a `<` after a name whose `>` is followed by `(` starts types.
fn misread_comparison(name: &str) -> String {
    format!(
        "'{0}' is a value, but '{0} < …, … > (…)' reads as a call of '{0}' given the types between '<' and '>'; to compare, put the comparison before the ',' in parentheses, as in '({0} < …), … > (…)'",
        name
    )
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
        Some(Type::Or(value, error)) if **value == Type::Nothing => {
            format!("; a 'nothing ~ {}' has no value, only an error to handle, as in 'x {{ failure e … }}'", error)
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
            Item::Task(_) | Item::Macro(_) => Vec::new(),
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
        (Type::Map(ak, av, ar), Type::Map(ek, ev, er)) => region(ar, er) && fits_regions(ak, ek) && fits_regions(av, ev),
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
        // A lambda that only reads what it keeps goes where one that changes
        // it is expected, never the other way.
        (Type::Closure(a), Type::Closure(e)) => a.region == e.region && a.task == e.task && (!a.flex || e.flex),
        _ => actual == expected,
    }
}

/// Whether a statement on the last line of a block gives that block its value: a
/// `match`, whose cases cover every value, or a `when` with an `else`, so that
/// every way through it gives one. A `when` without an `else` can't, so such a
/// block must send on every path, which is what its message says.
pub fn gives_value(stmt: &Stmt) -> bool {
    matches!(&stmt.kind, StmtKind::Match(..) | StmtKind::When(_, Some(_)))
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
        Type::Map(key, value, region) => {
            let inside = region.as_deref().or(inside);
            region.as_deref().and_then(conflict).or_else(|| region_conflict(key, inside)).or_else(|| region_conflict(value, inside))
        }
        Type::Text(Some(region)) | Type::Named(_, Some(region)) => conflict(region),
        Type::Applied(applied) => match applied.region.as_deref() {
            Some(region) => conflict(region).or_else(|| applied.types.iter().find_map(|ty| region_conflict(ty, Some(region)))),
            None => applied.types.iter().find_map(|ty| region_conflict(ty, inside)),
        },
        Type::Maybe(inner) | Type::Array(inner, _) | Type::Fail(inner) | Type::Range(inner) => region_conflict(inner, inside),
        Type::Or(value, error) => region_conflict(value, inside).or_else(|| region_conflict(error, inside)),
        Type::Tuple(types) | Type::Results(types) => types.iter().find_map(|ty| region_conflict(ty, inside)),
        // A lambda that keeps what it uses keeps it in its region.
        Type::Closure(closure) => conflict(&closure.region),
        _ => None,
    }
}

/// How deeply a type is built up: `int` is 1, `maybe int` 2, `(int, maybe int)` 3.
/// How many types `ty` is built of, itself included, counting up to a little
/// past `MAX_TYPE_SIZE` only.
fn type_size(ty: &Type) -> usize {
    let mut count = 0;
    let mut pending = vec![ty];
    while let Some(ty) = pending.pop() {
        count += 1;
        if count > MAX_TYPE_SIZE {
            break;
        }
        match ty {
            Type::Ptr(inner, _) | Type::Slice(inner, _) | Type::Array(inner, _) | Type::Range(inner) | Type::Maybe(inner) | Type::Fail(inner) => pending.push(inner),
            Type::Or(value, error) | Type::Map(value, error, _) => pending.extend([&**value, &**error]),
            Type::Applied(applied) => pending.extend(&applied.types),
            Type::Tuple(types) | Type::Results(types) => pending.extend(types),
            Type::Task(task) => {
                pending.extend(&task.params);
                pending.push(&task.ret);
            }
            Type::Closure(closure) => {
                pending.extend(&closure.task.params);
                pending.push(&closure.task.ret);
            }
            _ => {}
        }
    }
    count
}

fn type_depth(ty: &Type) -> usize {
    let inner = match ty {
        Type::Ptr(inner, _) | Type::Slice(inner, _) | Type::Array(inner, _) | Type::Range(inner) | Type::Maybe(inner) | Type::Fail(inner) => {
            type_depth(inner)
        }
        Type::Or(value, error) | Type::Map(value, error, _) => type_depth(value).max(type_depth(error)),
        Type::Applied(applied) => applied.types.iter().map(type_depth).max().unwrap_or(0),
        Type::Tuple(types) | Type::Results(types) => types.iter().map(type_depth).max().unwrap_or(0),
        Type::Task(task) => task.params.iter().chain([&task.ret]).map(type_depth).max().unwrap_or(0),
        Type::Closure(closure) => closure.task.params.iter().chain([&closure.task.ret]).map(type_depth).max().unwrap_or(0),
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
        Type::Map(key, value, region) => {
            keeps_params_of(key, region.as_deref().or(inside), kept);
            keeps_params_of(value, region.as_deref().or(inside), kept);
        }
        Type::Array(inner, _) | Type::Range(inner) | Type::Maybe(inner) | Type::Fail(inner) => keeps_params_of(inner, inside, kept),
        Type::Or(value, error) => {
            keeps_params_of(value, inside, kept);
            keeps_params_of(error, inside, kept);
        }
        Type::Tuple(types) | Type::Results(types) => types.iter().for_each(|ty| keeps_params_of(ty, inside, kept)),
        // `Box<T>@r`: the types a shape is given are in its region.
        Type::Applied(applied) => applied.types.iter().for_each(|ty| keeps_params_of(ty, applied.region.as_deref().or(inside), kept)),
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
    /// Each `==` or `!=` that the types given make compare by where values are
    /// (see `Checker::by_place`) with a side that is no place, and the type.
    placeless: Vec<(Span, Type)>,
    /// The parameters the compiler chooses how to give (`x: T`), with their
    /// types as the use gives them and as the task writes them (see
    /// `Checker::chosen_param`).
    chosen: HashMap<String, (Type, Type)>,
    /// Each side of an `==` by place, or an address, that reads where such a
    /// parameter is: where the parameter is named, what reads it, and its
    /// name.
    reads_chosen: Vec<(Span, String, String)>,
    /// Each read of a group's tuple item past the values the use gives: where,
    /// the item, and how many values there are (see `substitute_expr`).
    past_items: Vec<(Span, usize, usize)>,
}

impl Checker {
    /// Puts the types `given` in place of the type parameters of `task`, a copy
    /// of a generic task made for one use, points its calls of generic tasks at
    /// the ones made for them, and puts the shape made for its types in place of
    /// every shape given them (see `Made` and `settle_applications`). With no
    /// types given it changes nothing, and only fills `made`, which is how the
    /// checker reads a generic task's copies.
    fn substitute_task(&mut self, task: &mut TaskDef, given: &HashMap<String, Type>, made: &mut Made) -> Result<()> {
        // `args: P...` becomes a parameter for each type given for the group,
        // `args__1`, `args__2`, … (see `group_value`).
        // A value the group holds as a place is a parameter taking the place.
        if let Some(at) = task.params.iter().position(|param| matches!(param.ty, Type::Group(_)))
            && let Some(Field { ty: Type::Group(group), .. }) = task.params.get(at)
            && let Some((types, taken)) = group_items(given, group)
        {
            let param = task.params.remove(at);
            let values = types.into_iter().zip(taken).enumerate().map(|(i, (ty, how))| Field { name: group_value(&param.name, i), ty, passing: task_passing(&how), ..param.clone() });
            task.params.splice(at..at, values);
        }
        let alters = task.owner.as_ref().is_some_and(|owner| owner.alters);
        for param in &mut task.params {
            // A parameter of a type the task doesn't take has warned already,
            // when the body was checked (see `warn_chosen`).
            let takes = !param.ty.params().is_empty();
            let declared = param.ty.clone();
            self.substitute_type(&mut param.ty, given, param.ty_span)?;
            if takes && param.passing == Passing::Copy && param.ty != Type::Region && !(alters && param.name == THIS) {
                made.chosen.insert(param.name.clone(), (param.ty.clone(), declared));
            }
        }
        let at = task.span;
        self.substitute_type(&mut task.ret, given, at)?;
        self.substitute_block(&mut task.body, given, made)
    }

    /// One type of a task made for a set of types: the types given in place of
    /// the names it takes, and the shape made for them in place of a shape given
    /// them.
    /// A written type in a copy, as `in_copy` makes it.
    fn substitute_type(&mut self, ty: &mut Type, given: &HashMap<String, Type>, at: Span) -> Result<()> {
        *ty = in_copy(ty, given);
        self.settle_applications(ty, at)
    }

    fn substitute_block(&mut self, block: &mut Block, given: &HashMap<String, Type>, made: &mut Made) -> Result<()> {
        block.iter_mut().try_for_each(|stmt| self.substitute_stmt(stmt, given, made))
    }

    fn substitute_stmt(&mut self, stmt: &mut Stmt, given: &HashMap<String, Type>, made: &mut Made) -> Result<()> {
        let at = stmt.span;
        match &mut stmt.kind {
            StmtKind::Expr(e) => self.substitute_expr(e, given, made),
            StmtKind::Step { target, value, .. } => {
                self.substitute_expr(target, given, made)?;
                self.substitute_expr(value, given, made)
            }
            StmtKind::Send(values) | StmtKind::SetResults(_, values) => {
                values.iter_mut().try_for_each(|value| self.substitute_expr(value, given, made))
            }
            StmtKind::Break(_, Some(value)) => self.substitute_expr(value, given, made),
            StmtKind::Region { .. } | StmtKind::Break(..) | StmtKind::Continue(_) => Ok(()),
            StmtKind::Named(_, inner) => self.substitute_stmt(inner, given, made),
            StmtKind::Turns(turns) => turns.iter_mut().try_for_each(|turn| self.substitute_block(turn, given, made)),
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
            StmtKind::When(arms, else_block) => {
                // A condition this copy knows, as one on 'args.len' is: a branch
                // it never takes is emptied, so a read it guards against is never
                // checked or written. The `when` stays, so one used as a value
                // still gives it, from the branch that runs. The arms after one
                // it knows holds are dropped before anything is done to them, as
                // their conditions may read what only that arm guards against.
                let (mut taken, mut cut) = (false, arms.len());
                for (i, arm) in arms.iter_mut().enumerate() {
                    self.substitute_condition(&mut arm.cond, &mut arm.body, given, made)?;
                    match known_condition(&arm.cond).filter(|_| !given.is_empty()) {
                        Some(false) => arm.body.clear(),
                        Some(true) => taken = true,
                        None => {}
                    }
                    self.substitute_block(&mut arm.body, given, made)?;
                    if taken {
                        cut = i + 1;
                        break;
                    }
                }
                if taken {
                    arms.truncate(cut);
                }
                if taken && let Some(dropped) = else_block {
                    dropped.clear();
                }
                match else_block {
                    Some(else_block) => self.substitute_block(else_block, given, made),
                    None => Ok(()),
                }
            }
            StmtKind::Block(body) => self.substitute_block(body, given, made),
            StmtKind::Loop(cond, body) => {
                self.substitute_condition(cond, body, given, made)?;
                self.substitute_block(body, given, made)
            }
            StmtKind::Parted(parted) => {
                self.substitute_block(&mut parted.start, given, made)?;
                if let Some(cond) = &mut parted.cond {
                    self.substitute_condition(cond, &mut parted.body, given, made)?;
                }
                self.substitute_block(&mut parted.step, given, made)?;
                self.substitute_block(&mut parted.body, given, made)
            }
            StmtKind::Each(name, _, range, body) if matches!(&range.ty, Some(Type::Group(group)) if group_types(given, group).is_some()) => {
                let (name, range, body) = (name.clone(), range.clone(), std::mem::take(body));
                stmt.kind = self.each_of_group(&name, &range, body, given, made, at)?;
                Ok(())
            }
            StmtKind::Each(_, _, range, body) => {
                self.substitute_expr(range, given, made)?;
                self.substitute_block(body, given, made)
            }
            StmtKind::Match(subject, cases) => {
                self.substitute_expr(subject, given, made)?;
                cases.iter_mut().try_for_each(|case| self.substitute_block(&mut case.body, given, made))
            }
        }
    }

    /// In a copy of a task, makes `e` what this copy's group gives, when `e`
    /// is the group's tuple, `(args...)`, or an item of one, `t.k` or `t.#k`,
    /// and says whether it did. The tuple is the values for two or more of
    /// them, and for one or none a failure holding the one value or none; an
    /// item is the value, and one past the values given is a mistake of the use
    /// (see `Made::past_items`), or `none` when read as `t.#k`.
    fn substitute_group_tuple(&mut self, e: &mut Expr, given: &HashMap<String, Type>, made: &mut Made) -> Result<bool> {
        let span = e.span;
        let ExprKind::Field(inner, member) = &mut e.kind else { return Ok(false) };
        let group = match &inner.ty {
            Some(Type::Group(group)) if member == GROUP_TUPLE => group.clone(),
            Some(Type::Tuple(items)) if group_in_tuple(items).is_some() => group_in_tuple(items).unwrap_or_default(),
            _ => return Ok(false),
        };
        let Some(types) = group_types(given, &group) else { return Ok(false) };
        let generic = e.ty.clone().unwrap_or(Type::Unknown);
        let ty = in_copy(&generic, given);
        if member == GROUP_TUPLE {
            let ExprKind::Var(values) = &inner.kind else { return Err(Error::internal_at(span, "a group's tuple made of no group's values")) };
            let value = |place: usize, ty: &Type| {
                let mut value = Expr::new(ExprKind::Var(group_value(values, place)), span);
                value.ty = Some(ty.clone());
                value
            };
            let typed = |kind: ExprKind, ty: Type| {
                let mut made = Expr::new(kind, span);
                made.ty = Some(ty);
                made
            };
            let made_value = match types.as_slice() {
                [] | [_] => {
                    let one = match types.first() {
                        Some(first) => typed(ExprKind::Wrap(Box::new(value(0, first))), Type::Maybe(Box::new(first.clone()))),
                        None => typed(ExprKind::Wrap(Box::new(typed(ExprKind::NoneLit, Type::None))), Type::Maybe(Box::new(placeholder_type()))),
                    };
                    let failed = one.ty.clone().unwrap_or(Type::Unknown);
                    typed(ExprKind::Fail(Box::new(one)), Type::Fail(Box::new(failed)))
                }
                _ => {
                    let values: Vec<Expr> = types.iter().enumerate().map(|(place, ty)| value(place, ty)).collect();
                    typed(ExprKind::Tuple(values), Type::Tuple(types.clone()))
                }
            };
            e.kind = ExprKind::Wrap(Box::new(made_value));
            e.ty = Some(ty);
            return Ok(true);
        }
        let safe = member.starts_with(SAFE_ITEM);
        let Ok(place) = member.trim_start_matches(SAFE_ITEM).parse::<usize>() else { return Ok(false) };
        let count = types.len();
        self.substitute_expr(inner, given, made)?;
        // The tuple is made only for two or more values, so in a copy for fewer
        // its items are never read, and stand in as they are.
        let there = place < count && count >= 2;
        if !safe {
            if place >= count.max(2) {
                made.past_items.push((span, place, count));
            }
            e.ty = Some(ty);
            return Ok(true);
        }
        let item = types.get(place).cloned().filter(|_| there);
        let mut read = Expr::new(ExprKind::Field(inner.clone(), place.to_string()), span);
        match item {
            Some(item) => {
                read.ty = Some(item);
                e.kind = ExprKind::Wrap(Box::new(read));
            }
            None => {
                read.kind = ExprKind::Field(inner.clone(), member.clone());
                read.ty = Some(Type::None);
                e.kind = ExprKind::Wrap(Box::new(read));
            }
        }
        e.ty = Some(ty);
        Ok(true)
    }

    /// `each v in args` in a copy of a task, for `range`, a group's values: the
    /// body once for each value, in a block of its own giving `v` that value,
    /// of its own type (`P` in the body). The blocks are still one loop, which
    /// a `break` in them stops (see `StmtKind::Turns`).
    fn each_of_group(&mut self, name: &str, range: &Expr, body: Block, given: &HashMap<String, Type>, made: &mut Made, at: Span) -> Result<StmtKind> {
        let (Some(Type::Group(group)), ExprKind::Var(values)) = (&range.ty, &range.kind) else {
            return Err(Error::internal_at(at, "a loop over a group's values that names none"));
        };
        let mut turns = Vec::new();
        for (place, ty) in group_types(given, group).unwrap_or_default().into_iter().enumerate() {
            let mut turn_given = given.clone();
            turn_given.insert(format!("{}{}", group, GROUP_TURN), ty.clone());
            let mut turn_body = body.clone();
            self.substitute_block(&mut turn_body, &turn_given, made)?;
            let bound = Bound { name: name.to_string(), span: at, ty: Some(ty.clone()), ty_span: None, parts: Vec::new() };
            let mut value = Expr::new(ExprKind::Var(group_value(values, place)), at);
            value.ty = Some(ty);
            let mut turn = vec![Stmt::new(StmtKind::Binding { names: vec![bound], mutable: false, reference: false, init: vec![value] }, at)];
            turn.extend(turn_body);
            turns.push(turn);
        }
        Ok(StmtKind::Turns(turns))
    }

    fn substitute_bound(&mut self, bound: &mut Bound, given: &HashMap<String, Type>, at: Span) -> Result<()> {
        if let Some(ty) = &mut bound.ty {
            *ty = in_copy(ty, given);
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

    /// Records the `str`s writing `value` as text calls, once its type is the
    /// one a use gives (see `written_strs`).
    fn need_written_strs(&self, value: &Expr, made: &mut Made) {
        let Some(ty) = &value.ty else { return };
        let mut found = Vec::new();
        self.written_strs(ty, &mut Vec::new(), &mut found);
        made.needed.extend(found.into_iter().map(|(method, types)| (method, types, value.span)));
    }

    /// Makes `cond`, guarding `body`, what this copy makes it, a part (joined
    /// with `and`) at a time with its type tests settled: after a part the copy
    /// knows doesn't hold, the parts after it, which may read what it guards
    /// against, are never made and `body` is emptied. What the parts before it
    /// do still runs, so the condition is `false` only where they do nothing.
    fn substitute_condition(&mut self, cond: &mut Condition, body: &mut Block, given: &HashMap<String, Type>, made: &mut Made) -> Result<()> {
        let mut done = 0;
        while let Some(current) = cond.part_mut(done) {
            self.substitute_expr(&mut current.value, given, made)?;
            self.settle_type_tests(cond, body);
            done += 1;
            let Some(last) = cond.parts().take(done).position(|part| known_part(part) == Some(false)).filter(|_| !given.is_empty()) else { continue };
            // The part known not to hold is made already, and works out to
            // `false` where it runs.
            if cond.parts().take(last + 1).any(|part| !without_effects(&part.value))
                && let Some(part) = cond.part_mut(last)
            {
                part.and = None;
            } else {
                let mut never = Expr::new(ExprKind::BoolLit(false), cond.value.span);
                never.ty = Some(Type::Bool);
                *cond = Condition::new(never, None);
            }
            body.clear();
            break;
        }
        Ok(())
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
        // A group's tuple, and its items, as this copy's values make them.
        if self.substitute_group_tuple(e, given, made)? {
            return Ok(());
        }
        if let Some(ty) = &mut e.ty {
            *ty = in_copy(ty, given);
            self.settle_applications(ty, at)?;
        }
        // `a and b` where this copy knows `a` is false, or `a or b` where it
        // knows it is true: `b` never runs, and may read what `a` guards
        // against, so it isn't made.
        if let ExprKind::Binary(op @ (BinaryOp::And | BinaryOp::Or), a, _) = &mut e.kind {
            let decides = *op == BinaryOp::Or;
            self.substitute_expr(a, given, made)?;
            if !given.is_empty() && known_bool(a) == Some(decides) {
                // What `a` does still runs.
                if without_effects(a) {
                    e.kind = ExprKind::BoolLit(decides);
                } else if let ExprKind::Binary(_, _, b) = &mut e.kind {
                    let mut known = Expr::new(ExprKind::BoolLit(decides), b.span);
                    known.ty = Some(Type::Bool);
                    **b = known;
                }
            } else if let ExprKind::Binary(_, _, b) = &mut e.kind {
                self.substitute_expr(b, given, made)?;
            }
            return Ok(());
        }
        match &mut e.kind {
            ExprKind::Spread(inner) | ExprKind::Kept { value: inner, .. } => self.substitute_expr(inner, given, made),
            // No lambda is written in a task that takes types.
            ExprKind::Closure { .. } => Ok(()),
            ExprKind::Call { name, types, args } => {
                // A group's types, kept as a tuple, are as many as the call gives
                // it; any other tuple of a group's values is at least two items
                // long, as this copy makes the values (see `in_copy`).
                let groups: Vec<bool> = self.tasks.get(name.as_str()).map(|info| info.types.iter().map(|param| param.group).collect()).unwrap_or_default();
                for (i, (ty, _)) in types.iter_mut().enumerate() {
                    *ty = match groups.get(i) {
                        Some(true) => group_given_in_copy(ty, given),
                        _ => in_copy(ty, given),
                    };
                    self.settle_applications(ty, at)?;
                }
                if !types.is_empty() {
                    made.needed.push((name.clone(), types.iter().map(|(ty, _)| ty.clone()).collect(), at));
                }
                args.iter_mut().try_for_each(|arg| self.substitute_expr(arg, given, made))?;
                spread_values(args, given, name != "show");
                // `show(v)`, with `v` of a type given now: the `str`s it calls.
                if name == "show" {
                    for arg in args.iter() {
                        self.need_written_strs(arg, made);
                        note_chosen_address(arg, made);
                    }
                }
                Ok(())
            }
            ExprKind::ToText { value, .. } => {
                self.substitute_expr(value, given, made)?;
                self.need_written_strs(value, made);
                note_chosen_address(value, made);
                Ok(())
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
            ExprKind::TypeOf(ty) => {
                self.substitute_type(ty, given, at)?;
                if ty.params().is_empty() {
                    e.kind = ExprKind::TextLit(self.written_type(ty));
                }
                Ok(())
            }
            ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::CharLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) | ExprKind::NoneLit | ExprKind::Macro { .. } | ExprKind::Var(_) | ExprKind::RegionAfter(_) | ExprKind::Shared { .. } | ExprKind::TaskValue(_) => Ok(()),
            ExprKind::Invoke { callee, args } => {
                self.substitute_expr(callee, given, made)?;
                args.iter_mut().try_for_each(|arg| self.substitute_expr(arg, given, made))?;
                spread_values(args, given, true);
                Ok(())
            }
            // `args.len`: how many values this copy's group holds.
            ExprKind::Field(inner, member)
                if member == "len" && matches!(&inner.ty, Some(Type::Group(group)) if group_types(given, group).is_some()) =>
            {
                let count = match &inner.ty {
                    Some(Type::Group(group)) => group_types(given, group).map_or(0, |types| types.len()),
                    _ => 0,
                };
                // Written `count + 0`, which no copy checks again: the C then
                // checks an index or a sum it takes part in as it runs, as for
                // any value, rather than trusting the checker had (see
                // `codegen::int_constant`). A condition still knows it.
                let int = |n: i64| {
                    let mut lit = Expr::new(ExprKind::IntLit(n), e.span);
                    lit.ty = Some(Type::Int);
                    Box::new(lit)
                };
                e.kind = ExprKind::Binary(BinaryOp::Add, int(i64::try_from(count).unwrap_or(i64::MAX)), int(0));
                Ok(())
            }
            ExprKind::Unary(_, inner)
            | ExprKind::Deref(inner)
            | ExprKind::Ref(inner)
            | ExprKind::Field(inner, _)
            | ExprKind::Fail(inner)
            | ExprKind::Wrap(inner)
            | ExprKind::Copy { value: inner, .. }
            | ExprKind::Shortcut { value: inner, .. } => self.substitute_expr(inner, given, made),
            ExprKind::Pairs(pairs) => pairs.iter_mut().try_for_each(|(key, value)| {
                self.substitute_expr(key, given, made)?;
                self.substitute_expr(value, given, made)
            }),
            ExprKind::MapOp { map, key, .. } => {
                self.substitute_expr(map, given, made)?;
                key.as_deref_mut().map_or(Ok(()), |key| self.substitute_expr(key, given, made))
            }
            ExprKind::Binary(op @ (BinaryOp::Eq | BinaryOp::Ne), a, b) => {
                let symbol = if *op == BinaryOp::Ne { "!=" } else { "==" };
                self.substitute_expr(a, given, made)?;
                self.substitute_expr(b, given, made)?;
                // Where the body compares by place only for the types given:
                // it was checked once, when any type could be given.
                if let Some(ty) = a.ty.clone()
                    && !matches!(a.kind, ExprKind::Ref(_))
                    && !matches!(b.kind, ExprKind::Ref(_))
                    && self.by_place(&ty)
                    && !(names_place(a) && names_place(b))
                {
                    made.placeless.push((at, ty.clone()));
                }
                // A side that reads where a parameter the compiler chose how
                // to give is, now that its type is known.
                let places = [&**a, &**b].map(|side| match &side.kind {
                    ExprKind::Ref(place) => place.as_ref(),
                    _ => side,
                });
                if same_root(places[0], places[1]) {
                    // Two parts of one parameter: alike whichever way it was given.
                } else if [&**a, &**b].iter().any(|side| matches!(side.kind, ExprKind::Ref(_))) {
                    note_chosen_address(a, made);
                    note_chosen_address(b, made);
                } else if let Some(ty) = &a.ty
                    && self.by_place(ty)
                {
                    let what = self.compared_by_place(symbol, ty);
                    for side in [&**a, &**b] {
                        if let Some((name, at)) = param_root(side).filter(|(name, _)| made.chosen.contains_key(*name)) {
                            made.reads_chosen.push((at, what.clone(), name.to_string()));
                        }
                    }
                }
                Ok(())
            }
            ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) | ExprKind::Lookup(a, b) | ExprKind::Range(a, b) | ExprKind::Repeat { value: a, count: b } => {
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
            ExprKind::LoopValue(value) => self.substitute_stmt(&mut value.stmt, given, made),
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
    if *actual == Type::None || matches!(actual, Type::Fail(_)) || is_untold(actual) {
        return;
    }
    match (pattern, actual) {
        (Type::Param(name), _) => {
            given.entry(name.clone()).or_insert_with(|| actual.clone());
        }
        (Type::Ptr(p, _), Type::Ptr(a, _)) | (Type::Slice(p, _), Type::Slice(a, _)) | (Type::Array(p, _), Type::Array(a, _)) => {
            unify_params(p, a, given)
        }
        (Type::Map(pk, pv, _), Type::Map(ak, av, _)) => {
            unify_params(pk, ak, given);
            unify_params(pv, av, given)
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
        // `(int, P...) -> R`: the group is what the task takes between the
        // values named around it, with how it takes each.
        (Type::Closure(p), Type::Closure(a)) => unify_params(&Type::Task(Box::new(p.task.clone())), &Type::Task(Box::new(a.task.clone())), given),
        (Type::Task(p), Type::Task(a)) => {
            let Some((pairs, group)) = task_pairs(p, a) else { return };
            let a = regions_named_as(p, a);
            for (pi, ai) in pairs {
                if let (Some(p), Some(a)) = (p.params.get(pi), a.params.get(ai)) {
                    unify_params(p, a, given);
                }
            }
            if let Some((group, range)) = group {
                let types = a.params.get(range.clone()).unwrap_or_default().to_vec();
                let taken = a.taken.get(range).unwrap_or_default().to_vec();
                given.entry(group).or_insert_with(|| group_binding(types, taken));
            }
            unify_params(&p.ret, &a.ret, given);
        }
        // A plain value given where a `maybe` or a `T ~ E` is expected becomes
        // one, so it names the type inside.
        (Type::Maybe(p), a) | (Type::Or(p, _), a) => unify_params(p, a, given),
        _ => {}
    }
}

/// `actual`, a task type given where `pattern` is expected, with each region
/// it takes in the place of a region `pattern` names made [`OWN_REGION`]. A
/// task type's regions are its own (`(@r) -> [int]@r` is `(@s) -> [int]@s`),
/// so the region a type found there is in is the one the generic task's body
/// gives the task value there, which no name of the call says. A region in
/// the place of a type the generic task takes keeps its name, which the
/// values given for that type must be in too.
fn regions_named_as(pattern: &TaskType, actual: &TaskType) -> TaskType {
    let mut found = HashMap::new();
    let pairs = task_pairs(pattern, actual).map_or_else(Vec::new, |(pairs, _)| pairs);
    for (pi, ai) in pairs {
        let (Some(p), Some(a)) = (pattern.params.get(pi), actual.params.get(ai)) else { continue };
        if let (Some(p), Some(a)) = (pattern.region(pi), actual.region(ai)) {
            found.entry(p.to_string()).or_insert_with(|| a.to_string());
        }
        unify_regions(p, a, &mut found);
    }
    unify_regions(&pattern.ret, &actual.ret, &mut found);
    let own: HashSet<String> = found.into_values().collect();
    if own.is_empty() {
        return actual.clone();
    }
    let mut name = |region: &str| -> Option<String> { Some(if own.contains(region) { OWN_REGION.to_string() } else { region.to_string() }) };
    TaskType {
        params: actual.params.iter().map(|ty| ty.map_regions(&mut name)).collect(),
        taken: actual.taken.iter().map(|how| match how {
            Taken::Region(region) => Taken::Region(name(region).unwrap_or_else(|| region.clone())),
            other => other.clone(),
        }).collect(),
        ret: actual.ret.map_regions(&mut name),
    }
}

/// `actual`, a task type given where `pattern` is expected, with its regions
/// named as the caller's where `given` says which: a parameter of type `T`
/// in `pattern`, with `T` given `text@q` by another value, makes the region
/// of `actual`'s parameter there `q`, and so what `actual` sends there. A
/// task type's regions are its own, and the values given for `T` say which
/// of the caller's each one is at this call.
fn in_callers_regions(pattern: &Type, actual: &Type, given: &HashMap<String, Type>) -> Type {
    let (Type::Task(p), Type::Task(a)) = (pattern, actual) else { return actual.clone() };
    let Some((pairs, _)) = task_pairs(p, a) else { return actual.clone() };
    let mut rename = HashMap::new();
    for (pi, ai) in pairs {
        let (Some(p), Some(a)) = (p.params.get(pi), a.params.get(ai)) else { continue };
        if p.params().is_empty() {
            continue;
        }
        let expected = p.map_params(&mut |name| given.get(name).cloned());
        if expected.params().is_empty() {
            unify_regions(a, &expected, &mut rename);
        }
    }
    if rename.is_empty() {
        return actual.clone();
    }
    let mut name = |region: &str| -> Option<String> { Some(rename.get(region).cloned().unwrap_or_else(|| region.to_string())) };
    Type::Task(Box::new(TaskType { params: a.params.iter().map(|ty| ty.map_regions(&mut name)).collect(), taken: a.taken.clone(), ret: a.ret.map_regions(&mut name) }))
}

/// The places of a task type's parameters lined up with another's, and the
/// group between them with the ones it stands for (see `task_pairs`).
type TaskPairs = (Vec<(usize, usize)>, Option<(String, std::ops::Range<usize>)>);

/// Which parameter of `actual` stands in each place of `pattern`'s, a task
/// type given where `pattern` is expected: the places of the ones `pattern`
/// names one by one, and the group it takes between them, if any, with the
/// parameters of `actual` it stands for. `None` where they can't line up.
fn task_pairs(pattern: &TaskType, actual: &TaskType) -> Option<TaskPairs> {
    let Some(at) = pattern.params.iter().position(|ty| matches!(ty, Type::Group(_))) else {
        return (pattern.params.len() == actual.params.len()).then(|| ((0..pattern.params.len()).map(|i| (i, i)).collect(), None));
    };
    let after = pattern.params.len() - at - 1;
    let end = actual.params.len().checked_sub(after).filter(|end| *end >= at)?;
    let mut pairs: Vec<(usize, usize)> = (0..at).map(|i| (i, i)).collect();
    pairs.extend((0..after).map(|k| (at + 1 + k, end + k)));
    let Some(Type::Group(group)) = pattern.params.get(at) else { return None };
    Some((pairs, Some((group.clone(), at..end))))
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
        (Type::Map(pk, pv, pr), Type::Map(ak, av, ar)) => {
            record(pr, ar);
            unify_regions(pk, ak, regions);
            unify_regions(pv, av, regions);
        }
        (Type::Array(p, _), Type::Array(a, _)) => unify_regions(p, a, regions),
        (Type::Tuple(p), Type::Tuple(a)) => p.iter().zip(a).for_each(|(p, a)| unify_regions(p, a, regions)),
        (Type::Named(p, pr), Type::Named(a, ar)) if p == a => record(pr, ar),
        (Type::Applied(p), Type::Applied(a)) if p.name == a.name && p.types.len() == a.types.len() => {
            record(&p.region, &a.region);
            p.types.iter().zip(&a.types).for_each(|(p, a)| unify_regions(p, a, regions));
        }
        (Type::Text(pr), Type::Text(ar)) => record(pr, ar),
        (Type::Closure(p), Type::Closure(a)) => record(&Some(p.region.clone()), &Some(a.region.clone())),
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
        Type::Ptr(..) | Type::Slice(..) | Type::Map(..) => true,
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

/// Whether a value of type `ty` holds a slice, a map or a pointer another
/// name may share, given the shapes and kinds in `regional` that do, or what
/// `fail` sends holds one; with `lambdas`, also a lambda whose call may send
/// one back. A field can't hold a lambda that keeps what it uses, so a shape
/// or kind holds none.
fn shares_handle(ty: &Type, regional: &HashSet<String>, lambdas: bool) -> bool {
    match ty {
        Type::Results(items) | Type::Tuple(items) => items.iter().any(|item| shares_handle(item, regional, lambdas)),
        Type::Maybe(inner) | Type::Array(inner, _) | Type::Fail(inner) => shares_handle(inner, regional, lambdas),
        Type::Or(value, error) => shares_handle(value, regional, lambdas) || shares_handle(error, regional, lambdas),
        Type::Closure(closure) => lambdas && shares_handle(&closure.task.ret, regional, lambdas),
        _ => holds_pointers(ty, regional),
    }
}

/// Whether a value of type `ty` holds a pointer, a slice, or an array, whose
/// value can change, given the shapes and kinds in `changeable` that do.
fn holds_changeable(ty: &Type, changeable: &HashSet<String>) -> bool {
    match ty {
        Type::Ptr(..) | Type::Slice(..) | Type::Map(..) | Type::Array(..) => true,
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
/// `ty` with no region anywhere in it, a task type's own regions too.
fn strip_all_regions(ty: &Type) -> Type {
    match ty {
        Type::Task(task) => Type::Task(Box::new(TaskType { params: task.params.iter().map(strip_all_regions).collect(), taken: task.taken.clone(), ret: strip_all_regions(&task.ret) })),
        other => other.without_regions(),
    }
}

/// Whether `ty` is a task's type, `(int) -> int`.
fn is_task(ty: &Type) -> bool {
    matches!(ty, Type::Task(_))
}

/// Whether `ty` is the type of a `[flex @r]` lambda, which changes what it
/// keeps, so a value of it is never copied.
fn held_lambda(ty: &Type) -> bool {
    matches!(ty, Type::Closure(closure) if closure.flex)
}

/// Whether `ty` is or holds a lambda that keeps what it uses, `[@r](…)`.
fn has_closure(ty: &Type) -> bool {
    match ty {
        Type::Closure(_) => true,
        Type::Ptr(inner, _) | Type::Slice(inner, _) | Type::Array(inner, _) | Type::Range(inner) | Type::Maybe(inner) | Type::Fail(inner) => has_closure(inner),
        Type::Map(key, value, _) | Type::Or(key, value) => has_closure(key) || has_closure(value),
        Type::Tuple(items) | Type::Results(items) => items.iter().any(has_closure),
        Type::Applied(applied) => applied.types.iter().any(has_closure),
        _ => false,
    }
}

/// Whether `ty` holds a `[flex @r]` lambda inside it, which is held alone.
fn holds_held_lambda(ty: &Type) -> bool {
    match ty {
        Type::Closure(_) => false,
        Type::Ptr(inner, _) | Type::Slice(inner, _) | Type::Array(inner, _) | Type::Range(inner) | Type::Maybe(inner) | Type::Fail(inner) => held_lambda(inner) || holds_held_lambda(inner),
        Type::Map(key, value, _) | Type::Or(key, value) => [key, value].into_iter().any(|ty| held_lambda(ty) || holds_held_lambda(ty)),
        Type::Tuple(items) => items.iter().any(|ty| held_lambda(ty) || holds_held_lambda(ty)),
        Type::Applied(applied) => applied.types.iter().any(|ty| held_lambda(ty) || holds_held_lambda(ty)),
        _ => false,
    }
}

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

/// Whether `value`, a value a field or a parameter falls back on, is
/// arithmetic, which the checker works out before the program runs.
fn is_arithmetic(value: Option<&Expr>) -> bool {
    match value.map(|value| &value.kind) {
        Some(ExprKind::Binary(..) | ExprKind::Unary(..)) => true,
        Some(ExprKind::Call { name, .. }) => [INT_CAST, FLOAT_CAST, CHAR_CAST, UNIT_CAST].contains(&name.as_str()),
        _ => false,
    }
}

fn constant_value(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::CharLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) | ExprKind::NoneLit | ExprKind::Macro { .. } | ExprKind::TaskValue(_) => true,
        // A value the checker has already made part of a `maybe`, as an
        // array's items and a shared field's value are.
        ExprKind::Unary(_, inner) | ExprKind::Wrap(inner) => constant_value(inner),
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
        Type::Applied(_) => untold_kind(ty),
        Type::Array(items, _) => unsettled(items),
        Type::Tuple(items) => items.iter().any(unsettled),
        _ => false,
    }
}

/// `ty` with a type in place of each `none` and `fail e`, for a message
/// suggesting an annotation: `[maybe int#2]` for `[none#2]`.
fn settled_example(ty: &Type) -> Type {
    match ty {
        Type::None => Type::Maybe(Box::new(Type::Int)),
        Type::Fail(error) => Type::Or(Box::new(Type::Int), error.clone()),
        Type::Maybe(inner) if holds_untold(inner) => Type::Maybe(Box::new(settled_example(inner))),
        Type::Array(items, count) => Type::Array(Box::new(settled_example(items)), *count),
        Type::Tuple(items) => Type::Tuple(items.iter().map(settled_example).collect()),
        Type::Applied(applied) if untold_kind(ty) => {
            Type::Applied(Box::new(applied.with_types(applied.types.iter().map(|ty| if is_untold(ty) { Type::Int } else { ty.clone() }).collect())))
        }
        other => other.clone(),
    }
}

/// The message for a variant `tag` of `kind` whose type `param` nothing told,
/// suggesting its types as `example` writes them.
fn untold_kind_message(kind: &str, tag: &str, param: &str, example: &str) -> String {
    format!(
        "can't tell which type '{}' is for this '{}', since its values don't say and nothing this value goes into names one; write {}, as in '{}<{}>.{}'",
        param,
        unqualified(kind),
        if example.contains(", ") { "them" } else { "it" },
        unqualified(kind),
        example,
        tag
    )
}

/// What marks a type of a kind's variant that nothing has told yet, before the
/// name the kind gives it: `Option<?T>` for `Option.None` (see
/// `Checker::settle_variant`). No type's name starts with it.
const UNTOLD: char = '?';

/// Whether `ty` is a type of a variant nothing has told yet (see `UNTOLD`).
fn is_untold(ty: &Type) -> bool {
    matches!(ty, Type::Param(name) if name.starts_with(UNTOLD))
}

/// How a variant of the kind `ty` untold is written with its types: `Option<int>.…`.
fn untold_written(ty: &Type) -> String {
    match ty {
        Type::Maybe(inner) | Type::Array(inner, _) => untold_written(inner),
        Type::Tuple(items) => items.iter().find(|item| holds_untold(item)).map_or_else(String::new, untold_written),
        Type::Applied(applied) => format!("{}<{}>.…", unqualified(&applied.name), applied.types.iter().map(|ty| if is_untold(ty) { "int".to_string() } else { ty.to_string() }).collect::<Vec<_>>().join(", ")),
        other => other.to_string(),
    }
}

/// Whether `ty` is or holds a kind whose variant didn't tell all of its types.
fn holds_untold(ty: &Type) -> bool {
    match ty {
        Type::Maybe(inner) | Type::Array(inner, _) => holds_untold(inner),
        Type::Tuple(items) => items.iter().any(holds_untold),
        other => untold_kind(other),
    }
}

/// Whether `ty` holds a kind whose variant didn't tell all of its types,
/// as a kind given one does: `Option<Either<int, ?B>>`.
fn untold_inside(ty: &Type) -> bool {
    match ty {
        Type::Maybe(inner) | Type::Array(inner, _) => untold_inside(inner),
        Type::Tuple(items) => items.iter().any(untold_inside),
        Type::Applied(applied) => applied.types.iter().any(|ty| is_untold(ty) || untold_inside(ty)),
        _ => false,
    }
}

/// Whether `ty` is a kind whose variant didn't tell all of its types.
fn untold_kind(ty: &Type) -> bool {
    matches!(ty, Type::Applied(applied) if applied.types.iter().any(is_untold))
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
    let plain = |ty: &Type| !matches!(ty, Type::None | Type::Fail(_) | Type::Nothing | Type::Maybe(_) | Type::Or(..));
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
/// Whether `place` is a value in a map, or part of one: `m[k]`, `m[k].xs`.
fn in_map_value(place: &Expr) -> bool {
    match &place.kind {
        ExprKind::Field(base, _) => in_map_value(base),
        ExprKind::Index(base, _) => matches!(base.ty, Some(Type::Map(..))) || in_map_value(base),
        _ => false,
    }
}

fn place_region(place: &Expr) -> Option<String> {
    match &place.kind {
        ExprKind::Deref(pointer) => match pointer.ty.as_ref()? {
            Type::Ptr(_, region) => region.clone(),
            _ => None,
        },
        ExprKind::Field(base, _) => place_region(base),
        ExprKind::Index(base, _) => match base.ty.as_ref()? {
            Type::Slice(_, region) | Type::Map(_, _, region) => region.clone(),
            _ => place_region(base),
        },
        _ => None,
    }
}

/// At most how many bytes a value of type `ty` takes in C, given `sizes` for
/// shapes and kinds: each part counts at least 8 bytes, which covers padding.
fn value_bytes(ty: &Type, sizes: &HashMap<String, u128>) -> u128 {
    match ty {
        Type::Nothing => 0,
        Type::Named(name, _) => sizes.get(name).copied().unwrap_or(8),
        // A shape given types is as large as the shape itself, whose fields of a
        // type it takes are counted as a word each (see `check_sizes`).
        Type::Applied(applied) => sizes.get(&applied.name).copied().unwrap_or(8),
        Type::Array(items, count) => value_bytes(items, sizes).saturating_mul(u128::from(*count)),
        Type::Slice(..) => 24,
        Type::Map(..) => 8,
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
/// except `nothing` and `nothing ~ E`, which succeed by reaching their end.
fn sends_value(ret: &Type) -> bool {
    !matches!(ret, Type::Nothing) && !matches!(ret, Type::Or(value, _) if **value == Type::Nothing)
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
        Type::Map(key, value, _) => {
            let mut names = named_types(key);
            names.extend(named_types(value));
            names
        }
        Type::Tuple(items) => items.iter().flat_map(named_types).collect(),
        Type::Or(value, error) => {
            let mut names = named_types(value);
            names.extend(named_types(error));
            names
        }
        Type::Task(task) => task.params.iter().chain([&task.ret]).flat_map(named_types).collect(),
        Type::Closure(closure) => closure.task.params.iter().chain([&closure.task.ret]).flat_map(named_types).collect(),
        _ => Vec::new(),
    }
}

/// Whether a value of type `ty` can be created, given the shapes and kinds in
/// `can` that can be.
fn constructible(ty: &Type, can: &HashSet<&str>) -> bool {
    match ty {
        Type::Named(name, _) => can.contains(name.as_str()),
        Type::Applied(applied) => can.contains(applied.name.as_str()),
        Type::Ptr(inner, _) | Type::Array(inner, _) => constructible(inner, can),
        // A slice or a map may be empty, as a new one is, which needs no item.
        Type::Slice(..) | Type::Map(..) => true,
        Type::Or(value, error) => **value == Type::Nothing || constructible(value, can) || constructible(error, can),
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
        // The arms that may run, up to one that always holds, must send, and
        // the `else` too where none always holds. One known never to hold, as a
        // type test that fails is (see `settle_type_tests`), never runs.
        StmtKind::When(arms, else_block) => {
            for arm in arms {
                let known = known_condition(&arm.cond);
                if known == Some(false) {
                    continue;
                }
                if !always_sends(&arm.body) {
                    return false;
                }
                if known == Some(true) {
                    return true;
                }
            }
            else_block.as_ref().is_some_and(always_sends)
        }
        StmtKind::Match(_, cases) => cases.iter().all(|case| always_sends(&case.body)),
        StmtKind::Block(body) => always_sends(body),
        // A loop that never ends by itself, and that nothing in it stops, never
        // finishes: `endless`, or a loop written so.
        StmtKind::Named(name, inner) => never_finishes(inner, Some(name)),
        StmtKind::Loop(..) | StmtKind::Parted(_) => never_finishes(stmt, None),
        _ => false,
    })
}

/// Whether the loop `stmt`, named `name`, never ends by itself, and nothing
/// in it stops it.
fn never_finishes(stmt: &Stmt, name: Option<&str>) -> bool {
    match &stmt.kind {
        StmtKind::Loop(cond, body) if says_true(cond) => !stops_loop(&[body], name),
        StmtKind::Parted(parted) if parted.cond.as_ref().is_none_or(says_true) => !stops_loop(&[&parted.start, &parted.step, &parted.body], name),
        _ => false,
    }
}

/// Whether a `break` in `parts`, the statements of a loop named `name`, may
/// stop it, or a `continue` go on with a loop around it (see `loop_effects`).
fn stops_loop(parts: &[&Block], name: Option<&str>) -> bool {
    let mut effects = LoopEffects::default();
    parts.iter().for_each(|part| loop_effects(part, &[], &mut Vec::new(), name, &mut effects));
    effects.stops
}

/// The warning for a loop written to never end by itself, without the word.
const ENDLESS_UNSAID: &str = "this loop never ends by its condition; write 'endless' to say so";

/// The first name `parted`'s start declares that no part of the loop uses
/// after it, with where it is declared. A lambda in the loop may use one
/// where this can't see, so then none is told.
fn unused_in_loop(parted: &PartedLoop) -> Option<(String, Span)> {
    let mut closures = false;
    let mut used = |stmts: &[Stmt], names: &mut Vec<(String, Span)>| {
        walk_block(stmts, &mut |e| match &e.kind {
            ExprKind::Var(name) | ExprKind::Call { name, .. } => names.retain(|(declared, _)| declared != name),
            ExprKind::Closure { .. } => closures = true,
            _ => {}
        });
    };
    let mut declared: Vec<(String, Span)> = Vec::new();
    for (i, item) in parted.start.iter().enumerate() {
        if let StmtKind::Binding { names, .. } = &item.kind {
            for bound in names {
                bound_names(bound, &mut declared);
            }
        }
        used(parted.start.get(i + 1..).unwrap_or_default(), &mut declared);
    }
    if let Some(cond) = &parted.cond {
        let mut reads = Vec::new();
        walk_block(&[Stmt::new(StmtKind::Loop(cond.clone(), Vec::new()), cond.value.span)], &mut |e| {
            if let ExprKind::Var(name) = &e.kind {
                reads.push(name.clone());
            }
        });
        declared.retain(|(name, _)| !reads.contains(name));
    }
    used(&parted.step, &mut declared);
    used(&parted.body, &mut declared);
    if closures {
        return None;
    }
    declared.into_iter().next()
}

/// The names `bound` declares, `(a, b)` its items', with where each is.
fn bound_names(bound: &Bound, names: &mut Vec<(String, Span)>) {
    if bound.parts.is_empty() {
        if bound.name != DISCARD {
            names.push((bound.name.clone(), bound.span));
        }
    } else {
        bound.parts.iter().for_each(|part| bound_names(part, names));
    }
}

/// The places `target` sets, each item's of a tuple's.
fn target_places<'a>(target: &'a Target, places: &mut Vec<&'a Expr>) {
    match target {
        Target::Place(place) => places.push(place),
        Target::Tuple(parts, _) => parts.iter().for_each(|part| target_places(part, places)),
        Target::Discard(_) => {}
    }
}

/// The variables a loop's condition `cond` reads, where it is only those,
/// values and operators, and nothing in its `body` and `step`, as written,
/// leaves the loop or may change one of them (see `Checker::warn_unchanging`).
fn unchanging_reads(cond: &Condition, body: &[Stmt], step: &[Stmt], own: Option<&str>) -> Option<Vec<String>> {
    let mut names = Vec::new();
    if !cond.parts().all(|part| part.pattern.is_none() && plain_reads(&part.value, &mut names)) || names.is_empty() {
        return None;
    }
    let mut effects = LoopEffects::default();
    loop_effects(body, &names, &mut Vec::new(), own, &mut effects);
    loop_effects(step, &names, &mut Vec::new(), own, &mut effects);
    (!effects.leaves && !effects.changes).then_some(names)
}

/// Whether `e` only reads variables and values with operators, as a
/// condition `warn_unchanging` can tell about does, adding the variables
/// to `names`.
fn plain_reads(e: &Expr, names: &mut Vec<String>) -> bool {
    match &e.kind {
        ExprKind::Var(name) => {
            if !names.contains(name) {
                names.push(name.clone());
            }
            true
        }
        ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::CharLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) => true,
        ExprKind::Binary(_, left, right) => plain_reads(left, names) && plain_reads(right, names),
        ExprKind::Unary(_, inner) | ExprKind::Field(inner, _) => plain_reads(inner, names),
        _ => false,
    }
}

/// What a loop's statements may do to it (see `loop_effects`).
#[derive(Default)]
struct LoopEffects {
    /// Something may leave the loop: a `break` of it, a `continue` or a
    /// `break` of a loop around it, or a `<-`.
    leaves: bool,
    /// Something may change one of the variables looked at.
    changes: bool,
    /// A `break` may stop the loop, or a `continue` go on with one around it.
    stops: bool,
}

/// What `stmts`, inside a loop, may do to it and to the variables `names`
/// (see `warn_unchanging`). `inner` holds the loops around `stmts` inside it,
/// innermost last, each by its name if it has one, and `own` is the name of
/// the loop itself.
fn loop_effects(stmts: &[Stmt], names: &[String], inner: &mut Vec<Option<String>>, own: Option<&str>, effects: &mut LoopEffects) {
    for stmt in stmts {
        stmt_effects(stmt, names, inner, own, effects);
    }
}

fn stmt_effects(stmt: &Stmt, names: &[String], inner: &mut Vec<Option<String>>, own: Option<&str>, effects: &mut LoopEffects) {
    let within = |inner: &[Option<String>], name: &str| inner.iter().any(|loop_name| loop_name.as_deref() == Some(name));
    let exprs = |values: &[Expr], inner: &mut Vec<Option<String>>, effects: &mut LoopEffects| values.iter().for_each(|e| expr_effects(e, names, inner, own, effects));
    match &stmt.kind {
        StmtKind::Send(values) => {
            effects.leaves = true;
            exprs(values, inner, effects);
        }
        // A `break` stops the loop or one around it, but one of a loop inside
        // it, and a `continue` goes on with one around it.
        StmtKind::Break(name, value) => {
            let stops = match name {
                None => inner.is_empty(),
                Some((name, _)) => !within(inner, name),
            };
            effects.leaves |= stops;
            effects.stops |= stops;
            if let Some(value) = value {
                expr_effects(value, names, inner, own, effects);
            }
        }
        StmtKind::Continue(name) => {
            let stops = name.as_ref().is_some_and(|(name, _)| !within(inner, name) && own != Some(name.as_str()));
            effects.leaves |= stops;
            effects.stops |= stops;
        }
        StmtKind::Assign(targets, values) => {
            let mut set = Vec::new();
            targets.iter().for_each(|target| target_places(target, &mut set));
            effects.changes |= set.iter().any(|place| place_root(place).is_some_and(|name| names.contains(&name)));
            set.into_iter().for_each(|place| expr_effects(place, names, inner, own, effects));
            exprs(values, inner, effects);
        }
        StmtKind::Step { target, value, .. } => {
            effects.changes |= place_root(target).is_some_and(|name| names.contains(&name));
            expr_effects(target, names, inner, own, effects);
            expr_effects(value, names, inner, own, effects);
        }
        StmtKind::SetResults(set, values) => {
            effects.changes |= set.iter().any(|(name, _)| names.contains(name));
            exprs(values, inner, effects);
        }
        StmtKind::Binding { init, .. } => exprs(init, inner, effects),
        StmtKind::Expr(e) => expr_effects(e, names, inner, own, effects),
        StmtKind::When(arms, else_block) => {
            for arm in arms {
                arm.cond.parts().for_each(|part| expr_effects(&part.value, names, inner, own, effects));
                loop_effects(&arm.body, names, inner, own, effects);
            }
            if let Some(block) = else_block {
                loop_effects(block, names, inner, own, effects);
            }
        }
        StmtKind::Match(subject, cases) => {
            expr_effects(subject, names, inner, own, effects);
            cases.iter().for_each(|case| loop_effects(&case.body, names, inner, own, effects));
        }
        StmtKind::Block(block) => loop_effects(block, names, inner, own, effects),
        StmtKind::Turns(turns) => turns.iter().for_each(|turn| loop_effects(turn, names, inner, own, effects)),
        StmtKind::Named(..) | StmtKind::Loop(..) | StmtKind::Each(..) | StmtKind::Parted(_) => nested_loop_effects(stmt, None, names, inner, own, effects),
        StmtKind::Region { .. } => {}
    }
}

/// What the loop `stmt` inside the loop looked at, named `name`, may do (see
/// `loop_effects`). What runs before its passes, an `each`'s source and a
/// three-part loop's start, runs outside it.
fn nested_loop_effects(stmt: &Stmt, name: Option<String>, names: &[String], inner: &mut Vec<Option<String>>, own: Option<&str>, effects: &mut LoopEffects) {
    match &stmt.kind {
        StmtKind::Named(name, nested) => return nested_loop_effects(nested, Some(name.clone()), names, inner, own, effects),
        StmtKind::Each(_, _, source, _) => expr_effects(source, names, inner, own, effects),
        StmtKind::Parted(parted) => loop_effects(&parted.start, names, inner, own, effects),
        _ => {}
    }
    inner.push(name);
    match &stmt.kind {
        StmtKind::Loop(cond, block) => {
            cond.parts().for_each(|part| expr_effects(&part.value, names, inner, own, effects));
            loop_effects(block, names, inner, own, effects);
        }
        StmtKind::Each(_, _, _, block) => loop_effects(block, names, inner, own, effects),
        StmtKind::Parted(parted) => {
            parted.cond.iter().flat_map(Condition::parts).for_each(|part| expr_effects(&part.value, names, inner, own, effects));
            loop_effects(&parted.step, names, inner, own, effects);
            loop_effects(&parted.body, names, inner, own, effects);
        }
        _ => {}
    }
    inner.pop();
}

/// What the expression `e` may do to the loop it is in (see `loop_effects`):
/// a place given with `&`, a method that may alter its value, a lambda keeping
/// a place, and the statements of its plan Bs, of the `when`s and `match`es
/// it holds as values, and of the loops giving it a value.
fn expr_effects(e: &Expr, names: &[String], inner: &mut Vec<Option<String>>, own: Option<&str>, effects: &mut LoopEffects) {
    visit_expr(e, &mut |e| match &e.kind {
        ExprKind::Ref(place) => {
            effects.changes |= place_root(place).is_some_and(|name| names.contains(&name));
            true
        }
        ExprKind::MethodCall { value, .. } | ExprKind::Edit { items: value, .. } => {
            effects.changes |= place_root(value).is_some_and(|name| names.contains(&name));
            true
        }
        ExprKind::Closure { places, .. } => {
            effects.changes |= places.iter().any(|place| names.contains(&place.name));
            true
        }
        ExprKind::PlanB { value, body, .. } => {
            expr_effects(value, names, inner, own, effects);
            loop_effects(body, names, inner, own, effects);
            false
        }
        ExprKind::Choice(choice) => {
            stmt_effects(choice, names, inner, own, effects);
            false
        }
        ExprKind::LoopValue(value) => {
            nested_loop_effects(&value.stmt, value.name.as_ref().map(|(name, _)| name.clone()), names, inner, own, effects);
            false
        }
        _ => true,
    });
}

/// The variable a place is in: `a` of `a.b[2]`.
fn place_root(e: &Expr) -> Option<String> {
    let mut e = e;
    loop {
        match &e.kind {
            ExprKind::Var(name) => return Some(name.clone()),
            ExprKind::Field(base, _) | ExprKind::Index(base, _) => e = base,
            _ => return None,
        }
    }
}

/// Whether running `block` always leaves it before its end: it always sends
/// (see `always_sends`), or stops or goes on with a loop around it, with
/// `break` or `continue`, on every path. A loop inside it doesn't count, as a
/// `break` inside one stops that loop only.
fn always_leaves(block: &[Stmt]) -> bool {
    block.iter().any(|stmt| match &stmt.kind {
        StmtKind::Send(_) | StmtKind::Break(..) | StmtKind::Continue(_) => true,
        StmtKind::When(arms, else_block) => {
            for arm in arms {
                let known = known_condition(&arm.cond);
                if known == Some(false) {
                    continue;
                }
                if !always_leaves(&arm.body) {
                    return false;
                }
                if known == Some(true) {
                    return true;
                }
            }
            else_block.as_ref().is_some_and(|block| always_leaves(block))
        }
        StmtKind::Match(_, cases) => cases.iter().all(|case| always_leaves(&case.body)),
        StmtKind::Block(body) => always_leaves(body),
        _ => false,
    })
}

/// The passing a header writes for a parameter a task's type takes `how`.
fn task_passing(how: &Taken) -> Passing {
    match how {
        Taken::Place(passing) => *passing,
        _ => Passing::Copy,
    }
}

/// Parameter `name` of type `ty`, taken as `passing`, as a header writes it.
fn shown_param(name: &str, ty: &Type, passing: Passing) -> String {
    match passing {
        Passing::Copy => format!("{}: {}", name, ty),
        passing => format!("{} {}: {}", passing.word(), name, ty),
    }
}

/// How a task's type takes parameter `name` of type `ty`, which its header
/// takes as `passing` (see `Taken`): a region by its name, a place as one, and
/// anything else, a copy too, as a value.
fn taken_by(name: &str, ty: &Type, passing: Passing) -> Taken {
    match (ty, passing) {
        (Type::Region, _) => Taken::Region(name.to_string()),
        (_, passing) if passing.is_ref() => Taken::Place(passing),
        _ => Taken::Value,
    }
}

/// Whether a condition always holds: `is (x, y)`, whose items always match.
/// (Every other condition may be false, so its block may not run.)
fn always_holds(cond: &Condition) -> bool {
    cond.parts().all(|part| matches!(&part.pattern, Some(pattern) if matches!(pattern.kind, PatternKind::Tuple(_))))
}

/// How `op` is written.
fn binary_symbol(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Mod => "%",
        BinaryOp::Pow => "**",
        BinaryOp::Eq => "==",
        BinaryOp::Ne => "!=",
        BinaryOp::Similar => "~=",
        BinaryOp::NotSimilar => "<>",
        BinaryOp::Lt => "<",
        BinaryOp::Gt => ">",
        BinaryOp::Le => "<=",
        BinaryOp::Ge => ">=",
        BinaryOp::And => "and",
        BinaryOp::Or => "or",
    }
}

/// `name(value)`, a conversion in a macro's value, with the mistakes it has
/// when the program runs.
fn fold_cast(name: &str, value: Constant, span: Span) -> Result<Constant> {
    use Constant::*;
    let byte = |n: i64, ty: &str| u8::try_from(n).map_err(|_| Error::at(span, format!("the value, {}, is outside a {}'s 0..255", n, ty)));
    match (name, value) {
        (INT_CAST, Int(n)) => Ok(Int(n)),
        (INT_CAST, Unit(n) | Char(n)) => Ok(Int(i64::from(n))),
        (INT_CAST, Bool(b)) => Ok(Int(i64::from(b))),
        // As `vela__rt__int_of` does: toward zero, and a float outside the ints stops.
        (INT_CAST, Float(x)) if x.is_finite() && x.trunc() >= -9_223_372_036_854_775_808.0 && x.trunc() < 9_223_372_036_854_775_808.0 => Ok(Int(x.trunc() as i64)),
        (INT_CAST, Float(_)) => err(span, "the value doesn't fit in an int"),
        (FLOAT_CAST, Float(x)) => Ok(Float(x)),
        (FLOAT_CAST, Int(n)) => Ok(Float(n as f64)),
        (FLOAT_CAST, Unit(n)) => Ok(Float(f64::from(n))),
        (UNIT_CAST, Int(n)) => Ok(Unit(byte(n, "unit")?)),
        (UNIT_CAST, Unit(n) | Char(n)) => Ok(Unit(n)),
        (UNIT_CAST, Bool(b)) => Ok(Unit(u8::from(b))),
        // Toward zero, as `unit(x)` does when the program runs.
        (UNIT_CAST, Float(x)) if x.is_finite() && (0.0..256.0).contains(&x.trunc()) => Ok(Unit(x.trunc() as u8)),
        (UNIT_CAST, Float(x)) => err(span, format!("the value, {}, is outside a unit's 0..255", x.trunc())),
        (CHAR_CAST, Int(n)) => Ok(Char(byte(n, "char")?)),
        (CHAR_CAST, Unit(n) | Char(n)) => Ok(Char(n)),
        (_, value) => err(span, format!("'{}(…)' doesn't take a {} in a macro's value", name, value.ty())),
    }
}

/// `a op b` in a macro's value (see `Checker::fold_constant`), with the types
/// and the mistakes the operator has when the program runs.
fn fold_binary(op: BinaryOp, a: Constant, b: Constant, span: Span) -> Result<Constant> {
    use Constant::*;
    let symbol = binary_symbol(op);
    let too_large = |what: &str| Error::at(span, format!("the value doesn't fit in {}", what));
    let byte = |n: i64, ty: &str| u8::try_from(n).map_err(|_| Error::at(span, format!("the value, {}, is outside a {}'s 0..255", n, ty)));
    // A float that isn't finite: from dividing by zero, too large, or no number.
    let float = |x: f64, by_zero: bool| match x {
        _ if x.is_finite() => Ok(Float(x)),
        _ if by_zero => err(span, "the value is no number a float can hold, as dividing by zero gives"),
        _ if x.is_nan() => err(span, "the value is no number at all (nan), which a float here can't be"),
        _ => err(span, "the value is too large for a float"),
    };
    match (op, a, b) {
        (BinaryOp::And, Bool(x), Bool(y)) => Ok(Bool(x && y)),
        (BinaryOp::Or, Bool(x), Bool(y)) => Ok(Bool(x || y)),
        // A number beside a unit is a unit, as a literal is.
        (BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Gt | BinaryOp::Le | BinaryOp::Ge, x @ (Unit(_) | Int(_)), y @ (Unit(_) | Int(_))) if x.ty() != y.ty() => {
            let unit = |c: Constant| match c {
                Int(n) => byte(n, "unit").map(Unit),
                other => Ok(other),
            };
            fold_binary(op, unit(x)?, unit(y)?, span)
        }
        (BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Gt | BinaryOp::Le | BinaryOp::Ge, x, y) if x.ty() == y.ty() => {
            let order = match (&x, &y) {
                (Int(x), Int(y)) => x.partial_cmp(y),
                (Unit(x), Unit(y)) | (Char(x), Char(y)) => x.partial_cmp(y),
                (Float(x), Float(y)) => x.partial_cmp(y),
                (Bool(x), Bool(y)) => x.partial_cmp(y),
                (Text(x), Text(y)) => x.as_bytes().partial_cmp(y.as_bytes()),
                _ => None,
            };
            let Some(order) = order else { return Ok(Bool(op == BinaryOp::Ne)) };
            if matches!(x, Bool(_)) && !matches!(op, BinaryOp::Eq | BinaryOp::Ne) {
                return err(span, format!("'{}' orders numbers, chars, and text, not bools", symbol));
            }
            Ok(Bool(match op {
                BinaryOp::Eq => order.is_eq(),
                BinaryOp::Ne => order.is_ne(),
                BinaryOp::Lt => order.is_lt(),
                BinaryOp::Gt => order.is_gt(),
                BinaryOp::Le => order.is_le(),
                _ => order.is_ge(),
            }))
        }
        (BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod, Int(x), Int(y)) => {
            if y == 0 && matches!(op, BinaryOp::Div | BinaryOp::Mod) {
                return err(span, if op == BinaryOp::Div { "division by zero" } else { "remainder of division by zero" });
            }
            let value = match op {
                BinaryOp::Add => x.checked_add(y),
                BinaryOp::Sub => x.checked_sub(y),
                BinaryOp::Mul => x.checked_mul(y),
                BinaryOp::Div => x.checked_div(y),
                _ if y == -1 => Some(0),
                _ => x.checked_rem(y),
            };
            value.map(Int).ok_or_else(|| too_large("an int"))
        }
        // A unit beside an int: the int is a unit too, as a literal is.
        (BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod, x @ (Unit(_) | Int(_)), y @ (Unit(_) | Int(_))) => {
            let wide = |c: Constant| -> Result<i64> {
                match c {
                    Unit(n) => Ok(i64::from(n)),
                    Int(n) => Ok(i64::from(byte(n, "unit")?)),
                    _ => Ok(0),
                }
            };
            let (x, y) = (wide(x)?, wide(y)?);
            if y == 0 && matches!(op, BinaryOp::Div | BinaryOp::Mod) {
                return err(span, if op == BinaryOp::Div { "division by zero" } else { "remainder of division by zero" });
            }
            let value = match op {
                BinaryOp::Add => x + y,
                BinaryOp::Sub => x - y,
                BinaryOp::Mul => x * y,
                BinaryOp::Div => x / y,
                _ => x % y,
            };
            Ok(Unit(byte(value, "unit")?))
        }
        (BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod, Float(x), Float(y)) => float(
            match op {
                BinaryOp::Add => x + y,
                BinaryOp::Sub => x - y,
                BinaryOp::Mul => x * y,
                BinaryOp::Div => x / y,
                _ => x % y,
            },
            matches!(op, BinaryOp::Div | BinaryOp::Mod) && y == 0.0,
        ),
        // Worked out wide, so a huge int gives its true sum in the message.
        (BinaryOp::Add | BinaryOp::Sub, Char(c), Int(n)) | (BinaryOp::Add, Int(n), Char(c)) => {
            let value = if op == BinaryOp::Add { i128::from(c) + i128::from(n) } else { i128::from(c) - i128::from(n) };
            u8::try_from(value).map(Char).map_err(|_| Error::at(span, format!("the value, {}, is outside a char's 0..255", value)))
        }
        (BinaryOp::Sub, Char(c), Char(d)) => Ok(Int(i64::from(c) - i64::from(d))),
        (BinaryOp::Add, Text(x), Text(y)) => Ok(Text(x + &y)),
        // As `check_power` types a power: an int raised to a unit or to 0 or
        // more is an int, a unit raised to a unit a unit, anything else a float.
        (BinaryOp::Pow, Int(x), Int(y)) if y >= 0 => int_power(x, y).map(Int).ok_or_else(|| too_large("an int")),
        (BinaryOp::Pow, Int(x), Unit(y)) => int_power(x, i64::from(y)).map(Int).ok_or_else(|| too_large("an int")),
        (BinaryOp::Pow, x @ (Unit(_) | Int(_)), y @ (Unit(_) | Int(_))) if matches!(x, Unit(_)) => {
            let (Unit(x), Int(_) | Unit(_)) = (&x, &y) else { return err(span, "a unit is raised to a unit's power") };
            let y = match y {
                Unit(y) => i64::from(y),
                Int(y) => i64::from(byte(y, "unit")?),
                _ => 0,
            };
            let value = int_power(i64::from(*x), y).ok_or_else(|| too_large("a unit"))?;
            Ok(Unit(byte(value, "unit")?))
        }
        (BinaryOp::Pow, x @ (Int(_) | Unit(_) | Float(_)), y @ (Int(_) | Unit(_) | Float(_))) => {
            let wide = |c: &Constant| match c {
                Int(n) => *n as f64,
                Unit(n) => f64::from(*n),
                Float(x) => *x,
                _ => 0.0,
            };
            let (x, y) = (wide(&x), wide(&y));
            float(x.powf(y), x == 0.0 && y < 0.0)
        }
        (BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod, Int(_), Float(_)) | (BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod, Float(_), Int(_)) => {
            err(span, "an int and a float don't mix in arithmetic: turn one into the other first, as in 'float(n) + 2.5' (or 'n + int(2.5)', which truncates)")
        }
        (_, x, y) => err(span, format!("'{}' isn't defined for {} and {} in a macro's value", symbol, x.ty(), y.ty())),
    }
}

/// The value of an int expression written with literals only (`-5`,
/// `60 * 60 * 24`), which the program computes the same way every time, or
/// `None` for any other expression. Its parts have been checked, so each of
/// them fits in an `int`.
fn constant_int(e: &Expr) -> Option<i64> {
    match &e.kind {
        ExprKind::IntLit(n) => Some(*n),
        ExprKind::Macro { value, .. } => constant_int(value),
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
                BinaryOp::Pow if b >= 0 => int_power(a, b),
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

/// A key written out, as text a message can quote: `"a"`, `4`, `'x'`, `true`.
fn constant_key(e: &Expr) -> Option<String> {
    match &e.kind {
        ExprKind::TextLit(text) => Some(format!("\"{}\"", text)),
        ExprKind::CharLit(c) => Some(format!("'{}'", char::from(*c).escape_default())),
        ExprKind::BoolLit(b) => Some(b.to_string()),
        ExprKind::Macro { value, .. } => constant_key(value),
        _ => constant_int(e).map(|n| n.to_string()),
    }
}

/// Why `key` can't be a map's key, or `None` when it can: a value compared by
/// what it is and hashed, with nothing inside that changes or has no order of
/// its own (a float's nan is equal to nothing, itself included).
/// A type a generic declaration takes (`is_param` says which names are) is
/// checked where a use gives it one (see `settle_applications`).
fn key_problem(key: &Type, is_param: &dyn Fn(&str) -> bool) -> Option<String> {
    match key {
        Type::Int | Type::Unit | Type::Char | Type::Bool | Type::Text(_) | Type::Ptr(..) | Type::Unknown | Type::Param(_) => None,
        Type::Named(name, _) if is_param(name) => None,
        Type::Tuple(items) => items.iter().find_map(|item| key_problem(item, is_param)),
        Type::Float => Some("a float, which nan makes unequal to itself".to_string()),
        other => Some(format!("{}", other)),
    }
}

/// Makes `e`, when it is a number written out (`200`, `-1`, `3 - 1`), a value
/// of the byte type `byte` (a unit), if it fits: whether it did.
fn byte_literal(e: &mut Expr, byte: &Type) -> Result<bool> {
    if e.ty != Some(Type::Int) {
        return Ok(false);
    }
    let Some(n) = constant_int(e) else { return Ok(false) };
    fits_byte(n, byte, e.span)?;
    e.ty = Some(byte.clone());
    Ok(true)
}

/// Where a place written by a step (see `Checker::step_to_assignment`) calls a
/// task or changes something, which naming it twice would do twice; `None` for
/// a place made only of names, fields, items, pointers, and arithmetic.
/// The parts `value` adds to text with `+=`, into `region`: those of a
/// `+` chain, and the pieces of text holding values that names no region.
fn step_parts(value: Expr, region: &str, at: Span, parts: &mut Vec<Expr>) {
    match value.kind {
        ExprKind::Binary(BinaryOp::Add, left, right) => {
            step_parts(*left, region, at, parts);
            step_parts(*right, region, at, parts);
        }
        ExprKind::Join { parts: pieces, region: named, .. } if named.is_empty() => crate::parser::interpolated_parts(pieces, region, at, parts),
        kind => parts.push(Expr { kind, ..value }),
    }
}

fn changing_part(e: &Expr) -> Option<Span> {
    match &e.kind {
        ExprKind::Var(_) | ExprKind::Macro { .. } | ExprKind::Shared { .. } | ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::CharLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) => None,
        ExprKind::Field(inner, _) | ExprKind::Deref(inner) | ExprKind::Unary(_, inner) => changing_part(inner),
        ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) => changing_part(a).or_else(|| changing_part(b)),
        // `xs[int(b)]`: a conversion is no task, and gives the same value each time.
        ExprKind::Call { name, args, .. } if [INT_CAST, FLOAT_CAST, UNIT_CAST, CHAR_CAST].contains(&name.as_str()) => args.iter().find_map(changing_part),
        _ => Some(e.span),
    }
}

/// A type as a message names one: 'a float', 'text'.
fn a_type(ty: &Type) -> String {
    match ty {
        Type::Text(_) => "text".to_string(),
        Type::Int => "an int".to_string(),
        other => format!("a {}", other),
    }
}

/// Checks `l ** r` and returns its type. An int raised to a power it can't be
/// told is 0 or more may be a fraction, so it is a float, as is any power with a
/// float; an int raised to a unit or to a constant 0 or more stays an int, and a
/// unit raised to a unit a unit.
fn check_power(l: &Expr, r: &Expr, lt: &Type, rt: &Type, span: Span) -> Result<Type> {
    let number = |ty: &Type| matches!(ty, Type::Int | Type::Float | Type::Unit);
    if !number(lt) || !number(rt) {
        let other = if number(lt) { rt } else { lt };
        let hint = match other {
            Type::Char => "; a char is a character, so turn it into its code first, as in 'int(c) ** 2'".to_string(),
            _ => extract_hint(&[lt, rt]),
        };
        let what = if matches!(other, Type::Text(_)) { format!("{}", other) } else { format!("a {}", other) };
        return err(span, format!("'**' raises a number to a power, and this is {}{}", what, hint));
    }
    match (lt, rt) {
        (Type::Unit, Type::Unit) => Ok(Type::Unit),
        (Type::Unit, Type::Int) => err(
            r.span,
            "a unit is raised to a unit's power, as in 'b ** 2' or 'b ** unit(n)'; for an int, write 'int(b) ** n'",
        ),
        (Type::Int, Type::Unit) => Ok(Type::Int),
        (Type::Int, Type::Int) => match constant_int(r) {
            Some(exponent) if exponent >= 0 => {
                if let Some(base) = constant_int(l)
                    && int_power(base, exponent).is_none()
                {
                    return err(span, format!("integer overflow: {} ** {} doesn't fit in an int, which holds {} to {}", base, exponent, i64::MIN, i64::MAX));
                }
                Ok(Type::Int)
            }
            _ => Ok(Type::Float),
        },
        _ => Ok(Type::Float),
    }
}

/// `base ** exponent` for ints, `exponent` 0 or more, or `None` when it doesn't fit.
fn int_power(base: i64, exponent: i64) -> Option<i64> {
    match base {
        0 => Some(i64::from(exponent == 0)),
        1 => Some(1),
        -1 => Some(if exponent % 2 == 0 { 1 } else { -1 }),
        _ => base.checked_pow(u32::try_from(exponent).ok()?),
    }
}

/// Checks `l op r` where one side is a char, and returns its type: a char moved
/// by an int (`c + 1`, `1 + c`, `c - 1`) is a char, and the distance from one
/// char to another (`c - 'a'`) is an int. Nothing else is arithmetic on chars.
fn char_arithmetic(op: BinaryOp, l: &Expr, r: &Expr, lt: &Type, rt: &Type, span: Span) -> Result<Type> {
    let symbol = match op {
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        _ => "%",
    };
    let ty = match (op, lt, rt) {
        (BinaryOp::Add, Type::Char, Type::Int) | (BinaryOp::Add, Type::Int, Type::Char) | (BinaryOp::Sub, Type::Char, Type::Int) => Type::Char,
        (BinaryOp::Sub, Type::Char, Type::Char) => return Ok(Type::Int),
        (BinaryOp::Add, Type::Char, Type::Char) => {
            return err(span, "two chars don't add up to a char: move a char by a number, as in 'c + 1', or take one from another for how far apart they are, as in 'c - 'a''");
        }
        (BinaryOp::Sub, Type::Int, Type::Char) => {
            return err(span, "a char is taken from a char, as in 'c - 'a'', or moved back by a number, as in 'c - 1'; to take a char's code from a number, write 'n - int(c)'");
        }
        (BinaryOp::Add | BinaryOp::Sub, _, _) => {
            let other = if *lt == Type::Char { rt } else { lt };
            return err(span, format!("a char moves by an int, as in 'c {} 1', not by a {}{}", symbol, other, extract_hint(&[other])));
        }
        _ => {
            return err(span, format!("'{}' works on numbers, and a char is a character; turn it into its code first, as in 'int(c) {} 2'", symbol, symbol));
        }
    };
    // `'z' + 200`: a char and a number both written out, so the result is known.
    let (char_side, number) = if *lt == Type::Char { (l, r) } else { (r, l) };
    if let (ExprKind::CharLit(c), Some(n)) = (&char_side.kind, constant_int(number)) {
        let moved = if op == BinaryOp::Add { i128::from(*c) + i128::from(n) } else { i128::from(*c) - i128::from(n) };
        if !(0..=255).contains(&moved) {
            return err(span, format!("the result, {}, doesn't fit in a char, which holds 0 to 255", moved));
        }
    }
    Ok(ty)
}

/// Checks that `n` fits in the byte type `byte`.
fn fits_byte(n: i64, byte: &Type, span: Span) -> Result<()> {
    if (0..=255).contains(&n) {
        return Ok(());
    }
    err(span, format!("{} doesn't fit in a {}, which holds 0 to 255", n, byte))
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
    // A lambda that keeps what it uses goes only where its own type does.
    if let (Type::Closure(closure), Type::Task(_)) = (actual, expected) {
        return format!(
            "; a lambda that keeps what it uses goes only where a type naming its region is expected, as '[@{0}](…)', since a task's type names none and the lambda could outlive '{0}'",
            closure.region
        );
    }
    // `P#0`: the type of one item of a group's tuple, which no code writes.
    if let Some(item) = [actual, expected].into_iter().find_map(|ty| match ty {
        Type::Param(name) if name.contains(GROUP_ITEM) => Some(name.clone()),
        _ => None,
    }) {
        let (group, place) = item.split_once(GROUP_ITEM).unwrap_or((item.as_str(), "0"));
        // Against `P` alone, which the two are told apart from.
        let turn = [actual, expected].into_iter().any(|ty| matches!(ty, Type::Param(name) if name == group));
        let apart = if turn { format!(", and '{}' alone is the type of the value of a turn of 'each v in args', so they may be different types", group) } else { String::new() };
        // Values are counted from 1, as the messages about them say, and a
        // tuple's items from 0, as the program writes them.
        let value = place.parse::<usize>().map_or(1, |k| k + 1);
        return format!("; {0} is the type of value {1} of the group '{2}...', item {3} of its tuple{4}", item, value, group, place, apart);
    }
    match (actual, expected) {
        (Type::Array(items, _), Type::Slice(wanted, _)) if items.without_regions() == wanted.without_regions() => {
            "; an array never becomes a slice: write the values as a slice, as in '[1, 2, ...] at r', or copy them into one, as in 'new [int](xs) at r'".to_string()
        }
        (Type::Int | Type::Float, Type::Unit) => "; turn it into a unit with 'unit(x)', which stops the program when it isn't from 0 to 255".to_string(),
        (Type::Int, Type::Float) => "; an int doesn't become a float on its own: write a float as '1.0', or turn an int into one with 'float(n)'".to_string(),
        (Type::Unit, Type::Int | Type::Float) => format!("; turn it into {0} with '{1}(b)'", if *expected == Type::Int { "an int" } else { "a float" }, expected),
        (Type::None, Type::Ptr(..)) => format!("; a pointer can't be none, so use 'maybe {}'", expected),
        (Type::None, Type::Slice(..)) => format!("; a slice can't be none, so use 'maybe {}'", expected),
        (Type::None, Type::Map(..)) => format!("; a map can't be none, so use 'maybe {}'", expected),
        (Type::None, Type::Or(value, _)) if value.ptr_inner().is_some() => {
            format!("; a pointer can't be none, so use 'maybe {}'", value)
        }
        (_, Type::Or(_, error)) if fits_regions(actual, error) => "; an error is given with 'fail', as in '<- fail e'".to_string(),
        (Type::Fail(_), _) => "; 'fail …' is an error, which only a 'T ~ E' can hold".to_string(),
        // A task's regions are its own: `text` is text that lasts, and `text@r`
        // text of any region.
        (Type::Task(_), Type::Task(_)) if strip_all_regions(actual) == strip_all_regions(expected) => {
            "; a task's type says which regions its values are in, as its header does: 'text' is text that lasts as long as the program, and 'text@r' text of any region the caller gives".to_string()
        }
        (Type::Maybe(inner) | Type::Or(inner, _), _) if fits_regions(inner, expected) => {
            "; extract its value first, as in 'x { … }' or 'when x is {value}'".to_string()
        }
        _ if actual.regions().contains(&UNNAMED_REGION) => {
            format!("; a type test on an item whose type each use gives takes what may be in a region of the caller's, written '{}', which only this task sees; to keep it, copy it into a region of its own, as in 's.copy()@r'", UNNAMED_REGION)
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
        if let Some(number) = number_cast(name).filter(|number| *number != Type::Char) {
            return Ok(number);
        }
        // Text is what `scan` reads by itself, so naming it says to leave it out.
        if matches!(Type::builtin(name), Some(Type::Text(_))) {
            return err(arg.span, "'scan' reads text on its own, so 'text' is left out: write 'scan(\"Name: \") at r'");
        }
    }
    err(arg.span, "'scan' reads a number, so it names 'int', 'float', or 'unit', as in 'scan(\"Age: \", int)'")
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

/// Example names for a list of `n` values, as in `fixed a, b = …`.
fn example_names(n: usize) -> String {
    let names: Vec<String> = (0..n.min(3)).filter_map(|i| u8::try_from(i).ok()).map(|i| char::from(b'a' + i).to_string()).collect();
    if n > 3 { format!("{}, …", names.join(", ")) } else { names.join(", ") }
}

/// Checks the header of a method named `str`, which turns the value it is
/// called on into text for `x.str() at r` (and `text(x) at r`), so it has the
/// one header those can call.
fn check_str_method(t: &TaskDef) -> Result<()> {
    let written = "a method named 'str' turns the value it is called on into text, so it is written 'str(r: region) -> text@r'";
    // `show` and text holding values write the value from any file.
    if t.private {
        return err(t.span, "a method named 'str' says how the value is written as text wherever it is shown, so it isn't 'priv'");
    }
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
/// Gathers the startup decorators of each module's tasks, each a call
/// `d(task, args)`, into a task of the module (see `ast::STARTUP`), which runs
/// where the program starts: the modules in the program's order, their tasks in
/// the order written, and a task's decorators the nearest first, as Python runs
/// them. Its body is checked as any task's, with no variable or region yet.
/// `error`, found in a value worked out where the program starts (`what`: a
/// decorator's or a wrapper's), said as such: no variable and no region exist
/// there yet.
fn at_start_error(mut error: Error, what: &str) -> Error {
    let no_region = format!("{} are worked out where the program starts, before any region exists, so none of them can be made in one", what);
    if error.message.starts_with("unknown region") {
        error.message = no_region;
    } else if error.message.starts_with("unknown variable") {
        error.message.push_str(&format!("; {} are worked out where the program starts, where no variable exists yet", what));
    } else if error.message.contains("region") {
        error.message = format!("{}; but {}", error.message, no_region);
    }
    error
}

/// The call of its wrapper in the body of a task or layer with wrappers (see
/// `Checker::settle_wrapped`): the body's one statement, or the value it sends.
fn wrapper_call(body: &mut Block) -> Option<&mut Expr> {
    match &mut body.first_mut()?.kind {
        StmtKind::Expr(call) => Some(call),
        StmtKind::Send(values) => values.first_mut(),
        _ => None,
    }
}

/// What some code may call (see `Checker::note_calls`).
#[derive(Default)]
struct Calls {
    /// The tasks it calls or names as values, and the `str` methods of the
    /// values it writes.
    tasks: Vec<String>,
    /// Whether it calls a task held as a value, which may be any task named as
    /// one somewhere.
    invokes: bool,
}

/// What each task's body may call, for the order the values of wrappers are
/// worked out in where the program starts (see `Checker::order_start_values`).
struct CallGraph {
    calls: HashMap<String, Calls>,
    /// The tasks each task names as values, but the next layer a wrapper's
    /// layer gives its wrapper: a layer is reached only through the task it
    /// belongs to, which names it (see `Checker::add_wrappers`).
    named: HashMap<String, Vec<String>>,
    /// The tasks a call of a task held as a value may reach where the program
    /// starts: every task named as a value in what can run or be read there (a
    /// shared field's first value, a value worked out there, and the bodies they
    /// may reach), but none named only where the program runs later, such as a
    /// startup decorator's (see `settle_values`).
    values: Vec<String>,
}

/// How a task is reached from a value (see `CallGraph::reach`): from the value
/// itself, called by another task, or through a task held as a value.
#[derive(Clone)]
enum Reached {
    Start,
    From(String),
    ThroughValue(String),
}

impl CallGraph {
    fn of(items: &[Item], checker: &Checker) -> CallGraph {
        let mut calls = HashMap::new();
        let mut named = HashMap::new();
        let mut values = Vec::new();
        for item in items {
            match item {
                Item::Task(task) => {
                    let mut found = Calls::default();
                    let mut names = Vec::new();
                    walk_block(&task.body, &mut |e| {
                        checker.note_calls(e, &mut found);
                        note_value(e, &mut names);
                    });
                    calls.insert(task.name.clone(), found);
                    named.insert(task.name.clone(), names);
                }
                // What a shared field holds where the program starts.
                Item::Shape(shape) => {
                    for value in shape.shared.iter().filter_map(|member| member.field.default.as_ref()) {
                        walk_expr(value, &mut |e| note_value(e, &mut values));
                    }
                }
                Item::Kind(_) | Item::Macro(_) => {}
            }
        }
        CallGraph { calls, named, values }
    }

    /// Adds to `values` every task named as a value in the bodies the code
    /// making `roots` may reach, until they reach no more.
    fn settle_values(&mut self, roots: &Calls, values: Vec<String>) {
        for name in values {
            if !self.values.contains(&name) {
                self.values.push(name);
            }
        }
        loop {
            let before = self.values.len();
            let reached: Vec<String> = self.reach(roots).into_keys().collect();
            for task in reached {
                for name in self.named.get(&task).cloned().unwrap_or_default() {
                    if !self.values.contains(&name) {
                        self.values.push(name);
                    }
                }
            }
            if self.values.len() == before {
                return;
            }
        }
    }

    /// Every task the code that makes `roots` may reach, each with how.
    fn reach(&self, roots: &Calls) -> HashMap<String, Reached> {
        let mut reached: HashMap<String, Reached> = HashMap::new();
        let mut next: Vec<String> = Vec::new();
        let visit = |name: &String, how: Reached, reached: &mut HashMap<String, Reached>, next: &mut Vec<String>| {
            if !reached.contains_key(name) {
                reached.insert(name.clone(), how);
                next.push(name.clone());
            }
        };
        for name in &roots.tasks {
            visit(name, Reached::Start, &mut reached, &mut next);
        }
        if roots.invokes {
            for name in &self.values {
                visit(name, Reached::ThroughValue(String::new()), &mut reached, &mut next);
            }
        }
        while let Some(task) = next.pop() {
            let Some(calls) = self.calls.get(&task) else { continue };
            for name in &calls.tasks {
                visit(name, Reached::From(task.clone()), &mut reached, &mut next);
            }
            if calls.invokes {
                for name in &self.values {
                    visit(name, Reached::ThroughValue(task.clone()), &mut reached, &mut next);
                }
            }
        }
        reached
    }

    /// The tasks `target` is reached through, from the value, as a message
    /// ends: ` through 'describe'`, or nothing for a value calling it itself.
    fn path(&self, reached: &HashMap<String, Reached>, target: &str) -> String {
        // From `target` back to the value: each task's name, and `None` where a
        // task held as a value is called.
        let mut steps: Vec<Option<String>> = Vec::new();
        let mut at = target.to_string();
        // Each task is reached once, so the path back ends.
        for _ in 0..reached.len() {
            match reached.get(&at) {
                Some(Reached::From(from)) => at = from.clone(),
                Some(Reached::ThroughValue(from)) => {
                    if steps.last() != Some(&None) {
                        steps.push(None);
                    }
                    if from.is_empty() {
                        break;
                    }
                    at = from.clone();
                }
                Some(Reached::Start) | None => break,
            }
            let shown = Some(called_name(&at));
            if steps.last() != Some(&shown) && at != target {
                steps.push(shown);
            }
        }
        steps.reverse();
        let through: Vec<String> = steps
            .iter()
            .map(|step| match step {
                Some(name) => format!("'{}'", name),
                None => "a task held as a value".to_string(),
            })
            .collect();
        match through.len() {
            0 => String::new(),
            _ => format!(" through {}", through.join(", then ")),
        }
    }
}

/// Notes the task `e` names as a value, if it does, but a wrapper's layer
/// (see `CallGraph::named`).
fn note_value(e: &Expr, names: &mut Vec<String>) {
    if let ExprKind::TaskValue(name) = &e.kind
        && !name.contains(LAYER)
        && !names.contains(name)
    {
        names.push(name.clone());
    }
}

/// A task as a message about what calls it names it: a wrapper's layer as its
/// task, a method through its shape or kind, and a lambda as one.
fn called_name(name: &str) -> String {
    let name = name.split(LAYER).next().unwrap_or(name);
    if is_lambda(name) {
        return "a lambda".to_string();
    }
    match method_parts(name) {
        Some((owner, method)) => format!("{}{}{}", owner, MODULE_SEPARATOR, method),
        None => name.to_string(),
    }
}

/// The order of the tasks in `units` whose wrapper values are worked out where
/// the program starts: each after the tasks its values `needs`, and in the
/// order of `units` otherwise. A circle of them has no order, and is given
/// back: each value in it, with the task it calls.
fn start_order<'a>(units: &[&'a str], needs: &HashMap<&'a str, Vec<(&'a str, &'a StartValue)>>) -> std::result::Result<Vec<&'a str>, Vec<(&'a StartValue, String)>> {
    // Visiting, then done; the walk keeps its own stack, so a long line of
    // values can't overflow the compiler's.
    let mut state: HashMap<&str, bool> = HashMap::new();
    let mut order = Vec::new();
    for &unit in units {
        if state.contains_key(unit) {
            continue;
        }
        state.insert(unit, false);
        let mut stack: Vec<(&str, usize)> = vec![(unit, 0)];
        while let Some((task, i)) = stack.last_mut() {
            let task: &str = task;
            let wanted = needs.get(task).and_then(|wanted| wanted.get(*i)).copied();
            *i += 1;
            match wanted {
                Some((other, _)) => match state.get(other) {
                    None => {
                        state.insert(other, false);
                        stack.push((other, 0));
                    }
                    Some(false) => {
                        // A circle: from `other` along the stack to here, and back.
                        let from = stack.iter().position(|(on, _)| *on == other).unwrap_or(0);
                        let tasks: Vec<&str> = stack.get(from..).unwrap_or_default().iter().map(|(on, _)| *on).collect();
                        let circle = tasks
                            .iter()
                            .enumerate()
                            .filter_map(|(k, &on)| {
                                let then = tasks.get(k + 1).copied().unwrap_or(other);
                                needs.get(on)?.iter().find(|(called, _)| *called == then).map(|(_, start)| (*start, then.to_string()))
                            })
                            .collect();
                        return Err(circle);
                    }
                    Some(true) => {}
                },
                None => {
                    state.insert(task, true);
                    order.push(task);
                    stack.pop();
                }
            }
        }
    }
    Ok(order)
}

/// Calls `f` on every expression in `block`, through the blocks inside it.
fn walk_block(block: &[Stmt], f: &mut dyn FnMut(&Expr)) {
    for stmt in block {
        walk_stmt(stmt, f);
    }
}

fn walk_stmt(stmt: &Stmt, f: &mut dyn FnMut(&Expr)) {
    visit_stmt(stmt, &mut |e| {
        f(e);
        true
    });
}

fn visit_stmt(stmt: &Stmt, f: &mut dyn FnMut(&Expr) -> bool) {
    match &stmt.kind {
        StmtKind::Expr(e) => visit_expr(e, f),
        StmtKind::Step { target, value, .. } => {
            visit_expr(target, f);
            visit_expr(value, f);
        }
        StmtKind::Send(values) | StmtKind::SetResults(_, values) | StmtKind::Binding { init: values, .. } => values.iter().for_each(|e| visit_expr(e, f)),
        StmtKind::Assign(targets, values) => {
            targets.iter().for_each(|target| visit_target(target, f));
            values.iter().for_each(|e| visit_expr(e, f));
        }
        StmtKind::When(arms, else_block) => {
            for arm in arms {
                arm.cond.parts().for_each(|part| visit_expr(&part.value, f));
                visit_block(&arm.body, f);
            }
            if let Some(block) = else_block {
                visit_block(block, f);
            }
        }
        StmtKind::Loop(cond, block) => {
            cond.parts().for_each(|part| visit_expr(&part.value, f));
            visit_block(block, f);
        }
        StmtKind::Parted(parted) => {
            visit_block(&parted.start, f);
            parted.cond.iter().flat_map(Condition::parts).for_each(|part| visit_expr(&part.value, f));
            visit_block(&parted.step, f);
            visit_block(&parted.body, f);
        }
        StmtKind::Block(block) => visit_block(block, f),
        StmtKind::Turns(turns) => turns.iter().for_each(|turn| visit_block(turn, f)),
        StmtKind::Named(_, inner) => visit_stmt(inner, f),
        StmtKind::Each(_, _, source, block) => {
            visit_expr(source, f);
            visit_block(block, f);
        }
        StmtKind::Match(subject, cases) => {
            visit_expr(subject, f);
            cases.iter().for_each(|case| visit_block(&case.body, f));
        }
        StmtKind::Break(_, Some(value)) => visit_expr(value, f),
        StmtKind::Region { .. } | StmtKind::Break(..) | StmtKind::Continue(_) => {}
    }
}

fn visit_target(target: &Target, f: &mut dyn FnMut(&Expr) -> bool) {
    match target {
        Target::Discard(_) => {}
        Target::Place(place) => visit_expr(place, f),
        Target::Tuple(parts, _) => parts.iter().for_each(|part| visit_target(part, f)),
    }
}

/// Calls `f` on `e` and every expression inside it, through the blocks of its
/// plan Bs and of the `match`es and `when`s it holds as values.
fn walk_expr(e: &Expr, f: &mut dyn FnMut(&Expr)) {
    visit_expr(e, &mut |e| {
        f(e);
        true
    });
}

fn visit_block(block: &[Stmt], f: &mut dyn FnMut(&Expr) -> bool) {
    for stmt in block {
        visit_stmt(stmt, f);
    }
}

/// Calls `f` on `e` and every expression inside it, as `walk_expr` does, but
/// not inside one for which `f` says `false`.
fn visit_expr(e: &Expr, f: &mut dyn FnMut(&Expr) -> bool) {
    if !f(e) {
        return;
    }
    let each = |values: &[Expr], f: &mut dyn FnMut(&Expr) -> bool| values.iter().for_each(|value| visit_expr(value, f));
    match &e.kind {
        ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::CharLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) | ExprKind::NoneLit | ExprKind::Macro { .. } | ExprKind::Var(_) | ExprKind::TaskValue(_) | ExprKind::RegionAfter(_) | ExprKind::Shared { .. } | ExprKind::TypeOf(_) | ExprKind::Closure { .. } => {}
        ExprKind::Kept { value: inner, .. } | ExprKind::Spread(inner) | ExprKind::Field(inner, _) | ExprKind::Unary(_, inner) | ExprKind::Deref(inner) | ExprKind::Ref(inner) | ExprKind::Fail(inner) | ExprKind::Wrap(inner) => visit_expr(inner, f),
        ExprKind::New { value, .. } | ExprKind::Copy { value, .. } | ExprKind::Shortcut { value, .. } | ExprKind::ToText { value, .. } => visit_expr(value, f),
        ExprKind::Index(a, b) | ExprKind::Lookup(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Range(a, b) | ExprKind::Repeat { value: a, count: b } => {
            visit_expr(a, f);
            visit_expr(b, f);
        }
        ExprKind::Invoke { callee, args } => {
            visit_expr(callee, f);
            each(args, f);
        }
        ExprKind::Call { args, .. } | ExprKind::Construct { args, .. } | ExprKind::Array(args) | ExprKind::Tuple(args) | ExprKind::Scan { args, .. } => each(args, f),
        ExprKind::Join { parts, .. } => each(parts, f),
        ExprKind::MethodCall { value, args, .. } => {
            visit_expr(value, f);
            each(args, f);
        }
        ExprKind::Edit { items, args, .. } => {
            visit_expr(items, f);
            each(args, f);
        }
        ExprKind::StructInit { fields, .. } => fields.iter().for_each(|field| visit_expr(&field.value, f)),
        ExprKind::Pairs(pairs) => pairs.iter().for_each(|(key, value)| {
            visit_expr(key, f);
            visit_expr(value, f);
        }),
        ExprKind::MapOp { map, key, .. } => {
            visit_expr(map, f);
            if let Some(key) = key {
                visit_expr(key, f);
            }
        }
        ExprKind::PlanB { value, body, .. } => {
            visit_expr(value, f);
            visit_block(body, f);
        }
        ExprKind::Choice(stmt) => visit_stmt(stmt, f),
        ExprKind::LoopValue(value) => visit_stmt(&value.stmt, f),
    }
}

fn add_startups(items: &mut Vec<Item>) {
    let mut startups: Vec<TaskDef> = Vec::new();
    for item in items.iter_mut() {
        let Item::Task(task) = item else { continue };
        if task.decorators.is_empty() {
            continue;
        }
        let name = qualify(module_of(&task.name), STARTUP);
        let passes: Vec<bool> = std::mem::take(&mut task.passes).into_iter().rev().collect();
        let calls = std::mem::take(&mut task.decorators).into_iter().rev().map(|call| {
            let span = call.span;
            Stmt::new(StmtKind::Expr(call), span)
        });
        match startups.iter_mut().find(|startup| startup.name == name) {
            Some(startup) => {
                startup.body.extend(calls);
                startup.passes.extend(passes);
            }
            None => startups.push(TaskDef {
                name,
                types: Vec::new(),
                given: Vec::new(),
                params: Vec::new(),
                ret: Type::Nothing,
                results: Vec::new(),
                body: calls.collect(),
                span: task.span,
                private: true,
                native: false,
                owner: None,
                decorators: Vec::new(),
                passes,
                wrappers: Vec::new(),
                kept: None,
            }),
        }
    }
    items.extend(startups.into_iter().map(Item::Task));
}

/// The types written as one word that no declaration makes, which a type
/// test names after `maybe`.
pub const BUILTIN_TYPE_WORDS: [&str; 6] = ["int", "float", "text", "bool", "char", "unit"];

/// The region of what a type test takes from an item whose type is not known
/// yet: no name in the task is it, so nothing taken lasts past the call, as
/// the item may be in a region of the caller's.
const UNNAMED_REGION: &str = "#item";

/// A task value called, as a message names it: `f`, `c.step`, `fs[…]`, `pick(…)`.
fn callee_text(callee: &Expr) -> String {
    match &callee.kind {
        ExprKind::Var(name) => name.clone(),
        ExprKind::Field(base, field) => format!("{}.{}", callee_text(base), field),
        ExprKind::Index(base, _) => format!("{}[…]", callee_text(base)),
        ExprKind::Call { name, .. } => format!("{}(…)", unqualified(name)),
        ExprKind::Invoke { callee, .. } => format!("{}(…)", callee_text(callee)),
        ExprKind::Shared { shape, field } => format!("{}.{}", shape, field),
        _ => "value".to_string(),
    }
}

/// The mistake of a call of the generic `task` (a task, method or wrapper, as
/// `what` says) whose type `param` is found to be `ty`, in [`OWN_REGION`].
fn own_region_type(what: &str, task: &str, param: &str, ty: &Type) -> String {
    // The region the task value takes, named as the header would name it.
    let shown = ty.map_regions(&mut |region| Some(if region == OWN_REGION { "a".to_string() } else { region.to_string() }));
    format!(
        "{0} '{1}' takes '{2}' from a task value, where it is {3}, with '@a' a region that task value takes; which region that is only the body of '{1}' says, when it gives the task value one, so '{2}' can't be in it; write that type out in the header of '{1}', with the region it takes as a parameter of its own, 'a: region', as in '(next: (@a, int) -> [int]@a, a: region, n: int) -> [int]@a'",
        what,
        unqualified(task),
        param,
        shown
    )
}

/// Whether `region` is one no block declares, so no block's end frees it:
/// the region of a value made without pointers, of what a type test takes
/// from an item, or of a type found inside a task type.
/// `a`, or `(a, b)` for names that take a tuple apart, as written.
fn bound_text(bound: &Bound) -> String {
    if bound.parts.is_empty() {
        return bound.name.clone();
    }
    format!("({})", bound.parts.iter().map(bound_text).collect::<Vec<_>>().join(", "))
}

/// Where each name `bound` declares is written.
fn bound_spans(bound: &Bound, spans: &mut Vec<Span>) {
    if bound.parts.is_empty() {
        spans.push(bound.span);
    }
    bound.parts.iter().for_each(|part| bound_spans(part, spans));
}

/// The region a copy of a value of type `ty` would name in a message: the
/// value's own, or `r`.
fn copy_region(ty: &Type) -> &str {
    // A method can't allocate in the region of the value it is called on.
    ty.regions().into_iter().find(|region| !unnamed_region(region) && *region != THIS_WORD).unwrap_or("r")
}

/// Where the suggested copy names the owner, a value with no short text of its
/// own, as a literal holding it (see `copied_text`).
const OWNER_MARK: &str = "\u{0}";

/// The values a loop that gives one may give: its last line's, and every
/// `break = v` in it.
fn loop_values<'a>(stmt: &'a Stmt, values: &mut Vec<&'a Expr>) {
    match &stmt.kind {
        StmtKind::Loop(_, body) | StmtKind::Each(_, _, _, body) => {
            last_values(body, values);
            body.iter().for_each(|stmt| break_values(stmt, values));
        }
        StmtKind::Parted(parted) => {
            last_values(&parted.body, values);
            parted.body.iter().for_each(|stmt| break_values(stmt, values));
        }
        StmtKind::Named(_, inner) => loop_values(inner, values),
        _ => {}
    }
}

/// Every `break = v` in `stmt`, through its blocks.
fn break_values<'a>(stmt: &'a Stmt, values: &mut Vec<&'a Expr>) {
    let mut blocks: Vec<&'a [Stmt]> = Vec::new();
    match &stmt.kind {
        StmtKind::Break(_, Some(value)) => values.push(value),
        StmtKind::When(arms, else_block) => {
            blocks.extend(arms.iter().map(|arm| arm.body.as_slice()));
            blocks.extend(else_block.iter().map(Vec::as_slice));
        }
        StmtKind::Match(_, cases) => blocks.extend(cases.iter().map(|case| case.body.as_slice())),
        StmtKind::Block(body) | StmtKind::Loop(_, body) | StmtKind::Each(_, _, _, body) => blocks.push(body),
        StmtKind::Parted(parted) => blocks.push(&parted.body),
        StmtKind::Named(_, inner) => break_values(inner, values),
        _ => {}
    }
    for block in blocks {
        block.iter().for_each(|stmt| break_values(stmt, values));
    }
}

fn unnamed_region(region: &str) -> bool {
    [ANY_REGION, UNNAMED_REGION, OWN_REGION].contains(&region)
}

/// The region of a type found inside a task type given to a generic task,
/// where the task type's own region is named like another of the generic
/// task's (see `regions_named_as`): no call gives it, so nothing in it fits
/// anything.
const OWN_REGION: &str = "#own";

/// Why a type can't be tested where it is written.
const TYPE_TEST_ON_A_READ: &str = "a type after 'is' tests a tuple's item read with '.#' from a tuple with a name, as in 'when t.#2 is maybe int {x}'";

/// What a read of a tuple's item, `t.#k`, is known to find: `Some(Some(type))`
/// for an item, `Some(None)` past the tuple's end, and nothing where the type
/// isn't known yet (a group's tuple, or an item holding a `T`, in the task
/// taking it).
fn item_state(read: &Expr) -> Option<Option<Type>> {
    match &read.kind {
        ExprKind::Wrap(inner) if matches!(inner.kind, ExprKind::Field(..)) => match &inner.ty {
            Some(Type::None) => Some(None),
            Some(found) if found.params().is_empty() => Some(Some(found.clone())),
            _ => None,
        },
        ExprKind::Field(_, item) if item.starts_with(SAFE_ITEM) && read.ty == Some(Type::None) => Some(None),
        _ => None,
    }
}

/// The tuple's name and the item's place of `t.#k`, a read of a tuple with a
/// name, as the checker leaves it: as written, or the item in a `maybe`.
fn safe_read_of(read: &Expr) -> Option<(String, usize)> {
    let (ExprKind::Field(base, item), safe) = (match &read.kind {
        ExprKind::Wrap(inner) => (&inner.kind, true),
        other => (other, false),
    }) else {
        return None;
    };
    if !safe && !item.starts_with(SAFE_ITEM) {
        return None;
    }
    let ExprKind::Var(tuple) = &base.kind else { return None };
    Some((tuple.clone(), item.trim_start_matches(SAFE_ITEM).parse().ok()?))
}

/// Whether two types are one, their regions aside: `text@r` is a `text`.
fn same_ignoring_regions(a: &Type, b: &Type) -> bool {
    a.without_regions() == b.without_regions()
}

/// The type a plan B on `t.#5` past the tuple's end settles from its block (see
/// `Checker::past_end_plan_b`); no type can be named so.
const PAST_END: &str = "#past";

/// Whether `e` is `t.#5` read past the tuple's end: always `none`, of a
/// `maybe` the place it goes says.
fn past_end(e: &Expr) -> bool {
    matches!(&e.kind, ExprKind::Field(_, item) if item.starts_with(SAFE_ITEM)) && e.ty == Some(Type::None)
}

/// Why the tuple of a group's values can't be unpacked into names.
const GROUP_TUPLE_UNPACKED: &str = "this is the tuple of a group's values, which holds as many items as each use gives, so no list of names fits it; read its items as 't.0', or as 't.#0', a maybe";

/// The type `name` is given in a copy of a task: the one `given` names, and
/// for a group's `P` alone, the type of the value of the turn of an `each` over
/// its values. Where no value has one (a group of none, or a tuple's item past
/// the values given, both in code a copy never runs), a type stands in. A
/// generic task read with nothing given changes nothing.
fn copy_type(given: &HashMap<String, Type>, name: &str) -> Option<Type> {
    if given.is_empty() {
        return None;
    }
    if given.contains_key(&format!("{}{}", name, GROUP_MARK)) {
        return Some(given.get(&format!("{}{}", name, GROUP_TURN)).cloned().unwrap_or_else(placeholder_type));
    }
    given.get(name).cloned().or_else(|| name.contains(GROUP_ITEM).then(placeholder_type))
}

/// `ty` in a copy of a task given `given`: the types given in place of the
/// names it takes (see `copy_type`), with a group's tuple made at least two
/// items long (see `padded_group_tuples`). A generic task read with nothing
/// given keeps its types.
fn in_copy(ty: &Type, given: &HashMap<String, Type>) -> Type {
    if given.is_empty() {
        return ty.clone();
    }
    padded_group_tuples(&ty.map_group_items(&mut |name| group_items(given, name)).map_params(&mut |name| copy_type(given, name)))
}

/// The type standing in, in a copy of a task, for one no value has (see
/// `copy_type`): never read, since the code holding it never runs.
fn placeholder_type() -> Type {
    Type::Int
}

/// `ty` with each tuple of fewer than two items, which only a group's tuple
/// is in a copy given fewer than two values, made two items long: that tuple
/// is never made there (see `substitute_group_tuple`), and C has no tuple of
/// none.
fn padded_group_tuples(ty: &Type) -> Type {
    let boxed = |inner: &Type| Box::new(padded_group_tuples(inner));
    match ty {
        Type::Tuple(items) => {
            let mut items: Vec<Type> = items.iter().map(padded_group_tuples).collect();
            while items.len() < 2 {
                items.push(placeholder_type());
            }
            Type::Tuple(items)
        }
        Type::Or(value, error) => Type::Or(boxed(value), boxed(error)),
        Type::Maybe(inner) => Type::Maybe(boxed(inner)),
        Type::Fail(inner) => Type::Fail(boxed(inner)),
        Type::Ptr(inner, region) => Type::Ptr(boxed(inner), region.clone()),
        Type::Array(inner, count) => Type::Array(boxed(inner), *count),
        Type::Slice(inner, region) => Type::Slice(boxed(inner), region.clone()),
        Type::Results(types) => Type::Results(types.iter().map(padded_group_tuples).collect()),
        Type::Map(key, value, region) => Type::Map(boxed(key), boxed(value), region.clone()),
        Type::Range(inner) => Type::Range(boxed(inner)),
        Type::Applied(applied) => Type::Applied(Box::new(Applied {
            name: applied.name.clone(),
            types: applied.types.iter().map(padded_group_tuples).collect(),
            region: applied.region.clone(),
        })),
        Type::Task(task) => Type::Task(Box::new(TaskType { params: task.params.iter().map(padded_group_tuples).collect(), taken: task.taken.clone(), ret: padded_group_tuples(&task.ret) })),
        other => other.clone(),
    }
}

/// Names the loop `value`, which gives the variable `name` at `at` its value,
/// as that variable: unless written with `=>`, since a loop has one name.
fn name_value_loop(value: &mut ValueLoop, name: &str, at: Span) -> Result<()> {
    if let Some((written, written_at)) = value.name.as_ref().filter(|_| value.arrow) {
        return err(
            *written_at,
            format!("this loop gives '{0}' its value, which names it '{0}', so it takes no name of its own; leave out '{1} =>' and write 'break {0}'", name, written),
        );
    }
    value.name = Some((name.to_string(), at));
    Ok(())
}

/// The values the last line of `block` gives, through the branches of a
/// `when` or a `match` there.
fn last_values<'a>(block: &'a [Stmt], values: &mut Vec<&'a Expr>) {
    match block.last().map(|stmt| &stmt.kind) {
        Some(StmtKind::Expr(e)) => values.push(e),
        Some(StmtKind::When(arms, else_block)) => {
            arms.iter().for_each(|arm| last_values(&arm.body, values));
            if let Some(block) = else_block {
                last_values(block, values);
            }
        }
        Some(StmtKind::Match(_, cases)) => cases.iter().for_each(|case| last_values(&case.body, values)),
        _ => {}
    }
}

/// Calls `f` on each value the last line of `block` gives (see `last_values`).
fn last_values_mut(block: &mut [Stmt], f: &mut dyn FnMut(&mut Expr)) {
    match block.last_mut().map(|stmt| &mut stmt.kind) {
        Some(StmtKind::Expr(e)) => f(e),
        Some(StmtKind::When(arms, else_block)) => {
            arms.iter_mut().for_each(|arm| last_values_mut(&mut arm.body, f));
            if let Some(block) = else_block {
                last_values_mut(block, f);
            }
        }
        Some(StmtKind::Match(_, cases)) => cases.iter_mut().for_each(|case| last_values_mut(&mut case.body, f)),
        _ => {}
    }
}

/// `each x in xs … fi { 0 }` starting the last line of a block that gives a
/// value is the value of a loop, which a line of its own would only fail with.
fn last_loop_gives(last: &mut Stmt) {
    if let StmtKind::Expr(Expr { kind: ExprKind::PlanB { value, .. }, .. }) = &mut last.kind
        && let ExprKind::LoopValue(value) = &mut value.kind
    {
        value.gives = true;
    }
}

/// Whether the value `e` may fail, as its type says, but the value a branch
/// gives where another fails, which is made one that may (`Wrap`).
fn may_fail(e: &Expr) -> bool {
    let e = match &e.kind {
        ExprKind::Wrap(inner) => inner,
        _ => e,
    };
    matches!(e.ty, Some(Type::Or(..) | Type::Fail(_)))
}

/// Whether `cond` is `true` as written, which never ends a loop.
fn says_true(cond: &Condition) -> bool {
    cond.single() && cond.pattern.is_none() && matches!(cond.value.kind, ExprKind::BoolLit(true))
}

/// Whether `cond`, in a copy of a task, is known true or false: a condition
/// of numbers and bools written out, as `args.len` becomes (see
/// `substitute_expr`).
fn known_condition(cond: &Condition) -> Option<bool> {
    let mut taken = Some(true);
    for part in cond.parts() {
        match known_part(part) {
            Some(false) => return Some(false),
            Some(true) => {}
            None => taken = None,
        }
    }
    taken
}

/// Whether one part of a condition is known to hold: a `bool` the compiler
/// works out, a tuple's items, which always match, and what a type test leaves
/// where the item's type is known (see `settle_type_tests`): its value taken,
/// which is always there, and `none` matched on a read, which holds past the
/// tuple's end and nowhere else.
fn known_part(part: &Condition) -> Option<bool> {
    let item = item_state(&part.value);
    match part.pattern.as_ref().map(|pattern| &pattern.kind) {
        None => known_bool(&part.value),
        Some(PatternKind::Tuple(_)) => Some(true),
        Some(PatternKind::Value(_)) if matches!(item, Some(Some(_))) => Some(true),
        Some(PatternKind::None) => item.map(|found| found.is_none()),
        _ => None,
    }
}

fn known_bool(e: &Expr) -> Option<bool> {
    match &e.kind {
        ExprKind::BoolLit(b) => Some(*b),
        ExprKind::Unary(UnaryOp::Not, inner) => known_bool(inner).map(|b| !b),
        // Either side decides it where it is the one that would.
        ExprKind::Binary(op @ (BinaryOp::And | BinaryOp::Or), a, b) => {
            let decides = *op == BinaryOp::Or;
            match (known_bool(a), known_bool(b)) {
                (Some(side), _) | (_, Some(side)) if side == decides => Some(decides),
                (Some(_), Some(_)) => Some(!decides),
                _ => None,
            }
        }
        ExprKind::Binary(op, a, b) => {
            let (a, b) = (known_int(a)?, known_int(b)?);
            match op {
                BinaryOp::Eq => Some(a == b),
                BinaryOp::Ne => Some(a != b),
                BinaryOp::Lt => Some(a < b),
                BinaryOp::Gt => Some(a > b),
                BinaryOp::Le => Some(a <= b),
                BinaryOp::Ge => Some(a >= b),
                _ => None,
            }
        }
        _ => None,
    }
}

fn known_int(e: &Expr) -> Option<i64> {
    match &e.kind {
        ExprKind::IntLit(n) => Some(*n),
        ExprKind::Binary(BinaryOp::Add, a, b) => known_int(a)?.checked_add(known_int(b)?),
        ExprKind::Binary(BinaryOp::Sub, a, b) => known_int(a)?.checked_sub(known_int(b)?),
        _ => None,
    }
}

/// The type of `(args...)` for the group `group`, `(P...) ~ maybe P#0`: the
/// tuple of its values when there are two or more, and when there is one or
/// none, a failure holding that one value, or none.
fn group_tuple_type(group: &str) -> Type {
    Type::Or(Box::new(Type::Tuple(vec![Type::Group(group.to_string())])), Box::new(Type::Maybe(Box::new(Type::Param(group_item(group, 0))))))
}

/// The group a group's tuple holds, `(P...)`.
fn group_in_tuple(items: &[Type]) -> Option<String> {
    match items {
        [Type::Group(group)] => Some(group.clone()),
        _ => None,
    }
}

/// The type of item `place` of the group `group`'s tuple, `P#0`: the type the
/// call gives that value, in each copy of the task.
fn group_item(group: &str, place: usize) -> String {
    format!("{}{}{}", group, GROUP_ITEM, place)
}

/// The types given for the group of types `name`, kept among the types given
/// a call as a tuple of them (see `Checker::group_call_info`).
fn group_types(given: &HashMap<String, Type>, name: &str) -> Option<Vec<Type>> {
    group_items(given, name).map(|(types, _)| types)
}

/// The types given for the group `name`, with how a task takes each: a group
/// holding a place is kept as the parameters of a task type, which say so,
/// and one holding only values as a tuple (see `group_binding`).
fn group_items(given: &HashMap<String, Type>, name: &str) -> Option<(Vec<Type>, Vec<Taken>)> {
    match given.get(name) {
        Some(Type::Tuple(types)) => Some((types.clone(), vec![Taken::Value; types.len()])),
        Some(Type::Task(task)) if task.ret == GROUP_SENDS => Some((task.params.clone(), task.taken.clone())),
        _ => None,
    }
}

/// The type given for a group of `types`, each taken as `taken` says: a tuple
/// of them where all are values, and otherwise the parameters of a task type
/// sending [`GROUP_SENDS`], which only a group's types are (see
/// `group_items`). Its C name says how each is taken, so a use giving a
/// place and one giving a value have copies of their own.
fn group_binding(types: Vec<Type>, taken: Vec<Taken>) -> Type {
    if taken.iter().all(|how| *how == Taken::Value) {
        return Type::Tuple(types);
    }
    Type::Task(Box::new(TaskType { params: types, taken, ret: GROUP_SENDS }))
}

/// The types a call gives, by the name of the type each is given for, with the
/// types given for one group (`P#0`, `P#1`, …; see `Checker::group_call_info`)
/// kept together as the one type given for the group, a tuple of them, in the
/// group's place.
/// A value a task value the call gives takes as a place (in `taken`) is a
/// place in the group too (see `group_binding`).
fn packed_groups(types: Vec<(&str, TypeArg)>, taken: &HashMap<String, Taken>) -> Vec<TypeArg> {
    let mut packed: Vec<(Vec<Type>, Vec<Taken>, Span, bool)> = Vec::new();
    let mut group: Option<&str> = None;
    for (name, (ty, at)) in types {
        let how = taken.get(name).cloned().unwrap_or(Taken::Value);
        match name.split_once(GROUP_ITEM) {
            Some((of, _)) if group == Some(of) => {
                if let Some((items, hows, _, _)) = packed.last_mut() {
                    items.push(ty);
                    hows.push(how);
                }
            }
            Some((of, _)) => {
                group = Some(of);
                packed.push((vec![ty], vec![how], at, true));
            }
            None => {
                group = None;
                packed.push((vec![ty], Vec::new(), at, false));
            }
        }
    }
    packed.into_iter().map(|(mut items, hows, at, grouped)| (if grouped { group_binding(items, hows) } else { items.remove(0) }, at)).collect()
}

/// The name of the parameter holding the `i`th value of the group parameter
/// `name` in a task made for its types: `args__1`. No Vela name holds `__`.
fn group_value(name: &str, i: usize) -> String {
    format!("{}__{}", name, i + 1)
}

/// The values of a call in a task made for a group's types: `args...` becomes
/// the parameters the group parameter became, each a value of its own.
/// A value the group holds as a place is given on as that place, marked, to a
/// task (`places`), and read as a value by `show`.
fn spread_values(args: &mut Vec<Expr>, given: &HashMap<String, Type>, places: bool) {
    let Some(at) = args.iter().position(|arg| matches!(arg.kind, ExprKind::Spread(_))) else { return };
    let Some(Expr { kind: ExprKind::Spread(inner), span, .. }) = args.get(at) else { return };
    let (ExprKind::Var(name), Some(Type::Group(group))) = (&inner.kind, &inner.ty) else { return };
    let Some((types, taken)) = group_items(given, group) else { return };
    let (name, span) = (name.clone(), *span);
    let values: Vec<Expr> = types
        .into_iter()
        .zip(taken)
        .enumerate()
        .map(|(i, (ty, how))| {
            let mut value = Expr::new(ExprKind::Var(group_value(&name, i)), span);
            value.ty = Some(ty.clone());
            if places && matches!(how, Taken::Place(_)) {
                value = Expr { kind: ExprKind::Ref(Box::new(value)), span, ty: Some(ty) };
            }
            value
        })
        .collect();
    args.splice(at..=at, values);
}

/// The type a call in a copy gives the group of the task it calls, written
/// `ty` in the task the copy is made of (`(int, P...)` as a tuple), with the
/// values of this copy's group in place of `P...`, places as places (see
/// `spread_values`).
fn group_given_in_copy(ty: &Type, given: &HashMap<String, Type>) -> Type {
    let Type::Tuple(items) = ty else { return ty.map_groups(&mut |name| group_types(given, name)).map_params(&mut |name| copy_type(given, name)) };
    let (mut types, mut taken) = (Vec::new(), Vec::new());
    for item in items {
        match item {
            Type::Group(name) if let Some((group, how)) = group_items(given, name) => {
                types.extend(group);
                taken.extend(how);
            }
            other => {
                types.push(other.map_groups(&mut |name| group_types(given, name)).map_params(&mut |name| copy_type(given, name)));
                taken.push(Taken::Value);
            }
        }
    }
    group_binding(types, taken)
}

/// The mark before the place of a tuple's item read in a maybe, `t.#0`, kept
/// in the field's name until the checker makes it the read it is (see
/// `Checker::check_safe_item`).
pub const SAFE_ITEM: char = '#';

/// Added to a group's name, the key in the types a copy of a task is given
/// that marks it a group, holding its types (see `copy_type`).
const GROUP_MARK: &str = "...";

/// Added to a group's name, the key holding the type of the value of the turn
/// of an `each` over its values (see `Checker::each_of_group`).
const GROUP_TURN: &str = "*";

/// The marker between a group's name and the place of one of its types, in
/// the names a call of a task taking a group gives them (`P#0`), which no Vela
/// name holds.
const GROUP_ITEM: char = '#';

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

/// The warning that `what` (`'==' compares Point by where it is`) reads where
/// the parameter `name`, of type `ty`, is, which the compiler chose from its
/// `size`; `declared` is the type as the task writes it, for the hint, and
/// `given` names the use that gave the type, in a task that takes types.
fn chosen_message(what: &str, name: &str, ty: &Type, declared: &Type, size: usize, given: Option<String>) -> String {
    let written = ty.without_regions();
    // An address (`'&x' is where …`) compares no items, so '~=' is no way out.
    let address = what.starts_with("'&");
    let (whose, subject, words) = match (name == THIS, address) {
        (true, false) => ("the value the method is called on", "'.this'".to_string(), "; compare items with '~='".to_string()),
        (true, true) => ("the value the method is called on", "'.this'".to_string(), String::new()),
        (false, false) => (
            "a parameter",
            format!("'{}'", name),
            format!("; write 'fixed& {0}: {1}' for the caller's place, 'fixed {0}: {1}' for a copy of its own, or compare items with '~='", name, declared),
        ),
        (false, true) => (
            "a parameter",
            format!("'{}'", name),
            format!("; write 'fixed& {0}: {1}' for the caller's place, or 'fixed {0}: {1}' for a copy of its own", name, declared),
        ),
    };
    let given = given.map(|given| format!(" ({})", given)).unwrap_or_default();
    let went = if codegen::hidden_by_size(size) {
        format!(
            "{} takes {} bytes, more than {}, so {} is where the caller keeps it, or a copy the caller makes first of a value that might change while the task runs, such as one behind a pointer, in a slice or a shared field, or named by a reference",
            written,
            size,
            codegen::HIDDEN_ABOVE,
            subject
        )
    } else {
        format!("{} takes {} bytes, so {} is a copy, a place of its own, never the caller's", written, size, subject)
    };
    format!("{}, and {} is {}, given as the compiler chose{}: {}{}", what, subject, whose, given, went, words)
}

/// The end of the message for changing a parameter that never changes: how to
/// write one the task may change, and, with `shares`, that its copy still
/// names what the caller's value holds. Empty for anything else.
fn param_hint_for(name: &str, info: &VarInfo, shares: bool) -> String {
    if info.kept && info.passing == Passing::FixedRef {
        return format!("; this lambda keeps '{0}' as 'fixed& {0}', which only reads it, and one that changes it keeps it as 'flex& {0}'", name);
    }
    if info.kept {
        return format!("; '{0}' is the copy this lambda keeps of a variable of the code around it, which a '[@r]' lambda never changes; one that changes its copies is written '[flex @r](…)', and one that changes the variable itself keeps it as a place, '[flex& {0}, @r]'", name);
    }
    if info.refers.is_some() {
        return format!("; '{0}' only reads the place it names, and a reference that changes it is written 'flex& {0} = &…'", name);
    }
    if !info.param || name == THIS {
        return String::new();
    }
    if info.passing == Passing::FixedRef {
        return format!("; '{0}' is a 'fixed&' parameter, which only reads the caller's place, and one that changes it is written 'flex& {0}: {1}'", name, info.ty);
    }
    // The task's own copy of a handle still names what the caller's does.
    let own = match &info.ty {
        Type::Slice(..) => "the task's own copy of the slice, whose items are the caller's",
        Type::Map(..) => "the task's own copy of the map, which names the caller's map",
        Type::Ptr(..) => "the task's own copy of the pointer, which points to the caller's value",
        _ if shares => "the task's own copy, whose slices, maps and pointers name what the caller's do",
        _ => "and is the task's own copy",
    };
    format!("; a parameter that changes is written 'flex {}: {}', {}", name, info.ty, own)
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

/// What `place`, which `is_place` refuses, is instead, for a message: '… and
/// this is {}, with no place of its own'.
fn no_place(place: &Expr) -> &'static str {
    match &place.kind {
        ExprKind::Index(_, index) if matches!(index.kind, ExprKind::Range(..)) => "a part taken of a value, which is a new value",
        ExprKind::Index(base, _) if matches!(base.ty, Some(Type::Text(_))) => "a byte of text, read out of text that never changes",
        ExprKind::Lookup(..) => "a maybe that '[#…]' or 'at' made here",
        ExprKind::Field(base, _) if worked_out(base) => "a length, worked out from the value",
        ExprKind::Call { .. } | ExprKind::MethodCall { .. } => "a call's value, which is made here",
        ExprKind::Construct { .. } => "a variant, which is a value",
        ExprKind::Var(_) => "a variant, which is a value",
        ExprKind::Macro { .. } => "a macro, a value the compiler computed",
        _ => "a value made here",
    }
}

/// Whether a field of `base` is worked out from the value rather than kept
/// in it: `len` of an array, a slice, or text.
fn worked_out(base: &Expr) -> bool {
    matches!(base.ty, Some(Type::Array(..) | Type::Slice(..) | Type::Map(..) | Type::Text(_)))
}

/// Notes `e`, when it is an address (`&x`) of a parameter the compiler chose
/// how to give, or of a part of one, in a task made for types (see
/// `Made::reads_chosen`).
fn note_chosen_address(e: &Expr, made: &mut Made) {
    let ExprKind::Ref(place) = &e.kind else { return };
    if let Some((name, at)) = param_root(place).filter(|(name, _)| made.chosen.contains_key(*name)) {
        let written = place_path(place).map_or_else(|| name.to_string(), |path| path_text(&path));
        made.reads_chosen.push((at, format!("'&{}' is where the place is", written), name.to_string()));
    }
}

/// The variable a place written as `e` starts at, through its fields and
/// items, and where it is named: not through a pointer or a slice, whose items
/// are a place of their own, wherever the pointer or the slice is.
fn param_root(e: &Expr) -> Option<(&str, Span)> {
    match &e.kind {
        ExprKind::Var(name) => Some((name, e.span)),
        ExprKind::Field(base, _) => param_root(base),
        ExprKind::Index(base, index) if !matches!(index.kind, ExprKind::Range(..)) && !matches!(base.ty, Some(Type::Slice(..) | Type::Map(..))) => param_root(base),
        _ => None,
    }
}

/// Whether `a` and `b` are parts of one variable (`line.from`, `line.to`),
/// which are where they are alike however the variable was given.
fn same_root(a: &Expr, b: &Expr) -> bool {
    matches!((param_root(a), param_root(b)), (Some((x, _)), Some((y, _))) if x == y)
}

/// Whether `e` is written as a place: a variable, a shared field, what a
/// pointer points to, or a field or an item of one. (In a task made for
/// types, no variant is of a type the task takes, so a name is a variable.)
fn names_place(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Var(_) | ExprKind::Shared { .. } | ExprKind::Deref(_) => true,
        ExprKind::Field(base, _) => names_place(base),
        ExprKind::Index(base, index) => !matches!(index.kind, ExprKind::Range(..)) && !matches!(base.ty, Some(Type::Text(_))) && names_place(base),
        _ => false,
    }
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

/// What takes a place that changes: a `flex&` parameter, or a `flex&` local
/// reference, by its name, which the errors about the place name.
enum Taker<'a> {
    Param(&'a str),
    Reference(&'a str),
    /// A lambda keeping the variable as a place, `[flex& count, @r]`.
    Kept(&'a str),
}

impl Taker<'_> {
    /// How an error about the place starts.
    fn lead(&self) -> String {
        match self {
            Taker::Param(name) => format!("'{}' is given the place itself, to change", name),
            Taker::Reference(name) => format!("'flex& {}' names a place to change it", name),
            Taker::Kept(name) => format!("this lambda keeps '{0}' as 'flex& {0}', to change it", name),
        }
    }

    /// What giving it `place` is, for the error that it changes `.this`.
    fn using(&self, place: &str) -> String {
        match self {
            Taker::Param(name) => format!("giving '{}' to '{}'", place, name),
            Taker::Reference(name) => format!("naming '{}' with 'flex& {}'", place, name),
            Taker::Kept(name) => format!("keeping '{}' as 'flex& {}'", place, name),
        }
    }
}
