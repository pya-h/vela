use std::fmt;

use crate::ast::Span;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// Where the error was found, if it relates to a source position.
    pub span: Option<Span>,
    pub message: String,
}

impl Error {
    /// An error at a specific source position.
    pub fn at(span: Span, message: impl Into<String>) -> Self {
        Error {
            span: Some(span),
            message: message.into(),
        }
    }

    /// A broken invariant between compiler passes, such as code generation
    /// finding an expression the checker never typed. This is a bug in the
    /// compiler, not in the program being compiled, so it has no position.
    pub fn internal(message: impl AsRef<str>) -> Self {
        Error {
            span: None,
            message: format!("internal compiler error: {}", message.as_ref()),
        }
    }

    /// An internal error about the code at `span`. The position is part of
    /// the message, for bug reports, rather than a diagnostic location.
    pub fn internal_at(span: Span, message: impl AsRef<str>) -> Self {
        Error::internal(format!("{} (at {}:{})", message.as_ref(), span.line, span.col))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.span {
            Some(span) => write!(f, "{}:{}: {}", span.line, span.col, self.message),
            None => write!(f, "{}", self.message),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
