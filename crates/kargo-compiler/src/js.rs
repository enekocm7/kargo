//! Kotlin/JS compilation with the IR backend: sources -> klib -> a JavaScript module (`.mjs` or `.js`).
//!
//! The compiler empties the directory it links into, so the module is linked into a work directory
//! and copied from there into the output directory, which also holds the npm packages the module
//! imports, the page of a browser app and the `package.json` of a library.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::{FreshnessCheck, Outcome, ResolvedPlugin, SourceSet, fingerprint, plugin_args, run_kotlin_compiler};
use kargo_manifest::{JsEnvironment, JsModuleKind};
use kargo_maven::Version;
use kargo_toolchain::kotlin::JS_COMPILER_MAIN;
use kargo_toolchain::{Jdk, KotlinCompiler};
use kargo_util::fs::{join_paths, remove_dir_all_if_exists};
use kargo_util::process::{file_url, slash};
use kargo_util::shell::Shell;

/// Lists the files the last link copied into the output directory, so a file it no longer writes goes away.
const COMPILED_MARKER: &str = ".kargo-compiled";

/// The JavaScript expression whose array Node.js programs get as `main(args)`.
const NODE_ARGS: &str = "process.argv.slice(2)";

/// Before Kotlin 2.4.20 producing a klib had to be requested explicitly.
fn needs_klib_flag(version: &str) -> bool {
    Version::parse(version) < Version::parse("2.4.20")
}

#[derive(Debug, Clone)]
pub struct JsCompilation<'a> {
    pub name: &'a str,
    pub environment: JsEnvironment,
    pub module_kind: JsModuleKind,
    pub sources: SourceSet,
    /// Dependency klibs.
    pub libraries: Vec<PathBuf>,
    /// Klibs whose `internal` declarations are visible (tests -> main).
    pub friends: Vec<PathBuf>,
    pub plugins: &'a [ResolvedPlugin],
    pub kotlin_args: &'a [String],
    /// Directory receiving the linked `<name>.mjs` (or `.js`), its source map and declarations.
    pub output: PathBuf,
    /// Directory receiving `klib/<name>.klib` and the module before it is copied to `output`.
    pub work: PathBuf,
    /// The project directory: the source maps name the sources by their absolute `file://` URL from
    /// there, which still points at them once the module is copied out of `work`.
    pub root: PathBuf,
    /// Write source maps of the Kotlin sources.
    pub debug: bool,
    /// Remove code nothing reaches (`-Xir-dce`), which makes the module a lot smaller.
    pub optimize: bool,
    /// Write TypeScript declarations (`<name>.d.ts`) for the `@JsExport` declarations.
    pub declarations: bool,
    /// Pass the program arguments to `main(args)` on Node.js. Only for the program being built,
    /// not for libraries or tests.
    pub main_args: bool,
}

