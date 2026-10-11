use std::fmt;

use crate::error::{Error, Result};

/// Where the names inside each written type are: for the type starting at
/// `(file, line, col)`, each shape, kind, or builtin name, `@region`, `maybe`,
/// `region`, and `~` (at the error type after it), with its position. The
/// types themselves carry no positions; errors about a part of a type use this
/// to point at that part.
pub type TypePlaces = std::collections::HashMap<(usize, usize, usize), Vec<(String, Span)>>;

/// A position in a source file (1-based line and column). `file` tells which
/// file of a program: 0 is the file the program was compiled from, and each
/// module it includes gets the next number (see `modules::SourceFile`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub line: usize,
    pub col: usize,
    pub file: usize,
}

impl Span {
    /// A position in the program's first file.
    pub fn new(line: usize, col: usize) -> Self {
        Span { line, col, file: 0 }
    }

    /// The key of the type written at this position in `TypePlaces`.
    pub fn place_key(self) -> (usize, usize, usize) {
        (self.file, self.line, self.col)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    Int,
    Float,
    Bool,
    /// `text`, whose characters last as long as the program (a literal's), or
    /// `text@r`, whose characters are in the region `r`. Text that lasts as
    /// long as the program fits wherever text of a region is expected.
    Text(Option<String>),
    /// `unit`: a whole number from 0 to 255 in one byte (Rust's `u8`).
    Unit,
    /// `char`: one byte, as C's `char` is, written `'a'`: the code of a
    /// character, 0 to 255, so it holds any byte of text.
    Char,
    /// `nothing`: what a task that sends no value sends. It has no values.
    Nothing,
    /// The type of the `none` literal: fits any `maybe` type.
    None,
    /// `^T@r`: a pointer into the region `r`. Everything a type holds lives in
    /// one region, so the checker gives `T`'s own pointers and shapes the same
    /// region. A shape's or kind's fields have no region: theirs is the region
    /// of the value holding them.
    Ptr(Box<Type>, Option<String>),
    /// A shape or kind, with the region its pointers point into when it holds
    /// any (`Node@r`).
    Named(String, Option<String>),
    Range(Box<Type>),
    /// `maybe T`: a `T`, or `none`.
    Maybe(Box<Type>),
    /// `T ~ E`: a `T` on success, or an error `E`. `T` may be `nothing`.
    Or(Box<Type>, Box<Type>),
    /// The type of `fail e` before it becomes part of a `T ~ E`: the error's type.
    Fail(Box<Type>),
    /// `region`: the type of a region parameter, which a task allocates in.
    Region,
    /// `[T#n]`: `n` values of type `T`, held by value. It holds pointers, and
    /// lives in a region, when `T` does.
    Array(Box<Type>, u64),
    /// `[T]@r`: a view of values of type `T` in the region `r`, as a pointer
    /// to the first and how many there are. Like a pointer, it names its region
    /// everywhere but in a field and after `new`.
    Slice(Box<Type>, Option<String>),
    /// `[K: V]@r`: a hash map from keys of type `K` to values of type `V`, its
    /// table in the region `r`. A value is a handle: copies share one map. Like
    /// a slice, it names its region everywhere but in a field and after `new`.
    /// `[:V]` is written for `[text: V]`.
    Map(Box<Type>, Box<Type>, Option<String>),
    /// `(int, text)`: two or more values held together, read as `t.0`, `t.1`.
    /// Its items never change, so they are only values that can't change
    /// (no pointers, slices, or arrays). Each item has its own regions:
    /// `(int, text)@r` is written for `(int, text@r)`.
    Tuple(Vec<Type>),
    /// `-> int, int`: the several values a task sends, always two or more.
    /// Only a task's return type, and a call of such a task, ever have this
    /// type: the values are separate, so no variable, field, or operand holds
    /// one.
    Results(Vec<Type>),
    /// `Box<int>` or `Box<T>`: a shape with the types it is given, written where
    /// a type goes. A shape that takes types is never generated: each set of
    /// types becomes a shape of its own (see the checker's `shape_instance`), so
    /// a checked type names that shape instead, and this is left only inside a
    /// declaration that takes types itself, where a type given is not known yet.
    /// The types given name no region, as a shape's fields don't: they are in the
    /// region of the value holding them, which the whole type names.
    ///
    /// Boxed so that a `Type` stays small: they are held by the thousand while
    /// a program is checked, and deeply nested ones are checked on a small stack.
    Applied(Box<Applied>),
    /// `(int, text) -> int`: a task as a value, which a variable, a field, or a
    /// parameter holds and a call runs (see `TaskType`). Boxed, as `Applied` is.
    Task(Box<TaskType>),
    /// `[@r](int) -> int`: a lambda that keeps what it uses from the code around
    /// it in region `r`, which its type names, so it can't outlive it (see
    /// `ClosureType`). A plain task value goes where one is expected, never the
    /// other way.
    Closure(Box<ClosureType>),
    /// `T`: a type a generic declaration takes, standing for the type given at
    /// each use. It appears only inside the declaration that names it; a use is
    /// checked with the type itself in its place. Its regions are the given
    /// type's, so it holds no region of its own.
    Param(String),
    /// `P...`: the group of types `P` a generic task takes (see
    /// `TypeParam::group`), as the type of its last parameter, `args: P...`,
    /// or as the whole of what a task's type takes, `(P...) -> R`. Each use
    /// puts the types it gives in its place, as many as there are.
    Group(String),
    /// No type: what the checker uses in place of a type it could not work out,
    /// after reporting why. It stands in wherever the mistake was, so that the
    /// checks that follow have something to go on and report their own mistakes
    /// rather than this one again: it fits every type and every type fits it,
    /// and nothing about it is ever an error (see `Type::unknown`). No program
    /// that checks holds one, so the code generator never sees it.
    Unknown,
}

/// A shape with the types it is given, `Box<int>` (see `Type::Applied`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Applied {
    pub name: String,
    /// The types it is given, always at least one.
    pub types: Vec<Type>,
    /// The region its values live in, when it holds anything in one.
    pub region: Option<String>,
}

impl Applied {
    /// The same shape, given `types`.
    pub fn with_types(&self, types: Vec<Type>) -> Applied {
        Applied { name: self.name.clone(), types, region: self.region.clone() }
    }
}

/// A task's type, `(int, text) -> int`: the types of its parameters, and what it
/// sends (`nothing` for no value), written as a header without names. Its regions
/// are its own, as a header's are: `(text@r) -> text@r` takes text of any region
/// and sends text of the same one, so a task value holds nothing in a region and
/// goes anywhere. The checker names them in the order they first appear (see
/// `TaskType::normalized`), so two types that say the same are equal.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TaskType {
    pub params: Vec<Type>,
    /// How each of `params` is given, in its order.
    pub taken: Vec<Taken>,
    pub ret: Type,
}

/// The type of a lambda that keeps what it uses, `[@r](int) -> int` or
/// `[flex @r](int) -> int` (see `Type::Closure`): the task's type it is called
/// as, the region it keeps its copies in, and whether it changes them. Its
/// task's type may name `region`, the one kept, beside regions of its own.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClosureType {
    pub task: TaskType,
    pub region: String,
    /// `[flex @r]`: it changes its copies, so a value of it is never copied.
    pub flex: bool,
}

impl ClosureType {
    /// The same type with the task's own regions named in the order they
    /// first appear, the kept one as it is (see `TaskType::normalized`).
    pub fn normalized(&self) -> ClosureType {
        ClosureType { task: self.task.normalized_beside(Some(&self.region)), region: self.region.clone(), flex: self.flex }
    }

    /// The type with its task's type remade by `f`, as a task type.
    fn with_task(&self, f: impl FnOnce(&Type) -> Type) -> Type {
        match f(&Type::Task(Box::new(self.task.clone()))) {
            Type::Task(task) => Type::Closure(Box::new(ClosureType { task: *task, region: self.region.clone(), flex: self.flex })),
            other => other,
        }
    }
}

/// The types a use gives a group, each with how a task takes it (see
/// `Type::map_group_items`).
pub type GroupItems = (Vec<Type>, Vec<Taken>);

/// What the task type holding a group's types sends, where one is a place:
/// no results, which no task sends (see `check::group_binding`). Its regions
/// are the group's, unlike a task type's, which are its own.
pub const GROUP_SENDS: Type = Type::Results(Vec::new());

/// How a parameter of a task type is given at a call: as a value, as a value
/// whose items the task changes, as the caller's place, or as a region, which
/// the type names so that what the task sends can live there:
/// `(flex& int) -> nothing`, `(flex [int]@r) -> nothing`, `(@r, int) -> [int]@r`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Taken {
    /// A value the task doesn't change through: a whole value, `int`, which the
    /// task takes as `n: int`, `flex n: int`, or `fixed n: int`, all one to its
    /// caller; or a twofold one, a slice, a map or a pointer or what holds one,
    /// taken `xs: [int]@r` or `fixed xs: [int]@r`.
    Value,
    /// A twofold value the task may change through, `flex [int]@r`: a task
    /// taking `flex xs: [int]@r` changes the items the caller's slice names. A
    /// task taking it as a `Value` fits where one taking it so is expected, but
    /// never the other way (see `Checker::task_fits`).
    Flex,
    /// A value the task may change the contents of, but never assign as a whole.
    /// The compiler keeps this distinct from `Value` even for simple types.
    Bind,
    /// The caller's place, `flex& int` or `fixed& int` (`Passing::FlexRef` or
    /// `Passing::FixedRef`), which the call marks with `&`.
    Place(Passing),
    /// A region, `@r`, whose type in `params` is `Type::Region`.
    Region(String),
}

/// What marks a region of a task type's own while it is named again (see
/// `TaskType::normalized_avoiding`), which no region a program writes holds.
const OWN_MARK: &str = "~own";

/// The names a task type's regions are given, in the order they first appear
/// (see `TaskType::normalized`); a type with more names the rest `r7`, `r8`, ….
const TASK_REGIONS: [&str; 6] = ["r", "s", "t", "u", "v", "w"];

impl TaskType {
    /// The type of a task taking `params` as values and sending `ret`.
    pub fn new(params: Vec<Type>, ret: Type) -> TaskType {
        let taken = vec![Taken::Value; params.len()];
        TaskType { params, taken, ret }
    }

    /// How parameter `i` is given, as a header says it (a region as a value).
    pub fn passing(&self, i: usize) -> Passing {
        match self.taken.get(i) {
            Some(Taken::Place(passing)) => *passing,
            Some(Taken::Flex) => Passing::Flex,
            _ => Passing::Copy,
        }
    }

    /// The name of parameter `i` when it is a region, `@r`.
    pub fn region(&self, i: usize) -> Option<&str> {
        match self.taken.get(i) {
            Some(Taken::Region(name)) => Some(name),
            _ => None,
        }
    }

    /// The same type with its regions named in the order they first appear,
    /// `r`, then `s`, and so on, so that `(text@a) -> text@a` and
    /// `(text@b) -> text@b`, which say the same, are equal.
    pub fn normalized(&self) -> TaskType {
        self.normalized_beside(None)
    }

    /// The type with each region it names, its own ones included, replaced by
    /// what `f` gives for it.
    pub fn map_regions(&self, f: &mut dyn FnMut(&str) -> Option<String>) -> TaskType {
        let taken = self
            .taken
            .iter()
            .map(|how| match how {
                Taken::Region(name) => Taken::Region(f(name).unwrap_or_else(|| name.clone())),
                other => other.clone(),
            })
            .collect();
        TaskType { params: self.params.iter().map(|ty| ty.map_regions(&mut *f)).collect(), taken, ret: self.ret.map_regions(f) }
    }

    /// The type of a plain task, whose regions are all its own, as the task's
    /// type of a lambda keeping what it uses in `kept` names them (see
    /// `ClosureType`): none of them named `kept`.
    pub fn normalized_avoiding(&self, kept: &str) -> TaskType {
        self.map_regions(&mut |name| Some(if name == ANY_REGION { name.to_string() } else { format!("{}{}", name, OWN_MARK) })).normalized_beside(Some(kept))
    }

    /// `normalized`, leaving `kept` as it is: the region a lambda's type keeps
    /// its copies in (see `ClosureType`), which no region of its own is named.
    pub fn normalized_beside(&self, kept: Option<&str>) -> TaskType {
        let mut names: Vec<String> = Vec::new();
        let mut rename = |region: &str| -> Option<String> {
            if region == ANY_REGION || Some(region) == kept {
                return Some(region.to_string());
            }
            let at = match names.iter().position(|name| name == region) {
                Some(at) => at,
                None => {
                    names.push(region.to_string());
                    names.len() - 1
                }
            };
            let named = TASK_REGIONS.iter().map(|name| name.to_string()).chain((TASK_REGIONS.len() + 1..).map(|k| format!("r{}", k)));
            named.filter(|name| Some(name.as_str()) != kept).nth(at)
        };
        let mut params = Vec::new();
        let mut taken = Vec::new();
        for (ty, how) in self.params.iter().zip(&self.taken) {
            taken.push(match how {
                Taken::Region(name) => Taken::Region(rename(name).unwrap_or_else(|| name.clone())),
                other => other.clone(),
            });
            params.push(ty.map_regions(&mut rename));
        }
        let ret = self.ret.map_regions(&mut rename);
        TaskType { params, taken, ret }
    }
}

