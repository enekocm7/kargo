//! C interop of the Native target beyond running `cinterop`: the options of each platform, C
//! libraries found with `pkg-config`, `link = "static"`, the `.def` properties written in kargo.toml,
//! the project's own C sources (`[target.native.c-sources]`) and the dependencies a framework exports.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ignore::gitignore::GitignoreBuilder;

use kargo_compiler::Outcome;
use kargo_compiler::native::{CSourcesUnit, NativeLink, cinterop_linker_options, def_file_contents};
use kargo_manifest::{CSources, Cinterop, LinkMode, NativeOptions, TargetKind, is_apple_target};
use kargo_toolchain::native::Host;
use kargo_util::process::slash;

use crate::ops::build::{compiling, native_resolution};
use crate::ops::context::{BuildContext, TargetSetup};
use crate::pkg_config;

/// Extensions of the files a directory in `[target.native.c-sources] sources` contributes.
pub const C_EXTENSIONS: &[&str] = &["c", "cc", "cpp", "cxx", "c++", "m", "mm"];

/// The Konan target of this machine, when Kotlin/Native supports it.
fn host_target() -> Option<&'static str> {
    Host::current().ok().map(|h| h.konan_target)
}

/// The options of a `cinterop` entry for `konan_target`: kargo.toml's with absolute library paths,
/// `-I` for the include directories of the C sources, the flags of its `pkg-config` packages (on
/// the host only), and with `link = "static"` the libraries turned into static ones.
pub fn cinterop_options(ctx: &BuildContext, interop: &Cinterop, konan_target: &str) -> Result<NativeOptions> {
    let root = ctx.ws.root();
    let mut options = interop.options_for(konan_target);
    options.library_paths = options.library_paths.iter().map(|p| root.join(p)).collect();
    if let Some(c) = ctx.ws.manifest().targets.native.as_ref().and_then(|n| n.c_sources.as_ref()) {
        options.compiler_options.extend(c.include_dirs.iter().map(|d| format!("-I{}", slash(&root.join(d)))));
    }
    if host_target() == Some(konan_target) {
        for package in &interop.pkg_config {
            let flags = find_package(interop, package, &pkg_config::search_path())?;
            add_pkg_config_flags(&mut options, flags);
        }
    }
    if interop.link == LinkMode::Static {
        let embedded = link_statically(&mut options);
        if embedded.is_empty() && options.static_libraries.is_empty() {
            let dirs: Vec<String> = library_dirs(&options).iter().map(|d| d.display().to_string()).collect();
            bail!(
                "[target.native.cinterop.{}] has link = \"static\", but no static library (`lib<name>.a`) for its `-l` options was found{}; \
                 install the static library, add its directory to `library-paths`, or list it in `static-libraries`",
                interop.name,
                if dirs.is_empty() { " (no library directory is known)".to_owned() } else { format!(" in {}", dirs.join(", ")) }
            );
        }
    }
    Ok(options)
}

/// The flags of a `pkg-config` package, or an error that says how to install it.
fn find_package(interop: &Cinterop, package: &str, dirs: &[PathBuf]) -> Result<pkg_config::Flags> {
    let section = format!("target.native.cinterop.{}", interop.name);
    pkg_config::probe(package, dirs, interop.link == LinkMode::Static).map_err(|e| {
        let Some(missing) = e.downcast_ref::<pkg_config::NotFound>() else {
            return e.context(format!("failed to read the pkg-config files of `{package}` for [{section}]"));
        };
        let searched = if dirs.is_empty() {
            "PKG_CONFIG_PATH is not set and none of the usual directories exists".to_owned()
        } else {
            format!("searched {}", dirs.iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join(", "))
        };
        let host = host_target().unwrap_or("<platform>");
        anyhow::anyhow!(
            "C library `{package}` of [{section}] not found: {missing} ({searched}); {}, set PKG_CONFIG_PATH to the directory \
             holding `{}.pc`, or give its options in [{section}.platform.{host}] instead of `pkg-config`",
            pkg_config::install_hint(package),
            missing.0
        )
    })
}

