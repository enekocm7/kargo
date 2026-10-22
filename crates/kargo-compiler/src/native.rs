//! Kotlin/Native compilation with `konanc`, C library bindings with `cinterop`, and the
//! project's own C sources compiled with the toolchain's clang into a static library.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use super::{FreshnessCheck, Outcome, ResolvedPlugin, SourceSet, fingerprint, plugin_args};
use kargo_manifest::{CFlags, Cinterop, NativeKind, NativeOptions, is_apple_target};
use kargo_toolchain::NativeToolchain;
use kargo_toolchain::native::{cache_enabled, executable_extension};
use kargo_util::hash::Fingerprint;
use kargo_util::process::{self, slash};
use kargo_util::shell::Shell;

/// What a native binary links besides klibs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NativeLink {
    /// Passed to the linker one by one (`-linker-option`).
    pub linker_options: Vec<String>,
    /// Static libraries built from the project's C sources: linked into binaries, packed into klibs.
    pub archives: Vec<PathBuf>,
}

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
    pub link: NativeLink,
    pub output_dir: PathBuf,
    pub debug: bool,
    pub optimize: bool,
    /// Build a test runner executable instead of the program.
    pub test_runner: bool,
    /// Klibs among `libraries` whose API a framework or C library exports too (`-Xexport-library`).
    pub exported: Vec<PathBuf>,
    /// The `CFBundleIdentifier` of a framework.
    pub bundle_id: Option<String>,
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
            NativeKind::Framework => "framework",
        }
    }

    /// The main output file (a directory for a framework).
    pub fn artifact(&self) -> PathBuf {
        let base = self.output_dir.join(self.name);
        let windows = self.konan_target == "mingw_x64";
        let apple = is_apple_target(&self.konan_target);
        if self.test_runner {
            return base.with_extension(executable_extension(&self.konan_target));
        }
        match self.kind {
            NativeKind::Executable => base.with_extension(executable_extension(&self.konan_target)),
            NativeKind::Library => base.with_extension("klib"),
            NativeKind::Shared if windows => base.with_extension("dll"),
            NativeKind::Shared if apple => self.output_dir.join(format!("lib{}.dylib", self.name)),
            NativeKind::Shared => self.output_dir.join(format!("lib{}.so", self.name)),
            NativeKind::Static => self.output_dir.join(format!("lib{}.a", self.name)),
            NativeKind::Framework => self.output_dir.join(format!("{}.framework", self.name)),
        }
    }

    /// The C header the compiler writes next to a shared or static library (`lib<name>_api.h`).
    pub fn api_header(&self) -> Option<PathBuf> {
        if self.test_runner || !matches!(self.kind, NativeKind::Shared | NativeKind::Static) {
            return None;
        }
        let artifact = self.artifact();
        let stem = artifact.file_stem()?.to_string_lossy().into_owned();
        Some(self.output_dir.join(format!("{stem}_api.h")))
    }

    /// Where [`NativeCompilation::api_header`] is copied for C code to include: `include/` of the output directory.
    pub fn include_dir(&self) -> PathBuf {
        self.output_dir.join("include")
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
        if !self.test_runner && matches!(self.kind, NativeKind::Framework | NativeKind::Shared | NativeKind::Static) {
            for lib in &self.exported {
                args.push(format!("-Xexport-library={}", slash(lib)));
            }
        }
        if let (Some(id), NativeKind::Framework, false) = (&self.bundle_id, self.kind, self.test_runner) {
            args.push(format!("-Xbinary=bundleId={id}"));
        }
        for opt in &self.link.linker_options {
            args.push("-linker-option".to_owned());
            args.push(opt.clone());
        }
        // A klib carries the archive to whatever links it; a binary links it now.
        let pack = self.produce() == "library";
        for archive in &self.link.archives {
            args.push(if pack { "-include-binary" } else { "-linker-option" }.to_owned());
            args.push(slash(archive));
        }
        args.extend(plugin_args(self.plugins));
        args.extend(self.sources.fragment_args());
        args.extend(self.kotlin_args.iter().cloned());
        args.extend(self.sources.kotlin_files().iter().map(|p| slash(p)));
        args
    }

    /// Whether this compilation can link a precompiled standard library: a klib doesn't link
    /// it, and `-opt` optimizes the whole program, which the compiler can't do with precompiled code.
    fn can_use_cache(&self) -> bool {
        !self.optimize && self.produce() != "library"
    }

    /// Options that link the precompiled standard library instead of lowering it again.
    /// They don't change the output, so they stay out of the fingerprint.
    fn cache_args(&self, toolchain: &NativeToolchain) -> Vec<String> {
        if self.can_use_cache() && cache_enabled() { toolchain.cache_args() } else { Vec::new() }
    }

    pub fn compile(&self, shell: &Shell, toolchain: &NativeToolchain, fingerprint_dir: &Path, announce: &dyn Fn()) -> Result<Outcome> {
        if self.sources.kotlin_files().is_empty() {
            return Ok(Outcome::Empty);
        }
        let mut args = self.args();
        let mut all = args.clone();
        all.push(toolchain.version.clone());
        let inputs: Vec<PathBuf> = self.libraries.iter().chain(&self.link.archives).cloned().collect();
        let fp = fingerprint(&self.sources, &inputs, self.plugins, &all);
        let tag = if self.test_runner { "test" } else { "main" };
        let check = FreshnessCheck::new(fingerprint_dir.join(format!("{}-{}-{tag}.fingerprint", self.name, self.konan_target)), &fp);
        if check.is_fresh() && self.artifact().exists() {
            self.copy_api_header()?;
            return Ok(Outcome::Fresh);
        }
        check.invalidate();
        announce();
        std::fs::create_dir_all(&self.output_dir)?;

        let cache = self.cache_args(toolchain);
        if !cache.is_empty() && !toolchain.stdlib_cache_dir(&self.konan_target, self.debug).is_dir() {
            shell.note(format!(
                "building the Kotlin/Native standard library cache for {} (one time per Kotlin version, takes a few minutes); later builds are much faster",
                self.konan_target
            ));
        }
        args.splice(0..0, cache);

        let argfile = fingerprint_dir.join(format!("{}-{}-{tag}.konanc.args", self.name, self.konan_target));
        process::write_argfile(&argfile, &args)?;
        let mut cmd = toolchain.command();
        cmd.arg(format!("@{}", argfile.display()));
        let mut lines = Vec::new();
        let status = crate::diagnostics::status_observed(shell, &mut cmd, "the Kotlin/Native compiler", &mut |l| lines.push(l.to_owned()))?;
        if !status.success() {
            if let Some(hint) = link_hint(&lines) {
                shell.note(hint);
            }
            bail!("could not compile `{}` for {} due to previous errors", self.name, self.konan_target);
        }
        self.copy_api_header()?;
        check.record()?;
        Ok(Outcome::Compiled)
    }

    /// Copies the generated C header of a shared or static library into `include/`.
    fn copy_api_header(&self) -> Result<()> {
        let Some(header) = self.api_header().filter(|h| h.is_file()) else { return Ok(()) };
        let dir = self.include_dir();
        std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
        let to = dir.join(header.file_name().expect("a header has a file name"));
        std::fs::copy(&header, &to).with_context(|| format!("failed to copy {} to {}", header.display(), to.display()))?;
        Ok(())
    }
}

