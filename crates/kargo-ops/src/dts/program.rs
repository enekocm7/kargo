//! The `.d.ts` files of npm packages as one program: modules found as Node.js and TypeScript
//! find them (`node_modules/<name>`, its `types` entry, or `node_modules/@types/<name>`), their
//! scopes, and lookups of names through imports, namespaces and re-exports.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use super::parser::{self, Decl, DeclKind, ImportWhat, Stmt};

pub type ScopeId = usize;
pub type DeclId = usize;

/// The most files read for one generation, so that a huge dependency graph stays bounded.
const MAX_FILES: usize = 400;

/// What a name denotes: the declarations merged under it (an interface, a function and a
/// namespace can share one), or a whole module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ent {
    Decls(Vec<DeclId>),
    Module(ScopeId),
}

#[derive(Debug, Clone)]
enum Import {
    Module { spec: String, what: ImportWhat },
    Alias(Vec<String>),
}

/// The exports of a module scope.
#[derive(Debug, Default)]
struct ModuleInfo {
    /// `export = path`
    assign: Option<Vec<String>>,
    /// `export default path`
    default: Option<Vec<String>>,
    /// Exported name and the local path it denotes.
    names: Vec<(String, Vec<String>)>,
    /// `export { name as exported } from "spec"`: (spec, name, exported).
    from: Vec<(String, String, String)>,
    /// `export * from "spec"`
    star: Vec<String>,
    /// `export * as name from "spec"`
    star_as: Vec<(String, String)>,
}

pub struct Scope {
    pub parent: Option<ScopeId>,
    /// The file the scope is in.
    pub file: PathBuf,
    names: BTreeMap<String, Vec<DeclId>>,
    imports: HashMap<String, Import>,
    module: Option<ModuleInfo>,
    /// The namespace this scope is the body of.
    pub namespace: Option<String>,
}

pub struct DeclNode {
    pub decl: Decl,
    /// Where the declaration is.
    pub scope: ScopeId,
    /// A namespace's body.
    pub body: Option<ScopeId>,
}

#[derive(Default)]
pub struct Program {
    pub scopes: Vec<Scope>,
    pub decls: Vec<DeclNode>,
    files: HashMap<PathBuf, Option<ScopeId>>,
    ambient: HashMap<String, ScopeId>,
    /// Directories searched for packages after those above each file (`node_modules` folders).
    roots: Vec<PathBuf>,
    depth: usize,
}

