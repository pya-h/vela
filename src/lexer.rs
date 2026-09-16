use std::ops::Range;

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
    /// `<-`, which sends a task's result (the counterpart of `->` in its header).
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
    /// `region`: declares a region, or is the type of a region parameter.
    Region,
    And,
    Or,
    Not,
    Fi,
    Maybe,
    Is,
    Fail,
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
    /// `@`, which names the region a type lives in (`^Node@r`) or `new`
    /// allocates in (`new@r T(v)`). The word `at` is `Token::At`.
    AtSign,
    /// The word `at`, which names the region a `new` allocates in:
    /// `new T(v) at r`, the same as `new@r T(v)`.
    At,
    /// `_`, which matches or binds a value without naming it.
    Underscore,
    Newline,
    Eof,
}

/// Every keyword with its token: the one list of Vela's reserved words, used
/// by the lexer and by tools such as editors.
pub const KEYWORDS: &[(&str, Token)] = &[
    ("task", Token::Task),
    ("bind", Token::Bind),
    ("hold", Token::Hold),
    ("shape", Token::Shape),
    ("kind", Token::Kind),
    ("when", Token::When),
    ("else", Token::Else),
    ("loop", Token::Loop),
    ("each", Token::Each),
    ("in", Token::In),
    ("at", Token::At),
    ("match", Token::Match),
    ("case", Token::Case),
    ("new", Token::New),
    ("region", Token::Region),
    ("and", Token::And),
    ("or", Token::Or),
    ("not", Token::Not),
    ("true", Token::True),
    ("false", Token::False),
    ("none", Token::None),
    ("fi", Token::Fi),
    ("maybe", Token::Maybe),
    ("is", Token::Is),
    ("fail", Token::Fail),
];

/// The escapes allowed in text literals: the character after `\` and the
/// character it stands for.
pub const ESCAPES: &[(char, char)] = &[
    ('n', '\n'),
    ('t', '\t'),
    ('r', '\r'),
    ('\\', '\\'),
    ('"', '"'),
    ('0', '\0'),
];

impl Token {
    /// A human-readable description for diagnostics, e.g. `'fi'` or
    /// `identifier 'x'`.
    pub fn describe(&self) -> String {
        if let Some((word, _)) = KEYWORDS.iter().find(|(_, token)| token == self) {
            return format!("'{}'", word);
        }
        let symbol = match self {
            Token::Int(n) => return format!("integer {}", n),
            Token::Float(n) => return format!("float {}", float_text(*n)),
            Token::Text(_) => return "text literal".to_string(),
            Token::Ident(s) => return format!("identifier '{}'", s),
            Token::Newline => return "end of line".to_string(),
            Token::Eof => return "end of file".to_string(),
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
            Token::Send => "<-",
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
            Token::AtSign => "@",
            Token::Underscore => "_",
            // Keywords are found in `KEYWORDS` above.
            _ => return format!("{:?}", self),
        };
        format!("'{}'", symbol)
    }
}

/// A float as Vela writes it: digits with a `.`, never an exponent.
fn float_text(value: f64) -> String {
    let text = value.to_string();
    if text.contains('.') || !value.is_finite() { text } else { format!("{}.0", text) }
}

