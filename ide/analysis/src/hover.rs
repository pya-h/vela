//! Hover: a short description of the name under the cursor, written the way
//! it is declared.

use serde::Serialize;

use crate::TokenClass;
use crate::infer::Knowledge;
use crate::scan::SymbolKind;

/// What to show for a name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Hover {
    /// The range of the hovered name (UTF-16 offsets).
    pub from: usize,
    pub to: usize,
    /// Vela source describing it, such as `task fib(n: int) -> int`.
    pub code: String,
    /// A sentence about it, possibly empty.
    pub doc: String,
}

const BUILTIN_TYPE_DOCS: &[(&str, &str)] = &[
    ("int", "A 64-bit signed integer."),
    ("float", "A 64-bit floating-point number."),
    ("bool", "true or false."),
    ("text", "Text. == compares contents."),
    ("unit", "The return type of a task that sends no value."),
];

/// The hover for the classified token covering `byte`, as `(code, doc)` and
/// the token's byte range.
pub(crate) fn hover(k: &Knowledge, byte: usize) -> Option<((String, String), std::ops::Range<usize>)> {
    let tokens = &k.scan.tokens;
    // The token under `byte`, or else the one ending there: at the boundary of
    // `n)`, the name is what a hover means.
    let under = tokens.iter().find(|t| t.range.start <= byte && byte < t.range.end);
    let ending = tokens.iter().find(|t| t.range.end == byte);
    [under, ending].into_iter().flatten().find_map(|token| hover_token(k, token))
}

fn hover_token(k: &Knowledge, token: &crate::scan::Classified) -> Option<((String, String), std::ops::Range<usize>)> {
    let text = k.source.get(token.range.clone()).unwrap_or_default();
    let info = match (token.class, token.symbol) {
        (TokenClass::TypeBuiltin, _) => {
            let (name, doc) = BUILTIN_TYPE_DOCS.iter().find(|(name, _)| *name == text)?;
            (name.to_string(), doc.to_string())
        }
        (TokenClass::BuiltinFn, _) => (
            format!("task {}(value: int | float | bool | text)", text),
            "Prints the value followed by a newline.".to_string(),
        ),
        (_, Some(id)) => describe(k, id)?,
        _ => return None,
    };
    Some((info, token.range.clone()))
}

fn describe(k: &Knowledge, id: usize) -> Option<(String, String)> {
    let symbol = k.symbol(id)?;
    let parent = symbol.parent.and_then(|p| k.symbol(p));
    let typed = |name: &str| match k.type_of_symbol(id) {
        Some(ty) => format!("{}: {}", name, ty),
        None => name.to_string(),
    };
    Some(match symbol.kind {
        SymbolKind::Task => (format!("task {}{}", symbol.name, symbol.detail), String::new()),
        SymbolKind::Shape | SymbolKind::Kind => {
            let keyword = if symbol.kind == SymbolKind::Shape { "shape" } else { "kind" };
            let mut code = format!("{} {}\n", keyword, symbol.name);
            for (_, member) in k.children(id) {
                let separator = if member.kind == SymbolKind::Field { ": " } else { "" };
                code.push_str(&format!("    {}{}{}\n", member.name, separator, member.detail));
            }
            code.push_str("fi");
            (code, String::new())
        }
        SymbolKind::Variant => {
            let kind = parent.map(|p| p.name.as_str()).unwrap_or("?");
            (format!("{}{}", symbol.name, symbol.detail), format!("A variant of kind {}.", kind))
        }
        SymbolKind::Field => {
            let doc = match parent {
                Some(p) if p.kind == SymbolKind::Variant => format!("A payload field of variant {}.", p.name),
                Some(p) => format!("A field of shape {}.", p.name),
                None => String::new(),
            };
            (format!("{}: {}", symbol.name, symbol.detail), doc)
        }
        SymbolKind::Parameter => {
            let task = k
                .symbols()
                .iter()
                .find(|s| s.kind == SymbolKind::Task && s.range.start <= symbol.name_range.start && symbol.name_range.end <= s.range.end);
            let doc = task.map(|t| format!("A parameter of task {}.", t.name)).unwrap_or_default();
            (typed(&symbol.name), doc)
        }
        SymbolKind::Variable => match symbol.intro {
            Some("case") => {
                let name = k.at_byte(symbol.name_range.start)?;
                let case = (k.line_start(name)..name).find(|&q| k.token(q) == Some(&vela::lexer::Token::Case));
                let variant = case.and_then(|q| k.ident(q + 1));
                let doc = variant.map(|v| format!("Bound by case {}.", v)).unwrap_or_default();
                (typed(&symbol.name), doc)
            }
            Some("each") => (format!("each {}", typed(&symbol.name)), "The counter of an each loop.".to_string()),
            Some("hold") => (format!("hold {}", typed(&symbol.name)), "A mutable variable.".to_string()),
            _ => (format!("bind {}", typed(&symbol.name)), "An immutable variable.".to_string()),
        },
    })
}
