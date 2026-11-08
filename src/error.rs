use std::fmt;

use crate::ast::Span;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub span: Option<Span>,
    pub message: String,
}

impl Error {
    pub fn at(span: Span, message: impl Into<String>) -> Self {
        Error {
            span: Some(span),
            message: message.into(),
        }
    }

    pub fn internal(message: impl AsRef<str>) -> Self {
        Error {
            span: None,
            message: format!("internal compiler error: {}", message.as_ref()),
        }
    }

    // Location goes into the message because ICEs have no user diagnostic span.
    pub fn internal_at(span: Span, message: impl AsRef<str>) -> Self {
        Error::internal(format!(
            "{} (at {}:{})",
            message.as_ref(),
            span.line,
            span.col
        ))
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
