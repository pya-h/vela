use crate::ast::*;
use crate::error::{Error, Result};
use crate::lexer::{KEYWORDS, LexKind, LexToken, Piece, Token};

/// A loop's start, condition and step, each the first and the one after the
/// last position of its items (see `Parser::loop_parts`).
type LoopParts = Vec<Vec<(usize, usize)>>;

/// The error for `||` where it parts no loop's parts.
const PARTS_AS_OR: &str = "unexpected '||'; Vela writes 'or'";
/// The mistake of a macro written inside a task, shape, or kind.
const MISPLACED_MACRO: &str = "a macro is written outside every task, shape, and kind, as in '#P = 3.14159' at the start of a line";

/// A type alias inside a block, which names a type for the whole file.
const MISPLACED_TYPE_ALIAS: &str = "a type alias is written outside every task, shape, and kind, as in 'alias SampleTask => (int) -> int' at the start of a line";

const MISPLACED_PATTERN: &str = "a pattern is written outside every task, shape, and kind, as in 'pattern Shaped' at the start of a line, and closes with 'fi'";

/// How deeply code may nest: parentheses, operators in a row, unary
/// operators, `^` in types, and blocks inside blocks each add a level. Every
/// compiler stage walks the tree recursively, so without a limit deeply nested
/// input would overflow the stack and crash (taking Vela Studio's server or
/// its WebAssembly module with it) instead of reporting an error.
pub const MAX_NESTING: usize = 100;

/// What a tuple type needs, for the `(` of one that has too few items. One type
/// in parentheses is that type, so only `()` and `(T,)` are reported.
const TUPLE_TYPE_ITEMS: &str = "a tuple holds two or more items, as in '(int, text)'";

/// The same for a tuple of values, and what both say about `(1, 2,)`.
const TUPLE_ITEMS: &str = "a tuple holds two or more items, as in '(1, \"Ada\")'";
const TUPLE_TRAILING_COMMA: &str = "a tuple has no comma after its last item";

/// `fail` written where a failure is matched and named. The verb makes a
/// failure; the noun names one, so every program written before this says
/// `fail` in a pattern.
const FAILURE_NAMES: &str = "a case form names a failure with 'failure', as in 'failure e'; 'fail e' makes one";

/// `fail` on the first line of a plan B, where the failure it handles is named.
const PLAN_B_NAMES: &str =
    "a plan B names the failure it handles with 'failure', as in 'failure e'; to send a failure instead, write '<- fail e'";

/// `failure` written where a value goes: the mirror of `FAILURE_NAMES`.
const FAIL_MAKES: &str = "'failure' names a failure in a case form; to make one, write 'fail', as in '<- fail e'";

/// A second region after one already written: `f(x)@r at r2`, or `a + b at r at r2`.
const REGION_ALREADY_NAMED: &str =
    "the region is already named before this one; a part names a region of its own with '@', as in 'x.str()@r', or in parentheses, as in '(x.str() at r)'";

/// What `place_region` did with a region written after a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placing {
    Placed,
    /// The call already has its region written after it.
    Already,
    /// Nothing there takes a region written after it.
    Nowhere,
}

/// Gives the region written after `expr`, `f(x) at r` or `f(x)@r`, to the call
/// `expr` is: a task's or a method's, whose last argument it becomes (see
/// `ExprKind::RegionAfter`), or a `scan` or a copy, whose region it is. With
/// `through_prefix`, for `at r` over the whole of what comes before it, a
/// `fail`, `-`, or `not` in front of the call is looked through, since none of
/// them makes memory; `@r` belongs to the call right before it and looks
/// through nothing. Which calls take a region is the checker's to say.
fn place_region(expr: &mut Expr, region: &str, span: Span, through_prefix: bool) -> Placing {
    // `m.keys at r`: a map's keys made into a slice in `r` (the checker says
    // so when the value is no map).
    if let ExprKind::Field(_, word) = &expr.kind
        && let Some(op @ (MapOp::Keys | MapOp::Values)) = MapOp::from_word(word)
    {
        let taken = std::mem::replace(&mut expr.kind, ExprKind::NoneLit);
        if let ExprKind::Field(map, _) = taken {
            expr.kind = ExprKind::MapOp { map, op, key: None, region: Some(region.to_string()), region_span: span };
        }
        return Placing::Placed;
    }
    match &mut expr.kind {
        ExprKind::MapOp { region: Some(_), .. } => Placing::Already,
        ExprKind::Call { args, .. } | ExprKind::MethodCall { args, .. } => {
            if matches!(args.last(), Some(Expr { kind: ExprKind::RegionAfter(_), .. })) {
                return Placing::Already;
            }
            args.push(Expr::new(ExprKind::RegionAfter(region.to_string()), span));
            Placing::Placed
        }
        ExprKind::Scan { region: written, region_span, .. } | ExprKind::Copy { region: written, region_span, .. } => {
            if written.is_some() {
                return Placing::Already;
            }
            *written = Some(region.to_string());
            *region_span = span;
            Placing::Placed
        }
        ExprKind::Unary(_, inner) | ExprKind::Fail(inner) if through_prefix => place_region(inner, region, span, false),
        _ => Placing::Nowhere,
    }
}

/// Whether `@r` right after `expr` names a region `place_region` can give it.
fn takes_region_after(expr: &Expr) -> bool {
    matches!(expr.kind, ExprKind::Call { .. } | ExprKind::MethodCall { .. } | ExprKind::Scan { .. } | ExprKind::Copy { .. })
        || matches!(&expr.kind, ExprKind::Field(_, word) if matches!(MapOp::from_word(word), Some(MapOp::Keys | MapOp::Values)))
}

/// Where a `name: type` is written, which settles what may follow the type: a
/// shape's field takes `= value`, a task's parameter takes `default` on a region
/// it is given, and a variant's value takes neither (see `parse_field`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Field,
    Param,
    Value,
}

pub struct Parser {
    tokens: Vec<Token>,
    spans: Vec<Span>,
    /// Where each token ends, just after its last character.
    ends: Vec<Span>,
    pos: usize,
    /// Current nesting level; see `MAX_NESTING`.
    depth: usize,
    /// The operators open around the one being read, of every chain: `**`
    /// chains and ones in parentheses go deeper by recursing, not by a loop.
    operators: usize,
    /// The parts of the written types, for errors about them (see `TypePlaces`).
    type_places: TypePlaces,
    /// The parts of the type being parsed.
    type_parts: Vec<(String, Span)>,
    /// The file's `include` lines, which come before its items.
    includes: Vec<Include>,
    /// The file's type aliases, which name types and are no items: the
    /// resolver puts their types where their names are written.
    type_aliases: Vec<AliasDef>,
    /// The file's patterns, which are no items: the checker reads them, and
    /// the resolver gives each shape and kind following one its defaults.
    patterns: Vec<PatternDef>,
    /// Whether the header being read is a pattern's, where a `,` after its
    /// results may start the next header (see `parse_results`).
    in_pattern: bool,
    /// The methods of the shape or kind just read, each a task of its own (see
    /// `method_name`), which the file's items take in after it.
    methods: Vec<Item>,
    /// The lambdas of the item just read, each a task of its own (see
    /// `ast::lambda_name`), which the file's items take in after it.
    lambdas: Vec<Item>,
    /// How many lambdas the file has so far, which numbers the next one.
    lambda_count: usize,
    /// Whether the code being read is a generic task's or a generic shape's or
    /// kind's method, where no lambda is written yet.
    in_generic: bool,
    /// The position of a `<-` that may send without starting a line: the first
    /// token of a lambda's body written on the line of its header.
    send_after_header: Option<usize>,
    /// Whether a loop that gives a value was read, which needs `std/loops`.
    loop_values: bool,
}

/// An `is` joined with `or`, or after one.
const IS_WITH_OR: &str = "an 'is' is joined with 'and', not 'or': with 'or', the names it binds would be bound on one path only; put the 'or' in parentheses, as in 'm is {x} and (a or b)', or write a 'when' inside";

impl Parser {
    /// `tokens`, `spans` and `ends` are parallel, as produced by
    /// `Lexer::tokenize`.
    pub fn new(tokens: Vec<Token>, spans: Vec<Span>, ends: Vec<Span>) -> Self {
        Parser { tokens, spans, ends, pos: 0, depth: 0, operators: 0, type_places: TypePlaces::new(), type_parts: Vec::new(), includes: Vec::new(), type_aliases: Vec::new(), patterns: Vec::new(), in_pattern: false, methods: Vec::new(), lambdas: Vec::new(), lambda_count: 0, in_generic: false, send_after_header: None, loop_values: false }
    }

    /// Whether the file has a loop that gives a value (see `ValueLoop`).
    pub fn loop_values(&self) -> bool {
        self.loop_values
    }

    /// The `include` lines of the file parsed.
    pub fn includes(&mut self) -> Vec<Include> {
        std::mem::take(&mut self.includes)
    }

    /// The type aliases of the file parsed.
    pub fn type_aliases(&mut self) -> Vec<AliasDef> {
        std::mem::take(&mut self.type_aliases)
    }

    /// The patterns of the file parsed.
    pub fn patterns(&mut self) -> Vec<PatternDef> {
        std::mem::take(&mut self.patterns)
    }

    /// Where the parts of every type parsed so far are.
    pub fn type_places(&mut self) -> TypePlaces {
        std::mem::take(&mut self.type_places)
    }

    /// Parses a whole file: its `include` lines (see `includes`), then its items.
    pub fn parse_file(&mut self) -> Result<Vec<Item>> {
        let tokens: Vec<(&Token, Span, Span)> = self.tokens.iter().zip(&self.spans).zip(&self.ends).map(|((token, start), end)| (token, *start, *end)).collect();
        if let Some(error) = reserved_word(&tokens) {
            return Err(error);
        }
        let mut items = Vec::new();
        self.skip_newlines();
        while self.check(Token::Include) {
            let included = self.parse_include()?;
            self.includes.push(included);
            self.skip_newlines();
        }
        while !self.is_at_end() {
            if self.check(Token::Include) {
                return Err(Error::at(self.peek_span(), "'include' lines go at the top of the file, before any task, shape, or kind"));
            }
            if self.at_type_alias_after(0) || (self.check(Token::Priv) && self.at_type_alias_after(1)) {
                let alias = self.parse_type_alias()?;
                self.type_aliases.push(alias);
                self.skip_newlines();
                continue;
            }
            if self.at_pattern_after(0) || (self.check(Token::Priv) && self.at_pattern_after(1)) {
                let pattern = self.parse_pattern_def()?;
                self.patterns.push(pattern);
                self.skip_newlines();
                continue;
            }
            items.push(self.parse_item()?);
            // The methods of a shape or a kind are tasks of the file, after it.
            items.append(&mut self.methods);
            // So are its lambdas.
            items.append(&mut self.lambdas);
            self.skip_newlines();
        }
        Ok(items)
    }

    /// `include path`, `include path => alias`, `include path (part, …)`, or
    /// `include path {part, …}`, where the path is names and `..` separated by
    /// `/`, and a part is a name, maybe followed by `=> alias`.
    fn parse_include(&mut self) -> Result<Include> {
        self.advance(); // include
        let span = self.peek_span();
        if matches!(self.peek(), Token::Newline | Token::Eof) {
            return Err(Error::at(span, "expected a module after 'include', as in 'include geometry'"));
        }
        if let Token::Text(text) = self.peek() {
            let written = text.strip_suffix(".vela").unwrap_or(text).to_string();
            return Err(Error::at(span, format!("a module is named without quotes or '.vela', as in 'include {}'", written)));
        }
        let mut path = Vec::new();
        let name_span = loop {
            let part_span = self.peek_span();
            if self.match_token(Token::Range) {
                path.push("..".to_string());
                if !self.match_token(Token::Slash) {
                    return Err(Error::at(self.peek_span(), "a module path goes on after '..' with '/', as in 'include ../geometry'"));
                }
                continue;
            }
            path.push(self.expect_ident()?);
            if self.check(Token::Dot) {
                if matches!(self.tokens.get(self.pos + 1), Some(Token::Ident(word)) if word == "vela") {
                    return Err(Error::at(self.peek_span(), format!("a module is named without '.vela', as in 'include {}'", path.join("/"))));
                }
                return Err(Error::at(self.peek_span(), "a module path separates its folders with '/', as in 'include shapes/circle'"));
            }
            if !self.match_token(Token::Slash) {
                break part_span;
            }
        };
        let alias = self.parse_alias()?;
        let mut parts = Vec::new();
        let only_parts = self.check(Token::LBrace);
        if only_parts && let Some((_, at)) = &alias {
            return Err(Error::at(
                *at,
                "a module included with only its parts ('{…}') has no name in this file, so it can't be given one with '=>'",
            ));
        }
        if self.match_token(Token::LParen) || self.match_token(Token::LBrace) {
            let close = if only_parts { Token::RBrace } else { Token::RParen };
            loop {
                let at = self.peek_span();
                let name = self.expect_ident()?;
                parts.push(Part { name, span: at, alias: self.parse_alias()? });
                if !self.match_token(Token::Comma) {
                    break;
                }
            }
            self.expect(close)?;
        }
        if !matches!(self.peek(), Token::Newline | Token::Eof) {
            return Err(self.unexpected("the end of the 'include' line"));
        }
        Ok(Include { path, span, name_span, alias, parts, only_parts })
    }

    /// `=> name` after an included module or part, if it is there.
    fn parse_alias(&mut self) -> Result<Option<Binding>> {
        if !self.match_token(Token::FatArrow) {
            return Ok(None);
        }
        let at = self.peek_span();
        Ok(Some((self.expect_ident()?, at)))
    }

    /// Whether `name` is the name of a module this file includes.
    fn is_module(&self, name: &str) -> bool {
        self.includes.iter().any(|included| included.local_name() == Some(name))
    }

    /// After a name at the cursor's previous token: `.member` when the name is a
    /// module this file includes, giving `module.member`; otherwise the name alone.
    fn qualified(&mut self, name: String) -> Result<String> {
        if !self.is_module(&name) || !self.check(Token::Dot) {
            return Ok(name);
        }
        self.advance(); // .
        let member = self.expect_ident()?;
        Ok(format!("{}{}{}", name, MODULE_SEPARATOR, member))
    }

    /// `qualified` for a name that starts a value. A name, a dot, and a call is
    /// a kind's variant (`Shape.Circle(…)`, or `geometry.Shape.Circle(…)`) or a
    /// method called on a value (`p.area(…)`), which look the same until the
    /// file's names are known; `parse_suffix` reads the dot and the call, and
    /// keeps both readings for the resolver (see `ExprKind::MethodCall`).
    fn value_name(&mut self, name: String) -> Result<String> {
        self.qualified(name)
    }

    fn peek(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or(&Token::Eof)
    }

    fn peek_span(&self) -> Span {
        self.spans
            .get(self.pos)
            .or(self.spans.last())
            .copied()
            .unwrap_or_default()
    }

    /// Moves past the current token. What it was is read with `peek` first,
    /// so nothing is copied for a token only stepped over.
    fn advance(&mut self) {
        if !self.is_at_end() {
            self.pos += 1;
        }
    }

    fn is_at_end(&self) -> bool {
        self.pos >= self.tokens.len() || self.peek() == &Token::Eof
    }

    fn skip_newlines(&mut self) {
        while self.peek() == &Token::Newline {
            self.advance();
        }
    }

    /// Enters one nesting level, failing past `MAX_NESTING`. Pair with `leave`.
    fn enter(&mut self) -> Result<()> {
        self.depth += 1;
        if self.depth > MAX_NESTING {
            return Err(Error::at(
                self.peek_span(),
                format!("code is nested too deeply (more than {} levels)", MAX_NESTING),
            ));
        }
        Ok(())
    }

    /// `enter`, for one more operator: a long chain is told how to split,
    /// since each operator deepens the tree. Pair with `leave_operators`.
    fn enter_operator(&mut self) -> Result<()> {
        let at = self.peek_span();
        let operators = self.operators;
        self.enter().map_err(|error| match operators > MAX_NESTING / 2 {
            true => Error::at(
                at,
                format!(
                    "this chain of operators has more than {} terms, and each operator counts as a level of nesting, at most {} in all; split it with a name for part of it, as in 'fixed part = a + b + …' and then 'part + …'",
                    operators + 1,
                    MAX_NESTING
                ),
            ),
            false => error,
        })?;
        self.operators += 1;
        Ok(())
    }

    fn leave_operators(&mut self, levels: usize) {
        self.leave(levels);
        self.operators = self.operators.saturating_sub(levels);
    }

    fn leave(&mut self, levels: usize) {
        self.depth = self.depth.saturating_sub(levels);
    }

    fn unexpected(&self, expected: &str) -> Error {
        if self.check(Token::Send) && !self.at_line_start() {
            return self.misplaced_send();
        }
        if self.check(Token::Is) {
            return Error::at(
                self.peek_span(),
                "'is' tests a condition of a 'when' or 'loop', on its own or joined with 'and' outside any parentheses, as in 'when x is {value}' or 'when x is {value} and value > 0'",
            );
        }
        let previous = self.pos.checked_sub(1).and_then(|p| self.tokens.get(p));
        let next = self.tokens.get(self.pos + 1);
        // A `#` starting a line outside every block, where a macro goes.
        let macro_line = match previous {
            None | Some(Token::Newline) => self.peek_span().col == 1,
            Some(Token::Priv) => true,
            _ => false,
        };
        let hint = match (self.peek(), previous, next) {
            (Token::Assign, Some(op @ (Token::Plus | Token::Minus | Token::Star | Token::Power | Token::Slash | Token::Percent)), _) => {
                let op = op.describe();
                let op = op.trim_matches('\'');
                format!("; '{0}=' is one symbol, written without a space, as in 'x {0}= 2'", op)
            }
            (Token::Assign, Some(Token::Eq), _) => "; to compare, write '=='".to_string(),
            (Token::Assign, Some(Token::Range), _) => "; Vela has no '..=': a range leaves its end out, so '0..4' goes from 0 to 3".to_string(),
            (Token::Assign, _, _) if expected == "an expression" => "; '=' assigns, as a statement of its own".to_string(),
            // `case Skip = break`: a jump is a statement, which a block holds.
            (Token::Break | Token::Continue, _, _) if expected == "an expression" => {
                format!("; {} is a statement, not a value, so it goes in a block, as on the line after a 'case', or in a plan B: '{{ {} }}'", self.peek().describe(), self.peek().describe().trim_matches('\''))
            }
            // `0...3` and `0..=3`: a range has two dots and leaves its end out.
            (Token::Ellipsis, _, _) => "; a range is written with two dots, as in '0..3', which leaves 3 out, and '...' ends the values of a slice, as in '[1, 2, ...] at r'".to_string(),
            // `.05`: the digits after the dot lost their leading zeros as a number.
            (Token::Dot, _, Some(Token::Int(_))) => "; a number starts with a digit, so write '0' before the '.'".to_string(),
            (Token::LBrace, Some(Token::Newline) | None, _) => "; a plan B's '{' goes on the line of its value, and blocks close with 'fi'".to_string(),
            // `show(x++)` or `++x`: a step is a statement of its own.
            (Token::Increment | Token::Decrement, _, _) => {
                let symbol = self.peek().describe();
                format!("; {0} goes after a place, on a line of its own, as in 'x{1}', and gives no value", symbol, symbol.trim_matches('\''))
            }
            (Token::PlusAssign | Token::MinusAssign | Token::StarAssign | Token::SlashAssign | Token::PercentAssign | Token::PowerAssign, _, _) => {
                let symbol = self.peek().describe();
                format!("; {} changes a place, on a line of its own, as in 'x {} 2', and gives no value", symbol, symbol.trim_matches('\''))
            }
            // A comment written as other languages write one.
            // `#task = 1`, `#MAX(a, b) = …`, and `#define N 4` at a line's start:
            // a macro written as it can't be.
            (Token::Hash, _, Some(next)) if macro_line && !matches!(next, Token::Ident(_)) && next.describe().trim_matches('\'').bytes().all(|b| b.is_ascii_lowercase()) => {
                format!("; a macro is named by a name of its own, and {} is one of Vela's words", next.describe())
            }
            (Token::Hash, _, Some(Token::Ident(_))) if macro_line && self.tokens.get(self.pos + 2) == Some(&Token::LParen) => {
                "; a macro is a value, '#NAME = value', and macros that take values are not in Vela yet".to_string()
            }
            (Token::Hash, _, Some(Token::Ident(_))) if macro_line => "; a macro is written '#NAME = value', and a comment starts with ';'".to_string(),
            (Token::Hash, _, _) => "; '#' marks an array's count, as in '[int#4]', and a comment starts with ';'".to_string(),
            // `&` joins the words after a type's `:`, and marks a place given at
            // a call, which starts a value.
            (Token::Dollar, _, _) => "; '$' marks a decorator, written on the lines above a task or a shared method, as in '$register' or '$route(\"/\")'".to_string(),
            (Token::Amp, _, _) => "; Vela writes 'and' for both at once ('&' gives a place to a 'flex&' or 'fixed&' parameter, as in 'f(&a)', and joins the words a generic type must allow)".to_string(),
            _ => String::new(),
        };
        Error::at(self.peek_span(), format!("expected {}, got {}{}", expected, self.peek().describe(), hint))
    }

    /// Whether the next token is the first on its line.
    fn at_line_start(&self) -> bool {
        self.pos == 0 || matches!(self.tokens.get(self.pos - 1), Some(Token::Newline))
    }

    /// The error for a `<-` that doesn't start a line. After something that
    /// can end a value in a condition or statement, it was most likely meant as
    /// `< -`; on the line of a task's header, an `each`, or a `case`, the block's
    /// body was meant to start on the next line.
    fn misplaced_send(&self) -> Error {
        let line_start = (0..self.pos).rev().find(|&p| self.tokens.get(p) == Some(&Token::Newline)).map_or(0, |p| p + 1);
        if let Some(opener @ (Token::Task | Token::Each | Token::Case)) = self.tokens.get(line_start) {
            return Error::at(
                self.peek_span(),
                format!("'<-' must start a line: the body of a '{}' starts on the line after it", opener.describe().trim_matches('\'')),
            );
        }
        let after_value = self.pos > 0
            && matches!(
                self.tokens.get(self.pos - 1),
                Some(
                    Token::Ident(_)
                        | Token::Int(_)
                        | Token::Float(_)
                        | Token::Char(_)
                        | Token::Text(_)
                        | Token::Interpolated(_)
                        | Token::True
                        | Token::False
                        | Token::None
                        | Token::RParen
                        | Token::RBracket
                        | Token::RBrace
                        | Token::Caret
                )
            );
        let message = if after_value {
            "'<-' sends a value and must start a line; to compare with a negative number, write '< -'"
        } else {
            "'<-' sends a value and must start a line"
        };
        Error::at(self.peek_span(), message)
    }

    /// `expect(Token::Fi)` for the block opened by `opener` at `opened`: when the
    /// file ends, or a new task, shape, or kind starts first, the error says which
    /// block is still open.
    fn expect_fi(&mut self, opener: &str, opened: Span) -> Result<()> {
        if self.match_token(Token::Fi) {
            return Ok(());
        }
        // An indented macro was meant to be inside the block.
        let first = if self.check(Token::Priv) { 1 } else { 0 };
        if self.at_macro_after(first) && self.peek_span().col > 1 {
            return Err(Error::at(self.peek_span(), MISPLACED_MACRO));
        }
        if self.at_type_alias_after(first) && self.peek_span().col > 1 {
            return Err(Error::at(self.peek_span(), MISPLACED_TYPE_ALIAS));
        }
        if self.at_pattern_after(first) && self.peek_span().col > 1 {
            return Err(Error::at(self.peek_span(), MISPLACED_PATTERN));
        }
        if self.is_at_end() || self.at_item_start() || matches!(self.peek(), Token::Else | Token::Case | Token::Finally | Token::RBrace) {
            // Directly in a task, an `else` or `case` has no block to belong to.
            let hint = match self.peek() {
                Token::Else if opener == "task" => " (or an 'else' without its 'when')",
                Token::Case if opener == "task" => " (or a 'case' outside a 'match')",
                Token::Finally if opener == "task" => " (or a 'finally' outside a loop)",
                _ => "",
            };
            return Err(Error::at(
                self.peek_span(),
                format!(
                    "expected 'fi' to close the '{}' at {}:{}, got {}{}",
                    opener,
                    opened.line,
                    opened.col,
                    self.peek().describe(),
                    hint
                ),
            ));
        }
        Err(self.unexpected("'fi'"))
    }

    fn expect(&mut self, expected: Token) -> Result<()> {
        if self.peek() == &expected {
            self.advance();
            Ok(())
        } else {
            Err(self.unexpected(&expected.describe()))
        }
    }

    fn match_token(&mut self, tok: Token) -> bool {
        if self.peek() == &tok {
            self.advance();
            true
        } else {
            false
        }
    }

    fn check(&self, tok: Token) -> bool {
        self.peek() == &tok
    }

    /// Rejects a line holding just `end`, where a block could close. Blocks
    /// close with `fi`; `end` is an ordinary name, but alone on a line it is
    /// never meaningful code, so it is almost certainly the old keyword.
    fn reject_stray_end(&self) -> Result<()> {
        self.reject_stray_end_in(false)
    }

    /// `reject_stray_end` in a block that closes with `}` when `plan_b`.
    fn reject_stray_end_in(&self, plan_b: bool) -> Result<()> {
        let is_end = matches!(self.peek(), Token::Ident(name) if name == "end");
        let next = self.tokens.get(self.pos + 1).unwrap_or(&Token::Eof);
        if is_end && matches!(next, Token::Newline | Token::Eof) {
            let message = if plan_b { "a plan B's block closes with '}', not 'end'" } else { "blocks close with 'fi', not 'end'" };
            return Err(Error::at(self.peek_span(), message));
        }
        Ok(())
    }

    /// Whether the next token starts a top-level item, which can't appear
    /// inside a block (so a block before it is missing its `fi`).
    fn at_item_start(&self) -> bool {
        match self.peek() {
            Token::Task | Token::Shape | Token::Kind | Token::Include => true,
            // `priv` also marks a shape's field.
            Token::Priv => matches!(self.tokens.get(self.pos + 1), Some(Token::Task | Token::Shape | Token::Kind)) || self.at_macro_after(1) || self.at_type_alias_after(1) || self.at_pattern_after(1),
            _ => self.at_macro_after(0) || (self.at_line_start() && (self.at_type_alias_after(0) || self.at_pattern_after(0))),
        }
    }

    /// Whether the next token is `end` followed, after any blank lines, by the
    /// end of the file or a new item.
    fn end_closes_item(&self) -> bool {
        if !matches!(self.peek(), Token::Ident(name) if name == "end") {
            return false;
        }
        let rest = self.tokens.get(self.pos + 1..).unwrap_or_default();
        let next = rest.iter().find(|token| **token != Token::Newline).unwrap_or(&Token::Eof);
        matches!(next, Token::Eof | Token::Task | Token::Shape | Token::Kind | Token::Priv)
    }

    /// Whether the tokens `ahead` of the cursor, on a line the cursor starts,
    /// start a macro: `#NAME =`, or `#NAME` with a type or nothing after it,
    /// which `parse_macro` explains. `# the count`, a comment as other
    /// languages write one, is none.
    fn at_macro_after(&self, ahead: usize) -> bool {
        let at = |offset: usize| self.tokens.get(self.pos + ahead + offset);
        let starts = |index: usize| index == 0 || self.tokens.get(index - 1) == Some(&Token::Newline);
        let hash = self.pos + ahead;
        let line_start = starts(hash) || (hash > 0 && self.tokens.get(hash - 1) == Some(&Token::Priv) && starts(hash - 1));
        line_start
            && at(0) == Some(&Token::Hash)
            && matches!(at(1), Some(Token::Ident(_)))
            && matches!(at(2), Some(Token::Assign | Token::Colon | Token::Newline | Token::Eof) | None)
    }

    /// Whether the cursor is at `macro NAME =`, a macro as other languages
    /// write it (see `ast::MACRO`).
    fn at_macro_word(&self) -> bool {
        matches!(self.peek(), Token::Ident(word) if word == MACRO)
            && matches!(self.tokens.get(self.pos + 1), Some(Token::Ident(_)))
            && matches!(self.tokens.get(self.pos + 2), Some(Token::Assign | Token::Colon))
    }

    /// `#NAME = value`, at `#`, on a line of its own.
    fn parse_macro(&mut self) -> Result<MacroDef> {
        self.advance(); // #
        let span = self.peek_span();
        let name = self.expect_ident()?;
        if self.check(Token::Colon) {
            return Err(Error::at(self.peek_span(), format!("a macro's type is its value's, so none is written: '#{} = …'", name)));
        }
        if !self.match_token(Token::Assign) {
            return Err(self.unexpected(&format!("'=' and the value macro '{}' stands for", name)));
        }
        let value = self.parse_expr()?;
        if !matches!(self.peek(), Token::Newline | Token::Eof) {
            return Err(self.unexpected("the end of the line: a macro is written on a line of its own"));
        }
        Ok(MacroDef { name, value, span, private: false })
    }

    /// Whether the tokens `ahead` of the cursor start a type alias: `alias`
    /// and a name.
    fn at_pattern_after(&self, ahead: usize) -> bool {
        matches!(self.tokens.get(self.pos + ahead), Some(Token::Ident(word)) if word == PATTERN)
            && matches!(self.tokens.get(self.pos + ahead + 1), Some(Token::Ident(_)))
            && matches!(self.tokens.get(self.pos + ahead + 2), None | Some(Token::Newline | Token::Eof | Token::Colon | Token::Lt))
    }

    fn at_type_alias_after(&self, ahead: usize) -> bool {
        matches!(self.tokens.get(self.pos + ahead), Some(Token::Ident(word)) if word == ALIAS) && matches!(self.tokens.get(self.pos + ahead + 1), Some(Token::Ident(_)))
    }

    /// `alias Name => type`, or `priv alias Name => type`, on a line of its own.
    fn parse_type_alias(&mut self) -> Result<AliasDef> {
        let private = self.match_token(Token::Priv);
        self.advance(); // alias
        let span = self.peek_span();
        let name = self.expect_ident()?;
        if !self.match_token(Token::FatArrow) {
            return Err(self.unexpected(&format!("'=>' and the type '{}' stands for, as in 'alias {} => (int) -> int'", name, name)));
        }
        let ty_span = self.peek_span();
        let ty = self.parse_type()?;
        if !matches!(self.peek(), Token::Newline | Token::Eof) {
            return Err(self.unexpected("the end of the line: a type alias is written on a line of its own"));
        }
        Ok(AliasDef { name, ty, span, ty_span, private })
    }

