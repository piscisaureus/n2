//! The build graph, a graph between files and commands.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::hash::Hasher;
use std::hash::{self};
use std::rc::Rc;
use std::time::SystemTime;

use serde_derive::Deserialize;
use serde_derive::Serialize;

use crate::byte_string::*;
use crate::canon::canon_path;
use crate::canon::canon_path_in_place;
use crate::densemap::DenseMap;
use crate::densemap::{self};

/// Hash value used to identify a given instance of a Build's execution;
/// compared to verify whether a Build is up to date.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct Hash(pub u64);

/// Id for File nodes in the Graph.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct FileId(u32);
impl densemap::Index for FileId {
    fn index(&self) -> usize {
        self.0 as usize
    }
}
impl From<usize> for FileId {
    fn from(u: usize) -> FileId {
        FileId(u as u32)
    }
}

/// Id for Build nodes in the Graph.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct BuildId(u32);
impl densemap::Index for BuildId {
    fn index(&self) -> usize {
        self.0 as usize
    }
}
impl From<usize> for BuildId {
    fn from(u: usize) -> BuildId {
        BuildId(u as u32)
    }
}

/// A single file referenced as part of a build.
#[derive(Debug)]
pub struct File {
    /// Canonical path to the file.
    pub name: Rc<OsStr>,
    /// The Build that generates this file, if any.
    pub input: Option<BuildId>,
    /// The Builds that depend on this file as an input.
    pub dependents: Vec<BuildId>,
}

/// A textual location within a build.ninja file, used in error messages.
#[derive(Debug)]
pub struct FileLoc<P = FileId> {
    pub path: P,
    pub line: usize,
}

impl FileLoc<FileId> {
    pub fn fill<'a>(&self, graph: &'a Graph) -> FileLoc<&'a OsStr> {
        FileLoc {
            path: graph.files.get(self.path).name.as_ref(),
            line: self.line,
        }
    }
}

impl std::fmt::Display for FileLoc<&OsStr> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "{}:{}", self.path.as_str_lossy(), self.line)
    }
}

#[derive(Debug, Clone, Hash)]
pub struct RspFile {
    pub path: OsString,
    pub content: ByteString,
}

/// Input files to a Build.
pub struct BuildIns {
    /// Internally we stuff explicit/implicit/order-only ins all into one Vec.
    /// This is mostly to simplify some of the iteration and is a little more
    /// memory efficient than three separate Vecs, but it is kept internal to
    /// Build and only exposed via methods on Build.
    pub ids: Vec<FileId>,
    pub explicit: usize,
    pub implicit: usize,
    // order_only count implied by other counts.
    // pub order_only: usize,
}

/// Output files from a Build.
pub struct BuildOuts {
    /// Similar to ins, we keep both explicit and implicit outs in one Vec.
    pub ids: Vec<FileId>,
    pub explicit: usize,
}

/// A single build action, generating File outputs from File inputs with a command.
pub struct Build {
    /// Source location this Build was declared.
    pub location: FileLoc,

    /// User-provided description of the build step.
    pub desc: Option<ByteString>,

    /// Command line to run.  Absent for phony builds.
    pub cmdline: Option<OsString>,

    /// Path to generated `.d` file, if any.
    pub depfile: Option<OsString>,

    // Struct that contains the path to the rsp file and its contents, if any.
    pub rspfile: Option<RspFile>,

    /// Pool to execute this build in, if any.
    pub pool: Option<ByteString>,

    pub ins: BuildIns,

    /// Additional inputs discovered from a previous build.
    discovered_ins: Vec<FileId>,

    /// Output files.
    pub outs: BuildOuts,
}
impl Build {
    pub fn new(loc: FileLoc, ins: BuildIns, outs: BuildOuts) -> Self {
        Build {
            location: loc,
            desc: None,
            cmdline: None,
            depfile: None,
            rspfile: None,
            pool: None,
            ins,
            discovered_ins: Vec::new(),
            outs,
        }
    }

    /// Input paths that appear in `$in`.
    pub fn explicit_ins(&self) -> &[FileId] {
        &self.ins.ids[0..self.ins.explicit]
    }

    /// Input paths that, if changed, invalidate the output.
    /// Note this omits discovered_ins, which also invalidate the output.
    pub fn dirtying_ins(&self) -> &[FileId] {
        &self.ins.ids[0..(self.ins.explicit + self.ins.implicit)]
    }

    /// Order-only inputs: inputs that are only used for ordering execution.
    pub fn order_only_ins(&self) -> &[FileId] {
        &self.ins.ids[(self.ins.explicit + self.ins.implicit)..]
    }

