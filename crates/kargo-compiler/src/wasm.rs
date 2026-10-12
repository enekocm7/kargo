//! Kotlin/Wasm compilation: sources -> klib -> `.mjs` + `.wasm`.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::mainclass::{EntryKind, find_entry_points};
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
    /// Remove code nothing reaches (`-Xir-dce`), which makes the module a lot smaller.
    pub optimize: bool,
    /// Write TypeScript declarations (`<name>.d.mts`) for the `@JsExport` functions.
    pub declarations: bool,
    /// Pass the program arguments to `main(args)`, which Kotlin/Wasm itself
    /// always calls with an empty array. Only for the program being built,
    /// not for libraries or tests.
    pub main_args: bool,
}

/// How the program's `main` gets its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MainArgs {
    /// Nothing to do: no `main`, or one without parameters.
    None,
    /// Compile this generated source along with the program.
    Wrapper(String),
    /// `main(args)` is in the default package, where no wrapper can take precedence.
    DefaultPackage,
}

/// Escapes a package name for use in Kotlin source, e.g. `` `com`.`example` ``.
fn quoted_package(package: &str) -> String {
    package.split('.').map(|p| format!("`{p}`")).collect::<Vec<_>>().join(".")
}

/// A `main()` in the default package that reads the program arguments and
/// calls `<package>.main(args)`. The Kotlin/Wasm compiler starts the `main`
/// of the alphabetically first package, so this one runs instead of the program's.
fn main_wrapper(environment: WasmEnvironment, package: &str, suspend: bool) -> String {
    let prelude = match environment {
        // Node.js; elsewhere (a browser) there are no arguments.
        WasmEnvironment::Js | WasmEnvironment::Browser => {
            r#"@file:Suppress("OPT_IN_USAGE")

private fun kargoArgv(): JsArray<JsString> =
    js("(typeof process !== 'undefined' && Array.isArray(process.argv)) ? process.argv.slice(2) : []")

private fun kargoArgs(): Array<String> {
    val argv = kargoArgv()
    return Array(argv.length) { argv[it].toString() }
}
"#
        }
        // WASI `args_get`; argv[0] is the program, followed by the `.mjs` launcher under Node.js.
        WasmEnvironment::Wasi => {
            r#"@file:OptIn(kotlin.wasm.unsafe.UnsafeWasmMemoryApi::class, kotlin.wasm.ExperimentalWasmInterop::class)

import kotlin.wasm.WasmImport
import kotlin.wasm.unsafe.Pointer
import kotlin.wasm.unsafe.withScopedMemoryAllocator

@WasmImport("wasi_snapshot_preview1", "args_sizes_get")
private external fun kargoArgsSizesGet(argc: Int, bufSize: Int): Int

@WasmImport("wasi_snapshot_preview1", "args_get")
private external fun kargoArgsGet(argv: Int, buf: Int): Int

private fun kargoArgs(): Array<String> = withScopedMemoryAllocator { allocator ->
    val sizes = allocator.allocate(8)
    if (kargoArgsSizesGet(sizes.address.toInt(), (sizes + 4).address.toInt()) != 0) return@withScopedMemoryAllocator emptyArray()
    val argc = sizes.loadInt()
    val argv = allocator.allocate(maxOf(argc, 1) * 4)
    val buf = allocator.allocate(maxOf((sizes + 4).loadInt(), 1))
    if (kargoArgsGet(argv.address.toInt(), buf.address.toInt()) != 0) return@withScopedMemoryAllocator emptyArray()
    val all = List(argc) { i ->
        var p = Pointer((argv + i * 4).loadInt().toUInt())
        val bytes = ArrayList<Byte>()
        while (p.loadByte() != 0.toByte()) {
            bytes.add(p.loadByte())
            p += 1
        }
        bytes.toByteArray().decodeToString()
    }
    val skip = if (all.size >= 2 && all[1].endsWith(".mjs")) 2 else 1
    all.drop(skip).toTypedArray()
}
"#
        }
    };
    let modifier = if suspend { "suspend " } else { "" };
    format!(
        "// Generated by Kargo: Kotlin/Wasm calls `main` without arguments, so this `main` passes them on.\n\n{prelude}\n{modifier}fun main() {{\n    {}.main(kargoArgs())\n}}\n",
        quoted_package(package)
    )
}