impl Program {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Program { roots, ..Program::default() }
    }

    /// The module scope of the `.d.ts` file at `path`, read on first use.
    pub fn load(&mut self, path: &Path) -> Option<ScopeId> {
        let path = normalize(path);
        if let Some(scope) = self.files.get(&path) {
            return *scope;
        }
        if self.files.len() >= MAX_FILES {
            return None;
        }
        self.files.insert(path.clone(), None);
        let src = std::fs::read_to_string(&path).ok()?;
        let stmts = parser::parse(&src);
        let scope = self.new_scope(None, &path, None, true);
        self.files.insert(path, Some(scope));
        self.add(scope, stmts);
        Some(scope)
    }

    fn new_scope(&mut self, parent: Option<ScopeId>, file: &Path, namespace: Option<String>, module: bool) -> ScopeId {
        self.scopes.push(Scope {
            parent,
            file: file.to_path_buf(),
            names: BTreeMap::new(),
            imports: HashMap::new(),
            module: module.then(ModuleInfo::default),
            namespace,
        });
        self.scopes.len() - 1
    }

    fn add(&mut self, scope: ScopeId, stmts: Vec<Stmt>) {
        for stmt in stmts {
            match stmt {
                Stmt::Decl(decl) => self.add_decl(scope, decl),
                Stmt::Import { local, spec, what } => {
                    self.scopes[scope].imports.insert(local, Import::Module { spec, what });
                }
                Stmt::ImportAlias { local, path } => {
                    self.scopes[scope].imports.insert(local, Import::Alias(path));
                }
                Stmt::ExportAssign(path) => self.module_info(scope).assign = Some(path),
                Stmt::ExportDefault(path) => self.module_info(scope).default = Some(path),
                Stmt::ExportNames { spec: None, names } => {
                    for (local, exported) in names {
                        self.module_info(scope).names.push((exported, vec![local]));
                    }
                }
                Stmt::ExportNames { spec: Some(spec), names } => {
                    for (name, exported) in names {
                        self.module_info(scope).from.push((spec.clone(), name, exported));
                    }
                }
                Stmt::ExportStar { spec, alias: None } => self.module_info(scope).star.push(spec),
                Stmt::ExportStar { spec, alias: Some(alias) } => self.module_info(scope).star_as.push((alias, spec)),
                Stmt::AmbientModule { spec, body } => {
                    let file = self.scopes[scope].file.clone();
                    let module = match self.ambient.get(&spec) {
                        Some(&m) => m,
                        None => {
                            let m = self.new_scope(None, &file, None, true);
                            self.ambient.insert(spec, m);
                            m
                        }
                    };
                    self.add(module, body);
                }
            }
        }
    }

    fn module_info(&mut self, scope: ScopeId) -> &mut ModuleInfo {
        self.scopes[scope].module.get_or_insert_with(ModuleInfo::default)
    }

    fn add_decl(&mut self, scope: ScopeId, mut decl: Decl) {
        let name = decl.name.clone();
        if decl.exported && self.scopes[scope].namespace.is_none() {
            let info = self.module_info(scope);
            if decl.default {
                info.default = Some(vec![name.clone()]);
            } else {
                info.names.push((name.clone(), vec![name.clone()]));
            }
        }
        let body = match &mut decl.kind {
            DeclKind::Namespace(stmts) => {
                let stmts = std::mem::take(stmts);
                // Namespaces of one name in one scope share their members.
                let existing = self.scopes[scope].names.get(&name).into_iter().flatten().find_map(|&d| self.decls[d].body);
                let body = existing.unwrap_or_else(|| {
                    let file = self.scopes[scope].file.clone();
                    self.new_scope(Some(scope), &file, Some(name.clone()), false)
                });
                self.add(body, stmts);
                Some(body)
            }
            _ => None,
        };
        self.decls.push(DeclNode { decl, scope, body });
        let id = self.decls.len() - 1;
        self.scopes[scope].names.entry(name).or_default().push(id);
    }

    // ----- module resolution -----

    /// The module `spec` imported from a file in `from`.
    pub fn module(&mut self, from: ScopeId, spec: &str) -> Option<ScopeId> {
        if let Some(&m) = self.ambient.get(spec) {
            return Some(m);
        }
        let dir = self.scopes[from].file.parent()?.to_path_buf();
        let file = resolve_module(&dir, spec, &self.roots)?;
        self.load(&file)
    }

    /// The module of the npm package `name`, as Kotlin code importing it sees it.
    pub fn package(&mut self, name: &str) -> Option<ScopeId> {
        if let Some(&m) = self.ambient.get(name) {
            return Some(m);
        }
        let file = self.roots.iter().find_map(|root| resolve_bare(root, name))?;
        let scope = self.load(&file)?;
        // A file of `declare module "name" { ... }` declares the package instead.
        Some(self.ambient.get(name).copied().unwrap_or(scope))
    }

    // ----- names -----

    fn enter(&mut self) -> bool {
        if self.depth > 40 {
            return false;
        }
        self.depth += 1;
        true
    }

    /// What `path` denotes in `scope`.
    pub fn resolve(&mut self, scope: ScopeId, path: &[String]) -> Option<Ent> {
        if !self.enter() {
            return None;
        }
        let out = self.resolve_inner(scope, path);
        self.depth -= 1;
        out
    }

    fn resolve_inner(&mut self, scope: ScopeId, path: &[String]) -> Option<Ent> {
        let (first, rest) = path.split_first()?;
        let mut ent = self.lookup(scope, first)?;
        for name in rest {
            ent = self.member(&ent, name)?;
        }
        Some(ent)
    }

    fn lookup(&mut self, scope: ScopeId, name: &str) -> Option<Ent> {
        let mut current = Some(scope);
        while let Some(s) = current {
            if let Some(decls) = self.scopes[s].names.get(name) {
                return Some(Ent::Decls(decls.clone()));
            }
            if let Some(import) = self.scopes[s].imports.get(name).cloned() {
                return match import {
                    Import::Alias(path) => self.resolve(s, &path),
                    Import::Module { spec, what } => {
                        let module = self.module(s, &spec)?;
                        match what {
                            ImportWhat::Namespace | ImportWhat::Equals => {
                                Some(self.export_assignment(module).unwrap_or(Ent::Module(module)))
                            }
                            ImportWhat::Default => self.default_export(module),
                            ImportWhat::Named(n) => self.export(module, &n),
                        }
                    }
                };
            }
            current = self.scopes[s].parent;
        }
        None
    }

    /// A member of a namespace or module.
    pub fn member(&mut self, ent: &Ent, name: &str) -> Option<Ent> {
        match ent {
            Ent::Module(m) => self.export(*m, name),
            Ent::Decls(decls) => {
                let bodies: Vec<ScopeId> = decls.iter().filter_map(|&d| self.decls[d].body).collect();
                bodies.into_iter().find_map(|b| {
                    if let Some(decls) = self.scopes[b].names.get(name) {
                        return Some(Ent::Decls(decls.clone()));
                    }
                    self.scopes[b].imports.contains_key(name).then(|| self.lookup(b, name)).flatten()
                })
            }
        }
    }

    /// What a module's `export =` denotes.
    pub fn export_assignment(&mut self, module: ScopeId) -> Option<Ent> {
        let path = self.scopes[module].module.as_ref()?.assign.clone()?;
        self.resolve(module, &path)
    }

    /// A module's default export (or its `export =`, as `esModuleInterop` imports it).
    pub fn default_export(&mut self, module: ScopeId) -> Option<Ent> {
        let info = self.scopes[module].module.as_ref()?;
        match info.default.clone() {
            Some(path) => self.resolve(module, &path),
            None => self.export_assignment(module),
        }
    }

    /// The export `name` of a module.
    pub fn export(&mut self, module: ScopeId, name: &str) -> Option<Ent> {
        if !self.enter() {
            return None;
        }
        let out = self.export_inner(module, name);
        self.depth -= 1;
        out
    }

    fn export_inner(&mut self, module: ScopeId, name: &str) -> Option<Ent> {
        if let Some(assigned) = self.export_assignment(module) {
            return self.member(&assigned, name);
        }
        let info = self.scopes[module].module.as_ref()?;
        if let Some((_, path)) = info.names.iter().find(|(n, _)| n == name) {
            let path = path.clone();
            return self.resolve(module, &path);
        }
        if let Some((spec, original, _)) = info.from.iter().find(|(_, _, n)| n == name) {
            let (spec, original) = (spec.clone(), original.clone());
            let target = self.module(module, &spec)?;
            return if original == "default" { self.default_export(target) } else { self.export(target, &original) };
        }
        if let Some((_, spec)) = info.star_as.iter().find(|(n, _)| n == name) {
            let spec = spec.clone();
            return self.module(module, &spec).map(Ent::Module);
        }
        for spec in info.star.clone() {
            if let Some(target) = self.module(module, &spec)
                && let Some(ent) = self.export(target, name)
            {
                return Some(ent);
            }
        }
        None
    }

    /// The names a module exports, in declaration order, without its default export.
    pub fn export_names(&mut self, module: ScopeId) -> Vec<String> {
        if !self.enter() {
            return Vec::new();
        }
        let mut out: Vec<String> = Vec::new();
        if let Some(Ent::Decls(decls)) = self.export_assignment(module) {
            for body in decls.iter().filter_map(|&d| self.decls[d].body).collect::<Vec<_>>() {
                out.extend(self.scopes[body].names.keys().cloned());
            }
        } else if let Some(info) = self.scopes[module].module.as_ref() {
            out.extend(info.names.iter().map(|(n, _)| n.clone()));
            out.extend(info.from.iter().map(|(_, _, n)| n.clone()));
            out.extend(info.star_as.iter().map(|(n, _)| n.clone()));
            let star = info.star.clone();
            for spec in star {
                if let Some(target) = self.module(module, &spec) {
                    out.extend(self.export_names(target));
                }
            }
        }
        self.depth -= 1;
        let mut seen = std::collections::HashSet::new();
        out.retain(|n| n != "default" && seen.insert(n.clone()));
        out
    }

    /// The names of the namespaces a declaration is nested in, outermost first.
    pub fn namespaces(&self, decl: DeclId) -> Vec<String> {
        let mut out = Vec::new();
        let mut scope = Some(self.decls[decl].scope);
        while let Some(s) = scope {
            if let Some(ns) = &self.scopes[s].namespace {
                out.push(ns.clone());
            }
            scope = self.scopes[s].parent;
        }
        out.reverse();
        out
    }

    /// The `export =` path of the module a scope is in.
    pub fn assigned_name(&self, scope: ScopeId) -> Option<String> {
        let mut s = scope;
        while let Some(parent) = self.scopes[s].parent {
            s = parent;
        }
        self.scopes[s].module.as_ref()?.assign.as_ref()?.first().cloned()
    }
}