    fn parse_item(&mut self) -> Result<Item> {
        self.skip_newlines();
        if self.check(Token::Dollar) {
            return self.parse_decorated();
        }
        if self.check(Token::Priv) {
            let span = self.peek_span();
            self.advance();
            let mut item = match self.peek() {
                Token::Task => self.parse_task().map(Item::Task)?,
                _ if self.at_native() => self.parse_native().map(Item::Task)?,
                Token::Shape => self.parse_shape().map(Item::Shape)?,
                Token::Kind => self.parse_kind().map(Item::Kind)?,
                _ if self.at_macro_after(0) => self.parse_macro().map(Item::Macro)?,
                _ => return Err(Error::at(span, "'priv' goes before a task, shape, kind, pattern, type alias, or macro, or a shape's field")),
            };
            match &mut item {
                Item::Task(t) => t.private = true,
                Item::Shape(s) => s.private = true,
                Item::Kind(k) => k.private = true,
                Item::Macro(m) => m.private = true,
            }
            return Ok(item);
        }
        if self.at_native() {
            return self.parse_native().map(Item::Task);
        }
        if self.at_macro_after(0) {
            return self.parse_macro().map(Item::Macro);
        }
        if self.at_macro_word() {
            let name = match self.tokens.get(self.pos + 1) {
                Some(Token::Ident(name)) => name.clone(),
                _ => String::new(),
            };
            return Err(Error::at(self.peek_span(), format!("a macro is written with '#' before its name, as in '#{} = …'", name)));
        }
        match self.peek() {
            Token::Task => self.parse_task().map(Item::Task),
            Token::Shape => self.parse_shape().map(Item::Shape),
            Token::Kind => self.parse_kind().map(Item::Kind),
            Token::Fi => Err(Error::at(self.peek_span(), "expected 'task', 'shape', or 'kind', got 'fi'; is there an extra 'fi' above it?")),
            // `use m` or `import m`, as other languages bring in a file.
            Token::Ident(word)
                if (word == "use" || word == "import")
                    && self.at_line_start()
                    && matches!(self.tokens.get(self.pos + 1), Some(Token::Ident(_) | Token::Range | Token::Text(_))) =>
            {
                Err(Error::at(self.peek_span(), format!("expected 'task', 'shape', or 'kind', got name '{}'; another file is brought in with 'include', as in 'include geometry'", word)))
            }
            token if !matches!(token, Token::Send) || self.at_line_start() => {
                let statement = matches!(
                    token,
                    Token::Ident(_)
                        | Token::Fixed
                        | Token::Flex
                        | Token::When
                        | Token::Loop
                        | Token::Each
                        | Token::Block
                        | Token::Match
                        | Token::Send
                        | Token::Region
                        | Token::Else
                        | Token::Case
                        | Token::Break
                        | Token::Continue
                );
                let error = self.unexpected("'task', 'shape', or 'kind'");
                if statement {
                    return Err(Error::at(
                        self.peek_span(),
                        format!("{}; statements go inside a task (is there an extra 'fi' above it?)", error.message),
                    ));
                }
                Err(error)
            }
            _ => Err(self.unexpected("'task', 'shape', or 'kind'")),
        }
    }

    /// Decorators, `$d` or `$d(args)`, several on a line or on lines of their
    /// own, and the task they are written before, at the first `$`.
    fn parse_decorated(&mut self) -> Result<Item> {
        let first = self.peek_span();
        let (decorators, wrappers) = self.parse_decorator_lines()?;
        let decorated = matches!(self.peek(), Token::Task) || self.at_native() || (self.check(Token::Priv) && !self.at_macro_after(1) && matches!(self.tokens.get(self.pos + 1), Some(Token::Task)));
        let decorated = decorated || (self.check(Token::Priv) && matches!(self.tokens.get(self.pos + 1), Some(Token::Ident(word)) if word == NATIVE));
        if !decorated {
            return Err(Error::at(first, format!("a decorator goes on the lines above a task, and {} follows it", self.peek().describe())));
        }
        let mut item = self.parse_item()?;
        if let Item::Task(task) = &mut item {
            decorate(task, decorators, wrappers);
        }
        Ok(item)
    }

    /// The decorators on the lines above a task or a shared method, at the first
    /// `$`: the startup decorators, and the wrappers, each the call of its name
    /// with its own values.
    fn parse_decorator_lines(&mut self) -> Result<(Vec<Decorator>, Vec<Expr>)> {
        let mut decorators = Vec::new();
        let mut wrappers = Vec::new();
        while self.check(Token::Dollar) {
            self.advance(); // $
            if self.check(Token::LParen) {
                self.parse_wrappers(&mut wrappers)?;
                if self.check(Token::Question) {
                    return Err(Error::at(
                        self.peek_span(),
                        "'?' lets a startup decorator's failure pass, as in '$route(\"/\")?'; a wrapper runs at each call, and its failure is the task's own, sent to its caller",
                    ));
                }
            } else {
                decorators.push(self.parse_startup_decorator()?);
            }
            if !self.check(Token::Dollar) {
                if !matches!(self.peek(), Token::Newline) {
                    return Err(self.unexpected("another decorator or the end of the line: a task starts on the line after its decorators"));
                }
                self.skip_newlines();
            }
        }
        Ok((decorators, wrappers))
    }

    /// `d` or `d(values)` after a `$`: a startup decorator's name and values.
    fn parse_startup_decorator(&mut self) -> Result<Decorator> {
        let span = self.peek_span();
        if !matches!(self.peek(), Token::Ident(_)) {
            return Err(self.unexpected("the name of a task after '$', as in '$register' or '$route(\"/\")'"));
        }
        let name = self.decorator_name()?;
        let args = if self.check(Token::LParen) { self.parse_arg_list()? } else { Vec::new() };
        // `$d?`: a failure of the decorator lets the program go on.
        let passes = self.match_token(Token::Question);
        Ok((name, args, span, passes))
    }

    /// The name of a decorator, a task: `d`, `m.d`, or a shared method, `X.m`
    /// or `m.X.m`, which the resolver tells apart from a module's task.
    fn decorator_name(&mut self) -> Result<String> {
        let name = self.expect_ident()?;
        let mut name = self.qualified(name)?;
        // Any more is the resolver's to tell apart, and to refuse.
        while self.match_token(Token::Dot) {
            // `X.new`: the one keyword a member may be named, a shared method.
            let member = if self.match_token(Token::New) { NEW_WORD.to_string() } else { self.expect_ident()? };
            name = format!("{}{}{}", name, MODULE_SEPARATOR, member);
        }
        Ok(name)
    }

    /// `(w1, w2(values), …)` after a `$`: wrapper decorators, each kept as the
    /// call of its name with its own values.
    fn parse_wrappers(&mut self, wrappers: &mut Vec<Expr>) -> Result<()> {
        let opened = self.peek_span();
        self.advance(); // (
        if self.check(Token::RParen) {
            return Err(Error::at(opened, "'$( … )' names the wrappers of the task below it, as in '$(timed)' or '$(timed, logged(\"sq\"))'; leave it out when there are none"));
        }
        loop {
            let span = self.peek_span();
            // `$(.checked)`: a method of the value the wrapped method is called on.
            let member = self.check(Token::Dot) && matches!(self.tokens.get(self.pos + 1), Some(Token::Ident(_)));
            if member {
                self.advance(); // .
            }
            if !matches!(self.peek(), Token::Ident(_)) {
                return Err(self.unexpected("the name of a wrapper, as in '$(timed)' or '$(logged(\"sq\"))'"));
            }
            let name = if member { format!("{}{}", MEMBER_WRAPPER, self.expect_ident()?) } else { self.decorator_name()? };
            let args = if self.check(Token::LParen) { self.parse_arg_list()? } else { Vec::new() };
            wrappers.push(Expr::new(ExprKind::Call { name, types: Vec::new(), args }, span));
            if !self.match_token(Token::Comma) {
                break;
            }
        }
        if !self.match_token(Token::RParen) {
            return Err(self.unexpected("',' and another wrapper, or ')' to close the wrappers"));
        }
        Ok(())
    }

    fn parse_task(&mut self) -> Result<TaskDef> {
        let opened = self.peek_span();
        let mut task = self.parse_task_header()?;
        if self.check(Token::LBrace) {
            return Err(Error::at(self.peek_span(), "a task's body starts on the line after its header, and closes with 'fi'"));
        }
        self.skip_newlines();
        let outer = std::mem::replace(&mut self.in_generic, !task.types.is_empty());
        task.body = self.parse_block()?;
        self.in_generic = outer;
        self.expect_fi("task", opened)?;
        Ok(task)
    }

    /// `task name<types>(params) -> results`, at `task`: a task with no body yet.
    fn parse_task_header(&mut self) -> Result<TaskDef> {
        self.advance(); // task
        let span = self.peek_span();
        let name = self.expect_ident()?;
        let types = self.parse_type_params(true)?;
        let params = self.parse_field_list(Slot::Param)?;
        let mut results = Vec::new();
        let ret = if self.match_token(Token::Arrow) {
            self.parse_results(&mut results)?
        } else {
            Type::Nothing
        };
        Ok(TaskDef { name, types, given: Vec::new(), params, ret, results, body: Vec::new(), span, private: false, native: false, owner: None, decorators: Vec::new(), passes: Vec::new(), wrappers: Vec::new(), kept: None, shortcut: false })
    }

    /// Whether the cursor is at `native task` (see `ast::NATIVE`).
    fn at_native(&self) -> bool {
        matches!(self.peek(), Token::Ident(word) if word == NATIVE) && self.tokens.get(self.pos + 1) == Some(&Token::Task)
    }

    /// `native task name(params) -> results`: a task whose body is a function
    /// of the runtime, so its header is all there is of it. Whether its file
    /// may have one is for loading to say (see `modules::load`).
    fn parse_native(&mut self) -> Result<TaskDef> {
        let opened = self.peek_span();
        self.advance(); // native
        let mut task = self.parse_task_header()?;
        task.native = true;
        if !matches!(self.peek(), Token::Newline | Token::Eof) {
            return Err(self.unexpected("the end of the line: a native task is its header alone"));
        }
        self.skip_newlines();
        // A body written anyway is read, so that loading says what is wrong: in
        // a program, that it has no native tasks at all.
        if !matches!(self.peek(), Token::Task | Token::Shape | Token::Kind | Token::Priv | Token::Eof) && !self.at_native() && !self.at_macro_after(0) && !self.at_type_alias_after(0) && !self.at_pattern_after(0) {
            task.body = self.parse_block()?;
            self.expect_fi("task", opened)?;
        }
        Ok(task)
    }

    /// `<T, U: ordered>` after a declaration's name: the types it takes, with
    /// what each one must allow. Empty for a declaration that takes none.
    fn parse_type_params(&mut self, groups: bool) -> Result<Vec<TypeParam>> {
        // `<>` reads as one token, `not similar`, wherever values are.
        if self.check(Token::NotSimilar) {
            return Err(Error::at(self.peek_span(), "'< … >' names the types a declaration takes, as in '<T>'; leave it out when it takes none"));
        }
        if !self.match_token(Token::Lt) {
            return Ok(Vec::new());
        }
        let mut params: Vec<TypeParam> = Vec::new();
        loop {
            if self.check(Token::Gt) {
                // `<T,>`: a type follows every comma.
                let message = match params.is_empty() {
                    true => "'< … >' names the types a declaration takes, as in '<T>'; leave it out when it takes none",
                    false => "expected the name of another type after ',', as in '<T, U>'; leave the ',' out when there is none",
                };
                return Err(Error::at(self.peek_span(), message));
            }
            let span = self.peek_span();
            let name = self.expect_ident()?;
            if Type::builtin(&name).is_some() {
                return Err(Error::at(span, format!("'{}' is a builtin type, so it can't name a type a declaration takes; use another name, as in '<T>'", name)));
            }
            if let Some(before) = params.iter().find(|param| param.name == name) {
                return Err(Error::at(span, format!("'{}' is already taken at {}:{}", name, before.span.line, before.span.col)));
            }
            // `P...`: a group of types, which only a task takes.
            let group = self.check(Token::Ellipsis);
            if group {
                if !groups {
                    return Err(Error::at(self.peek_span(), "only a task takes a group of types, 'P...'; a shape takes each type by name, as in '<T, U>'"));
                }
                if let Some(before) = params.iter().find(|param| param.group) {
                    return Err(Error::at(span, format!("a task takes one group of types, and '{}...' is one already", before.name)));
                }
                self.advance(); // ...
            }
            let (needs, patterns) = if self.match_token(Token::Colon) { self.parse_needs()? } else { (Vec::new(), Vec::new()) };
            params.push(TypeParam { name, span, needs, patterns, group });
            if !self.match_token(Token::Comma) {
                break;
            }
        }
        if !self.match_token(Token::Gt) {
            return Err(self.unexpected("',' and another type, or '>' to close the types"));
        }
        Ok(params)
    }

    /// The words after a type's `:`, joined by `&`: what every type given for it
    /// must allow, each one thing, and the patterns it must follow, each a name
    /// that is no word (which the resolver checks names a pattern). A word
    /// another already means is an error, since it says nothing new.
    #[allow(clippy::type_complexity)]
    fn parse_needs(&mut self) -> Result<(Vec<(Need, Span)>, Vec<(String, Span)>)> {
        let mut kept: Vec<(Need, Span)> = Vec::new();
        let mut patterns: Vec<(String, Span)> = Vec::new();
        loop {
            let span = self.peek_span();
            let word = self.expect_ident().map_err(|_| Error::at(span, format!("expected what the type must allow: {}, or a pattern it follows", needs_list())))?;
            let Some(need) = Need::from_word(&word) else {
                let pattern = self.qualified(word)?;
                if let Some((_, before)) = patterns.iter().find(|(before, _)| *before == pattern) {
                    return Err(Error::at(span, format!("'{}' is written twice, at {}:{} too", pattern, before.line, before.col)));
                }
                patterns.push((pattern, span));
                if !self.match_token(Token::Amp) {
                    return Ok((kept, patterns));
                }
                continue;
            };
            if let Some(&(before, _)) = kept.iter().find(|(before, _)| before.means(need) || need.means(*before)) {
                if before == need {
                    return Err(Error::at(span, format!("'{}' is written twice", need.word())));
                }
                let (narrower, wider) = if before.means(need) { (before, need) } else { (need, before) };
                return Err(Error::at(span, format!("{}, so '{}' already means '{}'; write '{}' alone", why_means(narrower, wider), narrower.word(), wider.word(), narrower.word())));
            }
            kept.push((need, span));
            if !self.match_token(Token::Amp) {
                return Ok((kept, patterns));
            }
        }
    }

    /// The results after a task's `->`: one, or several separated by commas,
    /// each a type or a name and a type (`total: int`). Either all of them are
    /// named or none is.
    fn parse_results(&mut self, results: &mut Vec<ResultDef>) -> Result<Type> {
        let mut types = Vec::new();
        loop {
            if self.check(Token::Newline) || self.check(Token::Fi) || self.is_at_end() {
                let expected = if types.is_empty() { "expected a return type after '->'" } else { "expected another result after ','" };
                return Err(Error::at(self.peek_span(), expected));
            }
            // A name only when a `:` follows it; a result's type is a name too.
            let named = matches!(self.peek(), Token::Ident(_)) && self.tokens.get(self.pos + 1) == Some(&Token::Colon);
            let name = if named {
                let at = self.peek_span();
                let name = self.expect_ident()?;
                self.advance(); // :
                Some((name, at))
            } else {
                None
            };
            if let Some(first) = results.first()
                && first.name.is_some() != name.is_some()
            {
                let (at, which) = match &name {
                    Some((_, at)) => (*at, "has a name"),
                    None => (self.peek_span(), "has no name"),
                };
                return Err(Error::at(
                    at,
                    format!("either every result has a name or none does, and this one {}, as in '-> steps: int, peak: int'", which),
                ));
            }
            let ty_span = self.peek_span();
            results.push(ResultDef { name, ty_span });
            types.push(self.parse_type()?);
            // In a pattern, `area() -> float, name(r: region) -> text@r`: no
            // result's type is a name and a `(`, so one starts the next header.
            let next_header = self.in_pattern
                && (matches!(self.tokens.get(self.pos + 1), Some(Token::Ident(_))) && matches!(self.tokens.get(self.pos + 2), Some(Token::LParen | Token::Lt))
                    || matches!(self.tokens.get(self.pos + 1), Some(Token::Ident(word)) if word == ALTERS));
            if next_header || !self.match_token(Token::Comma) {
                break;
            }
        }
        Ok(if types.len() == 1 {
            types.into_iter().next().unwrap_or(Type::Nothing)
        } else {
            Type::Results(types)
        })
    }

    fn parse_shape(&mut self) -> Result<ShapeDef> {
        let opened = self.peek_span();
        self.advance(); // shape
        let span = self.peek_span();
        let name = self.expect_ident()?;
        let types = self.parse_type_params(false)?;
        let follows = self.parse_after_colons("shape", &name)?;
        self.skip_newlines();
        let mut fields: Vec<Field> = Vec::new();
        let mut shared: Vec<SharedField> = Vec::new();
        let mut methods: Vec<TaskDef> = Vec::new();
        while !self.check(Token::Fi) && !self.is_at_end() && !self.at_item_start() {
            self.skip_newlines();
            if self.check(Token::Fi) || self.at_item_start() { break; }
            self.reject_stray_end()?;
            let (decorators, wrappers) = self.parse_member_decorators("shape")?;
            let private = self.match_token(Token::Priv);
            self.reject_misplaced_method_words()?;
            // `shared new(…) -> X`: a method of the shape itself.
            if self.check(Token::Shared) && self.at_method_from(self.pos + 1) {
                self.advance(); // shared
                let mut method = self.parse_method(&name, "shape", &types, private, true, Above { fields: &fields, shared: &shared, methods: &methods })?;
                decorate(&mut method, decorators, wrappers);
                methods.push(method);
                self.skip_newlines();
                continue;
            }
            if self.check(Token::Shared) {
                let member = self.parse_shared_field(&name, &types, private, &methods)?;
                reject_same_name(&member.field, &fields, &shared)?;
                shared.push(member);
                self.skip_newlines();
                continue;
            }
            if self.check(Token::New) {
                return Err(Error::at(self.peek_span(), new_only_shared(&name)));
            }
            if self.at_method() {
                let mut method = self.parse_method(&name, "shape", &types, private, false, Above { fields: &fields, shared: &shared, methods: &methods })?;
                decorate(&mut method, decorators, wrappers);
                methods.push(method);
                self.skip_newlines();
                continue;
            }
            // `read shared f`: the words go the other way round.
            if matches!(self.peek(), Token::Ident(word) if word == READ) && self.tokens.get(self.pos + 1) == Some(&Token::Shared) {
                return Err(Error::at(self.peek_span(), format!("'shared' goes first, as in 'shared {} f: int = 0'", READ)));
            }
            let read = self.match_read(private)?;
            self.reject_field_below_method(&methods, "shape")?;
            let mut field = self.parse_field(Slot::Field)?;
            reject_member_named_this(&field.name, field.span, "a field")?;
            reject_same_name(&field, &fields, &shared)?;
            field.private = private;
            field.read = read;
            fields.push(field);
            self.skip_newlines();
        }
        self.expect_fi("shape", opened)?;
        self.methods.extend(methods.into_iter().map(Item::Task));
        Ok(ShapeDef { name, types, given: Vec::new(), fields, shared, span, private: false, follows })
    }

    /// A shape's `shared` field, with the cursor at `shared` and `private` saying
    /// `priv` came before it: `shared f: int = 0`, `shared flex f: int = 0`, or
    /// `shared read f: int = 0`. It always names its first value.
    fn parse_shared_field(&mut self, shape: &str, types: &[TypeParam], private: bool, methods: &[TaskDef]) -> Result<SharedField> {
        let at = self.peek_span();
        self.advance(); // shared
        if self.check(Token::Priv) {
            return Err(Error::at(self.peek_span(), "'priv' goes first, as in 'priv shared f: int = 0'"));
        }
        if self.check(Token::Shared) {
            return Err(Error::at(self.peek_span(), "'shared' is already written here"));
        }
        let word_at = self.peek_span();
        let flex = self.match_token(Token::Flex);
        let bind = !flex && self.match_token(Token::Bind);
        if (flex || bind) && self.check(Token::Amp) {
            return Err(Error::at(self.peek_span(), "a shared field is a value of its own, not a place elsewhere, so it is 'shared flex' or 'shared bind', without '&'"));
        }
        if (flex || bind) && self.check(Token::Flex) {
            return Err(Error::at(self.peek_span(), "'flex' is already written here"));
        }
        if (flex || bind) && self.check(Token::Bind) {
            return Err(Error::at(self.peek_span(), "'bind' is already written here"));
        }
        let read = self.match_read(private)?;
        if (flex || bind) && read {
            let word = if flex { "flex" } else { "bind" };
            return Err(Error::at(
                word_at,
                format!("a shared field is '{0}' or '{1}', not both: '{0}' lets any code that sees it change it, and '{1}' only the methods of its shape", word, READ),
            ));
        }
        if read && (self.check(Token::Flex) || self.check(Token::Bind)) {
            return Err(Error::at(
                self.peek_span(),
                format!("a shared field is 'flex' or 'bind' or '{0}', not both: 'flex' or 'bind' lets any code that sees it change it, and '{0}' only the methods of its shape change", READ),
            ));
        }
        if !types.is_empty() {
            return Err(Error::at(
                at,
                format!("a shape that takes types can't have shared fields yet ('{}' takes {}): it is a different shape for every set of types", shape, type_param_list(types)),
            ));
        }
        self.reject_field_below_method(methods, "shape")?;
        let mut field = self.parse_field(Slot::Field)?;
        reject_member_named_this(&field.name, field.span, "a field")?;
        if field.default.is_none() {
            return Err(Error::at(
                field.ty_span,
                format!("a shared field names its first value, as in 'shared {0}: {1} = …', since no literal ever gives it one", field.name, field.ty),
            ));
        }
        field.private = private;
        field.read = read;
        Ok(SharedField { field, flex, bind, start: None })
    }

    /// Rejects a field written below a method: a shape's or a kind's block reads
    /// as its values and then what they do.
    fn reject_field_below_method(&self, methods: &[TaskDef], what: &str) -> Result<()> {
        let Some(first) = methods.first() else { return Ok(()) };
        let name = first.owner.as_ref().map_or("", |owner| owner.name.as_str());
        let of = if what == "kind" { "variant" } else { "field" };
        Err(Error::at(
            self.peek_span(),
            format!(
                "every {} of a {} goes above its methods, and method '{}' is declared at {}:{}",
                of, what, name, first.span.line, first.span.col
            ),
        ))
    }

    /// Whether the cursor is at a member of a shape or a kind that is a method
    /// rather than a field or a variant: a name followed by `(`, or by `<` for
    /// one that names types (which `parse_method` refuses), with `alters` allowed
    /// before it.
    fn at_method(&self) -> bool {
        self.at_method_from(self.pos)
    }

    /// `at_method` for the member starting at token `pos`; `new(`, a shared
    /// method's name, counts too, which `parse_method` reads.
    fn at_method_from(&self, pos: usize) -> bool {
        let mut pos = pos;
        if matches!(self.tokens.get(pos), Some(Token::Ident(word)) if word == ALTERS) && matches!(self.tokens.get(pos + 1), Some(Token::Ident(_) | Token::New)) {
            pos += 1;
        }
        matches!(self.tokens.get(pos), Some(Token::Ident(_) | Token::New)) && matches!(self.tokens.get(pos + 1), Some(Token::LParen | Token::Lt))
    }

    /// The decorators on the lines above a member of a shape or a kind (`what`),
    /// which must be a shared method: one called on its type, which the
    /// program can give a decorator where it starts, and a wrapper at each
    /// call. None where no `$` starts the member.
    fn parse_member_decorators(&mut self, what: &str) -> Result<(Vec<Decorator>, Vec<Expr>)> {
        if !self.check(Token::Dollar) {
            return Ok((Vec::new(), Vec::new()));
        }
        let first = self.peek_span();
        let lines = self.parse_decorator_lines()?;
        let at = self.pos + usize::from(self.check(Token::Priv));
        if self.tokens.get(at) == Some(&Token::Shared) && self.at_method_from(at + 1) {
            return Ok(lines);
        }
        // In a kind, a variant carrying values looks like a method here, unless
        // `alters` says it is one; the kind tells them apart after.
        let alters = matches!(self.tokens.get(at), Some(Token::Ident(word)) if word == ALTERS);
        let method = self.at_method_from(at) || alters;
        // A method of every value takes wrappers, which run at each call.
        if method && lines.0.is_empty() {
            return Ok(lines);
        }
        let follows = self.tokens.get(at).unwrap_or(&Token::Eof).describe();
        // Only wrappers: what follows takes none.
        if lines.0.is_empty() {
            let what_follows = match self.tokens.get(at) {
                Some(Token::Shared) => "a shared field, which holds a value rather than running".to_string(),
                Some(Token::Ident(field)) if what == "shape" && self.tokens.get(at + 1) == Some(&Token::Colon) => format!("the field '{}', which holds a value rather than running", field),
                Some(Token::Ident(_)) if what == "kind" => format!("{}, a variant, which makes a value rather than running", follows),
                _ => follows,
            };
            return Err(Error::at(first, format!("a wrapper in a {} goes on the lines above a method, as in '$(timed)' above 'area() -> int', and what follows it is {}", what, what_follows)));
        }
        let follows = if (what == "shape" && method) || alters {
            "; this is a method of every value, which has no value to be called on where the program starts, though it takes wrappers, '$(w)', which run at each call".to_string()
        } else if self.tokens.get(at) == Some(&Token::Shared) {
            ", and a shared field follows it, which takes none".to_string()
        } else if what == "kind" && matches!(self.tokens.get(at), Some(Token::Ident(_))) {
            format!(", and {} follows it, a variant or a method of every value, which take no startup decorator", follows)
        } else {
            format!(", and {} follows it", follows)
        };
        Err(Error::at(first, format!("a startup decorator in a {} goes on the lines above a shared method, as in '$register' above 'shared new(…)'{}", what, follows)))
    }

    /// Refuses `alters shared f(…)`, which a shared method never is, with the
    /// cursor at a member of a shape or a kind.
    fn reject_misplaced_method_words(&self) -> Result<()> {
        if matches!(self.peek(), Token::Ident(word) if word == ALTERS) && self.tokens.get(self.pos + 1) == Some(&Token::Shared) {
            return Err(Error::at(self.peek_span(), SHARED_NEVER_ALTERS));
        }
        Ok(())
    }

    /// A method of the shape or kind `owner`, which takes `types`: its name, its
    /// parameters, what it sends, and its body, with `alters` before it when it
    /// changes the value it is called on. It becomes a task whose first parameter
    /// holds that value (see `method_name` and `THIS`).
    fn parse_method(&mut self, owner: &str, what: &str, types: &[TypeParam], private: bool, is_shared: bool, above: Above) -> Result<TaskDef> {
        let Above { fields, shared, methods } = above;
        let opened = self.peek_span();
        let alters_span = match self.peek() {
            Token::Ident(word) if word == ALTERS => {
                if is_shared {
                    return Err(Error::at(opened, SHARED_NEVER_ALTERS));
                }
                self.advance();
                Some(opened)
            }
            _ => None,
        };
        let span = self.peek_span();
        // `new` is a keyword, and a shared method's name: `X.new(…)` makes an X.
        let name = match self.peek() {
            Token::New if is_shared => {
                self.advance();
                NEW_WORD.to_string()
            }
            Token::New => return Err(Error::at(span, new_only_shared(owner))),
            _ => self.expect_ident()?,
        };
        if is_shared && name == STR {
            return Err(Error::at(span, "a method named 'str' writes a value it is called on, so it isn't 'shared'"));
        }
        if is_shared && !types.is_empty() {
            return Err(Error::at(
                span,
                format!("a {0} that takes types can't have shared methods yet ('{1}' takes {2}): it is a different {0} for every set of types", what, owner, type_param_list(types)),
            ));
        }
        reject_member_named_this(&name, span, "a method")?;
        if is_builtin_member(&name) || name == LEN {
            return Err(Error::at(
                span,
                format!("'{}' is a reserved name: every value or slice already answers to it, so a method can't be called that", name),
            ));
        }
        if let Some(field) = fields.iter().find(|field| field.name == name) {
            return Err(Error::at(
                span,
                format!(
                    "'{0}' is already a field of '{1}', at {2}:{3}; both are written '.{0}', so a field and a method can't share a name",
                    name, owner, field.span.line, field.span.col
                ),
            ));
        }
        if let Some(member) = shared.iter().find(|member| member.field.name == name) {
            return Err(Error::at(
                span,
                format!(
                    "'{0}' is already a shared field of '{1}', at {2}:{3}; a method and a shared field of one shape can't share a name",
                    name, owner, member.field.span.line, member.field.span.col
                ),
            ));
        }
        if let Some(before) = methods.iter().find(|before| before.owner.as_ref().is_some_and(|before| before.name == name)) {
            return Err(Error::at(span, format!("method '{}' is already defined at {}:{}", name, before.span.line, before.span.col)));
        }
        if self.check(Token::Lt) {
            let takes = if types.is_empty() { String::new() } else { format!(" ('{}' already gives it {})", owner, type_param_list(types)) };
            return Err(Error::at(
                self.peek_span(),
                format!("a method takes no types of its own yet{}; write a task that takes them, and call it from here", takes),
            ));
        }
        // A shared method has no value it is called on.
        let mut params = if is_shared { Vec::new() } else { vec![receiver(owner, types, span)] };
        let read = self.parse_field_list(Slot::Param);
        params.extend(if what == "kind" { read.map_err(Parser::kind_member)? } else { read? });
        if let Some(param) = params.iter().skip(usize::from(!is_shared)).find(|param| param.ty == Type::Region && param.name == THIS_WORD) {
            return Err(Error::at(param.span, format!("'{0}' is the region the value this method is called on lives in, written '@{0}', so a region parameter can't take the name", THIS_WORD)));
        }
        let mut results = Vec::new();
        let ret = if self.match_token(Token::Arrow) {
            self.parse_results(&mut results)?
        } else {
            Type::Nothing
        };
        if self.check(Token::LBrace) {
            return Err(Error::at(self.peek_span(), "a method's body starts on the line after its header, and closes with 'fi'"));
        }
        self.skip_newlines();
        let outer = std::mem::replace(&mut self.in_generic, !types.is_empty());
        let body = self.parse_block()?;
        self.in_generic = outer;
        self.expect_fi("method", opened)?;
        Ok(TaskDef {
            name: method_name(owner, &name),
            types: types.to_vec(),
            given: Vec::new(),
            params,
            ret,
            results,
            body,
            span,
            private,
            native: false,
            owner: Some(Owner { name, alters: alters_span.is_some(), alters_span, shared: is_shared }),
            decorators: Vec::new(),
            passes: Vec::new(),
            wrappers: Vec::new(),
            kept: None,
            shortcut: false,
        })
    }

