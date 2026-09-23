//! Types of names and expressions, for completion and hover. The checker's
//! types are used whenever it has them. Otherwise, as while the file is being
//! typed, types are read from the declarations: a parameter's written type, a
//! variable's annotation or initializer, a `case` binding's payload field,
//! and so on.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use vela::ast::{ANY_REGION, MODULE_SEPARATOR, Span, Type, unqualified};
use vela::lexer::{LexKind, LexToken, Token};

use crate::TokenClass;
use crate::scan::{Classified, Scan, Symbol, SymbolKind};
use crate::types::Checked;

/// Guards against initializers that refer to each other.
const MAX_DEPTH: usize = 8;

pub(crate) struct Knowledge<'a> {
    pub source: &'a str,
    pub scan: &'a Scan,
    pub checked: &'a Checked,
    /// Indices into `scan.lexed` of the tokens other than comments and `Eof`.
    sig: Vec<usize>,
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
        Knowledge { source, scan, checked, sig }
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
            (SymbolKind::Variable, Some("each")) => self.each_type(id, depth + 1),
            (SymbolKind::Variable, Some("case" | "is")) => self.case_binding_type(id, depth + 1),
            (SymbolKind::Variable, Some("fail")) => self.plan_b_error_type(id, depth + 1),
            (SymbolKind::Variable, _) if !symbol.detail.is_empty() => parse_type(&symbol.detail),
            // Text of no region in particular lasts, as the checker settles it.
            (SymbolKind::Variable, _) => self.initializer_type(symbol, depth).map(|ty| vela::check::lasting(&ty, &self.types_holding(holds_pointers))),
            _ => None,
        }
    }

    /// The type of the value a `bind` or `hold` without a written type gives
    /// `symbol`: `bind x = Point { … }`, `bind ok = a < b`. In a list, the name's
    /// place decides which value it takes.
    fn initializer_type(&self, symbol: &Symbol, depth: usize) -> Option<Type> {
        let name = self.at_byte(symbol.name_range.start)?;
        let (path, names, equals) = self.binding_place(name)?;
        let (&index, inside) = path.split_first()?;
        let end = self.line_end(name).checked_sub(1)?;
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
            // A plan B spanning lines (`{`, or `{ fail e`) gives the value it extracts.
            let names_error = self.token(last.checked_sub(1)?) == Some(&Token::Fail)
                && matches!(self.token(last), Some(Token::Ident(_) | Token::Underscore));
            let brace = if names_error { last.checked_sub(2)? } else { last };
            if self.token(brace) == Some(&Token::LBrace) {
                self.expression_type(brace.checked_sub(1)?, depth + 1)?.extracted().cloned()?
            } else {
                self.span_type(first, last, depth + 1)?
            }
        };
        // `bind a, (b, c) = …`: the item of each tuple the name is in.
        inside.iter().try_fold(value, |value, &item| match value {
            Type::Tuple(items) => items.into_iter().nth(item),
            _ => None,
        })
    }

    /// Where the `bind` or `hold` name at position `name` is in its list, and
    /// the position of the `=` that ends the names.
    /// Where the `bind` or `hold` name at position `name` is: its place in the
    /// list and, inside `( … )`, in each tuple it unpacks; how many names the
    /// list has; and the position of the `=` that ends the names.
    fn binding_place(&self, name: usize) -> Option<(Vec<usize>, usize, usize)> {
        let end = self.line_end(name);
        let equals = (name..end).find(|&q| self.token(q) == Some(&Token::Assign))?;
        let mut path = vec![0usize];
        let mut names = 1;
        for q in self.line_start(name)..equals {
            match self.token(q) {
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
        for q in self.line_start(name)..equals {
            match self.token(q) {
                Some(Token::LParen) => depth += 1,
                Some(Token::RParen) => depth = depth.saturating_sub(1),
                Some(Token::Comma) if depth == 0 => names += 1,
                _ => {}
            }
        }
        Some((path, names, equals))
    }

    /// The first and last position of each value between `first` and `last`,
    /// which commas outside brackets separate.
    fn value_list(&self, first: usize, last: usize) -> Vec<(usize, usize)> {
        let mut values = Vec::new();
        let mut start = first;
        let mut brackets = 0usize;
        for p in first..=last {
            match self.token(p) {
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
        match self.span_type(first, last, depth)? {
            Type::Array(items, _) | Type::Slice(items, _) => Some(*items),
            _ => None,
        }
    }

    /// For the name an `each` declares, the first and last position of the
    /// array or slice it goes through; `None` when it counts through a range.
    pub fn each_items(&self, id: usize) -> Option<(usize, usize)> {
        let symbol = self.symbol(id)?;
        let name = self.at_byte(symbol.name_range.start)?;
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
    /// Whether the pattern name at `name` was written `hold name`, directly or
    /// inside a `hold ( … )`, so it can be assigned.
    pub fn pattern_name_mutable(&self, name: usize) -> bool {
        let line = self.line_start(name);
        let mut at = name;
        loop {
            if at > line && self.token(at - 1) == Some(&Token::Hold) {
                return true;
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
                return false;
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
            self.span_type(self.line_start(case) + 1, case.checked_sub(1)?, depth)
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
        // `hold` before the names makes them changeable; the pattern goes on after it.
        let start = if self.token(case + 1) == Some(&Token::Hold) { case + 2 } else { case + 1 };
        match self.token(start)? {
            // `{v}` and `{(a, b)}`: the value inside, and its items.
            Token::LBrace => self.pattern_item(outcome()?.extracted().cloned()?, start + 1, name),
            // `fail e` and `fail (a, b)`.
            Token::Fail => match outcome()? {
                Type::Or(_, error) => self.pattern_item(*error, start + 1, name),
                _ => None,
            },
            // `(x, y)`: the items of a tuple, which always match.
            Token::LParen => self.pattern_item(outcome()?, start, name),
            _ => {
                let tag_at = self.pattern_variant_at(case);
                // The names before this one, which `hold` may be written before.
                let index = (tag_at + 1..name).filter(|&q| matches!(self.token(q), Some(Token::Ident(_) | Token::Underscore))).count();
                let tag = self.ident(tag_at)?;
                // The variant of the matched value's kind, which may be another
                // module's, whose variants the case names alone.
                let of_subject = outcome().and_then(|subject| self.variant_of(&subject, tag));
                let named = self.symbol_at(tag_at).filter(|&id| self.symbol(id).is_some_and(|s| s.kind == SymbolKind::Variant));
                let variant = of_subject.or(named).or_else(|| self.global(tag, SymbolKind::Variant))?;
                let (_, field) = self.children(variant).nth(index)?;
                let ty = parse_type(&field.detail)?;
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
                Some(Token::When | Token::Loop | Token::Each) if depth > 0 => depth -= 1,
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
        match self.token(q)? {
            // `t.0`: an item of a tuple, which the checker records at its position.
            Token::Int(n) if self.token_before(q) == Some(&Token::Dot) => exact(q).or_else(|| {
                let Type::Tuple(items) = self.expression_type(q.checked_sub(2)?, depth + 1)? else { return None };
                items.get(usize::try_from(*n).ok()?).cloned()
            }),
            Token::Int(_) => Some(Type::Int),
            Token::Float(_) => Some(Type::Float),
            Token::Text(_) => Some(Type::Text(None)),
            Token::True | Token::False => Some(Type::Bool),
            Token::Ident(name) => exact(q).or_else(|| {
                // `palette.Red`: a module's variant, recorded at the module's name.
                if let Some((full, start)) = self.qualified_at(q).filter(|(_, start)| *start != q) {
                    return exact(start).or_else(|| {
                        let variant = self.global(&full, SymbolKind::Variant)?;
                        self.variant_value(variant, None, depth)
                    });
                }
                // `Shape.Circle`: a kind's variant.
                if let Some(variant) = self.symbol_at(q).filter(|&id| self.symbol(id).is_some_and(|s| s.kind == SymbolKind::Variant)) {
                    return self.variant_value(variant, None, depth);
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
                    return match self.expression_type(q - 2, depth + 1)? {
                        Type::Ptr(pointee, _) => Some(Type::Ptr(Box::new(self.in_region(&pointee, name)), Some(name.to_string()))),
                        Type::Slice(items, _) => Some(Type::Slice(Box::new(self.in_region(&items, name)), Some(name.to_string()))),
                        Type::Maybe(line) if matches!(*line, Type::Text(_)) => Some(Type::Maybe(Box::new(Type::Text(Some(name.to_string()))))),
                        // `n.str() at r` and `text(n) at r`.
                        Type::Text(_) => Some(Type::Text(Some(name.to_string()))),
                        _ => None,
                    };
                }
                let id = self.symbol_at(q)?;
                match self.symbol(id)?.kind {
                    SymbolKind::Parameter | SymbolKind::Variable | SymbolKind::Region => self.symbol_type(id, depth + 1),
                    SymbolKind::Variant => self.variant_value(id, None, depth),
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
                // `new T(…)`, `new ^T(…)`, `new@r T(…)`, `new [T](…)`: the type is
                // everything between `new` (and its region) and `(`.
                let type_token = |r: usize| {
                    matches!(
                        self.token(r),
                        Some(Token::Caret | Token::Maybe | Token::New | Token::AtSign | Token::Dot | Token::Ident(_) | Token::LBracket | Token::RBracket | Token::Semicolon | Token::Int(_) | Token::Tilde)
                    )
                };
                let new = open.checked_sub(1).and_then(|before| (0..=before).rev().take_while(|&r| type_token(r)).find(|&r| self.token(r) == Some(&Token::New)));
                if let Some(new) = new.filter(|&new| (new + 1..open).all(|r| self.token(r) != Some(&Token::New))) {
                    return exact(new).or_else(|| {
                        let region = (self.token(new + 1) == Some(&Token::AtSign)).then(|| self.ident(new + 2)).flatten();
                        let from = if region.is_some() { self.range(new + 2).end } else { self.range(new).end };
                        let written = parse_type(self.source.get(from..self.range(open).start)?)?;
                        Some(match (written, region) {
                            // `new [T](…)` makes the slice itself.
                            (Type::Slice(items, _), Some(region)) => Type::Slice(Box::new(self.in_region(&items, region)), Some(region.to_string())),
                            (slice @ Type::Slice(..), None) => slice,
                            (pointee, Some(region)) => Type::Ptr(Box::new(self.in_region(&pointee, region)), Some(region.to_string())),
                            (pointee, None) => Type::Ptr(Box::new(pointee), None),
                        })
                    });
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
                // out, which the checker records at the word.
                if self.token_before(p) == Some(&Token::Dot)
                    && let Some(op) = vela::ast::EditOp::from_word(word)
                    && let Some(Type::Slice(items, _)) = p.checked_sub(2).and_then(|base| self.expression_type(base, depth + 1))
                {
                    return exact(p).or(Some(match op {
                        vela::ast::EditOp::Add => Type::Unit,
                        vela::ast::EditOp::Del => *items,
                    }));
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
                    return exact(start).or_else(|| self.variant_value(variant, Some((open, q)), depth));
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
        if indexes {
            let slicing = self.outside_brackets(first, last).any(|q| self.token(q) == Some(&Token::Range));
            return match (self.expression_type(open - 1, depth + 1)?, slicing) {
                (Type::Array(items, _) | Type::Slice(items, _), false) => Some(*items),
                (Type::Slice(items, region), true) => Some(Type::Slice(items, region)),
                (Type::Array(items, _), true) => Some(Type::Slice(items, None)),
                // A part of text is text where the text is.
                (text @ Type::Text(_), true) => Some(text),
                _ => None,
            };
        }
        // `[value; count]` or `[a, b, c]`.
        if let Some(semicolon) = self.outside_brackets(first, last).find(|&q| self.token(q) == Some(&Token::Semicolon)) {
            let item = self.span_type(first, semicolon.checked_sub(1)?, depth + 1)?;
            let count = match (semicolon + 1 == last, self.token(last)) {
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
        for p in first..=last {
            let token = self.token(p)?;
            match token {
                Token::LParen | Token::LBrace | Token::LBracket => brackets += 1,
                Token::RParen | Token::RBrace | Token::RBracket => brackets = brackets.saturating_sub(1),
                _ if brackets > 0 => {}
                Token::Eq | Token::Ne | Token::Lt | Token::Gt | Token::Le | Token::Ge | Token::And | Token::Or | Token::Not => {
                    return Some(Type::Bool);
                }
                Token::Range => return None,
                Token::Plus | Token::Minus | Token::Star | Token::Slash | Token::Percent => {
                    if !expect_operand {
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
        // to read.
        let mut number: Option<Type> = None;
        for end in operand_ends {
            match self.expression_type(end, depth + 1) {
                Some(ty @ (Type::Int | Type::Float)) if number.as_ref().is_none_or(|seen| *seen == ty) => number = Some(ty),
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

    /// The type a call of the task with signature `detail` gives, where its
    /// parentheses are at positions `open` and `close`. As the checker does, the
    /// task's regions become the caller's: a region parameter's argument, or the
    /// region an argument's type shows, and `_` for one no argument shows.
    fn call_type(&self, detail: &str, open: usize, close: usize, depth: usize) -> Type {
        // Each argument's first and last position.
        let mut args = Vec::new();
        let mut first = open + 1;
        let mut brackets = 0usize;
        for p in open + 1..close {
            match self.token(p) {
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
        let mut regions = HashMap::new();
        for ((first, last), (name, ty)) in args.into_iter().zip(parameters(detail)) {
            match ty {
                _ if first > last => {}
                Some(Type::Region) => {
                    if let (true, Some(region)) = (first == last, self.ident(first)) {
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
        return_type(detail).map_regions(&mut |region| Some(regions.get(region).cloned().unwrap_or_else(|| ANY_REGION.to_string())))
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
        holds_pointers(ty, &takes_region) || holds_text(ty, &takes_region)
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
                vela::check::unify_regions(&vela::check::in_region(&declared, OWN, &takes_region), &actual, &mut regions);
            }
        }
        // A value holding only text that lasts as long as the program names no
        // region, unless it holds pointers.
        match regions.remove(OWN) {
            Some(region) => Type::Named(name.to_string(), Some(region)),
            None if self.types_holding(holds_pointers).contains(name) => Type::Named(name.to_string(), Some(ANY_REGION.to_string())),
            None => Type::named(name),
        }
    }

    /// The kind a variant belongs to.
    pub fn variant_kind(&self, variant: usize) -> Option<String> {
        let kind = self.symbol(variant)?.parent?;
        Some(self.symbol(kind)?.name.clone())
    }

    /// `ty`, a field's type or the type after `new`, in a value of region
    /// `region`, as the checker settles it (see `vela::check::in_region`).
    fn in_region(&self, ty: &Type, region: &str) -> Type {
        vela::check::in_region(ty, region, &self.takes_region())
    }

    /// The shapes and kinds that can be given a region, as the checker finds
    /// them: those whose fields, as written, hold pointers or text (see
    /// `vela::check::regional_types` and `vela::check::textual_types`).
    fn takes_region(&self) -> HashSet<String> {
        let mut takes = self.types_holding(holds_pointers);
        takes.extend(self.types_holding(holds_text));
        takes
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
        Type::Ptr(..) | Type::Slice(..) => true,
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
/// `region`, `[int; 4]`, `[^Node]@r`, or `int ~ ParseError`. Written regions
/// are kept as written.
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
    // `~` gives a type its error side, with or without spaces around it.
    if let Some(at) = unbracketed(text, "~").next() {
        let (value, error) = (text.get(..at)?, text.get(at + "~".len()..)?);
        return Some(Type::Or(Box::new(parse_simple_type(value, depth)?), Box::new(parse_simple_type(error, depth)?)));
    }
    parse_simple_type(text, depth)
}

/// The bytes where `needle` starts in `text` outside brackets and parentheses.
fn unbracketed<'t>(text: &'t str, needle: &'t str) -> impl Iterator<Item = usize> + 't {
    let mut brackets = 0usize;
    text.char_indices().filter_map(move |(i, c)| {
        match c {
            '[' | '(' => brackets += 1,
            ']' | ')' => brackets = brackets.saturating_sub(1),
            _ => {}
        }
        (brackets == 0 && c != ']' && c != ')' && text.get(i..).is_some_and(|rest| rest.starts_with(needle))).then_some(i)
    })
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

/// A type without `or`: after any `^` and `maybe`, a name or brackets, and
/// its region.
fn parse_simple_type(text: &str, depth: usize) -> Option<Type> {
    let text = text.trim();
    let Some(at) = unbracketed(text, "@").last() else { return parse_prefixed_type(text, depth) };
    let (ty, region) = (text.get(..at)?, text.get(at + 1..)?.trim());
    if !is_name(region) {
        return None;
    }
    Some(parse_prefixed_type(ty, depth)?.map_regions_of_names(&mut |_| Some(region.to_string())))
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
        // `(T, U)`.
        parens if parens.starts_with('(') && parens.ends_with(')') => {
            let items: Option<Vec<Type>> = split_list(parens.get(1..parens.len() - 1)?).into_iter().map(|item| parse_type_within(item, depth + 1)).collect();
            Type::Tuple(items.filter(|items| items.len() >= 2)?)
        }
        // `[T; n]` or `[T]`.
        brackets if brackets.starts_with('[') && brackets.ends_with(']') => {
            let inside = brackets.get(1..brackets.len() - 1)?;
            match unbracketed(inside, ";").next() {
                Some(semicolon) => {
                    let count = inside.get(semicolon + 1..)?.trim().parse::<u64>().ok().filter(|count| *count > 0)?;
                    Type::Array(Box::new(parse_type_within(inside.get(..semicolon)?, depth + 1)?), count)
                }
                None => Type::Slice(Box::new(parse_type_within(inside, depth + 1)?), None),
            }
        }
        name if is_name(name) => Type::builtin(name).unwrap_or_else(|| Type::named(name)),
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
    let Some((_, ret)) = detail.rsplit_once("->") else { return Type::Unit };
    let written: Vec<&str> = split_list(ret).into_iter().map(result_type).collect();
    if let [only] = written.as_slice() {
        return parse_type(only).unwrap_or(Type::Unit);
    }
    let types: Option<Vec<Type>> = written.into_iter().map(parse_type).collect();
    types.map_or(Type::Unit, Type::Results)
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
    parameters(detail).into_iter().map(|(name, _)| name).collect()
}

/// The parameters in a task's detail, with their types where they parse.
pub(crate) fn parameters(detail: &str) -> Vec<(String, Option<Type>)> {
    // The parameters end at the `)` closing the first `(`.
    let mut depth = 0usize;
    let close = detail.char_indices().find(|&(_, c)| {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ => {}
        }
        c == ')' && depth == 0
    });
    let inside = close.filter(|_| detail.starts_with('(')).and_then(|(at, _)| detail.get(1..at)).unwrap_or("");
    split_list(inside)
        .into_iter()
        .filter_map(|param| param.split_once(':').map(|(name, ty)| (name.trim().to_string(), parse_type(ty))))
        .filter(|(name, _)| !name.is_empty())
        .collect()
}
