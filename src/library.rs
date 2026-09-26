//! Vela's own library, `std`: modules written in Vela that come with the
//! compiler, brought in as any module is, with `include std/files`. Their source
//! is part of the compiler, so the command, Vela Studio, and the web editor
//! all have them without reading a file, and a folder of a program's own can't
//! stand in for them (see `modules::load`).
//!
//! A task of the library may be `native`: its header is Vela, and its body is
//! a function of the runtime (`runtime/vela_rt.h`), for what Vela itself can't
//! do, such as asking the system about a file. Only the library has them.

/// The first part of every include path that names a module of the library.
pub const ROOT: &str = "std";

/// Each module of the library: its name after `std/`, and its source.
pub const MODULES: &[(&str, &str)] = &[("files", include_str!("../std/files.vela"))];

/// Each native task of the library, by its name in the program, with the
/// runtime function that is its body, which takes the same parameters.
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
];

/// The source of the library's module `name` (`files` for `std/files`).
pub fn module(name: &str) -> Option<&'static str> {
    MODULES.iter().find(|(module, _)| *module == name).map(|(_, source)| *source)
}

/// The runtime function that is the body of the native task named `full` in
/// the program (`std/files.path_exists`).
pub fn native(full: &str) -> Option<&'static str> {
    NATIVES.iter().find(|(task, _)| *task == full).map(|(_, function)| *function)
}

/// Whether the module with identity `id` (see `modules::ModuleScope::name`)
/// is one of the library's, or would read as one.
pub fn is_library(id: &str) -> bool {
    id == ROOT || id.strip_prefix(ROOT).is_some_and(|rest| rest.starts_with('/'))
}

/// The library's modules as an include names them, for a message.
pub fn listed() -> String {
    MODULES.iter().map(|(name, _)| format!("'{}/{}'", ROOT, name)).collect::<Vec<_>>().join(", ")
}
