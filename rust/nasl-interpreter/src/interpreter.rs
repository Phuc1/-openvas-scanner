// SPDX-FileCopyrightText: 2023 Greenbone AG
//
// SPDX-License-Identifier: GPL-2.0-or-later WITH x11vnc-openssl-exception

use std::{collections::HashMap, io};

use nasl_syntax::{
    IdentifierType, LoadError, NaslValue, Statement, StatementKind::*, Token, TokenCategory,
};
use storage::StorageError;

use crate::{
    assign::AssignExtension,
    call::CallExtension,
    declare::{DeclareFunctionExtension, DeclareVariableExtension},
    loop_extension::LoopExtension,
    operator::OperatorExtension,
    InterpretError, InterpretErrorKind,
};

use nasl_builtin_utils::{Context, ContextType, Register};

/// Is used to identify the depth of the current statement
///
/// Initial call of retry_resolce sets the first element all others are only
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Position {
    index: Vec<usize>,
}

impl Position {
    pub fn new(index: usize) -> Self {
        Self { index: vec![index] }
    }

    pub fn up(&mut self) {
        self.index.push(0);
    }

    pub fn down(&mut self) -> Option<usize> {
        self.index.pop()
    }

    pub fn current_init_statement(&self) -> Self {
        Self {
            index: vec![*self.index.first().unwrap_or(&0)],
        }
    }
}

/// Used to interpret a Statement
pub struct Interpreter<'a, K> {
    pub(crate) registrat: Register,
    pub(crate) ctxconfigs: &'a Context<'a, K>,
    pub(crate) position: Position,
    pub(crate) skip_until_return: Option<(Position, NaslValue)>,
    pub(crate) forked_interpreter: Vec<Interpreter<'a, K>>,
    pub(crate) forked_interpreter_index: usize,
}

/// Interpreter always returns a NaslValue or an InterpretError
///
/// When a result does not contain a value than NaslValue::Null must be returned.
pub type InterpretResult = Result<NaslValue, InterpretError>;