    /// Inputs that are needed before building.
    /// Distinct from dirtying_ins in that it includes order-only dependencies.
    /// Note that we don't order on discovered_ins, because they're not allowed to
    /// affect build order.
    pub fn ordering_ins(&self) -> &[FileId] {
        &self.ins.ids
    }

    /// Potentially update discovered_ins with a new set of deps, returning true if they changed.
    pub fn update_discovered(&mut self, mut deps: Vec<FileId>) -> bool {
        // Filter out any deps that were already listed in the build file.
        deps.retain(|id| !self.ins.ids.contains(id));
        if deps == self.discovered_ins {
            false
        } else {
            self.set_discovered_ins(deps);
            true
        }
    }

    pub fn set_discovered_ins(&mut self, deps: Vec<FileId>) {
        self.discovered_ins = deps;
    }

    /// Input paths that were discovered after building, for use in the next build.
    pub fn discovered_ins(&self) -> &[FileId] {
        &self.discovered_ins
    }

    /// Output paths that appear in `$out`.
    pub fn explicit_outs(&self) -> &[FileId] {
        &self.outs.ids[0..self.outs.explicit]
    }

    /// Output paths that are updated when the build runs.
    pub fn outs(&self) -> &[FileId] {
        &self.outs.ids
    }
}

/// The build graph: owns Files/Builds and maps FileIds/BuildIds to them,
/// as well as mapping string filenames to the underlying Files.
pub struct Graph {
    files: DenseMap<FileId, File>,
    pub builds: DenseMap<BuildId, Build>,
    // Although the `file_to_id`'s keys are filesystem paths, we're using
    // `OsStr` to store then and not `Path`. The reason for this is that `Path`
    // has a very slow `Hash` impl, which normalizes the path every time the
    // hash is computed. It's no more than a slight inconvenience, as `OsStr`
    // implements `AsRef<Path>` and vice versa.
    file_to_id: HashMap<Rc<OsStr>, FileId>,
}

impl Graph {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Graph {
            files: DenseMap::new(),
            builds: DenseMap::new(),
            file_to_id: HashMap::new(),
        }
    }

    /// Add a new file, generating a new FileId for it.
    fn add_file(&mut self, name: Rc<OsStr>) -> FileId {
        self.files.push(File {
            name,
            input: None,
            dependents: Vec::new(),
        })
    }

    /// Look up a file by its FileId.
    pub fn file(&self, id: FileId) -> &File {
        self.files.get(id)
    }

    /// Canonicalize a path and get/generate its FileId.
    pub fn file_id(&mut self, path_buf: impl Into<OsString>) -> FileId {
        let mut path_buf = path_buf.into();
        canon_path_in_place(&mut path_buf);
        match self.file_to_id.get(&*path_buf) {
            Some(id) => *id,
            None => {
                let path_rc1 = Rc::from(path_buf.as_os_str());
                let path_rc2 = Rc::clone(&path_rc1);
                let id = self.add_file(path_rc1);
                self.file_to_id.insert(path_rc2, id);
                id
            }
        }
    }

    /// Canonicalize a path and look up its FileId.
    pub fn lookup_file_id(&self, path: impl Into<OsString>) -> Option<FileId> {
        let canon = canon_path(path);
        self.file_to_id.get(canon.as_os_str()).copied()
    }

    /// Add a new Build, generating a BuildId for it.
    pub fn add_build(&mut self, build: Build) {
        let id = self.builds.next_id();
        for &inf in &build.ins.ids {
            self.files.get_mut(inf).dependents.push(id);
        }
        for &out in &build.outs.ids {
            let f = self.files.get_mut(out);
            match f.input {
                Some(b) => {
                    // TODO this occurs when two builds claim the same output
                    // file, which is an ordinary user error and which should
                    // be pretty-printed to the user as such.
                    panic!("double link {:?}", b);
                }
                None => f.input = Some(id),
            }
        }
        self.builds.push(build);
    }

    /// Look up a Build by BuildId.
    pub fn build(&self, id: BuildId) -> &Build {
        self.builds.get(id)
    }
    /// Look up a Build by BuildId.
    pub fn build_mut(&mut self, id: BuildId) -> &mut Build {
        self.builds.get_mut(id)
    }
}

/// MTime info gathered for a file.  This also models "file is absent".
/// It's not using an Option<> just because it makes the code using it easier
/// to follow.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum MTime {
    Missing,
    Stamp(SystemTime),
}