    fn parse_kind(&mut self) -> Result<KindDef> {
        let opened = self.peek_span();
        self.advance(); // kind
        let span = self.peek_span();
        let name = self.expect_ident()?;
        let types = self.parse_type_params(false)?;
        let imitates = self.parse_after_colons("kind", &name)?;
        self.skip_newlines();
        let mut variants = Vec::new();
        let mut methods: Vec<TaskDef> = Vec::new();
        while !self.check(Token::Fi) && !self.is_at_end() && !self.at_item_start() {
            self.skip_newlines();
            if self.check(Token::Fi) || self.at_item_start() { break; }
            // A variant may be named `end`; only a stray `end` that ends the
            // kind (followed by the file's end or a new item) is the old keyword.
            if self.end_closes_item() {
                self.reject_stray_end()?;
            }
            let (decorators, wrappers) = self.parse_member_decorators("kind")?;
            let variant_span = self.peek_span();
            // `priv` goes before a method, whose name is what the checks below read.
            let private = self.match_token(Token::Priv);
            self.reject_misplaced_method_words()?;
            // `shared make(…) -> K`: a method of the kind itself.
            if self.check(Token::Shared) && self.at_method_from(self.pos + 1) {
                self.advance(); // shared
                let mut method = self.parse_method(&name, "kind", &types, private, true, Above { fields: &[], shared: &[], methods: &methods })?;
                decorate(&mut method, decorators, wrappers);
                methods.push(method);
                self.skip_newlines();
                continue;
            }
            if self.check(Token::Shared) {
                return Err(Error::at(
                    self.peek_span(),
                    format!("a kind has no fields, so no shared ones; a shape has them, and a kind has shared methods, as in 'shared make() -> {}'", name),
                ));
            }
            if self.check(Token::New) {
                return Err(Error::at(self.peek_span(), new_only_shared(&name)));
            }
            // A variant carries its values in parentheses, as a method takes its
            // parameters, so `alters` or a `->` after them tells them apart.
            if self.at_method() && !self.at_variant_payload(&methods) {
                let mut method = self.parse_method(&name, "kind", &types, private, false, Above { fields: &[], shared: &[], methods: &methods })?;
                decorate(&mut method, decorators, wrappers);
                methods.push(method);
                self.skip_newlines();
                continue;
            }
            if let Some(wrapper) = wrappers.first() {
                return Err(Error::at(wrapper.span, "a wrapper in a kind goes on the lines above a method, and a variant follows it here, which makes a value rather than running"));
            }
            if private {
                return Err(Error::at(variant_span, "a variant can't be 'priv': every variant is as visible as its kind"));
            }
            self.reject_field_below_method(&methods, "kind")?;
            let tag = self.expect_ident()?;
            let payload = if self.check(Token::LParen) {
                self.parse_field_list(Slot::Value).map_err(Parser::kind_member)?
            } else {
                Vec::new()
            };
            variants.push(Variant { tag, payload, span: variant_span });
            self.skip_newlines();
        }
        self.expect_fi("kind", opened)?;
        // `Kind.Tag(…)` makes a variant, so a shared method can't take its name.
        for method in &methods {
            let Some(owner) = method.owner.as_ref().filter(|owner| owner.shared) else { continue };
            if let Some(variant) = variants.iter().find(|variant| variant.tag == owner.name) {
                return Err(Error::at(
                    method.span,
                    format!(
                        "'{0}' is already a variant of '{1}', at {2}:{3}; '{1}.{0}(…)' makes the variant, so a shared method can't take its name",
                        owner.name, name, variant.span.line, variant.span.col
                    ),
                ));
            }
        }
        self.methods.extend(methods.into_iter().map(Item::Task));
        Ok(KindDef { name, types, given: Vec::new(), variants, span, private: false, imitates, follows: Vec::new(), imitated: 0 })
    }

    /// `::Other & Shown` after a kind's or a shape's (`what`) name and types:
    /// the kinds a kind imitates and the patterns either follows, each a type
    /// with where it is written, which the resolver tells apart. Empty when no
    /// `::` follows the name.
    fn parse_after_colons(&mut self, what: &str, name: &str) -> Result<Vec<(Type, Span)>> {
        if !self.check(Token::Colon) {
            return Ok(Vec::new());
        }
        let (does, other, members) = match what {
            "kind" => ("imitates another kind or follows a pattern", "Other", "the variants of a kind start"),
            _ => ("follows a pattern", "Shaped", "the fields of a shape start"),
        };
        let first = self.peek_span();
        let doubled = self.tokens.get(self.pos + 1) == Some(&Token::Colon) && self.spans.get(self.pos + 1).is_some_and(|next| next.line == first.line && next.col == first.col + 1);
        if !doubled {
            return Err(Error::at(first, format!("a {} {} with '::' after its name, as in '{} {}::{}'", what, does, what, name, other)));
        }
        self.advance(); // :
        self.advance(); // :
        let mut after = Vec::new();
        loop {
            let at = self.peek_span();
            if matches!(self.peek(), Token::Newline | Token::Eof) {
                let expected = match (after.is_empty(), what) {
                    (true, "kind") => "the kind it imitates or the pattern it follows after '::'",
                    (true, _) => "the pattern it follows after '::'",
                    (false, "kind") => "another kind or pattern after '&'",
                    (false, _) => "another pattern after '&'",
                };
                return Err(Error::at(at, format!("expected {}, as in '{} {}::{}'", expected, what, name, other)));
            }
            after.push((self.parse_type()?, at));
            if !self.match_token(Token::Amp) {
                break;
            }
        }
        if !matches!(self.peek(), Token::Newline | Token::Eof) {
            let more = if what == "kind" { "another kind or pattern" } else { "another pattern" };
            return Err(self.unexpected(&format!("'&' and {}, or the end of the line: {} on the line after its name", more, members)));
        }
        Ok(after)
    }

    /// `pattern Shaped … fi`, or `priv pattern …`: its fields, then its methods,
    /// each line one header, or several parted by `,`. A line of one header
    /// followed by a line that starts no member (a bare `name()` counts) starts
    /// that method's default body, closed by `fi`; any other is required.
    fn parse_pattern_def(&mut self) -> Result<PatternDef> {
        let private = self.match_token(Token::Priv);
        let opened = self.peek_span();
        self.advance(); // pattern
        let span = self.peek_span();
        let name = self.expect_ident()?;
        if self.check(Token::Lt) {
            return Err(Error::at(self.peek_span(), "a pattern takes no types yet; the types a task takes follow it, as in 'task f<T: Shaped>(x: T)'"));
        }
        if self.check(Token::Colon) {
            return Err(Error::at(self.peek_span(), format!("a pattern follows no other pattern yet; ask for the other's methods in '{}' too, or have a shape follow both, joined with '&'", name)));
        }
        if !matches!(self.peek(), Token::Newline | Token::Eof) {
            return Err(self.unexpected("the end of the line: the members of a pattern start on the line after its name"));
        }
        self.skip_newlines();
        let mut fields: Vec<Field> = Vec::new();
        let mut methods: Vec<TaskDef> = Vec::new();
        // The last default body that started with a bare call, `reset()`.
        let mut last_bare: Option<(Span, String, String)> = None;
        while !self.check(Token::Fi) && !self.is_at_end() && !self.at_item_start() {
            self.reject_pattern_words()?;
            if matches!(self.peek(), Token::Ident(_)) && self.tokens.get(self.pos + 1) == Some(&Token::Colon) {
                if let Some(first) = methods.first() {
                    let method = first.owner.as_ref().map_or("", |owner| owner.name.as_str());
                    return Err(Error::at(self.peek_span(), format!("every field of a pattern goes above its methods, and method '{}' is declared at {}:{}", method, first.span.line, first.span.col)));
                }
                let field = self.parse_field(Slot::Field)?;
                reject_member_named_this(&field.name, field.span, "a field")?;
                if let Some(before) = fields.iter().find(|before| before.name == field.name) {
                    return Err(Error::at(field.span, format!("field '{}' is already defined at {}:{}", field.name, before.span.line, before.span.col)));
                }
                if field.default.is_some() {
                    return Err(Error::at(field.ty_span, "a pattern's field names no value it falls back on; the shape following the pattern declares the field, and may give it one"));
                }
                fields.push(field);
                if !matches!(self.peek(), Token::Newline | Token::Eof) {
                    return Err(self.unexpected("the end of the line: a pattern's field is written on a line of its own"));
                }
                self.skip_newlines();
                continue;
            }
            let mut line = vec![self.parse_pattern_header(&fields, &methods)?];
            while self.match_token(Token::Comma) {
                let mut above = methods.clone();
                above.extend(line.iter().cloned());
                line.push(self.parse_pattern_header(&fields, &above)?);
            }
            if !matches!(self.peek(), Token::Newline | Token::Eof) {
                return Err(self.unexpected("',' and another method, or the end of the line"));
            }
            self.skip_newlines();
            if line.len() == 1 && !self.check(Token::Fi) && !self.is_at_end() && !self.at_item_start() && !self.at_pattern_member() {
                // `reset()` alone on the line after a header starts its body;
                // a mistake after it may mean two headers were meant.
                let bare = matches!((self.tokens.get(self.pos), self.tokens.get(self.pos + 1), self.tokens.get(self.pos + 2), self.tokens.get(self.pos + 3)), (Some(Token::Ident(_)), Some(Token::LParen), Some(Token::RParen), Some(Token::Newline | Token::Eof)));
                let bare = bare.then(|| (self.peek_span(), match self.peek() { Token::Ident(name) => name.clone(), _ => String::new() }));
                let lambdas = self.lambdas.len();
                let header = line.first().map(|method| (method.span, method.owner.as_ref().map_or(String::new(), |owner| owner.name.clone())));
                let read = self.parse_block().and_then(|body| match &header {
                    Some((span, _)) => self.expect_fi("method", *span).map(|()| body),
                    None => Ok(body),
                });
                let body = match (read, &bare, &header) {
                    (Ok(body), _, _) => body,
                    (Err(_), Some((at, called)), Some((_, method))) => return Err(Error::at(*at, two_headers(called, method))),
                    (Err(error), _, _) => return Err(error),
                };
                if let Some(Item::Task(lambda)) = self.lambdas.get(lambdas) {
                    return Err(Error::at(lambda.span, "a pattern's default method holds no lambda yet; write a task outside the pattern, and name it here"));
                }
                if let Some(method) = line.first_mut() {
                    method.body = body;
                }
                if let (Some((at, called)), Some((_, method))) = (&bare, &header) {
                    last_bare = Some((*at, called.clone(), method.clone()));
                }
                self.skip_newlines();
            }
            methods.append(&mut line);
        }
        if !self.check(Token::Fi)
            && let Some((at, called, method)) = last_bare
        {
            return Err(Error::at(at, two_headers(&called, &method)));
        }
        self.expect_fi("pattern", opened)?;
        if fields.is_empty() && methods.is_empty() {
            return Err(Error::at(span, format!("pattern '{}' asks for nothing; give it a method a shape following it has, as in 'area() -> float'", name)));
        }
        Ok(PatternDef { name, fields, methods, span, private })
    }

    /// Refuses what a pattern's member doesn't take, at its start.
    fn reject_pattern_words(&self) -> Result<()> {
        let at = self.peek_span();
        match self.peek() {
            Token::Priv => Err(Error::at(at, "a pattern's members are as visible as the pattern; a method a pattern asks for is never 'priv', since any code using the pattern calls it")),
            Token::Shared => Err(Error::at(at, "a pattern has no shared fields or methods yet; it asks for what each value of a shape has")),
            Token::Dollar => Err(Error::at(at, "a pattern's method takes no decorators; the method a shape writes for it may")),
            Token::New => Err(Error::at(at, "a pattern has no shared methods yet, so no 'new'")),
            Token::Ident(word) if word == READ && matches!(self.tokens.get(self.pos + 1), Some(Token::Ident(_))) => {
                Err(Error::at(at, format!("a pattern's field is written without '{}'; the shape following it says who may change it", READ)))
            }
            _ => Ok(()),
        }
    }

    /// Whether the cursor starts a pattern's member rather than a statement: a
    /// field, `name: type`, or a method's header that no statement can be, with
    /// `alters`, a parameter, results, or another header after a `,`. A bare
    /// `name()` alone on its line is a statement here.
    fn at_pattern_member(&self) -> bool {
        let pos = self.pos;
        let token = |at: usize| self.tokens.get(at);
        match (token(pos), token(pos + 1)) {
            (Some(Token::Ident(word)), Some(Token::Ident(_))) if word == ALTERS => true,
            (Some(Token::Ident(_)), Some(Token::Colon | Token::Lt)) => true,
            (Some(Token::Ident(_)), Some(Token::LParen)) => {
                if matches!(token(pos + 2), Some(Token::Flex | Token::Bind | Token::Fixed)) || matches!((token(pos + 2), token(pos + 3)), (Some(Token::Ident(_)), Some(Token::Colon))) {
                    return true;
                }
                let mut depth = 0usize;
                let mut at = pos + 1;
                while let Some(token) = token(at) {
                    match token {
                        Token::LParen => depth += 1,
                        Token::RParen => {
                            depth = depth.saturating_sub(1);
                            if depth == 0 {
                                break;
                            }
                        }
                        Token::Newline | Token::Eof => return false,
                        _ => {}
                    }
                    at += 1;
                }
                matches!(token(at + 1), Some(Token::Arrow | Token::Comma))
            }
            _ => false,
        }
    }

    /// A method's header in a pattern, which `parse_method` reads in a shape:
    /// `alters` maybe, its name, its parameters, and its results. It becomes a
    /// task named as the method, whose first parameter holds the value it is
    /// called on, of the type following the pattern; its body is read after.
    fn parse_pattern_header(&mut self, fields: &[Field], methods: &[TaskDef]) -> Result<TaskDef> {
        let opened = self.peek_span();
        let alters_span = match self.peek() {
            Token::Ident(word) if word == ALTERS => {
                self.advance();
                Some(opened)
            }
            _ => None,
        };
        let span = self.peek_span();
        if !matches!(self.peek(), Token::Ident(_)) {
            return Err(self.unexpected("a method's header, as in 'area() -> float', or a field, as in 'name: text'"));
        }
        let name = self.expect_ident()?;
        reject_member_named_this(&name, span, "a method")?;
        if is_builtin_member(&name) || name == LEN {
            return Err(Error::at(span, format!("'{}' is a reserved name: every value or slice already answers to it, so a method can't be called that", name)));
        }
        if let Some(field) = fields.iter().find(|field| field.name == name) {
            return Err(Error::at(
                span,
                format!("'{0}' is already a field of this pattern, at {1}:{2}; both are written '.{0}', so a field and a method can't share a name", name, field.span.line, field.span.col),
            ));
        }
        if let Some(before) = methods.iter().find(|before| before.owner.as_ref().is_some_and(|before| before.name == name)) {
            return Err(Error::at(span, format!("method '{}' is already defined at {}:{}", name, before.span.line, before.span.col)));
        }
        if self.check(Token::Lt) {
            return Err(Error::at(self.peek_span(), "a method takes no types of its own yet; write a task that takes them, and call it from the method"));
        }
        if !self.check(Token::LParen) {
            return Err(self.unexpected(&format!("'(' and the parameters of method '{}', as in '{}() -> int'", name, name)));
        }
        let mut params = vec![Field {
            name: THIS.to_string(),
            ty: Type::Named(THIS_TYPE.to_string(), None),
            span,
            ty_span: span,
            private: false,
            read: false,
            default_region: false,
            passing: Passing::Copy,
            default: None,
        }];
        params.extend(self.parse_field_list(Slot::Param)?);
        if let Some(param) = params.iter().skip(1).find(|param| param.default.is_some()) {
            return Err(Error::at(param.span, "a pattern's method names no value a parameter falls back on yet; every call gives each one"));
        }
        if let Some(param) = params.iter().skip(1).find(|param| param.ty == Type::Region && param.name == THIS_WORD) {
            return Err(Error::at(param.span, format!("'{0}' is the region the value this method is called on lives in, written '@{0}', so a region parameter can't take the name", THIS_WORD)));
        }
        let mut results = Vec::new();
        let ret = if self.match_token(Token::Arrow) {
            let outer = std::mem::replace(&mut self.in_pattern, true);
            let read = self.parse_results(&mut results);
            self.in_pattern = outer;
            read?
        } else {
            Type::Nothing
        };
        Ok(TaskDef {
            name: name.clone(),
            types: Vec::new(),
            given: Vec::new(),
            params,
            ret,
            results,
            body: Vec::new(),
            span,
            private: false,
            native: false,
            owner: Some(Owner { name, alters: alters_span.is_some(), alters_span, shared: false }),
            decorators: Vec::new(),
            passes: Vec::new(),
            wrappers: Vec::new(),
            kept: None,
            shortcut: false,
        })
    }

    /// Whether the `(` after a name inside a kind holds a variant's values rather
    /// than a method's parameters, which are written the same way. A variant's
    /// line ends after them and it carries at least one; a method is told by
    /// `alters`, by naming what it sends, by taking no parameters, or by another
    /// method above it. A kind's first method that takes parameters and sends
    /// nothing is the one thing this can't tell apart: it names `-> nothing`.
    fn at_variant_payload(&self, methods: &[TaskDef]) -> bool {
        if !methods.is_empty() || matches!(self.peek(), Token::Ident(word) if word == ALTERS) {
            return false;
        }
        let mut depth = 0usize;
        let mut named = false;
        for pos in self.pos + 1.. {
            match self.tokens.get(pos) {
                Some(Token::LParen) => depth += 1,
                Some(Token::RParen) => {
                    depth = match depth.checked_sub(1) {
                        // A variant's line ends after its values.
                        Some(0) => return named && matches!(self.tokens.get(pos + 1), Some(Token::Newline | Token::Eof) | None),
                        Some(left) => left,
                        None => return false,
                    };
                }
                // A variant carries named values, `Circle(r: float)`.
                Some(Token::Colon) if depth == 1 => named = true,
                Some(_) => {}
                None => return false,
            }
        }
        false
    }

    /// A failure to read the values in parentheses inside a kind, where a
    /// variant and a method are written alike, with what tells them apart.
    fn kind_member(error: Error) -> Error {
        if !error.message.starts_with("expected ':'") {
            return error;
        }
        Error {
            message: format!(
                "{}; inside a kind a name with values in parentheses is a variant, which carries named values, so a method that takes values says what it sends, as in 'name(…) -> nothing'",
                error.message
            ),
            ..error
        }
    }

    /// `name: type`, `r: region default` in a task's parameters (see
    /// `ast::DEFAULT`), and `size: int = 0` for a shape's field, which `slot`
    /// says which of these is being read.
    fn parse_field(&mut self, slot: Slot) -> Result<Field> {
        let span = self.peek_span();
        if self.check(Token::Priv) {
            return Err(Error::at(span, "only a shape's field can be 'priv', not a parameter or a variant's value"));
        }
        // A name after `read` is a field it marks, which a parameter and a
        // variant's value never are (`parse_shape` reads the ones that are).
        if matches!(self.peek(), Token::Ident(word) if word == READ) && matches!(self.tokens.get(self.pos + 1), Some(Token::Ident(_))) {
            return Err(Error::at(span, format!("only a shape's field can be '{}', not a parameter or a variant's value", READ)));
        }
        let passing = self.match_passing(slot)?;
        let name = self.expect_ident()?;
        self.expect(Token::Colon)?;
        let ty_span = self.peek_span();
        let mut ty = self.parse_type()?;
        // `args: P...`: the values of a group of types the task takes.
        if self.check(Token::Ellipsis) {
            ty = self.group_type(ty, slot == Slot::Param, "a parameter's type, as in 'args: P...'")?;
        }
        let default_region = self.match_default(slot == Slot::Param)?;
        let default = self.match_field_default(slot)?;
        Ok(Field { name, ty, span, ty_span, private: false, read: false, default_region, passing, default })
    }

    /// `flex`, `flex&`, `fixed`, `fixed&`, `bind` or `bind&` before a task's
    /// parameter, which makes it the task's own copy to change, the caller's
    /// place, always a copy, the caller's place only read, or a bind place whose
    /// contents may change but which is never assigned as a whole (see
    /// `Passing`). A shape's field and a variant's value change with the value
    /// they are part of, and say so.
    fn match_passing(&mut self, slot: Slot) -> Result<Passing> {
        let (base, word) = match self.peek() {
            Token::Flex => (Passing::Flex, "flex"),
            Token::Fixed => (Passing::Fixed, "fixed"),
            Token::Bind => (Passing::Bind, "bind"),
            _ => return Ok(Passing::Copy),
        };
        let at = self.peek_span();
        match slot {
            Slot::Param => {}
            Slot::Field if matches!(base, Passing::Flex | Passing::Bind) => {
                return Err(Error::at(
                    at,
                    "only a parameter can be 'flex' or 'bind'; a shape's field changes where the value it is part of is held, as in 'flex p = Point {…}' then 'p.x = 1'",
                ));
            }
            Slot::Field => {
                return Err(Error::at(at, "only a parameter can be 'fixed'; a shape's field is part of the value holding it, and changes where that value is held"));
            }
            Slot::Value => {
                return Err(Error::at(at, format!("only a parameter can be '{}', not a variant's value", word)));
            }
        }
        self.advance();
        let by_place = if self.check(Token::Amp) {
            // `flex&` is one word: `flex &x` reads as the `&` of a call.
            if !self.amp_against_word() {
                return Err(Error::at(self.peek_span(), format!("a parameter that is the caller's place is written '{0}& x', with '&' against '{0}'", word)));
            }
            self.advance();
            true
        } else {
            false
        };
        let passing = match base {
            Passing::Flex if by_place => Passing::FlexRef,
            Passing::Flex => Passing::Flex,
            Passing::Fixed if by_place => Passing::FixedRef,
            Passing::Fixed => Passing::Fixed,
            Passing::Bind if by_place => Passing::BindRef,
            Passing::Bind => Passing::Bind,
            _ => unreachable!(),
        };
        if matches!(self.peek(), Token::Flex | Token::Fixed | Token::Bind) {
            return Err(Error::at(
                self.peek_span(),
                format!("'{}' is already written here: a parameter is written with one of 'flex', 'flex&', 'fixed', 'fixed&', 'bind' and 'bind&', or none", passing.word()),
            ));
        }
        if self.check(Token::Amp) {
            return Err(Error::at(self.peek_span(), format!("'&' marks a place given at a call, as in 'f(&a)'; a parameter's name is written without it, as in '{}& a: int'", word)));
        }
        Ok(passing)
    }

    /// The word `read` before a shape's field, which any file may read and only
    /// its own file may assign (see `ast::READ`). The word is a marker only where
    /// a name follows it: a field called `read` is written `read:` and a method
    /// `read(`, so both still work. `private` says `priv` was read just before,
    /// which a field is instead of `read`, not as well.
    fn match_read(&mut self, private: bool) -> Result<bool> {
        if !matches!(self.peek(), Token::Ident(word) if word == READ) || !matches!(self.tokens.get(self.pos + 1), Some(Token::Ident(_))) {
            return Ok(false);
        }
        let at = self.peek_span();
        // `read step()`: a method says what it changes with `alters`.
        if matches!(self.tokens.get(self.pos + 2), Some(Token::LParen | Token::Lt)) {
            return Err(Error::at(at, format!("only a field can be '{}', not a method; a method that changes the value it is called on says '{}'", READ, ALTERS)));
        }
        if private {
            return Err(Error::at(
                at,
                format!("a field is 'priv' or '{0}', not both: 'priv' keeps it to its own file, and '{0}' lets any file read it", READ),
            ));
        }
        self.advance();
        if matches!(self.peek(), Token::Ident(word) if word == READ) && matches!(self.tokens.get(self.pos + 1), Some(Token::Ident(_))) {
            return Err(Error::at(self.peek_span(), format!("'{}' is already written here: a field is read-only or it isn't", READ)));
        }
        Ok(true)
    }

    /// `= value` after a shape's field or a task's parameter: the value a
    /// literal that leaves the field out gives it, or a call that leaves the
    /// parameter out. A variant's value has none, and says so.
    fn match_field_default(&mut self, slot: Slot) -> Result<Option<Expr>> {
        let at = self.peek_span();
        if !self.match_token(Token::Assign) {
            return Ok(None);
        }
        if slot == Slot::Value {
            return Err(Error::at(at, "only a shape's field or a task's parameter can be given a value to fall back on, not a variant's value"));
        }
        Ok(Some(self.parse_expr()?))
    }

    /// Checks the values `params` fall back on, which only the last of them
    /// have: a call leaves out a tail of the list, never a value between two it
    /// gives. A place, a region, and a group's values have none to fall back on.
    fn check_param_defaults(params: &[Field]) -> Result<()> {
        let Some(first) = params.iter().position(|param| param.default.is_some()) else { return Ok(()) };
        for param in params.iter().filter(|param| param.default.is_some()) {
            let at = param.default.as_ref().map_or(param.span, |value| value.span);
            if param.passing.is_ref() {
                return Err(Error::at(
                    at,
                    format!("'{}' is the caller's place, '{}', which a call names with '&'; a place has no value to fall back on", param.name, param.passing.word()),
                ));
            }
            if param.ty == Type::Region {
                return Err(Error::at(at, format!("'{}' is a region, which a call names; a region has no value to fall back on", param.name)));
            }
        }
        if let Some(group) = params.iter().find(|param| matches!(param.ty, Type::Group(_))) {
            let at = params.get(first).map_or(group.span, |param| param.default.as_ref().map_or(param.span, |value| value.span));
            return Err(Error::at(
                at,
                format!(
                    "a task that takes a group, '{}', takes no values to fall back on: the values a call gives after the others would be the group's or these, and a group can already be given none",
                    group.name
                ),
            ));
        }
        let Some(after) = params.iter().skip(first).find(|param| param.default.is_none()) else { return Ok(()) };
        let before = params.get(first).map_or("", |param| param.name.as_str());
        Err(Error::at(
            after.span,
            format!(
                "'{}' has no value to fall back on, and comes after '{}', which has one; the parameters with values to fall back on come last, so that a call leaves out only the last ones",
                after.name, before
            ),
        ))
    }

    /// The word `default` after a task's parameter, which makes the region it
    /// takes its body's default (the checker refuses it on anything else). A
    /// field or a variant's value is never one, and says so.
    fn match_default(&mut self, params: bool) -> Result<bool> {
        if !matches!(self.peek(), Token::Ident(word) if word == DEFAULT) {
            return Ok(false);
        }
        if !params {
            return Err(Error::at(
                self.peek_span(),
                format!("only a region a task takes can be a default region, as in 'r: region {}'", DEFAULT),
            ));
        }
        self.advance();
        if matches!(self.peek(), Token::Ident(word) if word == DEFAULT) {
            return Err(Error::at(self.peek_span(), format!("'{}' is already written here: a region is the default or it isn't", DEFAULT)));
        }
        Ok(true)
    }

    /// `(name: type, ...)`; `slot` as in `parse_field`.
    fn parse_field_list(&mut self, slot: Slot) -> Result<Vec<Field>> {
        self.expect(Token::LParen)?;
        let mut fields = Vec::new();
        if !self.check(Token::RParen) {
            loop {
                fields.push(self.parse_field(slot)?);
                if !self.match_token(Token::Comma) {
                    break;
                }
            }
        }
        self.expect(Token::RParen)?;
        if slot == Slot::Param {
            Self::check_param_defaults(&fields)?;
        }
        Ok(fields)
    }

    /// A type, with an optional error type: `int`, `^Node@r`, `maybe int`,
    /// `int ~ ParseError`. `^` and `maybe` bind tighter than `@`, and `@`
    /// tighter than `~`.
    fn parse_type(&mut self) -> Result<Type> {
        let start = self.peek_span();
        self.type_parts.clear();
        let ty = self.parse_type_parts();
        self.type_places.insert(start.place_key(), std::mem::take(&mut self.type_parts));
        ty
    }

    fn parse_type_parts(&mut self) -> Result<Type> {
        // The functions reading a type keep little on the stack, each part in
        // one of its own: a type nests in another once per level.
        let value = self.parse_simple_type()?;
        self.no_or_in_type()?;
        if !self.check(Token::Tilde) {
            return Ok(value);
        }
        self.parse_error_side(value)
    }

    /// At the `~` after `value`, a type: the error type, and the whole type.
    fn parse_error_side(&mut self, value: Type) -> Result<Type> {
        self.advance(); // ~
        self.type_parts.push(("~".to_string(), self.peek_span()));
        let error = self.parse_simple_type()?;
        self.no_or_in_type()?;
        if self.check(Token::Tilde) {
            return Err(Error::at(self.peek_span(), "a type has one error type: '~' can't appear twice"));
        }
        Ok(Type::Or(Box::new(value), Box::new(error)))
    }

    /// `or` where a type's `~` belongs. Every program written before the
    /// separator changed says `or` there, so it is worth teaching rather than
    /// reporting as an unexpected word.
    fn no_or_in_type(&mut self) -> Result<()> {
        if self.check(Token::Or) {
            return Err(Error::at(
                self.peek_span(),
                "a type writes its error side with '~', as in 'int ~ ParseError'; 'or' is only the boolean operator",
            ));
        }
        Ok(())
    }

    /// A type without `~`, and the region it lives in: `maybe ^Node@r` is
    /// `(maybe ^Node)@r`, whose pointer and `Node` both live in `r`.
    fn parse_simple_type(&mut self) -> Result<Type> {
        let span = self.peek_span();
        let ty = self.parse_prefixed_type()?;
        if !self.match_token(Token::AtSign) {
            return Ok(ty);
        }
        self.parse_type_region(span, ty)
    }

    /// After the `@` of `ty`, written at `span`: the region it lives in, and
    /// the type in it.
    fn parse_type_region(&mut self, span: Span, ty: Type) -> Result<Type> {
        let region_span = self.peek_span();
        let region = self.expect_ident()?;
        self.type_parts.push((format!("@{}", region), region_span));
        if mentions_region(&ty) {
            return Err(Error::at(span, "a region doesn't live in another region"));
        }
        // Everything a value in a region holds is in that region, so a region
        // written inside must be the same one. A tuple or an array keeps no
        // region of its own, so only this sees it; the checker reports the
        // types that keep one, naming what is in the other region.
        if let Some(inner) = ty.regions().into_iter().find(|inner| *inner != region).filter(|_| keeps_no_region(&ty)) {
            let at = self.type_parts.iter().find(|(part, _)| *part == format!("@{}", inner)).map_or(region_span, |(_, at)| *at);
            return Err(Error::at(at, format!("everything a value in region '{0}' holds is in '{0}'; remove '@{1}'", region, inner)));
        }
        // A part that already names the region holds text or pointers itself,
        // as `(int, text@r)@r` and `[text@r]@r` do, even when nothing is left
        // to fill in.
        let mut named = !ty.regions().is_empty();
        let ty = ty.map_regions_of_names(&mut |_| {
            named = true;
            Some(region.clone())
        });
        if !named {
            return Err(Error::at(region_span, format!("'{}' holds no pointers or text, so it lives in no region; remove '@{}'", ty, region)));
        }
        Ok(ty)
    }