/// A token from `highlight`, with its exact extent in the source.
#[derive(Debug, Clone, PartialEq)]
pub struct LexToken {
    pub kind: LexKind,
    /// Position of the token's first character.
    pub start: Span,
    /// Position just after the token's last character.
    pub end: Span,
    /// The token's byte range in the source.
    pub range: Range<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LexKind {
    Token(Token),
    /// A `#` comment, up to (not including) the end of the line.
    Comment,
    /// Text the compiler rejects, with the error it reports. The first
    /// `Invalid` token's error is exactly the error `Lexer::tokenize` returns.
    Invalid(Error),
}

/// Splits `source` into tokens without ever failing, for editors: text the
/// compiler would reject becomes an `Invalid` token and lexing continues after
/// it, and comments are kept. The result always ends with `Eof`. For source the
/// compiler accepts, the tokens other than comments are exactly those
/// `Lexer::tokenize` returns.
pub fn highlight(source: &str) -> Vec<LexToken> {
    let mut lexer = Lexer::new(source);
    let mut tokens = Vec::new();
    loop {
        let token = lexer.next_item();
        let is_eof = token.kind == LexKind::Token(Token::Eof);
        tokens.push(token);
        if is_eof {
            return tokens;
        }
    }
}

pub struct Lexer {
    source: Vec<char>,
    /// Index of the next character in `source`.
    pos: usize,
    /// Byte offset of the next character.
    byte: usize,
    line: usize,
    col: usize,
}

/// A saved lexer position, for backing up.
#[derive(Clone, Copy)]
struct Mark {
    pos: usize,
    byte: usize,
    line: usize,
    col: usize,
}

impl Lexer {
    pub fn new(source: &str) -> Self {
        let source: Vec<char> = source.chars().collect();
        // A byte order mark (which some Windows editors write) is skipped
        // without taking a column; byte offsets still count it.
        let bom = source.first() == Some(&'\u{feff}');
        Lexer {
            source,
            pos: usize::from(bom),
            byte: if bom { '\u{feff}'.len_utf8() } else { 0 },
            line: 1,
            col: 1,
        }
    }

    /// Returns every token (ending with `Eof`) and, in parallel, its position,
    /// or the first error.
    pub fn tokenize(mut self) -> Result<(Vec<Token>, Vec<Span>)> {
        let mut tokens = Vec::new();
        let mut spans = Vec::new();
        loop {
            let item = self.next_item();
            match item.kind {
                LexKind::Token(token) => {
                    let is_eof = token == Token::Eof;
                    tokens.push(token);
                    spans.push(item.start);
                    if is_eof {
                        return Ok((tokens, spans));
                    }
                }
                LexKind::Comment => {}
                LexKind::Invalid(error) => return Err(error),
            }
        }
    }

    /// Whether the next characters end a line: `\n`, or the `\r\n` of a CRLF file.
    fn at_line_end(&self) -> bool {
        self.peek() == '\n' || (self.peek() == '\r' && self.peek2() == '\n')
    }

    /// Whether the whole source has been read. (`peek` returns `'\0'` there,
    /// but a NUL character can also appear in the source itself.)
    fn at_end(&self) -> bool {
        self.pos >= self.source.len()
    }

    fn peek(&self) -> char {
        self.source.get(self.pos).copied().unwrap_or('\0')
    }

    fn peek2(&self) -> char {
        self.source.get(self.pos + 1).copied().unwrap_or('\0')
    }

    /// Consumes and returns the next character (`'\0'` at the end).
    fn advance(&mut self) -> char {
        let c = self.peek();
        if self.at_end() {
            return c;
        }
        self.pos += 1;
        self.byte += c.len_utf8();
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else if !(c == '\r' && self.peek() == '\n') {
            // The `\r` of a CRLF line end takes no column, so positions match LF files.
            self.col += 1;
        }
        c
    }

    fn mark(&self) -> Mark {
        Mark { pos: self.pos, byte: self.byte, line: self.line, col: self.col }
    }

