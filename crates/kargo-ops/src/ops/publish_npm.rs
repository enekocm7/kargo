//! `kargo publish --npm`: builds the Wasm or Kotlin/JS library with the release profile and publishes its
//! npm package (the `package.json` next to the compiled module, see [`crate::npm_package`]) with npm, pnpm
//! or bun, whichever the project uses.
//!
//! Everything that can be checked is checked before anything is compiled: the package name, the version,
//! the registry and its credentials. The package is then packed into a tarball in
//! `target/release/npm/`, the files the tarball holds are listed and, unless `--dry-run` is set, the tarball
//! is published, so what was listed is what is uploaded.
//!
//! Credentials never come from `kargo.toml`. `NPM_TOKEN` is written to a temporary `.npmrc` (readable by
//! its owner only) in the package's directory for the length of the upload; without it the package manager
//! reads the user's own `.npmrc` as it always does.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result, bail};

use super::build::{build_js, build_wasm, finished};
use super::context::BuildContext;
use crate::npm_package::package_names;
use crate::npm_registry::{Npmrc, Registry, auth_key};
use crate::npm_semver::Semver;
use kargo_manifest::{Manifest, NpmAccess, PackageManager, TargetKind, WasmKind};
use kargo_util::fs::remove_dir_all_if_exists;
use kargo_util::process::{self, describe_status};
use kargo_util::shell::Shell;

/// The environment variable whose value is the registry's access token.
pub const TOKEN_VARIABLE: &str = "NPM_TOKEN";

/// What the command line says about an npm publish; it overrides `[publish.npm]`.
#[derive(Debug, Clone, Default)]
pub struct NpmOptions {
    pub registry: Option<String>,
    /// The dist-tag, such as `next`.
    pub tag: Option<String>,
    /// A one-time password for registries that ask for one.
    pub otp: Option<String>,
    /// Pack and check everything, but upload nothing.
    pub dry_run: bool,
}

/// Whether the package builds an npm package: a Wasm or Kotlin/JS target with `kind = "library"`.
pub fn is_npm_library(manifest: &Manifest) -> bool {
    !package_names(manifest).is_empty()
}

/// The targets of the package that make an npm package, with the name each is published under.
fn libraries(manifest: &Manifest) -> Result<Vec<(TargetKind, String)>> {
    let settings = manifest.publish.npm.as_ref();
    let mut out = Vec::new();
    if let Some(wasm) = manifest.targets.wasm.as_ref().filter(|w| w.kind == WasmKind::Library) {
        out.push((TargetKind::Wasm, crate::npm_package::scoped_name(wasm.npm_package_name(&manifest.package), settings)));
    }
    if let Some(js) = manifest.targets.js.as_ref().filter(|j| j.kind == WasmKind::Library) {
        out.push((TargetKind::Js, crate::npm_package::scoped_name(js.npm_package_name(&manifest.package), settings)));
    }
    if out.is_empty() {
        bail!(
            "`{}` builds no npm package; set `kind = \"library\"` under [target.wasm] (with environment = \"js\" or \"browser\") or [target.js]",
            manifest.package.name
        );
    }
    if let [(_, first), (_, second)] = out.as_slice()
        && first == second
    {
        bail!("the Wasm and the Kotlin/JS library are both the npm package `{first}`; give one of them another `npm-name`");
    }
    Ok(out)
}

