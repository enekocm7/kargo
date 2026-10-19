//! `kargo package --format`: what a JVM application is shipped as besides a fat jar.
//!
//! - `jlink`: the program's jars, a runtime trimmed by `jlink` to the JDK modules `jdeps` finds,
//!   and `bin/<name>` and `bin/<name>.bat` launchers. Runs without an installed Java.
//! - `native-image`: a native executable built by GraalVM's `native-image`, with the
//!   configuration from `native-image/` (written by `kargo run --native-agent`) and the GraalVM
//!   reachability metadata repository.
//! - `installer`: an `.msi`/`.exe`, `.dmg`/`.pkg` or `.deb`/`.rpm` built by `jpackage` around a
//!   `jlink` runtime, with file associations, and on macOS signed and notarized.
//!
//! All three build for the host OS and CPU: build each platform's on that platform. With
//! `[frameworks] compose`, the runtime gets the modules Compose needs and installers carry Skiko's
//! native library next to the application (see [`crate::compose`]).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::build::JvmBuild;
use super::context::BuildContext;
use crate::compose;
use kargo_compiler::jar::{JarManifest, write_jar};
use kargo_manifest::{FileAssociation, InstallerSettings, InstallerType, NativeImageSettings, Notarization};
use kargo_toolchain::Jdk;
use kargo_util::fs::{join_paths, remove_dir_all_if_exists};
use kargo_util::process;

/// What `kargo package` builds for the JVM target (`--format`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PackageFormat {
    /// A self-contained jar, or a library's jar and sources jar (the default).
    #[default]
    Jar,
    /// A directory with the jars, a trimmed Java runtime and launchers.
    Jlink,
    /// A GraalVM native executable.
    NativeImage,
    /// A `jpackage` installer for the host OS.
    Installer,
}

impl PackageFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            PackageFormat::Jar => "jar",
            PackageFormat::Jlink => "jlink",
            PackageFormat::NativeImage => "native-image",
            PackageFormat::Installer => "installer",
        }
    }
}

/// The program being packaged.
pub struct App<'a> {
    /// The launcher, executable and installer name: the package's, or the `--bin`'s.
    pub name: &'a str,
    pub main_class: String,
    pub jvm: &'a JvmBuild,
    /// `target/<profile>/jvm/package`.
    pub dir: PathBuf,
}

impl App<'_> {
    /// The dependency jars, in classpath order.
    fn dependency_jars(&self) -> Vec<PathBuf> {
        self.jvm.runtime_classpath.iter().filter(|p| p.extension().is_some_and(|e| e == "jar")).cloned().collect()
    }

    /// Writes the program's own jar (without the `Class-Path` of `kargo build`'s) and copies the
    /// dependencies next to it in `lib`, except those that `skip` matches. Returns the file names in
    /// classpath order, the program's first.
    fn stage_jars(&self, ctx: &BuildContext, lib: &Path, skip: impl Fn(&Path) -> bool) -> Result<Vec<String>> {
        std::fs::create_dir_all(lib).with_context(|| format!("failed to create {}", lib.display()))?;
        let pkg = &ctx.ws.manifest().package;
        let deps: Vec<PathBuf> = self.dependency_jars().into_iter().filter(|j| !skip(j)).collect();
        let names = jar_names(&format!("{}-{}.jar", pkg.name, pkg.version), &deps);
        let manifest = JarManifest {
            main_class: Some(self.main_class.clone()),
            class_path: Vec::new(),
            implementation_title: Some(pkg.name.clone()),
            implementation_version: Some(pkg.version.clone()),
        };
        write_jar(&lib.join(&names[0]), &self.jvm.classes, &manifest, &[]).context("failed to create the program's jar")?;
        for (jar, name) in deps.iter().zip(&names[1..]) {
            std::fs::copy(jar, lib.join(name)).with_context(|| format!("failed to copy {}", jar.display()))?;
        }
        Ok(names)
    }
}

/// File names for the program's jar and the dependency jars, unique within one directory: a
/// second `core-1.0.jar` from another group becomes `core-1.0-2.jar`.
fn jar_names(app: &str, deps: &[PathBuf]) -> Vec<String> {
    let mut names = vec![app.to_owned()];
    for jar in deps {
        let file = jar.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "dependency.jar".to_owned());
        let stem = file.strip_suffix(".jar").unwrap_or(&file).to_owned();
        let mut name = file;
        let mut n = 2;
        while names.contains(&name) {
            name = format!("{stem}-{n}.jar");
            n += 1;
        }
        names.push(name);
    }
    names
}

fn jdk_tool(jdk: &Jdk, name: &str) -> PathBuf {
    jdk.home.join("bin").join(if cfg!(windows) { format!("{name}.exe") } else { name.to_owned() })
}

