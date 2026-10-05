//! Hover: a short description of the name under the cursor, written the way
//! it is declared.

use serde::Serialize;

use crate::TokenClass;
use crate::infer::Knowledge;
use crate::scan::{LOOP_NAME, Symbol, SymbolKind};

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
    ("int", "A 64-bit signed integer. int(x) turns a value into one: a float becomes its whole part, toward zero (a float with none to give, such as nan, stops the program), and a bool becomes 1 or 0."),
    ("float", "A 64-bit floating-point number. float(n) turns an int into one, which always works."),
    ("bool", "true or false."),
    ("char", "One byte, as C's char: the code of a character, 0 to 255, written 'a' (with the escapes text has, and '\\''). c + 1 and c - 1 move it and c - 'a' is how far apart two are, an int; leaving 0 to 255 stops the program. t[i] reads a byte of text as a char and t.at(i) the same as a maybe char, none out of range. char(n) turns a code into one, int(c) the other way."),
    ("unit", "A whole number from 0 to 255 in one byte. A number written out becomes one where a unit is expected, and beside one (b + 1); arithmetic that leaves 0 to 255 stops the program. unit(x) turns an int, a float, or a bool into one, and stops the program when it doesn't fit; int(b) and float(b) always work."),
    ("text", "UTF-8 text. t.len is how many bytes it has and t[a..b] a part of it, the same bytes; == and < compare contents, a + b at r joins text into region r, and \"n = {n}\" at r is text holding values, each turned into text there (\\{ and \\} write braces). text(n) at r turns a value into text, as n.str() at r does. text lasts as long as the program, as a literal does; text@r is in region r, as the text of a value in r is."),
    ("nothing", "The return type of a task that sends no value."),
];

/// What `int("42")` sends when the text is no number: the builtin kind, which a
/// program reads, passes on, and may send itself, but never declares.
fn builtin_kind_doc(name: &str) -> Option<(&'static str, &'static str)> {
    match name {
        vela::ast::PARSE_ERROR => Some((
            "kind ParseError\n    NotANumber\n    TooLarge\nfi",
            "The error int(t) and float(t) send when the text is no number they can read. A program reads it with match, is, or a plan B, passes on one it was given, and may send one itself (fail ParseError.TooLarge). No file declares it, and no declaration may take its name or its variants.",
        )),
        vela::ast::NOT_A_NUMBER => {
            Some(("NotANumber", "A variant of ParseError: the text is no number at all, such as \"4x2\", \"\", or \" 4\" (a number in text carries no spaces, no +, no _, and no exponent)."))
        }
        vela::ast::TOO_LARGE => {
            Some(("TooLarge", "A variant of ParseError: the text is a number, but not one the type holds, such as \"9223372036854775808\" read as an int."))
        }
        vela::ast::INPUT_ERROR => Some((
            "kind InputError\n    Ended\n    NoNumber(why: ParseError)\nfi",
            "The error scan(prompt, int) and scan(prompt, float) send when there is no number to read: the input ended, or the line was no number, which holds why. A program reads it with match, is, or a plan B, passes on one it was given, and may send one itself (fail Ended). No file declares it, and no declaration may take its name or its variants.",
        )),
        vela::ast::ENDED => {
            Some(("Ended", "A variant of InputError: the input has ended, so there was no line to read a number from. A loop reading numbers stops on it."))
        }
        vela::ast::NO_NUMBER => Some((
            "NoNumber(why: ParseError)",
            "A variant of InputError: a line was read, but it was no number. It holds the ParseError reading the line gave, read with a match of its own: case NoNumber why.",
        )),
        _ => None,
    }
}

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
        "Copies the value into region r and gives a ^T@r. Also written new@r T(value). With a slice type, new [T](items) at r gives a [T]@r of the items: [v#n], an array, or a slice.",
    ),
    ("at", "new T(value) at r", "Allocates in region r, the same as new@r T(value) ('@' is 'at'). It also names where new text goes: a + b at r joins text, as (a + b)@r does, \"n = {n}\" at r holds values, and scan(prompt) at r reads a line."),
    ("in", "each i in a..b", "Counts from a up to, but not including, b. each v in xs goes through the items of an array or slice."),
    ("maybe", "maybe T", "A T, or none. Read it with match, is, or a plan B: x { fallback }."),
    ("none", "none", "The empty maybe. A pointer can't be none; use maybe ^T."),
    ("is", "value is pattern", "The condition holds when the pattern matches; its names are visible in the block."),
    (
        "$",
        "$d, $d(values), or $(w1, w2(values))",
        "A decorator, on a line above a task or a shared method, and itself a task or a shared method ($Registry.enroll). $d is a startup decorator: d(task, values) runs once where the program starts, before main, the nearest first; d takes the task first and sends nothing. $(w1, w2) wraps the task: each call of it runs w1, given the rest of the chain as a task of the task's type, its own values, and the task's values, and what w1 sends is what the call gives. Both kinds of values are worked out where the program starts, so they hold no variable or region; a wrapper's are worked out once, before the startup decorators, and every call reads them.",
    ),
    (
        "...",
        "P..., args: P..., (P...) -> R, f(args...)",
        "After a type a task takes, a group of types: any number, any mix. args: P... takes the values a call gives after the others, (P...) -> R is a task taking them, and f(args...) gives them on, whole. Each use fills the group in with the types of its values, as it fills in a type, so it costs nothing when the program runs. After the values of a slice, as in [1, 2, ...] at r, it makes a slice of them.",
    ),
    ("fail", "fail error", "Makes a failure, for a task whose type has a ~ side: <- fail e sends one. A pattern names a failure with 'failure' instead."),
    ("failure", "failure e", "Names the failure a pattern matches: case failure e, when x is failure e, and a plan B's first line. 'fail' makes one."),
    ("_", "_", "Matches anything left, or drops a value without naming it."),
    (
        "default",
        "region r default",
        "Makes r the block's default region: every allocation in the block that names no region goes there, as do those in the blocks inside it, until one names its own default. A task takes one the same way, 'r: region default'. A copy is the exception: it goes where the value it becomes goes.",
    ),
    (
        "alters",
        "alters name(…)",
        "Marks a method that changes the value it is called on: it may assign .this and what the value holds. Only a place that can change may call it: a hold variable, a field or item of one, or what a pointer points to.",
    ),
    (
        "type",
        "x.type: text",
        "The type of the value as code writes it, without regions: [int#3], [float], ^Point, Box<int>. The compiler knows it, so the value itself is never worked out; in a task that takes types, it is the type each use gives. 'type' is a reserved word, so nothing else takes the name.",
    ),
];

