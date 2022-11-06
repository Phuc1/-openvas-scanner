use crate::{
    infix_extension::Infix,
    parser::{AssignCategory, Statement, TokenError},
    postifx_extension::Postfix,
    prefix_extension::Prefix,
    token::{self, Category, Keyword, Token, Tokenizer}, operation::Operation,
};

pub(crate) struct Lexer<'a> {
    tokenizer: Tokenizer<'a>,
    pub(crate) previous_token: Option<Token>,
}


impl<'a> Lexer<'a> {
    fn new(tokenizer: Tokenizer<'a>) -> Lexer<'a> {
        Lexer {
            tokenizer,
            previous_token: None,
        }
    }

    pub(crate) fn next(&mut self) -> Option<Token> {
        self.tokenizer.next()
    }

    pub(crate) fn expression_bp(
        &mut self,
        min_bp: u8,
        abort: Category,
    ) -> Result<Statement, TokenError> {
        let token = self
            .previous_token
            .or_else(|| self.next())
            .ok_or_else(|| TokenError::unexpected_end("parsing prefix statement"))?;
        if token.category() == abort {
            return Ok(Statement::NoOp(Some(token)));
        }

        let mut lhs = self.prefix_statement(token, abort)?;
        loop {
            let token = {
                let r = match self.previous_token {
                    None => self.next(),
                    x => {
                        self.previous_token = None;
                        x
                    }
                };
                match r {
                    Some(x) => x,
                    None => break,
                }
            };
            if token.category() == abort {
                self.previous_token = Some(token);
                break;
            }
            let op = Operation::new(token).ok_or_else(|| TokenError::unexpected_token(token))?;

            if self.needs_postfix(op) {
                let stmt = self
                    .postfix_statement(op, token, lhs, abort)
                    .expect("needs postfix should have been validated before")?;
                lhs = stmt;
                continue;
            }

            if let Some(min_bp_reached) = self.handle_infix(op, min_bp) {
                if !min_bp_reached {
                    self.previous_token = Some(token);
                    break;
                }
                lhs = self.infix_statement(op, token, lhs, abort)?;
            }
        }

        Ok(lhs)
    }
}

pub fn expression(tokenizer: Tokenizer<'_>) -> Result<Statement, TokenError> {
    //let tokenizer = Tokenizer::new(code);
    let mut lexer = Lexer::new(tokenizer);
    let init = lexer.expression_bp(0, Category::Semicolon)?;
    Ok(init)
}