/// The region of a value built without pointers, such as `Node { next: none }`:
/// it fits a value of any region, and is never a variable's region.
pub const ANY_REGION: &str = "_";

/// The builtin types by name: the one list the parser, the checker, and tools
/// such as editors use.
pub const BUILTIN_TYPES: &[(&str, Type)] = &[
    ("int", Type::Int),
    ("float", Type::Float),
    ("bool", Type::Bool),
    ("text", Type::Text(None)),
    ("unit", Type::Unit),
    ("char", Type::Char),
    ("nothing", Type::Nothing),
];

/// The builtin tasks, which user code calls like its own tasks (`scan` names
/// a region after its call, as `new` does).
pub const BUILTIN_TASKS: &[&str] = &["show", "scan"];

/// The builtin task that reads a line of input: `scan(prompt) at r`.
pub const SCAN: &str = "scan";

/// The word of `value.copy()`, written after a dot like `add` and `del`.
pub const COPY: &str = "copy";

/// The method that turns a value into text in a region, `n.str() at r` (also
/// `n.str(r)`, and `text(n) at r`). Numbers, bools, and text have it built in,
/// and a shape or kind gives its own, `str(r: region) -> text@r`.
pub const STR: &str = "str";

/// `new`, a keyword, which also names a shared method: `X.new(…)` (see
/// `Owner::shared`).
pub const NEW_WORD: &str = "new";

/// The word other languages declare a macro with, `macro P = …`, which Vela
/// writes `#P = …` (see `ast::MacroDef`); only an error names it.
pub const MACRO: &str = "macro";

/// The type whose name, called, turns a value into text: `text(n) at r`.
pub const TEXT_CAST: &str = "text";

/// The type whose name, called, turns a value into an `int`: `int(2.9)`, which
/// truncates toward zero, and `int(ok)`, which is 1 or 0.
pub const INT_CAST: &str = "int";

/// The type whose name, called, turns a value into a `float`: `float(n)`.
pub const FLOAT_CAST: &str = "float";

/// The type whose name, called, turns a value into a `unit`: `unit(n)`, which
/// stops the program when `n` is not from 0 to 255.
pub const UNIT_CAST: &str = "unit";

/// The type whose name, called, turns a number into a `char`: `char(65)` is
/// `'A'`, and a number that is not from 0 to 255 stops the program.
pub const CHAR_CAST: &str = "char";

/// The method text has that reads one byte without stopping the program:
/// `t.at(i)`, a `maybe char`, none when `i` is out of range.
pub const AT: &str = "at";

/// The number that `name`, called as a task, turns a value into: `int(x)` and
/// `float(x)`, which make no memory, so they name no region as `text(x)` does.
pub fn number_cast(name: &str) -> Option<Type> {
    match name {
        INT_CAST => Some(Type::Int),
        FLOAT_CAST => Some(Type::Float),
        UNIT_CAST => Some(Type::Unit),
        CHAR_CAST => Some(Type::Char),
        _ => None,
    }
}

/// The kind that reading a number from text sends when the text is no number:
/// `int("42")` is an `int ~ ParseError`. No program declares it, and none may
/// take its name or the names of its variants.
pub const PARSE_ERROR: &str = "ParseError";

/// Its variants: text that is no number at all, and text that is a number too
/// large for the type read. Neither holds a value.
pub const NOT_A_NUMBER: &str = "NotANumber";
pub const TOO_LARGE: &str = "TooLarge";

/// The kind that reading a number from input sends: `scan("Age: ", int)` is an
/// `int ~ InputError`. Reading from input can end, which reading text cannot,
/// so it says that apart from the text being no number, which it holds as the
/// `ParseError` reading the line gave.
pub const INPUT_ERROR: &str = "InputError";

/// Its variants: the input has ended, and the line was no number, which holds
/// why (a `ParseError`).
pub const ENDED: &str = "Ended";
pub const NO_NUMBER: &str = "NoNumber";

/// The name of the reason `NoNumber` holds: `case NoNumber why`.
pub const WHY: &str = "why";

/// The names the builtin kinds bring, which no program may declare: each kind
/// and its variants, so that a program writes them as it writes any kind's.
pub const BUILTIN_KIND_NAMES: &[&str] = &[PARSE_ERROR, NOT_A_NUMBER, TOO_LARGE, INPUT_ERROR, ENDED, NO_NUMBER];

/// The kind each builtin variant belongs to, for a message about the name.
pub fn builtin_kind_of(variant: &str) -> Option<&'static str> {
    match variant {
        NOT_A_NUMBER | TOO_LARGE => Some(PARSE_ERROR),
        ENDED | NO_NUMBER => Some(INPUT_ERROR),
        _ => None,
    }
}

/// Both kinds no program declares, in the order the code generator needs them:
/// `InputError` holds a `ParseError`.
pub fn builtin_kinds() -> Vec<KindDef> {
    vec![parse_error_kind(), input_error_kind()]
}

/// `InputError` as a declaration, as `parse_error_kind` is. Its `NoNumber` holds
/// the `ParseError` that reading the line gave, so the two kinds say each reason
/// once (see `docs/future.html#inner-kind-variants` for the shortcut this waits
/// for).
pub fn input_error_kind() -> KindDef {
    let span = builtin_span();
    let why = Field {
        name: WHY.to_string(),
        ty: Type::Named(PARSE_ERROR.to_string(), None),
        span,
        ty_span: span,
        private: false,
        read: false,
        default_region: false,
        passing: Passing::Copy,
        default: None,
    };
    KindDef {
        types: Vec::new(),
        given: Vec::new(),
        name: INPUT_ERROR.to_string(),
        variants: vec![
            Variant { tag: ENDED.to_string(), payload: Vec::new(), span },
            Variant { tag: NO_NUMBER.to_string(), payload: vec![why], span },
        ],
        span,
        private: false,
    }
}

/// The position of a declaration no file holds: a file no program has, so a
/// message about a name it takes says which kind holds it rather than pointing
/// at a line (see `check::Checker::define`).
fn builtin_span() -> Span {
    Span { line: 0, col: 0, file: usize::MAX }
}

/// `ParseError` as a declaration, which the checker knows without reading it and
/// the code generator writes the C for (see `check::Checker::check_program`).
pub fn parse_error_kind() -> KindDef {
    let span = builtin_span();
    let variant = |tag: &str| Variant { tag: tag.to_string(), payload: Vec::new(), span };
    KindDef {
        types: Vec::new(),
        given: Vec::new(),
        name: PARSE_ERROR.to_string(),
        variants: vec![variant(NOT_A_NUMBER), variant(TOO_LARGE)],
        span,
        private: false,
    }
}

/// How many items an array or slice holds, read after a dot as a field.
pub const LEN: &str = "len";

/// `args.tuple`, or `(args...)`: a group's values made into a tuple (see
/// `Checker::check_group_member`).
pub const GROUP_TUPLE: &str = "tuple";

/// `(args...)`, read as a member no field can be named, so that only a group's
/// values make it (the checker reads it as `GROUP_TUPLE` then).
pub const GROUP_TUPLE_WRITTEN: &str = "...";

/// The word before a method that changes the value it is called on, written
/// inside a shape or a kind (`alters step()`). Not a keyword: it marks a member
/// and means nothing anywhere else, so a task or a variable may still use it.
pub const ALTERS: &str = "alters";

/// The word that marks a field any file may read and only its own file may
/// assign, written before the field's name (`read path: text`). Not a keyword:
/// like `alters`, it marks one thing in one position and means nothing anywhere
/// else, so a task, a field, or a method may still be called `read`, since a
/// field named so is written `read:` and a method `read(`.
pub const READ: &str = "read";

/// The word before `task` that marks a task of Vela's library whose body is a
/// function of the runtime (see `library`): `native task path_exists(path: text@p) -> bool`,
/// with no body and no `fi`. Not a keyword: it means something only there, so a
/// program may still name anything `native`.
pub const NATIVE: &str = "native";

/// The word that marks a block's default region, written after the region's name
/// in its declaration (`region r default`) or after the type of a region a task
/// takes (`r: region default`). Allocations that name no region go there. Not a
/// keyword: like `alters`, it marks one thing in one position and means nothing
/// anywhere else, so a task, a field, or a variable may still be called it.
pub const DEFAULT: &str = "default";

/// The word after a dot for the type of a value, `x.type`: its type as code
/// writes it, text the compiler knows. `type` is a reserved word, so no field
/// or method takes it.
pub const TYPE_WORD: &str = "type";

/// The word after the dot for the whole value a method is called on (`.this`),
/// and the name of the region that value lives in (`text@this`).
pub const THIS_WORD: &str = "this";

/// The name of the parameter holding the value a method is called on, which the
/// body writes `.this` and a member of it `.x`. No Vela name holds a `.`, so
/// nothing can shadow it or be written in its place, and a message that names
/// it reads as the program writes it.
pub const THIS: &str = ".this";

/// The start of a wrapper's name written `$(.checked)`: a method of the value
/// the wrapped method is called on, which no other name starts with.
pub const MEMBER_WRAPPER: &str = ".";

/// The name of the region a `show` makes for the values it writes with their own
/// `str`, freed once the line ends (see `codegen::gen_show`). As `.this` is, it
/// holds a `.`, which no Vela name does, so no program can name it or shadow it.
pub const SHOWN: &str = ".shown";

/// The name of the task a method becomes: `Counter__on__step` for the method
/// `step` of `Counter`. No Vela name holds `__`, so it can be no declaration of
/// a program, and a module's path stays in front of it
/// (`shapes/counter.Counter__on__step`).
pub fn method_name(owner: &str, method: &str) -> String {
    format!("{}__on__{}", owner, method)
}

/// The name of the task the `n`th lambda of a file becomes: `lambda__1`. No
/// Vela name holds `__`, so it can be no declaration of a program, and a
/// module's path stays in front of it (`ops.lambda__1`).
pub fn lambda_name(n: usize) -> String {
    format!("{}{}", LAMBDA_PREFIX, n)
}

/// What the name of every task a lambda becomes starts with (see `lambda_name`).
pub const LAMBDA_PREFIX: &str = "lambda__";

/// The name of the hidden shape of what lambda `lambda` keeps, `[@r](…)`:
/// a field for each value of the code around it the lambda uses.
pub fn kept_name(lambda: &str) -> String {
    format!("{}__kept", lambda)
}

/// Whether `name`, with its module or without, is the task a lambda became.
pub fn is_lambda(name: &str) -> bool {
    unqualified(name).starts_with(LAMBDA_PREFIX)
}

/// The name of the task each module's startup decorators are gathered into
/// (see `TaskDef::decorators`), with the module's path in front of it
/// (`web.startup__`). No Vela name holds `__`, so no program can name it.
pub const STARTUP: &str = "startup__";

/// Whether `name`, with its module or without, is a module's startup task.
pub fn is_startup(name: &str) -> bool {
    unqualified(name) == STARTUP
}

/// The name of the task layer `k` of task `name`'s wrappers becomes: the one
/// that calls wrapper `k + 1`, or, for the last, the task's own body
/// (`sq__layer__2`). No Vela name holds `__`, so no program can name it.
pub fn layer_name(name: &str, k: usize) -> String {
    format!("{}{}{}", name, LAYER, k)
}

/// What a wrapper layer's name holds after its task's (see `layer_name`).
pub const LAYER: &str = "__layer__";

/// The shape whose shared fields hold the values of wrappers worked out where
/// the program starts (`$(logged(label("sq")))`), one each, which every call of
/// the wrapped task reads: as a shared field's, each is given its value where
/// the program starts, after the program's own shared fields and before the
/// startup decorators. No Vela name holds `__`, so no program can name it.
pub const START_VALUES: &str = "wrapper__values";

/// Task `name` as a message names it: `task 'f'`, or `this lambda`.
pub fn shown_task(name: &str) -> String {
    if is_lambda(name) { "this lambda".to_string() } else { format!("task '{}'", name) }
}

/// The shape or kind, and the method, in the name of the task a method becomes.
pub fn method_parts(name: &str) -> Option<(&str, &str)> {
    name.split_once("__on__")
}

