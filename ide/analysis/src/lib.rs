//! Offsets in public results are UTF-16 code units; `scan` still runs when front-end reports errors.

mod complete;
mod hover;
mod infer;
mod language;
mod position;
mod scan;
mod types;

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use vela::error::Error;
use vela::lexer::{LexKind, Token as LexedToken};
use vela::modules::SourceFile;

pub use complete::{CompletionItem, CompletionKind, Completions};
pub use hover::Hover;
pub use language::{BLOCK_CLOSER, BLOCK_OPENERS, BRANCH_KEYWORDS, Language, language};
pub use position::LineIndex;
pub use scan::{SymbolKind, TokenClass};

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
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub from: usize,
    pub to: usize,
    /// Absent for internal compiler errors without a source position.
    pub line: Option<usize>,
    pub col: Option<usize>,
    pub message: String,
    pub severity: Severity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutlineItem {
    pub name: String,
    pub kind: SymbolKind,
    pub detail: String,
    pub from: usize,
    pub to: usize,
    pub name_from: usize,
    pub name_to: usize,
    pub children: Vec<OutlineItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Location {
    pub from: usize,
    pub to: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

pub struct Analysis {
    index: LineIndex<'static>,
    path: String,
    sources: Arc<HashMap<String, String>>,
    scan: scan::Scan,
    checked: types::Checked,
}

impl Analysis {
    pub fn new(source: &str) -> Self {
        Analysis::of_program(source.to_string(), String::new(), Arc::default())
    }

    pub fn with_modules(source: &str, path: &str, sources: &HashMap<String, String>) -> Self {
        Analysis::of_program(
            source.to_string(),
            path.to_string(),
            Arc::new(sources.clone()),
        )
    }

    pub fn of_program(source: String, path: String, sources: Arc<HashMap<String, String>>) -> Self {
        let context = types::Context {
            path: &path,
            sources: &sources,
        };
        let checked = types::Checked::new(&source, context);
        let scan = scan::scan(&source, &checked.field_accesses, context);
        Analysis {
            index: LineIndex::owned(source),
            path,
            sources,
            scan,
            checked,
        }
    }

    pub fn is_of(&self, source: &str, path: &str, sources: &Arc<HashMap<String, String>>) -> bool {
        self.source() == source && self.path == path && Arc::ptr_eq(&self.sources, sources)
    }

    fn source(&self) -> &str {
        self.index.source()
    }

    fn context(&self) -> types::Context<'_> {
        types::Context {
            path: &self.path,
            sources: &self.sources,
        }
    }

    pub fn missing_modules(&self) -> Vec<String> {
        self.checked.missing.clone()
    }

    pub fn tokens(&self) -> Vec<Token> {
        // Tokens are ordered, so one cursor converts all offsets in a single pass.
        let mut cursor = self.index.cursor();
        self.scan
            .tokens
            .iter()
            .map(|t| Token {
                from: cursor.utf16_of(t.range.start),
                to: cursor.utf16_of(t.range.end),
                class: t.class,
            })
            .collect()
    }

    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        let errors = self
            .checked
            .errors
            .iter()
            .map(|error| self.diagnostic(error, Severity::Error));
        // Warnings stay scoped to the main file; module errors still fail this program.
        let warnings = self
            .checked
            .warnings
            .iter()
            .filter(|warning| warning.span.is_none_or(|span| span.file == 0))
            .map(|warning| self.diagnostic(warning, Severity::Warning));
        let mut all: Vec<Diagnostic> = errors.chain(warnings).collect();
        // Stable sort keeps errors ahead of warnings at the same position.
        all.sort_by_key(|d| (d.line.is_none(), d.line, d.col));
        all
    }

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
        let files: &[SourceFile] = &self.checked.files;
        let span = SourceFile::in_main_file(files, found);
        let message = match files.get(found.file) {
            Some(file) if found.file > 0 => {
                format!(
                    "{}:{}:{}: {}",
                    types::slashed(&file.path),
                    found.line,
                    found.col,
                    error.message
                )
            }
            _ => error.message.clone(),
        };
        let start = self.index.byte_of(span);
        let token = self.scan.lexed.iter().find(|t| {
            t.range.start == start
                && t.range.end > start
                && !matches!(
                    t.kind,
                    LexKind::Token(LexedToken::Newline) | LexKind::Token(LexedToken::Eof)
                )
        });
        // Cross-file errors underline the entire `include` module path instead of a single segment.
        let path_end = (found.file > 0).then(|| {
            let lexed = &self.scan.lexed;
            let first = lexed.iter().position(|t| t.range.start == start)?;
            let path = lexed.get(first..)?.iter().take_while(|t| {
                matches!(
                    t.kind,
                    LexKind::Token(LexedToken::Ident(_) | LexedToken::Slash | LexedToken::Range)
                )
            });
            path.last().map(|t| t.range.end)
        });
        let end = match token {
            Some(_) if path_end.flatten().is_some() => path_end.flatten().unwrap_or(start),
            Some(token) => token.range.end,
            None => match self
                .source()
                .get(start..)
                .and_then(|rest| rest.chars().next())
            {
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

    pub fn outline(&self) -> Vec<OutlineItem> {
        let symbols = &self.scan.symbols;
        let mut children = vec![Vec::new(); symbols.len()];
        for (id, symbol) in symbols.iter().enumerate() {
            if let Some(list) = symbol.parent.and_then(|parent| children.get_mut(parent)) {
                list.push(id);
            }
        }
        symbols
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                s.module.is_none()
                    && matches!(
                        s.kind,
                        SymbolKind::Task
                            | SymbolKind::Shape
                            | SymbolKind::Kind
                            | SymbolKind::Alias
                            | SymbolKind::Pattern
                            | SymbolKind::Macro
                    )
            })
            .map(|(id, _)| self.outline_item(id, &children))
            .collect()
    }

    fn outline_item(&self, id: usize, children: &[Vec<usize>]) -> OutlineItem {
        let symbol = self.scan.symbols.get(id);
        let range = symbol
            .map(|s| self.index.utf16_range(&s.range))
            .unwrap_or_default();
        let name = symbol
            .map(|s| self.index.utf16_range(&s.name_range))
            .unwrap_or_default();
        OutlineItem {
            name: symbol.map(|s| s.name.clone()).unwrap_or_default(),
            kind: symbol.map_or(SymbolKind::Task, |s| s.kind),
            detail: symbol.map(|s| s.detail.clone()).unwrap_or_default(),
            from: range.start,
            to: range.end,
            name_from: name.start,
            name_to: name.end,
            children: children
                .get(id)
                .into_iter()
                .flatten()
                .map(|&child| self.outline_item(child, children))
                .collect(),
        }
    }

    pub fn definition(&self, offset: usize) -> Option<Location> {
        let byte = self.index.byte_of_utf16(offset);
        let tokens = &self.scan.tokens;
        let knowledge = infer::Knowledge::new(self.source(), &self.scan, &self.checked);
        // Prefer a symbol starting at the cursor over one ending there.
        let symbol = tokens
            .iter()
            .filter(|t| t.range.start <= byte && byte < t.range.end)
            .find_map(|t| knowledge.symbol_of(t))
            .or_else(|| {
                tokens
                    .iter()
                    .filter(|t| t.range.end == byte)
                    .find_map(|t| knowledge.symbol_of(t))
            })?;
        let symbol = self.scan.symbols.get(symbol)?;
        if symbol.kind == SymbolKind::Module {
            let module = self.scan.modules.iter().find(|m| {
                self.scan
                    .symbols
                    .get(m.symbol)
                    .is_some_and(|s| s.name_range == symbol.name_range)
            })?;
            if module.library {
                return None;
            }
            return module.source.as_ref().map(|_| Location {
                from: 0,
                to: 0,
                path: Some(module.path.clone()),
            });
        }
        match symbol.module.and_then(|m| self.scan.modules.get(m)) {
            Some(module) if module.library => None,
            Some(module) => {
                let source = module.source.as_deref()?;
                let range = LineIndex::new(source).utf16_range(&symbol.name_range);
                Some(Location {
                    from: range.start,
                    to: range.end,
                    path: Some(module.path.clone()),
                })
            }
            None => {
                let range = self.index.utf16_range(&symbol.name_range);
                Some(Location {
                    from: range.start,
                    to: range.end,
                    path: None,
                })
            }
        }
    }

    pub fn complete(&self, offset: usize) -> Completions {
        let byte = self.index.byte_of_utf16(offset);
        // An auto-closed `<` leaves `first<>`, which lexes as the `<>` operator; split it so type args complete.
        if byte > 0 && self.source().get(byte - 1..byte + 1) == Some("<>") {
            let apart = format!(
                "{} {}",
                self.source().get(..byte).unwrap_or_default(),
                self.source().get(byte..).unwrap_or_default()
            );
            return Analysis::of_program(apart, self.path.clone(), Arc::clone(&self.sources))
                .complete(offset);
        }
        let (items, from, to) = complete::complete(
            self.source(),
            &self.scan,
            &self.checked,
            self.context(),
            byte,
        );
        Completions {
            from: self.index.utf16_of(from),
            to: self.index.utf16_of(to),
            items,
        }
    }

    pub fn hover(&self, offset: usize) -> Option<Hover> {
        let knowledge = infer::Knowledge::new(self.source(), &self.scan, &self.checked);
        let ((code, doc), range) = hover::hover(&knowledge, self.index.byte_of_utf16(offset))?;
        let range = self.index.utf16_range(&range);
        Some(Hover {
            from: range.start,
            to: range.end,
            code,
            doc,
        })
    }
}
