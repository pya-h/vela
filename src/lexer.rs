use crate::ast::Span;
use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Int(i64),
    Float(f64),
    Text(String),
    True,
    False,
    None,
    Task,
    Bind,
    Hold,
    Send,
    Shape,
    Kind,
    When,
    Else,
    Loop,
    Each,
    In,
    Match,
    Case,
    New,
    Free,
    And,
    Or,
    Not,
    Fi,
    Ident(String),
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Colon,
    Comma,
    Dot,
    Arrow,
    Range,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    Assign,
    Caret,
    Newline,
    Eof,
}

impl Token {
    /// A human-readable description for diagnostics, e.g. `'fi'` or
    /// `identifier 'x'`.
    pub fn describe(&self) -> String {
        let symbol = match self {
            Token::Int(n) => return format!("integer {}", n),
            Token::Float(n) => return format!("float {:?}", n),
            Token::Text(_) => return "text literal".to_string(),
            Token::Ident(s) => return format!("identifier '{}'", s),
            Token::Newline => return "end of line".to_string(),
            Token::Eof => return "end of file".to_string(),
            Token::True => "true",
            Token::False => "false",
            Token::None => "none",
            Token::Task => "task",
            Token::Bind => "bind",
            Token::Hold => "hold",
            Token::Send => "send",
            Token::Shape => "shape",
            Token::Kind => "kind",
            Token::When => "when",
            Token::Else => "else",
            Token::Loop => "loop",
            Token::Each => "each",
            Token::In => "in",
            Token::Match => "match",
            Token::Case => "case",
            Token::New => "new",
            Token::Free => "free",
            Token::And => "and",
            Token::Or => "or",
            Token::Not => "not",
            Token::Fi => "fi",
            Token::LParen => "(",
            Token::RParen => ")",
            Token::LBrace => "{",
            Token::RBrace => "}",
            Token::LBracket => "[",
            Token::RBracket => "]",
            Token::Colon => ":",
            Token::Comma => ",",
            Token::Dot => ".",
            Token::Arrow => "->",
            Token::Range => "..",
            Token::Plus => "+",
            Token::Minus => "-",
            Token::Star => "*",
            Token::Slash => "/",
            Token::Percent => "%",
            Token::Eq => "==",
            Token::Ne => "!=",
            Token::Lt => "<",
            Token::Gt => ">",
            Token::Le => "<=",
            Token::Ge => ">=",
            Token::Assign => "=",
            Token::Caret => "^",
        };
        format!("'{}'", symbol)
    }
}

pub struct Lexer {
    source: Vec<char>,
    pos: usize,
    line: usize,
    col: usize,
}

impl Lexer {
    pub fn new(source: &str) -> Self {
        Lexer {
            source: source.chars().collect(),
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    /// Returns every token (ending with `Eof`) and, in parallel, its position.
    pub fn tokenize(mut self) -> Result<(Vec<Token>, Vec<Span>)> {
        let mut tokens = Vec::new();
        let mut spans = Vec::new();
        loop {
            let (tok, span) = self.next_token()?;
            let is_eof = tok == Token::Eof;
            tokens.push(tok);
            spans.push(span);
            if is_eof {
                break;
            }
        }
        Ok((tokens, spans))
    }

    fn peek(&self) -> char {
        self.source.get(self.pos).copied().unwrap_or('\0')
    }

    fn peek2(&self) -> char {
        self.source.get(self.pos + 1).copied().unwrap_or('\0')
    }

    fn advance(&mut self) -> char {
        let c = self.peek();
        self.pos += 1;
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        c
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), ' ' | '\t' | '\r') {
            self.advance();
        }
    }

    fn location(&self) -> Span {
        Span::new(self.line, self.col)
    }

