#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::io::{BufReader, Cursor};

use serde_json::json;
use vela_lsp::protocol::Id;
use vela_lsp::semantic::encode_tokens;
use vela_lsp::server::convert_codemirror_snippet;
use vela_lsp::{Server, Transport};
use vela_analysis::{LineIndex, Token, TokenClass};

#[test]
fn server_initializes_with_expected_capabilities() {
    let mut server = Server::new();
    let response = server.handle_request(
        Id::Number(1),
        "initialize",
        Some(json!({
            "rootUri": "file:///workspace"
        })),
    );

    assert_eq!(response.id, Id::Number(1));
    assert!(response.error.is_none());
    let res = response.result.unwrap();
    let capabilities = res.get("capabilities").unwrap();
    assert!(capabilities.get("completionProvider").is_some());
    assert!(capabilities.get("hoverProvider").is_some());
    assert!(capabilities.get("definitionProvider").is_some());
    assert!(capabilities.get("documentSymbolProvider").is_some());
    assert!(capabilities.get("semanticTokensProvider").is_some());
}

#[test]
fn diagnostics_reported_on_open_and_change() {
    let mut server = Server::new();
    let uri = "file:///test.vela";

    // Open valid document: no diagnostics
    let notifs = server.handle_notification(
        "textDocument/didOpen",
        Some(json!({
            "textDocument": {
                "uri": uri,
                "version": 1,
                "text": "task main()\n    show(\"hello\")\nfi\n"
            }
        })),
    );
    assert_eq!(notifs.len(), 1);
    let diags = &notifs[0];
    assert_eq!(diags.uri, uri);
    assert_eq!(diags.diagnostics.len(), 0);

    // Edit with error: reports diagnostic
    let notifs = server.handle_notification(
        "textDocument/didChange",
        Some(json!({
            "textDocument": {
                "uri": uri,
                "version": 2
            },
            "contentChanges": [{
                "text": "task main()\n    show(unknown_variable)\nfi\n"
            }]
        })),
    );
    assert_eq!(notifs.len(), 1);
    let diags = &notifs[0];
    assert!(!diags.diagnostics.is_empty());
    assert!(diags.diagnostics[0].message.contains("unknown_variable"));

    // Close document: clears diagnostics
    let closed = server.handle_notification(
        "textDocument/didClose",
        Some(json!({
            "textDocument": { "uri": uri }
        })),
    );
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].uri, uri);
    assert!(closed[0].diagnostics.is_empty());
}

#[test]
fn completion_offers_keywords_and_tasks() {
    let mut server = Server::new();
    let uri = "file:///test.vela";

    server.handle_notification(
        "textDocument/didOpen",
        Some(json!({
            "textDocument": {
                "uri": uri,
                "version": 1,
                "text": "task helper() -> int\n    <- 42\nfi\n\ntask main()\n    fixed x = hel\nfi\n"
            }
        })),
    );

    let response = server.handle_request(
        Id::Number(2),
        "textDocument/completion",
        Some(json!({
            "textDocument": { "uri": uri },
            "position": { "line": 5, "character": 17 }
        })),
    );

    assert_eq!(response.id, Id::Number(2));
    let items = response.result.unwrap();
    let arr = items.as_array().unwrap();
    assert!(arr.iter().any(|item| item.get("label").unwrap() == "helper"));
}

#[test]
fn hover_shows_type_and_signature() {
    let mut server = Server::new();
    let uri = "file:///test.vela";

    server.handle_notification(
        "textDocument/didOpen",
        Some(json!({
            "textDocument": {
                "uri": uri,
                "version": 1,
                "text": "task calculate(a: int, b: int) -> int\n    <- a + b\nfi\n"
            }
        })),
    );

    let response = server.handle_request(
        Id::Number(3),
        "textDocument/hover",
        Some(json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 7 }
        })),
    );

    assert_eq!(response.id, Id::Number(3));
    let result = response.result.unwrap();
    assert!(!result.is_null());
    let contents = result.get("contents").unwrap();
    let value = contents.get("value").unwrap().as_str().unwrap();
    assert!(value.contains("task calculate"));
}

