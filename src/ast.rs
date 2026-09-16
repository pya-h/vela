use std::fmt;

use crate::error::{Error, Result};

/// Where the names inside each written type are: for the type starting at
/// `(line, col)`, each shape, kind, or builtin name, `@region`, `maybe`,
/// `region`, and `or` (at the error type after it), with its position. The
/// types themselves carry no positions; errors about a part of a type use this
/// to point at that part.
pub type TypePlaces = std::collections::HashMap<(usize, usize), Vec<(String, Span)>>;

/// A position in the source file (1-based line and column).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub line: usize,
    pub col: usize,
}

impl Span {
    pub fn new(line: usize, col: usize) -> Self {
        Span { line, col }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    Int,
    Float,
    Bool,
    Text,
    Unit,
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
    /// `T or E`: a `T` on success, or an error `E`. `T` may be `unit`.
    Or(Box<Type>, Box<Type>),
    /// The type of `fail e` before it becomes part of a `T or E`: the error's type.
    Fail(Box<Type>),
    /// `region`: the type of a region parameter, which a task allocates in.
    Region,
    /// `-> int, int`: the several values a task sends, always two or more.
    /// Only a task's return type, and a call of such a task, ever have this
    /// type: the values are separate, so no variable, field, or operand holds
    /// one.
    Results(Vec<Type>),
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
    ("text", Type::Text),
    ("unit", Type::Unit),
];

/// The builtin tasks, which user code calls like its own tasks.
pub const BUILTIN_TASKS: &[&str] = &["show"];

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
        match self {
            Type::Int => write!(f, "int"),
            Type::Float => write!(f, "float"),
            Type::Bool => write!(f, "bool"),
            Type::Text => write!(f, "text"),
            Type::Unit => write!(f, "unit"),
            Type::None => write!(f, "none"),
            Type::Region => write!(f, "region"),
            Type::Ptr(inner, region) => {
                write!(f, "^")?;
                inner.write(f, region.as_deref().or(outer))?;
                suffix(f, region)
            }
            Type::Named(name, region) => {
                write!(f, "{}", name)?;
                suffix(f, region)
            }
            Type::Range(inner) => {
                write!(f, "range of ")?;
                inner.write(f, outer)
            }
            Type::Maybe(inner) => {
                write!(f, "maybe ")?;
                inner.write(f, outer)
            }
            Type::Or(value, error) => {
                value.write(f, outer)?;
                write!(f, " or ")?;
                error.write(f, outer)
            }
            Type::Fail(error) => {
                write!(f, "fail ")?;
                error.write(f, outer)
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
        }
    }
}

impl Type {
    /// The builtin type called `name`, if there is one.
    pub fn builtin(name: &str) -> Option<Type> {
        BUILTIN_TYPES.iter().find(|(builtin, _)| *builtin == name).map(|(_, ty)| ty.clone())
    }

