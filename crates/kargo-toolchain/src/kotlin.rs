//! The Kotlin compiler, fetched from Maven Central as `kotlin-compiler-embeddable`
//! and run on the detected JDK.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use kargo_maven::{Artifact, DependencyRequest, MavenClient, ModuleId, Platform, Resolver};

use crate::jdk::Jdk;

pub const KOTLIN_GROUP: &str = "org.jetbrains.kotlin";
pub const JVM_COMPILER_MAIN: &str = "org.jetbrains.kotlin.cli.jvm.K2JVMCompiler";
pub const JS_COMPILER_MAIN: &str = "org.jetbrains.kotlin.cli.js.K2JSCompiler";

/// The first Kotlin release whose compiler runs on JDK 25 and newer. Older
/// compilers bundle an IntelliJ `JavaVersion` that rejects these JDKs with an
/// `IllegalArgumentException` before compiling anything.
pub const FIRST_KOTLIN_FOR_JDK_25: &str = "2.1.10";

/// The JDK major version suggested when the compiler cannot run on the current one.
const SUGGESTED_JDK: u32 = 21;

#[derive(Debug, Clone)]
pub struct KotlinCompiler {
    pub version: String,
    pub classpath: Vec<PathBuf>,
    /// Whether `classpath` includes the Build Tools API implementation
    /// (`kotlin-build-tools-impl`), which provides incremental compilation.
    pub build_tools: bool,
}

impl KotlinCompiler {
    /// Resolves the compiler's classpath, with the Build Tools API implementation when the
    /// version publishes one. The result for a version never changes, so it is remembered
    /// in the download cache and later runs skip dependency resolution entirely. A project
    /// build locks the classpath in `kargo.lock` instead (see [`compiler_roots`]).
    pub fn resolve(client: &MavenClient, version: &str) -> Result<Self> {
        if let Some(classpath) = resolve_memoized(client, version, true)? {
            return Ok(Self::new(version, classpath, true));
        }
        let classpath =
            resolve_memoized(client, version, false)?.with_context(|| format!("failed to obtain the Kotlin {version} compiler"))?;
        Ok(Self::new(version, classpath, false))
    }

    /// The compiler `version` with the resolved `classpath` of [`compiler_roots`].
    pub fn new(version: &str, classpath: Vec<PathBuf>, build_tools: bool) -> Self {
        Self { version: version.to_owned(), classpath, build_tools }
    }

    /// The Build Tools API jar, against which Kargo's incremental compilation bridge is built.
    pub fn build_tools_api_jar(&self) -> Option<&PathBuf> {
        let prefix = format!("kotlin-build-tools-api-{}", self.version);
        self.build_tools.then(|| self.classpath.iter().find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(&prefix))))?
    }
}

/// The modules whose resolution is the compiler's classpath: `kotlin-compiler-embeddable`, and with
/// `build_tools` the Build Tools API implementation (`kotlin-build-tools-impl`).
pub fn compiler_roots(version: &str, build_tools: bool) -> Vec<DependencyRequest> {
    let mut roots = vec![DependencyRequest::new(ModuleId::new(KOTLIN_GROUP, "kotlin-compiler-embeddable"), Some(version.to_owned()))];
    if build_tools {
        roots.push(DependencyRequest::new(ModuleId::new(KOTLIN_GROUP, "kotlin-build-tools-impl"), Some(version.to_owned())));
    }
    roots
}

/// Whether Kotlin `version` publishes the Build Tools API implementation, which incremental compilation needs.
/// A version without one is remembered in the download cache. Offline, a version whose POM is not cached
/// compiles non-incrementally this time.
pub fn has_build_tools(client: &MavenClient, version: &str) -> bool {
    let memo = memo_path(client, "kotlin-build-tools-impl", version);
    if !client.refresh() && std::fs::read_to_string(&memo).is_ok_and(|text| text.trim() == UNAVAILABLE) {
        return false;
    }
    let pom = Artifact::new(ModuleId::new(KOTLIN_GROUP, "kotlin-build-tools-impl"), version).with_extension("pom").repo_path();
    match client.fetch(&pom) {
        Ok(Some(_)) => true,
        Ok(None) => {
            let _ = kargo_util::fs::write_string_atomic(&memo, UNAVAILABLE);
            false
        }
        Err(_) => false,
    }
}

/// The file that remembers the classpath of `name` (the compiler, or the compiler with the Build Tools API).
fn memo_path(client: &MavenClient, name: &str, version: &str) -> PathBuf {
    client.cache_dir().join(".classpaths").join(format!("{name}-{version}.txt"))
}

