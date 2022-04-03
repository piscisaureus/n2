//! Parser for .ninja files.
//!
//! See design notes on parsing in doc/design_notes.md.
//!
//! To avoid allocations parsing frequently uses references into the input
//! text, marked with the lifetime `'text`.

use std::ffi::OsStr;
use std::ffi::OsString;

use crate::byte_string::*;
use crate::eval::EvalPart;
use crate::eval::EvalString;
use crate::eval::LazyVars;
use crate::eval::Vars;
use crate::scanner::ParseError;
use crate::scanner::ParseResult;
use crate::scanner::Scanner;

#[derive(Debug)]
pub struct Rule<'text> {
    pub name: &'text bstr,
    pub vars: LazyVars,
}

#[derive(Debug)]
pub struct Build<'text, Path> {
    pub rule: &'text bstr,
    pub line: usize,
    pub outs: Vec<Path>,
    pub explicit_outs: usize,
    pub ins: Vec<Path>,
    pub explicit_ins: usize,
    pub implicit_ins: usize,
    pub order_only_ins: usize,
    pub vars: LazyVars,
}

#[derive(Debug)]
pub struct Pool<'text> {
    pub name: &'text bstr,
    pub depth: usize,
}

#[derive(Debug)]
pub enum Statement<'text, Path> {
    Rule(Rule<'text>),
    Build(Build<'text, Path>),
    Default(Vec<Path>),
    Include(Path),
    Subninja(Path),
    Pool(Pool<'text>),
}

pub struct Parser<'text> {
    scanner: Scanner<'text>,
    pub vars: Vars<'text>,
}

fn is_ident_char(c: u8) -> bool {
    matches!(c, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b'.' | b'/' | b',' | b'+' | b'@')
}

fn is_path_char(c: u8) -> bool {
    // Basically any character is allowed in paths, but we want to parse e.g.
    //   build foo: bar | baz
    // such that the colon is not part of the 'foo' path and such that b'|' is
    // not read as a path.
    !matches!(c, b'\0' | b' ' | b'\n' | b':' | b'|' | b'$')
}

pub trait Loader {
    type Path;
    fn path(&mut self, path_buf: OsString) -> Self::Path;
}

impl<'text> Parser<'text> {
    pub fn new(buf: &'text mut ByteString) -> Parser<'text> {
        Parser {
            scanner: Scanner::new(buf),
            vars: Vars::new(),
        }
    }

    pub fn format_parse_error(&self, filename: impl AsRef<OsStr>, err: ParseError) -> String {
        self.scanner.format_parse_error(filename, err)
    }

    pub fn read<L: Loader>(
        &mut self,
        loader: &mut L,
    ) -> ParseResult<Option<Statement<'text, L::Path>>> {
        loop {
            match self.scanner.peek() {
                b'\0' => return Ok(None),
                b'\n' => self.scanner.next(),
                b'#' => self.skip_comment()?,
                b' ' | b'\t' => return self.scanner.parse_error("unexpected whitespace"),
                _ => {
                    let ident = self.read_ident()?;
                    self.scanner.skip_spaces();
                    match ident {
                        b"rule" => return Ok(Some(Statement::Rule(self.read_rule()?))),
                        b"build" => return Ok(Some(Statement::Build(self.read_build(loader)?))),
                        b"default" => {
                            return Ok(Some(Statement::Default(self.read_default(loader)?)))
                        }
                        b"include" => {
                            let id = match self.read_path(loader)? {
                                None => return self.scanner.parse_error("expected path"),
                                Some(p) => p,
                            };
                            return Ok(Some(Statement::Include(id)));
                        }
                        b"subninja" => {
                            let id = match self.read_path(loader)? {
                                None => return self.scanner.parse_error("expected path"),
                                Some(p) => p,
                            };
                            return Ok(Some(Statement::Subninja(id)));
                        }
                        b"pool" => return Ok(Some(Statement::Pool(self.read_pool()?))),
                        ident => {
                            let val = self.read_vardef()?.evaluate(&[&self.vars]);
                            self.vars.insert(ident, val);
                        }
                    }
                }
            }
        }
    }

    fn read_vardef(&mut self) -> ParseResult<EvalString<&'text bstr>> {
        self.scanner.skip_spaces();
        self.scanner.expect(b'=')?;
        self.scanner.skip_spaces();
        self.read_eval()
    }

    fn read_scoped_vars(&mut self) -> ParseResult<LazyVars> {
        let mut vars = LazyVars::new();
        while self.scanner.peek() == b' ' {
            self.scanner.skip_spaces();
            let name = self.read_ident()?;
            self.scanner.skip_spaces();
            let val = self.read_vardef()?;
            vars.insert(name.to_owned(), val.into_owned());
        }
        Ok(vars)
    }

    fn read_rule(&mut self) -> ParseResult<Rule<'text>> {
        let name = self.read_ident()?;
        self.scanner.expect(b'\n')?;
        let vars = self.read_scoped_vars()?;
        Ok(Rule { name, vars })
    }

    fn read_pool(&mut self) -> ParseResult<Pool<'text>> {
        let name = self.read_ident()?;
        self.scanner.expect(b'\n')?;
        let vars = self.read_scoped_vars()?;
        let mut depth = 0;
        for (key, val) in vars.keyvals() {
            match key.as_str()? {
                "depth" => {
                    let val = val.evaluate(&[]);
                    depth = match val.as_str()?.parse::<usize>() {
                        Ok(d) => d,
                        Err(err) => {
                            return self.scanner.parse_error(format!("pool depth: {}", err))
                        }
                    }
                }
                _ => {
                    return self
                        .scanner
                        .parse_error(format!("unexpected pool attribute {:?}", key));
                }
            }
        }
        Ok(Pool { name, depth })
    }

    fn read_paths_to<L: Loader>(
        &mut self,
        loader: &mut L,
        v: &mut Vec<L::Path>,
    ) -> ParseResult<()> {
        self.scanner.skip_spaces();
        while let Some(path) = self.read_path(loader)? {
            v.push(path);
            self.scanner.skip_spaces();
        }
        Ok(())
    }

    fn read_build<L: Loader>(&mut self, loader: &mut L) -> ParseResult<Build<'text, L::Path>> {
        let line = self.scanner.line;
        let mut outs = Vec::new();
        self.read_paths_to(loader, &mut outs)?;
        let explicit_outs = outs.len();

        if self.scanner.peek() == b'|' {
            self.scanner.next();
            self.read_paths_to(loader, &mut outs)?;
        }

        self.scanner.expect(b':')?;
        self.scanner.skip_spaces();
        let rule = self.read_ident()?;

        let mut ins = Vec::new();
        self.read_paths_to(loader, &mut ins)?;
        let explicit_ins = ins.len();

        if self.scanner.peek() == b'|' {
            self.scanner.next();
            if self.scanner.peek() == b'|' {
                self.scanner.back();
            } else {
                self.read_paths_to(loader, &mut ins)?;
            }
        }
        let implicit_ins = ins.len() - explicit_ins;

        if self.scanner.peek() == b'|' {
            self.scanner.next();
            self.scanner.expect(b'|')?;
            self.read_paths_to(loader, &mut ins)?;
        }
        let order_only_ins = ins.len() - implicit_ins - explicit_ins;

        self.scanner.expect(b'\n')?;
        let vars = self.read_scoped_vars()?;
        Ok(Build {
            rule,
            line,
            outs,
            explicit_outs,
            ins,
            explicit_ins,
            implicit_ins,
            order_only_ins,
            vars,
        })
    }

    fn read_default<L: Loader>(&mut self, loader: &mut L) -> ParseResult<Vec<L::Path>> {
        let mut defaults = Vec::new();
        while let Some(path) = self.read_path(loader)? {
            defaults.push(path);
            self.scanner.skip_spaces();
        }
        if defaults.is_empty() {
            return self.scanner.parse_error("expected path");
        }
        self.scanner.expect(b'\n')?;
        Ok(defaults)
    }

    fn skip_comment(&mut self) -> ParseResult<()> {
        loop {
            match self.scanner.read() {
                b'\0' => {
                    self.scanner.back();
                    return Ok(());
                }
                b'\n' => return Ok(()),
                _ => {}
            }
        }
    }

    fn read_ident(&mut self) -> ParseResult<&'text bstr> {
        let start = self.scanner.ofs;
        while is_ident_char(self.scanner.read() as u8) {}
        self.scanner.back();
        let end = self.scanner.ofs;
        if end == start {
            return self.scanner.parse_error("failed to scan ident");
        }
        Ok(self.scanner.slice(start, end))
    }

    fn read_eval(&mut self) -> ParseResult<EvalString<&'text bstr>> {
        // Guaranteed at least one part.
        let mut parts = Vec::with_capacity(1);
        let mut ofs = self.scanner.ofs;
        loop {
            match self.scanner.read() {
                b'\0' => return self.scanner.parse_error("unexpected EOF"),
                b'\n' => break,
                b'$' => {
                    let end = self.scanner.ofs - 1;
                    if end > ofs {
                        parts.push(EvalPart::Literal(self.scanner.slice(ofs, end)));
                    }
                    parts.push(self.read_escape()?);
                    ofs = self.scanner.ofs;
                }
                _ => {}
            }
        }
        let end = self.scanner.ofs - 1;
        if end > ofs {
            parts.push(EvalPart::Literal(self.scanner.slice(ofs, end)));
        }
        Ok(EvalString::new(parts))
    }

    fn read_path<L: Loader>(&mut self, loader: &mut L) -> ParseResult<Option<L::Path>> {
        let mut byte_buf = ByteString::with_capacity(64);
        loop {
            let c = self.scanner.read();
            if is_path_char(c as u8) {
                byte_buf.push(c);
            } else {
                match c {
                    b'\0' => {
                        self.scanner.back();
                        return self.scanner.parse_error("unexpected EOF");
                    }
                    b'$' => {
                        let part = self.read_escape()?;
                        match part {
                            EvalPart::Literal(l) => byte_buf.extend_from_slice(l),
                            EvalPart::VarRef(v) => {
                                if let Some(v) = self.vars.get(v) {
                                    byte_buf.extend_from_slice(v);
                                }
                            }
                        }
                    }
                    b':' | b'|' | b' ' | b'\n' => {
                        self.scanner.back();
                        break;
                    }
                    c => {
                        self.scanner.back();
                        return self
                            .scanner
                            .parse_error(format!("unexpected character {:?}", c));
                    }
                }
            }
        }
        if byte_buf.is_empty() {
            Ok(None)
        } else {
            let file_id = loader.path(byte_buf.into_os_string()?);
            Ok(Some(file_id))
        }
    }

    fn read_escape(&mut self) -> ParseResult<EvalPart<&'text bstr>> {
        Ok(match self.scanner.read() {
            b'\n' => {
                self.scanner.skip_spaces();
                EvalPart::Literal(self.scanner.slice(0, 0))
            }
            b' ' | b'$' | b':' => {
                EvalPart::Literal(self.scanner.slice(self.scanner.ofs - 1, self.scanner.ofs))
            }
            b'{' => {
                let start = self.scanner.ofs;
                loop {
                    match self.scanner.read() {
                        b'\0' => return self.scanner.parse_error("unexpected EOF"),
                        b'}' => break,
                        _ => {}
                    }
                }
                let end = self.scanner.ofs - 1;
                EvalPart::VarRef(self.scanner.slice(start, end))
            }
            _ => {
                self.scanner.back();
                let ident = self.read_ident()?;
                EvalPart::VarRef(ident)
            }
        })
    }
}

struct StringLoader {}
impl Loader for StringLoader {
    type Path = OsString;
    fn path(&mut self, path_buf: OsString) -> Self::Path {
        path_buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_defaults() {
        let mut buf = "
var = 3
default a b$var c
        "
        .as_bytes()
        .to_vec();
        let mut parser = Parser::new(&mut buf);
        let default = match parser.read(&mut StringLoader {}).unwrap().unwrap() {
            Statement::Default(d) => d,
            s => panic!("expected default, got {:?}", s),
        };
        assert_eq!(
            default,
            &[OsStr::new("a"), OsStr::new("b3"), OsStr::new("c")]
        );
        println!("{:?}", default);
    }
}