    pub fn is_numeric(&self) -> bool {
        matches!(self, Type::Int | Type::Float)
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
            Type::Range(inner) => Type::Range(boxed(inner, f)),
            Type::Maybe(inner) => Type::Maybe(boxed(inner, f)),
            Type::Or(value, error) => Type::Or(boxed(value, f), boxed(error, f)),
            Type::Fail(error) => Type::Fail(boxed(error, f)),
            Type::Results(types) => Type::Results(types.iter().map(|ty| ty.map_regions(&mut *f)).collect()),
            other => other.clone(),
        }
    }

    /// The type with a region given to each pointer and shape or kind name
    /// that has none, by `f` (which also sees builtin-free types, and decides
    /// nothing about whether a shape holds pointers).
    pub fn map_regions_of_names(&self, f: &mut dyn FnMut(&str) -> Option<String>) -> Type {
        match self {
            Type::Ptr(inner, None) => {
                let region = f("");
                Type::Ptr(Box::new(inner.map_regions_of_names(f)), region)
            }
            Type::Named(name, None) => Type::Named(name.clone(), f(name)),
            Type::Ptr(inner, region) => Type::Ptr(Box::new(inner.map_regions_of_names(f)), region.clone()),
            Type::Range(inner) => Type::Range(Box::new(inner.map_regions_of_names(f))),
            Type::Maybe(inner) => Type::Maybe(Box::new(inner.map_regions_of_names(f))),
            Type::Or(value, error) => Type::Or(Box::new(value.map_regions_of_names(f)), Box::new(error.map_regions_of_names(f))),
            Type::Fail(error) => Type::Fail(Box::new(error.map_regions_of_names(f))),
            Type::Results(types) => Type::Results(types.iter().map(|ty| ty.map_regions_of_names(&mut *f)).collect()),
            other => other.clone(),
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
            Type::Ptr(inner, region) => {
                regions.extend(region.as_deref());
                inner.collect_regions(regions);
            }
            Type::Named(_, region) => regions.extend(region.as_deref()),
            Type::Range(inner) | Type::Maybe(inner) | Type::Fail(inner) => inner.collect_regions(regions),
            Type::Or(value, error) => {
                value.collect_regions(regions);
                error.collect_regions(regions);
            }
            Type::Results(types) => types.iter().for_each(|ty| ty.collect_regions(regions)),
            _ => {}
        }
    }

    /// The values a task with this return type sends: none for `unit`, each of
    /// a `Results`, or the type itself.
    pub fn results(&self) -> &[Type] {
        match self {
            Type::Unit => &[],
            Type::Results(types) => types,
            other => std::slice::from_ref(other),
        }
    }

    /// The type a plan B or a `{value}` pattern extracts: `T` from `maybe T`
    /// or `T or E`.
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
    Eq,
    Ne,
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
    BoolLit(bool),
    TextLit(String),
    NoneLit,
    Var(String),
    Field(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
    Unary(UnaryOp, Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    /// `new T(value) at r` or `new@r T(value)`; `ty_span` is the position of
    /// `T`, and `region_span` of `r`.
    New {
        ty: Type,
        ty_span: Span,
        value: Box<Expr>,
        region: String,
        region_span: Span,
    },
    Deref(Box<Expr>),
    StructInit(String, Vec<FieldInit>),
    /// A variant constructor, `Circle(1.0)` or `Empty`. The parser produces
    /// `Call` or `Var` for these; the checker rewrites them into `Construct`.
    Construct {
        kind: String,
        tag: String,
        args: Vec<Expr>,
    },
    Range(Box<Expr>, Box<Expr>),
    /// `fail e`: an error value, which fits a `T or E`.
    Fail(Box<Expr>),
    /// A plan B, `value { … }`: the value extracted from a `maybe` or a
    /// `T or E`, or else what the block gives. `error` is the name of a first
    /// line `fail e`, with its position.
    PlanB {
        value: Box<Expr>,
        error: Option<(String, Span)>,
        body: Block,
    },
    /// Added by the checker where a value becomes part of a `maybe` or a
    /// `T or E`: a plain value, `none`, or `fail e`. The node's `ty` is the
    /// type it becomes.
    Wrap(Box<Expr>),
}

/// One `name: value` in a shape literal.
#[derive(Debug, Clone)]
pub struct FieldInit {
    pub name: String,
    /// Position of the field name.
    pub span: Span,
    pub value: Expr,
}

pub type Block = Vec<Stmt>;

/// A statement. `span` points at the declared name for `bind`/`hold` and
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
    /// `bind` (immutable) or `hold` (mutable), with one name per value:
    /// `bind q, r = divmod(17, 5)`.
    Binding {
        names: Vec<Bound>,
        mutable: bool,
        /// The values: one per name, or a single call that sends them all.
        init: Vec<Expr>,
    },
    /// `a, b = b, a`: the places assigned, and the values, as in a binding.
    Assign(Vec<Target>, Vec<Expr>),
    When(Condition, Block, Option<Block>),
    Loop(Condition, Block),
    Each(String, Expr, Block),
    Match(Expr, Vec<Case>),
    /// `<-`, `<- value`, or `<- a, b`: one value per result.
    Send(Vec<Expr>),
    /// `total <- value`: sets named results, and keeps going.
    SetResults(Vec<Binding>, Vec<Expr>),
    /// `region r`: memory that `new` allocates in `r` until the block closes.
    Region(String),
}