#[test]
fn definition_locates_task() {
    let mut server = Server::new();
    let uri = "file:///test.vela";

    server.handle_notification(
        "textDocument/didOpen",
        Some(json!({
            "textDocument": {
                "uri": uri,
                "version": 1,
                "text": "task target_fn()\nfi\n\ntask main()\n    target_fn()\nfi\n"
            }
        })),
    );

    let response = server.handle_request(
        Id::Number(4),
        "textDocument/definition",
        Some(json!({
            "textDocument": { "uri": uri },
            "position": { "line": 4, "character": 8 }
        })),
    );

    assert_eq!(response.id, Id::Number(4));
    let result = response.result.unwrap();
    assert!(!result.is_null());
    let target_uri = result.get("uri").unwrap().as_str().unwrap();
    assert_eq!(target_uri, uri);
    let range = result.get("range").unwrap();
    let start = range.get("start").unwrap();
    assert_eq!(start.get("line").unwrap(), 0);
}

#[test]
fn document_symbols_outline_hierarchy() {
    let mut server = Server::new();
    let uri = "file:///test.vela";

    server.handle_notification(
        "textDocument/didOpen",
        Some(json!({
            "textDocument": {
                "uri": uri,
                "version": 1,
                "text": "shape Point\n    x: int\n    y: int\nfi\n\ntask main()\nfi\n"
            }
        })),
    );

    let response = server.handle_request(
        Id::Number(5),
        "textDocument/documentSymbol",
        Some(json!({
            "textDocument": { "uri": uri }
        })),
    );

    assert_eq!(response.id, Id::Number(5));
    let result = response.result.unwrap();
    let symbols = result.as_array().unwrap();
    assert!(symbols.iter().any(|s| s.get("name").unwrap() == "Point"));
    assert!(symbols.iter().any(|s| s.get("name").unwrap() == "main"));
}

#[test]
fn semantic_tokens_returns_delta_encoded_stream() {
    let mut server = Server::new();
    let uri = "file:///test.vela";

    server.handle_notification(
        "textDocument/didOpen",
        Some(json!({
            "textDocument": {
                "uri": uri,
                "version": 1,
                "text": "task main()\n    fixed x = 123\nfi\n"
            }
        })),
    );

    let response = server.handle_request(
        Id::Number(6),
        "textDocument/semanticTokens/full",
        Some(json!({
            "textDocument": { "uri": uri }
        })),
    );

    assert_eq!(response.id, Id::Number(6));
    let result = response.result.unwrap();
    let data = result.get("data").unwrap().as_array().unwrap();
    assert!(!data.is_empty());
    // In LSP semantic tokens, data length is always a multiple of 5
    assert_eq!(data.len() % 5, 0);
}

#[test]
fn converts_codemirror_snippets_to_lsp() {
    assert_eq!(
        convert_codemirror_snippet("task ${name}(${})\n\t${}\nfi"),
        "task ${1:name}($2)\n\t$0\nfi"
    );
    assert_eq!(
        convert_codemirror_snippet("when ${value} is {${name}}\n\t${}\nfi"),
        "when ${1:value} is {${2:name}}\n\t$0\nfi"
    );
    assert_eq!(convert_codemirror_snippet("x: ${}"), "x: $0");
    assert_eq!(
        convert_codemirror_snippet("added(${value}) at ${r}"),
        "added(${1:value}) at ${2:r}"
    );
}

#[test]
fn pointer_completions_include_text_edit_over_dot() {
    let mut server = Server::new();
    let uri = "file:///ptr.vela";

    server.handle_notification(
        "textDocument/didOpen",
        Some(json!({
            "textDocument": {
                "uri": uri,
                "version": 1,
                "text": "shape Point\n    x: int\nfi\n\ntask main()\n    region r\n    fixed p = new Point(Point { x: 1 }) at r\n    show(p.)\nfi\n"
            }
        })),
    );

    let response = server.handle_request(
        Id::Number(7),
        "textDocument/completion",
        Some(json!({
            "textDocument": { "uri": uri },
            "position": { "line": 7, "character": 11 }
        })),
    );
    let items = response.result.unwrap();
    let arr = items.as_array().unwrap();
    let x_item = arr.iter().find(|it| it.get("label").unwrap() == "x").unwrap();
    assert_eq!(x_item.get("filterText").unwrap(), ".x");
    let edit = x_item.get("textEdit").unwrap();
    assert_eq!(edit.get("newText").unwrap(), "^.x");
    assert_eq!(edit["range"]["start"]["character"], 10);
    assert_eq!(edit["range"]["end"]["character"], 11);
}

