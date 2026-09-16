//! The declaration scan: classifies every token for highlighting and resolves
//! names to their declarations, working from the compiler's error-tolerant
//! token stream rather than its parser, so it keeps working while code is half
//! typed.
//!
//! It runs in two passes. The first finds the top-level declarations (tasks,
//! shapes and their fields, kinds and their variants), which may be used
//! before they are declared. The second walks the tokens in order, tracking
//! blocks and the variables declared in them (parameters, `bind`/`hold`,
//! `each`, the names `case` and `is` patterns bind, the error a plan B names,
//! and regions), which are only visible after their declaration.

use std::collections::HashMap;
use std::ops::Range;

use serde::Serialize;
use vela::ast::{BUILTIN_TASKS, Span, Type};
use vela::lexer::{ESCAPES, LexKind, LexToken, Token, highlight};

/// What a highlighted piece of source is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TokenClass {
    Keyword,
    /// `int`, `float`, `bool`, `text`, `unit` where a type is written.
    TypeBuiltin,
    /// A shape or kind name.
    Type,
    Variant,
    Task,
    /// A builtin task such as `show`.
    BuiltinFn,
    Parameter,
    Variable,
    /// A region's name: `region r`, `r: region`, or `@r` in a type.
    Region,
    Field,
    Number,
    String,
    /// An escape such as `\n` inside a text literal.
    Escape,
    Comment,
    Operator,
    Punctuation,
    /// Text the compiler rejects.
    Invalid,
}

/// What a declared name is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SymbolKind {
    Task,
    Shape,
    Kind,
    Variant,
    /// A shape field or a variant's payload field.
    Field,
    Parameter,
    /// A `bind`, `hold`, `each`, `case`, `is`, or plan B (`fail e`) binding.
    Variable,
    /// A region: declared by `region r`, taken as a parameter `r: region`, or
    /// only named in a task header's types (`@r`).
    Region,
}

impl SymbolKind {
    fn class(self) -> TokenClass {
        match self {
            SymbolKind::Task => TokenClass::Task,
            SymbolKind::Shape | SymbolKind::Kind => TokenClass::Type,
            SymbolKind::Variant => TokenClass::Variant,
            SymbolKind::Field => TokenClass::Field,
            SymbolKind::Parameter => TokenClass::Parameter,
            SymbolKind::Variable => TokenClass::Variable,
            SymbolKind::Region => TokenClass::Region,
        }
    }
}

/// A declared name.
#[derive(Debug, Clone)]
pub(crate) struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    /// Bytes of the name where it is declared.
    pub name_range: Range<usize>,
    /// The compiler's position of the name.
    pub start: Span,
    /// For a variable or parameter, the bytes where it can be used: from the
    /// end of its declaring line to the end of its block. Everything else is
    /// visible everywhere.
    pub visible: Range<usize>,
    /// The keyword that declared a variable: `bind`, `hold`, `each`, `case`,
    /// `is`, or `fail` (the error a plan B names); for a region, `region` (a
    /// region statement) or `@` (a region a task header only names).
    pub intro: Option<&'static str>,
    /// Bytes of the whole declaration: from `task`/`shape`/`kind` to its
    /// `fi` for those, and just the name for everything else.
    pub range: Range<usize>,
    /// Source text describing it: a task's parameters and return type, a
    /// field's or parameter's type, a variant's payload.
    pub detail: String,
    /// The shape of a field, the kind of a variant, or the variant of a
    /// payload field.
    pub parent: Option<usize>,
}

/// A classified piece of source.
#[derive(Debug, Clone)]
pub(crate) struct Classified {
    pub range: Range<usize>,
    pub class: TokenClass,
    /// The declaration this token declares or refers to, if known.
    pub symbol: Option<usize>,
}

pub(crate) struct Scan {
    /// The compiler's tokens, including comments and invalid text.
    pub lexed: Vec<LexToken>,
    pub symbols: Vec<Symbol>,
    /// In source order, without overlaps.
    pub tokens: Vec<Classified>,
}