/// Whether working out `e` does nothing a program could see, a stop included:
/// names, literals, fields, and comparisons of them. A call, an index, a
/// lookup, or arithmetic, which may stop on a mistake or an overflow, does.
pub fn without_effects(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::IntLit(_) | ExprKind::FloatLit(_) | ExprKind::CharLit(_) | ExprKind::BoolLit(_) | ExprKind::TextLit(_) | ExprKind::NoneLit | ExprKind::Macro { .. } | ExprKind::Var(_) | ExprKind::Shared { .. } | ExprKind::TaskValue(_) => true,
        ExprKind::Field(inner, _) | ExprKind::Deref(inner) | ExprKind::Ref(inner) | ExprKind::Wrap(inner) | ExprKind::Unary(UnaryOp::Not, inner) => without_effects(inner),
        ExprKind::Binary(BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Similar | BinaryOp::NotSimilar | BinaryOp::Lt | BinaryOp::Gt | BinaryOp::Le | BinaryOp::Ge | BinaryOp::And | BinaryOp::Or, a, b) | ExprKind::Range(a, b) => {
            without_effects(a) && without_effects(b)
        }
        ExprKind::Array(items) | ExprKind::Tuple(items) => items.iter().all(without_effects),
        _ => false,
    }
}

/// Whether `word`, written after a dot and followed by `(`, is one of the
/// builtins every value or slice has rather than a kind's variant or a
/// module's task: `add`, `del`, or `copy`.
pub fn is_builtin_member(word: &str) -> bool {
    EditOp::from_word(word).is_some() || word == COPY
}

impl fmt::Display for Type {
    /// Formats the type as it is written in Vela source, naming each region
    /// once: `^Node@r`, not `^Node@r@r`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.write(f, None)
    }
}

impl Type {
    /// Writes the type inside a pointer or shape of region `outer`.
    fn write(&self, f: &mut fmt::Formatter<'_>, outer: Option<&str>) -> fmt::Result {
        let suffix = |f: &mut fmt::Formatter<'_>, region: &Option<String>| match region {
            Some(region) if Some(region.as_str()) != outer => write!(f, "@{}", region),
            _ => Ok(()),
        };
        // A `~` holds the whole type before it, so a `T ~ E` written after a
        // prefix needs its parentheses to read back as itself: `maybe (int ~ text)`
        // is not `maybe int ~ text`, which is `(maybe int) ~ text`. Brackets and
        // commas close a type themselves, so an item needs none.
        let grouped = |f: &mut fmt::Formatter<'_>, inner: &Type, outer: Option<&str>| match inner {
            Type::Or(..) => {
                write!(f, "(")?;
                inner.write(f, outer)?;
                write!(f, ")")
            }
            _ => inner.write(f, outer),
        };
        match self {
            Type::Int => write!(f, "int"),
            Type::Float => write!(f, "float"),
            Type::Bool => write!(f, "bool"),
            Type::Text(region) => {
                write!(f, "text")?;
                suffix(f, region)
            }
            Type::Unit => write!(f, "unit"),
            Type::Char => write!(f, "char"),
            Type::Nothing => write!(f, "nothing"),
            Type::None => write!(f, "none"),
            Type::Region => write!(f, "region"),
            Type::Ptr(inner, region) => {
                write!(f, "^")?;
                grouped(f, inner, region.as_deref().or(outer))?;
                suffix(f, region)
            }
            Type::Named(name, region) => {
                write!(f, "{}", name)?;
                suffix(f, region)
            }
            Type::Applied(applied) => {
                write!(f, "{}<", applied.name)?;
                for (i, ty) in applied.types.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    ty.write(f, None)?;
                }
                write!(f, ">")?;
                suffix(f, &applied.region)
            }
            // A type a variant's values didn't tell yet (`Option.None`), which
            // the checker names `?T`: the program never writes it.
            Type::Param(name) if name.starts_with('?') => write!(f, "…"),
            Type::Param(name) => write!(f, "{}", name),
            Type::Group(name) => write!(f, "{}...", name),
            // Its regions are its own, so none is left out for an outer one.
            Type::Task(task) => {
                write!(f, "(")?;
                for (i, ty) in task.params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    match task.taken.get(i) {
                        Some(Taken::Region(name)) => {
                            write!(f, "@{}", name)?;
                            continue;
                        }
                        Some(Taken::Place(passing)) => write!(f, "{} ", passing.word())?,
                        Some(Taken::Flex) => write!(f, "{} ", Passing::Flex.word())?,
                        Some(Taken::Bind) => write!(f, "{} ", Passing::Bind.word())?,
                        _ => {}
                    }
                    ty.write(f, None)?;
                }
                // The types given a group holding a place (see `GROUP_SENDS`).
                if task.ret == GROUP_SENDS {
                    return write!(f, ")");
                }
                write!(f, ") -> ")?;
                task.ret.write(f, None)
            }
            Type::Closure(closure) => {
                write!(f, "[{}@{}]", if closure.flex { "flex " } else { "" }, closure.region)?;
                Type::Task(Box::new(closure.task.clone())).write(f, None)
            }
            Type::Array(inner, count) => {
                write!(f, "[")?;
                inner.write(f, outer)?;
                write!(f, "#{}]", count)
            }
            Type::Slice(inner, region) => {
                write!(f, "[")?;
                inner.write(f, region.as_deref().or(outer))?;
                write!(f, "]")?;
                suffix(f, region)
            }
            Type::Map(key, value, region) => {
                write!(f, "[")?;
                key.write(f, region.as_deref().or(outer))?;
                write!(f, ": ")?;
                value.write(f, region.as_deref().or(outer))?;
                write!(f, "]")?;
                suffix(f, region)
            }
            Type::Range(inner) => {
                write!(f, "range of ")?;
                inner.write(f, outer)
            }
            Type::Maybe(inner) => {
                write!(f, "maybe ")?;
                grouped(f, inner, outer)
            }
            Type::Or(value, error) => {
                // `(int ~ text) ~ text`, which only a generic makes: `~` holds
                // the whole type before it, so the inner one is grouped, as is
                // a task, whose result would take the `~`.
                match &**value {
                    Type::Or(..) => grouped(f, value, outer)?,
                    Type::Task(_) | Type::Closure(_) => {
                        write!(f, "(")?;
                        value.write(f, outer)?;
                        write!(f, ")")?;
                    }
                    Type::Maybe(inner) if matches!(**inner, Type::Task(_) | Type::Closure(_)) => {
                        write!(f, "(")?;
                        value.write(f, outer)?;
                        write!(f, ")")?;
                    }
                    _ => value.write(f, outer)?,
                }
                write!(f, " ~ ")?;
                error.write(f, outer)
            }
            Type::Fail(error) => {
                write!(f, "fail ")?;
                grouped(f, error, outer)
            }
            Type::Tuple(items) => {
                write!(f, "(")?;
                for (i, ty) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    ty.write(f, outer)?;
                }
                write!(f, ")")
            }
            Type::Results(types) => {
                for (i, ty) in types.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    ty.write(f, outer)?;
                }
                Ok(())
            }
            // A message never names this type: every check that could report one
            // says nothing about it (see `Type::unknown`). It is written for a
            // compiler bug report, if one ever slips through.
            Type::Unknown => write!(f, "an unknown type"),
        }
    }
}

impl Type {
    /// The builtin type called `name`, if there is one.
    pub fn builtin(name: &str) -> Option<Type> {
        BUILTIN_TYPES.iter().find(|(builtin, _)| *builtin == name).map(|(_, ty)| ty.clone())
    }

    pub fn is_numeric(&self) -> bool {
        matches!(self, Type::Int | Type::Float | Type::Unit)
    }

    /// Whether the type is a whole number that fits in a byte, which an int
    /// literal becomes where one is expected (`flex b: unit = 200`).
    pub fn is_byte(&self) -> bool {
        matches!(self, Type::Unit)
    }

    /// Whether the type, or any type inside it, is the one the checker could not
    /// work out (`Type::Unknown`). Every check that would report a mistake about
    /// a type asks this first and says nothing when it is true: the mistake has
    /// already been reported where it was made, and saying more about it would
    /// only bury the other mistakes in the file.
    pub fn unknown(&self) -> bool {
        match self {
            Type::Unknown => true,
            Type::Ptr(inner, _) | Type::Slice(inner, _) | Type::Array(inner, _) | Type::Range(inner) | Type::Maybe(inner) | Type::Fail(inner) => inner.unknown(),
            Type::Or(value, error) | Type::Map(value, error, _) => value.unknown() || error.unknown(),
            Type::Tuple(types) | Type::Results(types) => types.iter().any(Type::unknown),
            Type::Applied(applied) => applied.types.iter().any(Type::unknown),
            Type::Task(task) => task.params.iter().any(Type::unknown) || task.ret.unknown(),
            Type::Closure(closure) => closure.task.params.iter().any(Type::unknown) || closure.task.ret.unknown(),
            Type::Int | Type::Float | Type::Bool | Type::Text(_) | Type::Unit | Type::Char | Type::Nothing | Type::None | Type::Region | Type::Named(..) | Type::Param(_) | Type::Group(_) => false,
        }
    }

    pub fn ptr_inner(&self) -> Option<&Type> {
        match self {
            Type::Ptr(t, _) => Some(t),
            _ => None,
        }
    }

    /// A shape or kind called `name`, outside any region.
    pub fn named(name: &str) -> Type {
        Type::Named(name.to_string(), None)
    }

    /// The type with every region removed, as C sees it.
    pub fn without_regions(&self) -> Type {
        self.map_regions(&mut |_| None)
    }

    /// The type with each region replaced by what `f` gives for it.
    pub fn map_regions(&self, f: &mut dyn FnMut(&str) -> Option<String>) -> Type {
        let boxed = |t: &Type, f: &mut dyn FnMut(&str) -> Option<String>| Box::new(t.map_regions(f));
        match self {
            Type::Ptr(inner, region) => Type::Ptr(boxed(inner, f), region.as_deref().and_then(&mut *f)),
            Type::Named(name, region) => Type::Named(name.clone(), region.as_deref().and_then(f)),
            Type::Applied(applied) => Type::Applied(Box::new(Applied {
                name: applied.name.clone(),
                types: applied.types.iter().map(|ty| ty.map_regions(&mut *f)).collect(),
                region: applied.region.as_deref().and_then(&mut *f),
            })),
            Type::Text(region) => Type::Text(region.as_deref().and_then(f)),
            Type::Array(inner, count) => Type::Array(boxed(inner, f), *count),
            Type::Slice(inner, region) => Type::Slice(boxed(inner, f), region.as_deref().and_then(&mut *f)),
            Type::Map(key, value, region) => Type::Map(boxed(key, f), boxed(value, f), region.as_deref().and_then(&mut *f)),
            Type::Range(inner) => Type::Range(boxed(inner, f)),
            Type::Maybe(inner) => Type::Maybe(boxed(inner, f)),
            Type::Or(value, error) => Type::Or(boxed(value, f), boxed(error, f)),
            Type::Fail(error) => Type::Fail(boxed(error, f)),
            Type::Results(types) => Type::Results(types.iter().map(|ty| ty.map_regions(&mut *f)).collect()),
            Type::Tuple(items) => Type::Tuple(items.iter().map(|ty| ty.map_regions(&mut *f)).collect()),
            // The types given a group holding a place, whose regions are the
            // caller's (see `GROUP_SENDS`).
            Type::Task(task) if task.ret == GROUP_SENDS => {
                Type::Task(Box::new(TaskType { params: task.params.iter().map(|ty| ty.map_regions(&mut *f)).collect(), taken: task.taken.clone(), ret: GROUP_SENDS }))
            }
            // The region it keeps, wherever its task's type names it too; the
            // task's own regions are its own.
            Type::Closure(closure) => {
                let kept = closure.region.clone();
                let Some(region) = f(&kept) else { return self.clone() };
                // A region of its own named as the new one is named otherwise
                // first, and all are named in order again after.
                let mut own = |name: &str| {
                    Some(match name {
                        _ if name == kept => region.clone(),
                        _ if name == region => format!("{}{}", name, OWN_MARK),
                        _ => name.to_string(),
                    })
                };
                let task = closure.task.map_regions(&mut own);
                Type::Closure(Box::new(ClosureType { task, region, flex: closure.flex }.normalized()))
            }
            other => other.clone(),
        }
    }

    /// The type with each shape or kind name replaced by what `f` gives for it.
    pub fn map_names(&self, f: &dyn Fn(&str) -> String) -> Type {
        let boxed = |t: &Type| Box::new(t.map_names(f));
        match self {
            Type::Named(name, region) => Type::Named(f(name), region.clone()),
            Type::Applied(applied) => Type::Applied(Box::new(Applied {
                name: f(&applied.name),
                types: applied.types.iter().map(|ty| ty.map_names(f)).collect(),
                region: applied.region.clone(),
            })),
            Type::Task(task) => Type::Task(Box::new(TaskType { params: task.params.iter().map(|ty| ty.map_names(f)).collect(), taken: task.taken.clone(), ret: task.ret.map_names(f) })),
            Type::Closure(closure) => closure.with_task(|task| task.map_names(f)),
            Type::Ptr(inner, region) => Type::Ptr(boxed(inner), region.clone()),
            Type::Array(inner, count) => Type::Array(boxed(inner), *count),
            Type::Slice(inner, region) => Type::Slice(boxed(inner), region.clone()),
            Type::Map(key, value, region) => Type::Map(boxed(key), boxed(value), region.clone()),
            Type::Range(inner) => Type::Range(boxed(inner)),
            Type::Maybe(inner) => Type::Maybe(boxed(inner)),
            Type::Or(value, error) => Type::Or(boxed(value), boxed(error)),
            Type::Fail(error) => Type::Fail(boxed(error)),
            Type::Results(types) => Type::Results(types.iter().map(|ty| ty.map_names(f)).collect()),
            Type::Tuple(items) => Type::Tuple(items.iter().map(|ty| ty.map_names(f)).collect()),
            other => other.clone(),
        }
    }

