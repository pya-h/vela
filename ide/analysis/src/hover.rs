use serde::Serialize;

use crate::TokenClass;
use crate::infer::Knowledge;
use crate::scan::{LOOP_NAME, Symbol, SymbolKind};
use vela::ast::Mode;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Hover {
    pub from: usize,
    pub to: usize,
    pub code: String,
    pub doc: String,
}

const BUILTIN_TYPE_DOCS: &[(&str, &str)] = &[
    (
        "int",
        "A 64-bit signed integer. int(x) turns a value into one: a float becomes its whole part, toward zero (a float with none to give, such as nan, stops the program), and a bool becomes 1 or 0.",
    ),
    (
        "float",
        "A 64-bit floating-point number. float(n) turns an int into one, which always works.",
    ),
    ("bool", "true or false."),
    (
        "char",
        "One byte, as C's char: the code of a character, 0 to 255, written 'a' (with the escapes text has, and '\\''). c + 1 and c - 1 move it and c - 'a' is how far apart two are, an int; leaving 0 to 255 stops the program. t[i] reads a byte of text as a char and t.at(i) the same as a maybe char, none out of range. char(n) turns a code into one, int(c) the other way.",
    ),
    (
        "unit",
        "A whole number from 0 to 255 in one byte. A number written out becomes one where a unit is expected, and beside one (b + 1); arithmetic that leaves 0 to 255 stops the program. unit(x) turns an int, a float, or a bool into one, and stops the program when it doesn't fit; int(b) and float(b) always work.",
    ),
    (
        "text",
        "UTF-8 text. t.len is how many bytes it has and t[a..b] a part of it, the same bytes; == and < compare contents, a + b at r joins text into region r, and \"n = {n}\" at r is text holding values, each turned into text there (\\{ and \\} write braces). text(n) at r turns a value into text, as n.str() at r does. text lasts as long as the program, as a literal does; text@r is in region r, as the text of a value in r is.",
    ),
    ("nothing", "The return type of a task that sends no value."),
    ("int64", "Another name for int, a 64-bit signed integer."),
    (
        "int32",
        "A 32-bit signed integer, -2147483648 to 2147483647. A number written out becomes one where an int32 is expected, and beside one (n + 1); arithmetic leaving its range stops the program. int32(x) turns a number into one, and stops the program when it doesn't fit; int(n) always works.",
    ),
    (
        "int16",
        "A 16-bit signed integer, -32768 to 32767. A number written out becomes one where an int16 is expected, and beside one (n + 1); arithmetic leaving its range stops the program. int16(x) turns a number into one, and stops the program when it doesn't fit; int(n) always works.",
    ),
    (
        "int8",
        "An 8-bit signed integer, -128 to 127. A number written out becomes one where an int8 is expected, and beside one (n + 1); arithmetic leaving its range stops the program. int8(x) turns a number into one, and stops the program when it doesn't fit; int(n) always works.",
    ),
    (
        "uint",
        "Another name for uint64, a 64-bit unsigned integer.",
    ),
    (
        "uint64",
        "A 64-bit unsigned integer, 0 to 18446744073709551615. A number written out becomes one where a uint64 is expected, and beside one (n + 1); arithmetic leaving its range stops the program, and so does int(n) of one past the largest int.",
    ),
    (
        "uint32",
        "A 32-bit unsigned integer, 0 to 4294967295. A number written out becomes one where a uint32 is expected, and beside one (n + 1); arithmetic leaving its range stops the program. uint32(x) turns a number into one, and stops the program when it doesn't fit; int(n) always works.",
    ),
    (
        "uint16",
        "A 16-bit unsigned integer, 0 to 65535. A number written out becomes one where a uint16 is expected, and beside one (n + 1); arithmetic leaving its range stops the program. uint16(x) turns a number into one, and stops the program when it doesn't fit; int(n) always works.",
    ),
    (
        "uint8",
        "Another name for unit, a whole number from 0 to 255 in one byte.",
    ),
    (
        "float64",
        "Another name for float, a 64-bit floating-point number.",
    ),
    (
        "float32",
        "A 32-bit floating-point number. A float written out becomes one where a float32 is expected, and beside one (f * 2.5). float32(x) turns a number into one, and float(f) turns one into a float.",
    ),
];

