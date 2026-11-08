use std::ops::Range;

use crate::ast::Span;
use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Int(i64),
    Float(f64),
    Char(u8),
    Text(String),
    Interpolated(Vec<Piece>),
    True,
    False,
    None,
    Task,
    Fixed,
    Flex,
    /// Reserved (formerly fixed/flex); diagnosed cleanly in the parser.
    Bind,
    Hold,
    Send,
    Shape,
    Kind,
    When,
    Else,
    Loop,
    Endless,
    Each,
    Break,
    Continue,
    Block,
    In,
    Match,
    Case,
    New,
    Region,
    And,
    Or,
    Not,
    Amp,
    Fi,
    Finally,
    Maybe,
    Is,
    Fail,
    Failure,
    Include,
    Priv,
    Shared,
    Type,
    Ident(String),
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Colon,
    Hash,
    Dollar,
    Question,
    Comma,
    Dot,
    Arrow,
    FatArrow,
    Parts,
    Pipe(usize),
    Range,
    Ellipsis,
    Plus,
    Minus,
    Star,
    Power,
    Increment,
    Decrement,
    PlusAssign,
    MinusAssign,
    StarAssign,
    SlashAssign,
    PercentAssign,
    PowerAssign,
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
    Tilde,
    Similar,
    NotSimilar,
    AtSign,
    At,
    Underscore,
    Newline,
    Eof,
}

pub const KEYWORDS: &[(&str, Token)] = &[
    ("task", Token::Task),
    ("fixed", Token::Fixed),
    ("flex", Token::Flex),
    ("bind", Token::Bind),
    ("hold", Token::Hold),
    ("shape", Token::Shape),
    ("kind", Token::Kind),
    ("when", Token::When),
    ("else", Token::Else),
    ("loop", Token::Loop),
    ("endless", Token::Endless),
    ("each", Token::Each),
    ("break", Token::Break),
    ("continue", Token::Continue),
    ("block", Token::Block),
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
    ("finally", Token::Finally),
    ("maybe", Token::Maybe),
    ("is", Token::Is),
    ("fail", Token::Fail),
    ("failure", Token::Failure),
    ("include", Token::Include),
    ("priv", Token::Priv),
    ("shared", Token::Shared),
    ("type", Token::Type),
];

pub const ESCAPES: &[(char, char)] = &[
    ('n', '\n'),
    ('t', '\t'),
    ('r', '\r'),
    ('\\', '\\'),
    ('"', '"'),
    ('0', '\0'),
    ('{', '{'),
    ('}', '}'),
];

/// How deeply text may hold values holding text holding values: each level
/// is lexed inside the one around it, so without a limit the lexer's own
/// recursion could overflow the stack (see `parser::MAX_NESTING`).
const MAX_TEXT_NESTING: usize = 32;

#[derive(Debug, Clone, PartialEq)]
pub enum Piece {
    Text(String),
    Value {
        open: Box<LexToken>,
        tokens: Vec<LexToken>,
        close: Box<LexToken>,
    },
}

impl Token {
    /// Tokens valid inside generic `<...>` arguments (see `Parser::at_type_args`).
    pub fn writes_type(&self) -> bool {
        matches!(
            self,
            Token::Ident(_)
                | Token::Comma
                | Token::Caret
                | Token::Maybe
                | Token::Tilde
                // `or` is no longer a type's error separator, but a `<int or E>`
                // still reads as types, so the parser can teach `~` there too.
                | Token::Or
                | Token::AtSign
                | Token::LBracket
                | Token::RBracket
                | Token::LParen
                | Token::RParen
                | Token::Hash
                | Token::Int(_)
                | Token::Dot
                | Token::Arrow
        )
    }