impl JsCompilation<'_> {
    /// The arguments of both steps. The module kind decides which `@JsModule` declarations type-check.
    fn base_args(&self) -> Vec<String> {
        let mut args = vec!["-module-kind".to_owned(), self.module_kind.as_str().to_owned()];
        let mut libraries = self.libraries.clone();
        libraries.extend(self.friends.iter().cloned());
        if !libraries.is_empty() {
            args.push("-libraries".to_owned());
            args.push(join_paths(&libraries));
        }
        if !self.friends.is_empty() {
            args.push(format!("-Xfriend-modules={}", join_paths(&self.friends)));
        }
        args
    }

    pub fn klib_path(&self) -> PathBuf {
        self.work.join("klib").join(format!("{}.klib", self.name))
    }

    fn link_dir(&self) -> PathBuf {
        self.work.join("link")
    }

    /// The linked module in `output`: `<name>.mjs` for an ES module, `<name>.js` otherwise.
    pub fn module_path(&self) -> PathBuf {
        self.output.join(format!("{}.{}", self.name, self.module_kind.extension()))
    }

    /// The TypeScript declarations written next to the module when `declarations` is set.
    pub fn declarations_path(&self) -> PathBuf {
        let extension = match self.module_kind {
            JsModuleKind::Es => "d.mts",
            JsModuleKind::CommonJs | JsModuleKind::Umd => "d.ts",
        };
        self.output.join(format!("{}.{extension}", self.name))
    }

    /// Arguments of the first step: sources -> `klib/<name>.klib`.
    fn klib_args(&self, version: &str) -> Vec<String> {
        let mut args = self.base_args();
        args.extend(["-ir-output-dir".to_owned(), slash(&self.work.join("klib")), "-ir-output-name".to_owned(), self.name.to_owned()]);
        if needs_klib_flag(version) {
            args.push("-Xir-produce-klib-file".to_owned());
        }
        args.extend(plugin_args(self.plugins));
        args.extend(self.sources.fragment_args());
        args.extend(self.kotlin_args.iter().cloned());
        args.extend(self.sources.kotlin_files().iter().map(|p| slash(p)));
        args
    }

    /// Arguments of the second step: klib -> the JavaScript module in the link directory.
    fn link_args(&self) -> Vec<String> {
        let mut args = self.base_args();
        args.extend([
            "-ir-output-dir".to_owned(),
            slash(&self.link_dir()),
            "-ir-output-name".to_owned(),
            self.name.to_owned(),
            "-Xir-produce-js".to_owned(),
            format!("-Xinclude={}", slash(&self.klib_path())),
        ]);
        if self.main_args && self.environment == JsEnvironment::Node {
            args.push(format!("-Xplatform-arguments-in-main-function={NODE_ARGS}"));
        }
        if self.debug {
            // The page of a browser app is served from the output directory, where the sources are out of reach.
            args.extend(["-source-map".to_owned(), "-source-map-embed-sources".to_owned(), "always".to_owned()]);
            let prefix = format!("{}/", file_url(&self.root).trim_end_matches('/'));
            args.extend(["-source-map-base-dirs".to_owned(), slash(&self.root), "-source-map-prefix".to_owned(), prefix]);
        }
        if self.optimize {
            args.push("-Xir-dce".to_owned());
        }
        if self.declarations {
            args.push("-Xgenerate-dts".to_owned());
        }
        args
    }

    pub fn compile(
        &self,
        shell: &Shell,
        jdk: &Jdk,
        compiler: &KotlinCompiler,
        fingerprint_dir: &Path,
        announce: &dyn Fn(),
    ) -> Result<Outcome> {
        self.run(shell, jdk, compiler, fingerprint_dir, announce, true)
    }

    /// Type-checks the sources: produces only the klib and skips linking the module.
    pub fn check(
        &self,
        shell: &Shell,
        jdk: &Jdk,
        compiler: &KotlinCompiler,
        fingerprint_dir: &Path,
        announce: &dyn Fn(),
    ) -> Result<Outcome> {
        self.run(shell, jdk, compiler, fingerprint_dir, announce, false)
    }

    fn run(
        &self,
        shell: &Shell,
        jdk: &Jdk,
        compiler: &KotlinCompiler,
        fingerprint_dir: &Path,
        announce: &dyn Fn(),
        link: bool,
    ) -> Result<Outcome> {
        if self.sources.kotlin_files().is_empty() {
            return Ok(Outcome::Empty);
        }
        let version = compiler.version.as_str();
        let klib_args = self.klib_args(version);
        let link_args = if link { self.link_args() } else { Vec::new() };

        let mut all = klib_args.clone();
        all.extend(link_args.iter().cloned());
        all.push(slash(&self.output));
        all.push(version.to_owned());
        let mut inputs = self.libraries.clone();
        inputs.extend(self.friends.iter().cloned());
        let fp = fingerprint(&self.sources, &inputs, self.plugins, &all);
        let check = FreshnessCheck::new(fingerprint_dir.join(format!("{}.fingerprint", self.name)), &fp);
        let output = if link { self.module_path() } else { self.klib_path() };
        if check.is_fresh() && output.is_file() {
            return Ok(Outcome::Fresh);
        }
        check.invalidate();
        announce();
        remove_dir_all_if_exists(&self.work)?;
        std::fs::create_dir_all(&self.work)?;
        std::fs::create_dir_all(fingerprint_dir)?;

        run_kotlin_compiler(
            shell,
            jdk,
            &compiler.classpath,
            JS_COMPILER_MAIN,
            &klib_args,
            &fingerprint_dir.join(format!("{}.klib.args", self.name)),
            self.name,
        )?;
        if link {
            run_kotlin_compiler(
                shell,
                jdk,
                &compiler.classpath,
                JS_COMPILER_MAIN,
                &link_args,
                &fingerprint_dir.join(format!("{}.link.args", self.name)),
                self.name,
            )?;
            copy_linked(&self.link_dir(), &self.output)?;
        }
        check.record()?;
        Ok(Outcome::Compiled)
    }
}

/// Copies what the compiler linked into `output`, after removing the files an earlier link copied there.
/// Everything else in `output` stays: the npm packages, the page, the files of `resources/`.
fn copy_linked(linked: &Path, output: &Path) -> Result<()> {
    let marker = output.join(COMPILED_MARKER);
    for old in std::fs::read_to_string(&marker).unwrap_or_default().lines() {
        let _ = std::fs::remove_file(output.join(old));
    }
    std::fs::create_dir_all(output).with_context(|| format!("failed to create {}", output.display()))?;
    let mut copied = Vec::new();
    for file in all_files(linked) {
        let relative = file.strip_prefix(linked).unwrap_or(&file);
        let dest = output.join(relative);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&file, &dest).with_context(|| format!("failed to copy {} to {}", file.display(), dest.display()))?;
        copied.push(slash(relative));
    }
    std::fs::write(&marker, copied.join("\n")).with_context(|| format!("failed to write {}", marker.display()))?;
    Ok(())
}

