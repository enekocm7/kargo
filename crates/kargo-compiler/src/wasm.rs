//! Kotlin/Wasm compilation: sources -> klib -> `.mjs` + `.wasm`.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{FreshnessCheck, Outcome, ResolvedPlugin, SourceSet, fingerprint, plugin_args, run_kotlin_compiler};
use kargo_manifest::WasmEnvironment;
use kargo_maven::Version;
use kargo_toolchain::kotlin::JS_COMPILER_MAIN;
use kargo_toolchain::{Jdk, KotlinCompiler};
use kargo_util::fs::{join_paths, remove_dir_all_if_exists};
use kargo_util::process::slash;
use kargo_util::shell::Shell;

const WASM_COMPILER_MAIN: &str = "org.jetbrains.kotlin.cli.js.KotlinWasmCompiler";

/// Kotlin 2.4 introduced a dedicated Wasm compiler entry point.
fn compiler_main(version: &str) -> (&'static str, bool) {
    if Version::parse(version) >= Version::parse("2.4.0") { (WASM_COMPILER_MAIN, false) } else { (JS_COMPILER_MAIN, true) }
}

/// Before Kotlin 2.4.20 producing a klib had to be requested explicitly.
fn needs_klib_flag(version: &str) -> bool {
    Version::parse(version) < Version::parse("2.4.20")
}

#[derive(Debug, Clone)]
pub struct WasmCompilation<'a> {
    pub name: &'a str,
    pub environment: WasmEnvironment,
    pub sources: SourceSet,
    /// Dependency klibs.
    pub libraries: Vec<PathBuf>,
    /// Klibs whose `internal` declarations are visible (tests -> main).
    pub friends: Vec<PathBuf>,
    pub plugins: &'a [ResolvedPlugin],
    pub kotlin_args: &'a [String],
    /// Directory receiving `<name>.klib` and the linked `<name>.mjs`/`.wasm`.
    pub output: PathBuf,
    pub debug: bool,
}

impl WasmCompilation<'_> {
    fn base_args(&self, version: &str) -> Vec<String> {
        let (_, legacy) = compiler_main(version);
        let mut args = Vec::new();
        if legacy {
            args.push("-Xwasm".to_owned());
        }
        let target = match self.environment {
            WasmEnvironment::Js => "wasm-js",
            WasmEnvironment::Wasi => "wasm-wasi",
        };
        args.push(format!("-Xwasm-target={target}"));
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
        self.output.join("klib").join(format!("{}.klib", self.name))
    }

    pub fn module_path(&self) -> PathBuf {
        self.output.join(format!("{}.mjs", self.name))
    }

    /// Arguments of the first step: sources -> `klib/<name>.klib`.
    fn klib_args(&self, version: &str) -> Vec<String> {
        let mut args = self.base_args(version);
        args.extend(["-ir-output-dir".to_owned(), slash(&self.output.join("klib")), "-ir-output-name".to_owned(), self.name.to_owned()]);
        if needs_klib_flag(version) {
            args.push("-Xir-produce-klib-file".to_owned());
        }
        args.extend(plugin_args(self.plugins));
        args.extend(self.sources.fragment_args());
        args.extend(self.kotlin_args.iter().cloned());
        args.extend(self.sources.kotlin_files().iter().map(|p| slash(p)));
        args
    }

    /// Arguments of the second step: klib -> `<name>.mjs` + `.wasm`.
    fn link_args(&self, version: &str) -> Vec<String> {
        let mut args = self.base_args(version);
        args.extend([
            "-ir-output-dir".to_owned(),
            slash(&self.output),
            "-ir-output-name".to_owned(),
            self.name.to_owned(),
            "-Xir-produce-js".to_owned(),
            format!("-Xinclude={}", slash(&self.klib_path())),
        ]);
        if self.debug {
            args.push("-Xwasm-debug-info".to_owned());
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

    /// Type-checks the sources: produces only the klib and skips linking the
    /// `.mjs`/`.wasm` module, the expensive Wasm backend step.
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
        let (main, _) = compiler_main(version);
        let klib_args = self.klib_args(version);
        let link_args = if link { self.link_args(version) } else { Vec::new() };

        let mut all = klib_args.clone();
        all.extend(link_args.iter().cloned());
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
        remove_dir_all_if_exists(&self.output)?;
        std::fs::create_dir_all(&self.output)?;

        run_kotlin_compiler(
            shell,
            jdk,
            &compiler.classpath,
            main,
            &klib_args,
            &fingerprint_dir.join(format!("{}.klib.args", self.name)),
            self.name,
        )?;
        if link {
            run_kotlin_compiler(
                shell,
                jdk,
                &compiler.classpath,
                main,
                &link_args,
                &fingerprint_dir.join(format!("{}.link.args", self.name)),
                self.name,
            )?;
        }
        check.record()?;
        Ok(Outcome::Compiled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiler_selection_by_version() {
        assert_eq!(compiler_main("2.4.21"), (WASM_COMPILER_MAIN, false));
        assert_eq!(compiler_main("2.3.0"), (JS_COMPILER_MAIN, true));
        assert!(needs_klib_flag("2.4.10"));
        assert!(!needs_klib_flag("2.4.20"));
    }

    #[test]
    fn args() {
        let unit = WasmCompilation {
            name: "app",
            environment: WasmEnvironment::Wasi,
            sources: SourceSet::new(vec!["a.kt".into()], vec![], "wasm"),
            libraries: vec!["std.klib".into()],
            friends: vec!["main.klib".into()],
            plugins: &[],
            kotlin_args: &[],
            output: "out".into(),
            debug: true,
        };
        let args = unit.base_args("2.3.0");
        assert_eq!(args[0], "-Xwasm");
        assert_eq!(args[1], "-Xwasm-target=wasm-wasi");
        assert_eq!(args[3], kargo_util::fs::join_paths(&["std.klib".into(), "main.klib".into()]));
        assert_eq!(args[4], "-Xfriend-modules=main.klib");
        assert!(unit.klib_path().ends_with("klib/app.klib") || unit.klib_path().ends_with("klib\\app.klib"));
        let klib = unit.klib_args("2.4.21");
        assert!(klib.contains(&"a.kt".to_owned()));
        assert!(!klib.contains(&"-Xir-produce-js".to_owned()), "the klib step does not link");
        let link = unit.link_args("2.4.21");
        assert!(link.contains(&"-Xir-produce-js".to_owned()));
        assert!(link.contains(&"-Xwasm-debug-info".to_owned()));
    }
}
