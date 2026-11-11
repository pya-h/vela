use std::fmt;

use crate::error::{Error, Result};

/// Where the names inside each written type are: for the type starting at
/// `(file, line, col)`, each shape, kind, or builtin name, `@region`, `maybe`,
/// `region`, and `~` (at the error type after it), with its position. The
/// types themselves carry no positions; errors about a part of a type use this
/// to point at that part.
pub type TypePlaces = std::collections::HashMap<(usize, usize, usize), Vec<(String, Span)>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub line: usize,
    pub col: usize,
    pub file: usize,
}

impl Span {
    pub fn new(line: usize, col: usize) -> Self {
        Span { line, col, file: 0 }
    }

    pub fn place_key(self) -> (usize, usize, usize) {
        (self.file, self.line, self.col)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    Int,
    Int8,
    Int16,
    Int32,
    Uint16,
    Uint32,
    Uint64,
    Float,
    Float32,
    Bool,
    /// `None` region denotes program-lifetime literal text, compatible with any region.
    Text(Option<String>),
    Unit,
    Char,
    Nothing,
    None,
    Ptr(Box<Type>, Option<String>),
    Named(String, Option<String>),
    Range(Box<Type>),
    Maybe(Box<Type>),
    Or(Box<Type>, Box<Type>),
    Fail(Box<Type>),
    Region,
    Array(Box<Type>, u64),
    Slice(Box<Type>, Option<String>),
    Map(Box<Type>, Box<Type>, Option<String>),
    Tuple(Vec<Type>),
    Results(Vec<Type>),
    /// Boxed to keep `Type` small during deep recursive type checking.
    Applied(Box<Applied>),
    Task(Box<TaskType>),
    Closure(Box<ClosureType>),
    Param(String),
    Group(String),
    /// No type: what the checker uses in place of a type it could not work out,
    /// after reporting why. It stands in wherever the mistake was, so that the
    /// checks that follow have something to go on and report their own mistakes
    /// rather than this one again: it fits every type and every type fits it,
    /// and nothing about it is ever an error (see `Type::unknown`). No program
    /// that checks holds one, so the code generator never sees it.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Applied {
    pub name: String,
    pub types: Vec<Type>,
    pub region: Option<String>,
}

impl Applied {
    pub fn with_types(&self, types: Vec<Type>) -> Applied {
        Applied {
            name: self.name.clone(),
            types,
            region: self.region.clone(),
        }
    }
}

/// Regions are canonicalized in order of appearance so alpha-equivalent task types compare equal.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TaskType {
    pub params: Vec<Type>,
    pub taken: Vec<Taken>,
    pub ret: Type,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KeptMode {
    Plain,
    Bind,
    Flex,
}

/// Keeps the captured region alongside canonical signature regions.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClosureType {
    pub task: TaskType,
    pub region: String,
    pub mode: KeptMode,
}

impl ClosureType {
    pub fn normalized(&self) -> ClosureType {
        ClosureType {
            task: self.task.normalized_beside(Some(&self.region)),
            region: self.region.clone(),
            mode: self.mode,
        }
    }

    /// The type with `put` putting types in place of names in each type its
    /// task's type names, its regions then named as `normalized` names them.
    /// A type put in a parameter's place in the region it keeps, as
    /// `text@this` is in a field's `[@this](T) -> R`, is in one of its own:
    /// the lambda takes text of any region.
    fn with_types_put(&self, put: &mut dyn FnMut(&Type) -> Type) -> Type {
        let kept = self.region.as_str();
        let params = self
            .task
            .params
            .iter()
            .map(|ty| match ty.regions().contains(&kept) {
                true => put(ty),
                false => put(ty).map_regions(&mut |region| {
                    Some(if region == kept { "_given" } else { region }.to_string())
                }),
            })
            .collect();
        let task = TaskType {
            params,
            taken: self.task.taken.clone(),
            ret: put(&self.task.ret),
        };
        Type::Closure(Box::new(
            ClosureType {
                task,
                region: self.region.clone(),
                mode: self.mode,
            }
            .normalized(),
        ))
    }

    fn with_task(&self, f: impl FnOnce(&Type) -> Type) -> Type {
        match f(&Type::Task(Box::new(self.task.clone()))) {
            Type::Task(task) => Type::Closure(Box::new(ClosureType {
                task: *task,
                region: self.region.clone(),
                mode: self.mode,
            })),
            other => other,
        }
    }
}

pub type GroupItems = (Vec<Type>, Vec<Taken>);

/// What the task type holding a group's types sends, where one is a place:
/// no results, which no task sends (see `check::group_binding`). Its regions
/// are the group's, unlike a task type's, which are its own.
pub const GROUP_SENDS: Type = Type::Results(Vec::new());

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Taken {
    Value,
    Flex,
    Bind,
    Place(Passing),
    Region(String),
}

/// What marks a region of a task type's own while it is named again (see
/// `TaskType::normalized_avoiding`), which no region a program writes holds.
const OWN_MARK: &str = "~own";

const TASK_REGIONS: [&str; 6] = ["r", "s", "t", "u", "v", "w"];

impl TaskType {
    pub fn new(params: Vec<Type>, ret: Type) -> TaskType {
        let taken = vec![Taken::Value; params.len()];
        TaskType { params, taken, ret }
    }

    pub fn passing(&self, i: usize) -> Passing {
        match self.taken.get(i) {
            Some(Taken::Place(passing)) => *passing,
            Some(Taken::Flex) => Passing::Flex,
            _ => Passing::Copy,
        }
    }

    pub fn region(&self, i: usize) -> Option<&str> {
        match self.taken.get(i) {
            Some(Taken::Region(name)) => Some(name),
            _ => None,
        }
    }

    pub fn normalized(&self) -> TaskType {
        self.normalized_beside(None)
    }

    pub fn map_regions(&self, f: &mut dyn FnMut(&str) -> Option<String>) -> TaskType {
        let taken = self
            .taken
            .iter()
            .map(|how| match how {
                Taken::Region(name) => Taken::Region(f(name).unwrap_or_else(|| name.clone())),
                other => other.clone(),
            })
            .collect();
        TaskType {
            params: self
                .params
                .iter()
                .map(|ty| ty.map_regions(&mut *f))
                .collect(),
            taken,
            ret: self.ret.map_regions(f),
        }
    }

