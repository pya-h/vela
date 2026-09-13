use std::fmt;

#[derive(Debug, Clone)]
pub struct Error {
    pub line: usize,
    pub col: usize,
    pub message: String,
}

impl Error {
    pub fn new(line: usize, col: usize, message: impl Into<String>) -> Self {
        Error {
            line,
            col,
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}:{}] {}", self.line, self.col, self.message)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
