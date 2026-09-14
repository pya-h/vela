//! Completion: what may be typed at the cursor, decided from the tokens
//! before it. Items are not filtered by the word being typed; the editor
//! filters and ranks them, using `boost` to prefer the likelier kinds.
//!
//! Where types matter (fields after `.`, the cases of a `match`), the file is
//! checked again with the half-typed text removed, so the compiler's types are
//! used whenever the rest of the file allows it.

use serde::Serialize;
use vela::ast::{BUILTIN_TASKS, BUILTIN_TYPES, Type};
use vela::lexer::{LexKind, Token};

use crate::infer::{Knowledge, parameter_names};
use crate::scan::{Scan, SymbolKind};
use crate::types::Checked;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CompletionKind {
    Keyword,
    Snippet,
    Task,
    BuiltinFn,
    Variable,
    Parameter,
    Field,
    Variant,
    Type,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompletionItem {
    pub label: String,
    pub kind: CompletionKind,
    /// A type, signature, or payload shown next to the label.
    pub detail: String,
    /// Text to insert instead of the label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apply: Option<String>,
    /// A CodeMirror snippet template to insert instead of the label:
    /// `${name}` marks a field to fill in (fields with the same name are
    /// edited together), `${}` an unnamed field, `${0}` where the cursor
    /// ends, and `\t` one level of indentation. Tab visits the fields in
    /// order; reaching the last one ends the snippet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    /// How much to prefer this item among equally good matches.
    pub boost: i32,
}

/// The completions at a position: the items replace the text from `from` to
/// `to` (UTF-16 offsets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Completions {
    pub from: usize,
    pub to: usize,
    pub items: Vec<CompletionItem>,
}

/// Boosts, from most to least likely.
const LOCAL: i32 = 3;
const GLOBAL: i32 = 2;
const SNIPPET: i32 = 1;
const KEYWORD: i32 = 0;

const STATEMENT_KEYWORDS: &[&str] = &["bind", "hold", "when", "loop", "each", "match", "free"];
const VALUE_KEYWORDS: &[&str] = &["true", "false", "none", "not", "new"];