    pub fn describe(&self) -> String {
        if let Some((word, _)) = KEYWORDS.iter().find(|(_, token)| token == self) {
            return format!("'{}'", word);
        }
        let symbol = match self {
            Token::Int(n) => return format!("integer {}", n),
            Token::Float(n) => return format!("float {}", float_text(*n)),
            Token::Text(_) | Token::Interpolated(_) => return "text literal".to_string(),
            Token::Char(_) => return "char literal".to_string(),
            Token::Ident(s) => return format!("identifier '{}'", s),
            Token::Newline => return "end of line".to_string(),
            Token::Pipe(n) => return format!("'|{}'", ">".repeat(*n)),
            Token::Eof => return "end of file".to_string(),
            Token::LParen => "(",
            Token::RParen => ")",
            Token::LBrace => "{",
            Token::RBrace => "}",
            Token::LBracket => "[",
            Token::RBracket => "]",
            Token::Colon => ":",
            Token::Hash => "#",
            Token::Dollar => "$",
            Token::Question => "?",
            Token::Comma => ",",
            Token::Dot => ".",
            Token::Arrow => "->",
            Token::FatArrow => "=>",
            Token::Parts => "||",
            Token::Send => "<-",
            Token::Range => "..",
            Token::Ellipsis => "...",
            Token::Plus => "+",
            Token::Minus => "-",
            Token::Star => "*",
            Token::Power => "**",
            Token::Increment => "++",
            Token::Decrement => "--",
            Token::PlusAssign => "+=",
            Token::MinusAssign => "-=",
            Token::StarAssign => "*=",
            Token::SlashAssign => "/=",
            Token::PercentAssign => "%=",
            Token::PowerAssign => "**=",
            Token::Slash => "/",
            Token::Percent => "%",
            Token::Eq => "==",
            Token::Ne => "!=",
            Token::Lt => "<",
            Token::Gt => ">",
            Token::Amp => "&",
            Token::Le => "<=",
            Token::Ge => ">=",
            Token::Assign => "=",
            Token::Caret => "^",
            Token::Tilde => "~",
            Token::Similar => "~=",
            Token::NotSimilar => "<>",
            Token::AtSign => "@",
            Token::Underscore => "_",
            _ => return format!("{:?}", self),
        };
        format!("'{}'", symbol)
    }
}

fn float_text(value: f64) -> String {
    let text = value.to_string();
    if text.contains('.') || !value.is_finite() {
        text
    } else {
        format!("{}.0", text)
    }
}

fn based(letter: char) -> Option<(u32, &'static str)> {
    match letter {
        'x' => Some((16, "a hex digit (0-9, a-f)")),
        'o' => Some((8, "an octal digit (0-7)")),
        'b' => Some((2, "a binary digit (0 or 1)")),
        _ => None,
    }
}

fn read_based(raw: &str) -> std::result::Result<i64, String> {
    let wrong = |why: String| Err(format!("invalid number '{}': {}", raw, why));
    let mut chars = raw.chars().skip(1);
    let letter = chars.next().unwrap_or('x');
    let lower = letter.to_ascii_lowercase();
    let Some((base, digit)) = based(lower) else {
        return wrong("no base".to_string());
    };
    if letter != lower {
        return wrong(format!(
            "its base is written in lowercase, as in '0{}{}'",
            lower,
            &raw[2..]
        ));
    }
    let written = &raw[2..];
    if written.is_empty() {
        return wrong(format!(
            "'0{}' is followed by its digits, as in '{}'",
            lower,
            based_example(lower)
        ));
    }
    if written.starts_with('_') || written.ends_with('_') || written.contains("__") {
        return wrong(format!(
            "an underscore goes between two digits, as in '{}'",
            based_example(lower)
        ));
    }
    if let Some(bad) = written.chars().find(|c| *c != '_' && !c.is_digit(base)) {
        return wrong(format!("'{}' is not {}", bad, digit));
    }
    let digits: String = written.chars().filter(|c| *c != '_').collect();
    match i64::from_str_radix(&digits, base) {
        Ok(value) => Ok(value),
        Err(_) => Err(format!("integer literal '{}' is too large", raw)),
    }
}