/// Adds the flags of a `pkg-config` package: compiler flags, `-L` directories, frameworks and the rest of the linker flags.
fn add_pkg_config_flags(options: &mut NativeOptions, flags: pkg_config::Flags) {
    options.compiler_options.extend(flags.cflags);
    let mut libs = flags.libs.into_iter();
    while let Some(flag) = libs.next() {
        if let Some(dir) = flag.strip_prefix("-L") {
            options.library_paths.push(PathBuf::from(dir));
        } else if flag == "-framework" {
            options.frameworks.extend(libs.next());
        } else {
            options.linker_options.push(flag);
        }
    }
}

/// The directories a linker searches: `library-paths`, then the `-L` linker options.
fn library_dirs(options: &NativeOptions) -> Vec<PathBuf> {
    let mut dirs = options.library_paths.clone();
    dirs.extend(options.linker_options.iter().filter_map(|o| o.strip_prefix("-L")).map(PathBuf::from));
    dirs
}

/// Turns each `-l<name>` whose `lib<name>.a` is in a library directory into a static library
/// embedded in the bindings, and returns their names. Others (system libraries) stay `-l` options.
fn link_statically(options: &mut NativeOptions) -> Vec<String> {
    let dirs = library_dirs(options);
    let mut embedded = Vec::new();
    let mut linker_options = Vec::new();
    for opt in std::mem::take(&mut options.linker_options) {
        let found = opt.strip_prefix("-l").and_then(|name| {
            let file = format!("lib{name}.a");
            dirs.iter().find(|d| d.join(&file).is_file()).map(|d| (name.to_owned(), file, d.clone()))
        });
        match found {
            Some((name, file, dir)) => {
                if !options.static_libraries.contains(&file) {
                    options.static_libraries.push(file);
                }
                if !options.library_paths.contains(&dir) {
                    options.library_paths.push(dir);
                }
                embedded.push(name);
            }
            None => linker_options.push(opt),
        }
    }
    options.linker_options = linker_options;
    embedded
}

/// Checks before any toolchain is downloaded what can fail without one: the C sources exist and,
/// when the host is one of the platforms, the `pkg-config` packages are installed.
pub fn check(ctx: &BuildContext, platforms: &[String]) -> Result<()> {
    let Some(native) = ctx.ws.manifest().targets.native.as_ref() else { return Ok(()) };
    if let Some(c) = &native.c_sources {
        c_source_files(ctx.ws.root(), c)?;
    }
    if let Some(host) = host_target().filter(|h| platforms.iter().any(|p| p == h)) {
        for interop in native.cinterop.iter().filter(|i| !i.pkg_config.is_empty()) {
            cinterop_options(ctx, interop, host)?;
        }
    }
    Ok(())
}

/// The `.def` file `cinterop` reads: the project's own, or, when kargo.toml sets `.def` properties
/// or `options` has linker options (which only the `.def` file records in the klib), one written
/// into `output_dir` with them followed by the project's. It is rewritten only when its text
/// changes, so the bindings stay up to date.
pub fn cinterop_def(
    interop: &Cinterop,
    project_def: Option<PathBuf>,
    options: &NativeOptions,
    konan_target: &str,
    output_dir: &Path,
) -> Result<Option<PathBuf>> {
    let linker_options = cinterop_linker_options(options, konan_target);
    if interop.def_properties().is_empty() && linker_options.is_empty() {
        return Ok(project_def);
    }
    let project_text = match &project_def {
        Some(def) => Some(std::fs::read_to_string(def).with_context(|| format!("failed to read {}", def.display()))?),
        None => None,
    };
    let text = def_file_contents(interop, &linker_options, project_text.as_deref());
    let path = output_dir.join(format!("{}.def", interop.name));
    if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
        std::fs::create_dir_all(output_dir).with_context(|| format!("failed to create {}", output_dir.display()))?;
        std::fs::write(&path, text).with_context(|| format!("failed to write {}", path.display()))?;
    }
    Ok(Some(path))
}

/// Whether `entry` of `sources` is a glob rather than a path.
fn is_glob(entry: &str) -> bool {
    entry.contains(['*', '?', '['])
}

