use crate::ast::*;
use crate::error::{Error, Result};
use crate::lexer::{KEYWORDS, LexKind, LexToken, Piece, Token};

/// How deeply code may nest: parentheses, operators in a row, unary
/// operators, `^` in types, and blocks inside blocks each add a level. Every
/// compiler stage walks the tree recursively, so without a limit deeply nested
/// input would overflow the stack and crash (taking Vela Studio's server or
/// its WebAssembly module with it) instead of reporting an error.
pub const MAX_NESTING: usize = 100;

/// What a tuple type needs, for the `(` of one that has too few items.
const TUPLE_TYPE_ITEMS: &str = "a tuple holds two or more items, as in '(int, text)'; a type isn't written in parentheses alone";

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
    /// The methods of the shape or kind just read, each a task of its own (see
    /// `method_name`), which the file's items take in after it.
    methods: Vec<Item>,
}

impl Parser {
    /// `tokens` and `spans` are parallel, as produced by `Lexer::tokenize`.
    pub fn new(tokens: Vec<Token>, spans: Vec<Span>) -> Self {
        Parser { tokens, spans, pos: 0, depth: 0, type_places: TypePlaces::new(), type_parts: Vec::new(), includes: Vec::new(), methods: Vec::new() }
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
            // The methods of a shape or a kind are tasks of the file, after it.
            items.append(&mut self.methods);
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

    /// `qualified` for a name that starts a value. A name, a dot, and a call is
    /// a kind's variant (`Shape.Circle(…)`, or `geometry.Shape.Circle(…)`) or a
    /// method called on a value (`p.area(…)`), which look the same until the
    /// file's names are known; `parse_suffix` reads the dot and the call, and
    /// keeps both readings for the resolver (see `ExprKind::MethodCall`).
    fn value_name(&mut self, name: String) -> Result<String> {
        self.qualified(name)
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
            // `&` joins the words after a type's `:` and means nothing else.
            (Token::Amp, _, _) => "; Vela writes 'and' for both at once ('&' only joins the words a generic type must allow)".to_string(),
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
                        | Token::Interpolated(_)
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
        let types = self.parse_type_params()?;
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
        Ok(TaskDef { name, types, given: Vec::new(), params, ret, results, body, span, private: false, owner: None })
    }

    /// `<T, U: ordered>` after a declaration's name: the types it takes, with
    /// what each one must allow. Empty for a declaration that takes none.
    fn parse_type_params(&mut self) -> Result<Vec<TypeParam>> {
        if !self.match_token(Token::Lt) {
            return Ok(Vec::new());
        }
        let mut params: Vec<TypeParam> = Vec::new();
        loop {
            if self.check(Token::Gt) {
                return Err(Error::at(self.peek_span(), "'< … >' names the types a declaration takes, as in '<T>'; leave it out when it takes none"));
            }
            let span = self.peek_span();
            let name = self.expect_ident()?;
            if Type::builtin(&name).is_some() {
                return Err(Error::at(span, format!("'{}' is a builtin type, so it can't name a type a declaration takes; use another name, as in '<T>'", name)));
            }
            if let Some(before) = params.iter().find(|param| param.name == name) {
                return Err(Error::at(span, format!("'{}' is already taken at {}:{}", name, before.span.line, before.span.col)));
            }
            let need = if self.match_token(Token::Colon) { self.parse_needs()? } else { None };
            params.push(TypeParam { name, span, need });
            if !self.match_token(Token::Comma) {
                break;
            }
        }
        if !self.match_token(Token::Gt) {
            return Err(self.unexpected("',' and another type, or '>' to close the types"));
        }
        Ok(params)
    }

    /// The words after a type's `:`, joined by `&`: what every type given for it
    /// must allow. A word another already means is an error, since it says
    /// nothing new.
    fn parse_needs(&mut self) -> Result<Option<(Need, Span)>> {
        let mut kept: Option<(Need, Span)> = None;
        loop {
            let span = self.peek_span();
            let word = self.expect_ident().map_err(|_| Error::at(span, format!("expected what the type must allow: {}", needs_list())))?;
            let Some(need) = Need::from_word(&word) else {
                return Err(Error::at(span, format!("'{}' isn't something a type can be asked to allow; the words are {}", word, needs_list())));
            };
            if let Some((before, _)) = kept {
                let (wider, narrower) = if before.means(need) { (need, before) } else { (before, need) };
                if wider == narrower {
                    return Err(Error::at(span, format!("'{}' is written twice", need.word())));
                }
                return Err(Error::at(span, format!("{}, so '{}' already means '{}'; write '{}' alone", why_means(narrower, wider), narrower.word(), wider.word(), narrower.word())));
            }
            kept = Some((need, span));
            if !self.match_token(Token::Amp) {
                return Ok(kept);
            }
        }
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
        let types = self.parse_type_params()?;
        self.skip_newlines();
        let mut fields = Vec::new();
        let mut methods: Vec<TaskDef> = Vec::new();
        while !self.check(Token::Fi) && !self.is_at_end() && !self.at_item_start() {
            self.skip_newlines();
            if self.check(Token::Fi) || self.at_item_start() { break; }
            self.reject_stray_end()?;
            let private = self.match_token(Token::Priv);
            if self.at_method() {
                let method = self.parse_method(&name, "shape", &types, private, &fields, &methods)?;
                methods.push(method);
                self.skip_newlines();
                continue;
            }
            self.reject_field_below_method(&methods, "shape")?;
            let mut field = self.parse_field()?;
            reject_member_named_this(&field.name, field.span, "a field")?;
            field.private = private;
            fields.push(field);
            self.skip_newlines();
        }
        self.expect_fi("shape", opened)?;
        self.methods.extend(methods.into_iter().map(Item::Task));
        Ok(ShapeDef { name, types, given: Vec::new(), fields, span, private: false })
    }

    /// Rejects a field written below a method: a shape's or a kind's block reads
    /// as its values and then what they do.
    fn reject_field_below_method(&self, methods: &[TaskDef], what: &str) -> Result<()> {
        let Some(first) = methods.first() else { return Ok(()) };
        let name = first.owner.as_ref().map_or("", |owner| owner.name.as_str());
        let of = if what == "kind" { "variant" } else { "field" };
        Err(Error::at(
            self.peek_span(),
            format!(
                "every {} of a {} goes above its methods, and method '{}' is declared at {}:{}",
                of, what, name, first.span.line, first.span.col
            ),
        ))
    }

    /// Whether the cursor is at a member of a shape or a kind that is a method
    /// rather than a field or a variant: a name followed by `(`, or by `<` for
    /// one that names types (which `parse_method` refuses), with `alters` allowed
    /// before it.
    fn at_method(&self) -> bool {
        let mut pos = self.pos;
        if matches!(self.tokens.get(pos), Some(Token::Ident(word)) if word == ALTERS) && matches!(self.tokens.get(pos + 1), Some(Token::Ident(_))) {
            pos += 1;
        }
        matches!(self.tokens.get(pos), Some(Token::Ident(_))) && matches!(self.tokens.get(pos + 1), Some(Token::LParen | Token::Lt))
    }

    /// A method of the shape or kind `owner`, which takes `types`: its name, its
    /// parameters, what it sends, and its body, with `alters` before it when it
    /// changes the value it is called on. It becomes a task whose first parameter
    /// holds that value (see `method_name` and `THIS`).
    fn parse_method(&mut self, owner: &str, what: &str, types: &[TypeParam], private: bool, fields: &[Field], methods: &[TaskDef]) -> Result<TaskDef> {
        let opened = self.peek_span();
        let alters_span = match self.peek() {
            Token::Ident(word) if word == ALTERS => {
                self.advance();
                Some(opened)
            }
            _ => None,
        };
        let span = self.peek_span();
        let name = self.expect_ident()?;
        reject_member_named_this(&name, span, "a method")?;
        if is_builtin_member(&name) || name == LEN {
            return Err(Error::at(
                span,
                format!(
                    "'{0}' is what every value or slice already answers to, so a method can't be called that; name it something else, as in '{0}Of'",
                    name
                ),
            ));
        }
        if let Some(field) = fields.iter().find(|field| field.name == name) {
            return Err(Error::at(
                span,
                format!(
                    "'{0}' is already a field of '{1}', at {2}:{3}; both are written '.{0}', so a field and a method can't share a name",
                    name, owner, field.span.line, field.span.col
                ),
            ));
        }
        if let Some(before) = methods.iter().find(|before| before.owner.as_ref().is_some_and(|before| before.name == name)) {
            return Err(Error::at(span, format!("method '{}' is already defined at {}:{}", name, before.span.line, before.span.col)));
        }
        if self.check(Token::Lt) {
            let takes = if types.is_empty() { String::new() } else { format!(" ('{}' already gives it {})", owner, type_param_list(types)) };
            return Err(Error::at(
                self.peek_span(),
                format!("a method takes no types of its own yet{}; write a task that takes them, and call it from here", takes),
            ));
        }
        let mut params = vec![receiver(owner, types, span)];
        let read = self.parse_field_list();
        params.extend(if what == "kind" { read.map_err(Parser::kind_member)? } else { read? });
        if let Some(param) = params.iter().skip(1).find(|param| param.ty == Type::Region && param.name == THIS_WORD) {
            return Err(Error::at(param.span, format!("'{0}' is the region the value this method is called on lives in, written '@{0}', so a region parameter can't take the name", THIS_WORD)));
        }
        let mut results = Vec::new();
        let ret = if self.match_token(Token::Arrow) {
            self.parse_results(&mut results)?
        } else {
            Type::Unit
        };
        if self.check(Token::LBrace) {
            return Err(Error::at(self.peek_span(), "a method's body starts on the line after its header, and closes with 'fi'"));
        }
        self.skip_newlines();
        let body = self.parse_block()?;
        self.expect_fi("method", opened)?;
        Ok(TaskDef {
            name: method_name(owner, &name),
            types: types.to_vec(),
            given: Vec::new(),
            params,
            ret,
            results,
            body,
            span,
            private,
            owner: Some(Owner { name, alters: alters_span.is_some(), alters_span }),
        })
    }

    fn parse_kind(&mut self) -> Result<KindDef> {
        let opened = self.peek_span();
        self.advance(); // kind
        let span = self.peek_span();
        let name = self.expect_ident()?;
        if self.check(Token::Lt) {
            return Err(Error::at(
                self.peek_span(),
                format!("a kind takes no types yet, so '{}' can't be given any; a task or a shape can, as in 'shape {}<T>'", name, name),
            ));
        }
        self.skip_newlines();
        let mut variants = Vec::new();
        let mut methods: Vec<TaskDef> = Vec::new();
        while !self.check(Token::Fi) && !self.is_at_end() && !self.at_item_start() {
            self.skip_newlines();
            if self.check(Token::Fi) || self.at_item_start() { break; }
            // A variant may be named `end`; only a stray `end` that ends the
            // kind (followed by the file's end or a new item) is the old keyword.
            if self.end_closes_item() {
                self.reject_stray_end()?;
            }
            let variant_span = self.peek_span();
            // `priv` goes before a method, whose name is what the checks below read.
            let private = self.match_token(Token::Priv);
            // A variant carries its values in parentheses, as a method takes its
            // parameters, so `alters` or a `->` after them tells them apart.
            if self.at_method() && !self.at_variant_payload(&methods) {
                let method = self.parse_method(&name, "kind", &[], private, &[], &methods)?;
                methods.push(method);
                self.skip_newlines();
                continue;
            }
            if private {
                return Err(Error::at(variant_span, "a variant can't be 'priv': every variant is as visible as its kind"));
            }
            self.reject_field_below_method(&methods, "kind")?;
            let tag = self.expect_ident()?;
            let payload = if self.check(Token::LParen) {
                self.parse_field_list().map_err(Parser::kind_member)?
            } else {
                Vec::new()
            };
            variants.push(Variant { tag, payload, span: variant_span });
            self.skip_newlines();
        }
        self.expect_fi("kind", opened)?;
        self.methods.extend(methods.into_iter().map(Item::Task));
        Ok(KindDef { name, variants, span, private: false })
    }

    /// Whether the `(` after a name inside a kind holds a variant's values rather
    /// than a method's parameters, which are written the same way. A variant's
    /// line ends after them and it carries at least one; a method is told by
    /// `alters`, by naming what it sends, by taking no parameters, or by another
    /// method above it. A kind's first method that takes parameters and sends
    /// nothing is the one thing this can't tell apart: it names `-> unit`.
    fn at_variant_payload(&self, methods: &[TaskDef]) -> bool {
        if !methods.is_empty() || matches!(self.peek(), Token::Ident(word) if word == ALTERS) {
            return false;
        }
        let mut depth = 0usize;
        let mut named = false;
        for pos in self.pos + 1.. {
            match self.tokens.get(pos) {
                Some(Token::LParen) => depth += 1,
                Some(Token::RParen) => {
                    depth = match depth.checked_sub(1) {
                        // A variant's line ends after its values.
                        Some(0) => return named && matches!(self.tokens.get(pos + 1), Some(Token::Newline | Token::Eof) | None),
                        Some(left) => left,
                        None => return false,
                    };
                }
                // A variant carries named values, `Circle(r: float)`.
                Some(Token::Colon) if depth == 1 => named = true,
                Some(_) => {}
                None => return false,
            }
        }
        false
    }

    /// A failure to read the values in parentheses inside a kind, where a
    /// variant and a method are written alike, with what tells them apart.
    fn kind_member(error: Error) -> Error {
        if !error.message.starts_with("expected ':'") {
            return error;
        }
        Error {
            message: format!(
                "{}; inside a kind a name with values in parentheses is a variant, which carries named values, so a method that takes values says what it sends, as in 'name(…) -> unit'",
                error.message
            ),
            ..error
        }
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
        // Everything a value in a region holds is in that region, so a region
        // written inside must be the same one. A tuple or an array keeps no
        // region of its own, so only this sees it; the checker reports the
        // types that keep one, naming what is in the other region.
        if let Some(inner) = ty.regions().into_iter().find(|inner| *inner != region).filter(|_| keeps_no_region(&ty)) {
            let at = self.type_parts.iter().find(|(part, _)| *part == format!("@{}", inner)).map_or(region_span, |(_, at)| *at);
            return Err(Error::at(at, format!("everything a value in region '{0}' holds is in '{0}'; remove '@{1}'", region, inner)));
        }
        // A part that already names the region holds text or pointers itself,
        // as `(int, text@r)@r` and `[text@r]@r` do, even when nothing is left
        // to fill in.
        let mut named = !ty.regions().is_empty();
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
        if self.match_token(Token::LParen) {
            self.enter()?;
            let items = self.parse_tuple_type(span);
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
        if self.check(Token::Lt) {
            if Type::builtin(&name).is_some() {
                return Err(Error::at(self.peek_span(), format!("'{}' takes no types, so '< … >' has nothing to name", name)));
            }
            let types = self.parse_given_types()?;
            return Ok(Type::Applied(Box::new(Applied { name, types, region: None })));
        }
        Ok(Type::builtin(&name).unwrap_or(Type::Named(name, None)))
    }

    /// After the `<` of the types a shape is given, inside a written type: the
    /// types and `>`. (The parts of these types belong to the type being read,
    /// so they are read with `parse_type_parts` rather than `parse_type`, which
    /// would start a written type of its own.)
    fn parse_given_types(&mut self) -> Result<Vec<Type>> {
        self.advance(); // <
        let mut types = Vec::new();
        loop {
            if self.check(Token::Gt) {
                return Err(Error::at(self.peek_span(), "'< … >' names the types a shape is given, as in '<int>'; leave it out for a shape that takes none"));
            }
            self.enter()?;
            let ty = self.parse_type_parts();
            self.leave(1);
            types.push(ty?);
            if !self.match_token(Token::Comma) {
                break;
            }
        }
        if !self.match_token(Token::Gt) {
            return Err(self.unexpected("',' and another type, or '>' to close the types"));
        }
        Ok(types)
    }

    /// After a type's `(` at `opened`: `T, U, …)`, a tuple of two or more items.
    fn parse_tuple_type(&mut self, opened: Span) -> Result<Type> {
        let mut items = Vec::new();
        loop {
            // `()` and `(int,)`: a missing item, rather than a missing name.
            if self.check(Token::RParen) {
                return Err(Error::at(opened, TUPLE_TYPE_ITEMS));
            }
            items.push(self.parse_type_parts()?);
            if !self.match_token(Token::Comma) {
                break;
            }
        }
        if !self.match_token(Token::RParen) {
            return Err(self.unexpected("',' and another item, or ')' to close the tuple"));
        }
        if items.len() < 2 {
            return Err(Error::at(opened, TUPLE_TYPE_ITEMS));
        }
        Ok(Type::Tuple(items))
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
        // `_` only takes a value, and `(a, b) =` unpacks a tuple, so a line
        // starting with either is an assignment.
        if self.check(Token::Underscore) || self.at_target_group(true) {
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

    /// One place an assignment writes to, `_`, or `(a, b, …)`, the places of a
    /// tuple's items.
    fn parse_target(&mut self) -> Result<Target> {
        let span = self.peek_span();
        if self.match_token(Token::Underscore) {
            return Ok(Target::Discard(span));
        }
        if self.at_target_group(false) {
            self.advance(); // (
            self.enter()?;
            let parts = self.parse_target_parts(span);
            self.leave(1);
            return Ok(Target::Tuple(parts?, span));
        }
        Ok(Target::Place(self.parse_expr()?))
    }

    /// After the `(` at `opened` of places unpacking a tuple: the places and `)`.
    fn parse_target_parts(&mut self, opened: Span) -> Result<Vec<Target>> {
        let mut parts = vec![self.parse_target()?];
        while self.match_token(Token::Comma) {
            parts.push(self.parse_target()?);
        }
        self.expect(Token::RParen)?;
        if parts.len() < 2 {
            return Err(Error::at(opened, "a tuple has two or more items, so '( … )' names two or more places, as in '(a, b) = pair'"));
        }
        Ok(parts)
    }

    /// Whether the cursor is at `(` of places unpacking a tuple: parentheses on
    /// one line with a comma directly inside, and, at the start of a statement
    /// (`statement`), followed by `=` or `,`.
    fn at_target_group(&self, statement: bool) -> bool {
        if !self.check(Token::LParen) {
            return false;
        }
        let mut depth = 0usize;
        let mut comma = false;
        for p in self.pos.. {
            match self.tokens.get(p) {
                Some(Token::LParen | Token::LBracket | Token::LBrace) => depth += 1,
                Some(Token::RParen | Token::RBracket | Token::RBrace) => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return comma && (!statement || matches!(self.tokens.get(p + 1), Some(Token::Assign | Token::Comma)));
                    }
                }
                Some(Token::Comma) if depth == 1 => comma = true,
                Some(Token::Newline | Token::Eof) | None => return false,
                _ => {}
            }
        }
        false
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
                | Token::Interpolated(_)
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
        let mut names = vec![self.parse_bound()?];
        while self.match_token(Token::Comma) {
            names.push(self.parse_bound()?);
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
    /// `none`, `fail e`, `(x, y)`, or `_`. A name it binds may be written
    /// `hold name`, which makes it a variable that can be assigned.
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
                let names = self.parse_pattern_names()?;
                if self.check(Token::Comma) {
                    return Err(Error::at(
                        self.peek_span(),
                        "'{…}' names the one value inside a maybe or a 'T or E'; to take a tuple apart too, write its names in parentheses, as in '{(name, age)}'",
                    ));
                }
                self.expect(Token::RBrace)?;
                PatternKind::Value(names)
            }
            Token::Fail => {
                self.advance();
                PatternKind::Fail(self.parse_pattern_names()?)
            }
            // `(x, y)` or `hold (x, y)`: the items of a tuple.
            Token::LParen | Token::Hold => {
                let names = self.parse_pattern_names()?;
                if names.parts.is_empty() {
                    return Err(Error::at(
                        names.span,
                        format!(
                            "'{0}' matches a value, so a name alone isn't a pattern: write '{{hold {1}}}' for the value inside a maybe or a 'T or E', or '{0} hold (x, y)' for a tuple's items",
                            keyword, names.name
                        ),
                    ));
                }
                PatternKind::Tuple(names)
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
                while matches!(self.peek(), Token::Ident(_) | Token::Underscore | Token::Hold) {
                    let names = self.parse_pattern_names()?;
                    if !names.parts.is_empty() {
                        return Err(Error::at(names.span, format!("a pattern names its bindings without parentheses, as in '{} {} value'", keyword, tag)));
                    }
                    binds.push(names);
                }
                PatternKind::Variant { kind, tag, binds }
            }
            _ => return Err(self.unexpected("a variant, '{value}', 'none', 'fail e', or '_'")),
        };
        Ok(Pattern { kind, span })
    }

    /// What a pattern names: `name`, `_`, or `(a, b)`, the names of a tuple's
    /// items, each of which `hold` may make changeable.
    fn parse_pattern_names(&mut self) -> Result<Names> {
        let span = self.peek_span();
        let mutable = self.match_token(Token::Hold);
        if !self.check(Token::LParen) {
            let (name, at) = self.parse_bound_name()?;
            return Ok(Names::one(name, at, mutable));
        }
        self.advance(); // (
        self.enter()?;
        let parts = self.parse_pattern_parts(span);
        self.leave(1);
        Ok(Names { name: String::new(), span, mutable, parts: parts? })
    }

    /// After the `(` at `opened` of a pattern's names: the names and `)`.
    fn parse_pattern_parts(&mut self, opened: Span) -> Result<Vec<Names>> {
        let mut parts = vec![self.parse_pattern_names()?];
        while self.match_token(Token::Comma) {
            parts.push(self.parse_pattern_names()?);
        }
        self.expect(Token::RParen)?;
        if parts.len() < 2 {
            return Err(Error::at(opened, "a tuple has two or more items, so '( … )' names two or more, as in 'is (x, y)'"));
        }
        Ok(parts)
    }

    /// A name a binding declares, with its type, or `(a, b, …)`, the names of a
    /// tuple's items.
    fn parse_bound(&mut self) -> Result<Bound> {
        let span = self.peek_span();
        if self.match_token(Token::LParen) {
            self.enter()?;
            let parts = self.parse_bound_parts(span);
            self.leave(1);
            let parts = parts?;
            if self.check(Token::Colon) {
                return Err(Error::at(
                    self.peek_span(),
                    "'( … )' takes a tuple apart, so each name in it takes its own type, as in 'bind (a: int, b: text) = t'",
                ));
            }
            return Ok(Bound { name: String::new(), span, ty: None, ty_span: None, parts });
        }
        let (name, name_span) = self.parse_bound_name()?;
        let (ty, ty_span) = if self.match_token(Token::Colon) {
            let ty_span = self.peek_span();
            (Some(self.parse_type()?), Some(ty_span))
        } else {
            (None, None)
        };
        Ok(Bound { name, span: name_span, ty, ty_span, parts: Vec::new() })
    }

    /// After the `(` at `opened` of names unpacking a tuple: the names and `)`.
    fn parse_bound_parts(&mut self, opened: Span) -> Result<Vec<Bound>> {
        let mut parts = vec![self.parse_bound()?];
        while self.match_token(Token::Comma) {
            parts.push(self.parse_bound()?);
        }
        self.expect(Token::RParen)?;
        if parts.len() < 2 {
            return Err(Error::at(opened, "a tuple has two or more items, so '( … )' names two or more, as in 'bind (a, b) = pair'"));
        }
        Ok(parts)
    }

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

    /// Precedence of `+`, whose chains `at r` joins.
    const ADD_PRECEDENCE: u8 = 6;

    /// Parses operators of precedence `min` or higher by precedence climbing,
    /// left-associative, with each node's span at its operator. `a..b` takes one
    /// `..` (`a..b..c` is not a range). Climbing keeps the recursion per
    /// parenthesis shallow, which matters for `MAX_NESTING`.
    fn parse_binary(&mut self, min: u8) -> Result<Expr> {
        let left = self.parse_unary()?;
        self.parse_operators(left, min)
    }

    /// The operators of precedence `min` or higher after `left`, the first
    /// operand `parse_binary` read. (A function of its own: `parse_binary`'s
    /// frame is on the stack once per nesting level, and a debug build gives a
    /// frame room for everything its function could hold at once.)
    fn parse_operators(&mut self, mut left: Expr, min: u8) -> Result<Expr> {
        // Each operator deepens the tree by one level, like a parenthesis.
        let mut levels = 0;
        let mut ranged = false;
        loop {
            // `a + b at r` or `"n = {n}" at r`: the `+` chain just read, or the
            // text holding values, is joined into `r`. The right operand of a
            // `+` leaves its `at` to the whole chain.
            if self.check(Token::At) && min <= Self::ADD_PRECEDENCE {
                let added = matches!(left.kind, ExprKind::Binary(BinaryOp::Add, ..));
                if (added && levels > 0) || pending_region(&left) {
                    left = self.parse_join(left)?;
                } else if added {
                    // A chain in parentheses names its region with `@`.
                    return Err(Error::at(
                        self.peek_span(),
                        "a '+' chain in parentheses names its region with '@', as in '(a + b)@r'; without them, write 'a + b at r'",
                    ));
                }
            }
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

    /// `a + b + c at r`, `(a + b)@r`, `"n = {n}" at r`, or `"n = {n}"@r`, with
    /// `chain` the `+` chain or the text holding values read, and the cursor at
    /// `at` or `@`: new text in `r` holding the chain's operands, or the text's
    /// pieces, each value turned into text in `r`.
    fn parse_join(&mut self, chain: Expr) -> Result<Expr> {
        self.advance(); // `at` or `@`
        let region_span = self.peek_span();
        let region = self.expect_ident()?;
        let span = chain.span;
        let mut parts = Vec::new();
        let mut waiting = vec![chain];
        // Operands left to right: the right operand of each `+` waits until
        // everything on its left is taken.
        while let Some(expr) = waiting.pop() {
            match expr.kind {
                ExprKind::Binary(BinaryOp::Add, left, right) => {
                    waiting.push(*right);
                    waiting.push(*left);
                }
                ExprKind::Join { parts: pieces, region: ref missing, .. } if missing.is_empty() => {
                    interpolated_parts(pieces, &region, region_span, &mut parts);
                }
                _ => parts.push(expr),
            }
        }
        Ok(Expr::new(ExprKind::Join { parts, region, region_span }, span))
    }

    /// Text holding values, `"n = {n}"`, at `span` and the cursor: a join of
    /// its pieces whose region is still to come (`at r` or `@r`, which
    /// `parse_join` reads), so it is left empty; the checker asks for one where
    /// none comes. (A function of its own: `parse_primary`'s frame is on the
    /// stack once per nesting level.)
    fn parse_interpolated(&mut self, span: Span) -> Result<Expr> {
        let Token::Interpolated(pieces) = self.advance() else { return Err(Error::internal_at(span, "text holding values that isn't")) };
        let mut parts = Vec::new();
        for piece in pieces {
            match piece {
                Piece::Text(text) => parts.push(Expr::new(ExprKind::TextLit(text), span)),
                Piece::Value { tokens, close, .. } => parts.push(self.parse_text_value(tokens, &close)?),
            }
        }
        Ok(Expr::new(ExprKind::Join { parts, region: String::new(), region_span: span }, span))
    }

    /// The value inside `{…}` in text, from its `tokens` and its closing `}`:
    /// read by this parser, on those tokens in place of the file's, so nesting
    /// counts as anywhere else.
    fn parse_text_value(&mut self, tokens: Vec<LexToken>, close: &LexToken) -> Result<Expr> {
        let (mut kinds, mut spans) = (Vec::new(), Vec::new());
        for token in tokens {
            if let LexKind::Token(kind) = token.kind {
                kinds.push(kind);
                spans.push(token.start);
            }
        }
        kinds.extend([Token::RBrace, Token::Eof]);
        spans.extend([close.start, close.end]);
        let file_tokens = std::mem::replace(&mut self.tokens, kinds);
        let file_spans = std::mem::replace(&mut self.spans, spans);
        let file_pos = std::mem::replace(&mut self.pos, 0);
        let value = self.parse_expr().and_then(|value| if self.check(Token::RBrace) { Ok(value) } else { Err(self.unexpected("'}' to end the value")) });
        self.tokens = file_tokens;
        self.spans = file_spans;
        self.pos = file_pos;
        value
    }

    fn parse_unary(&mut self) -> Result<Expr> {
        if matches!(self.peek(), Token::Fail | Token::Minus | Token::Not) {
            return self.parse_prefixed();
        }
        self.parse_postfix()
    }

    /// `fail e`, `-x`, or `not x`. (A function of its own: `parse_unary`'s frame
    /// is on the stack once per nesting level.)
    fn parse_prefixed(&mut self) -> Result<Expr> {
        let span = self.peek_span();
        if self.match_token(Token::Fail) {
            self.enter()?;
            let error = self.parse_operand()?;
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
        let expr = self.parse_operand()?;
        self.leave(1);
        Ok(Expr::new(ExprKind::Unary(op, Box::new(expr)), span))
    }

    /// The value after `fail`, `-`, or `not`, which takes only the value right
    /// after it, and text holding values with its region: `fail "{n} is
    /// negative" at r`.
    fn parse_operand(&mut self) -> Result<Expr> {
        let operand = self.parse_unary()?;
        if self.check(Token::At) && pending_region(&operand) {
            return self.parse_join(operand);
        }
        Ok(operand)
    }

    fn parse_postfix(&mut self) -> Result<Expr> {
        let parenthesized = self.check(Token::LParen);
        let expr = self.parse_primary()?;
        self.parse_suffixes(expr, parenthesized)
    }

    /// What follows `expr`, read by `parse_postfix` (and `parenthesized` when
    /// written in parentheses): a region its text is joined into, fields,
    /// items, `^`, and plan Bs. (A function of its own: `parse_postfix`'s frame
    /// is on the stack once per nesting level.)
    fn parse_suffixes(&mut self, mut expr: Expr, parenthesized: bool) -> Result<Expr> {
        // `(a + b)@r` or `"n = {n}"@r`: text joined into `r`.
        if self.check(Token::AtSign) && (pending_region(&expr) || (parenthesized && matches!(expr.kind, ExprKind::Binary(BinaryOp::Add, ..)))) {
            expr = self.parse_join(expr)?;
        }
        let mut levels = 0;
        while matches!(self.peek(), Token::Dot | Token::LBracket | Token::Caret | Token::LBrace) {
            self.enter()?;
            levels += 1;
            expr = self.parse_suffix(expr)?;
        }
        self.leave(levels);
        if self.check(Token::AtSign) {
            return Err(Error::at(
                self.peek_span(),
                "'@r' after a value names the region text is joined into only after parentheses or text holding values, as in '(a + b)@r' or '\"n = {n}\"@r'; or write 'a + b at r'",
            ));
        }
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
                // `t.0`: an item of a tuple.
                if let Token::Int(n) = self.peek() {
                    let item = n.to_string();
                    self.advance();
                    return Ok(Expr::new(ExprKind::Field(Box::new(expr), item), span));
                }
                let field = self.expect_ident()?;
                let expr = Expr::new(ExprKind::Field(Box::new(expr), field), span);
                // A `<` here opens the types a use gives (a method takes none of
                // its own, which `dotted_name_use` says), never a comparison:
                // see `at_type_args`.
                if self.check(Token::LParen) || self.at_type_args() || (self.check(Token::LBrace) && self.at_shape_literal()) {
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
            Token::Interpolated(_) => return self.parse_interpolated(span),
            Token::True => { self.advance(); ExprKind::BoolLit(true) }
            Token::False => { self.advance(); ExprKind::BoolLit(false) }
            Token::None => { self.advance(); ExprKind::NoneLit }
            Token::New => return self.parse_new(),
            Token::LBracket => return self.parse_array(),
            Token::LParen => return self.parse_parenthesized(),
            // `.this`, `.field`, or `.method(…)`: the value a method is called
            // on, and what it holds.
            Token::Dot if matches!(self.tokens.get(self.pos + 1), Some(Token::Ident(_))) => return self.parse_member_of_this(span),
            Token::Ident(_) => return self.parse_name_use(span),
            _ => return Err(self.unexpected("an expression")),
        };
        Ok(Expr::new(kind, span))
    }

    /// `.this`, `.field`, or `.method(…)` at `span`, with the cursor at the
    /// dot: the value a method is called on, and what it holds. Only a method
    /// has one, which the checker says; here it is a value like any other.
    fn parse_member_of_this(&mut self, span: Span) -> Result<Expr> {
        self.advance(); // .
        let at = self.peek_span();
        let member = self.expect_ident()?;
        let value = Expr::new(ExprKind::Var(THIS.to_string()), span);
        if member == THIS_WORD {
            return Ok(value);
        }
        let expr = Expr::new(ExprKind::Field(Box::new(value), member), at);
        // A `{` after it is a plan B on the member, which `parse_suffix`
        // reads: no shape is named `.x`.
        if self.check(Token::LParen) || self.at_type_args() {
            return self.dotted_name_use(expr);
        }
        Ok(expr)
    }

    /// A value that starts with a name, at `span` and the cursor: a variable,
    /// a call, or a shape literal. (A function of its own: `parse_primary`'s
    /// frame is on the stack once per nesting level.)
    fn parse_name_use(&mut self, span: Span) -> Result<Expr> {
        let name = self.expect_ident()?;
        let name = self.value_name(name)?;
        if name == SCAN && self.check(Token::LParen) {
            return self.parse_scan(span);
        }
        if name == TEXT_CAST && self.check(Token::LParen) {
            return self.parse_text_cast(span);
        }
        // The types a use gives are read only where a call or a shape
        // literal follows them (see `at_type_args`), so one of `(` and
        // `{` comes next for certain.
        let kind = if self.at_type_args() {
            let types = self.parse_type_args()?;
            if self.check(Token::LParen) {
                ExprKind::Call { name, types, args: self.parse_arg_list()? }
            } else {
                self.parse_struct_init(name, types)?
            }
        } else if self.check(Token::LBrace) && self.at_shape_literal() {
            self.parse_struct_init(name, Vec::new())?
        } else if self.check(Token::LParen) {
            ExprKind::Call { name, types: Vec::new(), args: self.parse_arg_list()? }
        } else {
            ExprKind::Var(name)
        };
        Ok(Expr::new(kind, span))
    }

    /// `scan(prompt) at r` or `scan(prompt)@r`, with the cursor at `(`. (A
    /// function of its own: `parse_primary`'s frame is on the stack once per
    /// nesting level.)
    fn parse_scan(&mut self, span: Span) -> Result<Expr> {
        let args = self.parse_arg_list()?;
        if self.check(Token::In) {
            // A read that names a type reads a number, which needs no region, so
            // nothing follows its call; the checker settles what a type may be.
            let hint = match args.len() {
                1 => "'scan' names its region with 'at', as in 'scan(\"Name: \") at r' (or 'scan(\"Name: \")@r'); 'in' counts through a range",
                _ => "'scan' reading a number names no region, so nothing follows the call, as in 'scan(\"Age: \", int)'; 'in' counts through a range",
            };
            return Err(Error::at(self.peek_span(), hint));
        }
        // A line read into a region names one, and a number read from a line
        // names none, which the checker tells apart by what the call takes; the
        // position of a region that isn't there is where one would be written.
        let mut region_span = self.peek_span();
        let mut region = None;
        if self.match_token(Token::At) || self.match_token(Token::AtSign) {
            region_span = self.peek_span();
            region = Some(self.expect_ident()?);
        }
        Ok(Expr::new(ExprKind::Scan { args, region, region_span }, span))
    }

    /// `(value)`, or a tuple, `(a, b, …)`, on one line.
    fn parse_parenthesized(&mut self) -> Result<Expr> {
        let span = self.peek_span();
        self.advance(); // (
        let first = self.parse_expr()?;
        if !self.check(Token::Comma) {
            self.expect(Token::RParen)?;
            return Ok(first);
        }
        // A tuple holds its values in a frame of its own, as an array does.
        self.enter()?;
        let mut values = vec![first];
        let result = loop {
            if !self.match_token(Token::Comma) {
                break self.expect(Token::RParen);
            }
            if self.check(Token::Newline) {
                break Err(Error::at(self.peek_span(), "a tuple is written on one line, as in '(1, \"Ada\")'"));
            }
            match self.parse_expr() {
                Ok(value) => values.push(value),
                Err(error) => break Err(error),
            }
        };
        self.leave(1);
        result.map(|()| Expr::new(ExprKind::Tuple(values), span))
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

    /// A call or shape literal after a field, as in `p.area()`, `p.x.f()`, or
    /// `geometry.Point { x: 1 }` where `geometry` isn't a module this file
    /// includes (`value_name` joins the names that are). A call is a method
    /// called on the value, which keeps the reading as one dotted name too when
    /// the value is a name: `Light.Red(…)` is a variant and `m.f(…)` a module's
    /// task, and only the file's names tell them apart (see the resolver). A
    /// change to a slice, `xs.add(…)` or `xs.del(…)`, and a copy, `x.copy()`,
    /// are neither. (A function of its own: `parse_postfix`'s frame is on the
    /// stack once per nesting level.)
    fn dotted_name_use(&mut self, expr: Expr) -> Result<Expr> {
        if matches!(&expr.kind, ExprKind::Field(_, word) if word == COPY) && self.check(Token::LParen) {
            return self.parse_copy(expr);
        }
        if matches!(&expr.kind, ExprKind::Field(_, word) if word == STR) && self.check(Token::LParen) {
            return self.parse_str(expr);
        }
        if let ExprKind::Field(_, word) = &expr.kind
            && let Some(op) = EditOp::from_word(word)
            && self.check(Token::LParen)
        {
            let args = self.parse_arg_list()?;
            let ExprKind::Field(items, _) = expr.kind else { return Err(Error::internal_at(expr.span, "a change without its slice")) };
            return Ok(Expr::new(ExprKind::Edit { items, op, args }, expr.span));
        }
        // `p.pick<int>(…)`: a task of a module is named before its types (see
        // `value_name`), so what is left here is a method, which takes none of
        // its own yet, or a variant, which takes none at all.
        let dotted = dotted_name(&expr);
        if self.at_type_args()
            && let ExprKind::Field(_, member) = &expr.kind
        {
            return Err(Error::at(
                self.peek_span(),
                format!(
                    "a method takes no types of its own yet, and a variant takes none at all, so '{0}' is given none; call it as '{0}(…)'",
                    member
                ),
            ));
        }
        if self.check(Token::LParen) {
            let span = expr.span;
            let ExprKind::Field(value, name) = expr.kind else { return Err(Error::internal_at(span, "a call without the value it is on")) };
            // `dotted_name` has read the whole of it, the method's name too.
            let dotted = dotted.map(|(whole, _)| whole);
            let args = self.parse_arg_list()?;
            return Ok(Expr::new(ExprKind::MethodCall { value, name, args, dotted }, span));
        }
        let Some((name, span)) = dotted else { return Ok(expr) };
        // `m.Box<int> { … }`: the types a use gives, told from a comparison by
        // the `{` that must follow the closing `>`.
        let kind = if self.at_type_args() {
            let types = self.parse_type_args()?;
            self.parse_struct_init(name, types)?
        } else {
            self.parse_struct_init(name, Vec::new())?
        };
        Ok(Expr::new(kind, span))
    }

    /// Whether the cursor is at the `<` of the types a use gives: a `<` whose
    /// matching `>` is followed by `(` (a call) or `{` (a shape literal), with
    /// only what a type is written with in between. Comparisons never look like
    /// that: `a < b > (c)` would compare a bool with `>`, and `a < b > {` has no
    /// value after the `>` at all, so both are errors either way.
    fn at_type_args(&self) -> bool {
        if !self.check(Token::Lt) {
            return false;
        }
        let mut depth = 0usize;
        for p in self.pos + 1.. {
            match self.tokens.get(p) {
                // `>` closes the types, or, nested, one type's own.
                // A call follows the types a task is given, and a literal the
                // types a shape is given; nothing else can follow a `>`.
                Some(Token::Gt) if depth == 0 => return matches!(self.tokens.get(p + 1), Some(Token::LParen | Token::LBrace)),
                Some(Token::Lt) => depth += 1,
                Some(Token::Gt) => depth -= 1,
                Some(token) if token.writes_type() => {}
                _ => return false,
            }
        }
        false
    }

    /// After the `<` of the types a call gives: the types and `>`.
    fn parse_type_args(&mut self) -> Result<Vec<TypeArg>> {
        self.advance(); // <
        let mut types = Vec::new();
        loop {
            let at = self.peek_span();
            self.enter()?;
            let ty = self.parse_type();
            self.leave(1);
            types.push((ty?, at));
            if !self.match_token(Token::Comma) {
                break;
            }
        }
        if !self.match_token(Token::Gt) {
            return Err(self.unexpected("',' and another type, or '>' to close the types"));
        }
        Ok(types)
    }

    /// `value.str() at r`, `value.str()@r`, or `value.str(r)`, with the cursor at
    /// `(` and `expr` the field `str` of the value: the method call `str`, given
    /// the region as its argument.
    fn parse_str(&mut self, expr: Expr) -> Result<Expr> {
        let dotted = dotted_name(&expr).map(|(whole, _)| whole);
        let span = expr.span;
        let ExprKind::Field(value, name) = expr.kind else { return Err(Error::internal_at(span, "a 'str' without its value")) };
        let mut args = self.parse_arg_list()?;
        // With the region given in the parentheses, an `at` after them is a
        // join's: `a.str(r) + b at r`.
        if !args.is_empty() {
            return Ok(Expr::new(ExprKind::MethodCall { value, name, args, dotted }, span));
        }
        if let Some(region) = self.parse_text_region()? {
            args.push(region);
        } else {
            return Err(Error::at(
                self.peek_span(),
                "'str' makes new text, so it names the region the text goes in, as in 'x.str() at r', 'x.str()@r', or 'x.str(r)'",
            ));
        }
        Ok(Expr::new(ExprKind::MethodCall { value, name, args, dotted }, span))
    }

    /// `text(value) at r` or `text(value)@r`, at `span`, with the cursor at `(`:
    /// the method call `str` on the value, given the region.
    fn parse_text_cast(&mut self, span: Span) -> Result<Expr> {
        let args = self.parse_arg_list()?;
        let [value] = <[Expr; 1]>::try_from(args).map_err(|args| {
            let got = if args.len() == 1 { "1 value".to_string() } else { format!("{} values", args.len()) };
            Error::at(span, format!("'text(…)' turns one value into text, as in 'text(n) at r', got {}", got))
        })?;
        let Some(region) = self.parse_text_region()? else {
            return Err(Error::at(self.peek_span(), "'text(…)' makes new text, so it names the region the text goes in, as in 'text(n) at r' or 'text(n)@r'"));
        };
        Ok(Expr::new(ExprKind::MethodCall { value: Box::new(value), name: STR.to_string(), args: vec![region], dotted: None }, span))
    }

    /// `at r` or `@r` after the parentheses of `str` or `text(…)`: the region,
    /// as the name it is.
    fn parse_text_region(&mut self) -> Result<Option<Expr>> {
        if !self.match_token(Token::At) && !self.match_token(Token::AtSign) {
            return Ok(None);
        }
        let span = self.peek_span();
        let region = self.expect_ident()?;
        Ok(Some(Expr::new(ExprKind::Var(region), span)))
    }

    /// `value.copy()`, with the cursor at `(` and `expr` the field `copy` of the
    /// value, and the region after it: `at r`, `@r`, or none.
    fn parse_copy(&mut self, expr: Expr) -> Result<Expr> {
        let ExprKind::Field(value, _) = expr.kind else { return Err(Error::internal_at(expr.span, "a copy without its value")) };
        let args = self.parse_arg_list()?;
        if let Some(first) = args.first() {
            return Err(Error::at(first.span, "'copy' takes no arguments; name the region to copy into after it, as in 'x.copy() at r' or 'x.copy()@r'"));
        }
        if self.check(Token::In) {
            return Err(Error::at(self.peek_span(), "a copy names its region with 'at', as in 'x.copy() at r' (or 'x.copy()@r'); 'in' counts through a range"));
        }
        let (region, region_span) = if self.match_token(Token::At) || self.match_token(Token::AtSign) {
            let region_span = self.peek_span();
            (Some(self.expect_ident()?), region_span)
        } else {
            (None, expr.span)
        };
        Ok(Expr::new(ExprKind::Copy { value, region, region_span }, expr.span))
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

    /// `Point { x: 1 }`, or `Box<int> { value: none }` with `types` the ones the
    /// literal names, with the cursor at `{`.
    fn parse_struct_init(&mut self, name: String, types: Vec<TypeArg>) -> Result<ExprKind> {
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
        Ok(ExprKind::StructInit { name, types, fields })
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

/// The words a generic type can be asked to allow, for a message, from what
/// most types allow to what fewest do.
fn needs_list() -> String {
    let words: Vec<String> = [Need::Fixed, Need::Same, Need::Shown, Need::Ordered].iter().map(|need| format!("'{}'", need.word())).collect();
    words.join(", ")
}

/// Why `narrower` already means `wider`, for the message that says to write one
/// word alone.
fn why_means(narrower: Need, wider: Need) -> String {
    let value = match narrower {
        Need::Ordered => "a value with an order is a number or text, which",
        Need::Shown => "a value that can be shown",
        Need::Same => "a value that can be compared",
        Need::Fixed => "a value that never changes",
    };
    let also = match wider {
        Need::Ordered => "has an order",
        Need::Shown => "can be shown",
        Need::Same => "can be compared",
        Need::Fixed => "never changes",
    };
    format!("{} {}", value, also)
}

/// Whether a region written on `ty` belongs to what it holds, because `ty`
/// keeps none of its own: a tuple or an array, or a `maybe` or `T or E` of one.
/// (A pointer, a slice, and a shape or kind keep the region they are given, and
/// the checker reports what such a value holds in another region.)
fn keeps_no_region(ty: &Type) -> bool {
    match ty {
        Type::Tuple(_) | Type::Array(..) => true,
        Type::Maybe(inner) | Type::Fail(inner) => keeps_no_region(inner),
        Type::Or(value, error) => keeps_no_region(value) || keeps_no_region(error),
        _ => false,
    }
}

/// Whether `ty` is `region`, or holds it (as `maybe region` would).
fn mentions_region(ty: &Type) -> bool {
    match ty {
        Type::Region => true,
        Type::Ptr(inner, _) | Type::Maybe(inner) | Type::Array(inner, _) | Type::Slice(inner, _) => mentions_region(inner),
        Type::Tuple(items) => items.iter().any(mentions_region),
        _ => false,
    }
}

/// `a.b.c` for a chain of fields on a variable, with where `a` is written.
/// The parameter that holds the value a method of `owner` is called on, at the
/// position of the method's name. The value names the region it lives in
/// (`@this`), which the checker drops where the shape or kind lives in none; a
/// shape that takes types is named with them, as its own fields name them.
fn receiver(owner: &str, types: &[TypeParam], span: Span) -> Field {
    let region = Some(THIS_WORD.to_string());
    let ty = if types.is_empty() {
        Type::Named(owner.to_string(), region)
    } else {
        let given = types.iter().map(|param| Type::Named(param.name.clone(), None)).collect();
        Type::Applied(Box::new(Applied { name: owner.to_string(), types: given, region }))
    };
    Field { name: THIS.to_string(), ty, span, ty_span: span, private: false }
}

/// Refuses a member named `this`, which is the whole value inside a method, so
/// nothing written after a dot could reach it.
fn reject_member_named_this(name: &str, span: Span, what: &str) -> Result<()> {
    if name != THIS_WORD {
        return Ok(());
    }
    Err(Error::at(
        span,
        format!("'{0}' is the value a method is called on, written '{1}', so {2} can't take the name", THIS_WORD, THIS, what),
    ))
}

/// The types a declaration takes, as they are written: `<T, U>`.
fn type_param_list(types: &[TypeParam]) -> String {
    let names: Vec<&str> = types.iter().map(|param| param.name.as_str()).collect();
    format!("'<{}>'", names.join(", "))
}

fn dotted_name(expr: &Expr) -> Option<(String, Span)> {
    match &expr.kind {
        // The value a method is called on is no part of a name: `.this` and
        // `.x` are its own, and `.x.f(…)` calls a method on the field.
        ExprKind::Var(name) if name == THIS => None,
        ExprKind::Var(name) => Some((name.clone(), expr.span)),
        ExprKind::Field(base, field) => dotted_name(base).map(|(name, span)| (format!("{}{}{}", name, MODULE_SEPARATOR, field), span)),
        _ => None,
    }
}

/// Whether `expr` is text holding values whose region is still to come (see
/// `Parser::parse_interpolated`).
fn pending_region(expr: &Expr) -> bool {
    matches!(&expr.kind, ExprKind::Join { region, .. } if region.is_empty())
}

/// The `pieces` of text holding values, joined into `region`, added to
/// `parts`: its text as it is, and each value turned into text in the region.
/// A value that is itself text holding values gives its own pieces.
fn interpolated_parts(pieces: Vec<Expr>, region: &str, region_span: Span, parts: &mut Vec<Expr>) {
    for piece in pieces {
        if pending_region(&piece) {
            if let ExprKind::Join { parts: inner, .. } = piece.kind {
                interpolated_parts(inner, region, region_span, parts);
            }
            continue;
        }
        if matches!(piece.kind, ExprKind::TextLit(_)) {
            parts.push(piece);
            continue;
        }
        let span = piece.span;
        let into = Expr::new(ExprKind::Var(region.to_string()), region_span);
        parts.push(Expr::new(ExprKind::MethodCall { value: Box::new(piece), name: STR.to_string(), args: vec![into], dotted: None }, span));
    }
}
