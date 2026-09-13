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

#[derive(Debug, Clone)]
pub enum Expr {
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

#[derive(Debug, Clone)]
pub enum Stmt {
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
    pub params: Vec<(String, Type)>,
    pub ret: Type,
    pub body: Block,
}

#[derive(Debug, Clone)]
pub struct ShapeDef {
    pub name: String,
    pub fields: Vec<(String, Type)>,
}

#[derive(Debug, Clone)]
pub struct Variant {
    pub tag: String,
    pub payload: Vec<(String, Type)>,
}

#[derive(Debug, Clone)]
pub struct KindDef {
    pub name: String,
    pub variants: Vec<Variant>,
}
