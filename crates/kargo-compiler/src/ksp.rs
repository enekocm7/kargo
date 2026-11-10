//! Kotlin Symbol Processing (KSP2) for the JVM and Kotlin/Native targets.
//!
//! KSP runs standalone (`symbol-processing-aa-embeddable`'s `KSPJvmMain`, or `KSPNativeMain`
//! once per Konan target) before the Kotlin compiler, over the same source directories. The
//! Kotlin (and on the JVM, Java) sources it generates are compiled together with the
//! hand-written ones, and on the JVM the resources (and any classes) it generates are copied
//! into the output.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Result, bail};

use super::FreshnessCheck;
use kargo_toolchain::Jdk;
use kargo_util::fs::{collect_files, join_paths, remove_dir_all_if_exists};
use kargo_util::hash::Fingerprint;
use kargo_util::process::{self, slash};
use kargo_util::shell::Shell;

pub const KSP_MAIN: &str = "com.google.devtools.ksp.cmdline.KSPJvmMain";
pub const KSP_NATIVE_MAIN: &str = "com.google.devtools.ksp.cmdline.KSPNativeMain";

/// The platform a KSP pass analyzes the sources for.
#[derive(Debug, Clone, Copy)]
pub enum KspPlatform<'a> {
    /// The JVM, with the bytecode version of `[target.jvm] jvm-target`.
    Jvm { jvm_target: &'a str },
    /// One Kotlin/Native platform (`linux_x64`, `ios_arm64`, ...); the libraries are klibs.
    Native { konan_target: &'a str },
}

/// One KSP pass (main or test sources).
#[derive(Debug, Clone)]
pub struct KspRun<'a> {
    /// The compilation unit's name (`app`, `app-test`), also KSP's module name.
    pub name: &'a str,
    /// Names the fingerprint and argument files when several passes share a unit's name
    /// (one per Native platform); `None` uses `name`.
    pub key: Option<&'a str>,
    /// Source directories; KSP reads the `.kt` and `.java` files under them.
    pub source_roots: Vec<PathBuf>,
    /// What the sources compile against: jars on the JVM, klibs on Native.
    pub libraries: Vec<PathBuf>,
    /// Output directories whose `internal` declarations are visible (tests -> main).
    pub friends: Vec<PathBuf>,
    /// The processors and their dependencies.
    pub processor_path: &'a [PathBuf],
    /// KSP itself (`symbol-processing-aa-embeddable` and its dependencies).
    pub tool_classpath: &'a [PathBuf],
    pub platform: KspPlatform<'a>,
    pub language_version: &'a str,
    pub api_version: &'a str,
    /// Root of the generated files: `kotlin/`, `java/`, `resources/`, `classes/` and `caches/`.
    pub output: PathBuf,
    pub project_dir: &'a Path,
    /// Options for the processors (`[kotlin] ksp-options`), passed as `-apoption=key=value`.
    pub options: Vec<(String, String)>,
}

/// What a KSP pass generated.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KspOutput {
    /// Generated `.kt` and `.java` files, to compile with the other sources.
    pub sources: Vec<PathBuf>,
    /// Directories to copy into the output like resources (generated resources and classes).
    pub resources: Vec<PathBuf>,
}

/// `2.4.21` -> `2.4`, the language version KSP analyzes the sources with by default.
pub fn language_version(kotlin_version: &str) -> String {
    kotlin_version.split(['.', '-']).take(2).collect::<Vec<_>>().join(".")
}