/// `kargo publish --npm` for one package: builds its libraries and publishes their npm packages.
pub fn publish(ctx: &BuildContext, opts: &NpmOptions) -> Result<()> {
    let start = Instant::now();
    let manifest = ctx.ws.manifest();
    let pkg = &manifest.package;
    if !pkg.publish {
        bail!("`{}` has `publish = false` in [package] and cannot be published", pkg.name);
    }
    let settings = manifest.publish.npm.clone().unwrap_or_default();
    let tag = opts.tag.clone().or_else(|| settings.tag.clone());
    check_version(&pkg.version, tag.as_deref())?;

    let mut prepared = Vec::new();
    for (target, name) in libraries(manifest)? {
        if !kargo_manifest::is_npm_name(&name) {
            bail!(
                "`{name}` is not a valid npm package name; use lowercase letters, digits, `-`, `.`, `_` and `~`, optionally scoped like \
                 `@scope/name` (set `npm-name` under [target.{target}] or `scope` in [publish.npm])"
            );
        }
        let mut npmrc = Npmrc::load(ctx.ws.root());
        if let Some(url) = opts.registry.clone().or_else(|| settings.registry.clone()) {
            npmrc.set_registry(&url);
        }
        let registry = npmrc.registry_for(&name);
        let token = std::env::var(TOKEN_VARIABLE).ok().filter(|t| !t.trim().is_empty());
        if let Some(token) = &token {
            npmrc.set_token(&registry, token);
        } else if !opts.dry_run && npmrc.token_for(&registry).is_none() {
            ctx.shell().warn(format!(
                "no credentials for {registry}: set `{TOKEN_VARIABLE}` or add `{}:_authToken=<token>` to your .npmrc",
                auth_key(&registry).trim_end_matches('/')
            ));
        }
        ensure_new_version(ctx, npmrc, &name, &pkg.version, opts.dry_run)?;
        let (manager, exe) = crate::npm::publisher(ctx, target)?;
        if settings.provenance {
            check_provenance(manager, opts.dry_run, &|name| std::env::var(name).ok())?;
        }
        prepared.push((target, name, registry, token, manager, exe));
    }

    let work = ctx.ws.profile_dir(&ctx.profile).join("npm");
    remove_dir_all_if_exists(&work)?;
    for (target, name, registry, token, manager, exe) in prepared {
        let dir = match target {
            TargetKind::Wasm => build_wasm(ctx, false)?.map(|b| b.dir),
            _ => build_js(ctx)?.map(|b| b.dir),
        }
        .with_context(|| format!("there are no {target} sources to publish; add them under `src/` or `src-{target}/`"))?;
        check_package(&dir, &name, &pkg.version)?;
        let package = Package {
            name: &name,
            dir: &dir,
            manager,
            exe: &exe,
            registry: &registry,
            token: token.as_deref(),
            tag: tag.as_deref(),
            access: settings.access,
            otp: opts.otp.as_deref(),
            provenance: settings.provenance,
        };
        ctx.shell().status("Packaging", format!("{name} v{} ({manager})", pkg.version));
        let tarball = package.pack(ctx.shell(), &work.join(file_name(&name)))?;
        list_tarball(ctx.shell(), &tarball)?;
        if opts.dry_run {
            ctx.shell().warn(format!("aborting upload of {name} to {registry} due to dry run"));
            continue;
        }
        let tag_note = tag.as_deref().map(|t| format!(" (tag {t})")).unwrap_or_default();
        ctx.shell().status("Uploading", format!("{name} v{}{tag_note} to {registry}", pkg.version));
        package.upload(ctx.shell(), &tarball)?;
        ctx.shell().status("Published", format!("{name} v{} to {registry}{tag_note}", pkg.version));
    }
    finished(ctx, start);
    Ok(())
}

/// The version must be a SemVer version, and a pre-release needs a dist-tag: registries would make it `latest`.
fn check_version(version: &str, tag: Option<&str>) -> Result<()> {
    if Semver::parse(version).is_none() || version.starts_with(['v', '=']) {
        bail!("`{version}` is not a version npm accepts; set `version` in [package] to a SemVer version such as `1.2.3` or `1.2.3-beta.1`");
    }
    let prerelease = version.split('+').next().is_some_and(|core| core.contains('-'));
    if prerelease && tag.is_none() {
        bail!(
            "`{version}` is a pre-release, which npm only publishes under a dist-tag so that it does not become `latest`; \
             pass `--tag next` or set `tag` in [publish.npm]"
        );
    }
    Ok(())
}

