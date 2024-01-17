//! Replaces the function calls within a feed.

use std::error::Error;

use nasl_syntax::Statement;

use crate::{verify, NaslFileFinder};

/// Is used to find parameter by either name or index within a ReplaceCommand
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum FindParameter {
    /// Find a parameter by name
    Name(String),
    /// Find a parameter by name and value
    NameValue(String, String),
    /// Find a parameter by index
    Index(usize),
}

/// Is used within Replacer to find a specific statement to operator on.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum Find {
    /// Finds a function by name.
    ///
    /// Uses the given string to identify functions by that name.
    FunctionByName(String),
    /// Finds a function by parameter.
    FunctionByParameter(Vec<FindParameter>),
    /// Finds a function by name and parameter.
    FunctionByNameAndParameter(String, Vec<FindParameter>),
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
/// Describes parameter
pub enum Parameter {
    /// Named parameter (e.g.: a: 1)
    Named(String, String),
    /// Parameter without a name
    Anon(String),
}

impl std::fmt::Display for Parameter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Parameter::Named(k, v) => write!(f, "NamedParameter({k}, {v})"),
            Parameter::Anon(v) => write!(f, "Parameter({v})"),
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
/// Describes how to manipulate parameter
pub enum ParameterOperation {
    /// Pushes a parameter to the end
    Push(Parameter),
    /// Adds parameter to the given index
    Add(usize, Parameter),
    /// Removes a parameter found by name
    RemoveNamed(String),
    /// Removes a parameter found by index
    Remove(usize),
    /// Removes all parameter
    RemoveAll,
    /// Renames a parameter
    Rename {
        /// The value to be replaced
        previous: String,
        /// The new value
        new: String,
    },
}
impl ParameterOperation {
    /// Creates a rename operation
    pub fn rename<S>(previous: S, new: S) -> Self
    where
        S: Into<String>,
    {
        Self::Rename {
            previous: previous.into(),
            new: new.into(),
        }
    }
    /// Creates a remove by name operation
    pub fn remove_named<S>(name: S) -> Self
    where
        S: Into<String>,
    {
        Self::RemoveNamed(name.into())
    }
}

impl std::fmt::Display for ParameterOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParameterOperation::Push(p) => write!(f, "Push {p}"),
            ParameterOperation::Add(i, p) => write!(f, "Add {p} to index {i}"),
            ParameterOperation::RemoveNamed(s) => write!(f, "Remove {s}"),
            ParameterOperation::Remove(i) => write!(f, "Remove {i}"),
            ParameterOperation::Rename { previous, new } => write!(f, "Rename {previous} to {new}"),
            ParameterOperation::RemoveAll => write!(f, "Remove all parameter."),
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
/// Replace function operation
pub enum Replace {
    /// Replaces name of a function
    Name(String),
    /// Remove finding
    Remove,
    /// Replace parameter
    Parameter(ParameterOperation),
}

impl std::fmt::Display for Replace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Replace::Name(name) => write!(f, "Replace: {name}"),
            Replace::Parameter(p) => {
                write!(f, "Replace parameter: {}", p)
            }
            Replace::Remove => write!(f, "Remove found statement"),
        }
    }
}

trait Matcher {
    fn matches(&self, s: &Statement) -> bool;
}

#[derive(Clone, Debug)]
struct CallMatcher {}
impl Matcher for CallMatcher {
    fn matches(&self, s: &Statement) -> bool {
        // Although Exit and Include are handled differently they share the call nature and hence
        // are treated equially.
        matches!(
            s,
            &Statement::FunctionDeclaration(..)
                | &Statement::Call(..)
                | &Statement::Exit(..)
                | &Statement::Include(..)
        )
    }
}

#[derive(Clone, Debug)]
struct FunctionNameMatcher<'a> {
    name: Option<&'a str>,
    parameter: Option<&'a [FindParameter]>,
}

