//! Completion: what may be typed at the cursor, decided from the tokens
//! before it. Items are not filtered by the word being typed; the editor
//! filters and ranks them, using `boost` to prefer the likelier kinds.
//!
//! Where types matter (fields after `.`, the cases of a `match`), the file is
//! checked again with the half-typed text removed, so the compiler's types are
//! used whenever the rest of the file allows it.

use serde::Serialize;
use vela::ast::{BUILTIN_TASKS, BUILTIN_TYPES, Need, Type, unqualified};
use vela::lexer::{LexKind, Token};

use crate::infer::{Knowledge, parameter_names, parameters};
use crate::scan::{Scan, Symbol, SymbolKind};
use crate::types::{Checked, Context};

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
    /// A method of a shape or kind, after a dot.
    Method,
    Variant,
    Type,
    Region,
    Module,
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
    /// edited together), `${}` an unnamed field, and `\t` one level of
    /// indentation. Tab visits the fields in order; reaching the last one ends
    /// the snippet. (`${0}` isn't used: CodeMirror visits it first when unnumbered
    /// fields come before it.)
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

const STATEMENT_KEYWORDS: &[&str] = &["bind", "hold", "when", "loop", "each", "block", "match", "region"];
const VALUE_KEYWORDS: &[&str] = &["true", "false", "none", "not", "new", "fail", "match", "when"];