    /// The type with a region given to each pointer, slice, text, and shape or
    /// kind name that has none, by `f`, which is called with the name of a shape
    /// or kind, and with an empty name for the others (it decides nothing about
    /// whether a shape holds pointers or text).
    pub fn map_regions_of_names(&self, f: &mut dyn FnMut(&str) -> Option<String>) -> Type {
        match self {
            Type::Text(None) => Type::Text(f("")),
            Type::Ptr(inner, None) => {
                let region = f("");
                Type::Ptr(Box::new(inner.map_regions_of_names(f)), region)
            }
            Type::Named(name, None) => Type::Named(name.clone(), f(name)),
            Type::Applied(applied) => {
                // One given types a task or shape takes may hold text or
                // pointers, as `Option<T>` may.
                let region = applied
                    .region
                    .clone()
                    .or_else(|| f(&applied.name))
                    .or_else(|| applied.types.iter().any(|ty| ty.params().iter().any(|param| !param.starts_with('?'))).then(|| f("")).flatten());
                Type::Applied(Box::new(Applied {
                    name: applied.name.clone(),
                    types: applied.types.iter().map(|ty| ty.map_regions_of_names(&mut *f)).collect(),
                    region,
                }))
            }
            Type::Ptr(inner, region) => Type::Ptr(Box::new(inner.map_regions_of_names(f)), region.clone()),
            Type::Slice(inner, None) => {
                let region = f("");
                Type::Slice(Box::new(inner.map_regions_of_names(f)), region)
            }
            Type::Slice(inner, region) => Type::Slice(Box::new(inner.map_regions_of_names(f)), region.clone()),
            Type::Map(key, value, None) => {
                let region = f("");
                Type::Map(Box::new(key.map_regions_of_names(f)), Box::new(value.map_regions_of_names(f)), region)
            }
            Type::Map(key, value, region) => {
                Type::Map(Box::new(key.map_regions_of_names(f)), Box::new(value.map_regions_of_names(f)), region.clone())
            }
            Type::Array(inner, count) => Type::Array(Box::new(inner.map_regions_of_names(f)), *count),
            Type::Range(inner) => Type::Range(Box::new(inner.map_regions_of_names(f))),
            Type::Maybe(inner) => Type::Maybe(Box::new(inner.map_regions_of_names(f))),
            Type::Or(value, error) => Type::Or(Box::new(value.map_regions_of_names(f)), Box::new(error.map_regions_of_names(f))),
            Type::Fail(error) => Type::Fail(Box::new(error.map_regions_of_names(f))),
            Type::Results(types) => Type::Results(types.iter().map(|ty| ty.map_regions_of_names(&mut *f)).collect()),
            Type::Tuple(items) => Type::Tuple(items.iter().map(|ty| ty.map_regions_of_names(&mut *f)).collect()),
            other => other.clone(),
        }
    }

    /// The type with each type parameter replaced by what `f` gives for it; a
    /// parameter `f` has no type for is left as it is.
    pub fn map_params(&self, f: &mut dyn FnMut(&str) -> Option<Type>) -> Type {
        let boxed = |t: &Type, f: &mut dyn FnMut(&str) -> Option<Type>| Box::new(t.map_params(f));
        match self {
            Type::Param(name) => f(name).unwrap_or_else(|| self.clone()),
            Type::Applied(applied) => Type::Applied(Box::new(Applied {
                name: applied.name.clone(),
                types: applied.types.iter().map(|ty| ty.map_params(&mut *f)).collect(),
                region: applied.region.clone(),
            })),
            Type::Task(task) => Type::Task(Box::new(TaskType { params: task.params.iter().map(|ty| ty.map_params(&mut *f)).collect(), taken: task.taken.clone(), ret: task.ret.map_params(f) })),
            Type::Closure(closure) => closure.with_task(|task| task.map_params(f)),
            Type::Ptr(inner, region) => Type::Ptr(boxed(inner, f), region.clone()),
            Type::Array(inner, count) => Type::Array(boxed(inner, f), *count),
            Type::Slice(inner, region) => Type::Slice(boxed(inner, f), region.clone()),
            Type::Map(key, value, region) => Type::Map(boxed(key, f), boxed(value, f), region.clone()),
            Type::Range(inner) => Type::Range(boxed(inner, f)),
            Type::Maybe(inner) => Type::Maybe(boxed(inner, f)),
            Type::Or(value, error) => Type::Or(boxed(value, f), boxed(error, f)),
            Type::Fail(error) => Type::Fail(boxed(error, f)),
            Type::Results(types) => Type::Results(types.iter().map(|ty| ty.map_params(&mut *f)).collect()),
            Type::Tuple(items) => Type::Tuple(items.iter().map(|ty| ty.map_params(&mut *f)).collect()),
            other => other.clone(),
        }
    }

    /// The type with the types of each group in it put in its place, where
    /// `f` knows them: `(P...) -> R` given `int, text` for `P` is
    /// `(int, text) -> R`.
    pub fn map_groups(&self, f: &mut dyn FnMut(&str) -> Option<Vec<Type>>) -> Type {
        self.map_group_items(&mut |name| f(name).map(|types| {
            let taken = vec![Taken::Value; types.len()];
            (types, taken)
        }))
    }

    /// `map_groups`, where `f` also says how a task takes each type it gives a
    /// group: a group's values may be places, which a task type taking the
    /// group takes as places.
    pub fn map_group_items(&self, f: &mut dyn FnMut(&str) -> Option<GroupItems>) -> Type {
        let boxed = |t: &Type, f: &mut dyn FnMut(&str) -> Option<GroupItems>| Box::new(t.map_group_items(f));
        match self {
            Type::Task(task) => {
                let mut params = Vec::new();
                let mut taken = Vec::new();
                for (param, how) in task.params.iter().zip(&task.taken) {
                    match param {
                        Type::Group(name) => match f(name) {
                            Some((types, how)) => {
                                taken.extend(how.into_iter().chain(std::iter::repeat(Taken::Value)).take(types.len()));
                                params.extend(types);
                            }
                            None => {
                                params.push(param.clone());
                                taken.push(how.clone());
                            }
                        },
                        other => {
                            params.push(other.map_group_items(&mut *f));
                            taken.push(how.clone());
                        }
                    }
                }
                Type::Task(Box::new(TaskType { params, taken, ret: task.ret.map_group_items(f) }))
            }
            Type::Closure(closure) => closure.with_task(|task| task.map_group_items(f)),
            Type::Applied(applied) => Type::Applied(Box::new(Applied {
                name: applied.name.clone(),
                types: applied.types.iter().map(|ty| ty.map_group_items(&mut *f)).collect(),
                region: applied.region.clone(),
            })),
            Type::Ptr(inner, region) => Type::Ptr(boxed(inner, f), region.clone()),
            Type::Array(inner, count) => Type::Array(boxed(inner, f), *count),
            Type::Slice(inner, region) => Type::Slice(boxed(inner, f), region.clone()),
            Type::Map(key, value, region) => Type::Map(boxed(key, f), boxed(value, f), region.clone()),
            Type::Range(inner) => Type::Range(boxed(inner, f)),
            Type::Maybe(inner) => Type::Maybe(boxed(inner, f)),
            Type::Or(value, error) => Type::Or(boxed(value, f), boxed(error, f)),
            Type::Fail(error) => Type::Fail(boxed(error, f)),
            Type::Results(types) => Type::Results(types.iter().map(|ty| ty.map_group_items(&mut *f)).collect()),
            // The types given a group in a call, kept as a tuple, end with the
            // group of the task the call is in when it gives its values on
            // (`f(1, args...)`), which becomes its types.
            Type::Tuple(items) => {
                let mut spliced = Vec::new();
                for item in items {
                    match item {
                        Type::Group(name) => match f(name) {
                            Some((types, _)) => spliced.extend(types),
                            None => spliced.push(item.clone()),
                        },
                        other => spliced.push(other.map_group_items(&mut *f)),
                    }
                }
                Type::Tuple(spliced)
            }
            other => other.clone(),
        }
    }

    /// The type with each task type in it given its regions' names anew (see
    /// `TaskType::normalized`), as a type made of others, whose regions it
    /// takes, needs once they are put in: `(T) -> T` given `text@a`.
    pub fn normalized_tasks(&self) -> Type {
        let boxed = |t: &Type| Box::new(t.normalized_tasks());
        match self {
            Type::Task(task) => {
                let task = TaskType { params: task.params.iter().map(Type::normalized_tasks).collect(), taken: task.taken.clone(), ret: task.ret.normalized_tasks() };
                Type::Task(Box::new(task.normalized()))
            }
            Type::Closure(closure) => {
                let task = TaskType { params: closure.task.params.iter().map(Type::normalized_tasks).collect(), taken: closure.task.taken.clone(), ret: closure.task.ret.normalized_tasks() };
                Type::Closure(Box::new(ClosureType { task, region: closure.region.clone(), flex: closure.flex }.normalized()))
            }
            Type::Applied(applied) => Type::Applied(Box::new(Applied {
                name: applied.name.clone(),
                types: applied.types.iter().map(Type::normalized_tasks).collect(),
                region: applied.region.clone(),
            })),
            Type::Ptr(inner, region) => Type::Ptr(boxed(inner), region.clone()),
            Type::Array(inner, count) => Type::Array(boxed(inner), *count),
            Type::Slice(inner, region) => Type::Slice(boxed(inner), region.clone()),
            Type::Map(key, value, region) => Type::Map(boxed(key), boxed(value), region.clone()),
            Type::Range(inner) => Type::Range(boxed(inner)),
            Type::Maybe(inner) => Type::Maybe(boxed(inner)),
            Type::Or(value, error) => Type::Or(boxed(value), boxed(error)),
            Type::Fail(error) => Type::Fail(boxed(error)),
            Type::Results(types) => Type::Results(types.iter().map(Type::normalized_tasks).collect()),
            Type::Tuple(items) => Type::Tuple(items.iter().map(Type::normalized_tasks).collect()),
            other => other.clone(),
        }
    }

    /// Every type parameter the type names, in order, with repeats.
    pub fn params(&self) -> Vec<&str> {
        let mut names = Vec::new();
        self.collect_params(&mut names);
        names
    }

    fn collect_params<'a>(&'a self, names: &mut Vec<&'a str>) {
        match self {
            Type::Param(name) | Type::Group(name) => names.push(name),
            Type::Applied(applied) => applied.types.iter().for_each(|ty| ty.collect_params(names)),
            Type::Task(task) => {
                task.params.iter().for_each(|ty| ty.collect_params(names));
                task.ret.collect_params(names);
            }
            Type::Closure(closure) => {
                closure.task.params.iter().for_each(|ty| ty.collect_params(names));
                closure.task.ret.collect_params(names);
            }
            Type::Ptr(inner, _) | Type::Slice(inner, _) | Type::Range(inner) | Type::Maybe(inner) | Type::Fail(inner) | Type::Array(inner, _) => {
                inner.collect_params(names)
            }
            Type::Map(key, value, _) => {
                key.collect_params(names);
                value.collect_params(names);
            }
            Type::Or(value, error) => {
                value.collect_params(names);
                error.collect_params(names);
            }
            Type::Results(types) | Type::Tuple(types) => types.iter().for_each(|ty| ty.collect_params(names)),
            _ => {}
        }
    }

    /// Every region the type names, in order, with repeats.
    pub fn regions(&self) -> Vec<&str> {
        let mut regions = Vec::new();
        self.collect_regions(&mut regions);
        regions
    }

    fn collect_regions<'a>(&'a self, regions: &mut Vec<&'a str>) {
        match self {
            Type::Ptr(inner, region) | Type::Slice(inner, region) => {
                regions.extend(region.as_deref());
                inner.collect_regions(regions);
            }
            Type::Map(key, value, region) => {
                regions.extend(region.as_deref());
                key.collect_regions(regions);
                value.collect_regions(regions);
            }
            Type::Named(_, region) | Type::Text(region) => regions.extend(region.as_deref()),
            Type::Applied(applied) => {
                regions.extend(applied.region.as_deref());
                applied.types.iter().for_each(|ty| ty.collect_regions(regions));
            }
            Type::Range(inner) | Type::Maybe(inner) | Type::Fail(inner) | Type::Array(inner, _) => inner.collect_regions(regions),
            Type::Or(value, error) => {
                value.collect_regions(regions);
                error.collect_regions(regions);
            }
            Type::Results(types) | Type::Tuple(types) => types.iter().for_each(|ty| ty.collect_regions(regions)),
            // The types given a group holding a place, whose regions are the
            // caller's.
            Type::Task(task) if task.ret == GROUP_SENDS => task.params.iter().for_each(|ty| ty.collect_regions(regions)),
            // The region it keeps its copies in; its task's own are its own.
            Type::Closure(closure) => regions.push(&closure.region),
            _ => {}
        }
    }

    /// The values a task with this return type sends: none for `nothing`, each of
    /// a `Results`, or the type itself.
    pub fn results(&self) -> &[Type] {
        match self {
            Type::Nothing => &[],
            Type::Results(types) => types,
            other => std::slice::from_ref(other),
        }
    }

    /// The type a plan B or a `{value}` pattern extracts: `T` from `maybe T`
    /// or `T ~ E`.
    pub fn extracted(&self) -> Option<&Type> {
        match self {
            Type::Maybe(inner) | Type::Or(inner, _) => Some(inner),
            _ => None,
        }
    }
}

