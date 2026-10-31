//! Compose Multiplatform resources: the `composeResources/` directory of a package and the `Res`
//! class that the Compose Gradle plugin would generate for it.
//!
//! ```text
//! composeResources/
//!   drawable/logo.png          Res.drawable.logo        (painterResource)
//!   drawable-dark/logo.png     the same resource, for the dark theme
//!   font/inter.ttf             Res.font.inter           (Font)
//!   values/strings.xml         Res.string.*, Res.array.*, Res.plurals.*
//!   values-de/strings.xml      the German strings
//!   files/data.json            Res.readBytes("files/data.json")
//! ```
//!
//! The files land on the classpath under `composeResources/<package>/`, where the runtime
//! (`org.jetbrains.compose.components:components-resources`) reads them. String values become
//! `.cvr` files: one `type|key|base64` line per string, which the generated accessors point into
//! by byte offset and size, exactly like the Gradle plugin's output.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine as _;

use crate::env::build_config_package;
use kargo_manifest::Manifest;

/// The directory of a package that holds its Compose resources.
pub const DIR: &str = "composeResources";

/// The runtime library the generated `Res` class uses.
pub const RUNTIME: &str = "org.jetbrains.compose.components:components-resources";

const API: &str = "org.jetbrains.compose.resources";

/// Whether the package at `root` has Compose resources.
pub fn present(root: &Path) -> bool {
    root.join(DIR).is_dir()
}

/// The package of the generated `Res` class: `[frameworks.compose] resources-package`, or
/// `<group>.<name>.generated.resources`, as the Compose Gradle plugin names it.
pub fn package(manifest: &Manifest) -> String {
    if let Some(package) = manifest.frameworks.compose.as_ref().and_then(|c| c.resources_package.clone()) {
        return package;
    }
    let pkg = &manifest.package;
    format!("{}.generated.resources", build_config_package(pkg.group.as_deref(), &pkg.name))
}

/// What [`generate`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generated {
    /// `Res.kt`, to compile with the main sources.
    pub source: PathBuf,
    /// The directory to put on the classpath: it holds `composeResources/<package>/`.
    pub resources: PathBuf,
}

/// Where [`generate`] writes for a target: `target/<profile>/<target>/generated/compose-resources`.
pub fn output_dir(target_output: &Path) -> PathBuf {
    target_output.join("generated").join("compose-resources")
}

/// Reads `root/composeResources` and writes `Res.kt` and the processed resources into `out`. Files
/// are rewritten only when their contents change, so unchanged builds stay incremental. Without
/// `composeResources/` (or without Compose), `out` is removed and the result is `None`.
pub fn generate(root: &Path, manifest: &Manifest, out: &Path) -> Result<Option<Generated>> {
    let Some(compose) = &manifest.frameworks.compose else {
        kargo_util::fs::remove_dir_all_if_exists(out)?;
        return Ok(None);
    };
    let dir = root.join(DIR);
    if !dir.is_dir() {
        kargo_util::fs::remove_dir_all_if_exists(out)?;
        return Ok(None);
    }
    let package = package(manifest);
    let scan = scan(&dir).with_context(|| format!("invalid Compose resources in {}", dir.display()))?;
    let prefix = format!("{DIR}/{package}/");
    let visibility = if compose.public_resources.unwrap_or(false) { "public" } else { "internal" };
    let source = out.join("kotlin").join("Res.kt");
    write_if_changed(&source, kotlin_source(&package, &prefix, visibility, &scan.resources).as_bytes())?;
    let resources = out.join("resources");
    let files: BTreeMap<String, Content> = scan.files.into_iter().map(|(rel, content)| (format!("{prefix}{rel}"), content)).collect();
    sync_tree(&resources, &files)?;
    Ok(Some(Generated { source, resources }))
}

/// The kinds of resources, in the order of the generated code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Drawable,
    String,
    StringArray,
    Plurals,
    Font,
}

