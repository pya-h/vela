//! The words and symbols an editor needs for behavior that doesn't depend on
//! analysis, such as indentation and comment toggling. Keywords and types come
//! from the compiler's own tables.

use serde::Serialize;
use vela::ast::{ALTERS, BUILTIN_TASKS, BUILTIN_TYPES};
use vela::lexer::KEYWORDS;

/// Keywords that start a block closed by `fi`.
pub const BLOCK_OPENERS: &[&str] = &["task", "shape", "kind", "when", "loop", "endless", "each", "block", "match"];
/// Keywords that may come before a block opener on its line, as in `priv task`.
pub const BLOCK_MODIFIERS: &[&str] = &["priv"];
/// Keywords that start a new branch inside an open block.
pub const BRANCH_KEYWORDS: &[&str] = &["else", "case"];
/// The keyword that closes a block.
pub const BLOCK_CLOSER: &str = "fi";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Language {
    pub keywords: Vec<&'static str>,
    pub builtin_types: Vec<&'static str>,
    pub builtin_tasks: Vec<&'static str>,
    pub block_openers: Vec<&'static str>,
    pub block_modifiers: Vec<&'static str>,
    pub branch_keywords: Vec<&'static str>,
    pub block_closer: &'static str,
    /// The word before a method that changes its value, `alters`. A method
    /// opens a block with no keyword of its own: its line, among a shape's or a
    /// kind's members, is a name followed by `(`, after `priv` and this word.
    pub method_marker: &'static str,
    pub line_comment: &'static str,
    /// Bracket pairs, for matching and auto-closing.
    pub brackets: Vec<(&'static str, &'static str)>,
}

pub fn language() -> Language {
    Language {
        keywords: KEYWORDS.iter().map(|(word, _)| *word).collect(),
        builtin_types: BUILTIN_TYPES.iter().map(|(name, _)| *name).collect(),
        builtin_tasks: BUILTIN_TASKS.to_vec(),
        block_openers: BLOCK_OPENERS.to_vec(),
        block_modifiers: BLOCK_MODIFIERS.to_vec(),
        branch_keywords: BRANCH_KEYWORDS.to_vec(),
        block_closer: BLOCK_CLOSER,
        method_marker: ALTERS,
        line_comment: ";",
        brackets: vec![("(", ")"), ("{", "}"), ("[", "]"), ("\"", "\""), ("'", "'")],
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

    use super::*;

    #[test]
    fn block_words_are_keywords() {
        let keywords = language().keywords;
        for word in BLOCK_OPENERS.iter().chain(BLOCK_MODIFIERS).chain(BRANCH_KEYWORDS).chain([&BLOCK_CLOSER]) {
            assert!(keywords.contains(word), "{:?} is not a keyword", word);
        }
    }

    #[test]
    fn every_block_opener_is_closed_by_fi() {
        // Each opener, as the compiler parses it, needs exactly one `fi`.
        let bodies = [
            "task t()\nfi\n",
            "shape S\nfi\n",
            "kind K\n    A\nfi\n",
            "task t()\n    when true\n    fi\nfi\n",
            "task t()\n    loop false\n    fi\nfi\n",
            "task t()\n    endless\n        break\n    fi\nfi\n",
            "task t()\n    each i in 0..1\n    fi\nfi\n",
            "task t()\n    block\n    fi\nfi\n",
            "kind K\n    A\nfi\ntask t(k: K)\n    match k\n    case A\n    fi\nfi\n",
        ];
        // A modifier comes before an item's opener, which still needs its `fi`.
        for modifier in BLOCK_MODIFIERS {
            assert!(vela::driver::front_end(&format!("{} task t()\nfi\n", modifier)).is_ok());
            assert!(vela::driver::front_end(&format!("{} task t()\n", modifier)).is_err());
        }
        for (opener, body) in BLOCK_OPENERS.iter().zip(bodies) {
            assert!(body.contains(opener));
            assert!(vela::driver::front_end(body).is_ok(), "{:?}", body);
            let unclosed = body.rsplit_once("fi\n").map(|(head, tail)| format!("{}{}", head, tail)).unwrap();
            assert!(vela::driver::front_end(&unclosed).is_err(), "{:?} without its last fi should fail", opener);
        }
    }
}
