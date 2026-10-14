//! Types of names and expressions, for completion and hover. The checker's
//! types are used whenever it has them. Otherwise, as while the file is being
//! typed, types are read from the declarations: a parameter's written type, a
//! variable's annotation or initializer, a `case` binding's payload field,
//! and so on.

use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

use vela::ast::{ANY_REGION, KeptMode, MODULE_SEPARATOR, Mode, Passing, Span, Taken, Type, unqualified};
use vela::lexer::{LexKind, LexToken, Token};

use crate::TokenClass;
use crate::scan::{Classified, LOOP_NAME, Scan, Symbol, SymbolKind};
use crate::types::Checked;

/// Guards against initializers that refer to each other.
const MAX_DEPTH: usize = 8;

pub(crate) struct Knowledge<'a> {
    pub source: &'a str,
    pub scan: &'a Scan,
    pub checked: &'a Checked,
    /// Indices into `scan.lexed` of the tokens other than comments and `Eof`.
    sig: Vec<usize>,
    /// The shapes and kinds holding pointers, and those that can be given a
    /// region, found once (see `types_holding`).
    regional: OnceCell<HashSet<String>>,
    takes_region: OnceCell<HashSet<String>>,
}

impl vela::parser::LineTokens for Knowledge<'_> {
    fn token_at(&self, p: usize) -> Option<&Token> {
        self.token(p)
    }

    fn gt_touches_next(&self, p: usize) -> bool {
        self.range(p).end == self.range(p + 1).start
    }
}

impl<'a> Knowledge<'a> {
    pub fn new(source: &'a str, scan: &'a Scan, checked: &'a Checked) -> Self {
        let sig = scan
            .lexed
            .iter()
            .enumerate()
            .filter(|(_, t)| !matches!(t.kind, LexKind::Comment | LexKind::Token(Token::Eof)))
            .map(|(i, _)| i)
            .collect();
        Knowledge { source, scan, checked, sig, regional: OnceCell::new(), takes_region: OnceCell::new() }
    }

    pub fn len(&self) -> usize {
        self.sig.len()
    }

