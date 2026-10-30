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

/// Sources of one compilation: common sources plus platform-specific ones, and the intermediate
/// source sets between them (`[source-sets.<name>]`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceSet {
    pub common: Vec<PathBuf>,
    pub platform: Vec<PathBuf>,
    /// Fragment name for the platform sources (`jvm`, `wasm`, `native`).
    pub platform_name: String,
    /// The intermediate source sets this compilation includes, wider ones first.
    pub groups: Vec<SourceGroup>,
}

/// An intermediate fragment of a multiplatform compilation: the sources that several targets share.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceGroup {
    pub name: String,
    pub files: Vec<PathBuf>,
    /// The groups this one refines directly. A group that refines none refines `common`.
    pub refines: Vec<String>,
}

impl SourceGroup {
    pub fn new(name: &str, files: Vec<PathBuf>, refines: &[&str]) -> Self {
        Self { name: name.to_owned(), files, refines: refines.iter().map(|r| (*r).to_owned()).collect() }
    }
}

impl SourceSet {
    pub fn new(common: Vec<PathBuf>, platform: Vec<PathBuf>, platform_name: &str) -> Self {
        Self { common, platform, platform_name: platform_name.to_owned(), groups: Vec::new() }
    }

    pub fn all(&self) -> impl Iterator<Item = &PathBuf> {
        self.common.iter().chain(self.groups.iter().flat_map(|g| &g.files)).chain(&self.platform)
    }

    pub fn kotlin_files(&self) -> Vec<PathBuf> {
        self.all().filter(|p| p.extension().is_some_and(|e| e == "kt")).cloned().collect()
    }

    pub fn java_files(&self) -> Vec<PathBuf> {
        self.all().filter(|p| p.extension().is_some_and(|e| e == "java")).cloned().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.common.is_empty() && self.platform.is_empty() && self.groups.iter().all(|g| g.files.is_empty())
    }

    /// Whether Kotlin files sit beside the common ones, in the platform's directory or in an
    /// intermediate source set, so the compilation has fragments. New generated files then join the
    /// platform side instead of turning a `src/`-only compilation multiplatform.
    pub fn has_platform_kotlin(&self) -> bool {
        let is_kt = |p: &PathBuf| p.extension().is_some_and(|e| e == "kt");
        self.platform.iter().any(is_kt) || self.groups.iter().any(|g| g.files.iter().any(is_kt))
    }

    /// Appends another source set (e.g. tests compiled together with main sources).
    pub fn extend(&mut self, other: &SourceSet) {
        self.common.extend(other.common.iter().cloned());
        self.platform.extend(other.platform.iter().cloned());
        for group in &other.groups {
            match self.groups.iter_mut().find(|g| g.name == group.name) {
                Some(mine) => {
                    mine.files.extend(group.files.iter().cloned());
                    for parent in &group.refines {
                        if !mine.refines.contains(parent) {
                            mine.refines.push(parent.clone());
                        }
                    }
                }
                None => self.groups.push(group.clone()),
            }
        }
    }

    /// Multiplatform arguments enabling `expect`/`actual` between common, intermediate and platform
    /// sources. Empty when only one side has Kotlin sources.
    ///
    /// The fragments are `common`, every group and the platform. A group refines the groups it names
    /// (or `common`), and the platform refines the narrowest groups. A fragment without Kotlin files
    /// is left out, and the fragments that refined it refine what it refined.
    pub fn fragment_args(&self) -> Vec<String> {
        self.fragment_args_with(slash)
    }