/// Completions at byte `byte` of `source`. Returns the items and the byte
/// range they replace.
pub(crate) fn complete(source: &str, scan: &Scan, checked: &Checked, byte: usize) -> (Vec<CompletionItem>, usize, usize) {
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let before = source.get(..byte).unwrap_or_default();
    let word_start = before.len() - before.chars().rev().take_while(|c| is_word(*c)).map(char::len_utf8).sum::<usize>();
    let after = source.get(byte..).unwrap_or_default();
    let word_end = byte + after.chars().take_while(|c| is_word(*c)).map(char::len_utf8).sum::<usize>();

    if in_comment_or_text(source, scan, byte) {
        return (Vec::new(), byte, byte);
    }
    let k = Knowledge::new(source, scan, checked);
    let prev = k.last_before(word_start);
    let prev_token = prev.and_then(|q| k.token(q));
    let blocks = open_blocks(&k, word_start);

    // After `.`: fields.
    if let (Some(dot), Some(Token::Dot)) = (prev, prev_token) {
        let probe = probe(source, k.range(dot).start, word_end);
        let exact = Knowledge::new(source, scan, &probe);
        let base = dot.checked_sub(1).and_then(|q| exact.type_ending_at(q));
        let (shape, through_pointer) = match base {
            Some(Type::Named(shape)) => (shape, false),
            Some(Type::Ptr(inner)) => match *inner {
                Type::Named(shape) => (shape, true),
                _ => return (Vec::new(), word_start, byte),
            },
            _ => return (Vec::new(), word_start, byte),
        };
        let Some(shape_id) = k.global(&shape, SymbolKind::Shape) else { return (Vec::new(), word_start, byte) };
        let items = k
            .children(shape_id)
            .map(|(_, field)| CompletionItem {
                label: field.name.clone(),
                kind: CompletionKind::Field,
                detail: field.detail.clone(),
                // A pointer's fields are reached through `^`: `p^.x`.
                apply: through_pointer.then(|| format!("^.{}", field.name)),
                snippet: None,
                boost: LOCAL,
            })
            .collect();
        let from = if through_pointer { k.range(dot).start } else { word_start };
        return (items, from, byte);
    }

    let line_first = prev.map(|q| k.line_start(q + 1));
    let first_token = line_first.and_then(|q| k.token(q));

    // Names being declared get no suggestions.
    if matches!(prev_token, Some(Token::Task | Token::Shape | Token::Kind | Token::Bind | Token::Hold | Token::Each)) {
        return (Vec::new(), word_start, byte);
    }
    let inside_parens = prev.is_some_and(|q| unclosed_on_line(&k, q, &Token::LParen, &Token::RParen));
    if matches!(prev_token, Some(Token::LParen | Token::Comma))
        && inside_parens
        && (first_token == Some(&Token::Task) || blocks.last() == Some(&Block::Kind))
    {
        return (Vec::new(), word_start, byte);
    }

    // `case`: the variants the match doesn't cover yet; after the variant, its bindings.
    if first_token == Some(&Token::Case) {
        if prev_token != Some(&Token::Case) {
            return (Vec::new(), word_start, byte);
        }
        let items = prev.map(|case| case_items(source, scan, &k, case)).unwrap_or_default();
        return (items, word_start, byte);
    }

    // A field name in a shape literal: the fields not given yet.
    if let Some(q) = prev
        && matches!(prev_token, Some(Token::LBrace | Token::Comma))
        && let Some(brace) = unclosed_brace(&k, q)
    {
        return (literal_field_items(&k, brace, word_start), word_start, byte);
    }

    // Types.
    let type_context = match prev_token {
        Some(Token::Colon) => !prev.is_some_and(|q| unclosed_on_line(&k, q, &Token::LBrace, &Token::RBrace)),
        Some(Token::Arrow | Token::New) => true,
        Some(Token::Caret) => {
            // `^` starts a type after `:`, `->`, `new`, or another type `^`.
            let mut q = prev.unwrap_or_default();
            while k.token(q) == Some(&Token::Caret) && q > 0 {
                q -= 1;
            }
            matches!(k.token(q), Some(Token::Colon | Token::Arrow | Token::New))
        }
        _ => false,
    };
    if type_context {
        let after_arrow = prev_token == Some(&Token::Arrow);
        return (type_items(&k, after_arrow), word_start, byte);
    }

    let at_line_start = matches!(prev_token, None | Some(Token::Newline));
    if at_line_start {
        let items = match blocks.first() {
            None => item_level_items(),
            Some(Block::Shape | Block::Kind) => Vec::new(),
            Some(_) => {
                let probe = probe(source, word_start, word_end);
                let exact = Knowledge::new(source, scan, &probe);
                statement_items(&exact, &blocks, word_start)
            }
        };
        return (items, word_start, byte);
    }
    if blocks.is_empty() || matches!(blocks.first(), Some(Block::Shape | Block::Kind)) {
        return (Vec::new(), word_start, byte);
    }
    (expression_items(&k, word_start), word_start, byte)
}

/// `source` with `from..to` removed, checked.
fn probe(source: &str, from: usize, to: usize) -> Checked {
    let text = format!("{}{}", source.get(..from).unwrap_or_default(), source.get(to..).unwrap_or_default());
    Checked::new(&text)
}

