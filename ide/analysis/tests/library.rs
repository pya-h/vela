//! Vela's library in the editor: `include std/files` needs no file from the
//! editor, since the library is the compiler's own, and highlighting,
//! diagnostics, hover, and completion follow its names as they do a module of
//! the program's. In the sources below, `@@` marks the cursor.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::collections::HashMap;

use vela_analysis::{Analysis, LineIndex, TokenClass};

const MAIN: &str = "\
include std/files => disk (File)

task main()
    bind here = File { path: \"notes.txt\" }
    show(here.exists())
    bind there = disk.File { path: \"other.txt\" }
    show(there.path)
    @@
fi
";

fn main_with(code: &str) -> String {
    MAIN.replace("    @@\n", &format!("    {}\n", code))
}

/// The UTF-16 offset of the first `needle` in `source`.
fn at(source: &str, needle: &str) -> usize {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    LineIndex::new(source).utf16_of(byte)
}

fn labels(marked: &str) -> Vec<String> {
    let offset = marked.find("@@").unwrap();
    let source = marked.replacen("@@", "", 1);
    let none = HashMap::new();
    let analysis = Analysis::with_modules(&source, "app/main.vela", &none);
    analysis.complete(LineIndex::new(&source).utf16_of(offset)).items.into_iter().map(|item| item.label).collect()
}

#[test]
fn the_library_needs_no_file_from_the_editor() {
    let source = main_with("show(1)").replace("include std/files => disk (File)\n", "include std/files {File}\n");
    let source = source.replace("    bind there = disk.File { path: \"other.txt\" }\n    show(there.path)\n", "");
    let none = HashMap::new();
    let analysis = Analysis::with_modules(&source, "app/main.vela", &none);
    assert_eq!(analysis.missing_modules(), Vec::<String>::new());
    assert_eq!(analysis.diagnostics(), Vec::new());
}

#[test]
fn hover_and_definitions_know_the_library() {
    let source = main_with("show(0)");
    let none = HashMap::new();
    let analysis = Analysis::with_modules(&source, "app/main.vela", &none);
    let exists = analysis.hover(at(&source, "exists()")).unwrap();
    assert_eq!(exists.code, "exists() -> bool");
    assert!(exists.doc.starts_with("A method of shape disk.File"), "{}", exists.doc);
    let file = analysis.hover(at(&source, "File {")).unwrap();
    // Its own private field and method are the library's.
    assert!(file.code.starts_with("shape disk.File\n    read path: text\n    read state: disk.FileState\n    read mode: maybe disk.FileMode\n    alters open("), "{}", file.code);
    assert!(!file.code.contains("handle") && !file.code.contains("left_closed"), "{}", file.code);
    assert_eq!(file.doc, "From module disk (Vela's library).");
    let module = analysis.hover(at(&source, "disk (")).unwrap();
    assert_eq!((module.code.as_str(), module.doc.as_str()), ("include std/files => disk", "A module of Vela's library, which comes with the compiler."));
    // The library is no file the editor can open.
    assert_eq!(analysis.definition(at(&source, "File {")), None);
    assert_eq!(analysis.definition(at(&source, "disk (")), None);
    assert_eq!(analysis.tokens().into_iter().find(|t| t.from == at(&source, "disk.File")).unwrap().class, TokenClass::Module);
}

#[test]
fn completion_offers_the_library() {
    assert!(labels(&main_with("show(here.@@)")).contains(&"exists".to_string()));
    assert!(labels(&main_with("show(disk.@@)")).contains(&"File".to_string()));
    // Its private native task is its own.
    assert!(!labels(&main_with("show(disk.@@)")).contains(&"path_exists".to_string()));
    let at_path = labels("include @@\n");
    assert!(at_path.contains(&"std/files".to_string()), "{:?}", at_path);
    let after_root = labels("include std/@@\n");
    assert_eq!(after_root, ["files", "loops", "math"]);
}

#[test]
fn native_is_a_word_only_before_task() {
    let source = "native task size(path: text) -> int\n\ntask main()\n    bind native = 1\n    show(native)\nfi\n";
    let analysis = Analysis::new(source);
    let class = |offset: usize| analysis.tokens().into_iter().find(|t| t.from == offset).unwrap().class;
    assert_eq!(class(0), TokenClass::Keyword);
    assert_ne!(class(at(source, "native = 1")), TokenClass::Keyword);
    // Only the library has native tasks.
    assert!(analysis.diagnostics()[0].message.starts_with("'native' marks a task whose body the compiler writes in C"));
}


const MATH: &str = "\
include std/math (P)

task main()
    bind angle = P / 2.0
    bind rise = math.sin(angle)
    bind grown = math.e ** 2.0
    show(rise, grown, math.log(100.0, 10.0))
    @@
fi
";

#[test]
fn the_math_module_hovers_and_completes_its_macros_and_tasks() {
    let source = MATH.replace("    @@\n", "");
    let none = HashMap::new();
    let analysis = Analysis::with_modules(&source, "app/main.vela", &none);
    assert_eq!(analysis.diagnostics(), Vec::new());
    let sin = analysis.hover(at(&source, "sin(angle)")).unwrap();
    assert_eq!(sin.code, "task math.sin(x: float) -> float");
    let pi = analysis.hover(at(&source, "P / 2.0")).unwrap();
    assert_eq!(pi.code, "#P = 3.141592653589793");
    assert!(pi.doc.contains("of type float"), "{}", pi.doc);
    assert_eq!(analysis.hover(at(&source, "rise =")).unwrap().code, "bind rise: float");
    assert_eq!(analysis.hover(at(&source, "grown =")).unwrap().code, "bind grown: float");
    let found = labels(&MATH.replace("    @@\n", "    show(math.@@)\n"));
    for name in ["P", "e", "pow", "sin", "cos", "tan", "asin", "acos", "atan", "sinh", "cosh", "tanh", "exp", "log"] {
        assert!(found.contains(&name.to_string()), "{} in {:?}", name, found);
    }
    assert!(labels("include std/@@").contains(&"math".to_string()));
}