    /// [`fragment_args`](Self::fragment_args) with the file paths written by `path`. The compiler matches
    /// the paths of `-Xfragment-sources` to the sources it is given as strings, so they must be written the
    /// way the caller passes the sources: `slash` for Kargo's own compiler calls, the native form for Gradle.
    pub fn fragment_args_with(&self, path: impl Fn(&Path) -> String) -> Vec<String> {
        let is_kt = |p: &&PathBuf| p.extension().is_some_and(|e| e == "kt");
        struct Fragment<'a> {
            name: &'a str,
            files: Vec<&'a PathBuf>,
            refines: Vec<&'a str>,
        }
        // A group can also refine the platform fragment itself (the sets of some Native platforms refine `native`), so
        // that it, and every group that refines it, is below the platform and not one of the platform's parents.
        fn below(groups: &[SourceGroup], name: &str, platform: &str, depth: usize) -> bool {
            groups
                .iter()
                .find(|g| g.name == name)
                .is_some_and(|g| g.refines.iter().any(|r| r == platform || (depth > 0 && below(groups, r, platform, depth - 1))))
        }
        let leaves: Vec<&str> = self
            .groups
            .iter()
            .filter(|g| !below(&self.groups, &g.name, &self.platform_name, self.groups.len()))
            .filter(|g| !self.groups.iter().any(|o| o.refines.contains(&g.name)))
            .map(|g| g.name.as_str())
            .collect();
        fn fragment(g: &SourceGroup) -> Fragment<'_> {
            let is_kt = |p: &&PathBuf| p.extension().is_some_and(|e| e == "kt");
            Fragment {
                name: &g.name,
                files: g.files.iter().filter(is_kt).collect(),
                refines: g.refines.iter().map(String::as_str).collect(),
            }
        }
        // A fragment comes after the ones it refines: common, the sets above the platform, the platform, the sets below it.
        let is_below = |g: &&SourceGroup| below(&self.groups, &g.name, &self.platform_name, self.groups.len());
        let mut all: Vec<Fragment> =
            vec![Fragment { name: "common", files: self.common.iter().filter(is_kt).collect(), refines: Vec::new() }];
        all.extend(self.groups.iter().filter(|g| !is_below(g)).map(fragment));
        all.push(Fragment { name: &self.platform_name, files: self.platform.iter().filter(is_kt).collect(), refines: leaves });
        all.extend(self.groups.iter().filter(is_below).map(fragment));

        let present = |name: &str| all.iter().any(|f| f.name == name && !f.files.is_empty());
        // The nearest fragments with files that `refines` leads to, `common` being the root.
        fn resolve<'a>(all: &[Fragment<'a>], refines: &[&'a str], present: &dyn Fn(&str) -> bool, depth: usize) -> Vec<&'a str> {
            let mut out: Vec<&str> = Vec::new();
            let parents: Vec<&str> = if refines.is_empty() { vec!["common"] } else { refines.to_vec() };
            for parent in parents {
                let found: Vec<&str> = if present(parent) {
                    vec![parent]
                } else if depth == 0 || parent == "common" {
                    Vec::new()
                } else {
                    let next = all.iter().find(|f| f.name == parent).map(|f| f.refines.clone()).unwrap_or_default();
                    resolve(all, &next, present, depth - 1)
                };
                for name in found {
                    if !out.contains(&name) {
                        out.push(name);
                    }
                }
            }
            out
        }
        let used: Vec<&Fragment> = all.iter().filter(|f| !f.files.is_empty()).collect();
        if used.len() < 2 {
            return Vec::new();
        }
        let mut args =
            vec!["-Xmulti-platform".to_owned(), format!("-Xfragments={}", used.iter().map(|f| f.name).collect::<Vec<_>>().join(","))];
        for fragment in &used {
            if fragment.name == "common" {
                continue;
            }
            for parent in resolve(&all, &fragment.refines, &present, all.len()) {
                args.push(format!("-Xfragment-refines={}:{parent}", fragment.name));
            }
        }
        let path = &path;
        let sources: Vec<String> = used.iter().flat_map(|f| f.files.iter().map(move |p| format!("{}:{}", f.name, path(p)))).collect();
        args.push(format!("-Xfragment-sources={}", sources.join(",")));
        args
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
    fn fragment_args_write_the_paths_the_caller_chooses() {
        let set = SourceSet::new(vec!["p/src/a.kt".into()], vec!["p/src-jvm/b.kt".into()], "jvm");
        let args = set.fragment_args_with(|p| p.to_string_lossy().replace('/', "|"));
        assert_eq!(args[3], "-Xfragment-sources=common:p|src|a.kt,jvm:p|src-jvm|b.kt");
    }

    #[test]
    fn fragment_args_with_intermediate_source_sets() {
        let kt = |p: &str| vec![PathBuf::from(p)];
        let mut set = SourceSet::new(kt("c.kt"), kt("jvm.kt"), "jvm");
        set.groups = vec![SourceGroup::new("nonWeb", kt("nw.kt"), &[]), SourceGroup::new("jvmAndAndroid", kt("ja.kt"), &["nonWeb"])];
        assert_eq!(
            set.fragment_args(),
            [
                "-Xmulti-platform",
                "-Xfragments=common,nonWeb,jvmAndAndroid,jvm",
                "-Xfragment-refines=nonWeb:common",
                "-Xfragment-refines=jvmAndAndroid:nonWeb",
                "-Xfragment-refines=jvm:jvmAndAndroid",
                "-Xfragment-sources=common:c.kt,nonWeb:nw.kt,jvmAndAndroid:ja.kt,jvm:jvm.kt",
            ]
        );

        // A source set with no Kotlin files drops out; what refined it refines what it refined.
        set.groups[1].files.clear();
        let args = set.fragment_args();
        assert_eq!(args[1], "-Xfragments=common,nonWeb,jvm");
        assert!(args.contains(&"-Xfragment-refines=jvm:nonWeb".to_owned()), "{args:?}");
        set.groups[0].files.clear();
        let args = set.fragment_args();
        assert_eq!(args[1], "-Xfragments=common,jvm");
        assert!(args.contains(&"-Xfragment-refines=jvm:common".to_owned()), "{args:?}");

        // A platform that shares only an intermediate set with another has two fragments with no common.
        let mut shared = SourceSet::new(Vec::new(), kt("jvm.kt"), "jvm");
        shared.groups = vec![SourceGroup::new("jvmAndAndroid", kt("ja.kt"), &[])];
        assert_eq!(
            shared.fragment_args(),
            [
                "-Xmulti-platform",
                "-Xfragments=jvmAndAndroid,jvm",
                "-Xfragment-refines=jvm:jvmAndAndroid",
                "-Xfragment-sources=jvmAndAndroid:ja.kt,jvm:jvm.kt",
            ]
        );
        assert!(shared.has_platform_kotlin());
        assert_eq!(shared.kotlin_files().len(), 2);
    }

    #[test]
    fn source_sets_of_native_platforms_are_below_the_native_fragment() {
        let kt = |p: &str| vec![PathBuf::from(p)];
        let mut set = SourceSet::new(kt("c.kt"), kt("native.kt"), "native");
        set.groups = vec![
            SourceGroup::new("apple", kt("a.kt"), &["native"]),
            SourceGroup::new("ios", kt("i.kt"), &["apple"]),
            SourceGroup::new("nonJs", kt("n.kt"), &[]),
        ];
        let args = set.fragment_args();
        assert_eq!(args[1], "-Xfragments=common,nonJs,native,apple,ios");
        // The whole-target set sits above the platform fragment, the platform sets below it.
        for edge in ["apple:native", "ios:apple", "nonJs:common", "native:nonJs"] {
            assert!(args.contains(&format!("-Xfragment-refines={edge}")), "{edge} in {args:?}");
        }
        assert!(!args.iter().any(|a| a == "-Xfragment-refines=native:ios" || a == "-Xfragment-refines=native:apple"), "{args:?}");

        // Without code in `src-native/` the platform sets refine what it refined.
        set.platform.clear();
        let args = set.fragment_args();
        assert_eq!(args[1], "-Xfragments=common,nonJs,apple,ios");
        assert!(args.contains(&"-Xfragment-refines=apple:nonJs".to_owned()), "{args:?}");
        assert!(args.contains(&"-Xfragment-refines=ios:apple".to_owned()), "{args:?}");
    }

    #[test]
    fn extending_a_source_set_merges_groups_by_name() {
        let kt = |p: &str| vec![PathBuf::from(p)];
        let mut main = SourceSet::new(kt("c.kt"), Vec::new(), "jvm");
        main.groups = vec![SourceGroup::new("shared", kt("a.kt"), &[])];
        let mut tests = SourceSet::new(kt("ct.kt"), Vec::new(), "jvm");
        tests.groups = vec![SourceGroup::new("shared", kt("at.kt"), &[]), SourceGroup::new("other", kt("o.kt"), &["shared"])];
        main.extend(&tests);
        assert_eq!(main.groups.len(), 2);
        assert_eq!(main.groups[0].files, [PathBuf::from("a.kt"), PathBuf::from("at.kt")]);
        assert_eq!(main.groups[1].refines, ["shared"]);
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