/// `field_accesses` maps the (line, column) of a field name in `a.b` to the
/// shape of `a`, when the checker knows it.
pub(crate) fn scan(source: &str, field_accesses: &HashMap<(usize, usize), String>) -> Scan {
    let lexed = highlight(source);
    let tokens = Tokens::new(source, &lexed);
    let (symbols, declared_at) = declarations(&tokens);
    let classified = Classifier::new(&tokens, symbols, declared_at, field_accesses).run();
    let (symbols, tokens) = classified;
    Scan { lexed, symbols, tokens }
}

/// The token stream with comments set aside, so that patterns such as
/// `task NAME` can look at neighboring tokens.
struct Tokens<'a> {
    source: &'a str,
    all: &'a [LexToken],
    /// Indices into `all` of every token except comments.
    significant: Vec<usize>,
}

impl<'a> Tokens<'a> {
    fn new(source: &'a str, all: &'a [LexToken]) -> Self {
        let significant = all
            .iter()
            .enumerate()
            .filter(|(_, t)| t.kind != LexKind::Comment)
            .map(|(i, _)| i)
            .collect();
        Tokens { source, all, significant }
    }

    fn len(&self) -> usize {
        self.significant.len()
    }

    /// The significant token at position `p`.
    fn get(&self, p: usize) -> Option<&'a LexToken> {
        self.significant.get(p).and_then(|&i| self.all.get(i))
    }

    fn token(&self, p: usize) -> Option<&'a Token> {
        match self.get(p).map(|t| &t.kind) {
            Some(LexKind::Token(token)) => Some(token),
            _ => None,
        }
    }

    fn before(&self, p: usize) -> Option<&'a Token> {
        p.checked_sub(1).and_then(|q| self.token(q))
    }

    fn range(&self, p: usize) -> Range<usize> {
        self.get(p).map(|t| t.range.clone()).unwrap_or(self.source.len()..self.source.len())
    }

    /// Whether the `{` at position `p` opens a block that continues on the next
    /// lines: it ends its line, or only `fail e` follows it there.
    fn opens_lines(&self, p: usize) -> bool {
        let names_error = self.token(p + 1) == Some(&Token::Fail) && matches!(self.token(p + 2), Some(Token::Ident(_) | Token::Underscore));
        let end = if names_error { p + 3 } else { p + 1 };
        matches!(self.token(end), Some(Token::Newline | Token::Eof) | None)
    }

    /// Whether position `p` starts a line.
    fn starts_line(&self, p: usize) -> bool {
        p == 0 || self.before(p) == Some(&Token::Newline)
    }

    /// The trimmed source text after token `p` up to the first token that
    /// `stop` accepts, a comment, or the end of the line.
    fn text_after(&self, p: usize, stop: fn(&Token) -> bool) -> String {
        let Some(&first) = self.significant.get(p) else { return String::new() };
        let from = self.range(p).end;
        let to = self
            .all
            .iter()
            .skip(first + 1)
            .find(|t| match &t.kind {
                LexKind::Token(token) => *token == Token::Newline || *token == Token::Eof || stop(token),
                LexKind::Comment => true,
                LexKind::Invalid(_) => false,
            })
            .map_or(self.source.len(), |t| t.range.start);
        self.source.get(from..to).unwrap_or_default().trim().to_string()
    }

    fn ident(&self, p: usize) -> Option<&'a str> {
        match self.token(p) {
            Some(Token::Ident(name)) => Some(name),
            _ => None,
        }
    }
}

fn never(_: &Token) -> bool {
    false
}