/// The compiler classpath, optionally with `kotlin-build-tools-impl`. `Ok(None)` when the
/// Build Tools API implementation is not published for `version`.
fn resolve_memoized(client: &MavenClient, version: &str, build_tools: bool) -> Result<Option<Vec<PathBuf>>> {
    let cache = client.cache_dir();
    let name = if build_tools { "kotlin-build-tools-impl" } else { "kotlin-compiler-embeddable" };
    let memo = memo_path(client, name, version);
    // `--refresh` resolves again.
    if let Ok(text) = std::fs::read_to_string(&memo)
        && !client.refresh()
    {
        if text.trim() == UNAVAILABLE {
            return Ok(None);
        }
        let classpath: Vec<PathBuf> = text.lines().filter(|l| !l.is_empty()).map(|l| cache.join(l)).collect();
        if !classpath.is_empty() && classpath.iter().all(|p| p.is_file()) {
            return Ok(Some(classpath));
        }
    }
    if build_tools && !has_build_tools(client, version) {
        return Ok(None);
    }
    let resolver = Resolver::new(client, Platform::Jvm);
    let resolution = match resolver.resolve(&compiler_roots(version, build_tools)) {
        Ok(r) => r,
        Err(_) if build_tools => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("failed to obtain the Kotlin {version} compiler")),
    };
    let classpath = resolution.files();
    let rel: Option<Vec<String>> = classpath.iter().map(|p| p.strip_prefix(cache).ok().map(|r| r.to_string_lossy().into_owned())).collect();
    if let Some(rel) = rel {
        // Only a speed-up: a failed write just means resolving again next time.
        let _ = kargo_util::fs::write_string_atomic(&memo, &(rel.join("\n") + "\n"));
    }
    Ok(Some(classpath))
}

/// Memo content for a Kotlin version without a published Build Tools API implementation.
const UNAVAILABLE: &str = "unavailable";

/// The newest JDK major version the Kotlin `version` compiler can run on, or
/// `None` if no limit is known.
pub fn max_compiler_jdk(version: &str) -> Option<u32> {
    let numbers = release_numbers(version)?;
    (numbers < release_numbers(FIRST_KOTLIN_FOR_JDK_25)?).then_some(24)
}

/// Fails with an actionable message if the Kotlin `version` compiler is known
/// not to run on `jdk`.
pub fn check_compiler_jdk(version: &str, jdk: &Jdk) -> Result<()> {
    if let Some(max) = max_compiler_jdk(version)
        && jdk.version > max
    {
        bail!(
            "Kotlin {version} cannot run on JDK {} (at {}): its compiler supports up to JDK {max}; \
             set `[toolchain] jdk = \"{SUGGESTED_JDK}\"` in kargo.toml, or use Kotlin {FIRST_KOTLIN_FOR_JDK_25} or newer",
            jdk.version,
            jdk.home.display()
        );
    }
    Ok(())
}

/// The numeric part of a Kotlin version (`2.1.20-RC` -> `[2, 1, 20]`), padded
/// to three components. Qualifiers are ignored so pre-releases compare like
/// their release.
fn release_numbers(version: &str) -> Option<[u32; 3]> {
    let base = version.split(['-', '+']).next()?;
    let mut out = [0; 3];
    for (i, part) in base.split('.').enumerate() {
        *out.get_mut(i)? = part.parse().ok()?;
    }
    Some(out)
}

/// Fetches a single jar (no transitive dependencies), e.g. a compiler plugin.
pub fn fetch_jar(client: &MavenClient, group: &str, artifact: &str, version: &str) -> Result<PathBuf> {
    let artifact = Artifact::new(ModuleId::new(group, artifact), version);
    client.fetch_required(&artifact.repo_path())
}

/// Fetches a Kotlin compiler plugin matching the compiler version.
pub fn fetch_kotlin_plugin(client: &MavenClient, artifact: &str, kotlin_version: &str) -> Result<PathBuf> {
    fetch_jar(client, KOTLIN_GROUP, artifact, kotlin_version)
        .with_context(|| format!("failed to fetch Kotlin compiler plugin `{artifact}` {kotlin_version}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jdk(version: u32) -> Jdk {
        Jdk { home: PathBuf::from("/jdk"), version }
    }

    #[test]
    fn old_compilers_reject_jdk_25() {
        assert_eq!(max_compiler_jdk("2.1.0"), Some(24));
        assert_eq!(max_compiler_jdk("1.9.24"), Some(24));
        assert_eq!(max_compiler_jdk("2.1.10"), None);
        assert_eq!(max_compiler_jdk("2.1.10-RC"), None);
        assert_eq!(max_compiler_jdk("2.4.21"), None);
        assert_eq!(max_compiler_jdk("not-a-version"), None);
        assert_eq!(max_compiler_jdk("2.1.0.1.5"), None);

        let err = check_compiler_jdk("2.1.0", &jdk(25)).unwrap_err().to_string();
        assert!(err.contains("Kotlin 2.1.0 cannot run on JDK 25"), "{err}");
        assert!(err.contains("jdk = \"21\"") && err.contains("2.1.10 or newer"), "{err}");
        assert!(check_compiler_jdk("2.1.0", &jdk(21)).is_ok());
        assert!(check_compiler_jdk("2.1.10", &jdk(25)).is_ok());
    }
}