impl Kind {
    /// The object under `Res` that holds the accessors.
    fn object(self) -> &'static str {
        match self {
            Kind::Drawable => "drawable",
            Kind::String => "string",
            Kind::StringArray => "array",
            Kind::Plurals => "plurals",
            Kind::Font => "font",
        }
    }

    fn class(self) -> &'static str {
        match self {
            Kind::Drawable => "DrawableResource",
            Kind::String => "StringResource",
            Kind::StringArray => "StringArrayResource",
            Kind::Plurals => "PluralStringResource",
            Kind::Font => "FontResource",
        }
    }

    /// The prefix of resource ids and the record type in `.cvr` files.
    fn id_prefix(self) -> &'static str {
        match self {
            Kind::Drawable => "drawable",
            Kind::String => "string",
            Kind::StringArray => "string-array",
            Kind::Plurals => "plurals",
            Kind::Font => "font",
        }
    }

    /// Whether the resource is a value with a key (it is a `.cvr` record, not a whole file).
    fn keyed(self) -> bool {
        matches!(self, Kind::String | Kind::StringArray | Kind::Plurals)
    }

    /// The name of the `Res.all*Resources` map.
    fn all(self) -> &'static str {
        match self {
            Kind::Drawable => "allDrawableResources",
            Kind::String => "allStringResources",
            Kind::StringArray => "allStringArrayResources",
            Kind::Plurals => "allPluralStringResources",
            Kind::Font => "allFontResources",
        }
    }
}

/// A resource qualifier from a directory name such as `values-de-rAT` or `drawable-dark-xxhdpi`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Qualifier {
    Language(String),
    Region(String),
    Theme(&'static str),
    Density(&'static str),
}

impl Qualifier {
    fn kotlin(&self) -> String {
        match self {
            Qualifier::Language(l) => format!("{API}.LanguageQualifier({})", kotlin_string(l)),
            Qualifier::Region(r) => format!("{API}.RegionQualifier({})", kotlin_string(r)),
            Qualifier::Theme(t) => format!("{API}.ThemeQualifier.{t}"),
            Qualifier::Density(d) => format!("{API}.DensityQualifier.{d}"),
        }
    }
}

const DENSITIES: [(&str, &str); 6] =
    [("ldpi", "LDPI"), ("mdpi", "MDPI"), ("hdpi", "HDPI"), ("xhdpi", "XHDPI"), ("xxhdpi", "XXHDPI"), ("xxxhdpi", "XXXHDPI")];

/// Splits a directory name into its type and qualifiers.
fn parse_dir_name(name: &str) -> Result<(&str, Vec<Qualifier>)> {
    let mut parts = name.split('-');
    let kind = parts.next().unwrap_or_default();
    let mut qualifiers = Vec::new();
    let mut language = false;
    for part in parts {
        let qualifier = if let Some((_, constant)) = DENSITIES.iter().find(|(d, _)| *d == part) {
            Qualifier::Density(constant)
        } else if part == "light" || part == "dark" {
            Qualifier::Theme(if part == "dark" { "DARK" } else { "LIGHT" })
        } else if part.len() == 3 && part.starts_with('r') && part[1..].chars().all(|c| c.is_ascii_uppercase()) {
            if !language {
                bail!("`{name}`: the region `{part}` must follow a language, as in `{kind}-en-{part}`");
            }
            Qualifier::Region(part[1..].to_owned())
        } else if (2..=3).contains(&part.len()) && part.chars().all(|c| c.is_ascii_lowercase()) {
            language = true;
            Qualifier::Language(part.to_owned())
        } else {
            bail!(
                "`{name}`: unknown qualifier `{part}` (expected a language such as `de`, a region such as `rAT`, `light`, `dark` or a density such as `xxhdpi`)"
            );
        };
        if qualifiers.iter().any(|q| std::mem::discriminant(q) == std::mem::discriminant(&qualifier)) {
            bail!("`{name}` has more than one qualifier of the same kind");
        }
        qualifiers.push(qualifier);
    }
    qualifiers.sort();
    Ok((kind, qualifiers))
}

/// One variant of a resource: the file it is in, and where in that file for `.cvr` records.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Item {
    qualifiers: Vec<Qualifier>,
    /// Relative to `composeResources/<package>/`.
    path: String,
    /// `None` for a whole file.
    range: Option<(usize, usize)>,
    /// Where it came from, for error messages.
    origin: String,
}

/// The contents of an output file.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Content {
    Copy(PathBuf),
    Bytes(Vec<u8>),
}

#[derive(Debug, Default)]
struct Scan {
    /// By kind, then by accessor name.
    resources: BTreeMap<Kind, BTreeMap<String, Vec<Item>>>,
    /// The output files, relative to `composeResources/<package>/`.
    files: BTreeMap<String, Content>,
}