    pub fn next_token(&mut self) -> Result<(Token, Span)> {
        self.skip_whitespace();
        let loc = self.location();
        let c = self.peek();

        if c == '\0' {
            return Ok((Token::Eof, loc));
        }

        if c == '\n' {
            self.advance();
            return Ok((Token::Newline, loc));
        }

        if c == '#' {
            while self.peek() != '\n' && self.peek() != '\0' {
                self.advance();
            }
            return self.next_token();
        }

        if c.is_ascii_alphabetic() {
            return self.identifier();
        }

        if c == '_' {
            return Err(Error::at(loc, "names must start with a letter"));
        }

        if !c.is_ascii() && c.is_alphanumeric() {
            return Err(non_ascii_name(loc, c));
        }

        if c.is_ascii_digit() {
            return self.number();
        }

        if c == '"' {
            return self.string();
        }

        self.advance();
        let tok = match c {
            '(' => Token::LParen,
            ')' => Token::RParen,
            '{' => Token::LBrace,
            '}' => Token::RBrace,
            '[' => Token::LBracket,
            ']' => Token::RBracket,
            ':' => Token::Colon,
            ',' => Token::Comma,
            '.' => {
                if self.peek() == '.' {
                    self.advance();
                    Token::Range
                } else {
                    Token::Dot
                }
            }
            '+' => Token::Plus,
            '-' => {
                if self.peek() == '>' {
                    self.advance();
                    Token::Arrow
                } else {
                    Token::Minus
                }
            }
            '*' => Token::Star,
            '/' => Token::Slash,
            '%' => Token::Percent,
            '^' => Token::Caret,
            '=' => {
                if self.peek() == '=' {
                    self.advance();
                    Token::Eq
                } else {
                    Token::Assign
                }
            }
            '!' => {
                if self.peek() == '=' {
                    self.advance();
                    Token::Ne
                } else {
                    return Err(Error::at(loc, "unexpected '!'"));
                }
            }
            '<' => {
                if self.peek() == '=' {
                    self.advance();
                    Token::Le
                } else {
                    Token::Lt
                }
            }
            '>' => {
                if self.peek() == '=' {
                    self.advance();
                    Token::Ge
                } else {
                    Token::Gt
                }
            }
            _ => return Err(Error::at(loc, format!("unexpected character '{}'", c))),
        };
        Ok((tok, loc))
    }

    fn identifier(&mut self) -> Result<(Token, Span)> {
        let loc = self.location();
        let mut s = String::new();
        while self.peek().is_ascii_alphanumeric() || self.peek() == '_' {
            s.push(self.advance());
        }
        if !self.peek().is_ascii() && self.peek().is_alphanumeric() {
            return Err(non_ascii_name(self.location(), self.peek()));
        }
        // Keeps Vela names apart from the compiler's C names (see codegen.rs).
        if s.contains("__") {
            return Err(Error::at(loc, format!("'{}': names cannot contain '__'", s)));
        }
        let tok = match s.as_str() {
            "task" => Token::Task,
            "bind" => Token::Bind,
            "hold" => Token::Hold,
            "send" => Token::Send,
            "shape" => Token::Shape,
            "kind" => Token::Kind,
            "when" => Token::When,
            "else" => Token::Else,
            "loop" => Token::Loop,
            "each" => Token::Each,
            "in" => Token::In,
            "match" => Token::Match,
            "case" => Token::Case,
            "new" => Token::New,
            "free" => Token::Free,
            "and" => Token::And,
            "or" => Token::Or,
            "not" => Token::Not,
            "true" => Token::True,
            "false" => Token::False,
            "none" => Token::None,
            "fi" => Token::Fi,
            _ => Token::Ident(s),
        };
        Ok((tok, loc))
    }

    fn number(&mut self) -> Result<(Token, Span)> {
        let loc = self.location();
        let mut s = String::new();
        while self.peek().is_ascii_digit() {
            s.push(self.advance());
        }
        if self.peek() == '.' && self.peek2().is_ascii_digit() {
            s.push(self.advance());
            while self.peek().is_ascii_digit() {
                s.push(self.advance());
            }
            let val = s.parse::<f64>().map_err(|_| Error::at(loc, "invalid float"))?;
            if !val.is_finite() {
                return Err(Error::at(loc, format!("float literal '{}' is too large", s)));
            }
            Ok((Token::Float(val), loc))
        } else {
            let val = s.parse::<i64>().map_err(|_| Error::at(loc, format!("integer literal '{}' is too large", s)))?;
            Ok((Token::Int(val), loc))
        }
    }

    fn string(&mut self) -> Result<(Token, Span)> {
        let loc = self.location();
        self.advance(); // opening "
        let mut s = String::new();
        while self.peek() != '"' && self.peek() != '\0' {
            if self.peek() == '\\' {
                let escape_loc = self.location();
                self.advance();
                let c = self.advance();
                match c {
                    '\0' => break, // backslash at end of file: unterminated
                    'n' => s.push('\n'),
                    't' => s.push('\t'),
                    'r' => s.push('\r'),
                    '\\' => s.push('\\'),
                    '"' => s.push('"'),
                    '0' => s.push('\0'),
                    _ => return Err(Error::at(escape_loc, format!("invalid escape '\\{}'", c))),
                }
            } else {
                s.push(self.advance());
            }
        }
        if self.peek() != '"' {
            return Err(Error::at(loc, "unterminated string"));
        }
        self.advance(); // closing "
        Ok((Token::Text(s), loc))
    }
}

/// Names are ASCII so they map directly to portable C identifiers.
fn non_ascii_name(span: Span, c: char) -> Error {
    Error::at(span, format!("names must use ASCII letters, digits, and '_', found '{}'", c))
}