/// Explains a failed link from the linker's messages: the symbols or libraries it did not find.
pub fn link_hint(lines: &[String]) -> Option<String> {
    let quoted = |line: &str, start: &str| {
        let rest = &line[line.find(start)? + start.len()..];
        let name: String = rest.trim().trim_start_matches(['`', '\'']).chars().take_while(|c| !matches!(c, '\'' | '`' | ' ')).collect();
        (!name.is_empty()).then_some(name)
    };
    let mut symbols = Vec::new();
    for line in lines {
        let found = quoted(line, "undefined symbol:").or_else(|| quoted(line, "undefined reference to"));
        if let Some(symbol) = found
            && !symbols.contains(&symbol)
        {
            symbols.push(symbol);
        }
        if let Some(lib) = quoted(line, "unable to find library -l").or_else(|| quoted(line, "cannot find -l")) {
            return Some(format!(
                "the linker did not find the C library `{lib}`: install it, or add the directory holding it to \
                 `library-paths` (or `-L<dir>` in `linker-options`) of the [target.native.cinterop.<name>] entry that links it"
            ));
        }
    }
    if symbols.is_empty() {
        return None;
    }
    let shown: Vec<String> = symbols.iter().take(3).map(|s| format!("`{s}`")).collect();
    Some(format!(
        "the linker did not find {}{}: link the C library that defines {} with `pkg-config`, `linker-options = [\"-l<library>\"]` \
         or `static-libraries` in its [target.native.cinterop.<name>] entry (or [target.native.platform.<platform>]), \
         or add its sources to [target.native.c-sources]",
        shown.join(", "),
        if symbols.len() > 3 { format!(" and {} more", symbols.len() - 3) } else { String::new() },
        if symbols.len() == 1 { "it" } else { "them" }
    ))
}

