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
    Ptr(Box<Type>),
    Named(String),
    Range(Box<Type>),
}


impl Type {
    pub fn is_numeric(&self) -> bool {
        matches!(self, Type::Int | Type::Float)
    }

    pub fn ptr_inner(&self) -> Option<&Type> {
        match self {
            Type::Ptr(t) => Some(t),
            _ => None,
        }
    }

    pub fn range_inner(&self) -> Option<&Type> {
        match self {
            Type::Range(t) => Some(t),
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
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Self {
        Expr { kind, span }
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
    New(Box<Type>, Box<Expr>),
    Deref(Box<Expr>),
    StructInit(String, Vec<(String, Expr)>),
    Range(Box<Expr>, Box<Expr>),
}

pub type Block = Vec<Stmt>;

/// A statement. `span` points at the bound name for `bind`/`hold`, and at the
/// first token otherwise.
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
    Bind(String, Type, Option<Expr>),
    Hold(String, Type, Option<Expr>),
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
    pub binds: Vec<String>,
    pub body: Block,
    /// Position of the variant tag.
    pub span: Span,
}

/// A named, typed slot: a task parameter, shape field, or variant payload field.
#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub ty: Type,
    pub span: Span,
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