fn ends_field(token: &Token) -> bool {
    matches!(token, Token::Comma | Token::RParen)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Block {
    Task,
    Shape,
    Kind,
    /// `when`, `loop`, `each`, or `match`: any block inside a task.
    Inner,
}

/// First pass: top-level declarations. Returns the symbols and, for each
/// declaring token, its symbol.
fn declarations(tokens: &Tokens) -> (Vec<Symbol>, HashMap<usize, usize>) {
    let mut symbols: Vec<Symbol> = Vec::new();
    let mut declared_at = HashMap::new();
    let mut blocks: Vec<Block> = Vec::new();
    // The open task, shape, or kind, and the variant whose payload is open.
    let mut item: Option<usize> = None;
    let mut variant: Option<usize> = None;
    let mut parens = 0usize;
    // End of the last token that isn't a newline, where an unclosed item ends.
    let mut last_end = 0;

    let mut declare = |symbols: &mut Vec<Symbol>, p: usize, kind: SymbolKind, detail: String, parent: Option<usize>| {
        let name = tokens.ident(p).unwrap_or_default().to_string();
        let range = tokens.range(p);
        let start = tokens.get(p).map(|t| t.start).unwrap_or_default();
        let visible = 0..tokens.source.len();
        symbols.push(Symbol { name, kind, name_range: range.clone(), range, start, visible, intro: None, detail, parent });
        declared_at.insert(p, symbols.len() - 1);
        symbols.len() - 1
    };

    for p in 0..tokens.len() {
        let Some(token) = tokens.token(p) else {
            last_end = tokens.range(p).end;
            continue;
        };
        match token {
            // Items only appear at the top level, so a new one also closes
            // any item left open by half-typed code.
            Token::Task | Token::Shape | Token::Kind => {
                if let Some(open) = item.take() {
                    close(&mut symbols, open, last_end);
                }
                blocks.clear();
                let (block, kind) = match token {
                    Token::Task => (Block::Task, SymbolKind::Task),
                    Token::Shape => (Block::Shape, SymbolKind::Shape),
                    _ => (Block::Kind, SymbolKind::Kind),
                };
                blocks.push(block);
                if tokens.ident(p + 1).is_some() {
                    let detail = if block == Block::Task { tokens.text_after(p + 1, never) } else { String::new() };
                    let id = declare(&mut symbols, p + 1, kind, detail, None);
                    if let Some(symbol) = symbols.get_mut(id) {
                        symbol.range.start = tokens.range(p).start;
                    }
                    item = Some(id);
                }
            }
            Token::When | Token::Loop | Token::Each | Token::Match => blocks.push(Block::Inner),
            Token::Fi => {
                blocks.pop();
                if blocks.is_empty()
                    && let Some(open) = item.take()
                {
                    close(&mut symbols, open, tokens.range(p).end);
                }
            }
            Token::LParen => parens += 1,
            Token::RParen => parens = parens.saturating_sub(1),
            Token::Newline => parens = 0,
            Token::Ident(_) => match (blocks.as_slice(), item) {
                ([Block::Shape], Some(shape)) if tokens.starts_line(p) && tokens.token(p + 1) == Some(&Token::Colon) => {
                    let detail = tokens.text_after(p + 1, never);
                    declare(&mut symbols, p, SymbolKind::Field, detail, Some(shape));
                }
                ([Block::Kind], Some(kind)) if tokens.starts_line(p) => {
                    let detail = tokens.text_after(p, never);
                    variant = Some(declare(&mut symbols, p, SymbolKind::Variant, detail, Some(kind)));
                }
                ([Block::Kind], Some(_))
                    if parens == 1
                        && tokens.token(p + 1) == Some(&Token::Colon)
                        && matches!(tokens.before(p), Some(Token::LParen | Token::Comma)) =>
                {
                    let detail = tokens.text_after(p + 1, ends_field);
                    declare(&mut symbols, p, SymbolKind::Field, detail, variant);
                }
                _ => {}
            },
            _ => {}
        }
        if *token != Token::Newline && *token != Token::Eof {
            last_end = tokens.range(p).end;
        }
    }
    if let Some(open) = item {
        close(&mut symbols, open, last_end);
    }
    (symbols, declared_at)
}

fn close(symbols: &mut [Symbol], id: usize, end: usize) {
    if let Some(symbol) = symbols.get_mut(id) {
        symbol.range.end = end.max(symbol.name_range.end);
    }
}

/// A block open during the second pass, with the variables declared in it.
struct Scope {
    kind: ScopeKind,
    variables: Vec<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopeKind {
    Item,
    When,
    Other,
    Match,
    Case,
    /// A plan B's block, `{ … }`.
    PlanB,
}

/// An open `{`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Brace {
    /// A shape literal, with its shape's name if known.
    Literal(Option<String>),
    /// A plan B's block, which may span lines.
    PlanB,
}

/// Second pass: classifies every token.
struct Classifier<'a> {
    tokens: &'a Tokens<'a>,
    symbols: Vec<Symbol>,
    declared_at: HashMap<usize, usize>,
    field_accesses: &'a HashMap<(usize, usize), String>,
    /// Tasks, shapes, kinds, and variants by name (the first, if repeated).
    globals: HashMap<String, usize>,
    scopes: Vec<Scope>,
    out: Vec<Classified>,
}

