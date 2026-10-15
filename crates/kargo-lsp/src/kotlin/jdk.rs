//! The JDK's classes, from its sources (`lib/src.zip`), parsed when a lookup first needs them.
//!
//! The archive holds about fifteen thousand files, so parsing it up front would cost seconds and
//! a lot of memory. This reads only the archive's table of contents and each module's
//! unqualified exports (`module-info.java`), and parses a class the first time a name in it is
//! looked up; a parsed file stays for the session. Each file is also written under
//! `~/.kargo/cache/lsp-sources`, so go to definition opens it. Packages that aren't exported
//! (`sun.*`, `jdk.internal.*`) are left out, as the compiler leaves them out.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use zip::ZipArchive;

use super::index::{JDK_UNITS, Origin, Platforms, SymId, Unit};
use super::java;
use super::lexer::{self, Tok};

/// One source file of the archive.
struct Entry {
    /// The name in the archive: `java.base/java/util/List.java`.
    path: String,
    package: String,
    simple: String,
    /// The fully qualified name of its class: `java.util.List`.
    fq: String,
}

/// The JDK's classes. Share it through an `Arc`: the parsed files are kept in it.
pub struct Jdk {
    name: String,
    zip: Mutex<ZipArchive<File>>,
    /// Where the files are written out for go to definition.
    dir: PathBuf,
    entries: Vec<Entry>,
    /// The entry of each exported top-level class, by its fully qualified name.
    by_fq: HashMap<String, usize>,
    /// The entries of exported top-level classes, by simple name, sorted.
    by_simple: BTreeMap<String, Vec<usize>>,
    /// The entries of exported top-level classes, by package.
    by_package: BTreeMap<String, Vec<usize>>,
    /// The exported packages, and the packages above them.
    pub packages: BTreeSet<String>,
    units: Vec<OnceLock<Arc<Unit>>>,
}

impl fmt::Debug for Jdk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Jdk").field("name", &self.name).field("classes", &self.entries.len()).finish_non_exhaustive()
    }
}

impl Jdk {
    /// The JDK at `home`, if it ships its sources. `name` is what its classes are called in
    /// documentation (`JDK 25`); `cache` is where its files are written.
    pub fn open(home: &Path, cache: &Path, name: &str) -> Option<Jdk> {
        let archive = home.join("lib").join("src.zip");
        if !archive.is_file() {
            return None;
        }
        let file = File::open(&archive).ok()?;
        let mut zip = ZipArchive::new(file).ok()?;
        let names: Vec<String> = zip.file_names().filter_map(Result::ok).filter(|n| n.ends_with(".java")).map(|n| n.into_owned()).collect();
        let exports = exported_packages(&mut zip, &names);
        let mut entries = Vec::new();
        for path in names {
            let Some((module, rest)) = path.split_once('/') else { continue };
            let Some(class) = rest.strip_suffix(".java") else { continue };
            if class.ends_with("module-info") || class.ends_with("package-info") {
                continue;
            }
            let (dir, simple) = class.rsplit_once('/').unwrap_or(("", class));
            let package = dir.replace('/', ".");
            if package.is_empty() || !exports.contains(&format!("{module}:{package}")) {
                continue;
            }
            let fq = format!("{package}.{simple}");
            entries.push(Entry { path: path.clone(), package, simple: simple.to_owned(), fq });
        }
        let mut fp = kargo_util::hash::Fingerprint::new();
        fp.file_stamp(&archive);
        let hash = fp.finish();
        let dir = cache.join("lsp-sources").join(format!("jdk-src-{}", &hash[..12]));
        let mut out = Jdk {
            name: name.to_owned(),
            zip: Mutex::new(zip),
            dir,
            by_fq: HashMap::new(),
            by_simple: BTreeMap::new(),
            by_package: BTreeMap::new(),
            packages: BTreeSet::new(),
            units: (0..entries.len()).map(|_| OnceLock::new()).collect(),
            entries,
        };
        for (i, entry) in out.entries.iter().enumerate() {
            out.by_fq.insert(entry.fq.clone(), i);
            out.by_simple.entry(entry.simple.clone()).or_default().push(i);
            out.by_package.entry(entry.package.clone()).or_default().push(i);
            let mut package = String::new();
            for part in entry.package.split('.') {
                if !package.is_empty() {
                    package.push('.');
                }
                package.push_str(part);
                out.packages.insert(package.clone());
            }
        }
        Some(out)
    }

    /// The parsed file of entry `i`, read and parsed the first time.
    pub fn unit(&self, i: usize) -> Option<&Arc<Unit>> {
        Some(self.units.get(i)?.get_or_init(|| self.load(i)))
    }