/// Completions at byte `byte` of `source`. Returns the items and the byte
/// range they replace.
pub(crate) fn complete(source: &str, scan: &Scan, checked: &Checked, context: Context, byte: usize) -> (Vec<CompletionItem>, usize, usize) {
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

    // On an `include` line: nothing for the path or a name after `=>`; in its
    // parentheses or braces, the names the module declares.
    if let Some(q) = prev
        && k.token(k.line_start(q)) == Some(&Token::Include)
    {
        let line = k.line_start(q);
        let items = match (line..=q).find(|&p| matches!(k.token(p), Some(Token::LParen | Token::LBrace))) {
            Some(open)
                if !(open..=q).any(|p| matches!(k.token(p), Some(Token::RParen | Token::RBrace)))
                    && matches!(prev_token, Some(Token::LParen | Token::LBrace | Token::Comma)) =>
            {
                let module = open.checked_sub(1).and_then(|p| k.module_at(p));
                module.map(|m| member_items(&k, m, Members::Listed)).unwrap_or_default()
            }
            // The path's start, where Vela's library is one of the modules
            // there are, and after `std/`, its modules.
            _ if prev_token == Some(&Token::Include) => library_items(true),
            _ if prev_token == Some(&Token::Slash)
                && q >= 2
                && k.ident(q - 1) == Some(vela::library::ROOT)
                && k.token(q - 2) == Some(&Token::Include) =>
            {
                library_items(false)
            }
            _ => Vec::new(),
        };
        return (items, word_start, byte);
    }

    // After a module's name and `.`: what the module declares.
    if let (Some(dot), Some(Token::Dot)) = (prev, prev_token)
        && let Some(module) = dot.checked_sub(1).and_then(|q| k.module_at(q))
    {
        let members = if dot.checked_sub(1).is_some_and(|q| starts_type(&k, q)) { Members::Types } else { Members::All };
        return (member_items(&k, module, members), word_start, byte);
    }

    // After the name of a kind no file declares and `.`: its variants, which no
    // symbol holds (see `builtin_variants`).
    if let (Some(dot), Some(Token::Dot)) = (prev, prev_token)
        && let Some(name) = dot.checked_sub(1).and_then(|q| k.ident(q))
        && let Some(variants) = builtin_variants(name)
    {
        let line = k.line_start(dot);
        let in_pattern = in_pattern(&k, line, dot);
        let covered = match k.token(line) == Some(&Token::Case) {
            true => k.enclosing_match(line).map(|match_q| covered_cases(&k, match_q, line)).unwrap_or_default(),
            false => Vec::new(),
        };
        let items = variants
            .into_iter()
            .filter(|(label, _, _)| !covered.contains(label))
            .map(|(label, pattern, detail)| {
                // A value is written `NoNumber(why)`, a pattern `NoNumber why`.
                let holds = pattern != label;
                let snippet = match (in_pattern, holds) {
                    (true, true) => Some(pattern),
                    (false, true) => Some(format!("{}({})", label, pattern.trim_start_matches(&label).trim())),
                    _ => None,
                };
                CompletionItem { label, kind: CompletionKind::Variant, detail, apply: None, snippet, boost: GLOBAL }
            })
            .collect();
        return (items, word_start, byte);
    }

    // After a kind's name and `.`: its variants, as values or in a pattern.
    if let (Some(dot), Some(Token::Dot)) = (prev, prev_token)
        && let Some(kind) = dot.checked_sub(1).and_then(|q| k.symbol_at(q)).filter(|&id| k.symbol(id).is_some_and(|s| s.kind == SymbolKind::Kind))
    {
        let line = k.line_start(dot);
        let in_pattern = in_pattern(&k, line, dot);
        let mut items = kind_variant_items(&k, kind, in_pattern);
        // A `case` offers the variants its match doesn't cover yet.
        if k.token(line) == Some(&Token::Case)
            && let Some(match_q) = k.enclosing_match(line)
        {
            let covered = covered_cases(&k, match_q, line);
            items.retain(|item| !covered.contains(&item.label));
        }
        // Outside a pattern, the kind's shared methods are called on it too.
        if !in_pattern {
            items.extend(shared_methods(&k, kind));
        }
        return (items, word_start, byte);
    }

    // After a shape's name and `.`: its shared fields and methods, which
    // belong to the shape itself rather than to any value of it.
    if let (Some(dot), Some(Token::Dot)) = (prev, prev_token)
        && let Some(shape) = dot.checked_sub(1).and_then(|q| k.symbol_at(q)).filter(|&id| k.symbol(id).is_some_and(|s| s.kind == SymbolKind::Shape))
    {
        let mut items: Vec<CompletionItem> = k
            .members(shape, SymbolKind::Field)
            .filter(|(_, field)| field.shared && (field.module.is_none() || !field.private))
            .map(|(_, field)| CompletionItem {
                label: field.name.clone(),
                kind: CompletionKind::Field,
                detail: field.detail.clone(),
                apply: None,
                snippet: None,
                boost: LOCAL,
            })
            .collect();
        items.extend(shared_methods(&k, shape));
        return (items, word_start, byte);
    }

    // `.` starting a value inside a method: the value it is called on, `.this`,
    // and its shape's or kind's fields and methods.
    if let (Some(dot), Some(Token::Dot)) = (prev, prev_token)
        && !dot.checked_sub(1).is_some_and(|q| ends_value(&k, q))
    {
        let mut items = Vec::new();
        if let Some(owner) = blocks.contains(&Block::Method).then(|| k.owner_around(word_start)).flatten() {
            let name = k.symbol(owner).map_or(String::new(), |s| s.name.clone());
            items.push(CompletionItem {
                label: vela::ast::THIS_WORD.to_string(),
                kind: CompletionKind::Keyword,
                detail: format!("{}, the value this method is called on", name),
                apply: None,
                snippet: None,
                boost: LOCAL,
            });
            items.extend(owner_members(&k, owner, ""));
        }
        return (items, word_start, byte);
    }

    // After `.`: fields and methods.
    if let (Some(dot), Some(Token::Dot)) = (prev, prev_token) {
        let probe = probe(source, context, k.range(dot).start, word_end);
        let exact = Knowledge::new(source, scan, &probe);
        let base = dot.checked_sub(1).and_then(|q| exact.type_ending_at(q));
        // An array's member is its length; a slice's are its length and its
        // changes, `add` and `del`.
        let members = |slice: bool, through_pointer: bool| {
            let prefix = if through_pointer { "^." } else { "" };
            let mut items = vec![CompletionItem {
                label: "len".to_string(),
                kind: CompletionKind::Field,
                detail: "int, the number of items".to_string(),
                apply: through_pointer.then(|| "^.len".to_string()),
                snippet: None,
                boost: LOCAL,
            }];
            if slice {
                items.push(CompletionItem {
                    label: "add".to_string(),
                    kind: CompletionKind::BuiltinFn,
                    detail: "add(value) or add(index, value)".to_string(),
                    apply: None,
                    snippet: Some(format!("{}add(${{value}})", prefix)),
                    boost: LOCAL,
                });
                items.push(CompletionItem {
                    label: "del".to_string(),
                    kind: CompletionKind::BuiltinFn,
                    detail: "del(index) or del(), gives the item".to_string(),
                    apply: None,
                    snippet: Some(format!("{}del(${{index}})", prefix)),
                    boost: LOCAL,
                });
            }
            items
        };
        // A number, a bool, or text turned into text in a region.
        let to_text = || CompletionItem {
            label: vela::ast::STR.to_string(),
            kind: CompletionKind::BuiltinFn,
            detail: "str() at r, the value as text in a region, as show writes it".to_string(),
            apply: None,
            snippet: Some(format!("{}() at ${{r}}", vela::ast::STR)),
            boost: LOCAL,
        };
        // Every value holding text, a slice, or a pointer can be copied; a
        // pointer's other members replace from the dot, and so does its copy.
        let can_copy = base.as_ref().is_some_and(|ty| exact.can_copy(ty));
        let copy = |from_dot: bool| {
            can_copy.then(|| CompletionItem {
                label: vela::ast::COPY.to_string(),
                kind: CompletionKind::BuiltinFn,
                detail: "copy() at r, a copy in a region".to_string(),
                apply: None,
                snippet: Some(format!("{}copy()", if from_dot { "." } else { "" })),
                boost: LOCAL,
            })
        };
        // Every value's type, as code writes it.
        let is_value = base.is_some();
        let type_of = |from_dot: bool| {
            is_value.then(|| CompletionItem {
                label: vela::ast::TYPE_WORD.to_string(),
                kind: CompletionKind::Field,
                detail: "text, the value's type as code writes it".to_string(),
                apply: None,
                snippet: from_dot.then(|| format!(".{}", vela::ast::TYPE_WORD)),
                boost: LOCAL,
            })
        };
        // An array, a slice, a tuple, a pointer, and a `maybe` are written as
        // text too (see the checker's `writable`); a shape or kind by its own.
        let written = matches!(
            base,
            Some(Type::Array(..) | Type::Slice(..) | Type::Map(..) | Type::Tuple(_) | Type::Ptr(..) | Type::Maybe(_) | Type::Named(..) | Type::Applied(_))
        );
        let with_copy = |mut items: Vec<CompletionItem>, from_dot: bool| {
            if written && !items.iter().any(|item| item.label == vela::ast::STR) {
                let mut str_item = to_text();
                if from_dot {
                    str_item.snippet = str_item.snippet.map(|snippet| format!(".{}", snippet));
                }
                items.push(str_item);
            }
            items.extend(copy(from_dot));
            items.extend(type_of(from_dot));
            items
        };
        match &base {
            // A tuple's items, by position.
            Some(Type::Tuple(items)) => {
                let items = items
                    .iter()
                    .enumerate()
                    .map(|(i, item)| CompletionItem {
                        label: i.to_string(),
                        kind: CompletionKind::Field,
                        detail: item.to_string(),
                        apply: None,
                        snippet: None,
                        boost: LOCAL,
                    })
                    .collect();
                return (with_copy(items, false), word_start, byte);
            }
            Some(Type::Array(..)) => return (with_copy(members(false, false), false), word_start, byte),
            // A map's number of keys, what it answers, its sides as slices, and
            // taking a key out; a key is added with `m[k] = v`.
            Some(Type::Map(..)) => {
                let member = |label: &str, kind: CompletionKind, detail: &str, snippet: Option<&str>| CompletionItem {
                    label: label.to_string(),
                    kind,
                    detail: detail.to_string(),
                    apply: None,
                    snippet: snippet.map(str::to_string),
                    boost: LOCAL,
                };
                let items = vec![
                    member("len", CompletionKind::Field, "int, the number of keys", None),
                    member("has", CompletionKind::BuiltinFn, "has(key), whether the map holds the key", Some("has(${key})")),
                    member("keys", CompletionKind::Field, "keys at r, the keys as a slice in r, in order", Some("keys at ${r}")),
                    member("values", CompletionKind::Field, "values at r, the values as a slice in r, in order", Some("values at ${r}")),
                    member("del", CompletionKind::BuiltinFn, "del(key), takes the key out and gives its value, or none", Some("del(${key})")),
                ];
                return (with_copy(items, false), word_start, byte);
            }
            // Text's members are its length in bytes, `at`, and `str`; a
            // number's, a char's, and a bool's, `str`.
            Some(Type::Text(_)) => {
                let len = CompletionItem {
                    label: vela::ast::LEN.to_string(),
                    kind: CompletionKind::Field,
                    detail: "int, the number of bytes".to_string(),
                    apply: None,
                    snippet: None,
                    boost: LOCAL,
                };
                let at = CompletionItem {
                    label: vela::ast::AT.to_string(),
                    kind: CompletionKind::BuiltinFn,
                    detail: "at(i), the byte at i as a maybe char, none out of range".to_string(),
                    apply: None,
                    snippet: Some(format!("{}(${{i}})", vela::ast::AT)),
                    boost: LOCAL,
                };
                return (with_copy(vec![len, at, to_text()], false), word_start, byte);
            }
            Some(Type::Int | Type::Unit | Type::Char | Type::Float | Type::Bool) => return (with_copy(vec![to_text()], false), word_start, byte),
            Some(Type::Slice(..)) => return (with_copy(members(true, false), false), word_start, byte),
            Some(Type::Ptr(inner, _)) if matches!(**inner, Type::Array(..) | Type::Slice(..)) => {
                return (with_copy(members(matches!(**inner, Type::Slice(..)), true), true), k.range(dot).start, byte);
            }
            _ => {}
        }
        // A shape given types (`Box<int>`) has the fields and methods its
        // declaration writes.
        let named = |ty: Type| match ty {
            Type::Named(name, _) => Some(name),
            Type::Applied(applied) => Some(applied.name),
            _ => None,
        };
        let (owner, through_pointer) = match base {
            Some(Type::Ptr(inner, _)) => match named(*inner) {
                Some(owner) => (owner, true),
                None => return (with_copy(Vec::new(), false), word_start, byte),
            },
            Some(other) => match named(other) {
                Some(owner) => (owner, false),
                None => return (with_copy(Vec::new(), false), word_start, byte),
            },
            None => return (with_copy(Vec::new(), false), word_start, byte),
        };
        let Some(owner_id) = k.global(&owner, SymbolKind::Shape).or_else(|| k.global(&owner, SymbolKind::Kind)) else {
            return (with_copy(Vec::new(), false), word_start, byte);
        };
        // A pointer's members are reached through `^`: `p^.x`.
        let items = with_copy(owner_members(&k, owner_id, if through_pointer { "^." } else { "" }), through_pointer);
        let from = if through_pointer { k.range(dot).start } else { word_start };
        return (items, from, byte);
    }

    let line_first = prev.map(|q| k.line_start(q + 1));
    let first_token = line_first.and_then(|q| k.token(q));

    // After `priv` starting a line outside any item: what it can mark. (Before a
    // shape's field, the field's name is being declared.)
    if let (Some(q), Some(Token::Priv)) = (prev, prev_token)
        && k.line_start(q) == q
    {
        let items = match blocks.as_slice() {
            [] => item_level_items().into_iter().filter(|item| item.label != "priv" && item.label != "include").collect(),
            // A private method may change its value, and a shape's shared
            // field or method, or a kind's shared method, may be private.
            [Block::Shape] => vec![keyword(vela::ast::ALTERS), keyword("shared")],
            [Block::Kind] => vec![keyword(vela::ast::ALTERS), keyword("shared")],
            _ => Vec::new(),
        };
        return (items, word_start, byte);
    }

    // After `shared` in a shape: whether the field changes, and who changes it,
    // or `new`, a shared method's name. (Otherwise its name is being declared.)
    if prev_token == Some(&Token::Shared) && blocks.as_slice() == [Block::Shape] {
        return (vec![keyword("hold"), keyword(vela::ast::READ), keyword(vela::ast::NEW_WORD)], word_start, byte);
    }
    if prev_token == Some(&Token::Shared) && blocks.as_slice() == [Block::Kind] {
        return (vec![keyword(vela::ast::NEW_WORD)], word_start, byte);
    }

    // `region r default` and `r: region default`: the word that makes a region
    // the default of a block or of a task's body (see `ast::DEFAULT`).
    if let Some(q) = prev {
        let before = q.checked_sub(1).and_then(|before| k.token(before));
        let declared = matches!(prev_token, Some(Token::Ident(_))) && before == Some(&Token::Region);
        let taken = matches!(prev_token, Some(Token::Region)) && before == Some(&Token::Colon);
        if declared || taken {
            return (vec![default_region()], word_start, byte);
        }
    }

    // Names being declared get no suggestions, including another name in a
    // `bind` or `hold` list (`bind q, r = …`).
    if matches!(prev_token, Some(Token::Task | Token::Shape | Token::Kind | Token::Bind | Token::Hold | Token::Each | Token::Region)) {
        return (Vec::new(), word_start, byte);
    }
    // `block` takes nothing on its line.
    if prev_token == Some(&Token::Block) {
        return (Vec::new(), word_start, byte);
    }
    // `hold& name` and `bind& name`: the `&` is part of the word.
    if prev_token == Some(&Token::Amp) && matches!(prev.and_then(|q| q.checked_sub(1)).and_then(|q| k.token(q)), Some(Token::Hold | Token::Bind)) {
        return (Vec::new(), word_start, byte);
    }
    if let (Some(q), Some(Token::Comma)) = (prev, prev_token)
        && in_binding_names(&k, q)
    {
        return (Vec::new(), word_start, byte);
    }
    // The next name of a named result list (`-> total: int, peak: int`).
    if let Some(q) = prev
        && names_a_result(&k, q)
    {
        return (Vec::new(), word_start, byte);
    }
    // A region: after `@` in a type or `new@`, or after the `at` of `new T(v) at`.
    // `new` allocates, so it takes only a region the task can allocate in.
    if let Some(q) = prev
        && (prev_token == Some(&Token::AtSign) || prev_token == Some(&Token::At))
    {
        // `scan(…)@r` and `"n = {n}"@r` allocate too: both make new text.
        let allocating = prev_token == Some(&Token::At) || matches!(k.token_before(q), Some(Token::New | Token::RParen | Token::Text(_)));
        return (region_items(&k, word_start, allocating), word_start, byte);
    }
    // After `is`: patterns. Names a pattern or plan B declares (`is {v}`,
    // `is failure e`, `{ failure e`) get no suggestions.
    if let (Some(is), Some(Token::Is)) = (prev, prev_token) {
        return (is_pattern_items(source, scan, context, &k, is), word_start, byte);
    }
    // The rest of the line after `is` is the pattern, whose names are
    // declared, up to an `and` joining it to what reads them.
    if let Some(q) = prev
        && (k.line_start(q + 1)..=q).rev().find(|&p| matches!(k.token(p), Some(Token::Is | Token::And))).is_some_and(|p| p < q && k.token(p) == Some(&Token::Is))
    {
        return (Vec::new(), word_start, byte);
    }
    let before_prev = prev.and_then(|q| k.token_before(q));
    let declares_name = match prev_token {
        Some(Token::LBrace) => before_prev == Some(&Token::Is),
        Some(Token::Failure) => matches!(before_prev, Some(Token::Is | Token::LBrace | Token::Newline))
            && prev.is_some_and(|q| starts_plan_b_or_pattern(&k, q)),
        _ => false,
    };
    if declares_name {
        return (Vec::new(), word_start, byte);
    }
    // Inside `< … >`: the types a declaration takes, or the ones a call gives.
    if let Some(open) = prev.and_then(|q| type_angle(&k, q)) {
        let declares = open.checked_sub(2).is_some_and(|before| matches!(k.token(before), Some(Token::Task | Token::Shape)));
        if declares {
            return match prev_token {
                // `<T: ` and `<T: fixed & `: what the type must allow.
                Some(Token::Colon | Token::Amp) => (need_items(), word_start, byte),
                // A name is being declared.
                _ => (Vec::new(), word_start, byte),
            };
        }
    }
    let inside_parens = prev.is_some_and(|q| unclosed_on_line(&k, q, &Token::LParen, &Token::RParen));
    // A parameter's or payload's name follows, unless the parentheses are a
    // tuple type's, where an item's type follows.
    if matches!(prev_token, Some(Token::LParen | Token::Comma))
        && inside_parens
        && prev.and_then(|q| type_paren(&k, q)).is_none()
        && (first_token == Some(&Token::Task) || blocks.last() == Some(&Block::Kind) || prev.is_some_and(|q| method_header(&k, q)))
    {
        // A task's or a method's parameter may be one it changes (`hold n: int`);
        // a variant's value never is. In a kind, `V(` may be either until the
        // line says `alters` or what the method sends.
        let in_kind = blocks.contains(&Block::Kind);
        let says_method = prev.is_some_and(|q| {
            (k.line_start(q)..)
                .take_while(|&p| !matches!(k.token(p), Some(Token::Newline | Token::Eof) | None))
                .any(|p| matches!(k.token(p), Some(Token::Arrow)) || k.ident(p) == Some(vela::ast::ALTERS))
        });
        let takes = first_token == Some(&Token::Task) || (prev.is_some_and(|q| method_header(&k, q)) && (!in_kind || says_method));
        let items = if takes { vec![keyword("hold"), keyword("hold&"), keyword("bind"), keyword("bind&")] } else { Vec::new() };
        return (items, word_start, byte);
    }

    // `case`: the variants the match doesn't cover yet; after the variant, its
    // bindings. Past an `=`, the case gives a value, which is typed like any
    // other, so the paths below have it.
    let gives_value = prev.is_some_and(|q| (k.line_start(q)..=q).any(|p| k.token(p) == Some(&Token::Assign)));
    if first_token == Some(&Token::Case) && !gives_value {
        if prev_token != Some(&Token::Case) {
            return (Vec::new(), word_start, byte);
        }
        let items = prev.map(|case| case_items(source, scan, context, &k, case)).unwrap_or_default();
        return (items, word_start, byte);
    }

    // A field name in a shape literal: the fields not given yet. (A `{` after
    // anything but a shape's name opens a plan B, where values are typed.)
    if let Some(q) = prev
        && matches!(prev_token, Some(Token::LBrace | Token::Comma))
        && let Some(brace) = unclosed_brace(&k, q)
        && brace.checked_sub(1).and_then(|b| k.qualified_at(b)).is_some_and(|(name, _)| k.global(&name, SymbolKind::Shape).is_some())
    {
        return (literal_field_items(&k, brace, word_start), word_start, byte);
    }

    // An array type's count, `[int#4]`, is a number.
    if let (Some(q), Some(Token::Hash)) = (prev, prev_token)
        && type_bracket(&k, q).is_some()
    {
        return (Vec::new(), word_start, byte);
    }

    // `#NAME`: a macro's new name, at the start of a line.
    if let (Some(q), Some(Token::Hash)) = (prev, prev_token)
        && matches!(k.token(q.wrapping_sub(1)), None | Some(Token::Newline | Token::Priv))
    {
        return (Vec::new(), word_start, byte);
    }

    // Types.
    // After a `scan`'s prompt: the type of a number to read, the only two types
    // this form takes.
    if let (Some(q), Some(Token::Comma)) = (prev, prev_token)
        && after_scan_prompt(&k, q)
    {
        let items = [
            (vela::ast::INT_CAST, "read a whole number"),
            (vela::ast::FLOAT_CAST, "read a number with a fraction"),
            (vela::ast::UNIT_CAST, "read a whole number from 0 to 255"),
        ]
            .into_iter()
            .map(|(name, detail)| CompletionItem {
                label: name.to_string(),
                kind: CompletionKind::Type,
                detail: detail.to_string(),
                apply: None,
                snippet: None,
                boost: SNIPPET,
            })
            .collect();
        return (items, word_start, byte);
    }

    let type_context = match (prev, prev_token) {
        (Some(q), Some(Token::Colon)) => !unclosed_on_line(&k, q, &Token::LBrace, &Token::RBrace),
        (_, Some(Token::Arrow | Token::New)) => true,
        // `-> int, int`: another result type follows the comma; so does the
        // next item of a tuple type, `(int, text)`.
        (Some(q), Some(Token::Comma)) => in_result_list(&k, q) || type_paren(&k, q).is_some() || type_angle(&k, q).is_some(),
        // `(int, text)`: the first item's type follows the `(`.
        (Some(q), Some(Token::LParen)) => starts_type(&k, q),
        // `first<int>(…)`: the types a call gives a generic task.
        (Some(q), Some(Token::Lt)) => opens_types(&k, q),
        // `new@r T`: the type follows the region.
        (Some(q), Some(Token::Ident(_))) => after_new_region(&k, q),
        // A type continues after `^`, `maybe`, and an array's or slice's `[`,
        // and `~` starts the error type.
        (Some(q), Some(Token::Caret | Token::Maybe | Token::LBracket)) => starts_type(&k, q),
        (Some(q), Some(Token::Tilde)) => q.checked_sub(1).is_some_and(|end| ends_type(&k, end)),
        _ => false,
    };
    if type_context {
        let after_arrow = prev_token == Some(&Token::Arrow);
        // A maybe can't hold a maybe, and an error type can't be one.
        let offer_maybe = !matches!(prev_token, Some(Token::Maybe | Token::Tilde));
        // The kind reading a number sends is offered where an error type goes,
        // which is where a program writes it.
        let mut items = type_items(&k, byte, after_arrow, offer_maybe, prev_token == Some(&Token::Tilde));
        // A parameter can be a region the task allocates in; a result can't,
        // since a task never sends one.
        if prev_token == Some(&Token::Colon) && (first_token == Some(&Token::Task) || prev.is_some_and(|q| method_header(&k, q))) && inside_parens {
            items.push(CompletionItem {
                label: "region".to_string(),
                kind: CompletionKind::Keyword,
                detail: "a region to allocate in".to_string(),
                apply: None,
                snippet: None,
                boost: SNIPPET,
            });
        }
        return (items, word_start, byte);
    }

    // Nothing but a `when` follows an `else` on its line: `else when …`
    // continues the chain, and one `fi` closes the whole of it.
    if prev_token == Some(&Token::Else) {
        return (vec![keyword("when")], word_start, byte);
    }

    // (`prev_token` is also `None` after invalid text, which isn't a line start.)
    let at_line_start = prev.is_none() || prev_token == Some(&Token::Newline);
    if at_line_start {
        // A method's body holds statements, as a task's does.
        let item = if blocks.contains(&Block::Method) { Some(&Block::Method) } else { blocks.first() };
        let items = match item {
            // `include` lines come before the file's items.
            None => {
                let before_items = !(0..prev.map_or(0, |q| q + 1)).any(|q| matches!(k.token(q), Some(Token::Task | Token::Shape | Token::Kind)));
                item_level_items().into_iter().filter(|item| before_items || item.label != "include").collect()
            }
            // A shape's field and a method can be private, a shape's field can
            // be read-only or shared, and a method can change its value.
            Some(Block::Shape) => vec![keyword("priv"), keyword(vela::ast::READ), keyword("shared"), keyword(vela::ast::ALTERS)],
            Some(Block::Kind) => vec![keyword("priv"), keyword(vela::ast::ALTERS), keyword("shared")],
            Some(_) => {
                let probe = probe(source, context, word_start, word_end);
                let exact = Knowledge::new(source, scan, &probe);
                // A plan B's first line may name its failure: `failure e`.
                let names_error = prev.is_some_and(|q| {
                    blocks.last() == Some(&Block::PlanB) && starts_plan_b_or_pattern(&k, q + 1) && plan_b_value_can_fail(&exact, q)
                });
                statement_items(&exact, &blocks, word_start, names_error)
            }
        };
        return (items, word_start, byte);
    }
    if blocks.is_empty() || (matches!(blocks.first(), Some(Block::Shape | Block::Kind)) && !blocks.contains(&Block::Method)) {
        return (Vec::new(), word_start, byte);
    }
    // A region is a value only as the argument for a task's region parameter,
    // which must be a region the caller can allocate in.
    let parameter = prev.filter(|_| inside_parens).and_then(|q| argument_parameter(&k, q));
    if parameter == Some(Some(Type::Region)) {
        return (region_items(&k, word_start, true), word_start, byte);
    }
    // After a finished value, only the words that can continue it.
    if let Some(q) = prev
        && k.range(q).end < word_start
        && ends_value(&k, q)
    {
        return (continuation_items(&k, q), word_start, byte);
    }
    let mut items = expression_items(&k, word_start);
    if !inside_parens || parameter.is_some() {
        items.retain(|item| item.kind != CompletionKind::Region);
    }
    // A `loop` takes its condition directly, so a `when` can't start it.
    if prev_token == Some(&Token::Loop) {
        items.retain(|item| item.label != "when" && item.label != "when … else");
    }
    (items, word_start, byte)
}

