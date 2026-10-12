//! Kotlin/JVM (and mixed Java) compilation.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::incremental::{IcOutcome, IncrementalUnit};
use super::{FreshnessCheck, Outcome, ResolvedPlugin, SourceSet, daemon, fingerprint, plugin_args, run_kotlin_compiler};
use kargo_toolchain::kotlin::JVM_COMPILER_MAIN;
use kargo_toolchain::{Jdk, KotlinCompiler};
use kargo_util::fs::{copy_dir, join_paths, remove_dir_all_if_exists};
use kargo_util::hash::Fingerprint;
use kargo_util::process::{self, slash};
use kargo_util::shell::Shell;

/// One JVM compilation unit (main or test sources).
#[derive(Debug, Clone)]
pub struct JvmCompilation<'a> {
    pub name: &'a str,
    pub sources: SourceSet,
    pub classpath: Vec<PathBuf>,
    /// Output directories whose `internal` declarations are visible (tests -> main).
    pub friend_paths: Vec<PathBuf>,
    pub output: PathBuf,
    pub jvm_target: &'a str,
    pub plugins: &'a [ResolvedPlugin],
    pub kotlin_args: &'a [String],
    pub javac_args: &'a [String],
    /// Java annotation processors (e.g. Lombok).
    pub processor_path: &'a [PathBuf],
    pub resources: Vec<PathBuf>,
}

/// `1.8` -> `8`, `17` -> `17`.
pub fn javac_release(jvm_target: &str) -> &str {
    jvm_target.strip_prefix("1.").unwrap_or(jvm_target)
}