/// The name a pattern or plan B uses to drop a value it doesn't need.
pub const DISCARD: &str = "_";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    /// `**`: `a` raised to the power `b` (see `Checker::check_power`).
    Pow,
    Eq,
    Ne,
    /// `~=`: similar, item by item (see `Checker::check_similar`).
    Similar,
    /// `<>`: not similar.
    NotSimilar,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
}

/// An expression. `span` points at the token that best identifies it: the
/// operator for unary/binary/range/deref/index, the field name for field
/// access, and the first token otherwise.
#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
    /// The expression's type, filled in by the checker.
    pub ty: Option<Type>,
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Self {
        Expr { kind, span, ty: None }
    }

    /// The type assigned by the checker. An internal error if the expression
    /// was never checked.
    pub fn ty(&self) -> Result<&Type> {
        self.ty
            .as_ref()
            .ok_or_else(|| Error::internal_at(self.span, "expression has not been type-checked"))
    }
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    IntLit(i64),
    FloatLit(f64),
    /// `'a'`: a char, the byte it is.
    CharLit(u8),
    BoolLit(bool),
    TextLit(String),
    NoneLit,
    /// A macro named as a value, `P` or `math.P`, which the checker makes of the
    /// name: the macro's name in the program, and the literal it stands for.
    Macro { name: String, value: Box<Expr> },
    Var(String),
    /// `square`: a task named as a value, without `()`, which the checker makes
    /// of a `Var` naming a task and no variable. It holds the task's name in the
    /// program, and its type is the task's (see `TaskType`).
    TaskValue(String),
    /// `f(x)` where `f` is a variable of a task type, or `ops[0](x)` and
    /// `pick()(x)`: the task a value holds, called. The parser makes one of a
    /// call after any value but a name, and the checker of a `Call` naming a
    /// variable.
    Invoke { callee: Box<Expr>, args: Vec<Expr> },
    /// `[@r](x: int) -> int … fi`: a lambda that keeps what it uses from the
    /// code around it in region `region`, `flex` for `[flex @r]`, which changes
    /// its copies. `lambda` names the task it is (see `ast::lambda_name`), and
    /// the checker gives the shape of what it keeps (see `kept_name`).
    Closure { lambda: String, region: String, flex: bool, places: Vec<KeptPlace> },
    /// A plain task value given where a lambda that keeps what it uses is
    /// expected, `[@r](int) -> int`: it keeps only the task, in `region`, or
    /// for a task named here, `None`, where the program keeps it.
    Kept { value: Box<Expr>, region: Option<String> },
    /// `args...`, the last value of a call: the values of a group parameter
    /// (see `Type::Group`), given on whole, each as a value of its own.
    Spread(Box<Expr>),
    Field(Box<Expr>, String),
    /// `xs[i]`, an item of an array or slice, or `xs[a..b]`, a slice of it.
    Index(Box<Expr>, Box<Expr>),
    /// `xs[#i]` or `t.at(i)`: the item at `i` of an array, a slice, or text (a
    /// char), in a `maybe`, none when `i` is out of range. A value, never a
    /// place.
    Lookup(Box<Expr>, Box<Expr>),
    /// `[a, b, c]`: an array of the values.
    Array(Vec<Expr>),
    /// `(a, b)`: a tuple of the values, two or more.
    Tuple(Vec<Expr>),
    /// `[value#count]`: an array of `count` copies of the value, computed
    /// once; in `new [T](…) at r`, a slice of them, whose count may be known
    /// only as the program runs.
    Repeat { value: Box<Expr>, count: Box<Expr> },
    /// `first(xs)`, or `first<text>(xs)` for a task that takes types: `types`
    /// holds what the call gives it, written or worked out from the arguments
    /// and the place the value goes (empty for an ordinary task).
    Call { name: String, types: Vec<TypeArg>, args: Vec<Expr> },
    Unary(UnaryOp, Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    /// `new T(value) at r` or `new@r T(value)`; `ty_span` is the position of
    /// `T`, and `region_span` of `r`. `region` is `None` where none was written,
    /// and the checker sets it to the block's default region, or reports that
    /// there is none (as it does for `Copy`).
    New {
        /// Boxed, which keeps every expression small: the compiler's stages
        /// recurse through expressions, and each level holds some on the stack.
        ty: Box<Type>,
        ty_span: Span,
        value: Box<Expr>,
        region: Option<String>,
        region_span: Span,
    },
    /// `["a": 1, name: 2]`: the pairs of a map, given to `new [K: V](…)`, or a
    /// map of their own in the block's default region (as a `Shortcut` is,
    /// with `at r` or `@r` written after it).
    Pairs(Vec<(Expr, Expr)>),
    /// `m.has(k)`, `m.keys at r`, `m.values at r`: what a map answers or makes.
    /// `key` is the key `has` looks for; `region` is where `keys` and `values`
    /// make their slice (the block's default when none is written, and none
    /// at all where `each` goes through them without making one).
    MapOp {
        map: Box<Expr>,
        op: MapOp,
        key: Option<Box<Expr>>,
        region: Option<String>,
        region_span: Span,
    },
    /// `[1, 2, ...] at r`: a slice written by its values, in region `r` (or
    /// the block's default when none is written), the short form of
    /// `new [int]([1, 2]) at r`. The checker works out the items' type from the
    /// values and makes it the `New` it stands for, so nothing after the
    /// checker sees one.
    Shortcut {
        value: Box<Expr>,
        region: Option<String>,
        region_span: Span,
    },
    Deref(Box<Expr>),
    /// `&a`: the place `a` given to a `flex&` parameter, which the task
    /// changes. Only an argument is one (see `check::check_call_args`).
    Ref(Box<Expr>),
    /// `X.f`: the shared field `field` of the shape `shape` (its name in the
    /// program), one value for the whole program. The parser reads it as a
    /// field of a name, and the resolver makes it this once it knows the name
    /// is a shape (no variable is named like one).
    Shared { shape: String, field: String },
    /// `v.type` in a task that takes types, where the value's type names one
    /// (`T`, `[T#3]`): its text is known only once the task is made for the
    /// types a use gives, when the checker makes it the `TextLit` it is (see
    /// `Checker::check_type_of`). Nothing after the checker sees one.
    TypeOf(Box<Type>),
    /// `Point { x: 1 }`, or `Box<int> { value: none }` for a shape that takes
    /// types: `types` holds the ones the literal names, empty when the values it
    /// gives say which they are.
    StructInit { name: String, types: Vec<TypeArg>, fields: Vec<FieldInit> },
    /// A variant constructor, `Circle(1.0)` or `Empty`. The parser produces
    /// `Call` or `Var` for these; the checker rewrites them into `Construct`.
    /// One naming the types of a kind that takes them, `Option<int>.None`, is
    /// a `Construct` from the parser on, with those `types`; they are empty
    /// otherwise, when the values and the place the value goes say them. A
    /// value of a kind made for its types is of that kind, which its type
    /// names: `kind` stays the kind it was made from.
    Construct {
        kind: String,
        tag: String,
        args: Vec<Expr>,
        types: Vec<TypeArg>,
    },
    Range(Box<Expr>, Box<Expr>),
    /// `fail e`: an error value, which fits a `T ~ E`.
    Fail(Box<Expr>),
    /// A plan B, `value { … }`: the value extracted from a `maybe` or a
    /// `T ~ E`, or else what the block gives. `error` is the name of a first
    /// line `fail e`, with its position.
    PlanB {
        value: Box<Expr>,
        error: Option<(String, Span)>,
        body: Block,
    },
    /// A `match` used as a value, `fixed area = match s … fi`: the statement is
    /// the same `StmtKind::Match` a `match` on its own is, so everything that
    /// reads statements reads this too. Each case's block gives the value on its
    /// last line, as a plan B's block does, or leaves the task with `<-`; a case
    /// written `case p = value` is a block of that one value.
    Choice(Box<Stmt>),
    /// A loop that gives a value, `fixed r = loop … fi` (see `ValueLoop`).
    LoopValue(Box<ValueLoop>),
    /// Added by the checker where a value becomes part of a `maybe` or a
    /// `T ~ E`: a plain value, `none`, or `fail e`. The node's `ty` is the
    /// type it becomes.
    Wrap(Box<Expr>),
    /// `scan(prompt) at r` or `scan(prompt)@r`: shows the prompt, and reads a
    /// line of input into region `r`, giving `none` when the input has ended.
    /// `scan(prompt, int)` reads a number from that line instead, giving
    /// `int ~ InputError`, and names no region because a number needs no
    /// memory. `args` are as written (the prompt, and the type for a typed
    /// read); `region` is `None` where none was written, which the checker
    /// reports for the form that needs one, and `region_span` is its position
    /// (the position after the call when there is none).
    Scan {
        args: Vec<Expr>,
        region: Option<String>,
        region_span: Span,
    },
    /// `a + b + c at r`, or `(a + b)@r`: new text in region `r` holding the
    /// bytes of each of `parts` in turn. The parser makes one from a chain of
    /// `+` followed by a region, every operand of the chain a part; the node's
    /// span is the first `+`, and `region_span` the position of `r`.
    Join {
        parts: Vec<Expr>,
        region: String,
        region_span: Span,
    },
    /// `n.str() at r`, `n.str(r)`, or `text(n) at r` on a number, a bool, or
    /// text: the value written as text, new text in region `r`. The parser
    /// reads each as the method call `str` given the region, and the checker
    /// makes one of these from it where the value has `str` built in (a shape
    /// or kind with its own stays the call of its method); `region_span` is the
    /// position of `r`.
    ToText {
        value: Box<Expr>,
        region: String,
        region_span: Span,
    },
    /// `value.copy()`, `value.copy() at r`, or `value.copy()@r`: a copy of what
    /// the value holds in regions (its text, its slices' items, and what its
    /// pointers point to, all the way down) in region `r`. Without a region
    /// written, the checker sets `region` to the region of the place the copy
    /// goes. The node's span is the word `copy`; `region_span` is the position
    /// of `r`, or of `copy` when none is written.
    Copy {
        value: Box<Expr>,
        region: Option<String>,
        region_span: Span,
    },
    /// `c.step(…)`, or `.step(…)` inside a method: a method called on a value.
    /// `dotted` is the whole thing read as one name, when the value is a name
    /// too (`Light.Red(…)` is a variant and `m.f(…)` a module's task, which
    /// look the same until the file's names are known); the resolver settles
    /// which, and the checker rewrites a method call into the call of the task
    /// the method is, with the value as its first argument.
    MethodCall {
        value: Box<Expr>,
        name: String,
        args: Vec<Expr>,
        dotted: Option<String>,
    },
    /// The region written after a call, `f(x) at r` or `f(x)@r`, which the
    /// parser adds to the call as its last argument. The checker makes it the
    /// `Var` a region argument is, once it knows the task takes a region last,
    /// and says what is wrong where it doesn't, so nothing after the checker
    /// sees one.
    RegionAfter(String),
    /// `xs.add(v)`, `xs.add(i, v)`, `xs.del(i)`, or `xs.del()`: a change to the
    /// slice `items` names, a place that holds it. Not a method: the builtin
    /// changes every slice has, which no method may take the name of. The
    /// node's span is the word after the dot.
    Edit {
        items: Box<Expr>,
        op: EditOp,
        args: Vec<Expr>,
    },
}

/// What `ExprKind::MapOp` asks of a map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapOp {
    /// `m.has(k)`: whether the key is in the map.
    Has,
    /// `m.keys at r`: its keys, as a slice, in the order they were added.
    Keys,
    /// `m.values at r`: its values, in the same order.
    Values,
}