/// Whether the token at position `q` can end a value, a type, or a loop's name.
fn ends_value(k: &Knowledge, q: usize) -> bool {
    k.token(q).is_some_and(crate::scan::ends_value)
}

/// Whether the `)` at position `q` ends a value that names its region after
/// it, with `at`: a `new T(…)` (not `new@r T(…)`), a `scan(…)`, or a
/// `x.copy()`.
fn names_region_after(k: &Knowledge, q: usize) -> bool {
    if k.copied_value(q).is_some() {
        return true;
    }
    let Some(before) = k.matching_open(q, &Token::LParen, &Token::RParen).and_then(|open| open.checked_sub(1)) else { return false };
    if k.ident(before) == Some(vela::ast::SCAN) {
        // The form that reads a number, `scan(prompt, int)`, names no region.
        let number = q.checked_sub(1).and_then(|p| k.ident(p)).is_some_and(|name| vela::ast::number_cast(name).is_some());
        return !number && k.token_before(before) != Some(&Token::Dot);
    }
    // The type between `new` and `(`.
    let mut p = before;
    loop {
        match k.token(p) {
            Some(Token::New) => return k.token(p + 1) != Some(&Token::AtSign),
            Some(Token::Ident(_) | Token::Dot | Token::LBracket | Token::RBracket | Token::Hash | Token::Int(_) | Token::Caret | Token::Maybe) => {}
            _ => return false,
        }
        let Some(previous) = p.checked_sub(1) else { return false };
        p = previous;
    }
}

/// The words that can follow the finished value, type, or name ending at
/// position `q`: `in` after an `each`'s name or a `new`'s value, `~` after a
/// written type, `is` in a condition, and `and`/`or` after a value.
fn continuation_items(k: &Knowledge, q: usize) -> Vec<CompletionItem> {
    let line = k.line_start(q);
    let on_line = |token: &Token| (line..=q).any(|p| k.token(p) == Some(token));
    if k.token_before(q) == Some(&Token::Each) {
        return vec![keyword("in")];
    }
    if ends_type(k, q) {
        return if on_line(&Token::Tilde) { Vec::new() } else { vec![error_separator()] };
    }
    let mut items = Vec::new();
    if names_region_after(k, q) {
        items.push(keyword("at"));
    }
    // After `and`, another part may be an `is`; an `is` is never joined with
    // `or`, so a condition holding one goes on with `and` alone.
    let condition = matches!(k.token(line), Some(Token::When | Token::Loop));
    let last_join = (line..q).rev().find(|&p| matches!(k.token(p), Some(Token::Is | Token::And)));
    if condition && last_join.is_none_or(|p| k.token(p) == Some(&Token::And)) {
        items.push(keyword("is"));
    }
    items.push(keyword("and"));
    if !(condition && on_line(&Token::Is)) {
        items.push(keyword("or"));
    }
    items
}

