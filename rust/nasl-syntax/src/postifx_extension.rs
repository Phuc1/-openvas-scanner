use crate::{
    operator_precedence_parser::{Lexer, Operator},
    parser::{AssignCategory, Statement, TokenError},
    token::{Category, Token},
};
pub(crate) trait Postfix {
    fn needs_postfix(&self, op:Operator) -> bool;
    fn postfix_statement(
        &mut self,
        op: Operator,
        token: Token,
        lhs: Statement,
        abort: Category,
    ) -> Option<Result<Statement, TokenError>>;
}

impl<'a> Lexer<'a> {
    fn flatten_parameter(
        &mut self,
        lhs: Statement,
        abort: Category,
    ) -> Result<Statement, TokenError> {
        let mut lhs = match lhs {
            Statement::Parameter(x) => x,
            x => vec![x],
        };
        match self.expression_bp(0, abort)? {
            Statement::Parameter(mut x) => lhs.append(&mut x),
            x => lhs.push(x),
        };
        Ok(Statement::Parameter(lhs))
    }
}

impl<'a> Postfix for Lexer<'a> {
    fn postfix_statement(
        &mut self,
        op: Operator,
        token: Token,
        lhs: Statement,
        abort: Category,
    ) -> Option<Result<Statement, TokenError>> {
        match op {
            Operator::Grouping(Category::Comma) => Some(self.flatten_parameter(lhs, abort)),
            Operator::AssignOperator(_, operator, amount) => match lhs {
                Statement::Variable(token) => Some(Ok(Statement::Assign(
                    AssignCategory::ReturnAssign,
                    token,
                    Box::new(Statement::Operator(
                        operator,
                        vec![Statement::Variable(token), Statement::RawNumber(amount)],
                    )),
                ))),
                _ => Some(Err(TokenError::unexpected_token(token))),
            },
            _ => None,
        }
    }

    fn needs_postfix(&self, op:Operator) -> bool {
        matches!(op, Operator::Grouping(Category::Comma) | Operator::AssignOperator(_, _, _))
    }
}