/// Fails when the registry already has `version` of `name`: a published version never changes.
fn ensure_new_version(ctx: &BuildContext, npmrc: Npmrc, name: &str, version: &str, dry_run: bool) -> Result<()> {
    if ctx.config.offline() {
        if dry_run {
            ctx.shell().note(format!("--offline: not checking whether {name} v{version} is on the registry"));
            return Ok(());
        }
        bail!("cannot publish {name} with --offline; run again without it");
    }
    match Registry::with(ctx.config, npmrc).published_versions(name) {
        Ok(Some(versions)) if versions.iter().any(|v| v == version) => {
            bail!("{name} v{version} is already on the registry and a published version cannot change; bump `version` in [package]")
        }
        Ok(_) => Ok(()),
        Err(error) if dry_run => {
            ctx.shell().warn(format!("could not check whether {name} v{version} is on the registry: {error:#}"));
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// Provenance needs a package manager that can sign (not bun) and a CI provider that gives npm an identity.
fn check_provenance(manager: PackageManager, dry_run: bool, env: &dyn Fn(&str) -> Option<String>) -> Result<()> {
    if manager == PackageManager::Bun {
        bail!("`provenance = true` in [publish.npm] needs npm or pnpm; bun cannot publish with provenance (set `package-manager`)");
    }
    if dry_run || provenance_supported(env) {
        return Ok(());
    }
    bail!(
        "`provenance = true` in [publish.npm] needs a CI run that can sign: on GitHub Actions give the job `permissions: {{ id-token: write }}` \
         (see the CI guide), or turn provenance off"
    )
}

fn provenance_supported(env: &dyn Fn(&str) -> Option<String>) -> bool {
    let set = |name: &str| env(name).is_some_and(|v| !v.is_empty());
    (set("GITHUB_ACTIONS") && set("ACTIONS_ID_TOKEN_REQUEST_URL")) || (set("GITLAB_CI") && set("SIGSTORE_ID_TOKEN"))
}

/// Checks the directory the build wrote: its `package.json` is the package being published and every file
/// it points at exists.
fn check_package(dir: &Path, name: &str, version: &str) -> Result<()> {
    let path = dir.join("package.json");
    let text = std::fs::read_to_string(&path).with_context(|| format!("the build did not write {}", path.display()))?;
    let json: serde_json::Value = serde_json::from_str(&text).with_context(|| format!("{} is not valid JSON", path.display()))?;
    if json["name"] != name || json["version"] != version {
        bail!("{} is for `{}` v{}, not `{name}` v{version}; build the library again", path.display(), json["name"], json["version"]);
    }
    for field in ["main", "module", "types"] {
        let Some(file) = json[field].as_str() else { continue };
        if !dir.join(file.trim_start_matches("./")).is_file() {
            bail!(
                "`{field}` of package.json is `{file}`, but the build did not write {}",
                dir.join(file.trim_start_matches("./")).display()
            );
        }
    }
    let present: Vec<String> = std::fs::read_dir(dir)?.filter_map(|e| e.ok()?.file_name().into_string().ok()).collect();
    for pattern in json["files"].as_array().into_iter().flatten().filter_map(|p| p.as_str()) {
        // Source maps are optional.
        if !pattern.ends_with(".map") && !present.iter().any(|file| glob(pattern, file)) {
            bail!("`files` of package.json lists `{pattern}`, which matches nothing in {}", dir.display());
        }
    }
    Ok(())
}

/// Whether `name` matches `pattern`, which has at most one `*`.
fn glob(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        Some((head, tail)) => name.len() >= head.len() + tail.len() && name.starts_with(head) && name.ends_with(tail),
        None => pattern == name,
    }
}

/// `@acme/hello` as a file name: `acme-hello`.
fn file_name(name: &str) -> String {
    name.trim_start_matches('@').replace('/', "-")
}

/// What is published and how.
struct Package<'a> {
    name: &'a str,
    /// The directory of the package, where `package.json` is.
    dir: &'a Path,
    manager: PackageManager,
    exe: &'a Path,
    registry: &'a str,
    token: Option<&'a str>,
    tag: Option<&'a str>,
    access: Option<NpmAccess>,
    otp: Option<&'a str>,
    provenance: bool,
}

impl Package<'_> {
    /// Packs the package into `dest` and returns the tarball.
    fn pack(&self, shell: &Shell, dest: &Path) -> Result<PathBuf> {
        remove_dir_all_if_exists(dest)?;
        std::fs::create_dir_all(dest).with_context(|| format!("failed to create {}", dest.display()))?;
        let mut cmd = Command::new(self.exe);
        cmd.args(pack_args(self.manager, dest)).current_dir(self.dir);
        shell.verbose("Running", format!("`{}`", process::describe(&cmd)));
        run_quietly(&mut cmd, self.manager, "pack")?;
        let mut tarballs: Vec<PathBuf> =
            std::fs::read_dir(dest)?.filter_map(|e| Some(e.ok()?.path())).filter(|p| p.extension().is_some_and(|e| e == "tgz")).collect();
        tarballs.sort();
        tarballs.pop().with_context(|| format!("`{} pack` wrote no tarball in {}", self.manager, dest.display()))
    }

    /// Publishes `tarball`, with the token (when there is one) in a temporary `.npmrc` for the length of the upload.
    fn upload(&self, shell: &Shell, tarball: &Path) -> Result<()> {
        let rc = match self.token {
            Some(token) => Some(TemporaryNpmrc::write(self.dir, self.registry, self.name, token)?),
            None => None,
        };
        let args = publish_args(self, tarball);
        let mut cmd = Command::new(self.exe);
        cmd.args(&args).current_dir(self.dir);
        shell.verbose("Running", format!("`{} {}`", self.manager, mask_otp(&args)));
        let result = run_quietly(&mut cmd, self.manager, "publish");
        drop(rc);
        result
    }
}