impl Scan {
    fn add(&mut self, kind: Kind, name: String, item: Item) -> Result<()> {
        let items = self.resources.entry(kind).or_default().entry(name.clone()).or_default();
        if let Some(other) = items.iter().find(|i| i.qualifiers == item.qualifiers) {
            bail!("the {} resource `{name}` is defined twice: in {} and in {}", kind.object(), other.origin, item.origin);
        }
        items.push(item);
        Ok(())
    }
}

/// Files and directories that are not resources: `.DS_Store`, `.gitkeep` and the like.
fn hidden(name: &str) -> bool {
    name.starts_with('.')
}

/// The sorted entries of `dir`, without hidden ones.
fn entries(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| !hidden(&file_name(p)))
        .collect();
    entries.sort();
    Ok(entries)
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn scan(dir: &Path) -> Result<Scan> {
    let mut scan = Scan::default();
    for sub in entries(dir)? {
        let dir_name = file_name(&sub);
        if !sub.is_dir() {
            bail!(
                "`{dir_name}` is not in a resource directory; put it in `drawable/`, `font/`, `values/` or `files/` (read with `Res.readBytes(\"files/{dir_name}\")`)"
            );
        }
        let (kind, qualifiers) = parse_dir_name(&dir_name)?;
        match kind {
            "files" => {
                if !qualifiers.is_empty() {
                    bail!("`{dir_name}`: `files/` takes no qualifiers");
                }
                for entry in walkdir::WalkDir::new(&sub).sort_by_file_name() {
                    let entry = entry?;
                    let rel = entry.path().strip_prefix(dir)?;
                    if entry.file_type().is_file() && !rel.components().any(|c| hidden(&c.as_os_str().to_string_lossy())) {
                        scan.files.insert(slash(rel), Content::Copy(entry.into_path()));
                    }
                }
            }
            "drawable" | "font" => {
                let kind = if kind == "drawable" { Kind::Drawable } else { Kind::Font };
                for file in entries(&sub)? {
                    let name = file_name(&file);
                    if file.is_dir() {
                        bail!("`{dir_name}/{name}` is a directory; {} resources are the files directly in `{dir_name}/`", kind.object());
                    }
                    let stem = name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(&name);
                    let rel = format!("{dir_name}/{name}");
                    let item = Item { qualifiers: qualifiers.clone(), path: rel.clone(), range: None, origin: rel.clone() };
                    scan.add(kind, accessor_name(stem, &rel)?, item)?;
                    scan.files.insert(rel, Content::Copy(file));
                }
            }
            "values" => {
                for file in entries(&sub)? {
                    let name = file_name(&file);
                    if file.is_dir() || !name.ends_with(".xml") {
                        bail!("`{dir_name}/{name}` is not an XML file; `{dir_name}/` holds `<resources>` files such as strings.xml");
                    }
                    let origin = format!("{dir_name}/{name}");
                    let text = std::fs::read_to_string(&file).with_context(|| format!("failed to read {}", file.display()))?;
                    let values = parse_values(&text).with_context(|| format!("invalid {origin}"))?;
                    let cvr_path = format!("{dir_name}/{}.cvr", name.trim_end_matches(".xml"));
                    let mut cvr = String::new();
                    for value in values {
                        let record = value.record();
                        let range = (cvr.len(), record.len());
                        cvr.push_str(&record);
                        cvr.push('\n');
                        let item =
                            Item { qualifiers: qualifiers.clone(), path: cvr_path.clone(), range: Some(range), origin: origin.clone() };
                        scan.add(value.kind, accessor_name(&value.key, &origin)?, item)?;
                    }
                    scan.files.insert(cvr_path, Content::Bytes(cvr.into_bytes()));
                }
            }
            other => bail!(
                "unknown Compose resources directory `{dir_name}`: `{other}` is not a resource type (expected `drawable`, `font`, `values` or `files`)"
            ),
        }
    }
    Ok(scan)
}

const KEYWORDS: &[&str] = &[
    "as",
    "break",
    "class",
    "continue",
    "do",
    "else",
    "false",
    "for",
    "fun",
    "if",
    "in",
    "interface",
    "is",
    "null",
    "object",
    "package",
    "return",
    "super",
    "this",
    "throw",
    "true",
    "try",
    "typealias",
    "typeof",
    "val",
    "var",
    "when",
    "while",
];