impl<'a, K> Interpreter<'a, K>
where
    K: AsRef<str>,
{
    // TODO rename to new
    // TODO: write an easier to use function for code: &str
    /// Creates a new Interpreter
    pub fn new(register: Register, ctxconfigs: &'a Context<K>) -> Self {
        Interpreter {
            registrat: register.clone(),
            ctxconfigs,
            position: Position::new(0),
            skip_until_return: None,
            forked_interpreter: Vec::with_capacity(10),
            forked_interpreter_index: 0,
        }
    }

    pub(crate) fn identifier(token: &Token) -> Result<String, InterpretError> {
        match token.category() {
            TokenCategory::Identifier(IdentifierType::Undefined(x)) => Ok(x.to_owned()),
            cat => Err(InterpretError::wrong_category(cat)),
        }
    }

    /// May return the next interpreter to run against that statement
    ///
    /// When the interpreter are done a None will be returned. Afterwards it will begin at at 0
    /// again. This is done to inform the caller that all intepreter interpret this statement and
    /// the next Statement can be executed.
    pub fn next_interpreter(&mut self) -> Option<&mut Interpreter<'a, K>> {
        let result = self
            .forked_interpreter
            .get_mut(self.forked_interpreter_index);
        if result.is_none() && self.forked_interpreter_index > 0 {
            self.forked_interpreter_index = 0;
        } else {
            self.forked_interpreter_index += 1;
        }
        result
    }

    /// Includes a script into to the current runtime by executing it and share the register as
    /// well as DB of the current runtime.
    ///
    // NOTE: This is currently optimized for interpreting runs, but it is very inefficient if we want to
    // switch to a jitc approach or do parallelization of statements within a script. For that it
    // would be necessary to include the statements within a statement list of a script prior of
    // execution. In the current usage (2024-04-02) it would be overkill, but I'm writing a note as
    // I think this can be easily overlooked.
    fn include(&mut self, name: &Statement) -> InterpretResult {
        match self.resolve(name)? {
            NaslValue::String(key) => {
                let code = self.ctxconfigs.loader().load(&key)?;

                let mut inter = Interpreter::new(self.registrat.clone(), self.ctxconfigs);
                let result = nasl_syntax::parse(&code)
                    .map(|parsed| match parsed {
                        Ok(stmt) => inter.resolve(&stmt),
                        Err(err) => Err(InterpretError::include_syntax_error(&key, err)),
                    })
                    .find(|e| e.is_err());
                match result {
                    Some(e) => e,
                    None => {
                        self.registrat = inter.registrat.clone();

                        Ok(NaslValue::Null)
                    }
                }
            }
            _ => Err(InterpretError::unsupported(name, "string")),
        }
    }

    /// Changes the internal position and tries to interpret a statement while retrying n times on specific error
    ///
    /// When encountering a retrievable error:
    /// - LoadError(Retry(_))
    /// - StorageError(Retry(_))
    /// - IOError(Interrupted(_))
    ///
    /// then it retries the statement for a given max_attempts times.
    ///
    /// When max_attempts is set to 0 it will it execute it once.
    pub fn retry_resolve_next(&mut self, stmt: &Statement, max_attempts: usize) -> InterpretResult {
        //self.position = Position::new(*self.position.index.last().unwrap_or(&0));
        //
        if let Some(last) = self.position.index.last_mut() {
            *last += 1;
        }
        self.retry_resolve(stmt, max_attempts)
    }

    /// Tries to interpret a statement and retries n times on a retry error
    ///
    /// When encountering a retrievable error:
    /// - LoadError(Retry(_))
    /// - StorageError(Retry(_))
    /// - IOError(Interrupted(_))
    ///
    /// then it retries the statement for a given max_attempts times.
    ///
    /// When max_attempts is set to 0 it will it execute it once.
    pub fn retry_resolve(&mut self, stmt: &Statement, max_attempts: usize) -> InterpretResult {
        match self.resolve(stmt) {
            Ok(x) => Ok(x),
            Err(e) => {
                if max_attempts > 0 {
                    match e.kind {
                        InterpretErrorKind::LoadError(LoadError::Retry(_))
                        | InterpretErrorKind::IOError(io::ErrorKind::Interrupted)
                        | InterpretErrorKind::StorageError(StorageError::Retry(_)) => {
                            self.retry_resolve_next(stmt, max_attempts - 1)
                        }
                        _ => Err(e),
                    }
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Interprets a Statement
    pub(crate) fn resolve(&mut self, statement: &Statement) -> InterpretResult {
        self.position.up();
        tracing::trace!(position=?self.position, statement=statement.to_string(), "executing");
        // TODO: should actually skip until it is reached, return value when it is reached and not
        // to NONE.
        if let Some((cp, rv)) = &self.skip_until_return {
            tracing::trace!(position=?self.position, check_position=?cp, "verify position");
            tracing::trace!(return=?rv, "skip execution");
            self.position.down();
            if cp == &self.position {
                tracing::trace!(return=?rv, "returning");
                let rv = rv.clone();
                self.skip_until_return = None;
                return Ok(rv);
            } else {
                return Ok(NaslValue::Null);
            }
        }

        let results = {
            match statement.kind(){
            Array(position) => {
                let name = Self::identifier(statement.start())?;
                let val = self
                    .registrat
                    .named(&name)
                    .unwrap_or(&ContextType::Value(NaslValue::Null));
                let val = val.clone();

                match (position, val) {
                    (None, ContextType::Value(v)) => Ok(v),
                    (Some(p), ContextType::Value(NaslValue::Array(x))) => {
                        let p: &Statement = p;
                        let position = self.resolve(p)?;
                        let position = i64::from(&position) as usize;
                        let result = x.get(position).unwrap_or(&NaslValue::Null);
                        Ok(result.clone())
                    }
                    (Some(p), ContextType::Value(NaslValue::Dict(x))) => {
                        let position = self.resolve(p)?.to_string();
                        let result = x.get(&position).unwrap_or(&NaslValue::Null);
                        Ok(result.clone())
                    }
                    (Some(_), ContextType::Value(NaslValue::Null)) => Ok(NaslValue::Null),
                    (Some(p), _) => Err(InterpretError::unsupported(p, "array")),
                    (None, ContextType::Function(_, _)) => {
                        Err(InterpretError::unsupported(statement, "variable"))
                    }
                }
            }
            Exit(stmt) => {
                let rc = self.resolve(stmt)?;
                match rc {
                    NaslValue::Number(rc) => Ok(NaslValue::Exit(rc)),
                    _ => Err(InterpretError::unsupported(stmt, "numeric")),
                }
            }
            Return(stmt) => {
                let rc = self.resolve(stmt)?;
                Ok(NaslValue::Return(Box::new(rc)))
            }
            Include(inc ) => self.include(inc),
            NamedParameter(..) => {
                unreachable!("named parameter should not be an executable statement.")
            }
            For(assignment, condition, update, body) => {
                self.for_loop(assignment, condition, update, body)
            }
            While(condition, body) => self.while_loop(condition, body),
            Repeat(body, condition) => self.repeat_loop(body, condition),
            ForEach(variable, iterable, body) => self.for_each_loop(variable, iterable, body),
            FunctionDeclaration(name, args, exec) => self.declare_function(name, args.children(), exec),
            Primitive => TryFrom::try_from(statement.as_token()).map_err(|e: TokenCategory| e.into()),
            Variable => {
                let name: NaslValue = TryFrom::try_from(statement.as_token())?;
                match self.registrat.named(&name.to_string()) {
                    Some(ContextType::Value(result)) => Ok(result.clone()),
                    None => Ok(NaslValue::Null),
                    Some(ContextType::Function(_, _)) => {
                        Err(InterpretError::unsupported(statement, "variable"))
                    }
                }
            }
            Call(arguments) => self.call(statement.as_token(), arguments.children()),
            Declare(stmts) => self.declare_variable(statement.as_token(), stmts),
            // array creation
            Parameter(x) => {
                let mut result = vec![];
                for stmt in x {
                    let val = self.resolve(stmt)?;
                    result.push(val);
                }
                Ok(NaslValue::Array(result))
            }
            Assign(cat, order, left, right) => self.assign(cat, order, left, right),
            Operator(sign, stmts) => self.operator(sign, stmts),
            If(condition, if_block, _, else_block) => match self.resolve(condition) {
                Ok(value) => {
                    if bool::from(value) {
                        return self.resolve(if_block);
                    } else if let Some(else_block) = else_block {
                        return self.resolve(else_block.as_ref());
                    }
                    Ok(NaslValue::Null)
                }
                Err(err) => Err(err),
            },
            Block(blocks) => {
                self.registrat.create_child(HashMap::default());
                for stmt in blocks {
                    match self.resolve(stmt) {
                        Ok(x) => {
                            if matches!(
                                x,
                                NaslValue::Exit(_)
                                    | NaslValue::Return(_)
                                    | NaslValue::Break
                                    | NaslValue::Continue
                            ) {
                                self.registrat.drop_last();
                                return Ok(x);
                            }
                        }
                        Err(e) => return Err(e),
                    }
                }
                self.registrat.drop_last();
                // currently blocks don't return something
                Ok(NaslValue::Null)
            }
            NoOp => Ok(NaslValue::Null),
            EoF => Ok(NaslValue::Null),
            AttackCategory => {
                match statement.as_token().category() {
                    TokenCategory::Identifier(IdentifierType::ACT(cat)) => Ok(NaslValue::AttackCategory(*cat)),
                    _ => unreachable!("AttackCategory must have ACT token but got {:?}, this is an bug within the lexer.", statement.as_token())

                }
            },
            Continue => Ok(NaslValue::Continue),
            Break => Ok(NaslValue::Break),
        }
        .map_err(|e| {
            if e.origin.is_none() {
                InterpretError::from_statement(statement, e.kind)
            } else {
                e
            }
        })
        };
        self.position.down();
        results
    }

    /// Returns used register
    pub fn register(&self) -> &Register {
        &self.registrat
    }
}
