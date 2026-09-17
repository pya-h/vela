use crate::ast::*;
use crate::error::{Error, Result};
use crate::lexer::{KEYWORDS, Token};

/// How deeply code may nest: parentheses, operators in a row, unary
/// operators, `^` in types, and blocks inside blocks each add a level. Every
/// compiler stage walks the tree recursively, so without a limit deeply nested
/// input would overflow the stack and crash (taking Vela Studio's server or
/// its WebAssembly module with it) instead of reporting an error.
pub const MAX_NESTING: usize = 100;

pub struct Parser {
    tokens: Vec<Token>,
    spans: Vec<Span>,
    pos: usize,
    /// Current nesting level; see `MAX_NESTING`.
    depth: usize,
    /// The parts of the written types, for errors about them (see `TypePlaces`).
    type_places: TypePlaces,
    /// The parts of the type being parsed.
    type_parts: Vec<(String, Span)>,
    /// The file's `include` lines, which come before its items.
    includes: Vec<Include>,
}

impl Parser {
    /// `tokens` and `spans` are parallel, as produced by `Lexer::tokenize`.
    pub fn new(tokens: Vec<Token>, spans: Vec<Span>) -> Self {
        Parser { tokens, spans, pos: 0, depth: 0, type_places: TypePlaces::new(), type_parts: Vec::new(), includes: Vec::new() }
    }

    /// The `include` lines of the file parsed.
    pub fn includes(&mut self) -> Vec<Include> {
        std::mem::take(&mut self.includes)
    }

    /// Where the parts of every type parsed so far are.
    pub fn type_places(&mut self) -> TypePlaces {
        std::mem::take(&mut self.type_places)
    }

    /// Parses a whole file: its `include` lines (see `includes`), then its items.
    pub fn parse_file(&mut self) -> Result<Vec<Item>> {
        let mut items = Vec::new();
        self.skip_newlines();
        while self.check(Token::Include) {
            let included = self.parse_include()?;
            self.includes.push(included);
            self.skip_newlines();
        }
        while !self.is_at_end() {
            if self.check(Token::Include) {
                return Err(Error::at(self.peek_span(), "'include' lines go at the top of the file, before any task, shape, or kind"));
            }
            items.push(self.parse_item()?);
            self.skip_newlines();
        }
        Ok(items)
    }

    /// `include path`, `include path => alias`, `include path (part, …)`, or
    /// `include path {part, …}`, where the path is names and `..` separated by
    /// `/`, and a part is a name, maybe followed by `=> alias`.
    fn parse_include(&mut self) -> Result<Include> {
        self.advance(); // include
        let span = self.peek_span();
        if matches!(self.peek(), Token::Newline | Token::Eof) {
            return Err(Error::at(span, "expected a module after 'include', as in 'include geometry'"));
        }
        if let Token::Text(text) = self.peek() {
            let written = text.strip_suffix(".vela").unwrap_or(text).to_string();
            return Err(Error::at(span, format!("a module is named without quotes or '.vela', as in 'include {}'", written)));
        }
        let mut path = Vec::new();
        let name_span = loop {
            let part_span = self.peek_span();
            if self.match_token(Token::Range) {
                path.push("..".to_string());
                if !self.match_token(Token::Slash) {
                    return Err(Error::at(self.peek_span(), "a module path goes on after '..' with '/', as in 'include ../geometry'"));
                }
                continue;
            }
            path.push(self.expect_ident()?);
            if self.check(Token::Dot) {
                if self.tokens.get(self.pos + 1) == Some(&Token::Ident("vela".to_string())) {
                    return Err(Error::at(self.peek_span(), format!("a module is named without '.vela', as in 'include {}'", path.join("/"))));
                }
                return Err(Error::at(self.peek_span(), "a module path separates its folders with '/', as in 'include shapes/circle'"));
            }
            if !self.match_token(Token::Slash) {
                break part_span;
            }
        };
        let alias = self.parse_alias()?;
        let mut parts = Vec::new();
        let only_parts = self.check(Token::LBrace);
        if only_parts && let Some((_, at)) = &alias {
            return Err(Error::at(
                *at,
                "a module included with only its parts ('{…}') has no name in this file, so it can't be given one with '=>'",
            ));
        }
        if self.match_token(Token::LParen) || self.match_token(Token::LBrace) {
            let close = if only_parts { Token::RBrace } else { Token::RParen };
            loop {
                let at = self.peek_span();
                let name = self.expect_ident()?;
                parts.push(Part { name, span: at, alias: self.parse_alias()? });
                if !self.match_token(Token::Comma) {
                    break;
                }
            }
            self.expect(close)?;
        }
        if !matches!(self.peek(), Token::Newline | Token::Eof) {
            return Err(self.unexpected("the end of the 'include' line"));
        }
        Ok(Include { path, span, name_span, alias, parts, only_parts })
    }

    /// `=> name` after an included module or part, if it is there.
    fn parse_alias(&mut self) -> Result<Option<Binding>> {
        if !self.match_token(Token::FatArrow) {
            return Ok(None);
        }
        let at = self.peek_span();
        Ok(Some((self.expect_ident()?, at)))
    }

    /// Whether `name` is the name of a module this file includes.
    fn is_module(&self, name: &str) -> bool {
        self.includes.iter().any(|included| included.local_name() == Some(name))
    }

    /// After a name at the cursor's previous token: `.member` when the name is a
    /// module this file includes, giving `module.member`; otherwise the name alone.
    fn qualified(&mut self, name: String) -> Result<String> {
        if !self.is_module(&name) || !self.check(Token::Dot) {
            return Ok(name);
        }
        self.advance(); // .
        let member = self.expect_ident()?;
        Ok(format!("{}{}{}", name, MODULE_SEPARATOR, member))
    }

    /// `qualified` for a name that starts a value, followed by `.Variant` when a
    /// call follows, giving `Kind.Variant`, as in `Shape.Circle(…)` (or
    /// `geometry.Shape.Circle(…)`): Vela has no methods, so a name, a dot, and a
    /// call is a kind's variant, unless the call is a change to a slice, as in
    /// `xs.add(…)` (which the resolver reads as a variant when the name is a
    /// kind's). (One call from `parse_primary`, whose frame is on the stack once
    /// per nesting level, and grows with each `?` in it.)
    fn value_name(&mut self, name: String) -> Result<String> {
        let name = self.qualified(name)?;
        let calls = matches!(self.tokens.get(self.pos + 1), Some(Token::Ident(member)) if EditOp::from_word(member).is_none())
            && self.tokens.get(self.pos + 2) == Some(&Token::LParen);
        if !self.check(Token::Dot) || !calls {
            return Ok(name);
        }
        self.advance(); // .
        let variant = self.expect_ident()?;
        Ok(format!("{}{}{}", name, MODULE_SEPARATOR, variant))
    }

    fn peek(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or(&Token::Eof)
    }

    fn peek_span(&self) -> Span {
        self.spans
            .get(self.pos)
            .or(self.spans.last())
            .copied()
            .unwrap_or_default()
    }

    fn advance(&mut self) -> Token {
        let tok = self.peek().clone();
        if !self.is_at_end() {
            self.pos += 1;
        }
        tok
    }