impl<'a> Classifier<'a> {
    fn new(
        tokens: &'a Tokens<'a>,
        symbols: Vec<Symbol>,
        declared_at: HashMap<usize, usize>,
        field_accesses: &'a HashMap<(usize, usize), String>,
    ) -> Self {
        let mut globals = HashMap::new();
        for (id, symbol) in symbols.iter().enumerate() {
            if !matches!(symbol.kind, SymbolKind::Field) {
                globals.entry(symbol.name.clone()).or_insert(id);
            }
        }
        Classifier { tokens, symbols, declared_at, field_accesses, globals, scopes: Vec::new(), out: Vec::new() }
    }

    fn run(mut self) -> (Vec<Symbol>, Vec<Classified>) {
        let tokens = self.tokens;
        // Context carried from one token to the next.
        let mut in_type = false;
        // The keyword declaring the next name as a variable.
        let mut declaring: Option<&'static str> = None;
        let mut in_task_header = false;
        // In a `case` or `is` pattern, whose names are declared.
        let mut in_pattern = false;
        // Just after a plan B's `{`, where `fail e` names the error.
        let mut plan_b_head = false;
        // Just after a type's name, where `or` starts the error type.
        let mut after_type = false;
        // After `@` or a `new …(…)`'s `in`, where a region is named; whether a
        // type follows it (as after `new@r`).
        let mut region_next: Option<bool> = None;
        let mut braces: Vec<Brace> = Vec::new();

        let mut p = 0;
        for lexed in tokens.all {
            let range = lexed.range.clone();
            let at = range.start;
            let token = match &lexed.kind {
                LexKind::Comment => {
                    self.push(range, TokenClass::Comment, None);
                    continue;
                }
                LexKind::Invalid(_) => {
                    if tokens.source.get(range.clone()).is_some_and(|text| text.starts_with('"')) {
                        self.string(range);
                    } else {
                        self.push(range, TokenClass::Invalid, None);
                    }
                    p += 1;
                    in_type = false;
                    declaring = None;
                    continue;
                }
                LexKind::Token(token) => token,
            };
            let was_type = std::mem::take(&mut in_type);
            let was_after_type = std::mem::take(&mut after_type);
            let declaring_now = declaring.take();
            let region_now = region_next.take();
            if !matches!(token, Token::Newline | Token::Fail) {
                plan_b_head = false;
            }

            match token {
                Token::Newline => {
                    in_task_header = false;
                    in_pattern = false;
                    // Shape literals are on one line, so an unclosed literal `{` ends
                    // with it; a plan B's block continues.
                    braces.retain(|brace| *brace == Brace::PlanB);
                }
                Token::Eof => {}
                Token::Int(_) | Token::Float(_) => self.push(range, TokenClass::Number, None),
                Token::Text(_) => self.string(range),
                Token::Ident(name) => {
                    if let Some(type_follows) = region_now {
                        let symbol = self.region(p, name, in_task_header);
                        self.push(range, TokenClass::Region, symbol);
                        in_type = type_follows;
                        after_type = !type_follows;
                    } else {
                        let (class, symbol) = self.identifier(p, name, was_type, declaring_now, in_task_header, in_pattern, &braces);
                        self.push(range, class, symbol);
                        after_type = was_type;
                    }
                }
                Token::At => {
                    self.push(range, TokenClass::Operator, None);
                    region_next = Some(was_type);
                }
                Token::Colon => {
                    self.push(range, TokenClass::Punctuation, None);
                    // A field's value in a shape literal is not a type.
                    in_type = !matches!(braces.last(), Some(Brace::Literal(_)));
                }
                Token::Arrow => {
                    self.push(range, TokenClass::Operator, None);
                    in_type = true;
                }
                Token::Caret => {
                    self.push(range, TokenClass::Operator, None);
                    in_type = was_type;
                }
                Token::LBrace => {
                    self.push(range, TokenClass::Punctuation, None);
                    let literal = matches!(tokens.token(p + 1), Some(Token::RBrace))
                        || (tokens.ident(p + 1).is_some() && tokens.token(p + 2) == Some(&Token::Colon));
                    if in_pattern {
                        // `case {value}`: the name inside is declared by the pattern.
                        braces.push(Brace::Literal(None));
                    } else if literal {
                        braces.push(Brace::Literal(p.checked_sub(1).and_then(|q| tokens.ident(q)).map(str::to_string)));
                    } else {
                        braces.push(Brace::PlanB);
                        self.open(ScopeKind::PlanB);
                        plan_b_head = true;
                    }
                }
                Token::RBrace => {
                    self.push(range, TokenClass::Punctuation, None);
                    if braces.pop() == Some(Brace::PlanB) && self.scopes.last().is_some_and(|s| s.kind == ScopeKind::PlanB) {
                        self.close_scope(at);
                    }
                }
                Token::LParen | Token::RParen | Token::LBracket | Token::RBracket | Token::Comma | Token::Dot => {
                    self.push(range, TokenClass::Punctuation, None);
                }
                Token::Plus | Token::Minus | Token::Star | Token::Slash | Token::Percent | Token::Eq | Token::Ne
                | Token::Lt | Token::Gt | Token::Le | Token::Ge | Token::Assign | Token::Range => {
                    self.push(range, TokenClass::Operator, None);
                }
                // A type after `maybe`, and the error type after `or` in a type.
                Token::Maybe => {
                    self.push(range, TokenClass::Keyword, None);
                    in_type = was_type;
                }
                Token::Or if was_after_type => {
                    self.push(range, TokenClass::Keyword, None);
                    in_type = true;
                }
                // Everything else is a keyword.
                keyword => {
                    self.push(range, TokenClass::Keyword, None);
                    match keyword {
                        Token::Task | Token::Shape | Token::Kind => {
                            while !self.scopes.is_empty() {
                                self.close_scope(at);
                            }
                            self.open(ScopeKind::Item);
                            in_task_header = *keyword == Token::Task;
                            braces.clear();
                        }
                        Token::Is => in_pattern = true,
                        Token::Fail if plan_b_head => declaring = Some("fail"),
                        Token::When => self.open(ScopeKind::When),
                        Token::Loop => self.open(ScopeKind::Other),
                        Token::Each => {
                            self.open(ScopeKind::Other);
                            declaring = Some("each");
                        }
                        Token::Match => self.open(ScopeKind::Match),
                        Token::Bind => declaring = Some("bind"),
                        Token::Hold => declaring = Some("hold"),
                        Token::Case => {
                            self.close_plan_b(at, &mut braces);
                            if self.scopes.last().is_some_and(|s| s.kind == ScopeKind::Case) {
                                self.close_scope(at);
                            }
                            self.open(ScopeKind::Case);
                            in_pattern = true;
                        }
                        Token::Fi => {
                            // A plan B still open here is missing its `}`.
                            self.close_plan_b(at, &mut braces);
                            if self.scopes.last().is_some_and(|s| s.kind == ScopeKind::Case) {
                                self.close_scope(at);
                            }
                            self.close_scope(at);
                        }
                        Token::Else => {
                            self.close_plan_b(at, &mut braces);
                            // The `else` branch can't see the `when` branch's variables.
                            if self.scopes.last().is_some_and(|s| s.kind == ScopeKind::When) {
                                self.close_scope(at);
                                self.open(ScopeKind::When);
                            }
                        }
                        Token::New => in_type = true,
                        // `new T(v) in r`; an `each`'s `in` follows its name.
                        Token::In if tokens.before(p) == Some(&Token::RParen) => region_next = Some(false),
                        // `region r` declares a region; `r: region` is a parameter's type.
                        Token::Region if !was_type => declaring = Some("region"),
                        _ => {}
                    }
                }
            }
            p += 1;
        }
        (self.symbols, self.out)
    }