/// What `@this` is, in a method's header or body.
const THIS_REGION_DOC: &str = "The region the value this method is called on lives in. The method may send what the value holds there and take values in it, but can't allocate in it, since nothing gives a method the region.";

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
        (TokenClass::BuiltinFn, _) if text == vela::ast::SCAN => (
            "task scan(prompt: text) at r -> maybe text@r\ntask scan(prompt: text, int | float) -> int | float ~ InputError".to_string(),
            "Shows the prompt, then reads one line of input into region r (also written scan(prompt)@r), without its line break; gives none when the input has ended. Naming int or float reads a number from that line instead, which needs no memory and so names no region: it gives that number, or an InputError saying the input ended or the line was no number."
                .to_string(),
        ),
        (TokenClass::BuiltinFn, _) => (
            format!("task {}(value: anything with str, …)", text),
            "Writes each value, with nothing between them, then ends the line: show(\"n = \", n) prints n = 5. An array or slice is written [1, 2], a tuple (1, \"a\"), a maybe none or { 4 }, a pointer as its address (0x7ffd…), and text inside any of them in quotes. A shape is written as a literal of the fields any file reads (Point { x: 1, y: 2 }) and a kind as its variant (Circle(1.5)), unless it has a method str of its own, which writes it in a region show makes for the line. show() writes an empty line, and the values are written left to right.".to_string(),
        ),
        // `.this`: the value the method around it is called on.
        (TokenClass::Keyword, _) if text == vela::ast::THIS_WORD => {
            let owner = owner_around(k, token.range.start);
            let code = owner.map_or_else(|| format!(".{}", text), |owner| format!(".{}: {}", text, owner.name));
            let doc = "The value this method is called on. A kind's method takes it apart with match .this, and a method marked alters may assign it.";
            (code, doc.to_string())
        }
        // `~`, which gives a type its error side.
        (TokenClass::Operator, _) if text == "~" => (
            "T ~ E".to_string(),
            "A T when it succeeds, or an error E when it fails. Read it with match, is, or a plan B: x { fallback }. Only a task's own error, never a maybe or the value type again.".to_string(),
        ),
        (TokenClass::Keyword, _) => {
            let (_, code, doc) = KEYWORD_DOCS.iter().find(|(word, _, _)| *word == text)?;
            (code.to_string(), doc.to_string())
        }
        // The builtin kind that reading a number sends, and its two variants,
        // which no file declares (see `ast::parse_error_kind`).
        (TokenClass::Type | TokenClass::Variant, None) if builtin_kind_doc(text).is_some() => {
            let (code, doc) = builtin_kind_doc(text)?;
            (code.to_string(), doc.to_string())
        }
        // `@this` in a method's body, which its header didn't name.
        (TokenClass::Region, None) if text == vela::ast::THIS_WORD => (format!("region {}", text), THIS_REGION_DOC.to_string()),
        // `xs.len` and `t.len`, which no shape declares.
        (TokenClass::Field, None) if text == "len" => {
            ("len: int".to_string(), "The number of items of an array or slice, or the number of bytes of text.".to_string())
        }
        // `t.0`: an item of a tuple.
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