fn builtin_kind_doc(name: &str) -> Option<(&'static str, &'static str)> {
    match name {
        vela::ast::PARSE_ERROR => Some((
            "kind ParseError\n    NotANumber\n    TooLarge\nfi",
            "The error int(t) and float(t) send when the text is no number they can read. A program reads it with match, is, or a plan B, passes on one it was given, and may send one itself (fail ParseError.TooLarge). No file declares it, and no declaration may take its name or its variants.",
        )),
        vela::ast::NOT_A_NUMBER => Some((
            "NotANumber",
            "A variant of ParseError: the text is no number at all, such as \"4x2\", \"\", or \" 4\" (a number in text carries no spaces, no +, no _, and no exponent).",
        )),
        vela::ast::TOO_LARGE => Some((
            "TooLarge",
            "A variant of ParseError: the text is a number, but not one the type holds, such as \"9223372036854775808\" read as an int.",
        )),
        vela::ast::INPUT_ERROR => Some((
            "kind InputError\n    Ended\n    NoNumber(why: ParseError)\nfi",
            "The error scan(prompt, int) and scan(prompt, float) send when there is no number to read: the input ended, or the line was no number, which holds why. A program reads it with match, is, or a plan B, passes on one it was given, and may send one itself (fail Ended). No file declares it, and no declaration may take its name or its variants.",
        )),
        vela::ast::ENDED => Some((
            "Ended",
            "A variant of InputError: the input has ended, so there was no line to read a number from. A loop reading numbers stops on it.",
        )),
        vela::ast::NO_NUMBER => Some((
            "NoNumber(why: ParseError)",
            "A variant of InputError: a line was read, but it was no number. It holds the ParseError reading the line gave, read with a match of its own: case NoNumber why.",
        )),
        _ => None,
    }
}

const KEYWORD_DOCS: &[(&str, &str, &str)] = &[
    (
        "region",
        "region r",
        "Memory new allocates in r lives until the block declaring r closes. As a parameter's type, r: region lets a task allocate in the caller's region.",
    ),
    (
        "new",
        "new T(value) at r",
        "Copies the value into region r and gives a ^T@r. Also written new@r T(value). With a slice type, new [T](items) at r gives a [T]@r of the items: [v#n], an array, or a slice; made of an array or a slice, it is a complete copy, as copy() is, sharing nothing at any depth, from any region. An empty value needs no ( ): new [T] at r and new [K: V] at r are empty, and new Point at r is a Point of the values its fields fall back on.",
    ),
    (
        "at",
        "new T(value) at r",
        "Allocates in region r, the same as new@r T(value) ('@' is 'at'). It also names where new text goes: a + b at r joins text, as (a + b)@r does, \"n = {n}\" at r holds values, and scan(prompt) at r reads a line.",
    ),
    (
        "in",
        "each i in a..b",
        "Counts from a up to, but not including, b. each v in xs goes through the items of an array or slice.",
    ),
    (
        "maybe",
        "maybe T",
        "A T, or none. Read it with match, is, or a plan B: x { fallback }.",
    ),
    (
        "none",
        "none",
        "The empty maybe. A pointer can't be none; use maybe ^T.",
    ),
    (
        "is",
        "value is case form",
        "The condition holds when the case form matches, as {v}, none, or a variant; its names are visible in the block.",
    ),
    (
        "$",
        "$d, $d(values), $d?, or $(w1, w2(values), .m)",
        "A decorator, on a line above a task or a shared method, and itself a task or a shared method ($Registry.enroll). $d is a startup decorator: d(task, values) runs once where the program starts, before main, the nearest first; d takes the task first and sends nothing, or nothing ~ E, which stops the program, unless $d? lets it pass with a notice (--no-notices hides it). $(w1, w2) wraps the task, and $(.m) a method by another method of its type: each call of it runs w1, given the rest of the chain as a task of the task's type, its own values, and the task's values, and what w1 sends is what the call gives. Both kinds of values are worked out where the program starts, so they hold no variable or region; a wrapper's are worked out once, before the startup decorators, and every call reads them.",
    ),
    (
        "...",
        "P..., args: P..., (P...) -> R, f(args...)",
        "After a type a task takes, a group of types: any number, any mix. args: P... takes the values a call gives after the others, (P...) -> R is a task taking them, and f(args...) gives them on, whole. Each use fills the group in with the types of its values, as it fills in a type, so it costs nothing when the program runs. After the values of a slice, as in [1, 2, ...] at r, it makes a slice of them.",
    ),
    (
        "fail",
        "fail error",
        "Makes a failure, for a task whose type has a ~ side: <- fail e sends one. A case form names a failure with 'failure' instead.",
    ),
    (
        "failure",
        "failure e",
        "Names the failure a case form matches: case failure e, when x is failure e, and a plan B's first line. 'fail' makes one.",
    ),
    (
        "_",
        "_",
        "Matches anything left, or drops a value without naming it.",
    ),
    (
        "default",
        "region r default",
        "Makes r the block's default region: every allocation in the block that names no region goes there, as do those in the blocks inside it, until one names its own default. A task takes one the same way, 'r: region default'. A copy is the exception: it goes where the value it becomes goes.",
    ),
    (
        "alters",
        "alters name(…)",
        "Marks a method that changes the value it is called on: it may assign .this and what the value holds. Only a place that can change may call it: a flex variable, a field or item of one, or what a pointer points to, reached from a flex name.",
    ),
    (
        "type",
        "x.type: text",
        "The type of the value as code writes it, without regions: [int#3], [float], ^Point, Box<int>. The compiler knows it, so the value itself is never worked out; in a task that takes types, it is the type each use gives. 'type' is a reserved word, so nothing else takes the name.",
    ),
];

