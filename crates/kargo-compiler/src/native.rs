//! Kotlin/Native compilation with `konanc`, and C library bindings with `cinterop`.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use super::{FreshnessCheck, Outcome, ResolvedPlugin, SourceSet, fingerprint, plugin_args};
use kargo_manifest::{Cinterop, NativeKind};
use kargo_toolchain::NativeToolchain;
use kargo_toolchain::native::executable_extension;
use kargo_util::process::{self, slash};
use kargo_util::shell::Shell;

#[derive(Debug, Clone)]
pub struct NativeCompilation<'a> {
    pub name: &'a str,
    pub konan_target: String,
    pub kind: NativeKind,
    pub sources: SourceSet,
    pub libraries: Vec<PathBuf>,
    pub plugins: &'a [ResolvedPlugin],
    pub kotlin_args: &'a [String],
    pub entry_point: Option<String>,
    pub linker_options: &'a [String],
    pub output_dir: PathBuf,
    pub debug: bool,
    pub optimize: bool,
    /// Build a test runner executable instead of the program.
    pub test_runner: bool,
}

impl NativeCompilation<'_> {
    fn produce(&self) -> &'static str {
        if self.test_runner {
            return "program";
        }
        match self.kind {
            NativeKind::Executable => "program",
            NativeKind::Library => "library",
            NativeKind::Shared => "dynamic",
            NativeKind::Static => "static",
        }
    }

    /// The main output file.
    pub fn artifact(&self) -> PathBuf {
        let base = self.output_dir.join(self.name);
        let windows = self.konan_target == "mingw_x64";
        let apple = self.konan_target.starts_with("macos") || self.konan_target.starts_with("ios");
        if self.test_runner {
            return base.with_extension(executable_extension(&self.konan_target));
        }
        match self.kind {
            NativeKind::Executable => base.with_extension(executable_extension(&self.konan_target)),
            NativeKind::Library => base.with_extension("klib"),
            NativeKind::Shared if windows => base.with_extension("dll"),
            NativeKind::Shared if apple => self.output_dir.join(format!("lib{}.dylib", self.name)),
            NativeKind::Shared => self.output_dir.join(format!("lib{}.so", self.name)),
            NativeKind::Static if windows => self.output_dir.join(format!("lib{}.a", self.name)),
            NativeKind::Static => self.output_dir.join(format!("lib{}.a", self.name)),
        }
    }

    pub fn args(&self) -> Vec<String> {
        let mut args = vec![
            "-target".to_owned(),
            self.konan_target.clone(),
            "-produce".to_owned(),
            self.produce().to_owned(),
            "-output".to_owned(),
            slash(&self.output_dir.join(self.name)),
            "-module-name".to_owned(),
            self.name.to_owned(),
        ];
        if self.test_runner {
            args.push("-generate-test-runner".to_owned());
        } else if let (Some(entry), NativeKind::Executable) = (&self.entry_point, self.kind) {
            args.push("-entry".to_owned());
            args.push(entry.clone());
        }
        if self.debug {
            args.push("-g".to_owned());
        }
        if self.optimize {
            args.push("-opt".to_owned());
        }
        for lib in &self.libraries {
            args.push("-library".to_owned());
            args.push(slash(lib));
        }
        for opt in self.linker_options {
            args.push("-linker-option".to_owned());
            args.push(opt.clone());
        }
        args.extend(plugin_args(self.plugins));
        args.extend(self.sources.fragment_args());
        args.extend(self.kotlin_args.iter().cloned());
        args.extend(self.sources.kotlin_files().iter().map(|p| slash(p)));
        args
    }

    pub fn compile(&self, shell: &Shell, toolchain: &NativeToolchain, fingerprint_dir: &Path, announce: &dyn Fn()) -> Result<Outcome> {
        if self.sources.kotlin_files().is_empty() {
            return Ok(Outcome::Empty);
        }
        let args = self.args();
        let mut all = args.clone();
        all.push(toolchain.version.clone());
        let fp = fingerprint(&self.sources, &self.libraries, self.plugins, &all);
        let tag = if self.test_runner { "test" } else { "main" };
        let check = FreshnessCheck::new(fingerprint_dir.join(format!("{}-{}-{tag}.fingerprint", self.name, self.konan_target)), &fp);
        if check.is_fresh() && self.artifact().is_file() {
            return Ok(Outcome::Fresh);
        }
        check.invalidate();
        announce();
        std::fs::create_dir_all(&self.output_dir)?;

        let argfile = fingerprint_dir.join(format!("{}-{}-{tag}.konanc.args", self.name, self.konan_target));
        process::write_argfile(&argfile, &args)?;
        let mut cmd = toolchain.command();
        cmd.arg(format!("@{}", argfile.display()));
        let status = process::status(shell, &mut cmd, "the Kotlin/Native compiler")?;
        if !status.success() {
            bail!("could not compile `{}` for {} due to previous errors", self.name, self.konan_target);
        }
        check.record()?;
        Ok(Outcome::Compiled)
    }
}