    #[allow(clippy::too_many_arguments)]
    fn identifier(
        &mut self,
        p: usize,
        name: &str,
        in_type: bool,
        declaring: Option<&'static str>,
        in_task_header: bool,
        in_pattern: bool,
        braces: &[Brace],
    ) -> (TokenClass, Option<usize>) {
        let tokens = self.tokens;
        let before = tokens.before(p);
        let after = tokens.token(p + 1);

        if let Some(&id) = self.declared_at.get(&p) {
            let kind = self.symbols.get(id).map_or(SymbolKind::Variable, |s| s.kind);
            return (kind.class(), Some(id));
        }
        if in_type {
            if Type::builtin(name).is_some() {
                return (TokenClass::TypeBuiltin, None);
            }
            return (TokenClass::Type, self.global(name, &[SymbolKind::Shape, SymbolKind::Kind]));
        }
        if let Some(intro) = declaring {
            if intro == "region" {
                return (TokenClass::Region, Some(self.declare_local(p, SymbolKind::Region, Some(intro), String::new())));
            }
            let detail = if after == Some(&Token::Colon) { tokens.text_after(p + 1, |t| *t == Token::Assign) } else { String::new() };
            return (TokenClass::Variable, Some(self.declare_local(p, SymbolKind::Variable, Some(intro), detail)));
        }
        if in_task_header && after == Some(&Token::Colon) && matches!(before, Some(Token::LParen | Token::Comma)) {
            let detail = tokens.text_after(p + 1, ends_field);
            let kind = if detail == "region" { SymbolKind::Region } else { SymbolKind::Parameter };
            return (kind.class(), Some(self.declare_local(p, kind, None, detail)));
        }
        if in_pattern {
            if matches!(before, Some(Token::Case | Token::Is)) {
                return (TokenClass::Variant, self.global(name, &[SymbolKind::Variant]));
            }
            let intro = if self.scopes.last().is_some_and(|s| s.kind == ScopeKind::Case) { "case" } else { "is" };
            return (TokenClass::Variable, Some(self.declare_local(p, SymbolKind::Variable, Some(intro), String::new())));
        }
        if before == Some(&Token::Dot) {
            let start = tokens.get(p).map(|t| (t.start.line, t.start.col));
            let shape = start.and_then(|at| self.field_accesses.get(&at)).cloned();
            return (TokenClass::Field, self.field(shape.as_deref(), name));
        }
        if let Some(Brace::Literal(Some(shape))) = braces.last()
            && after == Some(&Token::Colon)
            && matches!(before, Some(Token::LBrace | Token::Comma))
        {
            let shape = shape.clone();
            return (TokenClass::Field, self.field(Some(&shape), name));
        }
        let literal_follows = after == Some(&Token::LBrace)
            && (tokens.token(p + 2) == Some(&Token::RBrace)
                || (tokens.ident(p + 2).is_some() && tokens.token(p + 3) == Some(&Token::Colon)));
        if literal_follows && self.local(name, tokens.range(p).start).is_none() {
            return (TokenClass::Type, self.global(name, &[SymbolKind::Shape, SymbolKind::Kind]));
        }
        // A call always names a task or a variant, even when a variable has the same name.
        if after == Some(&Token::LParen)
            && let Some(id) = self.global(name, &[SymbolKind::Task, SymbolKind::Variant])
        {
            let kind = self.symbols.get(id).map_or(SymbolKind::Task, |s| s.kind);
            return (kind.class(), Some(id));
        }
        if let Some(id) = self.local(name, tokens.range(p).start) {
            let kind = self.symbols.get(id).map_or(SymbolKind::Variable, |s| s.kind);
            return (kind.class(), Some(id));
        }
        if let Some(id) = self.global(name, &[SymbolKind::Variant, SymbolKind::Task, SymbolKind::Shape, SymbolKind::Kind]) {
            let kind = self.symbols.get(id).map_or(SymbolKind::Variable, |s| s.kind);
            return (kind.class(), Some(id));
        }
        if BUILTIN_TASKS.contains(&name) {
            return (TokenClass::BuiltinFn, None);
        }
        if after == Some(&Token::LParen) {
            return (TokenClass::Task, None);
        }
        (TokenClass::Variable, None)
    }

