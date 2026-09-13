use crate::ast::*;
use crate::error::{Error, Result};
use crate::lexer::Token;

pub struct Parser {
    tokens: Vec<Token>,
    spans: Vec<Span>,
    pos: usize,
}

impl Parser {
    /// `tokens` and `spans` are parallel, as produced by `Lexer::tokenize`.
    pub fn new(tokens: Vec<Token>, spans: Vec<Span>) -> Self {
        Parser { tokens, spans, pos: 0 }
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

    fn unexpected(&self, expected: &str) -> Error {
        Error::at(self.peek_span(), format!("expected {}, got {}", expected, self.peek().describe()))
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
        self.advance(); // task
        let span = self.peek_span();
        let name = self.expect_ident()?;
        let params = self.parse_field_list()?;
        let mut ret_span = span;
        let ret = if self.match_token(Token::Arrow) {
            if self.check(Token::Newline) || self.check(Token::Fi) {
                Type::Unit
            } else {
                ret_span = self.peek_span();
                self.parse_type()?
            }
        } else {
            Type::Unit
        };
        self.skip_newlines();
        let body = self.parse_block()?;
        self.expect(Token::Fi)?;
        Ok(TaskDef { name, params, ret, ret_span, body, span })
    }

    fn parse_shape(&mut self) -> Result<ShapeDef> {
        self.advance(); // shape
        let span = self.peek_span();
        let name = self.expect_ident()?;
        self.skip_newlines();
        let mut fields = Vec::new();
        while !self.check(Token::Fi) && !self.is_at_end() {
            self.skip_newlines();
            if self.check(Token::Fi) { break; }
            self.reject_stray_end()?;
            fields.push(self.parse_field()?);
            self.skip_newlines();
        }
        self.expect(Token::Fi)?;
        Ok(ShapeDef { name, fields, span })
    }

    fn parse_kind(&mut self) -> Result<KindDef> {
        self.advance(); // kind
        let span = self.peek_span();
        let name = self.expect_ident()?;
        self.skip_newlines();
        let mut variants = Vec::new();
        while !self.check(Token::Fi) && !self.is_at_end() {
            self.skip_newlines();
            if self.check(Token::Fi) { break; }
            self.reject_stray_end()?;
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
        self.expect(Token::Fi)?;
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
            let inner = self.parse_type()?;
            return Ok(Type::Ptr(Box::new(inner)));
        }
        let name = self.expect_ident()?;
        Ok(match name.as_str() {
            "int" => Type::Int,
            "float" => Type::Float,
            "bool" => Type::Bool,
            "text" => Type::Text,
            "unit" => Type::Unit,
            _ => Type::Named(name),
        })
    }

    fn parse_block(&mut self) -> Result<Block> {
        self.parse_block_until(&[Token::Fi])
    }

    fn parse_block_until(&mut self, stops: &[Token]) -> Result<Block> {
        let mut stmts = Vec::new();
        loop {
            self.skip_newlines();
            if self.is_at_end() || stops.iter().any(|t| self.check(t.clone())) {
                break;
            }
            self.reject_stray_end()?;
            stmts.push(self.parse_stmt()?);
            self.skip_newlines();
        }
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
            Token::Free => self.parse_free(),
            _ => {
                let expr = self.parse_expr()?;
                if self.match_token(Token::Assign) {
                    let rhs = self.parse_expr()?;
                    Ok(Stmt::new(StmtKind::Assign(expr, rhs), span))
                } else {
                    Ok(Stmt::new(StmtKind::Expr(expr), span))
                }
            }
        }
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
        let cond = self.parse_expr()?;
        self.skip_newlines();
        let then_block = self.parse_block_until(&[Token::Else, Token::Fi])?;
        let else_block = if self.match_token(Token::Else) {
            self.skip_newlines();
            Some(self.parse_block_until(&[Token::Fi])?)
        } else {
            None
        };
        self.expect(Token::Fi)?;
        Ok(Stmt::new(StmtKind::When(cond, then_block, else_block), span))
    }

    fn parse_loop(&mut self) -> Result<Stmt> {
        let span = self.peek_span();
        self.advance(); // loop
        let cond = self.parse_expr()?;
        self.skip_newlines();
        let body = self.parse_block()?;
        self.expect(Token::Fi)?;
        Ok(Stmt::new(StmtKind::Loop(cond, body), span))
    }

    fn parse_each(&mut self) -> Result<Stmt> {
        self.advance(); // each
        let span = self.peek_span();
        let name = self.expect_ident()?;
        self.expect(Token::In)?;
        let range = self.parse_expr()?;
        self.skip_newlines();
        let body = self.parse_block()?;
        self.expect(Token::Fi)?;
        Ok(Stmt::new(StmtKind::Each(name, range, body), span))
    }