/// Whether `byte` is inside a comment or a text literal, where nothing is
/// completed.
fn in_comment_or_text(source: &str, scan: &Scan, byte: usize) -> bool {
    scan.lexed.iter().any(|t| {
        let text = source.get(t.range.clone()).unwrap_or_default();
        let inside = t.range.start < byte && byte < t.range.end;
        let at_end = t.range.start < byte && byte == t.range.end;
        match &t.kind {
            LexKind::Comment => inside || at_end,
            LexKind::Token(Token::Text(_)) => inside,
            // A rejected literal; an unterminated one runs to the cursor.
            LexKind::Invalid(_) if text.starts_with('"') => inside || (at_end && !(text.len() > 1 && text.ends_with('"'))),
            _ => false,
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Block {
    Task,
    Shape,
    Kind,
    When,
    Match,
    Other,
}

/// The blocks open at `byte`, outermost first.
fn open_blocks(k: &Knowledge, byte: usize) -> Vec<Block> {
    let mut blocks = Vec::new();
    let end = k.last_before(byte).map_or(0, |q| q + 1);
    for q in 0..end {
        match k.token(q) {
            Some(Token::Task | Token::Shape | Token::Kind) => {
                blocks.clear();
                blocks.push(match k.token(q) {
                    Some(Token::Task) => Block::Task,
                    Some(Token::Shape) => Block::Shape,
                    _ => Block::Kind,
                });
            }
            Some(Token::When) => blocks.push(Block::When),
            Some(Token::Match) => blocks.push(Block::Match),
            Some(Token::Loop | Token::Each) => blocks.push(Block::Other),
            Some(Token::Fi) => {
                blocks.pop();
            }
            _ => {}
        }
    }
    blocks
}

/// Whether an `open` bracket before position `q` (inclusive) on its line is
/// still unclosed.
fn unclosed_on_line(k: &Knowledge, q: usize, open: &Token, close: &Token) -> bool {
    let mut depth = 0usize;
    for p in (k.line_start(q)..=q).rev() {
        match k.token(p) {
            Some(t) if t == close => depth += 1,
            Some(t) if t == open => {
                if depth == 0 {
                    return true;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    false
}

/// The `{` of the shape literal directly around position `q`, if `q` is
/// inside one and not inside parentheses within it.
fn unclosed_brace(k: &Knowledge, q: usize) -> Option<usize> {
    let mut depth = 0usize;
    for p in (k.line_start(q)..=q).rev() {
        match k.token(p) {
            Some(Token::RParen | Token::RBrace) => depth += 1,
            Some(Token::LParen) if depth == 0 => return None,
            Some(Token::LBrace) if depth == 0 => return Some(p),
            Some(Token::LParen | Token::LBrace) => depth -= 1,
            _ => {}
        }
    }
    None
}

fn literal_field_items(k: &Knowledge, brace: usize, word_start: usize) -> Vec<CompletionItem> {
    let Some(shape) = brace.checked_sub(1).and_then(|q| k.ident(q)).and_then(|name| k.global(name, SymbolKind::Shape)) else {
        return Vec::new();
    };
    // Fields already given: `name:` directly inside these braces.
    let mut given = Vec::new();
    let mut depth = 0usize;
    for q in brace + 1..k.line_end(brace) {
        match k.token(q) {
            Some(Token::LParen | Token::LBrace) => depth += 1,
            Some(Token::RParen) => depth = depth.saturating_sub(1),
            Some(Token::RBrace) if depth == 0 => break,
            Some(Token::RBrace) => depth -= 1,
            Some(Token::Ident(name)) if depth == 0 && k.token(q + 1) == Some(&Token::Colon) && k.range(q).start != word_start => {
                given.push(name.clone());
            }
            _ => {}
        }
    }
    k.children(shape)
        .filter(|(_, field)| !given.contains(&field.name))
        .map(|(_, field)| CompletionItem {
            label: field.name.clone(),
            kind: CompletionKind::Field,
            detail: field.detail.clone(),
            apply: None,
            snippet: Some(format!("{}: ${{}}", field.name)),
            boost: LOCAL,
        })
        .collect()
}

fn case_items(source: &str, scan: &Scan, k: &Knowledge, case: usize) -> Vec<CompletionItem> {
    // The enclosing `match`, skipping nested blocks.
    let mut depth = 0usize;
    let mut found = None;
    for q in (0..case).rev() {
        match k.token(q) {
            Some(Token::Fi) => depth += 1,
            Some(Token::When | Token::Loop | Token::Each) if depth > 0 => depth -= 1,
            Some(Token::Match) if depth == 0 => {
                found = Some(q);
                break;
            }
            Some(Token::Match) => depth -= 1,
            Some(Token::Task | Token::Shape | Token::Kind) => break,
            _ => {}
        }
    }
    let Some(match_q) = found else { return Vec::new() };

    // The subject's kind, checked without the half-typed `case` line.
    let case_start = k.range(case).start;
    let line_end = k.range(k.line_end(case)).start;
    let probe = probe(source, case_start, line_end.max(case_start));
    let exact = Knowledge::new(source, scan, &probe);
    let subject_end = k.line_end(match_q).checked_sub(1).filter(|&q| q > match_q);
    let kind = match subject_end.and_then(|q| exact.type_ending_at(q)) {
        Some(Type::Named(name)) => k.global(&name, SymbolKind::Kind),
        _ => None,
    };

    // Cases already written in this match.
    let mut covered = Vec::new();
    let mut depth = 0usize;
    for q in match_q + 1..k.len() {
        match k.token(q) {
            Some(Token::When | Token::Loop | Token::Each | Token::Match) => depth += 1,
            Some(Token::Fi) if depth == 0 => break,
            Some(Token::Fi) => depth -= 1,
            Some(Token::Case) if depth == 0 && q != case => {
                if let Some(tag) = k.ident(q + 1) {
                    covered.push(tag.to_string());
                }
            }
            Some(Token::Task | Token::Shape | Token::Kind) => break,
            _ => {}
        }
    }

    let variants: Vec<usize> = match kind {
        Some(kind) => k.children(kind).map(|(id, _)| id).collect(),
        None => k.symbols().iter().enumerate().filter(|(_, s)| s.kind == SymbolKind::Variant).map(|(id, _)| id).collect(),
    };
    let uncovered: Vec<usize> = variants
        .into_iter()
        .filter(|&id| k.symbol(id).is_some_and(|v| !covered.contains(&v.name)))
        .collect();

    let mut items: Vec<CompletionItem> = uncovered
        .iter()
        .filter_map(|&id| {
            let variant = k.symbol(id)?;
            let template = case_template(k, id, &mut Vec::new());
            Some(CompletionItem {
                label: variant.name.clone(),
                kind: CompletionKind::Variant,
                detail: variant.detail.clone(),
                apply: None,
                snippet: (template != variant.name).then_some(template),
                boost: LOCAL,
            })
        })
        .collect();
    if kind.is_some() && uncovered.len() > 1 {
        let names: Vec<&str> = uncovered.iter().filter_map(|&id| k.symbol(id).map(|v| v.name.as_str())).collect();
        let mut used = Vec::new();
        let cases: Vec<String> = uncovered.iter().map(|&id| case_template(k, id, &mut used)).collect();
        items.push(CompletionItem {
            label: "all remaining cases".to_string(),
            kind: CompletionKind::Snippet,
            detail: names.join(", "),
            apply: None,
            snippet: Some(format!("{}\n\t${{}}", cases.join("\n\t\ncase "))),
            boost: SNIPPET,
        });
    }
    items
}

/// `Rectangle ${width} ${height}`: a variant with a field for each binding,
/// named after its payload fields. `used` holds the field names already in
/// the snippet, so that cases of variants sharing a payload field name get
/// separate fields.
fn case_template(k: &Knowledge, variant: usize, used: &mut Vec<String>) -> String {
    let mut text = k.symbol(variant).map(|v| v.name.clone()).unwrap_or_default();
    for (_, field) in k.children(variant) {
        text.push_str(&format!(" ${{{}}}", unique(used, &field.name)));
    }
    text
}

/// `name`, or `name2`, `name3`, … if already used: snippet fields with the
/// same name would be edited together.
fn unique(used: &mut Vec<String>, name: &str) -> String {
    let mut candidate = name.to_string();
    let mut n = 2;
    while used.contains(&candidate) {
        candidate = format!("{}{}", name, n);
        n += 1;
    }
    used.push(candidate.clone());
    candidate
}

fn type_items(k: &Knowledge, after_arrow: bool) -> Vec<CompletionItem> {
    let descriptions = [
        ("int", "64-bit signed integer"),
        ("float", "64-bit floating point"),
        ("bool", "true or false"),
        ("text", "text"),
        ("unit", "no value"),
    ];
    let builtin = BUILTIN_TYPES
        .iter()
        .filter(|(_, ty)| after_arrow || *ty != Type::Unit)
        .map(|(name, _)| CompletionItem {
            label: name.to_string(),
            kind: CompletionKind::Type,
            detail: descriptions.iter().find(|(n, _)| n == name).map_or("", |(_, d)| d).to_string(),
            apply: None,
            snippet: None,
            boost: SNIPPET,
        });
    let declared = k
        .symbols()
        .iter()
        .filter(|s| matches!(s.kind, SymbolKind::Shape | SymbolKind::Kind))
        .map(|s| CompletionItem {
            label: s.name.clone(),
            kind: CompletionKind::Type,
            detail: if s.kind == SymbolKind::Shape { "shape" } else { "kind" }.to_string(),
            apply: None,
            snippet: None,
            boost: GLOBAL,
        });
    builtin.chain(declared).collect()
}

fn keyword(word: &str) -> CompletionItem {
    CompletionItem {
        label: word.to_string(),
        kind: CompletionKind::Keyword,
        detail: String::new(),
        apply: None,
        snippet: None,
        boost: KEYWORD,
    }
}

fn snippet(label: &str, detail: &str, template: &str) -> CompletionItem {
    CompletionItem {
        label: label.to_string(),
        kind: CompletionKind::Snippet,
        detail: detail.to_string(),
        apply: None,
        snippet: Some(template.to_string()),
        boost: SNIPPET,
    }
}

fn item_level_items() -> Vec<CompletionItem> {
    vec![
        keyword("task"),
        keyword("shape"),
        keyword("kind"),
        snippet("task", "a task", "task ${name}(${})\n\t${0}\nfi"),
        snippet("task -> type", "a task that sends a value", "task ${name}(${}) -> ${type}\n\t<- ${value}\nfi"),
        snippet("shape", "a shape with a field", "shape ${Name}\n\t${field}: ${type}\nfi"),
        snippet("kind", "a kind with a variant", "kind ${Name}\n\t${Variant}\nfi"),
    ]
}

fn statement_items(k: &Knowledge, blocks: &[Block], byte: usize) -> Vec<CompletionItem> {
    let mut items: Vec<CompletionItem> = STATEMENT_KEYWORDS.iter().map(|word| keyword(word)).collect();
    items.push(keyword("fi"));
    match blocks.last() {
        Some(Block::When) => items.push(keyword("else")),
        Some(Block::Match) => items.push(keyword("case")),
        _ => {}
    }
    items.extend([
        snippet("when", "a condition", "when ${condition}\n\t${}\nfi"),
        snippet("when … else", "a condition with an alternative", "when ${condition}\n\t${}\nelse\n\t\nfi"),
        snippet("loop", "repeat while a condition holds", "loop ${condition}\n\t${}\nfi"),
        snippet("each", "count through a range", "each ${i} in ${start}..${end}\n\t${}\nfi"),
        snippet("match", "a match on a kind", "match ${value}\ncase ${Variant}\n\t${}\nfi"),
    ]);
    // A `match` with every case, for each visible value of a kind.
    for (id, symbol) in visible_locals(k, byte) {
        let Some(Type::Named(kind_name)) = k.type_of_symbol(id) else { continue };
        let Some(kind) = k.global(&kind_name, SymbolKind::Kind) else { continue };
        let mut used = Vec::new();
        let cases: Vec<String> = k.children(kind).map(|(variant, _)| case_template(k, variant, &mut used)).collect();
        if cases.is_empty() {
            continue;
        }
        let mut item = snippet(
            &format!("match {}", symbol.name),
            &format!("every case of {}", kind_name),
            &format!("match {}\ncase {}\n\t${{}}\nfi", symbol.name, cases.join("\n\t\ncase ")),
        );
        item.boost = GLOBAL;
        items.push(item);
    }
    items.extend(local_items(k, byte));
    items.extend(task_items(k));
    items
}

fn expression_items(k: &Knowledge, byte: usize) -> Vec<CompletionItem> {
    let mut items = local_items(k, byte);
    items.extend(task_items(k));
    for (_, variant) in k.symbols().iter().enumerate().filter(|(_, s)| s.kind == SymbolKind::Variant) {
        let fields: Vec<String> = parameter_names(&variant.detail);
        let mut used = Vec::new();
        let snippet = (!fields.is_empty()).then(|| {
            let args: Vec<String> = fields.iter().map(|f| format!("${{{}}}", unique(&mut used, f))).collect();
            format!("{}({})", variant.name, args.join(", "))
        });
        let kind = variant.parent.and_then(|p| k.symbol(p)).map(|s| s.name.as_str()).unwrap_or_default();
        items.push(CompletionItem {
            label: variant.name.clone(),
            kind: CompletionKind::Variant,
            detail: format!("{} {}", kind, variant.detail).trim().to_string(),
            apply: None,
            snippet,
            boost: GLOBAL,
        });
    }
    items.extend(VALUE_KEYWORDS.iter().map(|word| keyword(word)));
    items
}

/// The variables and parameters visible at `byte`, innermost last.
fn visible_locals<'k>(k: &'k Knowledge, byte: usize) -> impl Iterator<Item = (usize, &'k crate::scan::Symbol)> {
    k.symbols().iter().enumerate().filter(move |(_, s)| {
        matches!(s.kind, SymbolKind::Variable | SymbolKind::Parameter) && s.visible.start <= byte && byte <= s.visible.end
    })
}

fn local_items(k: &Knowledge, byte: usize) -> Vec<CompletionItem> {
    let mut seen: Vec<&str> = Vec::new();
    let mut items = Vec::new();
    // Innermost first, so a name appears once even if the scan saw it twice.
    let locals: Vec<_> = visible_locals(k, byte).collect();
    for (id, symbol) in locals.into_iter().rev() {
        if seen.contains(&symbol.name.as_str()) {
            continue;
        }
        seen.push(&symbol.name);
        let (kind, fallback) = if symbol.kind == SymbolKind::Parameter {
            (CompletionKind::Parameter, "parameter")
        } else {
            (CompletionKind::Variable, symbol.intro.unwrap_or("variable"))
        };
        items.push(CompletionItem {
            label: symbol.name.clone(),
            kind,
            detail: k.type_of_symbol(id).map_or_else(|| fallback.to_string(), |ty| ty.to_string()),
            apply: None,
            snippet: None,
            boost: LOCAL,
        });
    }
    items
}

fn task_items(k: &Knowledge) -> Vec<CompletionItem> {
    let mut items: Vec<CompletionItem> = k
        .symbols()
        .iter()
        .filter(|s| s.kind == SymbolKind::Task)
        .map(|task| {
            let params = parameter_names(&task.detail);
            let mut used = Vec::new();
            let args: Vec<String> = params.iter().map(|p| format!("${{{}}}", unique(&mut used, p))).collect();
            let (apply, snippet) = if args.is_empty() {
                (Some(format!("{}()", task.name)), None)
            } else {
                (None, Some(format!("{}({})", task.name, args.join(", "))))
            };
            CompletionItem { label: task.name.clone(), kind: CompletionKind::Task, detail: task.detail.clone(), apply, snippet, boost: GLOBAL }
        })
        .collect();
    items.extend(BUILTIN_TASKS.iter().map(|name| CompletionItem {
        label: name.to_string(),
        kind: CompletionKind::BuiltinFn,
        detail: "(value: int | float | bool | text)".to_string(),
        apply: None,
        snippet: Some(format!("{}(${{value}})", name)),
        boost: GLOBAL,
    }));
    items
}
