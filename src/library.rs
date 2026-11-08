//! Embedded standard library modules and their `runtime/vela_rt.h` native mappings.

pub const ROOT: &str = "std";

pub const MODULES: &[(&str, &str)] = &[
    ("collections", include_str!("../std/collections.vela")),
    ("files", include_str!("../std/files.vela")),
    ("int512", include_str!("../std/int512.vela")),
    ("loops", include_str!("../std/loops.vela")),
    ("math", include_str!("../std/math.vela")),
    ("web", include_str!("../std/web.vela")),
];

/// Implicitly loaded by value-producing loops.
pub const LOOPS: &str = "loops";
pub const LOOP_RESULT: &str = "std/loops.LoopResult";
/// Replaced with `exp(x)` when raised to a power.
pub const MATH_E: &str = "std/math.e";

pub const NATIVES: &[(&str, &str)] = &[
    ("std/files.path_exists", "vela__rt__files_path_exists"),
    ("std/files.open_handle", "vela__rt__files_open"),
    ("std/files.is_open", "vela__rt__files_is_open"),
    ("std/files.close_handle", "vela__rt__files_close"),
    ("std/files.read_rest", "vela__rt__files_read"),
    ("std/files.read_line", "vela__rt__files_line"),
    ("std/files.write_text", "vela__rt__files_write"),
    ("std/files.at_end", "vela__rt__files_ended"),
    ("std/files.size_of", "vela__rt__files_size"),
    ("std/files.rename_path", "vela__rt__files_rename"),
    ("std/files.remove_path", "vela__rt__files_remove"),
    ("std/files.status", "vela__rt__files_status"),
    ("std/int512.native_add", "vela__rt__int512_add"),
    ("std/int512.native_sub", "vela__rt__int512_sub"),
    ("std/int512.native_mul", "vela__rt__int512_mul"),
    ("std/int512.native_div", "vela__rt__int512_div"),
    ("std/int512.native_rem", "vela__rt__int512_rem"),
    ("std/int512.native_neg", "vela__rt__int512_neg"),
    ("std/int512.native_abs", "vela__rt__int512_abs"),
    ("std/int512.native_cmp", "vela__rt__int512_cmp"),
    ("std/int512.native_to_int64", "vela__rt__int512_to_int64"),
    ("std/int512.native_to_uint64", "vela__rt__int512_to_uint64"),
    ("std/int512.native_from_int64", "vela__rt__int512_from_int64"),
    ("std/int512.native_from_uint64", "vela__rt__int512_from_uint64"),
    ("std/int512.native_parse_status", "vela__rt__int512_parse_status"),
    ("std/int512.native_parse_value", "vela__rt__int512_parse_value"),
    ("std/int512.native_to_text", "vela__rt__int512_to_text"),
    // Directly mapped to `<math.h>`, except `log` which takes a base.
    ("std/math.pow", "pow"),
    ("std/math.sin", "sin"),
    ("std/math.cos", "cos"),
    ("std/math.tan", "tan"),
    ("std/math.asin", "asin"),
    ("std/math.acos", "acos"),
    ("std/math.atan", "atan"),
    ("std/math.sinh", "sinh"),
    ("std/math.cosh", "cosh"),
    ("std/math.tanh", "tanh"),
    ("std/math.exp", "exp"),
    ("std/math.log", "vela__rt__math_log"),
    ("std/web.native_socket_open", "vela__rt__web_socket_open"),
    ("std/web.native_socket_bind", "vela__rt__web_socket_bind"),
    ("std/web.native_socket_listen", "vela__rt__web_socket_listen"),
    ("std/web.native_socket_accept", "vela__rt__web_socket_accept"),
    ("std/web.native_socket_connect", "vela__rt__web_socket_connect"),
    ("std/web.native_socket_read", "vela__rt__web_socket_read"),
    ("std/web.native_socket_write", "vela__rt__web_socket_write"),
    ("std/web.native_socket_close", "vela__rt__web_socket_close"),
    ("std/web.native_socket_poll", "vela__rt__web_socket_poll"),
    ("std/web.native_socket_status", "vela__rt__web_socket_status"),
];

pub fn module(name: &str) -> Option<&'static str> {
    MODULES.iter().find(|(module, _)| *module == name).map(|(_, source)| *source)
}

pub fn native(full: &str) -> Option<&'static str> {
    NATIVES.iter().find(|(task, _)| *task == full).map(|(_, function)| *function)
}

pub fn is_library(id: &str) -> bool {
    id == ROOT || id.strip_prefix(ROOT).is_some_and(|rest| rest.starts_with('/'))
}

pub fn listed() -> String {
    MODULES.iter().map(|(name, _)| format!("'{}/{}'", ROOT, name)).collect::<Vec<_>>().join(", ")
}