    /// A type without `or` or `@`: a name, after any `^` and `maybe`, or an
    /// array or slice in brackets.
    fn parse_prefixed_type(&mut self) -> Result<Type> {
        let span = self.peek_span();
        if self.match_token(Token::Region) {
            self.type_parts.push(("region".to_string(), span));
            return Ok(Type::Region);
        }
        if self.check(Token::LBracket) && self.at_kept() {
            self.enter()?;
            let closure = self.parse_closure_type(span);
            self.leave(1);
            return closure;
        }
        if self.match_token(Token::LBracket) {
            self.enter()?;
            let items = self.parse_bracketed_type();
            self.leave(1);
            return items;
        }
        if self.match_token(Token::LParen) {
            self.enter()?;
            let items = self.parse_tuple_type(span);
            self.leave(1);
            return items;
        }
        let prefix = if self.match_token(Token::Caret) {
            Some(Token::Caret)
        } else if self.match_token(Token::Maybe) {
            self.type_parts.push(("maybe".to_string(), span));
            Some(Token::Maybe)
        } else {
            None
        };
        if let Some(prefix) = prefix {
            self.enter()?;
            let inner = Box::new(self.parse_prefixed_type()?);
            self.leave(1);
            return Ok(if prefix == Token::Caret { Type::Ptr(inner, None) } else { Type::Maybe(inner) });
        }
        self.parse_named_type(span)
    }

    /// Whether the cursor, at `[`, starts what a lambda keeps: `[@r]`,
    /// `[flex @r]`, `[bind @r]`, or a place first, `[flex& count, @r]`, before a
    /// lambda or its type.
    fn at_kept(&self) -> bool {
        match self.tokens.get(self.pos + 1) {
            Some(Token::AtSign) => true,
            Some(Token::Flex) => matches!(self.tokens.get(self.pos + 2), Some(Token::AtSign | Token::Amp)),
            Some(Token::Bind) => matches!(self.tokens.get(self.pos + 2), Some(Token::AtSign | Token::Amp)),
            Some(Token::Fixed) => self.tokens.get(self.pos + 2) == Some(&Token::Amp),
            _ => false,
        }
    }

    /// At `[`, what a lambda keeps: the region, `@r`, `flex @r` or `bind @r`,
    /// and the variables it keeps as places, `flex& count`, `bind& count` and
    /// `fixed& big`, in any order, then `]`.
    fn parse_kept(&mut self) -> Result<(String, KeptMode, Span, Vec<KeptPlace>)> {
        let opened = self.peek_span();
        self.advance(); // [
        let mut region: Option<(String, KeptMode, Span)> = None;
        let mut places: Vec<KeptPlace> = Vec::new();
        loop {
            let at = self.peek_span();
            // `fixed` only starts a place, `fixed& big`; a region is `@r`.
            let fixed = self.match_token(Token::Fixed);
            let mode = if fixed {
                KeptMode::Plain
            } else if self.match_token(Token::Flex) {
                KeptMode::Flex
            } else if self.match_token(Token::Bind) {
                KeptMode::Bind
            } else {
                KeptMode::Plain
            };
            if (fixed || mode != KeptMode::Plain) && self.match_token(Token::Amp) {
                let span = self.peek_span();
                let name = self.expect_ident()?;
                let passing = match mode {
                    KeptMode::Flex => Passing::FlexRef,
                    KeptMode::Bind => Passing::BindRef,
                    KeptMode::Plain => Passing::FixedRef,
                };
                if matches!(self.peek(), Token::Dot | Token::LBracket) {
                    return Err(Error::at(span, format!("a lambda keeps a variable as a place, not a field or an item of one; keep the variable, '{} {}'", passing.word(), name)));
                }
                if places.iter().any(|place| place.name == name) {
                    return Err(Error::at(span, format!("'{}' is already in this lambda's list; each place is kept once", name)));
                }
                places.push(KeptPlace { name, passing, span });
            } else if !fixed && self.match_token(Token::AtSign) {
                let span = self.peek_span();
                let name = self.expect_ident()?;
                if region.is_some() {
                    return Err(Error::at(at, "a lambda keeps what it uses in one region, so its list names one, as in '[flex& count, @r]'"));
                }
                region = Some((name, mode, span));
            } else {
                return Err(Error::at(
                    at,
                    "a lambda's list names the region it keeps what it uses in, '@r', 'flex @r' or 'bind @r', and the variables it keeps as places, 'flex& count', 'bind& count' or 'fixed& big', as in '[flex& count, @r]'",
                ));
            }
            if !self.match_token(Token::Comma) {
                break;
            }
        }
        if !self.match_token(Token::RBracket) {
            return Err(self.unexpected("',' and another item, or ']' to close what the lambda keeps"));
        }
        let Some((region, mode, at)) = region else {
            return Err(Error::at(opened, "a lambda keeps what it uses in a region, which its list names, as in '[flex& count, @r]'"));
        };
        Ok((region, mode, at, places))
    }

    /// `[@r](int) -> int`, at `[` written at `span`: the type of a lambda that
    /// keeps what it uses in `r` (see `Type::Closure`).
    fn parse_closure_type(&mut self, span: Span) -> Result<Type> {
        let (region, mode, at, places) = self.parse_kept()?;
        if let Some(place) = places.first() {
            return Err(Error::at(
                place.span,
                format!("the type of a lambda names only the region it keeps what it uses in, '[@{}]'; the places it keeps are its own, so its type doesn't name '{}'", region, place.name),
            ));
        }
        self.type_parts.push((format!("@{}", region), at));
        let mode_word = match mode {
            KeptMode::Plain => "",
            KeptMode::Flex => "flex ",
            KeptMode::Bind => "bind ",
        };
        let written = format!("[{}@{}]", mode_word, region);
        let opened = self.peek_span();
        let task = match self.match_token(Token::LParen) {
            true => self.parse_tuple_type(opened)?,
            false => Type::Unknown,
        };
        let Type::Task(task) = task else {
            return Err(Error::at(span, format!("the type of a lambda that keeps what it uses is a task's type after '{0}', as in '{0}(int) -> int'", written)));
        };
        if task.taken.iter().any(|how| *how == Taken::Region(region.clone())) {
            return Err(Error::at(at, format!("'{}' keeps what the lambda uses in '{}', so a region the lambda takes is named otherwise, as in '@a'", written, region)));
        }
        Ok(Type::Closure(Box::new(ClosureType { task: *task, region, mode })))
    }

    /// A type named at `span` and the cursor, with the types a shape is given.
    fn parse_named_type(&mut self, span: Span) -> Result<Type> {
        let name = self.expect_ident()?;
        let mut name = self.qualified(name)?;
        // `geometry.Point` for a module this file doesn't include: a dotted name
        // the resolver reports as naming nothing.
        while self.check(Token::Dot) && matches!(self.tokens.get(self.pos + 1), Some(Token::Ident(_))) {
            self.advance(); // .
            name = format!("{}{}{}", name, MODULE_SEPARATOR, self.expect_ident()?);
        }
        self.type_parts.push((name.clone(), span));
        if self.check(Token::NotSimilar) {
            return Err(Error::at(self.peek_span(), "'< … >' names the types a shape is given, as in '<int>'; leave it out for a shape that takes none"));
        }
        if self.check(Token::Lt) {
            if Type::builtin(&name).is_some() {
                return Err(Error::at(self.peek_span(), format!("'{}' takes no types, so '< … >' has nothing to name", name)));
            }
            let types = self.parse_given_types()?;
            return Ok(Type::Applied(Box::new(Applied { name, types, region: None })));
        }
        Ok(Type::builtin(&name).unwrap_or(Type::Named(name, None)))
    }

    /// After the `<` of the types a shape is given, inside a written type: the
    /// types and `>`. (The parts of these types belong to the type being read,
    /// so they are read with `parse_type_parts` rather than `parse_type`, which
    /// would start a written type of its own.)
    fn parse_given_types(&mut self) -> Result<Vec<Type>> {
        self.advance(); // <
        let mut types = Vec::new();
        loop {
            if self.check(Token::Gt) {
                return Err(Error::at(self.peek_span(), "'< … >' names the types a shape is given, as in '<int>'; leave it out for a shape that takes none"));
            }
            self.enter()?;
            let ty = self.parse_type_parts();
            self.leave(1);
            types.push(ty?);
            if !self.match_token(Token::Comma) {
                break;
            }
        }
        if !self.match_token(Token::Gt) {
            return Err(self.unexpected("',' and another type, or '>' to close the types"));
        }
        Ok(types)
    }

    /// After a type's `(` at `opened`: `T, U, …)`, a tuple of two or more items,
    /// or one type alone, which the parentheses only group — the way to write
    /// `maybe (int ~ text)`, since `maybe int ~ text` reads as `(maybe int) ~ text`.
    fn parse_tuple_type(&mut self, opened: Span) -> Result<Type> {
        let mut items = Vec::new();
        // How each item is given, which only a task's type says (see `Taken`),
        // with where the first that isn't a value is written.
        let mut taken = Vec::new();
        let mut marked: Option<(Span, String)> = None;
        // `() -> int`: a task that takes nothing.
        if self.check(Token::RParen) && self.tokens.get(self.pos + 1) == Some(&Token::Arrow) {
            self.advance(); // )
            return self.parse_task_type(items, taken);
        }
        loop {
            // `()` and `(int,)`: a missing item, rather than a missing name;
            // `(int, text,)`: the comma is the one after the last item.
            if self.check(Token::RParen) {
                return match items.as_slice() {
                    [] | [_] => Err(Error::at(opened, TUPLE_TYPE_ITEMS)),
                    _ => Err(Error::at(self.peek_span(), TUPLE_TRAILING_COMMA)),
                };
            }
            let at = self.peek_span();
            // `fixed T` reads as `T`, but only a task's type takes a word.
            let fixed_value = self.check(Token::Fixed) && !matches!(self.tokens.get(self.pos + 1), Some(Token::Amp));
            let how = self.parse_taken()?;
            if (how != Taken::Value || fixed_value) && marked.is_none() {
                let written = match &how {
                    Taken::Region(name) => format!("@{}", name),
                    Taken::Place(passing) => passing.word().to_string(),
                    Taken::Flex => Passing::Flex.word().to_string(),
                    Taken::Bind => Passing::Bind.word().to_string(),
                    Taken::Value => Passing::Fixed.word().to_string(),
                };
                marked = Some((at, written));
            }
            // `@r`: a region, whose name is all there is of it.
            let item = if matches!(how, Taken::Region(_)) { Type::Region } else { self.parse_type_parts()? };
            taken.push(how);
            // `(P...) -> R`: a task's type taking a group of types, all it takes.
            if self.check(Token::Ellipsis) {
                let written = match taken.last() {
                    Some(Taken::Region(name)) => Some(format!("@{}", name)),
                    Some(Taken::Place(passing)) => Some(passing.word().to_string()),
                    Some(Taken::Flex) => Some(Passing::Flex.word().to_string()),
                    Some(Taken::Bind) => Some(Passing::Bind.word().to_string()),
                    _ => None,
                };
                match written {
                    Some(written) if written.starts_with('@') => {
                        return Err(Error::at(at, format!("a group never holds a region, so '{}' can't come before '...'; name the region on its own beside the group, as in '({}, P...) -> R'", written, written)));
                    }
                    Some(written) => {
                        return Err(Error::at(at, format!("a group's values are taken as the task given for it takes them, so '{}' can't come before the group: '(P...) -> R' fits 'put(flex& n: int)' too", written)));
                    }
                    None => {}
                }
                // One group, with any values the task takes alone around it,
                // `(@a, P...) -> R`.
                let first = !items.iter().any(|item| matches!(item, Type::Group(_)));
                if !first {
                    return Err(Error::at(self.peek_span(), "a task's type takes one group of types, as in '(int, P...) -> R'; the values around it are named one by one"));
                }
                let group = self.group_type(item, true, "")?;
                items.push(group);
            } else {
                items.push(item);
            }
            if !self.match_token(Token::Comma) {
                break;
            }
        }
        if !self.match_token(Token::RParen) {
            return Err(self.unexpected("',' and another item, or ')' to close the tuple"));
        }
        // `(int, text) -> int`: a task's type.
        if self.check(Token::Arrow) {
            return self.parse_task_type(items, taken);
        }
        if let Some(Type::Group(group)) = items.iter().find(|item| matches!(item, Type::Group(_))) {
            return Err(Error::at(opened, format!("a group of types, '{}...', is only part of what a task's type takes, as in '({}...) -> R' or '(int, {}...) -> R'", group, group, group)));
        }
        if let Some((at, written)) = marked {
            return Err(Error::at(at, format!("'{}' says how a task's type takes a parameter, as in '(flex& int) -> nothing', '(flex [int]@r) -> nothing' or '(@r, int) -> [int]@r', and these parentheses hold no task's type: a '->' follows a task's", written)));
        }
        // One item, with no comma after it (which the loop reports above): the
        // parentheses group that type rather than making a tuple of it.
        if items.len() == 1 {
            return Ok(items.remove(0));
        }
        Ok(Type::Tuple(items))
    }

    /// `ty` followed by `...`, at the `...`: the group of types `ty` names,
    /// where `allowed` says one may be written, and `where_` is where one is.
    fn group_type(&mut self, ty: Type, allowed: bool, where_: &str) -> Result<Type> {
        let at = self.peek_span();
        let Type::Named(name, None) = &ty else {
            return Err(Error::at(at, format!("'...' after a type marks a group of types the task takes, as in 'P...', and '{}' is no name of one", ty)));
        };
        if !allowed {
            return Err(Error::at(at, format!("a group of types, '{}...', is only {}", name, where_)));
        }
        self.advance(); // ...
        Ok(Type::Group(name.clone()))
    }

    /// At the `->` after the parameters of a task's type, `(int, text) -> int`:
    /// what the task sends, one type, `nothing` when it sends no value. The
    /// arrow is always written, since `(int)` alone is `int` in parentheses.
    fn parse_task_type(&mut self, params: Vec<Type>, taken: Vec<Taken>) -> Result<Type> {
        self.advance(); // ->
        if matches!(self.peek(), Token::Newline | Token::Eof | Token::RParen | Token::RBracket | Token::Comma | Token::Assign | Token::Fi) {
            return Err(Error::at(self.peek_span(), "a task's type names what the task sends after '->', or 'nothing' when it sends no value, as in '(int) -> nothing'"));
        }
        let ret = self.parse_type_parts()?;
        // A group, `(P...) -> R`, is all a task's type takes, and as values.
        let taken = if taken.len() == params.len() { taken } else { vec![Taken::Value; params.len()] };
        Ok(Type::Task(Box::new(TaskType { params, taken, ret })))
    }

    /// How a parameter of a task's type is given, written before its type:
    /// `flex&`, `fixed&` or `bind&` for the caller's place, `@r` for a region
    /// (whose type is then all of it), `flex` or `bind` for a value whose items
    /// the task changes, `fixed` or nothing for a value. The checker reads
    /// `flex` or `bind` before a whole value, which has no items the caller
    /// sees, as nothing (see `Checker::resolve_task_type`).
    fn parse_taken(&mut self) -> Result<Taken> {
        if self.match_token(Token::AtSign) {
            if !matches!(self.peek(), Token::Ident(_)) {
                return Err(self.unexpected("the name of a region after '@', as in '(@r, int) -> [int]@r'"));
            }
            return Ok(Taken::Region(self.expect_ident()?));
        }
        let base = match self.peek() {
            Token::Flex => Passing::Flex,
            Token::Fixed => Passing::Fixed,
            Token::Bind => Passing::Bind,
            _ => return Ok(Taken::Value),
        };
        let at = self.peek_span();
        self.advance();
        // `flex [int]@r`, `bind [int]@r` and `fixed int`: a value, which the
        // task may change through or not; `flex&` and `bind&` are one word, as
        // in a header.
        if !self.check(Token::Amp) {
            return Ok(match base {
                Passing::Flex => Taken::Flex,
                Passing::Bind => Taken::Bind,
                _ => Taken::Value,
            });
        }
        if !self.amp_against_word() {
            let word = base.word();
            return Err(Error::at(at, format!("'{0}&' is written with '&' against '{0}', as in '({0}& int) -> nothing'", word)));
        }
        self.advance(); // &
        if self.check(Token::AtSign) {
            return Err(Error::at(at, "a region is given as it is, so a task's type writes it '@r' alone, never as a place"));
        }
        Ok(Taken::Place(match base {
            Passing::Flex => Passing::FlexRef,
            Passing::Bind => Passing::BindRef,
            _ => Passing::FixedRef,
        }))
    }

    /// After a type's `[`: `T#count]`, an array, `T]`, a slice, or `K: V]`, a
    /// map, with `:V]` for `text: V]`. The item type may have its own region and
    /// error type (`[^int@r#4]`).
    fn parse_bracketed_type(&mut self) -> Result<Type> {
        // `[:float]`: a map whose keys are text.
        if self.match_token(Token::Colon) {
            let value = Box::new(self.parse_type_parts()?);
            self.expect(Token::RBracket)?;
            return Ok(Type::Map(Box::new(Type::Text(None)), value, None));
        }
        let items = Box::new(self.parse_type_parts()?);
        if self.match_token(Token::RBracket) {
            return Ok(Type::Slice(items, None));
        }
        if self.match_token(Token::Colon) {
            let value = Box::new(self.parse_type_parts()?);
            self.expect(Token::RBracket)?;
            return Ok(Type::Map(items, value, None));
        }
        self.parse_array_count(items)
    }

    /// After the item type of an array, `items`: `#count]`.
    fn parse_array_count(&mut self, items: Box<Type>) -> Result<Type> {
        if !self.match_token(Token::Hash) {
            let error = self.unexpected("'#' and a count, as in '[int#4]', ']' for a slice, as in '[int]@r', or ':' and a value type for a map, as in '[text: int]@r'");
            // `[int; 4]`: the `;` started a comment, which ended the line.
            return Err(match self.check(Token::Newline) {
                true => Error { message: format!("{} (';' starts a comment)", error.message), ..error },
                false => error,
            });
        }
        let count_span = self.peek_span();
        let count = match self.peek() {
            Token::Int(n) => *n,
            _ => return Err(Error::at(count_span, "an array's count is a whole number, as in '[int#4]'")),
        };
        self.advance();
        let Some(count) = u64::try_from(count).ok().filter(|count| *count > 0) else {
            return Err(Error::at(count_span, "an array holds at least one value, so its count is 1 or more"));
        };
        self.expect(Token::RBracket)?;
        Ok(Type::Array(items, count))
    }

    fn parse_block(&mut self) -> Result<Block> {
        self.parse_block_until(&[Token::Fi])
    }

    fn parse_block_until(&mut self, stops: &[Token]) -> Result<Block> {
        self.enter()?;
        let mut stmts = Vec::new();
        loop {
            self.skip_newlines();
            // `else`, `case`, `finally`, and `}` can't start a statement: they end an
            // enclosing block, so this one is missing its end.
            let ends_outer = matches!(self.peek(), Token::Else | Token::Case | Token::Finally | Token::RBrace);
            if self.is_at_end() || self.at_item_start() || ends_outer || stops.contains(self.peek()) {
                break;
            }
            self.reject_stray_end_in(stops.contains(&Token::RBrace))?;
            stmts.push(self.parse_stmt()?);
            self.skip_newlines();
        }
        self.leave(1);
        Ok(stmts)
    }

    fn parse_stmt(&mut self) -> Result<Stmt> {
        self.skip_newlines();
        let span = self.peek_span();
        match self.peek() {
            Token::Fixed => self.parse_binding(Mode::Fixed),
            Token::Flex => self.parse_binding(Mode::Flex),
            Token::Bind => self.parse_binding(Mode::Bind),
            Token::When => self.parse_when(),
            Token::Loop | Token::Endless | Token::Each => {
                let stmt = match self.peek() {
                    Token::Loop => self.parse_loop()?,
                    Token::Each => self.parse_each()?,
                    _ => self.parse_endless()?,
                };
                self.loop_with_plan_b(stmt, None, span)
            }
            Token::Block => self.parse_plain_block(),
            Token::Break | Token::Continue => self.parse_jump(),
            Token::Finally => Err(Error::at(span, "'finally' belongs at the end of a loop, before 'fi'")),
            Token::Ident(_) if matches!(self.tokens.get(self.pos + 1), Some(Token::FatArrow)) => {
                let stmt = self.parse_named_loop()?;
                let StmtKind::Named(name, inner) = stmt.kind else { return Ok(stmt) };
                if self.check(Token::LBrace) {
                    return self.loop_with_plan_b(*inner, Some((name, span)), span);
                }
                Ok(Stmt::new(StmtKind::Named(name, inner), stmt.span))
            }
            Token::Match => self.parse_match(),
            Token::Send => self.parse_send(),
            Token::Ident(name) if name == "send" => {
                self.reject_old_send()?;
                self.parse_simple_stmt(span)
            }
            Token::Region => self.parse_region(),
            Token::Priv => Err(Error::at(span, "'priv' goes before a task, shape, kind, pattern, type alias, or macro, or a shape's field, not a statement")),
            _ if self.at_macro_after(0) => Err(Error::at(span, MISPLACED_MACRO)),
            Token::Ident(name) if name == "free" && matches!(self.tokens.get(self.pos + 1), Some(Token::Ident(_) | Token::New)) => Err(
                Error::at(span, "Vela has no 'free': memory in a region is freed when the block declaring the region closes"),
            ),
            _ => self.parse_simple_stmt(span),
        }
    }

    /// The loop `stmt`, named `name`, as a statement: with a plan B after its
    /// `fi`, a loop that fails as one giving a value does, and gives nothing.
    fn loop_with_plan_b(&mut self, stmt: Stmt, name: Option<(String, Span)>, span: Span) -> Result<Stmt> {
        if !self.check(Token::LBrace) {
            return Ok(match name {
                Some((name, at)) => Stmt::new(StmtKind::Named(name, Box::new(stmt)), at),
                None => stmt,
            });
        }
        let value = self.loop_value(stmt, name, false, span);
        let value = self.parse_suffixes(value, false)?;
        if !matches!(self.peek(), Token::Newline | Token::Eof | Token::Fi | Token::Else | Token::Case | Token::Finally | Token::RBrace) {
            return Err(self.unexpected("the end of the line after the loop's plan B"));
        }
        Ok(Stmt::new(StmtKind::Expr(value), span))
    }

    /// An assignment, to one place or to a list of them, a result update, or an
    /// expression used as a statement.
    fn parse_simple_stmt(&mut self, span: Span) -> Result<Stmt> {
        // `name <- value`, or `a, b <- …`: sets results. The checker knows
        // which names are results of this task.
        if self.at_result_update() {
            return self.parse_set_results(span);
        }
        // `_` only takes a value, and `(a, b) =` unpacks a tuple, so a line
        // starting with either is an assignment.
        if self.check(Token::Underscore) || self.at_target_group(true) {
            let first = self.parse_target()?;
            return self.parse_assignment(span, first);
        }
        let expr = self.parse_expr()?;
        if self.check(Token::Comma) || self.check(Token::Assign) {
            return self.parse_assignment(span, Target::Place(expr));
        }
        if let Some((op, symbol)) = step_operator(self.peek()) {
            let at = self.peek_span();
            self.advance();
            let value = match symbol {
                "++" | "--" => Expr::new(ExprKind::IntLit(1), at),
                _ => self.parse_expr()?,
            };
            // A statement ends its line, or a block or plan B on one line:
            // `when ok x++ else x-- fi`, `f() { n++ }`.
            if !matches!(self.peek(), Token::Newline | Token::Eof | Token::Fi | Token::Else | Token::Case | Token::Finally | Token::RBrace) {
                return Err(self.unexpected(&format!("the end of the line: '{}' is a statement of its own", symbol)));
            }
            return Ok(Stmt::new(StmtKind::Step { target: expr, op, value, symbol, at }, span));
        }
        Ok(Stmt::new(StmtKind::Expr(expr), span))
    }

    /// The rest of an assignment whose first place is `first`: any more places,
    /// then `= values`.
    fn parse_assignment(&mut self, span: Span, first: Target) -> Result<Stmt> {
        let mut targets = vec![first];
        while self.match_token(Token::Comma) {
            targets.push(self.parse_target()?);
        }
        if let Some((_, symbol)) = step_operator(self.peek()) {
            return Err(Error::at(self.peek_span(), format!("'{0}' changes one place, so each goes on a line of its own, as in 'a {0} …'", symbol)));
        }
        if !self.match_token(Token::Assign) {
            return Err(Error::at(
                self.peek_span(),
                "expected '=': a list of names takes the values of an assignment, as in 'a, b = divmod(17, 5)'",
            ));
        }
        let values = self.parse_value_list()?;
        Ok(Stmt::new(StmtKind::Assign(targets, values), span))
    }

    /// Whether the statement at the cursor is `name(, name)* <-`, which sets
    /// results. A `<-` anywhere else in a line is still an error.
    fn at_result_update(&self) -> bool {
        let mut p = self.pos;
        loop {
            if !matches!(self.tokens.get(p), Some(Token::Ident(_))) {
                return false;
            }
            match self.tokens.get(p + 1) {
                Some(Token::Send) => return true,
                Some(Token::Comma) => p += 2,
                _ => return false,
            }
        }
    }

    /// `name(, name)* <- values`: sets the named results.
    fn parse_set_results(&mut self, span: Span) -> Result<Stmt> {
        let mut names = Vec::new();
        loop {
            let at = self.peek_span();
            names.push((self.expect_ident()?, at));
            if !self.match_token(Token::Comma) {
                break;
            }
        }
        self.expect(Token::Send)?;
        if self.check(Token::Newline) || self.is_at_end() {
            return Err(Error::at(self.peek_span(), "expected the value to set the result to, after '<-'"));
        }
        let values = self.parse_value_list()?;
        Ok(Stmt::new(StmtKind::SetResults(names, values), span))
    }

    /// One place an assignment writes to, `_`, or `(a, b, …)`, the places of a
    /// tuple's items.
    fn parse_target(&mut self) -> Result<Target> {
        let span = self.peek_span();
        if self.match_token(Token::Underscore) {
            return Ok(Target::Discard(span));
        }
        if self.at_target_group(false) {
            self.advance(); // (
            self.enter()?;
            let parts = self.parse_target_parts(span);
            self.leave(1);
            return Ok(Target::Tuple(parts?, span));
        }
        Ok(Target::Place(self.parse_expr()?))
    }

    /// After the `(` at `opened` of places unpacking a tuple: the places and `)`.
    fn parse_target_parts(&mut self, opened: Span) -> Result<Vec<Target>> {
        let mut parts = vec![self.parse_target()?];
        while self.match_token(Token::Comma) {
            parts.push(self.parse_target()?);
        }
        self.expect(Token::RParen)?;
        if parts.len() < 2 {
            return Err(Error::at(opened, "a tuple has two or more items, so '( … )' names two or more places, as in '(a, b) = pair'"));
        }
        Ok(parts)
    }