/// The accessor of a resource: its name with `-`, `.` and spaces as `_`, and a leading `_`
/// before a digit, like the Compose Gradle plugin's.
fn accessor_name(name: &str, origin: &str) -> Result<String> {
    let mut accessor: String = name.chars().map(|c| if matches!(c, '-' | '.' | ' ') { '_' } else { c }).collect();
    if accessor.starts_with(|c: char| c.is_ascii_digit()) {
        accessor.insert(0, '_');
    }
    if accessor.is_empty() || !accessor.chars().all(|c| c.is_alphanumeric() || c == '_') {
        bail!("{origin}: `{name}` is not a valid resource name; use letters, digits, `_` and `-`");
    }
    Ok(accessor)
}

/// A `<string>`, `<string-array>` or `<plurals>` of a values file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Value {
    kind: Kind,
    key: String,
    /// The string, the items of an array, or `quantity` and string pairs of plurals.
    items: Vec<(Option<String>, String)>,
}

impl Value {
    /// The `.cvr` line of the value, which the runtime decodes (`StringResourcesUtils.kt`).
    fn record(&self) -> String {
        let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s.as_bytes());
        let data = match self.kind {
            Kind::String => b64(&self.items[0].1),
            Kind::StringArray => self.items.iter().map(|(_, s)| b64(s)).collect::<Vec<_>>().join(","),
            _ => self.items.iter().map(|(q, s)| format!("{}:{}", q.as_deref().unwrap_or_default(), b64(s))).collect::<Vec<_>>().join(","),
        };
        format!("{}|{}|{data}", self.kind.id_prefix(), self.key)
    }
}

const QUANTITIES: [&str; 6] = ["zero", "one", "two", "few", "many", "other"];

fn parse_values(text: &str) -> Result<Vec<Value>> {
    let doc = roxmltree::Document::parse(text).context("not well-formed XML")?;
    let root = doc.root_element();
    if root.tag_name().name() != "resources" {
        bail!("the root element must be <resources>, not <{}>", root.tag_name().name());
    }
    let mut values = Vec::new();
    for node in root.children().filter(|n| n.is_element()) {
        let tag = node.tag_name().name();
        let Some(key) = node.attribute("name") else {
            bail!("<{tag}> on line {} has no `name`", doc.text_pos_at(node.range().start).row);
        };
        let key = key.to_owned();
        let line = doc.text_pos_at(node.range().start).row;
        let items = |child: &'static str| node.children().filter(move |n| n.is_element() && n.tag_name().name() == child);
        let value = match tag {
            "string" => Value { kind: Kind::String, key, items: vec![(None, string_value(node))] },
            "string-array" => Value { kind: Kind::StringArray, key, items: items("item").map(|n| (None, string_value(n))).collect() },
            "plurals" => {
                let mut pairs = Vec::new();
                for item in items("item") {
                    let quantity = item.attribute("quantity").unwrap_or_default();
                    if !QUANTITIES.contains(&quantity) {
                        bail!(
                            "<plurals name=\"{key}\"> on line {line}: `quantity` must be one of {}, not `{quantity}`",
                            QUANTITIES.join(", ")
                        );
                    }
                    pairs.push((Some(quantity.to_owned()), string_value(item)));
                }
                if pairs.is_empty() {
                    bail!("<plurals name=\"{key}\"> on line {line} has no <item quantity=\"...\">");
                }
                Value { kind: Kind::Plurals, key, items: pairs }
            }
            other => bail!("<{other}> on line {line} is not a Compose resource (expected <string>, <string-array> or <plurals>)"),
        };
        values.push(value);
    }
    Ok(values)
}

