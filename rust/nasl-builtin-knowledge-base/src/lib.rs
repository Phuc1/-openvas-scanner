// SPDX-FileCopyrightText: 2023 Greenbone AG
//
// SPDX-License-Identifier: GPL-2.0-or-later WITH x11vnc-openssl-exception

use std::time::{SystemTime, UNIX_EPOCH};

use nasl_builtin_utils::{error::FunctionErrorKind, NaslFunction};
use storage::{Field, Kb, Retrieve};

use nasl_builtin_utils::{Context, Register};
use nasl_function_proc_macro::nasl_function;
use nasl_syntax::NaslValue;

/// NASL function to set a value under name in a knowledge base
/// Only pushes unique values for the given name.
#[nasl_function(named(name, value, expires))]
fn set_kb_item(
    name: NaslValue,
    value: NaslValue,
    expires: Option<NaslValue>,
    c: &Context,
) -> Result<NaslValue, FunctionErrorKind> {
    let expires = match expires {
        Some(NaslValue::Number(x)) => Some(x),
        Some(NaslValue::Exit(0)) => None,
        None => None,
        Some(x) => {
            return Err(FunctionErrorKind::Diagnostic(
                format!("expected expires to be a number but is {x}."),
                None,
            ))
        }
    }
    .map(|seconds| {
        let start = SystemTime::now();
        match start.duration_since(UNIX_EPOCH) {
            Ok(x) => x.as_secs() + seconds as u64,
            Err(_) => 0,
        }
    });
    c.dispatcher()
        .dispatch(
            c.key(),
            Field::KB(Kb {
                key: name.to_string(),
                value: value.clone().as_primitive(),
                expire: expires,
            }),
        )
        .map(|_| NaslValue::Null)
        .map_err(|e| e.into())
}

/// NASL function to get a knowledge base
#[nasl_function]
fn get_kb_item(key: &str, c: &Context) -> Result<NaslValue, FunctionErrorKind> {
    c.retriever()
        .retrieve(c.key(), Retrieve::KB(key.to_string()))
        .map(|r| {
            r.into_iter()
                .filter_map(|x| match x {
                    Field::NVT(_) | Field::NotusAdvisory(_) | Field::Result(_) => None,
                    Field::KB(kb) => Some(kb.value.into()),
                })
                .collect::<Vec<_>>()
        })
        .map(NaslValue::Fork)
        .map_err(|e| e.into())
}

/// NASL function to replace a kb list
#[nasl_function(named(name, value, expires))]
fn replace_kb_item(
    name: NaslValue,
    value: NaslValue,
    c: &Context,
) -> Result<NaslValue, FunctionErrorKind> {
    c.dispatcher()
        .dispatch_replace(
            c.key(),
            Field::KB(Kb {
                key: name.to_string(),
                value: value.clone().as_primitive(),
                expire: None,
            }),
        )
        .map(|_| NaslValue::Null)
        .map_err(|e| e.into())
}

/// NASL function to retrieve an item in a KB.
#[nasl_function(named(name, value, expires))]
fn get_kb_list(key: NaslValue, c: &Context) -> Result<NaslValue, FunctionErrorKind> {
    c.retriever()
        .retrieve(c.key(), Retrieve::KB(key.to_string()))
        .map(|r| {
            r.into_iter()
                .filter_map(|x| match x {
                    Field::NVT(_) | Field::NotusAdvisory(_) | Field::Result(_) => None,
                    Field::KB(kb) => Some(kb.value.into()),
                })
                .collect::<Vec<_>>()
        })
        .map(NaslValue::Array)
        .map_err(|e| e.into())
}

/// Returns found function for key or None when not found
pub fn lookup(key: &str) -> Option<NaslFunction> {
    match key {
        "set_kb_item" => Some(set_kb_item),
        "get_kb_item" => Some(get_kb_item),
        "get_kb_list" => Some(get_kb_list),
        "replace_kb_item" => Some(replace_kb_item),
        _ => None,
    }
}

pub struct KnowledgeBase;

impl nasl_builtin_utils::NaslFunctionExecuter for KnowledgeBase {
    fn nasl_fn_execute(
        &self,
        name: &str,
        register: &Register,
        context: &Context,
    ) -> Option<nasl_builtin_utils::NaslResult> {
        lookup(name).map(|x| x(register, context))
    }

    fn nasl_fn_defined(&self, name: &str) -> bool {
        lookup(name).is_some()
    }
}