    pub fn lexed(&self, q: usize) -> Option<&'a LexToken> {
        self.sig.get(q).and_then(|&i| self.scan.lexed.get(i))
    }

    pub fn token(&self, q: usize) -> Option<&'a Token> {
        match self.lexed(q).map(|t| &t.kind) {
            Some(LexKind::Token(token)) => Some(token),
            _ => None,
        }
    }

    /// The token before `q`, if `q` is a position.
    pub fn token_before(&self, q: usize) -> Option<&'a Token> {
        q.checked_sub(1).and_then(|p| self.token(p))
    }

    pub fn ident(&self, q: usize) -> Option<&'a str> {
        match self.token(q) {
            Some(Token::Ident(name)) => Some(name),
            _ => None,
        }
    }

    pub fn range(&self, q: usize) -> Range<usize> {
        self.lexed(q).map_or(self.source.len()..self.source.len(), |t| t.range.clone())
    }

    /// The `>` closing the types a use gives, when the `<` at `p` opens
    /// them, as the parser reads it: a call follows the types a task is
    /// given, a literal the types a shape is given, and a variant, its `.`
    /// touching the `>`, the types a kind is given.
    pub fn types_close(&self, p: usize) -> Option<usize> {
        vela::parser::types_close(self, p)
    }

    /// Whether the `(` at `q` starts a lambda (see `Scan::lambdas`).
    pub fn starts_lambda(&self, q: usize) -> bool {
        self.lexed(q).is_some_and(|t| self.scan.lambdas.contains(&t.range.start))
    }

    fn start(&self, q: usize) -> Option<Span> {
        self.lexed(q).map(|t| t.start)
    }

    /// The position of the last token that ends at or before `byte`.
    pub fn last_before(&self, byte: usize) -> Option<usize> {
        let count = self.sig.partition_point(|&i| self.scan.lexed.get(i).is_some_and(|t| t.range.end <= byte));
        count.checked_sub(1)
    }

    /// The position of the token that starts at `byte`.
    pub fn at_byte(&self, byte: usize) -> Option<usize> {
        let q = self.sig.partition_point(|&i| self.scan.lexed.get(i).is_some_and(|t| t.range.start < byte));
        self.lexed(q).filter(|t| t.range.start == byte).map(|_| q)
    }

    /// The position of the first token of the line containing position `q`.
    pub fn line_start(&self, q: usize) -> usize {
        (0..q).rev().find(|&p| self.token(p) == Some(&Token::Newline)).map_or(0, |p| p + 1)
    }

    /// The position of the newline ending the line of position `q` (or the
    /// end of the tokens).
    pub fn line_end(&self, q: usize) -> usize {
        (q..self.len()).find(|&p| self.token(p) == Some(&Token::Newline)).unwrap_or(self.len())
    }

    pub fn symbols(&self) -> &'a [Symbol] {
        &self.scan.symbols
    }

    pub fn symbol(&self, id: usize) -> Option<&'a Symbol> {
        self.scan.symbols.get(id)
    }

    /// The declaration the name at position `q` refers to.
    pub fn symbol_at(&self, q: usize) -> Option<usize> {
        let start = self.range(q).start;
        let tokens = &self.scan.tokens;
        let i = tokens.partition_point(|t| t.range.start < start);
        tokens.get(i).filter(|t| t.range.start == start).and_then(|t| t.symbol)
    }

    /// Whether the name at position `q` declares a method of this file.
    pub fn declares_method(&self, q: usize) -> bool {
        let start = self.range(q).start;
        self.symbol_at(q)
            .and_then(|id| self.symbol(id))
            .is_some_and(|s| s.kind == SymbolKind::Method && s.module.is_none() && s.name_range.start == start)
    }

    /// The shape or kind of this file whose block holds `byte`.
    pub fn owner_around(&self, byte: usize) -> Option<usize> {
        self.scan
            .symbols
            .iter()
            .position(|s| s.module.is_none() && matches!(s.kind, SymbolKind::Shape | SymbolKind::Kind) && s.range.start <= byte && byte < s.range.end)
    }

    /// The symbols that are children of `parent`, in source order: a shape's
    /// fields and methods, a kind's variants and methods, or a variant's payload
    /// fields.
    pub fn children(&self, parent: usize) -> impl Iterator<Item = (usize, &'a Symbol)> {
        self.scan.symbols.iter().enumerate().filter(move |(_, s)| s.parent == Some(parent))
    }

    /// The children of `parent` that are of `kind`: a shape's fields, or a
    /// kind's variants, without their methods and the types they take.
    pub fn members(&self, parent: usize, kind: SymbolKind) -> impl Iterator<Item = (usize, &'a Symbol)> {
        self.children(parent).filter(move |(_, s)| s.kind == kind)
    }

    /// The first top-level declaration of `kind` called `name`: one of this
    /// file's, another module's by its qualified name (`geometry.Point`), or one
    /// `include` brings in by name.
    pub fn global(&self, name: &str, kind: SymbolKind) -> Option<usize> {
        let symbols = &self.scan.symbols;
        let qualified = name.contains(MODULE_SEPARATOR);
        symbols
            .iter()
            .position(|s| s.kind == kind && s.name == name && (qualified || s.module.is_none()))
            .or_else(|| self.scan.imports.get(name).copied().filter(|&id| symbols.get(id).is_some_and(|s| s.kind == kind)))
    }

    /// The module included under the name at position `q`, when that name is one.
    pub fn module_at(&self, q: usize) -> Option<usize> {
        self.symbol_at(q).filter(|&id| self.symbol(id).is_some_and(|s| s.kind == SymbolKind::Module))
    }

    /// For the name at position `q`: `module.name` and the position where it
    /// starts when a module's name and `.` come before it, or else the name
    /// and `q`.
    pub fn qualified_at(&self, q: usize) -> Option<(String, usize)> {
        let name = self.ident(q)?;
        if self.token_before(q) == Some(&Token::Dot)
            && let Some(start) = q.checked_sub(2)
            && self.module_at(start).is_some()
        {
            return Some((format!("{}{}{}", self.ident(start)?, MODULE_SEPARATOR, name), start));
        }
        Some((name.to_string(), q))
    }

    /// The type of a variable or parameter.
    pub fn type_of_symbol(&self, id: usize) -> Option<Type> {
        self.symbol_type(id, 0)
    }

    fn symbol_type(&self, id: usize, depth: usize) -> Option<Type> {
        let symbol = self.symbol(id)?;
        if depth > MAX_DEPTH {
            return None;
        }
        // The checker's positions are in this file; another's declarations are
        // read as written.
        if symbol.module.is_none()
            && let Some(ty) = self.checked.decls.get(&(symbol.start.line, symbol.start.col))
        {
            return Some(ty.clone());
        }
        match (symbol.kind, symbol.intro) {
            (SymbolKind::Region, _) => Some(Type::Region),
            (SymbolKind::Parameter, _) | (SymbolKind::Field, _) => parse_type(&symbol.detail),
            // A loop's name is no value.
            (SymbolKind::Variable, Some(LOOP_NAME)) => None,
            (SymbolKind::Variable, Some("each")) => self.each_type(id, depth + 1),
            (SymbolKind::Variable, Some("case" | "is")) => self.case_binding_type(id, depth + 1),
            (SymbolKind::Variable, Some("failure")) => self.plan_b_error_type(id, depth + 1),
            (SymbolKind::Variable, _) if !symbol.detail.is_empty() => parse_type(&symbol.detail),
            (SymbolKind::Macro, _) => self.macro_type(symbol, depth),
            // Text of no region in particular lasts, as the checker settles it.
            (SymbolKind::Variable, _) => self.initializer_type(symbol, depth).map(|ty| vela::check::lasting(&ty, self.regional())),
            _ => None,
        }
    }

    /// The type of a macro's value, `= 3.14159`: in this file, of the value as
    /// written; in another, of the literal it starts with.
    fn macro_type(&self, symbol: &Symbol, depth: usize) -> Option<Type> {
        if symbol.module.is_none() {
            let name = self.at_byte(symbol.name_range.start)?;
            let end = self.line_end(name).checked_sub(1)?;
            return self.span_type(name + 2, end, depth + 1);
        }
        let value = symbol.detail.strip_prefix('=')?.trim_start();
        let first = value.chars().next()?;
        match first {
            '"' => Some(Type::Text(None)),
            '\'' => Some(Type::Char),
            _ if value.starts_with("true") || value.starts_with("false") => Some(Type::Bool),
            '0'..='9' | '-' => {
                let number: String = value.chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')).collect();
                // `0xfe` is an int, though it holds an `e`.
                let digits = number.trim_start_matches('-');
                let based = digits.starts_with('0') && matches!(digits.as_bytes().get(1), Some(b'x' | b'o' | b'b'));
                Some(if !based && (number.contains('.') || number.contains('e')) { Type::Float } else { Type::Int })
            }
            _ => None,
        }
    }

    /// The type of the value a `fixed` or `flex` without a written type gives
    /// `symbol`: `fixed x = Point { … }`, `fixed ok = a < b`. In a list, the name's
    /// place decides which value it takes.
    fn initializer_type(&self, symbol: &Symbol, depth: usize) -> Option<Type> {
        let name = self.at_byte(symbol.name_range.start)?;
        let (path, names, equals) = self.binding_place(name)?;
        let (&index, inside) = path.split_first()?;
        let end = match self.loop_item(name) {
            Some((_, last)) => last.checked_sub(1)?,
            None => self.line_end(name).checked_sub(1)?,
        };
        if end <= equals {
            return None;
        }
        let values = self.value_list(equals + 1, end);
        let value = if names > 1 && values.len() == 1 {
            // One call sends every value, or one tuple gives its items.
            match self.expression_type(end, depth + 1)? {
                Type::Results(types) | Type::Tuple(types) => types.into_iter().nth(index)?,
                _ => return None,
            }
        } else {
            let &(first, last) = values.get(index)?;
            if first > last {
                return None;
            }
            // A plan B spanning lines (`{`, or `{ failure e`) gives the value it extracts.
            let names_error = self.token(last.checked_sub(1)?) == Some(&Token::Failure)
                && matches!(self.token(last), Some(Token::Ident(_) | Token::Underscore));
            let brace = if names_error { last.checked_sub(2)? } else { last };
            if self.token(brace) == Some(&Token::LBrace) {
                let extracted = self.expression_type(brace.checked_sub(1)?, depth + 1)?.extracted().cloned()?;
                // One whose last line is `none` is a maybe of what it extracts.
                match self.plan_b_gives_none(brace) && !matches!(extracted, Type::Maybe(_)) {
                    true => Type::Maybe(Box::new(extracted)),
                    false => extracted,
                }
            } else {
                self.span_type(first, last, depth + 1)?
            }
        };
        // `fixed a, (b, c) = …`: the item of each tuple the name is in.
        inside.iter().try_fold(value, |value, &item| match value {
            Type::Tuple(items) => items.into_iter().nth(item),
            _ => None,
        })
    }

    /// Where the `fixed` or `flex` name at position `name` is in its list, and
    /// the position of the `=` that ends the names.
    /// Where the `fixed` or `flex` name at position `name` is: its place in the
    /// list and, inside `( … )`, in each tuple it unpacks; how many names the
    /// list has; and the position of the `=` that ends the names.
    fn binding_place(&self, name: usize) -> Option<(Vec<usize>, usize, usize)> {
        let end = self.line_end(name);
        let equals = (name..end).find(|&q| self.token(q) == Some(&Token::Assign))?;
        // In a loop's start, the names of its item only.
        let start = self.loop_item(name).map_or(self.line_start(name), |(first, _)| first);
        let mut path = vec![0usize];
        let mut names = 1;
        // Before the `=`, a `<` only opens the types a type is given, whose
        // commas part no names: `fixed e: Either<int, text> = …`.
        let mut angles = 0usize;
        for q in start..equals {
            match self.token(q) {
                Some(Token::Lt) => angles += 1,
                Some(Token::Gt) => angles = angles.saturating_sub(1),
                _ if angles > 0 => {}
                Some(Token::LParen) if q < name => path.push(0),
                Some(Token::RParen) if path.len() > 1 && q < name => {
                    path.pop();
                }
                Some(Token::Comma) if q < name => {
                    if let Some(last) = path.last_mut() {
                        *last += 1;
                    }
                }
                _ => {}
            }
        }
        // Names after `name` at the top level count too, and the parentheses after it don't matter.
        let mut depth = 0usize;
        for q in start..equals {
            match self.token(q) {
                Some(Token::LParen | Token::Lt) => depth += 1,
                Some(Token::Gt) => depth = depth.saturating_sub(1),
                Some(Token::RParen) => depth = depth.saturating_sub(1),
                Some(Token::Comma) if depth == 0 => names += 1,
                _ => {}
            }
        }
        Some((path, names, equals))
    }

    /// Where `q` is in a loop's start, `loop flex i = 0, j = 9 || …`: the
    /// first position of its item, and the one of the `,` or `||` ending it,
    /// as the parser parts them (see `vela::parser`'s `loop_items`).
    fn loop_item(&self, q: usize) -> Option<(usize, usize)> {
        let line = self.line_start(q);
        let first = (line..q).find(|&p| matches!(self.token(p), Some(Token::Loop)))? + 1;
        let bar = (first..self.line_end(q)).find(|&p| self.token(p) == Some(&Token::Parts))?;
        if q >= bar {
            return None;
        }
        vela::parser::loop_items(self, first, bar).into_iter().find(|&(_, last)| q <= last)
    }

    /// The first and last position of each value between `first` and `last`,
    /// which commas outside brackets separate.
    fn value_list(&self, first: usize, last: usize) -> Vec<(usize, usize)> {
        let mut values = Vec::new();
        let mut start = first;
        let mut brackets = 0usize;
        // The types a use gives, `Either<int, text>.Left(1)`, part no values.
        let mut types_end = None;
        for p in first..=last {
            if types_end.is_some_and(|end| p <= end) {
                continue;
            }
            match self.token(p) {
                Some(Token::Lt) => types_end = self.types_close(p),
                Some(Token::LParen | Token::LBrace | Token::LBracket) => brackets += 1,
                Some(Token::RParen | Token::RBrace | Token::RBracket) => brackets = brackets.saturating_sub(1),
                Some(Token::Comma) if brackets == 0 => {
                    values.push((start, p.saturating_sub(1)));
                    start = p + 1;
                }
                _ => {}
            }
        }
        values.push((start, last));
        values
    }

    /// The type of the name an `each` declares: an `int` counting through a
    /// range, or an item of the array or slice after `in`.
    fn each_type(&self, id: usize, depth: usize) -> Option<Type> {
        let Some((first, last)) = self.each_items(id) else { return Some(Type::Int) };
        let symbol = self.symbol(id)?;
        let name = self.at_byte(symbol.name_range.start)?;
        // `each k, v in m`: the second name is the value, the first the key.
        let second = self.token_before(name) == Some(&Token::Comma);
        // `each k in m.keys`: one side of a map.
        if last >= first + 2
            && self.token(last - 1) == Some(&Token::Dot)
            && let Some(side) = self.ident(last).and_then(vela::ast::MapOp::from_word)
            && let Some(Type::Map(key, value, _)) = self.span_type(first, last - 2, depth)
        {
            return match side {
                vela::ast::MapOp::Keys => Some(*key),
                vela::ast::MapOp::Values => Some(*value),
                vela::ast::MapOp::Has => None,
            };
        }
        match self.span_type(first, last, depth)? {
            Type::Array(items, _) | Type::Slice(items, _) => Some(*items),
            Type::Map(_, value, _) if second => Some(*value),
            Type::Map(key, _, _) => Some(*key),
            _ => None,
        }
    }

    /// For the name an `each` declares, the first and last position of the
    /// array or slice it goes through; `None` when it counts through a range.
    pub fn each_items(&self, id: usize) -> Option<(usize, usize)> {
        let symbol = self.symbol(id)?;
        let mut name = self.at_byte(symbol.name_range.start)?;
        // `each k, v in m`: what it goes through follows the second name.
        if self.token(name + 1) == Some(&Token::Comma) {
            name += 2;
        }
        let (first, last) = (name + 2, self.line_end(name).checked_sub(1)?);
        let counts = self.token(name + 1) != Some(&Token::In)
            || first > last
            || self.outside_brackets(first, last).any(|q| self.token(q) == Some(&Token::Range));
        (!counts).then_some((first, last))
    }

    /// The positions from `first` to `last` that no bracket between them encloses.
    fn outside_brackets(&self, first: usize, last: usize) -> impl Iterator<Item = usize> + '_ {
        let mut brackets = 0usize;
        (first..=last).filter(move |&q| match self.token(q) {
            Some(Token::LParen | Token::LBrace | Token::LBracket) => {
                brackets += 1;
                false
            }
            Some(Token::RParen | Token::RBrace | Token::RBracket) => {
                brackets = brackets.saturating_sub(1);
                false
            }
            _ => brackets == 0,
        })
    }

    /// The type of the name at `name` in the pattern starting at `first`, given
    /// the type `ty` the pattern matches: an item of a tuple for each `( … )`
    /// the name is inside.
    fn pattern_item(&self, ty: Type, first: usize, name: usize) -> Option<Type> {
        let mut path = vec![0usize];
        for q in first..name {
            match self.token(q) {
                Some(Token::LParen) => path.push(0),
                Some(Token::RParen) => {
                    path.pop();
                }
                Some(Token::Comma) => {
                    if let Some(last) = path.last_mut() {
                        *last += 1;
                    }
                }
                _ => {}
            }
        }
        // The first entry is the whole value, which `( … )` then takes apart.
        path.remove(0);
        path.into_iter().try_fold(ty, |ty, item| match ty {
            Type::Tuple(items) => items.into_iter().nth(item),
            _ => None,
        })
    }

    /// The position of the variant's name in the pattern after the `case` or
    /// `is` at `case`: `Circle`, `Shape.Circle`, or `m.Shape.Circle`.
    /// The mode of the pattern name at `name`, written `flex name` or
    /// inside a `flex ( … )`, so it can be assigned.
    pub fn pattern_name_mode(&self, name: usize) -> Mode {
        let line = self.line_start(name);
        let mut at = name;
        loop {
            if at > line && self.token(at - 1) == Some(&Token::Flex) {
                return Mode::Flex;
            }
            if at > line && self.token(at - 1) == Some(&Token::Bind) {
                return Mode::Bind;
            }
            let mut depth = 0usize;
            // The `(` of the group this name is in, if any.
            let Some(open) = (line..at).rev().find(|&q| match self.token(q) {
                Some(Token::RParen) => {
                    depth += 1;
                    false
                }
                Some(Token::LParen) if depth == 0 => true,
                Some(Token::LParen) => {
                    depth -= 1;
                    false
                }
                _ => false,
            }) else {
                return Mode::Fixed;
            };
            at = open;
        }
    }

    pub fn pattern_variant_at(&self, case: usize) -> usize {
        let mut at = case + 1;
        while self.token(at + 1) == Some(&Token::Dot) && self.ident(at + 2).is_some() {
            at += 2;
        }
        at
    }

    /// The type of the value the pattern after the `case` or `is` at position
    /// `case` matches: the `match`'s subject, or the value before `is`.
    fn pattern_subject(&self, case: usize, depth: usize) -> Option<Type> {
        if self.token(case) == Some(&Token::Is) {
            // The value an `is` matches starts after `when` or `loop`, or after
            // the `and` joining it to the parts before it (`a and m is {x}`).
            let line = self.line_start(case) + 1;
            let mut start = line;
            let mut nesting = 0usize;
            for q in line..case {
                match self.token(q) {
                    Some(Token::LParen | Token::LBracket | Token::LBrace) => nesting += 1,
                    Some(Token::RParen | Token::RBracket | Token::RBrace) => nesting = nesting.saturating_sub(1),
                    Some(Token::And) if nesting == 0 => start = q + 1,
                    _ => {}
                }
            }
            self.span_type(start, case.checked_sub(1)?, depth)
        } else {
            let matched = self.enclosing_match(case)?;
            self.span_type(matched + 1, self.line_end(matched).checked_sub(1)?, depth)
        }
    }

    /// The variant called `tag` of `subject`'s kind, which may be another
    /// module's, whose variants a pattern names alone.
    fn variant_of(&self, subject: &Type, tag: &str) -> Option<usize> {
        let Type::Named(kind, _) = subject else { return None };
        let kind = self.global(kind, SymbolKind::Kind)?;
        self.members(kind, SymbolKind::Variant).find(|(_, v)| unqualified(&v.name) == tag).map(|(id, _)| id)
    }

    /// The declaration `token` refers to. A variant named alone in a pattern
    /// (`case Circle r`) is the matched value's kind's, when that is known,
    /// rather than whichever variant of that name the file sees.
    pub fn symbol_of(&self, token: &Classified) -> Option<usize> {
        if token.class == TokenClass::Variant
            && let Some(q) = self.at_byte(token.range.start)
            && let Some(case) = q.checked_sub(1).filter(|&case| matches!(self.token(case), Some(Token::Case | Token::Is)))
            && let Some(tag) = self.ident(q)
            && let Some(variant) = self.pattern_subject(case, 0).and_then(|subject| self.variant_of(&subject, tag))
        {
            return Some(variant);
        }
        token.symbol
    }

    /// The type of a name a `case` or `is` pattern binds: a variant's payload
    /// field, or what `{value}` and `fail e` take from the subject.
    fn case_binding_type(&self, id: usize, depth: usize) -> Option<Type> {
        let symbol = self.symbol(id)?;
        let name = self.at_byte(symbol.name_range.start)?;
        let case = (self.line_start(name)..name).rev().find(|&q| matches!(self.token(q), Some(Token::Case | Token::Is)))?;
        let outcome = || self.pattern_subject(case, depth);
        // `flex` before the names makes them changeable; the pattern goes on after it.
        let start = if self.token(case + 1) == Some(&Token::Flex) { case + 2 } else { case + 1 };
        match self.token(start)? {
            // `{v}` and `{(a, b)}`: the value inside, and its items.
            Token::LBrace => self.pattern_item(outcome()?.extracted().cloned()?, start + 1, name),
            // `failure e` and `failure (a, b)`.
            Token::Failure => match outcome()? {
                Type::Or(_, error) => self.pattern_item(*error, start + 1, name),
                _ => None,
            },
            // `(x, y)`: the items of a tuple, which always match.
            Token::LParen => self.pattern_item(outcome()?, start, name),
            _ => {
                let tag_at = self.pattern_variant_at(case);
                // The names before this one, which `flex` may be written before.
                let index = (tag_at + 1..name).filter(|&q| matches!(self.token(q), Some(Token::Ident(_) | Token::Underscore))).count();
                let tag = self.ident(tag_at)?;
                // The variant of the matched value's kind, which may be another
                // module's, whose variants the case names alone.
                let of_subject = outcome().and_then(|subject| self.variant_of(&subject, tag));
                let named = self.symbol_at(tag_at).filter(|&id| self.symbol(id).is_some_and(|s| s.kind == SymbolKind::Variant));
                let variant = of_subject.or(named).or_else(|| self.global(tag, SymbolKind::Variant))?;
                let (_, field) = self.children(variant).nth(index)?;
                // A kind given types: its payload with those types in place of
                // the names it takes, `case Left q` of an `Either<int, text>`.
                let detail = match (outcome(), self.symbol(variant).and_then(|v| v.parent)) {
                    (Some(Type::Applied(applied)), Some(kind)) => {
                        let params: Vec<String> = self.children(kind).filter(|(_, c)| c.kind == SymbolKind::TypeParam).map(|(_, c)| c.name.clone()).collect();
                        given_in(&field.detail, &params, &applied.types)
                    }
                    _ => field.detail.clone(),
                };
                let ty = parse_type(&detail)?;
                // A payload's pointers are in the region of the value matched.
                Some(match outcome() {
                    Some(Type::Named(_, Some(region))) => self.in_region(&ty, &region),
                    _ => ty,
                })
            }
        }
    }

    /// The type of the error a plan B's `fail e` names: the `E` of the value
    /// before its `{`.
    fn plan_b_error_type(&self, id: usize, depth: usize) -> Option<Type> {
        let symbol = self.symbol(id)?;
        let name = self.at_byte(symbol.name_range.start)?;
        let mut open = name.checked_sub(1)?.checked_sub(1)?;
        while self.token(open) == Some(&Token::Newline) {
            open = open.checked_sub(1)?;
        }
        if self.token(open) != Some(&Token::LBrace) {
            return None;
        }
        match self.expression_type(open.checked_sub(1)?, depth)? {
            Type::Or(_, error) => Some(*error),
            _ => None,
        }
    }

    /// The `match` whose case is at position `case`, skipping nested blocks.
    pub fn enclosing_match(&self, case: usize) -> Option<usize> {
        let mut depth = 0usize;
        for q in (0..case).rev() {
            match self.token(q) {
                Some(Token::Fi) => depth += 1,
                Some(Token::When | Token::Loop | Token::Endless | Token::Each | Token::Block) if depth > 0 => depth -= 1,
                Some(Token::Match) if depth == 0 => return Some(q),
                Some(Token::Match) => depth -= 1,
                Some(Token::Task | Token::Shape | Token::Kind) => return None,
                _ => {}
            }
        }
        None
    }

    /// The type of the expression whose last token is at position `q`.
    pub fn type_ending_at(&self, q: usize) -> Option<Type> {
        self.expression_type(q, 0)
    }

    fn expression_type(&self, q: usize, depth: usize) -> Option<Type> {
        if depth > MAX_DEPTH {
            return None;
        }
        let exact = |p: usize| self.start(p).and_then(|s| self.checked.exprs.get(&(s.line, s.col))).cloned();
        // `new [int]` or `new Point`: a `new` of the empty value, written
        // without `( )`.
        if let Some(new) = self.bare_new(q) {
            return exact(new).or_else(|| self.new_type(new, self.range(q).end));
        }
        match self.token(q)? {
            // `t.0`: an item of a tuple, which the checker records at its position.
            Token::Int(n) if self.token_before(q) == Some(&Token::Dot) => exact(q).or_else(|| {
                let Type::Tuple(items) = self.expression_type(q.checked_sub(2)?, depth + 1)? else { return None };
                items.get(usize::try_from(*n).ok()?).cloned()
            }),
            Token::Int(_) => Some(Type::Int),
            Token::Float(_) => Some(Type::Float),
            Token::Char(_) => Some(Type::Char),
            Token::Text(_) => Some(Type::Text(None)),
            Token::True | Token::False => Some(Type::Bool),
            Token::Ident(name) => exact(q).or_else(|| {
                // `palette.Red`: a module's variant, recorded at the module's name.
                if let Some((full, start)) = self.qualified_at(q).filter(|(_, start)| *start != q) {
                    return exact(start).or_else(|| {
                        // `m.square`: a module's task named as a value.
                        if let Some(task) = self.global(&full, SymbolKind::Task) {
                            return task_value_type(&self.symbol(task)?.detail);
                        }
                        let variant = self.global(&full, SymbolKind::Variant)?;
                        self.variant_value(variant, None, depth)
                    });
                }
                // `Shape.Circle`: a kind's variant.
                if let Some(variant) = self.symbol_at(q).filter(|&id| self.symbol(id).is_some_and(|s| s.kind == SymbolKind::Variant)) {
                    return self.written_kind_type(q).or_else(|| self.variant_value(variant, None, depth));
                }
                // `.this` and `.x` inside a method: the value it is called on,
                // which lives in `@this`, and a field of it.
                if self.token_before(q) == Some(&Token::Dot) && !q.checked_sub(2).and_then(|b| self.token(b)).is_some_and(crate::scan::ends_value) {
                    let owner = self.symbol(self.owner_around(self.range(q).start)?)?;
                    let value = match self.takes_region().contains(&owner.name) {
                        true => Type::Named(owner.name.clone(), Some(vela::ast::THIS_WORD.to_string())),
                        false => Type::named(&owner.name),
                    };
                    if name == vela::ast::THIS_WORD {
                        return Some(value);
                    }
                    let ty = self.field_type(&owner.name, name)?;
                    return Some(match value {
                        Type::Named(_, Some(region)) => self.in_region(&ty, &region),
                        _ => ty,
                    });
                }
                if self.token_before(q) == Some(&Token::Dot) {
                    let base = self.expression_type(q.checked_sub(2)?, depth + 1)?;
                    if name == "len" && matches!(base, Type::Array(..) | Type::Slice(..) | Type::Text(_)) {
                        return Some(Type::Int);
                    }
                    let Type::Named(shape, region) = base else { return None };
                    let ty = self.field_type(&shape, name)?;
                    return Some(match region {
                        Some(region) => self.in_region(&ty, &region),
                        None => ty,
                    });
                }
                // `"n = {n}" at r` or `"n = {n}"@r`: text holding values, in `r`.
                if matches!(self.token_before(q), Some(Token::At | Token::AtSign)) && q >= 2 && self.closes_values(q - 2) {
                    return Some(Type::Text(Some(name.to_string())));
                }
                // `new [T] at r` or `new Point at r`: a `new` of the empty value,
                // written without `( )`, in `r`.
                if self.token_before(q) == Some(&Token::At)
                    && let Some(end) = q.checked_sub(2)
                    && let Some(new) = self.bare_new(end)
                {
                    return exact(new).or_else(|| Some(self.in_region(&self.new_type(new, self.range(end).end)?, name)));
                }
                // `new T(v) at r`: a pointer into `r`, or a slice for `new [T](…) at r`;
                // `scan(…) at r` or `scan(…)@r`: a line in `r`.
                if matches!(self.token_before(q), Some(Token::At | Token::AtSign)) && q >= 2 && self.token(q - 2) == Some(&Token::RParen) {
                    // `(a + b)@r`: text joined into `r`.
                    let open = self.matching_open(q - 2, &Token::LParen, &Token::RParen)?;
                    let bare = open.checked_sub(1).is_none_or(|before| !matches!(self.token(before), Some(Token::Ident(_) | Token::RParen | Token::RBracket)));
                    if bare && self.joins(open + 1, q - 3) {
                        return Some(Type::Text(Some(name.to_string())));
                    }
                    // `x.copy() at r`: the value's type, in `r`.
                    if let Some(value) = self.copied_value(q - 2) {
                        return Some(self.in_region(&self.expression_type(value, depth + 1)?.without_regions(), name));
                    }
                    // `f(x) at r`: what the task sends, with the region it takes
                    // last written after it, which only some of its type may be
                    // in (see `call_type`).
                    if self.calls_task(q - 2) {
                        return self.expression_type(q - 2, depth + 1);
                    }
                    return match self.expression_type(q - 2, depth + 1)? {
                        Type::Ptr(pointee, _) => Some(Type::Ptr(Box::new(self.in_region(&pointee, name)), Some(name.to_string()))),
                        Type::Slice(items, _) => Some(Type::Slice(Box::new(self.in_region(&items, name)), Some(name.to_string()))),
                        made @ Type::Map(..) => Some(self.in_region(&made, name)),
                        Type::Maybe(line) if matches!(*line, Type::Text(_)) => Some(Type::Maybe(Box::new(Type::Text(Some(name.to_string()))))),
                        // `n.str() at r` and `text(n) at r`.
                        Type::Text(_) => Some(Type::Text(Some(name.to_string()))),
                        other => Some(other),
                    };
                }
                let id = self.symbol_at(q)?;
                let symbol = self.symbol(id)?;
                match symbol.kind {
                    SymbolKind::Parameter | SymbolKind::Variable | SymbolKind::Region | SymbolKind::Macro => self.symbol_type(id, depth + 1),
                    SymbolKind::Variant => self.variant_value(id, None, depth),
                    // `square`, a task named as a value.
                    SymbolKind::Task => task_value_type(&symbol.detail),
                    _ => None,
                }
            }),
            Token::Caret => exact(q).or_else(|| self.expression_type(q.checked_sub(1)?, depth + 1)?.ptr_inner().cloned()),
            Token::RBracket => {
                let open = self.matching_open(q, &Token::LBracket, &Token::RBracket)?;
                // The checker records an item, a slice, and an array at the `[`.
                exact(open).or_else(|| self.bracket_type(open, q, depth))
            }
            Token::RBrace => {
                let open = self.matching_open(q, &Token::LBrace, &Token::RBrace)?;
                let literal = self.token(open + 1) == Some(&Token::RBrace)
                    || (self.ident(open + 1).is_some() && self.token(open + 2) == Some(&Token::Colon));
                if !literal {
                    // A plan B, whose type the checker records at its `{`:
                    // the value it extracts.
                    return exact(open).or_else(|| {
                        let extracted = self.expression_type(open.checked_sub(1)?, depth + 1)?.extracted().cloned()?;
                        // `v { none }` is a maybe of what it extracts.
                        if self.token(q.checked_sub(1)?) == Some(&Token::None) && open + 2 == q && !matches!(extracted, Type::Maybe(_)) {
                            return Some(Type::Maybe(Box::new(extracted)));
                        }
                        // A value without pointers takes its fallback's region.
                        let fallback = || self.expression_type(q.checked_sub(1)?, depth + 1).filter(|f| f.without_regions() == extracted.without_regions());
                        match extracted.regions().contains(&ANY_REGION).then(fallback).flatten() {
                            Some(fallback) => Some(fallback),
                            None => Some(extracted),
                        }
                    });
                }
                let (shape, _) = self.qualified_at(open.checked_sub(1)?)?;
                // Each `field: value`, with the field's declared type.
                let fields = self
                    .value_list(open + 1, q.checked_sub(1)?)
                    .into_iter()
                    .filter(|&(first, last)| first + 2 <= last && self.token(first + 1) == Some(&Token::Colon))
                    .map(|(first, last)| (self.ident(first).and_then(|field| self.field_type(&shape, field)), (first + 2, last)))
                    .collect();
                Some(self.value_type(&shape, fields, depth))
            }
            Token::RParen => {
                let open = self.matching_open(q, &Token::LParen, &Token::RParen)?;
                // `new T(…)`, `new ^T(…)`, `new@r T(…)`, `new [T](…)`, `new [K: V](…)`:
                // the type is everything between `new` (and its region) and `(`.
                let type_token = |r: usize| {
                    matches!(
                        self.token(r),
                        Some(Token::Caret | Token::Maybe | Token::New | Token::AtSign | Token::Dot | Token::Ident(_) | Token::LBracket | Token::RBracket | Token::Colon | Token::Hash | Token::Int(_) | Token::Tilde)
                    )
                };
                let new = open.checked_sub(1).and_then(|before| (0..=before).rev().take_while(|&r| type_token(r)).find(|&r| self.token(r) == Some(&Token::New)));
                if let Some(new) = new.filter(|&new| (new + 1..open).all(|r| self.token(r) != Some(&Token::New))) {
                    return exact(new).or_else(|| self.new_type(new, self.range(open).start));
                }
                // `t.at(i)`: a byte of text, or none.
                if open.checked_sub(1).is_some_and(|p| self.token(p) == Some(&Token::At) && self.token_before(p) == Some(&Token::Dot)) {
                    return exact(open - 1).or(Some(Type::Maybe(Box::new(Type::Char))));
                }
                // `pick()(x)` or `ops[0](x)`: the call of the task a value holds.
                if let Some(before) = open.checked_sub(1).filter(|&p| matches!(self.token(p), Some(Token::RParen | Token::RBracket)))
                    && let Some((task, kept)) = self.expression_type(before, depth + 1).and_then(called_task)
                {
                    return exact(open).or_else(|| Some(self.task_value_call_type(&task, kept.as_deref(), open, q, depth)));
                }
                let Some(callee) = open.checked_sub(1).and_then(|p| self.ident(p).map(|name| (p, name))) else {
                    // A tuple, `(a, b)`, or a parenthesized expression.
                    let values = self.value_list(open + 1, q.checked_sub(1)?);
                    if values.len() >= 2 {
                        let items: Option<Vec<Type>> = values.into_iter().map(|(first, last)| self.span_type(first, last, depth + 1)).collect();
                        return exact(open).or_else(|| items.map(Type::Tuple));
                    }
                    return self.span_type(open + 1, q.checked_sub(1)?, depth + 1);
                };
                let (p, word) = callee;
                // `scan(…)`, before its region, or `scan(…, int)`, which reads a
                // number and names no region.
                if word == vela::ast::SCAN && self.token_before(p) != Some(&Token::Dot) {
                    // The type it reads is the last argument: `scan(prompt, int)`.
                    let mut number = None;
                    if let Some(last) = q.checked_sub(1)
                        && last.checked_sub(1).map(|before| self.token(before)) == Some(Some(&Token::Comma))
                    {
                        number = self.ident(last).and_then(vela::ast::number_cast);
                    }
                    return Some(match number {
                        Some(number) => Type::Or(Box::new(number), Box::new(Type::Named(vela::ast::INPUT_ERROR.to_string(), None))),
                        None => Type::Maybe(Box::new(Type::Text(None))),
                    });
                }
                // `n.str(r)`: text in `r`; `n.str()` and `text(n)`, text before
                // their region.
                // `int(x)` and `float(x)`: the number the type they name is.
                if self.token_before(p) != Some(&Token::Dot)
                    && let Some(number) = vela::ast::number_cast(word)
                {
                    return Some(number);
                }
                let cast = word == vela::ast::TEXT_CAST && self.token_before(p) != Some(&Token::Dot);
                if cast || (word == vela::ast::STR && self.token_before(p) == Some(&Token::Dot)) {
                    let region = (!cast && open + 2 == q).then(|| self.ident(open + 1)).flatten();
                    return Some(Type::Text(region.map(str::to_string)));
                }
                // `x.copy()`, before its region: the value's type, in the region
                // the place it goes names, which the checker records at the word.
                if let Some(value) = self.copied_value(q) {
                    return exact(p).or_else(|| Some(self.in_region(&self.expression_type(value, depth + 1)?.without_regions(), ANY_REGION)));
                }
                // `xs.add(…)` gives nothing, and `xs.del(…)` the item it takes
                // out, which the checker records at the word; `xs.added(…)` and
                // `xs.without(…)` give a new slice, or map, as a copy does.
                if self.token_before(p) == Some(&Token::Dot)
                    && let Some(op) = vela::ast::EditOp::from_word(word)
                    && let Some(base @ (Type::Slice(..) | Type::Map(..))) = p.checked_sub(2).and_then(|base| self.expression_type(base, depth + 1))
                {
                    let given = match (op, base) {
                        (vela::ast::EditOp::Added | vela::ast::EditOp::Without, base) => Some(self.in_region(&base.without_regions(), ANY_REGION)),
                        (vela::ast::EditOp::Add, Type::Slice(..)) => Some(Type::Nothing),
                        (vela::ast::EditOp::Del, Type::Slice(items, _)) => Some(*items),
                        _ => None,
                    };
                    if let Some(given) = given {
                        return exact(p).or(Some(given));
                    }
                }
                // `c.total()` and `.total()`: what a method sends, which the
                // checker records at its name.
                if self.token_before(p) == Some(&Token::Dot)
                    && let Some(method) = self.symbol_at(p).and_then(|id| self.symbol(id)).filter(|s| s.kind == SymbolKind::Method)
                {
                    return exact(p).or_else(|| Some(self.call_type(&method.detail, open, q, depth)));
                }
                // `Shape.Circle(…)` and `m.Shape.Circle(…)`: a kind's variant,
                // recorded where its name starts.
                if let Some(variant) = self.symbol_at(p).filter(|&id| self.symbol(id).is_some_and(|s| s.kind == SymbolKind::Variant))
                    && self.token_before(p) == Some(&Token::Dot)
                {
                    let mut start = p;
                    while self.token_before(start) == Some(&Token::Dot) && start >= 2 && self.ident(start - 2).is_some() {
                        start -= 2;
                    }
                    return exact(start).or_else(|| self.written_kind_type(p)).or_else(|| self.variant_value(variant, Some((open, q)), depth));
                }
                // `f(x)`, where `f` holds a task: what that task sends.
                if let Some(id) = self.symbol_at(p).filter(|&id| self.symbol(id).is_some_and(|s| matches!(s.kind, SymbolKind::Parameter | SymbolKind::Variable)))
                    && let Some((task, kept)) = self.symbol_type(id, depth + 1).and_then(called_task)
                {
                    return exact(p).or_else(|| Some(self.task_value_call_type(&task, kept.as_deref(), open, q, depth)));
                }
                // A module's task or variant is recorded at the module's name.
                let (name, p) = self.qualified_at(p)?;
                exact(p).or_else(|| {
                    if let Some(variant) = self.global(&name, SymbolKind::Variant) {
                        return self.variant_value(variant, Some((open, q)), depth);
                    }
                    let task = self.global(&name, SymbolKind::Task)?;
                    Some(self.call_type(&self.symbol(task)?.detail, open, q, depth))
                })
            }
            _ => None,
        }
    }

    /// The type of the brackets from position `open` to `close`, as the file is
    /// written: an item or a slice of the value before them, or an array literal.
    fn bracket_type(&self, open: usize, close: usize, depth: usize) -> Option<Type> {
        let (first, last) = (open + 1, close.checked_sub(1)?);
        if first > last {
            return None;
        }
        let indexes = open.checked_sub(1).is_some_and(|b| {
            matches!(self.token(b), Some(Token::Ident(_) | Token::Text(_) | Token::RParen | Token::RBracket | Token::RBrace | Token::Caret))
        });
        // `xs[#i]`: the item in a maybe, none out of range.
        if indexes && self.token(first) == Some(&Token::Hash) {
            let item = match self.expression_type(open - 1, depth + 1)? {
                Type::Array(items, _) | Type::Slice(items, _) => *items,
                Type::Text(_) => Type::Char,
                _ => return None,
            };
            return Some(Type::Maybe(Box::new(item)));
        }
        if indexes {
            let slicing = self.outside_brackets(first, last).any(|q| self.token(q) == Some(&Token::Range));
            return match (self.expression_type(open - 1, depth + 1)?, slicing) {
                (Type::Array(items, _) | Type::Slice(items, _), false) => Some(*items),
                (Type::Slice(items, region), true) => Some(Type::Slice(items, region)),
                (Type::Array(items, _), true) => Some(Type::Slice(items, None)),
                // A part of text is text where the text is, and a byte of it a char.
                (text @ Type::Text(_), true) => Some(text),
                (Type::Text(_), false) => Some(Type::Char),
                _ => None,
            };
        }
        // `[value#count]` or `[a, b, c]`.
        if let Some(hash) = self.outside_brackets(first, last).find(|&q| self.token(q) == Some(&Token::Hash)) {
            let item = self.span_type(first, hash.checked_sub(1)?, depth + 1)?;
            let count = match (hash + 1 == last, self.token(last)) {
                (true, Some(Token::Int(n))) => u64::try_from(*n).ok()?,
                _ => return None,
            };
            return Some(Type::Array(Box::new(item), count));
        }
        // The items' type: text of a region, rather than a literal's, which fits it.
        let values = self.value_list(first, last);
        let types: Vec<Type> = values.iter().filter_map(|&(start, end)| self.span_type(start, end, depth + 1)).collect();
        let item = types.iter().find(|ty| ty.regions().iter().any(|region| *region != ANY_REGION)).or(types.first())?.clone();
        Some(Type::Array(Box::new(item), u64::try_from(values.len()).ok()?))
    }

    /// The type of the whole expression from position `first` to `last`. Its
    /// operators outside brackets decide: a comparison or `and`/`or`/`not`
    /// gives `bool`, and arithmetic gives `float` if any operand is a float
    /// (as the checker promotes it) and `int` if all are ints. Without
    /// operators, it is the type of the expression ending at `last`.
    fn span_type(&self, first: usize, last: usize, depth: usize) -> Option<Type> {
        if first > last || depth > MAX_DEPTH {
            return None;
        }
        // `[@r](x: int) -> int …`: a lambda that keeps what it uses in `r`.
        if self.token(first) == Some(&Token::LBracket)
            && let Some(open) = (first + 1..=last).find(|&q| self.token(q) == Some(&Token::RBracket)).map(|q| q + 1)
            && let Some((region, mode, _)) = kept_prefix(self.source.get(self.range(first).start..self.range(open.saturating_sub(1)).end)?)
            && self.token(open) == Some(&Token::LParen)
            && let Some(close) = self.matching_close(open, last)
            && self.token(close + 1) == Some(&Token::Arrow)
        {
            let end = (close + 2..=last).find(|&q| matches!(self.token(q), Some(Token::Send | Token::Fi))).map_or(last, |q| q.saturating_sub(1));
            let task = task_value_type_beside(self.source.get(self.range(open).start..self.range(end).end)?, Some(&region))?;
            return closure_of(task, region, mode);
        }
        // `(x: int) -> int …`: a lambda, whose header, up to its body, gives
        // its type.
        if self.token(first) == Some(&Token::LParen)
            && let Some(close) = self.matching_close(first, last)
            && self.token(close + 1) == Some(&Token::Arrow)
        {
            let end = (close + 2..=last).find(|&q| matches!(self.token(q), Some(Token::Send | Token::Fi))).map_or(last, |q| q.saturating_sub(1));
            return task_value_type(self.source.get(self.range(first).start..self.range(end).end)?);
        }
        // `a + b at r`: text joined into `r`.
        if let (Some(at), Some(region)) = (last.checked_sub(1), self.ident(last))
            && self.token(at) == Some(&Token::At)
            && at > first
            && (self.joins(first, at - 1) || self.closes_values(at - 1))
        {
            return Some(Type::Text(Some(region.to_string())));
        }
        let mut brackets = 0usize;
        // The end of each operand of the arithmetic, if there is any.
        let mut operand_ends = Vec::new();
        // Whether an operand comes next, so that a `-` there is unary minus.
        let mut expect_operand = true;
        // The operands raised to the power of the next one, by their place.
        let mut bases = Vec::new();
        // The `>` closing the types a use gives, `Option<int>.None`, which
        // compare nothing.
        let mut types_end = None;
        for p in first..=last {
            if types_end.is_some_and(|end| p <= end) {
                continue;
            }
            let token = self.token(p)?;
            match token {
                Token::Lt if brackets == 0 && self.types_close(p).is_some() => {
                    types_end = self.types_close(p);
                    continue;
                }
                Token::LParen | Token::LBrace | Token::LBracket => brackets += 1,
                Token::RParen | Token::RBrace | Token::RBracket => brackets = brackets.saturating_sub(1),
                _ if brackets > 0 => {}
                Token::Eq
                | Token::Ne
                | Token::Similar
                | Token::NotSimilar
                | Token::Lt
                | Token::Gt
                | Token::Le
                | Token::Ge
                | Token::And
                | Token::Or
                | Token::Not => {
                    return Some(Type::Bool);
                }
                Token::Range => return None,
                Token::Plus | Token::Minus | Token::Star | Token::Slash | Token::Percent | Token::Power => {
                    if !expect_operand {
                        if *token == Token::Power {
                            bases.push(operand_ends.len());
                        }
                        operand_ends.push(p.checked_sub(1)?);
                    }
                    expect_operand = true;
                    continue;
                }
                _ => {}
            }
            expect_operand = false;
        }
        if operand_ends.is_empty() {
            return self.expression_type(last, depth);
        }
        operand_ends.push(last);
        // Arithmetic works on two numbers of the same type: an int and a float
        // don't mix (see `check::check_binary`), so a chain of one of them has
        // that type, and one that mixes them, or holds anything else, has none
        // to read. A unit beside ints is a unit: the ints of a chain that
        // checks are numbers written out, which become units there.
        let mut types: Vec<Option<Type>> = operand_ends.iter().map(|&end| self.expression_type(end, depth + 1)).collect();
        // `a ** b`, as the checker types it (see `check::check_power`): an int
        // raised to a unit or to a number written out is an int, a unit raised
        // to a unit or a number a unit, and anything else a float.
        for &base in bases.iter().rev() {
            // A power written out, `2` or `(1 + 1)`: never negative.
            let written = operand_ends.get(base).zip(operand_ends.get(base + 1)).is_some_and(|(&end, &next)| {
                (end + 2..=next).all(|q| matches!(self.token(q), Some(Token::Int(_) | Token::LParen | Token::RParen | Token::Plus | Token::Star | Token::Power)))
            });
            let power = match (types.get(base).cloned().flatten(), types.get(base + 1).cloned().flatten()) {
                (Some(Type::Int), Some(Type::Unit)) => Type::Int,
                (Some(Type::Int), Some(Type::Int)) if written => Type::Int,
                (Some(Type::Unit), Some(Type::Unit)) => Type::Unit,
                (Some(Type::Unit), Some(Type::Int)) if written => Type::Unit,
                (Some(Type::Int | Type::Unit | Type::Float), Some(Type::Int | Type::Unit | Type::Float)) => Type::Float,
                _ => return None,
            };
            for side in [base, base + 1] {
                if let Some(ty) = types.get_mut(side) {
                    *ty = Some(power.clone());
                }
            }
        }
        let mut number: Option<Type> = None;
        for ty in types {
            match (ty, &number) {
                // A char moved by ints is a char; two chars apart are an int.
                (Some(Type::Char), Some(Type::Char)) => number = Some(Type::Int),
                (Some(Type::Char), Some(Type::Int)) | (Some(Type::Int), Some(Type::Char)) => number = Some(Type::Char),
                (Some(Type::Char), None) => number = Some(Type::Char),
                (Some(Type::Int), Some(Type::Unit)) => {}
                (Some(Type::Unit), Some(Type::Int)) => number = Some(Type::Unit),
                (Some(ty @ (Type::Int | Type::Float | Type::Unit)), _) if number.as_ref().is_none_or(|seen| *seen == ty) => number = Some(ty),
                _ => return None,
            }
        }
        number
    }

    /// Whether the tokens from `first` to `last` are a chain of `+`: a `+`
    /// outside any brackets, which a region after them makes a join.
    fn joins(&self, first: usize, last: usize) -> bool {
        let mut brackets = 0usize;
        for p in first..=last {
            match self.token(p) {
                Some(Token::LParen | Token::LBrace | Token::LBracket) => brackets += 1,
                Some(Token::RParen | Token::RBrace | Token::RBracket) => brackets = brackets.saturating_sub(1),
                Some(Token::Plus) if brackets == 0 => return true,
                None => return false,
                _ => {}
            }
        }
        false
    }

    /// Whether the token at `p` is the last piece of text holding values, from
    /// its last value's `}` to its closing quote (see `scan::spread`).
    fn closes_values(&self, p: usize) -> bool {
        matches!(self.token(p), Some(Token::Text(_))) && self.source.get(self.range(p)).is_some_and(|text| text.starts_with('}'))
    }

    /// What a call through a value of type `task` sends, between `open` and
    /// `close`, in the regions its arguments are in: a task type names its
    /// regions its own way.
    fn task_value_call_type(&self, task: &vela::ast::TaskType, kept: Option<&str>, open: usize, close: usize, depth: usize) -> Type {
        // The region a lambda keeps what it uses in is the caller's, as it is.
        let mut regions: HashMap<String, String> = kept.map(|region| (region.to_string(), region.to_string())).into_iter().collect();
        for (i, ((first, last), ty)) in self.call_args(open, close).into_iter().zip(&task.params).enumerate() {
            if first > last {
                continue;
            }
            match task.region(i) {
                Some(name) => {
                    if let Some(region) = self.region_arg(first, last) {
                        regions.insert(name.to_string(), region.to_string());
                    }
                }
                None => {
                    if let Some(actual) = self.span_type(first, last, depth + 1) {
                        vela::check::unify_regions(ty, &actual, &mut regions);
                    }
                }
            }
        }
        task.ret.map_regions(&mut |region| Some(regions.get(region).cloned().unwrap_or_else(|| ANY_REGION.to_string())))
    }

    /// Each argument's first and last position in the call between `open`
    /// and `close`.
    fn call_args(&self, open: usize, close: usize) -> Vec<(usize, usize)> {
        let mut args = Vec::new();
        let mut first = open + 1;
        let mut brackets = 0usize;
        // The types a use gives, `f(Either<int, text>.Left(1))`, part no values.
        let mut types_end = None;
        for p in open + 1..close {
            if types_end.is_some_and(|end| p <= end) {
                continue;
            }
            match self.token(p) {
                Some(Token::Lt) => types_end = self.types_close(p),
                Some(Token::LParen | Token::LBrace | Token::LBracket) => brackets += 1,
                Some(Token::RParen | Token::RBrace | Token::RBracket) => brackets = brackets.saturating_sub(1),
                Some(Token::Comma) if brackets == 0 => {
                    args.push((first, p - 1));
                    first = p + 1;
                }
                _ => {}
            }
        }
        args.push((first, close - 1));
        args
    }

    /// The region a call gives as its argument from `first` to `last`: a
    /// name alone, which names a region or nothing this file knows.
    fn region_arg(&self, first: usize, last: usize) -> Option<&'a str> {
        let named = self.symbol_at(first).and_then(|id| self.symbol(id));
        self.ident(first).filter(|_| first == last && named.is_none_or(|s| s.kind == SymbolKind::Region))
    }

    /// The type a call of the task with signature `detail` gives, where its
    /// parentheses are at positions `open` and `close`. As the checker does, the
    /// task's regions become the caller's: a region parameter's argument, or the
    /// region an argument's type shows, and `_` for one no argument shows.
    fn call_type(&self, detail: &str, open: usize, close: usize, depth: usize) -> Type {
        let args = self.call_args(open, close);
        let params = parameters(detail);
        let mut regions = HashMap::new();
        for ((first, last), (name, ty)) in args.into_iter().zip(params.iter().cloned()) {
            match ty {
                _ if first > last => {}
                Some(Type::Region) => {
                    if let Some(region) = self.region_arg(first, last) {
                        regions.insert(name, region.to_string());
                    }
                }
                Some(ty) => {
                    if let Some(actual) = self.span_type(first, last, depth + 1) {
                        vela::check::unify_regions(&ty, &actual, &mut regions);
                    }
                }
                None => {}
            }
        }
        // A region the task takes last and the parentheses leave out, written
        // after the call instead: `f(x) at r` or `f(x)@r`.
        if let Some((name, Some(Type::Region))) = params.last()
            && !regions.contains_key(name)
            && matches!(self.token(close + 1), Some(Token::At | Token::AtSign))
            && let Some(region) = self.ident(close + 2)
        {
            regions.insert(name.clone(), region.to_string());
        }
        return_type(detail).map_regions(&mut |region| Some(regions.get(region).cloned().unwrap_or_else(|| ANY_REGION.to_string())))
    }

    /// Whether the `)` at `q` ends a call of one of the program's tasks or
    /// methods, as opposed to `new`, `scan`, `text`, or a built-in `str`.
    fn calls_task(&self, q: usize) -> bool {
        let Some(word) = self.matching_open(q, &Token::LParen, &Token::RParen).and_then(|open| open.checked_sub(1)) else { return false };
        if self.ident(word).is_none() {
            return false;
        }
        if self.token_before(word) == Some(&Token::Dot) && self.symbol_at(word).and_then(|id| self.symbol(id)).is_some_and(|s| s.kind == SymbolKind::Method) {
            return true;
        }
        self.qualified_at(word).is_some_and(|(name, _)| self.global(&name, SymbolKind::Task).is_some())
    }

    /// When the type ending at `q` is a `new`'s, written without `( )` for
    /// the empty value, `new [int]` or `new Point`, which is a value already:
    /// the position of the `new`.
    pub fn bare_new(&self, q: usize) -> Option<usize> {
        let mut p = q;
        // The brackets open around `p`: a `,` or a `:` is a type's only inside one.
        let mut depth = 0usize;
        loop {
            match self.token(p) {
                Some(Token::New) => {
                    let start = if self.token(p + 1) == Some(&Token::AtSign) { p + 3 } else { p + 1 };
                    return (start <= q && depth == 0).then_some(p);
                }
                Some(Token::RBracket | Token::Gt) => depth += 1,
                Some(Token::LBracket | Token::Lt) => depth = depth.checked_sub(1)?,
                Some(Token::Colon | Token::Comma) if depth > 0 => {}
                Some(Token::Ident(_) | Token::Dot | Token::Hash | Token::Int(_) | Token::Caret | Token::Maybe | Token::AtSign) => {}
                _ => return None,
            }
            p = p.checked_sub(1)?;
        }
    }

    /// The value the `new` at `new` makes, its type written up to the byte
    /// `end`: a pointer, or the slice or map itself, in the region a `new@r`
    /// names.
    fn new_type(&self, new: usize, end: usize) -> Option<Type> {
        let region = (self.token(new + 1) == Some(&Token::AtSign)).then(|| self.ident(new + 2)).flatten();
        let from = if region.is_some() { self.range(new + 2).end } else { self.range(new).end };
        let written = parse_type(self.source.get(from..end)?)?;
        Some(match region {
            Some(region) => self.in_region(&Self::made_by_new(written), region),
            None => Self::made_by_new(written),
        })
    }

    /// What a `new` of the type `written` makes: the slice or map itself, or
    /// a pointer to the value.
    fn made_by_new(written: Type) -> Type {
        match written {
            made @ (Type::Slice(..) | Type::Map(..)) => made,
            pointee => Type::Ptr(Box::new(pointee), None),
        }
    }

    /// When the `)` at `q` ends `x.copy()`, the position where `x` ends.
    pub fn copied_value(&self, q: usize) -> Option<usize> {
        let word = self.matching_open(q, &Token::LParen, &Token::RParen)?.checked_sub(1)?;
        (q == word + 2 && self.ident(word) == Some(vela::ast::COPY) && self.token_before(word) == Some(&Token::Dot)).then_some(word.checked_sub(2)?)
    }

    /// Whether a value of type `ty` holds what `copy()` copies: text, a slice,
    /// or a pointer, directly or in shapes and kinds by value.
    pub fn can_copy(&self, ty: &Type) -> bool {
        let takes_region = self.takes_region();
        holds_pointers(ty, takes_region) || holds_text(ty, takes_region)
    }

    /// Whether the plan B whose `{` ends the line at `open` has `none` alone
    /// on its last line.
    fn plan_b_gives_none(&self, open: usize) -> bool {
        let mut depth = 0usize;
        let mut p = open + 1;
        while let Some(token) = self.token(p) {
            match token {
                Token::LBrace => depth += 1,
                Token::RBrace if depth > 0 => depth -= 1,
                Token::RBrace => {
                    let mut last = (open + 1..p).rev().filter(|&q| self.token(q) != Some(&Token::Newline));
                    return last.next().is_some_and(|q| self.token(q) == Some(&Token::None) && self.token(q - 1) == Some(&Token::Newline));
                }
                _ => {}
            }
            p += 1;
        }
        false
    }

    /// The position of the bracket that `close` at `q` closes, on the same line.
    pub fn matching_open(&self, q: usize, open: &Token, close: &Token) -> Option<usize> {
        let mut depth = 0usize;
        for p in (0..q).rev() {
            match self.token(p) {
                Some(Token::Newline) => return None,
                Some(t) if t == close => depth += 1,
                Some(t) if t == open => {
                    if depth == 0 {
                        return Some(p);
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
        None
    }

    /// The position of the `)` closing the `(` at `open`, no further than `last`.
    fn matching_close(&self, open: usize, last: usize) -> Option<usize> {
        let mut depth = 0usize;
        for p in open..=last {
            match self.token(p)? {
                Token::LParen => depth += 1,
                Token::RParen => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some(p);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// The value a variant makes, with the arguments in the parentheses at
    /// positions `parens`, if it has any.
    fn variant_value(&self, variant: usize, parens: Option<(usize, usize)>, depth: usize) -> Option<Type> {
        let kind = self.variant_kind(variant)?;
        let args = match parens {
            Some((open, close)) if open + 1 < close => self.value_list(open + 1, close - 1),
            _ => Vec::new(),
        };
        let fields = self.children(variant).map(|(_, field)| parse_type(&field.detail)).zip(args).collect();
        Some(self.value_type(&kind, fields, depth))
    }

    /// The type of a value of the shape or kind `name` whose fields, of the
    /// declared types given, take the values from each first to last position.
    /// As the checker settles it, a value holding pointers is in their region,
    /// or in any region (`@_`) while it holds none.
    fn value_type(&self, name: &str, fields: Vec<(Option<Type>, (usize, usize))>, depth: usize) -> Type {
        let takes_region = self.takes_region();
        if !takes_region.contains(name) {
            return Type::named(name);
        }
        // The region of the value itself, as its fields' types name it.
        const OWN: &str = "";
        let mut regions = HashMap::new();
        for (declared, (first, last)) in fields {
            if let (Some(declared), Some(actual)) = (declared, self.span_type(first, last, depth + 1)) {
                vela::check::unify_regions(&vela::check::in_region(&declared, OWN, takes_region), &actual, &mut regions);
            }
        }
        // A value holding only text that lasts as long as the program names no
        // region, unless it holds pointers.
        match regions.remove(OWN) {
            Some(region) => Type::Named(name.to_string(), Some(region)),
            None if self.regional().contains(name) => Type::Named(name.to_string(), Some(ANY_REGION.to_string())),
            None => Type::named(name),
        }
    }

    /// The kind a variant belongs to.
    /// The kind a variant at position `q` is of, as the types written before
    /// it give it: `Option<int>.None` is an `Option<int>`.
    fn written_kind_type(&self, q: usize) -> Option<Type> {
        let gt = q.checked_sub(2).filter(|_| self.token_before(q) == Some(&Token::Dot))?;
        if self.token(gt) != Some(&Token::Gt) {
            return None;
        }
        let mut depth = 0usize;
        let mut p = gt;
        let lt = loop {
            match self.token(p)? {
                Token::Gt => depth += 1,
                Token::Lt if depth == 1 => break p,
                Token::Lt => depth -= 1,
                _ => {}
            }
            p = p.checked_sub(1)?;
        };
        let name = lt.checked_sub(1).filter(|&n| self.ident(n).is_some())?;
        // `m.Option<int>.None`: the module's name too.
        let start = self.qualified_at(name).map_or(name, |(_, start)| start);
        match parse_type(self.source.get(self.range(start).start..self.range(gt).end)?)? {
            Type::Applied(applied) => Some(Type::Applied(applied)),
            _ => None,
        }
    }

    pub fn variant_kind(&self, variant: usize) -> Option<String> {
        let kind = self.symbol(variant)?.parent?;
        Some(self.symbol(kind)?.name.clone())
    }

    /// `ty`, a field's type or the type after `new`, in a value of region
    /// `region`, as the checker settles it (see `vela::check::in_region`).
    fn in_region(&self, ty: &Type, region: &str) -> Type {
        vela::check::in_region(ty, region, self.takes_region())
    }

    /// The shapes and kinds that can be given a region, as the checker finds
    /// them: those whose fields, as written, hold pointers or text (see
    /// `vela::check::regional_types` and `vela::check::textual_types`).
    fn takes_region(&self) -> &HashSet<String> {
        self.takes_region.get_or_init(|| {
            let mut takes = self.regional().clone();
            takes.extend(self.types_holding(holds_text));
            takes
        })
    }

    /// The shapes and kinds with a field, as written, that holds a pointer.
    fn regional(&self) -> &HashSet<String> {
        self.regional.get_or_init(|| self.types_holding(holds_pointers))
    }

    /// The shapes and kinds with a field, as written, for which `holds` is
    /// true, given the ones already found.
    fn types_holding(&self, holds: fn(&Type, &HashSet<String>) -> bool) -> HashSet<String> {
        let symbols = self.symbols();
        // Each shape's or kind's field types (a kind's are its variants' payloads).
        let mut fields: HashMap<usize, Vec<Type>> = HashMap::new();
        for field in symbols.iter().filter(|s| s.kind == SymbolKind::Field) {
            let parent = field.parent.and_then(|p| symbols.get(p).map(|s| (p, s)));
            let owner = match parent {
                Some((_, variant)) if variant.kind == SymbolKind::Variant => variant.parent,
                Some((p, _)) => Some(p),
                None => None,
            };
            if let (Some(owner), Some(ty)) = (owner, parse_type(&field.detail)) {
                fields.entry(owner).or_default().push(ty);
            }
        }
        // The owners whose fields name each type, to look at again when it
        // turns out to be regional.
        let mut users: HashMap<String, Vec<usize>> = HashMap::new();
        for (&owner, types) in &fields {
            for ty in types {
                for name in named_types(ty) {
                    users.entry(name).or_default().push(owner);
                }
            }
        }
        let mut regional = HashSet::new();
        let mut pending: Vec<usize> = fields.keys().copied().collect();
        while let Some(owner) = pending.pop() {
            let Some(symbol) = symbols.get(owner) else { continue };
            let holds = fields.get(&owner).is_some_and(|types| types.iter().any(|ty| holds(ty, &regional)));
            if holds && regional.insert(symbol.name.clone()) {
                pending.extend(users.get(&symbol.name).into_iter().flatten().copied());
            }
        }
        regional
    }

    /// The type of a shape's field.
    pub fn field_type(&self, shape: &str, field: &str) -> Option<Type> {
        let shape = self.global(shape, SymbolKind::Shape)?;
        let (_, field) = self.members(shape, SymbolKind::Field).find(|(_, s)| s.name == field)?;
        parse_type(&field.detail)
    }
}

/// The shape and kind names a type mentions.
fn named_types(ty: &Type) -> Vec<String> {
    match ty {
        Type::Named(name, _) => vec![name.clone()],
        Type::Ptr(inner, _) | Type::Maybe(inner) | Type::Array(inner, _) | Type::Slice(inner, _) => named_types(inner),
        Type::Or(value, error) => named_types(value).into_iter().chain(named_types(error)).collect(),
        _ => Vec::new(),
    }
}

/// Whether a value of type `ty` holds a pointer, given the shapes and kinds in
/// `regional` that do.
fn holds_pointers(ty: &Type, regional: &HashSet<String>) -> bool {
    match ty {
        Type::Ptr(..) | Type::Slice(..) | Type::Map(..) => true,
        Type::Named(name, _) => regional.contains(name),
        Type::Maybe(inner) | Type::Array(inner, _) => holds_pointers(inner, regional),
        Type::Or(value, error) => holds_pointers(value, regional) || holds_pointers(error, regional),
        _ => false,
    }
}

/// Whether a value of type `ty` holds text by value, given the shapes and kinds
/// in `textual` that do.
fn holds_text(ty: &Type, textual: &HashSet<String>) -> bool {
    match ty {
        Type::Text(_) => true,
        Type::Named(name, _) => textual.contains(name),
        Type::Maybe(inner) | Type::Array(inner, _) => holds_text(inner, textual),
        Type::Or(value, error) => holds_text(value, textual) || holds_text(error, textual),
        _ => false,
    }
}

/// A type written in source, such as `int`, `^Point@r`, `maybe ^Node@r`,
/// `region`, `[int#4]`, `[^Node]@r`, or `int ~ ParseError`. Written regions
/// are kept as written.
/// The type written `text`, with each of the names `params` a declaration
/// takes written as the type `types` gives it in the same place.
fn given_in(text: &str, params: &[String], types: &[Type]) -> String {
    let mut out = String::new();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        match params.iter().position(|param| param == word).and_then(|i| types.get(i)) {
            Some(ty) => out.push_str(&ty.to_string()),
            None => out.push_str(word),
        }
        word.clear();
    };
    for c in text.chars() {
        if c.is_alphanumeric() || c == '_' {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

pub(crate) fn parse_type(text: &str) -> Option<Type> {
    parse_type_within(text, 0)
}

/// `parse_type` inside `depth` brackets; no deeper than the compiler allows, so
/// a type with thousands of them isn't built (and walked) recursively.
fn parse_type_within(text: &str, depth: usize) -> Option<Type> {
    if depth > vela::parser::MAX_NESTING {
        return None;
    }
    let text = text.trim();
    // `(int) -> int`, maybe after `maybe` or `^`: a task's type, whose result
    // takes every `~` and `@` after its arrow.
    if unbracketed(text, "->").next().is_some() {
        return parse_task_type(text, depth);
    }
    // `~` gives a type its error side, with or without spaces around it.
    if let Some(at) = unbracketed(text, "~").next() {
        let (value, error) = (text.get(..at)?, text.get(at + "~".len()..)?);
        return Some(Type::Or(Box::new(parse_simple_type(value, depth)?), Box::new(parse_simple_type(error, depth)?)));
    }
    parse_simple_type(text, depth)
}

/// `(int, text) -> int`, after any `maybe` and `^`: a task's type, with its
/// regions named as the checker names them (see `TaskType::normalized`).
fn parse_task_type(text: &str, depth: usize) -> Option<Type> {
    let mut text = text.trim();
    let mut prefixes = Vec::new();
    loop {
        if let Some(inner) = text.strip_prefix('^') {
            prefixes.push(true);
            text = inner.trim_start();
        } else if let Some(inner) = text.strip_prefix("maybe ") {
            prefixes.push(false);
            text = inner.trim_start();
        } else {
            break;
        }
        if prefixes.len() > vela::parser::MAX_NESTING {
            return None;
        }
    }
    // `[@r](int) -> int`: a lambda's that keeps what it uses in `r`.
    let kept = kept_prefix(text);
    if let Some((_, _, rest)) = &kept {
        text = rest.trim_start();
    }
    let close = closing_paren(text)?;
    let ret = text.get(close + 1..)?.trim_start().strip_prefix("->")?;
    let inside = text.get(1..close)?;
    let mut params = Vec::new();
    let mut taken = Vec::new();
    if !inside.trim().is_empty() {
        for param in split_list(inside) {
            let param = param.trim();
            // `@r`, a region it takes, and `flex& int`, a place (see `Taken`).
            let (how, ty) = if let Some(region) = param.strip_prefix('@') {
                (Taken::Region(region.trim().to_string()), Type::Region)
            } else if let Some((passing, ty)) = place_word(param) {
                (Taken::Place(passing), parse_type_within(ty, depth + 1)?)
            } else if let Some(ty) = param.strip_prefix("flex ") {
                // `flex [int]@r`: a value whose items the task changes.
                let ty = parse_type_within(ty, depth + 1)?;
                (flex_taken(&ty), ty)
            } else if let Some(ty) = param.strip_prefix("bind ") {
                // `bind [int]@r`: a value whose contents the task changes.
                let ty = parse_type_within(ty, depth + 1)?;
                (bind_taken(&ty), ty)
            } else {
                (Taken::Value, parse_type_within(param.strip_prefix("fixed ").unwrap_or(param), depth + 1)?)
            };
            params.push(ty);
            taken.push(how);
        }
    }
    let ret = parse_type_within(ret, depth + 1)?;
    let task = vela::ast::TaskType { params, taken, ret };
    let mut ty = match kept {
        Some((region, mode, _)) => Type::Closure(Box::new(vela::ast::ClosureType { task, region, mode }.normalized())),
        None => Type::Task(Box::new(task.normalized())),
    };
    for pointer in prefixes.into_iter().rev() {
        ty = if pointer { Type::Ptr(Box::new(ty), None) } else { Type::Maybe(Box::new(ty)) };
    }
    Some(ty)
}

/// How a task's type takes a `flex` value of type `ty`: as one whose items it
/// changes where `ty` is a slice, a map or a pointer (or a maybe of one), as the
/// compiler does for every twofold type, and as a value otherwise.
fn flex_taken(ty: &Type) -> Taken {
    match ty {
        Type::Slice(..) | Type::Map(..) | Type::Ptr(..) => Taken::Flex,
        Type::Maybe(inner) => flex_taken(inner),
        _ => Taken::Value,
    }
}

/// How a task's type takes a `bind` value of type `ty`: as one the task may
/// change the contents of, but never assign as a whole. The compiler keeps
/// `bind` in the task type even for simple types, so this returns `Bind` for
/// every type.
fn bind_taken(_ty: &Type) -> Taken {
    Taken::Bind
}

/// Where the `)` closing the `(` that `text` starts with is.
fn closing_paren(text: &str) -> Option<usize> {
    list_end(text.strip_prefix('(')?).map(|at| at + 1)
}

/// The type of a task whose detail is `detail`, `(n: int) -> int`, as a value:
/// `(int) -> int`.
pub(crate) fn task_value_type(detail: &str) -> Option<Type> {
    task_value_type_beside(detail, None)
}

/// `task_value_type`, for a lambda that keeps what it uses in `kept`, which its
/// types may name as it is (see `TaskType::normalized_beside`).
fn task_value_type_beside(detail: &str, kept: Option<&str>) -> Option<Type> {
    let mut params = Vec::new();
    let mut taken = Vec::new();
    for (name, ty) in parameters(detail) {
        let ty = ty?;
        // The name carries how it is given, `flex& n`, which a type keeps for
        // a place; a region is named as its parameter is.
        taken.push(match (&ty, name.split_whitespace().collect::<Vec<_>>().as_slice()) {
            (Type::Region, [.., region]) => Taken::Region(region.to_string()),
            (_, ["flex", ..]) => flex_taken(&ty),
            (_, ["bind", ..]) => bind_taken(&ty),
            (_, [word, ..]) => place_word(word).map_or(Taken::Value, |(passing, _)| Taken::Place(passing)),
            _ => Taken::Value,
        });
        params.push(ty);
    }
    Some(Type::Task(Box::new(vela::ast::TaskType { params, taken, ret: return_type(detail) }.normalized_beside(kept))))
}

/// How a place in a task's type or a lambda's header is taken, `flex& int` or
/// `fixed& int`, and the text after the word and its `&`.
fn place_word(text: &str) -> Option<(Passing, &str)> {
    let after = |word: &str| text.strip_prefix(word)?.strip_prefix('&');
    after("flex")
        .map(|rest| (Passing::FlexRef, rest))
        .or_else(|| after("bind").map(|rest| (Passing::BindRef, rest)))
        .or_else(|| after("fixed").map(|rest| (Passing::FixedRef, rest)))
}

/// `item` without the word it may start with, `flex`, `fixed` or `bind`.
fn without_word(item: &str) -> &str {
    ["flex", "fixed", "bind"].iter().find_map(|word| item.strip_prefix(word)).unwrap_or(item)
}

/// `[@r]`, `[flex @r]` or `[bind @r]` at the start of `text`, what a lambda
/// keeps: the region, how it treats what it keeps, and the text after it.
fn kept_prefix(text: &str) -> Option<(String, KeptMode, &str)> {
    let inner = text.strip_prefix('[')?;
    let close = inner.find(']')?;
    // The region among the places it keeps, `[flex& count, @r]`.
    let mut region = None;
    for item in inner.get(..close)?.split(',').map(str::trim) {
        // `flex& count`, written `flex & count` too.
        let word = without_word(item);
        if word.len() < item.len() && word.trim_start().starts_with('&') {
            continue;
        }
        region = Some(if let Some(rest) = item.strip_prefix("flex") {
            if rest.trim_start().starts_with('@') {
                (KeptMode::Flex, rest.trim_start())
            } else {
                return None;
            }
        } else if let Some(rest) = item.strip_prefix("bind") {
            if rest.trim_start().starts_with('@') {
                (KeptMode::Bind, rest.trim_start())
            } else {
                return None;
            }
        } else {
            (KeptMode::Plain, item)
        });
    }
    let (mode, region) = region?;
    let region = region.strip_prefix('@')?.trim();
    if region.is_empty() || !region.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    Some((region.to_string(), mode, inner.get(close + 1..)?))
}

/// The task's type a value of type `ty` is called as, with the region it keeps
/// what it uses in, for a lambda that keeps them.
fn called_task(ty: Type) -> Option<(vela::ast::TaskType, Option<String>)> {
    match ty {
        Type::Task(task) => Some((*task, None)),
        Type::Closure(closure) => Some((closure.task, Some(closure.region))),
        _ => None,
    }
}

/// The type of a lambda that keeps what it uses, of a task's type `task`.
fn closure_of(task: Type, region: String, mode: KeptMode) -> Option<Type> {
    let Type::Task(task) = task else { return None };
    Some(Type::Closure(Box::new(vela::ast::ClosureType { task: *task, region, mode }.normalized())))
}

/// The bytes where `needle` starts in `text` outside brackets and parentheses.
fn unbracketed<'t>(text: &'t str, needle: &'t str) -> impl Iterator<Item = usize> + 't {
    let mut brackets = 0usize;
    // The quote of the text or char a parameter's value to fall back on is
    // inside, `fill: text = ", "`, and whether the last char escaped this one.
    let mut quoted: Option<char> = None;
    let mut escaped = false;
    text.char_indices().filter_map(move |(i, c)| {
        if let Some(quote) = quoted {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                _ if c == quote => quoted = None,
                _ => {}
            }
            return None;
        }
        match c {
            '"' | '\'' => {
                quoted = Some(c);
                return None;
            }
            '[' | '(' | '{' => brackets += 1,
            ']' | ')' | '}' => brackets = brackets.saturating_sub(1),
            _ => {}
        }
        (brackets == 0 && c != ']' && c != ')' && c != '}' && text.get(i..).is_some_and(|rest| rest.starts_with(needle))).then_some(i)
    })
}

/// Where the `)` closing a list that `text` starts inside of is: the first
/// that no bracket, brace, or quote in `text` holds, as a parameter's value to
/// fall back on may, `fill: text = ")"`.
fn list_end(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut quoted: Option<char> = None;
    let mut escaped = false;
    for (i, c) in text.char_indices() {
        if let Some(quote) = quoted {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                _ if c == quote => quoted = None,
                _ => {}
            }
            continue;
        }
        match c {
            '"' | '\'' => quoted = Some(c),
            '[' | '(' | '{' => depth += 1,
            ')' if depth == 0 => return Some(i),
            ']' | ')' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    None
}

/// The parts of `text` between its commas outside brackets and parentheses.
fn split_list(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    for at in unbracketed(text, ",") {
        parts.extend(text.get(start..at));
        start = at + 1;
    }
    parts.extend(text.get(start..));
    parts
}

/// The types between the `<` and `>` of a shape or kind given them, split at
/// their commas outside brackets, parentheses, and the `< … >` of another.
fn split_angled(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0usize, 0usize);
    let mut previous = ' ';
    for (i, c) in text.char_indices() {
        match c {
            '[' | '(' | '<' => depth += 1,
            // The `>` of an arrow, `->`, closes nothing.
            '>' if previous == '-' => {}
            ']' | ')' | '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.extend(text.get(start..i));
                start = i + 1;
            }
            _ => {}
        }
        previous = c;
    }
    parts.extend(text.get(start..));
    parts
}

/// A type without `or`: after any `^` and `maybe`, a name or brackets, and
/// its region.
fn parse_simple_type(text: &str, depth: usize) -> Option<Type> {
    let text = text.trim();
    let Some(at) = unbracketed(text, "@").last() else { return parse_prefixed_type(text, depth) };
    let (ty, region) = (text.get(..at)?, text.get(at + 1..)?.trim());
    if !is_name(region) {
        return None;
    }
    Some(given_no_regions(&parse_prefixed_type(ty, depth)?.map_regions_of_names(&mut |_| Some(region.to_string()))))
}

/// `ty` with the types each shape or kind in it is given written without
/// regions, as a program writes them: `Box<text>@r`, whose text is in `r`.
fn given_no_regions(ty: &Type) -> Type {
    match ty {
        Type::Applied(applied) => Type::Applied(Box::new(applied.with_types(applied.types.iter().map(Type::without_regions).collect()))),
        Type::Ptr(inner, region) => Type::Ptr(Box::new(given_no_regions(inner)), region.clone()),
        Type::Slice(inner, region) => Type::Slice(Box::new(given_no_regions(inner)), region.clone()),
        Type::Map(key, value, region) => Type::Map(Box::new(given_no_regions(key)), Box::new(given_no_regions(value)), region.clone()),
        Type::Array(inner, count) => Type::Array(Box::new(given_no_regions(inner)), *count),
        Type::Maybe(inner) => Type::Maybe(Box::new(given_no_regions(inner))),
        Type::Tuple(items) => Type::Tuple(items.iter().map(given_no_regions).collect()),
        other => other.clone(),
    }
}

fn parse_prefixed_type(text: &str, depth: usize) -> Option<Type> {
    // The prefixes, outermost first; no deeper than the compiler allows, so a
    // type with thousands of `^` isn't built (and walked) recursively.
    let mut text = text.trim();
    let mut pointers = Vec::new();
    loop {
        if let Some(inner) = text.strip_prefix('^') {
            pointers.push(true);
            text = inner.trim_start();
        } else if let Some(inner) = text.strip_prefix("maybe ") {
            pointers.push(false);
            text = inner.trim_start();
        } else {
            break;
        }
        if pointers.len() > vela::parser::MAX_NESTING {
            return None;
        }
    }
    let mut ty = match text.trim_end() {
        "region" => Type::Region,
        // `(T, U)`, a tuple of two or more items, or one type the parentheses
        // only group, as in `maybe (int ~ text)` (see `parser::parse_tuple_type`).
        parens if parens.starts_with('(') && parens.ends_with(')') => {
            let items: Vec<Type> = split_list(parens.get(1..parens.len() - 1)?).into_iter().map(|item| parse_type_within(item, depth + 1)).collect::<Option<_>>()?;
            match <[Type; 1]>::try_from(items) {
                Ok([only]) => only,
                Err(items) => Type::Tuple(Some(items).filter(|items| items.len() >= 2)?),
            }
        }
        // `[T#n]`, `[K: V]`, `[:V]` (keys of text), or `[T]`.
        brackets if brackets.starts_with('[') && brackets.ends_with(']') => {
            let inside = brackets.get(1..brackets.len() - 1)?;
            match (unbracketed(inside, "#").next(), unbracketed(inside, ":").next()) {
                (Some(hash), _) => {
                    let count = inside.get(hash + 1..)?.trim().parse::<u64>().ok().filter(|count| *count > 0)?;
                    Type::Array(Box::new(parse_type_within(inside.get(..hash)?, depth + 1)?), count)
                }
                (None, Some(colon)) => {
                    let key = inside.get(..colon)?.trim();
                    let key = if key.is_empty() { Type::Text(None) } else { parse_type_within(key, depth + 1)? };
                    Type::Map(Box::new(key), Box::new(parse_type_within(inside.get(colon + 1..)?, depth + 1)?), None)
                }
                (None, None) => Type::Slice(Box::new(parse_type_within(inside, depth + 1)?), None),
            }
        }
        name if is_name(name) => Type::builtin(name).unwrap_or_else(|| Type::named(name)),
        // `Box<int>` or `Option<text>`: a shape or kind given types.
        applied if applied.ends_with('>') && applied.find('<').is_some_and(|open| applied.get(..open).is_some_and(|name| is_name(name.trim()))) => {
            let open = applied.find('<')?;
            let inside = applied.get(open + 1..applied.len() - 1)?;
            let types: Vec<Type> = split_angled(inside).into_iter().map(|item| parse_type_within(item, depth + 1)).collect::<Option<_>>()?;
            if types.is_empty() {
                return None;
            }
            Type::Applied(Box::new(vela::ast::Applied { name: applied.get(..open)?.trim().to_string(), types, region: None }))
        }
        // Another module's shape or kind: `geometry.Point`.
        // A module the file can't name is written by its path: `shapes/geometry.Point`.
        name if name.rsplit_once(MODULE_SEPARATOR).is_some_and(|(module, member)| is_module_path(module) && is_name(member)) => Type::named(name),
        _ => return None,
    };
    for pointer in pointers.into_iter().rev() {
        ty = if pointer { Type::Ptr(Box::new(ty), None) } else { Type::Maybe(Box::new(ty)) };
    }
    Some(ty)
}

/// A module's name, or its path from the file's folder (`shapes/geometry`, `../common/text`).
fn is_module_path(text: &str) -> bool {
    text.split('/').all(|part| part == ".." || is_name(part))
}

fn is_name(text: &str) -> bool {
    text.starts_with(|c: char| c.is_ascii_alphabetic()) && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The return type in a task's detail, `(n: int) -> int`, or every result of
/// `(a: int, b: int) -> int, int`. A result may be named, `-> total: int`.
pub(crate) fn return_type(detail: &str) -> Type {
    // The results follow the `)` closing the parameters, which may hold task
    // types with arrows of their own.
    let after = match closing_paren(detail) {
        Some(close) => detail.get(close + 1..).unwrap_or(""),
        None => detail,
    };
    let Some(ret) = after.trim_start().strip_prefix("->") else { return Type::Nothing };
    let written: Vec<&str> = split_list(ret).into_iter().map(result_type).collect();
    if let [only] = written.as_slice() {
        return parse_type(only).unwrap_or(Type::Nothing);
    }
    let types: Option<Vec<Type>> = written.into_iter().map(parse_type).collect();
    types.map_or(Type::Nothing, Type::Results)
}

/// One result as written, without the name of a named one: `total: int` is an
/// `int`.
fn result_type(written: &str) -> &str {
    match written.split_once(':') {
        Some((name, ty)) if is_name(name.trim()) => ty,
        _ => written,
    }
}

/// The parameter names in a task's detail, `(a: int, b: int) -> int`.
pub(crate) fn parameter_names(detail: &str) -> Vec<String> {
    parameter_parts(detail).into_iter().map(|(name, _, _)| name).collect()
}

/// The names of the parameters in a task's detail that a call gives: all but
/// the last ones, which fall back on values, `width: int = 8`.
pub(crate) fn needed_parameter_names(detail: &str) -> Vec<String> {
    parameter_parts(detail).into_iter().filter(|(_, _, falls_back)| !falls_back).map(|(name, _, _)| name).collect()
}

/// The parameters in a task's detail, with their types where they parse.
pub(crate) fn parameters(detail: &str) -> Vec<(String, Option<Type>)> {
    // `r: region default` is a region.
    parameter_parts(detail).into_iter().map(|(name, ty, _)| (name, parse_type(without_default(ty)))).collect()
}

/// The parameters in a task's detail: each name, its type as written, and
/// whether it falls back on a value, `= 8`, which no type holds.
fn parameter_parts(detail: &str) -> Vec<(String, &str, bool)> {
    if !detail.starts_with('(') {
        return Vec::new();
    }
    // The parameters end at the `)` closing the first `(`.
    let rest = detail.get(1..).unwrap_or("");
    let inside = rest.get(..list_end(rest).unwrap_or(rest.len())).unwrap_or("");
    split_list(inside)
        .into_iter()
        .filter_map(|param| {
            let (name, ty) = param.split_once(':')?;
            let (ty, falls_back) = match ty.split_once('=') {
                Some((ty, _)) => (ty, true),
                None => (ty, false),
            };
            Some((name.trim().to_string(), ty, falls_back))
        })
        .filter(|(name, _, _)| !name.is_empty())
        .collect()
}

/// A parameter's type as written, `region` for `r: region default`, which the
/// call may leave out.
pub(crate) fn without_default(ty: &str) -> &str {
    match ty.split_whitespace().collect::<Vec<_>>()[..] {
        ["region", word] if word == vela::ast::DEFAULT => "region",
        _ => ty.trim(),
    }
}