fn based_example(letter: char) -> &'static str {
    match letter {
        'x' => "0xff_ff",
        'o' => "0o755",
        _ => "0b1010_0101",
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LexToken {
    pub kind: LexKind,
    pub start: Span,
    pub end: Span,
    pub range: Range<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LexKind {
    Token(Token),
    Comment,
    /// First `Invalid` token carries the exact error returned by `Lexer::tokenize`.
    Invalid(Error),
}

/// Infallible editor lexer turning compiler errors into `Invalid` tokens and retaining comments.
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
    pos: usize,
    byte: usize,
    line: usize,
    col: usize,
    file: usize,
    nesting: usize,
    too_deep: bool,
}

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
            file: 0,
            nesting: 0,
            too_deep: false,
        }
    }

    pub fn for_file(source: &str, file: usize) -> Self {
        Lexer {
            file,
            ..Lexer::new(source)
        }
    }

    pub fn tokenize(mut self) -> Result<(Vec<Token>, Vec<Span>, Vec<Span>)> {
        let mut tokens = Vec::new();
        let mut spans = Vec::new();
        let mut ends = Vec::new();
        loop {
            let item = self.next_item();
            match item.kind {
                LexKind::Token(token) => {
                    let is_eof = token == Token::Eof;
                    tokens.push(token);
                    spans.push(item.start);
                    ends.push(item.end);
                    if is_eof {
                        return Ok((tokens, spans, ends));
                    }
                }
                LexKind::Comment => {}
                LexKind::Invalid(error) => return Err(error),
            }
        }
    }

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
        Mark {
            pos: self.pos,
            byte: self.byte,
            line: self.line,
            col: self.col,
        }
    }

    fn reset(&mut self, mark: Mark) {
        self.pos = mark.pos;
        self.byte = mark.byte;
        self.line = mark.line;
        self.col = mark.col;
    }

    /// Reject lone CR (classic Mac endings) so files do not collapse onto line 1.
    fn lone_cr(&self) -> bool {
        self.peek() == '\r' && self.peek2() != '\n'
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), ' ' | '\t') || (self.peek() == '\r' && self.peek2() == '\n') {
            self.advance();
        }
    }

    fn location(&self) -> Span {
        Span {
            line: self.line,
            col: self.col,
            file: self.file,
        }
    }

    fn next_item(&mut self) -> LexToken {
        self.skip_whitespace();
        let start = self.location();
        let from = self.byte;
        let kind = self.scan(start);
        LexToken {
            kind,
            start,
            end: self.location(),
            range: from..self.byte,
        }
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

        if c == ';' {
            while !self.at_line_end() && !self.at_end() && !self.lone_cr() {
                self.advance();
            }
            return LexKind::Comment;
        }

        if self.lone_cr() {
            self.advance();
            return invalid(
                loc,
                "a line ends with a line feed (LF, as on Linux and macOS) or CR LF (as on Windows), not a carriage return (CR) alone; save the file with LF or CRLF line ends",
            );
        }

        if c.is_ascii_alphabetic() {
            return self.identifier(loc);
        }

        if c == '_' {
            if !(self.peek2().is_alphanumeric() || self.peek2() == '_') {
                self.advance();
                return LexKind::Token(Token::Underscore);
            }
            let start = self.pos;
            self.skip_name();
            let name: String = self
                .source
                .get(start..self.pos)
                .unwrap_or_default()
                .iter()
                .collect();
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

        if c == '\'' {
            return self.char_literal(loc);
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
            '#' => Token::Hash,
            '$' => Token::Dollar,
            '?' => Token::Question,
            ',' => Token::Comma,
            '.' => {
                if self.peek() == '.' {
                    self.advance();
                    if self.peek() == '.' {
                        self.advance();
                        Token::Ellipsis
                    } else {
                        Token::Range
                    }
                } else {
                    Token::Dot
                }
            }
            '+' if self.peek() == '+' => {
                self.advance();
                Token::Increment
            }
            '+' if self.peek() == '=' => {
                self.advance();
                Token::PlusAssign
            }
            '+' => Token::Plus,
            '-' => {
                if self.peek() == '>' {
                    self.advance();
                    Token::Arrow
                } else if self.peek() == '-' {
                    self.advance();
                    Token::Decrement
                } else if self.peek() == '=' {
                    self.advance();
                    Token::MinusAssign
                } else {
                    Token::Minus
                }
            }
            '*' if self.peek() == '*' && self.peek2() == '=' => {
                self.advance();
                self.advance();
                Token::PowerAssign
            }
            '*' if self.peek() == '*' => {
                self.advance();
                Token::Power
            }
            '*' if self.peek() == '=' => {
                self.advance();
                Token::StarAssign
            }
            '*' => Token::Star,
            '/' if self.peek() == '=' => {
                self.advance();
                Token::SlashAssign
            }
            '/' => Token::Slash,
            '%' if self.peek() == '=' => {
                self.advance();
                Token::PercentAssign
            }
            '%' => Token::Percent,
            '^' => Token::Caret,
            '~' => {
                if self.peek() == '=' {
                    self.advance();
                    Token::Similar
                } else {
                    Token::Tilde
                }
            }
            '@' => Token::AtSign,
            '=' => {
                if self.peek() == '=' {
                    self.advance();
                    Token::Eq
                } else if self.peek() == '>' {
                    self.advance();
                    Token::FatArrow
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
                if c == '|' && self.peek() == c {
                    self.advance();
                    Token::Parts
                } else if c == '|' && self.peek() == '>' {
                    let mut count = 0;
                    while self.peek() == '>' {
                        self.advance();
                        count += 1;
                    }
                    Token::Pipe(count)
                } else if self.peek() == c {
                    self.advance();
                    return invalid(
                        loc,
                        format!("unexpected '{}{}'; Vela writes '{}'", c, c, word),
                    );
                } else if c == '|' {
                    return invalid(
                        loc,
                        format!("unexpected character '{}'; Vela writes '{}'", c, word),
                    );
                } else {
                    Token::Amp
                }
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
                } else if self.peek() == '>' {
                    self.advance();
                    Token::NotSimilar
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
            _ => return invalid(loc, format!("unexpected character '{}'", c.escape_debug())),
        };
        LexKind::Token(tok)
    }

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
        // Every keyword is 2-8 lowercase ASCII letters.
        let could_be_keyword =
            (2..=8).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_lowercase());
        let token = KEYWORDS
            .iter()
            .filter(|_| could_be_keyword)
            .find(|(word, _)| *word == s)
            .map(|(_, token)| token.clone())
            .unwrap_or(Token::Ident(s));
        LexKind::Token(token)
    }

    fn number(&mut self, loc: Span) -> LexKind {
        let mut s = String::new();
        let mut raw = String::new();
        let digits = |lexer: &mut Self, s: &mut String, raw: &mut String| {
            while lexer.peek().is_ascii_digit() {
                let c = lexer.advance();
                s.push(c);
                raw.push(c);
                if lexer.peek() == '_' && lexer.peek2().is_ascii_digit() {
                    raw.push(lexer.advance());
                }
            }
        };
        // A tuple's item right after a dot, as in `t.1.0`, is a whole number:
        // its own `.` starts the next item. (After `..`, a range's end may be a float.)
        let before = |back: usize| {
            self.pos
                .checked_sub(back)
                .and_then(|at| self.source.get(at))
                .copied()
        };
        // So is one after `.#`, as in `t.#1`.
        let safe = before(1) == Some('#') && before(2) == Some('.') && before(3) != Some('.');
        let item = (before(1) == Some('.') && before(2) != Some('.')) || safe;
        if !item
            && self.peek() == '0'
            && self.peek2().is_ascii_alphabetic()
            && based(self.peek2().to_ascii_lowercase()).is_some()
        {
            return self.based_number(loc);
        }
        digits(self, &mut s, &mut raw);
        if item && !(self.peek().is_ascii_alphabetic() || self.peek() == '_') {
            // Reject leading zeros and underscores on tuple indices (`t.01`, `t.1_0`).
            if raw != s || (s.len() > 1 && s.starts_with('0')) {
                let plain = s.trim_start_matches('0');
                let plain = if plain.is_empty() { "0" } else { plain };
                let dot = if safe { ".#" } else { "." };
                return invalid(
                    loc,
                    format!(
                        "a tuple's item is written as a plain number, as in '{0}{1}', not '{0}{2}'",
                        dot, plain, raw
                    ),
                );
            }
            return match s.parse::<i64>() {
                Ok(val) => LexKind::Token(Token::Int(val)),
                Err(_) => invalid(loc, format!("integer literal '{}' is too large", raw)),
            };
        }
        let is_float = self.peek() == '.' && self.peek2().is_ascii_digit();
        // `1.` is not a float (and `1..5` is a range).
        if self.peek() == '.'
            && !self.peek2().is_ascii_digit()
            && self.peek2() != '.'
            && !self.peek2().is_ascii_alphabetic()
        {
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

    fn based_number(&mut self, loc: Span) -> LexKind {
        let mut raw = String::new();
        raw.push(self.advance());
        let letter = self.advance();
        raw.push(letter);
        while self.peek().is_ascii_alphanumeric() || self.peek() == '_' {
            raw.push(self.advance());
        }
        if self.peek() == '.' && self.peek2().is_ascii_digit() {
            raw.push(self.advance());
            while self.peek().is_ascii_alphanumeric() || self.peek() == '_' {
                raw.push(self.advance());
            }
            return invalid(
                loc,
                format!(
                    "invalid number '{}': a number written in another base is whole",
                    raw
                ),
            );
        }
        match read_based(&raw) {
            Ok(value) => LexKind::Token(Token::Int(value)),
            Err(message) => invalid(loc, message),
        }
    }

    /// Stops unterminated strings at end-of-line so following lines recover cleanly.
    fn string(&mut self, loc: Span) -> LexKind {
        self.advance();
        if self.nesting == 0 {
            self.too_deep = false;
        }
        let mut s = String::new();
        let mut pieces = Vec::new();
        let mut error = None;
        let mut line_end = None;
        while !self.at_end() {
            match self.peek() {
                '"' => {
                    self.advance();
                    return match error {
                        Some(error) => LexKind::Invalid(error),
                        None if pieces.is_empty() => LexKind::Token(Token::Text(s)),
                        None => {
                            if !s.is_empty() {
                                pieces.push(Piece::Text(s));
                            }
                            LexKind::Token(Token::Interpolated(pieces))
                        }
                    };
                }
                '{' if self.nesting >= MAX_TEXT_NESTING => {
                    self.too_deep = true;
                    error.get_or_insert(Error::at(
                        self.location(),
                        "text holds values nested too deeply",
                    ));
                    while !self.at_end() && !self.at_line_end() {
                        self.advance();
                    }
                }
                '{' => {
                    if !s.is_empty() {
                        pieces.push(Piece::Text(std::mem::take(&mut s)));
                    }
                    self.nesting += 1;
                    let value = self.value(&mut error);
                    self.nesting -= 1;
                    match value {
                        Some(value) => pieces.push(value),
                        None => break,
                    }
                }
                '}' => {
                    let at = self.location();
                    self.advance();
                    error.get_or_insert(Error::at(at, "a '}' in text closes a value opened with '{'; to write a brace, write '\\}'"));
                }
                '\\' => {
                    let escape_loc = self.location();
                    self.advance();
                    if self.at_end() {
                        break;
                    }
                    if self.at_line_end() && self.peek() == '\r' {
                        self.advance();
                    }
                    if self.peek() == '\n' {
                        line_end.get_or_insert(self.mark());
                    }
                    let c = self.advance();
                    match ESCAPES.iter().find(|(escape, _)| *escape == c) {
                        Some((_, value)) => s.push(*value),
                        None if error.is_some() => {}
                        None if c == '\n' => {
                            error = Some(Error::at(
                                escape_loc,
                                "invalid escape: '\\' at the end of a line",
                            ));
                        }
                        None if c.is_control() => {
                            let what = match c {
                                '\t' => "a tab",
                                '\0' => "a NUL character",
                                _ => "a control character",
                            };
                            error = Some(Error::at(
                                escape_loc,
                                format!("invalid escape: '\\' followed by {}", what),
                            ));
                        }
                        None => {
                            error = Some(Error::at(
                                escape_loc,
                                format!("invalid escape '\\{}'", c.escape_debug()),
                            ));
                        }
                    }
                }
                c if c == '\n' || self.at_line_end() => break,
                _ => s.push(self.advance()),
            }
        }
        if let Some(mark) = line_end {
            self.reset(mark);
        }
        LexKind::Invalid(error.unwrap_or_else(|| Error::at(loc, "text isn't closed on its line; end it with '\"' (a line break in text is written '\\n')")))
    }

    fn char_literal(&mut self, loc: Span) -> LexKind {
        self.advance();
        let unclosed = || {
            invalid(
                loc,
                "a char isn't closed on its line; end it with a quote, as in 'a'",
            )
        };
        if self.at_end() || self.at_line_end() {
            return unclosed();
        }
        let value = match self.peek() {
            '\'' => {
                self.advance();
                return invalid(
                    loc,
                    "'' holds no char; write one between the quotes, as in 'a', or '\\'' for a quote",
                );
            }
            '\\' => {
                let escape_loc = self.location();
                self.advance();
                if self.at_end() || self.at_line_end() {
                    return unclosed();
                }
                let c = self.advance();
                match ESCAPES
                    .iter()
                    .find(|(escape, _)| *escape == c)
                    .map(|(_, value)| *value)
                    .or((c == '\'').then_some('\''))
                {
                    Some(value) => value,
                    None => {
                        self.skip_char_rest();
                        return invalid(
                            escape_loc,
                            format!("invalid escape '\\{}' in a char", c.escape_debug()),
                        );
                    }
                }
            }
            _ => self.advance(),
        };
        if !value.is_ascii() {
            self.skip_char_rest();
            return invalid(
                loc,
                format!(
                    "a char is one byte, and '{}' is {} bytes in UTF-8; write it as text, as in \"{}\"",
                    value,
                    value.len_utf8(),
                    value
                ),
            );
        }
        if self.peek() != '\'' {
            let closed = self.skip_char_rest();
            if !closed {
                return unclosed();
            }
            return invalid(
                loc,
                "a char holds one character; for more, write text between double quotes, as in \"ab\"",
            );
        }
        self.advance();
        LexKind::Token(Token::Char(value as u8))
    }

    fn skip_char_rest(&mut self) -> bool {
        while !self.at_end() && !self.at_line_end() {
            if self.advance() == '\'' {
                return true;
            }
        }
        false
    }

    fn value(&mut self, error: &mut Option<Error>) -> Option<Piece> {
        let open = self.next_item();
        let mut tokens = Vec::new();
        let mut depth = 0usize;
        // An error inside a value the line ends in is most likely from the
        // missing `}` (`"n = {n" at r` reads `" at r)` as text), so that is
        // reported instead.
        let mut erred_before = error.is_some();
        loop {
            self.skip_whitespace();
            if self.at_end() || self.at_line_end() {
                if !erred_before && !self.too_deep {
                    *error = None;
                }
                error.get_or_insert(Error::at(
                    open.start,
                    "a '{' in text opens a value, which ends with '}' on the same line; to write a brace, write '\\{'",
                ));
                return None;
            }
            // `"{\"a\"}"`: the `\"` ends the text, so the `}` is missed too;
            // the `\` is the mistake to report.
            if self.peek() == '\\' && error.is_none() {
                *error = Some(Error::at(
                    self.location(),
                    "a value in text is code, so text inside it is written with plain quotes, as in '{f(\"a\")}', not '\\\"'",
                ));
                erred_before = true;
            }
            if self.peek() == ';' {
                error.get_or_insert(Error::at(
                    self.location(),
                    "a value in text can't hold a comment",
                ));
                self.advance();
                continue;
            }
            let token = self.next_item();
            match &token.kind {
                LexKind::Token(Token::LBrace) => depth += 1,
                LexKind::Token(Token::RBrace) if depth == 0 => {
                    if tokens.is_empty() {
                        error.get_or_insert(Error::at(
                            open.start,
                            "'{}' in text holds no value; put one inside, as in '{n}', or write a brace as '\\{'",
                        ));
                    }
                    return Some(Piece::Value {
                        open: Box::new(open),
                        tokens,
                        close: Box::new(token),
                    });
                }
                LexKind::Token(Token::RBrace) => depth -= 1,
                LexKind::Invalid(invalid) => {
                    error.get_or_insert(invalid.clone());
                }
                _ => {}
            }
            tokens.push(token);
        }
    }
}

fn invalid(span: Span, message: impl Into<String>) -> LexKind {
    LexKind::Invalid(Error::at(span, message))
}

/// Names are ASCII so they map directly to portable C identifiers.
fn non_ascii_name(span: Span, c: char) -> Error {
    Error::at(
        span,
        format!(
            "names must use ASCII letters, digits, and '_', found '{}'",
            c
        ),
    )
}
