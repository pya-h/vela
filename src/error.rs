use std::fmt;

use crate::ast::Span;

#[derive(Debug, Clone)]
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

    /// An error with no meaningful source position.
    pub fn general(message: impl Into<String>) -> Self {
        Error {
            span: None,
            message: message.into(),
        }
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
