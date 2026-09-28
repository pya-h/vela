//! Language analysis for Vela Studio: highlighting, diagnostics, outline,
//! go-to-definition, completion, and hover for one Vela source file, with the
//! other files of its program (the modules it includes) that the editor gives.
//!
//! Everything is computed from the compiler itself: tokens come from
//! `vela::lexer::highlight`, errors from `vela::driver::front_end`, and names
//! from a declaration scan over the compiler's tokens (see `scan`), which keeps
//! working while the file has errors. Nothing here does I/O.
//!
//! All offsets in the results are UTF-16 offsets into the source, as
//! JavaScript strings and CodeMirror count them.

mod complete;
mod hover;
mod infer;
mod language;
mod position;
mod scan;
mod types;

use std::collections::HashMap;
use std::sync::LazyLock;

use serde::Serialize;
use vela::error::Error;
use vela::lexer::{LexKind, Token as LexedToken};
use vela::modules::SourceFile;

pub use complete::{CompletionItem, CompletionKind, Completions};
pub use hover::Hover;
pub use language::{BLOCK_CLOSER, BLOCK_OPENERS, BRANCH_KEYWORDS, Language, language};
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
    /// What the program does that it may not mean; it builds all the same.
    Warning,
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

/// A task, shape, or kind, with its fields or variants and its methods as
/// children (and a variant's payload fields as its children).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutlineItem {
    pub name: String,
    pub kind: SymbolKind,
    /// A task's or a method's parameters and return type, a field's type, or
    /// a variant's payload, as written.
    pub detail: String,
    /// The whole declaration.
    pub from: usize,
    pub to: usize,
    /// The declared name.
    pub name_from: usize,
    pub name_to: usize,
    pub children: Vec<OutlineItem>,
}

/// A range of source, such as the name a definition points to, in the
/// analyzed file or, with `path`, in another file of the program.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Location {
    pub from: usize,
    pub to: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// No other files, for a file analyzed on its own.
static NO_SOURCES: LazyLock<HashMap<String, String>> = LazyLock::new(HashMap::new);

/// The analysis of one version of a source file. Create one per change and
/// ask it anything.
pub struct Analysis<'a> {
    source: &'a str,
    context: types::Context<'a>,
    index: LineIndex<'a>,
    scan: scan::Scan,
    /// What the compiler found: every mistake, and the checked types.
    checked: types::Checked,
}

impl<'a> Analysis<'a> {
    /// The analysis of a file on its own: a module it includes can't be read.
    pub fn new(source: &'a str) -> Self {
        Analysis::with_modules(source, "", &NO_SOURCES)
    }

    /// The analysis of the file at `path` (`/`-separated), whose program's
    /// other files are in `sources`, by path. A file it includes that isn't there is
    /// reported by `missing_modules`, for the editor to fetch.
    pub fn with_modules(source: &'a str, path: &'a str, sources: &'a HashMap<String, String>) -> Self {
        let context = types::Context { path, sources };
        let checked = types::Checked::new(source, context);
        let scan = scan::scan(source, &checked.field_accesses, context);
        Analysis { source, context, index: LineIndex::new(source), scan, checked }
    }

    /// The paths of the files the program includes, directly or through other
    /// modules, that weren't given, in the order they are needed.
    pub fn missing_modules(&self) -> Vec<String> {
        self.checked.missing.clone()
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

    /// The compiler's diagnostics: every mistake it found, in source order. The
    /// range of each covers the token it points at, or one character when it
    /// points inside a token, or is empty at the end of a line.
    ///
    /// The warnings come among them, in source order too: what the program
    /// does that it may not mean, though it builds.
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        let errors = self.checked.errors.iter().map(|error| self.diagnostic(error, Severity::Error));
        let warnings = self.checked.warnings.iter().map(|warning| self.diagnostic(warning, Severity::Warning));
        let mut all: Vec<Diagnostic> = errors.chain(warnings).collect();
        // A stable sort: the errors stay first at one position.
        all.sort_by_key(|d| (d.line.is_none(), d.line, d.col));
        all
    }

