#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use serde_json::{Value, json};
use vela_analysis::Analysis;
use vela_analysis_wasm::{analyses_made, handle, vela_alloc, vela_free_response, vela_handle};

const SOURCE: &str = "task main()\n    fixed x = 1\n    show(x)\nfi\n";

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
            { "kind": "definition", "offset": 37 },
            { "kind": "complete", "offset": 16 },
            { "kind": "hover", "offset": 22 },
        ]
    }));
    let analysis = Analysis::new(SOURCE);
    let results = response["results"].as_array().unwrap();
    assert_eq!(results.len(), 6);
    assert_eq!(results[0], serde_json::to_value(analysis.tokens()).unwrap());
    assert_eq!(results[1], json!([]));
    assert_eq!(results[2], serde_json::to_value(analysis.outline()).unwrap());
    assert_eq!(results[3], json!({ "from": 22, "to": 23 }));
    assert_eq!(results[4], serde_json::to_value(analysis.complete(16)).unwrap());
    assert_eq!(results[5]["code"], "fixed x: int");
}

#[test]
fn modules_given_once_are_kept_for_the_requests_after() {
    // Each test runs on a thread of its own, so this one starts with nothing kept.
    let main = "include lib/a\n\ntask main()\n    a.hello()\nfi\n";
    let asked = |extra: Value| {
        let mut body = json!({ "source": main, "path": "main.vela", "queries": [{ "kind": "diagnostics" }, { "kind": "missing-modules" }] });
        if let (Some(body), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
            body.extend(extra.clone());
        }
        request(body)
    };
    let response = asked(json!({ "same-modules": true }));
    assert_eq!(response["resend"], json!(true), "{}", response);
    assert!(response["error"].as_str().unwrap().contains("send them"));
    let modules = json!({ "lib/a.vela": "task hello()\n    show(1)\nfi\n" });
    assert_eq!(asked(json!({ "modules": modules })), json!({ "results": [[], []] }));
    assert_eq!(asked(json!({ "same-modules": true })), json!({ "results": [[], []] }));
    let hover = request(json!({ "source": main, "path": "main.vela", "same-modules": true, "queries": [{ "kind": "hover", "offset": 34 }] }));
    assert_eq!(hover["results"][0]["code"], "task a.hello()", "{}", hover);
    assert_eq!(asked(json!({}))["results"][1], json!(["lib/a.vela"]));
    assert_eq!(asked(json!({ "same-modules": true })), json!({ "results": [[], []] }));
    // Text given alone, as a tooltip's code is, doesn't replace the file's
    // analysis, which answers the next request about the file.
    let made = analyses_made();
    let tip = json!({ "source": "task tip()\nfi\n", "queries": [{ "kind": "tokens" }] });
    request(tip.clone());
    assert_eq!(asked(json!({ "same-modules": true })), json!({ "results": [[], []] }));
    assert_eq!(analyses_made(), made + 1);
    request(tip);
    assert_eq!(analyses_made(), made + 1);
    let changed = json!({ "lib/a.vela": "task other()\nfi\n" });
    assert_ne!(asked(json!({ "modules": changed }))["results"][0], json!([]));
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
    assert_eq!(language["lineComment"], ";");
    assert_eq!(language["brackets"][0], json!(["(", ")"]));
}

#[test]
fn malformed_requests_are_errors() {
    for bad in ["not json", "{}", r#"{"queries": [{"kind": "explode"}]}"#, r#"{"queries": [{"kind": "hover"}]}"#, r#"{"queries": [], "extra": 1}"#] {
        let response: Value = serde_json::from_str(&handle(bad)).unwrap();
        assert!(response["error"].as_str().unwrap().starts_with("invalid request: "), "{}: {}", bad, response);
    }
}

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
    assert!(call_raw(b"")["error"].is_string());
    assert_eq!(call_raw(&[0xff, 0xfe])["error"], "invalid request: not UTF-8");
}

#[test]
fn offsets_past_the_end_are_clamped() {
    let response = request(json!({
        "source": SOURCE,
        "queries": [
            { "kind": "complete", "offset": 4294967295u32 },
            { "kind": "hover", "offset": 4294967295u32 },
            { "kind": "definition", "offset": 4294967295u32 },
        ]
    }));
    let results = response["results"].as_array().unwrap_or_else(|| panic!("{}", response));
    let length = SOURCE.encode_utf16().count() as u64;
    assert_eq!((results[0]["from"].as_u64(), results[0]["to"].as_u64()), (Some(length), Some(length)));
    assert_eq!((&results[1], &results[2]), (&Value::Null, &Value::Null));
}

#[test]
fn a_type_deeper_than_the_compiler_allows_gets_an_answer() {
    let source = format!("task f(a: {}int)\n    show(1)\nfi\n", "^".repeat(12_000));
    let response = request(json!({ "source": source, "queries": [{ "kind": "hover", "offset": 8 }, { "kind": "diagnostics" }] }));
    let results = response["results"].as_array().unwrap_or_else(|| panic!("{}", response));
    assert!(results[1][0]["message"].as_str().is_some_and(|m| m.contains("nested too deeply")), "{}", results[1]);
}
