//! `kargo inspect`: the jar of a resolved dependency on disk and its public API, `javap` style.
//!
//! The module is found in the resolution a build uses (so the version is the one `kargo.lock`
//! pins), its jar is listed class by class, and a class is printed by the JDK's `javap` with the
//! whole resolved classpath behind it, so its supertypes resolve.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::Configuration;
use super::context::BuildContext;
use super::why::Query;
use kargo_manifest::TargetKind;
use kargo_maven::{ModuleId, Resolution};

/// Which members `javap` prints, and which classes of a jar are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    #[default]
    Public,
    Protected,
    Package,
    Private,
}

impl Visibility {
    fn javap_flag(self) -> &'static str {
        match self {
            Visibility::Public => "-public",
            Visibility::Protected => "-protected",
            Visibility::Package => "-package",
            Visibility::Private => "-private",
        }
    }

    /// Whether classes that are not public are listed.
    fn lists_internal_classes(self) -> bool {
        matches!(self, Visibility::Package | Visibility::Private)
    }
}

/// A module of a resolution with the jar that holds its classes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    /// The resolution: `jvm`.
    pub target: String,
    pub module: ModuleId,
    pub version: String,
    pub jar: PathBuf,
    /// Every artifact of the resolution, for `javap` to resolve supertypes against.
    pub classpath: Vec<PathBuf>,
}

/// The jar of `query` in `resolution`, or `None` when the resolution does not have the module.
///
/// A module that only redirects to another one (a Kotlin Multiplatform root such as
/// `kotlinx-coroutines-core`, whose classes are in `kotlinx-coroutines-core-jvm`) answers with the
/// jar of the module it redirects to.
pub fn locate(target: &str, resolution: &Resolution, query: &Query) -> Result<Option<Located>> {
    if !query.matches(resolution) {
        return Ok(None);
    }
    let Some(node) = resolution.node(&query.module) else { return Ok(None) };
    let jar = |files: &[PathBuf]| files.iter().find(|f| f.extension().is_some_and(|e| e == "jar")).cloned();
    let found = jar(&node.files).map(|jar| (node.module.clone(), node.version.clone(), jar)).or_else(|| {
        let prefix = format!("{}-", node.module.artifact);
        node.dependencies
            .iter()
            .filter(|(m, _)| m.group == node.module.group && m.artifact.starts_with(&prefix))
            .find_map(|(m, _)| resolution.node(m).and_then(|n| jar(&n.files).map(|j| (n.module.clone(), n.version.clone(), j))))
    });
    let Some((module, version, jar)) = found else {
        bail!("`{}:{}` has no jar to inspect: it is a POM that only brings in other modules (see `kargo tree`)", node.module, node.version);
    };
    Ok(Some(Located { target: target.to_owned(), module, version, jar, classpath: resolution.files() }))
}

/// The resolutions of the JVM target for `configuration`: the only ones with jars to read.
pub fn resolutions(ctx: &BuildContext, configuration: Configuration) -> Result<Vec<(String, Resolution)>> {
    if !ctx.ws.manifest().targets.enabled().contains(&TargetKind::Jvm) {
        bail!("`{}` has no JVM target; `kargo inspect` reads jars", ctx.ws.name());
    }
    super::resolution_for(ctx, TargetKind::Jvm, configuration)
}

/// The classes of `jar` as internal names (`com/acme/Outer$Inner`): the public ones, or with
/// `visibility` of `package` or `private` every one. Anonymous and local classes, `module-info`
/// and `package-info` are left out, as are the copies for newer JDKs in `META-INF/versions`.
pub fn classes(jar: &Path, visibility: Visibility) -> Result<Vec<String>> {
    let file = std::fs::File::open(jar).with_context(|| format!("failed to open {}", jar.display()))?;
    let mut archive = zip::ZipArchive::new(file).with_context(|| format!("{} is not a valid jar", jar.display()))?;
    let mut out = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).with_context(|| format!("failed to read {}", jar.display()))?;
        let entry_name = entry.name().with_context(|| format!("failed to read an entry name of {}", jar.display()))?.into_owned();
        let Some(name) = entry_name.strip_suffix(".class").map(str::to_owned) else { continue };
        if name.starts_with("META-INF/") || !is_named_class(&name) {
            continue;
        }
        if !visibility.lists_internal_classes() {
            let mut bytes = Vec::new();
            // The access flags are in the first few hundred bytes after the constant pool; a class
            // whose header can't be read is listed rather than hidden.
            entry.read_to_end(&mut bytes).with_context(|| format!("failed to read {name} in {}", jar.display()))?;
            if access_flags(&bytes).is_some_and(|flags| flags & ACC_PUBLIC == 0) {
                continue;
            }
        }
        out.push(name);
    }
    out.sort();
    Ok(out)
}

const ACC_PUBLIC: u16 = 0x0001;

/// Whether the last segment of an internal name is a class the user could write: not
/// `module-info`, `package-info` or an anonymous or local class (`Outer$1`, `Outer$1Local`).
fn is_named_class(internal: &str) -> bool {
    let simple = internal.rsplit('/').next().unwrap_or(internal);
    if simple == "module-info" || simple == "package-info" {
        return false;
    }
    !simple.split('$').skip(1).any(|part| part.starts_with(|c: char| c.is_ascii_digit()))
}

