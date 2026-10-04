//! Vela Studio's language analysis as a WebAssembly module, so the compiler's
//! own lexer, parser, and checker run in the editor page on every keystroke.
//!
//! The module speaks JSON through three exported functions, with no bindings
//! generator (all sizes and pointers are 32-bit on wasm32):
//!
//! - `vela_alloc(len) -> ptr` reserves `len` bytes for a request.
//! - `vela_handle(ptr, len) -> ptr` takes the UTF-8 JSON request written
//!   there (and frees it), and returns a response: a 4-byte little-endian
//!   length followed by that many bytes of UTF-8 JSON.
//! - `vela_free_response(ptr)` frees a response.
//!
//! A request is `{"source": "…", "queries": [{"kind": "tokens"}, {"kind":
//! "complete", "offset": 12}, …]}`, and its response is `{"results": […]}`
//! with one result per query, in order, or `{"error": "…"}` if the request is
//! malformed. Offsets are UTF-16, like JavaScript string indices. A request
//! may also give the file's `path` and `modules`, the text of other files of
//! its program by path; the `missing-modules` query lists the ones an `include`
//! needs that weren't given. Instead of `modules`, `"same-modules": true` uses
//! the ones the last request gave; a module that has none kept (it was just
//! started) answers `{"error": "…", "resend": true}`, and the request is sent
//! again with them.
//!
//! The module keeps the last analysis, so queries about the same text, path,
//! and modules (a hover, a completion, a definition) don't analyze it again.
//! One of text given alone, with neither modules nor `same-modules`, is kept
//! apart, so that analyzing a snippet doesn't replace the file's.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};
use vela_analysis::Analysis;

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum Query {
    Tokens,
    Diagnostics,
    Outline,
    Definition { offset: usize },
    Complete { offset: usize },
    Hover { offset: usize },
    /// The paths of the files the program includes that `modules` didn't give.
    MissingModules,
    /// Keywords and block words; ignores the source.
    Language,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    #[serde(default)]
    source: String,
    /// The file's path, `/`-separated; empty for a file without one.
    #[serde(default)]
    path: String,
    /// The text of the program's other files, by path.
    #[serde(default)]
    modules: Option<HashMap<String, String>>,
    /// The modules are the ones the last request that gave them gave.
    #[serde(default, rename = "same-modules")]
    same_modules: bool,
    queries: Vec<Query>,
}

/// What the module keeps between requests (it runs on one thread).
#[derive(Default)]
struct Kept {
    /// The modules the last request gave, if one did.
    modules: Option<Arc<HashMap<String, String>>>,
    /// No modules, shared by every request of text alone, so its kept
    /// analysis answers the next one about the same text.
    none: Arc<HashMap<String, String>>,
    /// The last analysis made of a file of a program, given modules.
    analysis: Option<Analysis>,
    /// The last one made of text alone, as the code in a hover's tooltip is,
    /// apart so that it doesn't replace the file's.
    bare: Option<Analysis>,
    /// How many analyses were made, which the tests read.
    made: usize,
}

thread_local! {
    static KEPT: RefCell<Kept> = RefCell::new(Kept::default());
}

/// Answers a JSON request (see the crate documentation) with a JSON response.
pub fn handle(request: &str) -> String {
    let request: Request = match serde_json::from_str(request) {
        Ok(request) => request,
        Err(error) => return json!({ "error": format!("invalid request: {}", error) }).to_string(),
    };
    let bare = request.modules.is_none() && !request.same_modules;
    let modules = match (request.modules, request.same_modules) {
        (Some(modules), _) => {
            let modules = Arc::new(modules);
            KEPT.with_borrow_mut(|kept| kept.modules = Some(Arc::clone(&modules)));
            modules
        }
        (None, true) => match KEPT.with_borrow(|kept| kept.modules.clone()) {
            Some(modules) => modules,
            None => return json!({ "error": "no modules kept; send them", "resend": true }).to_string(),
        },
        (None, false) => KEPT.with_borrow(|kept| Arc::clone(&kept.none)),
    };
    KEPT.with_borrow_mut(|kept| {
        let slot = if bare { &mut kept.bare } else { &mut kept.analysis };
        let fresh = !slot.as_ref().is_some_and(|analysis| analysis.is_of(&request.source, &request.path, &modules));
        if fresh {
            // The old one goes first, so two are never held at once.
            *slot = None;
            *slot = Some(Analysis::of_program(request.source, request.path, modules));
            kept.made += 1;
        }
        let Some(analysis) = slot.as_ref() else { return json!({ "error": "no analysis" }).to_string() };
        answer(analysis, &request.queries)
    })
}