/// A name a `bind` or `hold` declares, `q` or `q: int`. `ty` is the
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
}

/// One place an assignment writes to.
#[derive(Debug, Clone)]
pub enum Target {
    /// `_`: takes a value and drops it.
    Discard(Span),
    /// A variable, a field, or what a pointer points to.
    Place(Expr),
}

impl Target {
    pub fn span(&self) -> Span {
        match self {
            Target::Discard(span) => *span,
            Target::Place(expr) => expr.span,
        }
    }
}

/// The condition of a `when` or `loop`: a `bool`, or `value is pattern`.
#[derive(Debug, Clone)]
pub struct Condition {
    pub value: Expr,
    pub pattern: Option<Pattern>,
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
    /// `Circle r`: a variant, naming its payload fields in order.
    Variant { tag: String, binds: Vec<Binding> },
    /// `{v}`: the value in a `maybe`, or the success of a `T or E`.
    Value(Binding),
    /// `none`: an empty `maybe`.
    None,
    /// `fail e`: the error of a `T or E`.
    Fail(Binding),
    /// `_`: anything the earlier cases didn't match.
    Any,
}

impl Pattern {
    /// The pattern without its names, for messages: `Circle`, `{…}`, `none`,
    /// `fail …`, or `_`.
    pub fn label(&self) -> String {
        match &self.kind {
            PatternKind::Variant { tag, .. } => tag.clone(),
            PatternKind::Value(_) => "{…}".to_string(),
            PatternKind::None => "none".to_string(),
            PatternKind::Fail(_) => "fail …".to_string(),
            PatternKind::Any => DISCARD.to_string(),
        }
    }

    /// The names the pattern binds, in order, including discards.
    pub fn bindings(&self) -> &[Binding] {
        match &self.kind {
            PatternKind::Variant { binds, .. } => binds,
            PatternKind::Value(bind) | PatternKind::Fail(bind) => std::slice::from_ref(bind),
            PatternKind::None | PatternKind::Any => &[],
        }
    }
}

impl fmt::Display for Pattern {
    /// The pattern as it is written, with its names, as in `{value}`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            PatternKind::Variant { tag, binds } => {
                write!(f, "{}", tag)?;
                for (name, _) in binds {
                    write!(f, " {}", name)?;
                }
                Ok(())
            }
            PatternKind::Value((name, _)) => write!(f, "{{{}}}", name),
            PatternKind::None => write!(f, "none"),
            PatternKind::Fail((name, _)) => write!(f, "fail {}", name),
            PatternKind::Any => write!(f, "_"),
        }
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
}

#[derive(Debug, Clone)]
pub enum Item {
    Task(TaskDef),
    Shape(ShapeDef),
    Kind(KindDef),
}

#[derive(Debug, Clone)]
pub struct TaskDef {
    pub name: String,
    pub params: Vec<Field>,
    /// What the task sends: `unit` for nothing, one type, or `Type::Results`
    /// for several.
    pub ret: Type,
    /// One entry per result, in order; empty when the task sends nothing.
    pub results: Vec<ResultDef>,
    pub body: Block,
    /// Position of the task name.
    pub span: Span,
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
    pub fields: Vec<Field>,
    /// Position of the shape name.
    pub span: Span,
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
    pub variants: Vec<Variant>,
    /// Position of the kind name.
    pub span: Span,
}