    /// The region named at `p` (after `@`, or `in` after `new`). In a task's
    /// header, a name no parameter declares is a region the task only names in
    /// its types, declared where it first appears.
    fn region(&mut self, p: usize, name: &str, in_task_header: bool) -> Option<usize> {
        if !in_task_header {
            return self.local(name, self.tokens.range(p).start);
        }
        let header = self.scopes.last().map(|scope| scope.variables.clone()).unwrap_or_default();
        let known = header.into_iter().find(|&id| self.symbols.get(id).is_some_and(|s| s.name == name && s.kind == SymbolKind::Region));
        if known.is_some() {
            return known;
        }
        // A `region` parameter later in the header: `task f(a: ^int@r, r: region)`.
        let tokens = self.tokens;
        let later = (p + 1..tokens.len())
            .take_while(|&q| !matches!(tokens.token(q), Some(Token::Newline | Token::Eof) | None))
            .find(|&q| {
                tokens.ident(q) == Some(name)
                    && tokens.token(q + 1) == Some(&Token::Colon)
                    && tokens.token(q + 2) == Some(&Token::Region)
                    && matches!(tokens.before(q), Some(Token::LParen | Token::Comma))
            });
        if let Some(q) = later {
            let id = self.declare_local(q, SymbolKind::Region, None, "region".to_string());
            self.declared_at.insert(q, id);
            return Some(id);
        }
        Some(self.declare_local(p, SymbolKind::Region, Some("@"), String::new()))
    }