impl<'a> Matcher for FunctionNameMatcher<'a> {
    fn matches(&self, s: &Statement) -> bool {
        if !match s {
            Statement::Exit(t, ..)
            | Statement::Include(t, ..)
            | Statement::Call(t, ..)
            | Statement::FunctionDeclaration(_, t, ..) => match t.category() {
                nasl_syntax::TokenCategory::Identifier(nasl_syntax::IdentifierType::Undefined(
                    aname,
                )) => self.name.as_ref().map(|name| name == aname).unwrap_or(true),
                nasl_syntax::TokenCategory::Identifier(nasl_syntax::IdentifierType::Exit) => {
                    self.name.map(|name| name == "exit").unwrap_or(true)
                }
                nasl_syntax::TokenCategory::Identifier(nasl_syntax::IdentifierType::Include) => {
                    self.name.map(|name| name == "include").unwrap_or(true)
                }

                _ => false,
            },
            _ => false,
        } {
            return false;
        }
        if self.parameter.is_none() {
            return true;
        }
        let wanted = unsafe { self.parameter.unwrap_unchecked() };

        let (named, anon) = match s {
            Statement::Include(..) | Statement::Exit(..) => (vec![], 1),
            Statement::Call(_, p, ..) => {
                let named = p
                    .iter()
                    .filter_map(|p| match p {
                        Statement::NamedParameter(name, value) => {
                            Some((name.category().to_string(), value.to_string()))
                        }
                        _ => None,
                    })
                    .collect();
                let anon = p.iter().filter(|p| p.is_returnable()).count();
                (named, anon)
            }
            Statement::FunctionDeclaration(_, _, p, _, _block) => {
                let anon = {
                    // we don't know how many anon parameter an declared method is using.
                    // Theoretically we could guess by checking _block for _FC_ANON_ARGS and return
                    // the given indices number when available
                    //
                    // let fcta = _block.find(&|x| {
                    //     use nasl_syntax::{IdentifierType as IT, Token, TokenCategory as TC};
                    //     matches!(
                    //         x,
                    //         Statement::Array(
                    //             Token {
                    //                 category: TC::Identifier(IT::FCTAnonArgs),
                    //                 line_column: _,
                    //                 position: _
                    //             },
                    //             ..
                    //         )
                    //     )
                    // });
                    // if fcta.len() > 0 {
                    //     self.parameter.iter().flat_map(|x|x).find_map(|x|x.idx).unwrap_or_default()
                    // } else {
                    //     0
                    // }
                    // However I think it is better to skip each search parameter for
                    // anon args when finding a function declaration as this limitation is more obvious
                    // than wrongly changed anon parameter.
                    0
                };
                let named = p
                    .iter()
                    .filter_map(|p| match p {
                        Statement::Variable(name) => Some(name.category().to_string()),
                        _ => None,
                    })
                    .map(|x| (x, "".to_owned()))
                    .collect();

                (named, anon)
            }

            _ => unreachable!("Should be validated before"),
        };
        if wanted.len() != named.len() + anon {
            return false;
        }
        for w in wanted {
            let result = match w {
                FindParameter::Name(name) => !named.iter().any(|n| &n.0 == name),
                FindParameter::Index(x) => x != &anon,
                FindParameter::NameValue(n, v) => !named.iter().any(|(k, ov)| k == n && ov == v),
            };
            if result {
                return false;
            }
        }
        true
    }
}

impl Find {
    /// Checks if statement matches the wanted search operation
    pub fn matches(&self, s: &Statement) -> bool {
        let (name, parameter) = match self {
            Find::FunctionByName(name) => (Some(name as &str), None),
            Find::FunctionByParameter(x) => (None, Some(x as &[_])),
            Find::FunctionByNameAndParameter(x, y) => (Some(x as &str), Some(y as &[_])),
        };

        FunctionNameMatcher { name, parameter }.matches(s)
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
/// Describes what should be replaced
pub struct ReplaceCommand {
    /// The identifier to find
    pub find: Find,
    /// The replacement for found identifier
    pub with: Replace,
}

#[derive(Debug)]
/// Error cases on a replace operation
pub enum ReplaceError {
    /// The replace operation is invalid on statement
    Unsupported(Replace, Statement),
}
impl std::fmt::Display for ReplaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReplaceError::Unsupported(op, s) => {
                write!(f, "Operation {} not allowed on {}.", op, s)
            }
        }
    }
}

impl Error for ReplaceError {}

/// Handles the inplace replacements
pub struct CodeReplacer {
    // since the first position we need to add offset
    offsets: Vec<(usize, i64)>,
    code: String,
    changed: bool,
}

impl CodeReplacer {
    fn range_with_offset(&self, r: &(usize, usize)) -> (usize, usize) {
        let offset: i64 = self
            .offsets
            .iter()
            .filter_map(|(pos, offset)| if pos < &r.0 { Some(offset) } else { None })
            .sum();
        let start = (r.0 as i64 + offset) as usize;
        let end = (r.1 as i64 + offset) as usize;
        (start, end)
    }