    fn is_at_end(&self) -> bool {
        self.pos >= self.tokens.len() || self.peek() == &Token::Eof
    }

    fn skip_newlines(&mut self) {
        while self.peek() == &Token::Newline {
            self.advance();
        }
    }

    /// Enters one nesting level, failing past `MAX_NESTING`. Pair with `leave`.
    fn enter(&mut self) -> Result<()> {
        self.depth += 1;
        if self.depth > MAX_NESTING {
            return Err(Error::at(
                self.peek_span(),
                format!("code is nested too deeply (more than {} levels)", MAX_NESTING),
            ));
        }
        Ok(())
    }

    fn leave(&mut self, levels: usize) {
        self.depth = self.depth.saturating_sub(levels);
    }

    fn unexpected(&self, expected: &str) -> Error {
        if self.check(Token::Send) && !self.at_line_start() {
            return self.misplaced_send();
        }
        if self.check(Token::Is) {
            return Error::at(
                self.peek_span(),
                "'is' can only test the whole condition of a 'when' or 'loop', as in 'when x is {value}'",
            );
        }
        let previous = self.pos.checked_sub(1).and_then(|p| self.tokens.get(p));
        let next = self.tokens.get(self.pos + 1);
        let hint = match (self.peek(), previous, next) {
            (Token::Assign, Some(op @ (Token::Plus | Token::Minus | Token::Star | Token::Slash | Token::Percent)), _) => {
                let op = op.describe();
                let op = op.trim_matches('\'');
                format!("; Vela has no '{0}=': write 'x = x {0} value'", op)
            }
            (Token::Assign, Some(Token::Eq), _) => "; to compare, write '=='".to_string(),
            (Token::Assign, _, _) if expected == "an expression" => "; '=' assigns, as a statement of its own".to_string(),
            (Token::Dot, _, Some(Token::Int(n))) => format!("; a number starts with a digit, as in '0.{}'", n),
            (Token::LBrace, Some(Token::Newline) | None, _) => "; a plan B's '{' goes on the line of its value, and blocks close with 'fi'".to_string(),
            (Token::Semicolon, _, Some(Token::Newline | Token::Eof) | None) => "; a statement ends at the end of its line, without ';'".to_string(),
            _ => String::new(),
        };
        Error::at(self.peek_span(), format!("expected {}, got {}{}", expected, self.peek().describe(), hint))
    }

    /// Whether the next token is the first on its line.
    fn at_line_start(&self) -> bool {
        self.pos == 0 || matches!(self.tokens.get(self.pos - 1), Some(Token::Newline))
    }

    /// The error for a `<-` that doesn't start a line. After something that
    /// can end a value in a condition or statement, it was most likely meant as
    /// `< -`; on the line of a task's header, an `each`, or a `case`, the block's
    /// body was meant to start on the next line.
    fn misplaced_send(&self) -> Error {
        let line_start = (0..self.pos).rev().find(|&p| self.tokens.get(p) == Some(&Token::Newline)).map_or(0, |p| p + 1);
        if let Some(opener @ (Token::Task | Token::Each | Token::Case)) = self.tokens.get(line_start) {
            return Error::at(
                self.peek_span(),
                format!("'<-' must start a line: the body of a '{}' starts on the line after it", opener.describe().trim_matches('\'')),
            );
        }
        let after_value = self.pos > 0
            && matches!(
                self.tokens.get(self.pos - 1),
                Some(
                    Token::Ident(_)
                        | Token::Int(_)
                        | Token::Float(_)
                        | Token::Text(_)
                        | Token::True
                        | Token::False
                        | Token::None
                        | Token::RParen
                        | Token::RBracket
                        | Token::RBrace
                        | Token::Caret
                )
            );
        let message = if after_value {
            "'<-' sends a value and must start a line; to compare with a negative number, write '< -'"
        } else {
            "'<-' sends a value and must start a line"
        };
        Error::at(self.peek_span(), message)
    }

    /// `expect(Token::Fi)` for the block opened by `opener` at `opened`: when the
    /// file ends, or a new task, shape, or kind starts first, the error says which
    /// block is still open.
    fn expect_fi(&mut self, opener: &str, opened: Span) -> Result<()> {
        if self.match_token(Token::Fi) {
            return Ok(());
        }
        if self.is_at_end() || self.at_item_start() || matches!(self.peek(), Token::Else | Token::Case | Token::RBrace) {
            // Directly in a task, an `else` or `case` has no block to belong to.
            let hint = match self.peek() {
                Token::Else if opener == "task" => " (or an 'else' without its 'when')",
                Token::Case if opener == "task" => " (or a 'case' outside a 'match')",
                _ => "",
            };
            return Err(Error::at(
                self.peek_span(),
                format!(
                    "expected 'fi' to close the '{}' at {}:{}, got {}{}",
                    opener,
                    opened.line,
                    opened.col,
                    self.peek().describe(),
                    hint
                ),
            ));
        }
        Err(self.unexpected("'fi'"))
    }

    fn expect(&mut self, expected: Token) -> Result<()> {
        if self.peek() == &expected {
            self.advance();
            Ok(())
        } else {
            Err(self.unexpected(&expected.describe()))
        }
    }

    fn match_token(&mut self, tok: Token) -> bool {
        if self.peek() == &tok {
            self.advance();
            true
        } else {
            false
        }
    }

    fn check(&self, tok: Token) -> bool {
        self.peek() == &tok
    }

    /// Rejects a line holding just `end`, where a block could close. Blocks
    /// close with `fi`; `end` is an ordinary name, but alone on a line it is
    /// never meaningful code, so it is almost certainly the old keyword.
    fn reject_stray_end(&self) -> Result<()> {
        self.reject_stray_end_in(false)
    }

    /// `reject_stray_end` in a block that closes with `}` when `plan_b`.
    fn reject_stray_end_in(&self, plan_b: bool) -> Result<()> {
        let is_end = matches!(self.peek(), Token::Ident(name) if name == "end");
        let next = self.tokens.get(self.pos + 1).unwrap_or(&Token::Eof);
        if is_end && matches!(next, Token::Newline | Token::Eof) {
            let message = if plan_b { "a plan B's block closes with '}', not 'end'" } else { "blocks close with 'fi', not 'end'" };
            return Err(Error::at(self.peek_span(), message));
        }
        Ok(())
    }

    /// Whether the next token starts a top-level item, which can't appear
    /// inside a block (so a block before it is missing its `fi`).
    fn at_item_start(&self) -> bool {
        match self.peek() {
            Token::Task | Token::Shape | Token::Kind | Token::Include => true,
            // `priv` also marks a shape's field.
            Token::Priv => matches!(self.tokens.get(self.pos + 1), Some(Token::Task | Token::Shape | Token::Kind)),
            _ => false,
        }
    }

    /// Whether the next token is `end` followed, after any blank lines, by the
    /// end of the file or a new item.
    fn end_closes_item(&self) -> bool {
        if !matches!(self.peek(), Token::Ident(name) if name == "end") {
            return false;
        }
        let rest = self.tokens.get(self.pos + 1..).unwrap_or_default();
        let next = rest.iter().find(|token| **token != Token::Newline).unwrap_or(&Token::Eof);
        matches!(next, Token::Eof | Token::Task | Token::Shape | Token::Kind | Token::Priv)
    }