    fn open(&mut self, kind: ScopeKind) {
        self.scopes.push(Scope { kind, variables: Vec::new() });
    }

    /// Closes the plan B scopes open at the top, at byte `at`, and forgets
    /// their braces: a block keyword there means they are missing their `}`.
    fn close_plan_b(&mut self, at: usize, braces: &mut Vec<Brace>) {
        while self.scopes.last().is_some_and(|s| s.kind == ScopeKind::PlanB) {
            self.close_scope(at);
            if let Some(open) = braces.iter().rposition(|brace| *brace == Brace::PlanB) {
                braces.truncate(open);
            }
        }
    }

    /// Closes the innermost scope at byte `at`, where its variables stop being
    /// visible.
    fn close_scope(&mut self, at: usize) {
        let Some(scope) = self.scopes.pop() else { return };
        for id in scope.variables {
            if let Some(symbol) = self.symbols.get_mut(id) {
                symbol.visible.end = at.max(symbol.visible.start);
            }
        }
    }

    fn declare_local(&mut self, p: usize, kind: SymbolKind, intro: Option<&'static str>, detail: String) -> usize {
        let tokens = self.tokens;
        let name = tokens.ident(p).unwrap_or_default().to_string();
        let range = tokens.range(p);
        let start = tokens.get(p).map(|t| t.start).unwrap_or_default();
        // Usable from the next line: not in its own initializer, range, or
        // header, including a plan B there that spans lines. Other braces
        // (shape literals, plan Bs written on one line) end with their line.
        let mut open_plan_bs = 0usize;
        let mut line_braces = 0usize;
        let line_end = (p..tokens.len())
            .find(|&q| match tokens.token(q) {
                Some(Token::LBrace) if tokens.opens_lines(q) => {
                    open_plan_bs += 1;
                    false
                }
                Some(Token::LBrace) => {
                    line_braces += 1;
                    false
                }
                Some(Token::RBrace) if line_braces > 0 => {
                    line_braces -= 1;
                    false
                }
                Some(Token::RBrace) => {
                    open_plan_bs = open_plan_bs.saturating_sub(1);
                    false
                }
                Some(Token::Newline) => {
                    line_braces = 0;
                    open_plan_bs == 0
                }
                Some(Token::Eof) | None => true,
                _ => false,
            })
            .map_or(tokens.source.len(), |q| tokens.range(q).end);
        let visible = line_end..tokens.source.len();
        self.symbols.push(Symbol { name, kind, name_range: range.clone(), range, start, visible, intro, detail, parent: None });
        let id = self.symbols.len() - 1;
        if let Some(scope) = self.scopes.last_mut() {
            scope.variables.push(id);
        }
        id
    }