impl JvmCompilation<'_> {
    fn kotlin_args(&self, jdk: &Jdk) -> Vec<String> {
        let mut args = vec!["-d".to_owned(), slash(&self.output)];
        args.extend(self.kotlin_options(jdk));
        // Kotlin sees Java sources too, so Kotlin code can reference Java declarations.
        args.extend(self.sources.all().map(|p| slash(p)));
        args
    }

    /// Kotlin compiler arguments other than the output directory and the sources.
    fn kotlin_options(&self, jdk: &Jdk) -> Vec<String> {
        let mut args = vec![
            "-module-name".to_owned(),
            self.name.to_owned(),
            "-jvm-target".to_owned(),
            self.jvm_target.to_owned(),
            "-jdk-home".to_owned(),
            slash(&jdk.home),
            "-no-stdlib".to_owned(),
            "-no-reflect".to_owned(),
        ];
        if !self.classpath.is_empty() {
            args.push("-classpath".to_owned());
            args.push(join_paths(&self.classpath));
        }
        if !self.friend_paths.is_empty() {
            let friends: Vec<String> = self.friend_paths.iter().map(|p| slash(p)).collect();
            args.push(format!("-Xfriend-paths={}", friends.join(",")));
        }
        args.extend(plugin_args(self.plugins));
        args.extend(self.sources.fragment_args());
        args.extend(self.kotlin_args.iter().cloned());
        args
    }

    fn javac_args(&self) -> Vec<String> {
        let mut classpath = vec![self.output.clone()];
        classpath.extend(self.classpath.iter().cloned());
        let mut args = vec![
            "-d".to_owned(),
            slash(&self.output),
            "-encoding".to_owned(),
            "UTF-8".to_owned(),
            "--release".to_owned(),
            javac_release(self.jvm_target).to_owned(),
            "-classpath".to_owned(),
            join_paths(&classpath),
        ];
        if self.processor_path.is_empty() {
            args.push("-proc:none".to_owned());
        } else {
            args.push("-processorpath".to_owned());
            args.push(join_paths(self.processor_path));
            args.push("-proc:full".to_owned());
        }
        args.extend(self.javac_args.iter().cloned());
        args.extend(self.sources.java_files().iter().map(|p| slash(p)));
        args
    }

    /// Compiles if inputs changed; outputs go to a fresh `output` directory.
    /// `announce` is called right before real work starts.
    pub fn compile(
        &self,
        shell: &Shell,
        jdk: &Jdk,
        compiler: &KotlinCompiler,
        fingerprint_dir: &Path,
        announce: &dyn Fn(),
    ) -> Result<Outcome> {
        let resource_files: Vec<PathBuf> = self.resources.iter().flat_map(|d| all_files(d)).collect();
        if self.sources.is_empty() && resource_files.is_empty() {
            remove_dir_all_if_exists(&self.output)?;
            return Ok(Outcome::Empty);
        }

        let kotlin_args = self.kotlin_args(jdk);
        let javac_args = self.javac_args();
        let mut all_args = kotlin_args.clone();
        all_args.extend(javac_args.iter().cloned());
        all_args.push(compiler.version.clone());
        let mut fp = fingerprint(&self.sources, &self.classpath, self.plugins, &all_args);
        for f in &resource_files {
            fp.file_stamp(f);
        }
        for p in self.processor_path {
            fp.file_stamp(p);
        }
        let check = FreshnessCheck::new(fingerprint_dir.join(format!("{}.fingerprint", self.name)), &fp);
        if check.is_fresh() && self.output.is_dir() {
            return Ok(Outcome::Fresh);
        }
        check.invalidate();
        announce();

        // Classes javac and the resource copy put into the output last time; they are
        // replaced below, and removing them first drops those of deleted files.
        let produced = fingerprint_dir.join(format!("{}.produced", self.name));
        for file in std::fs::read_to_string(&produced).unwrap_or_default().lines().filter(|l| !l.is_empty()) {
            let _ = std::fs::remove_file(self.output.join(file));
        }
        let _ = std::fs::remove_file(&produced);

        let kotlin_files = !self.sources.kotlin_files().is_empty();
        let incremental =
            if kotlin_files { self.compile_incrementally(shell, jdk, compiler, fingerprint_dir)? } else { IcOutcome::Unavailable };
        match incremental {
            IcOutcome::Compiled => {}
            IcOutcome::Errors => bail!("could not compile `{}` due to previous errors", self.name),
            IcOutcome::Unavailable => {
                // A full compile leaves incremental caches behind that no longer match the output.
                remove_dir_all_if_exists(&fingerprint_dir.join(format!("{}.ic", self.name)))?;
                remove_dir_all_if_exists(&self.output)?;
                std::fs::create_dir_all(&self.output).with_context(|| format!("failed to create {}", self.output.display()))?;
                if kotlin_files {
                    run_kotlin_compiler(
                        shell,
                        jdk,
                        &compiler.classpath,
                        JVM_COMPILER_MAIN,
                        &kotlin_args,
                        &fingerprint_dir.join(format!("{}.kotlinc.args", self.name)),
                        self.name,
                    )?;
                }
            }
        }
        let before = output_files(&self.output);

        if !self.sources.java_files().is_empty() {
            if !jdk.has_javac() {
                bail!("the project has Java sources but {} has no `javac`; install a full JDK", jdk.home.display());
            }
            let argfile = fingerprint_dir.join(format!("{}.javac.args", self.name));
            process::write_argfile(&argfile, &javac_args)?;
            let success = match daemon::run(shell, jdk, &compiler.classpath, daemon::Tool::Javac, &javac_args) {
                Some(code) => code == 0,
                None => {
                    let mut cmd = Command::new(jdk.javac());
                    if jdk.version >= 23 {
                        // Annotation processors such as Lombok still use sun.misc.Unsafe.
                        cmd.arg("-J--sun-misc-unsafe-memory-access=allow");
                    }
                    cmd.arg(format!("@{}", argfile.display()));
                    crate::diagnostics::status(shell, &mut cmd, "javac")?.success()
                }
            };
            if !success {
                bail!("could not compile Java sources of `{}` due to previous errors", self.name);
            }
        }

        for dir in &self.resources {
            if dir.is_dir() {
                copy_dir(dir, &self.output)?;
            }
        }
        let after = output_files(&self.output);
        let added: Vec<&str> = after.iter().filter(|f| !before.contains(*f)).map(String::as_str).collect();
        kargo_util::fs::write_string_atomic(&produced, &added.join("\n"))?;
        check.record()?;
        Ok(Outcome::Compiled)
    }

    /// Recompiles only what changed, when the daemon and the Build Tools API are available.
    /// Multiplatform sources (`expect`/`actual` fragments) always compile from scratch.
    fn compile_incrementally(&self, shell: &Shell, jdk: &Jdk, compiler: &KotlinCompiler, fingerprint_dir: &Path) -> Result<IcOutcome> {
        if !compiler.build_tools || !self.sources.fragment_args().is_empty() {
            return Ok(IcOutcome::Unavailable);
        }
        let options = self.kotlin_options(jdk);
        let mut config = Fingerprint::new();
        config.str(&compiler.version).str(&slash(&self.output));
        // The classpath reaches the compiler as snapshots; everything else forces a clean build.
        let mut skip_next = false;
        for o in &options {
            if std::mem::take(&mut skip_next) {
                continue;
            }
            skip_next = o == "-classpath";
            if !skip_next {
                config.str(o);
            }
        }
        for p in self.plugins {
            config.file_stamp(&p.jar);
        }
        let unit = IncrementalUnit {
            state_dir: fingerprint_dir.join(format!("{}.ic", self.name)),
            output: &self.output,
            sources: self.sources.all().cloned().collect(),
            classpath: &self.classpath,
            options,
            config: config.finish(),
            compiler_version: &compiler.version,
        };
        unit.compile(shell, jdk, &compiler.classpath)
    }
}