    fn parse_item(&mut self) -> Result<Item> {
        self.skip_newlines();
        if self.check(Token::Priv) {
            let span = self.peek_span();
            self.advance();
            let mut item = match self.peek() {
                Token::Task => self.parse_task().map(Item::Task)?,
                Token::Shape => self.parse_shape().map(Item::Shape)?,
                Token::Kind => self.parse_kind().map(Item::Kind)?,
                _ => return Err(Error::at(span, "'priv' goes before a task, shape, or kind, or a shape's field")),
            };
            match &mut item {
                Item::Task(t) => t.private = true,
                Item::Shape(s) => s.private = true,
                Item::Kind(k) => k.private = true,
            }
            return Ok(item);
        }
        match self.peek() {
            Token::Task => self.parse_task().map(Item::Task),
            Token::Shape => self.parse_shape().map(Item::Shape),
            Token::Kind => self.parse_kind().map(Item::Kind),
            Token::Fi => Err(Error::at(self.peek_span(), "expected 'task', 'shape', or 'kind', got 'fi'; is there an extra 'fi' above it?")),
            // `use m` or `import m`, as other languages bring in a file.
            Token::Ident(word)
                if (word == "use" || word == "import")
                    && self.at_line_start()
                    && matches!(self.tokens.get(self.pos + 1), Some(Token::Ident(_) | Token::Range | Token::Text(_))) =>
            {
                Err(Error::at(self.peek_span(), format!("expected 'task', 'shape', or 'kind', got name '{}'; another file is brought in with 'include', as in 'include geometry'", word)))
            }
            token if !matches!(token, Token::Send) || self.at_line_start() => {
                let statement = matches!(
                    token,
                    Token::Ident(_) | Token::Bind | Token::Hold | Token::When | Token::Loop | Token::Each | Token::Match | Token::Send | Token::Region | Token::Else | Token::Case
                );
                let error = self.unexpected("'task', 'shape', or 'kind'");
                if statement {
                    return Err(Error::at(
                        self.peek_span(),
                        format!("{}; statements go inside a task (is there an extra 'fi' above it?)", error.message),
                    ));
                }
                Err(error)
            }
            _ => Err(self.unexpected("'task', 'shape', or 'kind'")),
        }
    }

    fn parse_task(&mut self) -> Result<TaskDef> {
        let opened = self.peek_span();
        self.advance(); // task
        let span = self.peek_span();
        let name = self.expect_ident()?;
        let params = self.parse_field_list()?;
        let mut results = Vec::new();
        let ret = if self.match_token(Token::Arrow) {
            self.parse_results(&mut results)?
        } else {
            Type::Unit
        };
        if self.check(Token::LBrace) {
            return Err(Error::at(self.peek_span(), "a task's body starts on the line after its header, and closes with 'fi'"));
        }
        self.skip_newlines();
        let body = self.parse_block()?;
        self.expect_fi("task", opened)?;
        Ok(TaskDef { name, params, ret, results, body, span, private: false })
    }

    /// The results after a task's `->`: one, or several separated by commas,
    /// each a type or a name and a type (`total: int`). Either all of them are
    /// named or none is.
    fn parse_results(&mut self, results: &mut Vec<ResultDef>) -> Result<Type> {
        let mut types = Vec::new();
        loop {
            if self.check(Token::Newline) || self.check(Token::Fi) || self.is_at_end() {
                let expected = if types.is_empty() { "expected a return type after '->'" } else { "expected another result after ','" };
                return Err(Error::at(self.peek_span(), expected));
            }
            // A name only when a `:` follows it; a result's type is a name too.
            let named = matches!(self.peek(), Token::Ident(_)) && self.tokens.get(self.pos + 1) == Some(&Token::Colon);
            let name = if named {
                let at = self.peek_span();
                let name = self.expect_ident()?;
                self.advance(); // :
                Some((name, at))
            } else {
                None
            };
            if let Some(first) = results.first()
                && first.name.is_some() != name.is_some()
            {
                let (at, which) = match &name {
                    Some((_, at)) => (*at, "has a name"),
                    None => (self.peek_span(), "has no name"),
                };
                return Err(Error::at(
                    at,
                    format!("either every result has a name or none does, and this one {}, as in '-> steps: int, peak: int'", which),
                ));
            }
            let ty_span = self.peek_span();
            results.push(ResultDef { name, ty_span });
            types.push(self.parse_type()?);
            if !self.match_token(Token::Comma) {
                break;
            }
        }
        Ok(if types.len() == 1 {
            types.into_iter().next().unwrap_or(Type::Unit)
        } else {
            Type::Results(types)
        })
    }

    fn parse_shape(&mut self) -> Result<ShapeDef> {
        let opened = self.peek_span();
        self.advance(); // shape
        let span = self.peek_span();
        let name = self.expect_ident()?;
        self.skip_newlines();
        let mut fields = Vec::new();
        while !self.check(Token::Fi) && !self.is_at_end() && !self.at_item_start() {
            self.skip_newlines();
            if self.check(Token::Fi) || self.at_item_start() { break; }
            self.reject_stray_end()?;
            let private = self.match_token(Token::Priv);
            let mut field = self.parse_field()?;
            field.private = private;
            fields.push(field);
            self.skip_newlines();
        }
        self.expect_fi("shape", opened)?;
        Ok(ShapeDef { name, fields, span, private: false })
    }

    fn parse_kind(&mut self) -> Result<KindDef> {
        let opened = self.peek_span();
        self.advance(); // kind
        let span = self.peek_span();
        let name = self.expect_ident()?;
        self.skip_newlines();
        let mut variants = Vec::new();
        while !self.check(Token::Fi) && !self.is_at_end() && !self.at_item_start() {
            self.skip_newlines();
            if self.check(Token::Fi) || self.at_item_start() { break; }
            // A variant may be named `end`; only a stray `end` that ends the
            // kind (followed by the file's end or a new item) is the old keyword.
            if self.end_closes_item() {
                self.reject_stray_end()?;
            }
            let variant_span = self.peek_span();
            if self.check(Token::Priv) {
                return Err(Error::at(variant_span, "a variant can't be 'priv': every variant is as visible as its kind"));
            }
            let tag = self.expect_ident()?;
            let payload = if self.check(Token::LParen) {
                self.parse_field_list()?
            } else {
                Vec::new()
            };
            variants.push(Variant { tag, payload, span: variant_span });
            self.skip_newlines();
        }
        self.expect_fi("kind", opened)?;
        Ok(KindDef { name, variants, span, private: false })
    }

    /// `name: type`
    fn parse_field(&mut self) -> Result<Field> {
        let span = self.peek_span();
        if self.check(Token::Priv) {
            return Err(Error::at(span, "only a shape's field can be 'priv', not a parameter or a variant's value"));
        }
        let name = self.expect_ident()?;
        self.expect(Token::Colon)?;
        let ty_span = self.peek_span();
        let ty = self.parse_type()?;
        Ok(Field { name, ty, span, ty_span, private: false })
    }

    /// `(name: type, ...)`
    fn parse_field_list(&mut self) -> Result<Vec<Field>> {
        self.expect(Token::LParen)?;
        let mut fields = Vec::new();
        if !self.check(Token::RParen) {
            loop {
                fields.push(self.parse_field()?);
                if !self.match_token(Token::Comma) {
                    break;
                }
            }
        }
        self.expect(Token::RParen)?;
        Ok(fields)
    }

