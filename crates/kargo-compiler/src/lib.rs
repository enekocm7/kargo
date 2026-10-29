//! Drivers for the Kotlin compilers (JVM, Wasm, Native) plus helpers shared
//! between them: source sets, compiler plugins and up-to-date checks.

pub mod abi;
pub mod daemon;
pub mod diagnostics;
pub mod incremental;
pub mod jar;
pub mod js;
pub mod junit;
pub mod jvm;
pub mod ksp;
pub mod mainclass;
pub mod native;
pub mod shade;
pub mod testreport;
pub mod wasm;

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Result, bail};

use kargo_toolchain::Jdk;
use kargo_util::hash::Fingerprint;
use kargo_util::process::{self, slash};
use kargo_util::shell::Shell;

/// Sources of one compilation: common sources plus platform-specific ones.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceSet {
    pub common: Vec<PathBuf>,
    pub platform: Vec<PathBuf>,
    /// Fragment name for the platform sources (`jvm`, `wasm`, `native`).
    pub platform_name: String,
}

impl SourceSet {
    pub fn new(common: Vec<PathBuf>, platform: Vec<PathBuf>, platform_name: &str) -> Self {
        Self { common, platform, platform_name: platform_name.to_owned() }
    }

    pub fn all(&self) -> impl Iterator<Item = &PathBuf> {
        self.common.iter().chain(&self.platform)
    }

    pub fn kotlin_files(&self) -> Vec<PathBuf> {
        self.all().filter(|p| p.extension().is_some_and(|e| e == "kt")).cloned().collect()
    }

    pub fn java_files(&self) -> Vec<PathBuf> {
        self.all().filter(|p| p.extension().is_some_and(|e| e == "java")).cloned().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.common.is_empty() && self.platform.is_empty()
    }

    /// Appends another source set (e.g. tests compiled together with main sources).
    pub fn extend(&mut self, other: &SourceSet) {
        self.common.extend(other.common.iter().cloned());
        self.platform.extend(other.platform.iter().cloned());
    }

    /// Multiplatform arguments enabling `expect`/`actual` between common and
    /// platform sources. Empty when only one side has Kotlin sources.
    pub fn fragment_args(&self) -> Vec<String> {
        let is_kt = |p: &&PathBuf| p.extension().is_some_and(|e| e == "kt");
        let common: Vec<&PathBuf> = self.common.iter().filter(is_kt).collect();
        let platform: Vec<&PathBuf> = self.platform.iter().filter(is_kt).collect();
        if common.is_empty() || platform.is_empty() {
            return Vec::new();
        }
        let name = &self.platform_name;
        let sources: Vec<String> =
            common.iter().map(|p| format!("common:{}", slash(p))).chain(platform.iter().map(|p| format!("{name}:{}", slash(p)))).collect();
        vec![
            "-Xmulti-platform".to_owned(),
            format!("-Xfragments=common,{name}"),
            format!("-Xfragment-refines={name}:common"),
            format!("-Xfragment-sources={}", sources.join(",")),
        ]
    }
}

/// A compiler plugin jar with its `-P` option values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPlugin {
    pub jar: PathBuf,
    pub options: Vec<String>,
}

pub fn plugin_args(plugins: &[ResolvedPlugin]) -> Vec<String> {
    let mut args = Vec::new();
    for plugin in plugins {
        args.push(format!("-Xplugin={}", slash(&plugin.jar)));
    }
    for plugin in plugins {
        for option in &plugin.options {
            args.push("-P".to_owned());
            args.push(option.clone());
        }
    }
    args
}

/// Result of a compilation step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Inputs unchanged since the last successful build.
    Fresh,
    Compiled,
    /// Nothing to compile.
    Empty,
}

/// Stores a fingerprint next to build outputs for up-to-date checks.
#[derive(Debug, Clone)]
pub struct FreshnessCheck {
    path: PathBuf,
    value: String,
}

impl FreshnessCheck {
    pub fn new(path: PathBuf, fingerprint: &Fingerprint) -> Self {
        Self { path, value: fingerprint.finish() }
    }

    pub fn is_fresh(&self) -> bool {
        std::fs::read_to_string(&self.path).is_ok_and(|v| v == self.value)
    }

    pub fn invalidate(&self) {
        let _ = std::fs::remove_file(&self.path);
    }

    pub fn record(&self) -> Result<()> {
        kargo_util::fs::write_string_atomic(&self.path, &self.value)
    }
}

/// Builds a fingerprint over source files, classpath, plugins and arguments.
pub fn fingerprint(sources: &SourceSet, classpath: &[PathBuf], plugins: &[ResolvedPlugin], args: &[String]) -> Fingerprint {
    let mut fp = Fingerprint::new();
    fp.str(env!("CARGO_PKG_VERSION"));
    for f in sources.all() {
        fp.file_stamp(f);
    }
    fp.str("--classpath");
    for f in classpath {
        // A path dependency's jar counts by its ABI, so a change to its bodies does not recompile this.
        abi::stamp_classpath_entry(&mut fp, f);
    }
    fp.str("--plugins");
    for p in plugins {
        fp.file_stamp(&p.jar);
        for o in &p.options {
            fp.str(o);
        }
    }
    fp.str("--args");
    for a in args {
        fp.str(a);
    }
    fp
}

