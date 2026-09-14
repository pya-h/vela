//! Language analysis for Vela Studio: highlighting, diagnostics, outline, and
//! go-to-definition for one Vela source file.
//!
//! Everything is computed from the compiler itself: tokens come from
//! `vela::lexer::highlight`, errors from `vela::driver::front_end`, and names
//! from a declaration scan over the compiler's tokens (see `scan`), which keeps
//! working while the file has errors. Nothing here does I/O.
//!
//! All offsets in the results are UTF-16 offsets into the source, as
//! JavaScript strings and CodeMirror count them.

mod fields;
mod position;
mod scan;

use serde::Serialize;
use vela::driver;
use vela::error::Error;
use vela::lexer::{LexKind, Token as LexedToken};

pub use position::LineIndex;
pub use scan::{SymbolKind, TokenClass};

/// A highlighted piece of source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Token {
    pub from: usize,
    pub to: usize,
    pub class: TokenClass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    Error,
}

/// A problem the compiler reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub from: usize,
    pub to: usize,
    /// The compiler's 1-based line and column, absent for an internal
    /// compiler error, which has no position.
    pub line: Option<usize>,
    pub col: Option<usize>,
    pub message: String,
    pub severity: Severity,
}

/// A task, shape, or kind, with its fields or variants as children (and a
/// variant's payload fields as its children).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutlineItem {
    pub name: String,
    pub kind: SymbolKind,
    /// A task's parameters and return type, a field's type, or a variant's
    /// payload, as written.
    pub detail: String,
    /// The whole declaration.
    pub from: usize,
    pub to: usize,
    /// The declared name.
    pub name_from: usize,
    pub name_to: usize,
    pub children: Vec<OutlineItem>,
}

/// A range of source, such as the name a definition points to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Location {
    pub from: usize,
    pub to: usize,
}

/// The analysis of one version of a source file. Create one per change and
/// ask it anything.
pub struct Analysis<'a> {
    source: &'a str,
    index: LineIndex<'a>,
    scan: scan::Scan,
    /// The compiler's first error, if the file doesn't check.
    error: Option<Error>,
}

impl<'a> Analysis<'a> {
    pub fn new(source: &'a str) -> Self {
        let (error, accesses) = match driver::front_end(source) {
            Ok(items) => (None, fields::field_accesses(&items)),
            Err(error) => (Some(error), Default::default()),
        };
        Analysis { source, index: LineIndex::new(source), scan: scan::scan(source, &accesses), error }
    }

    /// Every piece of source to highlight, in order and without overlaps.
    /// Whitespace and newlines are not included.
    pub fn tokens(&self) -> Vec<Token> {
        self.scan
            .tokens
            .iter()
            .map(|t| {
                let range = self.index.utf16_range(&t.range);
                Token { from: range.start, to: range.end, class: t.class }
            })
            .collect()
    }

    /// The compiler's diagnostics: its first error, if any. The range covers
    /// the token the error points at, or one character when the error points
    /// inside a token, or is empty at the end of a line.
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        let Some(error) = &self.error else { return Vec::new() };
        let Some(span) = error.span else {
            return vec![Diagnostic {
                from: 0,
                to: 0,
                line: None,
                col: None,
                message: error.message.clone(),
                severity: Severity::Error,
            }];
        };
        let start = self.index.byte_of(span);
        let token = self.scan.lexed.iter().find(|t| {
            t.range.start == start
                && t.range.end > start
                && !matches!(t.kind, LexKind::Token(LexedToken::Newline) | LexKind::Token(LexedToken::Eof))
        });
        let end = match token {
            Some(token) => token.range.end,
            None => match self.source.get(start..).and_then(|rest| rest.chars().next()) {
                Some(c) if c != '\n' && c != '\r' => start + c.len_utf8(),
                _ => start,
            },
        };
        vec![Diagnostic {
            from: self.index.utf16_of(start),
            to: self.index.utf16_of(end),
            line: Some(span.line),
            col: Some(span.col),
            message: error.message.clone(),
            severity: Severity::Error,
        }]
    }

    /// The file's tasks, shapes, and kinds, in source order.
    pub fn outline(&self) -> Vec<OutlineItem> {
        let symbols = &self.scan.symbols;
        let children_of = |parent: usize| symbols.iter().enumerate().filter(move |(_, s)| s.parent == Some(parent));
        symbols
            .iter()
            .enumerate()
            .filter(|(_, s)| matches!(s.kind, SymbolKind::Task | SymbolKind::Shape | SymbolKind::Kind))
            .map(|(id, _)| self.outline_item(id, &children_of))
            .collect()
    }

    fn outline_item<'s, I>(&self, id: usize, children_of: &impl Fn(usize) -> I) -> OutlineItem
    where
        I: Iterator<Item = (usize, &'s scan::Symbol)>,
    {
        let symbol = self.scan.symbols.get(id);
        let range = symbol.map(|s| self.index.utf16_range(&s.range)).unwrap_or_default();
        let name = symbol.map(|s| self.index.utf16_range(&s.name_range)).unwrap_or_default();
        OutlineItem {
            name: symbol.map(|s| s.name.clone()).unwrap_or_default(),
            kind: symbol.map_or(SymbolKind::Task, |s| s.kind),
            detail: symbol.map(|s| s.detail.clone()).unwrap_or_default(),
            from: range.start,
            to: range.end,
            name_from: name.start,
            name_to: name.end,
            children: children_of(id).map(|(child, _)| self.outline_item(child, children_of)).collect(),
        }
    }

    /// Where the name at `offset` is declared. The cursor may be anywhere in
    /// the name or just after it; a name that starts at the cursor wins over
    /// one that ends there.
    pub fn definition(&self, offset: usize) -> Option<Location> {
        let byte = self.index.byte_of_utf16(offset);
        let tokens = &self.scan.tokens;
        let symbol = tokens
            .iter()
            .filter(|t| t.range.start <= byte && byte < t.range.end)
            .find_map(|t| t.symbol)
            .or_else(|| tokens.iter().filter(|t| t.range.end == byte).find_map(|t| t.symbol))?;
        let range = self.index.utf16_range(&self.scan.symbols.get(symbol)?.name_range);
        Some(Location { from: range.start, to: range.end })
    }
}
