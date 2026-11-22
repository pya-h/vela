#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use serde_json::json;
use vela_lsp::protocol::Id;
use vela_lsp::Server;

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
    let notif = server.handle_notification(
        "textDocument/didOpen",
        Some(json!({
            "textDocument": {
                "uri": uri,
                "version": 1,
                "text": "task main()\n    show(\"hello\")\nfi\n"
            }
        })),
    );
    assert!(notif.is_some());
    let diags = notif.unwrap();
    assert_eq!(diags.uri, uri);
    assert_eq!(diags.diagnostics.len(), 0);

    // Edit with error: reports diagnostic
    let notif = server.handle_notification(
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
    assert!(notif.is_some());
    let diags = notif.unwrap();
    assert!(!diags.diagnostics.is_empty());
    assert!(diags.diagnostics[0].message.contains("unknown_variable"));
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