/// Files under `dir`, relative to it with `/` separators.
fn output_files(dir: &Path) -> std::collections::HashSet<String> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.path().strip_prefix(dir).ok().map(slash))
        .collect()
}

fn all_files(dir: &Path) -> Vec<PathBuf> {
    if !dir.is_dir() {
        return Vec::new();
    }
    let mut files: Vec<PathBuf> =
        walkdir::WalkDir::new(dir).into_iter().filter_map(|e| e.ok()).filter(|e| e.file_type().is_file()).map(|e| e.into_path()).collect();
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn javac_release_values() {
        assert_eq!(javac_release("1.8"), "8");
        assert_eq!(javac_release("21"), "21");
    }

    #[test]
    fn argument_construction() {
        let jdk = Jdk { home: PathBuf::from("/jdk"), version: 21 };
        let plugins = [ResolvedPlugin { jar: "ser.jar".into(), options: vec![] }];
        let unit = JvmCompilation {
            name: "app",
            sources: SourceSet::new(vec!["src/Main.kt".into(), "src/Util.java".into()], vec![], "jvm"),
            classpath: vec!["a.jar".into()],
            friend_paths: vec!["main-classes".into()],
            output: "out".into(),
            jvm_target: "17",
            plugins: &plugins,
            kotlin_args: &["-Xjsr305=strict".to_owned()],
            javac_args: &["-parameters".to_owned()],
            processor_path: &[PathBuf::from("lombok.jar")],
            resources: vec![],
        };
        let k = unit.kotlin_args(&jdk);
        assert!(k.windows(2).any(|w| w == ["-module-name", "app"]));
        assert!(k.windows(2).any(|w| w == ["-jvm-target", "17"]));
        assert!(k.contains(&"-Xfriend-paths=main-classes".to_owned()));
        assert!(k.contains(&"-Xplugin=ser.jar".to_owned()));
        assert!(k.contains(&"src/Util.java".to_owned()), "Kotlin sees Java sources");
        let j = unit.javac_args();
        assert!(j.windows(2).any(|w| w == ["--release", "17"]));
        assert!(j.contains(&"-processorpath".to_owned()));
        assert!(j.contains(&"-parameters".to_owned()));
        assert_eq!(j.last().unwrap(), "src/Util.java");
    }
}