impl MapOp {
    /// The operation written after a dot: `has`, `keys`, or `values`.
    pub fn from_word(word: &str) -> Option<MapOp> {
        match word {
            "has" => Some(MapOp::Has),
            "keys" => Some(MapOp::Keys),
            "values" => Some(MapOp::Values),
            _ => None,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            MapOp::Has => "has",
            MapOp::Keys => "keys",
            MapOp::Values => "values",
        }
    }
}

/// A change `ExprKind::Edit` makes to a slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditOp {
    /// `add(v)` puts a value at the end, `add(i, v)` before item `i`.
    Add,
    /// `del(i)` takes item `i` out, `del()` the last item; either gives it.
    Del,
}

impl EditOp {
    /// The change written after a dot: `add` or `del`.
    pub fn from_word(word: &str) -> Option<EditOp> {
        match word {
            "add" => Some(EditOp::Add),
            "del" => Some(EditOp::Del),
            _ => None,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            EditOp::Add => "add",
            EditOp::Del => "del",
        }
    }
}

/// One `name: value` in a shape literal.
#[derive(Debug, Clone)]
pub struct FieldInit {
    pub name: String,
    /// Position of the field name.
    pub span: Span,
    pub value: Expr,
    /// Whether the checker filled it in with the value the field falls back
    /// on, which is read as the shape's module wrote it (see `check_struct_init`).
    pub filled: bool,
}

pub type Block = Vec<Stmt>;

/// A statement. `span` points at the declared name for `fixed`/`flex` and
/// `each`, and at the first token otherwise.
#[derive(Debug, Clone)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

impl Stmt {
    pub fn new(kind: StmtKind, span: Span) -> Self {
        Stmt { kind, span }
    }
}

#[derive(Debug, Clone)]
pub enum StmtKind {
    Expr(Expr),
    /// `fixed`, `bind`, or `flex`, with one name per value:
    /// `fixed q, r = divmod(17, 5)`.
    Binding {
        names: Vec<Bound>,
        mode: Mode,
        /// `flex& y = &x` or `fixed& y = &x`: one name, which is another name for
        /// the place its value marks, changing it (`flex&`) or only reading it.
        reference: bool,
        /// The values: one per name, or a single call that sends them all.
        init: Vec<Expr>,
    },
    /// `a, b = b, a`: the places assigned, and the values, as in a binding.
    Assign(Vec<Target>, Vec<Expr>),
    /// `x++`, `x--`, or `x += v` (and `-=`, `*=`, `/=`, `%=`, `**=`): the place
    /// given its operator's result on itself and the value, which is one for
    /// `++` and `--`. `symbol` is the operator as written, and `at` where. The
    /// checker makes it the assignment it is (see `Checker::check_step`).
    Step { target: Expr, op: BinaryOp, value: Expr, symbol: &'static str, at: Span },
    /// `when c … else when d … else … fi`: the arms in order, each a condition
    /// and the block it guards, then the `else` block. A chain of `else when`s
    /// is one statement, so every pass goes through its arms in a loop, however
    /// many there are, and a `when` written alone inside an `else` is nested.
    When(Vec<WhenArm>, Option<Block>),
    Loop(Condition, Block),
    /// `loop start || condition || step`, and `endless` (see `PartedLoop`).
    Parted(Box<PartedLoop>),
    /// `block … fi`: statements in a block of their own, whose names, regions,
    /// and the files opened in them end at its `fi`.
    Block(Block),
    /// `each v in xs`, and `each k, v in m` for a map: the second name, with
    /// where it is written, is the value that goes with the key.
    Each(String, Option<(String, Span)>, Expr, Block),
    /// `outer => loop …` or `outer => each …`: a loop, and the name a `break`
    /// or a `continue` inside it may give to stop it or go on with it. The
    /// statement's span is where the name is written.
    Named(String, Box<Stmt>),
    /// `break`, or `break outer`: stops the loop around it, or the one named,
    /// with where the name is written. In a loop that gives a value,
    /// `break = v` gives `v`, which the loop fails with as `LoopResult.Broke`
    /// (see `ValueLoop`).
    Break(Option<(String, Span)>, Option<Box<Expr>>),
    /// `continue`, or `continue outer`: ends this pass of the loop around it,
    /// or of the one named, which goes on with its next.
    Continue(Option<(String, Span)>),
    /// In a copy of a task, `each v in args` written out for the values the
    /// copy is given: one block per value, each giving `v` first. It is still a
    /// loop, which a `break` stops and a `continue` takes to the next block (see
    /// `check::Checker::each_of_group`).
    Turns(Vec<Block>),
    Match(Expr, Vec<Case>),
    /// `<-`, `<- value`, or `<- a, b`: one value per result.
    Send(Vec<Expr>),
    /// `total <- value`: sets named results, and keeps going.
    SetResults(Vec<Binding>, Vec<Expr>),
    /// `region r`: memory that `new` allocates in `r` until the block closes.
    /// `region r default` also makes it the block's default region, which every
    /// allocation in the block that names no region goes into (see
    /// `check::Checker::default_region`). A block declares at most one.
    Region { name: String, default: bool },
}

/// A name a `fixed` or `flex` declares, `q` or `q: int`. `ty` is the
/// annotation, if any, at `ty_span`; the checker fills it in with the
/// inferred type otherwise. The name may be `_` (`DISCARD`), which declares
/// nothing and drops its value.
#[derive(Debug, Clone)]
pub struct Bound {
    pub name: String,
    /// Position of the name.
    pub span: Span,
    pub ty: Option<Type>,
    pub ty_span: Option<Span>,
    /// For `(a, b)`, which unpacks a tuple: its names, and an empty `name`;
    /// `span` is the position of `(` and `ty` the tuple's type once checked.
    /// Empty for a name.
    pub parts: Vec<Bound>,
}

impl Bound {
    /// Every name this binds, in order, through the parts of the tuples it unpacks.
    pub fn leaves(&self) -> Vec<&Bound> {
        if self.parts.is_empty() {
            return vec![self];
        }
        self.parts.iter().flat_map(Bound::leaves).collect()
    }
}

/// One place an assignment writes to.
#[derive(Debug, Clone)]
pub enum Target {
    /// `_`: takes a value and drops it.
    Discard(Span),
    /// A variable, a field, or what a pointer points to.
    Place(Expr),
    /// `(a, b)`: the places a tuple's items go, at the position of `(`.
    Tuple(Vec<Target>, Span),
}

impl Target {
    pub fn span(&self) -> Span {
        match self {
            Target::Discard(span) | Target::Tuple(_, span) => *span,
            Target::Place(expr) => expr.span,
        }
    }
}

/// `loop i = 0, flex j = 9 || i < j || i++, j--`: a loop in three parts, each
/// of which may be empty. The start runs once, before the loop, and the names
/// it declares, after `flex` or `fixed`, live in the loop; the condition, none
/// for a loop that never ends by itself, is tested before each pass; the step
/// runs after each pass, a `continue` too, and never after a `break`.
/// `endless` is all three left empty, said in a word.
#[derive(Debug, Clone)]
pub struct PartedLoop {
    pub start: Vec<Stmt>,
    pub cond: Option<Condition>,
    pub step: Vec<Stmt>,
    pub body: Block,
    /// Written `endless`.
    pub endless: bool,
}

/// A `loop` or `each` used as a value: each pass gives the value on its last
/// line, and the loop gives the one of its final pass, or fails with
/// `LoopResult` (`std/loops`): the body never ran (`Skipped`), a `break`
/// stopped it (`Broke`), its final pass ended with `continue`
/// (`EndedByContinue`), or the last line failed (`Manual`). As a statement
/// with a plan B, `loop … fi { … }`, it gives nothing, and only fails.
#[derive(Debug, Clone)]
pub struct ValueLoop {
    /// The loop: a `StmtKind::Loop`, `Each` or `Parted`.
    pub stmt: Stmt,
    /// Its name: the variable it gives (`fixed r = loop …`), or the one written
    /// with `outer =>` where there is none, with where it is written.
    pub name: Option<(String, Span)>,
    /// Whether written with `outer =>`, which a loop given to a variable
    /// can't be.
    pub arrow: bool,
    /// Whether its passes give a value; not for a statement with a plan B.
    pub gives: bool,
    /// Whether nothing in it tells `B` (no `break` gives a value) and `M` (no
    /// last line fails): each is `bool` until the place the value goes names
    /// it, `int ~ LoopResult<int, text>` (see `Checker::settle_variant`).
    pub open: (bool, bool),
}

impl ValueLoop {
    /// The loop's body.
    pub fn body(&self) -> Option<&Block> {
        match &self.stmt.kind {
            StmtKind::Loop(_, body) | StmtKind::Each(_, _, _, body) => Some(body),
            StmtKind::Parted(parted) => Some(&parted.body),
            _ => None,
        }
    }

    pub fn body_mut(&mut self) -> Option<&mut Block> {
        match &mut self.stmt.kind {
            StmtKind::Loop(_, body) | StmtKind::Each(_, _, _, body) => Some(body),
            StmtKind::Parted(parted) => Some(&mut parted.body),
            _ => None,
        }
    }
}

/// The condition of a `when` or `loop`: a `bool`, or `value is pattern`,
/// and what follows its `and` where an `is` is joined with others
/// (`m is {x} and x > 0`): the names a pattern binds are seen by the parts
/// after it and by the block. Without an `is`, `a and b` is one `bool`.
#[derive(Debug, Clone)]
pub struct Condition {
    pub value: Expr,
    pub pattern: Option<Pattern>,
    pub and: Option<Box<Condition>>,
}

impl Condition {
    /// A condition of one part.
    pub fn new(value: Expr, pattern: Option<Pattern>) -> Condition {
        Condition { value, pattern, and: None }
    }

    /// This part and the ones joined after it with `and`, in order.
    pub fn parts(&self) -> impl Iterator<Item = &Condition> {
        std::iter::successors(Some(self), |part| part.and.as_deref())
    }

    /// The part `n` places after this one, to change.
    pub fn part_mut(&mut self, n: usize) -> Option<&mut Condition> {
        let mut part = self;
        for _ in 0..n {
            part = part.and.as_deref_mut()?;
        }
        Some(part)
    }

    /// Whether this is one part: a `bool`, or one `is`.
    pub fn single(&self) -> bool {
        self.and.is_none()
    }
}

/// One arm of a `when`: `when c` or `else when c`, and the block it guards.
#[derive(Debug, Clone)]
pub struct WhenArm {
    pub cond: Condition,
    pub body: Block,
    /// Position of its `when`.
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Case {
    pub pattern: Pattern,
    pub body: Block,
}

/// What a `case` or an `is` condition matches.
#[derive(Debug, Clone)]
pub struct Pattern {
    pub kind: PatternKind,
    /// Position of the pattern's first token.
    pub span: Span,
}

/// A name with its position: one a pattern binds, where `_` (`DISCARD`) binds
/// nothing, or a result a `<-` sets.
pub type Binding = (String, Span);

#[derive(Debug, Clone)]
pub enum PatternKind {
    /// `Circle r` or `Shape.Circle r`: a variant, naming its payload fields in
    /// order. `kind` is the kind written before the variant, if any: the
    /// value's kind, which the checker makes sure of. (Boxed: patterns are held
    /// by value in frames that nest, and are rarely written with a kind.)
    Variant { kind: Option<Box<Binding>>, tag: String, binds: Vec<Names> },
    /// `{v}`: the value in a `maybe`, or the success of a `T ~ E`.
    Value(Names),
    /// `none`: an empty `maybe`.
    None,
    /// `fail e`: the error of a `T ~ E`.
    Fail(Names),
    /// `(x, y)`: the items of a tuple, which always match.
    Tuple(Names),
    /// `_`: anything the earlier cases didn't match.
    Any,
    /// `maybe int {x}`, `maybe int x`, or after `t.#k?` the type alone: a test
    /// of the type of a tuple's item read with `.#`, which the compiler knows
    /// (see `Checker::settle_type_tests`). `asks` is the `?` written.
    Typed { ty: Type, form: TypedForm, asks: bool },
}

/// What a type test on a tuple's item names (see `PatternKind::Typed`).
#[derive(Debug, Clone)]
pub enum TypedForm {
    /// `t.#k? is maybe int`: nothing; true for an `int` item or none.
    Only,
    /// `t.#k is maybe int x`: the read itself, a `maybe int`; true for an `int`
    /// item or none.
    Named(Names),
    /// `t.#k is maybe int {x}`: the item; true only for an `int` item.
    Value(Names),
}

/// What a pattern names: one name (or `_`), or `(a, b)`, the names of a
/// tuple's items. `mode` is `flex` written before it, which makes every
/// name it binds a variable that can be assigned.
#[derive(Debug, Clone)]
pub struct Names {
    /// Empty for `(a, b)`, which names its parts.
    pub name: String,
    /// Position of the name, or of `(`.
    pub span: Span,
    pub mode: Mode,
    /// The names of a tuple's items; empty for one name.
    pub parts: Vec<Names>,
}

impl Names {
    /// One name, at `span`, which `flex` may make changeable.
    pub fn one(name: String, span: Span, mode: Mode) -> Names {
        Names { name, span, mode, parts: Vec::new() }
    }