/// The arguments that pack the package in the current directory into `dest`, without lifecycle scripts.
fn pack_args(manager: PackageManager, dest: &Path) -> Vec<OsString> {
    let dest = dest.as_os_str().to_owned();
    match manager {
        PackageManager::Npm => vec!["pack".into(), "--ignore-scripts".into(), "--pack-destination".into(), dest],
        PackageManager::Pnpm => vec!["pack".into(), "--pack-destination".into(), dest],
        PackageManager::Bun => vec!["pm".into(), "pack".into(), "--destination".into(), dest],
    }
}

/// The arguments that publish `tarball`.
fn publish_args(package: &Package, tarball: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = match package.manager {
        PackageManager::Npm => vec!["publish".into(), tarball.into(), "--ignore-scripts".into()],
        // pnpm checks the git branch before publishing a directory; a tarball is what it is.
        PackageManager::Pnpm => vec!["publish".into(), tarball.into(), "--no-git-checks".into()],
        PackageManager::Bun => vec!["publish".into(), tarball.into()],
    };
    let mut flag = |name: &str, value: &str| {
        args.push(name.into());
        args.push(value.into());
    };
    flag("--registry", package.registry);
    if let Some(tag) = package.tag {
        flag("--tag", tag);
    }
    if let Some(access) = package.access {
        flag("--access", access.as_str());
    }
    if let Some(otp) = package.otp {
        flag("--otp", otp);
    }
    if package.provenance {
        args.push("--provenance".into());
    }
    args
}

/// The arguments for a log line: the one-time password is not for the log.
fn mask_otp(args: &[OsString]) -> String {
    let mut out = Vec::new();
    let mut hide = false;
    for arg in args {
        let arg = arg.to_string_lossy();
        out.push(if std::mem::take(&mut hide) { "******".to_owned() } else { process::quote_for_display(&arg).into_owned() });
        hide = arg == "--otp";
    }
    out.join(" ")
}

/// Runs `cmd`, showing what the package manager printed only when it fails.
fn run_quietly(cmd: &mut Command, manager: PackageManager, what: &str) -> Result<()> {
    let output = cmd.output().with_context(|| format!("failed to start {manager}"))?;
    if output.status.success() {
        return Ok(());
    }
    let log = format!("{}\n{}", String::from_utf8_lossy(&output.stdout).trim(), String::from_utf8_lossy(&output.stderr).trim());
    let hint = failure_hint(&log).map(|h| format!("\n\n{h}")).unwrap_or_default();
    bail!("`{manager} {what}` failed with {}{hint}\n\n{}", describe_status(output.status), log.trim())
}

/// What to do about a failed publish, for the failures that have a known fix.
fn failure_hint(log: &str) -> Option<&'static str> {
    let log = log.to_ascii_lowercase();
    if log.contains("eotp") || log.contains("one-time password") || log.contains("otp") {
        Some("the registry wants a one-time password: pass `--otp <code>`")
    } else if log.contains("eneedauth") || log.contains("e401") || log.contains("unauthorized") {
        Some(
            "the registry did not accept the credentials: set `NPM_TOKEN` to a token that can publish, or log in with your package manager",
        )
    } else if log.contains("previously published") || log.contains("cannot publish over") {
        Some("this version is already published: bump `version` in [package]")
    } else if log.contains("402") || log.contains("private packages") {
        Some(
            "a scoped package is private by default, and the account cannot publish private packages: set `access = \"public\"` in [publish.npm]",
        )
    } else if log.contains("e403") || log.contains("forbidden") {
        Some("the registry refused the publish: check that the token can publish this package or scope")
    } else {
        None
    }
}

/// A `.npmrc` with the registry and the token, in the package's directory, removed when dropped. The package
/// manager reads the `.npmrc` of the directory it runs in, over the user's own, so nothing else is touched.
struct TemporaryNpmrc {
    path: PathBuf,
}