/// The parameter whose argument is being typed after position `q`, inside a
/// call's parentheses on the same line: `None` when the call isn't of a known
/// task, and `Some(None)` when the task has no such parameter, it is `show`,
/// or the parameter's type doesn't parse.
fn argument_parameter(k: &Knowledge, q: usize) -> Option<Option<Type>> {
    let mut depth = 0usize;
    let mut index = 0;
    for p in (k.line_start(q)..=q).rev() {
        match k.token(p) {
            Some(Token::RParen | Token::RBrace | Token::RBracket) => depth += 1,
            Some(Token::LBrace | Token::LBracket) if depth == 0 => return None,
            Some(Token::LParen) if depth == 0 => {
                let (name, _) = k.qualified_at(p.checked_sub(1)?)?;
                if BUILTIN_TASKS.contains(&name.as_str()) {
                    return Some(None);
                }
                let task = k.symbol(k.global(&name, SymbolKind::Task)?)?;
                return Some(parameters(&task.detail).into_iter().nth(index).and_then(|(_, ty)| ty));
            }
            Some(Token::LParen | Token::LBrace | Token::LBracket) => depth -= 1,
            Some(Token::Comma) if depth == 0 => index += 1,
            _ => {}
        }
    }
    None
}

/// Whether the comma at position `q` separates a task's results, and whether
/// the list names them: `Some(true)` for `-> total: int, …`, where a name
/// follows the comma, and `Some(false)` for `-> int, …`, where a type does.
/// Either every result has a name or none does, so the result before the comma
/// says which list this is.
fn result_list(k: &Knowledge, q: usize) -> Option<bool> {
    let line = k.line_start(q);
    if k.token(line) != Some(&Token::Task) {
        return None;
    }
    let arrow = (line..q).find(|&p| k.token(p) == Some(&Token::Arrow))?;
    let start = (arrow + 1..q).rev().find(|&p| k.token(p) == Some(&Token::Comma)).map_or(arrow + 1, |p| p + 1);
    Some(k.ident(start).is_some() && k.token(start + 1) == Some(&Token::Colon))
}

/// Whether the comma at position `q` separates a task's result types, so
/// another type follows it.
fn in_result_list(k: &Knowledge, q: usize) -> bool {
    result_list(k, q) == Some(false)
}

/// Whether position `q` is where a named result list declares its next name,
/// after the comma and any `^` or `maybe` typed since.
fn names_a_result(k: &Knowledge, q: usize) -> bool {
    let mut p = q;
    while matches!(k.token(p), Some(Token::Caret | Token::Maybe)) {
        let Some(before) = p.checked_sub(1) else { return false };
        p = before;
    }
    k.token(p) == Some(&Token::Comma) && result_list(k, p) == Some(true)
}

/// Whether the comma at position `q` separates the names of a `bind` or
/// `hold`, so another name follows it. A comma inside a name's tuple type
/// separates its items, not names.
fn in_binding_names(k: &Knowledge, q: usize) -> bool {
    let line = k.line_start(q);
    matches!(k.token(line), Some(Token::Bind | Token::Hold))
        && !(line..q).any(|p| k.token(p) == Some(&Token::Assign))
        && type_paren(k, q).is_none()
}

/// Whether the type-starting token at position `q` (a name, `^`, `maybe`, or
/// `[`) is in a type: before it, after any `^`, `maybe`, and `[`, comes `:`
/// (outside a shape literal), `->`, `new`, `~` after a type, or a module's
/// name and `.` that are in a type themselves.
fn starts_type(k: &Knowledge, q: usize) -> bool {
    let mut p = q;
    while p > 0 && matches!(k.token(p - 1), Some(Token::Caret | Token::Maybe | Token::LBracket)) {
        p -= 1;
    }
    match p.checked_sub(1).map(|b| (b, k.token(b))) {
        Some((b, Some(Token::Colon))) => !unclosed_on_line(k, b, &Token::LBrace, &Token::RBrace),
        Some((_, Some(Token::Arrow | Token::New))) => true,
        Some((b, Some(Token::Comma))) => in_result_list(k, b) || type_paren(k, b).is_some(),
        // `(int, text)`: a tuple type's items are types themselves.
        Some((b, Some(Token::LParen))) => starts_type(k, b),
        Some((b, Some(Token::Ident(_)))) => after_new_region(k, b),
        Some((b, Some(Token::Tilde))) => b.checked_sub(1).is_some_and(|end| ends_type(k, end)),
        // `geometry.Point`: the type goes on after the module's name.
        Some((b, Some(Token::Dot))) => b.checked_sub(1).is_some_and(|module| k.module_at(module).is_some() && starts_type(k, module)),
        _ => false,
    }
}

/// The `<` of the types a declaration takes, or a call gives, that position `q`
/// is inside, on its line.
fn type_angle(k: &Knowledge, q: usize) -> Option<usize> {
    let mut depth = 0usize;
    for p in (k.line_start(q)..=q).rev() {
        match k.token(p) {
            Some(Token::Gt) => depth += 1,
            Some(Token::Lt) if depth == 0 => return opens_types(k, p).then_some(p),
            Some(Token::Lt) => depth -= 1,
            _ => {}
        }
    }
    None
}

/// Whether the `<` at `p` opens types: one right after the name of a `task` or
/// `shape`, one after the name of a shape that takes types in a written type, or
/// one whose matching `>` is followed by `(` (a call) or `{` (a shape literal),
/// as the parser reads them.
fn opens_types(k: &Knowledge, p: usize) -> bool {
    if p >= 2 && k.ident(p - 1).is_some() && matches!(k.token(p - 2), Some(Token::Task | Token::Shape)) {
        return true;
    }
    // `Box<` in a written type, where the name is a shape that takes types: the
    // `>` may not be typed yet.
    if let Some(name) = p.checked_sub(1).and_then(|q| k.ident(q))
        && k.global(name, SymbolKind::Shape).is_some_and(|id| k.symbols().get(id).is_some_and(|shape| shape.detail.starts_with('<')))
    {
        return true;
    }
    let mut depth = 0usize;
    // Parentheses pair up inside the types, and no `>` inside them closes
    // anything, as the parser has it (`(i < n), m > (k)` compares).
    let mut grouped = 0usize;
    for q in p + 1.. {
        match k.token(q) {
            Some(Token::Gt) if grouped > 0 && depth == 0 => return false,
            Some(Token::LParen) => grouped += 1,
            Some(Token::RParen) if grouped == 0 => return false,
            Some(Token::RParen) => grouped -= 1,
            // A call follows the types a task is given, and a literal the types
            // a shape is given.
            Some(Token::Gt) if depth == 0 => return matches!(k.token(q + 1), Some(Token::LParen | Token::LBrace)),
            Some(Token::Lt) => depth += 1,
            Some(Token::Gt) => depth -= 1,
            Some(token) if token.writes_type() => {}
            _ => return false,
        }
    }
    false
}

/// The `(` of a tuple type that position `q` is inside, on its line.
fn type_paren(k: &Knowledge, q: usize) -> Option<usize> {
    let mut depth = 0usize;
    for p in (k.line_start(q)..=q).rev() {
        match k.token(p) {
            Some(Token::RParen) => depth += 1,
            Some(Token::LParen) if depth == 0 => return starts_type(k, p).then_some(p),
            Some(Token::LParen) => depth -= 1,
            _ => {}
        }
    }
    None
}

/// Whether position `q`, a `,` inside a call's parentheses, is the one after a
/// `scan`'s prompt, where the type of a number to read goes:
/// `scan("Age: ", int)`.
fn after_scan_prompt(k: &Knowledge, q: usize) -> bool {
    let mut depth = 0usize;
    for p in (k.line_start(q)..q).rev() {
        match k.token(p) {
            Some(Token::RParen) => depth += 1,
            Some(Token::LParen) if depth == 0 => {
                // The name before the `(`, which a dot would make a method.
                return p.checked_sub(1).is_some_and(|name| k.ident(name) == Some(vela::ast::SCAN) && k.token_before(name) != Some(&Token::Dot));
            }
            Some(Token::LParen) => depth -= 1,
            // Only the first argument is the prompt.
            Some(Token::Comma) if depth == 0 => return false,
            _ => {}
        }
    }
    false
}

/// The `[` of an array or slice type that position `q` is inside, on its line.
fn type_bracket(k: &Knowledge, q: usize) -> Option<usize> {
    let mut depth = 0usize;
    for p in (k.line_start(q)..=q).rev() {
        match k.token(p) {
            Some(Token::RBracket) => depth += 1,
            Some(Token::LBracket) if depth == 0 => return starts_type(k, p).then_some(p),
            Some(Token::LBracket) => depth -= 1,
            _ => {}
        }
    }
    None
}

/// Whether position `q` is the region name of `new@r`, after which its type comes.
fn after_new_region(k: &Knowledge, q: usize) -> bool {
    k.ident(q).is_some() && k.token_before(q) == Some(&Token::AtSign) && q.checked_sub(2).is_some_and(|new| k.token(new) == Some(&Token::New))
}

/// Whether the type-ending name at position `q` (a type's name, or the region
/// after its `@`) ends a type, so that an `or` after it starts the error type.
fn ends_type(k: &Knowledge, q: usize) -> bool {
    // `[int#4]` or `[int]@r`.
    let closing = if k.token_before(q) == Some(&Token::AtSign) { q.checked_sub(2) } else { Some(q) };
    if let Some(close) = closing.filter(|&close| k.token(close) == Some(&Token::RBracket)) {
        return close.checked_sub(1).and_then(|inside| type_bracket(k, inside)).is_some();
    }
    if k.ident(q).is_none() {
        return false;
    }
    let name = if k.token_before(q) == Some(&Token::AtSign) { q.checked_sub(2) } else { Some(q) };
    name.is_some_and(|name| k.ident(name).is_some() && starts_type(k, name))
}

/// Whether the `fail` at position `q` names an error: it follows `is` or
/// Whether position `q` is in a pattern: after a `case` or an `is` on the
/// line starting at `line`, with no `and` since, which no pattern holds.
fn in_pattern(k: &Knowledge, line: usize, q: usize) -> bool {
    (line..q).rev().find(|&p| matches!(k.token(p), Some(Token::Case | Token::Is | Token::And))).is_some_and(|p| k.token(p) != Some(&Token::And))
}

/// `case`, or it is the first word inside a plan B's `{`.
fn starts_plan_b_or_pattern(k: &Knowledge, q: usize) -> bool {
    let mut p = q;
    while p > 0 && k.token(p - 1) == Some(&Token::Newline) {
        p -= 1;
    }
    matches!(p.checked_sub(1).and_then(|b| k.token(b)), Some(Token::Is | Token::Case | Token::LBrace))
}