    /// Whether the cursor is at `(` of places unpacking a tuple: parentheses on
    /// one line with a comma directly inside, and, at the start of a statement
    /// (`statement`), followed by `=` or `,`.
    fn at_target_group(&self, statement: bool) -> bool {
        if !self.check(Token::LParen) {
            return false;
        }
        let mut depth = 0usize;
        let mut comma = false;
        for p in self.pos.. {
            match self.tokens.get(p) {
                Some(Token::LParen | Token::LBracket | Token::LBrace) => depth += 1,
                Some(Token::RParen | Token::RBracket | Token::RBrace) => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return comma && (!statement || matches!(self.tokens.get(p + 1), Some(Token::Assign | Token::Comma)));
                    }
                }
                Some(Token::Comma) if depth == 1 => comma = true,
                Some(Token::Newline | Token::Eof) | None => return false,
                _ => {}
            }
        }
        false
    }

    /// One value, or several separated by commas.
    fn parse_value_list(&mut self) -> Result<Vec<Expr>> {
        let mut values = vec![self.parse_expr()?];
        while self.match_token(Token::Comma) {
            values.push(self.parse_expr()?);
        }
        Ok(values)
    }

    /// Rejects `send` used as the old keyword: alone on its line, or followed
    /// by a value on the same line. `send` is an ordinary name, but neither is
    /// meaningful code with it (a value can't follow a name), so it is almost
    /// certainly a send written the old way.
    fn reject_old_send(&self) -> Result<()> {
        let next = self.tokens.get(self.pos + 1).unwrap_or(&Token::Eof);
        let old_keyword = matches!(
            next,
            Token::Newline
                | Token::Eof
                | Token::Fi
                | Token::Int(_)
                | Token::Float(_)
                | Token::Char(_)
                | Token::Text(_)
                | Token::Interpolated(_)
                | Token::Ident(_)
                | Token::True
                | Token::False
                | Token::None
                | Token::New
                | Token::Not
                | Token::Minus
        );
        if old_keyword {
            return Err(Error::at(self.peek_span(), "a task sends its result with '<-', not 'send'"));
        }
        Ok(())
    }

    /// `fixed name [: type] [, name [: type]]* = values`, or the same with
    /// `flex`. A name may be `_`, which drops its value.
    fn parse_binding(&mut self, mode: Mode) -> Result<Stmt> {
        let word = mode.word();
        self.advance(); // fixed / flex
        let reference = self.match_reference(word)?;
        let span = self.peek_span();
        self.parse_binding_names(mode, reference, span)
    }

    /// A binding after its word: its names, `=`, and its values. A loop's
    /// start reads the ones after the first this way, which its word covers.
    fn parse_binding_names(&mut self, mode: Mode, reference: bool, span: Span) -> Result<Stmt> {
        let word = mode.word();
        let mut names = vec![self.parse_bound()?];
        while self.match_token(Token::Comma) {
            if reference {
                return Err(Error::at(self.peek_span(), format!("a reference is declared one at a time, each with its own '{}&'", word)));
            }
            names.push(self.parse_bound()?);
        }
        if reference && names.iter().any(|bound| !bound.parts.is_empty()) {
            return Err(Error::at(span, format!("a reference names one place, so '{}&' takes one name, not a tuple's items", word)));
        }
        self.expect(Token::Assign)?;
        let init = self.parse_value_list()?;
        if reference && init.len() > 1 {
            return Err(Error::at(init.get(1).map_or(span, |value| value.span), "a reference names one place, so it takes one value, as in '&x'"));
        }
        let kind = StmtKind::Binding { names, mode, reference, init };
        Ok(Stmt::new(kind, span))
    }

    /// Whether the `&` at the cursor is written against the word just read, as
    /// in `flex&`: it starts where the word ends, whichever its length.
    fn amp_against_word(&self) -> bool {
        self.check(Token::Amp) && self.pos.checked_sub(1).and_then(|word| self.ends.get(word)) == Some(&self.peek_span())
    }

    /// The `&` of `flex&` or `fixed&` before a binding's name, written against
    /// the word just read, which makes the binding a reference.
    fn match_reference(&mut self, word: &str) -> Result<bool> {
        if !self.check(Token::Amp) {
            return Ok(false);
        }
        if !self.amp_against_word() {
            return Err(Error::at(self.peek_span(), format!("a reference is written '{0}& y = &x', with '&' against '{0}'", word)));
        }
        self.advance();
        if self.check(Token::Amp) {
            return Err(Error::at(self.peek_span(), format!("'&' marks the place a reference names, after '=', as in '{}& y = &x'", word)));
        }
        Ok(true)
    }

    fn parse_when(&mut self) -> Result<Stmt> {
        let span = self.peek_span();
        self.advance(); // when
        let when = self.parse_when_rest(span)?;
        // One `fi` closes a whole chain, however many `else when`s it has.
        self.expect_fi("when", span)?;
        Ok(when)
    }

    /// A `when` after its keyword: its condition and block, then the arms of
    /// an `else when …` chain, in a loop, and an `else`, or neither. The `fi`
    /// closing the chain is the caller's to expect.
    fn parse_when_rest(&mut self, span: Span) -> Result<Stmt> {
        let mut arms = Vec::new();
        let mut at = span;
        loop {
            let cond = self.parse_condition()?;
            self.skip_newlines();
            let body = self.parse_block_until(&[Token::Else, Token::Fi])?;
            arms.push(WhenArm { cond, body, span: at });
            if !self.match_token(Token::Else) {
                return Ok(Stmt::new(StmtKind::When(arms, None), span));
            }
            if matches!(self.peek(), Token::Ident(name) if name == "if") {
                return Err(Error::at(self.peek_span(), "Vela writes 'else when', not 'else if'"));
            }
            if !self.check(Token::When) {
                self.skip_newlines();
                let else_block = self.parse_block_until(&[Token::Fi])?;
                return Ok(Stmt::new(StmtKind::When(arms, Some(else_block)), span));
            }
            at = self.peek_span();
            self.advance(); // when
        }
    }

    fn parse_loop(&mut self) -> Result<Stmt> {
        let span = self.peek_span();
        self.advance(); // loop
        if let Some(parts) = self.loop_parts()? {
            return self.parse_parted_loop(span, parts);
        }
        // A `when` is a value too, so `loop when x < 3` would read as a loop
        // whose condition is a whole `when`, taking the loop's own `fi` with it.
        if self.check(Token::When) {
            return Err(Error::at(self.peek_span(), "a 'loop' takes its condition directly, without 'when'"));
        }
        if matches!(self.peek(), Token::Flex | Token::Fixed | Token::Bind) {
            return Err(Error::at(
                self.peek_span(),
                "a loop declares names of its own in its start, which '||' ends before its condition, as in 'loop flex i = 0 || i < 10 || i++'",
            ));
        }
        let cond = self.parse_condition()?;
        let (body, finally) = self.parse_loop_body("loop", span)?;
        Ok(Stmt::new(StmtKind::Loop(cond, body, finally), span))
    }

    /// A loop's body and its optional `finally` block, closed by `fi`.
    fn parse_loop_body(&mut self, word: &str, span: Span) -> Result<(Block, Option<Block>)> {
        self.skip_newlines();
        let body = self.parse_block_until(&[Token::Fi, Token::Finally])?;
        let finally = if self.check(Token::Finally) {
            self.advance(); // finally
            self.skip_newlines();
            let block = self.parse_block()?;
            Some(block)
        } else {
            None
        };
        self.expect_fi(word, span)?;
        Ok((body, finally))
    }

    /// `break` or `continue`, and the name of the loop it stops or goes on
    /// with, when it is not the innermost one: `break outer`.
    fn parse_jump(&mut self) -> Result<Stmt> {
        let span = self.peek_span();
        let stop = self.check(Token::Break);
        self.advance(); // break or continue
        let word = if stop { "break" } else { "continue" };
        let name = match self.peek().clone() {
            Token::Ident(name) => {
                let at = self.peek_span();
                self.advance();
                Some((name, at))
            }
            _ => None,
        };
        // `break = v`: what a loop that gives a value fails with.
        let value = if self.check(Token::Assign) {
            if !stop {
                return Err(Error::at(self.peek_span(), "a 'continue' goes on with the loop's next pass, so it gives no value; 'break = v' stops a loop that gives one with a value"));
            }
            self.advance(); // =
            Some(Box::new(self.parse_expr()?))
        } else {
            None
        };
        // Nothing else follows on its line.
        if !matches!(self.peek(), Token::Newline | Token::Eof | Token::Fi | Token::Else | Token::Case | Token::Finally | Token::RBrace) {
            let what = if stop { "stops" } else { "goes on with" };
            let example = name.as_ref().map_or("outer", |(name, _)| name.as_str());
            let given = if stop { ", and the value it gives after '=', as in 'break = v'" } else { "" };
            return Err(Error::at(self.peek_span(), format!("a '{0}' takes the name of the loop it {1}, when that is not the innermost, as in '{0} {2}'{3}", word, what, example, given)));
        }
        Ok(Stmt::new(if stop { StmtKind::Break(name, value) } else { StmtKind::Continue(name) }, span))
    }

    /// `endless`, then its statements from the next line, closed by `fi`: a
    /// loop that never ends by itself, `loop || ||` said in a word.
    fn parse_endless(&mut self) -> Result<Stmt> {
        let span = self.peek_span();
        self.advance(); // endless
        if !matches!(self.peek(), Token::Newline | Token::Eof) {
            return Err(Error::at(
                self.peek_span(),
                "an 'endless' loop takes nothing on its line: it has no condition, and its statements start on the next one",
            ));
        }
        let (body, finally) = self.parse_loop_body("endless", span)?;
        let parted = PartedLoop { start: Vec::new(), cond: None, step: Vec::new(), body, finally, endless: true };
        Ok(Stmt::new(StmtKind::Parted(Box::new(parted)), span))
    }

    /// After `loop`, where its line holds `||` outside brackets: the parts it
    /// parts, each the positions of its items, from first to the one after
    /// the last. The `||`s, and the `,`s parting items, are made line ends,
    /// so each item is read as a statement on a line of its own. The
    /// condition alone may go on past the line, as a `match` there does, and
    /// the step follows its `fi`.
    fn loop_parts(&mut self) -> Result<Option<LoopParts>> {
        let mut depth = 0usize;
        let mut blocks = 0usize;
        let mut bars = Vec::new();
        let mut end = self.pos;
        while let Some(token) = self.tokens.get(end) {
            match token {
                Token::Eof => break,
                // The condition alone goes on past the line, inside a block
                // or brackets.
                Token::Newline if (blocks == 0 && depth == 0) || bars.len() != 1 => break,
                Token::Parts if depth == 0 => bars.push(end),
                Token::Lt => {
                    if let Some(close) = types_close(self, end) {
                        end = close;
                    }
                }
                _ => {
                    nests(self, end, &mut depth, &mut blocks);
                }
            }
            end += 1;
        }
        if bars.is_empty() {
            return Ok(None);
        }
        if let Some(&fourth) = bars.get(2) {
            return Err(Error::at(
                self.spans.get(fourth).copied().unwrap_or_default(),
                "a loop has three parts, its start, its condition and its step, parted by two '||'s; for either of two conditions, Vela writes 'or'",
            ));
        }
        let mut bounds = vec![self.pos];
        bounds.extend(bars.iter().map(|&bar| bar + 1));
        let mut ends = bars.clone();
        ends.push(end);
        let mut parts = Vec::new();
        for (i, (&first, &last)) in bounds.iter().zip(&ends).enumerate() {
            // The condition is one value, or none.
            let items = if i == 1 { if first < last { vec![(first, last)] } else { Vec::new() } } else { loop_items(self, first, last) };
            parts.push(items);
        }
        for p in bars.into_iter().chain(parts.iter().flatten().map(|&(_, last)| last)) {
            if let Some(token) = self.tokens.get_mut(p)
                && matches!(token, Token::Parts | Token::Comma)
            {
                *token = Token::Newline;
            }
        }
        Ok(Some(parts))
    }

    /// The rest of `loop start || condition || step` (see `loop_parts`).
    fn parse_parted_loop(&mut self, span: Span, parts: LoopParts) -> Result<Stmt> {
        let mut parts = parts.into_iter();
        let start = parts.next().unwrap_or_default();
        let cond = parts.next().unwrap_or_default();
        let step = parts.next().unwrap_or_default();
        // The start: statements, until `flex` or `fixed` declares the names
        // after it, each item one.
        let mut declaring: Option<Mode> = None;
        let mut started = Vec::new();
        let items = start.len();
        for (n, (first, last)) in start.into_iter().enumerate() {
            self.pos = first;
            let item = match self.peek() {
                Token::Flex | Token::Fixed | Token::Bind => {
                    let mode = if self.check(Token::Fixed) {
                        Mode::Fixed
                    } else if self.check(Token::Bind) {
                        Mode::Bind
                    } else {
                        Mode::Flex
                    };
                    declaring = Some(mode);
                    self.parse_binding(mode)?
                }
                Token::Newline => return Err(Error::at(self.peek_span(), "an item of a loop's start is missing between two ','s")),
                _ => match declaring {
                    Some(mode) => self.parse_binding_names(mode, false, self.peek_span())?,
                    None => self.parse_stmt()?,
                },
            };
            if let StmtKind::Binding { reference: true, .. } = item.kind {
                return Err(Error::at(item.span, "a loop's start declares values for the loop, not references; name the place itself"));
            }
            // `loop a || b`: a start that only gives a value is a condition,
            // and `||` was meant as 'or'.
            if let StmtKind::Expr(e) = &item.kind
                && !matches!(e.kind, ExprKind::Call { .. } | ExprKind::MethodCall { .. } | ExprKind::Invoke { .. } | ExprKind::PlanB { .. } | ExprKind::Pipe(_))
            {
                let bar = if n + 1 == items { last } else { first };
                return Err(Error::at(
                    self.spans.get(bar).copied().unwrap_or_default(),
                    "a loop's start sets places or calls tasks, and this one only gives a value; for either of two conditions, Vela writes 'or'",
                ));
            }
            self.end_loop_item(last, "',' or '||': a loop's start parts its items with ',', and '||' ends it")?;
            started.push(item);
        }
        let cond = match cond.first() {
            Some(&(first, last)) => {
                self.pos = first;
                if self.check(Token::When) {
                    return Err(Error::at(self.peek_span(), "a 'loop' takes its condition directly, without 'when'"));
                }
                let cond = self.parse_condition()?;
                let what = if step.is_empty() { "the end of the line or '||': a loop takes one condition, so join two with 'and' or 'or'" } else { "'||': a loop takes one condition, so join two with 'and' or 'or'" };
                self.end_loop_item(last, what)?;
                Some(cond)
            }
            None => None,
        };
        let mut steps = Vec::new();
        for (first, last) in step {
            self.pos = first;
            if self.check(Token::Newline) {
                return Err(Error::at(self.peek_span(), "an item of a loop's step is missing between two ','s"));
            }
            steps.push(self.parse_stmt()?);
            self.end_loop_item(last, "',' or the end of the line: a loop's step parts its items with ',', and its line ends after them")?;
        }
        // The line ends after the last part.
        while !matches!(self.peek(), Token::Newline | Token::Eof) {
            self.advance();
        }
        let (body, finally) = self.parse_loop_body("loop", span)?;
        let parted = PartedLoop { start: started, cond, step: steps, body, finally, endless: false };
        Ok(Stmt::new(StmtKind::Parted(Box::new(parted)), span))
    }

    /// After an item of a loop's parts, which ends at `last`; `what` is
    /// what may follow it.
    fn end_loop_item(&mut self, last: usize, what: &str) -> Result<()> {
        if self.pos < last {
            return Err(self.unexpected(what));
        }
        // A block an item opens closes on the loop's line.
        if self.pos > last {
            return Err(Error::at(
                self.spans.get(last).copied().unwrap_or_default(),
                "an item of a loop's start or step ends on the loop's line, so a 'when' or 'match' in it closes with 'fi' there; for more, put them in the loop's body",
            ));
        }
        Ok(())
    }

    /// `outer => loop …` or `outer => each …`: a loop and its name, which a
    /// `break` or a `continue` in a loop inside it gives to stop it or go on
    /// with it.
    fn parse_named_loop(&mut self) -> Result<Stmt> {
        let span = self.peek_span();
        let name = self.expect_ident()?;
        self.advance(); // =>
        let inner = match self.peek() {
            Token::Loop => self.parse_loop()?,
            Token::Endless => self.parse_endless()?,
            Token::Each => self.parse_each()?,
            _ => {
                return Err(Error::at(
                    self.peek_span(),
                    format!("'{} =>' names a loop, so 'loop', 'each' or 'endless' follows it on its line, as in '{} => each x in xs'", name, name),
                ));
            }
        };
        Ok(Stmt::new(StmtKind::Named(name, Box::new(inner)), span))
    }

    /// `block`, then its statements from the next line, closed by `fi`.
    fn parse_plain_block(&mut self) -> Result<Stmt> {
        let span = self.peek_span();
        self.advance(); // block
        if !matches!(self.peek(), Token::Newline | Token::Eof) {
            return Err(Error::at(self.peek_span(), "a 'block' takes nothing on its line: its statements start on the next one, and 'fi' closes it"));
        }
        self.skip_newlines();
        let body = self.parse_block()?;
        self.expect_fi("block", span)?;
        Ok(Stmt::new(StmtKind::Block(body), span))
    }

    fn parse_each(&mut self) -> Result<Stmt> {
        let opened = self.peek_span();
        self.advance(); // each
        let span = self.peek_span();
        // `_` drops a map's key or value (the checker allows it only there).
        let name = if self.match_token(Token::Underscore) { DISCARD.to_string() } else { self.expect_ident()? };
        // `each k, v in m`: a map's key, and the value that goes with it.
        let second = if self.match_token(Token::Comma) {
            let at = self.peek_span();
            let second = if self.match_token(Token::Underscore) { DISCARD.to_string() } else { self.expect_ident()? };
            Some((second, at))
        } else {
            None
        };
        self.expect(Token::In)?;
        let range = self.parse_expr()?;
        let (body, finally) = self.parse_loop_body("each", opened)?;
        Ok(Stmt::new(StmtKind::Each(name, second, range, body, finally), span))
    }

    /// A `match`. A case written `case p = value` gives that value, and one
    /// written as a block gives the value on the block's last line; both are the
    /// same case with a one-statement body, and whether the `match` gives a value
    /// at all is the checker's to say (see `check::Checker::check_match`), since a
    /// `match` on the last line of a block gives one wherever that block does.
    fn parse_match(&mut self) -> Result<Stmt> {
        let span = self.peek_span();
        self.advance(); // match
        let expr = self.parse_expr()?;
        self.skip_newlines();
        let mut cases = Vec::new();
        while !self.check(Token::Fi) && !self.is_at_end() && !self.at_item_start() {
            self.skip_newlines();
            if self.check(Token::Fi) || self.at_item_start() { break; }
            self.reject_stray_end()?;
            self.expect(Token::Case)?;
            let pattern = self.parse_pattern("case")?;
            if self.check(Token::Assign) {
                self.advance(); // =
                let value = self.parse_expr()?;
                let value_span = value.span;
                cases.push(Case { pattern, body: vec![Stmt::new(StmtKind::Expr(value), value_span)] });
                continue;
            }
            if !self.check(Token::Newline) && !self.check(Token::Fi) && !self.check(Token::Case) && !self.is_at_end() {
                if self.check(Token::Send) {
                    return Err(self.misplaced_send());
                }
                return Err(match pattern.kind {
                    PatternKind::Variant { .. } => self.name_expected(),
                    _ => self.unexpected("the end of the line"),
                });
            }
            self.skip_newlines();
            let body = self.parse_block_until(&[Token::Case, Token::Fi])?;
            cases.push(Case { pattern, body });
        }
        self.expect_fi("match", span)?;
        Ok(Stmt::new(StmtKind::Match(expr, cases), span))
    }

    /// What a `case` or `is` (the `keyword`) matches: `Circle r`, `{value}`,
    /// `none`, `fail e`, `(x, y)`, or `_`. A name it binds may be written
    /// `flex name`, which makes it a variable that can be assigned.
    fn parse_pattern(&mut self, keyword: &str) -> Result<Pattern> {
        let span = self.peek_span();
        let kind = match self.peek() {
            Token::Underscore => {
                self.advance();
                PatternKind::Any
            }
            Token::None => {
                self.advance();
                PatternKind::None
            }
            // `t.#5 is maybe int {x}`: a test of the type of a tuple's item.
            Token::Maybe if keyword == "is" => {
                let ty = self.parse_type()?;
                let form = match self.peek() {
                    Token::LBrace => {
                        self.advance();
                        let names = self.parse_pattern_names()?;
                        self.expect(Token::RBrace)?;
                        TypedForm::Value(names)
                    }
                    Token::Ident(_) | Token::Flex | Token::Bind | Token::LParen | Token::Underscore => TypedForm::Named(self.parse_pattern_names()?),
                    _ => TypedForm::Only,
                };
                PatternKind::Typed { ty, form, asks: false }
            }
            Token::Maybe => {
                return Err(Error::at(
                    span,
                    format!("a type after '{}' can't be matched; a type is tested after 'is', on a tuple's item read with '.#', as in 'when t.#2 is maybe int {{x}}'", keyword),
                ));
            }
            Token::LBrace => {
                self.advance();
                let names = self.parse_pattern_names()?;
                if self.check(Token::Comma) {
                    return Err(Error::at(
                        self.peek_span(),
                        "'{…}' names the one value inside a maybe or a 'T ~ E'; to take a tuple apart too, write its names in parentheses, as in '{(name, age)}'",
                    ));
                }
                self.expect(Token::RBrace)?;
                PatternKind::Value(names)
            }
            Token::Failure => {
                self.advance();
                PatternKind::Fail(self.parse_pattern_names()?)
            }
            // The verb makes a failure; a pattern names one with the noun.
            Token::Fail => return Err(Error::at(self.peek_span(), FAILURE_NAMES)),
            // `(x, y)`, `flex (x, y)` or `bind (x, y)`: the items of a tuple.
            Token::LParen | Token::Flex | Token::Bind => {
                let names = self.parse_pattern_names()?;
                if names.parts.is_empty() {
                    return Err(Error::at(
                        names.span,
                        format!(
                            "'{0}' matches a value, so a name alone isn't a case form: write '{{flex {1}}}' for the value inside a maybe or a 'T ~ E', or '{0} flex (x, y)' for a tuple's items",
                            keyword, names.name
                        ),
                    ));
                }
                PatternKind::Tuple(names)
            }
            Token::Ident(_) => {
                // `Circle`, `Shape.Circle`, or `geometry.Shape.Circle`.
                let mut written = vec![self.expect_ident()?];
                while self.check(Token::Dot) && matches!(self.tokens.get(self.pos + 1), Some(Token::Ident(_))) {
                    self.advance(); // .
                    written.push(self.expect_ident()?);
                }
                // `case Option<int>.Some v`: the value matched has its types.
                if self.at_kind_types() {
                    let kind = written.join(".");
                    return Err(Error::at(
                        self.peek_span(),
                        format!("a case form takes the types of '{0}' from the value it matches, so it names none: '{1} {0}.Tag …'", kind, keyword),
                    ));
                }
                let module_first = written.first().is_some_and(|first| self.is_module(first));
                let tag = written.pop().unwrap_or_default();
                // A kind after a name that isn't a module this file includes is
                // left for the resolver, which says what the name is instead.
                if written.len() > 2 || (written.len() == 1 && module_first) {
                    return Err(Error::at(
                        span,
                        format!(
                            "a case form names the variant alone or after its kind, as in '{0} {1}' or '{0} Kind.{1}': the value's kind tells which module it is from",
                            keyword, tag
                        ),
                    ));
                }
                // `is int {x}`: a type test, which names a maybe.
                if keyword == "is" && written.is_empty() && crate::check::BUILTIN_TYPE_WORDS.contains(&tag.as_str()) && self.check(Token::LBrace) {
                    return Err(Error::at(span, format!("a tuple's item read with '.#' is a maybe, so a type test writes its type as one, as in 'is maybe {0} {{x}}'", tag)));
                }
                let kind = (!written.is_empty()).then(|| Box::new((written.join(&MODULE_SEPARATOR.to_string()), span)));
                if self.check(Token::LParen) {
                    return Err(Error::at(
                        self.peek_span(),
                        format!("a case form names its bindings without parentheses, as in '{} {} value'", keyword, tag),
                    ));
                }
                let mut binds = Vec::new();
                while matches!(self.peek(), Token::Ident(_) | Token::Underscore | Token::Flex | Token::Bind) {
                    let names = self.parse_pattern_names()?;
                    if !names.parts.is_empty() {
                        return Err(Error::at(names.span, format!("a case form names its bindings without parentheses, as in '{} {} value'", keyword, tag)));
                    }
                    binds.push(names);
                }
                PatternKind::Variant { kind, tag, binds }
            }
            _ => return Err(self.unexpected("a variant, '{value}', 'none', 'failure e', or '_'")),
        };
        Ok(Pattern { kind, span })
    }

    /// What a pattern names: `name`, `_`, or `(a, b)`, the names of a tuple's
    /// items, each of which `flex` may make changeable or `bind` may make its
    /// contents changeable.
    fn parse_pattern_names(&mut self) -> Result<Names> {
        let span = self.peek_span();
        let mode = if self.match_token(Token::Flex) {
            Mode::Flex
        } else if self.match_token(Token::Bind) {
            Mode::Bind
        } else {
            Mode::Fixed
        };
        if !self.check(Token::LParen) {
            let (name, at) = self.parse_bound_name()?;
            return Ok(Names::one(name, at, mode));
        }
        self.advance(); // (
        self.enter()?;
        let parts = self.parse_pattern_parts(span);
        self.leave(1);
        Ok(Names { name: String::new(), span, mode, parts: parts? })
    }

    /// After the `(` at `opened` of a pattern's names: the names and `)`.
    fn parse_pattern_parts(&mut self, opened: Span) -> Result<Vec<Names>> {
        let mut parts = vec![self.parse_pattern_names()?];
        while self.match_token(Token::Comma) {
            parts.push(self.parse_pattern_names()?);
        }
        self.expect(Token::RParen)?;
        if parts.len() < 2 {
            return Err(Error::at(opened, "a tuple has two or more items, so '( … )' names two or more, as in 'is (x, y)'"));
        }
        Ok(parts)
    }

    /// A name a binding declares, with its type, or `(a, b, …)`, the names of a
    /// tuple's items.
    fn parse_bound(&mut self) -> Result<Bound> {
        let span = self.peek_span();
        if self.match_token(Token::LParen) {
            self.enter()?;
            let parts = self.parse_bound_parts(span);
            self.leave(1);
            let parts = parts?;
            if self.check(Token::Colon) {
                return Err(Error::at(
                    self.peek_span(),
                    "'( … )' takes a tuple apart, so each name in it takes its own type, as in 'fixed (a: int, b: text) = t'",
                ));
            }
            return Ok(Bound { name: String::new(), span, ty: None, ty_span: None, parts });
        }
        let (name, name_span) = self.parse_bound_name()?;
        let (ty, ty_span) = if self.match_token(Token::Colon) {
            let ty_span = self.peek_span();
            (Some(self.parse_type()?), Some(ty_span))
        } else {
            (None, None)
        };
        Ok(Bound { name, span: name_span, ty, ty_span, parts: Vec::new() })
    }

    /// After the `(` at `opened` of names unpacking a tuple: the names and `)`.
    fn parse_bound_parts(&mut self, opened: Span) -> Result<Vec<Bound>> {
        let mut parts = vec![self.parse_bound()?];
        while self.match_token(Token::Comma) {
            parts.push(self.parse_bound()?);
        }
        self.expect(Token::RParen)?;
        if parts.len() < 2 {
            return Err(Error::at(opened, "a tuple has two or more items, so '( … )' names two or more, as in 'fixed (a, b) = pair'"));
        }
        Ok(parts)
    }

    fn parse_bound_name(&mut self) -> Result<Binding> {
        let span = self.peek_span();
        if self.match_token(Token::Underscore) {
            return Ok((DISCARD.to_string(), span));
        }
        Ok((self.expect_ident()?, span))
    }

    /// `<-`, `<- value`, or `<- a, b`. A send starts its line (or a plan B's block, as in
    /// `x { <- 0 }`): `<-` after something else is almost always a comparison
    /// with a negative number written without a space (`a <-1`), which would
    /// otherwise end the statement and send.
    fn parse_send(&mut self) -> Result<Stmt> {
        let span = self.peek_span();
        let starts_line = self.pos == 0 || matches!(self.tokens.get(self.pos - 1), Some(Token::Newline | Token::LBrace)) || self.send_after_header == Some(self.pos);
        if !starts_line {
            return Err(self.misplaced_send());
        }
        self.advance(); // <-
        if matches!(self.peek(), Token::Newline | Token::Fi | Token::Else | Token::Case | Token::Finally | Token::RBrace) || self.is_at_end() {
            Ok(Stmt::new(StmtKind::Send(Vec::new()), span))
        } else {
            let values = self.parse_value_list()?;
            Ok(Stmt::new(StmtKind::Send(values), span))
        }
    }

    /// `region name`
    /// `region r`, or `region r default`, which makes it the block's default
    /// region (see `ast::DEFAULT`).
    fn parse_region(&mut self) -> Result<Stmt> {
        self.advance(); // region
        let span = self.peek_span();
        let name = self.expect_ident()?;
        let default = self.match_default(true)?;
        Ok(Stmt::new(StmtKind::Region { name, default }, span))
    }

    fn parse_expr(&mut self) -> Result<Expr> {
        self.enter()?;
        let expr = self.parse_binary(1)?;
        self.leave(1);
        Ok(expr)
    }

    /// A `when` or `loop` condition: `value`, `value is pattern`, or parts
    /// like these joined with `and` (`m is {x} and x > 0`), explaining `=`
    /// written for `==`. The parts are read above `and`, so that one can be an
    /// `is`; where none is, they become one `bool` as `and` makes it, and an
    /// `or` after them goes on as it does in any value. An `is` is never joined
    /// with `or`, nor after `not`: a name it binds would be bound on one path
    /// only.
    fn parse_condition(&mut self) -> Result<Condition> {
        const AND_PRECEDENCE: u8 = 2;
        let mut parts = Vec::new();
        let mut joins = Vec::new();
        loop {
            let value = self.parse_binary(AND_PRECEDENCE + 1)?;
            if self.check(Token::Assign) {
                return Err(Error::at(self.peek_span(), "'=' assigns; to compare, write '=='"));
            }
            // `t.#5? is maybe int`: the `?` asks for the item's type itself.
            let asks_at = self.peek_span();
            let asks = self.match_token(Token::Question);
            if asks && !self.check(Token::Is) {
                return Err(Error::at(asks_at, "'?' after a tuple's item read with '.#' asks for its type, which an 'is' then tests, as in 't.#5? is maybe int'"));
            }
            let is_at = self.peek_span();
            let pattern = match self.match_token(Token::Is) {
                true => {
                    if matches!(value.kind, ExprKind::Unary(UnaryOp::Not, _)) {
                        return Err(Error::at(
                            is_at,
                            "an 'is' can't follow 'not': the names it binds would be bound only where it doesn't match; match the other outcome instead, as in 'm is none'",
                        ));
                    }
                    let mut pattern = self.parse_pattern("is")?;
                    match &mut pattern.kind {
                        PatternKind::Typed { form: TypedForm::Only, asks: written, .. } if asks => *written = true,
                        PatternKind::Typed { form: TypedForm::Only, ty, .. } => {
                            return Err(Error::at(
                                pattern.span,
                                format!("a type alone after 'is' tests the type of a tuple's item, which a '?' asks for, as in 't.#5? is {0}'; or name the value, as in 'is {0} x' or 'is {0} {{x}}'", ty),
                            ));
                        }
                        _ if asks => {
                            return Err(Error::at(
                                pattern.span,
                                "'?' asks for the type of a tuple's item, so a type alone follows 'is', as in 't.#5? is maybe int'; to take the value, leave the '?' out",
                            ));
                        }
                        _ => {}
                    }
                    Some(pattern)
                }
                false => None,
            };
            parts.push((value, pattern));
            let join = self.peek_span();
            if !self.match_token(Token::And) {
                break;
            }
            joins.push(join);
        }
        let matched = parts.iter().any(|(_, pattern)| pattern.is_some());
        if matched && self.check(Token::Or) {
            return Err(Error::at(self.peek_span(), IS_WITH_OR));
        }
        if !matched {
            // `a and b`, and maybe `or c`: one value, as `parse_expr` reads it.
            let mut values = parts.into_iter().map(|(value, _)| value);
            let first = values.next().ok_or_else(|| Error::at(self.peek_span(), "expected a condition"))?;
            let joined = values.zip(joins).fold(first, |left, (right, span)| Expr::new(ExprKind::Binary(BinaryOp::And, Box::new(left), Box::new(right)), span));
            let value = self.parse_operators(joined, 1)?;
            if self.check(Token::Is) {
                return Err(Error::at(self.peek_span(), IS_WITH_OR));
            }
            if self.check(Token::Assign) {
                return Err(Error::at(self.peek_span(), "'=' assigns; to compare, write '=='"));
            }
            return Ok(Condition::new(value, None));
        }
        let chained = parts.into_iter().rev().fold(None, |and, (value, pattern)| Some(Box::new(Condition { value, pattern, and })));
        chained.map(|chained| *chained).ok_or_else(|| Error::at(self.peek_span(), "expected a condition"))
    }

    /// The binary operator `token` stands for, with its precedence (higher
    /// binds tighter). `..` is handled apart from these, as it doesn't chain.
    fn binary_op(token: &Token) -> Option<(BinaryOp, u8)> {
        Some(match token {
            Token::Or => (BinaryOp::Or, 1),
            Token::And => (BinaryOp::And, 2),
            Token::Eq => (BinaryOp::Eq, 3),
            Token::Ne => (BinaryOp::Ne, 3),
            Token::Similar => (BinaryOp::Similar, 3),
            Token::NotSimilar => (BinaryOp::NotSimilar, 3),
            Token::Lt => (BinaryOp::Lt, 4),
            Token::Gt => (BinaryOp::Gt, 4),
            Token::Le => (BinaryOp::Le, 4),
            Token::Ge => (BinaryOp::Ge, 4),
            Token::Plus => (BinaryOp::Add, Self::ADD_PRECEDENCE),
            Token::Minus => (BinaryOp::Sub, Self::ADD_PRECEDENCE),
            Token::Star => (BinaryOp::Mul, 8),
            Token::Slash => (BinaryOp::Div, 8),
            Token::Percent => (BinaryOp::Mod, 8),
            Token::Power => (BinaryOp::Pow, Self::POWER_PRECEDENCE),
            _ => return None,
        })
    }

    /// Precedence of `|>`: above comparisons and below `..` and `+`, so
    /// `a + 1 |> f() == 3` is `f(a + 1) == 3`.
    const PIPE_PRECEDENCE: u8 = 5;

    /// Precedence of `..`: between pipes and `+`/`-`.
    const RANGE_PRECEDENCE: u8 = 6;

    /// Precedence of `**`, above `*` and below a prefix `-`, which takes only the
    /// value right after it: `-2 ** 2` is `(-2) ** 2`. It groups from the right.
    const POWER_PRECEDENCE: u8 = 9;

    /// Precedence of `+`, whose chains `at r` joins.
    const ADD_PRECEDENCE: u8 = 7;

    /// Parses operators of precedence `min` or higher by precedence climbing,
    /// left-associative, with each node's span at its operator. `a..b` takes one
    /// `..` (`a..b..c` is not a range). Climbing keeps the recursion per
    /// parenthesis shallow, which matters for `MAX_NESTING`.
    fn parse_binary(&mut self, min: u8) -> Result<Expr> {
        let left = self.parse_unary()?;
        self.parse_operators(left, min)
    }

    /// The operators of precedence `min` or higher after `left`, the first
    /// operand `parse_binary` read. (A function of its own: `parse_binary`'s
    /// frame is on the stack once per nesting level, and a debug build gives a
    /// frame room for everything its function could hold at once.)
    fn parse_operators(&mut self, mut left: Expr, min: u8) -> Result<Expr> {
        // Each operator deepens the tree by one level, like a parenthesis.
        let mut levels = 0;
        let mut ranged = false;
        loop {
            // A loop's `||`s are read before its parts (see `parse_loop`), so
            // one here stands where an `or` was meant.
            if self.check(Token::Parts) {
                return Err(Error::at(self.peek_span(), PARTS_AS_OR));
            }
            // `a + b at r` or `"n = {n}" at r`: the `+` chain just read, or the
            // text holding values, is joined into `r`. The right operand of a
            // `+` leaves its `at` to the whole chain.
            if self.check(Token::At) && min <= Self::ADD_PRECEDENCE {
                let added = matches!(left.kind, ExprKind::Binary(BinaryOp::Add, ..));
                if (added && levels > 0) || pending_region(&left) {
                    left = self.parse_join(left)?;
                    self.reject_after_region()?;
                } else if added {
                    // A chain in parentheses names its region with `@`.
                    return Err(Error::at(
                        self.peek_span(),
                        "a '+' chain in parentheses names its region with '@', as in '(a + b)@r'; without them, write 'a + b at r'",
                    ));
                } else {
                    // Everything before `at` is one call, not a chain: the
                    // region is the call's (see `place_region`).
                    left = self.parse_call_region(left)?;
                }
            }
            // A line that starts with `|>` goes on with the chain before it.
            if self.check(Token::Newline) && min <= Self::PIPE_PRECEDENCE && !ranged && self.pipe_on_next_line() {
                while self.check(Token::Newline) {
                    self.advance();
                }
            }
            if let Token::Pipe(count) = *self.peek()
                && min <= Self::PIPE_PRECEDENCE
                && !ranged
            {
                self.enter_operator()?;
                levels += 1;
                left = self.parse_pipe(left, count)?;
                continue;
            }
            let span = self.peek_span();
            if self.check(Token::Range) && min <= Self::RANGE_PRECEDENCE && !ranged {
                self.enter_operator()?;
                levels += 1;
                self.advance();
                let right = self.parse_binary(Self::RANGE_PRECEDENCE + 1)?;
                left = Expr::new(ExprKind::Range(Box::new(left), Box::new(right)), span);
                ranged = true;
                continue;
            }
            let Some((op, precedence)) = Self::binary_op(self.peek()) else { break };
            if precedence < min {
                break;
            }
            // A range is an operand of comparisons only (`x < 0..5` is left as before).
            if ranged && precedence > Self::RANGE_PRECEDENCE {
                break;
            }
            self.enter_operator()?;
            levels += 1;
            self.advance();
            // `2 ** 3 ** 2` is `2 ** (3 ** 2)`.
            let next = if op == BinaryOp::Pow { precedence } else { precedence + 1 };
            let right = self.parse_binary(next)?;
            left = Expr::new(ExprKind::Binary(op, Box::new(left), Box::new(right)), span);
        }
        self.leave_operators(levels);
        Ok(left)
    }

    /// Whether the line after the cursor, at the end of a line, starts with
    /// `|>`, after any blank lines: it goes on with the chain before it.
    fn pipe_on_next_line(&self) -> bool {
        let mut at = self.pos;
        while self.tokens.get(at) == Some(&Token::Newline) {
            at += 1;
        }
        matches!(self.tokens.get(at), Some(Token::Pipe(_)))
    }

    /// `value |> f(b)`, with the cursor at the `|>` and `count` the number of
    /// its `>`: the call after it, given `value` as its argument `count`, as a
    /// `Piped` among the ones it writes (`f(value, b)`; `f(b, value)` for
    /// `|>>`), in a `Pipe`. A plan B after the call is the pipe's, and so
    /// handles every step before it as well: `a |> f() { 0 }`.
    fn parse_pipe(&mut self, value: Expr, count: usize) -> Result<Expr> {
        let span = self.peek_span();
        let pipe = Token::Pipe(count).describe();
        self.advance(); // |>
        if matches!(self.peek(), Token::RParen | Token::RBracket | Token::RBrace | Token::Comma | Token::Newline | Token::Eof) {
            return Err(Error::at(self.peek_span(), format!("{} gives the value before it to the call written after it, as in 'xs |> total()'", pipe)));
        }
        let parenthesized = self.check(Token::LParen);
        let mut step = self.parse_postfix()?;
        if self.check(Token::At) {
            step = self.parse_call_region(step)?;
        }
        let mut plans = Vec::new();
        while !parenthesized && matches!(step.kind, ExprKind::PlanB { .. }) {
            let ExprKind::PlanB { value, error, body } = step.kind else { break };
            plans.push((error, body, step.span));
            step = *value;
        }
        let callee = match &step.kind {
            ExprKind::Call { name, .. } => format!("'{}'", name),
            ExprKind::MethodCall { name, dotted: Some(whole), .. } if whole.ends_with(name.as_str()) => format!("'{}'", whole),
            ExprKind::MethodCall { name, .. } => format!("'{}'", name),
            _ => "the task".to_string(),
        };
        // `x |> make().larger()` would read as if `make` were given the value.
        if let ExprKind::MethodCall { value: on, .. } | ExprKind::Invoke { callee: on, .. } = &step.kind
            && matches!(on.kind, ExprKind::Call { .. } | ExprKind::MethodCall { .. } | ExprKind::Invoke { .. })
        {
            return Err(Error::at(
                step.span,
                format!("{} gives the value before it to one call, and this calls what another call gives, which reads as if that one were given the value; to go on from what the pipe gives, put it in parentheses, as in '(x |> f()).g()'", pipe),
            ));
        }
        let args = match &mut step.kind {
            ExprKind::Call { args, .. } | ExprKind::MethodCall { args, .. } | ExprKind::Invoke { args, .. } | ExprKind::Construct { args, .. } if !parenthesized => args,
            ExprKind::Var(name) if is_lambda(name) => {
                return Err(Error::at(
                    step.span,
                    format!("{} gives the value before it to a call, and a lambda written here is called by nothing; name it first, as in 'fixed twice = (n: int) -> int … fi', then write 'x |> twice()'", pipe),
                ));
            }
            ExprKind::Var(name) => {
                return Err(Error::at(step.span, format!("{0} gives the value before it to a call, so '{1}' is written with parentheses, as in '{1}()'", pipe, name)));
            }
            _ => {
                return Err(Error::at(
                    step.span,
                    format!("{} gives the value before it to the call right after it, as in 'xs |> total()', and this is no call; to go on from what the pipe gives, put it in parentheses, as in '(xs |> total()).str()'", pipe),
                ));
            }
        };
        // The region written after the call stays its last argument.
        let written = args.iter().filter(|arg| !matches!(arg.kind, ExprKind::RegionAfter(_))).count();
        if count > written + 1 {
            return Err(Error::at(
                span,
                format!(
                    "{0} gives the value before it to {1} as argument {2}, but the call writes {3}, so argument {4} would be missing; write the arguments before it, or fewer '>'",
                    pipe,
                    callee,
                    count,
                    match written {
                        0 => "none".to_string(),
                        1 => "only 1".to_string(),
                        n => format!("only {}", n),
                    },
                    written + 1
                ),
            ));
        }
        args.insert(count - 1, Expr::new(ExprKind::Piped(Box::new(value)), span));
        // `a |> f() + 1`: the call is the whole step.
        if Self::binary_op(self.peek()).is_some_and(|(_, precedence)| precedence > Self::PIPE_PRECEDENCE) {
            let op = self.peek().describe();
            let symbol = op.trim_matches('\'');
            return Err(Error::at(
                self.peek_span(),
                format!("the call after {0} is the whole of its step, so {1} can't follow it; to work with what the pipe gives, put it in parentheses, as in '(a |> f()) {2} 1'", pipe, op, symbol),
            ));
        }
        if self.check(Token::Range) {
            return Err(Error::at(self.peek_span(), format!("the call after {} is the whole of its step, so '..' can't follow it; put the pipe in parentheses, as in '(a |> f())..b'", pipe)));
        }
        let mut piped = Expr::new(ExprKind::Pipe(Box::new(step)), span);
        for (error, body, at) in plans.into_iter().rev() {
            piped = Expr::new(ExprKind::PlanB { value: Box::new(piped), error, body }, at);
        }
        Ok(piped)
    }

    /// `a + b + c at r`, `(a + b)@r`, `"n = {n}" at r`, or `"n = {n}"@r`, with
    /// `chain` the `+` chain or the text holding values read, and the cursor at
    /// `at` or `@`: new text in `r` holding the chain's operands, or the text's
    /// pieces, each value turned into text in `r`.
    fn parse_join(&mut self, chain: Expr) -> Result<Expr> {
        self.advance(); // `at` or `@`
        let region_span = self.peek_span();
        let region = self.expect_ident()?;
        let span = chain.span;
        let mut parts = Vec::new();
        let mut waiting = vec![chain];
        // Operands left to right: the right operand of each `+` waits until
        // everything on its left is taken.
        while let Some(expr) = waiting.pop() {
            match expr.kind {
                ExprKind::Binary(BinaryOp::Add, left, right) => {
                    waiting.push(*right);
                    waiting.push(*left);
                }
                ExprKind::Join { parts: pieces, region: ref missing, .. } if missing.is_empty() => {
                    interpolated_parts(pieces, &region, region_span, &mut parts);
                }
                _ => parts.push(expr),
            }
        }
        Ok(Expr::new(ExprKind::Join { parts, region, region_span }, span))
    }

    /// `at r` after `expr`, which is not a `+` chain: the region of the call it
    /// is, or of the call a `fail`, `-`, or `not` before it is written on (see
    /// `place_region`), with the plan B that may follow: `f(x) at r { … }`.
    fn parse_call_region(&mut self, mut expr: Expr) -> Result<Expr> {
        let at = self.peek_span();
        self.advance(); // at
        let region_span = self.peek_span();
        let region = self.expect_ident()?;
        match place_region(&mut expr, &region, region_span, true) {
            Placing::Placed => {}
            Placing::Already => return Err(Error::at(at, REGION_ALREADY_NAMED)),
            // `[1, 2] at r`: an array, which is no slice and names no region.
            Placing::Nowhere if matches!(expr.kind, ExprKind::Array(_) | ExprKind::Repeat { .. }) => {
                return Err(Error::at(
                    at,
                    "an array holds its values in place and names no region; a slice of them ends with '...', as in '[1, 2, ...] at r'",
                ));
            }
            Placing::Nowhere => {
                return Err(Error::at(
                    at,
                    "'at r' names the region of what comes before it: a call, a '+' chain of text, or text holding values; a call inside something else names its own, as in 'f(x)@r' or '(f(x) at r)'",
                ));
            }
        }
        self.reject_after_region()?;
        while self.check(Token::LBrace) {
            self.enter()?;
            let planned = self.parse_plan_b(expr);
            self.leave(1);
            expr = planned?;
        }
        Ok(expr)
    }

    /// Refuses what may not follow `at r`, which names the region of everything
    /// before it and so comes last: another `+` part, which would read as if the
    /// region were the whole chain's, a second region, and a field, an item, or a
    /// `^` of the value, which would read as if they were the region's.
    fn reject_after_region(&self) -> Result<()> {
        match self.peek() {
            Token::Dot | Token::LBracket | Token::Caret => Err(Error::at(
                self.peek_span(),
                "'at r' names the region of everything before it, so nothing reads from the value after it; name the region with '@', as in 'x.copy()@r.len', or put the whole in parentheses, as in '(x.copy() at r).len'",
            )),
            Token::Plus => Err(Error::at(
                self.peek_span(),
                "'at r' names the region of everything before it, so it comes after the last part, as in 'a + b + c at r'; a part names a region of its own with '@', as in 'x.str()@r', or in parentheses, as in '(x.str() at r)'",
            )),
            Token::At => Err(Error::at(self.peek_span(), REGION_ALREADY_NAMED)),
            _ => Ok(()),
        }
    }

    /// Text holding values, `"n = {n}"`, at `span` and the cursor: a join of
    /// its pieces whose region is still to come (`at r` or `@r`, which
    /// `parse_join` reads), so it is left empty; the checker asks for one where
    /// none comes. (A function of its own: `parse_primary`'s frame is on the
    /// stack once per nesting level.)
    fn parse_interpolated(&mut self, span: Span) -> Result<Expr> {
        let Token::Interpolated(pieces) = self.peek().clone() else { return Err(Error::internal_at(span, "text holding values that isn't")) };
        self.advance();
        let mut parts = Vec::new();
        for piece in pieces {
            match piece {
                Piece::Text(text) => parts.push(Expr::new(ExprKind::TextLit(text), span)),
                Piece::Value { tokens, close, .. } => parts.push(self.parse_text_value(tokens, &close)?),
            }
        }
        Ok(Expr::new(ExprKind::Join { parts, region: String::new(), region_span: span }, span))
    }

    /// The value inside `{…}` in text, from its `tokens` and its closing `}`:
    /// read by this parser, on those tokens in place of the file's, so nesting
    /// counts as anywhere else.
    fn parse_text_value(&mut self, tokens: Vec<LexToken>, close: &LexToken) -> Result<Expr> {
        let (mut kinds, mut spans) = (Vec::new(), Vec::new());
        for token in tokens {
            if let LexKind::Token(kind) = token.kind {
                kinds.push(kind);
                spans.push(token.start);
            }
        }
        kinds.extend([Token::RBrace, Token::Eof]);
        spans.extend([close.start, close.end]);
        let file_tokens = std::mem::replace(&mut self.tokens, kinds);
        let file_spans = std::mem::replace(&mut self.spans, spans);
        let file_pos = std::mem::replace(&mut self.pos, 0);
        let value = self.parse_expr().and_then(|value| if self.check(Token::RBrace) { Ok(value) } else { Err(self.unexpected("'}' to end the value")) });
        self.tokens = file_tokens;
        self.spans = file_spans;
        self.pos = file_pos;
        value
    }

    fn parse_unary(&mut self) -> Result<Expr> {
        // The noun names a failure; making one is the verb's work.
        if self.check(Token::Failure) {
            return Err(Error::at(self.peek_span(), FAIL_MAKES));
        }
        if matches!(self.peek(), Token::Fail | Token::Minus | Token::Not | Token::Amp) {
            return self.parse_prefixed();
        }
        self.parse_postfix()
    }

    /// `fail e`, `-x`, `not x`, or `&a`. (A function of its own: `parse_unary`'s frame
    /// is on the stack once per nesting level.)
    fn parse_prefixed(&mut self) -> Result<Expr> {
        let span = self.peek_span();
        if self.match_token(Token::Fail) {
            self.enter()?;
            let error = self.parse_operand()?;
            self.leave(1);
            return Ok(Expr::new(ExprKind::Fail(Box::new(error)), span));
        }
        if self.match_token(Token::Amp) {
            self.enter()?;
            let place = self.parse_operand()?;
            self.leave(1);
            return Ok(Expr::new(ExprKind::Ref(Box::new(place)), span));
        }
        let op = if self.match_token(Token::Minus) {
            UnaryOp::Neg
        } else if self.match_token(Token::Not) {
            UnaryOp::Not
        } else {
            return self.parse_postfix();
        };
        self.enter()?;
        let expr = self.parse_operand()?;
        self.leave(1);
        Ok(Expr::new(ExprKind::Unary(op, Box::new(expr)), span))
    }

    /// The value after `fail`, `-`, or `not`, which takes only the value right
    /// after it, and text holding values with its region: `fail "{n} is
    /// negative" at r`.
    fn parse_operand(&mut self) -> Result<Expr> {
        let operand = self.parse_unary()?;
        if self.check(Token::At) && pending_region(&operand) {
            return self.parse_join(operand);
        }
        Ok(operand)
    }

    fn parse_postfix(&mut self) -> Result<Expr> {
        let parenthesized = self.check(Token::LParen);
        let expr = self.parse_primary()?;
        self.parse_suffixes(expr, parenthesized)
    }

    /// What follows `expr`, read by `parse_postfix` (and `parenthesized` when
    /// written in parentheses): a region its text is joined into, fields,
    /// items, `^`, and plan Bs. (A function of its own: `parse_postfix`'s frame
    /// is on the stack once per nesting level.)
    fn parse_suffixes(&mut self, mut expr: Expr, parenthesized: bool) -> Result<Expr> {
        // `(a + b)@r` or `"n = {n}"@r`: text joined into `r`.
        if self.check(Token::AtSign) && (pending_region(&expr) || (parenthesized && matches!(expr.kind, ExprKind::Binary(BinaryOp::Add, ..)))) {
            expr = self.parse_join(expr)?;
        }
        let mut levels = 0;
        loop {
            match self.peek() {
                Token::Dot | Token::LBracket | Token::Caret | Token::LBrace => {
                    self.enter()?;
                    levels += 1;
                    expr = self.parse_suffix(expr)?;
                }
                // `ops[0](x)`, `pick()(x)`, or `(f)(x)`: the task a value holds,
                // called. A name's own call is read with the name.
                Token::LParen if parenthesized || calls_value(&expr) => {
                    self.enter()?;
                    levels += 1;
                    let span = self.peek_span();
                    let args = self.parse_arg_list()?;
                    expr = Expr::new(ExprKind::Invoke { callee: Box::new(expr), args }, span);
                }
                // `f(x)@r`: the region of the call right before it, which a
                // further suffix may follow (`x.str()@r.len`).
                Token::AtSign if takes_region_after(&expr) => {
                    let at = self.peek_span();
                    self.advance(); // @
                    let region_span = self.peek_span();
                    let region = self.expect_ident()?;
                    if place_region(&mut expr, &region, region_span, false) == Placing::Already {
                        self.leave(levels);
                        return Err(Error::at(at, REGION_ALREADY_NAMED));
                    }
                }
                _ => break,
            }
        }
        self.leave(levels);
        // `[1, 2]@r`: an array, which is no slice and names no region.
        if self.check(Token::AtSign) && matches!(expr.kind, ExprKind::Array(_) | ExprKind::Repeat { .. }) {
            return Err(Error::at(
                self.peek_span(),
                "an array holds its values in place and names no region; a slice of them ends with '...', as in '[1, 2, ...]@r'",
            ));
        }
        if self.check(Token::AtSign) {
            return Err(Error::at(
                self.peek_span(),
                "'@r' names the region of the call right before it, as in 'f(x)@r', or of text joined in parentheses or holding values, as in '(a + b)@r' or '\"n = {n}\"@r'",
            ));
        }
        Ok(expr)
    }

    /// `.field`, `[index]`, `^`, or a plan B after `expr`, with the cursor at
    /// its first token. (A function of its own: `parse_postfix`'s frame is on
    /// the stack once per nesting level of every expression.)
    fn parse_suffix(&mut self, expr: Expr) -> Result<Expr> {
        let span = self.peek_span();
        match self.peek() {
            Token::LBrace => self.parse_plan_b(expr),
            Token::Dot => {
                self.advance();
                let span = self.peek_span();
                // `t.#0`: an item of a tuple in a maybe, none past its end.
                if self.check(Token::Hash) {
                    self.advance();
                    let Token::Int(n) = self.peek() else {
                        return Err(Error::at(self.peek_span(), "'.#' reads a tuple's item in a maybe, by its place written as a number, as in 't.#0'"));
                    };
                    let item = format!("#{}", n);
                    self.advance();
                    return Ok(Expr::new(ExprKind::Field(Box::new(expr), item), span));
                }
                // `t.0`: an item of a tuple.
                if let Token::Int(n) = self.peek() {
                    let item = n.to_string();
                    self.advance();
                    return Ok(Expr::new(ExprKind::Field(Box::new(expr), item), span));
                }
                let field = self.expect_member()?;
                let expr = Expr::new(ExprKind::Field(Box::new(expr), field), span);
                // `m.Option<int>.None`: a kind of a module, given its types.
                if self.at_kind_types()
                    && let Some((kind, _)) = dotted_name(&expr)
                {
                    return self.parse_kind_types_use(kind, span);
                }
                // A `<` here opens the types a use gives (a method takes none of
                // its own, which `dotted_name_use` says), never a comparison:
                // see `at_type_args`.
                if self.check(Token::LParen) || self.at_type_args() || (self.check(Token::LBrace) && self.at_shape_literal()) {
                    return self.dotted_name_use(expr);
                }
                Ok(expr)
            }
            Token::LBracket => {
                self.advance();
                // `xs[#i]`: the item at `i` in a maybe, none out of range.
                if self.match_token(Token::Hash) {
                    let index = self.parse_expr()?;
                    if let ExprKind::Range(..) = index.kind {
                        return Err(Error::at(
                            index.span,
                            "'[#…]' reads one item, or none when it isn't there; a part is written without '#', as in 'xs[a..b]'",
                        ));
                    }
                    self.expect(Token::RBracket)?;
                    return Ok(Expr::new(ExprKind::Lookup(Box::new(expr), Box::new(index)), span));
                }
                let index = self.parse_expr()?;
                self.expect(Token::RBracket)?;
                Ok(Expr::new(ExprKind::Index(Box::new(expr), Box::new(index)), span))
            }
            _ => {
                self.advance(); // ^
                Ok(Expr::new(ExprKind::Deref(Box::new(expr)), span))
            }
        }
    }

    fn parse_primary(&mut self) -> Result<Expr> {
        // Each kind of value is read by a function of its own: this one's
        // frame is on the stack once per nesting level.
        let span = self.peek_span();
        match self.peek() {
            Token::Parts => Err(Error::at(span, PARTS_AS_OR)),
            Token::Pipe(_) => Err(Error::at(
                span,
                format!(
                    "{} gives the value before it to the call after it, so a value comes first, as in 'xs |> total()'; a line starting with it goes on with the chain of a line that ends with a value",
                    self.peek().describe()
                ),
            )),
            Token::Interpolated(_) => self.parse_interpolated(span),
            Token::New => self.parse_new(),
            Token::LBracket if self.at_kept() => self.parse_closure_lambda(),
            Token::LBracket => self.parse_array(),
            Token::LParen if self.at_full_lambda() => self.parse_lambda(),
            Token::LParen if self.at_shortcut_lambda() => self.parse_shortcut_lambda_parens(None, span),
            Token::LParen => self.parse_parenthesized(),
            Token::Match | Token::When => self.parse_choice(span),
            // A loop that gives a value, and one named so where no variable names it.
            Token::Loop | Token::Each | Token::Endless => self.parse_loop_value(span, None),
            Token::Ident(name) if matches!(self.tokens.get(self.pos + 1), Some(Token::FatArrow)) => {
                let name = (name.clone(), span);
                self.advance(); // the name
                self.advance(); // =>
                if !matches!(self.peek(), Token::Loop | Token::Each | Token::Endless) {
                    return Err(Error::at(
                        self.peek_span(),
                        format!("'{} =>' names a loop, so 'loop', 'each' or 'endless' follows it", name.0),
                    ));
                }
                let at = self.peek_span();
                self.parse_loop_value(at, Some(name))
            }
            Token::Ident(name) if matches!(self.tokens.get(self.pos + 1), Some(Token::Arrow)) => {
                let name = name.clone();
                self.advance(); // the name
                self.parse_shortcut_lambda(vec![(name, span)], None, span)
            }
            Token::Underscore if matches!(self.tokens.get(self.pos + 1), Some(Token::Arrow)) => {
                self.advance(); // _
                self.parse_shortcut_lambda(vec![(DISCARD.to_string(), span)], None, span)
            }
            // `.this`, `.field`, or `.method(…)`: the value a method is called
            // on, and what it holds.
            Token::Dot if matches!(self.tokens.get(self.pos + 1), Some(Token::Ident(_) | Token::Type))
                || (self.tokens.get(self.pos + 1) == Some(&Token::New) && self.tokens.get(self.pos + 2) == Some(&Token::LParen)) =>
            {
                self.parse_member_of_this(span)
            }
            Token::Ident(_) => self.parse_name_use(span),
            _ => self.parse_literal(span),
        }
    }

    /// A literal at `span` and the cursor: a number, a char, text, `true`,
    /// `false`, or `none`.
    fn parse_literal(&mut self, span: Span) -> Result<Expr> {
        let kind = match self.peek() {
            Token::Int(n) => ExprKind::IntLit(*n),
            Token::Float(n) => ExprKind::FloatLit(*n),
            Token::Char(c) => ExprKind::CharLit(*c),
            Token::Text(s) => ExprKind::TextLit(s.clone()),
            Token::True => ExprKind::BoolLit(true),
            Token::False => ExprKind::BoolLit(false),
            Token::None => ExprKind::NoneLit,
            _ => return Err(self.unexpected("an expression")),
        };
        self.advance();
        Ok(Expr::new(kind, span))
    }

    /// A `match` or a `when` at `span` and the cursor, where a value is
    /// expected: its branches give the value, each on the last line of its
    /// block.
    fn parse_choice(&mut self, span: Span) -> Result<Expr> {
        self.enter()?;
        let chosen = if self.check(Token::Match) { self.parse_match()? } else { self.parse_when()? };
        self.leave(1);
        Ok(Expr::new(ExprKind::Choice(Box::new(chosen)), span))
    }

    /// A `loop`, `each` or `endless` at `span` and the cursor, where a value is
    /// expected, named `name` with `=>`: its passes give the value, each on the
    /// last line of its body (see `ValueLoop`).
    fn parse_loop_value(&mut self, span: Span, name: Option<(String, Span)>) -> Result<Expr> {
        self.enter()?;
        let stmt = match self.peek() {
            Token::Loop => self.parse_loop(),
            Token::Each => self.parse_each(),
            _ => self.parse_endless(),
        };
        self.leave(1);
        Ok(self.loop_value(stmt?, name, true, span))
    }

    /// The loop `stmt` as a value (see `ValueLoop`).
    fn loop_value(&mut self, stmt: Stmt, name: Option<(String, Span)>, gives: bool, span: Span) -> Expr {
        self.loop_values = true;
        let arrow = name.is_some();
        let value = ValueLoop { stmt, name, arrow, gives, open: (false, false) };
        Expr::new(ExprKind::LoopValue(Box::new(value)), span)
    }

    /// `.this`, `.field`, or `.method(…)` at `span`, with the cursor at the
    /// dot: the value a method is called on, and what it holds. Only a method
    /// has one, which the checker says; here it is a value like any other.
    fn parse_member_of_this(&mut self, span: Span) -> Result<Expr> {
        self.advance(); // .
        let at = self.peek_span();
        let member = self.expect_member()?;
        let value = Expr::new(ExprKind::Var(THIS.to_string()), span);
        if member == THIS_WORD {
            return Ok(value);
        }
        let expr = Expr::new(ExprKind::Field(Box::new(value), member), at);
        // A `{` after it is a plan B on the member, which `parse_suffix`
        // reads: no shape is named `.x`.
        if self.check(Token::LParen) || self.at_type_args() {
            return self.dotted_name_use(expr);
        }
        Ok(expr)
    }

    /// A value that starts with a name, at `span` and the cursor: a variable,
    /// a call, or a shape literal. (A function of its own: `parse_primary`'s
    /// frame is on the stack once per nesting level.)
    fn parse_name_use(&mut self, span: Span) -> Result<Expr> {
        let name = self.expect_ident()?;
        let name = self.value_name(name)?;
        if name == SCAN && self.check(Token::LParen) {
            return self.parse_scan(span);
        }
        if name == TEXT_CAST && self.check(Token::LParen) {
            return self.parse_text_cast(span);
        }
        if self.at_kind_types() {
            return self.parse_kind_types_use(name, span);
        }
        // The types a use gives are read only where a call or a shape
        // literal follows them (see `at_type_args`), so one of `(` and
        // `{` comes next for certain.
        let kind = if self.at_type_args() {
            let types = self.parse_type_args()?;
            if self.check(Token::LParen) {
                ExprKind::Call { name, types, args: self.parse_arg_list()? }
            } else {
                self.parse_struct_init(name, types)?
            }
        } else if self.check(Token::LBrace) && self.at_shape_literal() {
            self.parse_struct_init(name, Vec::new())?
        } else if self.check(Token::LParen) {
            ExprKind::Call { name, types: Vec::new(), args: self.parse_arg_list()? }
        } else {
            ExprKind::Var(name)
        };
        Ok(Expr::new(kind, span))
    }

    /// `scan(prompt) at r` or `scan(prompt)@r`, with the cursor at `(`. (A
    /// function of its own: `parse_primary`'s frame is on the stack once per
    /// nesting level.)
    fn parse_scan(&mut self, span: Span) -> Result<Expr> {
        let args = self.parse_arg_list()?;
        if self.check(Token::In) {
            // A read that names a type reads a number, which needs no region, so
            // nothing follows its call; the checker settles what a type may be.
            let hint = match args.len() {
                1 => "'scan' names its region with 'at', as in 'scan(\"Name: \") at r' (or 'scan(\"Name: \")@r'); 'in' counts through a range",
                _ => "'scan' reading a number names no region, so nothing follows the call, as in 'scan(\"Age: \", int)'; 'in' counts through a range",
            };
            return Err(Error::at(self.peek_span(), hint));
        }
        // A line read into a region names one, and a number read from a line
        // names none, which the checker tells apart by what the call takes. The
        // region comes after, `at r` or `@r`, as for any call (see
        // `place_region`); until then its position is where one would be written.
        let region_span = self.peek_span();
        Ok(Expr::new(ExprKind::Scan { args, region: None, region_span }, span))
    }


    /// Whether the cursor, at `(`, starts a full lambda: parentheses with typed parameters
    /// or empty parentheses followed by `->` and a return type with `<-` or a multiline block.
    fn at_full_lambda(&self) -> bool {
        let next = self.tokens.get(self.pos + 1);
        let is_empty = matches!(next, Some(Token::RParen));
        let takes = is_empty
            || matches!(next, Some(Token::Flex | Token::Fixed))
            || (matches!(next, Some(Token::Ident(_))) && self.tokens.get(self.pos + 2) == Some(&Token::Colon));
        if !takes {
            return false;
        }
        let mut depth = 0usize;
        let mut arrow_at = None;
        for (at, token) in self.tokens.iter().enumerate().skip(self.pos) {
            match token {
                Token::LParen | Token::LBracket => depth += 1,
                Token::RParen | Token::RBracket => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        if *token == Token::RParen && self.tokens.get(at + 1) == Some(&Token::Arrow) {
                            arrow_at = Some(at + 1);
                        }
                        break;
                    }
                }
                Token::Newline | Token::Eof => return false,
                _ => {}
            }
        }
        let Some(arrow) = arrow_at else { return false };
        if !is_empty {
            return true;
        }
        // At `() ->`: check tokens on the same line after `->`.
        let mut line_tokens = Vec::new();
        for token in self.tokens.iter().skip(arrow + 1) {
            match token {
                Token::Newline | Token::Eof => break,
                tok => line_tokens.push(tok),
            }
        }
        if line_tokens.iter().any(|tok| matches!(tok, Token::Send | Token::Fi)) {
            return true;
        }
        let is_type_ident = |name: &str| {
            matches!(
                name,
                "int" | "int8" | "int16" | "int32" | "int64" | "int512" | "Int512"
                    | "uint" | "uint8" | "uint16" | "uint32" | "uint64"
                    | "float" | "float32" | "float64"
                    | "bool" | "char" | "byte" | "text" | "nothing" | "any" | "region"
            ) || name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        };
        let first_after_arrow = line_tokens.first();
        let is_bracket_type = matches!(first_after_arrow, Some(Token::LBracket))
            && line_tokens.get(1).is_some_and(|tok| {
                matches!(tok, Token::Ident(name) if is_type_ident(name))
                    || matches!(tok, Token::LBracket)
            });
        let is_type_start = is_bracket_type
            || matches!(first_after_arrow, Some(Token::Maybe))
            || matches!(first_after_arrow, Some(Token::Ident(name)) if is_type_ident(name));

        if is_type_start {
            let is_call = matches!(first_after_arrow, Some(Token::Ident(_)))
                && line_tokens.get(1).copied() == Some(&Token::LParen);
            if is_call {
                return false;
            }
            let has_expr_op = line_tokens.iter().any(|tok| {
                matches!(
                    tok,
                    Token::Plus
                        | Token::Minus
                        | Token::Star
                        | Token::Slash
                        | Token::Percent
                        | Token::Assign
                        | Token::Eq
                        | Token::Ne
                        | Token::Lt
                        | Token::Gt
                        | Token::Le
                        | Token::Ge
                        | Token::Pipe(_)
                        | Token::Dot
                )
            });
            let has_lit = !is_bracket_type
                && line_tokens.iter().any(|tok| {
                    matches!(
                        tok,
                        Token::Int(_)
                            | Token::Float(_)
                            | Token::Text(_)
                            | Token::Interpolated(_)
                            | Token::Char(_)
                            | Token::True
                            | Token::False
                            | Token::None
                    )
                });
            if !has_expr_op && !has_lit {
                return true;
            }
        }
        false
    }

    /// Whether the cursor, at `(`, starts a shortcut lambda:
    /// `(x) ->`, `(a, b) ->`, or `() ->` without `<-` on the line.
    fn at_shortcut_lambda(&self) -> bool {
        if self.at_full_lambda() {
            return false;
        }
        let mut depth = 0usize;
        let mut arrow_at = None;
        let mut rparen_at = None;
        for (at, token) in self.tokens.iter().enumerate().skip(self.pos) {
            match token {
                Token::LParen | Token::LBracket => depth += 1,
                Token::RParen | Token::RBracket => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        if *token == Token::RParen && self.tokens.get(at + 1) == Some(&Token::Arrow) {
                            arrow_at = Some(at + 1);
                            rparen_at = Some(at);
                        }
                        break;
                    }
                }
                Token::Newline | Token::Eof => return false,
                _ => {}
            }
        }
        let (Some(_arrow), Some(rparen)) = (arrow_at, rparen_at) else { return false };
        // Verify all tokens inside `(...)` are comma-separated idents or underscores
        for token in &self.tokens[self.pos + 1..rparen] {
            if !matches!(token, Token::Ident(_) | Token::Underscore | Token::Comma) {
                return false;
            }
        }
        true
    }

    /// `(params) -> type`, then its body on the lines after it or on its own
    /// line, and `fi`, at `(`: a task with no name, which takes nothing from the
    /// task around it. It becomes a task of the file (see `ast::lambda_name`),
    /// and the value here names it.
    fn parse_lambda(&mut self) -> Result<Expr> {
        // Its header and the task it becomes are read and made by functions
        // of their own: this one's frame is on the stack once per lambda
        // nested in another, and it counts as a level of its own, as a block
        // does, since its frames are larger than a block's.
        self.enter()?;
        let span = self.peek_span();
        let (params, ret, results) = self.parse_lambda_header(span)?;
        let body = if matches!(self.peek(), Token::Newline | Token::Eof) {
            let body = self.parse_block()?;
            self.expect_fi("lambda", span)?;
            body
        } else {
            self.parse_lambda_line(span)?
        };
        self.leave(1);
        Ok(self.add_lambda(span, params, ret, results, body, None, false))
    }

    /// `[@r](params) -> type`, its body, and `fi`, at `[`: a lambda that keeps
    /// what it uses from the code around it in region `r`, with `[flex @r]`
    /// changing its copies and `[bind @r]` letting their contents change (see
    /// `ExprKind::Closure`).
    fn parse_closure_lambda(&mut self) -> Result<Expr> {
        let span = self.peek_span();
        let (region, mode, _, places) = self.parse_kept()?;
        let kept = Kept { region: region.clone(), mode, places: places.clone() };
        if self.check(Token::LParen) && self.at_shortcut_lambda() {
            return self.parse_shortcut_lambda_parens(Some(kept), span);
        }
        if matches!(self.peek(), Token::Ident(_) | Token::Underscore) && self.tokens.get(self.pos + 1) == Some(&Token::Arrow) {
            let p_span = self.peek_span();
            let name = if self.match_token(Token::Underscore) { DISCARD.to_string() } else { self.expect_ident()? };
            return self.parse_shortcut_lambda(vec![(name, p_span)], Some(kept), span);
        }
        if !(self.check(Token::LParen) && self.at_full_lambda()) {
            let mode_word = match mode {
                KeptMode::Plain => "",
                KeptMode::Flex => "flex ",
                KeptMode::Bind => "bind ",
            };
            let written = format!("[{}@{}]", mode_word, region);
            return Err(Error::at(span, format!("'{0}' starts a lambda that keeps what it uses, so a lambda's header follows it, as in '{0}(x: int) -> int'", written)));
        }
        // A level of its own, as for any lambda (see `parse_lambda`).
        self.enter()?;
        let header = self.peek_span();
        let (params, ret, results) = self.parse_lambda_header(header)?;
        let body = if matches!(self.peek(), Token::Newline | Token::Eof) {
            let body = self.parse_block()?;
            self.expect_fi("lambda", span)?;
            body
        } else {
            self.parse_lambda_line(span)?
        };
        self.leave(1);
        let ExprKind::Var(lambda) = self.add_lambda(span, params, ret, results, body, Some(kept), false).kind else {
            return Err(Error::internal_at(span, "a lambda named by no variable"));
        };
        Ok(Expr::new(ExprKind::Closure { lambda, region, mode, places }, span))
    }

    /// A shortcut one-line lambda without `<-` or `fi`: `x -> x * 2`, `(a, b) -> a + b`, or `() -> 42`.
    fn parse_shortcut_lambda(&mut self, params: Vec<(String, Span)>, kept: Option<Kept>, span: Span) -> Result<Expr> {
        if self.in_generic {
            return Err(Error::at(span, "a lambda can't be written in a task or method that takes types yet; write a task of its own and name it, as in 'apply(double, x)'"));
        }
        self.enter()?;
        self.expect(Token::Arrow)?;
        let body_expr = self.parse_expr()?;
        self.leave(1);
        let body = vec![Stmt::new(StmtKind::Send(vec![body_expr]), span)];
        let fields: Vec<Field> = params.into_iter().map(|(name, p_span)| Field {
            name,
            ty: Type::Unknown,
            span: p_span,
            ty_span: p_span,
            private: false,
            read: false,
            default_region: false,
            passing: Passing::Copy,
            default: None,
        }).collect();
        let is_closure = kept.is_some();
        let (region, mode, places) = if let Some(k) = &kept {
            (k.region.clone(), k.mode, k.places.clone())
        } else {
            (String::new(), KeptMode::Plain, Vec::new())
        };
        let lambda_expr = self.add_lambda(span, fields, Type::Unknown, Vec::new(), body, kept, true);
        if is_closure {
            let ExprKind::Var(lambda) = lambda_expr.kind else {
                return Err(Error::internal_at(span, "a lambda named by no variable"));
            };
            Ok(Expr::new(ExprKind::Closure { lambda, region, mode, places }, span))
        } else {
            Ok(lambda_expr)
        }
    }

    /// Parenthesized shortcut lambda: `(x) -> expr`, `(a, b) -> expr`, or `() -> expr`.
    fn parse_shortcut_lambda_parens(&mut self, kept: Option<Kept>, span: Span) -> Result<Expr> {
        self.expect(Token::LParen)?;
        let mut params = Vec::new();
        if !self.check(Token::RParen) {
            loop {
                let p_span = self.peek_span();
                let name = if self.match_token(Token::Underscore) {
                    DISCARD.to_string()
                } else {
                    self.expect_ident()?
                };
                params.push((name, p_span));
                if !self.match_token(Token::Comma) {
                    break;
                }
            }
        }
        self.expect(Token::RParen)?;
        self.parse_shortcut_lambda(params, kept, span)
    }

    /// The header of a lambda at `span` and the cursor, up to its body.
    fn parse_lambda_header(&mut self, span: Span) -> Result<(Vec<Field>, Type, Vec<ResultDef>)> {
        if self.in_generic {
            return Err(Error::at(span, "a lambda can't be written in a task or method that takes types yet; write a task of its own and name it, as in 'apply(double, x)'"));
        }
        let params = self.parse_field_list(Slot::Param)?;
        if let Some(value) = params.iter().find_map(|param| param.default.as_ref()) {
            return Err(Error::at(
                value.span,
                "a lambda's parameters have no values to fall back on: a lambda is called through its type, which names every value; a task's parameters can have them",
            ));
        }
        let arrow = self.peek_span();
        self.expect(Token::Arrow)?;
        if matches!(self.peek(), Token::Newline | Token::Fi | Token::Eof) {
            return Err(Error::at(arrow, "a lambda names what it sends after '->', or 'nothing', as in '(x: int) -> int'"));
        }
        let mut results = Vec::new();
        let ret = self.parse_results(&mut results)?;
        if matches!(ret, Type::Results(_)) {
            return Err(Error::at(arrow, "a lambda sends one value or nothing, as a task value does; send a tuple for several, as in '-> (int, int)'"));
        }
        if self.check(Token::LBrace) {
            return Err(Error::at(self.peek_span(), "a lambda's body follows its header, on its line or the lines after it, and closes with 'fi'"));
        }
        Ok((params, ret, results))
    }

    /// Makes the lambda at `span` a task of the file, and gives the value
    /// naming it.
    fn add_lambda(&mut self, span: Span, params: Vec<Field>, ret: Type, results: Vec<ResultDef>, body: Block, kept: Option<Kept>, shortcut: bool) -> Expr {
        self.lambda_count += 1;
        let name = lambda_name(self.lambda_count);
        self.lambdas.push(Item::Task(TaskDef {
            name: name.clone(),
            types: Vec::new(),
            given: Vec::new(),
            params,
            ret,
            results,
            body,
            span,
            private: true,
            native: false,
            owner: None,
            decorators: Vec::new(),
            passes: Vec::new(),
            wrappers: Vec::new(),
            kept,
            shortcut,
        }));
        Expr::new(ExprKind::Var(name), span)
    }

    /// A lambda's body on the line of its header, opened at `opened`, and its
    /// `fi`, which ends the line's statements: `(x: int) -> int <- x * 2 fi`.
    fn parse_lambda_line(&mut self, opened: Span) -> Result<Block> {
        // It may start by sending, though `<-` starts no line there.
        let outer = self.send_after_header.replace(self.pos);
        let mut body = Vec::new();
        let result = loop {
            if self.match_token(Token::Fi) {
                break Ok(());
            }
            if matches!(self.peek(), Token::Newline | Token::Eof) {
                break Err(self.unclosed_lambda_line(opened));
            }
            match self.parse_stmt() {
                Ok(stmt) => body.push(stmt),
                Err(error) => break Err(error),
            }
        };
        self.send_after_header = outer;
        result.map(|()| body)
    }

    /// The mistake in a lambda opened at `opened` whose body starts on its
    /// header's line and has no `fi` on it.
    fn unclosed_lambda_line(&self, opened: Span) -> Error {
        Error::at(
            self.peek_span(),
            format!(
                "a lambda whose body starts on its header's line closes with 'fi' on that line, as in '(x: int) -> int <- x * 2 fi'; for a longer one, start the body on the next line (the lambda is at {}:{})",
                opened.line, opened.col
            ),
        )
    }

    /// `(value)`, or a tuple, `(a, b, …)`, on one line.
    fn parse_parenthesized(&mut self) -> Result<Expr> {
        let span = self.peek_span();
        self.advance(); // (
        let first = self.parse_expr()?;
        // `(args...)`: a group's values made into a tuple, as `args.tuple` is.
        if self.check(Token::Ellipsis) {
            self.advance(); // ...
            if !self.check(Token::RParen) {
                return Err(Error::at(self.peek_span(), "a group's values make a tuple on their own, as in '(args...)'"));
            }
            self.advance(); // )
            return Ok(Expr::new(ExprKind::Field(Box::new(first), GROUP_TUPLE_WRITTEN.to_string()), span));
        }
        if !self.check(Token::Comma) {
            self.expect(Token::RParen)?;
            return Ok(first);
        }
        // A tuple holds its values in a frame of its own, as an array does.
        self.enter()?;
        let mut values = vec![first];
        let result = loop {
            if !self.match_token(Token::Comma) {
                break self.expect(Token::RParen);
            }
            if self.check(Token::Newline) {
                break Err(Error::at(self.peek_span(), "a tuple is written on one line, as in '(1, \"Ada\")'"));
            }
            // `(1,)`: one value in parentheses is only grouped, as one type is.
            // `(1, 2,)`: the comma is the one after the last item.
            if self.check(Token::RParen) {
                break Err(match values.as_slice() {
                    [_] => Error::at(span, TUPLE_ITEMS.to_string()),
                    _ => Error::at(self.peek_span(), TUPLE_TRAILING_COMMA.to_string()),
                });
            }
            match self.parse_expr() {
                Ok(value) => values.push(value),
                Err(error) => break Err(error),
            }
        };
        self.leave(1);
        result.map(|()| Expr::new(ExprKind::Tuple(values), span))
    }

    /// `[a, b, c]` or `[value#count]`, on one line.
    fn parse_array(&mut self) -> Result<Expr> {
        // An array holds its values in a frame of its own, and takes more stack
        // than most expressions, so it counts as a level of its own.
        self.enter()?;
        let result = self.parse_array_inner();
        self.leave(1);
        result
    }

    fn parse_array_inner(&mut self) -> Result<Expr> {
        let span = self.peek_span();
        self.advance(); // [
        if self.check(Token::RBracket) {
            return Err(Error::at(span, "an array holds at least one value, as in '[0#4]'"));
        }
        // `[...]`: a slice of no values, which says nothing of their type.
        if self.check(Token::Ellipsis) {
            return Err(Error::at(span, "a slice of no values can't tell what type its items are; name it, as in 'new [int]() at r'"));
        }
        if self.check(Token::Colon) {
            return Err(Error::at(span, "a map written by its pairs holds at least one; an empty map names its types, as in 'new [text: int]() at r'"));
        }
        let first = self.parse_expr()?;
        if self.match_token(Token::Hash) {
            let count = self.parse_expr()?;
            self.expect_array_end(span, false)?;
            return Ok(Expr::new(ExprKind::Repeat { value: Box::new(first), count: Box::new(count) }, span));
        }
        // `["a": 1, "b": 2]`: the pairs of a map.
        if self.match_token(Token::Colon) {
            let value = self.parse_expr()?;
            let pairs = self.parse_pairs_after(vec![(first, value)], span, false)?;
            return self.pairs_with_region(pairs, span);
        }
        let mut values = vec![first];
        while self.match_token(Token::Comma) {
            if self.check(Token::Newline) {
                return Err(Error::at(self.peek_span(), "an array is written on one line, as in '[1, 2, 3]'"));
            }
            // `[1, 2, ...]`: the values of a slice, which can grow.
            if self.match_token(Token::Ellipsis) {
                self.expect_array_end(span, false)?;
                let value = Expr::new(ExprKind::Array(values), span);
                return self.parse_shortcut(value, span);
            }
            values.push(self.parse_expr()?);
        }
        // `[0; 4]`: one value, then the `;` of a count written as other
        // languages write one, which started a comment.
        self.expect_array_end(span, values.len() == 1)?;
        Ok(Expr::new(ExprKind::Array(values), span))
    }

    /// The pairs of a map after its first, up to and including the `]`; with
    /// `names`, a key is a bare name, its own text, or text in quotes (the
    /// `[:V]` form of `new`).
    fn parse_pairs_after(&mut self, mut pairs: Vec<(Expr, Expr)>, opened: Span, names: bool) -> Result<Vec<(Expr, Expr)>> {
        while self.match_token(Token::Comma) {
            if self.check(Token::Newline) {
                return Err(Error::at(self.peek_span(), "a map's pairs are written on one line, as in '[\"a\": 1, \"b\": 2]'"));
            }
            let key = if names { self.parse_named_key()? } else { self.parse_expr()? };
            if !self.match_token(Token::Colon) {
                return Err(self.unexpected("':' and the value that goes with the key"));
            }
            pairs.push((key, self.parse_expr()?));
        }
        if !self.match_token(Token::RBracket) {
            if matches!(self.peek(), Token::Newline | Token::Eof) {
                return Err(Error::at(self.peek_span(), format!("expected ']' to close the map at {}:{}; a map's pairs are written on one line", opened.line, opened.col)));
            }
            return Err(self.unexpected("',' and another pair, or ']'"));
        }
        Ok(pairs)
    }

    /// A key of the `[:V]` form: a bare name, which is its own text, or text.
    fn parse_named_key(&mut self) -> Result<Expr> {
        let span = self.peek_span();
        match self.peek().clone() {
            Token::Ident(name) => {
                self.advance();
                Ok(Expr::new(ExprKind::TextLit(name), span))
            }
            Token::Text(text) => {
                self.advance();
                Ok(Expr::new(ExprKind::TextLit(text), span))
            }
            _ => Err(Error::at(span, "a key of a '[:V]' map is a name, which is its own text, or text in quotes, as in '[width: 1.5, \"full name\": 2.0]'")),
        }
    }

    /// A map's pairs, with the region written after them, if any: a shortcut
    /// for a map of its own, or pairs given to `new`.
    fn pairs_with_region(&mut self, pairs: Vec<(Expr, Expr)>, span: Span) -> Result<Expr> {
        let value = Expr::new(ExprKind::Pairs(pairs), span);
        if self.check(Token::At) || self.check(Token::AtSign) {
            return self.parse_shortcut(value, span);
        }
        Ok(value)
    }

    /// The region after a shortcut's values, `[1, 2, ...] at r` or
    /// `[1, 2, ...]@r`, or none, for the block's default; `value` is what the
    /// brackets hold.
    fn parse_shortcut(&mut self, value: Expr, span: Span) -> Result<Expr> {
        let (region, region_span) = if self.match_token(Token::At) || self.match_token(Token::AtSign) {
            let region_span = self.peek_span();
            (Some(self.expect_ident()?), region_span)
        } else {
            (None, self.peek_span())
        };
        Ok(Expr::new(ExprKind::Shortcut { value: Box::new(value), region, region_span }, span))
    }

    /// The `]` closing the array opened at `opened`; `one` when it holds one
    /// value so far.
    fn expect_array_end(&mut self, opened: Span, one: bool) -> Result<()> {
        if self.match_token(Token::RBracket) {
            return Ok(());
        }
        if matches!(self.peek(), Token::Newline | Token::Eof) {
            let count = if one { ", with a count after '#', as in '[0#4]' (';' starts a comment)" } else { "" };
            return Err(Error::at(self.peek_span(), format!("expected ']' to close the array at {}:{}; an array is written on one line{}", opened.line, opened.col, count)));
        }
        Err(self.unexpected("']'"))
    }

    /// A call or shape literal after a field, as in `p.area()`, `p.x.f()`, or
    /// `geometry.Point { x: 1 }` where `geometry` isn't a module this file
    /// includes (`value_name` joins the names that are). A call is a method
    /// called on the value, which keeps the reading as one dotted name too when
    /// the value is a name: `Light.Red(…)` is a variant and `m.f(…)` a module's
    /// task, and only the file's names tell them apart (see the resolver). A
    /// change to a slice, `xs.add(…)` or `xs.del(…)`, and a copy, `x.copy()`,
    /// are neither. (A function of its own: `parse_postfix`'s frame is on the
    /// stack once per nesting level.)
    fn dotted_name_use(&mut self, expr: Expr) -> Result<Expr> {
        if matches!(&expr.kind, ExprKind::Field(_, word) if word == COPY) && self.check(Token::LParen) {
            return self.parse_copy(expr);
        }
        if matches!(&expr.kind, ExprKind::Field(_, word) if word == STR) && self.check(Token::LParen) {
            return self.parse_str(expr);
        }
        if let ExprKind::Field(_, word) = &expr.kind
            && let Some(op) = EditOp::from_word(word)
            && self.check(Token::LParen)
        {
            let args = self.parse_arg_list()?;
            let ExprKind::Field(items, _) = expr.kind else { return Err(Error::internal_at(expr.span, "a change without its slice")) };
            let edit = Expr::new(ExprKind::Edit { items, op, args }, expr.span);
            // `xs.added(v)` and `xs.without(i)` make a copy, which goes where a
            // copy goes: the region after them, `at r`, or the place's.
            if op.makes_copy() {
                if self.check(Token::In) {
                    return Err(Error::at(self.peek_span(), format!("'{0}' names its region with 'at', as in 'xs.{0}(v) at r' (or 'xs.{0}(v)@r'); 'in' counts through a range", op.word())));
                }
                return Ok(Expr::new(ExprKind::Copy { value: Box::new(edit), region: None, region_span: expr.span }, expr.span));
            }
            return Ok(edit);
        }
        // `p.pick<int>(…)`: a task of a module is named before its types (see
        // `value_name`), so what is left here is a method, which takes none of
        // its own yet, or a variant, which takes none at all.
        let dotted = dotted_name(&expr);
        if self.at_type_args()
            && let ExprKind::Field(_, member) = &expr.kind
        {
            return Err(Error::at(
                self.peek_span(),
                format!(
                    "a method takes no types of its own yet, and a variant takes none of its own (a kind's types are written after its name, as in 'Option<int>.Some(…)'), so '{0}' is given none; call it as '{0}(…)'",
                    member
                ),
            ));
        }
        if self.check(Token::LParen) {
            let span = expr.span;
            let ExprKind::Field(value, name) = expr.kind else { return Err(Error::internal_at(span, "a call without the value it is on")) };
            // `dotted_name` has read the whole of it, the method's name too.
            let dotted = dotted.map(|(whole, _)| whole);
            let args = self.parse_arg_list()?;
            return Ok(Expr::new(ExprKind::MethodCall { value, name, args, dotted }, span));
        }
        let Some((name, span)) = dotted else { return Ok(expr) };
        // `m.Box<int> { … }`: the types a use gives, told from a comparison by
        // the `{` that must follow the closing `>`.
        let kind = if self.at_type_args() {
            let types = self.parse_type_args()?;
            self.parse_struct_init(name, types)?
        } else {
            self.parse_struct_init(name, Vec::new())?
        };
        Ok(Expr::new(kind, span))
    }

    /// Whether the cursor is at the `<` of the types a use gives: a `<` whose
    /// matching `>` is followed by `(` (a call) or `{` (a shape literal), with
    /// only what a type is written with in between. Comparisons never look like
    /// that: `a < b > (c)` would compare a bool with `>`, and `a < b > {` has no
    /// value after the `>` at all, so both are errors either way.
    fn at_type_args(&self) -> bool {
        // `Box<> { … }`: no value starts with `{`, so this `<>` is no 'not
        // similar' but types left empty (see `parse_type_args`).
        if self.check(Token::NotSimilar) {
            return matches!(self.tokens.get(self.pos + 1), Some(Token::LBrace));
        }
        // A call follows the types a task is given, and a literal the types a
        // shape is given.
        self.type_args_close().is_some_and(|close| matches!(self.tokens.get(close + 1), Some(Token::LParen | Token::LBrace)))
    }

    /// Whether the cursor is at the `<` of the types a kind is given, before
    /// one of its variants: `Option<int>.None`, with the `.` touching the `>`.
    /// A comparison rarely looks like that: `a < b >.x` would compare a bool
    /// with `>`, and in `(a < b, c > .x)` a space parts `>` from the field of
    /// this that `.x` is.
    fn at_kind_types(&self) -> bool {
        self.type_args_close().is_some_and(|close| {
            let touching = match (self.spans.get(close), self.spans.get(close + 1)) {
                (Some(gt), Some(dot)) => gt.line == dot.line && gt.col + 1 == dot.col,
                _ => false,
            };
            touching && self.tokens.get(close + 1) == Some(&Token::Dot) && matches!(self.tokens.get(close + 2), Some(Token::Ident(_)))
        })
    }

    /// The position of the `>` closing the types a use gives, when the cursor
    /// is at their `<`, with only what a type is written with in between.
    fn type_args_close(&self) -> Option<usize> {
        if !self.check(Token::Lt) {
            return None;
        }
        let mut depth = 0usize;
        // Parentheses in types group one or hold a tuple's, so they pair up
        // inside the types, and no `>` closes anything inside them: `(i < n),
        // m > (k)` and `i < n, (m > (k))` are comparisons.
        let mut grouped = 0usize;
        for p in self.pos + 1.. {
            match self.tokens.get(p) {
                Some(Token::Gt) if grouped > 0 && depth == 0 => return None,
                // `>` closes the types, or, nested, one type's own.
                Some(Token::Gt) if depth == 0 => return Some(p),
                Some(Token::Lt) => depth += 1,
                Some(Token::Gt) => depth -= 1,
                Some(Token::LParen) => grouped += 1,
                Some(Token::RParen) if grouped == 0 => return None,
                Some(Token::RParen) => grouped -= 1,
                Some(token) if token.writes_type() => {}
                _ => return None,
            }
        }
        None
    }

    /// `Option<int>.None` or `Option<int>.Some(3)`, with the cursor at `<`
    /// after `kind`, written at `span`: a variant of a kind given its types.
    fn parse_kind_types_use(&mut self, kind: String, span: Span) -> Result<Expr> {
        let types = self.parse_type_args()?;
        self.expect(Token::Dot)?;
        let tag = self.expect_ident()?;
        if self.at_type_args() {
            return Err(Error::at(self.peek_span(), format!("'{}' already names the types of '{}', and a variant takes none of its own", kind, tag)));
        }
        let args = if self.check(Token::LParen) { self.parse_arg_list()? } else { Vec::new() };
        Ok(Expr::new(ExprKind::Construct { kind, tag, args, types }, span))
    }

    /// After the `<` of the types a call gives: the types and `>`.
    fn parse_type_args(&mut self) -> Result<Vec<TypeArg>> {
        if self.check(Token::NotSimilar) {
            return Err(Error::at(self.peek_span(), "'< … >' names the types a use gives, as in '<int>'; leave it out to let the values say which they are"));
        }
        self.advance(); // <
        let mut types = Vec::new();
        loop {
            let at = self.peek_span();
            self.enter()?;
            let ty = self.parse_type();
            self.leave(1);
            types.push((ty?, at));
            if !self.match_token(Token::Comma) {
                break;
            }
        }
        if !self.match_token(Token::Gt) {
            return Err(self.unexpected("',' and another type, or '>' to close the types"));
        }
        Ok(types)
    }

    /// `value.str(r)` or `value.str()`, with the cursor at `(` and `expr` the
    /// field `str` of the value: the method call `str`. A region written after
    /// it, `at r` or `@r`, is read as it is after any call (see
    /// `place_region`), and with none the checker gives it the block's default.
    fn parse_str(&mut self, expr: Expr) -> Result<Expr> {
        let dotted = dotted_name(&expr).map(|(whole, _)| whole);
        let span = expr.span;
        let ExprKind::Field(value, name) = expr.kind else { return Err(Error::internal_at(span, "a 'str' without its value")) };
        let args = self.parse_arg_list()?;
        Ok(Expr::new(ExprKind::MethodCall { value, name, args, dotted }, span))
    }

    /// `text(value)`, at `span`, with the cursor at `(`: the method call `str` on
    /// the value, whose region is written after it as for any call
    /// (`text(n) at r`, `text(n)@r`; see `place_region`).
    fn parse_text_cast(&mut self, span: Span) -> Result<Expr> {
        let args = self.parse_arg_list()?;
        let [value] = <[Expr; 1]>::try_from(args).map_err(|args| {
            let got = if args.len() == 1 { "1 value".to_string() } else { format!("{} values", args.len()) };
            Error::at(span, format!("'text(…)' turns one value into text, as in 'text(n) at r', got {}", got))
        })?;
        Ok(Expr::new(ExprKind::MethodCall { value: Box::new(value), name: STR.to_string(), args: Vec::new(), dotted: None }, span))
    }

    /// `value.copy()`, with the cursor at `(` and `expr` the field `copy` of the
    /// value, and the region after it: `at r`, `@r`, or none.
    fn parse_copy(&mut self, expr: Expr) -> Result<Expr> {
        let ExprKind::Field(value, _) = expr.kind else { return Err(Error::internal_at(expr.span, "a copy without its value")) };
        let args = self.parse_arg_list()?;
        if let Some(first) = args.first() {
            return Err(Error::at(first.span, "'copy' takes no arguments; name the region to copy into after it, as in 'x.copy() at r' or 'x.copy()@r'"));
        }
        if self.check(Token::In) {
            return Err(Error::at(self.peek_span(), "a copy names its region with 'at', as in 'x.copy() at r' (or 'x.copy()@r'); 'in' counts through a range"));
        }
        // The region comes after, `at r` or `@r`, as for any call (see
        // `place_region`); with none, the place the copy goes names it.
        Ok(Expr::new(ExprKind::Copy { value, region: None, region_span: expr.span }, expr.span))
    }

    /// Whether the `{` at the cursor, after a name, opens a shape literal
    /// (`Point { x: 1 }` or `Marker {}`) rather than a plan B (`x { 0 }`).
    fn at_shape_literal(&self) -> bool {
        let after = |n: usize| self.tokens.get(self.pos + n).unwrap_or(&Token::Eof);
        matches!(after(1), Token::RBrace) || (matches!(after(1), Token::Ident(_)) && matches!(after(2), Token::Colon))
    }

    /// `value { … }`, with the cursor at `{`: a plan B, whose block may start
    /// with `fail e` to name the error.
    fn parse_plan_b(&mut self, value: Expr) -> Result<Expr> {
        let opened = self.peek_span();
        self.advance(); // {
        let same_line = !self.check(Token::Newline);
        self.skip_newlines();
        let next = self.tokens.get(self.pos + 1).unwrap_or(&Token::Eof);
        // `Point {` with its fields on the next line: a line never starts with `name:`.
        if !same_line && matches!(self.peek(), Token::Ident(_)) && matches!(next, Token::Colon) {
            let example = dotted_name(&value).map_or_else(|| "Point { ".to_string(), |(name, _)| format!("{} {{ ", name));
            return Err(Error::at(
                self.peek_span(),
                format!("a shape literal is written on one line, starting after its '{{', as in '{}x: 1 }}'", example),
            ));
        }
        // `{ failure e` names the failure this plan B handles. `fail` and a name
        // here is the old spelling of that, never a value: a fallback replaces
        // the value, so a failure is sent (`<- fail e`) rather than given.
        if self.check(Token::Fail) && matches!(next, Token::Ident(_) | Token::Underscore) {
            return Err(Error::at(self.peek_span(), PLAN_B_NAMES));
        }
        let error = if self.check(Token::Failure) && matches!(next, Token::Ident(_) | Token::Underscore) {
            self.advance(); // failure
            Some(self.parse_bound_name()?)
        } else {
            None
        };
        let body = self.parse_block_until(&[Token::RBrace, Token::Fi, Token::Else, Token::Case])?;
        if !self.match_token(Token::RBrace) {
            return Err(Error::at(
                self.peek_span(),
                format!(
                    "expected '}}' to close the plan B at {}:{}, got {}",
                    opened.line,
                    opened.col,
                    self.peek().describe()
                ),
            ));
        }
        Ok(Expr::new(ExprKind::PlanB { value: Box::new(value), error, body }, opened))
    }

    /// `new T(value) at r` or `new@r T(value)`; `new [T] at r` for an empty
    /// value, as `new [T]() at r` writes it.
    fn parse_new(&mut self) -> Result<Expr> {
        // A `new` holds a type and a value, and takes more stack than most
        // expressions, so it counts as a level of its own.
        self.enter()?;
        let result = self.parse_new_inner();
        self.leave(1);
        result
    }

    /// Whether the `(` at the cursor opens a tuple type followed by the value
    /// `new` copies, `(int, int)((1, 2))`, rather than arguments alone.
    fn tuple_type_follows(&self) -> bool {
        let mut depth = 0usize;
        for pos in self.pos.. {
            match self.tokens.get(pos) {
                Some(Token::LParen) => depth += 1,
                Some(Token::RParen) => {
                    depth -= 1;
                    if depth == 0 {
                        return self.tokens.get(pos + 1) == Some(&Token::LParen);
                    }
                }
                Some(Token::Newline | Token::Eof) | None => return false,
                Some(_) => {}
            }
        }
        false
    }

    fn parse_new_inner(&mut self) -> Result<Expr> {
        let span = self.peek_span();
        self.advance(); // new
        // `new(…)` alone: a shared method's call written without its shape.
        // (`new (int, int)((1, 2))` names a tuple type, then gives its value.)
        if self.check(Token::LParen) && !self.tuple_type_follows() {
            return Err(Error::at(
                span,
                "'new' makes a pointer to a value in a region, as in 'new T(v) at r'; a shared method named 'new' is called on its shape or kind, as in 'X.new(…)'",
            ));
        }
        let before = if self.match_token(Token::AtSign) {
            let region_span = self.peek_span();
            let region = self.expect_ident()?;
            if self.check(Token::LParen) {
                return Err(Error::at(region_span, format!("after 'new@', write the region and then the type, as in 'new@r {}(…)'", region)));
            }
            Some((region, region_span))
        } else {
            None
        };
        let ty_span = self.peek_span();
        // `new [:float](…)`: a map whose keys are text, written as names.
        let named_keys = self.check(Token::LBracket) && self.tokens.get(self.pos + 1) == Some(&Token::Colon);
        let ty = self.parse_type()?;
        // `new [T]`, `new [K: V]` or `new Point`, with no `( )`: the empty
        // value `()` writes, which is all `new` can make without one.
        let bare = !self.check(Token::LParen);
        if bare && self.check(Token::LBrace) {
            let example = if matches!(ty, Type::Named(..) | Type::Applied(..)) { format!("{0}({0} {{ … }})", ty) } else { format!("{}(…)", ty) };
            return Err(Error::at(self.peek_span(), format!("'new' takes its value in '( )', as in 'new {}'; a 'new' never fails, so no plan B follows it", example)));
        }
        if !bare {
            self.advance(); // (
        }
        let empty = bare || self.check(Token::RParen);
        let empty_at = if bare { ty_span } else { self.peek_span() };
        // `new [T]() at r` makes an empty slice; its value is an array of no
        // values, which no literal can write. `new [K: V]() at r` makes an
        // empty map, of no pairs. `new Point() at r` makes a shape of the
        // values its fields fall back on, which the checker sees all have one
        // (see `Checker::check_new`).
        let value = if empty && matches!(ty, Type::Slice(..)) {
            Expr::new(ExprKind::Array(Vec::new()), empty_at)
        } else if empty && matches!(ty, Type::Map(..)) {
            Expr::new(ExprKind::Pairs(Vec::new()), empty_at)
        } else if empty && let Some((name, types)) = match &ty {
            Type::Named(name, _) => Some((name.clone(), Vec::new())),
            Type::Applied(applied) => Some((applied.name.clone(), applied.types.iter().map(|given| (given.clone(), ty_span)).collect())),
            _ => None,
        } {
            Expr::new(ExprKind::StructInit { name, types, fields: Vec::new() }, ty_span)
        } else if named_keys && self.check(Token::LBracket) {
            let opened = self.peek_span();
            self.advance(); // [
            let key = self.parse_named_key()?;
            if !self.match_token(Token::Colon) {
                return Err(self.unexpected("':' and the value that goes with the key"));
            }
            let value = self.parse_expr()?;
            let pairs = self.parse_pairs_after(vec![(key, value)], opened, true)?;
            Expr::new(ExprKind::Pairs(pairs), opened)
        } else if empty {
            return Err(Error::at(
                empty_at,
                format!(
                    "'new' needs the value to put in the region, as in 'new {}(…) at r'; only a slice, a map, or a shape whose fields all fall back on values can be made empty, as in 'new [int] at r' or 'new [text: int] at r'",
                    ty
                ),
            ));
        } else {
            self.parse_expr()?
        };
        if !bare {
            self.expect(Token::RParen)?;
        }
        if self.check(Token::In) {
            return Err(Error::at(self.peek_span(), "a 'new' names its region with 'at', as in 'new T(v) at r' (or 'new@r T(v)'); 'in' counts through a range"));
        }
        let after = if self.check(Token::At) {
            let at_span = self.peek_span();
            self.advance();
            if before.is_some() {
                return Err(Error::at(at_span, "the region is already named after 'new@'; write one of 'new T(v) at r' or 'new@r T(v)'"));
            }
            let region_span = self.peek_span();
            let region = self.expect_ident()?;
            Some((region, region_span))
        } else {
            None
        };
        // Without a region, the block's default is the region, which only the
        // checker knows; it reports the ones that have none (see `check_new`).
        let (region, region_span) = match before.or(after) {
            Some((region, at)) => (Some(region), at),
            None => (None, self.peek_span()),
        };
        Ok(Expr::new(ExprKind::New { ty: Box::new(ty), ty_span, value: Box::new(value), region, region_span }, span))
    }

    /// `Point { x: 1 }`, or `Box<int> { value: none }` with `types` the ones the
    /// literal names, with the cursor at `{`.
    fn parse_struct_init(&mut self, name: String, types: Vec<TypeArg>) -> Result<ExprKind> {
        let shape_name = name.clone();
        self.advance(); // {
        let mut fields = Vec::new();
        if !self.check(Token::RBrace) {
            loop {
                let span = self.peek_span();
                let name = self.expect_ident()?;
                self.expect(Token::Colon)?;
                let value = self.parse_expr()?;
                fields.push(FieldInit { name, span, value, filled: false });
                if !self.match_token(Token::Comma) {
                    break;
                }
                if self.check(Token::Newline) {
                    return Err(Error::at(
                        self.peek_span(),
                        format!("a shape literal is written on one line, as in '{} {{ x: 1, y: 2 }}'", shape_name),
                    ));
                }
            }
        }
        self.expect(Token::RBrace)?;
        Ok(ExprKind::StructInit { name, types, fields })
    }

    fn parse_arg_list(&mut self) -> Result<Vec<Expr>> {
        self.expect(Token::LParen)?;
        let mut args = Vec::new();
        if !self.check(Token::RParen) {
            loop {
                let arg = self.parse_expr()?;
                // `f(args...)` or `f(r, args..., s)`: a group's values, once.
                if self.check(Token::Ellipsis) {
                    let at = self.peek_span();
                    self.advance(); // ...
                    if args.iter().any(|arg: &Expr| matches!(arg.kind, ExprKind::Spread(_))) {
                        return Err(Error::at(at, "a call gives one group's values, as in 'next(r, args...)'; give the values of another one by one"));
                    }
                    args.push(Expr::new(ExprKind::Spread(Box::new(arg)), at));
                } else {
                    args.push(arg);
                }
                if !self.match_token(Token::Comma) {
                    break;
                }
            }
        }
        self.expect(Token::RParen)?;
        Ok(args)
    }

    /// The name after a dot: a name, `type`, the one reserved word read there
    /// (`x.type`), `new` (a shared method), or `at` before a `(`.
    fn expect_member(&mut self) -> Result<String> {
        if self.match_token(Token::Type) {
            return Ok(TYPE_WORD.to_string());
        }
        // `X.new(…)`: the one keyword a member may be named, a shared method,
        // which `X.new` names as a value.
        if self.check(Token::New) {
            self.advance();
            return Ok(NEW_WORD.to_string());
        }
        // `t.at(i)`: a keyword too, read as the method text has before its `(`.
        if self.check(Token::At) && self.tokens.get(self.pos + 1) == Some(&Token::LParen) {
            self.advance();
            return Ok(AT.to_string());
        }
        self.expect_ident()
    }

    fn expect_ident(&mut self) -> Result<String> {
        match self.peek() {
            Token::Ident(s) => { let v = s.clone(); self.advance(); Ok(v) }
            _ => Err(self.name_expected()),
        }
    }

    /// The error for a missing name at the cursor, explaining reserved words.
    fn name_expected(&self) -> Error {
        match KEYWORDS.iter().find(|(_, keyword)| keyword == self.peek()) {
            Some((word, _)) => Error::at(
                self.peek_span(),
                format!(
                    "expected a name, got '{}', which is a reserved word{}",
                    word,
                    if *word == TYPE_WORD { "; every value has '.type', its type as code writes it, so nothing else takes the name" } else { "" }
                ),
            ),
            None => self.unexpected("a name"),
        }
    }
}