impl TemporaryNpmrc {
    fn write(dir: &Path, registry: &str, name: &str, token: &str) -> Result<Self> {
        use std::io::Write;
        let path = dir.join(".npmrc");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&path)
            .with_context(|| format!("failed to create {}; remove it if an earlier publish was interrupted", path.display()))?;
        let mut text = format!("registry={registry}\n");
        if let Some(scope) = name.strip_prefix('@').and_then(|n| n.split('/').next()) {
            text.push_str(&format!("@{scope}:registry={registry}\n"));
        }
        text.push_str(&format!("{}:_authToken={token}\n", auth_key(registry)));
        // The file exists from here on, so it is removed even if the write fails.
        let rc = Self { path };
        file.write_all(text.as_bytes()).with_context(|| format!("failed to write {}", rc.path.display()))?;
        Ok(rc)
    }
}

impl Drop for TemporaryNpmrc {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Prints the files of `tarball` with their sizes.
fn list_tarball(shell: &Shell, tarball: &Path) -> Result<()> {
    let files = tarball_files(tarball)?;
    shell.status(
        "Packed",
        format!("{} files, {} ({})", files.len(), crate::wasm_output::human_size(tarball_size(tarball)), tarball.display()),
    );
    for (path, size) in &files {
        shell.status("", format!("{path}  {}", crate::wasm_output::human_size(*size)));
    }
    Ok(())
}

fn tarball_size(tarball: &Path) -> u64 {
    std::fs::metadata(tarball).map_or(0, |m| m.len())
}

/// The files of an npm tarball (they sit under `package/`) with their sizes.
fn tarball_files(tarball: &Path) -> Result<Vec<(String, u64)>> {
    let file = std::fs::File::open(tarball).with_context(|| format!("failed to open {}", tarball.display()))?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let mut files = Vec::new();
    for entry in archive.entries().with_context(|| format!("failed to read {}", tarball.display()))? {
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path()?.to_string_lossy().replace('\\', "/");
        // The size is what the entry holds, whatever its header says.
        let mut size = 0;
        let mut buffer = [0u8; 8192];
        loop {
            let read = entry.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            size += read as u64;
        }
        files.push((path.strip_prefix("package/").unwrap_or(&path).to_owned(), size));
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package<'a>(exe: &'a Path, dir: &'a Path) -> Package<'a> {
        Package {
            name: "@acme/hello",
            dir,
            manager: PackageManager::Npm,
            exe,
            registry: "https://npm.example.com",
            token: Some("s3cret"),
            tag: Some("next"),
            access: Some(NpmAccess::Public),
            otp: Some("123456"),
            provenance: true,
        }
    }

    fn strings(args: Vec<OsString>) -> Vec<String> {
        args.into_iter().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn pre_releases_need_a_tag() {
        assert!(check_version("1.2.3", None).is_ok());
        assert!(check_version("1.2.3+build.5", None).is_ok());
        assert!(check_version("1.2.3-beta.1", Some("next")).is_ok());
        assert!(check_version("1.2.3-beta.1", None).unwrap_err().to_string().contains("--tag next"));
        for bad in ["1.0", "v1.2.3", "1.2.3.4", "1.0-SNAPSHOT"] {
            assert!(check_version(bad, Some("next")).unwrap_err().to_string().contains("SemVer"), "{bad}");
        }
    }

    #[test]
    fn provenance_needs_a_signing_ci() {
        let github = |name: &str| matches!(name, "GITHUB_ACTIONS" | "ACTIONS_ID_TOKEN_REQUEST_URL").then(|| "true".to_owned());
        let plain = |_: &str| None;
        assert!(check_provenance(PackageManager::Npm, false, &github).is_ok());
        assert!(check_provenance(PackageManager::Pnpm, false, &github).is_ok());
        assert!(check_provenance(PackageManager::Npm, true, &plain).is_ok(), "a dry run signs nothing");
        assert!(check_provenance(PackageManager::Npm, false, &plain).unwrap_err().to_string().contains("id-token: write"));
        assert!(check_provenance(PackageManager::Bun, true, &github).unwrap_err().to_string().contains("bun"));
        let without_identity = |name: &str| (name == "GITHUB_ACTIONS").then(|| "true".to_owned());
        assert!(check_provenance(PackageManager::Npm, false, &without_identity).is_err(), "the job lacks `id-token: write`");
    }

    #[test]
    fn commands_per_package_manager() {
        let dest = Path::new("out");
        assert_eq!(strings(pack_args(PackageManager::Npm, dest)), ["pack", "--ignore-scripts", "--pack-destination", "out"]);
        assert_eq!(strings(pack_args(PackageManager::Pnpm, dest)), ["pack", "--pack-destination", "out"]);
        assert_eq!(strings(pack_args(PackageManager::Bun, dest)), ["pm", "pack", "--destination", "out"]);

        let (exe, dir) = (Path::new("npm"), Path::new("."));
        let mut p = package(exe, dir);
        assert_eq!(
            strings(publish_args(&p, Path::new("a.tgz"))),
            [
                "publish",
                "a.tgz",
                "--ignore-scripts",
                "--registry",
                "https://npm.example.com",
                "--tag",
                "next",
                "--access",
                "public",
                "--otp",
                "123456",
                "--provenance"
            ]
        );
        p.manager = PackageManager::Pnpm;
        assert!(strings(publish_args(&p, Path::new("a.tgz"))).contains(&"--no-git-checks".to_owned()));
        p.manager = PackageManager::Bun;
        (p.tag, p.access, p.otp, p.provenance) = (None, None, None, false);
        assert_eq!(strings(publish_args(&p, Path::new("a.tgz"))), ["publish", "a.tgz", "--registry", "https://npm.example.com"]);
    }

    #[test]
    fn the_one_time_password_stays_out_of_the_log() {
        let args: Vec<OsString> = ["publish", "a.tgz", "--otp", "123456", "--tag", "next"].into_iter().map(Into::into).collect();
        let shown = mask_otp(&args);
        assert!(shown.contains("--otp ******") && shown.contains("--tag next") && !shown.contains("123456"), "{shown}");
    }

    #[test]
    fn known_failures_have_hints() {
        assert!(failure_hint("npm error code EOTP").unwrap().contains("--otp"));
        assert!(failure_hint("npm error code ENEEDAUTH").unwrap().contains("NPM_TOKEN"));
        assert!(failure_hint("You cannot publish over the previously published versions: 1.0.0.").unwrap().contains("bump"));
        assert!(failure_hint("npm error 402 Payment Required - You must sign up for private packages").unwrap().contains("access"));
        assert!(failure_hint("something else").is_none());
    }

    #[test]
    fn names_become_file_names() {
        assert_eq!(file_name("@acme/hello"), "acme-hello");
        assert_eq!(file_name("hello"), "hello");
    }

    #[test]
    fn patterns_have_one_star() {
        assert!(glob("*.mjs", "hello.mjs") && glob("*.d.mts", "hello.d.mts") && glob("hello.wasm", "hello.wasm"));
        assert!(!glob("*.mjs", "hello.mjs.map") && !glob("*.wasm", "hello.mjs") && !glob("hello.wasm", "other.wasm"));
    }

    fn write(dir: &Path, name: &str, text: &str) {
        std::fs::write(dir.join(name), text).unwrap();
    }

    #[test]
    fn the_package_is_checked_before_it_is_packed() {
        let dir = tempfile::tempdir().unwrap();
        let json = |extra: &str| {
            format!(
                r#"{{ "name": "@acme/hello", "version": "1.2.3", "main": "./hello.mjs", "types": "./hello.d.mts",
                "files": ["*.mjs", "*.wasm", "*.d.mts", "*.mjs.map"]{extra} }}"#
            )
        };
        let check = || check_package(dir.path(), "@acme/hello", "1.2.3").map_err(|e| e.to_string());
        assert!(check().unwrap_err().contains("did not write"), "no package.json");
        write(dir.path(), "package.json", &json(""));
        assert!(check().unwrap_err().contains("`main` of package.json is `./hello.mjs`"));
        write(dir.path(), "hello.mjs", "");
        assert!(check().unwrap_err().contains("`types`"));
        write(dir.path(), "hello.d.mts", "");
        assert!(check().unwrap_err().contains("`*.wasm`"), "{:?}", check());
        write(dir.path(), "hello.wasm", "");
        assert_eq!(check(), Ok(()), "a missing source map is fine");
        assert!(check_package(dir.path(), "@acme/other", "1.2.3").unwrap_err().to_string().contains("build the library again"));
        assert!(check_package(dir.path(), "@acme/hello", "2.0.0").is_err(), "an old build");
    }

    #[test]
    fn lists_the_files_of_a_tarball() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hello-1.2.3.tgz");
        let mut builder =
            tar::Builder::new(flate2::write::GzEncoder::new(std::fs::File::create(&path).unwrap(), flate2::Compression::default()));
        for (name, body) in [("package/package.json", "{}"), ("package/hello.mjs", "export {}")] {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, body.as_bytes()).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
        assert_eq!(tarball_files(&path).unwrap(), [("package.json".to_owned(), 2), ("hello.mjs".to_owned(), 9)]);
    }