/// Runs `cmd`, failing with its output when it does not succeed.
fn output(ctx: &BuildContext, cmd: &mut Command, what: &str) -> Result<String> {
    ctx.shell().verbose("Running", format!("`{}`", process::describe(cmd)));
    let out = cmd.output().with_context(|| format!("failed to start {what} (`{}`)", cmd.get_program().to_string_lossy()))?;
    if !out.status.success() {
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        bail!("{what} failed ({}):\n{}", process::describe_status(out.status), text.trim_end());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The modules of `jdeps --print-module-deps`: a comma-separated list on the last line.
fn parse_module_deps(stdout: &str) -> Vec<String> {
    let line = stdout.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("");
    line.split(',').map(str::trim).filter(|m| !m.is_empty()).map(str::to_owned).collect()
}

/// `found` plus `extra`, sorted and without duplicates; `java.base` when both are empty.
fn module_list(found: Vec<String>, extra: &[String]) -> Vec<String> {
    let mut modules: Vec<String> = found.into_iter().chain(extra.iter().cloned()).collect();
    if modules.is_empty() {
        modules.push("java.base".to_owned());
    }
    modules.sort();
    modules.dedup();
    modules
}

/// The JDK modules the program needs: what `jdeps` finds in its classes and dependencies, plus
/// `[package.jlink] modules` and, for Compose, [`compose::RUNTIME_MODULES`].
fn runtime_modules(ctx: &BuildContext, app: &App) -> Result<Vec<String>> {
    let manifest = ctx.ws.manifest();
    let settings = &manifest.packaging.jlink;
    let mut extra = settings.modules.clone();
    if compose::enabled(manifest) {
        extra.extend(compose::RUNTIME_MODULES.map(str::to_owned));
    }
    if !settings.jdeps {
        return Ok(module_list(Vec::new(), &extra));
    }
    let jdk = ctx.jdk()?;
    let deps = app.dependency_jars();
    let mut cmd = Command::new(jdk_tool(jdk, "jdeps"));
    cmd.args(["--print-module-deps", "--ignore-missing-deps", "-q", "--multi-release"]).arg(jdk.version.to_string());
    if !deps.is_empty() {
        cmd.arg("--class-path").arg(join_paths(&deps));
    }
    cmd.arg(&app.jvm.classes).args(&deps);
    let stdout = output(ctx, &mut cmd, "jdeps")
        .context("could not find the JDK modules the program uses; list them in [package.jlink] modules and set `jdeps = false`")?;
    let modules = module_list(parse_module_deps(&stdout), &extra);
    ctx.shell().verbose("Modules", modules.join(","));
    Ok(modules)
}

/// `jlink --compress`: `zip-6` from JDK 21, the numeric level before.
fn jlink_compress(jdk_version: u32) -> &'static str {
    if jdk_version >= 21 { "zip-6" } else { "2" }
}

/// Builds a Java runtime with only `modules` into `dest`.
fn jlink(ctx: &BuildContext, modules: &[String], dest: &Path) -> Result<()> {
    let jdk = ctx.jdk()?;
    remove_dir_all_if_exists(dest)?;
    let mut cmd = Command::new(jdk_tool(jdk, "jlink"));
    cmd.arg("--add-modules")
        .arg(modules.join(","))
        .args(["--strip-debug", "--no-header-files", "--no-man-pages", "--compress", jlink_compress(jdk.version)])
        .arg("--output")
        .arg(dest);
    output(ctx, &mut cmd, "jlink").map(drop)
}

/// Quotes `arg` for a POSIX shell: as is when it is plain, else in single quotes.
fn sh_quote(arg: &str) -> String {
    if !arg.is_empty() && arg.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c)) {
        return arg.to_owned();
    }
    format!("'{}'", arg.replace('\'', r"'\''"))
}

/// Quotes `arg` for a `.bat` file: `%` doubled, and in double quotes when it has spaces or metacharacters.
fn bat_quote(arg: &str) -> String {
    let arg = arg.replace('%', "%%");
    if !arg.is_empty() && !arg.contains(|c: char| c.is_whitespace() || "&|<>^\"()".contains(c)) {
        return arg;
    }
    format!("\"{}\"", arg.replace('"', "\"\""))
}