/// Whether the plan B whose `{` is at or before position `q` (across line
/// breaks) is on a value that can fail, so its first line may name the error.
/// Unknown types are given the benefit of the doubt.
fn plan_b_value_can_fail(k: &Knowledge, q: usize) -> bool {
    let mut brace = q;
    while k.token(brace) == Some(&Token::Newline) {
        let Some(before) = brace.checked_sub(1) else { return true };
        brace = before;
    }
    if k.token(brace) != Some(&Token::LBrace) {
        return true;
    }
    match brace.checked_sub(1).and_then(|end| k.type_ending_at(end)) {
        Some(Type::Or(..)) | None => true,
        Some(_) => false,
    }
}

/// `source` with `from..to` removed, checked.
fn probe(source: &str, context: Context, from: usize, to: usize) -> Checked {
    let text = format!("{}{}", source.get(..from).unwrap_or_default(), source.get(to..).unwrap_or_default());
    Checked::new(&text, context)
}

/// Whether `byte` is inside a comment, a text literal, or a char, where
/// nothing is completed.
fn in_comment_or_text(source: &str, scan: &Scan, byte: usize) -> bool {
    scan.lexed.iter().any(|t| {
        let text = source.get(t.range.clone()).unwrap_or_default();
        let inside = t.range.start < byte && byte < t.range.end;
        let at_end = t.range.start < byte && byte == t.range.end;
        match &t.kind {
            LexKind::Comment => inside || at_end,
            LexKind::Token(Token::Text(_) | Token::Char(_)) => inside,
            // A rejected literal; an unterminated one runs to the cursor.
            LexKind::Invalid(_) if text.starts_with('"') => inside || (at_end && !closes_text(text)),
            LexKind::Invalid(_) if text.starts_with('\'') => inside || (at_end && (text.len() < 2 || !text.ends_with('\''))),
            _ => false,
        }
    })
}

