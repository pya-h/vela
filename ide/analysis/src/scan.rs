//! Token classification and scope resolution from the lexer stream, resilient to incomplete syntax.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::Path;
use std::rc::Rc;

use serde::Serialize;
use vela::ast::{
    ALTERS, BUILTIN_TASKS, DEFAULT, MODULE_SEPARATOR, NATIVE, NEW_WORD, PATTERN, READ, Span,
    THIS_WORD, Type, unqualified,
};
use vela::lexer::{ESCAPES, LexKind, LexToken, Piece, Token, highlight};

use crate::types::{Context, slashed};

/// Synthetic `intro` marker for loop labels (`outer => ...`), which are not value bindings.
pub(crate) const LOOP_NAME: &str = "=>";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TokenClass {
    Keyword,
    TypeBuiltin,
    Type,
    Variant,
    Task,
    Method,
    BuiltinFn,
    Parameter,
    Variable,
    Region,
    Field,
    Number,
    String,
    Escape,
    Comment,
    Operator,
    Punctuation,
    Invalid,
    Module,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SymbolKind {
    Task,
    Shape,
    Kind,
    Alias,
    Variant,
    Method,
    Field,
    Parameter,
    Variable,
    Region,
    Module,
    TypeParam,
    Macro,
    Pattern,
}

impl SymbolKind {
    fn class(self) -> TokenClass {
        match self {
            SymbolKind::Task => TokenClass::Task,
            SymbolKind::Shape | SymbolKind::Kind | SymbolKind::Alias | SymbolKind::Pattern => {
                TokenClass::Type
            }
            SymbolKind::Variant => TokenClass::Variant,
            SymbolKind::Method => TokenClass::Method,
            SymbolKind::Field => TokenClass::Field,
            SymbolKind::Parameter => TokenClass::Parameter,
            SymbolKind::Variable => TokenClass::Variable,
            SymbolKind::Region => TokenClass::Region,
            SymbolKind::Module => TokenClass::Module,
            SymbolKind::TypeParam => TokenClass::Type,
            // Constants highlight like enum variants.
            SymbolKind::Macro => TokenClass::Variant,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub name_range: Range<usize>,
    pub start: Span,
    pub visible: Range<usize>,
    /// Declaring keyword (`fixed`, `flex`, `case`, `is`, `failure`), `alters`, `@`, or `LOOP_NAME`.
    pub intro: Option<&'static str>,
    pub range: Range<usize>,
    pub detail: String,
    pub parent: Option<usize>,
    /// Ranges belong to the referenced module in `Scan::modules`.
    pub module: Option<usize>,
    pub private: bool,
    pub default: bool,
    pub read: bool,
    pub shared: bool,
    /// Imitated kinds and followed patterns listed after `::`, unqualified as written in their module.
    pub bases: Vec<String>,
}

fn ends_header(token: &Token) -> bool {
    matches!(token, Token::Comma)
}

fn imitated_names(tokens: &Tokens, p: usize) -> Vec<String> {
    let mut q = p;
    if tokens.token(q) == Some(&Token::Lt) {
        while !matches!(
            tokens.token(q),
            None | Some(Token::Gt | Token::Newline | Token::Eof)
        ) {
            q += 1;
        }
        q += 1;
    }
    if tokens.token(q) != Some(&Token::Colon) || tokens.token(q + 1) != Some(&Token::Colon) {
        return Vec::new();
    }
    q += 2;
    let mut names = Vec::new();
    let mut depth = 0usize;
    let mut name = String::new();
    while let Some(token) = tokens.token(q) {
        match token {
            Token::Newline | Token::Eof => break,
            Token::Lt => depth += 1,
            Token::Gt => depth = depth.saturating_sub(1),
            Token::Ident(word) if depth == 0 => name.push_str(word),
            Token::Dot if depth == 0 => name.push(MODULE_SEPARATOR),
            Token::Amp if depth == 0 => names.push(std::mem::take(&mut name)),
            _ => {}
        }
        q += 1;
    }
    names.push(name);
    names.retain(|name| !name.is_empty());
    names
}

pub(crate) fn kind_variants(
    symbols: &[Symbol],
    kind: usize,
    global: &dyn Fn(&str) -> Option<usize>,
) -> Vec<usize> {
    let mut variants = Vec::new();
    let mut through = Vec::new();
    collect_variants(symbols, kind, global, &mut variants, &mut through);
    variants
}

fn collect_variants(
    symbols: &[Symbol],
    kind: usize,
    global: &dyn Fn(&str) -> Option<usize>,
    variants: &mut Vec<usize>,
    through: &mut Vec<usize>,
) {
    // Stop cycles in self-imitating kinds for the compiler to diagnose.
    if through.contains(&kind) {
        return;
    }
    through.push(kind);
    for base in bases_of(symbols, kind, SymbolKind::Kind, global) {
        collect_variants(symbols, base, global, variants, through);
    }
    variants.extend(
        symbols
            .iter()
            .enumerate()
            .filter(|(_, s)| s.kind == SymbolKind::Variant && s.parent == Some(kind))
            .map(|(id, _)| id),
    );
}

fn bases_of(
    symbols: &[Symbol],
    owner: usize,
    kind: SymbolKind,
    global: &dyn Fn(&str) -> Option<usize>,
) -> Vec<usize> {
    let Some(symbol) = symbols.get(owner) else {
        return Vec::new();
    };
    symbol
        .bases
        .iter()
        .filter_map(|base| {
            let written = match (symbol.module, symbol.name.rsplit_once(MODULE_SEPARATOR)) {
                (Some(_), Some((module, _))) if !base.contains(MODULE_SEPARATOR) => {
                    format!("{}{}{}", module, MODULE_SEPARATOR, base)
                }
                _ => base.clone(),
            };
            symbols
                .iter()
                .position(|s| s.kind == kind && s.name == written)
                .filter(|_| symbol.module.is_some())
                .or_else(|| global(&written))
                .filter(|&id| symbols.get(id).is_some_and(|s| s.kind == kind))
        })
        .collect()
}

pub(crate) fn followed_methods(
    symbols: &[Symbol],
    owner: usize,
    global: &dyn Fn(&str) -> Option<usize>,
) -> Vec<usize> {
    let own: Vec<&str> = symbols
        .iter()
        .filter(|s| s.parent == Some(owner) && s.kind == SymbolKind::Method)
        .map(|s| s.name.as_str())
        .collect();
    let mut methods: Vec<usize> = Vec::new();
    for pattern in bases_of(symbols, owner, SymbolKind::Pattern, global) {
        for (id, method) in symbols
            .iter()
            .enumerate()
            .filter(|(_, s)| s.parent == Some(pattern) && s.kind == SymbolKind::Method)
        {
            if !own.contains(&method.name.as_str())
                && !methods
                    .iter()
                    .any(|&seen| symbols.get(seen).is_some_and(|s| s.name == method.name))
            {
                methods.push(id);
            }
        }
    }
    methods
}

#[derive(Debug, Clone)]
pub(crate) struct Module {
    pub path: String,
    pub source: Option<String>,
    /// Resolves to the local alias symbol, or path-qualified for brace-only imports not addressable in code.
    pub symbol: usize,
    pub named: bool,
    pub library: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct Classified {
    pub range: Range<usize>,
    pub class: TokenClass,
    pub symbol: Option<usize>,
}

pub(crate) struct Scan {
    pub lexed: Vec<LexToken>,
    pub symbols: Vec<Symbol>,
    pub tokens: Vec<Classified>,
    pub modules: Vec<Module>,
    pub imports: HashMap<String, usize>,
    pub lambdas: HashSet<usize>,
}

pub(crate) fn scan(
    source: &str,
    field_accesses: &HashMap<(usize, usize), String>,
    context: Context,
) -> Scan {
    let lexed = spread(highlight(source));
    let tokens = Tokens::new(source, &lexed);
    let lambdas = (0..tokens.len())
        .filter(|&p| tokens.token(p) == Some(&Token::LParen) && tokens.starts_lambda(p))
        .filter_map(|p| tokens.get(p).map(|t| t.range.start))
        .collect();
    let (mut symbols, mut declared_at, includes) = declarations(&tokens);
    let (modules, imports) = included_modules(&mut symbols, &mut declared_at, &includes, context);
    let classified = Classifier::new(
        &tokens,
        symbols,
        declared_at,
        field_accesses,
        imports.clone(),
    )
    .run();
    let (symbols, tokens) = classified;
    Scan {
        lexed,
        symbols,
        tokens,
        modules,
        imports,
        lambdas,
    }
}

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
    let text = |start, end, range| LexToken {
        kind: LexKind::Token(Token::Text(String::new())),
        start,
        end,
        range,
    };
    let (mut start, mut from) = (token.start, token.range.start);
    for piece in pieces {
        let Piece::Value {
            open,
            tokens,
            close,
        } = piece
        else {
            continue;
        };
        out.push(text(start, open.end, from..open.range.end));
        for inner in tokens {
            spread_into(inner, out);
        }
        (start, from) = (close.start, close.range.start);
    }
    out.push(text(start, token.end, from..token.range.end));
}

thread_local! {
    /// Caches scanned module declarations across keystrokes.
    static DECLARED: RefCell<HashMap<String, Declared>> = RefCell::new(HashMap::new());
}

struct Declared {
    text: String,
    symbols: Rc<Vec<Symbol>>,
}

fn module_declarations(path: &str, source: &str) -> Rc<Vec<Symbol>> {
    let kept = DECLARED.with_borrow(|declared| {
        declared
            .get(path)
            .filter(|kept| kept.text == source)
            .map(|kept| Rc::clone(&kept.symbols))
    });
    if let Some(symbols) = kept {
        return symbols;
    }
    let lexed = spread(highlight(source));
    let tokens = Tokens::new(source, &lexed);
    let symbols = Rc::new(declarations(&tokens).0);
    DECLARED.with_borrow_mut(|declared| {
        declared.insert(
            path.to_string(),
            Declared {
                text: source.to_string(),
                symbols: Rc::clone(&symbols),
            },
        )
    });
    symbols
}

struct IncludeLine {
    parts: Vec<String>,
    last: usize,
    symbol: usize,
    names: Vec<IncludedName>,
    only_parts: bool,
}

struct IncludedName {
    name: String,
    at: usize,
    alias: Option<(String, usize)>,
}

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
        let library = line
            .parts
            .split_first()
            .filter(|(first, _)| *first == vela::library::ROOT)
            .map(|(_, rest)| rest.join("/"));
        let path = match &library {
            Some(name) => format!("{}/{}.vela", vela::library::ROOT, name),
            None => slashed(&file),
        };
        let source = match &library {
            Some(name) => vela::library::module(name).map(str::to_string),
            None => context.sources.get(&path).cloned(),
        };
        // Brace-only imports have no module identifier in scope, so key them by path.
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
            symbols
                .get(line.symbol)
                .map(|s| s.name.clone())
                .unwrap_or_default()
        };
        let mut declared: HashMap<String, usize> = HashMap::new();
        if let Some(source) = &source {
            let theirs = module_declarations(&path, source);
            let listed = |written: &str| line.names.iter().find(|n| n.name == written);
            let written_as = |written: &str| match listed(written) {
                Some(IncludedName {
                    alias: Some((alias, _)),
                    ..
                }) => alias.clone(),
                Some(_) => written.to_string(),
                None => format!("{}{}{}", name, MODULE_SEPARATOR, written),
            };
            let types: HashMap<String, String> = theirs
                .iter()
                .filter(|s| {
                    matches!(
                        s.kind,
                        SymbolKind::Shape | SymbolKind::Kind | SymbolKind::Alias
                    )
                })
                .map(|s| (s.name.clone(), written_as(&s.name)))
                .collect();
            // Skip the module's own `include` items when reindexing imported symbols.
            let kept: Vec<usize> = (0..theirs.len())
                .filter(|&i| theirs.get(i).is_some_and(|s| s.kind != SymbolKind::Module))
                .collect();
            let base = symbols.len();
            for &i in &kept {
                let Some(symbol) = theirs.get(i) else {
                    continue;
                };
                let mut symbol = symbol.clone();
                if matches!(
                    symbol.kind,
                    SymbolKind::Task
                        | SymbolKind::Shape
                        | SymbolKind::Kind
                        | SymbolKind::Alias
                        | SymbolKind::Pattern
                        | SymbolKind::Variant
                        | SymbolKind::Macro
                ) {
                    declared.entry(symbol.name.clone()).or_insert(symbols.len());
                    // Only whole declarations take an `=>` alias; variants keep their own name.
                    let kind = symbol
                        .parent
                        .and_then(|parent| theirs.get(parent))
                        .map(|kind| kind.name.as_str());
                    symbol.name = match symbol.kind {
                        SymbolKind::Variant
                            if line.only_parts && listed(&symbol.name).is_some() =>
                        {
                            symbol.name.clone()
                        }
                        SymbolKind::Variant
                            if line.only_parts
                                && kind.is_some_and(|kind| listed(kind).is_some()) =>
                        {
                            format!(
                                "{}{}{}",
                                written_as(kind.unwrap_or_default()),
                                MODULE_SEPARATOR,
                                symbol.name
                            )
                        }
                        SymbolKind::Variant => {
                            format!("{}{}{}", name, MODULE_SEPARATOR, symbol.name)
                        }
                        _ if listed(&symbol.name)
                            .is_some_and(|n| n.alias.is_some() || line.only_parts) =>
                        {
                            written_as(&symbol.name)
                        }
                        _ => format!("{}{}{}", name, MODULE_SEPARATOR, symbol.name),
                    };
                }
                symbol.detail = qualify_types(&symbol.detail, &types);
                symbol.parent = symbol
                    .parent
                    .and_then(|parent| kept.iter().position(|&k| k == parent))
                    .map(|p| base + p);
                symbol.module = Some(index);
                symbols.push(symbol);
            }
        }
        for written in &line.names {
            let found = declared.get(&written.name).copied().filter(|&id| {
                symbols.get(id).is_some_and(|s| {
                    !s.private
                        && matches!(
                            s.kind,
                            SymbolKind::Task
                                | SymbolKind::Shape
                                | SymbolKind::Kind
                                | SymbolKind::Alias
                                | SymbolKind::Pattern
                                | SymbolKind::Variant
                                | SymbolKind::Macro
                        )
                })
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
        modules.push(Module {
            path,
            source,
            symbol: line.symbol,
            named: !line.only_parts,
            library: library.is_some(),
        });
    }
    (modules, imports)
}

fn qualify_types(text: &str, types: &HashMap<String, String>) -> String {
    let lexed = highlight(text);
    let mut out = String::new();
    let mut at = 0;
    let significant: Vec<&LexToken> = lexed
        .iter()
        .filter(|t| t.kind != LexKind::Comment)
        .collect();
    for (i, token) in significant.iter().enumerate() {
        let LexKind::Token(Token::Ident(name)) = &token.kind else {
            continue;
        };
        let before = i
            .checked_sub(1)
            .and_then(|b| significant.get(b))
            .map(|t| &t.kind);
        let after = significant.get(i + 1).map(|t| &t.kind);
        let written = types.get(name).filter(|_| {
            !matches!(before, Some(LexKind::Token(Token::Dot | Token::AtSign)))
                && !matches!(after, Some(LexKind::Token(Token::Colon)))
        });
        if let Some(written) = written {
            out.push_str(text.get(at..token.range.start).unwrap_or_default());
            out.push_str(written);
            at = token.range.end;
        }
    }
    out.push_str(text.get(at..).unwrap_or_default());
    out
}

struct Tokens<'a> {
    source: &'a str,
    all: &'a [LexToken],
    significant: Vec<usize>,
}

fn named_key(tokens: &Tokens, p: usize) -> bool {
    if tokens.token(p + 1) != Some(&Token::Colon)
        || !matches!(tokens.before(p), Some(Token::LBracket | Token::Comma))
    {
        return false;
    }
    let mut depth = 0usize;
    let mut at = p;
    let open = loop {
        let Some(before) = at.checked_sub(1) else {
            return false;
        };
        at = before;
        match tokens.token(at) {
            Some(Token::RBracket | Token::RParen | Token::RBrace) => depth += 1,
            Some(Token::LParen | Token::LBrace) if depth == 0 => return false,
            Some(Token::LBracket) if depth == 0 => break at,
            Some(Token::LBracket | Token::LParen | Token::LBrace) => depth -= 1,
            Some(Token::Newline) | None => return false,
            _ => {}
        }
    };
    let (Some(paren), Some(close)) = (open.checked_sub(1), open.checked_sub(2)) else {
        return false;
    };
    if tokens.token(paren) != Some(&Token::LParen) || tokens.token(close) != Some(&Token::RBracket)
    {
        return false;
    }
    let mut depth = 0usize;
    let mut at = close;
    loop {
        let Some(before) = at.checked_sub(1) else {
            return false;
        };
        at = before;
        match tokens.token(at) {
            Some(Token::RBracket) => depth += 1,
            Some(Token::LBracket) if depth == 0 => {
                return tokens.token(at + 1) == Some(&Token::Colon);
            }
            Some(Token::LBracket) => depth -= 1,
            Some(Token::Newline) | None => return false,
            _ => {}
        }
    }
}

impl vela::parser::LineTokens for Tokens<'_> {
    fn token_at(&self, p: usize) -> Option<&Token> {
        self.token(p)
    }

    fn gt_touches_next(&self, p: usize) -> bool {
        self.range(p).end == self.range(p + 1).start
    }
}

impl<'a> Tokens<'a> {
    fn starts_loop_item(&self, first: usize, p: usize) -> bool {
        let last = (p..)
            .find(|&q| matches!(self.token(q), Some(Token::Parts | Token::Newline) | None))
            .unwrap_or(p);
        vela::parser::loop_items(self, first, last)
            .iter()
            .any(|&(start, _)| start == p)
    }

    fn new(source: &'a str, all: &'a [LexToken]) -> Self {
        let significant = all
            .iter()
            .enumerate()
            .filter(|(_, t)| t.kind != LexKind::Comment)
            .map(|(i, _)| i)
            .collect();
        Tokens {
            source,
            all,
            significant,
        }
    }

    fn len(&self) -> usize {
        self.significant.len()
    }

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
        self.get(p)
            .map(|t| t.range.clone())
            .unwrap_or(self.source.len()..self.source.len())
    }

    fn opens_lines(&self, p: usize) -> bool {
        let names_error = self.token(p + 1) == Some(&Token::Failure)
            && matches!(self.token(p + 2), Some(Token::Ident(_) | Token::Underscore));
        let end = if names_error { p + 3 } else { p + 1 };
        matches!(self.token(end), Some(Token::Newline | Token::Eof) | None)
    }

    fn ends_in_a_list(&self, p: usize) -> bool {
        let mut depth = 0usize;
        let mut q = p;
        loop {
            match self.token(q) {
                Some(Token::Eof) | None => return false,
                Some(Token::LParen | Token::LBracket | Token::Lt) => depth += 1,
                Some(Token::RParen | Token::RBracket | Token::Gt) if depth > 0 => depth -= 1,
                Some(
                    Token::Ident(_)
                    | Token::Dot
                    | Token::Caret
                    | Token::Maybe
                    | Token::AtSign
                    | Token::Tilde
                    | Token::Hash
                    | Token::Int(_)
                    | Token::Ellipsis
                    | Token::Arrow
                    | Token::Region,
                ) => {}
                Some(Token::Comma) if depth > 0 => {}
                Some(
                    Token::Comma | Token::RParen | Token::RBracket | Token::Gt | Token::Assign,
                ) => return true,
                _ if depth > 0 => {}
                _ => return false,
            }
            q += 1;
        }
    }

    fn after_kept(&self, p: usize) -> bool {
        self.kept_open(p).is_some()
    }

    fn kept_open(&self, p: usize) -> Option<usize> {
        let close = p
            .checked_sub(1)
            .filter(|&close| self.token(close) == Some(&Token::RBracket))?;
        (0..close)
            .rev()
            .find(|&q| !self.kept_part(q))
            .filter(|&open| {
                self.token(open) == Some(&Token::LBracket) && self.kept_list(open) == Some(close)
            })
    }

    fn kept_part(&self, q: usize) -> bool {
        matches!(
            self.token(q),
            Some(
                Token::Ident(_)
                    | Token::AtSign
                    | Token::Flex
                    | Token::Fixed
                    | Token::Bind
                    | Token::Amp
                    | Token::Comma
            )
        )
    }

    fn kept_list(&self, open: usize) -> Option<usize> {
        let close = (open + 1..).find(|&q| !self.kept_part(q))?;
        let region = (open + 1..close).any(|q| self.token(q) == Some(&Token::AtSign));
        (self.token(close) == Some(&Token::RBracket) && region && close > open + 1).then_some(close)
    }

    pub(crate) fn in_kept_list(&self, p: usize) -> bool {
        (0..p)
            .rev()
            .find(|&q| !self.kept_part(q))
            .is_some_and(|open| {
                self.token(open) == Some(&Token::LBracket)
                    && self.kept_list(open).is_some_and(|close| close > p)
            })
    }

    pub(crate) fn shortcut_lambda(&self, p: usize) -> Option<usize> {
        match self.token(p)? {
            Token::Ident(_) | Token::Underscore => {
                (self.token(p + 1) == Some(&Token::Arrow)).then_some(p + 1)
            }
            Token::LParen
                if self.kept_open(p).is_none()
                    && self.before(p).is_some_and(|before| {
                        ends_value(before) || matches!(before, Token::Gt | Token::New)
                    }) =>
            {
                None
            }
            Token::LParen => {
                let close = (p + 1..).find(|&q| {
                    !matches!(
                        self.token(q),
                        Some(Token::Ident(_) | Token::Underscore | Token::Comma)
                    )
                })?;
                if self.token(close) != Some(&Token::RParen)
                    || self.token(close + 1) != Some(&Token::Arrow)
                {
                    return None;
                }
                if close == p + 1 {
                    let line: Vec<&Token> = (close + 2..)
                        .map_while(|q| self.token(q))
                        .take_while(|token| !matches!(token, Token::Newline | Token::Eof))
                        .collect();
                    if vela::parser::empty_lambda_has_results(&line) {
                        return None;
                    }
                }
                Some(close + 1)
            }
            _ => None,
        }
    }

    pub(crate) fn shortcut_body_end(&self, arrow: usize) -> usize {
        let mut depth = 0usize;
        let mut q = arrow + 1;
        loop {
            match self.token(q) {
                Some(Token::LParen | Token::LBracket | Token::LBrace) => depth += 1,
                Some(Token::RParen | Token::RBracket | Token::RBrace) if depth > 0 => depth -= 1,
                Some(Token::Comma | Token::RParen | Token::RBracket | Token::RBrace)
                    if depth == 0 =>
                {
                    break;
                }
                Some(Token::Newline | Token::Eof) | None => break,
                _ => {}
            }
            q += 1;
        }
        self.get(q).map_or(self.source.len(), |t| t.range.start)
    }

    fn starts_lambda(&self, p: usize) -> bool {
        let kept = self.kept_open(p);
        if kept
            .and_then(|open| self.before(open))
            .is_some_and(|before| {
                matches!(
                    before,
                    Token::Colon
                        | Token::Arrow
                        | Token::Maybe
                        | Token::Caret
                        | Token::LBracket
                        | Token::Lt
                        | Token::Tilde
                )
            })
        {
            return false;
        }
        if self.token(p) != Some(&Token::LParen)
            || (kept.is_none()
                && self.before(p).is_some_and(|before| {
                    ends_value(before) || matches!(before, Token::Gt | Token::New)
                }))
        {
            return false;
        }
        // Check empty parens first to avoid scanning nested calls to EOF.
        if self.token(p + 1) == Some(&Token::RParen) {
            return self.token(p + 2) == Some(&Token::Arrow)
                && !matches!(
                    self.before(p),
                    Some(
                        Token::Arrow
                            | Token::Colon
                            | Token::Maybe
                            | Token::Caret
                            | Token::LBracket
                            | Token::Lt
                            | Token::Tilde
                    )
                )
                && !self.ends_in_a_list(p + 3);
        }
        let named = |q: usize| self.ident(q).is_some() && self.token(q + 1) == Some(&Token::Colon);
        let after_word = if self.token(p + 2) == Some(&Token::Amp) {
            p + 3
        } else {
            p + 2
        };
        if !(matches!(
            self.token(p + 1),
            Some(Token::Flex | Token::Fixed | Token::Bind)
        ) && named(after_word))
            && !named(p + 1)
        {
            return false;
        }
        let mut depth = 0usize;
        let mut q = p;
        let close = loop {
            match self.token(q) {
                Some(Token::LParen | Token::LBracket) => depth += 1,
                Some(Token::RParen | Token::RBracket) => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        break q;
                    }
                }
                Some(Token::Newline | Token::Eof) | None => return false,
                _ => {}
            }
            q += 1;
        };
        self.token(close) == Some(&Token::RParen) && self.token(close + 1) == Some(&Token::Arrow)
    }

    fn opens_types(&self, p: usize) -> bool {
        if self.token(p) != Some(&Token::Lt) {
            return false;
        }
        let declares = p >= 2
            && self.ident(p - 1).is_some()
            && (matches!(
                self.token(p - 2),
                Some(Token::Task | Token::Shape | Token::Kind)
            ) || self.ident(p - 2) == Some(vela::ast::PATTERN));
        if declares {
            return true;
        }
        let header = p >= 1
            && self.ident(p - 1).is_some()
            && (self.starts_line(p - 1)
                || (p >= 2 && self.ident(p - 2) == Some(vela::ast::ALTERS)));
        let mut depth = 0usize;
        // Paren-grouped comparisons like `(i < n), m > (k)` do not close outer `<...>`.
        let mut grouped = 0usize;
        for q in p + 1.. {
            match self.token(q) {
                Some(Token::Gt) if grouped > 0 && depth == 0 => return false,
                Some(Token::LParen) => grouped += 1,
                Some(Token::RParen) if grouped == 0 => return false,
                Some(Token::RParen) => grouped -= 1,
                // Generic variant uses require `.` directly after `>` per parser rules.
                Some(Token::Gt) if depth == 0 => {
                    let touching = matches!((self.get(q), self.get(q + 1)), (Some(gt), Some(dot)) if gt.range.end == dot.range.start);
                    return matches!(self.token(q + 1), Some(Token::LParen | Token::LBrace))
                        || (touching
                            && self.token(q + 1) == Some(&Token::Dot)
                            && self.ident(q + 2).is_some());
                }
                Some(Token::Lt) => depth += 1,
                Some(Token::Gt) => depth -= 1,
                Some(Token::Colon | Token::Amp) if header => {}
                Some(token) if token.writes_type() => {}
                _ => return false,
            }
        }
        false
    }

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

    fn starts_line(&self, p: usize) -> bool {
        p == 0 || self.before(p) == Some(&Token::Newline)
    }

    fn text_after(&self, p: usize, stop: fn(&Token) -> bool) -> String {
        let Some(&first) = self.significant.get(p) else {
            return String::new();
        };
        let from = self.range(p).end;
        let mut depth = 0usize;
        let to = self
            .all
            .iter()
            .skip(first + 1)
            .find(|t| match &t.kind {
                LexKind::Token(Token::LParen | Token::LBracket | Token::LBrace | Token::Lt) => {
                    depth += 1;
                    false
                }
                LexKind::Token(Token::RParen | Token::RBracket | Token::RBrace | Token::Gt)
                    if depth > 0 =>
                {
                    depth -= 1;
                    false
                }
                LexKind::Token(token) => {
                    *token == Token::Newline || *token == Token::Eof || (depth == 0 && stop(token))
                }
                LexKind::Comment => true,
                LexKind::Invalid(_) => false,
            })
            .map_or(self.source.len(), |t| t.range.start);
        self.source
            .get(from..to)
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    fn loop_variable(&self, p: usize) -> Option<usize> {
        if self.before(p) != Some(&Token::Assign) {
            return None;
        }
        let mut start = p - 1;
        while start > 0 && !matches!(self.token(start - 1), Some(Token::Newline) | None) {
            start -= 1;
        }
        let name = match self.token(start) {
            Some(Token::Fixed | Token::Flex) => start + 1,
            _ => start,
        };
        self.ident(name)?;
        let typed = self.token(name + 1) == Some(&Token::Colon);
        let assigns = (name + 1..p - 1).any(|q| self.token(q) == Some(&Token::Assign));
        (typed || name + 1 == p - 1)
            .then_some(name)
            .filter(|_| !assigns)
    }

    fn ident(&self, p: usize) -> Option<&'a str> {
        match self.token(p) {
            Some(Token::Ident(name)) => Some(name),
            _ => None,
        }
    }

    fn starts_member(&self, p: usize) -> bool {
        self.starts_line(p) || (self.before(p) == Some(&Token::Priv) && self.starts_line(p - 1))
    }

    fn pattern_body_follows(&self, name: usize) -> bool {
        let mut q = name;
        let mut depth = 0usize;
        loop {
            match self.token(q) {
                None | Some(Token::Eof) => return false,
                Some(Token::Newline) => break,
                Some(Token::LParen) => depth += 1,
                Some(Token::RParen) => depth = depth.saturating_sub(1),
                Some(Token::Comma)
                    if depth == 0
                        && self.ident(q + 1).is_some()
                        && matches!(self.token(q + 2), Some(Token::LParen)) =>
                {
                    return false;
                }
                _ => {}
            }
            q += 1;
        }
        while self.token(q) == Some(&Token::Newline) {
            q += 1;
        }
        match self.token(q) {
            None
            | Some(
                Token::Eof
                | Token::Fi
                | Token::Task
                | Token::Shape
                | Token::Kind
                | Token::Include
                | Token::Priv,
            ) => false,
            Some(Token::Ident(word)) if word == ALTERS && self.ident(q + 1).is_some() => false,
            Some(Token::Ident(_)) => match self.token(q + 1) {
                Some(Token::Colon | Token::Lt) => false,
                Some(Token::LParen) => !self.header_shaped(q),
                _ => true,
            },
            _ => true,
        }
    }

    fn header_shaped(&self, q: usize) -> bool {
        if matches!(
            self.token(q + 2),
            Some(Token::Flex | Token::Bind | Token::Fixed)
        ) || (self.ident(q + 2).is_some() && self.token(q + 3) == Some(&Token::Colon))
        {
            return true;
        }
        let mut depth = 0usize;
        let mut at = q + 1;
        while let Some(token) = self.token(at) {
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
        matches!(self.token(at + 1), Some(Token::Arrow | Token::Comma))
    }

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
                        let ends =
                            matches!(self.token(q + 1), Some(Token::Newline | Token::Eof) | None);
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

pub(crate) fn ends_value(token: &Token) -> bool {
    matches!(
        token,
        Token::Ident(_)
            | Token::Int(_)
            | Token::Float(_)
            | Token::Char(_)
            | Token::Text(_)
            | Token::True
            | Token::False
            | Token::None
            | Token::RParen
            | Token::RBracket
            | Token::RBrace
            | Token::Caret
    )
}

fn reads_a_number(tokens: &Tokens, p: usize) -> bool {
    let mut depth = 0usize;
    for q in (0..p).rev() {
        match tokens.token(q) {
            Some(Token::Newline | Token::Eof) | None => return false,
            Some(Token::RParen) => depth += 1,
            Some(Token::LParen) if depth == 0 => {
                return q.checked_sub(1).is_some_and(|name| {
                    tokens.ident(name) == Some(vela::ast::SCAN)
                        && tokens.before(name) != Some(&Token::Dot)
                });
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
    matches!(token, Token::Comma | Token::RParen | Token::Assign)
}

fn read_marker(tokens: &Tokens, p: usize) -> bool {
    p.checked_sub(1)
        .is_some_and(|q| tokens.ident(q) == Some(READ) && tokens.starts_line(q))
}

fn shared_marker(tokens: &Tokens, p: usize) -> Option<Option<&'static str>> {
    let (at, word) = match p.checked_sub(1).and_then(|q| tokens.token(q)) {
        Some(Token::Shared) => (p - 1, None),
        Some(Token::Flex) => (p.checked_sub(2)?, Some("flex")),
        Some(Token::Bind) => (p.checked_sub(2)?, Some("bind")),
        Some(Token::Ident(word)) if word == READ => (p.checked_sub(2)?, None),
        _ => return None,
    };
    let starts = tokens.starts_line(at)
        || (tokens.before(at) == Some(&Token::Priv) && tokens.starts_line(at - 1));
    (tokens.token(at) == Some(&Token::Shared) && starts).then_some(word)
}

fn ends_shape_field(token: &Token) -> bool {
    matches!(token, Token::Assign)
}

fn ends_type_param(token: &Token) -> bool {
    matches!(token, Token::Comma | Token::Gt)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Block {
    Task,
    Shape,
    Kind,
    Pattern,
    Method,
    Inner,
}

fn declarations(tokens: &Tokens) -> (Vec<Symbol>, HashMap<usize, usize>, Vec<IncludeLine>) {
    let mut symbols: Vec<Symbol> = Vec::new();
    let mut declared_at = HashMap::new();
    let mut includes = Vec::new();
    let mut blocks: Vec<Block> = Vec::new();
    let mut item: Option<usize> = None;
    let mut variant: Option<usize> = None;
    let mut method: Option<usize> = None;
    let mut methods_above = false;
    let mut parens = 0usize;
    let mut header = false;
    let mut last_end = 0;

    let mut declare = |symbols: &mut Vec<Symbol>,
                       p: usize,
                       kind: SymbolKind,
                       detail: String,
                       parent: Option<usize>| {
        let name = tokens.ident(p).unwrap_or_default().to_string();
        let range = tokens.range(p);
        let start = tokens.get(p).map(|t| t.start).unwrap_or_default();
        let visible = 0..tokens.source.len();
        let native = p >= 2 && tokens.ident(p - 2) == Some(NATIVE);
        let private = tokens.before(p) == Some(&Token::Priv)
            || tokens.before(p.saturating_sub(1)) == Some(&Token::Priv)
            || (native && tokens.before(p - 2) == Some(&Token::Priv));
        symbols.push(Symbol {
            name,
            kind,
            name_range: range.clone(),
            range,
            start,
            visible,
            intro: None,
            detail,
            parent,
            module: None,
            private,
            default: false,
            read: false,
            shared: false,
            bases: Vec::new(),
        });
        declared_at.insert(p, symbols.len() - 1);
        symbols.len() - 1
    };

    for p in 0..tokens.len() {
        let Some(token) = tokens.token(p) else {
            last_end = tokens.range(p).end;
            continue;
        };
        match token {
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
                        Token::Ident(name) if in_names => names.push(IncludedName {
                            name: name.clone(),
                            at: q,
                            alias: None,
                        }),
                        Token::Ident(name) => {
                            parts.push(name.clone());
                            last_part = Some(q);
                        }
                        Token::Range if !in_names => parts.push("..".to_string()),
                        _ => after_arrow = false,
                    }
                    q += 1;
                }
                if let Some(last) =
                    last_part.filter(|&last| tokens.token(last + 1) != Some(&Token::Slash))
                {
                    let written = tokens.text_after(p, never);
                    let path = written
                        .split(['(', '{'])
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    let symbol = declare(
                        &mut symbols,
                        alias.unwrap_or(last),
                        SymbolKind::Module,
                        path,
                        None,
                    );
                    includes.push(IncludeLine {
                        parts,
                        last,
                        symbol,
                        names,
                        only_parts,
                    });
                }
            }
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
                header = block == Block::Task;
                blocks.push(block);
                if tokens.ident(p + 1).is_some() {
                    let bases = if block == Block::Task {
                        Vec::new()
                    } else {
                        imitated_names(tokens, p + 2)
                    };
                    let detail = if block == Block::Kind && bases.is_empty() {
                        String::new()
                    } else {
                        tokens.text_after(p + 1, never)
                    };
                    let id = declare(&mut symbols, p + 1, kind, detail, None);
                    if let Some(symbol) = symbols.get_mut(id) {
                        symbol.range.start = tokens.range(p).start;
                        symbol.bases = bases;
                    }
                    item = Some(id);
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
            Token::Hash
                if tokens.ident(p + 1).is_some()
                    && matches!(
                        tokens.token(p + 2),
                        None | Some(Token::Assign | Token::Colon | Token::Newline | Token::Eof)
                    )
                    && matches!(tokens.before(p), None | Some(Token::Newline | Token::Priv))
                    && ((item.is_none() && method.is_none() && blocks.is_empty())
                        || tokens
                            .get(if tokens.before(p) == Some(&Token::Priv) {
                                p - 1
                            } else {
                                p
                            })
                            .is_some_and(|t| t.start.col == 1)) =>
            {
                for open in [method.take(), item.take()].into_iter().flatten() {
                    close(&mut symbols, open, last_end);
                }
                methods_above = false;
                blocks.clear();
                let detail = tokens.text_after(p + 1, never);
                let id = declare(&mut symbols, p + 1, SymbolKind::Macro, detail, None);
                if let Some(symbol) = symbols.get_mut(id) {
                    let first = if tokens.before(p) == Some(&Token::Priv) {
                        p - 1
                    } else {
                        p
                    };
                    let mut last = p + 1;
                    while !matches!(
                        tokens.token(last + 1),
                        None | Some(Token::Newline | Token::Eof)
                    ) {
                        last += 1;
                    }
                    symbol.range = tokens.range(first).start..tokens.range(last).end;
                }
            }
            Token::Ident(word)
                if word == PATTERN
                    && tokens.ident(p + 1).is_some()
                    && matches!(
                        tokens.token(p + 2),
                        None | Some(Token::Newline | Token::Eof | Token::Colon | Token::Lt)
                    )
                    && matches!(tokens.before(p), None | Some(Token::Newline | Token::Priv))
                    && ((item.is_none() && method.is_none() && blocks.is_empty())
                        || tokens
                            .get(if tokens.before(p) == Some(&Token::Priv) {
                                p - 1
                            } else {
                                p
                            })
                            .is_some_and(|t| t.start.col == 1)) =>
            {
                for open in [method.take(), item.take()].into_iter().flatten() {
                    close(&mut symbols, open, last_end);
                }
                methods_above = false;
                blocks.clear();
                blocks.push(Block::Pattern);
                let id = declare(
                    &mut symbols,
                    p + 1,
                    SymbolKind::Pattern,
                    String::new(),
                    None,
                );
                if let Some(symbol) = symbols.get_mut(id) {
                    let private = tokens.before(p) == Some(&Token::Priv);
                    symbol.range.start = tokens.range(if private { p - 1 } else { p }).start;
                    symbol.private = private;
                }
                item = Some(id);
            }
            Token::Ident(_)
                if let (Some(pattern), [Block::Pattern]) = (item, blocks.as_slice())
                    && tokens.starts_line(p)
                    && tokens.token(p + 1) == Some(&Token::Colon) =>
            {
                let detail = tokens.text_after(p + 1, never);
                declare(&mut symbols, p, SymbolKind::Field, detail, Some(pattern));
            }
            Token::Ident(_)
                if let (Some(pattern), [Block::Pattern]) = (item, blocks.as_slice())
                    && parens == 0
                    && (tokens.starts_line(p) || tokens.before(p) == Some(&Token::Comma))
                    && let Some(name) = tokens.method_at(p, false, true) =>
            {
                let detail = tokens.text_after(name, ends_header);
                let id = declare(
                    &mut symbols,
                    name,
                    SymbolKind::Method,
                    detail,
                    Some(pattern),
                );
                let default = tokens.starts_line(p) && tokens.pattern_body_follows(name);
                if let Some(symbol) = symbols.get_mut(id) {
                    symbol.range.start = tokens.range(p).start;
                    symbol.intro = (name != p).then_some(ALTERS);
                    symbol.default = default;
                }
                if default {
                    blocks.push(Block::Method);
                    method = Some(id);
                }
            }
            Token::Ident(word)
                if word == "alias"
                    && tokens.ident(p + 1).is_some()
                    && tokens.token(p + 2) == Some(&Token::FatArrow)
                    && matches!(tokens.before(p), None | Some(Token::Newline | Token::Priv))
                    && ((item.is_none() && method.is_none() && blocks.is_empty())
                        || tokens
                            .get(if tokens.before(p) == Some(&Token::Priv) {
                                p - 1
                            } else {
                                p
                            })
                            .is_some_and(|t| t.start.col == 1)) =>
            {
                for open in [method.take(), item.take()].into_iter().flatten() {
                    close(&mut symbols, open, last_end);
                }
                methods_above = false;
                blocks.clear();
                let detail = tokens.text_after(p + 2, never);
                let id = declare(&mut symbols, p + 1, SymbolKind::Alias, detail, None);
                if let Some(symbol) = symbols.get_mut(id) {
                    let first = if tokens.before(p) == Some(&Token::Priv) {
                        p - 1
                    } else {
                        p
                    };
                    let mut last = p + 1;
                    while !matches!(
                        tokens.token(last + 1),
                        None | Some(Token::Newline | Token::Eof)
                    ) {
                        last += 1;
                    }
                    symbol.range = tokens.range(first).start..tokens.range(last).end;
                }
            }
            Token::When
            | Token::Loop
            | Token::Endless
            | Token::Each
            | Token::Block
            | Token::Match => blocks.push(Block::Inner),
            Token::LParen if !header && tokens.starts_lambda(p) => {
                blocks.push(Block::Inner);
                parens += 1;
            }
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
            Token::Newline => {
                parens = 0;
                header = false;
            }
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
                    symbol.range.start = tokens
                        .range(if tokens.before(p) == Some(&Token::Priv) {
                            p - 1
                        } else {
                            p
                        })
                        .start;
                    symbol.private = tokens.before(p) == Some(&Token::Priv);
                    symbol.shared = true;
                    symbol.intro = Some("shared");
                }
                blocks.push(Block::Method);
                method = Some(id);
                methods_above = true;
                header = true;
            }
            Token::Ident(_)
                if let (Some(owner), [only @ (Block::Shape | Block::Kind)]) =
                    (item, blocks.as_slice())
                    && tokens.starts_member(p)
                    && let Some(name) =
                        tokens.method_at(p, *only == Block::Kind, methods_above) =>
            {
                let detail = tokens.text_after(name, never);
                let id = declare(&mut symbols, name, SymbolKind::Method, detail, Some(owner));
                if let Some(symbol) = symbols.get_mut(id) {
                    symbol.range.start = tokens
                        .range(if tokens.before(p) == Some(&Token::Priv) {
                            p - 1
                        } else {
                            p
                        })
                        .start;
                    symbol.intro = (name != p).then_some(ALTERS);
                }
                blocks.push(Block::Method);
                method = Some(id);
                methods_above = true;
                header = true;
            }
            Token::Ident(_) => match (blocks.as_slice(), item) {
                ([Block::Shape], Some(shape))
                    if (tokens.starts_line(p)
                        || (tokens.before(p) == Some(&Token::Priv)
                            && tokens.starts_line(p - 1))
                        || read_marker(tokens, p)
                        || shared_marker(tokens, p).is_some())
                        && tokens.token(p + 1) == Some(&Token::Colon) =>
                {
                    let whole = tokens.text_after(p + 1, never);
                    let detail = tokens.text_after(p + 1, ends_shape_field);
                    let falls_back = whole != detail;
                    let shared = shared_marker(tokens, p);
                    let id = declare(&mut symbols, p, SymbolKind::Field, detail, Some(shape));
                    if let Some(field) = symbols.get_mut(id) {
                        field.default = falls_back;
                        field.read = read_marker(tokens, p)
                            || (shared.is_some() && tokens.ident(p - 1) == Some(READ));
                        field.shared = shared.is_some();
                        field.intro = shared.flatten();
                        // Look past `priv` to detect `shared` before the field name.
                        if shared.is_some() {
                            let start = (p.saturating_sub(3)..p)
                                .find(|q| tokens.token(*q) == Some(&Token::Shared));
                            field.private =
                                start.is_some_and(|q| tokens.before(q) == Some(&Token::Priv));
                        }
                    }
                }
                ([Block::Kind], Some(kind)) if tokens.starts_line(p) => {
                    let detail = tokens.text_after(p, never);
                    variant = Some(declare(
                        &mut symbols,
                        p,
                        SymbolKind::Variant,
                        detail,
                        Some(kind),
                    ));
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
    PlanB,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Brace {
    Literal(Option<String>),
    PlanB,
}

struct Classifier<'a> {
    tokens: &'a Tokens<'a>,
    symbols: Vec<Symbol>,
    declared_at: HashMap<usize, usize>,
    field_accesses: &'a HashMap<(usize, usize), String>,
    globals: HashMap<String, usize>,
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
            if !matches!(symbol.kind, SymbolKind::Field | SymbolKind::Method) {
                globals.entry(symbol.name.clone()).or_insert(id);
            }
        }
        Classifier {
            tokens,
            symbols,
            declared_at,
            field_accesses,
            globals,
            imports,
            scopes: Vec::new(),
            out: Vec::new(),
        }
    }

    fn run(mut self) -> (Vec<Symbol>, Vec<Classified>) {
        let tokens = self.tokens;
        let mut in_type = false;
        let mut declaring: Option<&'static str> = None;
        let mut loop_name: Option<(usize, usize)> = None;
        let mut in_task_header = false;
        let mut in_pattern = false;
        let mut headers_open = 0usize;
        let mut plan_b_head = false;
        let mut after_type = false;
        let mut region_next: Option<bool> = None;
        let mut in_results = false;
        let mut lambda_params = false;
        let mut lambda_results = false;
        let mut shortcut: Option<(usize, usize)> = None;
        let mut declaring_list: Option<&'static str> = None;
        let mut loop_start = false;
        let mut loop_words: Option<&'static str> = None;
        let mut loop_first = 0usize;
        let mut in_include = false;
        let mut in_include_names = false;
        let mut braces: Vec<Brace> = Vec::new();
        let mut type_brackets = 0usize;
        let mut type_parens = 0usize;
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
                    if tokens
                        .source
                        .get(range.clone())
                        .is_some_and(|text| text.starts_with('"'))
                    {
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
            if *token == Token::Comma && p >= 2 && tokens.token(p - 2) == Some(&Token::Each) {
                declaring = Some("each");
            }
            let region_now = region_next.take();
            if lambda_results
                && (*token == Token::Send
                    || (matches!(token, Token::Ident(_)) && !was_type && region_now.is_none()))
            {
                lambda_results = false;
                in_task_header = false;
                in_results = false;
            }
            if !matches!(token, Token::Newline | Token::Failure) {
                plan_b_head = false;
            }

            match token {
                Token::Newline => {
                    for _ in 0..std::mem::take(&mut headers_open) {
                        self.close_scope(at);
                    }
                    lambda_params = false;
                    lambda_results = false;
                    shortcut = None;
                    in_include = false;
                    in_include_names = false;
                    in_task_header = false;
                    in_pattern = false;
                    in_results = false;
                    declaring_list = None;
                    loop_start = false;
                    loop_words = None;
                    type_brackets = 0;
                    type_parens = 0;
                    type_angles = 0;
                    needs_word = false;
                    // Shape literals are single-line, whereas Plan B blocks span lines.
                    braces.retain(|brace| *brace == Brace::PlanB);
                }
                Token::Eof => {}
                Token::Int(_) if p > 0 && tokens.token(p - 1) == Some(&Token::Dot) => {
                    self.push(range, TokenClass::Field, None)
                }
                Token::Int(_) | Token::Float(_) => self.push(range, TokenClass::Number, None),
                Token::Text(_) | Token::Char(_) => self.string(range),
                Token::Ident(_) if in_include => {
                    let (class, symbol) = match self.declared_at.get(&p) {
                        Some(&id) => (
                            self.symbols
                                .get(id)
                                .map_or(TokenClass::Variable, |s| s.kind.class()),
                            Some(id),
                        ),
                        None if in_include_names => (TokenClass::Variable, None),
                        None => (TokenClass::Module, None),
                    };
                    self.push(range, class, symbol);
                }
                Token::FatArrow => {
                    self.push(range, TokenClass::Operator, None);
                    in_type |= p >= 2
                        && tokens.ident(p - 2) == Some("alias")
                        && matches!(
                            tokens.before(p - 2),
                            None | Some(Token::Newline | Token::Priv)
                        );
                }
                Token::LBrace | Token::RBrace if in_include => {
                    self.push(range, TokenClass::Punctuation, None);
                    in_include_names = true;
                }
                Token::Ident(name) if needs_word => {
                    needs_word = false;
                    if vela::ast::Need::from_word(name).is_some() {
                        self.push(range, TokenClass::Keyword, None);
                    } else {
                        let symbol = self.global(name, &[SymbolKind::Pattern]);
                        self.push(range, TokenClass::Type, symbol);
                        after_type = true;
                    }
                }
                Token::Ident(name)
                    if tokens.before(p) == Some(&Token::Amp)
                        && tokens.token(p + 1) != Some(&Token::Dot)
                        && type_angles == 0
                        && self.in_item_header(p) =>
                {
                    let symbol = self.global(
                        name,
                        &[SymbolKind::Pattern, SymbolKind::Kind, SymbolKind::Alias],
                    );
                    self.push(range, TokenClass::Type, symbol);
                }
                // `new` acts as a method/value identifier in `shared new` and `X.new`.
                Token::New
                    if self.declared_at.contains_key(&p)
                        || tokens.before(p) == Some(&Token::Dot) =>
                {
                    if self.declared_at.contains_key(&p) {
                        self.open(ScopeKind::Item);
                        in_task_header = true;
                    }
                    let (class, symbol) = self.identifier(
                        p,
                        NEW_WORD,
                        false,
                        None,
                        in_task_header,
                        in_pattern,
                        &braces,
                    );
                    self.push(range, class, symbol);
                }
                Token::Ident(_) if named_key(tokens, p) => {
                    self.push(range, TokenClass::String, None)
                }
                // `at` names text's `.at(i)` method after `.`.
                Token::At
                    if tokens.before(p) == Some(&Token::Dot)
                        && tokens.token(p + 1) == Some(&Token::LParen) =>
                {
                    self.push(range, TokenClass::Method, None);
                }
                Token::Ident(word)
                    if word == PATTERN
                        && self
                            .declared_at
                            .get(&(p + 1))
                            .and_then(|&id| self.symbols.get(id))
                            .is_some_and(|s| s.kind == SymbolKind::Pattern) =>
                {
                    self.push(range, TokenClass::Keyword, None);
                    while !self.scopes.is_empty() {
                        self.close_scope(at);
                    }
                    self.open(ScopeKind::Item);
                    braces.clear();
                }
                Token::Ident(word)
                    if word == "alias"
                        && tokens.ident(p + 1).is_some()
                        && tokens.token(p + 2) == Some(&Token::FatArrow)
                        && matches!(
                            tokens.before(p),
                            None | Some(Token::Newline | Token::Priv)
                        ) =>
                {
                    self.push(range, TokenClass::Keyword, None);
                }
                Token::Ident(_)
                    if !in_include
                        && tokens.token(p + 1) == Some(&Token::FatArrow)
                        && matches!(
                            tokens.token(p + 2),
                            Some(Token::Loop | Token::Endless | Token::Each)
                        ) =>
                {
                    loop_name = Some((p, self.out.len()));
                    self.push(range, TokenClass::Variable, None);
                }
                Token::Ident(_)
                    if !was_type
                        && region_now.is_none()
                        && (shortcut.is_some_and(|(arrow, _)| p < arrow)
                            || tokens.shortcut_lambda(p).is_some()) =>
                {
                    let arrow = shortcut.map_or(p + 1, |(arrow, _)| arrow);
                    let end =
                        shortcut.map_or_else(|| tokens.shortcut_body_end(arrow), |(_, end)| end);
                    shortcut = Some((arrow, end));
                    let id = self.declare_local(p, SymbolKind::Parameter, None, String::new());
                    if let Some(param) = self.symbols.get_mut(id) {
                        param.visible = tokens.range(arrow).end..end;
                    }
                    self.push(range, TokenClass::Parameter, Some(id));
                }
                Token::Underscore if !was_type && tokens.shortcut_lambda(p).is_some() => {
                    self.push(range, TokenClass::Keyword, None);
                    shortcut = Some((p + 1, tokens.shortcut_body_end(p + 1)));
                }
                Token::Ident(name) => {
                    if let Some(type_follows) = region_now {
                        let symbol = self.region(p, name, in_task_header);
                        self.push(range, TokenClass::Region, symbol);
                        in_type = type_follows;
                        after_type = !type_follows;
                    } else {
                        if let Some(method) = self
                            .declared_at
                            .get(&p)
                            .and_then(|&id| self.symbols.get(id))
                            .filter(|s| s.kind == SymbolKind::Method)
                        {
                            if !method.default
                                && method
                                    .parent
                                    .and_then(|parent| self.symbols.get(parent))
                                    .is_some_and(|parent| parent.kind == SymbolKind::Pattern)
                            {
                                headers_open += 1;
                            }
                            self.open(ScopeKind::Item);
                            in_task_header = true;
                        }
                        let declaring_now = match tokens.token(p + 1) {
                            Some(Token::Colon) if in_results && was_type => Some("result"),
                            _ => declaring_now,
                        };
                        let (class, symbol) = self.identifier(
                            p,
                            name,
                            was_type,
                            declaring_now,
                            in_task_header,
                            in_pattern,
                            &braces,
                        );
                        if lambda_params
                            && class == TokenClass::Parameter
                            && let Some(param) = symbol.and_then(|id| self.symbols.get_mut(id))
                            && param.range.start == range.start
                        {
                            param.visible.start = range.end;
                        }
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
                    in_type = in_task_header || !matches!(braces.last(), Some(Brace::Literal(_)));
                }
                Token::Arrow if shortcut.is_some_and(|(arrow, _)| arrow == p) => {
                    self.push(range, TokenClass::Operator, None);
                    shortcut = None;
                }
                Token::Arrow => {
                    self.push(range, TokenClass::Operator, None);
                    in_type = true;
                    let results = !(was_after_type && tokens.before(p) == Some(&Token::RParen));
                    in_results |= results;
                    if lambda_params && results {
                        lambda_params = false;
                        lambda_results = true;
                    }
                }
                Token::Caret => {
                    self.push(range, TokenClass::Operator, None);
                    in_type = was_type;
                }
                Token::LBrace => {
                    self.push(range, TokenClass::Punctuation, None);
                    let literal = matches!(tokens.token(p + 1), Some(Token::RBrace))
                        || (tokens.ident(p + 1).is_some()
                            && tokens.token(p + 2) == Some(&Token::Colon));
                    if in_pattern {
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
                    if braces.pop() == Some(Brace::PlanB)
                        && self
                            .scopes
                            .last()
                            .is_some_and(|s| s.kind == ScopeKind::PlanB)
                    {
                        self.close_scope(at);
                    }
                }
                Token::LBracket if was_type => {
                    self.push(range, TokenClass::Punctuation, None);
                    type_brackets += 1;
                    in_type = true;
                }
                Token::Hash
                    if self
                        .declared_at
                        .get(&(p + 1))
                        .and_then(|&id| self.symbols.get(id))
                        .is_some_and(|s| s.kind == SymbolKind::Macro) =>
                {
                    self.push(range, TokenClass::Keyword, None);
                }
                Token::Hash => self.push(range, TokenClass::Punctuation, None),
                Token::RBracket if type_brackets > 0 => {
                    self.push(range, TokenClass::Punctuation, None);
                    type_brackets -= 1;
                    if tokens.after_kept(p + 1) {
                        in_type = true;
                    } else {
                        after_type = true;
                    }
                }
                Token::LParen if !was_type && tokens.shortcut_lambda(p).is_some() => {
                    self.push(range, TokenClass::Punctuation, None);
                    if let Some(arrow) = tokens.shortcut_lambda(p) {
                        shortcut = Some((arrow, tokens.shortcut_body_end(arrow)));
                    }
                }
                Token::LParen if !was_type && tokens.starts_lambda(p) => {
                    self.push(range, TokenClass::Punctuation, None);
                    self.open(ScopeKind::Other);
                    in_task_header = true;
                    lambda_params = true;
                }
                Token::LParen if was_type => {
                    self.push(range, TokenClass::Punctuation, None);
                    type_parens += 1;
                    in_type = true;
                }
                Token::RParen if type_parens > 0 => {
                    self.push(range, TokenClass::Punctuation, None);
                    type_parens -= 1;
                    after_type = true;
                }
                Token::LParen if declaring_now.is_some() => {
                    self.push(range, TokenClass::Punctuation, None);
                    declaring = declaring_now;
                }
                Token::LParen | Token::RParen | Token::LBracket | Token::RBracket | Token::Dot => {
                    self.push(range, TokenClass::Punctuation, None);
                    match token {
                        Token::LParen if in_include => in_include_names = true,
                        Token::Dot => in_type = was_after_type,
                        _ => {}
                    }
                }
                Token::Comma if type_parens > 0 || type_angles > 0 => {
                    self.push(range, TokenClass::Punctuation, None);
                    in_type = true;
                }
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
                Token::Amp
                    if matches!(
                        tokens.before(p),
                        Some(Token::Flex | Token::Fixed | Token::Bind)
                    ) =>
                {
                    self.push(range, TokenClass::Keyword, None);
                    if was_type {
                        in_type = true;
                    } else {
                        declaring = declaring_now;
                    }
                }
                Token::Amp => self.push(range, TokenClass::Operator, None),
                Token::Comma => {
                    self.push(range, TokenClass::Punctuation, None);
                    match (declaring_list, loop_words) {
                        _ if in_results => in_type = true,
                        (Some(intro), _) => declaring = Some(intro),
                        (None, Some(intro))
                            if loop_start
                                && tokens.ident(p + 1).is_some()
                                && matches!(
                                    tokens.token(p + 2),
                                    Some(Token::Assign | Token::Comma | Token::Colon)
                                )
                                && tokens.starts_loop_item(loop_first, p + 1) =>
                        {
                            declaring = Some(intro);
                            declaring_list = Some(intro);
                        }
                        _ => {}
                    }
                }
                Token::Parts => {
                    self.push(range, TokenClass::Operator, None);
                    loop_start = false;
                    loop_words = None;
                }
                Token::Plus
                | Token::Minus
                | Token::Star
                | Token::Power
                | Token::Slash
                | Token::Percent
                | Token::Eq
                | Token::Ne
                | Token::Pipe(_)
                | Token::Increment
                | Token::Decrement
                | Token::PlusAssign
                | Token::MinusAssign
                | Token::StarAssign
                | Token::SlashAssign
                | Token::PercentAssign
                | Token::PowerAssign
                | Token::Similar
                | Token::NotSimilar
                | Token::Lt
                | Token::Gt
                | Token::Le
                | Token::Ge
                | Token::Assign
                | Token::ColonAssign
                | Token::Range => {
                    self.push(range, TokenClass::Operator, None);
                    if *token == Token::Assign || *token == Token::ColonAssign {
                        declaring_list = None;
                        in_pattern = false;
                    }
                }
                Token::Maybe => {
                    self.push(range, TokenClass::Keyword, None);
                    in_type = was_type;
                }
                Token::Tilde => {
                    self.push(range, TokenClass::Operator, None);
                    in_type = was_after_type;
                }
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
                        // Patterns cannot contain `and`, so `and` after `is` returns to expression mode.
                        Token::And => in_pattern = false,
                        Token::Failure if plan_b_head => declaring = Some("failure"),
                        // `else when` reuses the open `when` block so one `fi` closes the whole chain.
                        Token::When if tokens.before(p) == Some(&Token::Else) => {}
                        Token::When => self.open(ScopeKind::When),
                        Token::Block => self.open(ScopeKind::Other),
                        Token::Loop | Token::Endless | Token::Each => {
                            self.open(ScopeKind::Other);
                            loop_start = *token == Token::Loop;
                            loop_words = None;
                            loop_first = p + 1;
                            if let Some((q, at)) = loop_name.take() {
                                let id = self.declare_local(
                                    q,
                                    SymbolKind::Variable,
                                    Some(LOOP_NAME),
                                    String::new(),
                                );
                                if let Some(named) = self.out.get_mut(at) {
                                    named.symbol = Some(id);
                                }
                            } else if let Some(q) = tokens.loop_variable(p) {
                                self.declare_local(
                                    q,
                                    SymbolKind::Variable,
                                    Some(LOOP_NAME),
                                    "=".to_string(),
                                );
                            }
                            if *token == Token::Each {
                                declaring = Some("each");
                            }
                        }
                        Token::Match => self.open(ScopeKind::Match),
                        Token::Flex | Token::Fixed | Token::Bind if tokens.in_kept_list(p) => {}
                        Token::Flex | Token::Fixed | Token::Bind if was_type => in_type = true,
                        Token::Flex | Token::Fixed | Token::Bind
                            if in_task_header
                                && matches!(
                                    tokens.before(p),
                                    Some(Token::LParen | Token::Comma)
                                ) => {}
                        Token::Fixed | Token::Flex | Token::Bind => {
                            let reference = tokens.token(p + 1) == Some(&Token::Amp);
                            let intro = match (keyword.clone(), reference) {
                                (Token::Fixed, false) => "fixed",
                                (Token::Fixed, true) => "fixed&",
                                (Token::Bind, false) => "bind",
                                (Token::Bind, true) => "bind&",
                                (Token::Flex, false) => "flex",
                                (Token::Flex, true) => "flex&",
                                _ => unreachable!(),
                            };
                            declaring = Some(intro);
                            declaring_list = Some(intro);
                            if loop_start {
                                loop_words = Some(intro);
                            }
                        }
                        Token::Case => {
                            self.close_plan_b(at, &mut braces);
                            if self
                                .scopes
                                .last()
                                .is_some_and(|s| s.kind == ScopeKind::Case)
                            {
                                self.close_scope(at);
                            }
                            self.open(ScopeKind::Case);
                            in_pattern = true;
                        }
                        Token::Fi => {
                            self.close_plan_b(at, &mut braces);
                            if self
                                .scopes
                                .last()
                                .is_some_and(|s| s.kind == ScopeKind::Case)
                            {
                                self.close_scope(at);
                            }
                            self.close_scope(at);
                        }
                        Token::Else => {
                            self.close_plan_b(at, &mut braces);
                            // Reset local scope at `else` so `when`-branch locals stay isolated.
                            if self
                                .scopes
                                .last()
                                .is_some_and(|s| s.kind == ScopeKind::When)
                            {
                                self.close_scope(at);
                                self.open(ScopeKind::When);
                            }
                        }
                        Token::New => in_type = true,
                        Token::At => region_next = Some(false),
                        Token::Region if !was_type => {
                            declaring = Some("region");
                            declaring_list = Some("region");
                        }
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
            let kind = self
                .symbols
                .get(id)
                .map_or(SymbolKind::Variable, |s| s.kind);
            return (kind.class(), Some(id));
        }
        if name == ALTERS
            && self
                .declared_at
                .get(&(p + 1))
                .and_then(|&id| self.symbols.get(id))
                .is_some_and(|s| s.kind == SymbolKind::Method)
        {
            return (TokenClass::Keyword, None);
        }
        // Inside a `pattern`, standalone `this` denotes the conforming type.
        if name == THIS_WORD && before != Some(&Token::Dot) && before != Some(&Token::AtSign) {
            let at = tokens.range(p).start;
            if self.symbols.iter().any(|s| {
                s.module.is_none()
                    && s.kind == SymbolKind::Pattern
                    && s.range.start <= at
                    && at < s.range.end
            }) {
                return (TokenClass::Type, None);
            }
        }
        if name == READ
            && self
                .declared_at
                .get(&(p + 1))
                .and_then(|&id| self.symbols.get(id))
                .is_some_and(|s| s.kind == SymbolKind::Field && s.read)
        {
            return (TokenClass::Keyword, None);
        }
        if name == NATIVE && after == Some(&Token::Task) {
            return (TokenClass::Keyword, None);
        }
        if name == DEFAULT {
            let two_back = p.checked_sub(2).and_then(|q| tokens.token(q));
            let after_declaration =
                tokens.ident(p.wrapping_sub(1)).is_some() && two_back == Some(&Token::Region);
            if before == Some(&Token::Region) || after_declaration {
                return (TokenClass::Keyword, None);
            }
        }
        // `Option<int>.None` has `.` after `>`, so it is a qualified variant rather than `.member`.
        let after_types = p.checked_sub(2).and_then(|q| tokens.token(q)) == Some(&Token::Gt)
            && self.kind_before().is_some();
        if before == Some(&Token::Dot)
            && !after_types
            && !p
                .checked_sub(2)
                .and_then(|q| tokens.token(q))
                .is_some_and(ends_value)
        {
            if name == THIS_WORD {
                return (TokenClass::Keyword, None);
            }
            let at = tokens.range(p).start;
            let owner = self.symbols.iter().position(|s| {
                s.module.is_none()
                    && matches!(
                        s.kind,
                        SymbolKind::Shape | SymbolKind::Kind | SymbolKind::Pattern
                    )
                    && s.range.start <= at
                    && at < s.range.end
            });
            let member = owner.and_then(|owner| {
                self.symbols
                    .iter()
                    .position(|s| {
                        s.parent == Some(owner)
                            && s.name == name
                            && matches!(s.kind, SymbolKind::Field | SymbolKind::Method)
                    })
                    .or_else(|| {
                        followed_methods(&self.symbols, owner, &|written| {
                            self.global(written, &[SymbolKind::Pattern])
                        })
                        .into_iter()
                        .find(|&id| self.symbols.get(id).is_some_and(|s| s.name == name))
                    })
            });
            return match member.and_then(|id| self.symbols.get(id).map(|s| (id, s.kind))) {
                Some((id, kind)) => (kind.class(), Some(id)),
                None if after == Some(&Token::LParen) => (TokenClass::Method, None),
                None => (TokenClass::Field, None),
            };
        }
        let at = tokens.range(p).start;
        if after == Some(&Token::Dot)
            && let Some(module) = self.module_named(name, at)
        {
            return (TokenClass::Module, Some(module));
        }
        if before == Some(&Token::Dot)
            && let Some(module) = p.checked_sub(2).and_then(|q| tokens.ident(q))
            && self
                .module_named(module, tokens.range(p - 2).start)
                .is_some()
        {
            let full = format!("{}{}{}", module, MODULE_SEPARATOR, name);
            return match self
                .globals
                .get(&full)
                .and_then(|&id| self.symbols.get(id).map(|s| (id, s.kind)))
            {
                Some((id, kind)) => (kind.class(), Some(id)),
                None if in_type || in_pattern => (TokenClass::Type, None),
                None => (TokenClass::Variable, None),
            };
        }
        if before == Some(&Token::Dot)
            && after == Some(&Token::LParen)
            && let Some(owner) = self.type_before()
        {
            let method = self.symbols.iter().position(|s| {
                s.kind == SymbolKind::Method
                    && s.shared
                    && s.parent == Some(owner)
                    && s.name == name
            });
            let is_shape = self
                .symbols
                .get(owner)
                .is_some_and(|s| s.kind == SymbolKind::Shape);
            if method.is_some() || is_shape {
                return (TokenClass::Method, method);
            }
        }
        if before == Some(&Token::Dot)
            && let Some(kind) = self.kind_before()
        {
            let variant = kind_variants(&self.symbols, kind, &|written| {
                self.global(written, &[SymbolKind::Kind])
            })
            .into_iter()
            .find(|&id| {
                self.symbols
                    .get(id)
                    .is_some_and(|s| unqualified(&s.name) == name)
            });
            if variant.is_none()
                && let Some(method) = self.symbols.iter().position(|s| {
                    s.kind == SymbolKind::Method
                        && s.shared
                        && s.parent == Some(kind)
                        && s.name == name
                })
            {
                return (TokenClass::Method, Some(method));
            }
            return (TokenClass::Variant, variant);
        }
        // Do not highlight unresolved `Kind.Variant` inside patterns as a fresh variable.
        if in_pattern && before == Some(&Token::Dot) {
            return (TokenClass::Variant, None);
        }
        if in_pattern
            && after == Some(&Token::Dot)
            && matches!(before, Some(Token::Case | Token::Is))
        {
            return (TokenClass::Type, self.global(name, &[SymbolKind::Kind]));
        }
        // In patterns like `is flex (x, y)`, types still come from the matched subject.
        if let Some(intro) = declaring.filter(|intro| !(in_pattern && *intro == "flex")) {
            if intro == "region" {
                return (
                    TokenClass::Region,
                    Some(self.declare_local(p, SymbolKind::Region, Some(intro), String::new())),
                );
            }
            let detail = if after == Some(&Token::Colon) {
                tokens.text_after(p + 1, |t| matches!(t, Token::Assign | Token::ColonAssign | Token::Comma))
            } else {
                String::new()
            };
            return (
                TokenClass::Variable,
                Some(self.declare_local(p, SymbolKind::Variable, Some(intro), detail)),
            );
        }
        if in_type {
            if Type::builtin(name).is_some() {
                return (TokenClass::TypeBuiltin, None);
            }
            return (
                TokenClass::Type,
                self.global(
                    name,
                    &[
                        SymbolKind::Shape,
                        SymbolKind::Kind,
                        SymbolKind::Alias,
                        SymbolKind::TypeParam,
                    ],
                ),
            );
        }
        let opens_param = |q: usize| matches!(tokens.before(q), Some(Token::LParen | Token::Comma));
        let word_at = |q: usize| match tokens.token(q) {
            Some(Token::Flex) if opens_param(q) => Some("flex"),
            Some(Token::Fixed) if opens_param(q) => Some("fixed"),
            Some(Token::Bind) if opens_param(q) => Some("bind"),
            _ => None,
        };
        let marked = match before {
            Some(Token::Flex | Token::Fixed | Token::Bind) => {
                word_at(p - 1).map(|word| word.to_string())
            }
            Some(Token::Amp) if p >= 2 => word_at(p - 2).map(|word| format!("{}&", word)),
            _ => None,
        };
        if in_task_header
            && after == Some(&Token::Colon)
            && (marked.is_some() || matches!(before, Some(Token::LParen | Token::Comma)))
        {
            let detail = tokens.text_after(p + 1, ends_field);
            let takes_region = crate::infer::without_default(&detail) == "region";
            let kind = if takes_region {
                SymbolKind::Region
            } else {
                SymbolKind::Parameter
            };
            let intro = match marked.as_deref() {
                Some("flex&") => Some("flex&"),
                Some("flex") => Some("flex"),
                Some("bind&") => Some("bind&"),
                Some("bind") => Some("bind"),
                Some("fixed&") => Some("fixed&"),
                Some("fixed") => Some("fixed"),
                _ => None,
            };
            return (
                kind.class(),
                Some(self.declare_local(p, kind, intro, detail)),
            );
        }
        if in_pattern {
            if matches!(before, Some(Token::Case | Token::Is)) {
                return (
                    TokenClass::Variant,
                    self.global(name, &[SymbolKind::Variant]),
                );
            }
            let intro = if self
                .scopes
                .last()
                .is_some_and(|s| s.kind == ScopeKind::Case)
            {
                "case"
            } else {
                "is"
            };
            return (
                TokenClass::Variable,
                Some(self.declare_local(p, SymbolKind::Variable, Some(intro), String::new())),
            );
        }
        // Require `Kind.` prefix so same-named shape fields are not shadowed.
        if before == Some(&Token::Dot)
            && let Some(kind) = vela::ast::builtin_kind_of(name)
            && p.checked_sub(2).and_then(|owner| tokens.ident(owner)) == Some(kind)
        {
            return (TokenClass::Variant, None);
        }
        if before == Some(&Token::Dot) {
            let start = tokens.get(p).map(|t| (t.start.line, t.start.col));
            let typed = self
                .type_before()
                .and_then(|id| self.symbols.get(id))
                .map(|s| s.name.clone());
            let owner = start
                .and_then(|at| self.field_accesses.get(&at))
                .cloned()
                .or_else(|| typed.clone());
            let kinds: &[SymbolKind] = match (after == Some(&Token::LParen), typed.is_some()) {
                (true, _) => &[SymbolKind::Method],
                (false, true) => &[SymbolKind::Field, SymbolKind::Method],
                (false, false) => &[SymbolKind::Field],
            };
            return match self.member(owner.as_deref(), name, kinds) {
                Some(id) => (
                    self.symbols
                        .get(id)
                        .map_or(TokenClass::Field, |s| s.kind.class()),
                    Some(id),
                ),
                None => (TokenClass::Field, None),
            };
        }
        if let Some(Brace::Literal(Some(shape))) = braces.last()
            && after == Some(&Token::Colon)
            && matches!(before, Some(Token::LBrace | Token::Comma))
        {
            let shape = shape.clone();
            return (
                TokenClass::Field,
                self.member(Some(&shape), name, &[SymbolKind::Field]),
            );
        }
        let literal_follows = after == Some(&Token::LBrace)
            && (tokens.token(p + 2) == Some(&Token::RBrace)
                || (tokens.ident(p + 2).is_some() && tokens.token(p + 3) == Some(&Token::Colon)));
        if literal_follows && self.local(name, tokens.range(p).start).is_none() {
            return (
                TokenClass::Type,
                self.global(
                    name,
                    &[SymbolKind::Shape, SymbolKind::Kind, SymbolKind::Alias],
                ),
            );
        }
        if after == Some(&Token::LParen) && BUILTIN_TASKS.contains(&name) {
            return (TokenClass::BuiltinFn, None);
        }
        if after == Some(&Token::LParen)
            && (name == vela::ast::TEXT_CAST || vela::ast::number_cast(name).is_some())
        {
            return (TokenClass::TypeBuiltin, None);
        }
        if after == Some(&Token::RParen)
            && before == Some(&Token::Comma)
            && Type::builtin(name).is_some()
            && reads_a_number(tokens, p)
        {
            return (TokenClass::TypeBuiltin, None);
        }
        if after == Some(&Token::LParen)
            && let Some(id) = self.global(name, &[SymbolKind::Task, SymbolKind::Variant])
        {
            let kind = self.symbols.get(id).map_or(SymbolKind::Task, |s| s.kind);
            return (kind.class(), Some(id));
        }
        let local = match tokens.before(p) {
            Some(Token::Break | Token::Continue) => self.loop_named(name, tokens.range(p).start),
            _ => self.local(name, tokens.range(p).start),
        };
        if let Some(id) = local {
            let kind = self
                .symbols
                .get(id)
                .map_or(SymbolKind::Variable, |s| s.kind);
            return (kind.class(), Some(id));
        }
        if let Some(id) = self.global(
            name,
            &[
                SymbolKind::Variant,
                SymbolKind::Macro,
                SymbolKind::Task,
                SymbolKind::Shape,
                SymbolKind::Kind,
            ],
        ) {
            let kind = self
                .symbols
                .get(id)
                .map_or(SymbolKind::Variable, |s| s.kind);
            return (kind.class(), Some(id));
        }
        if BUILTIN_TASKS.contains(&name) {
            return (TokenClass::BuiltinFn, None);
        }
        // Fallback for built-in error kinds (`ParseError`, `InputError`) absent from `symbols`.
        if vela::ast::BUILTIN_KIND_NAMES.contains(&name) {
            let class = if vela::ast::builtin_kind_of(name).is_some() {
                TokenClass::Variant
            } else {
                TokenClass::Type
            };
            return (class, None);
        }
        if after == Some(&Token::LParen) {
            return (TokenClass::Task, None);
        }
        (TokenClass::Variable, None)
    }

    fn region(&mut self, p: usize, name: &str, in_task_header: bool) -> Option<usize> {
        if self.in_task_type(p) {
            return None;
        }
        if !in_task_header {
            return self.local(name, self.tokens.range(p).start);
        }
        let header = self
            .scopes
            .last()
            .map(|scope| scope.variables.clone())
            .unwrap_or_default();
        let known = header.into_iter().find(|&id| {
            self.symbols
                .get(id)
                .is_some_and(|s| s.name == name && s.kind == SymbolKind::Region)
        });
        if known.is_some() {
            return known;
        }
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

    fn in_task_type(&self, p: usize) -> bool {
        let tokens = self.tokens;
        let (mut depth, mut arrows, mut cut) = (0usize, 0usize, None);
        for q in (0..p).rev() {
            match tokens.token(q) {
                Some(Token::RParen | Token::RBracket) => depth += 1,
                Some(Token::LParen) if depth == 1 && cut.is_none() && names_paren(tokens, q) => {
                    return arrows > 1;
                }
                Some(Token::LParen | Token::LBracket) if depth > 0 => depth -= 1,
                Some(Token::LParen) if names_paren(tokens, q) => return cut.unwrap_or(arrows) > 0,
                Some(Token::LParen)
                    if closing(tokens, q)
                        .is_some_and(|end| tokens.token(end + 1) == Some(&Token::Arrow)) =>
                {
                    return true;
                }
                Some(Token::LParen | Token::LBracket) => {
                    if cut.unwrap_or(arrows) > 0 {
                        return true;
                    }
                    (arrows, cut) = (0, None);
                }
                Some(Token::Comma) if depth == 0 && cut.is_none() => cut = Some(arrows),
                Some(Token::Arrow) if depth == 0 && cut.is_none() => arrows += 1,
                Some(Token::Newline | Token::Colon | Token::Assign) | None if depth == 0 => {
                    let own = usize::from(!matches!(
                        tokens.token(q),
                        Some(Token::Colon | Token::Assign)
                    ));
                    return cut.unwrap_or(arrows) > own;
                }
                _ => {}
            }
        }
        cut.unwrap_or(arrows) > 1
    }

    fn open(&mut self, kind: ScopeKind) {
        self.scopes.push(Scope {
            kind,
            variables: Vec::new(),
        });
    }

    fn close_plan_b(&mut self, at: usize, braces: &mut Vec<Brace>) {
        while self
            .scopes
            .last()
            .is_some_and(|s| s.kind == ScopeKind::PlanB)
        {
            self.close_scope(at);
            if let Some(open) = braces.iter().rposition(|brace| *brace == Brace::PlanB) {
                braces.truncate(open);
            }
        }
    }

    fn close_scope(&mut self, at: usize) {
        let Some(scope) = self.scopes.pop() else {
            return;
        };
        for id in scope.variables {
            if let Some(symbol) = self.symbols.get_mut(id) {
                symbol.visible.end = at.max(symbol.visible.start);
            }
        }
    }

    fn declare_local(
        &mut self,
        p: usize,
        kind: SymbolKind,
        intro: Option<&'static str>,
        detail: String,
    ) -> usize {
        let tokens = self.tokens;
        let name = tokens.ident(p).unwrap_or_default().to_string();
        let range = tokens.range(p);
        let start = tokens.get(p).map(|t| t.start).unwrap_or_default();
        // `case` bindings enter scope immediately for same-line arms like `case Ok v = v * 2`.
        if intro == Some("case") {
            let visible = range.end..tokens.source.len();
            self.symbols.push(Symbol {
                name,
                kind,
                name_range: range.clone(),
                range,
                start,
                visible,
                intro,
                detail,
                parent: None,
                default: false,
                read: false,
                shared: false,
                module: None,
                private: false,
                bases: Vec::new(),
            });
            let id = self.symbols.len() - 1;
            if let Some(scope) = self.scopes.last_mut() {
                scope.variables.push(id);
            }
            return id;
        }
        // Other locals enter scope on the next line, past initializers and multi-line header Plan Bs.
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
        let joined = match intro {
            Some("is") => (p..tokens.len())
                .take_while(|&q| {
                    !matches!(tokens.token(q), Some(Token::Newline | Token::Eof) | None)
                })
                .find(|&q| tokens.token(q) == Some(&Token::And)),
            _ => None,
        };
        let item_end = match intro {
            Some("flex" | "fixed") => {
                let line: Vec<usize> = (p..tokens.len())
                    .take_while(|&q| {
                        !matches!(tokens.token(q), Some(Token::Newline | Token::Eof) | None)
                    })
                    .collect();
                let mut depth = 0usize;
                let mut assigned = false;
                let parted = line.iter().any(|&q| tokens.token(q) == Some(&Token::Parts));
                line.into_iter().filter(|_| parted).find(|&q| {
                    match tokens.token(q) {
                        Some(Token::LParen | Token::LBracket | Token::LBrace) => depth += 1,
                        Some(Token::RParen | Token::RBracket | Token::RBrace) => {
                            depth = depth.saturating_sub(1)
                        }
                        Some(Token::Assign) if depth == 0 => assigned = true,
                        Some(Token::Comma) if depth == 0 && assigned => return true,
                        Some(Token::Parts) if depth == 0 => return true,
                        _ => {}
                    }
                    false
                })
            }
            _ => None,
        };
        let from = joined
            .or(item_end)
            .map_or(line_end, |end| tokens.range(end).end);
        let visible = from..tokens.source.len();
        self.symbols.push(Symbol {
            name,
            kind,
            name_range: range.clone(),
            range,
            start,
            visible,
            intro,
            detail,
            parent: None,
            default: false,
            read: false,
            shared: false,
            module: None,
            private: false,
            bases: Vec::new(),
        });
        let id = self.symbols.len() - 1;
        if let Some(scope) = self.scopes.last_mut() {
            scope.variables.push(id);
        }
        id
    }

    /// Locals are invisible inside their own initializers (`fixed y = y + 1`).
    fn local(&self, name: &str, at: usize) -> Option<usize> {
        self.scoped(name, at, false)
    }

    fn loop_named(&self, name: &str, at: usize) -> Option<usize> {
        self.scoped(name, at, true)
    }

    fn scoped(&self, name: &str, at: usize, loop_name: bool) -> Option<usize> {
        self.scopes
            .iter()
            .rev()
            .flat_map(|scope| scope.variables.iter().rev())
            .copied()
            .find(|&id| {
                self.symbols.get(id).is_some_and(|s| {
                    s.name == name
                        && s.visible.start <= at
                        && at <= s.visible.end
                        && (s.intro == Some(LOOP_NAME)) == loop_name
                })
            })
    }

    fn global(&self, name: &str, kinds: &[SymbolKind]) -> Option<usize> {
        let own = self
            .globals
            .get(name)
            .copied()
            .filter(|&id| self.symbols.get(id).is_some_and(|s| s.module.is_none()));
        own.or_else(|| self.imports.get(name).copied())
            .filter(|&id| {
                self.symbols
                    .get(id)
                    .is_some_and(|s| kinds.contains(&s.kind))
            })
    }

    fn type_before(&self) -> Option<usize> {
        let mut classified = self
            .out
            .iter()
            .rev()
            .filter(|token| token.class != TokenClass::Comment);
        let dot = classified.next()?;
        let named = classified.next()?;
        let text = self.tokens.source.get(dot.range.clone())?;
        let id = named.symbol.filter(|_| text == ".")?;
        self.symbols
            .get(id)
            .is_some_and(|s| matches!(s.kind, SymbolKind::Shape | SymbolKind::Kind))
            .then_some(id)
    }

    fn kind_before(&self) -> Option<usize> {
        let mut classified = self
            .out
            .iter()
            .rev()
            .filter(|token| token.class != TokenClass::Comment);
        let dot = classified.next()?;
        let mut named = classified.next()?;
        let text = |token: &Classified| {
            self.tokens
                .source
                .get(token.range.clone())
                .unwrap_or_default()
        };
        if text(named) == ">" {
            let mut depth = 0usize;
            loop {
                match text(named) {
                    ">" => depth += 1,
                    "<" if depth == 1 => break,
                    "<" => depth -= 1,
                    _ => {}
                }
                named = classified.next()?;
            }
            named = classified.next()?;
        }
        let id = named.symbol.filter(|_| text(dot) == ".")?;
        self.symbols
            .get(id)
            .is_some_and(|s| s.kind == SymbolKind::Kind)
            .then_some(id)
    }

    fn module_named(&self, name: &str, at: usize) -> Option<usize> {
        self.global(name, &[SymbolKind::Module])
            .filter(|_| self.local(name, at).is_none())
    }

    fn in_item_header(&self, p: usize) -> bool {
        let tokens = self.tokens;
        let mut q = p;
        let mut colons = false;
        while let Some(before) = q.checked_sub(1) {
            match tokens.token(before) {
                Some(Token::Newline) | None => return false,
                Some(Token::Shape | Token::Kind) => return colons && tokens.starts_member(before),
                Some(Token::Colon) if tokens.token(before + 1) == Some(&Token::Colon) => {
                    colons = true
                }
                _ => {}
            }
            q = before;
        }
        false
    }

    fn member(&self, owner: Option<&str>, name: &str, kinds: &[SymbolKind]) -> Option<usize> {
        let found = |owned: &dyn Fn(&Symbol) -> bool| {
            let mut candidates = self.symbols.iter().enumerate().filter(|(_, s)| {
                kinds.contains(&s.kind)
                    && s.name == name
                    && s.parent
                        .and_then(|parent| self.symbols.get(parent))
                        .is_some_and(|parent| {
                            matches!(parent.kind, SymbolKind::Shape | SymbolKind::Kind)
                                && owned(parent)
                        })
            });
            let first = candidates.next().map(|(id, _)| id);
            // Ambiguous unowned member names resolve to neither candidate.
            if owner.is_none() && candidates.next().is_some() {
                return None;
            }
            first
        };
        let exact = found(&|parent: &Symbol| owner.is_none_or(|owner| parent.name == owner));
        if exact.is_some() || owner.is_none() {
            return exact;
        }
        if kinds.contains(&SymbolKind::Method)
            && let Some(owner) = owner.and_then(|owner| {
                self.symbols.iter().position(|s| {
                    s.name == owner && matches!(s.kind, SymbolKind::Shape | SymbolKind::Kind)
                })
            })
            && let Some(method) = followed_methods(&self.symbols, owner, &|written| {
                self.global(written, &[SymbolKind::Pattern])
            })
            .into_iter()
            .find(|&id| self.symbols.get(id).is_some_and(|s| s.name == name))
        {
            return Some(method);
        }
        // Match both qualified (`d.File`) and imported (`File`) owner spellings.
        let suffix = format!("{}{}", MODULE_SEPARATOR, owner.unwrap_or_default());
        let mut loose = self.symbols.iter().enumerate().filter(|(_, s)| {
            kinds.contains(&s.kind)
                && s.name == name
                && s.parent
                    .and_then(|parent| self.symbols.get(parent))
                    .is_some_and(|parent| {
                        parent.module.is_some()
                            && matches!(parent.kind, SymbolKind::Shape | SymbolKind::Kind)
                            && parent.name.ends_with(&suffix)
                    })
        });
        let first = loose.next().map(|(id, _)| id);
        if loose.next().is_some() { None } else { first }
    }

    fn push(&mut self, range: Range<usize>, class: TokenClass, symbol: Option<usize>) {
        if range.start < range.end {
            self.out.push(Classified {
                range,
                class,
                symbol,
            });
        }
    }

    fn string(&mut self, range: Range<usize>) {
        let text = self.tokens.source.get(range.clone()).unwrap_or_default();
        let mut piece = range.start;
        let mut chars = text.char_indices();
        // Char literals allow `\'`, which string literals reject.
        let quoted = text.starts_with('\'');
        while let Some((i, c)) = chars.next() {
            if c != '\\' {
                continue;
            }
            let start = range.start + i;
            let next = chars.next();
            let end = next.map_or(range.end, |(j, n)| range.start + j + n.len_utf8());
            let valid = next.is_some_and(|(_, n)| {
                n != '\n'
                    && (ESCAPES.iter().any(|(escape, _)| *escape == n) || (quoted && n == '\''))
            });
            self.push(piece..start, TokenClass::String, None);
            self.push(
                start..end,
                if valid {
                    TokenClass::Escape
                } else {
                    TokenClass::Invalid
                },
                None,
            );
            piece = end;
        }
        self.push(piece..range.end, TokenClass::String, None);
    }
}

fn header_paren(tokens: &Tokens<'_>, q: usize) -> bool {
    tokens.ident(q.wrapping_sub(1)).is_some() || tokens.before(q) == Some(&Token::Gt)
}

fn names_paren(tokens: &Tokens<'_>, q: usize) -> bool {
    header_paren(tokens, q)
        || matches!(
            tokens.token(q + 1),
            Some(Token::Flex | Token::Fixed | Token::Bind)
        )
        || (tokens.ident(q + 1).is_some() && tokens.token(q + 2) == Some(&Token::Colon))
}

fn closing(tokens: &Tokens<'_>, q: usize) -> Option<usize> {
    let mut depth = 0usize;
    for at in q + 1.. {
        match tokens.token(at) {
            Some(Token::LParen | Token::LBracket) => depth += 1,
            Some(Token::RParen) if depth == 0 => return Some(at),
            Some(Token::RParen | Token::RBracket) => depth = depth.saturating_sub(1),
            Some(Token::Newline | Token::Eof) | None => return None,
            _ => {}
        }
    }
    None
}
