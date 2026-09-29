//! The declaration scan: classifies every token for highlighting and resolves
//! names to their declarations, working from the compiler's error-tolerant
//! token stream rather than its parser, so it keeps working while code is half
//! typed.
//!
//! It runs in two passes. The first finds the top-level declarations (tasks,
//! shapes and their fields, kinds and their variants, and the modules `include`
//! lines name), which may be used before they are declared. The declarations
//! of each module the file includes are added then too, named as the file names
//! them (`geometry.area`). The second walks the tokens in order, tracking
//! blocks and the variables declared in them (parameters, `bind`/`hold`,
//! `each`, the names `case` and `is` patterns bind, the error a plan B names,
//! and regions), which are only visible after their declaration.

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;

use serde::Serialize;
use vela::ast::{ALTERS, BUILTIN_TASKS, DEFAULT, MODULE_SEPARATOR, NATIVE, NEW_WORD, READ, Span, THIS_WORD, Type, unqualified};
use vela::lexer::{ESCAPES, LexKind, LexToken, Piece, Token, highlight};

use crate::types::{Context, slashed};

/// What a highlighted piece of source is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TokenClass {
    Keyword,
    /// `int`, `float`, `bool`, `text`, `nothing` where a type is written.
    TypeBuiltin,
    /// A shape or kind name.
    Type,
    Variant,
    Task,
    /// A method of a shape or kind, where it is declared and where it is called.
    Method,
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
    /// A module's name, and the folders of an `include` path.
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
    /// A method of a shape or kind, declared inside it; `intro` is `alters` for
    /// one that changes the value it is called on.
    Method,
    /// A shape field or a variant's payload field.
    Field,
    Parameter,
    /// A `bind`, `hold`, `each`, `case`, `is`, or plan B (`fail e`) binding.
    Variable,
    /// A region: declared by `region r`, taken as a parameter `r: region`, or
    /// only named in a task header's types (`@r`).
    Region,
    /// A module the file includes, declared by its `include` line.
    Module,
    /// A type a generic declaration takes, `T` in `task first<T>(…)`.
    TypeParam,
}

