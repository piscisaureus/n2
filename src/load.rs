//! Graph loading: runs .ninja parsing and constructs the build graph from it.

use crate::byte_string::*;
use crate::graph::{FileId, RspFile};
use crate::parse::Statement;
use crate::{db, eval, graph, parse, trace};
use anyhow::{anyhow, bail};
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// A variable lookup environment for magic $in/$out variables.
struct BuildImplicitVars<'a> {
    graph: &'a graph::Graph,
    build: &'a graph::Build,
}
impl<'a> BuildImplicitVars<'a> {
    fn file_name(&self, id: FileId) -> Cow<bstr> {
        Cow::Borrowed(self.graph.file(id).name.as_bstr())
    }

    fn file_list(&self, ids: &[FileId], sep: u8) -> Cow<bstr> {
        match ids.len() {
            0 => Cow::Borrowed(&[]),
            1 => self.file_name(ids[0]),
            _ => {
                let mut out = ByteString::new();
                for &id in ids {
                    if !out.is_empty() {
                        out.push(sep);
                    }
                    out.extend_from_slice(&self.file_name(id));
                }
                Cow::Owned(out)
            }
        }
    }
}

impl<'a> eval::Env for BuildImplicitVars<'a> {
    fn get_var(&self, var: &bstr) -> Option<Cow<bstr>> {
        Some(match var {
            b"in" => self.file_list(self.build.explicit_ins(), b' '),
            b"in_newline" => self.file_list(self.build.explicit_ins(), b'\n'),
            b"out" => self.file_list(self.build.explicit_outs(), b' '),
            b"out_newline" => self.file_list(self.build.explicit_outs(), b'\n'),
            _ => return None,
        })
    }
}

/// Internal state used while loading.
struct Loader {
    graph: graph::Graph,
    default: Vec<FileId>,
    rules: HashMap<ByteString, eval::LazyVars>,
    pools: Vec<(ByteString, usize)>,
}

impl parse::Loader for Loader {
    type Path = FileId;
    fn path(&mut self, path_buf: PathBuf) -> Self::Path {
        self.graph.file_id(path_buf)
    }
}

impl Loader {
    fn new() -> Self {
        let mut loader = Loader {
            graph: graph::Graph::new(),
            default: Vec::new(),
            rules: HashMap::new(),
            pools: Vec::new(),
        };

        loader
            .rules
            .insert("phony".to_byte_string(), eval::LazyVars::new());

        loader
    }

    fn add_build<'a>(
        &mut self,
        filename: Rc<PathBuf>,
        env: &eval::Vars<'a>,
        b: parse::Build<FileId>,
    ) -> anyhow::Result<()> {
        let ins = graph::BuildIns {
            ids: b.ins,
            explicit: b.explicit_ins,
            implicit: b.implicit_ins,
            // order_only is unused
        };
        let outs = graph::BuildOuts {
            ids: b.outs,
            explicit: b.explicit_outs,
        };
        let mut build = graph::Build::new(
            graph::FileLoc {
                filename,
                line: b.line,
            },
            ins,
            outs,
        );

        let rule = match self.rules.get(b.rule) {
            Some(r) => r,
            None => bail!("unknown rule {:?}", b.rule),
        };

        let implicit_vars = BuildImplicitVars {
            graph: &self.graph,
            build: &build,
        };
        let build_vars = &b.vars;
        let envs: [&dyn eval::Env; 4] = [&implicit_vars, build_vars, rule, env];

        let lookup = |key: &bstr| {
            build_vars
                .get(key)
                .or_else(|| rule.get(key))
                .map(|var| var.evaluate(&envs))
        };

        let desc = lookup(b"description");
        let pool = lookup(b"pool");

        let cmdline = lookup(b"command")
            .map(ByteString::into_os_string)
            .transpose()?;
        let depfile = lookup(b"depfile")
            .map(ByteString::into_path_buf)
            .transpose()?;

        let rspfile_path = lookup(b"rspfile");
        let rspfile_content = lookup(b"rspfile_content");
        let rspfile = match (rspfile_path, rspfile_content) {
            (None, None) => None,
            (Some(path), Some(content)) => Some(RspFile {
                path: path.into_path_buf()?,
                content,
            }),
            _ => bail!("rspfile and rspfile_content need to be both specified"),
        };

        build.cmdline = cmdline;
        build.desc = desc;
        build.depfile = depfile;
        build.rspfile = rspfile;
        build.pool = pool;

        self.graph.add_build(build);
        Ok(())
    }

    fn file_name(&self, id: FileId) -> &Path {
        &**self.graph.file(id).name
    }

    fn read_file(&mut self, id: FileId) -> anyhow::Result<()> {
        let path = self.file_name(id);
        let bytes = match trace::scope("fs::read", || std::fs::read(path)) {
            Ok(b) => b,
            Err(e) => bail!("read {:?}: {}", path, e),
        };
        self.parse(id, bytes)
    }

    fn parse(&mut self, id: FileId, mut bytes: ByteString) -> anyhow::Result<()> {
        let mut parser = parse::Parser::new(&mut bytes);
        loop {
            let stmt = match parser
                .read(self)
                .map_err(|err| anyhow!(parser.format_parse_error(self.file_name(id), err)))?
            {
                None => break,
                Some(s) => s,
            };
            match stmt {
                Statement::Include(id) => trace::scope("include", || self.read_file(id))?,
                // TODO: implement scoping for subninja
                Statement::Subninja(id) => trace::scope("subninja", || self.read_file(id))?,
                Statement::Default(defaults) => {
                    self.default.extend(defaults);
                }
                Statement::Rule(rule) => {
                    self.rules.insert(rule.name.to_owned(), rule.vars);
                }
                Statement::Build(build) => {
                    self.add_build(Rc::clone(&self.graph.file(id).name), &parser.vars, build)?
                }
                Statement::Pool(pool) => {
                    self.pools.push((pool.name.to_owned(), pool.depth));
                }
            };
        }
        Ok(())
    }
}

/// State loaded by read().
pub struct State {
    pub graph: graph::Graph,
    pub db: db::Writer,
    pub hashes: graph::Hashes,
    pub default: Vec<FileId>,
    pub pools: Vec<(ByteString, usize)>,
}

/// Load build.ninja/.n2_db and return the loaded build graph and state.
pub fn read() -> anyhow::Result<State> {
    let mut loader = Loader::new();
    trace::scope("loader.read_file", || {
        let id = loader.graph.file_id("build.ninja".to_owned());
        loader.read_file(id)
    })?;
    let mut hashes = graph::Hashes::new();
    let db = trace::scope("db::open", || {
        db::open(".n2_db", &mut loader.graph, &mut hashes)
    })
    .map_err(|err| anyhow!("load .n2_db: {}", err))?;
    Ok(State {
        graph: loader.graph,
        db,
        hashes,
        default: loader.default,
        pools: loader.pools,
    })
}

/// Parse a single file's content.
#[cfg(test)]
pub fn parse(path: impl AsRef<Path>, content: ByteString) -> anyhow::Result<graph::Graph> {
    let mut loader = Loader::new();
    let id = loader.graph.file_id(path.as_ref());
    trace::scope("loader.parse", || loader.parse(id, content))?;
    Ok(loader.graph)
}