/// `bin/<name>`: runs the program with the image's runtime, following symlinks to itself.
fn unix_launcher(main_class: &str, jvm_args: &[String], jars: &[String]) -> String {
    let classpath: Vec<String> = jars.iter().map(|j| format!("$APP_HOME/lib/{j}")).collect();
    let args: String = jvm_args.iter().map(|a| format!("{} ", sh_quote(a))).collect();
    format!(
        r#"#!/bin/sh
# Generated by Kargo.
app_path=$0
while [ -h "$app_path" ]; do
    ls=$(ls -ld "$app_path")
    link=${{ls#*' -> '}}
    case $link in
        /*) app_path=$link ;;
        *) app_path=$(dirname "$app_path")/$link ;;
    esac
done
APP_HOME=$(cd "$(dirname "$app_path")/.." && pwd -P) || exit
exec "$APP_HOME/runtime/bin/java" {args}-cp "{}" {} "$@"
"#,
        classpath.join(":"),
        sh_quote(main_class)
    )
}

/// `bin/<name>.bat`, with CRLF line endings.
fn windows_launcher(main_class: &str, jvm_args: &[String], jars: &[String]) -> String {
    let classpath: Vec<String> = jars.iter().map(|j| format!("%APP_HOME%\\lib\\{}", j.replace('%', "%%"))).collect();
    let args: String = jvm_args.iter().map(|a| format!("{} ", bat_quote(a))).collect();
    let lines = [
        "@echo off".to_owned(),
        "rem Generated by Kargo.".to_owned(),
        "setlocal".to_owned(),
        "set \"APP_HOME=%~dp0..\"".to_owned(),
        format!("\"%APP_HOME%\\runtime\\bin\\java.exe\" {args}-cp \"{}\" {} %*", classpath.join(";"), bat_quote(main_class)),
        "exit /b %ERRORLEVEL%".to_owned(),
    ];
    lines.iter().map(|l| format!("{l}\r\n")).collect()
}

/// `--format jlink`: `target/<profile>/jvm/package/<name>-<version>/` with `bin/`, `lib/` and `runtime/`.
pub fn jlink_image(ctx: &BuildContext, app: &App) -> Result<PathBuf> {
    let version = &ctx.ws.manifest().package.version;
    let image = app.dir.join(format!("{}-{version}", app.name));
    ctx.shell().status("Packaging", format!("{} v{version} runtime image ({})", app.name, image.display()));
    let modules = runtime_modules(ctx, app)?;
    remove_dir_all_if_exists(&image)?;
    jlink(ctx, &modules, &image.join("runtime"))?;
    let jars = app.stage_jars(ctx, &image.join("lib"), |_| false)?;
    let bin = image.join("bin");
    std::fs::create_dir_all(&bin)?;
    let script = bin.join(app.name);
    let jvm_args = app_jvm_args(ctx, app, None)?;
    kargo_util::fs::write_string_atomic(&script, &unix_launcher(&app.main_class, &jvm_args, &jars))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
    }
    kargo_util::fs::write_string_atomic(&bin.join(format!("{}.bat", app.name)), &windows_launcher(&app.main_class, &jvm_args, &jars))?;
    Ok(image)
}

/// The JVM options of a packaged application: for Compose, [`compose::SWING_GLOBALS`], native
/// access for Skiko (JDK 21+, which warns without it) and `-Dskiko.library.path=<skiko_dir>` when
/// the native library was unpacked; then the `jvm-args` of `[target.jvm]` and the profile.
fn app_jvm_args(ctx: &BuildContext, app: &App, skiko_dir: Option<&str>) -> Result<Vec<String>> {
    let mut args = Vec::new();
    if compose::enabled(ctx.ws.manifest()) {
        args.push(compose::SWING_GLOBALS.to_owned());
        if ctx.jdk()?.version >= 21 {
            args.push("--enable-native-access=ALL-UNNAMED".to_owned());
        }
        if let Some(dir) = skiko_dir {
            args.push(format!("-Dskiko.library.path={dir}"));
        }
    }
    args.extend(app.jvm.jvm_args.iter().cloned());
    Ok(args)
}

/// The `native-image` arguments before the classpath and the main class. GraalVM 25 removed the
/// fallback image (a JVM launcher built when compilation fails), so only older ones need `--no-fallback`.
fn native_image_args(settings: &NativeImageSettings, graalvm_version: u32, output: &Path) -> Vec<String> {
    let mut args = Vec::new();
    if graalvm_version < 25 {
        args.push("--no-fallback".to_owned());
    }
    args.extend(["-o".to_owned(), output.to_string_lossy().into_owned()]);
    if settings.static_link {
        args.extend(["--static".to_owned(), "--libc=musl".to_owned()]);
    }
    if !settings.initialize_at_build_time.is_empty() {
        args.push(format!("--initialize-at-build-time={}", settings.initialize_at_build_time.join(",")));
    }
    args.extend(settings.args.iter().cloned());
    args
}

/// Collects the configuration `native-image` reads from `META-INF/native-image` on the classpath
/// into `dir`: the project's `native-image/` and the reachability metadata of its dependencies.
fn native_image_config(ctx: &BuildContext, app: &App, dir: &Path) -> Result<()> {
    remove_dir_all_if_exists(dir)?;
    let meta_inf = dir.join("META-INF").join("native-image");
    let manifest = ctx.ws.manifest();
    let own = ctx.ws.root().join("native-image");
    if own.is_dir() {
        let group = manifest.package.group.as_deref().unwrap_or("kargo");
        kargo_util::fs::copy_dir(&own, &meta_inf.join(group).join(&manifest.package.name))?;
    }
    if manifest.packaging.native_image.metadata_repository && !app.jvm.runtime_modules.is_empty() {
        let repository = kargo_toolchain::graalvm::reachability_metadata(ctx.config)?;
        let mut found = 0;
        for (module, version) in &app.jvm.runtime_modules {
            if let Some(metadata) = kargo_toolchain::graalvm::metadata_dir(&repository, &module.group, &module.artifact, version) {
                kargo_util::fs::copy_dir(&metadata, &meta_inf.join(&module.group).join(&module.artifact))?;
                found += 1;
            }
        }
        ctx.shell().verbose("Using", format!("reachability metadata for {found} of {} libraries", app.jvm.runtime_modules.len()));
    }
    std::fs::create_dir_all(dir)?;
    Ok(())
}

/// `--format native-image`: `target/<profile>/jvm/package/<name>` (`.exe` on Windows).
pub fn native_image(ctx: &BuildContext, app: &App) -> Result<PathBuf> {
    let settings = &ctx.ws.manifest().packaging.native_image;
    if settings.static_link && std::env::consts::OS != "linux" {
        bail!("[package.native-image] `static = true` links against musl and only works on Linux");
    }
    let graalvm = kargo_toolchain::graalvm::resolve(ctx.config, &ctx.ws.manifest().toolchain.graalvm)?;
    ctx.shell().verbose("Using", format!("GraalVM for JDK {} at {}", graalvm.version, graalvm.home.display()));
    let output = app.dir.join(app.name);
    let executable = if cfg!(windows) { app.dir.join(format!("{}.exe", app.name)) } else { output.clone() };
    ctx.shell()
        .status("Packaging", format!("{} v{} native executable ({})", app.name, ctx.ws.manifest().package.version, executable.display()));
    let config = app.dir.join("native-image-config");
    native_image_config(ctx, app, &config)?;
    let mut classpath = vec![app.jvm.classes.clone()];
    classpath.extend(app.dependency_jars());
    classpath.push(config);
    let mut cmd = Command::new(graalvm.native_image());
    cmd.args(native_image_args(settings, graalvm.version, &output))
        .arg("-cp")
        .arg(join_paths(&classpath))
        .arg(&app.main_class)
        .current_dir(&app.dir)
        // `kargo package` prints the files it made on stdout; native-image's progress goes to stderr.
        .stdout(std::io::stderr());
    if std::env::var_os("INCLUDE").is_none()
        && let Some(path) = std::env::var_os("PATH").and_then(|p| path_without_msvc(&p))
    {
        cmd.env("PATH", path);
    }
    process::run(ctx.shell(), &mut cmd, "native-image")?;
    if !executable.is_file() {
        bail!("native-image finished but {} was not created", executable.display());
    }
    let _ = remove_dir_all_if_exists(&app.dir.join("native-image-config"));
    Ok(executable)
}

/// `PATH` without the directories that hold MSVC's `cl.exe`, when there are any (Windows only).
/// Outside a Visual Studio developer prompt `cl.exe` finds no headers (`INCLUDE` is not set);
/// without it on `PATH`, `native-image` finds Visual Studio and sets up its environment itself.
fn path_without_msvc(path: &std::ffi::OsStr) -> Option<std::ffi::OsString> {
    if !cfg!(windows) {
        return None;
    }
    let dirs: Vec<PathBuf> = std::env::split_paths(path).collect();
    let kept: Vec<&PathBuf> = dirs.iter().filter(|d| !d.join("cl.exe").is_file()).collect();
    if kept.len() == dirs.len() {
        return None;
    }
    std::env::join_paths(kept).ok()
}

/// The numeric start of a version, which `jpackage` requires: `1.2.3-SNAPSHOT` -> `1.2.3`.
fn numeric_version(version: &str) -> String {
    let parts: Vec<&str> = version
        .split(['-', '+'])
        .next()
        .unwrap_or("")
        .split('.')
        .take_while(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        .take(3)
        .collect();
    if parts.is_empty() { "1.0".to_owned() } else { parts.join(".") }
}

/// What the `jpackage` command line is built from.
struct Jpackage<'a> {
    settings: &'a InstallerSettings,
    kind: InstallerType,
    /// The host OS (`std::env::consts::OS`).
    os: &'a str,
    name: &'a str,
    version: String,
    description: Option<&'a str>,
    main_jar: &'a str,
    main_class: &'a str,
    jvm_args: &'a [String],
    input: &'a Path,
    runtime: &'a Path,
    dest: &'a Path,
    root: &'a Path,
    /// The properties files of `file-associations`.
    file_associations: &'a [PathBuf],
}

impl Jpackage<'_> {
    fn args(&self) -> Vec<String> {
        let path = |p: &Path| self.root.join(p).to_string_lossy().into_owned();
        let s = self.settings;
        let mut args: Vec<String> = [
            "--type",
            self.kind.as_str(),
            "--name",
            self.name,
            "--app-version",
            &self.version,
            "--input",
            &self.input.to_string_lossy(),
            "--main-jar",
            self.main_jar,
            "--main-class",
            self.main_class,
            "--runtime-image",
            &self.runtime.to_string_lossy(),
            "--dest",
            &self.dest.to_string_lossy(),
        ]
        .map(str::to_owned)
        .into();
        let mut opt = |flag: &str, value: Option<String>| {
            if let Some(value) = value {
                args.extend([flag.to_owned(), value]);
            }
        };
        opt("--vendor", s.vendor.clone());
        opt("--description", self.description.map(str::to_owned));
        opt("--icon", s.icon.for_os(self.os).map(path));
        if self.kind != InstallerType::AppImage {
            opt("--license-file", s.license_file.as_deref().map(path));
        }
        for arg in self.jvm_args {
            args.extend(["--java-options".to_owned(), java_option(arg)]);
        }
        let installer = self.kind != InstallerType::AppImage;
        // File associations are registered when the application is installed.
        if installer {
            for file in self.file_associations {
                args.extend(["--file-associations".to_owned(), file.to_string_lossy().into_owned()]);
            }
        }
        if self.os == "windows" && s.console {
            args.push("--win-console".to_owned());
        }
        match self.os {
            "windows" if installer => {
                if s.menu {
                    args.push("--win-menu".to_owned());
                }
                if s.shortcut {
                    args.push("--win-shortcut".to_owned());
                }
                if let Some(uuid) = &s.upgrade_uuid {
                    args.extend(["--win-upgrade-uuid".to_owned(), uuid.clone()]);
                }
            }
            "macos" => {
                if let Some(id) = &s.bundle_id {
                    args.extend(["--mac-package-identifier".to_owned(), id.clone()]);
                }
                if let Some(identity) = &s.signing_identity {
                    args.extend(["--mac-sign".to_owned(), "--mac-signing-key-user-name".to_owned(), identity.clone()]);
                }
            }
            "linux" if installer => {
                if s.menu || s.shortcut {
                    args.push("--linux-shortcut".to_owned());
                }
                if let Some(category) = &s.category {
                    args.extend(["--linux-app-category".to_owned(), category.clone()]);
                }
            }
            _ => {}
        }
        args
    }
}

/// A `--java-options` value: jpackage splits them at whitespace outside quotes, so an option with
/// spaces is quoted.
fn java_option(arg: &str) -> String {
    if !arg.contains(char::is_whitespace) {
        arg.to_owned()
    } else if arg.contains('"') {
        format!("'{arg}'")
    } else {
        format!("\"{arg}\"")
    }
}

/// The files (or, for `app-image`, the directory) `jpackage` wrote for `name` into `dest`.
fn installer_outputs(dest: &Path, name: &str, kind: InstallerType) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dest) else { return Vec::new() };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let file = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            match kind {
                InstallerType::AppImage => p.is_dir() && (file == name || file == format!("{name}.app")),
                _ => p.is_file() && file.starts_with(name) && file.ends_with(&format!(".{}", kind.as_str())),
            }
        })
        .collect();
    found.sort();
    found
}

/// `--format installer`: `jpackage` around a `jlink` runtime, written to `target/<profile>/jvm/package/`.
/// Builds each `[package.installer] type` that the host OS builds (its default type when none are
/// listed), then notarizes the macOS ones when `notarize` is set.
pub fn installer(ctx: &BuildContext, app: &App) -> Result<Vec<PathBuf>> {
    let manifest = ctx.ws.manifest();
    let settings = &manifest.packaging.installer;
    let os = std::env::consts::OS;
    let kinds = settings.kinds_for(os);
    if kinds.is_empty() {
        let listed: Vec<&str> = settings.kinds.iter().map(|k| k.as_str()).collect();
        bail!(
            "[package.installer] type = [\"{}\"] has no type that {os} builds: jpackage builds installers for the OS it runs on, so add \"{}\" or build on another OS",
            listed.join("\", \""),
            InstallerType::default_for(os).as_str()
        );
    }
    let jdk = ctx.jdk()?;
    if jdk.version < 16 {
        bail!("installers need jpackage from JDK 16 or newer, but the project uses JDK {}; set [toolchain] jdk = \"21\"", jdk.version);
    }
    let name = settings.app_name(app.name);
    let root = ctx.ws.root();
    let association_icons = settings.file_associations.iter().map(|a| ("file-associations` `icon", a.icon.as_deref()));
    let files_to_check = [("license-file", settings.license_file.as_deref()), ("icon", settings.icon.for_os(os))];
    for (key, file) in files_to_check.into_iter().chain(association_icons) {
        if let Some(file) = file.filter(|f| !root.join(f).is_file()) {
            bail!("[package.installer] `{key}` {} does not exist", file.display());
        }
    }
    let notarize = settings.notarize.as_ref().filter(|_| os == "macos").map(notary_credentials).transpose()?;
    let version = match &settings.app_version {
        Some(v) => v.clone(),
        None => numeric_version(&manifest.package.version),
    };
    let types: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
    let what = if let [one] = types[..] { format!("{one} installer") } else { format!("installers ({})", types.join(", ")) };
    ctx.shell().status("Packaging", format!("{name} v{version} {what} ({})", app.dir.display()));

    let work = app.dir.join(format!("{}-installer", app.name));
    remove_dir_all_if_exists(&work)?;
    let runtime = work.join("runtime");
    jlink(ctx, &runtime_modules(ctx, app)?, &runtime)?;
    let input = work.join("input");
    // Skiko's native library goes next to the jars (jpackage's `$APPDIR`), where macOS signs it
    // with the rest of the application, instead of being unpacked from a jar at startup.
    let skiko: Vec<PathBuf> = if compose::enabled(manifest) {
        app.dependency_jars().into_iter().filter(|j| compose::is_skiko_runtime_jar(j)).collect()
    } else {
        Vec::new()
    };
    for jar in &skiko {
        let files = compose::unpack_skiko_runtime(jar, &input)?;
        ctx.shell().verbose("Unpacked", format!("{} from {}", files.join(", "), jar.display()));
    }
    let jvm_args = app_jvm_args(ctx, app, (!skiko.is_empty()).then_some("$APPDIR"))?;
    let jars = app.stage_jars(ctx, &input, |jar| skiko.iter().any(|s| s == jar))?;
    let associations = write_file_associations(&settings.file_associations, root, &work)?;

    let mut files = Vec::new();
    for kind in kinds {
        for old in installer_outputs(&app.dir, name, kind) {
            if old.is_dir() { remove_dir_all_if_exists(&old)? } else { std::fs::remove_file(&old)? }
        }
        let job = Jpackage {
            settings,
            kind,
            os,
            name,
            version: version.clone(),
            description: settings.description.as_deref().or(manifest.package.description.as_deref()),
            main_jar: &jars[0],
            main_class: &app.main_class,
            jvm_args: &jvm_args,
            input: &input,
            runtime: &runtime,
            dest: &app.dir,
            root,
            file_associations: &associations,
        };
        let mut cmd = Command::new(jdk_tool(jdk, "jpackage"));
        cmd.args(job.args());
        if let Err(err) = output(ctx, &mut cmd, "jpackage") {
            let hint = match kind {
                InstallerType::Msi | InstallerType::Exe => {
                    "\njpackage needs the WiX Toolset (https://wixtoolset.org) on PATH to build Windows installers"
                }
                InstallerType::Rpm => "\njpackage needs `rpmbuild` to build .rpm packages",
                InstallerType::Deb => "\njpackage needs `dpkg-deb` and `fakeroot` to build .deb packages",
                _ => "",
            };
            bail!("{err:#}{hint}");
        }
        let outputs = installer_outputs(&app.dir, name, kind);
        if outputs.is_empty() {
            bail!("jpackage finished but wrote no {} for `{name}` into {}", kind.as_str(), app.dir.display());
        }
        if let Some(credentials) = &notarize {
            if matches!(kind, InstallerType::Dmg | InstallerType::Pkg) {
                for file in &outputs {
                    notarize_file(ctx, file, credentials)?;
                }
            } else {
                ctx.shell().note(format!("`notarize` applies to dmg and pkg installers, so the {} is not notarized", kind.as_str()));
            }
        }
        files.extend(outputs);
    }
    let _ = remove_dir_all_if_exists(&work);
    Ok(files)
}

/// Escapes a value for a Java `.properties` file.
fn properties_value(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\n', "\\n")
}

/// The properties file `jpackage --file-associations` reads for one file type.
fn file_association_properties(association: &FileAssociation, root: &Path) -> String {
    let mut lines = vec![
        format!("extension={}", properties_value(&association.extension)),
        format!("mime-type={}", properties_value(&association.mime_type)),
    ];
    if let Some(description) = &association.description {
        lines.push(format!("description={}", properties_value(description)));
    }
    if let Some(icon) = &association.icon {
        lines.push(format!("icon={}", properties_value(&root.join(icon).to_string_lossy())));
    }
    lines.iter().map(|l| format!("{l}\n")).collect()
}

/// Writes one properties file per file association into `work`; returns their paths.
fn write_file_associations(associations: &[FileAssociation], root: &Path, work: &Path) -> Result<Vec<PathBuf>> {
    associations
        .iter()
        .map(|association| {
            let path = work.join(format!("file-association-{}.properties", association.extension));
            kargo_util::fs::write_string_atomic(&path, &file_association_properties(association, root))?;
            Ok(path)
        })
        .collect()
}

/// The `xcrun notarytool` options that authenticate: `--keychain-profile`, or the Apple ID,
/// password and team ID, the first two read from the environment.
fn notary_credentials(notarize: &Notarization) -> Result<Vec<String>> {
    Ok(match notarize {
        Notarization::KeychainProfile(profile) => vec!["--keychain-profile".to_owned(), profile.clone()],
        Notarization::AppleId { apple_id_env, password_env, team_id } => {
            let var = |name: &str, what: &str| match std::env::var(name) {
                Ok(value) if !value.is_empty() => Ok(value),
                _ => bail!("[package.installer] `notarize` reads the {what} from ${name}, which is not set"),
            };
            vec![
                "--apple-id".to_owned(),
                var(apple_id_env, "Apple ID")?,
                "--password".to_owned(),
                var(password_env, "app-specific password")?,
                "--team-id".to_owned(),
                team_id.clone(),
            ]
        }
    })
}

/// The `xcrun` arguments that submit `file` to the notary service and wait for its verdict.
fn notarytool_args(file: &Path, credentials: &[String]) -> Vec<String> {
    let mut args = vec!["notarytool".to_owned(), "submit".to_owned(), file.to_string_lossy().into_owned(), "--wait".to_owned()];
    args.extend(credentials.iter().cloned());
    args
}

/// Sends `file` to Apple's notary service, waits for the verdict and staples the ticket to it.
fn notarize_file(ctx: &BuildContext, file: &Path, credentials: &[String]) -> Result<()> {
    ctx.shell().status("Notarizing", format!("{} (this can take a few minutes)", file.display()));
    let mut cmd = Command::new("xcrun");
    // The command line holds the password, so unlike other tools it is not shown with `-v`.
    cmd.args(notarytool_args(file, credentials));
    let out = cmd.output().context("failed to start `xcrun notarytool`; notarizing needs Xcode or its command line tools")?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if !out.status.success() || !text.contains("status: Accepted") {
        bail!(
            "Apple did not notarize {} ({}):\n{}\nrun `xcrun notarytool log <submission id>` with the same credentials to see why",
            file.display(),
            process::describe_status(out.status),
            text.trim_end()
        );
    }
    let mut staple = Command::new("xcrun");
    staple.arg("stapler").arg("staple").arg(file);
    output(ctx, &mut staple, "xcrun stapler").map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_jar_names() {
        let deps = [PathBuf::from("/m2/a/core-1.0.jar"), PathBuf::from("/m2/b/core-1.0.jar"), PathBuf::from("/m2/c/app-1.0.jar")];
        assert_eq!(jar_names("app-1.0.jar", &deps), ["app-1.0.jar", "core-1.0.jar", "core-1.0-2.jar", "app-1.0-2.jar"]);
    }

    #[test]
    fn module_deps() {
        assert_eq!(parse_module_deps("java.base,java.logging,jdk.unsupported\n"), ["java.base", "java.logging", "jdk.unsupported"]);
        assert_eq!(parse_module_deps("Warning: x\njava.base\n\n"), ["java.base"]);
        assert!(parse_module_deps("").is_empty());
        let extra = ["java.naming".to_owned(), "java.base".to_owned()];
        assert_eq!(module_list(vec!["java.logging".into(), "java.base".into()], &extra), ["java.base", "java.logging", "java.naming"]);
        assert_eq!(module_list(Vec::new(), &[]), ["java.base"]);
        assert_eq!(jlink_compress(17), "2");
        assert_eq!(jlink_compress(21), "zip-6");
    }

    #[test]
    fn launchers() {
        let jars = ["app-1.0.jar".to_owned(), "kotlin-stdlib-2.4.21.jar".to_owned()];
        let args = ["-Xmx512m".to_owned(), "-Dgreeting=hello world".to_owned(), "-Dpct=50%".to_owned()];
        let sh = unix_launcher("com.example.MainKt", &args, &jars);
        assert!(sh.starts_with("#!/bin/sh\n"));
        assert!(
            sh.contains(
                r#"exec "$APP_HOME/runtime/bin/java" -Xmx512m '-Dgreeting=hello world' -Dpct=50% -cp "$APP_HOME/lib/app-1.0.jar:$APP_HOME/lib/kotlin-stdlib-2.4.21.jar" com.example.MainKt "$@""#
            ),
            "{sh}"
        );
        assert!(!sh.contains('\r'));
        assert_eq!(sh_quote("it's"), r"'it'\''s'");

        let bat = windows_launcher("com.example.MainKt", &args, &jars);
        assert!(bat.contains("\r\n") && bat.starts_with("@echo off\r\n"));
        assert!(
            bat.contains(
                r#""%APP_HOME%\runtime\bin\java.exe" -Xmx512m "-Dgreeting=hello world" -Dpct=50%% -cp "%APP_HOME%\lib\app-1.0.jar;%APP_HOME%\lib\kotlin-stdlib-2.4.21.jar" com.example.MainKt %*"#
            ),
            "{bat}"
        );
        assert!(bat.contains("exit /b %ERRORLEVEL%"));
    }

    #[test]
    fn native_image_command_line() {
        let out = Path::new("out/hello");
        let plain = NativeImageSettings::default();
        assert_eq!(native_image_args(&plain, 21, out), ["--no-fallback", "-o", &out.to_string_lossy()]);
        assert_eq!(native_image_args(&plain, 25, out), ["-o", &out.to_string_lossy()]);
        let full = NativeImageSettings {
            args: vec!["--enable-url-protocols=https".into()],
            static_link: true,
            initialize_at_build_time: vec!["kotlinx.serialization".into(), "org.slf4j".into()],
            metadata_repository: true,
        };
        let args = native_image_args(&full, 25, out);
        assert_eq!(
            args[2..],
            ["--static", "--libc=musl", "--initialize-at-build-time=kotlinx.serialization,org.slf4j", "--enable-url-protocols=https"]
        );
    }

    #[test]
    fn hides_msvc_without_its_environment() {
        let tmp = tempfile::tempdir().unwrap();
        let (msvc, other) = (tmp.path().join("msvc"), tmp.path().join("bin"));
        std::fs::create_dir_all(&msvc).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(msvc.join("cl.exe"), "").unwrap();
        let path = std::env::join_paths([&other, &msvc]).unwrap();
        let filtered = path_without_msvc(&path);
        if cfg!(windows) {
            assert!(path_without_msvc(&std::env::join_paths([&other]).unwrap()).is_none());
            assert_eq!(std::env::split_paths(&filtered.unwrap()).collect::<Vec<_>>(), [other]);
        } else {
            assert!(filtered.is_none());
        }
    }

    #[test]
    fn numeric_versions() {
        assert_eq!(numeric_version("1.2.3"), "1.2.3");
        assert_eq!(numeric_version("1.2.3-SNAPSHOT"), "1.2.3");
        assert_eq!(numeric_version("2.0.0+build.5"), "2.0.0");
        assert_eq!(numeric_version("1.2.3.4"), "1.2.3");
        assert_eq!(numeric_version("0.1.0-alpha.1"), "0.1.0");
        assert_eq!(numeric_version("2024.1.rc1"), "2024.1");
        assert_eq!(numeric_version("dev"), "1.0");
    }

    fn jpackage_args(settings: &InstallerSettings, kind: InstallerType, os: &str) -> Vec<String> {
        Jpackage {
            settings,
            kind,
            os,
            name: "Hello",
            version: "1.2.3".into(),
            description: Some("Says hello"),
            main_jar: "hello-1.2.3.jar",
            main_class: "com.example.MainKt",
            jvm_args: &["-Xmx1g".to_owned(), "-Dgreeting=Hi there".to_owned()],
            input: Path::new("in"),
            runtime: Path::new("rt"),
            dest: Path::new("out"),
            root: Path::new("proj"),
            file_associations: &[PathBuf::from("work/file-association-note.properties")],
        }
        .args()
    }

    #[test]
    fn file_associations() {
        let association = FileAssociation {
            extension: "note".into(),
            mime_type: "application/x-note".into(),
            description: Some("Note".into()),
            icon: Some("icons/note.ico".into()),
        };
        let root = Path::new(r"C:\proj");
        let text = file_association_properties(&association, root);
        let icon = root.join("icons/note.ico").to_string_lossy().replace('\\', "\\\\");
        assert_eq!(text, format!("extension=note\nmime-type=application/x-note\ndescription=Note\nicon={icon}\n"));
        let plain = FileAssociation { description: None, icon: None, ..association };
        assert_eq!(file_association_properties(&plain, root), "extension=note\nmime-type=application/x-note\n");

        let msi = jpackage_args(&InstallerSettings::default(), InstallerType::Msi, "windows");
        assert!(pairs(&msi).contains(&("--file-associations".to_owned(), "work/file-association-note.properties".to_owned())));
        let image = jpackage_args(&InstallerSettings::default(), InstallerType::AppImage, "windows");
        assert!(!image.contains(&"--file-associations".to_owned()), "an app image is not installed");
    }

    #[test]
    fn notarization() {
        let profile = notary_credentials(&Notarization::KeychainProfile("kargo".into())).unwrap();
        assert_eq!(profile, ["--keychain-profile", "kargo"]);
        assert_eq!(
            notarytool_args(Path::new("out/Notes-1.0.dmg"), &profile),
            ["notarytool", "submit", &Path::new("out/Notes-1.0.dmg").to_string_lossy(), "--wait", "--keychain-profile", "kargo"]
        );
        let missing = Notarization::AppleId {
            apple_id_env: "KARGO_TEST_UNSET_APPLE_ID".into(),
            password_env: "KARGO_TEST_UNSET_PASSWORD".into(),
            team_id: "ABCDE12345".into(),
        };
        assert!(notary_credentials(&missing).unwrap_err().to_string().contains("$KARGO_TEST_UNSET_APPLE_ID, which is not set"));
    }

    fn pairs(args: &[String]) -> Vec<(String, String)> {
        args.windows(2).map(|w| (w[0].clone(), w[1].clone())).collect()
    }

    #[test]
    fn jpackage_command_line() {
        let settings = InstallerSettings {
            vendor: Some("Example".into()),
            icon: kargo_manifest::InstallerIcons {
                windows: Some("icons/app.ico".into()),
                macos: Some("icons/app.icns".into()),
                linux: None,
            },
            license_file: Some("LICENSE".into()),
            menu: true,
            shortcut: true,
            upgrade_uuid: Some("0b5b2d6e-8f5c-4a37-9d1e-3a6c2f4b7e90".into()),
            bundle_id: Some("com.example.hello".into()),
            signing_identity: Some("Developer ID Application: Example".into()),
            category: Some("devel".into()),
            console: true,
            ..Default::default()
        };
        let windows = jpackage_args(&settings, InstallerType::Msi, "windows");
        let p = pairs(&windows);
        for (flag, value) in [
            ("--type", "msi"),
            ("--name", "Hello"),
            ("--app-version", "1.2.3"),
            ("--main-jar", "hello-1.2.3.jar"),
            ("--main-class", "com.example.MainKt"),
            ("--runtime-image", "rt"),
            ("--vendor", "Example"),
            ("--description", "Says hello"),
            ("--java-options", "-Xmx1g"),
            ("--java-options", "\"-Dgreeting=Hi there\""),
            ("--win-upgrade-uuid", "0b5b2d6e-8f5c-4a37-9d1e-3a6c2f4b7e90"),
        ] {
            assert!(p.contains(&(flag.to_owned(), value.to_owned())), "{flag} {value} in {windows:?}");
        }
        assert!(p.contains(&("--icon".to_owned(), Path::new("proj").join("icons/app.ico").to_string_lossy().into_owned())));
        assert!(windows.contains(&"--win-menu".to_owned()) && windows.contains(&"--win-shortcut".to_owned()));
        assert!(windows.contains(&"--win-console".to_owned()));
        assert!(!windows.iter().any(|a| a.starts_with("--mac") || a.starts_with("--linux")));

        let mac = jpackage_args(&settings, InstallerType::Dmg, "macos");
        let p = pairs(&mac);
        assert!(p.contains(&("--mac-package-identifier".to_owned(), "com.example.hello".to_owned())));
        assert!(p.contains(&("--mac-signing-key-user-name".to_owned(), "Developer ID Application: Example".to_owned())));
        assert!(mac.contains(&"--mac-sign".to_owned()));

        let linux = jpackage_args(&settings, InstallerType::Deb, "linux");
        assert!(pairs(&linux).contains(&("--linux-app-category".to_owned(), "devel".to_owned())));
        assert!(linux.contains(&"--linux-shortcut".to_owned()));
        assert!(!linux.contains(&"--icon".to_owned()), "no Linux icon configured");

        // An app image has no installer, so no license, menu or shortcut.
        let image = jpackage_args(&settings, InstallerType::AppImage, "windows");
        assert!(!image.contains(&"--license-file".to_owned()) && !image.contains(&"--win-menu".to_owned()));
        assert!(image.contains(&"--win-console".to_owned()), "an app image's launcher is a console program too");

        let plain = jpackage_args(&InstallerSettings::default(), InstallerType::Deb, "linux");
        assert!(!plain.contains(&"--vendor".to_owned()) && !plain.contains(&"--linux-shortcut".to_owned()));
    }

    #[test]
    fn finds_installer_outputs() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for file in ["Hello-1.2.3.msi", "Hello-1.2.3.dmg", "Other-1.0.msi", "hello_1.2.3-1_amd64.deb"] {
            std::fs::write(dir.join(file), "").unwrap();
        }
        std::fs::create_dir_all(dir.join("Hello")).unwrap();
        assert_eq!(installer_outputs(dir, "Hello", InstallerType::Msi), [dir.join("Hello-1.2.3.msi")]);
        assert_eq!(installer_outputs(dir, "hello", InstallerType::Deb), [dir.join("hello_1.2.3-1_amd64.deb")]);
        assert_eq!(installer_outputs(dir, "Hello", InstallerType::AppImage), [dir.join("Hello")]);
        assert!(installer_outputs(&dir.join("missing"), "Hello", InstallerType::Msi).is_empty());
    }
}