    fn load(&self, i: usize) -> Arc<Unit> {
        let entry = &self.entries[i];
        let text = self.read(&entry.path).unwrap_or_default();
        let path = self.dir.join(&entry.path);
        if !path.is_file() {
            let _ = kargo_util::fs::write_string_atomic(&path, &text);
        }
        Arc::new(Unit::new(path, text, Origin::Library { name: self.name.clone(), main: Platforms::JVM, test: Platforms::NONE }))
    }

    fn read(&self, name: &str) -> Option<String> {
        let mut zip = self.zip.lock().unwrap_or_else(|e| e.into_inner());
        let mut file = zip.by_name(name).ok()?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).ok()?;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// The symbol of the top-level class of entry `i`, parsing its file.
    fn top(&self, i: usize) -> Option<SymId> {
        let file = &self.unit(i)?.file;
        let simple = &self.entries[i].simple;
        let decl = file.top.iter().copied().find(|&d| file.decls[d].name == *simple && !file.decls[d].hidden)?;
        Some(SymId { lib: true, unit: JDK_UNITS + i as u32, decl: decl as u32 })
    }

    /// The classes with the fully qualified name `fq`: a top-level class, or a class nested in one.
    pub fn by_fq(&self, fq: &str) -> Vec<SymId> {
        let mut end = fq.len();
        loop {
            let prefix = &fq[..end];
            if let Some(&i) = self.by_fq.get(prefix) {
                if prefix.len() == fq.len() {
                    return self.top(i).into_iter().collect();
                }
                let Some(unit) = self.unit(i) else { return Vec::new() };
                let file = &unit.file;
                return (0..file.decls.len())
                    .filter(|&d| file.decls[d].kind.is_class() && !file.decls[d].hidden && file.fq_name(d) == fq)
                    .map(|d| SymId { lib: true, unit: JDK_UNITS + i as u32, decl: d as u32 })
                    .collect();
            }
            match prefix.rfind('.') {
                Some(dot) => end = dot,
                None => return Vec::new(),
            }
        }
    }

    /// The top-level classes with the simple name `name` that Kotlin doesn't map to its own.
    pub fn named(&self, name: &str) -> Vec<SymId> {
        let Some(list) = self.by_simple.get(name) else { return Vec::new() };
        list.iter().filter(|&&i| java::kotlin_name(&self.entries[i].fq).is_none()).filter_map(|&i| self.top(i)).collect()
    }

    /// The top-level classes of `package` that Kotlin doesn't map to its own.
    pub fn in_package(&self, package: &str) -> Vec<SymId> {
        let Some(list) = self.by_package.get(package) else { return Vec::new() };
        list.iter().filter(|&&i| java::kotlin_name(&self.entries[i].fq).is_none()).filter_map(|&i| self.top(i)).collect()
    }

    /// Every simple name of an exported top-level class, sorted.
    pub fn simple_names(&self) -> impl Iterator<Item = &str> {
        self.by_simple.keys().map(String::as_str)
    }
}