    /// A type, with an optional error type: `int`, `^Node@r`, `maybe int`,
    /// `int or ParseError`. `^` and `maybe` bind tighter than `@`, and `@`
    /// tighter than `or`.
    fn parse_type(&mut self) -> Result<Type> {
        let start = self.peek_span();
        self.type_parts.clear();
        let ty = self.parse_type_parts();
        self.type_places.insert(start.place_key(), std::mem::take(&mut self.type_parts));
        ty
    }

    fn parse_type_parts(&mut self) -> Result<Type> {
        let value = self.parse_simple_type()?;
        if !self.match_token(Token::Or) {
            return Ok(value);
        }
        self.type_parts.push(("or".to_string(), self.peek_span()));
        let error = self.parse_simple_type()?;
        if self.check(Token::Or) {
            return Err(Error::at(self.peek_span(), "a type has one error type: 'or' can't appear twice"));
        }
        Ok(Type::Or(Box::new(value), Box::new(error)))
    }

    /// A type without `or`, and the region it lives in: `maybe ^Node@r` is
    /// `(maybe ^Node)@r`, whose pointer and `Node` both live in `r`.
    fn parse_simple_type(&mut self) -> Result<Type> {
        let span = self.peek_span();
        let ty = self.parse_prefixed_type()?;
        if !self.match_token(Token::AtSign) {
            return Ok(ty);
        }
        let region_span = self.peek_span();
        let region = self.expect_ident()?;
        self.type_parts.push((format!("@{}", region), region_span));
        if mentions_region(&ty) {
            return Err(Error::at(span, "a region doesn't live in another region"));
        }
        let mut named = false;
        let ty = ty.map_regions_of_names(&mut |_| {
            named = true;
            Some(region.clone())
        });
        if !named {
            return Err(Error::at(region_span, format!("'{}' holds no pointers or text, so it lives in no region; remove '@{}'", ty, region)));
        }
        Ok(ty)
    }

    /// A type without `or` or `@`: a name, after any `^` and `maybe`, or an
    /// array or slice in brackets.
    fn parse_prefixed_type(&mut self) -> Result<Type> {
        let span = self.peek_span();
        if self.match_token(Token::Region) {
            self.type_parts.push(("region".to_string(), span));
            return Ok(Type::Region);
        }
        if self.match_token(Token::LBracket) {
            self.enter()?;
            let items = self.parse_bracketed_type();
            self.leave(1);
            return items;
        }
        let prefix = if self.match_token(Token::Caret) {
            Some(Token::Caret)
        } else if self.match_token(Token::Maybe) {
            self.type_parts.push(("maybe".to_string(), span));
            Some(Token::Maybe)
        } else {
            None
        };
        if let Some(prefix) = prefix {
            self.enter()?;
            let inner = Box::new(self.parse_prefixed_type()?);
            self.leave(1);
            return Ok(if prefix == Token::Caret { Type::Ptr(inner, None) } else { Type::Maybe(inner) });
        }
        let name = self.expect_ident()?;
        let mut name = self.qualified(name)?;
        // `geometry.Point` for a module this file doesn't include: a dotted name
        // the resolver reports as naming nothing.
        while self.check(Token::Dot) && matches!(self.tokens.get(self.pos + 1), Some(Token::Ident(_))) {
            self.advance(); // .
            name = format!("{}{}{}", name, MODULE_SEPARATOR, self.expect_ident()?);
        }
        self.type_parts.push((name.clone(), span));
        Ok(Type::builtin(&name).unwrap_or(Type::Named(name, None)))
    }

    /// After a type's `[`: `T; count]`, an array, or `T]`, a slice. The item
    /// type may have its own region and error type (`[^int@r; 4]`).
    fn parse_bracketed_type(&mut self) -> Result<Type> {
        let items = Box::new(self.parse_type_parts()?);
        if self.match_token(Token::RBracket) {
            return Ok(Type::Slice(items, None));
        }
        if !self.match_token(Token::Semicolon) {
            return Err(self.unexpected("';' and a count, as in '[int; 4]', or ']' for a slice, as in '[int]@r'"));
        }
        let count_span = self.peek_span();
        let count = match self.peek() {
            Token::Int(n) => *n,
            _ => return Err(Error::at(count_span, "an array's count is a whole number, as in '[int; 4]'")),
        };
        self.advance();
        let Some(count) = u64::try_from(count).ok().filter(|count| *count > 0) else {
            return Err(Error::at(count_span, "an array holds at least one value, so its count is 1 or more"));
        };
        self.expect(Token::RBracket)?;
        Ok(Type::Array(items, count))
    }

    fn parse_block(&mut self) -> Result<Block> {
        self.parse_block_until(&[Token::Fi])
    }

    fn parse_block_until(&mut self, stops: &[Token]) -> Result<Block> {
        self.enter()?;
        let mut stmts = Vec::new();
        loop {
            self.skip_newlines();
            // `else`, `case`, and `}` can't start a statement: they end an
            // enclosing block, so this one is missing its end.
            let ends_outer = matches!(self.peek(), Token::Else | Token::Case | Token::RBrace);
            if self.is_at_end() || self.at_item_start() || ends_outer || stops.iter().any(|t| self.check(t.clone())) {
                break;
            }
            self.reject_stray_end_in(stops.contains(&Token::RBrace))?;
            stmts.push(self.parse_stmt()?);
            self.skip_newlines();
        }
        self.leave(1);
        Ok(stmts)
    }

    fn parse_stmt(&mut self) -> Result<Stmt> {
        self.skip_newlines();
        let span = self.peek_span();
        match self.peek() {
            Token::Bind => self.parse_binding(true),
            Token::Hold => self.parse_binding(false),
            Token::When => self.parse_when(),
            Token::Loop => self.parse_loop(),
            Token::Each => self.parse_each(),
            Token::Match => self.parse_match(),
            Token::Send => self.parse_send(),
            Token::Ident(name) if name == "send" => {
                self.reject_old_send()?;
                self.parse_simple_stmt(span)
            }
            Token::Region => self.parse_region(),
            Token::Priv => Err(Error::at(span, "'priv' goes before a task, shape, or kind, or a shape's field, not a statement")),
            Token::Ident(name) if name == "free" && matches!(self.tokens.get(self.pos + 1), Some(Token::Ident(_) | Token::New)) => Err(
                Error::at(span, "Vela has no 'free': memory in a region is freed when the block declaring the region closes"),
            ),
            _ => self.parse_simple_stmt(span),
        }
    }

    /// An assignment, to one place or to a list of them, a result update, or an
    /// expression used as a statement.
    fn parse_simple_stmt(&mut self, span: Span) -> Result<Stmt> {
        // `name <- value`, or `a, b <- …`: sets results. The checker knows
        // which names are results of this task.
        if self.at_result_update() {
            return self.parse_set_results(span);
        }
        // `_` only takes a value, so a line starting with it is an assignment.
        if self.check(Token::Underscore) {
            let first = self.parse_target()?;
            return self.parse_assignment(span, first);
        }
        let expr = self.parse_expr()?;
        if self.check(Token::Comma) || self.check(Token::Assign) {
            return self.parse_assignment(span, Target::Place(expr));
        }
        Ok(Stmt::new(StmtKind::Expr(expr), span))
    }