/// JVM options for running Kotlin compiler tools.
pub fn compiler_jvm_options(jdk: &Jdk) -> Vec<String> {
    let mut opts = vec!["-Xss8m".to_owned(), "-Djava.awt.headless=true".to_owned()];
    if jdk.version >= 23 {
        // The compiler uses sun.misc.Unsafe; silence JDK 23+ deprecation noise.
        opts.push("--sun-misc-unsafe-memory-access=allow".to_owned());
    }
    if let Ok(extra) = std::env::var("KARGO_COMPILER_JVM_OPTS") {
        opts.extend(extra.split_whitespace().map(str::to_owned));
    }
    opts
}

/// Runs a Kotlin compiler main class with arguments passed via an `@argfile`.
pub fn run_kotlin_compiler(
    shell: &Shell,
    jdk: &Jdk,
    compiler_classpath: &[PathBuf],
    main_class: &str,
    args: &[String],
    argfile: &Path,
    what: &str,
) -> Result<()> {
    process::write_argfile(argfile, args)?;
    if shell.is_verbose() {
        shell.verbose("Arguments", args.join(" "));
    }
    if let Some(code) = daemon::run(shell, jdk, compiler_classpath, daemon::Tool::Kotlin(main_class), args) {
        if code != 0 {
            bail!("could not compile `{what}` due to previous errors");
        }
        return Ok(());
    }
    let mut cmd = Command::new(jdk.java());
    cmd.args(daemon::jvm_options(jdk, None))
        .args(daemon::cds_options(jdk, "kotlinc", compiler_classpath))
        .arg("-cp")
        .arg(kargo_util::fs::join_paths(compiler_classpath))
        .arg(main_class)
        .arg(format!("@{}", argfile.display()));
    let status = diagnostics::status(shell, &mut cmd, "the Kotlin compiler")?;
    if !status.success() {
        bail!("could not compile `{what}` due to previous errors");
    }
    Ok(())
}

/// Shared Kotlin arguments derived from the manifest and profile.
pub fn common_kotlin_args(manifest: &kargo_manifest::Manifest, profile: &kargo_manifest::Profile, extra: &[String]) -> Vec<String> {
    let k = &manifest.kotlin;
    let mut args = Vec::new();
    if let Some(v) = &k.language_version {
        args.push(format!("-language-version={v}"));
    }
    if let Some(v) = &k.api_version {
        args.push(format!("-api-version={v}"));
    }
    for o in &k.opt_in {
        args.push(format!("-opt-in={o}"));
    }
    if k.warnings_as_errors {
        args.push("-Werror".to_owned());
    }
    args.extend(extra.iter().cloned());
    args.extend(k.compiler_args.iter().cloned());
    args.extend(profile.compiler_args.iter().cloned());
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragment_args_only_when_both_sides_have_kotlin() {
        let set = SourceSet::new(vec!["a/common.kt".into()], vec!["b/jvm.kt".into(), "b/X.java".into()], "jvm");
        let args = set.fragment_args();
        assert_eq!(args[0], "-Xmulti-platform");
        assert_eq!(args[1], "-Xfragments=common,jvm");
        assert_eq!(args[2], "-Xfragment-refines=jvm:common");
        assert_eq!(args[3], "-Xfragment-sources=common:a/common.kt,jvm:b/jvm.kt");
        assert!(SourceSet::new(vec!["a.kt".into()], vec![], "jvm").fragment_args().is_empty());
        assert!(SourceSet::new(vec!["a.kt".into()], vec!["X.java".into()], "jvm").fragment_args().is_empty());
        assert_eq!(set.java_files(), [PathBuf::from("b/X.java")]);
        assert_eq!(set.kotlin_files().len(), 2);
    }

    #[test]
    fn plugin_argument_layout() {
        let plugins = vec![
            ResolvedPlugin { jar: "p1.jar".into(), options: vec!["plugin:a:k=v".into()] },
            ResolvedPlugin { jar: "p2.jar".into(), options: vec![] },
        ];
        assert_eq!(plugin_args(&plugins), ["-Xplugin=p1.jar", "-Xplugin=p2.jar", "-P", "plugin:a:k=v"]);
    }

    #[test]
    fn freshness_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.kt");
        std::fs::write(&src, "fun a() {}").unwrap();
        let set = SourceSet::new(vec![src.clone()], vec![], "jvm");
        let check = FreshnessCheck::new(dir.path().join("fp"), &fingerprint(&set, &[], &[], &["-x".into()]));
        assert!(!check.is_fresh());
        check.record().unwrap();
        assert!(check.is_fresh());
        let other = FreshnessCheck::new(dir.path().join("fp"), &fingerprint(&set, &[], &[], &["-y".into()]));
        assert!(!other.is_fresh(), "argument changes invalidate");
        check.invalidate();
        assert!(!check.is_fresh());
    }

    #[test]
    fn kotlin_args_from_manifest() {
        let m = kargo_manifest::Manifest::parse(
            "[package]\nname = \"a\"\nversion = \"1\"\n[kotlin]\nlanguage-version = \"2.3\"\nopt-in = [\"X\"]\nwarnings-as-errors = true\ncompiler-args = [\"-Xfoo\"]\n",
        )
        .unwrap();
        let args = common_kotlin_args(&m, &m.profiles["release"], &["-Xjsr305=strict".into()]);
        assert_eq!(args, ["-language-version=2.3", "-opt-in=X", "-Werror", "-Xjsr305=strict", "-Xfoo"]);
    }
}