/// The exports of each module that aren't qualified (`exports p;`, not `exports p to m;`), as
/// `module:package`, read from the modules' `module-info.java`.
fn exported_packages(zip: &mut ZipArchive<File>, names: &[String]) -> HashSet<String> {
    let mut out = HashSet::new();
    for name in names.iter().filter(|n| n.ends_with("/module-info.java")) {
        let Some(module) = name.strip_suffix("/module-info.java") else { continue };
        let mut text = String::new();
        let Ok(mut file) = zip.by_name(name) else { continue };
        if file.read_to_string(&mut text).is_err() {
            continue;
        }
        let lexed = lexer::lex(&text);
        let tokens = &lexed.tokens;
        let mut i = 0;
        while i < tokens.len() {
            if lexed.is(&text, i, "exports") {
                let mut package = String::new();
                let mut qualified = false;
                let mut j = i + 1;
                while j < tokens.len() && tokens[j].kind != Tok::Semi {
                    if lexed.is(&text, j, "to") {
                        qualified = true;
                    } else if tokens[j].kind == Tok::Ident {
                        package.push_str(lexed.text(&text, j));
                    } else if tokens[j].kind == Tok::Dot {
                        package.push('.');
                    }
                    j += 1;
                }
                if !qualified && !package.is_empty() {
                    out.insert(format!("{module}:{package}"));
                }
                i = j;
            }
            i += 1;
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write;
    use std::path::Path;

    use super::*;

    /// A JDK home whose `lib/src.zip` holds `files`, in a temporary directory that lives as long as the result.
    pub(crate) fn fake_jdk(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("jdk");
        std::fs::create_dir_all(home.join("lib")).unwrap();
        let mut zip = zip::ZipWriter::new(File::create(home.join("lib").join("src.zip")).unwrap());
        for (name, text) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(text.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        (dir, home)
    }

    const FILES: &[(&str, &str)] = &[
        (
            "java.base/module-info.java",
            "module java.base {\n    exports java.util;\n    exports java.io;\n    exports sun.nio.cs to jdk.charsets;\n}\n",
        ),
        (
            "java.base/java/util/ArrayList.java",
            "package java.util;\npublic class ArrayList<E> extends AbstractList<E> implements List<E> {\n  public ArrayList() {}\n  public int size() { return 0; }\n  public static class Builder {}\n  void hidden() {}\n}\n",
        ),
        ("java.base/java/util/List.java", "package java.util;\npublic interface List<E> extends Collection<E> { int size(); }\n"),
        ("java.base/java/util/AbstractList.java", "package java.util;\nabstract class AbstractList<E> {}\n"),
        (
            "java.base/java/io/File.java",
            "package java.io;\npublic class File { public File(String path) {} public static final String separator = \"/\"; }\n",
        ),
        ("java.base/sun/nio/cs/Hidden.java", "package sun.nio.cs;\npublic class Hidden {}\n"),
        ("java.base/module-info.class", "not a source"),
    ];

    #[test]
    fn exposes_only_exported_packages_and_their_public_classes() {
        let (dir, home) = fake_jdk(FILES);
        let jdk = Jdk::open(&home, &dir.path().join("cache"), "JDK test").expect("a JDK with sources");
        assert!(jdk.packages.contains("java.util") && jdk.packages.contains("java") && jdk.packages.contains("java.io"));
        assert!(!jdk.packages.contains("sun.nio.cs"), "{:?}", jdk.packages);
        assert_eq!(jdk.by_fq("java.util.ArrayList").len(), 1);
        assert!(jdk.by_fq("java.util.AbstractList").is_empty(), "a package-private class isn't offered");
        assert!(jdk.by_fq("sun.nio.cs.Hidden").is_empty());
        assert_eq!(jdk.by_fq("java.util.ArrayList.Builder").len(), 1, "a nested class is found by its name");
        assert_eq!(jdk.named("File").len(), 1);
        let package: Vec<String> = jdk
            .in_package("java.util")
            .iter()
            .map(|s| jdk.unit(s.unit as usize - JDK_UNITS as usize).unwrap().file.decls[s.decl_id()].name.clone())
            .collect();
        // `java.util.List` is Kotlin's `MutableList`, so the package doesn't list it.
        assert_eq!(package, ["ArrayList"], "{package:?}");
        assert!(!package.contains(&"AbstractList".to_owned()));
    }

    #[test]
    fn parses_a_class_when_it_is_looked_up_and_writes_its_source() {
        let (dir, home) = fake_jdk(FILES);
        let cache = dir.path().join("cache");
        let jdk = Jdk::open(&home, &cache, "JDK test").unwrap();
        let [sym] = jdk.by_fq("java.util.ArrayList")[..] else { panic!("one ArrayList") };
        let unit = jdk.unit(sym.unit as usize - JDK_UNITS as usize).unwrap();
        assert!(unit.is_library() && unit.file.package == "java.util");
        assert!(unit.file.decls[sym.decl_id()].members.iter().any(|&m| unit.file.decls[m].name == "size"));
        assert!(unit.path.is_file(), "{} is written out for go to definition", unit.path.display());
        assert!(unit.path.starts_with(&cache), "{}", unit.path.display());
        assert!(unit.path.ends_with(Path::new("java.base").join("java").join("util").join("ArrayList.java")), "{}", unit.path.display());
        assert_eq!(std::fs::read_to_string(&unit.path).unwrap(), unit.text);
    }

    /// Parses every class of the JDK on this machine (from `JAVA_HOME`), to find what the parser
    /// doesn't read. Run with `cargo test -p kargo-lsp -- --ignored parses_the_whole_jdk --nocapture`.
    #[test]
    #[ignore]
    fn parses_the_whole_jdk() {
        let home = PathBuf::from(std::env::var("JAVA_HOME").expect("JAVA_HOME"));
        let cache = tempfile::tempdir().unwrap();
        let jdk = Jdk::open(&home, cache.path(), "JDK").expect("a JDK with sources");
        let start = std::time::Instant::now();
        let names: Vec<String> = jdk.simple_names().map(str::to_owned).collect();
        let (mut classes, mut members) = (0, 0);
        for name in &names {
            for sym in jdk.named(name) {
                classes += 1;
                let unit = jdk.unit(sym.unit as usize - JDK_UNITS as usize).unwrap();
                members += unit.file.decls[sym.decl_id()].members.len();
            }
        }
        println!("{} names, {classes} classes, {members} members, in {:?}", names.len(), start.elapsed());
        assert!(classes > 2000, "{classes} classes");
    }
}