    /// The rest of an assignment whose first place is `first`: any more places,
    /// then `= values`.
    fn parse_assignment(&mut self, span: Span, first: Target) -> Result<Stmt> {
        let mut targets = vec![first];
        while self.match_token(Token::Comma) {
            targets.push(self.parse_target()?);
        }
        if !self.match_token(Token::Assign) {
            return Err(Error::at(
                self.peek_span(),
                "expected '=': a list of names takes the values of an assignment, as in 'a, b = divmod(17, 5)'",
            ));
        }
        let values = self.parse_value_list()?;
        Ok(Stmt::new(StmtKind::Assign(targets, values), span))
    }

    /// Whether the statement at the cursor is `name(, name)* <-`, which sets
    /// results. A `<-` anywhere else in a line is still an error.
    fn at_result_update(&self) -> bool {
        let mut p = self.pos;
        loop {
            if !matches!(self.tokens.get(p), Some(Token::Ident(_))) {
                return false;
            }
            match self.tokens.get(p + 1) {
                Some(Token::Send) => return true,
                Some(Token::Comma) => p += 2,
                _ => return false,
            }
        }
    }

    /// `name(, name)* <- values`: sets the named results.
    fn parse_set_results(&mut self, span: Span) -> Result<Stmt> {
        let mut names = Vec::new();
        loop {
            let at = self.peek_span();
            names.push((self.expect_ident()?, at));
            if !self.match_token(Token::Comma) {
                break;
            }
        }
        self.expect(Token::Send)?;
        if self.check(Token::Newline) || self.is_at_end() {
            return Err(Error::at(self.peek_span(), "expected the value to set the result to, after '<-'"));
        }
        let values = self.parse_value_list()?;
        Ok(Stmt::new(StmtKind::SetResults(names, values), span))
    }

    /// One place an assignment writes to, or `_`.
    fn parse_target(&mut self) -> Result<Target> {
        let span = self.peek_span();
        if self.match_token(Token::Underscore) {
            return Ok(Target::Discard(span));
        }
        Ok(Target::Place(self.parse_expr()?))
    }

    /// One value, or several separated by commas.
    fn parse_value_list(&mut self) -> Result<Vec<Expr>> {
        let mut values = vec![self.parse_expr()?];
        while self.match_token(Token::Comma) {
            values.push(self.parse_expr()?);
        }
        Ok(values)
    }

    /// Rejects `send` used as the old keyword: alone on its line, or followed
    /// by a value on the same line. `send` is an ordinary name, but neither is
    /// meaningful code with it (a value can't follow a name), so it is almost
    /// certainly a send written the old way.
    fn reject_old_send(&self) -> Result<()> {
        let next = self.tokens.get(self.pos + 1).unwrap_or(&Token::Eof);
        let old_keyword = matches!(
            next,
            Token::Newline
                | Token::Eof
                | Token::Fi
                | Token::Int(_)
                | Token::Float(_)
                | Token::Text(_)
                | Token::Ident(_)
                | Token::True
                | Token::False
                | Token::None
                | Token::New
                | Token::Not
                | Token::Minus
        );
        if old_keyword {
            return Err(Error::at(self.peek_span(), "a task sends its result with '<-', not 'send'"));
        }
        Ok(())
    }

    /// `bind name [: type] [, name [: type]]* = values`, or the same with
    /// `hold`. A name may be `_`, which drops its value.
    fn parse_binding(&mut self, immutable: bool) -> Result<Stmt> {
        self.advance(); // bind / hold
        let span = self.peek_span();
        let mut names = Vec::new();
        loop {
            let (name, name_span) = self.parse_bound_name()?;
            let (ty, ty_span) = if self.match_token(Token::Colon) {
                let ty_span = self.peek_span();
                (Some(self.parse_type()?), Some(ty_span))
            } else {
                (None, None)
            };
            names.push(Bound { name, span: name_span, ty, ty_span });
            if !self.match_token(Token::Comma) {
                break;
            }
        }
        self.expect(Token::Assign)?;
        let init = self.parse_value_list()?;
        let kind = StmtKind::Binding { names, mutable: !immutable, init };
        Ok(Stmt::new(kind, span))
    }

    fn parse_when(&mut self) -> Result<Stmt> {
        let span = self.peek_span();
        self.advance(); // when
        let cond = self.parse_condition()?;
        self.skip_newlines();
        let then_block = self.parse_block_until(&[Token::Else, Token::Fi])?;
        let else_block = if self.match_token(Token::Else) {
            if self.check(Token::When) || matches!(self.peek(), Token::Ident(name) if name == "if") {
                return Err(Error::at(self.peek_span(), "Vela has no 'else when'; put a 'when' inside the 'else', on its own line"));
            }
            self.skip_newlines();
            Some(self.parse_block_until(&[Token::Fi])?)
        } else {
            None
        };
        self.expect_fi("when", span)?;
        Ok(Stmt::new(StmtKind::When(cond, then_block, else_block), span))
    }

    fn parse_loop(&mut self) -> Result<Stmt> {
        let span = self.peek_span();
        self.advance(); // loop
        let cond = self.parse_condition()?;
        self.skip_newlines();
        let body = self.parse_block()?;
        self.expect_fi("loop", span)?;
        Ok(Stmt::new(StmtKind::Loop(cond, body), span))
    }

    fn parse_each(&mut self) -> Result<Stmt> {
        let opened = self.peek_span();
        self.advance(); // each
        let span = self.peek_span();
        let name = self.expect_ident()?;
        self.expect(Token::In)?;
        let range = self.parse_expr()?;
        self.skip_newlines();
        let body = self.parse_block()?;
        self.expect_fi("each", opened)?;
        Ok(Stmt::new(StmtKind::Each(name, range, body), span))
    }

    fn parse_match(&mut self) -> Result<Stmt> {
        let span = self.peek_span();
        self.advance(); // match
        let expr = self.parse_expr()?;
        self.skip_newlines();
        let mut cases = Vec::new();
        while !self.check(Token::Fi) && !self.is_at_end() && !self.at_item_start() {
            self.skip_newlines();
            if self.check(Token::Fi) || self.at_item_start() { break; }
            self.reject_stray_end()?;
            self.expect(Token::Case)?;
            let pattern = self.parse_pattern("case")?;
            if !self.check(Token::Newline) && !self.check(Token::Fi) && !self.check(Token::Case) && !self.is_at_end() {
                if self.check(Token::Send) {
                    return Err(self.misplaced_send());
                }
                return Err(match pattern.kind {
                    PatternKind::Variant { .. } => self.name_expected(),
                    _ => self.unexpected("the end of the line"),
                });
            }
            self.skip_newlines();
            let body = self.parse_block_until(&[Token::Case, Token::Fi])?;
            cases.push(Case { pattern, body });
        }
        self.expect_fi("match", span)?;
        Ok(Stmt::new(StmtKind::Match(expr, cases), span))
    }