    pub fn normalized_avoiding(&self, kept: &str) -> TaskType {
        self.map_regions(&mut |name| {
            Some(if name == ANY_REGION {
                name.to_string()
            } else {
                format!("{}{}", name, OWN_MARK)
            })
        })
        .normalized_beside(Some(kept))
    }

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
            let named = TASK_REGIONS
                .iter()
                .map(|name| name.to_string())
                .chain((TASK_REGIONS.len() + 1..).map(|k| format!("r{}", k)));
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

pub const BUILTIN_TYPES: &[(&str, Type)] = &[
    ("int", Type::Int),
    ("int64", Type::Int),
    ("int32", Type::Int32),
    ("int16", Type::Int16),
    ("int8", Type::Int8),
    ("uint", Type::Uint64),
    ("uint64", Type::Uint64),
    ("uint32", Type::Uint32),
    ("uint16", Type::Uint16),
    ("uint8", Type::Unit),
    ("unit", Type::Unit),
    ("float", Type::Float),
    ("float64", Type::Float),
    ("float32", Type::Float32),
    ("bool", Type::Bool),
    ("text", Type::Text(None)),
    ("char", Type::Char),
    ("nothing", Type::Nothing),
];

pub const BUILTIN_TASKS: &[&str] = &["show", "scan"];

pub const SCAN: &str = "scan";

pub const COPY: &str = "copy";

pub const STR: &str = "str";

pub const JSON: &str = "json";

pub const NEW_WORD: &str = "new";

pub const MACRO: &str = "macro";

pub const TEXT_CAST: &str = "text";

pub const INT_CAST: &str = "int";

pub const FLOAT_CAST: &str = "float";

pub const UNIT_CAST: &str = "unit";

pub const CHAR_CAST: &str = "char";

pub const AT: &str = "at";

pub fn number_cast(name: &str) -> Option<Type> {
    match name {
        "int" | "int64" => Some(Type::Int),
        "int8" => Some(Type::Int8),
        "int16" => Some(Type::Int16),
        "int32" => Some(Type::Int32),
        "uint" | "uint64" => Some(Type::Uint64),
        "uint32" => Some(Type::Uint32),
        "uint16" => Some(Type::Uint16),
        "unit" | "uint8" => Some(Type::Unit),
        "float" | "float64" => Some(Type::Float),
        "float32" => Some(Type::Float32),
        CHAR_CAST => Some(Type::Char),
        _ => None,
    }
}

pub const PARSE_ERROR: &str = "ParseError";

pub const NOT_A_NUMBER: &str = "NotANumber";
pub const TOO_LARGE: &str = "TooLarge";

pub const INPUT_ERROR: &str = "InputError";

pub const ENDED: &str = "Ended";
pub const NO_NUMBER: &str = "NoNumber";

pub const WHY: &str = "why";

pub const BUILTIN_KIND_NAMES: &[&str] = &[
    PARSE_ERROR,
    NOT_A_NUMBER,
    TOO_LARGE,
    INPUT_ERROR,
    ENDED,
    NO_NUMBER,
];

pub fn builtin_kind_of(variant: &str) -> Option<&'static str> {
    match variant {
        NOT_A_NUMBER | TOO_LARGE => Some(PARSE_ERROR),
        ENDED | NO_NUMBER => Some(INPUT_ERROR),
        _ => None,
    }
}

pub fn builtin_kinds() -> Vec<KindDef> {
    vec![parse_error_kind(), input_error_kind()]
}

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
            Variant {
                tag: ENDED.to_string(),
                payload: Vec::new(),
                span,
            },
            Variant {
                tag: NO_NUMBER.to_string(),
                payload: vec![why],
                span,
            },
        ],
        span,
        private: false,
        imitates: Vec::new(),
        follows: Vec::new(),
        imitated: 0,
    }
}

fn builtin_span() -> Span {
    Span {
        line: 0,
        col: 0,
        file: usize::MAX,
    }
}

pub fn parse_error_kind() -> KindDef {
    let span = builtin_span();
    let variant = |tag: &str| Variant {
        tag: tag.to_string(),
        payload: Vec::new(),
        span,
    };
    KindDef {
        types: Vec::new(),
        given: Vec::new(),
        name: PARSE_ERROR.to_string(),
        variants: vec![variant(NOT_A_NUMBER), variant(TOO_LARGE)],
        span,
        private: false,
        imitates: Vec::new(),
        follows: Vec::new(),
        imitated: 0,
    }
}

pub const LEN: &str = "len";

pub const GROUP_TUPLE: &str = "tuple";

/// Synthetic member marker for `(args...)`; checker lowers it to `GROUP_TUPLE`.
pub const GROUP_TUPLE_WRITTEN: &str = "...";

pub const ALTERS: &str = "alters";

pub const READ: &str = "read";

pub const NATIVE: &str = "native";

pub const PATTERN: &str = "pattern";

/// Placeholder type for `this` inside pattern definitions; leading `_` avoids user collisions.
pub const THIS_TYPE: &str = "_this";

pub const ALIAS: &str = "alias";

pub const DEFAULT: &str = "default";

pub const TYPE_WORD: &str = "type";

pub const THIS_WORD: &str = "this";

/// Parameter name for the method receiver (`.this`); leading `.` cannot collide with user identifiers.
pub const THIS: &str = ".this";

pub const MEMBER_WRAPPER: &str = ".";

/// Temporary per-line region name used by `show` for `.str()`; leading `.` prevents user shadowing.
pub const SHOWN: &str = ".shown";

/// Mangled method task name (`Owner__on__method`); `__` is rejected in user code.
pub fn method_name(owner: &str, method: &str) -> String {
    format!("{}__on__{}", owner, method)
}

/// Generic task name for type-checking a pattern's default method once without emitting C.
pub fn pattern_default(pattern: &str, method: &str) -> String {
    format!("{}{}{}", pattern, DEFAULT_MARK, method)
}

pub const DEFAULT_MARK: &str = "__default__";

pub fn pattern_call(pattern: &str, method: &str) -> String {
    format!("{}__call__{}", pattern, method)
}

pub fn pattern_call_parts(name: &str) -> Option<(&str, &str)> {
    name.split_once("__call__")
}

/// Mangled inherited variant name (`Kind__Tag`) distinguishing imitated from own variants.
pub fn imitated_variant(kind: &str, tag: &str) -> String {
    format!("{}__{}", kind, tag)
}

/// Synthetic task name for a lambda; `__` cannot collide with user names.
pub fn lambda_name(n: usize) -> String {
    format!("{}{}", LAMBDA_PREFIX, n)
}

pub const LAMBDA_PREFIX: &str = "lambda__";

pub fn kept_name(lambda: &str) -> String {
    format!("{}__kept", lambda)
}

pub fn is_lambda(name: &str) -> bool {
    unqualified(name).starts_with(LAMBDA_PREFIX)
}

/// Synthetic per-module task running startup decorators at program start.
pub const STARTUP: &str = "startup__";

pub fn is_startup(name: &str) -> bool {
    unqualified(name) == STARTUP
}

/// Synthetic wrapper chain task (`task__layer__k`).
pub fn layer_name(name: &str, k: usize) -> String {
    format!("{}{}{}", name, LAYER, k)
}

pub const LAYER: &str = "__layer__";

/// Synthetic shape whose shared fields store wrapper values evaluated at startup.
pub const START_VALUES: &str = "wrapper__values";

pub fn shown_task(name: &str) -> String {
    if is_lambda(name) {
        "this lambda".to_string()
    } else {
        format!("task '{}'", name)
    }
}

pub fn method_parts(name: &str) -> Option<(&str, &str)> {
    name.split_once("__on__")
}

pub fn without_effects(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::IntLit(_)
        | ExprKind::FloatLit(_)
        | ExprKind::CharLit(_)
        | ExprKind::BoolLit(_)
        | ExprKind::TextLit(_)
        | ExprKind::NoneLit
        | ExprKind::Macro { .. }
        | ExprKind::Var(_)
        | ExprKind::Shared { .. }
        | ExprKind::TaskValue(_) => true,
        ExprKind::Piped(_) => true,
        ExprKind::Field(inner, _)
        | ExprKind::Deref(inner)
        | ExprKind::Ref(inner)
        | ExprKind::Wrap(inner)
        | ExprKind::Widen(inner)
        | ExprKind::Unary(UnaryOp::Not, inner) => without_effects(inner),
        ExprKind::Binary(
            BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::Similar
            | BinaryOp::NotSimilar
            | BinaryOp::Lt
            | BinaryOp::Gt
            | BinaryOp::Le
            | BinaryOp::Ge
            | BinaryOp::And
            | BinaryOp::Or,
            a,
            b,
        )
        | ExprKind::Range(a, b) => without_effects(a) && without_effects(b),
        ExprKind::Array(items) | ExprKind::Tuple(items) => items.iter().all(without_effects),
        _ => false,
    }
}

