//! Represents parsed Ninja strings with embedded variable references, e.g.
//! `c++ $in -o $out`, and mechanisms for expanding those into plain strings.

use std::borrow::Cow;
use std::borrow::ToOwned;
use std::collections::HashMap;

use crate::byte_string::*;

/// An environment providing a mapping of variable name to variable value.
/// A given EvalString may be expanded with multiple environments as possible
/// context.
pub trait Env {
    fn get_var(&self, var: &bstr) -> Option<Cow<bstr>>;
}

/// One token within an EvalString, either literal text or a variable reference.
#[derive(Debug)]
pub enum EvalPart<T> {
    Literal(T),
    VarRef(T),
}

/// A parsed but unexpanded variable-reference string, e.g. "cc $in -o $out".
/// This is generic to support EvalString<&bstr>, which is used for immediately-
/// expanded evals, like top-level bindings, and EvalString<ByteString>, which is
/// used for delayed evals like in `rule` blocks.
#[derive(Debug)]
pub struct EvalString<T>(Vec<EvalPart<T>>);

impl<T: AsRef<bstr>> EvalString<T> {
    pub fn new(parts: Vec<EvalPart<T>>) -> Self {
        EvalString(parts)
    }
    pub fn evaluate(&self, envs: &[&dyn Env]) -> ByteString {
        let mut val = ByteString::new();
        for part in &self.0 {
            match part {
                EvalPart::Literal(s) => val.extend_from_slice(s.as_ref()),
                EvalPart::VarRef(v) => {
                    for env in envs {
                        if let Some(v) = env.get_var(v.as_ref()) {
                            val.extend_from_slice(&v);
                            break;
                        }
                    }
                }
            }
        }
        val
    }
}
impl EvalString<&bstr> {
    pub fn into_owned(self) -> EvalString<ByteString> {
        EvalString(
            self.0
                .into_iter()
                .map(|part| match part {
                    EvalPart::Literal(s) => EvalPart::Literal(s.to_owned()),
                    EvalPart::VarRef(s) => EvalPart::VarRef(s.to_owned()),
                })
                .collect(),
        )
    }
}

/// A single scope's worth of variable definitions.
#[derive(Debug)]
pub struct Vars<'text>(HashMap<&'text bstr, ByteString>);
#[allow(clippy::new_without_default)]
impl<'text> Vars<'text> {
    pub fn new() -> Vars<'text> {
        Vars(HashMap::new())
    }
    pub fn insert(&mut self, key: &'text bstr, val: ByteString) {
        self.0.insert(key, val);
    }
    pub fn get(&self, key: &'text bstr) -> Option<&bstr> {
        self.0.get(key).map(|v| (&**v))
    }
}
impl<'a> Env for Vars<'a> {
    fn get_var(&self, var: &bstr) -> Option<Cow<bstr>> {
        self.0.get(var).map(|bstr| Cow::Borrowed(&**bstr))
    }
}

/// A single scope's worth of variable definitions, before $-expansion.
/// For variables attached to a rule we keep them unexpanded in memory because
/// they may be expanded in multiple different ways depending on which rule uses
/// them.
#[derive(Debug)]
pub struct LazyVars(Vec<(ByteString, EvalString<ByteString>)>);
#[allow(clippy::new_without_default)]
impl LazyVars {
    pub fn new() -> Self {
        LazyVars(Vec::new())
    }
    pub fn insert(&mut self, key: ByteString, val: EvalString<ByteString>) {
        self.0.push((key, val));
    }
    pub fn get(&self, key: &bstr) -> Option<&EvalString<ByteString>> {
        for (k, v) in &self.0 {
            if k == key {
                return Some(v);
            }
        }
        None
    }
    pub fn keyvals(&self) -> &Vec<(ByteString, EvalString<ByteString>)> {
        &self.0
    }
}
impl<'a> Env for LazyVars {
    fn get_var(&self, var: &bstr) -> Option<Cow<bstr>> {
        self.get(var).map(|val| Cow::Owned(val.evaluate(&[])))
    }
}
