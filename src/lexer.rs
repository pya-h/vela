use std::ops::Range;

use crate::ast::Span;
use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Int(i64),
    Float(f64),
    /// `'a'`: one byte, as a char.
    Char(u8),
    Text(String),
    /// Text holding values, `"n = {n}"`: its pieces in order.
    Interpolated(Vec<Piece>),
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
    /// `block`: a block of its own, which ends at its `fi` with its names and
    /// regions.
    Block,
    In,
    Match,
    Case,
    New,
    /// `region`: declares a region, or is the type of a region parameter.
    Region,
    And,
    Or,
    Not,
    /// `&`, which joins the words a generic type must allow (`<T: same & shown>`),
    /// marks a place given to `hold&` (`f(&a)`), and makes `hold&` and `bind&`.
    /// Vela writes `and` for both conditions at once.
    Amp,
    Fi,
    Maybe,
    Is,
    /// `fail`, the verb: makes a failure, as in `<- fail e`.
    Fail,
    /// `failure`, the noun: names a failure where one is matched, as in
    /// `case failure e` and a plan B's first line.
    Failure,
    /// `include`: makes another file's tasks, shapes, and kinds usable in this one.
    Include,
    /// `priv`: keeps a task, shape, kind, or shape's field to its own file.
    Priv,
    /// `shared`: a shape's field that is one value for the whole program rather
    /// than part of each value, reached through the shape's name, `X.f`.
    Shared,
    /// `type`: reserved, and read after a dot, `x.type`, the type of a value
    /// as code writes it.
    Type,
    Ident(String),
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Colon,
    /// `#`, which separates an array's type or value from its count, as in
    /// item #4: `[int#4]`, `[0#4]`.
    Hash,
    /// `$`, which marks a decorator before a task: `$d`, `$d(args)`, or the
    /// wrappers `$(w1, w2(a))`.
    Dollar,
    /// `?`, after a startup decorator whose failure doesn't stop the program:
    /// `$route("/")?`.
    Question,
    Comma,
    Dot,
    Arrow,
    /// `=>`, which gives an included module or name another name:
    /// `include shop/cart => basket`.
    FatArrow,
    Range,
    /// `...`: after the last value in brackets, `[1, 2, ...]`, it makes a slice
    /// of the values rather than an array.
    Ellipsis,
    Plus,
    Minus,
    Star,
    /// `**`: a number raised to a power.
    Power,
    /// `++` and `--`, after a place: one more, or one less.
    Increment,
    Decrement,
    /// `+=`, `-=`, `*=`, `/=`, `%=`, and `**=`: a place given the result of its
    /// operator on it and a value.
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
    /// `~`, which separates a type from the error it can be instead:
    /// `int ~ ParseError`.
    Tilde,
    /// `~=`: whether two values are similar, item by item.
    Similar,
    /// `<>`: whether two values aren't similar, `not (a ~= b)`. Where types
    /// are read, it is `< … >` with nothing inside.
    NotSimilar,
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
    ("maybe", Token::Maybe),
    ("is", Token::Is),
    ("fail", Token::Fail),
    ("failure", Token::Failure),
    ("include", Token::Include),
    ("priv", Token::Priv),
    ("shared", Token::Shared),
    ("type", Token::Type),
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
    ('{', '{'),
    ('}', '}'),
];

/// How deeply text may hold values holding text holding values: each level
/// is lexed inside the one around it, so without a limit the lexer's own
/// recursion could overflow the stack (see `parser::MAX_NESTING`).
const MAX_TEXT_NESTING: usize = 32;

/// A piece of text holding values (`Token::Interpolated`).
#[derive(Debug, Clone, PartialEq)]
pub enum Piece {
    /// Text as written, its escapes read.
    Text(String),
    /// A value, `{…}`: its tokens, between its `{` and its `}`.
    Value { open: Box<LexToken>, tokens: Vec<LexToken>, close: Box<LexToken> },
}

impl Token {
    /// Whether the token can be part of a type written inside the `< … >` of the
    /// types a declaration takes or a call gives. Reading a `<` as the start of
    /// those types rather than as a comparison looks for the matching `>`
    /// through these (see `Parser::at_type_args`).
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
                // `Box<(int) -> int>`: a task's type.
                | Token::Arrow
        )
    }

    /// A human-readable description for diagnostics, e.g. `'fi'` or
    /// `identifier 'x'`.
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

/// The bases a whole number may be written in besides ten, by the letter after
/// its `0`: the base, and the digits it has, for messages. `int(text)` reads
/// the same ones (see `vela__rt__read_int`).
fn based(letter: char) -> Option<(u32, &'static str)> {
    match letter {
        'x' => Some((16, "a hex digit (0-9, a-f)")),
        'o' => Some((8, "an octal digit (0-7)")),
        'b' => Some((2, "a binary digit (0 or 1)")),
        _ => None,
    }
}