    /// What a `case` or `is` (the `keyword`) matches: `Circle r`, `{value}`,
    /// `none`, `fail e`, or `_`.
    fn parse_pattern(&mut self, keyword: &str) -> Result<Pattern> {
        let span = self.peek_span();
        let kind = match self.peek() {
            Token::Underscore => {
                self.advance();
                PatternKind::Any
            }
            Token::None => {
                self.advance();
                PatternKind::None
            }
            Token::LBrace => {
                self.advance();
                let bind = self.parse_bound_name()?;
                self.expect(Token::RBrace)?;
                PatternKind::Value(bind)
            }
            Token::Fail => {
                self.advance();
                PatternKind::Fail(self.parse_bound_name()?)
            }
            Token::Ident(_) => {
                // `Circle`, `Shape.Circle`, or `geometry.Shape.Circle`.
                let mut written = vec![self.expect_ident()?];
                while self.check(Token::Dot) && matches!(self.tokens.get(self.pos + 1), Some(Token::Ident(_))) {
                    self.advance(); // .
                    written.push(self.expect_ident()?);
                }
                let module_first = written.first().is_some_and(|first| self.is_module(first));
                let tag = written.pop().unwrap_or_default();
                // A kind after a name that isn't a module this file includes is
                // left for the resolver, which says what the name is instead.
                if written.len() > 2 || (written.len() == 1 && module_first) {
                    return Err(Error::at(
                        span,
                        format!(
                            "a pattern names the variant alone or after its kind, as in '{0} {1}' or '{0} Kind.{1}': the value's kind tells which module it is from",
                            keyword, tag
                        ),
                    ));
                }
                let kind = (!written.is_empty()).then(|| Box::new((written.join(&MODULE_SEPARATOR.to_string()), span)));
                if self.check(Token::LParen) {
                    return Err(Error::at(
                        self.peek_span(),
                        format!("a pattern names its bindings without parentheses, as in '{} {} value'", keyword, tag),
                    ));
                }
                let mut binds = Vec::new();
                while matches!(self.peek(), Token::Ident(_) | Token::Underscore) {
                    binds.push(self.parse_bound_name()?);
                }
                PatternKind::Variant { kind, tag, binds }
            }
            _ => return Err(self.unexpected("a variant, '{value}', 'none', 'fail e', or '_'")),
        };
        Ok(Pattern { kind, span })
    }

    /// A name a pattern binds, or `_`.
    fn parse_bound_name(&mut self) -> Result<Binding> {
        let span = self.peek_span();
        if self.match_token(Token::Underscore) {
            return Ok((DISCARD.to_string(), span));
        }
        Ok((self.expect_ident()?, span))
    }

    /// `<-`, `<- value`, or `<- a, b`. A send starts its line (or a plan B's block, as in
    /// `x { <- 0 }`): `<-` after something else is almost always a comparison
    /// with a negative number written without a space (`a <-1`), which would
    /// otherwise end the statement and send.
    fn parse_send(&mut self) -> Result<Stmt> {
        let span = self.peek_span();
        let starts_line = self.pos == 0 || matches!(self.tokens.get(self.pos - 1), Some(Token::Newline | Token::LBrace));
        if !starts_line {
            return Err(self.misplaced_send());
        }
        self.advance(); // <-
        let ends = [Token::Newline, Token::Fi, Token::Else, Token::Case, Token::RBrace];
        if ends.iter().any(|token| self.check(token.clone())) || self.is_at_end() {
            Ok(Stmt::new(StmtKind::Send(Vec::new()), span))
        } else {
            let values = self.parse_value_list()?;
            Ok(Stmt::new(StmtKind::Send(values), span))
        }
    }

    /// `region name`
    fn parse_region(&mut self) -> Result<Stmt> {
        self.advance(); // region
        let span = self.peek_span();
        let name = self.expect_ident()?;
        Ok(Stmt::new(StmtKind::Region(name), span))
    }

    fn parse_expr(&mut self) -> Result<Expr> {
        self.enter()?;
        let expr = self.parse_binary(1)?;
        self.leave(1);
        Ok(expr)
    }

    /// A `when` or `loop` condition, `value` or `value is pattern`,
    /// explaining `=` written for `==`.
    fn parse_condition(&mut self) -> Result<Condition> {
        let value = self.parse_expr()?;
        if self.check(Token::Assign) {
            return Err(Error::at(self.peek_span(), "'=' assigns; to compare, write '=='"));
        }
        if !self.match_token(Token::Is) {
            return Ok(Condition { value, pattern: None });
        }
        let pattern = self.parse_pattern("is")?;
        if self.check(Token::And) || self.check(Token::Or) {
            return Err(Error::at(
                self.peek_span(),
                "an 'is' condition can't be combined with 'and' or 'or'; put a 'when' inside it instead",
            ));
        }
        Ok(Condition { value, pattern: Some(pattern) })
    }

    /// The binary operator `token` stands for, with its precedence (higher
    /// binds tighter). `..` is handled apart from these, as it doesn't chain.
    fn binary_op(token: &Token) -> Option<(BinaryOp, u8)> {
        Some(match token {
            Token::Or => (BinaryOp::Or, 1),
            Token::And => (BinaryOp::And, 2),
            Token::Eq => (BinaryOp::Eq, 3),
            Token::Ne => (BinaryOp::Ne, 3),
            Token::Lt => (BinaryOp::Lt, 4),
            Token::Gt => (BinaryOp::Gt, 4),
            Token::Le => (BinaryOp::Le, 4),
            Token::Ge => (BinaryOp::Ge, 4),
            Token::Plus => (BinaryOp::Add, 6),
            Token::Minus => (BinaryOp::Sub, 6),
            Token::Star => (BinaryOp::Mul, 7),
            Token::Slash => (BinaryOp::Div, 7),
            Token::Percent => (BinaryOp::Mod, 7),
            _ => return None,
        })
    }

    /// Precedence of `..`: between comparisons and `+`/`-`.
    const RANGE_PRECEDENCE: u8 = 5;