    /// The variable or parameter called `name` visible at byte `at`, innermost
    /// first. (A variable isn't visible in its own initializer: `bind y = y + 1`.)
    fn local(&self, name: &str, at: usize) -> Option<usize> {
        self.scopes
            .iter()
            .rev()
            .flat_map(|scope| scope.variables.iter().rev())
            .copied()
            .find(|&id| self.symbols.get(id).is_some_and(|s| s.name == name && s.visible.start <= at))
    }

    /// The top-level declaration called `name`, if it is one of `kinds`.
    fn global(&self, name: &str, kinds: &[SymbolKind]) -> Option<usize> {
        self.globals
            .get(name)
            .copied()
            .filter(|&id| self.symbols.get(id).is_some_and(|s| kinds.contains(&s.kind)))
    }

    /// A shape's field called `name`. Without a known shape, a field name that
    /// only one shape declares still resolves.
    fn field(&self, shape: Option<&str>, name: &str) -> Option<usize> {
        let mut candidates = self.symbols.iter().enumerate().filter(|(_, s)| {
            s.kind == SymbolKind::Field
                && s.name == name
                && s.parent
                    .and_then(|parent| self.symbols.get(parent))
                    .is_some_and(|parent| parent.kind == SymbolKind::Shape && shape.is_none_or(|shape| parent.name == shape))
        });
        let first = candidates.next().map(|(id, _)| id);
        if shape.is_none() && candidates.next().is_some() {
            return None;
        }
        first
    }

    fn push(&mut self, range: Range<usize>, class: TokenClass, symbol: Option<usize>) {
        if range.start < range.end {
            self.out.push(Classified { range, class, symbol });
        }
    }

    /// A text literal, valid or not, split into plain text and escapes. An
    /// escape the compiler rejects is `Invalid`.
    fn string(&mut self, range: Range<usize>) {
        let text = self.tokens.source.get(range.clone()).unwrap_or_default();
        let mut piece = range.start;
        let mut chars = text.char_indices();
        while let Some((i, c)) = chars.next() {
            if c != '\\' {
                continue;
            }
            let start = range.start + i;
            let next = chars.next();
            let end = next.map_or(range.end, |(j, n)| range.start + j + n.len_utf8());
            let valid = next.is_some_and(|(_, n)| n != '\n' && ESCAPES.iter().any(|(escape, _)| *escape == n));
            self.push(piece..start, TokenClass::String, None);
            self.push(start..end, if valid { TokenClass::Escape } else { TokenClass::Invalid }, None);
            piece = end;
        }
        self.push(piece..range.end, TokenClass::String, None);
    }
}