    /// A package manager that records its arguments and the `.npmrc` it runs with, and succeeds or fails.
    fn fake_manager(dir: &Path, exit: i32) -> (PathBuf, PathBuf) {
        let log = dir.join("log.txt");
        let (exe, script) = if cfg!(windows) {
            (
                dir.join("fake-npm.cmd"),
                format!(
                    "@echo off\r\necho %* > \"{0}\"\r\ntype .npmrc >> \"{0}\"\r\necho EOTP 1>&2\r\nexit /b {1}\r\n",
                    log.display(),
                    exit
                ),
            )
        } else {
            (
                dir.join("fake-npm"),
                format!("#!/bin/sh\necho \"$@\" > '{0}'\ncat .npmrc >> '{0}'\necho EOTP >&2\nexit {1}\n", log.display(), exit),
            )
        };
        std::fs::write(&exe, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        (exe, log)
    }

    #[test]
    fn the_token_lives_in_a_temporary_npmrc() {
        let work = tempfile::tempdir().unwrap();
        let package_dir = work.path().join("package");
        std::fs::create_dir(&package_dir).unwrap();
        let (exe, log) = fake_manager(work.path(), 0);
        let shell = Shell::default();
        package(&exe, &package_dir).upload(&shell, Path::new("hello.tgz")).unwrap();

        let seen = std::fs::read_to_string(log).unwrap();
        assert!(seen.contains("publish hello.tgz") && seen.contains("--tag next") && seen.contains("--provenance"), "{seen}");
        assert!(seen.contains("registry=https://npm.example.com"), "{seen}");
        assert!(seen.contains("@acme:registry=https://npm.example.com"), "the scope too:\n{seen}");
        assert!(seen.contains("//npm.example.com/:_authToken=s3cret"), "{seen}");
        assert!(!package_dir.join(".npmrc").exists(), "the token does not stay on disk");

        // No token: the user's own .npmrc is used, and nothing is written.
        let mut anonymous = package(&exe, &package_dir);
        anonymous.token = None;
        std::fs::write(package_dir.join(".npmrc"), "user=1\n").unwrap();
        anonymous.upload(&shell, Path::new("hello.tgz")).unwrap();
        assert_eq!(std::fs::read_to_string(package_dir.join(".npmrc")).unwrap(), "user=1\n", "a .npmrc that is there stays");
    }

    #[cfg(unix)]
    #[test]
    fn the_temporary_npmrc_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let rc = TemporaryNpmrc::write(dir.path(), "https://npm.example.com", "hello", "s3cret").unwrap();
        assert_eq!(std::fs::metadata(dir.path().join(".npmrc")).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(!std::fs::read_to_string(dir.path().join(".npmrc")).unwrap().contains("@"), "an unscoped name has no scope line");
        drop(rc);
        assert!(!dir.path().join(".npmrc").exists());
    }

    #[test]
    fn a_failed_upload_explains_and_still_removes_the_token() {
        let work = tempfile::tempdir().unwrap();
        let package_dir = work.path().join("package");
        std::fs::create_dir(&package_dir).unwrap();
        let (exe, _) = fake_manager(work.path(), 1);
        let error = package(&exe, &package_dir).upload(&Shell::default(), Path::new("hello.tgz")).unwrap_err().to_string();
        assert!(error.contains("`npm publish` failed") && error.contains("--otp <code>"), "{error}");
        assert!(!error.contains("s3cret"), "the token is not in the message");
        assert!(!package_dir.join(".npmrc").exists());

        // An earlier run that left its file behind is reported, not overwritten.
        std::fs::write(package_dir.join(".npmrc"), "mine\n").unwrap();
        let error = package(&exe, &package_dir).upload(&Shell::default(), Path::new("hello.tgz")).unwrap_err().to_string();
        assert!(error.contains("failed to create"), "{error}");
        assert_eq!(std::fs::read_to_string(package_dir.join(".npmrc")).unwrap(), "mine\n");
    }
}