/// `a/./b/../c` as `a/c`, so that a file has one key however it was reached.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// The declaration file of `spec` imported from `dir`.
fn resolve_module(dir: &Path, spec: &str, roots: &[PathBuf]) -> Option<PathBuf> {
    if spec.starts_with("./") || spec.starts_with("../") || spec == "." || spec == ".." {
        return resolve_file(&dir.join(spec));
    }
    if spec.starts_with("node:") {
        return None;
    }
    // `node_modules` folders above the importing file, nearest first, then the roots.
    let mut current = Some(dir);
    while let Some(d) = current {
        if d.file_name().is_some_and(|n| n != "node_modules")
            && let Some(file) = resolve_bare(&d.join("node_modules"), spec)
        {
            return Some(file);
        }
        current = d.parent();
    }
    roots.iter().find_map(|root| resolve_bare(root, spec))
}

/// The declaration file of the bare specifier `spec` (`name` or `name/sub/path`) in the
/// `node_modules` folder `root`: from the package itself, or from `@types`.
pub fn resolve_bare(root: &Path, spec: &str) -> Option<PathBuf> {
    let (name, sub) = split_package(spec);
    let typed = types_name(name);
    for dir in [root.join(name), root.join("@types").join(&typed)] {
        if !dir.is_dir() {
            continue;
        }
        let found = match sub {
            Some(sub) => resolve_file(&dir.join(sub)),
            None => package_entry(&dir),
        };
        if found.is_some() {
            return found;
        }
    }
    None
}

