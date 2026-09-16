//! Types of names and expressions, for completion and hover. The checker's
//! types are used whenever it has them. Otherwise, as while the file is being
//! typed, types are read from the declarations: a parameter's written type, a
//! variable's annotation or initializer, a `case` binding's payload field,
//! and so on.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use vela::ast::{ANY_REGION, MODULE_SEPARATOR, Span, Type, unqualified};
use vela::lexer::{LexKind, LexToken, Token};

use crate::scan::{Scan, Symbol, SymbolKind};
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

    /// The symbols that are children of `parent`, in source order: a shape's
    /// fields, a kind's variants, or a variant's payload fields.
    pub fn children(&self, parent: usize) -> impl Iterator<Item = (usize, &'a Symbol)> {
        self.scan.symbols.iter().enumerate().filter(move |(_, s)| s.parent == Some(parent))
    }

    /// The first top-level declaration of `kind` called `name`: one of this
    /// file's, another module's by its qualified name (`geometry.Point`), or one
    /// `use` brings in by name.
    pub fn global(&self, name: &str, kind: SymbolKind) -> Option<usize> {
        let symbols = &self.scan.symbols;
        let qualified = name.contains(MODULE_SEPARATOR);
        symbols
            .iter()
            .position(|s| s.kind == kind && s.name == name && (qualified || s.module.is_none()))
            .or_else(|| self.scan.imports.get(name).copied().filter(|&id| symbols.get(id).is_some_and(|s| s.kind == kind)))
    }

    /// The module used under the name at position `q`, when that name is one.
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
            (SymbolKind::Variable, Some("each")) => Some(Type::Int),
            (SymbolKind::Variable, Some("case" | "is")) => self.case_binding_type(id, depth + 1),
            (SymbolKind::Variable, Some("fail")) => self.plan_b_error_type(id, depth + 1),
            (SymbolKind::Variable, _) if !symbol.detail.is_empty() => parse_type(&symbol.detail),
            (SymbolKind::Variable, _) => {
                // The initializer's type: `bind x = Point { … }`, `bind ok = a < b`.
                // In a list, the name's place decides which value it takes.
                let name = self.at_byte(symbol.name_range.start)?;
                let (index, equals) = self.binding_place(name)?;
                let end = self.line_end(name).checked_sub(1)?;
                if end <= equals {
                    return None;
                }
                let values = self.value_list(equals + 1, end);
                let names = (self.line_start(name)..equals).filter(|&q| self.token(q) == Some(&Token::Comma)).count() + 1;
                if names > 1 && values.len() == 1 {
                    // One call sends every value.
                    let Type::Results(types) = self.expression_type(end, depth + 1)? else { return None };
                    return types.into_iter().nth(index);
                }
                let &(first, last) = values.get(index)?;
                if first > last {
                    return None;
                }
                // A plan B spanning lines (`{`, or `{ fail e`) gives the value it extracts.
                let names_error = self.token(last.checked_sub(1)?) == Some(&Token::Fail)
                    && matches!(self.token(last), Some(Token::Ident(_) | Token::Underscore));
                let brace = if names_error { last.checked_sub(2)? } else { last };
                if self.token(brace) == Some(&Token::LBrace) {
                    return self.expression_type(brace.checked_sub(1)?, depth + 1)?.extracted().cloned();
                }
                self.span_type(first, last, depth + 1)
            }
            _ => None,
        }
    }

    /// Where the `bind` or `hold` name at position `name` is in its list, and
    /// the position of the `=` that ends the names.
    fn binding_place(&self, name: usize) -> Option<(usize, usize)> {
        let end = self.line_end(name);
        let equals = (name..end).find(|&q| self.token(q) == Some(&Token::Assign))?;
        let index = (self.line_start(name)..name).filter(|&q| self.token(q) == Some(&Token::Comma)).count();
        Some((index, equals))
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

    /// The type of a name a `case` or `is` pattern binds: a variant's payload
    /// field, or what `{value}` and `fail e` take from the subject.
    fn case_binding_type(&self, id: usize, depth: usize) -> Option<Type> {
        let symbol = self.symbol(id)?;
        let name = self.at_byte(symbol.name_range.start)?;
        let case = (self.line_start(name)..name).rev().find(|&q| matches!(self.token(q), Some(Token::Case | Token::Is)))?;
        let outcome = || {
            let subject = if self.token(case) == Some(&Token::Is) {
                self.span_type(self.line_start(case) + 1, case.checked_sub(1)?, depth)?
            } else {
                let matched = self.enclosing_match(case)?;
                self.span_type(matched + 1, self.line_end(matched).checked_sub(1)?, depth)?
            };
            Some(subject)
        };
        match self.token(case + 1)? {
            Token::LBrace => outcome()?.extracted().cloned(),
            Token::Fail => match outcome()? {
                Type::Or(_, error) => Some(*error),
                _ => None,
            },
            _ => {
                let index = name.checked_sub(case + 2)?;
                let tag = self.ident(case + 1)?;
                // The variant of the matched value's kind, which may be another
                // module's, whose variants the case names alone.
                let of_subject = match outcome() {
                    Some(Type::Named(kind, _)) => self
                        .global(&kind, SymbolKind::Kind)
                        .and_then(|kind| self.children(kind).find(|(_, v)| unqualified(&v.name) == tag).map(|(id, _)| id)),
                    _ => None,
                };
                let variant = of_subject.or_else(|| self.global(tag, SymbolKind::Variant))?;
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
            Token::Int(_) => Some(Type::Int),
            Token::Float(_) => Some(Type::Float),
            Token::Text(_) => Some(Type::Text),
            Token::True | Token::False => Some(Type::Bool),
            Token::Ident(name) => exact(q).or_else(|| {
                // `palette.Red`: a module's variant, recorded at the module's name.
                if let Some((full, start)) = self.qualified_at(q).filter(|(_, start)| *start != q) {
                    return exact(start).or_else(|| {
                        let variant = self.global(&full, SymbolKind::Variant)?;
                        self.variant_kind(variant).map(|kind| Type::named(&kind))
                    });
                }
                if self.token_before(q) == Some(&Token::Dot) {
                    let Type::Named(shape, region) = self.expression_type(q.checked_sub(2)?, depth + 1)? else { return None };
                    let ty = self.field_type(&shape, name)?;
                    return Some(match region {
                        Some(region) => self.in_region(&ty, &region),
                        None => ty,
                    });
                }
                // `new T(v) at r`: a pointer into `r`.
                if self.token_before(q) == Some(&Token::At) && q >= 2 && self.token(q - 2) == Some(&Token::RParen) {
                    let pointer = self.expression_type(q - 2, depth + 1)?;
                    let Type::Ptr(pointee, _) = pointer else { return None };
                    return Some(Type::Ptr(Box::new(self.in_region(&pointee, name)), Some(name.to_string())));
                }
                let id = self.symbol_at(q)?;
                match self.symbol(id)?.kind {
                    SymbolKind::Parameter | SymbolKind::Variable | SymbolKind::Region => self.symbol_type(id, depth + 1),
                    SymbolKind::Variant => self.variant_kind(id).map(|kind| Type::named(&kind)),
                    _ => None,
                }
            }),
            Token::Caret => exact(q).or_else(|| self.expression_type(q.checked_sub(1)?, depth + 1)?.ptr_inner().cloned()),
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
                Some(Type::named(&shape))
            }
            Token::RParen => {
                let open = self.matching_open(q, &Token::LParen, &Token::RParen)?;
                let Some(callee) = open.checked_sub(1).and_then(|p| self.ident(p).map(|name| (p, name))) else {
                    // A parenthesized expression.
                    return self.span_type(open + 1, q.checked_sub(1)?, depth + 1);
                };
                let (p, _) = callee;
                // A module's task or variant is recorded at the module's name.
                let (name, p) = self.qualified_at(p)?;
                // `new T(…)`, `new ^T(…)`, `new@r T(…)`: the type is everything
                // between `new` (and its region) and `(`.
                let new = (0..p).rev().take_while(|&r| matches!(self.token(r), Some(Token::Caret | Token::Maybe | Token::New | Token::AtSign | Token::Dot | Token::Ident(_)))).find(|&r| self.token(r) == Some(&Token::New));
                if let Some(new) = new.filter(|&new| (new + 1..p).all(|r| self.token(r) != Some(&Token::New))) {
                    return exact(new).or_else(|| {
                        let region = (self.token(new + 1) == Some(&Token::AtSign)).then(|| self.ident(new + 2)).flatten();
                        let from = if region.is_some() { self.range(new + 2).end } else { self.range(new).end };
                        let pointee = parse_type(self.source.get(from..self.range(open).start)?)?;
                        Some(match region {
                            Some(region) => Type::Ptr(Box::new(self.in_region(&pointee, region)), Some(region.to_string())),
                            None => Type::Ptr(Box::new(pointee), None),
                        })
                    });
                }
                exact(p).or_else(|| {
                    if let Some(variant) = self.global(&name, SymbolKind::Variant) {
                        return self.variant_kind(variant).map(|kind| Type::named(&kind));
                    }
                    let task = self.global(&name, SymbolKind::Task)?;
                    Some(self.call_type(&self.symbol(task)?.detail, open, q, depth))
                })
            }
            _ => None,
        }
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
        let mut all_int = true;
        for end in operand_ends {
            match self.expression_type(end, depth + 1) {
                Some(Type::Float) => return Some(Type::Float),
                Some(Type::Int) => {}
                _ => all_int = false,
            }
        }
        all_int.then_some(Type::Int)
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

    /// The position of the bracket that `close` at `q` closes, on the same line.
    fn matching_open(&self, q: usize, open: &Token, close: &Token) -> Option<usize> {
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

    /// The kind a variant belongs to.
    pub fn variant_kind(&self, variant: usize) -> Option<String> {
        let kind = self.symbol(variant)?.parent?;
        Some(self.symbol(kind)?.name.clone())
    }

    /// `ty`, a field's type or the type after `new`, in a value of region
    /// `region`, as the checker settles it (see `vela::check::in_region`).
    fn in_region(&self, ty: &Type, region: &str) -> Type {
        vela::check::in_region(ty, region, &self.regional())
    }

    /// The shapes and kinds whose fields, as written, hold pointers, found as
    /// the checker finds them (see `vela::check::regional_types`).
    fn regional(&self) -> HashSet<String> {
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
            let holds = fields.get(&owner).is_some_and(|types| types.iter().any(|ty| holds_pointers(ty, &regional)));
            if holds && regional.insert(symbol.name.clone()) {
                pending.extend(users.get(&symbol.name).into_iter().flatten().copied());
            }
        }
        regional
    }

    /// The type of a shape's field.
    pub fn field_type(&self, shape: &str, field: &str) -> Option<Type> {
        let shape = self.global(shape, SymbolKind::Shape)?;
        let (_, field) = self.children(shape).find(|(_, s)| s.name == field)?;
        parse_type(&field.detail)
    }
}

/// The shape and kind names a type mentions.
fn named_types(ty: &Type) -> Vec<String> {
    match ty {
        Type::Named(name, _) => vec![name.clone()],
        Type::Ptr(inner, _) | Type::Maybe(inner) => named_types(inner),
        Type::Or(value, error) => named_types(value).into_iter().chain(named_types(error)).collect(),
        _ => Vec::new(),
    }
}

/// Whether a value of type `ty` holds a pointer, given the shapes and kinds in
/// `regional` that do.
fn holds_pointers(ty: &Type, regional: &HashSet<String>) -> bool {
    match ty {
        Type::Ptr(..) => true,
        Type::Named(name, _) => regional.contains(name),
        Type::Maybe(inner) => holds_pointers(inner, regional),
        Type::Or(value, error) => holds_pointers(value, regional) || holds_pointers(error, regional),
        _ => false,
    }
}

/// A type written in source, such as `int`, `^Point@r`, `maybe ^Node@r`,
/// `region`, or `int or ParseError`. Written regions are kept as written.
pub(crate) fn parse_type(text: &str) -> Option<Type> {
    let text = text.trim();
    if let Some((value, error)) = text.split_once(" or ") {
        return Some(Type::Or(Box::new(parse_simple_type(value)?), Box::new(parse_simple_type(error)?)));
    }
    parse_simple_type(text)
}

/// A type without `or`: after any `^` and `maybe`, a name, and its region.
fn parse_simple_type(text: &str) -> Option<Type> {
    let text = text.trim();
    let Some((ty, region)) = text.rsplit_once('@') else { return parse_prefixed_type(text) };
    let region = region.trim();
    if !is_name(region) {
        return None;
    }
    Some(parse_prefixed_type(ty)?.map_regions_of_names(&mut |_| Some(region.to_string())))
}

fn parse_prefixed_type(text: &str) -> Option<Type> {
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
        name if is_name(name) => Type::builtin(name).unwrap_or_else(|| Type::named(name)),
        // Another module's shape or kind: `geometry.Point`.
        name if name.split_once(MODULE_SEPARATOR).is_some_and(|(module, member)| is_name(module) && is_name(member)) => Type::named(name),
        _ => return None,
    };
    for pointer in pointers.into_iter().rev() {
        ty = if pointer { Type::Ptr(Box::new(ty), None) } else { Type::Maybe(Box::new(ty)) };
    }
    Some(ty)
}

fn is_name(text: &str) -> bool {
    text.starts_with(|c: char| c.is_ascii_alphabetic()) && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The return type in a task's detail, `(n: int) -> int`, or every result of
/// `(a: int, b: int) -> int, int`. A result may be named, `-> total: int`.
pub(crate) fn return_type(detail: &str) -> Type {
    let Some((_, ret)) = detail.rsplit_once("->") else { return Type::Unit };
    let written: Vec<&str> = ret.split(',').map(result_type).collect();
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
    let inside = detail.strip_prefix('(').and_then(|rest| rest.split_once(')')).map_or("", |(params, _)| params);
    inside
        .split(',')
        .filter_map(|param| param.split_once(':').map(|(name, ty)| (name.trim().to_string(), parse_type(ty))))
        .filter(|(name, _)| !name.is_empty())
        .collect()
}