/// The files `[target.native.c-sources]` compiles, sorted: the files listed, the C files under the
/// directories listed, and the files the globs match (gitignore syntax, relative to `root`).
pub fn c_source_files(root: &Path, c: &CSources) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut globs = GitignoreBuilder::new(root);
    let mut glob_entries = Vec::new();
    for entry in &c.sources {
        if is_glob(entry) {
            globs.add_line(None, entry).with_context(|| format!("invalid glob `{entry}` in [target.native.c-sources] sources"))?;
            glob_entries.push(entry.as_str());
            continue;
        }
        let path = root.join(entry);
        if path.is_file() {
            files.push(path);
        } else if path.is_dir() {
            files.extend(kargo_util::fs::collect_files(&path, C_EXTENSIONS));
        } else if c.sources == [CSources::DEFAULT_DIR] {
            bail!(
                "[target.native.c-sources] compiles the C files under `{}/`, which does not exist; create it or list the sources in `sources`",
                CSources::DEFAULT_DIR
            );
        } else {
            bail!("the C source `{entry}` of [target.native.c-sources] does not exist");
        }
    }
    if !glob_entries.is_empty() {
        let globs = globs.build().context("invalid glob in [target.native.c-sources] sources")?;
        let before = files.len();
        let walk = walkdir::WalkDir::new(root).into_iter().filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            e.depth() == 0 || !(name.starts_with('.') || (e.depth() == 1 && name == "target"))
        });
        for entry in walk.filter_map(|e| e.ok()).filter(|e| e.file_type().is_file()) {
            if let Ok(relative) = entry.path().strip_prefix(root)
                && globs.matched(relative, false).is_ignore()
            {
                files.push(entry.into_path());
            }
        }
        if files.len() == before {
            bail!("no file matches the sources {} of [target.native.c-sources]", glob_entries.join(", "));
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

/// Compiles `[target.native.c-sources]` for `konan_target` into a static library, or returns `None` without the table.
pub fn compile_c_sources(ctx: &BuildContext, konan_target: &str) -> Result<Option<PathBuf>> {
    let manifest = ctx.ws.manifest();
    let Some(c) = manifest.targets.native.as_ref().and_then(|n| n.c_sources.as_ref()) else { return Ok(None) };
    let root = ctx.ws.root();
    let profile = ctx.ws.profile(&ctx.profile)?;
    let name = format!("{}_c", manifest.package.name.replace('-', "_"));
    let unit = CSourcesUnit {
        name: &name,
        konan_target: konan_target.to_owned(),
        root,
        files: c_source_files(root, c)?,
        include_dirs: c.include_dirs.iter().map(|d| root.join(d)).collect(),
        flags: c.flags_for(konan_target),
        debug: profile.debug,
        optimize: profile.optimize,
        output_dir: ctx.ws.output_dir(&ctx.profile, TargetKind::Native).join(konan_target).join("c"),
    };
    let toolchain = ctx.native_toolchain()?;
    if !unit.files.is_empty() {
        toolchain.ensure_dependencies(ctx.config, konan_target)?;
    }
    let what = format!(", {konan_target}, C sources");
    let outcome =
        unit.compile(ctx.shell(), toolchain, &ctx.fingerprint_dir(TargetKind::Native), &|| compiling(ctx, TargetKind::Native, &what))?;
    Ok((outcome != Outcome::Empty).then(|| unit.artifact()))
}

/// What the native binaries for `konan_target` link besides klibs: the `[target.native]` options
/// of the platform and the static library of the C sources (compiled here when out of date).
pub fn native_link(ctx: &BuildContext, konan_target: &str) -> Result<NativeLink> {
    let native = ctx.ws.manifest().targets.native.as_ref().context("the Native target is not enabled")?;
    let root = ctx.ws.root();
    let options = native.options_for(konan_target);
    let dirs: Vec<PathBuf> = options.library_paths.iter().map(|p| root.join(p)).collect();
    let mut link = NativeLink::default();
    link.linker_options.extend(dirs.iter().map(|d| format!("-L{}", slash(d))));
    link.linker_options.extend(options.linker_options.iter().cloned());
    for lib in &options.static_libraries {
        let Some(file) = dirs.iter().map(|d| d.join(lib)).find(|f| f.is_file()) else {
            bail!(
                "the static library `{lib}` of [target.native] (for {konan_target}) is in none of its `library-paths`{}",
                if dirs.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", dirs.iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join(", "))
                }
            );
        };
        link.linker_options.push(slash(&file));
    }
    if is_apple_target(konan_target) {
        for framework in &options.frameworks {
            link.linker_options.push("-framework".to_owned());
            link.linker_options.push(framework.clone());
        }
    }
    link.archives.extend(compile_c_sources(ctx, konan_target)?);
    Ok(link)
}

