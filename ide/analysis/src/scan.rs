//! The declaration scan: classifies every token for highlighting and resolves
//! names to their declarations, working from the compiler's error-tolerant
//! token stream rather than its parser, so it keeps working while code is half
//! typed.
//!
//! It runs in two passes. The first finds the top-level declarations (tasks,
//! shapes and their fields, kinds and their variants, and the modules `use`
//! lines name), which may be used before they are declared. The declarations
//! of each module the file uses are added then too, named as the file names
//! them (`geometry.area`). The second walks the tokens in order, tracking
//! blocks and the variables declared in them (parameters, `bind`/`hold`,
//! `each`, the names `case` and `is` patterns bind, the error a plan B names,
//! and regions), which are only visible after their declaration.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::Path;

use serde::Serialize;
use vela::ast::{BUILTIN_TASKS, MODULE_SEPARATOR, Span, Type};
use vela::lexer::{ESCAPES, LexKind, LexToken, Token, highlight};

use crate::types::{Context, slashed};

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
    /// A module's name, and the folders of a `use` path.
    Module,
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
    /// A module the file uses, declared by its `use` line.
    Module,
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
            SymbolKind::Module => TokenClass::Module,
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
    /// The module it is declared in (an index into `Scan::modules`), for a
    /// declaration of another file; its ranges are in that file.
    pub module: Option<usize>,
    /// Marked `priv`: a task, shape, kind, or field of its own file only.
    pub private: bool,
}

/// A module the file uses.
#[derive(Debug, Clone)]
pub(crate) struct Module {
    /// Its path, as the editor names files; empty when the file has no path.
    pub path: String,
    /// Its text, when the editor gave it.
    pub source: Option<String>,
    /// Its `use` line's symbol.
    pub symbol: usize,
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
    /// The modules the file uses, in the order of its `use` lines.
    pub modules: Vec<Module>,
    /// The names `use m (a, b)` brings in, each with the declaration it names.
    pub imports: HashMap<String, usize>,
}

/// `field_accesses` maps the (line, column) of a field name in `a.b` to the
/// shape of `a`, when the checker knows it. `context` gives the file's path
/// and the other files it may use.
pub(crate) fn scan(source: &str, field_accesses: &HashMap<(usize, usize), String>, context: Context) -> Scan {
    let lexed = highlight(source);
    let tokens = Tokens::new(source, &lexed);
    let (mut symbols, declared_at, uses) = declarations(&tokens);
    let (modules, imports) = used_modules(&mut symbols, &uses, context);
    let classified = Classifier::new(&tokens, symbols, declared_at, field_accesses, imports.clone()).run();
    let (symbols, tokens) = classified;
    Scan { lexed, symbols, tokens, modules, imports }
}

/// A `use` line found by the first pass.
struct UseLine {
    /// The path as written: names and `..`.
    parts: Vec<String>,
    /// The module's symbol.
    symbol: usize,
    /// The names in parentheses.
    names: Vec<String>,
}

/// Adds the declarations of each module in `uses` to `symbols`, named as this
/// file names them (`geometry.area`, whose signature says `geometry.Point`),
/// and returns the modules and the names brought in by name.
fn used_modules(symbols: &mut Vec<Symbol>, uses: &[UseLine], context: Context) -> (Vec<Module>, HashMap<String, usize>) {
    let folder = Path::new(context.path).parent().unwrap_or(Path::new(""));
    let mut modules = Vec::new();
    let mut imports = HashMap::new();
    for line in uses {
        let index = modules.len();
        let path = slashed(&vela::modules::module_file(folder, &line.parts));
        let source = context.sources.get(&path).cloned();
        let name = line.parts.last().cloned().unwrap_or_default();
        if let Some(source) = &source {
            let lexed = highlight(source);
            let tokens = Tokens::new(source, &lexed);
            let (theirs, _, _) = declarations(&tokens);
            let own_types: HashSet<String> =
                theirs.iter().filter(|s| matches!(s.kind, SymbolKind::Shape | SymbolKind::Kind)).map(|s| s.name.clone()).collect();
            // Their own `use` lines aren't this file's modules; indices move past them.
            let kept: Vec<usize> = (0..theirs.len()).filter(|&i| theirs.get(i).is_some_and(|s| s.kind != SymbolKind::Module)).collect();
            let base = symbols.len();
            for &i in &kept {
                let Some(symbol) = theirs.get(i) else { continue };
                let mut symbol = symbol.clone();
                if matches!(symbol.kind, SymbolKind::Task | SymbolKind::Shape | SymbolKind::Kind | SymbolKind::Variant) {
                    symbol.name = format!("{}{}{}", name, MODULE_SEPARATOR, symbol.name);
                }
                symbol.detail = qualify_types(&symbol.detail, &name, &own_types);
                symbol.parent = symbol.parent.and_then(|parent| kept.iter().position(|&k| k == parent)).map(|p| base + p);
                symbol.module = Some(index);
                symbols.push(symbol);
            }
        }
        for written in &line.names {
            let full = format!("{}{}{}", name, MODULE_SEPARATOR, written);
            let found = symbols.iter().position(|s| {
                s.module == Some(index) && s.name == full && !s.private && matches!(s.kind, SymbolKind::Task | SymbolKind::Shape | SymbolKind::Kind | SymbolKind::Variant)
            });
            if let Some(id) = found {
                imports.entry(written.clone()).or_insert(id);
            }
        }
        modules.push(Module { path, source, symbol: line.symbol });
    }
    (modules, imports)
}