    fn parse_match(&mut self) -> Result<Stmt> {
        let span = self.peek_span();
        self.advance(); // match
        let expr = self.parse_expr()?;
        self.skip_newlines();
        let mut cases = Vec::new();
        while !self.check(Token::Fi) && !self.is_at_end() {
            self.skip_newlines();
            if self.check(Token::Fi) { break; }
            self.reject_stray_end()?;
            self.expect(Token::Case)?;
            let case_span = self.peek_span();
            let tag = self.expect_ident()?;
            let mut binds = Vec::new();
            while !self.check(Token::Newline) && !self.check(Token::Fi) && !self.check(Token::Case) && !self.is_at_end() {
                let bind_span = self.peek_span();
                binds.push((self.expect_ident()?, bind_span));
            }
            self.skip_newlines();
            let body = self.parse_block_until(&[Token::Case, Token::Fi])?;
            cases.push(Case { tag, binds, body, span: case_span });
        }
        self.expect(Token::Fi)?;
        Ok(Stmt::new(StmtKind::Match(expr, cases), span))
    }

    fn parse_send(&mut self) -> Result<Stmt> {
        let span = self.peek_span();
        self.advance(); // send
        if self.check(Token::Newline) || self.check(Token::Fi) || self.is_at_end() {
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
        self.parse_or()
    }

    /// Parses a left-associative chain of the binary operators in `ops`, with
    /// operands parsed by `operand`. The resulting span is the operator's.
    fn parse_binary_chain(
        &mut self,
        ops: &[(Token, BinaryOp)],
        operand: fn(&mut Self) -> Result<Expr>,
    ) -> Result<Expr> {
        let mut left = operand(self)?;
        'chain: loop {
            for (tok, op) in ops {
                if self.check(tok.clone()) {
                    let span = self.peek_span();
                    self.advance();
                    let right = operand(self)?;
                    left = Expr::new(ExprKind::Binary(*op, Box::new(left), Box::new(right)), span);
                    continue 'chain;
                }
            }
            break;
        }
        Ok(left)
    }

    fn parse_or(&mut self) -> Result<Expr> {
        self.parse_binary_chain(&[(Token::Or, BinaryOp::Or)], Self::parse_and)
    }

    fn parse_and(&mut self) -> Result<Expr> {
        self.parse_binary_chain(&[(Token::And, BinaryOp::And)], Self::parse_equality)
    }

    fn parse_equality(&mut self) -> Result<Expr> {
        self.parse_binary_chain(
            &[(Token::Eq, BinaryOp::Eq), (Token::Ne, BinaryOp::Ne)],
            Self::parse_comparison,
        )
    }

    fn parse_comparison(&mut self) -> Result<Expr> {
        self.parse_binary_chain(
            &[
                (Token::Lt, BinaryOp::Lt),
                (Token::Gt, BinaryOp::Gt),
                (Token::Le, BinaryOp::Le),
                (Token::Ge, BinaryOp::Ge),
            ],
            Self::parse_range,
        )
    }

    fn parse_range(&mut self) -> Result<Expr> {
        let left = self.parse_add()?;
        if self.check(Token::Range) {
            let span = self.peek_span();
            self.advance();
            let right = self.parse_add()?;
            Ok(Expr::new(ExprKind::Range(Box::new(left), Box::new(right)), span))
        } else {
            Ok(left)
        }
    }

    fn parse_add(&mut self) -> Result<Expr> {
        self.parse_binary_chain(
            &[(Token::Plus, BinaryOp::Add), (Token::Minus, BinaryOp::Sub)],
            Self::parse_mul,
        )
    }

    fn parse_mul(&mut self) -> Result<Expr> {
        self.parse_binary_chain(
            &[
                (Token::Star, BinaryOp::Mul),
                (Token::Slash, BinaryOp::Div),
                (Token::Percent, BinaryOp::Mod),
            ],
            Self::parse_unary,
        )
    }

    fn parse_unary(&mut self) -> Result<Expr> {
        let span = self.peek_span();
        if self.match_token(Token::Minus) {
            let expr = self.parse_unary()?;
            Ok(Expr::new(ExprKind::Unary(UnaryOp::Neg, Box::new(expr)), span))
        } else if self.match_token(Token::Not) {
            let expr = self.parse_unary()?;
            Ok(Expr::new(ExprKind::Unary(UnaryOp::Not, Box::new(expr)), span))
        } else {
            self.parse_postfix()
        }
    }

    fn parse_postfix(&mut self) -> Result<Expr> {
        let mut expr = self.parse_primary()?;
        loop {
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
            _ => Err(self.unexpected("a name")),
        }
    }
}
