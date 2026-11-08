//! Highlighting snapshots in `tests/snapshots/`. Regenerate with `UPDATE_SNAPSHOTS=1 cargo test -p vela-analysis`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use vela_analysis::{Analysis, LineIndex};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn vela_files(dir: &str) -> Vec<String> {
    let mut files = Vec::new();
    for entry in fs::read_dir(root().join(dir)).unwrap() {
        let path = entry.unwrap().path();
        let name = format!("{}/{}", dir, path.file_name().unwrap().to_string_lossy());
        if path.is_dir() {
            files.extend(vela_files(&name));
        } else if path.extension().is_some_and(|ext| ext == "vela") {
            files.push(name);
        }
    }
    files.sort();
    files
}

fn render(path: &str, source: &str) -> String {
    let mut sources = HashMap::new();
    loop {
        let missing = Analysis::with_modules(source, path, &sources).missing_modules();
        let unread: Vec<String> = missing.into_iter().filter(|m| !sources.contains_key(m)).collect();
        if unread.is_empty() {
            break;
        }
        for module in unread {
            let text = fs::read_to_string(root().join(&module)).unwrap_or_default();
            sources.insert(module, text);
        }
    }
    let analysis = Analysis::with_modules(source, path, &sources);
    let index = LineIndex::new(source);
    let tokens = analysis.tokens();
    let mut out = String::new();
    let mut line_start = 0;
    for (number, line) in source.split('\n').enumerate() {
        let line_end = line_start + line.len();
        let (from, to) = (index.utf16_of(line_start), index.utf16_of(line_end));
        let pieces: Vec<String> = tokens
            .iter()
            .filter(|t| t.from >= from && t.to <= to && t.from < to)
            .map(|t| {
                let (a, b) = (index.byte_of_utf16(t.from), index.byte_of_utf16(t.to));
                format!("{}:{}", serde_json::to_value(t.class).unwrap().as_str().unwrap(), &source[a..b])
            })
            .collect();
        if !line.trim().is_empty() {
            writeln!(out, "{:>3} | {}", number + 1, line).unwrap();
            writeln!(out, "    | {}", pieces.join("  ")).unwrap();
        }
        line_start = line_end + 1;
    }
    out
}

#[test]
fn token_classes_match_snapshots() {
    let update = std::env::var_os("UPDATE_SNAPSHOTS").is_some();
    let mut failures = Vec::new();
    for dir in ["examples", "tests/run"] {
        for path in vela_files(dir) {
            let source = fs::read_to_string(root().join(&path)).unwrap();
            let name = format!("{}.txt", path.trim_end_matches(".vela").replace('/', "-"));
            let snapshot = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots").join(&name);
            let actual = render(&path, &source);
            if update {
                fs::write(&snapshot, &actual).unwrap();
            } else if fs::read_to_string(&snapshot).ok().as_deref() != Some(actual.as_str()) {
                failures.push(name);
            }
        }
    }
    assert!(failures.is_empty(), "snapshots differ (UPDATE_SNAPSHOTS=1 to regenerate, then review): {:?}", failures);
}
