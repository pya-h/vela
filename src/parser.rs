use crate::ast::*;
use crate::error::{Error, Result};
use crate::lexer::{Token};

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    lines: Vec<usize>,
    cols: Vec<usize>,
}

impl Parser {
    pub fn new(tokens: Vec<Token>, lines: Vec<usize>, cols: Vec<usize>) -> Self {
        Parser { tokens, pos: 0, lines, cols }
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

    fn peek_loc(&self) -> (usize, usize) {
        (self.lines.get(self.pos).copied().unwrap_or(0), self.cols.get(self.pos).copied().unwrap_or(0))
    }

    fn advance(&mut self) -> Token {
        let tok = self.tokens[self.pos].clone();
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

    fn expect(&mut self, expected: Token) -> Result<()> {
        let loc = self.peek_loc();
        if self.peek() == &expected {
            self.advance();
            Ok(())
        } else {
            Err(Error::new(loc.0, loc.1, format!("expected {:?}, got {:?}", expected, self.peek())))
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

    fn parse_item(&mut self) -> Result<Item> {
        self.skip_newlines();
        match self.peek() {
            Token::Task => self.parse_task().map(Item::Task),
            Token::Shape => self.parse_shape().map(Item::Shape),
            Token::Kind => self.parse_kind().map(Item::Kind),
            _ => {
                let loc = self.peek_loc();
                Err(Error::new(loc.0, loc.1, "expected task, shape, or kind"))
            }
        }
    }

    fn parse_task(&mut self) -> Result<TaskDef> {
        self.advance(); // task
        let name = self.expect_ident()?;
        let params = self.parse_param_list()?;
        let ret = if self.match_token(Token::Arrow) {
            if self.check(Token::Newline) || self.check(Token::End) {
                Type::Unit
            } else {
                self.parse_type()?
            }
        } else {
            Type::Unit
        };
        self.skip_newlines();
        let body = self.parse_block()?;
        self.expect(Token::End)?;
        Ok(TaskDef { name, params, ret, body })
    }

    fn parse_shape(&mut self) -> Result<ShapeDef> {
        self.advance(); // shape
        let name = self.expect_ident()?;
        self.skip_newlines();
        let mut fields = Vec::new();
        while !self.check(Token::End) && !self.is_at_end() {
            self.skip_newlines();
            if self.check(Token::End) { break; }
            let field_name = self.expect_ident()?;
            self.expect(Token::Colon)?;
            let ty = self.parse_type()?;
            fields.push((field_name, ty));
            self.skip_newlines();
        }
        self.expect(Token::End)?;
        Ok(ShapeDef { name, fields })
    }

    fn parse_kind(&mut self) -> Result<KindDef> {
        self.advance(); // kind
        let name = self.expect_ident()?;
        self.skip_newlines();
        let mut variants = Vec::new();
        while !self.check(Token::End) && !self.is_at_end() {
            self.skip_newlines();
            if self.check(Token::End) { break; }
            let tag = self.expect_ident()?;
            let payload = if self.check(Token::LParen) {
                self.parse_param_list()?
            } else {
                Vec::new()
            };
            variants.push(Variant { tag, payload });
            self.skip_newlines();
        }
        self.expect(Token::End)?;
        Ok(KindDef { name, variants })
    }

    fn parse_param_list(&mut self) -> Result<Vec<(String, Type)>> {
        self.expect(Token::LParen)?;
        let mut params = Vec::new();
        if !self.check(Token::RParen) {
            loop {
                let name = self.expect_ident()?;
                self.expect(Token::Colon)?;
                let ty = self.parse_type()?;
                params.push((name, ty));
                if !self.match_token(Token::Comma) {
                    break;
                }
            }
        }
        self.expect(Token::RParen)?;
        Ok(params)
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
        self.parse_block_until(&[Token::End])
    }

    fn parse_block_until(&mut self, stops: &[Token]) -> Result<Block> {
        let mut stmts = Vec::new();
        loop {
            self.skip_newlines();
            if self.is_at_end() || stops.iter().any(|t| self.check(t.clone())) {
                break;
            }
            stmts.push(self.parse_stmt()?);
            self.skip_newlines();
        }
        Ok(stmts)
    }

    fn check(&self, tok: Token) -> bool {
        self.peek() == &tok
    }

    fn parse_stmt(&mut self) -> Result<Stmt> {
        self.skip_newlines();
        let _loc = self.peek_loc();
        match self.peek() {
            Token::Bind => self.parse_bind(),
            Token::Hold => self.parse_hold(),
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
                    Ok(Stmt::Assign(expr, rhs))
                } else {
                    Ok(Stmt::Expr(expr))
                }
            }
        }
    }

    fn parse_bind(&mut self) -> Result<Stmt> {
        self.advance(); // bind
        let name = self.expect_ident()?;
        let ty = if self.match_token(Token::Colon) {
            self.parse_type()?
        } else {
            Type::Unit // placeholder, inferred later
        };
        self.expect(Token::Assign)?;
        let expr = self.parse_expr()?;
        Ok(Stmt::Bind(name, ty, Some(expr)))
    }

    fn parse_hold(&mut self) -> Result<Stmt> {
        self.advance(); // hold
        let name = self.expect_ident()?;
        let ty = if self.match_token(Token::Colon) {
            self.parse_type()?
        } else {
            Type::Unit
        };
        self.expect(Token::Assign)?;
        let expr = self.parse_expr()?;
        Ok(Stmt::Hold(name, ty, Some(expr)))
    }

    fn parse_when(&mut self) -> Result<Stmt> {
        self.advance(); // when
        let cond = self.parse_expr()?;
        self.skip_newlines();
        let then_block = self.parse_block_until(&[Token::Else, Token::End])?;
        let else_block = if self.match_token(Token::Else) {
            self.skip_newlines();
            Some(self.parse_block_until(&[Token::End])?)
        } else {
            None
        };
        self.expect(Token::End)?;
        Ok(Stmt::When(cond, then_block, else_block))
    }

    fn parse_loop(&mut self) -> Result<Stmt> {
        self.advance(); // loop
        let cond = self.parse_expr()?;
        self.skip_newlines();
        let body = self.parse_block()?;
        self.expect(Token::End)?;
        Ok(Stmt::Loop(cond, body))
    }

    fn parse_each(&mut self) -> Result<Stmt> {
        self.advance(); // each
        let name = self.expect_ident()?;
        self.expect(Token::In)?;
        let range = self.parse_expr()?;
        self.skip_newlines();
        let body = self.parse_block()?;
        self.expect(Token::End)?;
        Ok(Stmt::Each(name, range, body))
    }

    fn parse_match(&mut self) -> Result<Stmt> {
        self.advance(); // match
        let expr = self.parse_expr()?;
        self.skip_newlines();
        let mut cases = Vec::new();
        while !self.check(Token::End) && !self.is_at_end() {
            self.skip_newlines();
            if self.check(Token::End) { break; }
            self.expect(Token::Case)?;
            let tag = self.expect_ident()?;
            let mut binds = Vec::new();
            while !self.check(Token::Newline) && !self.check(Token::End) && !self.check(Token::Case) && !self.is_at_end() {
                binds.push(self.expect_ident()?);
            }
            self.skip_newlines();
            let body = self.parse_block_until(&[Token::Case, Token::End])?;
            cases.push(Case { tag, binds, body });
        }
        self.expect(Token::End)?;
        Ok(Stmt::Match(expr, cases))
    }

    fn parse_send(&mut self) -> Result<Stmt> {
        self.advance(); // send
        if self.check(Token::Newline) || self.check(Token::End) {
            Ok(Stmt::Send(None))
        } else {
            let expr = self.parse_expr()?;
            Ok(Stmt::Send(Some(expr)))
        }
    }

    fn parse_free(&mut self) -> Result<Stmt> {
        self.advance(); // free
        let expr = self.parse_expr()?;
        Ok(Stmt::Free(expr))
    }

    fn parse_expr(&mut self) -> Result<Expr> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Expr> {
        let mut left = self.parse_and()?;
        while self.match_token(Token::Or) {
            let right = self.parse_and()?;
            left = Expr::Binary(BinaryOp::Or, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr> {
        let mut left = self.parse_equality()?;
        while self.match_token(Token::And) {
            let right = self.parse_equality()?;
            left = Expr::Binary(BinaryOp::And, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_equality(&mut self) -> Result<Expr> {
        let mut left = self.parse_comparison()?;
        loop {
            if self.match_token(Token::Eq) {
                let right = self.parse_comparison()?;
                left = Expr::Binary(BinaryOp::Eq, Box::new(left), Box::new(right));
            } else if self.match_token(Token::Ne) {
                let right = self.parse_comparison()?;
                left = Expr::Binary(BinaryOp::Ne, Box::new(left), Box::new(right));
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_comparison(&mut self) -> Result<Expr> {
        let mut left = self.parse_range()?;
        loop {
            if self.match_token(Token::Lt) {
                let right = self.parse_range()?;
                left = Expr::Binary(BinaryOp::Lt, Box::new(left), Box::new(right));
            } else if self.match_token(Token::Gt) {
                let right = self.parse_range()?;
                left = Expr::Binary(BinaryOp::Gt, Box::new(left), Box::new(right));
            } else if self.match_token(Token::Le) {
                let right = self.parse_range()?;
                left = Expr::Binary(BinaryOp::Le, Box::new(left), Box::new(right));
            } else if self.match_token(Token::Ge) {
                let right = self.parse_range()?;
                left = Expr::Binary(BinaryOp::Ge, Box::new(left), Box::new(right));
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_range(&mut self) -> Result<Expr> {
        let left = self.parse_add()?;
        if self.match_token(Token::Range) {
            let right = self.parse_add()?;
            Ok(Expr::Range(Box::new(left), Box::new(right)))
        } else {
            Ok(left)
        }
    }

    fn parse_add(&mut self) -> Result<Expr> {
        let mut left = self.parse_mul()?;
        loop {
            if self.match_token(Token::Plus) {
                let right = self.parse_mul()?;
                left = Expr::Binary(BinaryOp::Add, Box::new(left), Box::new(right));
            } else if self.match_token(Token::Minus) {
                let right = self.parse_mul()?;
                left = Expr::Binary(BinaryOp::Sub, Box::new(left), Box::new(right));
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_mul(&mut self) -> Result<Expr> {
        let mut left = self.parse_unary()?;
        loop {
            if self.match_token(Token::Star) {
                let right = self.parse_unary()?;
                left = Expr::Binary(BinaryOp::Mul, Box::new(left), Box::new(right));
            } else if self.match_token(Token::Slash) {
                let right = self.parse_unary()?;
                left = Expr::Binary(BinaryOp::Div, Box::new(left), Box::new(right));
            } else if self.match_token(Token::Percent) {
                let right = self.parse_unary()?;
                left = Expr::Binary(BinaryOp::Mod, Box::new(left), Box::new(right));
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr> {
        if self.match_token(Token::Minus) {
            let expr = self.parse_unary()?;
            Ok(Expr::Unary(UnaryOp::Neg, Box::new(expr)))
        } else if self.match_token(Token::Not) {
            let expr = self.parse_unary()?;
            Ok(Expr::Unary(UnaryOp::Not, Box::new(expr)))
        } else {
            self.parse_postfix()
        }
    }

    fn parse_postfix(&mut self) -> Result<Expr> {
        let mut expr = self.parse_primary()?;
        loop {
            if self.match_token(Token::Dot) {
                let field = self.expect_ident()?;
                expr = Expr::Field(Box::new(expr), field);
            } else if self.match_token(Token::LBracket) {
                let index = self.parse_expr()?;
                self.expect(Token::RBracket)?;
                expr = Expr::Index(Box::new(expr), Box::new(index));
            } else if self.match_token(Token::Caret) {
                expr = Expr::Deref(Box::new(expr));
            } else {
                break;
            }
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr> {
        let loc = self.peek_loc();
        match self.peek() {
            Token::Int(n) => { let v = *n; self.advance(); Ok(Expr::IntLit(v)) }
            Token::Float(n) => { let v = *n; self.advance(); Ok(Expr::FloatLit(v)) }
            Token::Text(s) => { let v = s.clone(); self.advance(); Ok(Expr::TextLit(v)) }
            Token::True => { self.advance(); Ok(Expr::BoolLit(true)) }
            Token::False => { self.advance(); Ok(Expr::BoolLit(false)) }
            Token::None => { self.advance(); Ok(Expr::NoneLit) }
            Token::New => self.parse_new(),
            Token::LParen => {
                self.advance();
                let expr = self.parse_expr()?;
                self.expect(Token::RParen)?;
                Ok(expr)
            }
            Token::Ident(name) => {
                let name = name.clone();
                self.advance();
                if self.check(Token::LBrace) {
                    self.parse_struct_init(&name)
                } else if self.check(Token::LParen) {
                    let args = self.parse_arg_list()?;
                    Ok(Expr::Call(name, args))
                } else {
                    Ok(Expr::Var(name))
                }
            }
            _ => Err(Error::new(loc.0, loc.1, format!("unexpected token {:?}", self.peek()))),
        }
    }

    fn parse_new(&mut self) -> Result<Expr> {
        self.advance(); // new
        let ty = self.parse_type()?;
        self.expect(Token::LParen)?;
        let expr = self.parse_expr()?;
        self.expect(Token::RParen)?;
        Ok(Expr::New(Box::new(ty), Box::new(expr)))
    }

    fn parse_struct_init(&mut self, name: &str) -> Result<Expr> {
        self.advance(); // {
        let mut fields = Vec::new();
        if !self.check(Token::RBrace) {
            loop {
                let field_name = self.expect_ident()?;
                self.expect(Token::Colon)?;
                let expr = self.parse_expr()?;
                fields.push((field_name, expr));
                if !self.match_token(Token::Comma) {
                    break;
                }
            }
        }
        self.expect(Token::RBrace)?;
        Ok(Expr::StructInit(name.to_string(), fields))
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
        let loc = self.peek_loc();
        match self.peek() {
            Token::Ident(s) => { let v = s.clone(); self.advance(); Ok(v) }
            _ => Err(Error::new(loc.0, loc.1, format!("expected identifier, got {:?}", self.peek()))),
        }
    }
}
