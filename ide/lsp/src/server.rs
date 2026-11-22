use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use vela_analysis::{
    Analysis, CompletionKind, LineIndex, OutlineItem, Severity, SymbolKind,
};

use crate::protocol::{
    CompletionItem, Diagnostic, DocumentSymbol, Hover, Id, Location, MarkupContent,
    Position, PublishDiagnosticsParams, Range, Response, ResponseError, SemanticTokens,
};
use crate::semantic::{TOKEN_MODIFIERS, TOKEN_TYPES, encode_tokens};

pub struct Document {
    pub uri: String,
    pub path: String,
    pub version: Option<i64>,
    pub text: String,
    pub analysis: Analysis,
    pub index: LineIndex<'static>,
}

pub struct Server {
    pub documents: HashMap<String, Document>,
    pub workspace_root: Option<PathBuf>,
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}

impl Server {
    pub fn new() -> Self {
        Server {
            documents: HashMap::new(),
            workspace_root: None,
        }
    }

    pub fn handle_request(&mut self, id: Id, method: &str, params: Option<Value>) -> Response {
        match method {
            "initialize" => self.handle_initialize(id, params),
            "shutdown" => Response {
                jsonrpc: "2.0",
                id,
                result: Some(Value::Null),
                error: None,
            },
            "textDocument/completion" => self.handle_completion(id, params),
            "textDocument/hover" => self.handle_hover(id, params),
            "textDocument/definition" => self.handle_definition(id, params),
            "textDocument/documentSymbol" => self.handle_document_symbol(id, params),
            "textDocument/semanticTokens/full" => self.handle_semantic_tokens(id, params),
            _ => Response {
                jsonrpc: "2.0",
                id,
                result: None,
                error: Some(ResponseError {
                    code: -32601,
                    message: format!("method not found: {}", method),
                }),
            },
        }
    }

    pub fn handle_notification(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Option<PublishDiagnosticsParams> {
        match method {
            "textDocument/didOpen" => self.handle_did_open(params),
            "textDocument/didChange" => self.handle_did_change(params),
            "textDocument/didClose" => {
                self.handle_did_close(params);
                None
            }
            _ => None,
        }
    }

    fn handle_initialize(&mut self, id: Id, params: Option<Value>) -> Response {
        if let Some(params) = params
            && let Some(root_uri) = params.get("rootUri").and_then(|v| v.as_str())
        {
            self.workspace_root = uri_to_path(root_uri);
        }

        let capabilities = json!({
            "capabilities": {
                "textDocumentSync": {
                    "openClose": true,
                    "change": 1
                },
                "completionProvider": {
                    "triggerCharacters": [".", "@", "<", ">", ":"]
                },
                "hoverProvider": true,
                "definitionProvider": true,
                "documentSymbolProvider": true,
                "semanticTokensProvider": {
                    "legend": {
                        "tokenTypes": TOKEN_TYPES,
                        "tokenModifiers": TOKEN_MODIFIERS
                    },
                    "full": true
                }
            },
            "serverInfo": {
                "name": "vela-lsp",
                "version": "0.1.0"
            }
        });

        Response {
            jsonrpc: "2.0",
            id,
            result: Some(capabilities),
            error: None,
        }
    }

    fn handle_did_open(&mut self, params: Option<Value>) -> Option<PublishDiagnosticsParams> {
        let params = params?;
        let doc = params.get("textDocument")?;
        let uri = doc.get("uri")?.as_str()?.to_string();
        let version = doc.get("version").and_then(|v| v.as_i64());
        let text = doc.get("text")?.as_str()?.to_string();

        let path = uri_to_path(&uri)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| uri.clone());

        let (analysis, index) = self.analyze_text(&text, &path);

        let diagnostics = build_diagnostics(&analysis, &index);

        self.documents.insert(
            uri.clone(),
            Document {
                uri: uri.clone(),
                path,
                version,
                text,
                analysis,
                index,
            },
        );

        Some(PublishDiagnosticsParams {
            uri,
            version,
            diagnostics,
        })
    }