/// Reads `raw`, a whole number written `0x1F`, `0o17`, or `0b101`, or says
/// what is wrong with it.
fn read_based(raw: &str) -> std::result::Result<i64, String> {
    let wrong = |why: String| Err(format!("invalid number '{}': {}", raw, why));
    let mut chars = raw.chars().skip(1);
    let letter = chars.next().unwrap_or('x');
    let lower = letter.to_ascii_lowercase();
    let Some((base, digit)) = based(lower) else { return wrong("no base".to_string()) };
    if letter != lower {
        return wrong(format!("its base is written in lowercase, as in '0{}{}'", lower, &raw[2..]));
    }
    let written = &raw[2..];
    if written.is_empty() {
        return wrong(format!("'0{}' is followed by its digits, as in '{}'", lower, based_example(lower)));
    }
    if written.starts_with('_') || written.ends_with('_') || written.contains("__") {
        return wrong(format!("an underscore goes between two digits, as in '{}'", based_example(lower)));
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

/// A number written in the base `letter` names, for messages.
fn based_example(letter: char) -> &'static str {
    match letter {
        'x' => "0xff_ff",
        'o' => "0o755",
        _ => "0b1010_0101",
    }
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
    /// A `;` comment, up to (not including) the end of the line.
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
    /// The file the source is, for the positions of its tokens (see `Span::file`).
    file: usize,
    /// How many values in text the lexer is inside (see `MAX_TEXT_NESTING`).
    nesting: usize,
    /// Whether the text being read holds values nested too deeply, the error
    /// reported for it whatever else is wrong.
    too_deep: bool,
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
            file: 0,
            nesting: 0,
            too_deep: false,
        }
    }

    /// A lexer for file number `file` of a program, whose positions say so.
    pub fn for_file(source: &str, file: usize) -> Self {
        Lexer { file, ..Lexer::new(source) }
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

    /// Whether the next character is a `\r` that isn't part of a CRLF line
    /// end, as old Mac files end lines: an error, since the file would read as
    /// one line.
    fn lone_cr(&self) -> bool {
        self.peek() == '\r' && self.peek2() != '\n'
    }

    fn skip_whitespace(&mut self) {
        // A `\r` is only the start of a CRLF line end (see `lone_cr`).
        while matches!(self.peek(), ' ' | '\t') || (self.peek() == '\r' && self.peek2() == '\n') {
            self.advance();
        }
    }

    fn location(&self) -> Span {
        Span { line: self.line, col: self.col, file: self.file }
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

        // `;` starts a comment, as it adds a remark to a sentence.
        if c == ';' {
            while !self.at_line_end() && !self.at_end() && !self.lone_cr() {
                self.advance();
            }
            return LexKind::Comment;
        }

        if self.lone_cr() {
            self.advance();
            return invalid(loc, "a line ends with a line feed (LF, as on Linux and macOS) or CR LF (as on Windows), not a carriage return (CR) alone; save the file with LF or CRLF line ends");
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
            // `x++` and `x += 1`, statements of their own (see `ast::StmtKind::Step`).
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
                if self.peek() == c {
                    self.advance();
                    return invalid(loc, format!("unexpected '{}{}'; Vela writes '{}'", c, c, word));
                }
                if c == '|' {
                    return invalid(loc, format!("unexpected character '{}'; Vela writes '{}'", c, word));
                }
                // A single `&` joins the words a generic type must allow; the
                // parser reports it as 'and' anywhere else.
                Token::Amp
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
        // Every keyword is two to seven lowercase letters, so most names are
        // told apart without comparing them to each.
        let could_be_keyword = (2..=7).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_lowercase());
        let token = KEYWORDS
            .iter()
            .filter(|_| could_be_keyword)
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
        // A tuple's item right after a dot, as in `t.1.0`, is a whole number:
        // its own `.` starts the next item. (After `..`, a range's end may be a float.)
        let before = |back: usize| self.pos.checked_sub(back).and_then(|at| self.source.get(at)).copied();
        let item = before(1) == Some('.') && before(2) != Some('.');
        if !item && self.peek() == '0' && self.peek2().is_ascii_alphabetic() && based(self.peek2().to_ascii_lowercase()).is_some() {
            return self.based_number(loc);
        }
        digits(self, &mut s, &mut raw);
        if item && !(self.peek().is_ascii_alphabetic() || self.peek() == '_') {
            // `t.01` and `t.1_0` would read as `t.1` and `t.10`.
            if raw != s || (s.len() > 1 && s.starts_with('0')) {
                let plain = s.trim_start_matches('0');
                let plain = if plain.is_empty() { "0" } else { plain };
                return invalid(loc, format!("a tuple's item is written as a plain number, as in '.{}', not '.{}'", plain, raw));
            }
            return match s.parse::<i64>() {
                Ok(val) => LexKind::Token(Token::Int(val)),
                Err(_) => invalid(loc, format!("integer literal '{}' is too large", raw)),
            };
        }
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

    /// A whole number written in another base, `0x1F`, `0o17`, or `0b101`
    /// (see `based`), with underscores between its digits as a decimal one has.
    fn based_number(&mut self, loc: Span) -> LexKind {
        let mut raw = String::new();
        raw.push(self.advance());
        let letter = self.advance();
        raw.push(letter);
        while self.peek().is_ascii_alphanumeric() || self.peek() == '_' {
            raw.push(self.advance());
        }
        // `0x1F.5`: a number in another base is whole.
        if self.peek() == '.' && self.peek2().is_ascii_digit() {
            raw.push(self.advance());
            while self.peek().is_ascii_alphanumeric() || self.peek() == '_' {
                raw.push(self.advance());
            }
            return invalid(loc, format!("invalid number '{}': a number written in another base is whole", raw));
        }
        match read_based(&raw) {
            Ok(value) => LexKind::Token(Token::Int(value)),
            Err(message) => invalid(loc, message),
        }
    }

    /// A text literal, which may hold values (`"n = {n}"`). An invalid one is
    /// still read up to its closing quote, reporting its first error. One whose
    /// line ends before its closing quote is an error; as a token it ends with
    /// its line, so the lines after it are lexed normally.
    fn string(&mut self, loc: Span) -> LexKind {
        self.advance(); // opening "
        if self.nesting == 0 {
            self.too_deep = false;
        }
        let mut s = String::new();
        let mut pieces = Vec::new();
        let mut error = None;
        // Where the literal's first line ends, in case it is unterminated.
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
                    error.get_or_insert(Error::at(self.location(), "text holds values nested too deeply"));
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
                        None => break, // the line ended inside the value
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

    /// A char literal, `'a'`: one byte, written as itself or as an escape of
    /// text (`'\n'`), or `'\''` for the quote. It ends on its line, so a missing
    /// quote is reported there, as for text.
    fn char_literal(&mut self, loc: Span) -> LexKind {
        self.advance(); // opening '
        let unclosed = || invalid(loc, "a char isn't closed on its line; end it with a quote, as in 'a'");
        if self.at_end() || self.at_line_end() {
            return unclosed();
        }
        let value = match self.peek() {
            '\'' => {
                self.advance();
                return invalid(loc, "'' holds no char; write one between the quotes, as in 'a', or '\\'' for a quote");
            }
            '\\' => {
                let escape_loc = self.location();
                self.advance();
                if self.at_end() || self.at_line_end() {
                    return unclosed();
                }
                let c = self.advance();
                match ESCAPES.iter().find(|(escape, _)| *escape == c).map(|(_, value)| *value).or((c == '\'').then_some('\'')) {
                    Some(value) => value,
                    None => {
                        self.skip_char_rest();
                        return invalid(escape_loc, format!("invalid escape '\\{}' in a char", c.escape_debug()));
                    }
                }
            }
            _ => self.advance(),
        };
        if !value.is_ascii() {
            self.skip_char_rest();
            return invalid(
                loc,
                format!("a char is one byte, and '{}' is {} bytes in UTF-8; write it as text, as in \"{}\"", value, value.len_utf8(), value),
            );
        }
        if self.peek() != '\'' {
            let closed = self.skip_char_rest();
            if !closed {
                return unclosed();
            }
            return invalid(loc, "a char holds one character; for more, write text between double quotes, as in \"ab\"");
        }
        self.advance(); // closing '
        LexKind::Token(Token::Char(value as u8))
    }

    /// Skips the rest of a char literal that went wrong, up to its closing
    /// quote on the same line: whether there was one.
    fn skip_char_rest(&mut self) -> bool {
        while !self.at_end() && !self.at_line_end() {
            if self.advance() == '\'' {
                return true;
            }
        }
        false
    }

    /// A value in text, `{…}`, with the cursor at its `{`: its tokens, up to the
    /// `}` that closes it (the braces of a plan B inside it pair up). Records
    /// its first error in `error`, and returns `None` when the line ends first.
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
                *error = Some(Error::at(self.location(), "a value in text is code, so text inside it is written with plain quotes, as in '{f(\"a\")}', not '\\\"'"));
                erred_before = true;
            }
            if self.peek() == ';' {
                error.get_or_insert(Error::at(self.location(), "a value in text can't hold a comment"));
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
                    return Some(Piece::Value { open: Box::new(open), tokens, close: Box::new(token) });
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
    Error::at(span, format!("names must use ASCII letters, digits, and '_', found '{}'", c))
}
