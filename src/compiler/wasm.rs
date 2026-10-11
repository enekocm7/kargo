//! Kotlin/Wasm compilation: sources -> klib -> `.mjs` + `.wasm`.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{FreshnessCheck, Outcome, ResolvedPlugin, SourceSet, fingerprint, plugin_args, run_kotlin_compiler};
use crate::manifest::WasmEnvironment;
use crate::maven::Version;
use crate::shell::Shell;
use crate::toolchain::kotlin::JS_COMPILER_MAIN;
use crate::toolchain::{Jdk, KotlinCompiler};
use crate::util::fs::{join_paths, remove_dir_all_if_exists};
use crate::util::process::slash;

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

    pub fn compile(
        &self,
        shell: &Shell,
        jdk: &Jdk,
        compiler: &KotlinCompiler,
        fingerprint_dir: &Path,
        announce: &dyn Fn(),
    ) -> Result<Outcome> {
        if self.sources.kotlin_files().is_empty() {
            return Ok(Outcome::Empty);
        }
        let version = compiler.version.as_str();
        let (main, _) = compiler_main(version);

        let mut klib_args = self.base_args(version);
        klib_args.extend([
            "-ir-output-dir".to_owned(),
            slash(&self.output.join("klib")),
            "-ir-output-name".to_owned(),
            self.name.to_owned(),
        ]);
        if needs_klib_flag(version) {
            klib_args.push("-Xir-produce-klib-file".to_owned());
        }
        klib_args.extend(plugin_args(self.plugins));
        klib_args.extend(self.sources.fragment_args());
        klib_args.extend(self.kotlin_args.iter().cloned());
        klib_args.extend(self.sources.kotlin_files().iter().map(|p| slash(p)));

        let mut link_args = self.base_args(version);
        link_args.extend([
            "-ir-output-dir".to_owned(),
            slash(&self.output),
            "-ir-output-name".to_owned(),
            self.name.to_owned(),
            "-Xir-produce-js".to_owned(),
            format!("-Xinclude={}", slash(&self.klib_path())),
        ]);
        if self.debug {
            link_args.push("-Xwasm-debug-info".to_owned());
        }

        let mut all = klib_args.clone();
        all.extend(link_args.iter().cloned());
        all.push(version.to_owned());
        let mut inputs = self.libraries.clone();
        inputs.extend(self.friends.iter().cloned());
        let fp = fingerprint(&self.sources, &inputs, self.plugins, &all);
        let check = FreshnessCheck::new(fingerprint_dir.join(format!("{}.fingerprint", self.name)), &fp);
        if check.is_fresh() && self.module_path().is_file() {
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
        run_kotlin_compiler(
            shell,
            jdk,
            &compiler.classpath,
            main,
            &link_args,
            &fingerprint_dir.join(format!("{}.link.args", self.name)),
            self.name,
        )?;
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
        assert_eq!(args[3], crate::util::fs::join_paths(&["std.klib".into(), "main.klib".into()]));
        assert_eq!(args[4], "-Xfriend-modules=main.klib");
        assert!(unit.klib_path().ends_with("klib/app.klib") || unit.klib_path().ends_with("klib\\app.klib"));
    }
}
