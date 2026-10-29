//! Shading: `[package.shade]` rewrites the jars `kargo package` bundles. Packages move to other names (in
//! the class files, the `META-INF/services` entries and the resource paths), the files that several jars
//! carry are merged, and `minimize` drops the classes of dependencies that nothing reaches.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use kargo_manifest::ShadeSettings;

use crate::jar::{JarManifest, is_mergeable, is_signature, options};

/// What shading a jar found worth telling the user.
#[derive(Debug, Default)]
pub struct ShadeReport {
    /// Entries that several jars carry with different contents and that cannot be merged; the first one won.
    pub duplicates: Vec<String>,
    /// The jars that put at least one class into the result.
    pub bundled: Vec<PathBuf>,
    /// The classes of dependencies that `minimize` removed.
    pub removed: usize,
}

struct Entry {
    name: String,
    data: Vec<u8>,
    /// The index in the list of jars; `None` for the project's own files.
    source: Option<usize>,
}

/// Writes the jar of `classes_dir` with the contents of `jars` bundled and `settings` applied.
pub fn write_shaded_jar(
    dest: &Path,
    classes_dir: &Path,
    manifest: &JarManifest,
    jars: &[PathBuf],
    settings: &ShadeSettings,
) -> Result<ShadeReport> {
    let mut report = ShadeReport::default();
    let mut entries: Vec<Entry> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut reported: HashSet<String> = HashSet::new();
    let mut add =
        |entries: &mut Vec<Entry>, report: &mut ShadeReport, name: String, data: Vec<u8>, source: Option<usize>| match index.get(&name) {
            None => {
                index.insert(name.clone(), entries.len());
                entries.push(Entry { name, data, source });
            }
            Some(&at) if settings.merge_services && is_mergeable(&name) => {
                let existing = &mut entries[at].data;
                if !existing.ends_with(b"\n") {
                    existing.push(b'\n');
                }
                existing.extend_from_slice(&data);
            }
            Some(&at) => {
                if entries[at].data != data && reported.insert(name.clone()) {
                    report.duplicates.push(name);
                }
            }
        };

    if classes_dir.is_dir() {
        let mut files: Vec<_> =
            walkdir::WalkDir::new(classes_dir).into_iter().filter_map(|e| e.ok()).filter(|e| e.file_type().is_file()).collect();
        files.sort_by(|a, b| a.path().cmp(b.path()));
        for file in files {
            let rel = file.path().strip_prefix(classes_dir)?;
            let name = rel.to_string_lossy().replace('\\', "/");
            if name.ends_with(".fingerprint") || name == "META-INF/MANIFEST.MF" {
                continue;
            }
            let data = std::fs::read(file.path()).with_context(|| format!("failed to read {}", file.path().display()))?;
            add(&mut entries, &mut report, name, data, None);
        }
    }
    for (i, jar) in jars.iter().enumerate() {
        let mut archive = zip::ZipArchive::new(File::open(jar).with_context(|| format!("failed to open {}", jar.display()))?)
            .with_context(|| format!("failed to read {}", jar.display()))?;
        for at in 0..archive.len() {
            let mut file = archive.by_index(at)?;
            let name = file.name()?.into_owned();
            if file.is_dir()
                || name == "META-INF/MANIFEST.MF"
                || is_signature(&name)
                || name == "module-info.class"
                || name.starts_with("META-INF/versions/") && name.ends_with("module-info.class")
                || excluded(&name, &settings.exclude)
            {
                continue;
            }
            let mut data = Vec::with_capacity(file.size() as usize);
            file.read_to_end(&mut data)?;
            add(&mut entries, &mut report, name, data, Some(i));
        }
    }

    if settings.minimize {
        report.removed = minimize(&mut entries, settings);
    }
    let mut bundled: Vec<usize> = entries.iter().filter(|e| e.name.ends_with(".class")).filter_map(|e| e.source).collect();
    bundled.sort_unstable();
    bundled.dedup();
    report.bundled = bundled.into_iter().map(|i| jars[i].clone()).collect();

    let relocator = Relocator::new(settings);
    let mut out: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for entry in entries {
        let (name, data) = relocator.entry(entry.name, entry.data);
        out.entry(name).or_insert(data);
    }

    let mut manifest = manifest.clone();
    manifest.multi_release = out.keys().any(|n| n.starts_with("META-INF/versions/"));
    kargo_util::fs::write_atomic(dest, |file| {
        let mut zip = zip::ZipWriter::new(file);
        let to_io = std::io::Error::other;
        zip.start_file("META-INF/MANIFEST.MF", options()).map_err(to_io)?;
        zip.write_all(manifest.render().as_bytes())?;
        for (name, data) in &out {
            zip.start_file(name, options()).map_err(to_io)?;
            zip.write_all(data)?;
        }
        zip.finish().map_err(to_io)?;
        Ok(())
    })
    .with_context(|| format!("failed to write {}", dest.display()))?;
    Ok(report)
}

