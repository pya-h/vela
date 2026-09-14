//! The JSON protocol and the raw memory functions, tested natively.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use serde_json::{Value, json};
use vela_analysis::Analysis;
use vela_analysis_wasm::{handle, vela_alloc, vela_free_response, vela_handle};

const SOURCE: &str = "task main()\n    bind x = 1\n    show(x)\nfi\n";

fn request(value: Value) -> Value {
    serde_json::from_str(&handle(&value.to_string())).unwrap()
}

#[test]
fn answers_each_query_in_order() {
    let response = request(json!({
        "source": SOURCE,
        "queries": [
            { "kind": "tokens" },
            { "kind": "diagnostics" },
            { "kind": "outline" },
            { "kind": "definition", "offset": 36 },
            { "kind": "complete", "offset": 16 },
            { "kind": "hover", "offset": 21 },
        ]
    }));
    let analysis = Analysis::new(SOURCE);
    let results = response["results"].as_array().unwrap();
    assert_eq!(results.len(), 6);
    assert_eq!(results[0], serde_json::to_value(analysis.tokens()).unwrap());
    assert_eq!(results[1], json!([]));
    assert_eq!(results[2], serde_json::to_value(analysis.outline()).unwrap());
    assert_eq!(results[3], json!({ "from": 21, "to": 22 }));
    assert_eq!(results[4], serde_json::to_value(analysis.complete(16)).unwrap());
    assert_eq!(results[5]["code"], "bind x: int");
}

#[test]
fn missing_results_are_null() {
    let response = request(json!({ "source": SOURCE, "queries": [{ "kind": "hover", "offset": 0 }, { "kind": "definition", "offset": 0 }] }));
    assert_eq!(response, json!({ "results": [null, null] }));
}

#[test]
fn language_needs_no_source() {
    let response = request(json!({ "queries": [{ "kind": "language" }] }));
    let language = &response["results"][0];
    assert!(language["keywords"].as_array().unwrap().contains(&json!("fi")));
    assert_eq!(language["blockCloser"], "fi");
    assert_eq!(language["lineComment"], "#");
    assert_eq!(language["brackets"][0], json!(["(", ")"]));
}

#[test]
fn malformed_requests_are_errors() {
    for bad in ["not json", "{}", r#"{"queries": [{"kind": "explode"}]}"#, r#"{"queries": [{"kind": "hover"}]}"#, r#"{"queries": [], "extra": 1}"#] {
        let response: Value = serde_json::from_str(&handle(bad)).unwrap();
        assert!(response["error"].as_str().unwrap().starts_with("invalid request: "), "{}: {}", bad, response);
    }
}

/// Sends `bytes` through the exported functions, as JavaScript does.
fn call_raw(bytes: &[u8]) -> Value {
    let ptr = vela_alloc(bytes.len());
    // SAFETY: `ptr` has room for `bytes.len()` bytes, and each pointer is used
    // as the functions require.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        let response = vela_handle(ptr, bytes.len());
        let len = u32::from_le_bytes(std::ptr::read_unaligned(response.cast::<[u8; 4]>())) as usize;
        let json = std::slice::from_raw_parts(response.add(4), len).to_vec();
        vela_free_response(response);
        serde_json::from_slice(&json).unwrap()
    }
}

#[test]
fn raw_functions_round_trip() {
    let body = json!({ "source": "task main()\n    show(\"é𝄞\")\nfi\n", "queries": [{ "kind": "tokens" }] }).to_string();
    let response = call_raw(body.as_bytes());
    assert!(response["results"][0].as_array().unwrap().len() > 5);
    // Empty and invalid requests.
    assert!(call_raw(b"")["error"].is_string());
    assert_eq!(call_raw(&[0xff, 0xfe])["error"], "invalid request: not UTF-8");
}