    /// Every name it binds, in order, through the tuples it takes apart.
    pub fn leaves(&self) -> Vec<&Names> {
        if self.parts.is_empty() {
            return vec![self];
        }
        self.parts.iter().flat_map(Names::leaves).collect()
    }
}

impl Pattern {
    /// The pattern without its names, for messages: `Circle`, `{…}`, `none`,
    /// `fail …`, or `_`.
    pub fn label(&self) -> String {
        match &self.kind {
            PatternKind::Variant { tag, .. } => tag.clone(),
            PatternKind::Value(_) => "{…}".to_string(),
            PatternKind::None => "none".to_string(),
            PatternKind::Fail(_) => "failure …".to_string(),
            PatternKind::Tuple(_) => "( … )".to_string(),
            PatternKind::Any => DISCARD.to_string(),
            PatternKind::Typed { ty, .. } => ty.to_string(),
        }
    }

    /// The names the pattern binds, in order, including discards.
    pub fn bindings(&self) -> Vec<&Names> {
        match &self.kind {
            PatternKind::Variant { binds, .. } => binds.iter().flat_map(Names::leaves).collect(),
            PatternKind::Value(names) | PatternKind::Fail(names) | PatternKind::Tuple(names) => names.leaves(),
            PatternKind::Typed { form: TypedForm::Named(names) | TypedForm::Value(names), .. } => names.leaves(),
            PatternKind::None | PatternKind::Any | PatternKind::Typed { .. } => Vec::new(),
        }
    }
}

impl fmt::Display for Pattern {
    /// The pattern as it is written, with its names, as in `{value}`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            PatternKind::Variant { tag, binds, .. } => {
                write!(f, "{}", tag)?;
                for names in binds {
                    write!(f, " {}", names)?;
                }
                Ok(())
            }
            PatternKind::Value(names) => write!(f, "{{{}}}", names),
            PatternKind::None => write!(f, "none"),
            PatternKind::Fail(names) => write!(f, "fail {}", names),
            PatternKind::Tuple(names) => write!(f, "{}", names),
            PatternKind::Any => write!(f, "_"),
            PatternKind::Typed { ty, form: TypedForm::Only, .. } => write!(f, "{}", ty),
            PatternKind::Typed { ty, form: TypedForm::Named(names), .. } => write!(f, "{} {}", ty, names),
            PatternKind::Typed { ty, form: TypedForm::Value(names), .. } => write!(f, "{} {{{}}}", ty, names),
        }
    }
}

impl fmt::Display for Names {
    /// The names as they are written: `value`, `flex value`, `bind value`, or `(a, b)`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.mode != Mode::Fixed {
            write!(f, "{} ", self.mode.word())?;
        }
        if self.parts.is_empty() {
            return write!(f, "{}", self.name);
        }
        write!(f, "(")?;
        for (i, part) in self.parts.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{}", part)?;
        }
        write!(f, ")")
    }
}

/// A named, typed slot: a task parameter, shape field, or variant payload field.
#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub ty: Type,
    /// Position of the name.
    pub span: Span,
    /// Position of the type.
    pub ty_span: Span,
    /// `priv`: a shape's field that only its own file reads, writes, or sets
    /// in a literal. Parameters and payloads are never private.
    pub private: bool,
    /// `read`: a shape's field any file may read and only its own file may
    /// assign (see `ast::READ`). A literal gives it exactly when it falls back
    /// on no value, so what the shape keeps to itself stays its own business.
    /// Never `priv` as well, and never a parameter or a payload.
    pub read: bool,
    /// `r: region default`: a region the task takes that is its body's default
    /// region (see `ast::DEFAULT`). Only a parameter of type `region` can be
    /// one, and a task takes at most one; a field or a payload never is.
    pub default_region: bool,
    /// How a parameter takes its value: `x: T` never changes, `flex x: T` is
    /// the task's own copy, which it may change, and `flex& x: T` is the
    /// caller's place. A field or a payload is always `Passing::Copy`.
    pub passing: Passing,
    /// `size: int = 0`: the value a shape's field takes where a literal leaves
    /// it out, which is what lets a literal name some fields and not others, or
    /// a task's parameter where a call leaves it out, one of the last (see
    /// `Checker::fill_defaults`). It is a value the compiler works out without
    /// running the program (see `check::constant_value`), and the checker writes
    /// it into every literal or call that leaves it out, so nothing later in the
    /// compiler sees a literal missing a field or a call missing a value. A
    /// variant's value never has one.
    pub default: Option<Expr>,
}

/// How a task's parameter takes the value it is given (see `Field::passing`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Passing {
    /// `x: T`: a value that never changes in the task.
    #[default]
    Copy,
    /// `flex x: T`: the task's own copy, which it may change; the caller's
    /// variable never does, so the call has no mark, though what a twofold
    /// value names (a slice's items, a map, what a pointer points to) is the
    /// caller's.
    Flex,
    /// `flex& x: T`: the caller's place itself, which the call marks,
    /// `f(&a)`; every change to `x` is a change to it.
    FlexRef,
    /// `fixed x: T`: always a copy, even of a large value the compiler would
    /// otherwise read where the caller keeps it (see `Copy`).
    Fixed,
    /// `fixed& x: T`: the caller's place, read and never changed, which the call
    /// marks, `f(&a)`.
    FixedRef,
    /// `bind x: T`: the task's own copy, which it may change the contents of,
    /// but never assign as a whole.
    Bind,
    /// `bind& x: T`: the caller's place, which the call marks, `f(&a)`: it may
    /// change the contents there, but never assigns the place as a whole.
    BindRef,
}

impl Passing {
    /// Whether the parameter is the caller's place, given as `&a`.
    pub fn is_ref(self) -> bool {
        matches!(self, Passing::FlexRef | Passing::FixedRef | Passing::BindRef)
    }

    /// Whether the task may change the parameter.
    pub fn changes(self) -> bool {
        matches!(self, Passing::Flex | Passing::FlexRef | Passing::Bind | Passing::BindRef)
    }

    /// The words written before the parameter's name, for messages.
    pub fn word(self) -> &'static str {
        match self {
            Passing::Copy => "",
            Passing::Flex => "flex",
            Passing::FlexRef => "flex&",
            Passing::Fixed => "fixed",
            Passing::FixedRef => "fixed&",
            Passing::Bind => "bind",
            Passing::BindRef => "bind&",
        }
    }
}

/// Whether a variable is `fixed`, `bind`, or `flex`. A `fixed` variable never
/// changes. A `bind` variable is bound to one value: it cannot be assigned as a
/// whole, but its contents may change. A `flex` variable may be assigned and may
/// change its contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Fixed,
    Bind,
    Flex,
}

impl Mode {
    /// Whether the variable may be assigned as a whole (`v = …`).
    pub fn can_assign(self) -> bool {
        self == Mode::Flex
    }

    /// Whether the variable's contents may change: fields, items, or what a
    /// pointer it holds points to. For `bind`, this is true only when the type
    /// actually has contents that can change; a `bind` of a simple type acts as
    /// `fixed`.
    pub fn can_change_contents(self, has_contents: bool) -> bool {
        match self {
            Mode::Flex => true,
            Mode::Fixed => false,
            Mode::Bind => has_contents,
        }
    }

    /// Whether the variable never changes in any way.
    pub fn is_fixed(self) -> bool {
        self == Mode::Fixed
    }

    /// The keyword written before the name.
    pub fn word(self) -> &'static str {
        match self {
            Mode::Fixed => "fixed",
            Mode::Bind => "bind",
            Mode::Flex => "flex",
        }
    }
}

/// `include shapes/circle`, `include shapes/circle => round`,
/// `include geometry (area, Point => Spot)`, or `include geometry {area}`:
/// another file of the program, whose tasks, shapes, and kinds this file names
/// through the module's name (`circle.area`), and the parts it also brings in
/// by name.
#[derive(Debug, Clone)]
pub struct Include {
    /// The path as written: names and `..`, the last one the module's name.
    pub path: Vec<String>,
    /// Position of the path's first part.
    pub span: Span,
    /// Position of the module's name, the path's last part.
    pub name_span: Span,
    /// The name after `=>`, which this file names the module by instead.
    pub alias: Option<Binding>,
    /// The parts in parentheses or braces.
    pub parts: Vec<Part>,
    /// Written with braces: only the parts are included, and the module
    /// itself has no name in this file.
    pub only_parts: bool,
}

/// A task, shape, kind, or variant an `include` brings in by name, maybe
/// under another name: `Point` or `Point => Spot`.
#[derive(Debug, Clone)]
pub struct Part {
    pub name: String,
    pub span: Span,
    /// The name after `=>`, which this file names the part by instead.
    pub alias: Option<Binding>,
}

impl Part {
    /// The name this file writes for the part: its alias, or else its own.
    pub fn local_name(&self) -> &str {
        self.alias.as_ref().map_or(self.name.as_str(), |(alias, _)| alias.as_str())
    }

    /// Where the name this file writes for the part is.
    pub fn local_span(&self) -> Span {
        self.alias.as_ref().map_or(self.span, |(_, at)| *at)
    }
}

impl Include {
    /// The name the path gives the module: its last part.
    pub fn file_name(&self) -> &str {
        self.path.last().map_or("", String::as_str)
    }

    /// The name this file names the module by: its alias, or the path's last
    /// part; none when only its parts are included.
    pub fn local_name(&self) -> Option<&str> {
        if self.only_parts {
            return None;
        }
        Some(self.alias.as_ref().map_or(self.file_name(), |(alias, _)| alias.as_str()))
    }

    /// Where the name this file names the module by is written.
    pub fn local_span(&self) -> Span {
        self.alias.as_ref().map_or(self.name_span, |(_, at)| *at)
    }

    /// The path as written, as in `../shapes/circle`.
    pub fn written(&self) -> String {
        self.path.join("/")
    }
}

/// The separator between a module's name and a name inside it, both as
/// written (`geometry.area`) and in the names the checker and code generator
/// use for the items of modules other than the program's main file.
pub const MODULE_SEPARATOR: char = '.';

/// The module a name belongs to: `geometry` for `geometry.Point`, and the
/// empty string for a name of the program's main file.
pub fn module_of(name: &str) -> &str {
    name.rsplit_once(MODULE_SEPARATOR).map_or("", |(module, _)| module)
}

/// A name without its module: `Point` for `geometry.Point`.
pub fn unqualified(name: &str) -> &str {
    name.rsplit_once(MODULE_SEPARATOR).map_or(name, |(_, name)| name)
}

#[derive(Debug, Clone)]
pub enum Item {
    Task(TaskDef),
    Shape(ShapeDef),
    Kind(KindDef),
    Macro(MacroDef),
}