/// The dotted name of a jar entry that `exclude` and `keep` patterns match: no `.class`, no multi-release prefix.
fn dotted(entry: &str) -> String {
    let base = strip_version(entry);
    base.strip_suffix(".class").unwrap_or(base).replace('/', ".")
}

fn strip_version(entry: &str) -> &str {
    match entry.strip_prefix("META-INF/versions/").and_then(|rest| rest.split_once('/')) {
        Some((version, rest)) if version.chars().all(|c| c.is_ascii_digit()) => rest,
        _ => entry,
    }
}

fn matches(name: &str, patterns: &[String]) -> bool {
    let name = dotted(name);
    patterns.iter().any(|pattern| match pattern.strip_suffix(".*") {
        Some(package) => name.starts_with(&format!("{package}.")),
        None => name == *pattern || name.starts_with(&format!("{pattern}$")),
    })
}

fn excluded(name: &str, patterns: &[String]) -> bool {
    !patterns.is_empty() && matches(name, patterns)
}

/// Removes the classes of dependencies that no class of the project (or `keep`) reaches. Returns how many.
fn minimize(entries: &mut Vec<Entry>, settings: &ShadeSettings) -> usize {
    let mut classes: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        if e.source.is_some()
            && let Some(class) = strip_version(&e.name).strip_suffix(".class")
        {
            classes.entry(class.to_owned()).or_default().push(i);
        }
    }
    let mut reachable: HashSet<String> = HashSet::new();
    let mut queue: Vec<usize> = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        if !e.name.ends_with(".class") {
            continue;
        }
        if e.source.is_none() || matches(&e.name, &settings.keep) {
            if let Some(class) = strip_version(&e.name).strip_suffix(".class")
                && e.source.is_some()
                && reachable.insert(class.to_owned())
            {
                queue.extend(classes[class].iter().copied());
            }
            if e.source.is_none() {
                queue.push(i);
            }
        }
    }
    loop {
        while let Some(i) = queue.pop() {
            for token in class_references(&entries[i].data) {
                if classes.contains_key(&token) && reachable.insert(token.clone()) {
                    queue.extend(classes[&token].iter().copied());
                }
            }
        }
        // A provider is kept when its service can be asked for: the service is reachable or not bundled.
        let mut grew = false;
        for e in entries.iter() {
            let Some(service) = e.name.strip_prefix("META-INF/services/") else { continue };
            if !service_wanted(&service.replace('.', "/"), &classes, &reachable) {
                continue;
            }
            for line in String::from_utf8_lossy(&e.data).lines() {
                let provider = line.split('#').next().unwrap_or("").trim().replace('.', "/");
                if classes.contains_key(&provider) && reachable.insert(provider.clone()) {
                    queue.extend(classes[&provider].iter().copied());
                    grew = true;
                }
            }
        }
        if !grew {
            break;
        }
    }
    let before = entries.len();
    entries.retain(|e| {
        e.source.is_none()
            || !e.name.ends_with(".class")
            || strip_version(&e.name).strip_suffix(".class").is_some_and(|c| reachable.contains(c))
    });
    before - entries.len()
}

fn service_wanted(service: &str, classes: &HashMap<String, Vec<usize>>, reachable: &HashSet<String>) -> bool {
    !classes.contains_key(service) || reachable.contains(service)
}

/// The class names (internal form) a class file mentions in any of its constants, names, descriptors and
/// strings; a string like `"com.example.Foo"` counts, since it may be loaded by name.
fn class_references(data: &[u8]) -> Vec<String> {
    let Some(pool) = Pool::parse(data) else { return Vec::new() };
    let mut out = Vec::new();
    for (tag, start, end) in pool.slots.iter().flatten() {
        if *tag != 1 {
            continue;
        }
        let text = String::from_utf8_lossy(&data[*start..*end]);
        for token in text.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '/' | '.'))) {
            if token.is_empty() {
                continue;
            }
            let token = token.replace('.', "/");
            if let Some(rest) = token.strip_prefix('L') {
                out.push(rest.to_owned());
            }
            out.push(token);
        }
    }
    out
}