const THIS_REGION_DOC: &str = "The region the value this method is called on lives in. The method may send what the value holds there and take values in it, but can't allocate in it, since nothing gives a method the region.";

pub(crate) fn hover(
    k: &Knowledge,
    byte: usize,
) -> Option<((String, String), std::ops::Range<usize>)> {
    let tokens = &k.scan.tokens;
    // Fall back to a token ending at `byte` so hovering right after a name (e.g. before `)`) finds it.
    let under = tokens
        .iter()
        .find(|t| t.range.start <= byte && byte < t.range.end);
    let ending = tokens.iter().find(|t| t.range.end == byte);
    [under, ending]
        .into_iter()
        .flatten()
        .find_map(|token| hover_token(k, token))
}

fn hover_token(
    k: &Knowledge,
    token: &crate::scan::Classified,
) -> Option<((String, String), std::ops::Range<usize>)> {
    let text = k.source.get(token.range.clone()).unwrap_or_default();
    let info = match (token.class, k.symbol_of(token)) {
        (TokenClass::TypeBuiltin, _) => {
            let (name, doc) = BUILTIN_TYPE_DOCS.iter().find(|(name, _)| *name == text)?;
            (name.to_string(), doc.to_string())
        }
        (TokenClass::BuiltinFn, _) if text == vela::ast::SCAN => (
            "task scan(prompt: text) at r -> maybe text@r\ntask scan(prompt: text, int | float) -> int | float ~ InputError".to_string(),
            "Shows the prompt, then reads one line of input into region r (also written scan(prompt)@r), without its line break; gives none when the input has ended. Naming int or float reads a number from that line instead, which needs no memory and so names no region: it gives that number, or an InputError saying the input ended or the line was no number."
                .to_string(),
        ),
        (TokenClass::BuiltinFn, _) => (
            format!("task {}(value: anything with str, …)", text),
            "Writes each value, with nothing between them, then ends the line: show(\"n = \", n) prints n = 5. An array or slice is written [1, 2], a tuple (1, \"a\"), a maybe none or { 4 }, a pointer as its address (0x7ffd…), and text inside any of them in quotes. A shape is written as a literal of the fields any file reads (Point { x: 1, y: 2 }) and a kind as its variant (Circle(1.5)), unless it has a method str of its own, which writes it in a region show makes for the line. show() writes an empty line, and the values are written left to right.".to_string(),
        ),
        (TokenClass::Keyword, _) if text == vela::ast::THIS_WORD => {
            let owner = owner_around(k, token.range.start);
            let code = owner.map_or_else(|| format!(".{}", text), |owner| format!(".{}: {}", text, owner.name));
            let doc = "The value this method is called on. A kind's method takes it apart with match .this, and a method marked alters may assign it.";
            (code, doc.to_string())
        }
        (TokenClass::Operator, _) if text == "~" => (
            "T ~ E".to_string(),
            "A T when it succeeds, or an error E when it fails. Read it with match, is, or a plan B: x { fallback }. Only a task's own error, never a maybe or the value type again.".to_string(),
        ),
        (TokenClass::Keyword, _) => {
            let (_, code, doc) = KEYWORD_DOCS.iter().find(|(word, _, _)| *word == text)?;
            (code.to_string(), doc.to_string())
        }
        (TokenClass::Type | TokenClass::Variant, None) if builtin_kind_doc(text).is_some() => {
            let (code, doc) = builtin_kind_doc(text)?;
            (code.to_string(), doc.to_string())
        }
        (TokenClass::Region, None) if text == vela::ast::THIS_WORD => (format!("region {}", text), THIS_REGION_DOC.to_string()),
        (TokenClass::Field, None) if text == "len" => {
            ("len: int".to_string(), "The number of items of an array or slice, or the number of bytes of text.".to_string())
        }
        (TokenClass::Field, None) if text.parse::<usize>().is_ok() => {
            let ty = k.at_byte(token.range.start).and_then(|q| k.type_ending_at(q));
            let code = ty.map_or_else(|| format!("item {}", text), |ty| format!("item {}: {}", text, ty));
            (code, "An item of a tuple, by its position from 0. A tuple's items never change.".to_string())
        }
        (TokenClass::Field, None) if text == vela::ast::STR => (
            "str() at r or str(r) -> text@r".to_string(),
            "Turns a value into text in region r, written as show writes it (also str()@r, and text(n) at r): a number, a bool, text, an array or slice ([1, 2]), a tuple ((1, \"a\")), a maybe (none or { 4 }), a pointer (its address, 0x…, which int(…) reads back), a shape (Point { x: 1, y: 2 }), or a kind (Circle(1.5)). A shape or kind replaces its default with a method str(r: region) -> text@r, which is never priv.".to_string(),
        ),
        (TokenClass::Field, None) if text == vela::ast::COPY => (
            "copy() at r or copy()@r".to_string(),
            "Copies a value into a region: its text, its slices' items, and what its pointers point to, all the way down (shared nodes stay shared, and cycles stay cycles). Without a region, the copy goes into the region of the place it is put in.".to_string(),
        ),
        (TokenClass::Field, None) if text == "add" => (
            "add(value) or add(index, value)".to_string(),
            "Adds a value to a slice: last, or before the item at the index (which may be the length). The slice must be in a place that can change, and the task must have its region; the items stay in that region.".to_string(),
        ),
        (TokenClass::Field, None) if text == "added" => (
            "added(value), added(index, value), or added(key, value) at r".to_string(),
            "A new slice with the value put last, or before the item at the index, or a new map with the key set: a complete copy, sharing nothing with the original, which never changes, so it works on fixed, bind and flex names alike. It goes where a copy goes: the region of the place it is put in, or the one after it, at r. Each call copies the whole value, so build a list with add on a flex one instead.".to_string(),
        ),
        (TokenClass::Field, None) if text == "without" => (
            "without(index), without(), or without(key) at r".to_string(),
            "A new slice without the item at the index, or the last, or a new map without the key: a complete copy, sharing nothing with the original, which never changes. A slice's index out of range stops the program, as del's does; a key the map doesn't hold gives an unchanged copy. It goes where a copy goes, as added does.".to_string(),
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

fn pattern_name(k: &Knowledge, at: usize, typed: &str, doc: &str) -> (String, String) {
    match k.pattern_name_mode(at) {
        Mode::Flex => (
            format!("flex {}", typed),
            format!("{} It can be assigned.", doc),
        ),
        Mode::Bind => (
            format!("bind {}", typed),
            format!(
                "{} It is bound to one value; its contents may change, but it is never assigned as a whole.",
                doc
            ),
        ),
        Mode::Fixed => (typed.to_string(), doc.to_string()),
    }
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
                Some(m) if m.library => "A module of Vela's library, which comes with the compiler.".to_string(),
                Some(m) if m.source.is_some() => format!("A module this file includes: the file {}.", m.path),
                Some(m) => format!("A module this file includes: the file {}, which can't be read.", m.path),
                None => "A module this file includes.".to_string(),
            };
            (format!("include {}", symbol.detail), doc)
        }
        SymbolKind::Task => (format!("task {}{}", symbol.name, symbol.detail), from_module(k, symbol)),
        SymbolKind::Alias => {
            let private = if symbol.private { "priv " } else { "" };
            let doc = format!("A type alias: another name for {}, which it is wherever its name is written.", symbol.detail);
            let module = from_module(k, symbol);
            (format!("{}alias {} => {}", private, vela::ast::unqualified(&symbol.name), symbol.detail), if module.is_empty() { doc } else { format!("{} {}", doc, module) })
        }
        SymbolKind::Macro => {
            let private = if symbol.private { "priv " } else { "" };
            let ty = k.type_of_symbol(id).map_or(String::new(), |ty| format!(", of type {}", ty));
            let doc = format!("A macro: a value the compiler computes{}. Each use is that value, which never changes.", ty);
            let module = from_module(k, symbol);
            (format!("{}#{} {}", private, vela::ast::unqualified(&symbol.name), symbol.detail), if module.is_empty() { doc } else { format!("{} {}", doc, module) })
        }
        SymbolKind::Method => {
            let private = if symbol.private { "priv " } else { "" };
            let alters = symbol.intro.map_or(String::new(), |word| format!("{} ", word));
            let (what, owner) = match parent {
                Some(p) if p.kind == SymbolKind::Kind => ("kind", p.name.as_str()),
                Some(p) => ("shape", p.name.as_str()),
                None => ("shape", "?"),
            };
            let changes = if symbol.intro.is_some() {
                " It changes the value it is called on, so only a place that can change may call it."
            } else {
                ""
            };
            let doc = if symbol.shared {
                format!("A shared method of {0} {1}, called on '{1}' itself: {1}.{2}(…). It has no value it is called on, so no '.this'.", what, owner, symbol.name)
            } else {
                format!("A method of {} {}, called on a value of it: value.{}(…).{}", what, owner, symbol.name, changes)
            };
            let module = from_module(k, symbol);
            (format!("{}{}{}{}", private, alters, symbol.name, symbol.detail), if module.is_empty() { doc } else { format!("{} {}", doc, module) })
        }
        SymbolKind::TypeParam => {
            let what = match parent.map(|p| p.kind) {
                Some(SymbolKind::Shape) => "shape",
                _ => "task",
            };
            let task = parent.map(|p| p.name.as_str()).unwrap_or("?");
            let code = if symbol.detail.is_empty() { symbol.name.clone() } else { format!("{}: {}", symbol.name, symbol.detail) };
            let allows = match symbol.detail.as_str() {
                "" => "Its values can be stored, passed, and sent, and nothing else.".to_string(),
                words => words
                    .split('&')
                    .map(|word| match word.trim() {
                        "ordered" => "Every type given for it has an order, so '<' and '>' compare its values, and '==' too.".to_string(),
                        "shown" => "Every type given for it can be shown.".to_string(),
                        "same" => "Every type given for it can be compared with '==', each by its own rule (an array, a shape, or a tuple by where it is).".to_string(),
                        "similar" => "Every type given for it can be compared item by item with '~=' and '<>'.".to_string(),
                        "frozen" => "Every type given for it never changes, so it can be a tuple's item.".to_string(),
                        other => format!("Every type given for it must be '{}'.", other),
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
            };
            let gives = if what == "shape" { "each value of it is given" } else { "each call gives it" };
            (code, format!("A type {} {} takes, standing for the type {}. {}", what, task, gives, allows))
        }
        SymbolKind::Pattern => {
            let private = if symbol.private { "priv " } else { "" };
            let mut code = format!("{}pattern {}\n", private, vela::ast::unqualified(&symbol.name));
            for (_, member) in k.children(id) {
                let line = match member.kind {
                    SymbolKind::Field => format!("{}: {}", member.name, member.detail),
                    _ => {
                        let alters = member.intro.map_or(String::new(), |word| format!("{} ", word));
                        let default = if member.default { " ; default" } else { "" };
                        format!("{}{}{}{}", alters, member.name, member.detail, default)
                    }
                };
                code.push_str(&format!("    {}\n", line));
            }
            code.push_str("fi");
            let doc = "A pattern: what a shape or kind that follows it has, written after its name, as in 'shape Circle::Shaped', and asked of the types a task takes, as in '<T: Shaped>'. A method marked default comes with its body, which a follower has unless it writes its own.";
            let module = from_module(k, symbol);
            (code, if module.is_empty() { doc.to_string() } else { format!("{} {}", doc, module) })
        }
        SymbolKind::Shape | SymbolKind::Kind => {
            let keyword = if symbol.kind == SymbolKind::Shape { "shape" } else { "kind" };
            let private = if symbol.private { "priv " } else { "" };
            let takes = if symbol.detail.starts_with('<') || symbol.detail.starts_with(':') { symbol.detail.as_str() } else { "" };
            let mut code = format!("{}{} {}{}\n", private, keyword, symbol.name, takes);
            for (_, variant) in k.members(id, SymbolKind::Variant).filter(|(_, variant)| variant.parent != Some(id)) {
                let from = variant.parent.and_then(|kind| k.symbol(kind)).map_or("?", |kind| vela::ast::unqualified(&kind.name));
                code.push_str(&format!("    {}{} ; from {}\n", vela::ast::unqualified(&variant.name), variant.detail, from));
            }
            let followed = crate::scan::followed_methods(k.symbols(), id, &|written| k.global(written, SymbolKind::Pattern));
            let mut tail = String::new();
            for method in followed.iter().filter_map(|&method| k.symbol(method)).filter(|method| method.default) {
                let from = method.parent.and_then(|pattern| k.symbol(pattern)).map_or("?", |pattern| vela::ast::unqualified(&pattern.name));
                let alters = method.intro.map_or(String::new(), |word| format!("{} ", word));
                tail.push_str(&format!("    {}{}{} ; from {}\n", alters, method.name, method.detail, from));
            }
            for (_, member) in k.children(id) {
                if member.kind == SymbolKind::TypeParam || (member.private && symbol.module.is_some()) {
                    continue;
                }
                let separator = if member.kind == SymbolKind::Field { ": " } else { "" };
                let (private, alters) = if member.kind == SymbolKind::Method {
                    (if member.private { "priv ".to_string() } else { String::new() }, member.intro.map_or(String::new(), |word| format!("{} ", word)))
                } else {
                    (shared_words(member), String::new())
                };
                code.push_str(&format!("    {}{}{}{}{}\n", private, alters, vela::ast::unqualified(&member.name), separator, member.detail));
            }
            code.push_str(&tail);
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
            if symbol.shared {
                let shape = parent.map(|p| p.name.as_str()).unwrap_or("?");
                let changes = match (symbol.intro, symbol.read) {
                    (Some("flex"), _) => " Any code that sees it may change it.",
                    (Some("bind"), _) => " Any code may change its contents, but not assign it as a whole.",
                    (Some(_), _) => " Any code that sees it may change it.",
                    (None, true) => " Any file reads it; only the methods of its shape change it.",
                    (None, false) => " It never changes.",
                };
                let doc = format!(
                    "A shared field of shape {0}: one value for the whole program, not part of each value, read as {0}.{1}.{2}",
                    vela::ast::unqualified(shape),
                    symbol.name,
                    changes
                );
                return Some((format!("{}{}: {}", shared_words(symbol), symbol.name, symbol.detail), doc));
            }
            let marker = if symbol.read { "read " } else { "" };
            let doc = if symbol.read { format!("{} Any file reads it; only the alters methods of its shape change it.", doc) } else { doc };
            (format!("{}{}: {}", marker, symbol.name, symbol.detail), doc)
        }
        SymbolKind::Parameter => {
            let doc = task_around(k, symbol.name_range.start).map(|task| format!("A parameter of {}.", task)).unwrap_or_default();
            if symbol.intro == Some("flex&") {
                let doc = format!("{} The caller's place, which the call gives as '&a': every change to it is a change there.", doc);
                return Some((format!("flex& {}", typed(&symbol.name)), doc.trim_start().to_string()));
            }
            if symbol.intro == Some("flex") {
                let doc = format!("{} The task's own copy, which it may change; the caller's variable never does, though what a twofold value names, a slice's items, a map, or what a pointer points to, is the caller's.", doc);
                return Some((format!("flex {}", typed(&symbol.name)), doc.trim_start().to_string()));
            }
            if symbol.intro == Some("bind&") {
                let doc = format!("{} The caller's place, which the call gives as '&a': it may change the contents there, but never assigns the place as a whole.", doc);
                return Some((format!("bind& {}", typed(&symbol.name)), doc.trim_start().to_string()));
            }
            if symbol.intro == Some("bind") {
                let doc = format!("{} The task's own copy; it may change the contents, but never assigns it as a whole. The caller's variable never does, though what a twofold value names, a slice's items, a map, or what a pointer points to, is the caller's.", doc);
                return Some((format!("bind {}", typed(&symbol.name)), doc.trim_start().to_string()));
            }
            if symbol.intro == Some("fixed&") {
                let doc = format!("{} The caller's place, which the call gives as '&a', read where it is and never changed.", doc);
                return Some((format!("fixed& {}", typed(&symbol.name)), doc.trim_start().to_string()));
            }
            if symbol.intro == Some("fixed") {
                let doc = format!("{} Always a copy, even of a large value the compiler would otherwise read where the caller keeps it.", doc);
                return Some((format!("fixed {}", typed(&symbol.name)), doc.trim_start().to_string()));
            }
            (typed(&symbol.name), doc)
        }
        SymbolKind::Region if symbol.name == vela::ast::THIS_WORD && symbol.intro == Some("@") && is_in_method(k, symbol.name_range.start) => {
            (format!("region {}", symbol.name), THIS_REGION_DOC.to_string())
        }
        SymbolKind::Region => {
            let task = task_around(k, symbol.name_range.start).unwrap_or_else(|| "this task".to_string());
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
            Some(LOOP_NAME) if symbol.detail == "=" => (
                format!("{} = loop", symbol.name),
                "The name of a loop, given by the variable the loop gives its value, which 'break' in a loop inside it gives to stop it, as in 'break found = v'.".to_string(),
            ),
            Some(LOOP_NAME) => (format!("{} => loop", symbol.name), "The name of a loop, which 'break' and 'continue' in a loop inside it give to stop it or go on with it.".to_string()),
            Some("case") => {
                let name = k.at_byte(symbol.name_range.start)?;
                let case = (k.line_start(name)..name).find(|&q| k.token(q) == Some(&vela::lexer::Token::Case));
                let variant = case.and_then(|q| k.ident(k.pattern_variant_at(q)));
                let doc = variant.map_or_else(|| "Bound by a case.".to_string(), |v| format!("Bound by case {}.", v));
                pattern_name(k, name, &typed(&symbol.name), &doc)
            }
            Some("is") => {
                let name = k.at_byte(symbol.name_range.start)?;
                pattern_name(k, name, &typed(&symbol.name), "Bound by an is condition.")
            }
            Some("failure") => (format!("failure {}", typed(&symbol.name)), "The failure this plan B handles.".to_string()),
            Some("each") => {
                let doc = match k.each_items(id) {
                    None => "The counter of an each loop.",
                    Some(_) => "An item of the array or slice an each loop goes through.",
                };
                (format!("each {}", typed(&symbol.name)), doc.to_string())
            }
            Some("flex") => (format!("flex {}", typed(&symbol.name)), "A mutable variable.".to_string()),
            Some("bind") => (
                format!("bind {}", typed(&symbol.name)),
                "A variable bound to one value; its contents may change, but it is never assigned as a whole.".to_string(),
            ),
            Some("flex&") => (
                format!("flex& {}", typed(&symbol.name)),
                "A reference: another name for the place given after '=', so a change to either is a change to both.".to_string(),
            ),
            Some("bind&") => (
                format!("bind& {}", typed(&symbol.name)),
                "A reference to a place whose contents may change, but which is never assigned as a whole.".to_string(),
            ),
            Some("fixed&") => (
                format!("fixed& {}", typed(&symbol.name)),
                "A reference that reads the place given after '=', as it is when read, and never changes it.".to_string(),
            ),
            Some("result") => {
                let doc = match task_around(k, symbol.name_range.start) {
                    Some(task) => format!("A result of {}, which reaching its end sends.", task),
                    None => "A result of this task, which reaching its end sends.".to_string(),
                };
                (format!("result {}", typed(&symbol.name)), format!("{} Set it with '{} <- …'.", doc, symbol.name))
            }
            _ => (format!("fixed {}", typed(&symbol.name)), "An immutable variable: nothing changes through it, at any depth.".to_string()),
        },
    })
}

fn owner_around<'a>(k: &Knowledge<'a>, byte: usize) -> Option<&'a crate::scan::Symbol> {
    k.symbols().iter().find(|s| {
        s.module.is_none()
            && matches!(s.kind, SymbolKind::Shape | SymbolKind::Kind)
            && s.range.start <= byte
            && byte < s.range.end
    })
}

