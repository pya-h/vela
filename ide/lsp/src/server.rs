use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use vela_analysis::{
    Analysis, CompletionKind, LineIndex, OutlineItem, Severity, SymbolKind,
};

use crate::protocol::{
    CompletionItem, Diagnostic, DocumentSymbol, Hover, Id, Location, MarkupContent,
    Position, PublishDiagnosticsParams, Range, Response, ResponseError, SemanticTokens,
    TextEdit,
};
use crate::semantic::{TOKEN_MODIFIERS, TOKEN_TYPES, encode_tokens};

pub struct Document {
    pub uri: String,
    pub path: String,
    pub version: Option<i64>,
    pub text: String,
    pub analysis: Analysis,
    pub index: LineIndex<'static>,
    dependencies: HashSet<String>,
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
    ) -> Vec<PublishDiagnosticsParams> {
        match method {
            "textDocument/didOpen" => self.handle_did_open(params),
            "textDocument/didChange" => self.handle_did_change(params),
            "textDocument/didSave" => self.handle_did_save(params),
            "textDocument/didClose" => self.handle_did_close(params),
            _ => Vec::new(),
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
                    "change": 1,
                    "save": { "includeText": false }
                },
                "completionProvider": {
                    "triggerCharacters": [".", "@", "<", ">", ":", "/"]
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
                "version": env!("CARGO_PKG_VERSION")
            }
        });

        Response {
            jsonrpc: "2.0",
            id,
            result: Some(capabilities),
            error: None,
        }
    }

    fn handle_did_open(&mut self, params: Option<Value>) -> Vec<PublishDiagnosticsParams> {
        let Some(params) = params else {
            return Vec::new();
        };
        let Some(doc) = params.get("textDocument") else {
            return Vec::new();
        };
        let Some(uri) = doc.get("uri").and_then(|v| v.as_str()).map(str::to_string) else {
            return Vec::new();
        };
        let version = doc.get("version").and_then(|v| v.as_i64());
        let Some(text) = doc.get("text").and_then(|v| v.as_str()).map(str::to_string) else {
            return Vec::new();
        };

        self.upsert_document(uri, version, text)
    }

    fn handle_did_change(&mut self, params: Option<Value>) -> Vec<PublishDiagnosticsParams> {
        let Some(params) = params else {
            return Vec::new();
        };
        let Some(doc) = params.get("textDocument") else {
            return Vec::new();
        };
        let Some(uri) = doc.get("uri").and_then(|v| v.as_str()).map(str::to_string) else {
            return Vec::new();
        };
        let version = doc.get("version").and_then(|v| v.as_i64());
        let Some(changes) = params.get("contentChanges").and_then(|v| v.as_array()) else {
            return Vec::new();
        };
        let Some(last_change) = changes.last() else {
            return Vec::new();
        };
        let Some(text) = last_change.get("text").and_then(|v| v.as_str()).map(str::to_string) else {
            return Vec::new();
        };

        self.upsert_document(uri, version, text)
    }

    fn handle_did_save(&mut self, params: Option<Value>) -> Vec<PublishDiagnosticsParams> {
        let Some(uri) = params
            .as_ref()
            .and_then(|p| p.get("textDocument"))
            .and_then(|d| d.get("uri"))
            .and_then(|u| u.as_str())
            .map(str::to_string)
        else {
            return Vec::new();
        };
        let Some(existing) = self.documents.get(&uri) else {
            return Vec::new();
        };
        let version = existing.version;
        let text = existing.text.clone();
        self.upsert_document(uri, version, text)
    }

    fn handle_did_close(&mut self, params: Option<Value>) -> Vec<PublishDiagnosticsParams> {
        let Some(uri) = params
            .as_ref()
            .and_then(|p| p.get("textDocument"))
            .and_then(|d| d.get("uri"))
            .and_then(|u| u.as_str())
            .map(str::to_string)
        else {
            return Vec::new();
        };
        let removed = self.documents.remove(&uri);
        let mut updates = vec![PublishDiagnosticsParams {
            uri: uri.clone(),
            version: None,
            diagnostics: Vec::new(),
        }];
        if let Some(doc) = removed {
            updates.extend(self.refresh_dependents(&doc.path));
        }
        updates
    }

    fn upsert_document(
        &mut self,
        uri: String,
        version: Option<i64>,
        text: String,
    ) -> Vec<PublishDiagnosticsParams> {
        let path = uri_to_path(&uri)
            .map(|p| slashed(&p))
            .unwrap_or_else(|| uri.clone());

        let (analysis, index, dependencies) = self.analyze_text(&text, &path);
        let diagnostics = build_diagnostics(&analysis, &index);

        self.documents.insert(
            uri.clone(),
            Document {
                uri: uri.clone(),
                path: path.clone(),
                version,
                text,
                analysis,
                index,
                dependencies,
            },
        );

        let mut updates = vec![PublishDiagnosticsParams {
            uri,
            version,
            diagnostics,
        }];
        updates.extend(self.refresh_dependents(&path));
        updates
    }

    fn refresh_dependents(&mut self, changed_path: &str) -> Vec<PublishDiagnosticsParams> {
        let dependent_uris: Vec<String> = self
            .documents
            .iter()
            .filter(|(_, doc)| {
                !paths_match(&doc.path, changed_path)
                    && doc
                        .dependencies
                        .iter()
                        .any(|dep| paths_match(dep, changed_path))
            })
            .map(|(uri, _)| uri.clone())
            .collect();

        let mut updates = Vec::with_capacity(dependent_uris.len());
        for uri in dependent_uris {
            let Some(doc) = self.documents.get(&uri) else {
                continue;
            };
            let path = doc.path.clone();
            let text = doc.text.clone();
            let version = doc.version;
            let (analysis, index, dependencies) = self.analyze_text(&text, &path);
            let diagnostics = build_diagnostics(&analysis, &index);
            if let Some(slot) = self.documents.get_mut(&uri) {
                slot.analysis = analysis;
                slot.index = index;
                slot.dependencies = dependencies;
            }
            updates.push(PublishDiagnosticsParams {
                uri,
                version,
                diagnostics,
            });
        }
        updates
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
        let replace_range = range_of(&doc.index, completions.from, completions.to);
        let from_byte = doc.index.byte_of_utf16(completions.from);
        let to_byte = doc.index.byte_of_utf16(completions.to);
        let replacing = doc.text.get(from_byte..to_byte).unwrap_or_default();
        let dot_prefixed = replacing.starts_with('.');

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
                let (new_text, insert_format) = if let Some(snip) = item.snippet {
                    (convert_codemirror_snippet(&snip), 2)
                } else {
                    (item.apply.unwrap_or_else(|| item.label.clone()), 1)
                };
                // When the replacement span begins at `.` (pointer `^.member`), match filterText with `.`.
                let filter_text = if dot_prefixed && !item.label.starts_with('.') {
                    Some(format!(".{}", item.label))
                } else {
                    None
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
                    filter_text,
                    insert_text: Some(new_text.clone()),
                    insert_text_format: Some(insert_format),
                    text_edit: Some(TextEdit {
                        range: replace_range,
                        new_text,
                    }),
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
            range: Some(range_of(&doc.index, hover.from, hover.to)),
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

        let (target_uri, range) = match loc.path {
            None => (doc.uri.clone(), range_of(&doc.index, loc.from, loc.to)),
            Some(path) => {
                if let Some(open) = self.find_open_document(&path) {
                    (open.uri.clone(), range_of(&open.index, loc.from, loc.to))
                } else {
                    let resolved = self.resolve_module_path(&path, &doc.path);
                    let file_uri = path_to_uri(&resolved);
                    let range = if let Ok(content) = fs::read_to_string(&resolved) {
                        let idx = LineIndex::new(&content);
                        range_of(&idx, loc.from, loc.to)
                    } else {
                        Range {
                            start: Position { line: 0, character: 0 },
                            end: Position { line: 0, character: 0 },
                        }
                    };
                    (file_uri, range)
                }
            }
        };

        let location = Location {
            uri: target_uri,
            range,
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

    fn analyze_text(
        &self,
        text: &str,
        path: &str,
    ) -> (Analysis, LineIndex<'static>, HashSet<String>) {
        let mut sources = HashMap::new();
        let mut dependencies = HashSet::new();
        let mut loaded = Analysis::with_modules(text, path, &sources);

        let mut missing = loaded.missing_modules();
        let mut iterations = 0;
        while !missing.is_empty() && iterations < 32 {
            iterations += 1;
            let mut added = false;
            for m in &missing {
                dependencies.insert(m.clone());
                if sources.contains_key(m) {
                    continue;
                }
                if let Some(open) = self.find_open_document(m) {
                    sources.insert(m.clone(), open.text.clone());
                    added = true;
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
        (loaded, index, dependencies)
    }

    fn find_open_document(&self, module_path: &str) -> Option<&Document> {
        self.documents
            .values()
            .find(|doc| paths_match(&doc.path, module_path))
    }

    fn resolve_module_path(&self, module_path: &str, current_file: &str) -> PathBuf {
        let candidate = Path::new(module_path);
        if candidate.is_absolute() || candidate.is_file() {
            return candidate.to_path_buf();
        }
        let with_ext = if module_path.ends_with(".vela") {
            PathBuf::from(module_path)
        } else {
            PathBuf::from(format!("{}.vela", module_path))
        };
        if with_ext.is_file() {
            return with_ext;
        }
        if let Some(parent) = Path::new(current_file).parent() {
            let direct = parent.join(&with_ext);
            if direct.is_file() {
                return direct;
            }
        }
        if let Some(root) = &self.workspace_root {
            let in_root = root.join(&with_ext);
            if in_root.is_file() {
                return in_root;
            }
        }
        with_ext
    }
}

/// Converts CodeMirror snippet templates (`${name}`, `${}`) to LSP snippet syntax (`${1:name}`, `$2`, `$0`).
pub fn convert_codemirror_snippet(template: &str) -> String {
    let mut parsed: Vec<(bool, &str)> = Vec::new();
    let bytes = template.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes.get(i) == Some(&b'$') && bytes.get(i + 1) == Some(&b'{') {
            let start = i + 2;
            if let Some(rel_end) = template.get(start..).and_then(|s| s.find('}')) {
                let end = start + rel_end;
                let inner = template.get(start..end).unwrap_or_default();
                parsed.push((true, inner));
                i = end + 1;
                continue;
            }
        }
        let Some(ch) = template.get(i..).and_then(|s| s.chars().next()) else {
            break;
        };
        let next = i + ch.len_utf8();
        parsed.push((false, template.get(i..next).unwrap_or_default()));
        i = next;
    }

    let last_empty_idx = parsed
        .iter()
        .enumerate()
        .rev()
        .find(|(_, (is_ph, name))| *is_ph && name.is_empty())
        .map(|(idx, _)| idx);

    let mut out = String::with_capacity(template.len() + 8);
    let mut next_stop = 1usize;

    for (idx, (is_ph, text)) in parsed.into_iter().enumerate() {
        if !is_ph {
            if text == "$" {
                out.push_str("\\$");
            } else {
                out.push_str(text);
            }
            continue;
        }
        if text.is_empty() {
            if Some(idx) == last_empty_idx {
                out.push_str("$0");
            } else {
                out.push('$');
                out.push_str(&next_stop.to_string());
                next_stop += 1;
            }
        } else {
            out.push_str(&format!("${{{}:{}}}", next_stop, text));
            next_stop += 1;
        }
    }

    out
}

fn range_of(index: &LineIndex<'_>, from: usize, to: usize) -> Range {
    let (sl, sc) = index.line_col_utf16(from);
    let (el, ec) = index.line_col_utf16(to);
    Range {
        start: Position {
            line: u32::try_from(sl).unwrap_or(0),
            character: u32::try_from(sc).unwrap_or(0),
        },
        end: Position {
            line: u32::try_from(el).unwrap_or(0),
            character: u32::try_from(ec).unwrap_or(0),
        },
    }
}

fn build_diagnostics(analysis: &Analysis, index: &LineIndex<'_>) -> Vec<Diagnostic> {
    analysis
        .diagnostics()
        .into_iter()
        .map(|d| Diagnostic {
            range: range_of(index, d.from, d.to),
            severity: Some(match d.severity {
                Severity::Error => 1,
                Severity::Warning => 2,
            }),
            message: d.message,
            source: Some("vela".to_string()),
        })
        .collect()
}

fn map_outline_item(item: &OutlineItem, index: &LineIndex<'_>) -> DocumentSymbol {
    let kind = match item.kind {
        SymbolKind::Task | SymbolKind::Macro => 12,
        SymbolKind::Shape => 23,
        SymbolKind::Kind => 10,
        SymbolKind::Alias | SymbolKind::TypeParam => 26,
        SymbolKind::Variant => 22,
        SymbolKind::Method => 6,
        SymbolKind::Field => 8,
        SymbolKind::Parameter
        | SymbolKind::Variable
        | SymbolKind::Region
        | SymbolKind::Pattern => 13,
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
        range: range_of(index, item.from, item.to),
        selection_range: range_of(index, item.name_from, item.name_to),
        children,
    }
}

fn slashed(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn paths_match(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let pa = Path::new(a);
    let pb = Path::new(b);
    if let (Ok(ca), Ok(cb)) = (fs::canonicalize(pa), fs::canonicalize(pb)) {
        return ca == cb;
    }
    false
}

fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let decoded = percent_decode(rest);
    // Windows URIs look like `file:///c:/...`
    let path_str = if decoded.starts_with('/') && decoded.get(2..3) == Some(":") {
        decoded.get(1..)?
    } else {
        decoded.as_str()
    };
    Some(PathBuf::from(path_str))
}

fn path_to_uri(path: &Path) -> String {
    let raw = slashed(path);
    let encoded = percent_encode_path(&raw);
    if encoded.starts_with('/') {
        format!("file://{}", encoded)
    } else {
        format!("file:///{}", encoded)
    }
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes.get(i) == Some(&b'%')
            && let (Some(&h1), Some(&h2)) = (bytes.get(i + 1), bytes.get(i + 2))
            && let (Some(d1), Some(d2)) = (hex_val(h1), hex_val(h2))
        {
            out.push((d1 << 4) | d2);
            i += 3;
            continue;
        }
        if let Some(&b) = bytes.get(i) {
            out.push(b);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn percent_encode_path(input: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        let keep = byte.is_ascii_alphanumeric()
            || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~' | b':');
        if keep {
            out.push(char::from(byte));
        } else {
            out.push('%');
            let hi = HEX.get(usize::from(byte >> 4)).copied().unwrap_or(b'0');
            let lo = HEX.get(usize::from(byte & 0x0f)).copied().unwrap_or(b'0');
            out.push(char::from(hi));
            out.push(char::from(lo));
        }
    }
    out
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
