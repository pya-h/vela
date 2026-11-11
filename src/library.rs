//! Embedded standard library modules and their `runtime/vela_rt.h` native mappings.

pub const ROOT: &str = "std";

pub const MODULES: &[(&str, &str)] = &[
    ("collections", include_str!("../std/collections.vela")),
    ("files", include_str!("../std/files.vela")),
    ("gui", include_str!("../std/gui.vela")),
    ("int512", include_str!("../std/int512.vela")),
    ("json", include_str!("../std/json.vela")),
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
    ("std/web.native_text_find", "vela__rt__web_text_find"),
    ("std/web.native_text_lower", "vela__rt__web_text_lower"),
    ("std/web.native_text_trim", "vela__rt__web_text_trim"),
    ("std/web.native_text_bytes", "vela__rt__web_text_bytes"),
    ("std/web.native_socket_stash", "vela__rt__web_socket_stash"),
    ("std/web.native_socket_stashed", "vela__rt__web_socket_stashed"),
    ("std/web.native_socket_unstash", "vela__rt__web_socket_unstash"),
    ("std/web.native_socket_flush", "vela__rt__web_socket_flush"),
    ("std/web.native_socket_queue", "vela__rt__web_socket_queue"),
    ("std/web.native_socket_queued", "vela__rt__web_socket_queued"),
    ("std/web.native_socket_accept_owned", "vela__rt__web_socket_accept_owned"),
    ("std/web.native_url_well_formed", "vela__rt__web_url_well_formed"),
    ("std/web.native_url_decode", "vela__rt__web_url_decode"),
    ("std/web.native_text_is_utf8", "vela__rt__web_text_is_utf8"),
    ("std/web.native_is_interactive", "vela__rt__web_is_interactive"),
    ("std/web.native_text_eq_ignore_case", "vela__rt__web_text_eq_ignore_case"),
    ("std/web.native_time_ms", "vela__rt__web_time_ms"),
    ("std/web.native_sleep_ms", "vela__rt__web_sleep_ms"),
    ("std/web.native_poll_clear", "vela__rt__web_poll_clear"),
    ("std/web.native_poll_add", "vela__rt__web_poll_add"),
    ("std/web.native_poll_remove", "vela__rt__web_poll_remove"),
    ("std/web.native_poll_wait", "vela__rt__web_poll_wait"),
    ("std/web.native_poll_revents", "vela__rt__web_poll_revents"),
    ("std/web.native_poll_is_readable", "vela__rt__web_poll_is_readable"),
    ("std/web.native_poll_is_writable", "vela__rt__web_poll_is_writable"),
    ("std/web.native_ws_accept_key", "vela__rt__web_ws_accept_key"),
    ("std/web.native_ws_encode_frame", "vela__rt__web_ws_encode_frame"),
    ("std/web.native_ws_encode_close", "vela__rt__web_ws_encode_close"),
    ("std/web.native_ws_frame_info", "vela__rt__web_ws_frame_info"),
    ("std/web.native_ws_frame_opcode", "vela__rt__web_ws_frame_opcode"),
    ("std/web.native_ws_frame_payload", "vela__rt__web_ws_frame_payload"),
    ("std/web.native_ws_frame_close_code", "vela__rt__web_ws_frame_close_code"),
    ("std/web.native_socket_stash_consume", "vela__rt__web_socket_stash_consume"),
    ("std/gui.native_gui_window_open", "vela__rt__gui_window_open"),
    ("std/gui.native_gui_window_close", "vela__rt__gui_window_close"),
    ("std/gui.native_gui_window_is_open", "vela__rt__gui_window_is_open"),
    ("std/gui.native_gui_window_poll_event", "vela__rt__gui_window_poll_event"),
    ("std/gui.native_gui_window_event_arg", "vela__rt__gui_window_event_arg"),
    ("std/gui.native_gui_window_push_event", "vela__rt__gui_window_push_event"),
    ("std/gui.native_gui_window_present", "vela__rt__gui_window_present"),
    ("std/gui.native_gui_canvas_clear", "vela__rt__gui_canvas_clear"),
    ("std/gui.native_gui_canvas_draw_pixel", "vela__rt__gui_canvas_draw_pixel"),
    ("std/gui.native_gui_canvas_pixel_r", "vela__rt__gui_canvas_pixel_r"),
    ("std/gui.native_gui_canvas_pixel_g", "vela__rt__gui_canvas_pixel_g"),
    ("std/gui.native_gui_canvas_pixel_b", "vela__rt__gui_canvas_pixel_b"),
    ("std/gui.native_gui_canvas_pixel_a", "vela__rt__gui_canvas_pixel_a"),
    ("std/gui.native_gui_canvas_fill_rect", "vela__rt__gui_canvas_fill_rect"),
    ("std/gui.native_gui_canvas_draw_rect", "vela__rt__gui_canvas_draw_rect"),
    ("std/gui.native_gui_canvas_draw_line", "vela__rt__gui_canvas_draw_line"),
    ("std/gui.native_gui_canvas_draw_text", "vela__rt__gui_canvas_draw_text"),
    ("std/gui.native_gui_canvas_export_ppm", "vela__rt__gui_canvas_export_ppm"),
    ("std/gui.native_is_interactive", "vela__rt__is_interactive"),
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