impl WasmCompilation<'_> {
    fn base_args(&self, version: &str) -> Vec<String> {
        let (_, legacy) = compiler_main(version);
        let mut args = Vec::new();
        if legacy {
            args.push("-Xwasm".to_owned());
        }
        args.push(format!("-Xwasm-target=wasm-{}", self.environment.platform()));
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

    /// What the program's `main` needs to receive its arguments.
    fn main_args(&self) -> MainArgs {
        // A page has no command line.
        if !self.main_args || self.environment == WasmEnvironment::Browser {
            return MainArgs::None;
        }
        // The compiler starts the `main` of the alphabetically first package.
        let entry = find_entry_points(&self.sources.kotlin_files())
            .into_iter()
            .filter(|e| e.kind == EntryKind::TopLevel)
            .min_by(|a, b| a.package.cmp(&b.package));
        match entry {
            Some(e) if e.takes_args && e.package.is_empty() => MainArgs::DefaultPackage,
            Some(e) if e.takes_args => MainArgs::Wrapper(main_wrapper(self.environment, &e.package, e.suspend)),
            _ => MainArgs::None,
        }
    }

    pub fn klib_path(&self) -> PathBuf {
        self.output.join("klib").join(format!("{}.klib", self.name))
    }

    pub fn module_path(&self) -> PathBuf {
        self.output.join(format!("{}.mjs", self.name))
    }

    /// The TypeScript declarations written next to the module when `declarations` is set.
    pub fn declarations_path(&self) -> PathBuf {
        self.output.join(format!("{}.d.mts", self.name))
    }

    /// Arguments of the first step: sources (and `wrapper`) -> `klib/<name>.klib`.
    fn klib_args(&self, version: &str, wrapper: Option<&Path>) -> Vec<String> {
        let mut args = self.base_args(version);
        args.extend(["-ir-output-dir".to_owned(), slash(&self.output.join("klib")), "-ir-output-name".to_owned(), self.name.to_owned()]);
        if needs_klib_flag(version) {
            args.push("-Xir-produce-klib-file".to_owned());
        }
        args.extend(plugin_args(self.plugins));
        args.extend(self.sources.fragment_args());
        args.extend(self.kotlin_args.iter().cloned());
        args.extend(self.sources.kotlin_files().iter().map(|p| slash(p)));
        args.extend(wrapper.map(slash));
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
        let main_args = if link { self.main_args() } else { MainArgs::None };
        let wrapper_path = fingerprint_dir.join(format!("{}.main.kt", self.name));
        let wrapper = match &main_args {
            MainArgs::Wrapper(source) => Some(source),
            _ => None,
        };
        let klib_args = self.klib_args(version, wrapper.map(|_| wrapper_path.as_path()));
        let link_args = if link { self.link_args(version) } else { Vec::new() };

        let mut all = klib_args.clone();
        all.extend(link_args.iter().cloned());
        all.extend(wrapper.cloned());
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
        if main_args == MainArgs::DefaultPackage {
            shell.warn(
                "Kotlin/Wasm passes no arguments to a `main` in the default package; \
                 declare a `package` in its file for `main(args)` to receive them",
            );
        }
        remove_dir_all_if_exists(&self.output)?;
        std::fs::create_dir_all(&self.output)?;
        if let Some(source) = wrapper {
            kargo_util::fs::write_string_atomic(&wrapper_path, source)?;
        }

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

/// The npm packages the given klibs need at runtime, as `name -> version`.
///
/// Kotlin/JS and Kotlin/Wasm libraries declare them (`npm("@js-joda/core", ...)`
/// in Gradle) in a `package.json` at the root of the klib. When two libraries
/// ask for different versions of a package, the highest one wins.
pub fn npm_dependencies(libraries: &[PathBuf]) -> Result<BTreeMap<String, String>> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for klib in libraries {
        let Some(json) = read_package_json(klib).with_context(|| format!("failed to read {}", klib.display()))? else {
            continue;
        };
        let Ok(package) = serde_json::from_str::<serde_json::Value>(&json) else {
            continue;
        };
        let Some(deps) = package.get("dependencies").and_then(|d| d.as_object()) else {
            continue;
        };
        for (name, version) in deps {
            let Some(version) = version.as_str() else { continue };
            match out.get(name) {
                Some(existing) if npm_version(existing) >= npm_version(version) => {}
                _ => {
                    out.insert(name.clone(), version.to_owned());
                }
            }
        }
    }
    Ok(out)
}

/// Compares npm versions and simple ranges (`3.2.0`, `^3.2.0`, `~3.2`) by their lower bound.
fn npm_version(spec: &str) -> Version {
    Version::parse(spec.trim().trim_start_matches(['^', '~', '=', '>', 'v']).trim())
}

/// Reads `package.json` from a klib, which is a zip file or an unpacked directory.
fn read_package_json(klib: &Path) -> Result<Option<String>> {
    if klib.is_dir() {
        let path = klib.join("package.json");
        return Ok(path.is_file().then(|| std::fs::read_to_string(&path)).transpose()?);
    }
    if !klib.extension().is_some_and(|e| e == "klib") {
        return Ok(None);
    }
    let mut archive = zip::ZipArchive::new(File::open(klib)?)?;
    let Ok(mut entry) = archive.by_name("package.json") else {
        return Ok(None);
    };
    let mut json = String::new();
    entry.read_to_string(&mut json)?;
    Ok(Some(json))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn klib_with_package_json(dir: &Path, name: &str, json: &str) -> PathBuf {
        use std::io::Write;
        let path = dir.join(format!("{name}.klib"));
        let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
        zip.start_file("default/manifest", zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(b"unique_name=x").unwrap();
        if !json.is_empty() {
            zip.start_file("package.json", zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(json.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    #[test]
    fn reads_npm_dependencies_from_klibs() {
        let dir = tempfile::tempdir().unwrap();
        let a = klib_with_package_json(dir.path(), "a", r#"{"name":"a","dependencies":{"@js-joda/core":"3.2.0","left-pad":"^1.1.0"}}"#);
        let b = klib_with_package_json(
            dir.path(),
            "b",
            r#"{"name":"b","dependencies":{"@js-joda/core":"3.10.1"},"devDependencies":{"x":"1"}}"#,
        );
        let c = klib_with_package_json(dir.path(), "c", "");
        let unpacked = dir.path().join("d");
        std::fs::create_dir(&unpacked).unwrap();
        std::fs::write(unpacked.join("package.json"), r#"{"dependencies":{"left-pad":"1.0.0"}}"#).unwrap();
        let jar = dir.path().join("e.jar");
        std::fs::write(&jar, "not a zip").unwrap();

        let deps = npm_dependencies(&[a, b, c, unpacked, jar]).unwrap();
        let deps: Vec<_> = deps.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        assert_eq!(deps, [("@js-joda/core", "3.10.1"), ("left-pad", "^1.1.0")]);
    }

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
            optimize: false,
            declarations: false,
            main_args: false,
        };
        let args = unit.base_args("2.3.0");
        assert_eq!(args[0], "-Xwasm");
        assert_eq!(args[1], "-Xwasm-target=wasm-wasi");
        assert_eq!(args[3], kargo_util::fs::join_paths(&["std.klib".into(), "main.klib".into()]));
        assert_eq!(args[4], "-Xfriend-modules=main.klib");
        assert!(unit.klib_path().ends_with("klib/app.klib") || unit.klib_path().ends_with("klib\\app.klib"));
        let klib = unit.klib_args("2.4.21", None);
        assert!(klib.contains(&"a.kt".to_owned()));
        assert_eq!(unit.klib_args("2.4.21", Some(Path::new("w.kt"))).last().unwrap(), "w.kt");
        assert!(!klib.contains(&"-Xir-produce-js".to_owned()), "the klib step does not link");
        let link = unit.link_args("2.4.21");
        assert!(link.contains(&"-Xir-produce-js".to_owned()));
        assert!(link.contains(&"-Xwasm-debug-info".to_owned()));
        assert!(!link.contains(&"-Xir-dce".to_owned()) && !link.contains(&"-Xgenerate-dts".to_owned()));
    }

    #[test]
    fn release_and_library_link_flags() {
        let unit = WasmCompilation {
            name: "lib",
            environment: WasmEnvironment::Browser,
            sources: SourceSet::new(vec![], vec!["a.kt".into()], "wasm"),
            libraries: vec![],
            friends: vec![],
            plugins: &[],
            kotlin_args: &[],
            output: "out".into(),
            debug: false,
            optimize: true,
            declarations: true,
            main_args: false,
        };
        // A browser runs the same Kotlin/Wasm target as Node.js.
        assert!(unit.base_args("2.4.21").contains(&"-Xwasm-target=wasm-js".to_owned()));
        let link = unit.link_args("2.4.21");
        assert!(link.contains(&"-Xir-dce".to_owned()), "release links drop dead code: {link:?}");
        assert!(link.contains(&"-Xgenerate-dts".to_owned()));
        assert!(!link.contains(&"-Xwasm-debug-info".to_owned()));
        // Only the link step removes code and writes declarations.
        let klib = unit.klib_args("2.4.21", None);
        assert!(!klib.contains(&"-Xir-dce".to_owned()) && !klib.contains(&"-Xgenerate-dts".to_owned()));
        assert!(unit.declarations_path().ends_with("out/lib.d.mts") || unit.declarations_path().ends_with("out\\lib.d.mts"));
    }

    fn program(dir: &Path, files: &[(&str, &str)], environment: WasmEnvironment, main_args: bool) -> MainArgs {
        let files: Vec<PathBuf> = files
            .iter()
            .map(|(name, src)| {
                let path = dir.join(name);
                std::fs::write(&path, src).unwrap();
                path
            })
            .collect();
        let unit = WasmCompilation {
            name: "app",
            environment,
            sources: SourceSet::new(vec![], files, "wasm"),
            libraries: vec![],
            friends: vec![],
            plugins: &[],
            kotlin_args: &[],
            output: dir.join("out"),
            debug: false,
            optimize: false,
            declarations: false,
            main_args,
        };
        unit.main_args()
    }

    #[test]
    fn main_wrapper_passes_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        let with_args = ("Main.kt", "package com.example.app\n\nfun main(args: Array<String>) {}\n");
        let MainArgs::Wrapper(js) = program(dir, &[with_args], WasmEnvironment::Js, true) else { panic!("expected a wrapper") };
        assert!(js.contains("process.argv.slice(2)"), "{js}");
        assert!(js.contains("\nfun main() {\n    `com`.`example`.`app`.main(kargoArgs())\n}\n"), "{js}");
        assert!(!js.contains("package "), "the wrapper must stay in the default package:\n{js}");
        let MainArgs::Wrapper(wasi) = program(dir, &[with_args], WasmEnvironment::Wasi, true) else { panic!("expected a wrapper") };
        assert!(wasi.contains("\"args_get\""), "{wasi}");

        let suspend = ("Main.kt", "package p\nsuspend fun main(args: Array<String>) {}\n");
        let MainArgs::Wrapper(s) = program(dir, &[suspend], WasmEnvironment::Js, true) else { panic!("expected a wrapper") };
        assert!(s.contains("suspend fun main() {\n    `p`.main(kargoArgs())"), "{s}");

        // The compiler starts the `main` of the alphabetically first package.
        let other = ("Other.kt", "package a.first\nfun main() {}\n");
        assert_eq!(program(dir, &[with_args, other], WasmEnvironment::Js, true), MainArgs::None);

        assert_eq!(program(dir, &[with_args], WasmEnvironment::Js, false), MainArgs::None);
        // A page has no command line, so `args` stays empty without a wrapper.
        assert_eq!(program(dir, &[with_args], WasmEnvironment::Browser, true), MainArgs::None);
        assert_eq!(program(dir, &[("Main.kt", "package p\nfun main() {}\n")], WasmEnvironment::Js, true), MainArgs::None);
        assert_eq!(program(dir, &[("Lib.kt", "package p\nfun f() {}\n")], WasmEnvironment::Js, true), MainArgs::None);
        assert_eq!(program(dir, &[("Main.kt", "fun main(args: Array<String>) {}\n")], WasmEnvironment::Js, true), MainArgs::DefaultPackage);
    }
}
