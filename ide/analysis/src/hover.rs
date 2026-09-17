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

/// The words of `maybe`, error results, and regions, which are easy to forget.
const KEYWORD_DOCS: &[(&str, &str, &str)] = &[
    (
        "region",
        "region r",
        "Memory new allocates in r lives until the block declaring r closes. As a parameter's type, r: region lets a task allocate in the caller's region.",
    ),
    (
        "new",
        "new T(value) at r",
        "Copies the value into region r and gives a ^T@r. Also written new@r T(value). With a slice type, new [T](items) at r gives a [T]@r of the items: [v; n], an array, or a slice.",
    ),
    ("at", "new T(value) at r", "Allocates in region r, the same as new@r T(value) ('@' is 'at')."),
    ("in", "each i in a..b", "Counts from a up to, but not including, b. each v in xs goes through the items of an array or slice."),
    ("maybe", "maybe T", "A T, or none. Read it with match, is, or a plan B: x { fallback }."),
    ("none", "none", "The empty maybe. A pointer can't be none; use maybe ^T."),
    ("is", "value is pattern", "The condition holds when the pattern matches; its names are visible in the block."),
    ("fail", "fail error", "An error, for a task that sends T or E. In a plan B, a first line fail e names the error."),
    ("_", "_", "Matches anything left, or drops a value without naming it."),
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
    let info = match (token.class, k.symbol_of(token)) {
        (TokenClass::TypeBuiltin, _) => {
            let (name, doc) = BUILTIN_TYPE_DOCS.iter().find(|(name, _)| *name == text)?;
            (name.to_string(), doc.to_string())
        }
        (TokenClass::BuiltinFn, _) => (
            format!("task {}(value: int | float | bool | text | maybe)", text),
            "Prints the value followed by a newline; a maybe prints its value or none.".to_string(),
        ),
        (TokenClass::Keyword, _) => {
            let (_, code, doc) = KEYWORD_DOCS.iter().find(|(word, _, _)| *word == text)?;
            (code.to_string(), doc.to_string())
        }
        // `xs.len`, which no shape declares.
        (TokenClass::Field, None) if text == "len" => {
            ("len: int".to_string(), "The number of items of an array or slice.".to_string())
        }
        (TokenClass::Field, None) if text == "add" => (
            "add(value) or add(index, value)".to_string(),
            "Adds a value to a slice: last, or before the item at the index (which may be the length). The slice must be in a place that can change, and the task must have its region; the items stay in that region.".to_string(),
        ),
        (TokenClass::Field, None) if text == "del" => (
            "del(index) or del()".to_string(),
            "Takes the item at the index out of a slice, or the last item, and gives it. The items after it move down.".to_string(),
        ),
        (_, Some(id)) => {
            let (code, doc) = describe(k, id)?;
            if code.contains(&format!("@{}", vela::ast::ANY_REGION)) {
                let note = "@_ means no region: the value was made without a pointer into one, so it fits in any region, but can't be given a pointer.";
                (code, if doc.is_empty() { note.to_string() } else { format!("{} {}", doc, note) })
            } else {
                (code, doc)
            }
        }
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
        SymbolKind::Module => {
            let module = k.scan.modules.iter().find(|m| m.symbol == id);
            let doc = match module {
                Some(m) if m.source.is_some() => format!("A module this file includes: the file {}.", m.path),
                Some(m) => format!("A module this file includes: the file {}, which can't be read.", m.path),
                None => "A module this file includes.".to_string(),
            };
            (format!("include {}", symbol.detail), doc)
        }
        SymbolKind::Task => (format!("task {}{}", symbol.name, symbol.detail), from_module(k, symbol)),
        SymbolKind::Shape | SymbolKind::Kind => {
            let keyword = if symbol.kind == SymbolKind::Shape { "shape" } else { "kind" };
            let private = if symbol.private { "priv " } else { "" };
            let mut code = format!("{}{} {}\n", private, keyword, symbol.name);
            for (_, member) in k.children(id) {
                let separator = if member.kind == SymbolKind::Field { ": " } else { "" };
                let private = if member.private { "priv " } else { "" };
                code.push_str(&format!("    {}{}{}{}\n", private, vela::ast::unqualified(&member.name), separator, member.detail));
            }
            code.push_str("fi");
            (code, from_module(k, symbol))
        }
        SymbolKind::Variant => {
            let kind = parent.map(|p| p.name.as_str()).unwrap_or("?");
            let doc = format!("A variant of kind {}.", kind);
            let module = from_module(k, symbol);
            (format!("{}{}", symbol.name, symbol.detail), if module.is_empty() { doc } else { format!("{} {}", doc, module) })
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
        SymbolKind::Region => {
            let task = k
                .symbols()
                .iter()
                .find(|s| s.kind == SymbolKind::Task && s.range.start <= symbol.name_range.start && symbol.name_range.end <= s.range.end);
            let task = task.map_or("this task".to_string(), |t| format!("task {}", t.name));
            match symbol.intro {
                Some("region") => (
                    format!("region {}", symbol.name),
                    "A region: memory allocated in it lives until the block declaring it closes.".to_string(),
                ),
                Some(_) => (
                    format!("region {}", symbol.name),
                    format!("A region of the caller's that {} names in its types; it reads and links pointers there, but can't allocate in it.", task),
                ),
                None => (
                    format!("{}: region", symbol.name),
                    format!("A region parameter of {}, which it can allocate in.", task),
                ),
            }
        }
        SymbolKind::Variable => match symbol.intro {
            Some("case") => {
                let name = k.at_byte(symbol.name_range.start)?;
                let case = (k.line_start(name)..name).find(|&q| k.token(q) == Some(&vela::lexer::Token::Case));
                let variant = case.and_then(|q| k.ident(k.pattern_variant_at(q)));
                let doc = variant.map_or_else(|| "Bound by a case.".to_string(), |v| format!("Bound by case {}.", v));
                (typed(&symbol.name), doc)
            }
            Some("is") => (typed(&symbol.name), "Bound by an is condition.".to_string()),
            Some("fail") => (format!("fail {}", typed(&symbol.name)), "The error this plan B handles.".to_string()),
            Some("each") => {
                let doc = match k.each_items(id) {
                    None => "The counter of an each loop.",
                    Some(_) => "An item of the array or slice an each loop goes through.",
                };
                (format!("each {}", typed(&symbol.name)), doc.to_string())
            }
            Some("hold") => (format!("hold {}", typed(&symbol.name)), "A mutable variable.".to_string()),
            Some("result") => {
                let task = k
                    .symbols()
                    .iter()
                    .find(|s| s.kind == SymbolKind::Task && s.range.start <= symbol.name_range.start && symbol.name_range.end <= s.range.end);
                let doc = match task {
                    Some(task) => format!("A result of task {}, which reaching the end of the task sends.", task.name),
                    None => "A result of this task, which reaching its end sends.".to_string(),
                };
                (format!("result {}", typed(&symbol.name)), format!("{} Set it with '{} <- …'.", doc, symbol.name))
            }
            _ => (format!("bind {}", typed(&symbol.name)), "An immutable variable.".to_string()),
        },
    })
}

/// For a declaration of another file, which module it is from.
fn from_module(k: &Knowledge, symbol: &crate::scan::Symbol) -> String {
    match symbol.module.and_then(|m| k.scan.modules.get(m)) {
        Some(module) => {
            let name = k.symbol(module.symbol).map_or("", |s| s.name.as_str());
            format!("From module {} ({}).", name, module.path)
        }
        None => String::new(),
    }
}