/// Explains a failed `cinterop` run from clang's messages: a header it did not find.
pub fn header_hint(lines: &[String], interop: &str) -> Option<String> {
    for line in lines {
        let Some(at) = line.find("fatal error: '") else { continue };
        let rest = &line[at + "fatal error: '".len()..];
        if let Some(end) = rest.find("' file not found") {
            let header = &rest[..end];
            return Some(format!(
                "the C header `{header}` was not found: add the directory that holds it to `compiler-options` (`-I<dir>`) of \
                 [target.native.cinterop.{interop}] or of its `platform.<platform>` table, name the library's package in \
                 `pkg-config`, or install the library's development files"
            ));
        }
    }
    None
}

/// The linker options a `cinterop` klib records for every program that links it: `-L` for the
/// library paths, the linker options, and on Apple targets `-framework` for the frameworks.
pub fn cinterop_linker_options(options: &NativeOptions, konan_target: &str) -> Vec<String> {
    let mut opts: Vec<String> = options.library_paths.iter().map(|dir| format!("-L{}", slash(dir))).collect();
    opts.extend(options.linker_options.iter().cloned());
    if is_apple_target(konan_target) {
        for framework in &options.frameworks {
            opts.push("-framework".to_owned());
            opts.push(framework.clone());
        }
    }
    opts
}