fn is_in_method(k: &Knowledge, byte: usize) -> bool {
    k.symbols().iter().any(|s| {
        s.module.is_none()
            && s.kind == SymbolKind::Method
            && s.range.start <= byte
            && byte < s.range.end
    })
}

fn task_around(k: &Knowledge, byte: usize) -> Option<String> {
    let around = |s: &&crate::scan::Symbol| {
        s.module.is_none() && s.range.start <= byte && byte < s.range.end
    };
    let symbols = k.symbols();
    if let Some(method) = symbols
        .iter()
        .filter(around)
        .find(|s| s.kind == SymbolKind::Method)
    {
        let owner = method
            .parent
            .and_then(|p| k.symbol(p))
            .map_or("?", |p| p.name.as_str());
        return Some(format!("method {}.{}", owner, method.name));
    }
    symbols
        .iter()
        .filter(around)
        .find(|s| s.kind == SymbolKind::Task)
        .map(|task| format!("task {}", task.name))
}

fn from_module(k: &Knowledge, symbol: &crate::scan::Symbol) -> String {
    match symbol.module.and_then(|m| k.scan.modules.get(m)) {
        Some(module) => {
            let name = k.symbol(module.symbol).map_or("", |s| s.name.as_str());
            let from = if module.library {
                "Vela's library".to_string()
            } else {
                module.path.clone()
            };
            format!("From module {} ({}).", name, from)
        }
        None => String::new(),
    }
}

fn shared_words(member: &Symbol) -> String {
    let private = if member.private { "priv " } else { "" };
    if !member.shared {
        return if member.private {
            private.to_string()
        } else if member.read {
            "read ".to_string()
        } else {
            String::new()
        };
    }
    let change = if let Some(intro) = member.intro {
        format!("{} ", intro)
    } else if member.read {
        "read ".to_string()
    } else {
        String::new()
    };
    format!("{}shared {}", private, change)
}