impl SymbolKind {
    fn class(self) -> TokenClass {
        match self {
            SymbolKind::Task => TokenClass::Task,
            SymbolKind::Shape | SymbolKind::Kind => TokenClass::Type,
            SymbolKind::Variant => TokenClass::Variant,
            SymbolKind::Method => TokenClass::Method,
            SymbolKind::Field => TokenClass::Field,
            SymbolKind::Parameter => TokenClass::Parameter,
            SymbolKind::Variable => TokenClass::Variable,
            SymbolKind::Region => TokenClass::Region,
            SymbolKind::Module => TokenClass::Module,
            SymbolKind::TypeParam => TokenClass::Type,
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
    /// region statement) or `@` (a region a task header only names); for a
    /// method, `alters` when it changes the value it is called on.
    pub intro: Option<&'static str>,
    /// Bytes of the whole declaration: from `task`/`shape`/`kind` (or a
    /// method's first word) to its `fi` for those, and just the name for
    /// everything else.
    pub range: Range<usize>,
    /// Source text describing it: a task's parameters and return type, a
    /// field's or parameter's type, a variant's payload.
    pub detail: String,
    /// The shape of a field, the kind of a variant, the shape or kind of a
    /// method, or the variant of a payload field.
    pub parent: Option<usize>,
    /// The module it is declared in (an index into `Scan::modules`), for a
    /// declaration of another file; its ranges are in that file.
    pub module: Option<usize>,
    /// Marked `priv`: a task, shape, kind, or field of its own file only.
    pub private: bool,
    /// A field with a value a literal that leaves it out gives it
    /// (`size: int = 0`).
    pub default: bool,
    /// A field marked `read` (`read path: text`): any file reads it, and only
    /// the alters methods of its shape change it. A literal gives it exactly
    /// when it has no `default`.
    pub read: bool,
    /// A shape's `shared` field (`shared made: int = 0`): one value for the
    /// whole program, read through the shape's name, never a value's. `intro`
    /// is `hold` for one any code may change. A shared method (`shared
    /// new(…)`), called on the shape's or kind's name, has `intro` `shared`.
    pub shared: bool,
}

/// A module the file includes.
#[derive(Debug, Clone)]
pub(crate) struct Module {
    /// Its path, as the editor names files; empty when the file has no path.
    pub path: String,
    /// Its text, when the editor gave it.
    pub source: Option<String>,
    /// Its `include` line's symbol, named by the name this file names the
    /// module by (or, when only its parts are included, by its path from this
    /// file's folder, which no code can write).
    pub symbol: usize,
    /// Whether this file names the module: not when only its parts are included.
    pub named: bool,
    /// Whether it is a module of Vela's library, whose text is the compiler's
    /// and no file the editor has (see `vela::library`).
    pub library: bool,
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
    /// The modules the file includes, in the order of its `include` lines.
    pub modules: Vec<Module>,
    /// The names `include m (a, b)` brings in, each by the name this file
    /// writes (its alias, if it has one), with the declaration it names.
    pub imports: HashMap<String, usize>,
}

/// `field_accesses` maps the (line, column) of a field name in `a.b` to the
/// shape of `a`, when the checker knows it. `context` gives the file's path
/// and the other files it may include.
pub(crate) fn scan(source: &str, field_accesses: &HashMap<(usize, usize), String>, context: Context) -> Scan {
    let lexed = spread(highlight(source));
    let tokens = Tokens::new(source, &lexed);
    let (mut symbols, mut declared_at, includes) = declarations(&tokens);
    let (modules, imports) = included_modules(&mut symbols, &mut declared_at, &includes, context);
    let classified = Classifier::new(&tokens, symbols, declared_at, field_accesses, imports.clone()).run();
    let (symbols, tokens) = classified;
    Scan { lexed, symbols, tokens, modules, imports }
}

/// `lexed` with each text holding values spread into its pieces: its text as
/// text tokens, each taking the braces next to it, and each value's own tokens
/// between them, so the analysis sees the names in a value as it sees any.
pub(crate) fn spread(lexed: Vec<LexToken>) -> Vec<LexToken> {
    let mut out = Vec::with_capacity(lexed.len());
    for token in lexed {
        spread_into(token, &mut out);
    }
    out
}

fn spread_into(token: LexToken, out: &mut Vec<LexToken>) {
    let LexKind::Token(Token::Interpolated(pieces)) = token.kind else {
        out.push(token);
        return;
    };
    let text = |start, end, range| LexToken { kind: LexKind::Token(Token::Text(String::new())), start, end, range };
    // Where the text before the next value starts.
    let (mut start, mut from) = (token.start, token.range.start);
    for piece in pieces {
        let Piece::Value { open, tokens, close } = piece else { continue };
        out.push(text(start, open.end, from..open.range.end));
        for inner in tokens {
            spread_into(inner, out);
        }
        (start, from) = (close.start, close.range.start);
    }
    out.push(text(start, token.end, from..token.range.end));
}

/// An `include` line found by the first pass.
struct IncludeLine {
    /// The path as written: names and `..`.
    parts: Vec<String>,
    /// The position of the path's last part.
    last: usize,
    /// The module's symbol, declared at its alias if it has one.
    symbol: usize,
    /// The names in parentheses or braces.
    names: Vec<IncludedName>,
    /// Written with braces: only the names are included.
    only_parts: bool,
}

/// A name on an `include` line: `Point`, or `Point => Spot`.
struct IncludedName {
    name: String,
    /// The position of the name.
    at: usize,
    /// The name after `=>` and its position.
    alias: Option<(String, usize)>,
}

/// Adds the declarations of each module in `includes` to `symbols`, named as
/// this file names them (`geometry.area`, whose signature says
/// `geometry.Point`), and returns the modules and the names brought in by name.
/// Each name on an `include` line is recorded in `declared_at` with the
/// declaration it names.
fn included_modules(
    symbols: &mut Vec<Symbol>,
    declared_at: &mut HashMap<usize, usize>,
    includes: &[IncludeLine],
    context: Context,
) -> (Vec<Module>, HashMap<String, usize>) {
    let folder = Path::new(context.path).parent().unwrap_or(Path::new(""));
    let mut modules = Vec::new();
    let mut imports = HashMap::new();
    for line in includes {
        let index = modules.len();
        declared_at.insert(line.last, line.symbol);
        let file = vela::modules::module_file(folder, &line.parts);
        // A module of Vela's library is the compiler's own, whatever the
        // editor has (see `vela::library`).
        let library = line.parts.split_first().filter(|(first, _)| *first == vela::library::ROOT).map(|(_, rest)| rest.join("/"));
        let path = match &library {
            Some(name) => format!("{}/{}.vela", vela::library::ROOT, name),
            None => slashed(&file),
        };
        let source = match &library {
            Some(name) => vela::library::module(name).map(str::to_string),
            None => context.sources.get(&path).cloned(),
        };
        // The name this file names the module by, or its path, which the
        // compiler names a declaration this file can't write by.
        let name = if line.only_parts {
            let id = match &library {
                Some(name) => format!("{}/{}", vela::library::ROOT, name),
                None => vela::modules::module_id(folder, &file),
            };
            if let Some(symbol) = symbols.get_mut(line.symbol) {
                symbol.name = id.clone();
            }
            id
        } else {
            symbols.get(line.symbol).map(|s| s.name.clone()).unwrap_or_default()
        };
        // Each declaration of the module by its own name.
        let mut declared: HashMap<String, usize> = HashMap::new();
        if let Some(source) = &source {
            let lexed = spread(highlight(source));
            let tokens = Tokens::new(source, &lexed);
            let (theirs, _, _) = declarations(&tokens);
            let listed = |written: &str| line.names.iter().find(|n| n.name == written);
            // How this file writes a name of the module: its alias, the name
            // alone when it is brought in, or through the module.
            let written_as = |written: &str| match listed(written) {
                Some(IncludedName { alias: Some((alias, _)), .. }) => alias.clone(),
                Some(_) => written.to_string(),
                None => format!("{}{}{}", name, MODULE_SEPARATOR, written),
            };
            let types: HashMap<String, String> = theirs
                .iter()
                .filter(|s| matches!(s.kind, SymbolKind::Shape | SymbolKind::Kind))
                .map(|s| (s.name.clone(), written_as(&s.name)))
                .collect();
            // Their own `include` lines aren't this file's modules; indices move past them.
            let kept: Vec<usize> = (0..theirs.len()).filter(|&i| theirs.get(i).is_some_and(|s| s.kind != SymbolKind::Module)).collect();
            let base = symbols.len();
            for &i in &kept {
                let Some(symbol) = theirs.get(i) else { continue };
                let mut symbol = symbol.clone();
                if matches!(symbol.kind, SymbolKind::Task | SymbolKind::Shape | SymbolKind::Kind | SymbolKind::Variant) {
                    declared.entry(symbol.name.clone()).or_insert(symbols.len());
                    // A variant keeps its name: only a whole declaration takes an alias.
                    let kind = symbol.parent.and_then(|parent| theirs.get(parent)).map(|kind| kind.name.as_str());
                    symbol.name = match symbol.kind {
                        // Without the module's name, a variant is written alone
                        // when it is brought in, or else through its kind.
                        SymbolKind::Variant if line.only_parts && listed(&symbol.name).is_some() => symbol.name.clone(),
                        SymbolKind::Variant if line.only_parts && kind.is_some_and(|kind| listed(kind).is_some()) => {
                            format!("{}{}{}", written_as(kind.unwrap_or_default()), MODULE_SEPARATOR, symbol.name)
                        }
                        SymbolKind::Variant => format!("{}{}{}", name, MODULE_SEPARATOR, symbol.name),
                        // Named as written: under its alias, or alone when the module has no name here.
                        _ if listed(&symbol.name).is_some_and(|n| n.alias.is_some() || line.only_parts) => written_as(&symbol.name),
                        _ => format!("{}{}{}", name, MODULE_SEPARATOR, symbol.name),
                    };
                }
                symbol.detail = qualify_types(&symbol.detail, &types);
                symbol.parent = symbol.parent.and_then(|parent| kept.iter().position(|&k| k == parent)).map(|p| base + p);
                symbol.module = Some(index);
                symbols.push(symbol);
            }
        }
        for written in &line.names {
            let found = declared.get(&written.name).copied().filter(|&id| {
                symbols.get(id).is_some_and(|s| !s.private && matches!(s.kind, SymbolKind::Task | SymbolKind::Shape | SymbolKind::Kind | SymbolKind::Variant))
            });
            if let Some(id) = found {
                declared_at.insert(written.at, id);
                let local = match &written.alias {
                    Some((alias, at)) => {
                        declared_at.insert(*at, id);
                        alias
                    }
                    None => &written.name,
                };
                imports.entry(local.clone()).or_insert(id);
            }
        }
        modules.push(Module { path, source, symbol: line.symbol, named: !line.only_parts, library: library.is_some() });
    }
    (modules, imports)
}

/// `text`, a declaration's detail written in another module, with the names of
/// that module's shapes and kinds as this file writes them (`types`): `(p: Point)`
/// becomes `(p: geometry.Point)`.
fn qualify_types(text: &str, types: &HashMap<String, String>) -> String {
    let lexed = highlight(text);
    let mut out = String::new();
    let mut at = 0;
    let significant: Vec<&LexToken> = lexed.iter().filter(|t| t.kind != LexKind::Comment).collect();
    for (i, token) in significant.iter().enumerate() {
        let LexKind::Token(Token::Ident(name)) = &token.kind else { continue };
        let before = i.checked_sub(1).and_then(|b| significant.get(b)).map(|t| &t.kind);
        let after = significant.get(i + 1).map(|t| &t.kind);
        let written = types
            .get(name)
            .filter(|_| !matches!(before, Some(LexKind::Token(Token::Dot | Token::AtSign))) && !matches!(after, Some(LexKind::Token(Token::Colon))));
        if let Some(written) = written {
            out.push_str(text.get(at..token.range.start).unwrap_or_default());
            out.push_str(written);
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
    /// lines: it ends its line, or only `failure e` follows it there.
    fn opens_lines(&self, p: usize) -> bool {
        let names_error = self.token(p + 1) == Some(&Token::Failure) && matches!(self.token(p + 2), Some(Token::Ident(_) | Token::Underscore));
        let end = if names_error { p + 3 } else { p + 1 };
        matches!(self.token(end), Some(Token::Newline | Token::Eof) | None)
    }

    /// Whether the `<` at position `p` opens the types a declaration takes or a
    /// use gives: one right after the name of a `task` or `shape`, or one whose
    /// matching `>` is followed by `(` (a call) or `{` (a shape literal), as the
    /// parser reads them.
    fn opens_types(&self, p: usize) -> bool {
        if self.token(p) != Some(&Token::Lt) {
            return false;
        }
        let declares = p >= 2 && self.ident(p - 1).is_some() && matches!(self.token(p - 2), Some(Token::Task | Token::Shape));
        if declares {
            return true;
        }
        let mut depth = 0usize;
        // Parentheses pair up inside the types, and no `>` inside them closes
        // anything, as the parser has it (`(i < n), m > (k)` compares).
        let mut grouped = 0usize;
        for q in p + 1.. {
            match self.token(q) {
                Some(Token::Gt) if grouped > 0 && depth == 0 => return false,
                Some(Token::LParen) => grouped += 1,
                Some(Token::RParen) if grouped == 0 => return false,
                Some(Token::RParen) => grouped -= 1,
                // A call follows the types a task is given, and a literal the
                // types a shape is given.
                Some(Token::Gt) if depth == 0 => return matches!(self.token(q + 1), Some(Token::LParen | Token::LBrace)),
                Some(Token::Lt) => depth += 1,
                Some(Token::Gt) => depth -= 1,
                Some(token) if token.writes_type() => {}
                _ => return false,
            }
        }
        false
    }

    /// The shape whose literal opens with the `{` at `p`: the name before it, or,
    /// for a shape given types (`Box<int> { … }`), the name before those.
    fn literal_shape(&self, p: usize) -> Option<&'a str> {
        let mut before = p.checked_sub(1)?;
        if self.token(before) == Some(&Token::Gt) {
            let mut depth = 0usize;
            loop {
                before = before.checked_sub(1)?;
                match self.token(before) {
                    Some(Token::Lt) if depth == 0 => break,
                    Some(Token::Lt) => depth -= 1,
                    Some(Token::Gt) => depth += 1,
                    Some(_) => {}
                    None => return None,
                }
            }
            before = before.checked_sub(1)?;
        }
        self.ident(before)
    }

    /// Whether position `p` starts a line.
    fn starts_line(&self, p: usize) -> bool {
        p == 0 || self.before(p) == Some(&Token::Newline)
    }

    /// The trimmed source text after token `p`, up to a comment, the end of the
    /// line, or the first token `stop` names outside brackets: a tuple type's
    /// commas and its `)` are part of it, as in `(int, text)`.
    fn text_after(&self, p: usize, stop: fn(&Token) -> bool) -> String {
        let Some(&first) = self.significant.get(p) else { return String::new() };
        let from = self.range(p).end;
        let mut depth = 0usize;
        let to = self
            .all
            .iter()
            .skip(first + 1)
            .find(|t| match &t.kind {
                LexKind::Token(Token::LParen | Token::LBracket | Token::LBrace) => {
                    depth += 1;
                    false
                }
                LexKind::Token(Token::RParen | Token::RBracket | Token::RBrace) if depth > 0 => {
                    depth -= 1;
                    false
                }
                LexKind::Token(token) => *token == Token::Newline || *token == Token::Eof || (depth == 0 && stop(token)),
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

    /// Whether position `p` starts a member of a shape or a kind: the first
    /// word of its line, or the first after `priv`.
    fn starts_member(&self, p: usize) -> bool {
        self.starts_line(p) || (self.before(p) == Some(&Token::Priv) && self.starts_line(p - 1))
    }

    /// Where the name of the method whose line starts at position `p` (after
    /// any `priv`) is, as the parser reads a shape's or a kind's members: a
    /// name followed by `(`, or by `<` for one that names types, with `alters`
    /// allowed before it. Inside a kind, `in_kind`, a variant's values are
    /// written as a method's parameters are, so the parser's rule tells them
    /// apart (see `vela::parser`'s `at_variant_payload`): with no method above
    /// it, `methods_above`, and without `alters`, a member whose line ends after
    /// parentheses holding named values is a variant.
    fn method_at(&self, p: usize, in_kind: bool, methods_above: bool) -> Option<usize> {
        let alters = self.ident(p) == Some(ALTERS) && self.ident(p + 1).is_some();
        let name = if alters { p + 1 } else { p };
        self.ident(name)?;
        if !matches!(self.token(name + 1), Some(Token::LParen | Token::Lt)) {
            return None;
        }
        if !in_kind || methods_above || alters {
            return Some(name);
        }
        let mut depth = 0usize;
        let mut named = false;
        for q in name + 1.. {
            match self.token(q) {
                Some(Token::LParen) => depth += 1,
                Some(Token::RParen) => match depth.checked_sub(1) {
                    Some(0) => {
                        let ends = matches!(self.token(q + 1), Some(Token::Newline | Token::Eof) | None);
                        return (!(named && ends)).then_some(name);
                    }
                    Some(left) => depth = left,
                    None => return Some(name),
                },
                Some(Token::Colon) if depth == 1 => named = true,
                Some(_) => {}
                None => return Some(name),
            }
        }
        Some(name)
    }
}

/// Whether a value can end with `token`, so that a `.` after it reads a member
/// of that value (`p.x`, `f().m()`, `xs[0].x`, `p^.x`, `t.0.x`); a `.` after
/// anything else starts a member of the value a method is called on, `.x`.
pub(crate) fn ends_value(token: &Token) -> bool {
    matches!(
        token,
        Token::Ident(_) | Token::Int(_) | Token::Float(_) | Token::Char(_) | Token::Text(_) | Token::True | Token::False | Token::None | Token::RParen | Token::RBracket | Token::RBrace | Token::Caret
    )
}

/// Whether the argument at position `p` belongs to a `scan` call, so that the
/// type it names is the number that call reads: `scan("Age: ", int)`.
fn reads_a_number(tokens: &Tokens, p: usize) -> bool {
    let mut depth = 0usize;
    for q in (0..p).rev() {
        match tokens.token(q) {
            Some(Token::Newline | Token::Eof) | None => return false,
            Some(Token::RParen) => depth += 1,
            Some(Token::LParen) if depth == 0 => {
                return q.checked_sub(1).is_some_and(|name| tokens.ident(name) == Some(vela::ast::SCAN) && tokens.before(name) != Some(&Token::Dot));
            }
            Some(Token::LParen) => depth -= 1,
            _ => {}
        }
    }
    false
}

fn never(_: &Token) -> bool {
    false
}

fn ends_field(token: &Token) -> bool {
    matches!(token, Token::Comma | Token::RParen)
}

/// Whether the name at `p` is a shape's field marked `read`, the word starting
/// its line (`read path: text`). A field named `read` is followed by `:`, so it
/// is never taken for the word.
fn read_marker(tokens: &Tokens, p: usize) -> bool {
    p.checked_sub(1).is_some_and(|q| tokens.ident(q) == Some(READ) && tokens.starts_line(q))
}

/// Whether the name at `p` is a shape's `shared` field, as in `shared f`,
/// `shared hold f`, or `shared read f`, with `shared` starting its line (after
/// `priv`, if it is written); and if so, whether it is `hold`.
fn shared_marker(tokens: &Tokens, p: usize) -> Option<bool> {
    let (at, hold) = match p.checked_sub(1).and_then(|q| tokens.token(q)) {
        Some(Token::Shared) => (p - 1, false),
        Some(Token::Hold) => (p.checked_sub(2)?, true),
        Some(Token::Ident(word)) if word == READ => (p.checked_sub(2)?, false),
        _ => return None,
    };
    let starts = tokens.starts_line(at) || (tokens.before(at) == Some(&Token::Priv) && tokens.starts_line(at - 1));
    (tokens.token(at) == Some(&Token::Shared) && starts).then_some(hold)
}

/// What ends a shape's field, whose type may be followed by the value a literal
/// that leaves the field out gives it (`size: int = 0`).
fn ends_shape_field(token: &Token) -> bool {
    matches!(token, Token::Assign)
}

/// What ends the words a type a declaration takes must allow.
fn ends_type_param(token: &Token) -> bool {
    matches!(token, Token::Comma | Token::Gt)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Block {
    Task,
    Shape,
    Kind,
    /// A method, inside a shape or a kind.
    Method,
    /// `when`, `loop`, `each`, or `match`: any block inside a task.
    Inner,
}

/// First pass: top-level declarations. Returns the symbols, for each
/// declaring token its symbol, and the `include` lines.
fn declarations(tokens: &Tokens) -> (Vec<Symbol>, HashMap<usize, usize>, Vec<IncludeLine>) {
    let mut symbols: Vec<Symbol> = Vec::new();
    let mut declared_at = HashMap::new();
    let mut includes = Vec::new();
    let mut blocks: Vec<Block> = Vec::new();
    // The open task, shape, or kind, and the variant whose payload is open.
    let mut item: Option<usize> = None;
    let mut variant: Option<usize> = None;
    // The open method, and whether the open shape or kind has one above.
    let mut method: Option<usize> = None;
    let mut methods_above = false;
    let mut parens = 0usize;
    // End of the last token that isn't a newline, where an unclosed item ends.
    let mut last_end = 0;

    let mut declare = |symbols: &mut Vec<Symbol>, p: usize, kind: SymbolKind, detail: String, parent: Option<usize>| {
        let name = tokens.ident(p).unwrap_or_default().to_string();
        let range = tokens.range(p);
        let start = tokens.get(p).map(|t| t.start).unwrap_or_default();
        let visible = 0..tokens.source.len();
        // `priv name`, `priv task name`, or `priv native task name`.
        let native = p >= 2 && tokens.ident(p - 2) == Some(NATIVE);
        let private = tokens.before(p) == Some(&Token::Priv)
            || tokens.before(p.saturating_sub(1)) == Some(&Token::Priv)
            || (native && tokens.before(p - 2) == Some(&Token::Priv));
        symbols.push(Symbol { name, kind, name_range: range.clone(), range, start, visible, intro: None, detail, parent, module: None, private, default: false, read: false, shared: false });
        declared_at.insert(p, symbols.len() - 1);
        symbols.len() - 1
    };

    for p in 0..tokens.len() {
        let Some(token) = tokens.token(p) else {
            last_end = tokens.range(p).end;
            continue;
        };
        match token {
            // `include path/name => alias (a, b => c)` or `include path/name {a}`:
            // declares the module `name`, or `alias`.
            Token::Include => {
                let mut parts = Vec::new();
                let mut names: Vec<IncludedName> = Vec::new();
                let mut last_part = None;
                let mut alias = None;
                let mut in_names = false;
                let mut only_parts = false;
                let mut after_arrow = false;
                let mut q = p + 1;
                while let Some(part) = tokens.token(q) {
                    match part {
                        Token::Newline | Token::Eof => break,
                        Token::LParen => in_names = true,
                        Token::LBrace => {
                            in_names = true;
                            only_parts = true;
                        }
                        Token::FatArrow => after_arrow = true,
                        Token::Ident(name) if after_arrow => {
                            after_arrow = false;
                            match names.last_mut() {
                                Some(last) if in_names => last.alias = Some((name.clone(), q)),
                                _ if !in_names => alias = Some(q),
                                _ => {}
                            }
                        }
                        Token::Ident(name) if in_names => names.push(IncludedName { name: name.clone(), at: q, alias: None }),
                        Token::Ident(name) => {
                            parts.push(name.clone());
                            last_part = Some(q);
                        }
                        Token::Range if !in_names => parts.push("..".to_string()),
                        _ => after_arrow = false,
                    }
                    q += 1;
                }
                if let Some(last) = last_part.filter(|&last| tokens.token(last + 1) != Some(&Token::Slash)) {
                    let written = tokens.text_after(p, never);
                    let path = written.split(['(', '{']).next().unwrap_or_default().trim().to_string();
                    let symbol = declare(&mut symbols, alias.unwrap_or(last), SymbolKind::Module, path, None);
                    includes.push(IncludeLine { parts, last, symbol, names, only_parts });
                }
            }
            // Items only appear at the top level, so a new one also closes
            // any item left open by half-typed code.
            Token::Task | Token::Shape | Token::Kind => {
                for open in [method.take(), item.take()].into_iter().flatten() {
                    close(&mut symbols, open, last_end);
                }
                methods_above = false;
                blocks.clear();
                let (block, kind) = match token {
                    Token::Task => (Block::Task, SymbolKind::Task),
                    Token::Shape => (Block::Shape, SymbolKind::Shape),
                    _ => (Block::Kind, SymbolKind::Kind),
                };
                blocks.push(block);
                if tokens.ident(p + 1).is_some() {
                    // A shape's line holds the types it takes, if any; a
                    // task's holds its parameters and results too.
                    let detail = if block == Block::Kind { String::new() } else { tokens.text_after(p + 1, never) };
                    let id = declare(&mut symbols, p + 1, kind, detail, None);
                    if let Some(symbol) = symbols.get_mut(id) {
                        symbol.range.start = tokens.range(p).start;
                    }
                    item = Some(id);
                    // `task first<T: ordered>(…)`: the types the task takes,
                    // each with the words it must allow.
                    let mut q = p + 2;
                    if tokens.token(q) == Some(&Token::Lt) {
                        let mut names = true;
                        while let Some(token) = tokens.token(q) {
                            match token {
                                Token::Gt | Token::Newline | Token::Eof | Token::LParen => break,
                                Token::Lt | Token::Comma => names = true,
                                Token::Colon | Token::Amp => names = false,
                                Token::Ident(_) if names => {
                                    names = false;
                                    let word = if tokens.token(q + 1) == Some(&Token::Colon) {
                                        tokens.text_after(q + 1, ends_type_param)
                                    } else {
                                        String::new()
                                    };
                                    declare(&mut symbols, q, SymbolKind::TypeParam, word, Some(id));
                                }
                                _ => {}
                            }
                            q += 1;
                        }
                    }
                }
            }
            Token::When | Token::Loop | Token::Each | Token::Block | Token::Match => blocks.push(Block::Inner),
            Token::Fi => {
                if blocks.pop() == Some(Block::Method)
                    && let Some(open) = method.take()
                {
                    close(&mut symbols, open, tokens.range(p).end);
                }
                if blocks.is_empty()
                    && let Some(open) = item.take()
                {
                    close(&mut symbols, open, tokens.range(p).end);
                }
            }
            Token::LParen => parens += 1,
            Token::RParen => parens = parens.saturating_sub(1),
            Token::Newline => parens = 0,
            // `shared new(…)`: a method of the shape or kind itself, named by a
            // name or by `new`, a block of its own as any method is.
            Token::Shared
                if let (Some(owner), [Block::Shape | Block::Kind]) = (item, blocks.as_slice())
                    && tokens.starts_member(p)
                    && matches!(tokens.token(p + 1), Some(Token::Ident(_) | Token::New))
                    && matches!(tokens.token(p + 2), Some(Token::LParen | Token::Lt)) =>
            {
                let name = p + 1;
                let detail = tokens.text_after(name, never);
                let id = declare(&mut symbols, name, SymbolKind::Method, detail, Some(owner));
                if let Some(symbol) = symbols.get_mut(id) {
                    if tokens.token(name) == Some(&Token::New) {
                        symbol.name = NEW_WORD.to_string();
                    }
                    symbol.range.start = tokens.range(if tokens.before(p) == Some(&Token::Priv) { p - 1 } else { p }).start;
                    symbol.private = tokens.before(p) == Some(&Token::Priv);
                    symbol.shared = true;
                    symbol.intro = Some("shared");
                }
                blocks.push(Block::Method);
                method = Some(id);
                methods_above = true;
            }
            // A method, which a shape or a kind declares below its fields or
            // variants: a block of its own, closed by its `fi`.
            Token::Ident(_)
                if let (Some(owner), [only @ (Block::Shape | Block::Kind)]) = (item, blocks.as_slice())
                    && tokens.starts_member(p)
                    && let Some(name) = tokens.method_at(p, *only == Block::Kind, methods_above) =>
            {
                let detail = tokens.text_after(name, never);
                let id = declare(&mut symbols, name, SymbolKind::Method, detail, Some(owner));
                if let Some(symbol) = symbols.get_mut(id) {
                    symbol.range.start = tokens.range(if tokens.before(p) == Some(&Token::Priv) { p - 1 } else { p }).start;
                    symbol.intro = (name != p).then_some(ALTERS);
                }
                blocks.push(Block::Method);
                method = Some(id);
                methods_above = true;
            }
            Token::Ident(_) => match (blocks.as_slice(), item) {
                ([Block::Shape], Some(shape))
                    if (tokens.starts_line(p)
                        || (tokens.before(p) == Some(&Token::Priv) && tokens.starts_line(p - 1))
                        || read_marker(tokens, p)
                        || shared_marker(tokens, p).is_some())
                        && tokens.token(p + 1) == Some(&Token::Colon) =>
                {
                    // The type, and whether a value it falls back on follows it.
                    let whole = tokens.text_after(p + 1, never);
                    let detail = tokens.text_after(p + 1, ends_shape_field);
                    let falls_back = whole != detail;
                    let shared = shared_marker(tokens, p);
                    let id = declare(&mut symbols, p, SymbolKind::Field, detail, Some(shape));
                    if let Some(field) = symbols.get_mut(id) {
                        field.default = falls_back;
                        field.read = read_marker(tokens, p) || (shared.is_some() && tokens.ident(p - 1) == Some(READ));
                        field.shared = shared.is_some();
                        field.intro = shared.filter(|hold| *hold).map(|_| "hold");
                        // `priv shared f`: `declare` looks only just before the name.
                        if shared.is_some() {
                            let start = (p.saturating_sub(3)..p).find(|q| tokens.token(*q) == Some(&Token::Shared));
                            field.private = start.is_some_and(|q| tokens.before(q) == Some(&Token::Priv));
                        }
                    }
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
    for open in [method, item].into_iter().flatten() {
        close(&mut symbols, open, last_end);
    }
    (symbols, declared_at, includes)
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
    /// The names `include m (a, b)` brings in, with their declarations.
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
        // A field or a method is only ever written after a dot.
        for (id, symbol) in symbols.iter().enumerate() {
            if !matches!(symbol.kind, SymbolKind::Field | SymbolKind::Method) {
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
        // On an `include` line, and inside its parentheses of names.
        let mut in_include = false;
        let mut in_include_names = false;
        let mut braces: Vec<Brace> = Vec::new();
        // How many `[`s are open in a type, as in `[int#4]`.
        let mut type_brackets = 0usize;
        // How many `(`s of tuple types are open, as in `(int, text)`.
        let mut type_parens = 0usize;
        // How many `<`s of the types a declaration takes, or a call gives, are
        // open; and whether the next name is a word a type must allow.
        let mut type_angles = 0usize;
        let mut needs_word = false;

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
            if !matches!(token, Token::Newline | Token::Failure) {
                plan_b_head = false;
            }

            match token {
                Token::Newline => {
                    in_include = false;
                    in_include_names = false;
                    in_task_header = false;
                    in_pattern = false;
                    in_results = false;
                    declaring_list = None;
                    type_brackets = 0;
                    type_parens = 0;
                    type_angles = 0;
                    needs_word = false;
                    // Shape literals are on one line, so an unclosed literal `{` ends
                    // with it; a plan B's block continues.
                    braces.retain(|brace| *brace == Brace::PlanB);
                }
                Token::Eof => {}
                // `t.0`: a tuple's item.
                Token::Int(_) if p > 0 && tokens.token(p - 1) == Some(&Token::Dot) => self.push(range, TokenClass::Field, None),
                Token::Int(_) | Token::Float(_) => self.push(range, TokenClass::Number, None),
                Token::Text(_) | Token::Char(_) => self.string(range),
                Token::Ident(_) if in_include => {
                    let (class, symbol) = match self.declared_at.get(&p) {
                        // The module's name or alias, or a name brought in and its alias.
                        Some(&id) => (self.symbols.get(id).map_or(TokenClass::Variable, |s| s.kind.class()), Some(id)),
                        // A name the module doesn't declare (or whose file isn't known).
                        None if in_include_names => (TokenClass::Variable, None),
                        // A folder of the path.
                        None => (TokenClass::Module, None),
                    };
                    self.push(range, class, symbol);
                }
                Token::FatArrow => self.push(range, TokenClass::Operator, None),
                Token::LBrace | Token::RBrace if in_include => {
                    self.push(range, TokenClass::Punctuation, None);
                    in_include_names = true;
                }
                // `<T: ordered>`: what the type must allow.
                Token::Ident(_) if needs_word => {
                    needs_word = false;
                    self.push(range, TokenClass::Keyword, None);
                }
                // `shared new(…)` and `X.new(…)`: the keyword naming a shared
                // method, read as the name it is there.
                Token::New
                    if self.declared_at.contains_key(&p) || (tokens.before(p) == Some(&Token::Dot) && tokens.token(p + 1) == Some(&Token::LParen)) =>
                {
                    if self.declared_at.contains_key(&p) {
                        self.open(ScopeKind::Item);
                        in_task_header = true;
                    }
                    let (class, symbol) = self.identifier(p, NEW_WORD, false, None, in_task_header, in_pattern, &braces);
                    self.push(range, class, symbol);
                }
                // `t.at(i)`: the keyword naming the method text has.
                Token::At if tokens.before(p) == Some(&Token::Dot) && tokens.token(p + 1) == Some(&Token::LParen) => {
                    self.push(range, TokenClass::Method, None);
                }
                Token::Ident(name) => {
                    if let Some(type_follows) = region_now {
                        let symbol = self.region(p, name, in_task_header);
                        self.push(range, TokenClass::Region, symbol);
                        in_type = type_follows;
                        after_type = !type_follows;
                    } else {
                        // A method's header declares its parameters, which its
                        // body sees, as a task's does.
                        if self.declared_at.get(&p).and_then(|&id| self.symbols.get(id)).is_some_and(|s| s.kind == SymbolKind::Method) {
                            self.open(ScopeKind::Item);
                            in_task_header = true;
                        }
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
                Token::Colon if type_angles > 0 => {
                    self.push(range, TokenClass::Punctuation, None);
                    needs_word = true;
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
                        braces.push(Brace::Literal(tokens.literal_shape(p).map(str::to_string)));
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
                // `[int#4]` and `[int]@r` in a type: the item's type, then its count.
                Token::LBracket if was_type => {
                    self.push(range, TokenClass::Punctuation, None);
                    type_brackets += 1;
                    in_type = true;
                }
                Token::Hash => self.push(range, TokenClass::Punctuation, None),
                Token::RBracket if type_brackets > 0 => {
                    self.push(range, TokenClass::Punctuation, None);
                    type_brackets -= 1;
                    // `@r` or `or E` may follow the type.
                    after_type = true;
                }
                // `(int, text)` in a type: each item's type.
                Token::LParen if was_type => {
                    self.push(range, TokenClass::Punctuation, None);
                    type_parens += 1;
                    in_type = true;
                }
                Token::RParen if type_parens > 0 => {
                    self.push(range, TokenClass::Punctuation, None);
                    type_parens -= 1;
                    // `@r` or `or E` may follow the type.
                    after_type = true;
                }
                // `bind a, (b, c) = t`: the names of a tuple's items.
                Token::LParen if declaring_now.is_some() => {
                    self.push(range, TokenClass::Punctuation, None);
                    declaring = declaring_now;
                }
                Token::LParen | Token::RParen | Token::LBracket | Token::RBracket | Token::Dot => {
                    self.push(range, TokenClass::Punctuation, None);
                    match token {
                        Token::LParen if in_include => in_include_names = true,
                        // `geometry.Point`: a type goes on after the module's name,
                        // which ended a type as far as it went.
                        Token::Dot => in_type = was_after_type,
                        _ => {}
                    }
                }
                Token::Comma if type_parens > 0 || type_angles > 0 => {
                    self.push(range, TokenClass::Punctuation, None);
                    in_type = true;
                }
                // `<T: ordered>`, `first<int>(…)`, and `Box<int>` in a type:
                // the types a declaration takes, or the ones a use gives it.
                Token::Lt if was_after_type || tokens.opens_types(p) => {
                    self.push(range, TokenClass::Punctuation, None);
                    type_angles += 1;
                    in_type = true;
                }
                Token::Gt if type_angles > 0 => {
                    self.push(range, TokenClass::Punctuation, None);
                    type_angles -= 1;
                    after_type = true;
                }
                Token::Amp if type_angles > 0 => {
                    self.push(range, TokenClass::Punctuation, None);
                    needs_word = true;
                }
                // `hold&` is one word, and `&a` gives a place at a call.
                // The name after `hold&` or `bind&` is still being declared.
                Token::Amp if matches!(tokens.before(p), Some(Token::Hold | Token::Bind)) => {
                    self.push(range, TokenClass::Keyword, None);
                    declaring = declaring_now;
                }
                Token::Amp => self.push(range, TokenClass::Operator, None),
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
                | Token::Similar | Token::NotSimilar
                | Token::Lt | Token::Gt | Token::Le | Token::Ge | Token::Assign | Token::Range => {
                    self.push(range, TokenClass::Operator, None);
                    if *token == Token::Assign {
                        declaring_list = None;
                        // `case p = value`: the pattern ends at the `=`, and what
                        // follows is the value that case gives.
                        in_pattern = false;
                    }
                }
                // A type after `maybe`, and the error type after `~` in a type.
                Token::Maybe => {
                    self.push(range, TokenClass::Keyword, None);
                    in_type = was_type;
                }
                Token::Tilde => {
                    self.push(range, TokenClass::Operator, None);
                    in_type = was_after_type;
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
                        // `m is {x} and x > 0`: no pattern holds an `and`, so
                        // what follows it is a value that reads the names.
                        Token::And => in_pattern = false,
                        Token::Failure if plan_b_head => declaring = Some("failure"),
                        // `else when …` continues the block the `else` reopened,
                        // and one `fi` closes the whole chain, so this `when`
                        // opens no scope of its own. A `when` on the line after
                        // an `else` is a statement inside it, which does.
                        Token::When if tokens.before(p) == Some(&Token::Else) => {}
                        Token::When => self.open(ScopeKind::When),
                        Token::Loop | Token::Block => self.open(ScopeKind::Other),
                        Token::Each => {
                            self.open(ScopeKind::Other);
                            declaring = Some("each");
                        }
                        Token::Match => self.open(ScopeKind::Match),
                        // `(hold n: int`: a parameter the task changes, which
                        // the header declares below, not a variable.
                        // (and `bind`, `hold&`, `bind&`).
                        Token::Hold | Token::Bind if in_task_header && matches!(tokens.before(p), Some(Token::LParen | Token::Comma)) => {}
                        Token::Bind | Token::Hold => {
                            // `hold& y = &x` and `bind& y = &x`: references.
                            let reference = tokens.token(p + 1) == Some(&Token::Amp);
                            let intro = match (*keyword == Token::Bind, reference) {
                                (true, false) => "bind",
                                (true, true) => "bind&",
                                (false, false) => "hold",
                                (false, true) => "hold&",
                            };
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
                        Token::Include => in_include = true,
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
        // `alters step()`: the word before a method that changes its value.
        if name == ALTERS && self.declared_at.get(&(p + 1)).and_then(|&id| self.symbols.get(id)).is_some_and(|s| s.kind == SymbolKind::Method) {
            return (TokenClass::Keyword, None);
        }
        // `read path: text`: the word before a field only its shape's methods
        // assign.
        if name == READ && self.declared_at.get(&(p + 1)).and_then(|&id| self.symbols.get(id)).is_some_and(|s| s.kind == SymbolKind::Field && s.read) {
            return (TokenClass::Keyword, None);
        }
        // `native task …`: a task of Vela's library whose body is C.
        if name == NATIVE && after == Some(&Token::Task) {
            return (TokenClass::Keyword, None);
        }
        // `region r default` and `r: region default`: the word that makes a
        // region the default of a block or of a task's body.
        if name == DEFAULT {
            let two_back = p.checked_sub(2).and_then(|q| tokens.token(q));
            let after_declaration = tokens.ident(p.wrapping_sub(1)).is_some() && two_back == Some(&Token::Region);
            if before == Some(&Token::Region) || after_declaration {
                return (TokenClass::Keyword, None);
            }
        }
        // `.this`, `.x`, and `.m()`: the value a method is called on, and a
        // member of it, which its shape or kind declares.
        if before == Some(&Token::Dot) && !p.checked_sub(2).and_then(|q| tokens.token(q)).is_some_and(ends_value) {
            if name == THIS_WORD {
                return (TokenClass::Keyword, None);
            }
            let at = tokens.range(p).start;
            let owner = self.symbols.iter().position(|s| {
                s.module.is_none() && matches!(s.kind, SymbolKind::Shape | SymbolKind::Kind) && s.range.start <= at && at < s.range.end
            });
            let member = owner.and_then(|owner| {
                self.symbols.iter().position(|s| s.parent == Some(owner) && s.name == name && matches!(s.kind, SymbolKind::Field | SymbolKind::Method))
            });
            return match member.and_then(|id| self.symbols.get(id).map(|s| (id, s.kind))) {
                Some((id, kind)) => (kind.class(), Some(id)),
                None if after == Some(&Token::LParen) => (TokenClass::Method, None),
                None => (TokenClass::Field, None),
            };
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
                // A pattern names a kind after the module's name.
                None if in_type || in_pattern => (TokenClass::Type, None),
                None => (TokenClass::Variable, None),
            };
        }
        // `Point.new(…)` and `Light.start()`: a shared method, called on the
        // name of its shape or kind (a kind's variant is made the same way).
        if before == Some(&Token::Dot)
            && after == Some(&Token::LParen)
            && let Some(owner) = self.type_before()
        {
            let method = self.symbols.iter().position(|s| s.kind == SymbolKind::Method && s.shared && s.parent == Some(owner) && s.name == name);
            let is_shape = self.symbols.get(owner).is_some_and(|s| s.kind == SymbolKind::Shape);
            if method.is_some() || is_shape {
                return (TokenClass::Method, method);
            }
        }
        // `Shape.Circle`: a kind's variant, after the kind's name.
        if before == Some(&Token::Dot)
            && let Some(kind) = self.kind_before()
        {
            let variant = self.symbols.iter().position(|s| s.kind == SymbolKind::Variant && s.parent == Some(kind) && unqualified(&s.name) == name);
            return (TokenClass::Variant, variant);
        }
        // In a pattern, a name after `.` is a variant even when its kind isn't
        // known (a module not read yet, or a misspelled kind): not a new variable.
        if in_pattern && before == Some(&Token::Dot) {
            return (TokenClass::Variant, None);
        }
        // `case Shape.Circle`: the kind's name, before its variant.
        if in_pattern && after == Some(&Token::Dot) && matches!(before, Some(Token::Case | Token::Is)) {
            return (TokenClass::Type, self.global(name, &[SymbolKind::Kind]));
        }
        // A name being declared is one even where a type would be written,
        // as a result's name is (`-> total: int`).
        // `is hold (x, y)`: the pattern declares the names, `hold` only says they
        // can be assigned, so they take their types from the value matched.
        if let Some(intro) = declaring.filter(|intro| !(in_pattern && *intro == "hold")) {
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
            return (TokenClass::Type, self.global(name, &[SymbolKind::Shape, SymbolKind::Kind, SymbolKind::TypeParam]));
        }
        // `hold n: int`, `hold& n: int`, `bind n: int`, and `bind& n: int`:
        // the word, and its `&`, before a parameter's name.
        let opens_param = |q: usize| matches!(tokens.before(q), Some(Token::LParen | Token::Comma));
        let word_at = |q: usize| match tokens.token(q) {
            Some(Token::Hold) if opens_param(q) => Some("hold"),
            Some(Token::Bind) if opens_param(q) => Some("bind"),
            _ => None,
        };
        let marked = match before {
            Some(Token::Hold | Token::Bind) => word_at(p - 1).map(|word| word.to_string()),
            Some(Token::Amp) if p >= 2 => word_at(p - 2).map(|word| format!("{}&", word)),
            _ => None,
        };
        if in_task_header && after == Some(&Token::Colon) && (marked.is_some() || matches!(before, Some(Token::LParen | Token::Comma))) {
            let detail = tokens.text_after(p + 1, ends_field);
            // `r: region`, and `r: region default`, which also makes it the
            // default region of the task's body (see `ast::DEFAULT`).
            let takes_region = detail == "region" || detail == format!("region {}", DEFAULT);
            let kind = if takes_region { SymbolKind::Region } else { SymbolKind::Parameter };
            let intro = match marked.as_deref() {
                Some("hold&") => Some("hold&"),
                Some("hold") => Some("hold"),
                Some("bind&") => Some("bind&"),
                Some("bind") => Some("bind"),
                _ => None,
            };
            return (kind.class(), Some(self.declare_local(p, kind, intro, detail)));
        }
        if in_pattern {
            if matches!(before, Some(Token::Case | Token::Is)) {
                return (TokenClass::Variant, self.global(name, &[SymbolKind::Variant]));
            }
            let intro = if self.scopes.last().is_some_and(|s| s.kind == ScopeKind::Case) { "case" } else { "is" };
            return (TokenClass::Variable, Some(self.declare_local(p, SymbolKind::Variable, Some(intro), String::new())));
        }
        // `ParseError.TooLarge`: a variant of a kind no file declares, written
        // through its kind. A field of that name belongs to a shape, so only the
        // kind's own name before the dot makes this one.
        if before == Some(&Token::Dot)
            && let Some(kind) = vela::ast::builtin_kind_of(name)
            && p.checked_sub(2).and_then(|owner| tokens.ident(owner)) == Some(kind)
        {
            return (TokenClass::Variant, None);
        }
        // `p.x` and `p.m()`: a field or a method of the value's shape or kind.
        if before == Some(&Token::Dot) {
            let start = tokens.get(p).map(|t| (t.start.line, t.start.col));
            let owner = start.and_then(|at| self.field_accesses.get(&at)).cloned();
            let kinds: &[SymbolKind] = if after == Some(&Token::LParen) { &[SymbolKind::Method] } else { &[SymbolKind::Field] };
            return match self.member(owner.as_deref(), name, kinds) {
                Some(id) => (self.symbols.get(id).map_or(TokenClass::Field, |s| s.kind.class()), Some(id)),
                None => (TokenClass::Field, None),
            };
        }
        if let Some(Brace::Literal(Some(shape))) = braces.last()
            && after == Some(&Token::Colon)
            && matches!(before, Some(Token::LBrace | Token::Comma))
        {
            let shape = shape.clone();
            return (TokenClass::Field, self.member(Some(&shape), name, &[SymbolKind::Field]));
        }
        let literal_follows = after == Some(&Token::LBrace)
            && (tokens.token(p + 2) == Some(&Token::RBrace)
                || (tokens.ident(p + 2).is_some() && tokens.token(p + 3) == Some(&Token::Colon)));
        if literal_follows && self.local(name, tokens.range(p).start).is_none() {
            return (TokenClass::Type, self.global(name, &[SymbolKind::Shape, SymbolKind::Kind]));
        }
        // A call always names a task or a variant, even when a variable has the
        // same name, and `show(` and `scan(` always the builtin tasks.
        if after == Some(&Token::LParen) && BUILTIN_TASKS.contains(&name) {
            return (TokenClass::BuiltinFn, None);
        }
        // `text(n) at r`, `int(2.9)`, and `float(n)`: the type, turning a value
        // into it.
        if after == Some(&Token::LParen) && (name == vela::ast::TEXT_CAST || vela::ast::number_cast(name).is_some()) {
            return (TokenClass::TypeBuiltin, None);
        }
        // `scan("Age: ", int)`: the type of the number it reads, which is a type
        // and not a value, as the one a cast names is. Another builtin type there
        // is one this form cannot read, which the checker reports; it is still
        // written as a type.
        if after == Some(&Token::RParen) && before == Some(&Token::Comma) && Type::builtin(name).is_some() && reads_a_number(tokens, p) {
            return (TokenClass::TypeBuiltin, None);
        }
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
        // The kinds no file declares, where a value of one is written rather than
        // its type: `fail ParseError.TooLarge`, or the bare `fail Ended`. No
        // declaration takes these names, and a variable or field that does was
        // found above, so what is left is the kind itself (see `hover`).
        if vela::ast::BUILTIN_KIND_NAMES.contains(&name) {
            let class = if vela::ast::builtin_kind_of(name).is_some() { TokenClass::Variant } else { TokenClass::Type };
            return (class, None);
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
        // A name a `case` pattern binds is usable as soon as it is written, since
        // the case may give its value on the same line: `case Ok v = v * 2`.
        if intro == Some("case") {
            let visible = range.end..tokens.source.len();
            self.symbols.push(Symbol { name, kind, name_range: range.clone(), range, start, visible, intro, detail, parent: None, default: false, read: false, shared: false, module: None, private: false });
            let id = self.symbols.len() - 1;
            if let Some(scope) = self.scopes.last_mut() {
                scope.variables.push(id);
            }
            return id;
        }
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
        // `m is {x} and x > 0`: a name an `is` binds is seen from the `and`
        // after its pattern, which no pattern holds.
        let joined = match intro {
            Some("is") => (p..tokens.len()).take_while(|&q| !matches!(tokens.token(q), Some(Token::Newline | Token::Eof) | None)).find(|&q| tokens.token(q) == Some(&Token::And)),
            _ => None,
        };
        let from = joined.map_or(line_end, |and| tokens.range(and).end);
        let visible = from..tokens.source.len();
        self.symbols.push(Symbol { name, kind, name_range: range.clone(), range, start, visible, intro, detail, parent: None, default: false, read: false, shared: false, module: None, private: false });
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
    /// this file's, or one `include` brings in by name.
    fn global(&self, name: &str, kinds: &[SymbolKind]) -> Option<usize> {
        let own = self.globals.get(name).copied().filter(|&id| self.symbols.get(id).is_some_and(|s| s.module.is_none()));
        own.or_else(|| self.imports.get(name).copied())
            .filter(|&id| self.symbols.get(id).is_some_and(|s| kinds.contains(&s.kind)))
    }

    /// The shape or kind named just before the `.` last classified, as in
    /// `Point.` or `Light.`.
    fn type_before(&self) -> Option<usize> {
        let mut classified = self.out.iter().rev().filter(|token| token.class != TokenClass::Comment);
        let dot = classified.next()?;
        let named = classified.next()?;
        let text = self.tokens.source.get(dot.range.clone())?;
        let id = named.symbol.filter(|_| text == ".")?;
        self.symbols.get(id).is_some_and(|s| matches!(s.kind, SymbolKind::Shape | SymbolKind::Kind)).then_some(id)
    }

    /// The kind named just before the `.` last classified, as in `Shape.`.
    fn kind_before(&self) -> Option<usize> {
        let mut classified = self.out.iter().rev().filter(|token| token.class != TokenClass::Comment);
        let dot = classified.next()?;
        let named = classified.next()?;
        let text = self.tokens.source.get(dot.range.clone())?;
        let id = named.symbol.filter(|_| text == ".")?;
        self.symbols.get(id).is_some_and(|s| s.kind == SymbolKind::Kind).then_some(id)
    }

    /// The module the file includes under `name`, unless a variable at byte `at`
    /// has that name.
    fn module_named(&self, name: &str, at: usize) -> Option<usize> {
        self.global(name, &[SymbolKind::Module]).filter(|_| self.local(name, at).is_none())
    }

    /// A shape's field, or a shape's or a kind's method, called `name`, as
    /// `kinds` allows. Without a known owner, a name that only one shape or
    /// kind declares still resolves.
    fn member(&self, owner: Option<&str>, name: &str, kinds: &[SymbolKind]) -> Option<usize> {
        let found = |owned: &dyn Fn(&Symbol) -> bool| {
            let mut candidates = self.symbols.iter().enumerate().filter(|(_, s)| {
                kinds.contains(&s.kind)
                    && s.name == name
                    && s.parent
                        .and_then(|parent| self.symbols.get(parent))
                        .is_some_and(|parent| matches!(parent.kind, SymbolKind::Shape | SymbolKind::Kind) && owned(parent))
            });
            let first = candidates.next().map(|(id, _)| id);
            // With no owner known, a name two shapes have is neither's.
            if owner.is_none() && candidates.next().is_some() {
                return None;
            }
            first
        };
        let exact = found(&|parent: &Symbol| owner.is_none_or(|owner| parent.name == owner));
        if exact.is_some() || owner.is_none() {
            return exact;
        }
        // A shape of a module this file names and also brings in by name
        // (`include m => d (File)`) is `d.File` among the symbols, and `File`
        // as the compiler spells it: the one such shape, if only one is.
        let suffix = format!("{}{}", MODULE_SEPARATOR, owner.unwrap_or_default());
        let mut loose = self.symbols.iter().enumerate().filter(|(_, s)| {
            kinds.contains(&s.kind)
                && s.name == name
                && s.parent
                    .and_then(|parent| self.symbols.get(parent))
                    .is_some_and(|parent| parent.module.is_some() && matches!(parent.kind, SymbolKind::Shape | SymbolKind::Kind) && parent.name.ends_with(&suffix))
        });
        let first = loose.next().map(|(id, _)| id);
        if loose.next().is_some() { None } else { first }
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
        // A char literal has one escape text doesn't: `'\''`.
        let quoted = text.starts_with('\'');
        while let Some((i, c)) = chars.next() {
            if c != '\\' {
                continue;
            }
            let start = range.start + i;
            let next = chars.next();
            let end = next.map_or(range.end, |(j, n)| range.start + j + n.len_utf8());
            let valid = next.is_some_and(|(_, n)| n != '\n' && (ESCAPES.iter().any(|(escape, _)| *escape == n) || (quoted && n == '\'')));
            self.push(piece..start, TokenClass::String, None);
            self.push(start..end, if valid { TokenClass::Escape } else { TokenClass::Invalid }, None);
            piece = end;
        }
        self.push(piece..range.end, TokenClass::String, None);
    }
}