/// The text of an element, with Android's escapes (`\n`, `\t`, `\'`, `\"`, `\@`, `\uXXXX`) applied.
fn string_value(node: roxmltree::Node) -> String {
    let text: String = node.descendants().filter(|n| n.is_text()).filter_map(|n| n.text()).collect();
    unescape(&text)
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('u') => {
                let hex: String = chars.clone().take(4).collect();
                match u32::from_str_radix(&hex, 16).ok().filter(|_| hex.len() == 4).and_then(char::from_u32) {
                    Some(ch) => {
                        out.push(ch);
                        for _ in 0..4 {
                            chars.next();
                        }
                    }
                    None => out.push('u'),
                }
            }
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// A Kotlin string literal.
fn kotlin_string(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '$' => out.push_str("\\$"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A Kotlin identifier, in backticks when it is a keyword.
fn kotlin_name(name: &str) -> String {
    if KEYWORDS.contains(&name) { format!("`{name}`") } else { name.to_owned() }
}

fn kotlin_source(package: &str, prefix: &str, visibility: &str, resources: &BTreeMap<Kind, BTreeMap<String, Vec<Item>>>) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "// Generated by Kargo from {DIR}/. The next build overwrites it.");
    let _ = writeln!(s, "@file:OptIn({API}.InternalResourceApi::class)\n");
    let package_line: Vec<String> = package.split('.').map(kotlin_name).collect();
    let _ = writeln!(s, "package {}\n", package_line.join("."));
    let _ = writeln!(s, "private const val PREFIX: String = {}\n", kotlin_string(prefix));
    let _ = writeln!(s, "{visibility} object Res {{");
    let _ = writeln!(s, "    /** Reads a file of {DIR}/, such as `Res.readBytes(\"files/data.json\")`. */");
    let _ = writeln!(s, "    suspend fun readBytes(path: String): ByteArray = {API}.readResourceBytes(PREFIX + path)\n");
    let _ = writeln!(s, "    /** The URI of a file of {DIR}/, such as `Res.getUri(\"files/index.html\")`. */");
    let _ = writeln!(s, "    fun getUri(path: String): String = {API}.getResourceUri(PREFIX + path)");
    for kind in [Kind::Drawable, Kind::String, Kind::StringArray, Kind::Plurals, Kind::Font] {
        let _ = writeln!(s, "\n    object {}", kind.object());
    }
    let _ = writeln!(s, "}}");
    for kind in [Kind::Drawable, Kind::String, Kind::StringArray, Kind::Plurals, Kind::Font] {
        let class = format!("{API}.{}", kind.class());
        let named = resources.get(&kind).cloned().unwrap_or_default();
        for (name, items) in &named {
            let id = kotlin_string(&format!("{}:{name}", kind.id_prefix()));
            let key = if kind.keyed() { format!(", {}", kotlin_string(name)) } else { String::new() };
            let _ = writeln!(s, "\n{visibility} val Res.{}.{}: {class} by lazy {{", kind.object(), kotlin_name(name));
            let _ = writeln!(s, "    {class}({id}{key}, setOf(");
            for item in items {
                let qualifiers: Vec<String> = item.qualifiers.iter().map(Qualifier::kotlin).collect();
                let (offset, size) = item.range.map(|(o, n)| (o as i64, n as i64)).unwrap_or((-1, -1));
                let _ = writeln!(
                    s,
                    "        {API}.ResourceItem(setOf({}), {}, {offset}, {size}),",
                    qualifiers.join(", "),
                    kotlin_string(&format!("{prefix}{}", item.path))
                );
            }
            let _ = writeln!(s, "    ))\n}}");
        }
        let entries: Vec<String> =
            named.keys().map(|name| format!("        {} to Res.{}.{},", kotlin_string(name), kind.object(), kotlin_name(name))).collect();
        let _ = writeln!(s, "\n{visibility} val Res.{}: Map<String, {class}> by lazy {{", kind.all());
        if entries.is_empty() {
            let _ = writeln!(s, "    emptyMap()");
        } else {
            let _ = writeln!(s, "    mapOf(\n{}\n    )", entries.join("\n"));
        }
        let _ = writeln!(s, "}}");
    }
    s
}

fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Writes `contents` to `path` unless it already holds them.
fn write_if_changed(path: &Path, contents: &[u8]) -> Result<()> {
    if std::fs::read(path).ok().as_deref() == Some(contents) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    }
    std::fs::write(path, contents).with_context(|| format!("failed to write {}", path.display()))
}

