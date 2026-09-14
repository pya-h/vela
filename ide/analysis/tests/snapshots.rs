//! Highlighting snapshots: the token classes of every example and run test,
//! one annotated line per source line, in `tests/snapshots/`. A change in
//! classification shows up as a readable diff. Regenerate after an intended
//! change with `UPDATE_SNAPSHOTS=1 cargo test -p vela-analysis`, and review
//! the diff.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use vela_analysis::{Analysis, LineIndex};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn vela_files(dir: &str) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(root().join(dir))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "vela"))
        .collect();
    files.sort();
    files
}

/// Each source line, followed by its tokens as `class:text`.
fn render(source: &str) -> String {
    let analysis = Analysis::new(source);
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
        for file in vela_files(dir) {
            let source = fs::read_to_string(&file).unwrap();
            let name = format!("{}-{}.txt", dir.replace('/', "-"), file.file_stem().unwrap().to_string_lossy());
            let snapshot = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots").join(&name);
            let actual = render(&source);
            if update {
                fs::write(&snapshot, &actual).unwrap();
            } else if fs::read_to_string(&snapshot).ok().as_deref() != Some(actual.as_str()) {
                failures.push(name);
            }
        }
    }
    assert!(failures.is_empty(), "snapshots differ (UPDATE_SNAPSHOTS=1 to regenerate, then review): {:?}", failures);
}