/// The constant pool of a class file: for each index the tag and the byte range of its payload (for a UTF-8
/// constant, of its bytes).
struct Pool {
    slots: Vec<Option<(u8, usize, usize)>>,
    /// Where the constant pool ends.
    end: usize,
}

impl Pool {
    fn parse(data: &[u8]) -> Option<Pool> {
        if data.len() < 10 || data[..4] != [0xCA, 0xFE, 0xBA, 0xBE] {
            return None;
        }
        let count = u16::from_be_bytes([data[8], data[9]]) as usize;
        let mut slots = vec![None; count];
        let mut at = 10;
        let mut i = 1;
        while i < count {
            let tag = *data.get(at)?;
            at += 1;
            let size = match tag {
                1 => {
                    let len = u16::from_be_bytes([*data.get(at)?, *data.get(at + 1)?]) as usize;
                    slots[i] = Some((1, at + 2, at + 2 + len));
                    at += 2 + len;
                    i += 1;
                    continue;
                }
                3 | 4 | 9 | 10 | 11 | 12 | 17 | 18 => 4,
                5 | 6 => 8,
                7 | 8 | 16 | 19 | 20 => 2,
                15 => 3,
                _ => return None,
            };
            slots[i] = Some((tag, at, at + size));
            at += size;
            i += if matches!(tag, 5 | 6) { 2 } else { 1 };
        }
        (at <= data.len()).then_some(Pool { slots, end: at })
    }
}

/// The relocation rules, as bytes in the internal (`com/foo`) and dotted (`com.foo`) forms.
struct Relocator {
    slash: Vec<(Vec<u8>, Vec<u8>)>,
    dot: Vec<(Vec<u8>, Vec<u8>)>,
    strings: bool,
}

impl Relocator {
    fn new(settings: &ShadeSettings) -> Self {
        let slash = settings.relocate.iter().map(|(a, b)| (a.replace('.', "/").into_bytes(), b.replace('.', "/").into_bytes())).collect();
        let dot = settings.relocate.iter().map(|(a, b)| (a.clone().into_bytes(), b.clone().into_bytes())).collect();
        Relocator { slash, dot, strings: settings.relocate_strings }
    }

    /// The new name and contents of an entry.
    fn entry(&self, name: String, data: Vec<u8>) -> (String, Vec<u8>) {
        if self.slash.is_empty() {
            return (name, data);
        }
        if name.ends_with(".class") {
            let data = self.class(&data).unwrap_or(data);
            return (self.path(&name), data);
        }
        if let Some(service) = name.strip_prefix("META-INF/services/") {
            let moved = replace_names(service.as_bytes(), &self.dot, false);
            let text = String::from_utf8_lossy(&data);
            let lines: Vec<String> = text
                .lines()
                .map(|line| {
                    let (code, comment) = line.split_once('#').map_or((line, None), |(c, rest)| (c, Some(rest)));
                    let code = String::from_utf8_lossy(&replace_names(code.as_bytes(), &self.dot, false)).into_owned();
                    match comment {
                        Some(rest) => format!("{code}#{rest}"),
                        None => code,
                    }
                })
                .collect();
            let mut joined = lines.join("\n");
            joined.push('\n');
            return (format!("META-INF/services/{}", String::from_utf8_lossy(&moved)), joined.into_bytes());
        }
        (self.path(&name), data)
    }

    /// The path of an entry whose directory or class moved; `META-INF/versions/N/` stays in front. Only
    /// a path that starts with a relocated package moves.
    fn path(&self, name: &str) -> String {
        let base = strip_version(name);
        let prefix = &name[..name.len() - base.len()];
        match self.slash.iter().find(|(from, _)| starts_with_name(base.as_bytes(), from)) {
            Some((from, to)) => format!("{prefix}{}{}", String::from_utf8_lossy(to), &base[from.len()..]),
            None => name.to_owned(),
        }
    }