/// The `access_flags` of a class file, read past its constant pool.
fn access_flags(class: &[u8]) -> Option<u16> {
    let u16_at = |at: usize| Some(u16::from_be_bytes([*class.get(at)?, *class.get(at + 1)?]));
    if class.get(..4)? != [0xCA, 0xFE, 0xBA, 0xBE] {
        return None;
    }
    let count = usize::from(u16_at(8)?);
    let mut at = 10;
    let mut index = 1;
    while index < count {
        let tag = *class.get(at)?;
        let (size, slots) = match tag {
            1 => (3 + usize::from(u16_at(at + 1)?), 1),
            3 | 4 | 9 | 10 | 11 | 12 | 17 | 18 => (5, 1),
            5 | 6 => (9, 2),
            7 | 8 | 16 | 19 | 20 => (3, 1),
            15 => (4, 1),
            _ => return None,
        };
        at += size;
        index += slots;
    }
    u16_at(at)
}

/// The classes among `available` (internal names) that `name` means. `name` is a class name in any
/// of the usual spellings: `com.acme.Outer.Inner`, `com/acme/Outer$Inner`, or just `Inner` or
/// `Outer.Inner` when only one class of the jar ends that way.
pub fn find_class(available: &[String], name: &str) -> Result<String> {
    let wanted = name.replace('/', ".");
    let dotted = |internal: &str| internal.replace('/', ".");
    let exact: Vec<&String> = available.iter().filter(|c| dotted(c) == wanted || dotted(c).replace('$', ".") == wanted).collect();
    if let [only] = exact.as_slice() {
        return Ok((*only).clone());
    }
    let ends_with = |c: &str| {
        let (plain, nested) = (dotted(c), dotted(c).replace('$', "."));
        [plain, nested].iter().any(|d| d.ends_with(&format!(".{wanted}")))
    };
    let candidates: Vec<&String> = if exact.is_empty() { available.iter().filter(|c| ends_with(c)).collect() } else { exact };
    match candidates.as_slice() {
        [] => bail!("no class `{name}` in the jar; run `kargo inspect <module>` to list its classes"),
        [only] => Ok((*only).clone()),
        several => {
            let names: Vec<String> = several.iter().map(|c| format!("`{}`", dotted(c).replace('$', "."))).collect();
            bail!("`{name}` matches several classes: {}; use the full name", names.join(", "))
        }
    }
}

/// The name `javap` and the Java source use for an internal name: `com.acme.Outer$Inner`.
pub fn binary_name(internal: &str) -> String {
    internal.replace('/', ".")
}

/// The name Kotlin and Java source use for an internal name: `com.acme.Outer.Inner`.
pub fn source_name(internal: &str) -> String {
    binary_name(internal).replace('$', ".")
}

/// Runs the JDK's `javap` over `classes` (internal names) of `located`, printing to stdout.
pub fn javap(ctx: &BuildContext, located: &Located, classes: &[String], visibility: Visibility) -> Result<()> {
    let jdk = ctx.jdk()?;
    let javap = jdk.javap();
    if !javap.is_file() {
        bail!("{} has no `javap`; install a full JDK or pin one with `[toolchain] jdk`", jdk.home.display());
    }
    let mut classpath = vec![located.jar.clone()];
    classpath.extend(located.classpath.iter().filter(|f| **f != located.jar).cloned());
    let mut cmd = Command::new(javap);
    cmd.arg(visibility.javap_flag()).arg("-classpath").arg(kargo_util::fs::join_paths(&classpath));
    cmd.args(classes.iter().map(|c| binary_name(c)));
    kargo_util::process::run(ctx.shell(), &mut cmd, "javap")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn anonymous_and_local_classes_are_not_named() {
        assert!(is_named_class("com/acme/Outer"));
        assert!(is_named_class("com/acme/Outer$Inner"));
        assert!(is_named_class("com/acme/Outer$$serializer"));
        assert!(!is_named_class("com/acme/Outer$1"));
        assert!(!is_named_class("com/acme/Outer$1Local"));
        assert!(!is_named_class("module-info"));
        assert!(!is_named_class("com/acme/package-info"));
    }

    #[test]
    fn finds_a_class_by_any_spelling() {
        let available = names(&["com/acme/Outer", "com/acme/Outer$Inner", "com/acme/Util", "org/other/Util"]);
        assert_eq!(find_class(&available, "com.acme.Outer").unwrap(), "com/acme/Outer");
        assert_eq!(find_class(&available, "com/acme/Outer").unwrap(), "com/acme/Outer");
        assert_eq!(find_class(&available, "com.acme.Outer.Inner").unwrap(), "com/acme/Outer$Inner");
        assert_eq!(find_class(&available, "com.acme.Outer$Inner").unwrap(), "com/acme/Outer$Inner");
        assert_eq!(find_class(&available, "Inner").unwrap(), "com/acme/Outer$Inner");
        assert_eq!(find_class(&available, "Outer.Inner").unwrap(), "com/acme/Outer$Inner");
        assert_eq!(find_class(&available, "acme.Util").unwrap(), "com/acme/Util");
    }

    #[test]
    fn an_ambiguous_or_missing_class_is_an_error() {
        let available = names(&["com/acme/Util", "org/other/Util"]);
        let ambiguous = find_class(&available, "Util").unwrap_err().to_string();
        assert!(ambiguous.contains("`com.acme.Util`") && ambiguous.contains("`org.other.Util`"), "{ambiguous}");
        assert!(find_class(&available, "Missing").unwrap_err().to_string().contains("no class `Missing`"));
    }

    #[test]
    fn reads_the_access_flags_after_the_constant_pool() {
        // magic, version 0.52, a pool of 4 entries (a UTF-8 of 2 bytes, a long taking two slots), then the flags.
        let mut class = vec![0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 52, 0, 4];
        class.extend([1, 0, 2, b'h', b'i']);
        class.extend([5, 0, 0, 0, 0, 0, 0, 0, 1]);
        class.extend([0x00, 0x21]);
        assert_eq!(access_flags(&class), Some(0x21));
        assert_eq!(access_flags(&class[..12]), None);
        assert_eq!(access_flags(b"not a class"), None);
    }
}
