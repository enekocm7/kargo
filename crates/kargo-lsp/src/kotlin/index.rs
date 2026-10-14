//! Parsed files and the maps that find their declarations: by package, by fully qualified name,
//! and extensions by receiver.
//!
//! There are two indexes: the libraries' (the standard library and the sources jars of
//! dependencies), built once in the background, and the project's, rebuilt when a file changes.
//! A [`SymId`] names a declaration in either.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use super::syntax::{self, Decl, DeclId, File, Kind};

/// Where a file comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// A file of the project, in one of its source sets.
    Source(SourceSet),
    /// A file outside every source root, open in the editor: it sees the main sources of
    /// every package.
    Loose,
    /// A sources jar; `jvm` for files that only exist on the JVM.
    Library { name: String, jvm: bool },
}

/// A source set of a Kargo package: which package, main, test or example code, and which
/// target (`None` for the common sources in `src/` and `tests/`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceSet {
    pub package: usize,
    pub kind: SetKind,
    pub target: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SetKind {
    Main,
    Test,
    Example,
}

/// One parsed file.
#[derive(Debug)]
pub struct Unit {
    pub path: PathBuf,
    pub text: String,
    pub file: File,
    pub origin: Origin,
}

impl Unit {
    pub fn new(path: PathBuf, text: String, origin: Origin) -> Self {
        let file = syntax::parse(&text);
        Self { path, text, file, origin }
    }

    pub fn is_library(&self) -> bool {
        matches!(self.origin, Origin::Library { .. })
    }

    pub fn decl(&self, id: DeclId) -> &Decl {
        &self.file.decls[id]
    }
}

/// A declaration in one of the indexes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SymId {
    pub lib: bool,
    pub unit: u32,
    pub decl: u32,
}

/// A file in one of the indexes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UnitId {
    pub lib: bool,
    pub unit: u32,
}

impl SymId {
    pub fn unit_id(self) -> UnitId {
        UnitId { lib: self.lib, unit: self.unit }
    }

    pub fn decl_id(self) -> DeclId {
        self.decl as DeclId
    }
}

impl UnitId {
    pub fn sym(self, decl: DeclId) -> SymId {
        SymId { lib: self.lib, unit: self.unit, decl: decl as u32 }
    }
}

/// The files of the libraries or of the project, with their declarations by name.
#[derive(Debug, Default)]
pub struct Index {
    pub lib: bool,
    pub units: Vec<Arc<Unit>>,
    /// Top-level declarations that aren't extensions, by package.
    pub by_package: HashMap<String, Vec<SymId>>,
    /// Classes, objects, type aliases, and top-level functions and properties, by fully
    /// qualified name (nested classes too).
    pub by_fq: HashMap<String, Vec<SymId>>,
    /// Top-level extensions by the simple name of their receiver type; `*` for a type parameter.
    pub extensions: HashMap<String, Vec<SymId>>,
    /// Top-level declarations that aren't extensions, by simple name.
    pub by_name: HashMap<String, Vec<SymId>>,
    /// Every package with declarations, and its parents.
    pub packages: BTreeSet<String>,
    /// The names of all non-local declarations, for workspace symbols.
    pub names: Vec<(String, SymId)>,
}

impl Index {
    pub fn new(lib: bool, units: Vec<Arc<Unit>>) -> Self {
        let mut index = Index { lib, units, ..Index::default() };
        for u in 0..index.units.len() {
            let unit = Arc::clone(&index.units[u]);
            let id = UnitId { lib, unit: u as u32 };
            let file = &unit.file;
            let mut package = String::new();
            for part in file.package.split('.').filter(|p| !p.is_empty()) {
                if !package.is_empty() {
                    package.push('.');
                }
                package.push_str(part);
                index.packages.insert(package.clone());
            }
            for &d in &file.top {
                index.add_top(file, id, d);
            }
        }
        index
    }

    fn add_top(&mut self, file: &File, unit: UnitId, d: DeclId) {
        let decl = &file.decls[d];
        if decl.name.is_empty() {
            return;
        }
        let sym = unit.sym(d);
        match &decl.receiver {
            Some(receiver) => {
                let key = if decl.type_params.iter().any(|t| t.name == receiver.name) || receiver.func.is_some() {
                    "*".to_owned()
                } else {
                    receiver.simple_name().to_owned()
                };
                self.extensions.entry(key).or_default().push(sym);
            }
            None => {
                self.by_package.entry(file.package.clone()).or_default().push(sym);
                self.by_name.entry(decl.name.clone()).or_default().push(sym);
                self.by_fq.entry(file.fq_name(d)).or_default().push(sym);
            }
        }
        self.names.push((decl.name.clone(), sym));
        if decl.kind.is_class() {
            self.add_nested(file, unit, d);
        }
    }

    fn add_nested(&mut self, file: &File, unit: UnitId, class: DeclId) {
        for &m in &file.decls[class].members {
            let decl = &file.decls[m];
            if decl.kind.is_class() && !decl.name.is_empty() {
                self.by_fq.entry(file.fq_name(m)).or_default().push(unit.sym(m));
                self.names.push((decl.name.clone(), unit.sym(m)));
                self.add_nested(file, unit, m);
            } else if matches!(decl.kind, Kind::Function | Kind::Property) && !decl.name.is_empty() {
                self.names.push((decl.name.clone(), unit.sym(m)));
            }
        }
    }

    pub fn unit(&self, id: UnitId) -> Option<&Arc<Unit>> {
        self.units.get(id.unit as usize)
    }
}

/// Packages Kotlin imports into every file.
pub const DEFAULT_IMPORTS: &[&str] = &[
    "kotlin",
    "kotlin.annotation",
    "kotlin.collections",
    "kotlin.comparisons",
    "kotlin.io",
    "kotlin.ranges",
    "kotlin.sequences",
    "kotlin.text",
    "kotlin.jvm",
];

/// Whether a library package is an implementation detail, left out of completion.
pub fn is_internal_package(package: &str) -> bool {
    package.split('.').any(|p| p == "internal" || p == "impl") || package.starts_with("kotlinx.cinterop.internal")
}