/// stat() an on-disk path, producing its MTime.
pub fn stat(path: impl AsRef<OsStr>) -> std::io::Result<MTime> {
    // TODO: On Windows, use FindFirstFileEx()/FindNextFile() to get timestamps per
    //       directory, for better stat perf.
    Ok(match std::fs::metadata(path.as_ref()) {
        Ok(meta) => MTime::Stamp(meta.modified().unwrap()),
        Err(err) => {
            if err.kind() == std::io::ErrorKind::NotFound {
                MTime::Missing
            } else {
                return Err(err);
            }
        }
    })
}

/// Gathered state of on-disk files.
/// Due to discovered deps this map may grow after graph initialization.
pub struct FileState(DenseMap<FileId, Option<MTime>>);

impl FileState {
    pub fn new(graph: &Graph) -> Self {
        FileState(DenseMap::new_sized(graph.files.next_id(), None))
    }

    pub fn get(&self, id: FileId) -> Option<MTime> {
        *self.0.lookup(id).unwrap_or(&None)
    }

    pub fn restat(&mut self, id: FileId, path: impl AsRef<OsStr>) -> std::io::Result<MTime> {
        let mtime = stat(path)?;
        self.0.set_grow(id, Some(mtime), None);
        Ok(mtime)
    }
}

const UNIT_SEPARATOR: u8 = 0x1F;

// Add a list of files to a hasher; used by hash_build.
fn hash_files(
    hasher: &mut std::collections::hash_map::DefaultHasher,
    graph: &Graph,
    file_state: &mut FileState,
    ids: &[FileId],
) {
    for &id in ids {
        let name = &graph.file(id).name;
        let mtime = file_state
            .get(id)
            .unwrap_or_else(|| panic!("no state for {:?}", name));
        let mtime = match mtime {
            MTime::Stamp(mtime) => mtime,
            MTime::Missing => panic!("missing file: {:?}", name),
        };
        std::hash::Hash::hash(name, hasher);
        std::hash::Hash::hash(&mtime, hasher);
        hasher.write_u8(UNIT_SEPARATOR);
    }
}

// Hashes the inputs of a build to compute a signature.
// Prerequisite: all referenced files have already been stat()ed and are present.
// (It doesn't make sense to hash a build with missing files, because it's out
// of date regardless of the state of the other files.)
pub fn hash_build(
    graph: &Graph,
    file_state: &mut FileState,
    build: &Build,
) -> std::io::Result<Hash> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hash_files(&mut hasher, graph, file_state, build.dirtying_ins());
    hasher.write_u8(UNIT_SEPARATOR);
    hash_files(&mut hasher, graph, file_state, build.discovered_ins());
    hasher.write_u8(UNIT_SEPARATOR);
    hash::Hash::hash(&build.cmdline, &mut hasher);
    hasher.write_u8(UNIT_SEPARATOR);
    hash::Hash::hash(&build.rspfile, &mut hasher);
    hasher.write_u8(UNIT_SEPARATOR);
    hash_files(&mut hasher, graph, file_state, build.outs());
    Ok(Hash(hasher.finish()))
}

pub struct Hashes(HashMap<BuildId, Hash>);

impl Hashes {
    pub fn new() -> Hashes {
        Hashes(HashMap::new())
    }

    pub fn set(&mut self, id: BuildId, hash: Hash) {
        self.0.insert(id, hash);
    }

    pub fn changed(&self, id: BuildId, hash: Hash) -> bool {
        let last_hash = match self.0.get(&id) {
            None => return true,
            Some(h) => *h,
        };
        hash != last_hash
    }
}

#[test]
fn stat_mtime_resolution() {
    use std::time::Duration;

    let temp_dir = tempfile::tempdir().unwrap();
    let filename = temp_dir.path().join("dummy");

    // Write once and stat.
    std::fs::write(&filename, "foo").unwrap();
    let mtime1 = match stat(&filename).unwrap() {
        MTime::Stamp(mtime) => mtime,
        _ => panic!("File not found: {}", filename.as_str_lossy()),
    };

    // Sleep for a short interval.
    std::thread::sleep(std::time::Duration::from_millis(10));

    // Write twice and stat.
    std::fs::write(&filename, "foo").unwrap();
    let mtime2 = match stat(&filename).unwrap() {
        MTime::Stamp(mtime) => mtime,
        _ => panic!("File not found: {}", filename.as_str_lossy()),
    };

    let diff = mtime2.duration_since(mtime1).unwrap();
    assert!(diff > Duration::ZERO);
    assert!(diff < Duration::from_millis(100));
}