    fn find_named_parameter<'a>(s: &'a Statement, wanted: &str) -> Option<&'a Statement> {
        match s {
            Statement::FunctionDeclaration(_, _, stmts, ..) | Statement::Call(_, stmts, ..) => {
                use nasl_syntax::IdentifierType::Undefined;
                use nasl_syntax::TokenCategory::Identifier;
                for s in stmts {
                    match s {
                        Statement::Variable(t) | Statement::NamedParameter(t, _) => {
                            if let nasl_syntax::Token {
                                category: Identifier(Undefined(name)),
                                ..
                            } = t
                            {
                                if name == wanted {
                                    return Some(s);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        None
    }

    fn replace_range_with_offset(&mut self, new: &str, position: &(usize, usize)) {
        let new_pos = self.range_with_offset(position);
        self.replace_range(&new_pos, new, position)
    }

    fn replace_range(
        &mut self,
        (start, end): &(usize, usize),
        new: &str,
        (previous_start, previous_end): &(usize, usize),
    ) {
        self.code.replace_range(start..end, new);
        self.changed = true;
        let offset = new.len() as i64 - (previous_end - previous_start) as i64;
        match offset.cmp(&0) {
            std::cmp::Ordering::Less => {
                self.offsets.push((*start, offset));
            }
            std::cmp::Ordering::Equal => {}
            std::cmp::Ordering::Greater => {
                self.offsets.push((*previous_start, offset));
            }
        }
    }
    fn replace_as_string(&mut self, s: &Statement, r: &Replace) -> Result<(), ReplaceError> {
        match r {
            Replace::Remove => {
                self.replace_range_with_offset("", &s.position());
                Ok(())
            }
            Replace::Name(name) => match s {
                Statement::FunctionDeclaration(_, n, ..)
                | Statement::Call(n, ..)
                | Statement::Exit(n, ..)
                | Statement::Include(n, ..) => {
                    self.replace_range_with_offset(name, &n.position);
                    Ok(())
                }
                _ => Err(ReplaceError::Unsupported(r.clone(), s.clone())),
            },
            Replace::Parameter(params) => {
                let range = match s {
                    Statement::FunctionDeclaration(_, _, stmts, ..)
                    | Statement::Call(_, stmts, ..) => match &stmts[..] {
                        &[] => None,
                        [first, ref tail @ ..] => {
                            let first = first.range();
                            let end = tail.last().map(|x| x.range().end).unwrap_or(first.end);
                            let start = first.start;
                            Some((start, end))
                        }
                    },
                    Statement::Exit(_, stmt, ..) | Statement::Include(_, stmt, ..) => {
                        Some(stmt.position())
                    }
                    _ => return Err(ReplaceError::Unsupported(r.clone(), s.clone())),
                };

                match params {
                    ParameterOperation::Push(p) => self.push_parameter(s, p),
                    ParameterOperation::Add(i, p) => self.add_parameter(s, *i, p),

                    ParameterOperation::Remove(i) => self.remove_indexed_parameter(s, *i),
                    ParameterOperation::RemoveNamed(wanted) => {
                        self.remove_named_parameter(s, wanted)
                    }
                    ParameterOperation::Rename { previous, new } => {
                        self.rename_parameter(s, previous, new)
                    }
                    ParameterOperation::RemoveAll => {
                        if let Some(range) = range {
                            self.replace_range_with_offset("", &range);
                        }
                    }
                };

                Ok(())
            }
        }
    }

    /// Replaces findings based on given replace within code and returns the result as String
    ///
    /// Spawns a Replacer that contains a copy of the source code and manipulates it iteratively
    /// based on the order of the given commands.
    pub fn replace(code: &str, replace: &[ReplaceCommand]) -> Result<String, Box<dyn Error>> {
        let mut code = code.to_string();
        let mut cached_stmts = Vec::new();
        // We need to be aware of parameter changes otherwise it can bug out
        // with the ordering of new parameter.
        for r in replace {
            let mut replacer = CodeReplacer {
                offsets: Vec::with_capacity(replace.len()),
                code: code.clone(),
                changed: false,
            };
            if cached_stmts.is_empty() {
                cached_stmts = nasl_syntax::parse(&code).filter_map(|x| x.ok()).collect();
            }

            for s in cached_stmts.iter() {
                let results = s.find(&|s| r.find.matches(s));
                for s in results {
                    replacer.replace_as_string(s, &r.with)?;
                }
            }
            if replacer.changed {
                cached_stmts.clear();
                code = replacer.code;
            }
        }

        Ok(code)
    }

    fn push_parameter(&mut self, s: &Statement, p: &Parameter) {
        fn calculate_fn_decl(p: &Parameter, is_only: bool) -> Option<String> {
            if let Parameter::Named(n, _) = p {
                Some(if is_only {
                    n.to_owned()
                } else {
                    format!(", {n}")
                })
            } else {
                None
            }
        }

        fn calculate_call(p: &Parameter, is_only: bool) -> Option<String> {
            Some(match (is_only, p) {
                (true, Parameter::Named(n, v)) => {
                    format!("{n}: {v}")
                }
                (true, Parameter::Anon(s)) => s.to_string(),
                (false, Parameter::Named(n, v)) => {
                    format!(", {n}: {v}")
                }
                (false, Parameter::Anon(s)) => {
                    format!(", {s}")
                }
            })
        }

        if let Some((pos, np)) = match s {
            Statement::FunctionDeclaration(_, _, args, rp, _) => {
                calculate_fn_decl(p, args.is_empty()).map(|x| (rp.position, x))
            }
            Statement::Call(_, args, rp) => {
                calculate_call(p, args.is_empty()).map(|x| (rp.position, x))
            }
            _ => None,
        } {
            let npos = self.range_with_offset(&pos);
            let before = &self.code[npos.0..npos.1];
            let param = format!("{np}{before}");
            self.replace_range(&npos, &param, &pos)
        }
    }

    fn add_parameter(&mut self, s: &Statement, i: usize, p: &Parameter) {
        fn calculate_known_index(s: &Statement, p: &Parameter) -> Option<String> {
            match (matches!(s, Statement::Call(..)), p) {
                (true, Parameter::Named(n, v)) => Some(format!("{n}: {v}, ")),
                (true, Parameter::Anon(s)) => Some(format!("{s}, ")),
                (false, Parameter::Named(n, _)) => Some(format!("{n}, ")),
                (false, Parameter::Anon(_)) => None,
            }
        }
        fn calculate_unknown_index(
            s: &Statement,
            p: &Parameter,
            params: &[Statement],
        ) -> Option<String> {
            let np = match (matches!(s, Statement::Call(..)), params.is_empty(), p) {
                (true, true, Parameter::Named(n, v)) => {
                    format!("{n}: {v}")
                }

                (true, false, Parameter::Named(n, v)) => {
                    format!(", {n}: {v}")
                }
                (true, false, Parameter::Anon(s)) => {
                    format!(", {s}")
                }
                (true, true, Parameter::Anon(s)) => s.to_owned(),
                (false, true, Parameter::Named(n, _)) => n.to_owned(),

                (false, false, Parameter::Named(n, _)) => format!(", {n}"),
                (false, _, Parameter::Anon(_)) => return None,
            };
            Some(np)
        }
        match s {
            Statement::FunctionDeclaration(_, _, params, end, _)
            | Statement::Call(_, params, end)
                if i <= params.len() || i == 0 =>
            {
                let index_exits = params
                    .get(i)
                    .iter()
                    .flat_map(|s| s.as_token())
                    .map(|t| t.position)
                    .next();
                let np = if index_exits.is_some() {
                    calculate_known_index(s, p)
                } else {
                    calculate_unknown_index(s, p, params)
                };

                if let Some(s) = np {
                    let position = index_exits.unwrap_or(end.position);
                    let new_position = self.range_with_offset(&position);
                    let before = &self.code[new_position.0..new_position.1];
                    self.replace_range(&new_position, &format!("{s}{before}"), &position);
                }
            }
            _ => {}
        }
    }

    fn remove_parameter(&mut self, s: &Statement) {
        let position = s.position();
        let (start, end) = self.range_with_offset(&position);
        let new_position = {
            let (count, last) = self
                .code
                .chars()
                .skip(end)
                .take_while(|x| x.is_whitespace() || x == &',' || x == &')')
                .fold((0, '0'), |a, b| (a.0 + 1, b));
            // unless it is the last parameter
            if last == ')' {
                let (count, last) = self.code[0..start]
                    .chars()
                    .rev()
                    .take_while(|c| c.is_whitespace() || c == &',' || c == &'(')
                    .fold((0, '0'), |a, b| (a.0 + 1, b));
                let is_only_parameter = last == '(';
                if is_only_parameter {
                    (start, end)
                } else {
                    (start - count, end)
                }
            } else {
                (start, end + count)
            }
        };

        self.replace_range(&new_position, "", &new_position);
    }
    fn remove_indexed_parameter(&mut self, s: &Statement, i: usize) {
        match s {
            Statement::FunctionDeclaration(_, _, stmts, ..) | Statement::Call(_, stmts, ..) => {
                if let Some(x) = stmts.get(i) {
                    self.remove_parameter(x);
                }
            }
            _ => {}
        }
    }

    fn remove_named_parameter(&mut self, s: &Statement, wanted: &str) {
        Self::find_named_parameter(s, wanted).iter().for_each(|s| {
            self.remove_parameter(s);
        })
    }

    fn rename_parameter(&mut self, s: &Statement, previous: &str, new: &str) {
        Self::find_named_parameter(s, previous)
            .iter()
            .for_each(|s| {
                let pos = s.as_token().map(|x| x.position).unwrap_or_default();
                self.replace_range_with_offset(new, &pos)
            })
    }
}

/// Finds all nasl and inc files of feed and executes given replace commands
pub struct FeedReplacer<'a> {
    finder: NaslFileFinder,
    replace: &'a [ReplaceCommand],
}

impl<'a> FeedReplacer<'a> {
    /// Creates a new FeedReplacer
    pub fn new<S>(root: S, replace: &'a [ReplaceCommand]) -> FeedReplacer<'_>
    where
        S: AsRef<str>,
    {
        let finder = crate::NaslFileFinder::new(&root, false);
        FeedReplacer { finder, replace }
    }
    fn replace(
        &mut self,
        path: Result<String, verify::Error>,
    ) -> Result<Option<(String, String)>, Box<dyn Error>> {
        let name = path?;
        let code = nasl_syntax::load_non_utf8_path(&name)?;
        let new_code = CodeReplacer::replace(&code, self.replace)?;
        // otherwise  we will transform the whole feed to utf-8
        if code != new_code {
            Ok(Some((name, new_code)))
        } else {
            Ok(None)
        }
    }
}

impl<'a> Iterator for FeedReplacer<'a> {
    type Item = Result<Option<(String, String)>, Box<dyn Error>>;

    fn next(&mut self) -> Option<Self::Item> {
        let path = self.finder.next()?;
        Some(self.replace(path))
    }
}

#[cfg(test)]
mod parsing {
    use crate::transpile::FindParameter;

    use super::ReplaceCommand;

    pub fn generate_replace_commands() -> Vec<ReplaceCommand> {
        // register_product(cpe:cpe, location:"/", port:port, service:"www");
        // register_product(location:"/", port:port, service:"world-wide-web")
        vec![
            ReplaceCommand {
                find: crate::transpile::Find::FunctionByNameAndParameter(
                    "register_product".into(),
                    vec![
                        FindParameter::Name("cpe".into()),
                        FindParameter::Name("location".into()),
                        FindParameter::Name("port".into()),
                        FindParameter::NameValue("service".into(), "\"www\"".into()),
                    ],
                ),
                with: crate::transpile::Replace::Parameter(
                    crate::transpile::ParameterOperation::Push(crate::transpile::Parameter::Named(
                        "service_to_be".into(),
                        "\"world-wide-shop\"".into(),
                    )),
                ),
            },
            ReplaceCommand {
                find: crate::transpile::Find::FunctionByNameAndParameter(
                    "register_product".into(),
                    vec![
                        FindParameter::Name("cpe".into()),
                        FindParameter::Name("location".into()),
                        FindParameter::Name("port".into()),
                        FindParameter::Name("service".into()),
                        FindParameter::Name("service_to_be".into()),
                    ],
                ),
                with: crate::transpile::Replace::Parameter(
                    crate::transpile::ParameterOperation::RemoveNamed("service".into()),
                ),
            },
            ReplaceCommand {
                find: crate::transpile::Find::FunctionByName("register_product".into()),
                with: crate::transpile::Replace::Parameter(
                    crate::transpile::ParameterOperation::Rename {
                        previous: "service_to_be".to_string(),
                        new: "service".to_string(),
                    },
                ),
            },
            ReplaceCommand {
                find: crate::transpile::Find::FunctionByName("register_product".into()),
                with: crate::transpile::Replace::Parameter(
                    crate::transpile::ParameterOperation::Rename {
                        previous: "cpe".into(),
                        new: "runtime_information".into(),
                    },
                ),
            },
            ReplaceCommand {
                find: crate::transpile::Find::FunctionByName("register_host_detail".into()),
                with: crate::transpile::Replace::Name("hokus_pokus".into()),
            },
            ReplaceCommand {
                find: crate::transpile::Find::FunctionByName("script_xref".into()),
                with: crate::transpile::Replace::Remove,
            },
        ]
    }
    #[test]
    fn to_toml() {
        #[derive(serde::Deserialize, serde::Serialize)]
        struct Wrapper {
            cmds: Vec<crate::transpile::ReplaceCommand>,
        }
        let options = generate_replace_commands();
        let w = Wrapper { cmds: options };
        let _config = toml::to_string_pretty(&w).unwrap();
        // use std::io::Write;
        // write!(
        //     std::fs::File::create("/tmp/rename_example.toml").unwrap(),
        //     "{_config}"
        // )
        // .unwrap();
    }
}
#[cfg(test)]
mod functions {
    use super::*;

    macro_rules! parameter_check {
        ($name:expr, $code:expr, $params:expr, $expected:expr) => {{
            let name = $name.to_string();
            let replaces = [ReplaceCommand {
                find: Find::FunctionByName(name),
                with: Replace::Parameter($params),
            }];
            let result = CodeReplacer::replace($code, &replaces).unwrap();

            assert_eq!(&result, $expected);
        }};
        ($code:expr, $params:expr, $expected:expr) => {{
            if let Some((name, _)) = $code.rsplit_once("(") {
                let name = name.replace("function ", "");
                parameter_check!(name, $code, $params, $expected)
            } else {
                panic!(
                    "expected {} to contain `(` so that it can be used as a function name",
                    $code
                );
            }
        }};
    }

    #[test]
    fn parameter_test() {
        parameter_check!(
            "my_call",
            "function my_call(a){};my_call();",
            ParameterOperation::Add(0, Parameter::Named("test".into(), "test".into())),
            "function my_call(test, a){};my_call(test: test);"
        );
    }

    #[test]
    fn add_parameter_on_fn_dclr() {
        parameter_check!(
            "function my_call(a, b){};",
            ParameterOperation::Add(1, Parameter::Named("test".into(), "test".into())),
            "function my_call(a, test, b){};"
        );
        parameter_check!(
            "function my_call(a){};",
            ParameterOperation::Add(1, Parameter::Named("test".into(), "test".into())),
            "function my_call(a, test){};"
        );
        parameter_check!(
            "function my_call(a){};",
            ParameterOperation::Add(0, Parameter::Named("test".into(), "test".into())),
            "function my_call(test, a){};"
        );

        // should not add when there insufficient previous parameter
        parameter_check!(
            "function my_call(a){};",
            ParameterOperation::Add(2, Parameter::Named("test".into(), "test".into())),
            "function my_call(a){};"
        );
        // but should push on first parameter even when there were none
        parameter_check!(
            "function my_call(){};",
            ParameterOperation::Add(0, Parameter::Named("test".into(), "test".into())),
            "function my_call(test){};"
        );
    }

    #[test]
    fn push_parameter_side_effects() {
        let code = r#"
if (admin_ports = get_kb_list("sophos/xg_firewall/http-admin/port")) {
  foreach port (admin_ports) {
    register_product(cpe: os_cpe1, location: location, port: port, service: "www");
    register_product(cpe: os_cpe2, location: location, port: port, service: "www");
    register_product(cpe: hw_cpe, location: location, port: port, service: "www");
  }
}

if (user_ports = get_kb_list("sophos/xg_firewall/http-user/port")) {
  foreach port (user_ports) {
    register_product(cpe: os_cpe1, location: location, port: port, service: "www");
    register_product(cpe: os_cpe2, location: location, port: port, service: "www");
    register_product(cpe: hw_cpe, location: location, port: port, service: "www");
  }
}
        "#;

        let expected = r#"
if (admin_ports = get_kb_list("sophos/xg_firewall/http-admin/port")) {
  foreach port (admin_ports) {
    register_product(runtime_information: os_cpe1, location: location, port: port, service: "world-wide-shop");
    register_product(runtime_information: os_cpe2, location: location, port: port, service: "world-wide-shop");
    register_product(runtime_information: hw_cpe, location: location, port: port, service: "world-wide-shop");
  }
}

if (user_ports = get_kb_list("sophos/xg_firewall/http-user/port")) {
  foreach port (user_ports) {
    register_product(runtime_information: os_cpe1, location: location, port: port, service: "world-wide-shop");
    register_product(runtime_information: os_cpe2, location: location, port: port, service: "world-wide-shop");
    register_product(runtime_information: hw_cpe, location: location, port: port, service: "world-wide-shop");
  }
}
        "#;

        let replaces = parsing::generate_replace_commands();
        let result = CodeReplacer::replace(code, &replaces).unwrap();
        assert_eq!(&result, expected);
    }

    #[test]
    fn remove_parameter_side_effects() {
        let code = r#"
    if(vers == "unknown") {
      register_host_detail(name:"App", value:string("cpe:/a:aeromail:aeromail"), desc:SCRIPT_DESC);
    } else {
      register_host_detail(name:"App", value:string("cpe:/a:aeromail:aeromail:",vers), desc:SCRIPT_DESC2);
    }

      register_host_detail(name:"App", value:string("cpe:/a:aeromail:aeromail:",vers), desc:SCRIPT_DESC2);
      register_host_detail(name:"App", value:string("cpe:/a:aeromail:aeromail:",vers), desc:SCRIPT_DESC2);
    function my_call(a){};my_call();
    info = string("AeroMail Version '");"#;

        let expected = r#"
    if(vers == "unknown") {
      register_host_detail(name:"App", value:string("cpe:/a:aeromail:aeromail"));
    } else {
      register_host_detail(name:"App", value:string("cpe:/a:aeromail:aeromail:",vers));
    }

      register_host_detail(name:"App", value:string("cpe:/a:aeromail:aeromail:",vers));
      register_host_detail(name:"App", value:string("cpe:/a:aeromail:aeromail:",vers));
    function my_call(test, a, aha){};my_call(test: test, aha: "soso");
    info = string("AeroMail Version '");"#;

        let replaces = [
            ReplaceCommand {
                find: Find::FunctionByName("register_host_detail".to_string()),
                with: Replace::Parameter(ParameterOperation::remove_named("desc")),
            },
            ReplaceCommand {
                find: Find::FunctionByName("my_call".to_string()),
                with: Replace::Parameter(ParameterOperation::Add(
                    0,
                    Parameter::Named("test".into(), "test".into()),
                )),
            },
            ReplaceCommand {
                find: Find::FunctionByName("my_call".to_string()),
                with: Replace::Parameter(ParameterOperation::Push(Parameter::Named(
                    "aha".into(),
                    "\"soso\"".into(),
                ))),
            },
        ];
        let result = CodeReplacer::replace(code, &replaces).unwrap();

        assert_eq!(&result, expected);
    }

    #[test]
    fn remove_parameter_on_fn_dclr() {
        parameter_check!(
            "function my_call(a, b, c){};",
            ParameterOperation::remove_named("a"),
            "function my_call(b, c){};"
        );
        parameter_check!(
            "function my_call(a, b, c){};",
            ParameterOperation::remove_named("c"),
            "function my_call(a, b){};"
        );
        parameter_check!(
            "function my_call(a, b, c){};",
            ParameterOperation::Remove(1),
            "function my_call(a, c){};"
        );
    }

    #[test]
    fn remove_all_parameter_on_fn_dclr() {
        parameter_check!(
            "function my_call(a){};",
            ParameterOperation::RemoveAll,
            "function my_call(){};"
        );
    }

    #[test]
    fn rename_parameter_on_fn_dclr() {
        parameter_check!(
            "function my_call(a){};",
            ParameterOperation::rename("a", "b"),
            "function my_call(b){};"
        );
    }

    #[test]
    fn push_parameter_on_fn_declaration() {
        parameter_check!(
            "function my_call(){};",
            ParameterOperation::Push(Parameter::Named("x".to_owned(), "'moep'".to_owned())),
            "function my_call(x){};"
        );
        parameter_check!(
            "function my_call(a){};",
            ParameterOperation::Push(Parameter::Named("x".to_owned(), "'moep'".to_owned())),
            "function my_call(a, x){};"
        );
    }

    #[test]
    fn remove_all_parameter_on_call() {
        parameter_check!("my_call(1);", ParameterOperation::RemoveAll, "my_call();");
        parameter_check!(
            "my_call(1, 2, 4);",
            ParameterOperation::RemoveAll,
            "my_call();"
        );
        parameter_check!(
            "my_call(a: 1, 2, 4);",
            ParameterOperation::RemoveAll,
            "my_call();"
        );
    }

    #[test]
    fn rename_parameter_on_call() {
        parameter_check!(
            "my_call(a: 1, 2, 4);",
            ParameterOperation::rename("a", "b"),
            "my_call(b: 1, 2, 4);"
        );
    }

    #[test]
    fn remove_parameter_on_call() {
        parameter_check!(
            "my_call(a: 1, 2, 4);",
            ParameterOperation::remove_named("a"),
            "my_call(2, 4);"
        );
        parameter_check!(
            "my_call(a: 1, 2, 4);",
            ParameterOperation::Remove(1),
            "my_call(a: 1, 4);"
        );
    }

    #[test]
    fn push_parameter_on_call() {
        parameter_check!(
            "my_call();",
            ParameterOperation::Push(Parameter::Named("x".to_owned(), "'moep'".to_owned())),
            "my_call(x: 'moep');"
        );
        parameter_check!(
            "my_call(a: 1);",
            ParameterOperation::Push(Parameter::Named("x".to_owned(), "'moep'".to_owned())),
            "my_call(a: 1, x: 'moep');"
        );
    }

    #[test]
    fn add_parameter_on_call() {
        parameter_check!(
            "my_call(a: 1, 2, 4);",
            ParameterOperation::Add(1, Parameter::Anon("test".into())),
            "my_call(a: 1, test, 2, 4);"
        );
        parameter_check!(
            "my_call(a: 1);",
            ParameterOperation::Add(1, Parameter::Anon("test".into())),
            "my_call(a: 1, test);"
        );
        parameter_check!(
            "my_call(a: 1);",
            ParameterOperation::Add(0, Parameter::Anon("test".into())),
            "my_call(test, a: 1);"
        );

        // should not add when there insufficient previous parameter
        parameter_check!(
            "my_call(a: 1);",
            ParameterOperation::Add(2, Parameter::Anon("test".into())),
            "my_call(a: 1);"
        );
        // but should push on first parameter even when there were none
        parameter_check!(
            "my_call();",
            ParameterOperation::Add(0, Parameter::Anon("test".into())),
            "my_call(test);"
        );
    }

    #[test]
    fn find_parameter() {
        let code = r#"
        function funker() { # Sometimes I think it is too much, because
            return aha(_FCT_ANON_ARGS[0]); # my little secret is memory inefficiency.
        }

        function funker(a, b) { # Sometimes I think it is too much, because
            return funker(a: a + b); # my little secret is memory inefficiency.
        }
        function funker(a) { # Sometimes I think it is too much, because
            return funker(a); # my little secret is memory inefficiency.
        }
        funker(a: 42);
        funker(a: 42, b: 3);
        aha(b: "lol");
        aha(b: 42);
        "#;
        let expected = r#"
        function funker() { # Sometimes I think it is too much, because
            return aha(_FCT_ANON_ARGS[0]); # my little secret is memory inefficiency.
        }

        function funker(a, b) { # Sometimes I think it is too much, because
            return funkerino(a: a + b); # my little secret is memory inefficiency.
        }
        function funkerino(a) { # Sometimes I think it is too much, because
            return internal_funker(a); # my little secret is memory inefficiency.
        }
        funkerino(a: 42);
        funker(a: 42, b: 3);
        ;
        aha(b: 42);
        "#;

        let replaces = [
            ReplaceCommand {
                find: Find::FunctionByNameAndParameter(
                    "funker".to_string(),
                    vec![FindParameter::Name("a".into())],
                ),
                with: Replace::Name("funkerino".to_string()),
            },
            ReplaceCommand {
                find: Find::FunctionByNameAndParameter(
                    "funker".to_string(),
                    vec![FindParameter::Index(1_usize)],
                ),
                with: Replace::Name("internal_funker".to_string()),
            },
            ReplaceCommand {
                find: Find::FunctionByNameAndParameter(
                    "aha".to_string(),
                    vec![FindParameter::NameValue("b".into(), "\"lol\"".into())],
                ),
                with: Replace::Remove,
            },
        ];
        let result = CodeReplacer::replace(code, &replaces).unwrap();
        assert_eq!(result, expected.to_owned(),);
    }

    #[test]
    fn replace_name() {
        let code = r#"
        include("aha.inc");
        function test(a, b) { # Sometimes I think it is too much, because
            return funker(a + b); # my little secret is memory inefficiency.
        }
        a = funker(1);
        while (funker(1) == 1) {
           if (funker(2) == 2) {
               return funker(2);
           } else {
              for ( i = funker(3); i < funker(5) + funker(5); i + funker(1)) 
                exit(funker(10));
           }
        }
        b = test(a: 1, b: 2);
        exit(42);
        "#;
        let replaces = [
            ReplaceCommand {
                find: Find::FunctionByName("funker".to_string()),
                with: Replace::Name("funkerino".to_string()),
            },
            ReplaceCommand {
                find: Find::FunctionByName("test".to_string()),
                with: Replace::Name("tee".to_string()),
            },
            ReplaceCommand {
                find: Find::FunctionByName("include".to_string()),
                with: Replace::Name("inklusion".to_string()),
            },
            ReplaceCommand {
                find: Find::FunctionByName("exit".to_string()),
                with: Replace::Name("ausgang".to_string()),
            },
        ];
        let result = CodeReplacer::replace(code, &replaces).unwrap();
        assert_eq!(
            result,
            //code.replace("funker", "funkerino")
            code.replace("funker", "funkerino")
                .replace("test", "tee")
                .replace("include", "inklusion")
                .replace("exit", "ausgang")
        );
    }
}