    /// Parses operators of precedence `min` or higher by precedence climbing,
    /// left-associative, with each node's span at its operator. `a..b` takes one
    /// `..` (`a..b..c` is not a range). Climbing keeps the recursion per
    /// parenthesis shallow, which matters for `MAX_NESTING`.
    fn parse_binary(&mut self, min: u8) -> Result<Expr> {
        let mut left = self.parse_unary()?;
        // Each operator deepens the tree by one level, like a parenthesis.
        let mut levels = 0;
        let mut ranged = false;
        loop {
            let span = self.peek_span();
            if self.check(Token::Range) && min <= Self::RANGE_PRECEDENCE && !ranged {
                self.enter()?;
                levels += 1;
                self.advance();
                let right = self.parse_binary(Self::RANGE_PRECEDENCE + 1)?;
                left = Expr::new(ExprKind::Range(Box::new(left), Box::new(right)), span);
                ranged = true;
                continue;
            }
            let Some((op, precedence)) = Self::binary_op(self.peek()) else { break };
            if precedence < min {
                break;
            }
            // A range is an operand of comparisons only (`x < 0..5` is left as before).
            if ranged && precedence > Self::RANGE_PRECEDENCE {
                break;
            }
            self.enter()?;
            levels += 1;
            self.advance();
            let right = self.parse_binary(precedence + 1)?;
            left = Expr::new(ExprKind::Binary(op, Box::new(left), Box::new(right)), span);
        }
        self.leave(levels);
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr> {
        let span = self.peek_span();
        if self.match_token(Token::Fail) {
            self.enter()?;
            let error = self.parse_unary()?;
            self.leave(1);
            return Ok(Expr::new(ExprKind::Fail(Box::new(error)), span));
        }
        let op = if self.match_token(Token::Minus) {
            UnaryOp::Neg
        } else if self.match_token(Token::Not) {
            UnaryOp::Not
        } else {
            return self.parse_postfix();
        };
        self.enter()?;
        let expr = self.parse_unary()?;
        self.leave(1);
        Ok(Expr::new(ExprKind::Unary(op, Box::new(expr)), span))
    }

    fn parse_postfix(&mut self) -> Result<Expr> {
        let mut expr = self.parse_primary()?;
        let mut levels = 0;
        while matches!(self.peek(), Token::Dot | Token::LBracket | Token::Caret | Token::LBrace) {
            self.enter()?;
            levels += 1;
            expr = self.parse_suffix(expr)?;
        }
        self.leave(levels);
        Ok(expr)
    }

    /// `.field`, `[index]`, `^`, or a plan B after `expr`, with the cursor at
    /// its first token. (A function of its own: `parse_postfix`'s frame is on
    /// the stack once per nesting level of every expression.)
    fn parse_suffix(&mut self, expr: Expr) -> Result<Expr> {
        let span = self.peek_span();
        match self.peek() {
            Token::LBrace => self.parse_plan_b(expr),
            Token::Dot => {
                self.advance();
                let span = self.peek_span();
                let field = self.expect_ident()?;
                let expr = Expr::new(ExprKind::Field(Box::new(expr), field), span);
                if self.check(Token::LParen) || (self.check(Token::LBrace) && self.at_shape_literal()) {
                    return self.dotted_name_use(expr);
                }
                Ok(expr)
            }
            Token::LBracket => {
                self.advance();
                let index = self.parse_expr()?;
                self.expect(Token::RBracket)?;
                Ok(Expr::new(ExprKind::Index(Box::new(expr), Box::new(index)), span))
            }
            _ => {
                self.advance(); // ^
                Ok(Expr::new(ExprKind::Deref(Box::new(expr)), span))
            }
        }
    }

    fn parse_primary(&mut self) -> Result<Expr> {
        let span = self.peek_span();
        let kind = match self.peek() {
            Token::Int(n) => { let v = *n; self.advance(); ExprKind::IntLit(v) }
            Token::Float(n) => { let v = *n; self.advance(); ExprKind::FloatLit(v) }
            Token::Text(s) => { let v = s.clone(); self.advance(); ExprKind::TextLit(v) }
            Token::True => { self.advance(); ExprKind::BoolLit(true) }
            Token::False => { self.advance(); ExprKind::BoolLit(false) }
            Token::None => { self.advance(); ExprKind::NoneLit }
            Token::New => return self.parse_new(),
            Token::LBracket => return self.parse_array(),
            Token::LParen => {
                self.advance();
                let expr = self.parse_expr()?;
                self.expect(Token::RParen)?;
                return Ok(expr);
            }
            Token::Ident(name) => {
                let name = name.clone();
                self.advance();
                let name = self.value_name(name)?;
                if self.check(Token::LBrace) && self.at_shape_literal() {
                    self.parse_struct_init(name)?
                } else if self.check(Token::LParen) {
                    let args = self.parse_arg_list()?;
                    ExprKind::Call(name, args)
                } else {
                    ExprKind::Var(name)
                }
            }
            _ => return Err(self.unexpected("an expression")),
        };
        Ok(Expr::new(kind, span))
    }

    /// `[a, b, c]` or `[value; count]`, on one line.
    fn parse_array(&mut self) -> Result<Expr> {
        // An array holds its values in a frame of its own, and takes more stack
        // than most expressions, so it counts as a level of its own.
        self.enter()?;
        let result = self.parse_array_inner();
        self.leave(1);
        result
    }

    fn parse_array_inner(&mut self) -> Result<Expr> {
        let span = self.peek_span();
        self.advance(); // [
        if self.check(Token::RBracket) {
            return Err(Error::at(span, "an array holds at least one value, as in '[0; 4]'"));
        }
        let first = self.parse_expr()?;
        if self.match_token(Token::Semicolon) {
            let count = self.parse_expr()?;
            self.expect_array_end(span)?;
            return Ok(Expr::new(ExprKind::Repeat { value: Box::new(first), count: Box::new(count) }, span));
        }
        let mut values = vec![first];
        while self.match_token(Token::Comma) {
            if self.check(Token::Newline) {
                return Err(Error::at(self.peek_span(), "an array is written on one line, as in '[1, 2, 3]'"));
            }
            values.push(self.parse_expr()?);
        }
        self.expect_array_end(span)?;
        Ok(Expr::new(ExprKind::Array(values), span))
    }

    /// The `]` closing the array opened at `opened`.
    fn expect_array_end(&mut self, opened: Span) -> Result<()> {
        if self.match_token(Token::RBracket) {
            return Ok(());
        }
        if matches!(self.peek(), Token::Newline | Token::Eof) {
            return Err(Error::at(self.peek_span(), format!("expected ']' to close the array at {}:{}; an array is written on one line", opened.line, opened.col)));
        }
        Err(self.unexpected("']'"))
    }

    /// A call or shape literal after a field, as in `p.x.f()` or
    /// `geometry.Point { x: 1 }` where `geometry` isn't a module this file
    /// includes (`value_name` joins the names that are): one dotted name, which
    /// the resolver reports as naming nothing. Vela has no methods, so a call
    /// on any other value is an error here, except a change to a slice,
    /// `xs.add(…)` or `xs.del(…)`. (A function of its own: `parse_postfix`'s
    /// frame is on the stack once per nesting level.)
    fn dotted_name_use(&mut self, expr: Expr) -> Result<Expr> {
        if let ExprKind::Field(_, word) = &expr.kind
            && let Some(op) = EditOp::from_word(word)
            && self.check(Token::LParen)
        {
            let args = self.parse_arg_list()?;
            let ExprKind::Field(items, _) = expr.kind else { return Err(Error::internal_at(expr.span, "a change without its slice")) };
            return Ok(Expr::new(ExprKind::Edit { items, op, args }, expr.span));
        }
        let Some((name, span)) = dotted_name(&expr) else {
            if self.check(Token::LParen) {
                let hint = match &expr.kind {
                    ExprKind::Field(_, word) if matches!(word.as_str(), "push" | "append" | "insert") => {
                        "; a slice adds an item with 'add', as in 'xs.add(v)' or 'xs.add(i, v)'"
                    }
                    ExprKind::Field(_, word) if matches!(word.as_str(), "pop" | "remove" | "delete") => {
                        "; a slice takes an item out with 'del', as in 'xs.del(i)' or 'xs.del()'"
                    }
                    _ => "; pass the value to a task, as in 'f(value)'",
                };
                return Err(Error::at(expr.span, format!("only a task or a variant can be called, and Vela has no methods{}", hint)));
            }
            return Ok(expr);
        };
        let kind = if self.check(Token::LParen) { ExprKind::Call(name, self.parse_arg_list()?) } else { self.parse_struct_init(name)? };
        Ok(Expr::new(kind, span))
    }

    /// Whether the `{` at the cursor, after a name, opens a shape literal
    /// (`Point { x: 1 }` or `Marker {}`) rather than a plan B (`x { 0 }`).
    fn at_shape_literal(&self) -> bool {
        let after = |n: usize| self.tokens.get(self.pos + n).unwrap_or(&Token::Eof);
        matches!(after(1), Token::RBrace) || (matches!(after(1), Token::Ident(_)) && matches!(after(2), Token::Colon))
    }

    /// `value { … }`, with the cursor at `{`: a plan B, whose block may start
    /// with `fail e` to name the error.
    fn parse_plan_b(&mut self, value: Expr) -> Result<Expr> {
        let opened = self.peek_span();
        self.advance(); // {
        let same_line = !self.check(Token::Newline);
        self.skip_newlines();
        let next = self.tokens.get(self.pos + 1).unwrap_or(&Token::Eof);
        // `Point {` with its fields on the next line: a line never starts with `name:`.
        if !same_line && matches!(self.peek(), Token::Ident(_)) && matches!(next, Token::Colon) {
            let example = dotted_name(&value).map_or_else(|| "Point { ".to_string(), |(name, _)| format!("{} {{ ", name));
            return Err(Error::at(
                self.peek_span(),
                format!("a shape literal is written on one line, starting after its '{{', as in '{}x: 1 }}'", example),
            ));
        }
        let error = if self.check(Token::Fail) && matches!(next, Token::Ident(_) | Token::Underscore) {
            self.advance(); // fail
            Some(self.parse_bound_name()?)
        } else {
            None
        };
        let body = self.parse_block_until(&[Token::RBrace, Token::Fi, Token::Else, Token::Case])?;
        if !self.match_token(Token::RBrace) {
            return Err(Error::at(
                self.peek_span(),
                format!(
                    "expected '}}' to close the plan B at {}:{}, got {}",
                    opened.line,
                    opened.col,
                    self.peek().describe()
                ),
            ));
        }
        Ok(Expr::new(ExprKind::PlanB { value: Box::new(value), error, body }, opened))
    }

    /// `new T(value) at r` or `new@r T(value)`.
    fn parse_new(&mut self) -> Result<Expr> {
        // A `new` holds a type and a value, and takes more stack than most
        // expressions, so it counts as a level of its own.
        self.enter()?;
        let result = self.parse_new_inner();
        self.leave(1);
        result
    }

    fn parse_new_inner(&mut self) -> Result<Expr> {
        let span = self.peek_span();
        self.advance(); // new
        let before = if self.match_token(Token::AtSign) {
            let region_span = self.peek_span();
            let region = self.expect_ident()?;
            if self.check(Token::LParen) {
                return Err(Error::at(region_span, format!("after 'new@', write the region and then the type, as in 'new@r {}(…)'", region)));
            }
            Some((region, region_span))
        } else {
            None
        };
        let ty_span = self.peek_span();
        let ty = self.parse_type()?;
        self.expect(Token::LParen)?;
        // `new [T]() at r` makes an empty slice; its value is an array of no
        // values, which no literal can write.
        let value = if self.check(Token::RParen) && matches!(ty, Type::Slice(..)) {
            Expr::new(ExprKind::Array(Vec::new()), self.peek_span())
        } else if self.check(Token::RParen) {
            return Err(Error::at(
                self.peek_span(),
                format!("'new' needs the value to put in the region, as in 'new {}(…) at r'; only a slice can be made empty, as in 'new [int]() at r'", ty),
            ));
        } else {
            self.parse_expr()?
        };
        self.expect(Token::RParen)?;
        if self.check(Token::In) {
            return Err(Error::at(self.peek_span(), "a 'new' names its region with 'at', as in 'new T(v) at r' (or 'new@r T(v)'); 'in' counts through a range"));
        }
        let after = if self.check(Token::At) {
            let at_span = self.peek_span();
            self.advance();
            if before.is_some() {
                return Err(Error::at(at_span, "the region is already named after 'new@'; write one of 'new T(v) at r' or 'new@r T(v)'"));
            }
            let region_span = self.peek_span();
            let region = self.expect_ident()?;
            Some((region, region_span))
        } else {
            None
        };
        let Some((region, region_span)) = before.or(after) else {
            return Err(Error::at(
                self.peek_span(),
                format!("'new' needs a region to allocate in, as in 'new {}(…) at r' or 'new@r {}(…)'", ty, ty),
            ));
        };
        Ok(Expr::new(ExprKind::New { ty: Box::new(ty), ty_span, value: Box::new(value), region, region_span }, span))
    }

    fn parse_struct_init(&mut self, name: String) -> Result<ExprKind> {
        let shape_name = name.clone();
        self.advance(); // {
        let mut fields = Vec::new();
        if !self.check(Token::RBrace) {
            loop {
                let span = self.peek_span();
                let name = self.expect_ident()?;
                self.expect(Token::Colon)?;
                let value = self.parse_expr()?;
                fields.push(FieldInit { name, span, value });
                if !self.match_token(Token::Comma) {
                    break;
                }
                if self.check(Token::Newline) {
                    return Err(Error::at(
                        self.peek_span(),
                        format!("a shape literal is written on one line, as in '{} {{ x: 1, y: 2 }}'", shape_name),
                    ));
                }
            }
        }
        self.expect(Token::RBrace)?;
        Ok(ExprKind::StructInit(name, fields))
    }

    fn parse_arg_list(&mut self) -> Result<Vec<Expr>> {
        self.expect(Token::LParen)?;
        let mut args = Vec::new();
        if !self.check(Token::RParen) {
            loop {
                args.push(self.parse_expr()?);
                if !self.match_token(Token::Comma) {
                    break;
                }
            }
        }
        self.expect(Token::RParen)?;
        Ok(args)
    }

    fn expect_ident(&mut self) -> Result<String> {
        match self.peek() {
            Token::Ident(s) => { let v = s.clone(); self.advance(); Ok(v) }
            _ => Err(self.name_expected()),
        }
    }

    /// The error for a missing name at the cursor, explaining reserved words.
    fn name_expected(&self) -> Error {
        match KEYWORDS.iter().find(|(_, keyword)| keyword == self.peek()) {
            Some((word, _)) => Error::at(
                self.peek_span(),
                format!("expected a name, got '{}', which is a reserved word", word),
            ),
            None => self.unexpected("a name"),
        }
    }
}

/// Whether `ty` is `region`, or holds it (as `maybe region` would).
fn mentions_region(ty: &Type) -> bool {
    match ty {
        Type::Region => true,
        Type::Ptr(inner, _) | Type::Maybe(inner) | Type::Array(inner, _) | Type::Slice(inner, _) => mentions_region(inner),
        _ => false,
    }
}

/// `a.b.c` for a chain of fields on a variable, with where `a` is written.
fn dotted_name(expr: &Expr) -> Option<(String, Span)> {
    match &expr.kind {
        ExprKind::Var(name) => Some((name.clone(), expr.span)),
        ExprKind::Field(base, field) => dotted_name(base).map(|(name, span)| (format!("{}{}{}", name, MODULE_SEPARATOR, field), span)),
        _ => None,
    }
}