/// The text of the `.def` file Kargo writes for a `cinterop` entry with `.def` properties or linker
/// options in kargo.toml: those properties, then the project's own `.def` file (whose values win,
/// being read last). The linker options go into `linkerOpts`, after the project's own if it sets
/// them: `cinterop` records only the `.def` file's `linkerOpts` in the klib, not `-linker-option`.
pub fn def_file_contents(interop: &Cinterop, linker_options: &[String], project_def: Option<&str>) -> String {
    let mut out = format!("# Written by Kargo from [target.native.cinterop.{}] in kargo.toml.\n", interop.name);
    for (key, value) in interop.def_properties() {
        out.push_str(&format!("{key} = {value}\n"));
    }
    let linker_opts = def_list(linker_options);
    let merged = project_def.filter(|_| !linker_opts.is_empty()).and_then(|def| append_to_property(def, "linkerOpts", &linker_opts));
    if merged.is_none() && !linker_opts.is_empty() {
        out.push_str(&format!("linkerOpts = {linker_opts}\n"));
    }
    if let Some(def) = merged.as_deref().or(project_def) {
        out.push_str(def);
        if !def.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

/// A `.def` list value: the arguments separated by spaces, quoted when they hold whitespace, with
/// backslashes doubled for the properties syntax.
fn def_list(args: &[String]) -> String {
    let quoted: Vec<String> = args
        .iter()
        .map(|arg| {
            let arg = arg.replace('\\', "\\\\");
            if arg.chars().any(char::is_whitespace) { format!("\"{arg}\"") } else { arg }
        })
        .collect();
    quoted.join(" ")
}

/// `def` with `value` appended to the last `key` property before its `---` line, or `None` when
/// it sets no `key`. A property spans the lines ending in an odd number of backslashes.
fn append_to_property(def: &str, key: &str, value: &str) -> Option<String> {
    let lines: Vec<&str> = def.split_inclusive('\n').collect();
    let continues = |line: &str| line.trim_end_matches(['\n', '\r']).chars().rev().take_while(|&c| c == '\\').count() % 2 == 1;
    let mut found = None;
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim_end_matches(['\n', '\r']) == "---" {
            break;
        }
        let line = lines[i].trim_start();
        let mut end = i;
        if !line.starts_with(['#', '!']) {
            while end + 1 < lines.len() && continues(lines[end]) {
                end += 1;
            }
            if line.split(|c: char| c == '=' || c == ':' || c.is_whitespace()).next() == Some(key) {
                found = Some(end);
            }
        }
        i = end + 1;
    }
    let end = found?;
    let last = lines[end];
    let body = last.trim_end_matches(['\n', '\r']);
    Some(format!("{}{body} {value}{}{}", lines[..end].concat(), &last[body.len()..], lines[end + 1..].concat()))
}

/// One `cinterop` run: a C library bound to a klib for one Konan target.
#[derive(Debug, Clone)]
pub struct CinteropUnit<'a> {
    pub interop: &'a Cinterop,
    pub konan_target: String,
    /// The project root; relative paths (`def`, `-I` options) resolve against it.
    pub root: &'a Path,
    /// The `.def` file, or `None` when only `headers` or `modules` describe the library.
    pub def: Option<PathBuf>,
    /// The options for this platform: kargo.toml's, then pkg-config's and the C sources' include directories.
    pub options: NativeOptions,
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
        for opt in &self.options.compiler_options {
            args.push("-compiler-option".to_owned());
            args.push(opt.clone());
        }
        // Where `-staticLibrary` looks. The linker options are the `.def` file's `linkerOpts`
        // (see `def_file_contents`), the only ones `cinterop` records in the klib.
        for dir in &self.options.library_paths {
            args.push("-libraryPath".to_owned());
            args.push(slash(dir));
        }
        // Embedded in the klib, so programs run without the library installed.
        for lib in &self.options.static_libraries {
            args.push("-staticLibrary".to_owned());
            args.push(lib.clone());
        }
        args
    }

    /// Runs `cinterop` unless the klib is up to date. Changes to the `.def` file, the options,
    /// the static libraries and the files under project-relative `-I` directories trigger a rerun.
    pub fn generate(&self, shell: &Shell, toolchain: &NativeToolchain, fingerprint_dir: &Path, announce: &dyn Fn()) -> Result<Outcome> {
        let args = self.args();
        let mut fp = Fingerprint::new();
        fp.str(env!("CARGO_PKG_VERSION")).str(&toolchain.version);
        for a in &args {
            fp.str(a);
        }
        if let Some(def) = &self.def {
            fp.file_stamp(def);
        }
        for dir in self.options.compiler_options.iter().filter_map(|o| o.strip_prefix("-I")) {
            let dir = Path::new(dir);
            if dir.is_relative() {
                fp.file_stamp(&self.root.join(dir));
            }
        }
        for lib in &self.options.static_libraries {
            if let Some(file) = self.options.library_paths.iter().map(|d| self.root.join(d).join(lib)).find(|f| f.is_file()) {
                fp.file_stamp(&file);
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
        let mut lines = Vec::new();
        let status =
            crate::diagnostics::status_observed(shell, &mut cmd, "the Kotlin/Native cinterop tool", &mut |l| lines.push(l.to_owned()))?;
        if !status.success() {
            if let Some(hint) = header_hint(&lines, &self.interop.name) {
                shell.note(hint);
            }
            bail!("could not generate the `{}` C bindings for {} due to previous errors", self.interop.name, self.konan_target);
        }
        if !self.artifact().is_file() {
            bail!("cinterop did not produce {}", self.artifact().display());
        }
        check.record()?;
        Ok(Outcome::Compiled)
    }
}

/// The project's C, C++ and Objective-C sources for one Konan target, compiled with the clang of
/// the Kotlin/Native toolchain (with that target's sysroot) into one static library.
#[derive(Debug, Clone)]
pub struct CSourcesUnit<'a> {
    /// Names the library: `lib<name>.a`.
    pub name: &'a str,
    pub konan_target: String,
    /// The project root; object files are named after the sources' paths relative to it.
    pub root: &'a Path,
    pub files: Vec<PathBuf>,
    pub include_dirs: Vec<PathBuf>,
    pub flags: CFlags,
    pub debug: bool,
    pub optimize: bool,
    pub output_dir: PathBuf,
}