    /// One mistake, or one warning, as the editor marks it.
    fn diagnostic(&self, error: &Error, severity: Severity) -> Diagnostic {
        let Some(found) = error.span else {
            return Diagnostic {
                from: 0,
                to: 0,
                line: None,
                col: None,
                message: error.message.clone(),
                severity,
            };
        };
        // An error in another file is shown at the `include` that leads to it, with
        // that file's path and position.
        let files: &[SourceFile] = &self.checked.files;
        let span = SourceFile::in_main_file(files, found);
        let message = match files.get(found.file) {
            Some(file) if found.file > 0 => {
                format!("{}:{}:{}: {}", types::slashed(&file.path), found.line, found.col, error.message)
            }
            _ => error.message.clone(),
        };
        let start = self.index.byte_of(span);
        let token = self.scan.lexed.iter().find(|t| {
            t.range.start == start
                && t.range.end > start
                && !matches!(t.kind, LexKind::Token(LexedToken::Newline) | LexKind::Token(LexedToken::Eof))
        });
        // An error from another file underlines the whole module path of the `include`.
        let path_end = (found.file > 0).then(|| {
            let lexed = &self.scan.lexed;
            let first = lexed.iter().position(|t| t.range.start == start)?;
            let path = lexed.get(first..)?.iter().take_while(|t| matches!(t.kind, LexKind::Token(LexedToken::Ident(_) | LexedToken::Slash | LexedToken::Range)));
            path.last().map(|t| t.range.end)
        });
        let end = match token {
            Some(_) if path_end.flatten().is_some() => path_end.flatten().unwrap_or(start),
            Some(token) => token.range.end,
            None => match self.source.get(start..).and_then(|rest| rest.chars().next()) {
                Some(c) if c != '\n' && c != '\r' => start + c.len_utf8(),
                _ => start,
            },
        };
        Diagnostic {
            from: self.index.utf16_of(start),
            to: self.index.utf16_of(end),
            line: Some(span.line),
            col: Some(span.col),
            message,
            severity,
        }
    }

    /// The file's tasks, shapes, and kinds, in source order.
    pub fn outline(&self) -> Vec<OutlineItem> {
        let symbols = &self.scan.symbols;
        let children_of = |parent: usize| symbols.iter().enumerate().filter(move |(_, s)| s.parent == Some(parent));
        symbols
            .iter()
            .enumerate()
            .filter(|(_, s)| s.module.is_none() && matches!(s.kind, SymbolKind::Task | SymbolKind::Shape | SymbolKind::Kind))
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
        let knowledge = infer::Knowledge::new(self.source, &self.scan, &self.checked);
        let symbol = tokens
            .iter()
            .filter(|t| t.range.start <= byte && byte < t.range.end)
            .find_map(|t| knowledge.symbol_of(t))
            .or_else(|| tokens.iter().filter(|t| t.range.end == byte).find_map(|t| knowledge.symbol_of(t)))?;
        let symbol = self.scan.symbols.get(symbol)?;
        // A module's name leads to its file.
        if symbol.kind == SymbolKind::Module {
            let module = self.scan.modules.iter().find(|m| self.scan.symbols.get(m.symbol).is_some_and(|s| s.name_range == symbol.name_range))?;
            if module.library {
                return None;
            }
            return module.source.as_ref().map(|_| Location { from: 0, to: 0, path: Some(module.path.clone()) });
        }
        match symbol.module.and_then(|m| self.scan.modules.get(m)) {
            // Vela's library is no file the editor can open.
            Some(module) if module.library => None,
            Some(module) => {
                let source = module.source.as_deref()?;
                let range = LineIndex::new(source).utf16_range(&symbol.name_range);
                Some(Location { from: range.start, to: range.end, path: Some(module.path.clone()) })
            }
            None => {
                let range = self.index.utf16_range(&symbol.name_range);
                Some(Location { from: range.start, to: range.end, path: None })
            }
        }
    }

    /// What may be typed at `offset`: keywords, snippets, names, fields, or
    /// types, depending on where the cursor is.
    pub fn complete(&self, offset: usize) -> Completions {
        let byte = self.index.byte_of_utf16(offset);
        // `first<|>(xs)`, as an editor closing the `<` leaves it: the types a
        // use gives are being written, though `<>` is read as one operator,
        // not similar. They are completed as if the two stood apart; nothing
        // offered reaches past the cursor, so the ranges stay the file's.
        if byte > 0 && self.source.get(byte - 1..byte + 1) == Some("<>") {
            let apart = format!("{} {}", self.source.get(..byte).unwrap_or_default(), self.source.get(byte..).unwrap_or_default());
            return Analysis::with_modules(&apart, self.context.path, self.context.sources).complete(offset);
        }
        let (items, from, to) = complete::complete(self.source, &self.scan, &self.checked, self.context, byte);
        Completions { from: self.index.utf16_of(from), to: self.index.utf16_of(to), items }
    }

    /// A description of the name at `offset`: its declaration, type, or
    /// signature.
    pub fn hover(&self, offset: usize) -> Option<Hover> {
        let knowledge = infer::Knowledge::new(self.source, &self.scan, &self.checked);
        let ((code, doc), range) = hover::hover(&knowledge, self.index.byte_of_utf16(offset))?;
        let range = self.index.utf16_range(&range);
        Some(Hover { from: range.start, to: range.end, code, doc })
    }
}