/// One `cinterop` run: a C library bound to a klib for one Konan target.
#[derive(Debug, Clone)]
pub struct CinteropUnit<'a> {
    pub interop: &'a Cinterop,
    pub konan_target: String,
    /// The project root; relative paths (`def`, `-I` options) resolve against it.
    pub root: &'a Path,
    /// The `.def` file, or `None` when only `headers` describe the library.
    pub def: Option<PathBuf>,
    pub output_dir: PathBuf,
}

impl CinteropUnit<'_> {
    /// The klib holding the bindings.
    pub fn artifact(&self) -> PathBuf {
        self.output_dir.join(format!("{}.klib", self.interop.name))
    }

    pub fn args(&self) -> Vec<String> {
        let mut args = vec!["-target".to_owned(), self.konan_target.clone(), "-o".to_owned(), slash(&self.artifact())];
        if let Some(def) = &self.def {
            args.push("-def".to_owned());
            args.push(slash(def));
        }
        if let Some(pkg) = &self.interop.package {
            args.push("-pkg".to_owned());
            args.push(pkg.clone());
        }
        for header in &self.interop.headers {
            args.push("-header".to_owned());
            args.push(header.clone());
        }
        for opt in &self.interop.compiler_options {
            args.push("-compiler-option".to_owned());
            args.push(opt.clone());
        }
        // Recorded in the klib, so every program linking it gets them.
        for opt in &self.interop.linker_options {
            args.push("-linker-option".to_owned());
            args.push(opt.clone());
        }
        args
    }

    /// Runs `cinterop` unless the klib is up to date. Changes to the `.def` file, the
    /// options and the files under project-relative `-I` directories trigger a rerun.
    pub fn generate(&self, shell: &Shell, toolchain: &NativeToolchain, fingerprint_dir: &Path, announce: &dyn Fn()) -> Result<Outcome> {
        let args = self.args();
        let mut fp = kargo_util::hash::Fingerprint::new();
        fp.str(env!("CARGO_PKG_VERSION")).str(&toolchain.version);
        for a in &args {
            fp.str(a);
        }
        if let Some(def) = &self.def {
            fp.file_stamp(def);
        }
        for dir in self.interop.compiler_options.iter().filter_map(|o| o.strip_prefix("-I")) {
            let dir = Path::new(dir);
            if dir.is_relative() {
                fp.file_stamp(&self.root.join(dir));
            }
        }
        let check =
            FreshnessCheck::new(fingerprint_dir.join(format!("cinterop-{}-{}.fingerprint", self.interop.name, self.konan_target)), &fp);
        if check.is_fresh() && self.artifact().is_file() {
            return Ok(Outcome::Fresh);
        }
        check.invalidate();
        announce();
        std::fs::create_dir_all(&self.output_dir)?;
        std::fs::create_dir_all(fingerprint_dir)?;
        let mut cmd = toolchain.cinterop_command();
        cmd.args(&args).current_dir(self.root);
        let status = process::status(shell, &mut cmd, "the Kotlin/Native cinterop tool")?;
        if !status.success() {
            bail!("could not generate the `{}` C bindings for {} due to previous errors", self.interop.name, self.konan_target);
        }
        if !self.artifact().is_file() {
            bail!("cinterop did not produce {}", self.artifact().display());
        }
        check.record()?;
        Ok(Outcome::Compiled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cinterop_args() {
        let interop = Cinterop {
            name: "sqlite".into(),
            def: None,
            package: Some("sqlite3".into()),
            headers: vec!["sqlite3.h".into()],
            compiler_options: vec!["-Iinclude".into()],
            linker_options: vec!["-lsqlite3".into()],
        };
        let unit = CinteropUnit {
            interop: &interop,
            konan_target: "linux_x64".into(),
            root: Path::new("."),
            def: Some(PathBuf::from("interop/sqlite.def")),
            output_dir: PathBuf::from("out"),
        };
        assert!(unit.artifact().ends_with("sqlite.klib"));
        let args = unit.args();
        assert!(args.windows(2).any(|w| w == ["-target", "linux_x64"]));
        assert!(args.windows(2).any(|w| w == ["-o", "out/sqlite.klib"]));
        assert!(args.windows(2).any(|w| w == ["-def", "interop/sqlite.def"]));
        assert!(args.windows(2).any(|w| w == ["-pkg", "sqlite3"]));
        assert!(args.windows(2).any(|w| w == ["-header", "sqlite3.h"]));
        assert!(args.windows(2).any(|w| w == ["-compiler-option", "-Iinclude"]));
        assert!(args.windows(2).any(|w| w == ["-linker-option", "-lsqlite3"]));
        let headers_only = CinteropUnit { def: None, ..unit };
        assert!(!headers_only.args().contains(&"-def".to_owned()));
    }

    fn unit(kind: NativeKind, target: &str) -> NativeCompilation<'static> {
        NativeCompilation {
            name: "app",
            konan_target: target.into(),
            kind,
            sources: SourceSet::new(vec!["a.kt".into()], vec![], "native"),
            libraries: vec!["dep.klib".into()],
            plugins: &[],
            kotlin_args: &[],
            entry_point: Some("com.x.main".into()),
            linker_options: &[],
            output_dir: PathBuf::from("out"),
            debug: true,
            optimize: false,
            test_runner: false,
        }
    }

    #[test]
    fn artifact_names() {
        assert!(unit(NativeKind::Executable, "mingw_x64").artifact().ends_with("app.exe"));
        assert!(unit(NativeKind::Executable, "linux_x64").artifact().ends_with("app.kexe"));
        assert!(unit(NativeKind::Library, "linux_x64").artifact().ends_with("app.klib"));
        assert!(unit(NativeKind::Shared, "mingw_x64").artifact().ends_with("app.dll"));
        assert!(unit(NativeKind::Shared, "macos_arm64").artifact().ends_with("libapp.dylib"));
        assert!(unit(NativeKind::Static, "linux_x64").artifact().ends_with("libapp.a"));
    }

    #[test]
    fn args_layout() {
        let u = unit(NativeKind::Executable, "linux_x64");
        let args = u.args();
        assert!(args.windows(2).any(|w| w == ["-produce", "program"]));
        assert!(args.windows(2).any(|w| w == ["-entry", "com.x.main"]));
        assert!(args.windows(2).any(|w| w == ["-library", "dep.klib"]));
        assert!(args.contains(&"-g".to_owned()));
        let mut t = unit(NativeKind::Library, "linux_x64");
        t.test_runner = true;
        let args = t.args();
        assert!(args.contains(&"-generate-test-runner".to_owned()));
        assert!(!args.contains(&"-entry".to_owned()));
        assert!(args.windows(2).any(|w| w == ["-produce", "program"]));
    }
}
