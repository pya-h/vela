//! WebAssembly analysis JSON RPC: 4-byte LE length-prefixed responses; all offsets are UTF-16.

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
    MissingModules,
    Language,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    #[serde(default)]
    source: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    modules: Option<HashMap<String, String>>,
    #[serde(default, rename = "same-modules")]
    same_modules: bool,
    queries: Vec<Query>,
}

#[derive(Default)]
struct Kept {
    modules: Option<Arc<HashMap<String, String>>>,
    none: Arc<HashMap<String, String>>,
    analysis: Option<Analysis>,
    /// Separate slot for standalone snippets (e.g. hover tooltips) so they do not evict program state.
    bare: Option<Analysis>,
    made: usize,
}

thread_local! {
    static KEPT: RefCell<Kept> = RefCell::new(Kept::default());
}

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
            // Drop the previous analysis first so two are never live in wasm memory at once.
            *slot = None;
            *slot = Some(Analysis::of_program(request.source, request.path, modules));
            kept.made += 1;
        }
        let Some(analysis) = slot.as_ref() else { return json!({ "error": "no analysis" }).to_string() };
        answer(analysis, &request.queries)
    })
}

#[doc(hidden)]
pub fn analyses_made() -> usize {
    KEPT.with_borrow(|kept| kept.made)
}

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

#[unsafe(no_mangle)]
pub extern "C" fn vela_alloc(len: usize) -> *mut u8 {
    let mut buffer = Vec::<u8>::with_capacity(len.max(1));
    let ptr = buffer.as_mut_ptr();
    std::mem::forget(buffer);
    ptr
}

/// # Safety
///
/// `ptr` must come from `vela_alloc(len)` with the same `len`, all `len` bytes
/// initialized, and must not be used after this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vela_handle(ptr: *mut u8, len: usize) -> *mut u8 {
    // SAFETY: caller guarantees `ptr` was allocated by `vela_alloc(len)` with capacity `len.max(1)` and initialized bytes.
    let request = unsafe { Vec::from_raw_parts(ptr, len, len.max(1)) };
    let response = match std::str::from_utf8(&request) {
        Ok(text) => handle(text),
        Err(_) => json!({ "error": "invalid request: not UTF-8" }).to_string(),
    };
    into_response(response)
}

/// # Safety
///
/// `ptr` must have been returned by `vela_handle` and must not be used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vela_free_response(ptr: *mut u8) {
    // SAFETY: `into_response` writes a 4-byte LE length prefix followed by `len` bytes.
    unsafe {
        let len = u32::from_le_bytes(ptr.cast::<[u8; 4]>().read_unaligned()) as usize;
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, 4 + len)));
    }
}

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