/// The error for the first `bind` or `hold` in `tokens` (each with where it
/// starts and ends), or in a value in their text: words reserved with no use
/// yet, which are an error wherever they are written. Written where a
/// variable's word goes (before a name, a `&`, an `@`, or a `(` apart from it),
/// the error names the word a program writes today.
fn reserved_word(tokens: &[(&Token, Span, Span)]) -> Option<Error> {
    for (i, (token, start, end)) in tokens.iter().enumerate() {
        if let Token::Interpolated(pieces) = token {
            for piece in pieces {
                if let Piece::Value { tokens: inner, .. } = piece {
                    let inner: Vec<(&Token, Span, Span)> = inner
                        .iter()
                        .filter_map(|t| match &t.kind {
                            LexKind::Token(token) => Some((token, t.start, t.end)),
                            _ => None,
                        })
                        .collect();
                    if let Some(error) = reserved_word(&inner) {
                        return Some(error);
                    }
                }
            }
        }
        let (word, today) = match token {
            Token::Hold => ("hold", "a variable that changes is written 'flex'"),
            _ => continue,
        };
        let before = i.checked_sub(1).and_then(|at| tokens.get(at)).map(|(token, _, _)| *token);
        // Where a line or a pattern starts, `(` may begin the names a variable's
        // word takes apart, as in `fixed (a, b) = t` or `is flex(x, y)`.
        let starts_line = matches!(before, None | Some(Token::Newline));
        let starts = starts_line || matches!(before, Some(Token::Is | Token::Case | Token::Each | Token::Loop));
        let as_word = match tokens.get(i + 1) {
            Some((Token::Ident(_) | Token::Amp, _, _)) => true,
            // `[flex @r]`, a lambda's copies, and not a type such as `text@r`.
            Some((Token::AtSign, _, _)) => matches!(before, Some(Token::LBracket)),
            // Written against the word at a line's start, it is a call, as
            // `hold(1)` is, unless the names are assigned to.
            Some((Token::LParen, next, _)) => starts && (next != end || !starts_line || assigned_after(tokens, i + 1)),
            _ => false,
        };
        return Some(if as_word {
            Error::at(*start, format!("'{}' is a reserved word with no use yet; {}", word, today))
        } else {
            Error::at(*start, format!("'{}' is a reserved word, so it can't be a name", word))
        });
    }
    None
}