/// Every file below `dir`, sorted.
fn all_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(all_files(&path));
        } else {
            files.push(path);
        }
    }
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(environment: JsEnvironment, module_kind: JsModuleKind) -> JsCompilation<'static> {
        JsCompilation {
            name: "app",
            environment,
            module_kind,
            sources: SourceSet::new(vec!["a.kt".into()], vec![], "js"),
            libraries: vec!["std.klib".into()],
            friends: vec![],
            plugins: &[],
            kotlin_args: &[],
            output: "out".into(),
            work: "work".into(),
            root: "/p".into(),
            debug: true,
            optimize: false,
            declarations: false,
            main_args: true,
        }
    }

    #[test]
    fn klib_and_link_args() {
        let unit = unit(JsEnvironment::Node, JsModuleKind::Es);
        let klib = unit.klib_args("2.4.21");
        assert_eq!(klib[..4], ["-module-kind", "es", "-libraries", "std.klib"].map(str::to_owned));
        assert!(klib.contains(&"a.kt".to_owned()));
        assert!(!klib.contains(&"-Xir-produce-js".to_owned()), "the klib step does not link");
        assert!(!klib.contains(&"-Xir-produce-klib-file".to_owned()));
        assert!(unit.klib_args("2.4.10").contains(&"-Xir-produce-klib-file".to_owned()));

        let link = unit.link_args();
        assert!(link.contains(&"-Xir-produce-js".to_owned()));
        assert!(link.windows(2).any(|w| w == ["-module-kind", "es"]), "{link:?}");
        assert!(link.contains(&format!("-Xplatform-arguments-in-main-function={NODE_ARGS}")));
        assert!(link.contains(&"-source-map".to_owned()));
        assert!(link.windows(2).any(|w| w == ["-source-map-prefix", "file:///p/"]), "{link:?}");
        assert!(!link.contains(&"-Xir-dce".to_owned()) && !link.contains(&"-Xgenerate-dts".to_owned()));
        // The compiler empties the directory it links into, so that is not the output.
        assert!(link.windows(2).any(|w| w[0] == "-ir-output-dir" && w[1].starts_with("work")), "{link:?}");
        assert!(unit.module_path().ends_with("app.mjs"));
        assert!(unit.klib_path().starts_with("work"));
    }

    #[test]
    fn release_library_and_browser_args() {
        let mut unit = unit(JsEnvironment::Browser, JsModuleKind::Umd);
        unit.debug = false;
        unit.optimize = true;
        unit.declarations = true;
        let link = unit.link_args();
        assert!(link.contains(&"-Xir-dce".to_owned()), "release links drop dead code: {link:?}");
        assert!(link.contains(&"-Xgenerate-dts".to_owned()));
        assert!(link.windows(2).any(|w| w == ["-module-kind", "umd"]), "{link:?}");
        assert!(!link.contains(&"-source-map".to_owned()));
        assert!(!link.iter().any(|a| a.starts_with("-Xplatform-arguments")), "a page has no command line");
        assert!(unit.module_path().ends_with("app.js"));
        assert!(unit.declarations_path().ends_with("app.d.ts"));
        assert!(!unit.klib_args("2.4.21").contains(&"-Xir-dce".to_owned()), "only the link step removes code");
        assert!(JsCompilation { module_kind: JsModuleKind::Es, ..unit }.declarations_path().ends_with("app.d.mts"));
    }

    #[test]
    fn copies_the_linked_files_and_forgets_old_ones() {
        let dir = tempfile::tempdir().unwrap();
        let (linked, output) = (dir.path().join("link"), dir.path().join("out"));
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::create_dir_all(output.join("node_modules")).unwrap();
        std::fs::write(linked.join("app.mjs"), "1").unwrap();
        std::fs::write(linked.join("old.mjs"), "1").unwrap();
        copy_linked(&linked, &output).unwrap();
        std::fs::remove_file(linked.join("old.mjs")).unwrap();
        std::fs::write(linked.join("app.mjs"), "2").unwrap();
        copy_linked(&linked, &output).unwrap();
        assert_eq!(std::fs::read_to_string(output.join("app.mjs")).unwrap(), "2");
        assert!(!output.join("old.mjs").exists(), "a file the compiler no longer writes goes away");
        assert!(output.join("node_modules").is_dir(), "what Kargo put there stays");
    }
}