/// How many analyses this thread's module made, for the tests to see which
/// requests were answered from a kept one.
#[doc(hidden)]
pub fn analyses_made() -> usize {
    KEPT.with_borrow(|kept| kept.made)
}

/// The results of `queries` about one analysis, as the response.
fn answer(analysis: &Analysis, queries: &[Query]) -> String {
    let results: Vec<Value> = queries
        .iter()
        .map(|query| {
            let result = match query {
                Query::Tokens => serde_json::to_value(analysis.tokens()),
                Query::Diagnostics => serde_json::to_value(analysis.diagnostics()),
                Query::Outline => serde_json::to_value(analysis.outline()),
                Query::Definition { offset } => serde_json::to_value(analysis.definition(*offset)),
                Query::Complete { offset } => serde_json::to_value(analysis.complete(*offset)),
                Query::Hover { offset } => serde_json::to_value(analysis.hover(*offset)),
                Query::MissingModules => serde_json::to_value(analysis.missing_modules()),
                Query::Language => serde_json::to_value(vela_analysis::language()),
            };
            result.unwrap_or_else(|error| json!({ "error": error.to_string() }))
        })
        .collect();
    json!({ "results": results }).to_string()
}

/// Reserves `len` bytes for a request and returns their address.
#[unsafe(no_mangle)]
pub extern "C" fn vela_alloc(len: usize) -> *mut u8 {
    let mut buffer = Vec::<u8>::with_capacity(len.max(1));
    let ptr = buffer.as_mut_ptr();
    std::mem::forget(buffer);
    ptr
}

/// Handles the request in the `len` bytes at `ptr`, frees them, and returns
/// the response (see the crate documentation).
///
/// # Safety
///
/// `ptr` must come from `vela_alloc(len)` with the same `len`, with all `len`
/// bytes written, and must not be used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vela_handle(ptr: *mut u8, len: usize) -> *mut u8 {
    // SAFETY: the caller guarantees `ptr` is a `vela_alloc(len)` buffer, which
    // has capacity `len.max(1)`, with `len` initialized bytes.
    let request = unsafe { Vec::from_raw_parts(ptr, len, len.max(1)) };
    let response = match std::str::from_utf8(&request) {
        Ok(text) => handle(text),
        Err(_) => json!({ "error": "invalid request: not UTF-8" }).to_string(),
    };
    into_response(response)
}

/// Frees a response returned by `vela_handle`.
///
/// # Safety
///
/// `ptr` must come from `vela_handle` and must not be used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vela_free_response(ptr: *mut u8) {
    // SAFETY: a response starts with its 4-byte length, and was allocated as
    // a boxed slice of exactly 4 + length bytes (see `into_response`).
    unsafe {
        let len = u32::from_le_bytes(ptr.cast::<[u8; 4]>().read_unaligned()) as usize;
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, 4 + len)));
    }
}

/// A response buffer: the JSON's length as 4 little-endian bytes, then the
/// JSON.
fn into_response(json: String) -> *mut u8 {
    let json = match u32::try_from(json.len()) {
        Ok(_) => json,
        Err(_) => json!({ "error": "response too large" }).to_string(),
    };
    let len = u32::try_from(json.len()).unwrap_or(0);
    let mut buffer = Vec::with_capacity(4 + json.len());
    buffer.extend_from_slice(&len.to_le_bytes());
    buffer.extend_from_slice(json.as_bytes());
    Box::into_raw(buffer.into_boxed_slice()).cast::<u8>()
}