    fn reset(&mut self, mark: Mark) {
        self.pos = mark.pos;
        self.byte = mark.byte;
        self.line = mark.line;
        self.col = mark.col;
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), ' ' | '\t' | '\r') {
            self.advance();
        }
    }

    fn location(&self) -> Span {
        Span::new(self.line, self.col)
    }

    /// Reads the next token, comment, or invalid text.
    fn next_item(&mut self) -> LexToken {
        self.skip_whitespace();
        let start = self.location();
        let from = self.byte;
        let kind = self.scan(start);
        LexToken { kind, start, end: self.location(), range: from..self.byte }
    }

    fn scan(&mut self, loc: Span) -> LexKind {
        if self.at_end() {
            return LexKind::Token(Token::Eof);
        }
        let c = self.peek();

        if c == '\n' {
            self.advance();
            return LexKind::Token(Token::Newline);
        }

        if c == '#' {
            while !self.at_line_end() && !self.at_end() {
                self.advance();
            }
            return LexKind::Comment;
        }

        if c.is_ascii_alphabetic() {
            return self.identifier(loc);
        }

        if c == '_' {
            // `_` alone is a symbol; a name can't start with it.
            if !(self.peek2().is_alphanumeric() || self.peek2() == '_') {
                self.advance();
                return LexKind::Token(Token::Underscore);
            }
            let start = self.pos;
            self.skip_name();
            let name: String = self.source.get(start..self.pos).unwrap_or_default().iter().collect();
            return invalid(loc, format!("'{}': names must start with a letter", name));
        }

        if !c.is_ascii() && c.is_alphanumeric() {
            self.skip_name();
            return LexKind::Invalid(non_ascii_name(loc, c));
        }

        if c.is_ascii_digit() {
            return self.number(loc);
        }

        if c == '"' {
            return self.string(loc);
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
            '@' => Token::AtSign,
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
                    return invalid(loc, "unexpected '!'; Vela negates with 'not'");
                }
            }
            '&' | '|' => {
                let word = if c == '&' { "and" } else { "or" };
                if self.peek() == c {
                    self.advance();
                    return invalid(loc, format!("unexpected '{}{}'; Vela writes '{}'", c, c, word));
                }
                return invalid(loc, format!("unexpected character '{}'", c));
            }
            '<' => {
                if self.peek() == '=' {
                    self.advance();
                    Token::Le
                } else if self.peek() == '-' {
                    // Always one token, so `a <-1` never quietly means `a < -1`
                    // (the parser explains; see `Parser::parse_send`).
                    self.advance();
                    Token::Send
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
            // `escape_debug` keeps control characters such as NUL readable.
            _ => return invalid(loc, format!("unexpected character '{}'", c.escape_debug())),
        };
        LexKind::Token(tok)
    }

    /// Skips the rest of a malformed name, including any non-ASCII letters, so
    /// it becomes one invalid token.
    fn skip_name(&mut self) {
        while self.peek().is_alphanumeric() || self.peek() == '_' {
            self.advance();
        }
    }

    fn identifier(&mut self, loc: Span) -> LexKind {
        let mut s = String::new();
        while self.peek().is_ascii_alphanumeric() || self.peek() == '_' {
            s.push(self.advance());
        }
        if !self.peek().is_ascii() && self.peek().is_alphanumeric() {
            let error = non_ascii_name(self.location(), self.peek());
            self.skip_name();
            return LexKind::Invalid(error);
        }
        // Keeps Vela names apart from the compiler's C names (see codegen.rs).
        if s.contains("__") {
            return invalid(loc, format!("'{}': names cannot contain '__'", s));
        }
        let token = KEYWORDS
            .iter()
            .find(|(word, _)| *word == s)
            .map(|(_, token)| token.clone())
            .unwrap_or(Token::Ident(s));
        LexKind::Token(token)
    }

    fn number(&mut self, loc: Span) -> LexKind {
        // `s` is the number without its underscores; `raw` is as written, for messages.
        let mut s = String::new();
        let mut raw = String::new();
        let digits = |lexer: &mut Self, s: &mut String, raw: &mut String| {
            while lexer.peek().is_ascii_digit() {
                let c = lexer.advance();
                s.push(c);
                raw.push(c);
                // Underscores between digits are ignored: 1_000_000 reads as 1000000.
                if lexer.peek() == '_' && lexer.peek2().is_ascii_digit() {
                    raw.push(lexer.advance());
                }
            }
        };
        digits(self, &mut s, &mut raw);
        let is_float = self.peek() == '.' && self.peek2().is_ascii_digit();
        // `1.` is not a float (and `1..5` is a range).
        if self.peek() == '.' && !self.peek2().is_ascii_digit() && self.peek2() != '.' && !self.peek2().is_ascii_alphabetic() {
            self.advance();
            return invalid(loc, format!("invalid number '{}.': write '{}.0'", raw, raw));
        }
        if is_float {
            let dot = self.advance();
            s.push(dot);
            raw.push(dot);
            digits(self, &mut s, &mut raw);
        }
        // Letters, digits, or `_` right after a number, as in `12abc`, `1e5`, or
        // `0x1F`, make it invalid rather than a number followed by a name.
        if self.peek().is_ascii_alphabetic() || self.peek() == '_' {
            while self.peek().is_ascii_alphanumeric() || self.peek() == '_' {
                raw.push(self.advance());
            }
            return invalid(loc, format!("invalid number '{}'", raw));
        }
        if is_float {
            match s.parse::<f64>() {
                Ok(val) if val.is_finite() => LexKind::Token(Token::Float(val)),
                Ok(_) => invalid(loc, format!("float literal '{}' is too large", raw)),
                Err(_) => invalid(loc, "invalid float"),
            }
        } else {
            match s.parse::<i64>() {
                Ok(val) => LexKind::Token(Token::Int(val)),
                Err(_) => invalid(loc, format!("integer literal '{}' is too large", raw)),
            }
        }
    }

    /// A text literal. An invalid one is still read up to its closing quote,
    /// reporting its first bad escape. One whose line ends before its closing
    /// quote is an error; as a token it ends with its line, so the lines after
    /// it are lexed normally.
    fn string(&mut self, loc: Span) -> LexKind {
        self.advance(); // opening "
        let mut s = String::new();
        let mut error = None;
        // Where the literal's first line ends, in case it is unterminated.
        let mut line_end = None;
        while !self.at_end() {
            match self.peek() {
                '"' => {
                    self.advance();
                    return match error {
                        Some(error) => LexKind::Invalid(error),
                        None => LexKind::Token(Token::Text(s)),
                    };
                }
                '\\' => {
                    let escape_loc = self.location();
                    self.advance();
                    if self.at_end() {
                        break; // backslash at end of file: unterminated
                    }
                    if self.at_line_end() && self.peek() == '\r' {
                        self.advance(); // a CRLF line end reads like LF
                    }
                    if self.peek() == '\n' {
                        line_end.get_or_insert(self.mark());
                    }
                    let c = self.advance();
                    match ESCAPES.iter().find(|(escape, _)| *escape == c) {
                        Some((_, value)) => s.push(*value),
                        None if error.is_some() => {}
                        None if c == '\n' => {
                            error = Some(Error::at(escape_loc, "invalid escape: '\\' at the end of a line"));
                        }
                        None if c.is_control() => {
                            let what = match c {
                                '\t' => "a tab",
                                '\0' => "a NUL character",
                                _ => "a control character",
                            };
                            error = Some(Error::at(escape_loc, format!("invalid escape: '\\' followed by {}", what)));
                        }
                        None => {
                            error = Some(Error::at(escape_loc, format!("invalid escape '\\{}'", c.escape_debug())));
                        }
                    }
                }
                // Text ends on the line it starts, so a missing `"` is reported
                // where it is missing, not at a later literal.
                c if c == '\n' || self.at_line_end() => break,
                _ => s.push(self.advance()),
            }
        }
        if let Some(mark) = line_end {
            self.reset(mark);
        }
        LexKind::Invalid(error.unwrap_or_else(|| Error::at(loc, "text isn't closed on its line; end it with '\"' (a line break in text is written '\\n')")))
    }
}

fn invalid(span: Span, message: impl Into<String>) -> LexKind {
    LexKind::Invalid(Error::at(span, message))
}

/// Names are ASCII so they map directly to portable C identifiers.
fn non_ascii_name(span: Span, c: char) -> Error {
    Error::at(span, format!("names must use ASCII letters, digits, and '_', found '{}'", c))
}