/// `@scope/name/sub` as (`@scope/name`, `sub`).
fn split_package(spec: &str) -> (&str, Option<&str>) {
    let parts = if spec.starts_with('@') { 2 } else { 1 };
    match spec.match_indices('/').nth(parts - 1) {
        Some((i, _)) => (&spec[..i], Some(&spec[i + 1..])),
        None => (spec, None),
    }
}

/// The name of a package's `@types` package: `@scope/name` is `scope__name`.
pub fn types_name(name: &str) -> String {
    match name.strip_prefix('@') {
        Some(scoped) => scoped.replacen('/', "__", 1),
        None => name.to_owned(),
    }
}

/// The declaration file a package directory points to: `types`/`typings`, the types of its
/// `exports`, the `.d.ts` next to its `main`, or `index.d.ts`.
pub fn package_entry(dir: &Path) -> Option<PathBuf> {
    let manifest: serde_json::Value =
        std::fs::read_to_string(dir.join("package.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let mut candidates: Vec<String> = Vec::new();
    for key in ["types", "typings"] {
        if let Some(s) = manifest[key].as_str() {
            candidates.push(s.to_owned());
        }
    }
    let root_export = match &manifest["exports"] {
        serde_json::Value::Object(map) if map.contains_key(".") => map.get(".").cloned().unwrap_or_default(),
        other => other.clone(),
    };
    collect_types(&root_export, &mut candidates);
    if let Some(main) = manifest["main"].as_str() {
        candidates.push(main.to_owned());
    }
    candidates.push("index".to_owned());
    candidates.iter().find_map(|c| resolve_file(&dir.join(c)))
}

/// The `types` conditions of an `exports` entry.
fn collect_types(entry: &serde_json::Value, out: &mut Vec<String>) {
    if let serde_json::Value::Object(map) = entry {
        for (key, value) in map {
            match value {
                serde_json::Value::String(s) if key == "types" => out.push(s.clone()),
                serde_json::Value::Object(_) => collect_types(value, out),
                _ => {}
            }
        }
    }
}

/// The declaration file for `base`: itself if it is one, `base.d.ts` (or the `.d.ts` of a `.js`
/// path), or `base/index.d.ts`. A TypeScript source (`base.ts`, the `.ts` of a `.js` path) comes
/// after the declaration files, for the project's own modules.
fn resolve_file(base: &Path) -> Option<PathBuf> {
    const EXTENSIONS: [&str; 5] = [".d.ts", ".d.mts", ".d.cts", ".ts", ".mts"];
    let text = base.to_string_lossy();
    let is_typed = |p: &Path| p.is_file() && EXTENSIONS.iter().any(|e| p.to_string_lossy().ends_with(e));
    if is_typed(base) {
        return Some(base.to_path_buf());
    }
    let stem = [".js", ".mjs", ".cjs", ".ts"].iter().find_map(|e| text.strip_suffix(e)).unwrap_or(&text).to_owned();
    for ext in EXTENSIONS {
        let p = PathBuf::from(format!("{stem}{ext}"));
        if p.is_file() {
            return Some(p);
        }
    }
    if base.is_dir() {
        if base.join("package.json").is_file() {
            return package_entry(base);
        }
        for index in ["index.d.ts", "index.ts"] {
            let index = base.join(index);
            if index.is_file() {
                return Some(index);
            }
        }
    }
    None
}

/// The npm package a file belongs to: the folder after its last `node_modules`, with `@types/x`
/// standing for `x`.
pub fn package_of(file: &Path) -> Option<String> {
    let parts: Vec<String> = file.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    let i = parts.iter().rposition(|p| p == "node_modules")?;
    let first = parts.get(i + 1)?;
    let name = if first.starts_with('@') { format!("{first}/{}", parts.get(i + 2)?) } else { first.clone() };
    Some(match name.strip_prefix("@types/") {
        Some(typed) if typed.contains("__") => format!("@{}", typed.replacen("__", "/", 1)),
        Some(typed) => typed.to_owned(),
        None => name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_names() {
        assert_eq!(split_package("@a/b/c/d"), ("@a/b", Some("c/d")));
        assert_eq!(split_package("left-pad"), ("left-pad", None));
        assert_eq!(types_name("@js-joda/core"), "js-joda__core");
        assert_eq!(package_of(Path::new("/p/node_modules/@types/express/index.d.ts")).as_deref(), Some("express"));
        assert_eq!(package_of(Path::new("/p/node_modules/@types/a__b/index.d.ts")).as_deref(), Some("@a/b"));
        assert_eq!(package_of(Path::new("/p/node_modules/x/node_modules/@s/y/lib/i.d.ts")).as_deref(), Some("@s/y"));
    }
}