impl KspRun<'_> {
    fn kotlin_dir(&self) -> PathBuf {
        self.output.join("kotlin")
    }

    fn java_dir(&self) -> PathBuf {
        self.output.join("java")
    }

    fn resource_dir(&self) -> PathBuf {
        self.output.join("resources")
    }

    fn class_dir(&self) -> PathBuf {
        self.output.join("classes")
    }

    /// The `java` arguments: the classpath, the main class and KSP's options.
    fn args(&self, jdk: &Jdk) -> Vec<String> {
        let roots: Vec<PathBuf> = self.source_roots.iter().filter(|d| d.is_dir()).cloned().collect();
        let jvm = matches!(self.platform, KspPlatform::Jvm { .. });
        let main = if jvm { KSP_MAIN } else { KSP_NATIVE_MAIN };
        let mut args = vec!["-cp".to_owned(), join_paths(self.tool_classpath), main.to_owned(), format!("-module-name={}", self.name)];
        match self.platform {
            KspPlatform::Jvm { jvm_target } => {
                args.push(format!("-jvm-target={jvm_target}"));
                args.push(format!("-jdk-home={}", slash(&jdk.home)));
            }
            KspPlatform::Native { konan_target } => args.push(format!("-target={konan_target}")),
        }
        args.extend([
            format!("-language-version={}", self.language_version),
            format!("-api-version={}", self.api_version),
            format!("-source-roots={}", join_paths(&roots)),
        ]);
        // KSPNativeMain knows no Java options.
        if jvm {
            args.push(format!("-java-source-roots={}", join_paths(&roots)));
        }
        args.extend([
            format!("-project-base-dir={}", slash(self.project_dir)),
            format!("-output-base-dir={}", slash(&self.output)),
            format!("-caches-dir={}", slash(&self.output.join("caches"))),
            format!("-kotlin-output-dir={}", slash(&self.kotlin_dir())),
        ]);
        if jvm {
            args.push(format!("-java-output-dir={}", slash(&self.java_dir())));
        }
        args.extend([
            format!("-resource-output-dir={}", slash(&self.resource_dir())),
            format!("-class-output-dir={}", slash(&self.class_dir())),
            "-incremental=false".to_owned(),
        ]);
        if !self.libraries.is_empty() {
            args.push(format!("-libraries={}", join_paths(&self.libraries)));
        }
        if !self.friends.is_empty() {
            args.push(format!("-friends={}", join_paths(&self.friends)));
        }
        for (key, value) in &self.options {
            args.push(format!("-apoption={key}={value}"));
        }
        args.push(join_paths(self.processor_path));
        args
    }

    fn generated(&self) -> KspOutput {
        let mut sources = collect_files(&self.kotlin_dir(), &["kt"]);
        let resources = match self.platform {
            KspPlatform::Jvm { .. } => {
                sources.extend(collect_files(&self.java_dir(), &["java"]));
                vec![self.resource_dir(), self.class_dir()]
            }
            // A klib has no resources or classes.
            KspPlatform::Native { .. } => Vec::new(),
        };
        sources.sort();
        KspOutput { sources, resources }
    }

    /// Runs the processors unless the sources, libraries and processors are unchanged since the
    /// last run. `announce` is called right before KSP starts.
    pub fn run(&self, shell: &Shell, jdk: &Jdk, fingerprint_dir: &Path, announce: &dyn Fn()) -> Result<KspOutput> {
        let extensions: &[&str] = match self.platform {
            KspPlatform::Jvm { .. } => &["kt", "java"],
            KspPlatform::Native { .. } => &["kt"],
        };
        let sources: Vec<PathBuf> = self.source_roots.iter().flat_map(|d| collect_files(d, extensions)).collect();
        if sources.is_empty() {
            remove_dir_all_if_exists(&self.output)?;
            return Ok(KspOutput::default());
        }
        let args = self.args(jdk);
        let mut fp = Fingerprint::new();
        fp.str(env!("CARGO_PKG_VERSION"));
        for f in sources.iter().chain(&self.libraries).chain(self.processor_path).chain(self.tool_classpath) {
            fp.file_stamp(f);
        }
        for a in &args {
            fp.str(a);
        }
        let key = self.key.unwrap_or(self.name);
        let check = FreshnessCheck::new(fingerprint_dir.join(format!("{key}.ksp.fingerprint")), &fp);
        if check.is_fresh() && self.output.is_dir() {
            return Ok(self.generated());
        }
        check.invalidate();
        announce();

        // Without incremental processing, files of a previous run would linger as stale sources.
        remove_dir_all_if_exists(&self.output)?;
        let argfile = fingerprint_dir.join(format!("{key}.ksp.args"));
        process::write_argfile(&argfile, &args)?;
        if shell.is_verbose() {
            shell.verbose("Arguments", args.join(" "));
        }
        let mut cmd = Command::new(jdk.java());
        cmd.args(super::compiler_jvm_options(jdk)).arg(format!("@{}", argfile.display())).current_dir(self.project_dir);
        let status = super::diagnostics::status(shell, &mut cmd, "KSP")?;
        if !status.success() {
            bail!("symbol processing (KSP) of `{}` failed; see the errors above", self.name);
        }
        check.record()?;
        Ok(self.generated())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_version_is_major_minor() {
        assert_eq!(language_version("2.4.21"), "2.4");
        assert_eq!(language_version("2.5.0-RC"), "2.5");
        assert_eq!(language_version("2.3"), "2.3");
    }

    #[test]
    fn argument_construction() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let jdk = Jdk { home: PathBuf::from("/jdk"), version: 21 };
        let processors = [PathBuf::from("moshi-codegen.jar"), PathBuf::from("kotlinpoet.jar")];
        let tool = [PathBuf::from("ksp.jar")];
        let run = KspRun {
            name: "app-test",
            key: None,
            source_roots: vec![src.clone(), dir.path().join("src-jvm")],
            libraries: vec!["moshi.jar".into()],
            friends: vec!["classes".into()],
            processor_path: &processors,
            tool_classpath: &tool,
            platform: KspPlatform::Jvm { jvm_target: "17" },
            language_version: "2.4",
            api_version: "2.4",
            output: "gen".into(),
            project_dir: dir.path(),
            options: vec![("room.schemaLocation".into(), "/p/schemas".into())],
        };
        let args = run.args(&jdk);
        assert_eq!(args[..3], ["-cp".to_owned(), "ksp.jar".to_owned(), KSP_MAIN.to_owned()]);
        assert!(args.contains(&"-module-name=app-test".to_owned()));
        assert!(args.contains(&"-jvm-target=17".to_owned()));
        // Missing source directories are left out.
        assert!(args.contains(&format!("-source-roots={}", join_paths(&[src]))));
        assert!(args.contains(&"-kotlin-output-dir=gen/kotlin".to_owned()));
        assert!(args.contains(&"-libraries=moshi.jar".to_owned()));
        assert!(args.contains(&"-friends=classes".to_owned()));
        assert!(args.contains(&"-apoption=room.schemaLocation=/p/schemas".to_owned()));
        // The processor classpath is the positional argument at the end.
        assert_eq!(args.last().unwrap(), &join_paths(&processors));
    }

    #[test]
    fn native_argument_construction() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let jdk = Jdk { home: PathBuf::from("/jdk"), version: 21 };
        let processors = [PathBuf::from("room-compiler.jar")];
        let tool = [PathBuf::from("ksp.jar")];
        let libraries = vec![PathBuf::from("stdlib"), PathBuf::from("room-runtime.klib")];
        let run = KspRun {
            name: "shared",
            key: Some("shared-ios_arm64"),
            source_roots: vec![src],
            libraries: libraries.clone(),
            friends: vec![],
            processor_path: &processors,
            tool_classpath: &tool,
            platform: KspPlatform::Native { konan_target: "ios_arm64" },
            language_version: "2.4",
            api_version: "2.4",
            output: "gen".into(),
            project_dir: dir.path(),
        };
        let args = run.args(&jdk);
        assert_eq!(args[..3], ["-cp".to_owned(), "ksp.jar".to_owned(), KSP_NATIVE_MAIN.to_owned()]);
        assert!(args.contains(&"-module-name=shared".to_owned()));
        assert!(args.contains(&"-target=ios_arm64".to_owned()));
        assert!(args.contains(&format!("-libraries={}", join_paths(&libraries))));
        // KSPNativeMain knows no JVM options.
        assert!(!args.iter().any(|a| a.starts_with("-jvm-target") || a.starts_with("-jdk-home") || a.starts_with("-java-")), "{args:?}");
        assert_eq!(args.last().unwrap(), &join_paths(&processors));
    }

    #[test]
    fn no_sources_means_nothing_to_process() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("gen");
        std::fs::create_dir_all(out.join("kotlin")).unwrap();
        let run = KspRun {
            name: "app",
            key: None,
            source_roots: vec![dir.path().join("src")],
            libraries: vec![],
            friends: vec![],
            processor_path: &[],
            tool_classpath: &[],
            platform: KspPlatform::Jvm { jvm_target: "17" },
            language_version: "2.4",
            api_version: "2.4",
            output: out.clone(),
            project_dir: dir.path(),
            options: vec![],
        };
        let shell = Shell::new(kargo_util::shell::Verbosity::Quiet);
        let jdk = Jdk { home: PathBuf::from("/jdk"), version: 21 };
        let generated = run.run(&shell, &jdk, dir.path(), &|| panic!("KSP must not run")).unwrap();
        assert_eq!(generated, KspOutput::default());
        assert!(!out.exists(), "stale generated files are removed");
    }
}