impl CSourcesUnit<'_> {
    /// The static library.
    pub fn artifact(&self) -> PathBuf {
        self.output_dir.join(format!("lib{}.a", self.name))
    }

    /// The object file of `source`.
    fn object(&self, source: &Path) -> PathBuf {
        let relative = source.strip_prefix(self.root).unwrap_or(source);
        let name = relative.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
        self.output_dir.join("obj").join(format!("{}.o", name.trim_start_matches('/')))
    }

    /// The clang driver for `source` (`clang` or `clang++`) and its options.
    pub fn clang_args(&self, source: &Path) -> (&'static str, Vec<String>) {
        let ext = source.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let tool = if matches!(ext.as_str(), "cc" | "cpp" | "cxx" | "c++" | "mm") { "clang++" } else { "clang" };
        let mut args = vec!["-c".to_owned()];
        args.push(if self.optimize { "-O2" } else { "-O0" }.to_owned());
        if self.debug {
            args.push("-g".to_owned());
        }
        // Shared libraries and frameworks link the archive too.
        if self.konan_target != "mingw_x64" {
            args.push("-fPIC".to_owned());
        }
        for dir in &self.include_dirs {
            args.push(format!("-I{}", slash(dir)));
        }
        for define in &self.flags.defines {
            args.push(format!("-D{define}"));
        }
        args.extend(self.flags.compiler_options.iter().cloned());
        args.push(slash(source));
        args.push("-o".to_owned());
        args.push(slash(&self.object(source)));
        (tool, args)
    }

    /// Compiles the sources and archives them unless the library is up to date. Changes to the
    /// sources, the files under the include directories or the flags trigger a rebuild.
    ///
    /// clang comes with the target's toolchain dependencies, so the caller downloads them first
    /// ([`NativeToolchain::ensure_dependencies`]).
    pub fn compile(&self, shell: &Shell, toolchain: &NativeToolchain, fingerprint_dir: &Path, announce: &dyn Fn()) -> Result<Outcome> {
        if self.files.is_empty() {
            return Ok(Outcome::Empty);
        }
        let mut fp = Fingerprint::new();
        fp.str(env!("CARGO_PKG_VERSION")).str(&toolchain.version).str(&self.konan_target);
        for file in &self.files {
            let (tool, args) = self.clang_args(file);
            fp.str(tool);
            for a in &args {
                fp.str(a);
            }
            fp.file_stamp(file);
        }
        for dir in &self.include_dirs {
            fp.file_stamp(dir);
        }
        let check = FreshnessCheck::new(fingerprint_dir.join(format!("c-{}-{}.fingerprint", self.name, self.konan_target)), &fp);
        if check.is_fresh() && self.artifact().is_file() {
            return Ok(Outcome::Fresh);
        }
        check.invalidate();
        announce();
        std::fs::create_dir_all(fingerprint_dir)?;
        kargo_util::fs::remove_dir_all_if_exists(&self.output_dir.join("obj"))?;

        let mut objects = Vec::new();
        for file in &self.files {
            let (tool, args) = self.clang_args(file);
            let object = self.object(file);
            if let Some(dir) = object.parent() {
                std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
            }
            // A response file keeps `=` and spaces intact through the `run_konan` script.
            let rsp = object.with_extension("o.rsp");
            write_response_file(&rsp, &args)?;
            let mut cmd = toolchain.run_konan_command("clang");
            cmd.args([tool, &self.konan_target]).arg(format!("@{}", slash(&rsp))).current_dir(self.root).stdout(Stdio::null());
            if !crate::diagnostics::status(shell, &mut cmd, "clang")?.success() {
                bail!("could not compile the C sources of `{}` for {} due to previous errors", self.name, self.konan_target);
            }
            objects.push(object);
        }

        let archive = self.artifact();
        if archive.exists() {
            std::fs::remove_file(&archive).with_context(|| format!("failed to remove {}", archive.display()))?;
        }
        let rsp = self.output_dir.join("obj").join("objects.rsp");
        write_response_file(&rsp, &objects.iter().map(|o| slash(o)).collect::<Vec<_>>())?;
        let mut cmd = toolchain.run_konan_command("llvm");
        cmd.args(["llvm-ar", "rcs", &slash(&archive)]).arg(format!("@{}", slash(&rsp))).stdout(Stdio::null());
        archive_with(shell, &mut cmd)?;
        if !archive.is_file() {
            bail!("llvm-ar did not produce {}", archive.display());
        }
        check.record()?;
        Ok(Outcome::Compiled)
    }
}

fn archive_with(shell: &Shell, cmd: &mut Command) -> Result<()> {
    let status = crate::diagnostics::status(shell, cmd, "llvm-ar")?;
    if !status.success() {
        bail!("could not archive the C objects ({})", process::describe_status(status));
    }
    Ok(())
}

