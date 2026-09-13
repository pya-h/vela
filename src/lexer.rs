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
    End,
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

    pub fn tokenize(mut self) -> Result<(Vec<Token>, Vec<usize>, Vec<usize>)> {
        let mut tokens = Vec::new();
        let mut lines = Vec::new();
        let mut cols = Vec::new();
        loop {
            let (tok, (line, col)) = self.next_token()?;
            let is_eof = tok == Token::Eof;
            tokens.push(tok);
            lines.push(line);
            cols.push(col);
            if is_eof {
                break;
            }
        }
        Ok((tokens, lines, cols))
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

    fn location(&self) -> (usize, usize) {
        (self.line, self.col)
    }

    pub fn next_token(&mut self) -> Result<(Token, (usize, usize))> {
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

        if c.is_alphabetic() || c == '_' {
            return self.identifier();
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
                    return Err(Error::new(loc.0, loc.1, "unexpected '!'"));
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
            _ => return Err(Error::new(loc.0, loc.1, format!("unexpected character '{}'", c))),
        };
        Ok((tok, loc))
    }

    fn identifier(&mut self) -> Result<(Token, (usize, usize))> {
        let loc = self.location();
        let mut s = String::new();
        while self.peek().is_alphanumeric() || self.peek() == '_' {
            s.push(self.advance());
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
            "end" => Token::End,
            _ => Token::Ident(s),
        };
        Ok((tok, loc))
    }

    fn number(&mut self) -> Result<(Token, (usize, usize))> {
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
            let val = s.parse::<f64>().map_err(|_| Error::new(loc.0, loc.1, "invalid float"))?;
            Ok((Token::Float(val), loc))
        } else {
            let val = s.parse::<i64>().map_err(|_| Error::new(loc.0, loc.1, "invalid integer"))?;
            Ok((Token::Int(val), loc))
        }
    }

    fn string(&mut self) -> Result<(Token, (usize, usize))> {
        let loc = self.location();
        self.advance(); // opening "
        let mut s = String::new();
        while self.peek() != '"' && self.peek() != '\0' {
            if self.peek() == '\\' {
                self.advance();
                let c = self.advance();
                match c {
                    'n' => s.push('\n'),
                    't' => s.push('\t'),
                    'r' => s.push('\r'),
                    '\\' => s.push('\\'),
                    '"' => s.push('"'),
                    '0' => s.push('\0'),
                    _ => return Err(Error::new(self.line, self.col, format!("invalid escape '\\{}'", c))),
                }
            } else {
                s.push(self.advance());
            }
        }
        if self.peek() != '"' {
            return Err(Error::new(loc.0, loc.1, "unterminated string"));
        }
        self.advance(); // closing "
        Ok((Token::Text(s), loc))
    }
}
