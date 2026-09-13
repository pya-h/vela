use std::fmt;

use crate::error::{Error, Result};

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
    /// The type of the `none` literal: fits any pointer type.
    None,
    Ptr(Box<Type>),
    Named(String),
    Range(Box<Type>),
}

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
    /// Formats the type as it is written in Vela source.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Int => write!(f, "int"),
            Type::Float => write!(f, "float"),
            Type::Bool => write!(f, "bool"),
            Type::Text => write!(f, "text"),
            Type::Unit => write!(f, "unit"),
            Type::None => write!(f, "none"),
            Type::Ptr(inner) => write!(f, "^{}", inner),
            Type::Named(name) => write!(f, "{}", name),
            Type::Range(inner) => write!(f, "range of {}", inner),
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
            Type::Ptr(t) => Some(t),
            _ => None,
        }
    }
}

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
    /// `new T(value)`; `ty_span` is the position of `T`.
    New {
        ty: Type,
        ty_span: Span,
        value: Box<Expr>,
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
    /// `bind` (immutable) or `hold` (mutable). `ty` is the annotation, if
    /// any, at `ty_span`; the checker fills it in with the inferred type
    /// otherwise.
    Binding {
        name: String,
        mutable: bool,
        ty: Option<Type>,
        ty_span: Option<Span>,
        init: Expr,
    },
    Assign(Expr, Expr),
    When(Expr, Block, Option<Block>),
    Loop(Expr, Block),
    Each(String, Expr, Block),
    Match(Expr, Vec<Case>),
    Send(Option<Expr>),
    Free(Expr),
}

#[derive(Debug, Clone)]
pub struct Case {
    pub tag: String,
    /// Names bound to the variant's payload fields, with their positions.
    pub binds: Vec<(String, Span)>,
    pub body: Block,
    /// Position of the variant tag.
    pub span: Span,
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
    pub ret: Type,
    /// Position of the return type, or of the task name when there is none.
    pub ret_span: Span,
    pub body: Block,
    /// Position of the task name.
    pub span: Span,
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
