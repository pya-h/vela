//! Types of names and expressions, for completion and hover. The checker's
//! types are used whenever it has them. Otherwise, as while the file is being
//! typed, types are read from the declarations: a parameter's written type, a
//! variable's annotation or initializer, a `case` binding's payload field,
//! and so on.

use std::ops::Range;

use vela::ast::{Span, Type};
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

    /// The first top-level declaration of `kind` called `name`.
    pub fn global(&self, name: &str, kind: SymbolKind) -> Option<usize> {
        self.scan.symbols.iter().position(|s| s.kind == kind && s.name == name)
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
        if let Some(ty) = self.checked.decls.get(&(symbol.start.line, symbol.start.col)) {
            return Some(ty.clone());
        }
        match (symbol.kind, symbol.intro) {
            (SymbolKind::Parameter, _) | (SymbolKind::Field, _) => parse_type(&symbol.detail),
            (SymbolKind::Variable, Some("each")) => Some(Type::Int),
            (SymbolKind::Variable, Some("case" | "is")) => self.case_binding_type(id, depth + 1),
            (SymbolKind::Variable, Some("fail")) => self.plan_b_error_type(id, depth + 1),
            (SymbolKind::Variable, _) if !symbol.detail.is_empty() => parse_type(&symbol.detail),
            (SymbolKind::Variable, _) => {
                // The initializer's type: `bind x = Point { … }`, `bind ok = a < b`.
                let name = self.at_byte(symbol.name_range.start)?;
                if self.token(name + 1) != Some(&Token::Assign) {
                    return None;
                }
                let last = self.line_end(name).checked_sub(1)?;
                if last <= name + 1 {
                    return None;
                }
                if self.token(last) == Some(&Token::LBrace) {
                    // A plan B spanning lines gives the value it extracts.
                    return self.expression_type(last.checked_sub(1)?, depth + 1)?.extracted().cloned();
                }
                self.span_type(name + 2, last, depth + 1)
            }
            _ => None,
        }
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
                let variant = self.global(self.ident(case + 1)?, SymbolKind::Variant)?;
                let (_, field) = self.children(variant).nth(index)?;
                parse_type(&field.detail)
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
                if self.token_before(q) == Some(&Token::Dot) {
                    let Type::Named(shape) = self.expression_type(q.checked_sub(2)?, depth + 1)? else { return None };
                    return self.field_type(&shape, name);
                }
                let id = self.symbol_at(q)?;
                match self.symbol(id)?.kind {
                    SymbolKind::Parameter | SymbolKind::Variable => self.symbol_type(id, depth + 1),
                    SymbolKind::Variant => self.variant_kind(id).map(Type::Named),
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
                    return exact(open).or_else(|| self.expression_type(open.checked_sub(1)?, depth + 1)?.extracted().cloned());
                }
                let shape = self.ident(open.checked_sub(1)?)?;
                Some(Type::Named(shape.to_string()))
            }
            Token::RParen => {
                let open = self.matching_open(q, &Token::LParen, &Token::RParen)?;
                let Some(callee) = open.checked_sub(1).and_then(|p| self.ident(p).map(|name| (p, name))) else {
                    // A parenthesized expression.
                    return self.span_type(open + 1, q.checked_sub(1)?, depth + 1);
                };
                let (p, name) = callee;
                if self.token_before(p) == Some(&Token::New) {
                    let new = p.checked_sub(1)?;
                    return exact(new).or_else(|| {
                        let text = self.source.get(self.range(new).end..self.range(open).start)?;
                        Some(Type::Ptr(Box::new(parse_type(text)?)))
                    });
                }
                if self.token_before(p) == Some(&Token::Caret) {
                    // `new ^T(…)`: the type is everything between `new` and `(`.
                    let new = (0..p).rev().find(|&r| self.token(r) != Some(&Token::Caret))?;
                    if self.token(new) == Some(&Token::New) {
                        return exact(new).or_else(|| {
                            let text = self.source.get(self.range(new).end..self.range(open).start)?;
                            Some(Type::Ptr(Box::new(parse_type(text)?)))
                        });
                    }
                }
                exact(p).or_else(|| {
                    if let Some(variant) = self.global(name, SymbolKind::Variant) {
                        return self.variant_kind(variant).map(Type::Named);
                    }
                    let task = self.global(name, SymbolKind::Task)?;
                    Some(return_type(&self.symbol(task)?.detail))
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

    /// The type of a shape's field.
    pub fn field_type(&self, shape: &str, field: &str) -> Option<Type> {
        let shape = self.global(shape, SymbolKind::Shape)?;
        let (_, field) = self.children(shape).find(|(_, s)| s.name == field)?;
        parse_type(&field.detail)
    }
}

/// A type written in source, such as `int`, `^Point`, `maybe ^Node`, or
/// `int or ParseError`.
pub(crate) fn parse_type(text: &str) -> Option<Type> {
    let text = text.trim();
    if let Some((value, error)) = text.split_once(" or ") {
        return Some(Type::Or(Box::new(parse_simple_type(value)?), Box::new(parse_simple_type(error)?)));
    }
    parse_simple_type(text)
}

/// A type without `or`.
fn parse_simple_type(text: &str) -> Option<Type> {
    let text = text.trim();
    if let Some(inner) = text.strip_prefix('^') {
        return parse_simple_type(inner).map(|ty| Type::Ptr(Box::new(ty)));
    }
    if let Some(inner) = text.strip_prefix("maybe ") {
        return parse_simple_type(inner).map(|ty| Type::Maybe(Box::new(ty)));
    }
    let is_name = text.starts_with(|c: char| c.is_ascii_alphabetic()) && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !is_name {
        return None;
    }
    Some(Type::builtin(text).unwrap_or_else(|| Type::Named(text.to_string())))
}

/// The return type in a task's detail, `(n: int) -> int`.
pub(crate) fn return_type(detail: &str) -> Type {
    match detail.rsplit_once("->") {
        Some((_, ret)) => parse_type(ret).unwrap_or(Type::Unit),
        None => Type::Unit,
    }
}

/// The parameter names in a task's detail, `(a: int, b: int) -> int`.
pub(crate) fn parameter_names(detail: &str) -> Vec<String> {
    let inside = detail.strip_prefix('(').and_then(|rest| rest.split_once(')')).map_or("", |(params, _)| params);
    inside
        .split(',')
        .filter_map(|param| param.split_once(':').map(|(name, _)| name.trim().to_string()))
        .filter(|name| !name.is_empty())
        .collect()
}