/// Whether a text literal's last `"` closes it, rather than being its opening
/// quote or an escaped `\"`.
fn closes_text(text: &str) -> bool {
    let Some(body) = text.strip_prefix('"').and_then(|rest| rest.strip_suffix('"')) else { return false };
    body.chars().rev().take_while(|c| *c == '\\').count() % 2 == 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Block {
    Task,
    Shape,
    Kind,
    /// A method, inside a shape or a kind: its header and its body.
    Method,
    When,
    Match,
    Other,
    /// A plan B's block, opened by a `{` ending its line.
    PlanB,
}

/// The fields and methods of the shape or kind `owner`, written after a dot:
/// `prefix` goes before each (`^.` through a pointer). Another module's private
/// members can't be used here.
fn owner_members(k: &Knowledge, owner: usize, prefix: &str) -> Vec<CompletionItem> {
    k.children(owner)
        // A shared field is the shape's own, read through its name.
        .filter(|(_, member)| matches!(member.kind, SymbolKind::Field | SymbolKind::Method) && !member.shared && (member.module.is_none() || !member.private))
        .map(|(_, member)| {
            if member.kind == SymbolKind::Field {
                return CompletionItem {
                    label: member.name.clone(),
                    kind: CompletionKind::Field,
                    detail: member.detail.clone(),
                    apply: (!prefix.is_empty()).then(|| format!("{}{}", prefix, member.name)),
                    snippet: None,
                    boost: LOCAL,
                };
            }
            method_item(member, prefix)
        })
        .collect()
}

/// The shared methods of the shape or kind `owner`, called on its name
/// (`Point.new(…)`). Another module's private ones can't be used here.
fn shared_methods(k: &Knowledge, owner: usize) -> Vec<CompletionItem> {
    k.members(owner, SymbolKind::Method)
        .filter(|(_, method)| method.shared && (method.module.is_none() || !method.private))
        .map(|(_, method)| method_item(method, ""))
        .collect()
}

/// A method as offered after a dot, with `prefix` before it: its name, and its
/// parameters to fill in, headed by its word (`alters` or `shared`).
fn method_item(member: &Symbol, prefix: &str) -> CompletionItem {
    let params = parameter_names(&member.detail);
    let mut used = Vec::new();
    let args: Vec<String> = params.iter().map(|p| format!("${{{}}}", unique(&mut used, p))).collect();
    let (apply, snippet) = if args.is_empty() {
        (Some(format!("{}{}()", prefix, member.name)), None)
    } else {
        (None, Some(format!("{}{}({})", prefix, member.name, args.join(", "))))
    };
    let alters = member.intro.map_or(String::new(), |word| format!("{} ", word));
    CompletionItem { label: member.name.clone(), kind: CompletionKind::Method, detail: format!("{}{}", alters, member.detail), apply, snippet, boost: LOCAL }
}

/// Whether position `q` is on the header line of a method of this file.
fn method_header(k: &Knowledge, q: usize) -> bool {
    (k.line_start(q)..=q).any(|p| k.declares_method(p))
}

/// The blocks open at `byte`, outermost first.
fn open_blocks(k: &Knowledge, byte: usize) -> Vec<Block> {
    let mut blocks = Vec::new();
    // Braces opened and not yet closed on the current line.
    let mut inline_braces = 0usize;
    let end = k.last_before(byte).map_or(0, |q| q + 1);
    for q in 0..end {
        match k.token(q) {
            Some(Token::Newline) => inline_braces = 0,
            Some(Token::LBrace) if opens_block_of_lines(k, q) => blocks.push(Block::PlanB),
            Some(Token::LBrace) => inline_braces += 1,
            Some(Token::RBrace) if inline_braces > 0 => inline_braces -= 1,
            // Closing a plan B also closes any block left open inside it.
            Some(Token::RBrace) => {
                if let Some(at) = blocks.iter().rposition(|b| *b == Block::PlanB) {
                    blocks.truncate(at);
                }
            }
            Some(Token::Fi) if blocks.last() == Some(&Block::PlanB) => {}
            Some(Token::Task | Token::Shape | Token::Kind) => {
                blocks.clear();
                blocks.push(match k.token(q) {
                    Some(Token::Task) => Block::Task,
                    Some(Token::Shape) => Block::Shape,
                    _ => Block::Kind,
                });
            }
            // A method has no word of its own: it opens where the scan
            // declares its name.
            Some(Token::Ident(_)) if k.declares_method(q) => blocks.push(Block::Method),
            Some(Token::When) => blocks.push(Block::When),
            Some(Token::Match) => blocks.push(Block::Match),
            Some(Token::Loop | Token::Each | Token::Block) => blocks.push(Block::Other),
            Some(Token::Fi) => {
                blocks.pop();
            }
            _ => {}
        }
    }
    blocks
}

/// Whether the `{` at position `q` opens a plan B whose block continues on the
/// next lines: it ends its line, or only `failure e` follows it there.
pub(crate) fn opens_block_of_lines(k: &Knowledge, q: usize) -> bool {
    let names_error = k.token(q + 1) == Some(&Token::Failure) && matches!(k.token(q + 2), Some(Token::Ident(_) | Token::Underscore));
    let end = if names_error { q + 3 } else { q + 1 };
    matches!(k.token(end), Some(Token::Newline) | None)
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
/// inside one and not inside parentheses or brackets within it.
fn unclosed_brace(k: &Knowledge, q: usize) -> Option<usize> {
    let mut depth = 0usize;
    for p in (k.line_start(q)..=q).rev() {
        match k.token(p) {
            Some(Token::RParen | Token::RBrace | Token::RBracket) => depth += 1,
            Some(Token::LParen | Token::LBracket) if depth == 0 => return None,
            Some(Token::LBrace) if depth == 0 => return Some(p),
            Some(Token::LParen | Token::LBrace | Token::LBracket) => depth -= 1,
            _ => {}
        }
    }
    None
}

fn literal_field_items(k: &Knowledge, brace: usize, word_start: usize) -> Vec<CompletionItem> {
    let Some(shape) = brace.checked_sub(1).and_then(|q| k.qualified_at(q)).and_then(|(name, _)| k.global(&name, SymbolKind::Shape)) else {
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
    k.members(shape, SymbolKind::Field)
        // Another module's private fields can't be given here, and a `read`
        // field that falls back on a value is given by no literal, and nor is
        // a shared field, which is no part of a value.
        .filter(|(_, field)| !given.contains(&field.name) && (field.module.is_none() || !field.private) && !(field.read && field.default) && !field.shared)
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

fn case_items(source: &str, scan: &Scan, context: Context, k: &Knowledge, case: usize) -> Vec<CompletionItem> {
    let Some(match_q) = k.enclosing_match(case) else { return Vec::new() };

    // The subject's kind, checked without the half-typed `case` line.
    let case_start = k.range(case).start;
    let line_end = k.range(k.line_end(case)).start;
    let probe = probe(source, context, case_start, line_end.max(case_start));
    let exact = Knowledge::new(source, scan, &probe);
    let subject_end = k.line_end(match_q).checked_sub(1).filter(|&q| q > match_q);
    let subject = subject_end.and_then(|q| exact.type_ending_at(q));
    let kind = match &subject {
        Some(Type::Named(name, _)) => k.global(name, SymbolKind::Kind),
        _ => None,
    };

    let covered = covered_cases(k, match_q, case);

    // A maybe or `T ~ E`: a value, and none or an error.
    if let Some(outcomes) = subject.as_ref().and_then(outcome_patterns) {
        let uncovered: Vec<&(String, String, String)> = outcomes.iter().filter(|(label, _, _)| !covered.contains(label)).collect();
        let mut items: Vec<CompletionItem> = uncovered
            .iter()
            .map(|(label, detail, template)| CompletionItem {
                label: label.clone(),
                kind: CompletionKind::Snippet,
                detail: detail.clone(),
                apply: None,
                snippet: (template != label).then(|| template.clone()),
                boost: LOCAL,
            })
            .collect();
        if !uncovered.is_empty() && !covered.iter().any(|label| label == "_") {
            items.push(CompletionItem {
                label: "_".to_string(),
                kind: CompletionKind::Keyword,
                detail: "every other case".to_string(),
                apply: None,
                snippet: None,
                boost: KEYWORD,
            });
        }
        if uncovered.len() > 1 {
            let cases: Vec<&str> = uncovered.iter().map(|(_, _, template)| template.as_str()).collect();
            items.push(snippet("all remaining cases", "a value, and the other case", &format!("{}\n\t${{}}", cases.join("\n\t\ncase "))));
        }
        return items;
    }

    // A kind no file declares has no symbol, so its cases come from the language.
    if kind.is_none()
        && let Some(Type::Named(name, _)) = &subject
        && let items @ [_, ..] = builtin_case_items(name, &covered, true).as_slice()
    {
        return items.to_vec();
    }

    let variants: Vec<usize> = match kind {
        Some(kind) => k.members(kind, SymbolKind::Variant).map(|(id, _)| id).collect(),
        None => k.symbols().iter().enumerate().filter(|(_, s)| s.kind == SymbolKind::Variant && s.module.is_none()).map(|(id, _)| id).collect(),
    };
    // A case names a variant alone, even another module's.
    let uncovered: Vec<usize> = variants
        .into_iter()
        .filter(|&id| k.symbol(id).is_some_and(|v| !covered.iter().any(|c| c == unqualified(&v.name))))
        .collect();

    let mut items: Vec<CompletionItem> = uncovered
        .iter()
        .filter_map(|&id| {
            let variant = k.symbol(id)?;
            let template = case_template(k, id, &mut Vec::new());
            let label = unqualified(&variant.name).to_string();
            Some(CompletionItem {
                snippet: (template != label).then_some(template),
                label,
                kind: CompletionKind::Variant,
                detail: variant.detail.clone(),
                apply: None,
                boost: LOCAL,
            })
        })
        .collect();
    if kind.is_some() && !uncovered.is_empty() {
        items.push(CompletionItem {
            label: "_".to_string(),
            kind: CompletionKind::Keyword,
            detail: "every other case".to_string(),
            apply: None,
            snippet: None,
            boost: KEYWORD,
        });
    }
    if kind.is_some() && uncovered.len() > 1 {
        let names: Vec<&str> = uncovered.iter().filter_map(|&id| k.symbol(id).map(|v| unqualified(&v.name))).collect();
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

/// The cases already written in the `match` at position `match_q`, other than
/// the one at position `case`: each variant by its name alone (also when written
/// `Kind.Variant`), `{value}`, `none`, `fail e`, or `_`.
fn covered_cases(k: &Knowledge, match_q: usize, case: usize) -> Vec<String> {
    let mut covered = Vec::new();
    let mut depth = 0usize;
    for q in match_q + 1..k.len() {
        match k.token(q) {
            Some(Token::When | Token::Loop | Token::Each | Token::Block | Token::Match) => depth += 1,
            Some(Token::Fi) if depth == 0 => break,
            Some(Token::Fi) => depth -= 1,
            Some(Token::Case) if depth == 0 && q != case => {
                let label = match k.token(q + 1) {
                    Some(Token::Ident(_)) => match k.ident(k.pattern_variant_at(q)) {
                        Some(tag) => tag.to_string(),
                        None => continue,
                    },
                    Some(Token::LBrace) => "{value}".to_string(),
                    Some(Token::None) => "none".to_string(),
                    Some(Token::Failure) => "failure e".to_string(),
                    Some(Token::Underscore) => "_".to_string(),
                    _ => continue,
                };
                covered.push(label);
            }
            Some(Token::Task | Token::Shape | Token::Kind) => break,
            _ => {}
        }
    }
    covered
}

/// The patterns of a `maybe` or `T ~ E` as `(label, detail, snippet)`: its
/// value, and `none` or its error.
fn outcome_patterns(ty: &Type) -> Option<Vec<(String, String, String)>> {
    // A tuple's items are named in parentheses, `{(a, b)}` and `failure (a, b)`.
    let items = |extracted: &Type| match extracted {
        Type::Tuple(items) => Some(format!("({})", (0..items.len()).map(|i| format!("${{item{}}}", i)).collect::<Vec<_>>().join(", "))),
        _ => None,
    };
    let value = |extracted: &Type| {
        let name = match (extracted, items(extracted)) {
            (Type::Nothing, _) => "{_}".to_string(),
            (_, Some(parts)) => format!("{{{}}}", parts),
            _ => "{${value}}".to_string(),
        };
        ("{value}".to_string(), format!("the {}", extracted), name)
    };
    let failure = |error: &Type| {
        let template = items(error).map_or_else(|| "failure ${e}".to_string(), |parts| format!("failure {}", parts));
        ("failure e".to_string(), format!("the {} error", error), template)
    };
    match ty {
        Type::Maybe(inner) => Some(vec![value(inner), ("none".to_string(), "no value".to_string(), "none".to_string())]),
        Type::Or(ok, error) => Some(vec![value(ok), failure(error)]),
        // A tuple's items always match, so naming them is the only pattern.
        Type::Tuple(items) => Some(vec![(
            "(items)".to_string(),
            format!("the {} items", items.len()),
            format!("({})", (0..items.len()).map(|i| format!("${{item{}}}", i)).collect::<Vec<_>>().join(", ")),
        )]),
        _ => None,
    }
}

/// The patterns after `is`, for the value before it.
fn is_pattern_items(source: &str, scan: &Scan, context: Context, k: &Knowledge, is: usize) -> Vec<CompletionItem> {
    let line_end = k.range(k.line_end(is)).start;
    let probe = probe(source, context, k.range(is).start, line_end.max(k.range(is).start));
    let exact = Knowledge::new(source, scan, &probe);
    let subject = is.checked_sub(1).and_then(|q| exact.type_ending_at(q));
    if let Some(outcomes) = subject.as_ref().and_then(outcome_patterns) {
        return outcomes
            .into_iter()
            .map(|(label, detail, template)| CompletionItem {
                snippet: (template != label).then_some(template),
                label,
                kind: CompletionKind::Snippet,
                detail,
                apply: None,
                boost: LOCAL,
            })
            .collect();
    }
    let kind = match &subject {
        Some(Type::Named(name, _)) => k.global(name, SymbolKind::Kind),
        _ => None,
    };
    if kind.is_none()
        && let Some(Type::Named(name, _)) = &subject
        && let items @ [_, ..] = builtin_case_items(name, &[], false).as_slice()
    {
        return items.to_vec();
    }
    let variants: Vec<usize> = match kind {
        Some(kind) => k.members(kind, SymbolKind::Variant).map(|(id, _)| id).collect(),
        None => k.symbols().iter().enumerate().filter(|(_, s)| s.kind == SymbolKind::Variant && s.module.is_none()).map(|(id, _)| id).collect(),
    };
    variants
        .into_iter()
        .filter_map(|id| {
            let variant = k.symbol(id)?;
            let template = case_template(k, id, &mut Vec::new());
            let label = unqualified(&variant.name).to_string();
            Some(CompletionItem {
                snippet: (template != label).then_some(template),
                label,
                kind: CompletionKind::Variant,
                detail: variant.detail.clone(),
                apply: None,
                boost: LOCAL,
            })
        })
        .collect()
}

/// The variants of a kind no file declares, each as its name, the pattern that
/// takes it apart, and what it holds: the file's symbols hold nothing about them,
/// so they come from the language itself (see `ast::builtin_kinds`). None when
/// `name` is no such kind.
fn builtin_variants(name: &str) -> Option<Vec<(String, String, String)>> {
    let kind = vela::ast::builtin_kinds().into_iter().find(|kind| kind.name == name)?;
    Some(
        kind.variants
            .iter()
            .map(|variant| {
                let mut used = Vec::new();
                let mut pattern = variant.tag.clone();
                for field in &variant.payload {
                    pattern.push_str(&format!(" ${{{}}}", unique(&mut used, &field.name)));
                }
                let holds: Vec<String> = variant.payload.iter().map(|field| format!("{}: {}", field.name, field.ty)).collect();
                let detail = if holds.is_empty() { kind.name.clone() } else { format!("{} ({})", kind.name, holds.join(", ")) };
                (variant.tag.clone(), pattern, detail)
            })
            .collect(),
    )
}

/// Those of them a `match` or an `is` has not covered yet, as cases.
fn builtin_case_items(name: &str, covered: &[String], every: bool) -> Vec<CompletionItem> {
    let Some(variants) = builtin_variants(name) else { return Vec::new() };
    let uncovered: Vec<(String, String, String)> = variants.into_iter().filter(|(label, _, _)| !covered.iter().any(|c| c == label)).collect();
    let mut items: Vec<CompletionItem> = uncovered
        .iter()
        .map(|(label, pattern, detail)| CompletionItem {
            snippet: (pattern != label).then(|| pattern.clone()),
            label: label.clone(),
            kind: CompletionKind::Variant,
            detail: detail.clone(),
            apply: None,
            boost: LOCAL,
        })
        .collect();
    // An `is` runs on one case, so it offers no `_` and no set of the rest.
    if every && !uncovered.is_empty() {
        items.push(CompletionItem {
            label: "_".to_string(),
            kind: CompletionKind::Keyword,
            detail: "every other case".to_string(),
            apply: None,
            snippet: None,
            boost: KEYWORD,
        });
    }
    if every && uncovered.len() > 1 {
        let names: Vec<&str> = uncovered.iter().map(|(label, _, _)| label.as_str()).collect();
        let cases: Vec<&str> = uncovered.iter().map(|(_, pattern, _)| pattern.as_str()).collect();
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
    let mut text = k.symbol(variant).map(|v| unqualified(&v.name).to_string()).unwrap_or_default();
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

fn type_items(k: &Knowledge, at: usize, after_arrow: bool, offer_maybe: bool, after_or: bool) -> Vec<CompletionItem> {
    let taken = type_params_here(k, at);
    let descriptions = [
        ("int", "64-bit signed integer"),
        ("float", "64-bit floating point"),
        ("bool", "true or false"),
        ("unit", "whole number from 0 to 255, one byte"),
        ("char", "one byte, a character, as 'a'"),
        ("text", "text"),
        ("nothing", "no value"),
    ];
    let builtin = BUILTIN_TYPES
        .iter()
        .filter(|(_, ty)| after_arrow || *ty != Type::Nothing)
        .map(|(name, _)| CompletionItem {
            label: name.to_string(),
            kind: CompletionKind::Type,
            detail: descriptions.iter().find(|(n, _)| n == name).map_or("", |(_, d)| d).to_string(),
            apply: None,
            snippet: None,
            boost: SNIPPET,
        });
    // The builtin kinds reading a number sends, which no file declares. Both are
    // error types, so they are offered after `or`, where one goes.
    let builtin_kinds = [
        (vela::ast::PARSE_ERROR, "what reading a number from text sends"),
        (vela::ast::INPUT_ERROR, "what reading a number from input sends"),
    ];
    let builtin_kind = builtin_kinds
        .into_iter()
        .map(|(name, detail)| CompletionItem {
            label: name.to_string(),
            kind: CompletionKind::Type,
            detail: detail.to_string(),
            apply: None,
            snippet: None,
            boost: SNIPPET,
        })
        .filter(move |_| after_or);
    let declared = visible_globals(k, &[SymbolKind::Shape, SymbolKind::Kind]).into_iter().map(|(label, s)| CompletionItem {
        label,
        kind: CompletionKind::Type,
        detail: if s.kind == SymbolKind::Shape { "shape" } else { "kind" }.to_string(),
        apply: None,
        snippet: None,
        boost: GLOBAL,
    });
    // A type of another module is written after its name: `geometry.Point`.
    let modules = module_items(k);
    let maybe = offer_maybe.then(|| CompletionItem {
        label: "maybe".to_string(),
        kind: CompletionKind::Keyword,
        detail: "a value, or none".to_string(),
        apply: None,
        snippet: None,
        boost: SNIPPET,
    });
    builtin.chain(builtin_kind).chain(taken).chain(declared).chain(modules).chain(maybe).collect()
}

/// The types the declaration around the cursor takes (`<T: ordered>`), which
/// are types inside it.
fn type_params_here(k: &Knowledge, at: usize) -> Vec<CompletionItem> {
    let declares = |kind: SymbolKind| matches!(kind, SymbolKind::Task | SymbolKind::Shape);
    let Some((item, _)) = k.symbols().iter().enumerate().find(|(_, s)| declares(s.kind) && s.range.contains(&at)) else {
        return Vec::new();
    };
    k.symbols()
        .iter()
        .filter(|s| s.kind == SymbolKind::TypeParam && s.parent == Some(item))
        .map(|s| CompletionItem {
            label: s.name.clone(),
            kind: CompletionKind::Type,
            detail: if s.detail.is_empty() { "a type this declaration takes".to_string() } else { format!("a type this declaration takes: {}", s.detail) },
            apply: None,
            snippet: None,
            boost: LOCAL,
        })
        .collect()
}

/// The words a type a declaration takes can be asked to allow.
fn need_items() -> Vec<CompletionItem> {
    let words = [
        (Need::Fixed, "never changes, so it can be a tuple's item"),
        (Need::Same, "can be compared with == and !="),
        (Need::Similar, "can be compared item by item with ~= and <>"),
        (Need::Shown, "can be shown"),
        (Need::Ordered, "has an order, so < and > compare it"),
    ];
    words
        .into_iter()
        .map(|(need, detail)| CompletionItem {
            label: need.word().to_string(),
            kind: CompletionKind::Keyword,
            detail: detail.to_string(),
            apply: None,
            snippet: None,
            boost: SNIPPET,
        })
        .collect()
}

/// The names of the tasks, shapes, kinds, or variants (as `kinds` says) usable
/// without a module's name: the file's own, and those `include` brings in, with
/// the name to write for each.
fn visible_globals<'k>(k: &'k Knowledge, kinds: &[SymbolKind]) -> Vec<(String, &'k crate::scan::Symbol)> {
    let own = k.symbols().iter().filter(|s| s.module.is_none() && kinds.contains(&s.kind)).map(|s| (s.name.clone(), s));
    let mut imported: Vec<(String, &crate::scan::Symbol)> = k
        .scan
        .imports
        .iter()
        .filter_map(|(name, &id)| k.symbol(id).filter(|s| kinds.contains(&s.kind)).map(|s| (name.clone(), s)))
        .collect();
    imported.sort_by(|a, b| a.0.cmp(&b.0));
    own.chain(imported).collect()
}

/// The variants of `kind`, after `Kind.`: with their values' fields as a
/// value, or with names for them in a pattern.
fn kind_variant_items(k: &Knowledge, kind: usize, in_pattern: bool) -> Vec<CompletionItem> {
    let kind_name = k.symbol(kind).map(|s| s.name.clone()).unwrap_or_default();
    k.members(kind, SymbolKind::Variant)
        .map(|(id, variant)| {
            let label = unqualified(&variant.name).to_string();
            let snippet = if in_pattern {
                Some(case_template(k, id, &mut Vec::new())).filter(|template| *template != label)
            } else {
                let fields = parameter_names(&variant.detail);
                let mut used = Vec::new();
                (!fields.is_empty()).then(|| format!("{}({})", label, fields.iter().map(|f| format!("${{{}}}", unique(&mut used, f))).collect::<Vec<_>>().join(", ")))
            };
            CompletionItem {
                label,
                kind: CompletionKind::Variant,
                detail: format!("{} {}", kind_name, variant.detail).trim().to_string(),
                apply: None,
                snippet,
                boost: GLOBAL,
            }
        })
        .collect()
}

/// The modules the file names, as names to complete.
fn module_items(k: &Knowledge) -> Vec<CompletionItem> {
    k.scan
        .modules
        .iter()
        .filter(|module| module.named)
        .filter_map(|module| {
            let symbol = k.symbol(module.symbol)?;
            Some(CompletionItem {
                label: symbol.name.clone(),
                kind: CompletionKind::Module,
                detail: if module.library { "Vela's library".to_string() } else { module.path.clone() },
                apply: None,
                snippet: None,
                boost: GLOBAL,
            })
        })
        .collect()
}

/// Which of a module's declarations `member_items` offers.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Members {
    /// What `m.` reaches: its tasks, shapes, kinds, and variants, except those
    /// the file brings in under another name.
    All,
    /// What `m.` reaches where a type is written: its shapes and kinds.
    Types,
    /// What an `include` line can list: all of them, by their own names.
    Listed,
}

/// What module `module` (its `include` line's symbol) declares that another file
/// can use, named without the module (see `Members`).
fn member_items(k: &Knowledge, module: usize, members: Members) -> Vec<CompletionItem> {
    let Some(index) = k.scan.modules.iter().position(|m| m.symbol == module) else { return Vec::new() };
    let types_only = members == Members::Types;
    let kinds: &[SymbolKind] =
        if types_only { &[SymbolKind::Shape, SymbolKind::Kind] } else { &[SymbolKind::Task, SymbolKind::Shape, SymbolKind::Kind, SymbolKind::Variant, SymbolKind::Macro] };
    let module_name = k.symbol(module).map_or("", |s| s.name.as_str());
    // A declaration's own name, which the file may write another way.
    let own_name = |s: &crate::scan::Symbol| {
        let source = k.scan.modules.get(index).and_then(|m| m.source.as_deref());
        source.and_then(|source| source.get(s.name_range.clone())).map_or_else(|| unqualified(&s.name).to_string(), str::to_string)
    };
    k.symbols()
        .iter()
        .enumerate()
        .filter(|(_, s)| s.module == Some(index) && !s.private && kinds.contains(&s.kind))
        // A variant is as private as its kind.
        .filter(|(_, s)| s.kind != SymbolKind::Variant || s.parent.and_then(|p| k.symbol(p)).is_none_or(|kind| !kind.private))
        // A name brought in under another name isn't reached through the module.
        .filter(|(_, s)| members == Members::Listed || s.name == format!("{}{}{}", module_name, vela::ast::MODULE_SEPARATOR, own_name(s)))
        .map(|(id, s)| {
            let label = own_name(s);
            let (kind, detail, apply, snippet) = match s.kind {
                SymbolKind::Task => {
                    let params = parameter_names(&s.detail);
                    let mut used = Vec::new();
                    let args: Vec<String> = params.iter().map(|p| format!("${{{}}}", unique(&mut used, p))).collect();
                    let (apply, snippet) = if args.is_empty() { (Some(format!("{}()", label)), None) } else { (None, Some(format!("{}({})", label, args.join(", ")))) };
                    (CompletionKind::Task, s.detail.clone(), apply, snippet)
                }
                SymbolKind::Variant => {
                    let fields = parameter_names(&s.detail);
                    let mut used = Vec::new();
                    let snippet = (!fields.is_empty()).then(|| format!("{}({})", label, fields.iter().map(|f| format!("${{{}}}", unique(&mut used, f))).collect::<Vec<_>>().join(", ")));
                    let kind_name = s.parent.and_then(|p| k.symbol(p)).map(|p| p.name.as_str()).unwrap_or_default();
                    (CompletionKind::Variant, format!("{} {}", kind_name, s.detail).trim().to_string(), None, snippet)
                }
                SymbolKind::Macro => (CompletionKind::Variant, format!("#{} {}", s.name, s.detail), None, None),
                SymbolKind::Shape if !types_only => (CompletionKind::Type, "shape".to_string(), None, literal_snippet(k, id, &label)),
                SymbolKind::Shape => (CompletionKind::Type, "shape".to_string(), None, None),
                _ => (CompletionKind::Type, "kind".to_string(), None, None),
            };
            // An include line lists names alone: no call or literal.
            let (apply, snippet) = if members == Members::Listed { (None, None) } else { (apply, snippet) };
            CompletionItem { label, kind, detail, apply, snippet, boost: GLOBAL }
        })
        .collect()
}

/// A literal of the shape `shape`, written `label`, with every field a literal
/// must give: `Point { x: ${x}, y: ${y} }`. A field the shape gives a value to
/// fall back on is left out, since the literal needn't write it. None for
/// another module's shape with a private field of that kind, which only its own
/// module can build.
fn literal_snippet(k: &Knowledge, shape: usize, label: &str) -> Option<String> {
    let foreign = k.symbol(shape).is_some_and(|s| s.module.is_some());
    if foreign && k.members(shape, SymbolKind::Field).any(|(_, field)| field.private && !field.default) {
        return None;
    }
    let mut used = Vec::new();
    let fields: Vec<String> = k
        .members(shape, SymbolKind::Field)
        .filter(|(_, field)| !field.default)
        .map(|(_, field)| format!("{}: ${{{}}}", field.name, unique(&mut used, &field.name)))
        .collect();
    let body = if fields.is_empty() { "{}".to_string() } else { format!("{{ {} }}", fields.join(", ")) };
    Some(format!("{} {}", label, body))
}

/// `~`, offered after a finished type, which an error type follows:
/// `int ~ ParseError`. A symbol rather than a word, so it says what it is for.
fn error_separator() -> CompletionItem {
    CompletionItem {
        label: "~".to_string(),
        kind: CompletionKind::Keyword,
        detail: "an error type follows".to_string(),
        apply: None,
        snippet: None,
        boost: KEYWORD,
    }
}

/// `default`, which makes a region the one that allocations naming none use.
fn default_region() -> CompletionItem {
    CompletionItem {
        label: vela::ast::DEFAULT.to_string(),
        kind: CompletionKind::Keyword,
        detail: "allocations that name no region go here".to_string(),
        apply: None,
        snippet: None,
        boost: KEYWORD,
    }
}

/// The modules of Vela's library, each written whole (`std/files`) or, after
/// `std/`, by its own name.
fn library_items(whole: bool) -> Vec<CompletionItem> {
    vela::library::MODULES
        .iter()
        .map(|(name, _)| CompletionItem {
            label: if whole { format!("{}/{}", vela::library::ROOT, name) } else { name.to_string() },
            kind: CompletionKind::Module,
            detail: "Vela's library".to_string(),
            apply: None,
            snippet: None,
            boost: 0,
        })
        .collect()
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
        keyword("priv"),
        keyword("include"),
        snippet("include", "include another file of the program", "include ${module}"),
        snippet("task", "a task", "task ${name}(${})\n\t${}\nfi"),
        snippet("task -> type", "a task that sends a value", "task ${name}(${}) -> ${type}\n\t<- ${value}\nfi"),
        snippet("shape", "a shape with a field", "shape ${Name}\n\t${field}: ${type}\nfi"),
        snippet("kind", "a kind with a variant", "kind ${Name}\n\t${Variant}\nfi"),
        snippet("macro", "a value the compiler computes", "#${NAME} = ${value}"),
    ]
}

/// The word that last opened or divided the innermost block before `byte`:
/// `when`, `else`, `loop`, `each`, `block`, `match`, `case`, or a declaration word.
fn innermost_block_word<'k>(k: &'k Knowledge, byte: usize) -> Option<&'k Token> {
    let mut depth = 0usize;
    let mut q = k.last_before(byte)?;
    loop {
        match k.token(q) {
            Some(Token::Fi) => depth += 1,
            Some(token @ (Token::When | Token::Loop | Token::Each | Token::Block | Token::Match)) => {
                if depth == 0 {
                    return Some(token);
                }
                depth -= 1;
            }
            Some(token @ (Token::Else | Token::Case)) if depth == 0 => return Some(token),
            Some(token @ (Token::Task | Token::Shape | Token::Kind)) => return Some(token),
            _ => {}
        }
        q = q.checked_sub(1)?;
    }
}

fn statement_items(k: &Knowledge, blocks: &[Block], byte: usize, names_error: bool) -> Vec<CompletionItem> {
    let innermost = innermost_block_word(k, byte);
    // Right after `match`, only a `case` can come.
    if blocks.last() == Some(&Block::Match) && innermost == Some(&Token::Match) {
        return vec![keyword("case")];
    }
    let mut items: Vec<CompletionItem> = STATEMENT_KEYWORDS.iter().map(|word| keyword(word)).collect();
    // Inside a plan B, `}` closes the block, not `fi`.
    match blocks.last() {
        Some(Block::PlanB) => {}
        // A `when` has one `else`.
        Some(Block::When) if innermost == Some(&Token::Else) => items.push(keyword("fi")),
        Some(Block::When) => items.extend([keyword("fi"), keyword("else")]),
        Some(Block::Match) => items.extend([keyword("fi"), keyword("case")]),
        _ => items.push(keyword("fi")),
    }
    if names_error {
        items.push(snippet("failure", "name the failure this plan B handles", "failure ${e}"));
    }
    items.extend([
        snippet("when", "a condition", "when ${condition}\n\t${}\nfi"),
        snippet("when … else", "a condition with an alternative", "when ${condition}\n\t${}\nelse\n\t\nfi"),
        snippet("loop", "repeat while a condition holds", "loop ${condition}\n\t${}\nfi"),
        snippet("when … is", "run a block when a pattern matches", "when ${value} is {${name}}\n\t${}\nfi"),
        snippet("loop … is", "repeat while a pattern matches", "loop ${value} is {${name}}\n\t${}\nfi"),
        snippet("each", "count through a range", "each ${i} in ${start}..${end}\n\t${}\nfi"),
        snippet("block", "a block of its own, whose names and regions end at its fi", "block\n\t${}\nfi"),
        snippet("match", "a match on a kind", "match ${value}\ncase ${Variant}\n\t${}\nfi"),
    ]);
    // A `match` with every case, for each visible value of a kind, a maybe, or
    // a `T ~ E`.
    for (id, symbol) in visible_locals(k, byte) {
        let ty = k.type_of_symbol(id);
        if let Some(outcomes) = ty.as_ref().and_then(outcome_patterns) {
            let cases: Vec<&str> = outcomes.iter().map(|(_, _, template)| template.as_str()).collect();
            let mut item = snippet(
                &format!("match {}", symbol.name),
                &format!("both cases of {}", ty.as_ref().map(ToString::to_string).unwrap_or_default()),
                &format!("match {}\ncase {}\n\t${{}}\nfi", symbol.name, cases.join("\n\t\ncase ")),
            );
            item.boost = GLOBAL;
            items.push(item);
            continue;
        }
        let Some(Type::Named(kind_name, _)) = ty else { continue };
        // A kind no file declares has its variants from the language, not from a
        // symbol (see `builtin_variants`).
        let cases: Vec<String> = match k.global(&kind_name, SymbolKind::Kind) {
            Some(kind) => {
                let mut used = Vec::new();
                k.members(kind, SymbolKind::Variant).map(|(variant, _)| case_template(k, variant, &mut used)).collect()
            }
            None => builtin_variants(&kind_name).unwrap_or_default().into_iter().map(|(_, pattern, _)| pattern).collect(),
        };
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
    // Setting a named result is a statement of its own: `total <- value`.
    for (_, symbol) in visible_locals(k, byte).filter(|(_, s)| s.intro == Some("result")) {
        let mut item = snippet(
            &format!("{} <- ", symbol.name),
            &format!("set result {}", symbol.detail),
            &format!("{} <- ${{value}}", symbol.name),
        );
        item.boost = GLOBAL;
        items.push(item);
    }
    // A region is only an argument, never a statement.
    items.extend(local_items(k, byte).into_iter().filter(|item| item.kind != CompletionKind::Region));
    items.extend(task_items(k));
    items
}

fn expression_items(k: &Knowledge, byte: usize) -> Vec<CompletionItem> {
    let mut items = local_items(k, byte);
    items.extend(task_items(k));
    items.extend(module_items(k));
    for (label, variant) in visible_globals(k, &[SymbolKind::Variant]) {
        let fields: Vec<String> = parameter_names(&variant.detail);
        let mut used = Vec::new();
        let snippet = (!fields.is_empty()).then(|| {
            let args: Vec<String> = fields.iter().map(|f| format!("${{{}}}", unique(&mut used, f))).collect();
            format!("{}({})", label, args.join(", "))
        });
        let kind = variant.parent.and_then(|p| k.symbol(p)).map(|s| s.name.as_str()).unwrap_or_default();
        items.push(CompletionItem {
            label,
            kind: CompletionKind::Variant,
            detail: format!("{} {}", kind, variant.detail).trim().to_string(),
            apply: None,
            snippet,
            boost: GLOBAL,
        });
    }
    for (label, value) in visible_globals(k, &[SymbolKind::Macro]) {
        items.push(CompletionItem { label, kind: CompletionKind::Variant, detail: format!("#{} {}", value.name, value.detail), apply: None, snippet: None, boost: GLOBAL });
    }
    // A shape's value is a literal with every field: `Point { x: ${x}, y: ${y} }`.
    for (label, shape) in visible_globals(k, &[SymbolKind::Shape]) {
        let Some(id) = k.symbols().iter().position(|s| std::ptr::eq(s, shape)) else { continue };
        items.push(CompletionItem {
            snippet: literal_snippet(k, id, &label),
            label,
            kind: CompletionKind::Type,
            detail: "shape".to_string(),
            apply: None,
            boost: GLOBAL,
        });
    }
    // `new` needs a region to allocate in, and `fail` a `T ~ E` to hold its error.
    let can_allocate = !region_items(k, byte, true).is_empty();
    let has_errors = k.symbols().iter().any(|symbol| symbol.module.is_none() && symbol.detail.contains('~'));
    items.extend(
        VALUE_KEYWORDS
            .iter()
            .filter(|word| (**word != "new" || can_allocate) && (**word != "fail" || has_errors))
            .map(|word| keyword(word)),
    );
    if can_allocate {
        items.push(snippet("new … at", "allocate a value in a region", "new ${T}(${value}) at ${r}"));
    }
    // A `match` or a `when` used as a value: every branch gives it.
    items.push(snippet("match … = value", "a value from each case", "match ${value}\ncase ${Variant} = ${}\nfi"));
    items.push(snippet("when … else", "a value from either branch", "when ${condition}\n\t${}\nelse\n\t${}\nfi"));
    items
}

/// The regions that can be named at `byte`. `allocating` leaves out regions a
/// task only names in its types, which `new` can't allocate in. In a task's
/// header, the regions its parameters already name on that line.
fn region_items(k: &Knowledge, byte: usize, allocating: bool) -> Vec<CompletionItem> {
    let same_line = |from: usize| k.source.get(from..byte).is_some_and(|text| !text.contains('\n'));
    let mut seen: Vec<&str> = Vec::new();
    let mut items = Vec::new();
    for symbol in k.symbols().iter().rev().filter(|s| s.kind == SymbolKind::Region) {
        let visible = symbol.visible.start <= byte && byte <= symbol.visible.end;
        let in_header = symbol.name_range.end <= byte && same_line(symbol.name_range.start);
        if !(visible || in_header) || seen.contains(&symbol.name.as_str()) || (allocating && symbol.intro == Some("@")) {
            continue;
        }
        seen.push(&symbol.name);
        let detail = if symbol.intro == Some("@") { "region, named in the task's types" } else { "region" };
        items.push(CompletionItem {
            label: symbol.name.clone(),
            kind: CompletionKind::Region,
            detail: detail.to_string(),
            apply: None,
            snippet: None,
            boost: LOCAL,
        });
    }
    items
}

/// The variables, parameters, and regions visible at `byte`, innermost last.
fn visible_locals<'k>(k: &'k Knowledge, byte: usize) -> impl Iterator<Item = (usize, &'k crate::scan::Symbol)> {
    k.symbols().iter().enumerate().filter(move |(_, s)| {
        matches!(s.kind, SymbolKind::Variable | SymbolKind::Parameter | SymbolKind::Region) && s.visible.start <= byte && byte <= s.visible.end
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
        let (kind, fallback) = match symbol.kind {
            SymbolKind::Parameter => (CompletionKind::Parameter, "parameter"),
            SymbolKind::Region => (CompletionKind::Region, "region"),
            _ => (CompletionKind::Variable, symbol.intro.unwrap_or("variable")),
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
    let mut items: Vec<CompletionItem> = visible_globals(k, &[SymbolKind::Task])
        .into_iter()
        .map(|(label, task)| {
            let params = parameter_names(&task.detail);
            let mut used = Vec::new();
            let args: Vec<String> = params.iter().map(|p| format!("${{{}}}", unique(&mut used, p))).collect();
            let (apply, snippet) = if args.is_empty() {
                (Some(format!("{}()", label)), None)
            } else {
                (None, Some(format!("{}({})", label, args.join(", "))))
            };
            CompletionItem { label, kind: CompletionKind::Task, detail: task.detail.clone(), apply, snippet, boost: GLOBAL }
        })
        .collect();
    items.extend(BUILTIN_TASKS.iter().map(|name| {
        // `scan` names the region it reads a line into after its call; the form
        // that reads a number names no region (see `hover`).
        let (detail, snippet) = if *name == vela::ast::SCAN {
            ("(prompt: text) at r -> maybe text@r, or (prompt: text, int | float) -> int | float ~ InputError", format!("{}(${{prompt}}) at ${{region}}", name))
        } else {
            ("(value: int | float | bool | text | maybe | anything with str, …)", format!("{}(${{value}})", name))
        };
        CompletionItem { label: name.to_string(), kind: CompletionKind::BuiltinFn, detail: detail.to_string(), apply: None, snippet: Some(snippet), boost: GLOBAL }
    }));
    items
}