pub fn is_builtin_member(word: &str) -> bool {
    EditOp::from_word(word).is_some() || word == COPY
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.write(f, None)
    }
}

impl Type {
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
            Type::Int8 => write!(f, "int8"),
            Type::Int16 => write!(f, "int16"),
            Type::Int32 => write!(f, "int32"),
            Type::Uint16 => write!(f, "uint16"),
            Type::Uint32 => write!(f, "uint32"),
            Type::Uint64 => write!(f, "uint64"),
            Type::Float => write!(f, "float"),
            Type::Float32 => write!(f, "float32"),
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
                write!(f, "{}", if name == THIS_TYPE { THIS_WORD } else { name })?;
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
            Type::Param(name) if name.starts_with('?') => write!(f, "…"),
            Type::Param(name) => write!(f, "{}", if name == THIS_TYPE { THIS_WORD } else { name }),
            Type::Group(name) => write!(f, "{}...", name),
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
                if task.ret == GROUP_SENDS {
                    return write!(f, ")");
                }
                write!(f, ") -> ")?;
                task.ret.write(f, None)
            }
            Type::Closure(closure) => {
                let mode_word = match closure.mode {
                    KeptMode::Plain => "",
                    KeptMode::Flex => "flex ",
                    KeptMode::Bind => "bind ",
                };
                write!(f, "[{}@{}]", mode_word, closure.region)?;
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
    pub fn builtin(name: &str) -> Option<Type> {
        BUILTIN_TYPES
            .iter()
            .find(|(builtin, _)| *builtin == name)
            .map(|(_, ty)| ty.clone())
    }

    pub fn is_numeric(&self) -> bool {
        matches!(
            self,
            Type::Int
                | Type::Int8
                | Type::Int16
                | Type::Int32
                | Type::Uint16
                | Type::Uint32
                | Type::Uint64
                | Type::Float
                | Type::Float32
                | Type::Unit
        )
    }

    pub fn is_integer(&self) -> bool {
        matches!(
            self,
            Type::Int
                | Type::Int8
                | Type::Int16
                | Type::Int32
                | Type::Uint16
                | Type::Uint32
                | Type::Uint64
                | Type::Unit
        )
    }

    pub fn is_signed_int(&self) -> bool {
        matches!(self, Type::Int | Type::Int8 | Type::Int16 | Type::Int32)
    }

    pub fn is_unsigned_int(&self) -> bool {
        matches!(
            self,
            Type::Unit | Type::Uint16 | Type::Uint32 | Type::Uint64
        )
    }

    pub fn is_float(&self) -> bool {
        matches!(self, Type::Float | Type::Float32)
    }

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
            Type::Ptr(inner, _)
            | Type::Slice(inner, _)
            | Type::Array(inner, _)
            | Type::Range(inner)
            | Type::Maybe(inner)
            | Type::Fail(inner) => inner.unknown(),
            Type::Or(value, error) | Type::Map(value, error, _) => {
                value.unknown() || error.unknown()
            }
            Type::Tuple(types) | Type::Results(types) => types.iter().any(Type::unknown),
            Type::Applied(applied) => applied.types.iter().any(Type::unknown),
            Type::Task(task) => task.params.iter().any(Type::unknown) || task.ret.unknown(),
            Type::Closure(closure) => {
                closure.task.params.iter().any(Type::unknown) || closure.task.ret.unknown()
            }
            Type::Int
            | Type::Int8
            | Type::Int16
            | Type::Int32
            | Type::Uint16
            | Type::Uint32
            | Type::Uint64
            | Type::Float
            | Type::Float32
            | Type::Bool
            | Type::Text(_)
            | Type::Unit
            | Type::Char
            | Type::Nothing
            | Type::None
            | Type::Region
            | Type::Named(..)
            | Type::Param(_)
            | Type::Group(_) => false,
        }
    }

    pub fn ptr_inner(&self) -> Option<&Type> {
        match self {
            Type::Ptr(t, _) => Some(t),
            _ => None,
        }
    }

    pub fn named(name: &str) -> Type {
        Type::Named(name.to_string(), None)
    }

    pub fn without_regions(&self) -> Type {
        self.map_regions(&mut |_| None)
    }

    pub fn map_regions(&self, f: &mut dyn FnMut(&str) -> Option<String>) -> Type {
        let boxed =
            |t: &Type, f: &mut dyn FnMut(&str) -> Option<String>| Box::new(t.map_regions(f));
        match self {
            Type::Ptr(inner, region) => {
                Type::Ptr(boxed(inner, f), region.as_deref().and_then(&mut *f))
            }
            Type::Named(name, region) => Type::Named(name.clone(), region.as_deref().and_then(f)),
            Type::Applied(applied) => Type::Applied(Box::new(Applied {
                name: applied.name.clone(),
                types: applied
                    .types
                    .iter()
                    .map(|ty| ty.map_regions(&mut *f))
                    .collect(),
                region: applied.region.as_deref().and_then(&mut *f),
            })),
            Type::Text(region) => Type::Text(region.as_deref().and_then(f)),
            Type::Array(inner, count) => Type::Array(boxed(inner, f), *count),
            Type::Slice(inner, region) => {
                Type::Slice(boxed(inner, f), region.as_deref().and_then(&mut *f))
            }
            Type::Map(key, value, region) => Type::Map(
                boxed(key, f),
                boxed(value, f),
                region.as_deref().and_then(&mut *f),
            ),
            Type::Range(inner) => Type::Range(boxed(inner, f)),
            Type::Maybe(inner) => Type::Maybe(boxed(inner, f)),
            Type::Or(value, error) => Type::Or(boxed(value, f), boxed(error, f)),
            Type::Fail(error) => Type::Fail(boxed(error, f)),
            Type::Results(types) => {
                Type::Results(types.iter().map(|ty| ty.map_regions(&mut *f)).collect())
            }
            Type::Tuple(items) => {
                Type::Tuple(items.iter().map(|ty| ty.map_regions(&mut *f)).collect())
            }
            Type::Task(task) if task.ret == GROUP_SENDS => Type::Task(Box::new(TaskType {
                params: task
                    .params
                    .iter()
                    .map(|ty| ty.map_regions(&mut *f))
                    .collect(),
                taken: task.taken.clone(),
                ret: GROUP_SENDS,
            })),
            Type::Closure(closure) => {
                let kept = closure.region.clone();
                let Some(region) = f(&kept) else {
                    return self.clone();
                };
                let mut own = |name: &str| {
                    Some(match name {
                        _ if name == kept => region.clone(),
                        _ if name == region => format!("{}{}", name, OWN_MARK),
                        _ => name.to_string(),
                    })
                };
                let task = closure.task.map_regions(&mut own);
                Type::Closure(Box::new(
                    ClosureType {
                        task,
                        region,
                        mode: closure.mode,
                    }
                    .normalized(),
                ))
            }
            other => other.clone(),
        }
    }

    pub fn map_names(&self, f: &dyn Fn(&str) -> String) -> Type {
        let boxed = |t: &Type| Box::new(t.map_names(f));
        match self {
            Type::Named(name, region) => Type::Named(f(name), region.clone()),
            Type::Applied(applied) => Type::Applied(Box::new(Applied {
                name: f(&applied.name),
                types: applied.types.iter().map(|ty| ty.map_names(f)).collect(),
                region: applied.region.clone(),
            })),
            Type::Task(task) => Type::Task(Box::new(TaskType {
                params: task.params.iter().map(|ty| ty.map_names(f)).collect(),
                taken: task.taken.clone(),
                ret: task.ret.map_names(f),
            })),
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

    pub fn replace_names(&self, f: &dyn Fn(&str) -> Option<Type>) -> Type {
        let boxed = |t: &Type| Box::new(t.replace_names(f));
        match self {
            Type::Named(name, region) => match f(name) {
                Some(ty) => match region {
                    Some(region) => ty.map_regions_of_names(&mut |_| Some(region.clone())),
                    None => ty,
                },
                None => self.clone(),
            },
            Type::Applied(applied) => Type::Applied(Box::new(Applied {
                name: applied.name.clone(),
                types: applied.types.iter().map(|ty| ty.replace_names(f)).collect(),
                region: applied.region.clone(),
            })),
            Type::Task(task) => Type::Task(Box::new(TaskType {
                params: task.params.iter().map(|ty| ty.replace_names(f)).collect(),
                taken: task.taken.clone(),
                ret: task.ret.replace_names(f),
            })),
            Type::Closure(closure) => closure.with_types_put(&mut |ty| ty.replace_names(f)),
            Type::Ptr(inner, region) => Type::Ptr(boxed(inner), region.clone()),
            Type::Array(inner, count) => Type::Array(boxed(inner), *count),
            Type::Slice(inner, region) => Type::Slice(boxed(inner), region.clone()),
            Type::Map(key, value, region) => Type::Map(boxed(key), boxed(value), region.clone()),
            Type::Range(inner) => Type::Range(boxed(inner)),
            Type::Maybe(inner) => Type::Maybe(boxed(inner)),
            Type::Or(value, error) => Type::Or(boxed(value), boxed(error)),
            Type::Fail(error) => Type::Fail(boxed(error)),
            Type::Results(types) => {
                Type::Results(types.iter().map(|ty| ty.replace_names(f)).collect())
            }
            Type::Tuple(items) => Type::Tuple(items.iter().map(|ty| ty.replace_names(f)).collect()),
            other => other.clone(),
        }
    }

    pub fn map_regions_of_names(&self, f: &mut dyn FnMut(&str) -> Option<String>) -> Type {
        match self {
            Type::Text(None) => Type::Text(f("")),
            Type::Closure(closure) if closure.region == THIS_WORD => match f("") {
                Some(region) => Type::Closure(Box::new(ClosureType {
                    task: closure.task.clone(),
                    region,
                    mode: closure.mode,
                })),
                None => self.clone(),
            },
            Type::Ptr(inner, None) => {
                let region = f("");
                Type::Ptr(Box::new(inner.map_regions_of_names(f)), region)
            }
            Type::Named(name, None) => Type::Named(name.clone(), f(name)),
            Type::Applied(applied) => {
                let region = applied
                    .region
                    .clone()
                    .or_else(|| f(&applied.name))
                    .or_else(|| {
                        applied
                            .types
                            .iter()
                            .any(|ty| ty.params().iter().any(|param| !param.starts_with('?')))
                            .then(|| f(""))
                            .flatten()
                    });
                Type::Applied(Box::new(Applied {
                    name: applied.name.clone(),
                    types: applied
                        .types
                        .iter()
                        .map(|ty| ty.map_regions_of_names(&mut *f))
                        .collect(),
                    region,
                }))
            }
            Type::Ptr(inner, region) => {
                Type::Ptr(Box::new(inner.map_regions_of_names(f)), region.clone())
            }
            Type::Slice(inner, None) => {
                let region = f("");
                Type::Slice(Box::new(inner.map_regions_of_names(f)), region)
            }
            Type::Slice(inner, region) => {
                Type::Slice(Box::new(inner.map_regions_of_names(f)), region.clone())
            }
            Type::Map(key, value, None) => {
                let region = f("");
                Type::Map(
                    Box::new(key.map_regions_of_names(f)),
                    Box::new(value.map_regions_of_names(f)),
                    region,
                )
            }
            Type::Map(key, value, region) => Type::Map(
                Box::new(key.map_regions_of_names(f)),
                Box::new(value.map_regions_of_names(f)),
                region.clone(),
            ),
            Type::Array(inner, count) => {
                Type::Array(Box::new(inner.map_regions_of_names(f)), *count)
            }
            Type::Range(inner) => Type::Range(Box::new(inner.map_regions_of_names(f))),
            Type::Maybe(inner) => Type::Maybe(Box::new(inner.map_regions_of_names(f))),
            Type::Or(value, error) => Type::Or(
                Box::new(value.map_regions_of_names(f)),
                Box::new(error.map_regions_of_names(f)),
            ),
            Type::Fail(error) => Type::Fail(Box::new(error.map_regions_of_names(f))),
            Type::Results(types) => Type::Results(
                types
                    .iter()
                    .map(|ty| ty.map_regions_of_names(&mut *f))
                    .collect(),
            ),
            Type::Tuple(items) => Type::Tuple(
                items
                    .iter()
                    .map(|ty| ty.map_regions_of_names(&mut *f))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    pub fn map_params(&self, f: &mut dyn FnMut(&str) -> Option<Type>) -> Type {
        let boxed = |t: &Type, f: &mut dyn FnMut(&str) -> Option<Type>| Box::new(t.map_params(f));
        match self {
            Type::Param(name) => f(name).unwrap_or_else(|| self.clone()),
            Type::Applied(applied) => Type::Applied(Box::new(Applied {
                name: applied.name.clone(),
                types: applied
                    .types
                    .iter()
                    .map(|ty| ty.map_params(&mut *f))
                    .collect(),
                region: applied.region.clone(),
            })),
            Type::Task(task) => Type::Task(Box::new(
                TaskType {
                    params: task
                        .params
                        .iter()
                        .map(|ty| ty.map_params(&mut *f))
                        .collect(),
                    taken: task.taken.clone(),
                    ret: task.ret.map_params(f),
                }
                .normalized(),
            )),
            Type::Closure(closure) => closure.with_types_put(&mut |ty| ty.map_params(&mut *f)),
            Type::Ptr(inner, region) => Type::Ptr(boxed(inner, f), region.clone()),
            Type::Array(inner, count) => Type::Array(boxed(inner, f), *count),
            Type::Slice(inner, region) => Type::Slice(boxed(inner, f), region.clone()),
            Type::Map(key, value, region) => {
                Type::Map(boxed(key, f), boxed(value, f), region.clone())
            }
            Type::Range(inner) => Type::Range(boxed(inner, f)),
            Type::Maybe(inner) => Type::Maybe(boxed(inner, f)),
            Type::Or(value, error) => Type::Or(boxed(value, f), boxed(error, f)),
            Type::Fail(error) => Type::Fail(boxed(error, f)),
            Type::Results(types) => {
                Type::Results(types.iter().map(|ty| ty.map_params(&mut *f)).collect())
            }
            Type::Tuple(items) => {
                Type::Tuple(items.iter().map(|ty| ty.map_params(&mut *f)).collect())
            }
            other => other.clone(),
        }
    }

    pub fn map_groups(&self, f: &mut dyn FnMut(&str) -> Option<Vec<Type>>) -> Type {
        self.map_group_items(&mut |name| {
            f(name).map(|types| {
                let taken = vec![Taken::Value; types.len()];
                (types, taken)
            })
        })
    }

    pub fn map_group_items(&self, f: &mut dyn FnMut(&str) -> Option<GroupItems>) -> Type {
        let boxed = |t: &Type, f: &mut dyn FnMut(&str) -> Option<GroupItems>| {
            Box::new(t.map_group_items(f))
        };
        match self {
            Type::Task(task) => {
                let mut params = Vec::new();
                let mut taken = Vec::new();
                for (param, how) in task.params.iter().zip(&task.taken) {
                    match param {
                        Type::Group(name) => match f(name) {
                            Some((types, how)) => {
                                taken.extend(
                                    how.into_iter()
                                        .chain(std::iter::repeat(Taken::Value))
                                        .take(types.len()),
                                );
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
                Type::Task(Box::new(TaskType {
                    params,
                    taken,
                    ret: task.ret.map_group_items(f),
                }))
            }
            Type::Closure(closure) => closure.with_task(|task| task.map_group_items(f)),
            Type::Applied(applied) => Type::Applied(Box::new(Applied {
                name: applied.name.clone(),
                types: applied
                    .types
                    .iter()
                    .map(|ty| ty.map_group_items(&mut *f))
                    .collect(),
                region: applied.region.clone(),
            })),
            Type::Ptr(inner, region) => Type::Ptr(boxed(inner, f), region.clone()),
            Type::Array(inner, count) => Type::Array(boxed(inner, f), *count),
            Type::Slice(inner, region) => Type::Slice(boxed(inner, f), region.clone()),
            Type::Map(key, value, region) => {
                Type::Map(boxed(key, f), boxed(value, f), region.clone())
            }
            Type::Range(inner) => Type::Range(boxed(inner, f)),
            Type::Maybe(inner) => Type::Maybe(boxed(inner, f)),
            Type::Or(value, error) => Type::Or(boxed(value, f), boxed(error, f)),
            Type::Fail(error) => Type::Fail(boxed(error, f)),
            Type::Results(types) => {
                Type::Results(types.iter().map(|ty| ty.map_group_items(&mut *f)).collect())
            }
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

    pub fn normalized_tasks(&self) -> Type {
        let boxed = |t: &Type| Box::new(t.normalized_tasks());
        match self {
            Type::Task(task) => {
                let task = TaskType {
                    params: task.params.iter().map(Type::normalized_tasks).collect(),
                    taken: task.taken.clone(),
                    ret: task.ret.normalized_tasks(),
                };
                Type::Task(Box::new(task.normalized()))
            }
            Type::Closure(closure) => {
                let task = TaskType {
                    params: closure
                        .task
                        .params
                        .iter()
                        .map(Type::normalized_tasks)
                        .collect(),
                    taken: closure.task.taken.clone(),
                    ret: closure.task.ret.normalized_tasks(),
                };
                Type::Closure(Box::new(
                    ClosureType {
                        task,
                        region: closure.region.clone(),
                        mode: closure.mode,
                    }
                    .normalized(),
                ))
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
            Type::Results(types) => {
                Type::Results(types.iter().map(Type::normalized_tasks).collect())
            }
            Type::Tuple(items) => Type::Tuple(items.iter().map(Type::normalized_tasks).collect()),
            other => other.clone(),
        }
    }

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
                closure
                    .task
                    .params
                    .iter()
                    .for_each(|ty| ty.collect_params(names));
                closure.task.ret.collect_params(names);
            }
            Type::Ptr(inner, _)
            | Type::Slice(inner, _)
            | Type::Range(inner)
            | Type::Maybe(inner)
            | Type::Fail(inner)
            | Type::Array(inner, _) => inner.collect_params(names),
            Type::Map(key, value, _) => {
                key.collect_params(names);
                value.collect_params(names);
            }
            Type::Or(value, error) => {
                value.collect_params(names);
                error.collect_params(names);
            }
            Type::Results(types) | Type::Tuple(types) => {
                types.iter().for_each(|ty| ty.collect_params(names))
            }
            _ => {}
        }
    }

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
                applied
                    .types
                    .iter()
                    .for_each(|ty| ty.collect_regions(regions));
            }
            Type::Range(inner) | Type::Maybe(inner) | Type::Fail(inner) | Type::Array(inner, _) => {
                inner.collect_regions(regions)
            }
            Type::Or(value, error) => {
                value.collect_regions(regions);
                error.collect_regions(regions);
            }
            Type::Results(types) | Type::Tuple(types) => {
                types.iter().for_each(|ty| ty.collect_regions(regions))
            }
            Type::Task(task) if task.ret == GROUP_SENDS => task
                .params
                .iter()
                .for_each(|ty| ty.collect_regions(regions)),
            Type::Closure(closure) => regions.push(&closure.region),
            _ => {}
        }
    }

    pub fn results(&self) -> &[Type] {
        match self {
            Type::Nothing => &[],
            Type::Results(types) => types,
            other => std::slice::from_ref(other),
        }
    }

    pub fn extracted(&self) -> Option<&Type> {
        match self {
            Type::Maybe(inner) | Type::Or(inner, _) => Some(inner),
            _ => None,
        }
    }
}

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
    Pow,
    Eq,
    Ne,
    Similar,
    NotSimilar,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
    pub ty: Option<Type>,
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Self {
        Expr {
            kind,
            span,
            ty: None,
        }
    }

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
    CharLit(u8),
    BoolLit(bool),
    TextLit(String),
    NoneLit,
    Macro {
        name: String,
        value: Box<Expr>,
    },
    Var(String),
    TaskValue(String),
    Invoke {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    Closure {
        lambda: String,
        region: String,
        mode: KeptMode,
        places: Vec<KeptPlace>,
    },
    Kept {
        value: Box<Expr>,
        region: Option<String>,
    },
    Spread(Box<Expr>),
    Field(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    Lookup(Box<Expr>, Box<Expr>),
    Array(Vec<Expr>),
    Tuple(Vec<Expr>),
    Repeat {
        value: Box<Expr>,
        count: Box<Expr>,
    },
    Call {
        name: String,
        types: Vec<TypeArg>,
        args: Vec<Expr>,
    },
    Unary(UnaryOp, Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    New {
        /// Boxed, which keeps every expression small: the compiler's stages
        /// recurse through expressions, and each level holds some on the stack.
        ty: Box<Type>,
        ty_span: Span,
        value: Box<Expr>,
        region: Option<String>,
        region_span: Span,
    },
    Pairs(Vec<(Expr, Expr)>),
    MapOp {
        map: Box<Expr>,
        op: MapOp,
        key: Option<Box<Expr>>,
        region: Option<String>,
        region_span: Span,
    },
    /// `[a, b, ...]` slice literal; lowered to `ExprKind::New` by the checker.
    Shortcut {
        value: Box<Expr>,
        region: Option<String>,
        region_span: Span,
    },
    Deref(Box<Expr>),
    Ref(Box<Expr>),
    Shared {
        shape: String,
        field: String,
    },
    /// `v.type` in a task that takes types, where the value's type names one
    /// (`T`, `[T#3]`): its text is known only once the task is made for the
    /// types a use gives, when the checker makes it the `TextLit` it is (see
    /// `Checker::check_type_of`). Nothing after the checker sees one.
    TypeOf(Box<Type>),
    StructInit {
        name: String,
        types: Vec<TypeArg>,
        fields: Vec<FieldInit>,
    },
    Construct {
        kind: String,
        tag: String,
        args: Vec<Expr>,
        types: Vec<TypeArg>,
    },
    Range(Box<Expr>, Box<Expr>),
    /// A value of a kind that goes where a kind imitating it is expected
    /// (`kind AppError::ParseErr`), made a value of that kind, its type: the
    /// same variant, carrying the same values. Only the checker makes one.
    Widen(Box<Expr>),
    Pipe(Box<Expr>),
    Piped(Box<Expr>),
    Fail(Box<Expr>),
    PlanB {
        value: Box<Expr>,
        error: Option<(String, Span)>,
        body: Block,
    },
    Choice(Box<Stmt>),
    LoopValue(Box<ValueLoop>),
    Wrap(Box<Expr>),
    Scan {
        args: Vec<Expr>,
        region: Option<String>,
        region_span: Span,
    },
    Join {
        parts: Vec<Expr>,
        region: String,
        region_span: Span,
    },
    ToText {
        value: Box<Expr>,
        region: String,
        region_span: Span,
    },
    ToJson {
        value: Box<Expr>,
        region: String,
        region_span: Span,
    },
    Copy {
        value: Box<Expr>,
        region: Option<String>,
        region_span: Span,
    },
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
    Edit {
        items: Box<Expr>,
        op: EditOp,
        args: Vec<Expr>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapOp {
    Has,
    Keys,
    Values,
}

impl MapOp {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditOp {
    Add,
    Del,
    Added,
    Without,
}

impl EditOp {
    pub fn from_word(word: &str) -> Option<EditOp> {
        match word {
            "add" => Some(EditOp::Add),
            "del" => Some(EditOp::Del),
            "added" => Some(EditOp::Added),
            "without" => Some(EditOp::Without),
            _ => None,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            EditOp::Add => "add",
            EditOp::Del => "del",
            EditOp::Added => "added",
            EditOp::Without => "without",
        }
    }

    pub fn makes_copy(self) -> bool {
        matches!(self, EditOp::Added | EditOp::Without)
    }
}

#[derive(Debug, Clone)]
pub struct FieldInit {
    pub name: String,
    pub span: Span,
    pub value: Expr,
    pub filled: bool,
}

pub type Block = Vec<Stmt>;

#[derive(Debug, Clone)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

impl Stmt {
    pub fn new(kind: StmtKind, span: Span) -> Self {
        Stmt { kind, span }
    }

    pub fn finally(&self) -> Option<&Block> {
        match &self.kind {
            StmtKind::Loop(_, _, finally) | StmtKind::Each(_, _, _, _, finally) => finally.as_ref(),
            StmtKind::Parted(parted) => parted.finally.as_ref(),
            _ => None,
        }
    }

    pub fn finally_mut(&mut self) -> Option<&mut Block> {
        match &mut self.kind {
            StmtKind::Loop(_, _, finally) | StmtKind::Each(_, _, _, _, finally) => finally.as_mut(),
            StmtKind::Parted(parted) => parted.finally.as_mut(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum StmtKind {
    Expr(Expr),
    Binding {
        names: Vec<Bound>,
        mode: Mode,
        reference: bool,
        init: Vec<Expr>,
        colon: bool,
    },
    Assign(Vec<Target>, Vec<Expr>),
    Step {
        target: Expr,
        op: BinaryOp,
        value: Expr,
        symbol: &'static str,
        at: Span,
    },
    When(Vec<WhenArm>, Option<Block>),
    Loop(Condition, Block, Option<Block>),
    Parted(Box<PartedLoop>),
    Block(Block),
    Each(String, Option<(String, Span)>, Expr, Block, Option<Block>),
    Named(String, Box<Stmt>),
    Break(Option<(String, Span)>, Option<Box<Expr>>),
    Continue(Option<(String, Span)>),
    /// In a copy of a task, `each v in args` written out for the values the
    /// copy is given: one block per value, each giving `v` first. It is still a
    /// loop, which a `break` stops and a `continue` takes to the next block (see
    /// `check::Checker::each_of_group`).
    Turns(Vec<Block>),
    Match(Expr, Vec<Case>),
    Send(Vec<Expr>),
    SetResults(Vec<Binding>, Vec<Expr>),
    Region {
        names: Vec<RegionDecl>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionDecl {
    pub name: String,
    pub default: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Bound {
    pub name: String,
    pub span: Span,
    pub ty: Option<Type>,
    pub ty_span: Option<Span>,
    pub parts: Vec<Bound>,
}

impl Bound {
    pub fn leaves(&self) -> Vec<&Bound> {
        if self.parts.is_empty() {
            return vec![self];
        }
        self.parts.iter().flat_map(Bound::leaves).collect()
    }
}

#[derive(Debug, Clone)]
pub enum Target {
    Discard(Span),
    Place(Expr),
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

#[derive(Debug, Clone)]
pub struct PartedLoop {
    pub start: Vec<Stmt>,
    pub cond: Option<Condition>,
    pub step: Vec<Stmt>,
    pub body: Block,
    pub finally: Option<Block>,
    pub endless: bool,
}

#[derive(Debug, Clone)]
pub struct ValueLoop {
    pub stmt: Stmt,
    pub name: Option<(String, Span)>,
    pub arrow: bool,
    pub gives: bool,
    /// Whether nothing in it tells `B` (no `break` gives a value) and `M` (no
    /// last line fails): each is `bool` until the place the value goes names
    /// it, `int ~ LoopResult<int, text>` (see `Checker::settle_variant`).
    pub open: (bool, bool),
}

impl ValueLoop {
    pub fn body(&self) -> Option<&Block> {
        match &self.stmt.kind {
            StmtKind::Loop(_, body, _) | StmtKind::Each(_, _, _, body, _) => Some(body),
            StmtKind::Parted(parted) => Some(&parted.body),
            _ => None,
        }
    }

    pub fn body_mut(&mut self) -> Option<&mut Block> {
        match &mut self.stmt.kind {
            StmtKind::Loop(_, body, _) | StmtKind::Each(_, _, _, body, _) => Some(body),
            StmtKind::Parted(parted) => Some(&mut parted.body),
            _ => None,
        }
    }

    pub fn finally(&self) -> Option<&Block> {
        self.stmt.finally()
    }

    pub fn finally_mut(&mut self) -> Option<&mut Block> {
        self.stmt.finally_mut()
    }
}

#[derive(Debug, Clone)]
pub struct Condition {
    pub value: Expr,
    pub pattern: Option<Pattern>,
    pub and: Option<Box<Condition>>,
}

impl Condition {
    pub fn new(value: Expr, pattern: Option<Pattern>) -> Condition {
        Condition {
            value,
            pattern,
            and: None,
        }
    }

    pub fn parts(&self) -> impl Iterator<Item = &Condition> {
        std::iter::successors(Some(self), |part| part.and.as_deref())
    }

    pub fn part_mut(&mut self, n: usize) -> Option<&mut Condition> {
        let mut part = self;
        for _ in 0..n {
            part = part.and.as_deref_mut()?;
        }
        Some(part)
    }

    pub fn single(&self) -> bool {
        self.and.is_none()
    }
}

#[derive(Debug, Clone)]
pub struct WhenArm {
    pub cond: Condition,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Case {
    pub pattern: Pattern,
    pub body: Block,
}

#[derive(Debug, Clone)]
pub struct Pattern {
    pub kind: PatternKind,
    pub span: Span,
}

pub type Binding = (String, Span);

#[derive(Debug, Clone)]
pub enum PatternKind {
    Variant {
        kind: Option<Box<Binding>>,
        tag: String,
        binds: Vec<Names>,
    },
    Value(Names),
    None,
    Fail(Names),
    Tuple(Names),
    Any,
    Typed {
        ty: Type,
        form: TypedForm,
        asks: bool,
    },
}

#[derive(Debug, Clone)]
pub enum TypedForm {
    Only,
    Named(Names),
    Value(Names),
}

#[derive(Debug, Clone)]
pub struct Names {
    pub name: String,
    pub span: Span,
    pub mode: Mode,
    pub parts: Vec<Names>,
}

impl Names {
    pub fn one(name: String, span: Span, mode: Mode) -> Names {
        Names {
            name,
            span,
            mode,
            parts: Vec::new(),
        }
    }

    pub fn leaves(&self) -> Vec<&Names> {
        if self.parts.is_empty() {
            return vec![self];
        }
        self.parts.iter().flat_map(Names::leaves).collect()
    }
}

impl Pattern {
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

    pub fn bindings(&self) -> Vec<&Names> {
        match &self.kind {
            PatternKind::Variant { binds, .. } => binds.iter().flat_map(Names::leaves).collect(),
            PatternKind::Value(names) | PatternKind::Fail(names) | PatternKind::Tuple(names) => {
                names.leaves()
            }
            PatternKind::Typed {
                form: TypedForm::Named(names) | TypedForm::Value(names),
                ..
            } => names.leaves(),
            PatternKind::None | PatternKind::Any | PatternKind::Typed { .. } => Vec::new(),
        }
    }
}

impl fmt::Display for Pattern {
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
            PatternKind::Typed {
                ty,
                form: TypedForm::Only,
                ..
            } => write!(f, "{}", ty),
            PatternKind::Typed {
                ty,
                form: TypedForm::Named(names),
                ..
            } => write!(f, "{} {}", ty, names),
            PatternKind::Typed {
                ty,
                form: TypedForm::Value(names),
                ..
            } => write!(f, "{} {{{}}}", ty, names),
        }
    }
}

impl fmt::Display for Names {
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

#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub ty: Type,
    pub span: Span,
    pub ty_span: Span,
    pub private: bool,
    pub read: bool,
    pub default_region: bool,
    pub passing: Passing,
    pub default: Option<Expr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Passing {
    #[default]
    Copy,
    Flex,
    FlexRef,
    Fixed,
    FixedRef,
    Bind,
    BindRef,
}

impl Passing {
    pub fn is_ref(self) -> bool {
        matches!(
            self,
            Passing::FlexRef | Passing::FixedRef | Passing::BindRef
        )
    }

    pub fn changes(self) -> bool {
        matches!(
            self,
            Passing::Flex | Passing::FlexRef | Passing::Bind | Passing::BindRef
        )
    }

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Fixed,
    Bind,
    Flex,
    Hold,
}

impl Mode {
    pub fn can_assign(self) -> bool {
        matches!(self, Mode::Flex | Mode::Hold)
    }

    pub fn can_change_contents(self, has_contents: bool) -> bool {
        match self {
            Mode::Flex => true,
            Mode::Fixed | Mode::Hold => false,
            Mode::Bind => has_contents,
        }
    }

    pub fn is_fixed(self) -> bool {
        self == Mode::Fixed
    }

    pub fn word(self) -> &'static str {
        match self {
            Mode::Fixed => "fixed",
            Mode::Bind => "bind",
            Mode::Flex => "flex",
            Mode::Hold => "hold",
        }
    }
}

pub fn has_contents(ty: &Type, is_shape: &dyn Fn(&str) -> bool) -> bool {
    match ty {
        Type::Array(..) | Type::Slice(..) | Type::Map(..) | Type::Ptr(..) => true,
        // A `[bind @r]` lambda changes what it shares when called, as a slice's items change.
        Type::Closure(closure) => closure.mode == KeptMode::Bind,
        Type::Maybe(inner) | Type::Fail(inner) => has_contents(inner, is_shape),
        Type::Or(a, b) => has_contents(a, is_shape) || has_contents(b, is_shape),
        Type::Named(name, _) => is_shape(name),
        Type::Applied(applied) => is_shape(&applied.name),
        _ => false,
    }
}

#[derive(Debug, Clone)]
pub struct Include {
    pub path: Vec<String>,
    pub span: Span,
    pub name_span: Span,
    pub alias: Option<Binding>,
    pub parts: Vec<Part>,
    pub only_parts: bool,
}

#[derive(Debug, Clone)]
pub struct Part {
    pub name: String,
    pub span: Span,
    pub alias: Option<Binding>,
}

impl Part {
    pub fn local_name(&self) -> &str {
        self.alias
            .as_ref()
            .map_or(self.name.as_str(), |(alias, _)| alias.as_str())
    }

    pub fn local_span(&self) -> Span {
        self.alias.as_ref().map_or(self.span, |(_, at)| *at)
    }
}

impl Include {
    pub fn file_name(&self) -> &str {
        self.path.last().map_or("", String::as_str)
    }

    pub fn local_name(&self) -> Option<&str> {
        if self.only_parts {
            return None;
        }
        Some(
            self.alias
                .as_ref()
                .map_or(self.file_name(), |(alias, _)| alias.as_str()),
        )
    }

    pub fn local_span(&self) -> Span {
        self.alias.as_ref().map_or(self.name_span, |(_, at)| *at)
    }

    pub fn written(&self) -> String {
        self.path.join("/")
    }
}

pub const MODULE_SEPARATOR: char = '.';

pub fn module_of(name: &str) -> &str {
    name.rsplit_once(MODULE_SEPARATOR)
        .map_or("", |(module, _)| module)
}

pub fn unqualified(name: &str) -> &str {
    name.rsplit_once(MODULE_SEPARATOR)
        .map_or(name, |(_, name)| name)
}

#[derive(Debug, Clone)]
pub struct GlobalRegionDef {
    pub capacity: u64,
    pub span: Span,
    pub stmts: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub enum Item {
    Task(TaskDef),
    Shape(ShapeDef),
    Kind(KindDef),
    Macro(MacroDef),
    GlobalRegion(GlobalRegionDef),
}

impl Item {
    pub fn name(&self) -> &str {
        match self {
            Item::Task(t) => &t.name,
            Item::Shape(s) => &s.name,
            Item::Kind(k) => &k.name,
            Item::Macro(m) => &m.name,
            Item::GlobalRegion(_) => "global",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TaskDef {
    pub name: String,
    pub types: Vec<TypeParam>,
    pub given: Vec<Type>,
    pub params: Vec<Field>,
    pub ret: Type,
    pub results: Vec<ResultDef>,
    pub body: Block,
    pub span: Span,
    pub private: bool,
    pub native: bool,
    pub owner: Option<Owner>,
    pub decorators: Vec<Expr>,
    pub passes: Vec<bool>,
    pub wrappers: Vec<Expr>,
    pub kept: Option<Kept>,
    pub shortcut: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KeptPlace {
    pub name: String,
    pub passing: Passing,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Kept {
    pub region: String,
    pub mode: KeptMode,
    pub places: Vec<KeptPlace>,
}

#[derive(Debug, Clone)]
pub struct Owner {
    pub name: String,
    pub alters: bool,
    pub alters_span: Option<Span>,
    pub shared: bool,
}

impl TaskDef {
    pub fn owner_type(&self) -> Option<&str> {
        if self.owner.as_ref().is_some_and(|owner| owner.shared) {
            return method_parts(&self.name).map(|(owner, _)| owner);
        }
        self.owner
            .as_ref()
            .and_then(|_| self.params.first())
            .and_then(|param| match &param.ty {
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
        Type::Int8 => "int8".to_string(),
        Type::Int16 => "int16".to_string(),
        Type::Int32 => "int32".to_string(),
        Type::Unit => "unit".to_string(),
        Type::Uint16 => "uint16".to_string(),
        Type::Uint32 => "uint32".to_string(),
        Type::Uint64 => "uint64".to_string(),
        Type::Float => "float".to_string(),
        Type::Float32 => "float32".to_string(),
        Type::Bool => "bool".to_string(),
        Type::Text(_) => "text".to_string(),
        Type::Char => "char".to_string(),
        Type::Nothing => "nothing".to_string(),
        Type::None => "none".to_string(),
        Type::Region => "region".to_string(),
        Type::Named(name, _) => format!("n_{}", flat_name(name)),
        Type::Param(name) => format!("param__{}", name),
        Type::Group(name) => format!("group__{}", name),
        Type::Applied(applied) => format!(
            "n_{}",
            flat_name(&instance_name(&applied.name, &applied.types))
        ),
        Type::Ptr(inner, _) => format!("ptr__{}", type_word(inner)),
        Type::Range(inner) => format!("range__{}", type_word(inner)),
        Type::Maybe(inner) => format!("maybe__{}", type_word(inner)),
        Type::Or(value, error) => format!("or__{}__{}", type_word(value), type_word(error)),
        Type::Unknown => "unknown".to_string(),
        Type::Fail(error) => format!("fail__{}", type_word(error)),
        Type::Array(items, count) => format!("array__{}__{}", count, type_word(items)),
        Type::Slice(items, _) => format!("slice__{}", type_word(items)),
        Type::Map(key, value, _) => format!("map__{}__{}", type_word(key), type_word(value)),
        Type::Results(types) => format!(
            "results__{}__{}",
            types.len(),
            types.iter().map(type_word).collect::<Vec<_>>().join("__")
        ),
        Type::Tuple(items) => format!(
            "tuple__{}__{}",
            items.len(),
            items.iter().map(type_word).collect::<Vec<_>>().join("__")
        ),
        Type::Task(task) => {
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
            format!(
                "task__{}__{}__{}",
                task.params.len(),
                params.join("__"),
                type_word(&task.ret)
            )
        }
        Type::Closure(closure) => format!(
            "closure__{}",
            type_word(&Type::Task(Box::new(closure.task.clone())))
        ),
    }
}

/// A declaration's name as one word: the name itself, or, for a name from
/// another module, the module's path before a `.` with the `.` written
/// `__0__`, each `/` of the path `__1__`, and each `..` in it `__2__`
/// (`shapes/geometry.area` is `shapes__1__geometry__0__area`).
pub fn flat_name(name: &str) -> String {
    let Some((module, own)) = name.rsplit_once(MODULE_SEPARATOR) else {
        return name.to_string();
    };
    let path: Vec<&str> = module
        .split('/')
        .map(|part| if part == ".." { "__2__" } else { part })
        .collect();
    format!("{}__0__{}", path.join("__1__"), own)
}

pub type TypeArg = (Type, Span);

#[derive(Debug, Clone)]
pub struct TypeParam {
    pub name: String,
    pub span: Span,
    pub needs: Vec<(Need, Span)>,
    pub patterns: Vec<Followed>,
    pub group: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Followed {
    pub name: String,
    pub types: Vec<Type>,
    pub span: Span,
}

pub fn followed_name(ty: &Type) -> Option<&str> {
    match ty {
        Type::Named(name, _) => Some(name),
        Type::Applied(applied) => Some(&applied.name),
        _ => None,
    }
}

pub fn followed_types(ty: &Type) -> &[Type] {
    match ty {
        Type::Applied(applied) => &applied.types,
        _ => &[],
    }
}

impl Followed {
    pub fn plain(name: &str, span: Span) -> Followed {
        Followed {
            name: name.to_string(),
            types: Vec::new(),
            span,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    Ordered,
    Shown,
    Same,
    Similar,
    Frozen,
}

impl Need {
    pub const ALL: [Need; 5] = [
        Need::Ordered,
        Need::Shown,
        Need::Same,
        Need::Similar,
        Need::Frozen,
    ];

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

    pub fn means(self, other: Need) -> bool {
        self == other || (self == Need::Ordered && other == Need::Same)
    }
}

#[derive(Debug, Clone)]
pub struct ResultDef {
    pub name: Option<Binding>,
    pub ty_span: Span,
}

impl TaskDef {
    pub fn ret_span(&self) -> Span {
        self.results
            .first()
            .map_or(self.span, |result| result.ty_span)
    }

    pub fn result_span(&self, i: usize) -> Span {
        self.results
            .get(i)
            .map_or_else(|| self.ret_span(), |result| result.ty_span)
    }

    pub fn result_names(&self) -> impl Iterator<Item = &Binding> {
        self.results
            .iter()
            .filter_map(|result| result.name.as_ref())
    }

    pub fn names_results(&self) -> bool {
        self.results.iter().any(|result| result.name.is_some())
    }
}

#[derive(Debug, Clone)]
pub struct ShapeDef {
    pub name: String,
    pub types: Vec<TypeParam>,
    pub given: Vec<Type>,
    pub fields: Vec<Field>,
    pub shared: Vec<SharedField>,
    pub span: Span,
    pub private: bool,
    pub follows: Vec<(Type, Span)>,
}

#[derive(Debug, Clone)]
pub struct SharedField {
    pub field: Field,
    pub flex: bool,
    pub bind: bool,
    pub start: Option<String>,
}

impl SharedField {
    pub fn changes(&self) -> bool {
        self.flex || self.bind || self.field.read
    }

    pub fn can_change_outside(&self) -> bool {
        self.flex || self.bind
    }
}

#[derive(Debug, Clone)]
pub struct Variant {
    pub tag: String,
    pub payload: Vec<Field>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct KindDef {
    pub name: String,
    pub types: Vec<TypeParam>,
    pub given: Vec<Type>,
    pub variants: Vec<Variant>,
    pub span: Span,
    pub private: bool,
    pub imitates: Vec<(Type, Span)>,
    pub follows: Vec<(Type, Span)>,
    /// How many of `variants`, from the first, come from the kinds it imitates.
    /// Each is named in the program by `imitated_variant`, not by its tag,
    /// which names the variant of the kind it comes from.
    pub imitated: usize,
}

#[derive(Debug, Clone)]
pub struct MacroDef {
    pub name: String,
    pub value: Expr,
    pub span: Span,
    pub private: bool,
}

#[derive(Debug, Clone)]
pub struct PatternDef {
    pub name: String,
    pub types: Vec<TypeParam>,
    pub fields: Vec<Field>,
    pub methods: Vec<TaskDef>,
    pub span: Span,
    pub private: bool,
}

impl PatternDef {
    pub fn method(&self, name: &str) -> Option<&TaskDef> {
        self.methods.iter().find(|method| {
            method
                .owner
                .as_ref()
                .is_some_and(|owner| owner.name == name)
        })
    }
}

#[derive(Debug, Clone)]
pub struct AliasDef {
    pub name: String,
    pub ty: Type,
    pub span: Span,
    pub ty_span: Span,
    pub private: bool,
}