/// Whether the `(` at `open` in `tokens` closes with `)` and `=` after it.
fn assigned_after(tokens: &[(&Token, Span, Span)], open: usize) -> bool {
    let mut depth = 0;
    for (at, (token, _, _)) in tokens.iter().enumerate().skip(open) {
        match token {
            Token::LParen => depth += 1,
            Token::RParen => {
                depth -= 1;
                if depth == 0 {
                    return matches!(tokens.get(at + 1), Some((Token::Assign, _, _)));
                }
            }
            Token::Newline => return false,
            _ => {}
        }
    }
    false
}

/// The words a generic type can be asked to allow, for a message, from what
/// most types allow to what fewest do.
/// The error for a bare `called()` read as the first line of the default body
/// of pattern method `method`, after which the pattern didn't read on.
fn two_headers(called: &str, method: &str) -> String {
    format!(
        "'{0}()' is read as the first line of the body of '{1}', since a line of one header followed by a statement starts that method's body; to ask for both methods, write them on one line, as in '{1}(), {0}()'",
        called, method
    )
}

pub fn needs_list() -> String {
    let words: Vec<String> = [Need::Frozen, Need::Same, Need::Similar, Need::Shown, Need::Ordered].iter().map(|need| format!("'{}'", need.word())).collect();
    words.join(", ")
}