    /// The class file with every relocated name in its UTF-8 constants rewritten.
    fn class(&self, data: &[u8]) -> Option<Vec<u8>> {
        let pool = Pool::parse(data)?;
        let strings: HashSet<usize> = if self.strings {
            pool.slots
                .iter()
                .flatten()
                .filter(|(tag, ..)| *tag == 8)
                .map(|(_, start, _)| u16::from_be_bytes([data[*start], data[*start + 1]]) as usize)
                .collect()
        } else {
            HashSet::new()
        };
        let mut out = data[..10].to_vec();
        for (i, slot) in pool.slots.iter().enumerate() {
            let Some((tag, start, end)) = slot else { continue };
            out.push(*tag);
            if *tag == 1 {
                let mut text = replace_names(&data[*start..*end], &self.slash, true);
                if strings.contains(&i) {
                    text = replace_names(&text, &self.dot, false);
                }
                out.extend_from_slice(&u16::try_from(text.len()).ok()?.to_be_bytes());
                out.extend_from_slice(&text);
            } else {
                out.extend_from_slice(&data[*start..*end]);
            }
        }
        out.extend_from_slice(&data[pool.end..]);
        Some(out)
    }
}

fn identifier(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn name_char(b: u8) -> bool {
    identifier(b) || matches!(b, b'$' | b'/' | b'.')
}

/// Whether `text` starts with the package or class `name` (not just with the same letters).
fn starts_with_name(text: &[u8], name: &[u8]) -> bool {
    text.starts_with(name) && text.get(name.len()).is_none_or(|&next| !identifier(next))
}

/// Replaces each occurrence of a rule's name that stands alone: not the middle of a longer name, and
/// (`descriptors`) possibly behind the `L` of a type descriptor.
fn replace_names(text: &[u8], rules: &[(Vec<u8>, Vec<u8>)], descriptors: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    'scan: while i < text.len() {
        let before_ok = i == 0 || !name_char(text[i - 1]) || descriptors && text[i - 1] == b'L' && (i == 1 || !name_char(text[i - 2]));
        if before_ok {
            for (from, to) in rules {
                if starts_with_name(&text[i..], from) {
                    out.extend_from_slice(to);
                    i += from.len();
                    continue 'scan;
                }
            }
        }
        out.push(text[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A class file with the given name, superclass `java/lang/Object` and class and string constants.
    fn class_file(this: &str, classes: &[&str], strings: &[&str]) -> Vec<u8> {
        let mut pool: Vec<Vec<u8>> = Vec::new();
        let utf8 = |s: &str| {
            let mut v = vec![1];
            v.extend_from_slice(&(s.len() as u16).to_be_bytes());
            v.extend_from_slice(s.as_bytes());
            v
        };
        let reference = |tag: u8, index: usize| vec![tag, (index >> 8) as u8, index as u8];
        pool.push(utf8(this));
        pool.push(reference(7, 1));
        pool.push(utf8("java/lang/Object"));
        pool.push(reference(7, 3));
        for class in classes {
            pool.push(utf8(class));
            pool.push(reference(7, pool.len()));
        }
        for string in strings {
            pool.push(utf8(string));
            pool.push(reference(8, pool.len()));
        }
        let mut out = vec![0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 65];
        out.extend_from_slice(&((pool.len() + 1) as u16).to_be_bytes());
        for entry in pool {
            out.extend(entry);
        }
        // access flags, this, super, then no interfaces, fields, methods or attributes
        out.extend_from_slice(&[0, 0x21, 0, 2, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0]);
        out
    }

    fn utf8_constants(data: &[u8]) -> Vec<String> {
        let pool = Pool::parse(data).unwrap();
        pool.slots.iter().flatten().filter(|(t, ..)| *t == 1).map(|(_, s, e)| String::from_utf8_lossy(&data[*s..*e]).into_owned()).collect()
    }

    fn make_jar(path: &Path, files: &[(&str, &[u8])]) {
        let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
        for (name, content) in files {
            zip.start_file(*name, options()).unwrap();
            zip.write_all(content).unwrap();
        }
        zip.finish().unwrap();
    }

    fn read_jar(path: &Path) -> BTreeMap<String, Vec<u8>> {
        let mut archive = zip::ZipArchive::new(File::open(path).unwrap()).unwrap();
        (0..archive.len())
            .map(|i| {
                let mut f = archive.by_index(i).unwrap();
                let mut data = Vec::new();
                f.read_to_end(&mut data).unwrap();
                (f.name().unwrap().into_owned(), data)
            })
            .collect()
    }

    fn settings() -> ShadeSettings {
        ShadeSettings {
            relocate: vec![("com.google.common".into(), "com.example.shaded.guava".into())],
            exclude: Vec::new(),
            minimize: false,
            keep: Vec::new(),
            merge_services: true,
            relocate_strings: false,
        }
    }

    #[test]
    fn relocates_names_descriptors_and_signatures_but_not_lookalikes() {
        let relocator = Relocator::new(&settings());
        let class = class_file(
            "com/google/common/base/Strings",
            &[
                "com/google/common/collect/ImmutableList",
                "[Lcom/google/common/base/Joiner;",
                "com/google/commonish/Other",
                "x/com/google/common/Y",
            ],
            &["com.google.common.Foo", "(Lcom/google/common/A;)Ljava/util/List<Lcom/google/common/B;>;"],
        );
        let moved = relocator.class(&class).unwrap();
        let names = utf8_constants(&moved);
        assert!(names.contains(&"com/example/shaded/guava/base/Strings".to_owned()));
        assert!(names.contains(&"com/example/shaded/guava/collect/ImmutableList".to_owned()));
        assert!(names.contains(&"[Lcom/example/shaded/guava/base/Joiner;".to_owned()));
        assert!(names.contains(&"com/google/commonish/Other".to_owned()), "a longer package name is not a match");
        assert!(names.contains(&"x/com/google/common/Y".to_owned()), "only a name that starts there moves");
        assert!(names.contains(&"com.google.common.Foo".to_owned()), "strings stay unless relocate-strings is set");
        assert!(names.contains(&"(Lcom/example/shaded/guava/A;)Ljava/util/List<Lcom/example/shaded/guava/B;>;".to_owned()));
        assert_eq!(&moved[moved.len() - 14..], &class[class.len() - 14..], "the rest of the class is copied");
    }

    #[test]
    fn relocates_string_constants_that_name_classes_when_asked() {
        let relocator = Relocator::new(&ShadeSettings { relocate_strings: true, ..settings() });
        let class = class_file("a/B", &[], &["com.google.common.Foo", "com.google.commonish.Foo"]);
        let names = utf8_constants(&relocator.class(&class).unwrap());
        assert!(names.contains(&"com.example.shaded.guava.Foo".to_owned()));
        assert!(names.contains(&"com.google.commonish.Foo".to_owned()));
    }

    #[test]
    fn moves_entries_services_and_resources() {
        let relocator = Relocator::new(&settings());
        assert_eq!(relocator.path("com/google/common/base/Strings.class"), "com/example/shaded/guava/base/Strings.class");
        assert_eq!(relocator.path("META-INF/versions/9/com/google/common/A.class"), "META-INF/versions/9/com/example/shaded/guava/A.class");
        assert_eq!(relocator.path("com/google/common/x.properties"), "com/example/shaded/guava/x.properties");
        assert_eq!(relocator.path("org/other/com/google/common/x.properties"), "org/other/com/google/common/x.properties");
        let (name, data) =
            relocator.entry("META-INF/services/com.google.common.Spi".into(), b"com.google.common.Impl # c\norg.x.Y\n".to_vec());
        assert_eq!(name, "META-INF/services/com.example.shaded.guava.Spi");
        assert_eq!(String::from_utf8(data).unwrap(), "com.example.shaded.guava.Impl # c\norg.x.Y\n");
    }

    #[test]
    fn writes_a_shaded_jar_with_merged_files_and_a_duplicate_report() {
        let dir = tempfile::tempdir().unwrap();
        let classes = dir.path().join("classes");
        std::fs::create_dir_all(classes.join("app")).unwrap();
        std::fs::write(classes.join("app/Main.class"), class_file("app/Main", &["com/google/common/base/Strings"], &[])).unwrap();
        let (one, two) = (dir.path().join("one.jar"), dir.path().join("two.jar"));
        make_jar(
            &one,
            &[
                ("com/google/common/base/Strings.class", &class_file("com/google/common/base/Strings", &[], &[])),
                ("META-INF/services/a.Spi", b"impl.One"),
                ("META-INF/SIGNER.SF", b"sig"),
                ("META-INF/versions/9/x/V.class", b"v"),
                ("data.txt", b"first"),
                ("reference.conf", b"a = 1"),
            ],
        );
        make_jar(&two, &[("META-INF/services/a.Spi", b"impl.Two\n"), ("data.txt", b"second"), ("reference.conf", b"b = 2\n")]);
        let dest = dir.path().join("out.jar");
        let report = write_shaded_jar(&dest, &classes, &JarManifest::default(), &[one.clone(), two], &settings()).unwrap();
        let jar = read_jar(&dest);
        assert!(jar.contains_key("com/example/shaded/guava/base/Strings.class"));
        assert!(!jar.contains_key("com/google/common/base/Strings.class"));
        let main = utf8_constants(&jar["app/Main.class"]);
        assert!(main.contains(&"com/example/shaded/guava/base/Strings".to_owned()));
        assert_eq!(jar["META-INF/services/a.Spi"], b"impl.One\nimpl.Two\n");
        assert_eq!(jar["reference.conf"], b"a = 1\nb = 2\n");
        assert_eq!(jar["data.txt"], b"first");
        assert_eq!(report.duplicates, ["data.txt"]);
        assert!(!jar.contains_key("META-INF/SIGNER.SF"));
        assert!(String::from_utf8_lossy(&jar["META-INF/MANIFEST.MF"]).contains("Multi-Release: true"));
        assert_eq!(report.bundled, [one]);
    }

    #[test]
    fn without_merge_services_the_first_file_wins() {
        let dir = tempfile::tempdir().unwrap();
        let (one, two) = (dir.path().join("one.jar"), dir.path().join("two.jar"));
        make_jar(&one, &[("META-INF/services/a.Spi", b"impl.One\n")]);
        make_jar(&two, &[("META-INF/services/a.Spi", b"impl.Two\n")]);
        let dest = dir.path().join("out.jar");
        let shade = ShadeSettings { merge_services: false, ..settings() };
        let report = write_shaded_jar(&dest, &dir.path().join("none"), &JarManifest::default(), &[one, two], &shade).unwrap();
        assert_eq!(read_jar(&dest)["META-INF/services/a.Spi"], b"impl.One\n");
        assert_eq!(report.duplicates, ["META-INF/services/a.Spi"]);
    }

    #[test]
    fn exclude_leaves_classes_out_of_the_jar() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib.jar");
        make_jar(&lib, &[("org/slf4j/Logger.class", b"x"), ("org/slf4j/impl/L.class", b"x"), ("org/slf4jx/K.class", b"x")]);
        let dest = dir.path().join("out.jar");
        let shade = ShadeSettings { exclude: vec!["org.slf4j.*".into()], ..settings() };
        write_shaded_jar(&dest, &dir.path().join("none"), &JarManifest::default(), &[lib], &shade).unwrap();
        let names: Vec<String> = read_jar(&dest).into_keys().collect();
        assert_eq!(names, ["META-INF/MANIFEST.MF", "org/slf4jx/K.class"]);
    }

    #[test]
    fn minimize_keeps_what_the_project_reaches() {
        let dir = tempfile::tempdir().unwrap();
        let classes = dir.path().join("classes");
        std::fs::create_dir_all(classes.join("app")).unwrap();
        std::fs::write(classes.join("app/Main.class"), class_file("app/Main", &["lib/Used"], &["lib.Loaded"])).unwrap();
        let lib = dir.path().join("lib.jar");
        make_jar(
            &lib,
            &[
                ("lib/Used.class", &class_file("lib/Used", &["lib/Helper"], &[])),
                ("lib/Helper.class", &class_file("lib/Helper", &[], &[])),
                ("lib/Loaded.class", &class_file("lib/Loaded", &[], &[])),
                ("lib/Kept.class", &class_file("lib/Kept", &[], &[])),
                ("lib/Provider.class", &class_file("lib/Provider", &[], &[])),
                ("lib/Unused.class", &class_file("lib/Unused", &["lib/Orphan"], &[])),
                ("lib/Orphan.class", &class_file("lib/Orphan", &[], &[])),
                ("lib/Api.class", &class_file("lib/Api", &[], &[])),
                ("lib/Unreached.class", &class_file("lib/Unreached", &[], &[])),
                ("META-INF/services/lib.Api", b"lib.Provider\n"),
                ("META-INF/services/lib.Unreached", b"lib.Orphan\n"),
            ],
        );
        let dest = dir.path().join("out.jar");
        let shade = ShadeSettings { minimize: true, keep: vec!["lib.Kept".into()], ..settings() };
        let report = write_shaded_jar(&dest, &classes, &JarManifest::default(), &[lib], &shade).unwrap();
        let names: Vec<String> = read_jar(&dest).into_keys().filter(|n| n.ends_with(".class")).collect();
        assert_eq!(names, ["app/Main.class", "lib/Helper.class", "lib/Kept.class", "lib/Loaded.class", "lib/Used.class"]);
        assert_eq!(report.removed, 5);
    }
}