    fn handle_did_change(&mut self, params: Option<Value>) -> Option<PublishDiagnosticsParams> {
        let params = params?;
        let doc = params.get("textDocument")?;
        let uri = doc.get("uri")?.as_str()?.to_string();
        let version = doc.get("version").and_then(|v| v.as_i64());
        let changes = params.get("contentChanges")?.as_array()?;
        let first_change = changes.first()?;
        let text = first_change.get("text")?.as_str()?.to_string();

        let path = uri_to_path(&uri)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| uri.clone());

        let (analysis, index) = self.analyze_text(&text, &path);

        let diagnostics = build_diagnostics(&analysis, &index);

        self.documents.insert(
            uri.clone(),
            Document {
                uri: uri.clone(),
                path,
                version,
                text,
                analysis,
                index,
            },
        );

        Some(PublishDiagnosticsParams {
            uri,
            version,
            diagnostics,
        })
    }

    fn handle_did_close(&mut self, params: Option<Value>) {
        if let Some(params) = params
            && let Some(doc) = params.get("textDocument")
            && let Some(uri) = doc.get("uri").and_then(|v| v.as_str())
        {
            self.documents.remove(uri);
        }
    }

    fn handle_completion(&self, id: Id, params: Option<Value>) -> Response {
        let Some((doc, offset)) = self.lookup_position(params.as_ref()) else {
            return Response {
                jsonrpc: "2.0",
                id,
                result: Some(json!([])),
                error: None,
            };
        };

        let completions = doc.analysis.complete(offset);
        let items: Vec<CompletionItem> = completions
            .items
            .into_iter()
            .map(|item| {
                let kind = match item.kind {
                    CompletionKind::Keyword => 14,
                    CompletionKind::Snippet => 15,
                    CompletionKind::Task | CompletionKind::BuiltinFn => 3,
                    CompletionKind::Variable | CompletionKind::Parameter | CompletionKind::Region => 6,
                    CompletionKind::Field => 5,
                    CompletionKind::Method => 2,
                    CompletionKind::Variant => 4,
                    CompletionKind::Type => 7,
                    CompletionKind::Module => 9,
                };
                let (insert_text, insert_format) = if let Some(snip) = item.snippet {
                    (Some(snip), 2)
                } else {
                    (item.apply.or(Some(item.label.clone())), 1)
                };
                CompletionItem {
                    label: item.label,
                    kind: Some(kind),
                    detail: if item.detail.is_empty() {
                        None
                    } else {
                        Some(item.detail)
                    },
                    documentation: None,
                    insert_text,
                    insert_text_format: Some(insert_format),
                }
            })
            .collect();

        Response {
            jsonrpc: "2.0",
            id,
            result: Some(json!(items)),
            error: None,
        }
    }

    fn handle_hover(&self, id: Id, params: Option<Value>) -> Response {
        let Some((doc, offset)) = self.lookup_position(params.as_ref()) else {
            return Response {
                jsonrpc: "2.0",
                id,
                result: Some(Value::Null),
                error: None,
            };
        };

        let Some(hover) = doc.analysis.hover(offset) else {
            return Response {
                jsonrpc: "2.0",
                id,
                result: Some(Value::Null),
                error: None,
            };
        };

        let (start_line, start_col) = doc.index.line_col_utf16(hover.from);
        let (end_line, end_col) = doc.index.line_col_utf16(hover.to);

        let markdown = if hover.doc.is_empty() {
            format!("```vela\n{}\n```", hover.code)
        } else {
            format!("```vela\n{}\n```\n\n{}", hover.code, hover.doc)
        };

        let result = Hover {
            contents: MarkupContent {
                kind: "markdown",
                value: markdown,
            },
            range: Some(Range {
                start: Position {
                    line: u32::try_from(start_line).unwrap_or(0),
                    character: u32::try_from(start_col).unwrap_or(0),
                },
                end: Position {
                    line: u32::try_from(end_line).unwrap_or(0),
                    character: u32::try_from(end_col).unwrap_or(0),
                },
            }),
        };

        Response {
            jsonrpc: "2.0",
            id,
            result: Some(json!(result)),
            error: None,
        }
    }

    fn handle_definition(&self, id: Id, params: Option<Value>) -> Response {
        let Some((doc, offset)) = self.lookup_position(params.as_ref()) else {
            return Response {
                jsonrpc: "2.0",
                id,
                result: Some(Value::Null),
                error: None,
            };
        };

        let Some(loc) = doc.analysis.definition(offset) else {
            return Response {
                jsonrpc: "2.0",
                id,
                result: Some(Value::Null),
                error: None,
            };
        };

        let (target_uri, (start_line, start_col), (end_line, end_col)) = match loc.path {
            None => {
                let (sl, sc) = doc.index.line_col_utf16(loc.from);
                let (el, ec) = doc.index.line_col_utf16(loc.to);
                (doc.uri.clone(), (sl, sc), (el, ec))
            }
            Some(path) => {
                let resolved = self.resolve_module_path(&path, &doc.path);
                let file_uri = path_to_uri(&resolved);
                if let Ok(content) = fs::read_to_string(&resolved) {
                    let idx = LineIndex::new(&content);
                    let (sl, sc) = idx.line_col_utf16(loc.from);
                    let (el, ec) = idx.line_col_utf16(loc.to);
                    (file_uri, (sl, sc), (el, ec))
                } else {
                    (file_uri, (0, 0), (0, 0))
                }
            }
        };

        let location = Location {
            uri: target_uri,
            range: Range {
                start: Position {
                    line: u32::try_from(start_line).unwrap_or(0),
                    character: u32::try_from(start_col).unwrap_or(0),
                },
                end: Position {
                    line: u32::try_from(end_line).unwrap_or(0),
                    character: u32::try_from(end_col).unwrap_or(0),
                },
            },
        };

        Response {
            jsonrpc: "2.0",
            id,
            result: Some(json!(location)),
            error: None,
        }
    }

    fn handle_document_symbol(&self, id: Id, params: Option<Value>) -> Response {
        let Some(uri) = params
            .as_ref()
            .and_then(|p| p.get("textDocument"))
            .and_then(|d| d.get("uri"))
            .and_then(|u| u.as_str())
        else {
            return Response {
                jsonrpc: "2.0",
                id,
                result: Some(json!([])),
                error: None,
            };
        };

        let Some(doc) = self.documents.get(uri) else {
            return Response {
                jsonrpc: "2.0",
                id,
                result: Some(json!([])),
                error: None,
            };
        };

        let outline = doc.analysis.outline();
        let symbols: Vec<DocumentSymbol> = outline
            .into_iter()
            .map(|item| map_outline_item(&item, &doc.index))
            .collect();

        Response {
            jsonrpc: "2.0",
            id,
            result: Some(json!(symbols)),
            error: None,
        }
    }

    fn handle_semantic_tokens(&self, id: Id, params: Option<Value>) -> Response {
        let Some(uri) = params
            .as_ref()
            .and_then(|p| p.get("textDocument"))
            .and_then(|d| d.get("uri"))
            .and_then(|u| u.as_str())
        else {
            return Response {
                jsonrpc: "2.0",
                id,
                result: Some(json!(SemanticTokens { data: Vec::new() })),
                error: None,
            };
        };

        let Some(doc) = self.documents.get(uri) else {
            return Response {
                jsonrpc: "2.0",
                id,
                result: Some(json!(SemanticTokens { data: Vec::new() })),
                error: None,
            };
        };

        let tokens = doc.analysis.tokens();
        let data = encode_tokens(&tokens, &doc.index);

        Response {
            jsonrpc: "2.0",
            id,
            result: Some(json!(SemanticTokens { data })),
            error: None,
        }
    }

    fn lookup_position<'a>(&'a self, params: Option<&Value>) -> Option<(&'a Document, usize)> {
        let params = params?;
        let doc_val = params.get("textDocument")?;
        let uri = doc_val.get("uri")?.as_str()?;
        let pos_val = params.get("position")?;
        let line = pos_val.get("line")?.as_u64()?;
        let char_offset = pos_val.get("character")?.as_u64()?;

        let doc = self.documents.get(uri)?;
        let line = usize::try_from(line).ok()?;
        let col = usize::try_from(char_offset).ok()?;
        let offset = doc.index.offset_of_line_col_utf16(line, col);
        Some((doc, offset))
    }

    fn analyze_text(&self, text: &str, path: &str) -> (Analysis, LineIndex<'static>) {
        let mut sources = HashMap::new();
        let mut loaded = Analysis::with_modules(text, path, &sources);

        // Transitively load missing local modules from disk if available
        let mut missing = loaded.missing_modules();
        let mut iterations = 0;
        while !missing.is_empty() && iterations < 32 {
            iterations += 1;
            let mut added = false;
            for m in &missing {
                if sources.contains_key(m) {
                    continue;
                }
                let resolved = self.resolve_module_path(m, path);
                if let Ok(src) = fs::read_to_string(&resolved) {
                    sources.insert(m.clone(), src);
                    added = true;
                }
            }
            if !added {
                break;
            }
            loaded = Analysis::with_modules(text, path, &sources);
            missing = loaded.missing_modules();
        }

        let index = LineIndex::owned(text.to_string());
        (loaded, index)
    }

    fn resolve_module_path(&self, module_name: &str, current_file: &str) -> PathBuf {
        let candidate_rel = format!("{}.vela", module_name);
        if let Some(parent) = Path::new(current_file).parent() {
            let direct = parent.join(&candidate_rel);
            if direct.is_file() {
                return direct;
            }
        }
        if let Some(root) = &self.workspace_root {
            let in_root = root.join(&candidate_rel);
            if in_root.is_file() {
                return in_root;
            }
        }
        PathBuf::from(candidate_rel)
    }
}