/// Makes `dest` hold exactly `files` (relative paths with `/`): writes what changed and removes the rest.
fn sync_tree(dest: &Path, files: &BTreeMap<String, Content>) -> Result<()> {
    for (rel, content) in files {
        let path = dest.join(rel);
        match content {
            Content::Bytes(bytes) => write_if_changed(&path, bytes)?,
            Content::Copy(from) => {
                let same = match (std::fs::metadata(from), std::fs::metadata(&path)) {
                    (Ok(a), Ok(b)) => a.len() == b.len() && std::fs::read(from).ok() == std::fs::read(&path).ok(),
                    _ => false,
                };
                if !same {
                    if let Some(dir) = path.parent() {
                        std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
                    }
                    std::fs::copy(from, &path).with_context(|| format!("failed to copy {}", from.display()))?;
                }
            }
        }
    }
    if !dest.is_dir() {
        return Ok(());
    }
    let mut stale_dirs = Vec::new();
    for entry in walkdir::WalkDir::new(dest).min_depth(1).contents_first(true) {
        let entry = entry?;
        let rel = slash(entry.path().strip_prefix(dest)?);
        if entry.file_type().is_dir() {
            stale_dirs.push(entry.into_path());
        } else if !files.contains_key(&rel) {
            std::fs::remove_file(entry.path()).with_context(|| format!("failed to remove {}", entry.path().display()))?;
        }
    }
    for dir in stale_dirs {
        // Fails, as it should, for a directory that still has files.
        let _ = std::fs::remove_dir(dir);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(extra: &str) -> Manifest {
        Manifest::parse(&format!("[package]\nname = \"Notes-App\"\nversion = \"1.0.0\"\ngroup = \"com.Example\"\n{extra}")).unwrap()
    }

    fn write(root: &Path, rel: &str, contents: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn default_and_configured_packages() {
        assert_eq!(package(&manifest("")), "com.example.notesapp.generated.resources");
        assert_eq!(package(&manifest("[frameworks]\ncompose = { resources-package = \"com.example.res\" }")), "com.example.res");
        let no_group = Manifest::parse("[package]\nname = \"app\"\nversion = \"1\"\n").unwrap();
        assert_eq!(package(&no_group), "app.generated.resources");
    }

    #[test]
    fn directory_qualifiers() {
        assert_eq!(parse_dir_name("drawable").unwrap(), ("drawable", vec![]));
        let (kind, qualifiers) = parse_dir_name("values-de-rAT-dark").unwrap();
        assert_eq!(kind, "values");
        assert_eq!(qualifiers, [Qualifier::Language("de".into()), Qualifier::Region("AT".into()), Qualifier::Theme("DARK")]);
        assert_eq!(parse_dir_name("drawable-xxhdpi").unwrap().1, [Qualifier::Density("XXHDPI")]);
        assert!(parse_dir_name("values-rAT").unwrap_err().to_string().contains("must follow a language"));
        assert!(parse_dir_name("drawable-land").unwrap_err().to_string().contains("unknown qualifier `land`"));
        assert!(parse_dir_name("drawable-dark-light").is_err());
    }

    #[test]
    fn values_and_records() {
        let values = parse_values(
            r#"<resources>
    <string name="app_name">Notes</string>
    <string name="greeting">Hello, %1$s!\nIt\'s <b>nice</b> &amp; é</string>
    <string-array name="colors"><item>Red</item><item>Green</item></string-array>
    <plurals name="notes"><item quantity="one">%1$d note</item><item quantity="other">%1$d notes</item></plurals>
</resources>"#,
        )
        .unwrap();
        assert_eq!(values.len(), 4);
        assert_eq!(values[1].items[0].1, "Hello, %1$s!\nIt's nice & \u{e9}");
        assert_eq!(values[0].record(), "string|app_name|Tm90ZXM=");
        assert_eq!(values[2].record(), "string-array|colors|UmVk,R3JlZW4=");
        assert_eq!(values[3].record(), "plurals|notes|one:JTEkZCBub3Rl,other:JTEkZCBub3Rlcw==");
        assert!(parse_values("<resources><color name=\"x\">#fff</color></resources>").unwrap_err().to_string().contains("<color>"));
        assert!(parse_values("<resources><string>x</string></resources>").unwrap_err().to_string().contains("no `name`"));
        let bad = "<resources><plurals name=\"p\"><item quantity=\"lots\">x</item></plurals></resources>";
        assert!(parse_values(bad).unwrap_err().to_string().contains("quantity"));
        assert!(parse_values("<strings/>").is_err());
        assert_eq!(unescape(r#"a\\b\"c\@d\u12"#), "a\\b\"c@du12");
    }

    #[test]
    fn generates_res_and_resources() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "composeResources/drawable/compose-logo.png", "png");
        write(root, "composeResources/drawable-dark/compose-logo.png", "dark png");
        write(root, "composeResources/drawable/.DS_Store", "");
        write(root, "composeResources/font/Inter.ttf", "ttf");
        write(
            root,
            "composeResources/values/strings.xml",
            "<resources><string name=\"app_name\">Notes</string><string name=\"in\">In</string></resources>",
        );
        write(root, "composeResources/values-de/strings.xml", "<resources><string name=\"app_name\">Notizen</string></resources>");
        write(root, "composeResources/files/data/info.json", "{}");
        let m = manifest("[frameworks]\ncompose = true\n");
        let out = root.join("target/gen");
        let generated = generate(root, &m, &out).unwrap().unwrap();

        let res = std::fs::read_to_string(&generated.source).unwrap();
        let prefix = "composeResources/com.example.notesapp.generated.resources/";
        assert!(res.contains("package com.example.notesapp.generated.resources\n"), "{res}");
        assert!(res.contains("internal object Res {"));
        assert!(res.contains("internal val Res.drawable.compose_logo: org.jetbrains.compose.resources.DrawableResource by lazy {"));
        assert!(res.contains(&format!(
            "org.jetbrains.compose.resources.ResourceItem(setOf(org.jetbrains.compose.resources.ThemeQualifier.DARK), \"{prefix}drawable-dark/compose-logo.png\", -1, -1),"
        )));
        assert!(res.contains("internal val Res.font.Inter: org.jetbrains.compose.resources.FontResource"));
        assert!(res.contains("internal val Res.string.`in`:"), "keywords are quoted");
        assert!(res.contains("StringResource(\"string:app_name\", \"app_name\", setOf("));
        assert!(res.contains(&format!("\"{prefix}values/strings.cvr\", 0, 24),")), "{res}");
        assert!(res.contains(&format!("\"{prefix}values/strings.cvr\", 25, 14),")), "{res}");
        assert!(res.contains("LanguageQualifier(\"de\")), \"composeResources/"));
        assert!(res.contains("\"app_name\" to Res.string.app_name,"));
        assert!(res.contains("internal val Res.allPluralStringResources: Map<String, org.jetbrains.compose.resources.PluralStringResource> by lazy {\n    emptyMap()"));

        let files = generated.resources.join(prefix);
        assert_eq!(std::fs::read_to_string(files.join("values/strings.cvr")).unwrap(), "string|app_name|Tm90ZXM=\nstring|in|SW4=\n");
        assert!(files.join("drawable-dark/compose-logo.png").is_file() && files.join("files/data/info.json").is_file());
        assert!(!files.join("drawable/.DS_Store").exists());

        // A removed resource disappears from the output; unchanged files are not rewritten.
        std::fs::remove_file(root.join("composeResources/font/Inter.ttf")).unwrap();
        let before = std::fs::metadata(files.join("values/strings.cvr")).unwrap().modified().unwrap();
        generate(root, &m, &out).unwrap().unwrap();
        assert!(!files.join("font/Inter.ttf").exists() && !files.join("font").exists());
        assert_eq!(std::fs::metadata(files.join("values/strings.cvr")).unwrap().modified().unwrap(), before);

        let public = manifest("[frameworks]\ncompose = { public-resources = true }\n");
        let res = std::fs::read_to_string(generate(root, &public, &out).unwrap().unwrap().source).unwrap();
        assert!(res.contains("public object Res {") && res.contains("public val Res.drawable.compose_logo"));

        std::fs::remove_dir_all(root.join("composeResources")).unwrap();
        assert!(generate(root, &m, &out).unwrap().is_none());
        assert!(!out.exists());
    }

    #[test]
    fn rejects_bad_layouts() {
        let err = |files: &[(&str, &str)]| {
            let tmp = tempfile::tempdir().unwrap();
            for (rel, contents) in files {
                write(tmp.path(), rel, contents);
            }
            let m = manifest("[frameworks]\ncompose = true\n");
            format!("{:#}", generate(tmp.path(), &m, &tmp.path().join("out")).unwrap_err())
        };
        assert!(err(&[("composeResources/images/a.png", "")]).contains("`images` is not a resource type"));
        assert!(err(&[("composeResources/logo.png", "")]).contains("not in a resource directory"));
        assert!(err(&[("composeResources/drawable/a.png", ""), ("composeResources/drawable/a.xml", "")]).contains("defined twice"));
        assert!(err(&[("composeResources/drawable/sub/a.png", "")]).contains("is a directory"));
        assert!(err(&[("composeResources/values/a.txt", "")]).contains("not an XML file"));
        assert!(err(&[("composeResources/files-de/a.txt", "")]).contains("no qualifiers"));
        assert!(err(&[("composeResources/drawable/a+b.png", "")]).contains("not a valid resource name"));
    }
}