/// `text`, a declaration's detail written in module `module`, with the names
/// of that module's shapes and kinds (`own_types`) qualified: `(p: Point)`
/// becomes `(p: geometry.Point)`.
fn qualify_types(text: &str, module: &str, own_types: &HashSet<String>) -> String {
    let lexed = highlight(text);
    let mut out = String::new();
    let mut at = 0;
    let significant: Vec<&LexToken> = lexed.iter().filter(|t| t.kind != LexKind::Comment).collect();
    for (i, token) in significant.iter().enumerate() {
        let LexKind::Token(Token::Ident(name)) = &token.kind else { continue };
        let before = i.checked_sub(1).and_then(|b| significant.get(b)).map(|t| &t.kind);
        let after = significant.get(i + 1).map(|t| &t.kind);
        let names_type = own_types.contains(name)
            && !matches!(before, Some(LexKind::Token(Token::Dot | Token::AtSign)))
            && !matches!(after, Some(LexKind::Token(Token::Colon)));
        if names_type {
            out.push_str(text.get(at..token.range.start).unwrap_or_default());
            out.push_str(&format!("{}{}{}", module, MODULE_SEPARATOR, name));
            at = token.range.end;
        }
    }
    out.push_str(text.get(at..).unwrap_or_default());
    out
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

/// First pass: top-level declarations. Returns the symbols, for each
/// declaring token its symbol, and the `use` lines.
fn declarations(tokens: &Tokens) -> (Vec<Symbol>, HashMap<usize, usize>, Vec<UseLine>) {
    let mut symbols: Vec<Symbol> = Vec::new();
    let mut declared_at = HashMap::new();
    let mut uses = Vec::new();
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
        let private = tokens.before(p) == Some(&Token::Priv) || tokens.before(p.saturating_sub(1)) == Some(&Token::Priv);
        symbols.push(Symbol { name, kind, name_range: range.clone(), range, start, visible, intro: None, detail, parent, module: None, private });
        declared_at.insert(p, symbols.len() - 1);
        symbols.len() - 1
    };

    for p in 0..tokens.len() {
        let Some(token) = tokens.token(p) else {
            last_end = tokens.range(p).end;
            continue;
        };
        match token {
            // `use path/name (a, b)`: declares the module `name`.
            Token::Use => {
                let mut parts = Vec::new();
                let mut names = Vec::new();
                let mut alias = None;
                let mut in_names = false;
                let mut q = p + 1;
                while let Some(part) = tokens.token(q) {
                    match part {
                        Token::Newline | Token::Eof => break,
                        Token::LParen => in_names = true,
                        Token::Ident(name) if in_names => names.push(name.clone()),
                        Token::Ident(name) => {
                            parts.push(name.clone());
                            alias = Some(q);
                        }
                        Token::Range if !in_names => parts.push("..".to_string()),
                        _ => {}
                    }
                    q += 1;
                }
                if let Some(alias) = alias.filter(|&a| tokens.token(a + 1) != Some(&Token::Slash)) {
                    let written = tokens.text_after(p, never);
                    let path = written.split('(').next().unwrap_or_default().trim().to_string();
                    let symbol = declare(&mut symbols, alias, SymbolKind::Module, path, None);
                    uses.push(UseLine { parts, symbol, names });
                }
            }
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
                ([Block::Shape], Some(shape))
                    if (tokens.starts_line(p) || (tokens.before(p) == Some(&Token::Priv) && tokens.starts_line(p - 1)))
                        && tokens.token(p + 1) == Some(&Token::Colon) =>
                {
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
    (symbols, declared_at, uses)
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
    /// Tasks, shapes, kinds, variants, and modules by name (the first, if
    /// repeated); another module's by their qualified names (`geometry.area`).
    globals: HashMap<String, usize>,
    /// The names `use m (a, b)` brings in, with their declarations.
    imports: HashMap<String, usize>,
    scopes: Vec<Scope>,
    out: Vec<Classified>,
}

impl<'a> Classifier<'a> {
    fn new(
        tokens: &'a Tokens<'a>,
        symbols: Vec<Symbol>,
        declared_at: HashMap<usize, usize>,
        field_accesses: &'a HashMap<(usize, usize), String>,
        imports: HashMap<String, usize>,
    ) -> Self {
        let mut globals = HashMap::new();
        for (id, symbol) in symbols.iter().enumerate() {
            if !matches!(symbol.kind, SymbolKind::Field) {
                globals.entry(symbol.name.clone()).or_insert(id);
            }
        }
        Classifier { tokens, symbols, declared_at, field_accesses, globals, imports, scopes: Vec::new(), out: Vec::new() }
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
        // After `@` or a `new …(…)`'s `at`, where a region is named; whether a
        // type follows it (as after `new@r`).
        let mut region_next: Option<bool> = None;
        // In a task header after `->`, where each comma starts another result
        // type.
        let mut in_results = false;
        // In the names of a `bind` or `hold`, where each comma declares
        // another one.
        let mut declaring_list: Option<&'static str> = None;
        // On a `use` line, and inside its parentheses of names.
        let mut in_use = false;
        let mut in_use_names = false;
        let mut braces: Vec<Brace> = Vec::new();
        // How many `[`s are open in a type, as in `[int; 4]`.
        let mut type_brackets = 0usize;

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
                    in_use = false;
                    in_use_names = false;
                    in_task_header = false;
                    in_pattern = false;
                    in_results = false;
                    declaring_list = None;
                    type_brackets = 0;
                    // Shape literals are on one line, so an unclosed literal `{` ends
                    // with it; a plan B's block continues.
                    braces.retain(|brace| *brace == Brace::PlanB);
                }
                Token::Eof => {}
                Token::Int(_) | Token::Float(_) => self.push(range, TokenClass::Number, None),
                Token::Text(_) => self.string(range),
                Token::Ident(name) if in_use => {
                    let (class, symbol) = match self.declared_at.get(&p) {
                        Some(&id) => (TokenClass::Module, Some(id)),
                        None if in_use_names => {
                            let id = self.imports.get(name.as_str()).copied();
                            let class = id.and_then(|id| self.symbols.get(id)).map_or(TokenClass::Variable, |s| s.kind.class());
                            (class, id)
                        }
                        // A folder of the path.
                        None => (TokenClass::Module, None),
                    };
                    self.push(range, class, symbol);
                }
                Token::Ident(name) => {
                    if let Some(type_follows) = region_now {
                        let symbol = self.region(p, name, in_task_header);
                        self.push(range, TokenClass::Region, symbol);
                        in_type = type_follows;
                        after_type = !type_follows;
                    } else {
                        // `-> total: int`: the result's name, not its type.
                        let declaring_now = match tokens.token(p + 1) {
                            Some(Token::Colon) if in_results && was_type => Some("result"),
                            _ => declaring_now,
                        };
                        let (class, symbol) = self.identifier(p, name, was_type, declaring_now, in_task_header, in_pattern, &braces);
                        self.push(range, class, symbol);
                        after_type = was_type;
                    }
                }
                Token::AtSign => {
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
                    in_results = true;
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
                // `[int; 4]` and `[int]@r` in a type: the item's type, then its count.
                Token::LBracket if was_type => {
                    self.push(range, TokenClass::Punctuation, None);
                    type_brackets += 1;
                    in_type = true;
                }
                Token::Semicolon => self.push(range, TokenClass::Punctuation, None),
                Token::RBracket if type_brackets > 0 => {
                    self.push(range, TokenClass::Punctuation, None);
                    type_brackets -= 1;
                    // `@r` or `or E` may follow the type.
                    after_type = true;
                }
                Token::LParen | Token::RParen | Token::LBracket | Token::RBracket | Token::Dot => {
                    self.push(range, TokenClass::Punctuation, None);
                    match token {
                        Token::LParen if in_use => in_use_names = true,
                        // `geometry.Point`: a type goes on after the module's name.
                        Token::Dot => in_type = was_type,
                        _ => {}
                    }
                }
                Token::Comma => {
                    self.push(range, TokenClass::Punctuation, None);
                    match declaring_list {
                        // `-> int, int`: another result type.
                        _ if in_results => in_type = true,
                        // `bind q, r = …`: another name.
                        Some(intro) => declaring = Some(intro),
                        None => {}
                    }
                }
                Token::Plus | Token::Minus | Token::Star | Token::Slash | Token::Percent | Token::Eq | Token::Ne
                | Token::Lt | Token::Gt | Token::Le | Token::Ge | Token::Assign | Token::Range => {
                    self.push(range, TokenClass::Operator, None);
                    if *token == Token::Assign {
                        declaring_list = None;
                    }
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
                        Token::Bind | Token::Hold => {
                            let intro = if *keyword == Token::Bind { "bind" } else { "hold" };
                            declaring = Some(intro);
                            declaring_list = Some(intro);
                        }
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
                        // `new T(v) at r` names a region; `in` belongs to `each`.
                        Token::At => region_next = Some(false),
                        // `region r` declares a region; `r: region` is a parameter's type.
                        Token::Region if !was_type => declaring = Some("region"),
                        Token::Use => in_use = true,
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
        // `geometry.area`: a module's name, then a name in it.
        let at = tokens.range(p).start;
        if after == Some(&Token::Dot)
            && let Some(module) = self.module_named(name, at)
        {
            return (TokenClass::Module, Some(module));
        }
        if before == Some(&Token::Dot)
            && let Some(module) = p.checked_sub(2).and_then(|q| tokens.ident(q))
            && self.module_named(module, tokens.range(p - 2).start).is_some()
        {
            let full = format!("{}{}{}", module, MODULE_SEPARATOR, name);
            return match self.globals.get(&full).and_then(|&id| self.symbols.get(id).map(|s| (id, s.kind))) {
                Some((id, kind)) => (kind.class(), Some(id)),
                None if in_type => (TokenClass::Type, None),
                None => (TokenClass::Variable, None),
            };
        }
        // A name being declared is one even where a type would be written,
        // as a result's name is (`-> total: int`).
        if let Some(intro) = declaring {
            if intro == "region" {
                return (TokenClass::Region, Some(self.declare_local(p, SymbolKind::Region, Some(intro), String::new())));
            }
            // `bind q: int, r = …` and `-> steps: int, peak: int`: the type
            // ends at the next name.
            let detail = if after == Some(&Token::Colon) {
                tokens.text_after(p + 1, |t| matches!(t, Token::Assign | Token::Comma))
            } else {
                String::new()
            };
            return (TokenClass::Variable, Some(self.declare_local(p, SymbolKind::Variable, Some(intro), detail)));
        }
        if in_type {
            if Type::builtin(name).is_some() {
                return (TokenClass::TypeBuiltin, None);
            }
            return (TokenClass::Type, self.global(name, &[SymbolKind::Shape, SymbolKind::Kind]));
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
        self.symbols.push(Symbol { name, kind, name_range: range.clone(), range, start, visible, intro, detail, parent: None, module: None, private: false });
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

    /// The top-level declaration called `name`, if it is one of `kinds`: one of
    /// this file's, or one `use` brings in by name.
    fn global(&self, name: &str, kinds: &[SymbolKind]) -> Option<usize> {
        let own = self.globals.get(name).copied().filter(|&id| self.symbols.get(id).is_some_and(|s| s.module.is_none()));
        own.or_else(|| self.imports.get(name).copied())
            .filter(|&id| self.symbols.get(id).is_some_and(|s| kinds.contains(&s.kind)))
    }

    /// The module the file uses under `name`, unless a variable at byte `at`
    /// has that name.
    fn module_named(&self, name: &str, at: usize) -> Option<usize> {
        self.global(name, &[SymbolKind::Module]).filter(|_| self.local(name, at).is_none())
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