impl Item {
    /// The name the declaration is known by in the program, with its module.
    pub fn name(&self) -> &str {
        match self {
            Item::Task(t) => &t.name,
            Item::Shape(s) => &s.name,
            Item::Kind(k) => &k.name,
            Item::Macro(m) => &m.name,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TaskDef {
    pub name: String,
    /// `task first<T: ordered>(…)`: the types the task takes, empty for an
    /// ordinary task. Its body is checked once with these in place of types,
    /// and once more for each set of types a call gives it (see the checker's
    /// instances).
    pub types: Vec<TypeParam>,
    /// The types this task was made for, when it is one use of a generic task
    /// (see the checker's `instances`); empty otherwise. Such a task's `types`
    /// is empty: every `T` in it has been replaced.
    pub given: Vec<Type>,
    pub params: Vec<Field>,
    /// What the task sends: `nothing` for no value, one type, or `Type::Results`
    /// for several.
    pub ret: Type,
    /// One entry per result, in order; empty when the task sends nothing.
    pub results: Vec<ResultDef>,
    pub body: Block,
    /// Position of the task name.
    pub span: Span,
    /// `priv task`: only its own file can call it.
    pub private: bool,
    /// `native task`: its body is a function of the runtime, and `body` is empty
    /// (see `NATIVE`).
    pub native: bool,
    /// What makes this task a method of a shape or a kind, and not a task the
    /// program can name; `None` for a task.
    pub owner: Option<Owner>,
    /// `$d` and `$d(args)` before the task: its startup decorators, each the
    /// call `d(task, args)` that runs once where the program starts, in the
    /// order written (the nearest the task runs first; see `STARTUP`).
    pub decorators: Vec<Expr>,
    /// For each of `decorators`, whether it is written `$d?`, whose failure
    /// doesn't stop the program; for the task they are gathered into (see
    /// `STARTUP`), the same for each statement of its body.
    pub passes: Vec<bool>,
    /// `$(w1, w2(a))` before the task: its wrapper decorators, leftmost
    /// outermost, each the call `w(values)` as written, without the task that
    /// comes next or the task's values (see `Checker::add_wrappers`).
    pub wrappers: Vec<Expr>,
    /// For a lambda that keeps what it uses, `[@r](…)` or `[flex @r](…)`: the
    /// region it keeps its copies in, and whether it changes them.
    pub kept: Option<Kept>,
}

/// A variable a lambda keeps as a place, named in its list beside the region:
/// `flex& count`, which it changes, or `fixed& big`, which it reads where it is.
#[derive(Debug, Clone, PartialEq)]
pub struct KeptPlace {
    pub name: String,
    /// `Passing::FlexRef` or `Passing::FixedRef`.
    pub passing: Passing,
    pub span: Span,
}

/// What a lambda written `[@r](…)` keeps (see `TaskDef::kept`).
#[derive(Debug, Clone, PartialEq)]
pub struct Kept {
    pub region: String,
    /// `[flex @r]`: it changes its copies.
    pub flex: bool,
    /// `[flex& count, @r]`: the variables it keeps as places.
    pub places: Vec<KeptPlace>,
}

/// What makes a task a method: it is written inside a shape or a kind, and its
/// first parameter is the value it is called on (named `THIS`, written `.this`),
/// unless it is `shared`.
#[derive(Debug, Clone)]
pub struct Owner {
    /// The method's own name, which a call writes after a dot (`step`).
    pub name: String,
    /// `alters`: the method may change the value it is called on, so the call
    /// must name a place that can change and the value is given by pointer.
    pub alters: bool,
    /// Position of `alters`, when it is written.
    pub alters_span: Option<Span>,
    /// `shared new(…)`: a method of the shape or kind itself, called on its
    /// name (`X.new(…)`), with no value it is called on, so no `.this` and no
    /// first parameter holding one.
    pub shared: bool,
}

impl TaskDef {
    /// The shape or kind whose method this is: the type of its first parameter,
    /// which holds the value the method is called on. `None` for a task.
    pub fn owner_type(&self) -> Option<&str> {
        // A shared method has no value it is called on; its name says whose it is.
        if self.owner.as_ref().is_some_and(|owner| owner.shared) {
            return method_parts(&self.name).map(|(owner, _)| owner);
        }
        self.owner.as_ref().and_then(|_| self.params.first()).and_then(|param| match &param.ty {
            Type::Named(name, _) => Some(name.as_str()),
            Type::Applied(applied) => Some(applied.name.as_str()),
            _ => None,
        })
    }
}

/// The name of the task made for one set of types: `first<int, text>` becomes
/// `first__of__int__and__text`. No Vela name contains `__`, so an instance's
/// name can't be any other declaration's, and a module's path stays in front
/// of it (`shapes/geometry.first__of__int`). Regions are left out, since a
/// value's C type never depends on them, so one instance serves every region.
pub fn instance_name(name: &str, given: &[Type]) -> String {
    if given.is_empty() {
        return name.to_string();
    }
    let words: Vec<String> = given.iter().map(type_word).collect();
    format!("{}__of__{}", name, words.join("__and__"))
}

/// A type as one word of a name, distinct for distinct types: words separated
/// by `__`, with a shape or kind name marked `n_` so it can't read as a builtin
/// or as `ptr`, `maybe`, or `or`, and a count before the types of a tuple or a
/// list of results so one can't read as the start of another.
pub fn type_word(ty: &Type) -> String {
    match ty {
        Type::Int => "int".to_string(),
        Type::Float => "float".to_string(),
        Type::Bool => "bool".to_string(),
        Type::Text(_) => "text".to_string(),
        Type::Unit => "unit".to_string(),
        Type::Char => "char".to_string(),
        Type::Nothing => "nothing".to_string(),
        Type::None => "none".to_string(),
        Type::Region => "region".to_string(),
        Type::Named(name, _) => format!("n_{}", flat_name(name)),
        Type::Param(name) => format!("param__{}", name),
        Type::Group(name) => format!("group__{}", name),
        Type::Applied(applied) => format!("n_{}", flat_name(&instance_name(&applied.name, &applied.types))),
        Type::Ptr(inner, _) => format!("ptr__{}", type_word(inner)),
        Type::Range(inner) => format!("range__{}", type_word(inner)),
        Type::Maybe(inner) => format!("maybe__{}", type_word(inner)),
        Type::Or(value, error) => format!("or__{}__{}", type_word(value), type_word(error)),
        // Only a program with an error holds a type the checker couldn't work
        // out, and such a program is never generated (see `Type::Unknown`).
        Type::Unknown => "unknown".to_string(),
        Type::Fail(error) => format!("fail__{}", type_word(error)),
        Type::Array(items, count) => format!("array__{}__{}", count, type_word(items)),
        Type::Slice(items, _) => format!("slice__{}", type_word(items)),
        Type::Map(key, value, _) => format!("map__{}__{}", type_word(key), type_word(value)),
        Type::Results(types) => format!("results__{}__{}", types.len(), types.iter().map(type_word).collect::<Vec<_>>().join("__")),
        Type::Tuple(items) => format!("tuple__{}__{}", items.len(), items.iter().map(type_word).collect::<Vec<_>>().join("__")),
        Type::Task(task) => {
            // A place is given as a pointer, which C tells apart from a value.
            let params: Vec<String> = task
                .params
                .iter()
                .zip(&task.taken)
                .map(|(ty, how)| match how {
                    Taken::Place(Passing::FlexRef) => format!("flexref__{}", type_word(ty)),
                    Taken::Place(Passing::BindRef) => format!("bindref__{}", type_word(ty)),
                    Taken::Place(_) => format!("fixedref__{}", type_word(ty)),
                    _ => type_word(ty),
                })
                .collect();
            format!("task__{}__{}__{}", task.params.len(), params.join("__"), type_word(&task.ret))
        }
        // `[flex @r]` and `[@r]` are one C type: a call and what it keeps.
        Type::Closure(closure) => format!("closure__{}", type_word(&Type::Task(Box::new(closure.task.clone())))),
    }
}

/// A declaration's name as one word: the name itself, or, for a name from
/// another module, the module's path before a `.` with the `.` written
/// `__0__`, each `/` of the path `__1__`, and each `..` in it `__2__`
/// (`shapes/geometry.area` is `shapes__1__geometry__0__area`).
pub fn flat_name(name: &str) -> String {
    let Some((module, own)) = name.rsplit_once(MODULE_SEPARATOR) else { return name.to_string() };
    let path: Vec<&str> = module.split('/').map(|part| if part == ".." { "__2__" } else { part }).collect();
    format!("{}__0__{}", path.join("__1__"), own)
}

/// A type given to a generic task at a call, and where it is written (the call
/// itself, for one worked out rather than written).
pub type TypeArg = (Type, Span);

/// One type a generic declaration takes, `T` or `T: ordered`.
#[derive(Debug, Clone)]
pub struct TypeParam {
    pub name: String,
    /// Position of the name.
    pub span: Span,
    /// What every type given for it must allow, each word with where it is
    /// written: `<T: same & shown>` asks for both. None lets any type fit, and
    /// its values can only be stored, passed, and sent.
    pub needs: Vec<(Need, Span)>,
    /// `P...`: a group of types, any number and any mix, which a task takes
    /// as its last parameter (`args: P...`) and names in a task's type
    /// (`(P...) -> R`). Each use fills it in with the types of the values it
    /// gives, as a type is filled in (see `Type::Group`).
    pub group: bool,
}

/// What a generic declaration requires of a type given to it, written after the
/// type's name (`<T: ordered>`). Each word names one thing the body may do with
/// a value of that type, and `&` asks for several (`<T: same & shown>`); the
/// only word that means another is `ordered`, since what has an order can be
/// compared. Each must be written, so a declaration says all it relies on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// `< > <= >=`, and so `==` too: a number or text.
    Ordered,
    /// `show`, `str`, and text holding values: anything with a text form.
    Shown,
    /// `==` and `!=`: what those compare, each type by its own rule (see
    /// `Checker::check_same`).
    Same,
    /// `~=` and `<>`: values compared item by item.
    Similar,
    /// A value that never changes, so it can be a tuple's item.
    Frozen,
}

impl Need {
    /// The words, in the order messages list them.
    pub const ALL: [Need; 5] = [Need::Ordered, Need::Shown, Need::Same, Need::Similar, Need::Frozen];

    pub fn word(self) -> &'static str {
        match self {
            Need::Ordered => "ordered",
            Need::Shown => "shown",
            Need::Same => "same",
            Need::Similar => "similar",
            Need::Frozen => "frozen",
        }
    }

    pub fn from_word(word: &str) -> Option<Need> {
        Need::ALL.into_iter().find(|need| need.word() == word)
    }

    /// Whether this word already means `other`, so writing both says nothing
    /// new: each word means itself, and `ordered` means `same`, since what has
    /// an order can be compared. No other word means another.
    pub fn means(self, other: Need) -> bool {
        self == other || (self == Need::Ordered && other == Need::Same)
    }
}

/// One result in a task's header, `int` or `total: int`. Either every result
/// of a task has a name or none does.
#[derive(Debug, Clone)]
pub struct ResultDef {
    /// The name the task sets with `total <- value`, and its position.
    pub name: Option<Binding>,
    /// Position of the result's type.
    pub ty_span: Span,
}

impl TaskDef {
    /// Position of the return type, or of the task name when there is none.
    pub fn ret_span(&self) -> Span {
        self.results.first().map_or(self.span, |result| result.ty_span)
    }

    /// Position of result `i`, or of the return type when it isn't known.
    pub fn result_span(&self, i: usize) -> Span {
        self.results.get(i).map_or_else(|| self.ret_span(), |result| result.ty_span)
    }

    /// The names of the results, in order, when they have them.
    pub fn result_names(&self) -> impl Iterator<Item = &Binding> {
        self.results.iter().filter_map(|result| result.name.as_ref())
    }

    /// Whether the results are named, so that setting them with `<-` is what
    /// the task sends, and reaching its end sends them.
    pub fn names_results(&self) -> bool {
        self.results.iter().any(|result| result.name.is_some())
    }
}

#[derive(Debug, Clone)]
pub struct ShapeDef {
    pub name: String,
    /// `shape Box<T: frozen>`: the types the shape takes, empty for an ordinary
    /// shape. Its fields are checked once with these in place of types, and one
    /// shape is made for each set of types it is given (see the checker's
    /// `shape_instance`).
    pub types: Vec<TypeParam>,
    /// The types this shape was made for, when it is one use of a shape that
    /// takes types; empty otherwise. Such a shape's `types` is empty: every `T`
    /// in it has been replaced.
    pub given: Vec<Type>,
    pub fields: Vec<Field>,
    /// `shared f: int = 0`: fields that are one value for the whole program,
    /// never part of a value of the shape, in the order they are written.
    pub shared: Vec<SharedField>,
    /// Position of the shape name.
    pub span: Span,
    /// `priv shape`: only its own file can name it.
    pub private: bool,
}

/// A shape's `shared` field: one value for the whole program, reached only
/// through the shape's name, `X.f`, never through a value. Its `field` always
/// has a first value, its `default`, one the compiler works out without running
/// the program, and may be `priv` or `read` as any field.
#[derive(Debug, Clone)]
pub struct SharedField {
    pub field: Field,
    /// `shared flex f`: any code that sees it may change it. A `read` one only
    /// the shape's own methods change, and one that is neither never changes.
    pub flex: bool,
    /// For a value of a wrapper worked out where the program starts (a member
    /// of `START_VALUES`), where it is, as a runtime message names it:
    /// `wrapper 'logged' on task 'sq'`. `None` for a field a program declares.
    pub start: Option<String>,
}

impl SharedField {
    /// Whether anything may ever change it: `flex`, or `read`, which its own
    /// shape's methods change.
    pub fn changes(&self) -> bool {
        self.flex || self.field.read
    }
}

#[derive(Debug, Clone)]
pub struct Variant {
    pub tag: String,
    pub payload: Vec<Field>,
    /// Position of the variant tag.
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct KindDef {
    pub name: String,
    /// `kind Option<T>`: the types the kind takes, empty for an ordinary kind.
    /// Its variants are checked once with these in place of types, and one
    /// kind is made for each set of types it is given (see the checker's
    /// `kind_instance`), as for a shape.
    pub types: Vec<TypeParam>,
    /// The types this kind was made for, when it is one use of a kind that
    /// takes types; empty otherwise.
    pub given: Vec<Type>,
    pub variants: Vec<Variant>,
    /// Position of the kind name.
    pub span: Span,
    /// `priv kind`: only its own file can name it or its variants.
    pub private: bool,
}

/// `#P = 3.14159`: a name for a value the compiler computes, outside every
/// task, shape, and kind. Its type is its value's, and C has it as a `#define`.
#[derive(Debug, Clone)]
pub struct MacroDef {
    pub name: String,
    /// As written; the checker computes it into a literal (see `Checker::macros`).
    pub value: Expr,
    /// Position of the macro's name.
    pub span: Span,
    /// `priv #P = …`: only its own file can name it.
    pub private: bool,
}