#[test]
fn cross_file_modules_work_in_memory_and_from_disk() {
    let mut server = Server::new();
    let main_uri = "file:///my%20app/main.vela";
    let helper_uri = "file:///my%20app/sub/helper.vela";

    // Open helper in memory first (even with spaces in directory path)
    server.handle_notification(
        "textDocument/didOpen",
        Some(json!({
            "textDocument": {
                "uri": helper_uri,
                "version": 1,
                "text": "task greet() -> int\n    <- 7\nfi\n"
            }
        })),
    );

    // Open main referencing sub/helper
    let open_main = server.handle_notification(
        "textDocument/didOpen",
        Some(json!({
            "textDocument": {
                "uri": main_uri,
                "version": 1,
                "text": "include sub/helper\n\ntask main()\n    show(helper.greet())\nfi\n"
            }
        })),
    );
    assert_eq!(open_main[0].diagnostics.len(), 0);

    // Go-to-definition from main jumps into open helper URI
    let def = server.handle_request(
        Id::Number(8),
        "textDocument/definition",
        Some(json!({
            "textDocument": { "uri": main_uri },
            "position": { "line": 3, "character": 17 }
        })),
    );
    let loc = def.result.unwrap();
    assert_eq!(loc["uri"], helper_uri);
    assert_eq!(loc["range"]["start"]["line"], 0);

    // Editing helper in memory to remove `greet` refreshes diagnostics in main!
    let updates = server.handle_notification(
        "textDocument/didChange",
        Some(json!({
            "textDocument": { "uri": helper_uri, "version": 2 },
            "contentChanges": [{ "text": "task other() -> int\n    <- 1\nfi\n" }]
        })),
    );
    let main_update = updates.iter().find(|u| u.uri == main_uri).unwrap();
    assert!(!main_update.diagnostics.is_empty());
}

#[test]
fn real_disk_modules_resolve_without_double_extension() {
    let dir = std::env::temp_dir().join(format!("vela_lsp_test_{}", std::process::id()));
    let sub = dir.join("shop");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("cart.vela"), "task total() -> int\n    <- 10\nfi\n").unwrap();
    let main_path = dir.join("shop.vela");

    let mut server = Server::new();
    let main_uri = format!("file://{}", main_path.to_string_lossy().replace('\\', "/"));
    let updates = server.handle_notification(
        "textDocument/didOpen",
        Some(json!({
            "textDocument": {
                "uri": main_uri,
                "version": 1,
                "text": "include shop/cart\n\ntask main()\n    show(cart.total())\nfi\n"
            }
        })),
    );

    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(updates[0].diagnostics.len(), 0);
}

#[test]
fn multiline_tokens_split_across_lines_for_lsp() {
    let source = "ab\ncd";
    let index = LineIndex::new(source);
    let tokens = vec![Token {
        from: 0,
        to: 5,
        class: TokenClass::String,
    }];
    let encoded = encode_tokens(&tokens, &index);
    // Line 0: deltaLine=0, deltaStart=0, len=2; Line 1: deltaLine=1, deltaStart=0, len=2
    assert_eq!(encoded, vec![0, 0, 2, 8, 0, 1, 0, 2, 8, 0]);
}

#[test]
fn transport_handles_framed_messages_and_case_insensitive_headers() {
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
    let wire = format!("content-length: {}\r\n\r\n{}", body.len(), body);
    let reader = BufReader::new(Cursor::new(wire.into_bytes()));
    let mut out = Vec::new();
    let mut transport = Transport::new(reader, &mut out);

    let msg = transport.read_message().unwrap().unwrap();
    assert_eq!(msg.method.as_deref(), Some("initialize"));
    assert_eq!(msg.id, Some(Id::Number(1)));

    let mut server = Server::new();
    let resp = server.handle_request(msg.id.unwrap(), "initialize", msg.params);
    transport.write_response(&resp).unwrap();
    let written = String::from_utf8(out).unwrap();
    assert!(written.starts_with("Content-Length: "));
    assert!(written.contains("vela-lsp"));
}