fn build_diagnostics(analysis: &Analysis, index: &LineIndex<'_>) -> Vec<Diagnostic> {
    analysis
        .diagnostics()
        .into_iter()
        .map(|d| {
            let (sl, sc) = index.line_col_utf16(d.from);
            let (el, ec) = index.line_col_utf16(d.to);
            Diagnostic {
                range: Range {
                    start: Position {
                        line: u32::try_from(sl).unwrap_or(0),
                        character: u32::try_from(sc).unwrap_or(0),
                    },
                    end: Position {
                        line: u32::try_from(el).unwrap_or(0),
                        character: u32::try_from(ec).unwrap_or(0),
                    },
                },
                severity: Some(match d.severity {
                    Severity::Error => 1,
                    Severity::Warning => 2,
                }),
                message: d.message,
                source: Some("vela".to_string()),
            }
        })
        .collect()
}

fn map_outline_item(item: &OutlineItem, index: &LineIndex<'_>) -> DocumentSymbol {
    let (sl, sc) = index.line_col_utf16(item.from);
    let (el, ec) = index.line_col_utf16(item.to);
    let (nsl, nsc) = index.line_col_utf16(item.name_from);
    let (nel, nec) = index.line_col_utf16(item.name_to);

    let kind = match item.kind {
        SymbolKind::Task | SymbolKind::Macro => 12,
        SymbolKind::Shape => 23,
        SymbolKind::Kind => 10,
        SymbolKind::Alias | SymbolKind::TypeParam => 26,
        SymbolKind::Variant => 22,
        SymbolKind::Method => 6,
        SymbolKind::Field => 8,
        SymbolKind::Parameter | SymbolKind::Variable | SymbolKind::Region | SymbolKind::Pattern => 13,
        SymbolKind::Module => 2,
    };

    let children = if item.children.is_empty() {
        None
    } else {
        Some(
            item.children
                .iter()
                .map(|child| map_outline_item(child, index))
                .collect(),
        )
    };

    DocumentSymbol {
        name: item.name.clone(),
        detail: if item.detail.is_empty() {
            None
        } else {
            Some(item.detail.clone())
        },
        kind,
        range: Range {
            start: Position {
                line: u32::try_from(sl).unwrap_or(0),
                character: u32::try_from(sc).unwrap_or(0),
            },
            end: Position {
                line: u32::try_from(el).unwrap_or(0),
                character: u32::try_from(ec).unwrap_or(0),
            },
        },
        selection_range: Range {
            start: Position {
                line: u32::try_from(nsl).unwrap_or(0),
                character: u32::try_from(nsc).unwrap_or(0),
            },
            end: Position {
                line: u32::try_from(nel).unwrap_or(0),
                character: u32::try_from(nec).unwrap_or(0),
            },
        },
        children,
    }
}

fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // Windows file:///c:/... vs Unix file:///path
    let path_str = if rest.starts_with('/') && rest.get(2..3) == Some(":") {
        rest.get(1..)?
    } else {
        rest
    };
    Some(PathBuf::from(path_str))
}

fn path_to_uri(path: &Path) -> String {
    format!("file://{}", path.to_string_lossy())
}