/// The klibs of the dependencies `[target.native] export` names, whose API a framework or C library exports too.
pub fn exported_klibs(ctx: &BuildContext, setup: &TargetSetup, konan_target: &str) -> Result<Vec<PathBuf>> {
    let native = ctx.ws.manifest().targets.native.as_ref().context("the Native target is not enabled")?;
    if native.export.is_empty() {
        return Ok(Vec::new());
    }
    let resolution = native_resolution(ctx, setup, konan_target, false)?;
    let mut klibs = Vec::new();
    for entry in &native.export {
        let (group, artifact) = entry.split_once(':').context("an export entry is `group:artifact`")?;
        let is_klib = |f: &&PathBuf| f.extension().is_some_and(|e| e == "klib");
        // A multiplatform library resolves to its module for this platform (`<artifact>-<platform>`).
        let found: Vec<PathBuf> = resolution
            .nodes
            .iter()
            .filter(|n| {
                n.module.group == group && (n.module.artifact == artifact || n.module.artifact.starts_with(&format!("{artifact}-")))
            })
            .flat_map(|n| n.files.iter().filter(is_klib).cloned())
            .collect();
        if found.is_empty() {
            bail!(
                "[target.native] export lists `{entry}`, which is not a dependency of the native target for {konan_target}; add it to [dependencies]"
            );
        }
        klibs.extend(found);
    }
    Ok(klibs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_linking_embeds_the_libraries_it_finds() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("libsqlite3.a"), "").unwrap();
        let lib = dir.path().to_path_buf();
        let mut options = NativeOptions {
            linker_options: vec![format!("-L{}", lib.display()), "-lsqlite3".into(), "-lm".into(), "-pthread".into()],
            ..Default::default()
        };
        assert_eq!(link_statically(&mut options), ["sqlite3"]);
        assert_eq!(options.static_libraries, ["libsqlite3.a"]);
        assert_eq!(options.library_paths, std::slice::from_ref(&lib));
        assert_eq!(options.linker_options, [format!("-L{}", lib.display()), "-lm".into(), "-pthread".into()]);
        // Nothing to embed without a directory holding the archive.
        let mut dynamic = NativeOptions { linker_options: vec!["-lcurl".into()], ..Default::default() };
        assert!(link_statically(&mut dynamic).is_empty());
        assert_eq!(dynamic.linker_options, ["-lcurl"]);
    }

    #[test]
    fn pkg_config_flags_are_sorted_into_options() {
        let mut options = NativeOptions::default();
        let flags = pkg_config::Flags {
            cflags: vec!["-I/opt/x/include".into(), "-DX".into()],
            libs: vec!["-L/opt/x/lib".into(), "-lx".into(), "-framework".into(), "CoreFoundation".into(), "-Wl,-rpath,/opt".into()],
        };
        add_pkg_config_flags(&mut options, flags);
        assert_eq!(options.compiler_options, ["-I/opt/x/include", "-DX"]);
        assert_eq!(options.library_paths, [PathBuf::from("/opt/x/lib")]);
        assert_eq!(options.linker_options, ["-lx", "-Wl,-rpath,/opt"]);
        assert_eq!(options.frameworks, ["CoreFoundation"]);
    }

    #[test]
    fn c_sources_from_files_directories_and_globs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for file in ["c/a.c", "c/sub/b.cpp", "c/a.h", "vendor/z/z.c", "vendor/z/skip.txt", "target/c/x.c", "one.c"] {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        let sources = |list: &[&str]| CSources {
            sources: list.iter().map(|s| s.to_string()).collect(),
            include_dirs: Vec::new(),
            defines: Vec::new(),
            compiler_options: Vec::new(),
            platform: Default::default(),
        };
        let relative = |files: Vec<PathBuf>| files.iter().map(|f| slash(f.strip_prefix(root).unwrap())).collect::<Vec<_>>();
        assert_eq!(relative(c_source_files(root, &sources(&["c"])).unwrap()), ["c/a.c", "c/sub/b.cpp"]);
        assert_eq!(relative(c_source_files(root, &sources(&["vendor/**/*.c", "one.c"])).unwrap()), ["one.c", "vendor/z/z.c"]);
        // `target/` is never searched.
        let err = c_source_files(root, &sources(&["**/x.c", "one.c"])).unwrap_err().to_string();
        assert!(err.contains("no file matches the sources **/x.c"), "{err}");
        let err = c_source_files(root, &sources(&["missing.c"])).unwrap_err().to_string();
        assert!(err.contains("`missing.c` of [target.native.c-sources] does not exist"), "{err}");
        let err = c_source_files(root, &sources(&["*.cc"])).unwrap_err().to_string();
        assert!(err.contains("no file matches"), "{err}");
        let empty = tempfile::tempdir().unwrap();
        let err = c_source_files(empty.path(), &sources(&["c"])).unwrap_err().to_string();
        assert!(err.contains("which does not exist; create it"), "{err}");
    }

    #[test]
    fn generated_def_is_written_only_when_it_changes() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("x.def");
        std::fs::write(&project, "headers = x.h\n").unwrap();
        let mut interop = Cinterop {
            name: "x".into(),
            def: None,
            package: None,
            headers: Vec::new(),
            options: NativeOptions::default(),
            platform: Default::default(),
            header_filter: Vec::new(),
            excluded_functions: Vec::new(),
            strict_enums: Vec::new(),
            non_strict_enums: Vec::new(),
            modules: Vec::new(),
            language: None,
            pkg_config: Vec::new(),
            link: LinkMode::Dynamic,
        };
        let out = dir.path().join("out");
        let none = NativeOptions::default();
        assert_eq!(cinterop_def(&interop, Some(project.clone()), &none, "linux_x64", &out).unwrap(), Some(project.clone()));
        interop.excluded_functions = vec!["x_debug".into()];
        let written = cinterop_def(&interop, Some(project.clone()), &none, "linux_x64", &out).unwrap().unwrap();
        assert_eq!(written, out.join("x.def"));
        let text = std::fs::read_to_string(&written).unwrap();
        assert!(text.contains("excludedFunctions = x_debug\nheaders = x.h\n"), "{text}");
        let modified = std::fs::metadata(&written).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        cinterop_def(&interop, Some(project.clone()), &none, "linux_x64", &out).unwrap();
        assert_eq!(std::fs::metadata(&written).unwrap().modified().unwrap(), modified);
    }

    #[test]
    fn linker_options_are_written_to_the_def_file() {
        let dir = tempfile::tempdir().unwrap();
        let interop = Cinterop {
            name: "raylib".into(),
            def: None,
            package: None,
            headers: vec!["raylib.h".into()],
            options: NativeOptions::default(),
            platform: Default::default(),
            header_filter: Vec::new(),
            excluded_functions: Vec::new(),
            strict_enums: Vec::new(),
            non_strict_enums: Vec::new(),
            modules: Vec::new(),
            language: None,
            pkg_config: Vec::new(),
            link: LinkMode::Dynamic,
        };
        let options = NativeOptions {
            linker_options: vec!["-lopengl32".into(), "-lgdi32".into(), "-lwinmm".into()],
            frameworks: vec!["Cocoa".into()],
            ..Default::default()
        };
        // Without a `.def` file of the project, one is written just for the linker options.
        let written = cinterop_def(&interop, None, &options, "mingw_x64", dir.path()).unwrap().unwrap();
        let text = std::fs::read_to_string(written).unwrap();
        assert!(text.ends_with("\nlinkerOpts = -lopengl32 -lgdi32 -lwinmm\n"), "{text}");
        let written = cinterop_def(&interop, None, &options, "macos_arm64", dir.path()).unwrap().unwrap();
        let text = std::fs::read_to_string(written).unwrap();
        assert!(text.ends_with("\nlinkerOpts = -lopengl32 -lgdi32 -lwinmm -framework Cocoa\n"), "{text}");
    }
}