/// Writes `args` to a response file for clang and llvm-ar, one per line. Arguments with spaces or
/// quotes are quoted; Windows hosts read backslashes literally, others as escapes.
fn write_response_file(path: &Path, args: &[String]) -> Result<()> {
    let mut text = String::new();
    for arg in args {
        let arg = if cfg!(windows) { arg.clone() } else { arg.replace('\\', "\\\\") };
        if arg.is_empty() || arg.contains([' ', '\t', '"', '\'']) {
            text.push('"');
            text.push_str(&arg.replace('"', "\\\""));
            text.push('"');
        } else {
            text.push_str(&arg);
        }
        text.push('\n');
    }
    std::fs::write(path, text).with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn interop() -> Cinterop {
        Cinterop {
            name: "sqlite".into(),
            def: None,
            package: Some("sqlite3".into()),
            headers: vec!["sqlite3.h".into()],
            options: NativeOptions::default(),
            platform: BTreeMap::new(),
            header_filter: Vec::new(),
            excluded_functions: Vec::new(),
            strict_enums: Vec::new(),
            non_strict_enums: Vec::new(),
            modules: Vec::new(),
            language: None,
            pkg_config: Vec::new(),
            link: Default::default(),
        }
    }

    #[test]
    fn cinterop_args() {
        let interop = interop();
        let options = NativeOptions {
            compiler_options: vec!["-Iinclude".into()],
            linker_options: vec!["-lsqlite3".into()],
            static_libraries: vec!["libz.a".into()],
            library_paths: vec![PathBuf::from("/opt/lib")],
            frameworks: vec!["Security".into()],
        };
        let unit = CinteropUnit {
            interop: &interop,
            konan_target: "linux_x64".into(),
            root: Path::new("."),
            def: Some(PathBuf::from("interop/sqlite.def")),
            options,
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
        assert!(args.windows(2).any(|w| w == ["-libraryPath", "/opt/lib"]));
        assert!(args.windows(2).any(|w| w == ["-staticLibrary", "libz.a"]));
        // cinterop does not record `-linker-option` in the klib; the `.def` file carries them.
        assert!(!args.contains(&"-linker-option".to_owned()));
        let headers_only = CinteropUnit { def: None, ..unit };
        assert!(!headers_only.args().contains(&"-def".to_owned()));
    }

    #[test]
    fn cinterop_linker_options_include_library_paths_and_frameworks() {
        let options = NativeOptions {
            linker_options: vec!["-lsqlite3".into()],
            library_paths: vec![PathBuf::from("/opt/lib")],
            frameworks: vec!["Security".into()],
            ..Default::default()
        };
        assert_eq!(cinterop_linker_options(&options, "linux_x64"), ["-L/opt/lib", "-lsqlite3"]);
        // Frameworks only exist on Apple platforms.
        assert_eq!(cinterop_linker_options(&options, "macos_arm64"), ["-L/opt/lib", "-lsqlite3", "-framework", "Security"]);
    }

    #[test]
    fn generated_def_puts_the_project_def_last() {
        let mut interop = interop();
        interop.header_filter = vec!["sqlite3*.h".into()];
        interop.strict_enums = vec!["A".into(), "B".into()];
        let text = def_file_contents(&interop, &[], Some("headers = sqlite3.h\n---\nstatic int x = 1;"));
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with("# Written by Kargo"));
        assert_eq!(lines[1..], ["headerFilter = sqlite3*.h", "strictEnums = A B", "headers = sqlite3.h", "---", "static int x = 1;"]);
        assert!(def_file_contents(&interop, &[], None).ends_with("strictEnums = A B\n"));
    }

    #[test]
    fn generated_def_records_the_linker_options() {
        let interop = interop();
        let opts = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let linker = opts(&["-LC:\\raylib lib", "-lgdi32", "-lwinmm"]);
        let text = def_file_contents(&interop, &linker, None);
        assert!(text.ends_with("\nlinkerOpts = \"-LC:\\\\raylib lib\" -lgdi32 -lwinmm\n"), "{text}");
        // Before the project's own properties.
        let text = def_file_contents(&interop, &opts(&["-lm"]), Some("headers = x.h\n---\nint linkerOpts = 1;\n"));
        assert!(text.ends_with("\nlinkerOpts = -lm\nheaders = x.h\n---\nint linkerOpts = 1;\n"), "{text}");
        // Appended to the project's `linkerOpts`, which would otherwise replace them, even over several lines.
        let project = "linkerOpts.mingw = -lopengl32\nlinkerOpts = -lx \\\n  -ly\nheaders = x.h\n";
        let text = def_file_contents(&interop, &opts(&["-lm"]), Some(project));
        assert!(text.ends_with("\nlinkerOpts.mingw = -lopengl32\nlinkerOpts = -lx \\\n  -ly -lm\nheaders = x.h\n"), "{text}");
        let text = def_file_contents(&interop, &opts(&["-lm"]), Some("# linkerOpts = -lold\nlinkerOpts:-lx"));
        assert!(text.ends_with("\n# linkerOpts = -lold\nlinkerOpts:-lx -lm\n"), "{text}");
        assert!(!def_file_contents(&interop, &[], None).contains("linkerOpts"));
    }

    #[test]
    fn hints_for_missing_headers_and_symbols() {
        let lines = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let hint = header_hint(
            &lines(&["Exception in thread \"main\" java.lang.Error: x.h:1:10: fatal error: 'sqlite3.h' file not found"]),
            "sqlite",
        )
        .unwrap();
        assert!(hint.contains("`sqlite3.h` was not found"), "{hint}");
        assert!(hint.contains("[target.native.cinterop.sqlite]"), "{hint}");
        assert!(header_hint(&lines(&["error: something else"]), "sqlite").is_none());

        let hint = link_hint(&lines(&["ld.lld: error: undefined symbol: sqlite3_open", ">>> referenced by x.o"])).unwrap();
        assert!(hint.contains("did not find `sqlite3_open`"), "{hint}");
        let hint = link_hint(&lines(&["/usr/bin/ld: x.o: undefined reference to `curl_easy_init'"])).unwrap();
        assert!(hint.contains("`curl_easy_init`"), "{hint}");
        let hint = link_hint(&lines(&["ld.lld: error: unable to find library -lSDL2"])).unwrap();
        assert!(hint.contains("C library `SDL2`"), "{hint}");
        assert!(link_hint(&lines(&["error: unresolved reference 'x'"])).is_none());
    }

    #[test]
    fn c_sources_args_and_objects() {
        let unit = CSourcesUnit {
            name: "app_c",
            konan_target: "linux_x64".into(),
            root: Path::new("/p"),
            files: vec![PathBuf::from("/p/c/a.c"), PathBuf::from("/p/c/sub/b.cpp")],
            include_dirs: vec![PathBuf::from("/p/c")],
            flags: CFlags { defines: vec!["LEVEL=2".into()], compiler_options: vec!["-Wall".into()] },
            debug: true,
            optimize: false,
            output_dir: PathBuf::from("/p/target/native/c"),
        };
        assert_eq!(unit.artifact(), Path::new("/p/target/native/c/libapp_c.a"));
        let (tool, args) = unit.clang_args(Path::new("/p/c/a.c"));
        assert_eq!(tool, "clang");
        assert_eq!(args[..3], ["-c", "-O0", "-g"]);
        assert!(args.contains(&"-fPIC".to_owned()));
        assert!(args.contains(&"-I/p/c".to_owned()));
        assert!(args.contains(&"-DLEVEL=2".to_owned()));
        assert!(args.contains(&"-Wall".to_owned()));
        assert!(args.ends_with(&["/p/c/a.c".to_owned(), "-o".to_owned(), "/p/target/native/c/obj/c/a.c.o".to_owned()]));
        let (tool, args) = unit.clang_args(Path::new("/p/c/sub/b.cpp"));
        assert_eq!(tool, "clang++");
        assert_eq!(args.last().unwrap(), "/p/target/native/c/obj/c/sub/b.cpp.o");
        let windows = CSourcesUnit { konan_target: "mingw_x64".into(), optimize: true, debug: false, ..unit };
        let (_, args) = windows.clang_args(Path::new("/p/c/a.c"));
        assert_eq!(args[1], "-O2");
        assert!(!args.contains(&"-fPIC".to_owned()) && !args.contains(&"-g".to_owned()));
    }

    #[test]
    fn response_files_quote_spaces() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rsp");
        write_response_file(&path, &["-DNAME=\"x y\"".into(), "-O2".into()]).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "\"-DNAME=\\\"x y\\\"\"\n-O2\n");
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
            link: NativeLink::default(),
            output_dir: PathBuf::from("out"),
            debug: true,
            optimize: false,
            test_runner: false,
            exported: Vec::new(),
            bundle_id: None,
        }
    }

    #[test]
    fn artifact_names() {
        assert!(unit(NativeKind::Executable, "mingw_x64").artifact().ends_with("app.exe"));
        assert!(unit(NativeKind::Executable, "linux_x64").artifact().ends_with("app.kexe"));
        assert!(unit(NativeKind::Library, "linux_x64").artifact().ends_with("app.klib"));
        assert!(unit(NativeKind::Shared, "mingw_x64").artifact().ends_with("app.dll"));
        assert!(unit(NativeKind::Shared, "macos_arm64").artifact().ends_with("libapp.dylib"));
        assert!(unit(NativeKind::Shared, "ios_arm64").artifact().ends_with("libapp.dylib"));
        assert!(unit(NativeKind::Static, "linux_x64").artifact().ends_with("libapp.a"));
        assert!(unit(NativeKind::Framework, "ios_arm64").artifact().ends_with("app.framework"));
    }

    #[test]
    fn api_headers() {
        assert_eq!(unit(NativeKind::Shared, "linux_x64").api_header(), Some(PathBuf::from("out/libapp_api.h")));
        assert_eq!(unit(NativeKind::Shared, "mingw_x64").api_header(), Some(PathBuf::from("out/app_api.h")));
        assert_eq!(unit(NativeKind::Static, "macos_arm64").api_header(), Some(PathBuf::from("out/libapp_api.h")));
        assert_eq!(unit(NativeKind::Executable, "linux_x64").api_header(), None);
        assert_eq!(unit(NativeKind::Framework, "ios_arm64").api_header(), None);
        assert_eq!(unit(NativeKind::Shared, "linux_x64").include_dir(), PathBuf::from("out/include"));
    }

    #[test]
    fn stdlib_cache_is_for_unoptimized_binaries() {
        for kind in [NativeKind::Executable, NativeKind::Shared, NativeKind::Static, NativeKind::Framework] {
            assert!(unit(kind, "linux_x64").can_use_cache(), "{kind:?}");
        }
        assert!(!unit(NativeKind::Library, "linux_x64").can_use_cache(), "klibs don't link the standard library");
        let mut optimized = unit(NativeKind::Executable, "linux_x64");
        optimized.optimize = true;
        assert!(!optimized.can_use_cache(), "-opt can't use precompiled code");
        // The test runner links a program whatever `kind` says.
        let mut tests = unit(NativeKind::Library, "linux_x64");
        tests.test_runner = true;
        assert!(tests.can_use_cache());
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

    #[test]
    fn archives_frameworks_and_exports() {
        let link = NativeLink { linker_options: vec!["-lm".into()], archives: vec![PathBuf::from("c/libapp_c.a")] };
        let exe = NativeCompilation { link: link.clone(), ..unit(NativeKind::Executable, "linux_x64") };
        let args = exe.args();
        assert!(args.windows(2).any(|w| w == ["-linker-option", "-lm"]));
        assert!(args.windows(2).any(|w| w == ["-linker-option", "c/libapp_c.a"]));
        let klib = NativeCompilation { link, ..unit(NativeKind::Library, "linux_x64") };
        assert!(klib.args().windows(2).any(|w| w == ["-include-binary", "c/libapp_c.a"]));

        let framework = NativeCompilation {
            exported: vec![PathBuf::from("dep.klib")],
            bundle_id: Some("com.example.App".into()),
            ..unit(NativeKind::Framework, "ios_arm64")
        };
        let args = framework.args();
        assert!(args.windows(2).any(|w| w == ["-produce", "framework"]));
        assert!(args.contains(&"-Xexport-library=dep.klib".to_owned()));
        assert!(args.contains(&"-Xbinary=bundleId=com.example.App".to_owned()));
        // An executable exports nothing.
        let exe = NativeCompilation { exported: vec![PathBuf::from("dep.klib")], ..unit(NativeKind::Executable, "linux_x64") };
        assert!(!exe.args().iter().any(|a| a.starts_with("-Xexport-library")));
    }
}
