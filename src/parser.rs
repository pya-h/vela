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
}

impl Parser {
    /// `tokens` and `spans` are parallel, as produced by `Lexer::tokenize`.
    pub fn new(tokens: Vec<Token>, spans: Vec<Span>) -> Self {
        Parser { tokens, spans, pos: 0, depth: 0 }
    }

    pub fn parse_file(&mut self) -> Result<Vec<Item>> {
        let mut items = Vec::new();
        self.skip_newlines();
        while !self.is_at_end() {
            items.push(self.parse_item()?);
            self.skip_newlines();
        }
        Ok(items)
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
        if self.check(Token::Send) {
            return self.misplaced_send();
        }
        Error::at(self.peek_span(), format!("expected {}, got {}", expected, self.peek().describe()))
    }

    /// The error for a `<-` that doesn't start a line. After something that
    /// can end a value, it was most likely meant as `< -`.
    fn misplaced_send(&self) -> Error {
        let after_value = self.pos > 0
            && matches!(
                self.tokens.get(self.pos - 1),
                Some(Token::Ident(_) | Token::Int(_) | Token::Float(_) | Token::RParen | Token::RBracket | Token::Caret)
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
        if self.is_at_end() || matches!(self.peek(), Token::Task | Token::Shape | Token::Kind) {
            return Err(Error::at(
                self.peek_span(),
                format!(
                    "expected 'fi' to close the '{}' at {}:{}, got {}",
                    opener,
                    opened.line,
                    opened.col,
                    self.peek().describe()
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
        let is_end = matches!(self.peek(), Token::Ident(name) if name == "end");
        let next = self.tokens.get(self.pos + 1).unwrap_or(&Token::Eof);
        if is_end && matches!(next, Token::Newline | Token::Eof) {
            return Err(Error::at(self.peek_span(), "blocks close with 'fi', not 'end'"));
        }
        Ok(())
    }

    /// Whether the next token starts a top-level item, which can't appear
    /// inside a block (so a block before it is missing its `fi`).
    fn at_item_start(&self) -> bool {
        matches!(self.peek(), Token::Task | Token::Shape | Token::Kind)
    }

    /// Whether the next token is `end` followed, after any blank lines, by the
    /// end of the file or a new item.
    fn end_closes_item(&self) -> bool {
        if !matches!(self.peek(), Token::Ident(name) if name == "end") {
            return false;
        }
        let rest = self.tokens.get(self.pos + 1..).unwrap_or_default();
        let next = rest.iter().find(|token| **token != Token::Newline).unwrap_or(&Token::Eof);
        matches!(next, Token::Eof | Token::Task | Token::Shape | Token::Kind)
    }

    fn parse_item(&mut self) -> Result<Item> {
        self.skip_newlines();
        match self.peek() {
            Token::Task => self.parse_task().map(Item::Task),
            Token::Shape => self.parse_shape().map(Item::Shape),
            Token::Kind => self.parse_kind().map(Item::Kind),
            _ => Err(self.unexpected("'task', 'shape', or 'kind'")),
        }
    }

    fn parse_task(&mut self) -> Result<TaskDef> {
        let opened = self.peek_span();
        self.advance(); // task
        let span = self.peek_span();
        let name = self.expect_ident()?;
        let params = self.parse_field_list()?;
        let mut ret_span = span;
        let ret = if self.match_token(Token::Arrow) {
            if self.check(Token::Newline) || self.check(Token::Fi) || self.is_at_end() {
                return Err(Error::at(self.peek_span(), "expected a return type after '->'"));
            }
            ret_span = self.peek_span();
            self.parse_type()?
        } else {
            Type::Unit
        };
        self.skip_newlines();
        let body = self.parse_block()?;
        self.expect_fi("task", opened)?;
        Ok(TaskDef { name, params, ret, ret_span, body, span })
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
            fields.push(self.parse_field()?);
            self.skip_newlines();
        }
        self.expect_fi("shape", opened)?;
        Ok(ShapeDef { name, fields, span })
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
        Ok(KindDef { name, variants, span })
    }

    /// `name: type`
    fn parse_field(&mut self) -> Result<Field> {
        let span = self.peek_span();
        let name = self.expect_ident()?;
        self.expect(Token::Colon)?;
        let ty_span = self.peek_span();
        let ty = self.parse_type()?;
        Ok(Field { name, ty, span, ty_span })
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

    fn parse_type(&mut self) -> Result<Type> {
        if self.match_token(Token::Caret) {
            self.enter()?;
            let inner = self.parse_type()?;
            self.leave(1);
            return Ok(Type::Ptr(Box::new(inner)));
        }
        let name = self.expect_ident()?;
        Ok(Type::builtin(&name).unwrap_or(Type::Named(name)))
    }

    fn parse_block(&mut self) -> Result<Block> {
        self.parse_block_until(&[Token::Fi])
    }

    fn parse_block_until(&mut self, stops: &[Token]) -> Result<Block> {
        self.enter()?;
        let mut stmts = Vec::new();
        loop {
            self.skip_newlines();
            if self.is_at_end() || self.at_item_start() || stops.iter().any(|t| self.check(t.clone())) {
                break;
            }
            self.reject_stray_end()?;
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
            Token::Free => self.parse_free(),
            _ => self.parse_simple_stmt(span),
        }
    }

    /// An assignment, or an expression used as a statement.
    fn parse_simple_stmt(&mut self, span: Span) -> Result<Stmt> {
        let expr = self.parse_expr()?;
        if self.match_token(Token::Assign) {
            let rhs = self.parse_expr()?;
            Ok(Stmt::new(StmtKind::Assign(expr, rhs), span))
        } else {
            Ok(Stmt::new(StmtKind::Expr(expr), span))
        }
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

    /// `bind name [: type] = expr` or `hold name [: type] = expr`
    fn parse_binding(&mut self, immutable: bool) -> Result<Stmt> {
        self.advance(); // bind / hold
        let span = self.peek_span();
        let name = self.expect_ident()?;
        let (ty, ty_span) = if self.match_token(Token::Colon) {
            let ty_span = self.peek_span();
            (Some(self.parse_type()?), Some(ty_span))
        } else {
            (None, None)
        };
        self.expect(Token::Assign)?;
        let init = self.parse_expr()?;
        let kind = StmtKind::Binding { name, mutable: !immutable, ty, ty_span, init };
        Ok(Stmt::new(kind, span))
    }

    fn parse_when(&mut self) -> Result<Stmt> {
        let span = self.peek_span();
        self.advance(); // when
        let cond = self.parse_condition()?;
        self.skip_newlines();
        let then_block = self.parse_block_until(&[Token::Else, Token::Fi])?;
        let else_block = if self.match_token(Token::Else) {
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
            let case_span = self.peek_span();
            let tag = self.expect_ident()?;
            if self.check(Token::LParen) {
                return Err(Error::at(
                    self.peek_span(),
                    format!("a case names its bindings without parentheses, as in 'case {} value'", tag),
                ));
            }
            let mut binds = Vec::new();
            while !self.check(Token::Newline) && !self.check(Token::Fi) && !self.check(Token::Case) && !self.is_at_end() {
                let bind_span = self.peek_span();
                binds.push((self.expect_ident()?, bind_span));
            }
            self.skip_newlines();
            let body = self.parse_block_until(&[Token::Case, Token::Fi])?;
            cases.push(Case { tag, binds, body, span: case_span });
        }
        self.expect_fi("match", span)?;
        Ok(Stmt::new(StmtKind::Match(expr, cases), span))
    }

    /// `<-` or `<- value`. A send starts its line: `<-` after something else
    /// is almost always a comparison with a negative number written without a
    /// space (`a <-1`), which would otherwise end the statement and send.
    fn parse_send(&mut self) -> Result<Stmt> {
        let span = self.peek_span();
        let starts_line = self.pos == 0 || matches!(self.tokens.get(self.pos - 1), Some(Token::Newline));
        if !starts_line {
            return Err(self.misplaced_send());
        }
        self.advance(); // <-
        let ends = [Token::Newline, Token::Fi, Token::Else, Token::Case];
        if ends.iter().any(|token| self.check(token.clone())) || self.is_at_end() {
            Ok(Stmt::new(StmtKind::Send(None), span))
        } else {
            let expr = self.parse_expr()?;
            Ok(Stmt::new(StmtKind::Send(Some(expr)), span))
        }
    }

    fn parse_free(&mut self) -> Result<Stmt> {
        let span = self.peek_span();
        self.advance(); // free
        let expr = self.parse_expr()?;
        Ok(Stmt::new(StmtKind::Free(expr), span))
    }

    fn parse_expr(&mut self) -> Result<Expr> {
        self.enter()?;
        let expr = self.parse_binary(1)?;
        self.leave(1);
        Ok(expr)
    }

    /// A `when` or `loop` condition, explaining `=` written for `==`.
    fn parse_condition(&mut self) -> Result<Expr> {
        let cond = self.parse_expr()?;
        if self.check(Token::Assign) {
            return Err(Error::at(self.peek_span(), "'=' assigns; to compare, write '=='"));
        }
        Ok(cond)
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
        loop {
            if matches!(self.peek(), Token::Dot | Token::LBracket | Token::Caret) {
                self.enter()?;
                levels += 1;
            }
            if self.match_token(Token::Dot) {
                let span = self.peek_span();
                let field = self.expect_ident()?;
                expr = Expr::new(ExprKind::Field(Box::new(expr), field), span);
            } else if self.check(Token::LBracket) {
                let span = self.peek_span();
                self.advance();
                let index = self.parse_expr()?;
                self.expect(Token::RBracket)?;
                expr = Expr::new(ExprKind::Index(Box::new(expr), Box::new(index)), span);
            } else if self.check(Token::Caret) {
                let span = self.peek_span();
                self.advance();
                expr = Expr::new(ExprKind::Deref(Box::new(expr)), span);
            } else {
                break;
            }
        }
        self.leave(levels);
        Ok(expr)
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
            Token::LParen => {
                self.advance();
                let expr = self.parse_expr()?;
                self.expect(Token::RParen)?;
                return Ok(expr);
            }
            Token::Ident(name) => {
                let name = name.clone();
                self.advance();
                if self.check(Token::LBrace) {
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

    fn parse_new(&mut self) -> Result<Expr> {
        let span = self.peek_span();
        self.advance(); // new
        let ty_span = self.peek_span();
        let ty = self.parse_type()?;
        self.expect(Token::LParen)?;
        let value = self.parse_expr()?;
        self.expect(Token::RParen)?;
        Ok(Expr::new(ExprKind::New { ty, ty_span, value: Box::new(value) }, span))
    }

    fn parse_struct_init(&mut self, name: String) -> Result<ExprKind> {
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
            token => match KEYWORDS.iter().find(|(_, keyword)| keyword == token) {
                Some((word, _)) => Err(Error::at(
                    self.peek_span(),
                    format!("expected a name, got '{}', which is a reserved word", word),
                )),
                None => Err(self.unexpected("a name")),
            },
        }
    }
}