/// A pattern's name, written `hold name` when the pattern can assign it.
fn pattern_name(k: &Knowledge, at: usize, typed: &str, doc: &str) -> (String, String) {
    if k.pattern_name_mutable(at) {
        return (format!("hold {}", typed), format!("{} It can be assigned.", doc));
    }
    (typed.to_string(), doc.to_string())
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
            // Each word joined by `&` is one thing every type given for it allows.
            let allows = match symbol.detail.as_str() {
                "" => "Its values can be stored, passed, and sent, and nothing else.".to_string(),
                words => words
                    .split('&')
                    .map(|word| match word.trim() {
                        "ordered" => "Every type given for it has an order, so '<' and '>' compare its values, and '==' too.".to_string(),
                        "shown" => "Every type given for it can be shown.".to_string(),
                        "same" => "Every type given for it can be compared with '==', each by its own rule (an array, a shape, or a tuple by where it is).".to_string(),
                        "similar" => "Every type given for it can be compared item by item with '~=' and '<>'.".to_string(),
                        "fixed" => "Every type given for it never changes, so it can be a tuple's item.".to_string(),
                        other => format!("Every type given for it must be '{}'.", other),
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
            };
            let gives = if what == "shape" { "each value of it is given" } else { "each call gives it" };
            (code, format!("A type {} {} takes, standing for the type {}. {}", what, task, gives, allows))
        }
        SymbolKind::Shape | SymbolKind::Kind => {
            let keyword = if symbol.kind == SymbolKind::Shape { "shape" } else { "kind" };
            let private = if symbol.private { "priv " } else { "" };
            // The types a shape takes go on its own line, as written; its
            // fields and a kind's variants each go on a line of their own.
            let takes = if symbol.detail.starts_with('<') { symbol.detail.as_str() } else { "" };
            let mut code = format!("{}{} {}{}\n", private, keyword, symbol.name, takes);
            for (_, member) in k.children(id) {
                // Another file's private members are its own, as completion leaves them out too.
                if member.kind == SymbolKind::TypeParam || (member.private && symbol.module.is_some()) {
                    continue;
                }
                let separator = if member.kind == SymbolKind::Field { ": " } else { "" };
                // A method's own word, `alters` or `shared`, is its intro.
                let (private, alters) = if member.kind == SymbolKind::Method {
                    (if member.private { "priv ".to_string() } else { String::new() }, member.intro.map_or(String::new(), |word| format!("{} ", word)))
                } else {
                    (shared_words(member), String::new())
                };
                code.push_str(&format!("    {}{}{}{}{}\n", private, alters, vela::ast::unqualified(&member.name), separator, member.detail));
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
            if symbol.shared {
                let shape = parent.map(|p| p.name.as_str()).unwrap_or("?");
                let changes = match (symbol.intro, symbol.read) {
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
            if symbol.intro == Some("hold&") {
                let doc = format!("{} The caller's place, which the call gives as '&a': every change to it is a change there.", doc);
                return Some((format!("hold& {}", typed(&symbol.name)), doc.trim_start().to_string()));
            }
            if symbol.intro == Some("hold") {
                let doc = format!("{} The task's own copy, which it may change; the caller's value never does.", doc);
                return Some((format!("hold {}", typed(&symbol.name)), doc.trim_start().to_string()));
            }
            if symbol.intro == Some("bind&") {
                let doc = format!("{} The caller's place, which the call gives as '&a', read where it is and never changed.", doc);
                return Some((format!("bind& {}", typed(&symbol.name)), doc.trim_start().to_string()));
            }
            if symbol.intro == Some("bind") {
                let doc = format!("{} Always a copy, even of a large value the compiler would otherwise read where the caller keeps it.", doc);
                return Some((format!("bind {}", typed(&symbol.name)), doc.trim_start().to_string()));
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
            Some("hold") => (format!("hold {}", typed(&symbol.name)), "A mutable variable.".to_string()),
            Some("hold&") => (
                format!("hold& {}", typed(&symbol.name)),
                "A reference: another name for the place given after '=', so a change to either is a change to both.".to_string(),
            ),
            Some("bind&") => (
                format!("bind& {}", typed(&symbol.name)),
                "A reference that reads the place given after '=', as it is when read, and never changes it.".to_string(),
            ),
            Some("result") => {
                let doc = match task_around(k, symbol.name_range.start) {
                    Some(task) => format!("A result of {}, which reaching its end sends.", task),
                    None => "A result of this task, which reaching its end sends.".to_string(),
                };
                (format!("result {}", typed(&symbol.name)), format!("{} Set it with '{} <- …'.", doc, symbol.name))
            }
            _ => (format!("bind {}", typed(&symbol.name)), "An immutable variable.".to_string()),
        },
    })
}

/// The shape or kind of this file whose block holds `byte`.
fn owner_around<'a>(k: &Knowledge<'a>, byte: usize) -> Option<&'a crate::scan::Symbol> {
    k.symbols()
        .iter()
        .find(|s| s.module.is_none() && matches!(s.kind, SymbolKind::Shape | SymbolKind::Kind) && s.range.start <= byte && byte < s.range.end)
}

/// Whether `byte` is inside a method of this file.
fn is_in_method(k: &Knowledge, byte: usize) -> bool {
    k.symbols().iter().any(|s| s.module.is_none() && s.kind == SymbolKind::Method && s.range.start <= byte && byte < s.range.end)
}

/// The task or method of this file around `byte`, as a message names it:
/// `task fib`, or `method Counter.step`.
fn task_around(k: &Knowledge, byte: usize) -> Option<String> {
    let around = |s: &&crate::scan::Symbol| s.module.is_none() && s.range.start <= byte && byte < s.range.end;
    let symbols = k.symbols();
    if let Some(method) = symbols.iter().filter(around).find(|s| s.kind == SymbolKind::Method) {
        let owner = method.parent.and_then(|p| k.symbol(p)).map_or("?", |p| p.name.as_str());
        return Some(format!("method {}.{}", owner, method.name));
    }
    symbols.iter().filter(around).find(|s| s.kind == SymbolKind::Task).map(|task| format!("task {}", task.name))
}

/// For a declaration of another file, which module it is from.
fn from_module(k: &Knowledge, symbol: &crate::scan::Symbol) -> String {
    match symbol.module.and_then(|m| k.scan.modules.get(m)) {
        Some(module) => {
            let name = k.symbol(module.symbol).map_or("", |s| s.name.as_str());
            let from = if module.library { "Vela's library".to_string() } else { module.path.clone() };
            format!("From module {} ({}).", name, from)
        }
        None => String::new(),
    }
}

/// The words a shape's member is declared with before its name, as a hover
/// writes them: `priv`, `read`, and for a shared field, `shared` and `hold`.
fn shared_words(member: &Symbol) -> String {
    let private = if member.private { "priv " } else { "" };
    if !member.shared {
        return if member.private { private.to_string() } else if member.read { "read ".to_string() } else { String::new() };
    }
    let change = if member.intro.is_some() { "hold " } else if member.read { "read " } else { "" };
    format!("{}shared {}", private, change)
}