/// Why `narrower` already means `wider`, for the message that says to write one
/// word alone.
fn why_means(narrower: Need, wider: Need) -> String {
    let value = match narrower {
        Need::Ordered => "a value with an order is a number or text, which",
        Need::Shown => "a value that can be shown",
        Need::Same => "a value that can be compared",
        Need::Similar => "a value compared item by item",
        Need::Frozen => "a value that never changes",
    };
    let also = match wider {
        Need::Ordered => "has an order",
        Need::Shown => "can be shown",
        Need::Same => "can be compared",
        Need::Similar => "is compared item by item",
        Need::Frozen => "never changes",
    };
    format!("{} {}", value, also)
}

/// Whether a region written on `ty` belongs to what it holds, because `ty`
/// keeps none of its own: a tuple or an array, or a `maybe` or `T ~ E` of one.
/// (A pointer, a slice, and a shape or kind keep the region they are given, and
/// the checker reports what such a value holds in another region.)
fn keeps_no_region(ty: &Type) -> bool {
    match ty {
        Type::Tuple(_) | Type::Array(..) => true,
        Type::Maybe(inner) | Type::Fail(inner) => keeps_no_region(inner),
        Type::Or(value, error) => keeps_no_region(value) || keeps_no_region(error),
        _ => false,
    }
}

/// Whether `ty` is `region`, or holds it (as `maybe region` would).
fn mentions_region(ty: &Type) -> bool {
    match ty {
        Type::Region => true,
        Type::Ptr(inner, _) | Type::Maybe(inner) | Type::Array(inner, _) | Type::Slice(inner, _) => mentions_region(inner),
        Type::Map(key, value, _) => mentions_region(key) || mentions_region(value),
        Type::Tuple(items) => items.iter().any(mentions_region),
        _ => false,
    }
}

/// `a.b.c` for a chain of fields on a variable, with where `a` is written.
/// The parameter that holds the value a method of `owner` is called on, at the
/// position of the method's name. The value names the region it lives in
/// (`@this`), which the checker drops where the shape or kind lives in none; a
/// shape that takes types is named with them, as its own fields name them.
pub fn receiver(owner: &str, types: &[TypeParam], span: Span) -> Field {
    let region = Some(THIS_WORD.to_string());
    let ty = if types.is_empty() {
        Type::Named(owner.to_string(), region)
    } else {
        let given = types.iter().map(|param| Type::Named(param.name.clone(), None)).collect();
        Type::Applied(Box::new(Applied { name: owner.to_string(), types: given, region }))
    };
    Field { name: THIS.to_string(), ty, span, ty_span: span, private: false, read: false, default_region: false, passing: Passing::Copy, default: None }
}

/// The message for `new` named as anything but a shared method of `owner`.
fn new_only_shared(owner: &str) -> String {
    format!("'new' names only a shared method, one called on '{0}' itself, as in 'shared new(…) -> {0}', then '{0}.new(…)'", owner)
}

/// The message for `alters` on a shared method.
const SHARED_NEVER_ALTERS: &str = "a shared method is called on its shape or kind, with no value it could change, so it is never 'alters'";

/// A startup decorator as written: its name, its values, where it is, and
/// whether a failure of it passes (`$d?`).
type Decorator = (String, Vec<Expr>, Span, bool);

/// Gives `task` the decorators written above it: each startup decorator a call
/// taking the task first, then its values, and the wrappers as they are.
fn decorate(task: &mut TaskDef, decorators: Vec<Decorator>, wrappers: Vec<Expr>) {
    task.wrappers = wrappers;
    task.passes = decorators.iter().map(|(_, _, _, passes)| *passes).collect();
    task.decorators = decorators
        .into_iter()
        .map(|(name, args, span, _)| {
            let mut given = vec![Expr::new(ExprKind::Var(task.name.clone()), span)];
            given.extend(args);
            Expr::new(ExprKind::Call { name, types: Vec::new(), args: given }, span)
        })
        .collect();
}

/// What a shape or a kind declares above a method, whose names the method's
/// can't take (see `Parser::parse_method`).
#[derive(Clone, Copy)]
struct Above<'a> {
    fields: &'a [Field],
    shared: &'a [SharedField],
    methods: &'a [TaskDef],
}

/// Refuses a field or shared field named like one written above it in the
/// same shape: both are named after a dot, so they can't share a name.
fn reject_same_name(field: &Field, fields: &[Field], shared: &[SharedField]) -> Result<()> {
    let before = fields.iter().chain(shared.iter().map(|member| &member.field)).find(|before| before.name == field.name);
    match before {
        Some(before) => Err(Error::at(field.span, format!("field '{}' is already defined at {}:{}", field.name, before.span.line, before.span.col))),
        None => Ok(()),
    }
}

/// Refuses a member named `this`, which is the whole value inside a method, so
/// nothing written after a dot could reach it.
fn reject_member_named_this(name: &str, span: Span, what: &str) -> Result<()> {
    if name != THIS_WORD {
        return Ok(());
    }
    Err(Error::at(
        span,
        format!("'{0}' is the value a method is called on, written '{1}', so {2} can't take the name", THIS_WORD, THIS, what),
    ))
}

/// The types a declaration takes, as they are written: `<T, U>`.
fn type_param_list(types: &[TypeParam]) -> String {
    let names: Vec<&str> = types.iter().map(|param| param.name.as_str()).collect();
    format!("'<{}>'", names.join(", "))
}

fn dotted_name(expr: &Expr) -> Option<(String, Span)> {
    match &expr.kind {
        // The value a method is called on is no part of a name: `.this` and
        // `.x` are its own, and `.x.f(…)` calls a method on the field.
        ExprKind::Var(name) if name == THIS => None,
        ExprKind::Var(name) => Some((name.clone(), expr.span)),
        ExprKind::Field(base, field) => dotted_name(base).map(|(name, span)| (format!("{}{}{}", name, MODULE_SEPARATOR, field), span)),
        _ => None,
    }
}

/// Whether `expr` is text holding values whose region is still to come (see
/// `Parser::parse_interpolated`).
pub(crate) fn pending_region(expr: &Expr) -> bool {
    matches!(&expr.kind, ExprKind::Join { region, .. } if region.is_empty())
}

/// The `pieces` of text holding values, joined into `region`, added to
/// `parts`: its text as it is, and each value turned into text in the region.
/// A value that is itself text holding values gives its own pieces.
pub(crate) fn interpolated_parts(pieces: Vec<Expr>, region: &str, region_span: Span, parts: &mut Vec<Expr>) {
    for piece in pieces {
        if pending_region(&piece) {
            if let ExprKind::Join { parts: inner, .. } = piece.kind {
                interpolated_parts(inner, region, region_span, parts);
            }
            continue;
        }
        if matches!(piece.kind, ExprKind::TextLit(_)) {
            parts.push(piece);
            continue;
        }
        let span = piece.span;
        let into = Expr::new(ExprKind::Var(region.to_string()), region_span);
        parts.push(Expr::new(ExprKind::MethodCall { value: Box::new(piece), name: STR.to_string(), args: vec![into], dotted: None }, span));
    }
}

/// The operator a step statement writes (see `ast::StmtKind::Step`), with how
/// it is written.
/// Whether a `(` right after `expr` calls the task its value holds: after an
/// item, a call, a pointer's value, or a tuple's item (`t.0(x)`). A name's own
/// call, and a method's, are read with the name.
fn calls_value(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Index(..) | ExprKind::Call { .. } | ExprKind::Invoke { .. } | ExprKind::Deref(_) | ExprKind::MethodCall { .. } => true,
        ExprKind::Field(_, item) => item.bytes().all(|b| b.is_ascii_digit()),
        _ => false,
    }
}

/// The operator a step is written with (`++`, `+=`, …) and how, or none.
pub fn step_operator(token: &Token) -> Option<(BinaryOp, &'static str)> {
    Some(match token {
        Token::Increment => (BinaryOp::Add, "++"),
        Token::Decrement => (BinaryOp::Sub, "--"),
        Token::PlusAssign => (BinaryOp::Add, "+="),
        Token::MinusAssign => (BinaryOp::Sub, "-="),
        Token::StarAssign => (BinaryOp::Mul, "*="),
        Token::SlashAssign => (BinaryOp::Div, "/="),
        Token::PercentAssign => (BinaryOp::Mod, "%="),
        Token::PowerAssign => (BinaryOp::Pow, "**="),
        _ => return None,
    })
}

/// The tokens a loop's line is read from, by position, as `loop_items` and
/// `types_close` read them: the parser's, or Studio's, which hold comments
/// too (no token at their positions).
pub trait LineTokens {
    fn token_at(&self, p: usize) -> Option<&Token>;
    /// Whether the `>` at `p` touches what follows it.
    fn gt_touches_next(&self, p: usize) -> bool;
}

impl LineTokens for Parser {
    fn token_at(&self, p: usize) -> Option<&Token> {
        self.tokens.get(p)
    }

    fn gt_touches_next(&self, p: usize) -> bool {
        match (self.spans.get(p), self.spans.get(p + 1)) {
            (Some(gt), Some(next)) => gt.line == next.line && gt.col + 1 == next.col,
            _ => false,
        }
    }
}

/// How the token at `p` nests what follows it: brackets, and the words a
/// `fi` closes, which `blocks` counts, the `when` of an `else when` aside, as
/// one `fi` closes the chain. A lambda opens a block at its `(` as well as
/// the parentheses, so that its `fi` closes its own. Whether it nests.
fn nests(tokens: &impl LineTokens, p: usize, depth: &mut usize, blocks: &mut usize) -> bool {
    match tokens.token_at(p) {
        Some(Token::LParen) if lambda_at(tokens, p) => {
            *depth += 2;
            *blocks += 1;
        }
        Some(Token::LParen | Token::LBracket | Token::LBrace) => *depth += 1,
        Some(Token::When) if p > 0 && tokens.token_at(p - 1) == Some(&Token::Else) => {}
        Some(Token::When | Token::Match | Token::Block | Token::Loop | Token::Each | Token::Endless) => {
            *depth += 1;
            *blocks += 1;
        }
        Some(Token::RParen | Token::RBracket | Token::RBrace) => *depth = depth.saturating_sub(1),
        Some(Token::Fi) => {
            if *blocks > 0 {
                *blocks -= 1;
                *depth = depth.saturating_sub(1);
            }
        }
        _ => return false,
    }
    true
}

/// Whether the `(` at `p` starts a lambda, as the parser tells one (see
/// `Parser::at_lambda`): it takes nothing or starts with a parameter, its
/// parentheses close on their line, and `->` follows them. One in a type,
/// after `:` or `->`, is a task's type.
fn lambda_at(tokens: &impl LineTokens, p: usize) -> bool {
    if p > 0 && matches!(tokens.token_at(p - 1), Some(Token::Colon | Token::Arrow)) {
        return false;
    }
    let next = tokens.token_at(p + 1);
    let takes = matches!(next, Some(Token::RParen | Token::Flex | Token::Fixed)) || (matches!(next, Some(Token::Ident(_))) && tokens.token_at(p + 2) == Some(&Token::Colon));
    if !takes {
        return false;
    }
    let mut depth = 0usize;
    let mut q = p;
    loop {
        match tokens.token_at(q) {
            Some(Token::LParen | Token::LBracket) => depth += 1,
            Some(Token::RParen | Token::RBracket) => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return tokens.token_at(q) == Some(&Token::RParen) && tokens.token_at(q + 1) == Some(&Token::Arrow);
                }
            }
            Some(Token::Newline | Token::Eof) => return false,
            _ => {}
        }
        q += 1;
        // Studio's tokens leave no token where a comment is; a line ends
        // long before this.
        if q > p + 10_000 {
            return false;
        }
    }
}

/// The `>` closing the types a use gives, when the `<` at `p` opens them, as
/// the parser reads them: a call follows the types a task is given, a
/// literal the types a shape is given, and a variant, its `.` touching the
/// `>`, the types a kind is given.
pub fn types_close(tokens: &impl LineTokens, p: usize) -> Option<usize> {
    if tokens.token_at(p) != Some(&Token::Lt) {
        return None;
    }
    let mut depth = 0usize;
    // Parentheses pair up inside the types, and no `>` inside them closes
    // anything: `(i < n), m > (k)` compares.
    let mut grouped = 0usize;
    for q in p + 1.. {
        match tokens.token_at(q) {
            Some(Token::Gt) if grouped > 0 && depth == 0 => return None,
            Some(Token::LParen) => grouped += 1,
            Some(Token::RParen) if grouped == 0 => return None,
            Some(Token::RParen) => grouped -= 1,
            Some(Token::Gt) if depth == 0 => {
                let next = tokens.token_at(q + 1);
                let opens = matches!(next, Some(Token::LParen | Token::LBrace))
                    || (tokens.gt_touches_next(q) && next == Some(&Token::Dot) && matches!(tokens.token_at(q + 2), Some(Token::Ident(_))));
                return opens.then_some(q);
            }
            Some(Token::Lt) => depth += 1,
            Some(Token::Gt) => depth -= 1,
            Some(token) if token.writes_type() => {}
            _ => return None,
        }
    }
    None
}

/// The items of a loop's start or step from `first` to the one before
/// `last`, parted by `,`s, each its first position and the one after its
/// last. A `,` parts two items only where the next one starts, so `a, b =
/// b, a` and `flex q, r = divmod(a, b)` are one item each: before an item's
/// `=`, a `,` follows a place it sets, and after it, one of the values for
/// those places, until each has one. No `,` inside brackets, a block, or the
/// types a use gives or a name is written with parts anything.
pub fn loop_items(tokens: &impl LineTokens, first: usize, last: usize) -> Vec<(usize, usize)> {
    let mut items = Vec::new();
    let mut start = first;
    let (mut depth, mut blocks) = (0usize, 0usize);
    // The `<…>`s open in a type written after `:`.
    let mut typed: Option<usize> = None;
    let mut places = 1usize;
    let mut values: Option<usize> = None;
    let mut p = first;
    while p < last {
        let token = tokens.token_at(p);
        if typed.is_some() && values.is_none() {
            match token {
                Some(Token::Lt) => typed = typed.map(|open| open + 1),
                Some(Token::Gt) => typed = typed.map(|open| open.saturating_sub(1)),
                Some(Token::Comma) if typed > Some(0) => {}
                _ => {}
            }
            if typed > Some(0) {
                p += 1;
                continue;
            }
        }
        match token {
            Some(Token::Lt) if depth == 0 && let Some(close) = types_close(tokens, p) => p = close,
            _ if nests(tokens, p, &mut depth, &mut blocks) => {}
            _ if depth > 0 => {}
            Some(Token::Colon) if values.is_none() => typed = Some(0),
            Some(Token::Assign) if values.is_none() => values = Some(1),
            Some(token) if values.is_none() && step_operator(token).is_some() => {
                values = Some(if matches!(token, Token::Increment | Token::Decrement) { places } else { 1 });
            }
            Some(Token::Comma) => {
                let parts = match values {
                    // `a, b = …`: another place, or the end of a statement with none.
                    None if sets_places(tokens, start, p) => {
                        places += 1;
                        false
                    }
                    None => true,
                    Some(given) => given >= places || starts_loop_item(tokens, p + 1, last),
                };
                if parts {
                    items.push((start, p));
                    start = p + 1;
                    places = 1;
                    values = None;
                    typed = None;
                } else if let Some(given) = values.as_mut() {
                    *given += 1;
                }
            }
            _ => {}
        }
        p += 1;
    }
    if start < last || !items.is_empty() {
        items.push((start, last));
    }
    items
}

/// Whether the tokens from `first` to the one before `last` are places an
/// assignment or a declaration sets, as `flex q` and `t.x` are, rather than
/// a statement of its own, as `advance(&it)` is.
fn sets_places(tokens: &impl LineTokens, first: usize, last: usize) -> bool {
    let mut depth = 0usize;
    first < last
        && (first..last).all(|p| match tokens.token_at(p) {
            Some(Token::LBracket) => {
                depth += 1;
                true
            }
            Some(Token::RBracket) => {
                depth = depth.saturating_sub(1);
                true
            }
            _ if depth > 0 => true,
            Some(Token::Flex | Token::Fixed) => p == first,
            Some(Token::Ident(_) | Token::Dot | Token::Int(_) | Token::Caret | Token::Underscore | Token::Comma) | None => true,
            _ => false,
        })
}

/// Whether a loop's item starts at `p`: `flex` or `fixed`, a name and the
/// type it is written with, or what sets a place, before the next `,` or
/// `last`.
fn starts_loop_item(tokens: &impl LineTokens, p: usize, last: usize) -> bool {
    if matches!(tokens.token_at(p), Some(Token::Flex | Token::Fixed)) {
        return true;
    }
    let (mut depth, mut blocks) = (0usize, 0usize);
    let mut q = p;
    while q < last {
        let token = tokens.token_at(q);
        match token {
            Some(Token::Lt) if depth == 0 && let Some(close) = types_close(tokens, q) => q = close,
            _ if nests(tokens, q, &mut depth, &mut blocks) => {}
            _ if depth > 0 => {}
            Some(Token::Comma) => return false,
            Some(Token::Assign | Token::Colon) => return true,
            Some(token) if step_operator(token).is_some() => return true,
            _ => {}
        }
        q += 1;
    }
    false
}
