//! `kargo publish`: builds a library with the release profile and publishes it to a Maven
//! repository: the local one (`~/.m2/repository`), a `[repositories]` entry (a directory or an
//! HTTP repository such as Nexus, Artifactory or GitHub Packages), or Maven Central. The npm
//! package of a Wasm or Kotlin/JS library goes to an npm registry ([`super::publish_npm`]);
//! [`publish_package`] runs whichever of the two the package and the flags ask for.
//!
//! Every module is first staged in `target/release/publish/repository` with its POM, Gradle module
//! metadata, sources and javadoc jars, signatures and checksums (see [`crate::publication`]), then
//! copied to the destination. `--dry-run` stops after staging.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};

use super::build::{
    build_jvm, compile_js, compile_wasm, finished, hook_env, jvm_classpaths, native_cinterop, native_cinterop_defs, native_klib,
    native_platforms, native_resolution, warn_about_sources, wasm_resolution,
};
use super::context::{BuildContext, merge_deps};
use crate::env::EnvOptions;
use crate::publication::{
    DependencyScope, PlatformKind, PlatformPublication, Publication, PublishedDependency, StagedModule, is_multiplatform, platform_module,
};
use crate::signing::Signer;
use crate::workspace::{Project, Workspace};
use kargo_compiler::jar::{JarManifest, write_jar};
use kargo_manifest::{
    CentralPublishing, Dependency, GeneratedKind, Hook, Javadoc, MAVEN_CENTRAL_DESTINATION, Manifest, PublishKind, Scope, TargetKind,
};
use kargo_maven::central::{self, Portal, PublishingType};
use kargo_maven::deploy::{DeployFile, Deployer};
use kargo_maven::repo::MAVEN_LOCAL;
use kargo_maven::{ModuleId, Repository, Resolution};
use kargo_util::fs::{collect_files, remove_dir_all_if_exists};
use kargo_util::hash;

#[derive(Debug, Clone, Default)]
pub struct PublishOptions {
    /// Where to publish: a `[repositories]` name, `maven-central` or `maven-local`;
    /// `None` uses `[publish] repository`.
    pub repository: Option<String>,
    /// Build, stage and sign everything, but upload nothing.
    pub dry_run: bool,
    /// Which of Maven and npm to publish to.
    pub only: Only,
    /// The npm registry, dist-tag and one-time password of the command line (`--registry`, `--tag`, `--otp`).
    pub npm: super::publish_npm::NpmOptions,
}

/// Which kinds of repository `kargo publish` publishes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Only {
    /// Maven when a repository is given or set in `[publish]`, npm when `[publish.npm]` is set: both when both are.
    #[default]
    Configured,
    /// `--maven`.
    Maven,
    /// `--npm`.
    Npm,
}

/// Where a package is published, as (Maven, npm).
pub fn destinations(manifest: &Manifest, opts: &PublishOptions) -> (bool, bool) {
    let npm_set = manifest.publish.npm.is_some();
    match opts.only {
        Only::Maven => (true, false),
        Only::Npm => (false, true),
        // A package with nothing configured goes down the Maven path, which says what to configure.
        Only::Configured => (opts.repository.is_some() || manifest.publish.repository.is_some() || !npm_set, npm_set),
    }
}

/// `kargo publish` for one package: to Maven, to npm, or to both. Maven goes first.
pub fn publish_package(ctx: &BuildContext, opts: &PublishOptions) -> Result<()> {
    let (maven, npm) = destinations(ctx.ws.manifest(), opts);
    if maven {
        publish(ctx, opts)?;
    }
    if npm {
        super::publish_npm::publish(ctx, &super::publish_npm::NpmOptions { dry_run: opts.dry_run, ..opts.npm.clone() })?;
    }
    Ok(())
}

/// Where a package is published.
#[derive(Debug, Clone)]
enum Destination {
    /// Maven Central, through the Central Portal.
    Central,
    /// `~/.m2/repository`, written like `mvn install`.
    Local(Repository),
    /// A `[repositories]` entry, written like `mvn deploy`.
    Repository(Repository),
}

impl Destination {
    fn resolve(manifest: &Manifest, name: &str) -> Result<Self> {
        if name == MAVEN_CENTRAL_DESTINATION {
            return Ok(Destination::Central);
        }
        if name == MAVEN_LOCAL {
            return Ok(Destination::Local(Repository::maven_local()?));
        }
        match manifest.repositories.iter().find(|r| r.name == name) {
            Some(repo) => Ok(Destination::Repository(repo.clone())),
            None => {
                let mut names = vec![MAVEN_CENTRAL_DESTINATION, MAVEN_LOCAL];
                names.extend(manifest.repositories.iter().map(|r| r.name.as_str()).filter(|n| *n != MAVEN_CENTRAL_DESTINATION));
                bail!("no repository named `{name}` to publish to (expected one of: {}); declare it in [repositories]", names.join(", "))
            }
        }
    }

    fn describe(&self) -> String {
        match self {
            Destination::Central => "Maven Central".to_owned(),
            Destination::Local(repo) => format!("{MAVEN_LOCAL} ({})", repo.local_root().display()),
            Destination::Repository(repo) => format!("`{}` ({})", repo.name, repo.url),
        }
    }
}

/// The packages of `selected` in the order they are published: path dependencies before
/// the packages that depend on them.
pub fn publish_order(project: &Project, selected: &[usize]) -> Vec<usize> {
    fn visit(project: &Project, selected: &[usize], index: usize, seen: &mut Vec<usize>, out: &mut Vec<usize>) {
        if seen.contains(&index) {
            return;
        }
        seen.push(index);
        for (_, dep) in project.package(index).path_dependencies() {
            visit(project, selected, *dep, seen, out);
        }
        // An npm package is published after the packages it depends on.
        for dep in crate::npm_package::member_dependencies(project, index) {
            visit(project, selected, dep, seen, out);
        }
        if selected.contains(&index) {
            out.push(index);
        }
    }
    let (mut seen, mut out) = (Vec::new(), Vec::new());
    for &index in selected {
        visit(project, selected, index, &mut seen, &mut out);
    }
    out
}

/// `kargo publish` for one package. Returns the staged modules.
pub fn publish(ctx: &BuildContext, opts: &PublishOptions) -> Result<Vec<StagedModule>> {
    let start = Instant::now();
    let ws = ctx.ws;
    let manifest = ws.manifest();
    let pkg = &manifest.package;
    if !pkg.publish {
        bail!("`{}` has `publish = false` in [package] and cannot be published", pkg.name);
    }
    let group = pkg.group.clone().with_context(|| {
        format!("`{}` has no Maven group to publish under; set `group` in [package], e.g. `group = \"com.example\"`", pkg.name)
    })?;
    let name = opts.repository.clone().or_else(|| manifest.publish.repository.clone()).with_context(|| {
        format!(
            "no repository to publish `{}` to; pass `--repository <name>` (`maven-central`, `maven-local` or a [repositories] entry), \
             pass `--local`, or set `repository` in [publish]",
            pkg.name
        )
    })?;
    let destination = Destination::resolve(manifest, &name)?;
    let central = matches!(destination, Destination::Central);
    let bom = manifest.publish.kind == PublishKind::Bom;
    let sign = manifest.publish.sign.unwrap_or(central);
    if central {
        check_central(ws, sign, bom)?;
        if !opts.dry_run {
            central::credentials_from_env()?;
        }
    }
    for (dep, index) in ws.path_dependencies().iter().filter(|(d, _)| !d.dev) {
        let other = ctx.project.package(*index).manifest();
        if !other.package.publish {
            bail!(
                "`{}` depends on `{}`, which has `publish = false`; consumers of `{}` could not download it",
                pkg.name,
                dep.name,
                pkg.name
            );
        }
        if other.package.group.is_none() {
            bail!("`{}` depends on `{}`, which has no `group` in [package]; set one so it can be published too", pkg.name, dep.name);
        }
    }
    let root = ModuleId::new(&group, &pkg.name);
    if manifest.publish.relocations.contains(&root) {
        bail!("[publish] `relocations` lists `{root}`, the package's own coordinates; list the coordinates it was published under before");
    }
    // Fail on a missing key before compiling anything.
    let signer = if sign { Some(Signer::from_env(ctx.shell())?) } else { None };

    let work = ws.profile_dir(&ctx.profile).join("publish");
    remove_dir_all_if_exists(&work)?;
    let repo_dir = work.join("repository");
    let lone = Publication { group: group.clone(), package: pkg, platforms: Vec::new(), common_sources: None, common_javadoc: None };
    let mut staged = if bom {
        ctx.shell().status("Packaging", format!("{} v{} (bom)", pkg.name, pkg.version));
        vec![lone.stage_bom(&repo_dir, &bom_constraints(ctx)?)?]
    } else {
        stage_library(ctx, &group, &work, &repo_dir)?
    };
    for from in &manifest.publish.relocations {
        staged.push(lone.stage_relocation(&repo_dir, from)?);
    }
    let staged = finish_files(ctx, staged, signer.as_ref())?;
    let files: usize = staged.iter().map(|m| m.files.len()).sum();

    if opts.dry_run {
        list_staged(ctx, &staged)?;
        ctx.shell().status("Packaged", format!("{files} files in {}", repo_dir.display()));
        finished(ctx, start);
        ctx.shell().warn(format!("aborting upload to {} due to dry run", destination.describe()));
        return Ok(staged);
    }

    let coordinates = format!("{group}:{}:{}", pkg.name, pkg.version);
    match &destination {
        Destination::Central => publish_to_central(ctx, &repo_dir, &work, &coordinates, files)?,
        Destination::Local(repo) | Destination::Repository(repo) => {
            let local = matches!(destination, Destination::Local(_));
            let deployer = if local { Deployer::maven_local(&ctx.client, repo) } else { Deployer::new(&ctx.client, repo) };
            let mut uploaded = 0;
            for module in &staged {
                // A release in a shared repository is never overwritten; publishing it again is a no-op.
                if !local && !kargo_maven::coord::is_snapshot(&module.version) {
                    match published_already(ctx, repo, module)? {
                        Some(true) => {
                            ctx.shell().status("Skipping", format!("{} v{} (already published, unchanged)", module.module, module.version));
                            continue;
                        }
                        Some(false) => bail!(
                            "{}:{} is already published to {} with different contents; a release cannot change, so bump \
                             `version` in [package] (or publish a -SNAPSHOT version while it is in progress)",
                            module.module,
                            module.version,
                            destination.describe()
                        ),
                        None => {}
                    }
                }
                ctx.shell().status("Uploading", format!("{} v{} ({} files)", module.module, module.version, module.files.len()));
                let paths: Vec<PathBuf> = module.files.iter().map(|f| module.dir.join(f)).collect();
                let files: Vec<DeployFile> =
                    module.files.iter().zip(&paths).map(|(name, path)| DeployFile { name: name.clone(), path }).collect();
                deployer.deploy(&module.module, &module.version, &files)?;
                uploaded += 1;
            }
            if uploaded == 0 {
                ctx.shell().status("Unchanged", format!("{coordinates} is already in {}", destination.describe()));
            } else {
                ctx.shell().status("Published", format!("{coordinates} to {}", destination.describe()));
            }
        }
    }
    finished(ctx, start);
    Ok(staged)
}

/// Builds every publishable target and stages the library's modules in `repo_dir`.
fn stage_library(ctx: &BuildContext, group: &str, work: &Path, repo_dir: &Path) -> Result<Vec<StagedModule>> {
    let ws = ctx.ws;
    let manifest = ws.manifest();
    let pkg = &manifest.package;
    let targets = publishable_targets(ctx)?;
    super::build::prepare_native_hooks(ctx, &targets)?;
    let env = hook_env(ctx, &EnvOptions::default())?;
    crate::plugins::run_hook(ctx.config, ws, Hook::PreBuild, &env)?;
    super::build::generate_sources(ctx, &targets)?;
    warn_about_sources(ctx, &targets);
    let jars = work.join("jars");
    std::fs::create_dir_all(&jars).with_context(|| format!("failed to create {}", jars.display()))?;
    let platforms = build_platforms(ctx, &targets, &jars)?;
    crate::plugins::run_hook(ctx.config, ws, Hook::PostBuild, &env)?;

    let kinds: Vec<PlatformKind> = platforms.iter().map(|p| p.kind.clone()).collect();
    let multiplatform = is_multiplatform(&kinds);
    let suffixes: Vec<String> = kinds.iter().map(PlatformKind::module_suffix).collect();
    ctx.shell().status("Packaging", format!("{} v{} ({})", pkg.name, pkg.version, suffixes.join(", ")));
    let javadoc = match manifest.publish.javadoc {
        Javadoc::Off => None,
        Javadoc::Empty => Some(empty_javadoc_jar(ctx, &jars)?),
        Javadoc::Dokka => Some(dokka_javadoc_jar(ctx, &targets, &jars)?),
    };
    let sources = manifest.publish.sources;
    let mut publication = Publication {
        group: group.to_owned(),
        package: pkg,
        platforms: Vec::new(),
        common_sources: if sources && multiplatform {
            let dirs: Vec<(&str, PathBuf)> = ws.common_source_dirs().into_iter().map(|d| ("commonMain", d)).collect();
            Some(sources_jar(&jars, "common", &dirs)?)
        } else {
            None
        },
        common_javadoc: javadoc.clone().filter(|_| multiplatform),
    };
    for (platform, target) in platforms.into_iter().zip(targets_per_platform(&targets, &kinds)) {
        let mut platform = platform;
        if sources {
            let mut all: Vec<(String, PathBuf)> = Vec::new();
            for d in ws.common_source_dirs() {
                all.push((if multiplatform { "commonMain".to_owned() } else { String::new() }, d));
            }
            for d in ws.target_source_dirs(target) {
                all.push((if multiplatform { platform_source_root(&platform.kind).to_owned() } else { String::new() }, d));
            }
            for g in ws.manifest().all_groups_for(target) {
                for d in ws.group_source_dirs(&g.name) {
                    all.push((if multiplatform { format!("{}Main", g.name) } else { String::new() }, d));
                }
            }
            let refs: Vec<(&str, PathBuf)> = all.iter().map(|(p, d)| (p.as_str(), d.clone())).collect();
            // What `[[generate]]` steps wrote is part of the library's code too.
            let generated = if multiplatform { platform_source_root(&platform.kind) } else { "" };
            let mut with_gen = refs;
            with_gen
                .extend(crate::generate::dirs(ws, &ctx.profile, target, GeneratedKind::Sources).into_iter().map(|dir| (generated, dir)));
            platform.sources = Some(sources_jar(&jars, &platform.kind.module_suffix(), &with_gen)?);
        }
        platform.javadoc = javadoc.clone();
        publication.platforms.push(platform);
    }
    publication.stage(repo_dir)
}

/// What a bill of materials pins: every `[dependencies]` entry at its declared version (BOMs are
/// imported), and every module that a path dependency publishes, at that package's version.
fn bom_constraints(ctx: &BuildContext) -> Result<Vec<PublishedDependency>> {
    let manifest = ctx.ws.manifest();
    let mut out = Vec::new();
    for dep in &manifest.dependencies {
        let version = dep.version.clone().with_context(|| {
            format!("`{}` has no version; every dependency of a BOM (`kind = \"bom\"` in [publish]) needs one", dep.name)
        })?;
        out.push(PublishedDependency {
            module: dep.module.clone(),
            pom_module: dep.module.clone(),
            version,
            scope: DependencyScope::Compile,
            classifier: dep.classifier.clone(),
            extension: dep.extension.clone(),
            exclusions: Vec::new(),
            transitive: true,
            bom: dep.bom,
        });
    }
    for (_, index) in ctx.ws.path_dependencies().iter().filter(|(d, _)| !d.dev) {
        let other = ctx.project.package(*index).manifest();
        for module in published_modules(other)? {
            out.push(PublishedDependency {
                module: module.clone(),
                pom_module: module,
                version: other.package.version.clone(),
                scope: DependencyScope::Compile,
                classifier: None,
                extension: None,
                exclusions: Vec::new(),
                transitive: true,
                bom: other.publish.kind == PublishKind::Bom,
            });
        }
    }
    if out.is_empty() {
        bail!(
            "the BOM `{}` manages no versions; add the libraries it pins to [dependencies], or the workspace members as path dependencies",
            manifest.package.name
        );
    }
    Ok(out)
}

/// The modules `kargo publish` writes for a package: its root module, plus one per platform for a
/// multiplatform library.
fn published_modules(manifest: &Manifest) -> Result<Vec<ModuleId>> {
    let pkg = &manifest.package;
    let group = pkg.group.clone().with_context(|| format!("`{}` has no `group` in [package]", pkg.name))?;
    let root = ModuleId::new(&group, &pkg.name);
    if manifest.publish.kind == PublishKind::Bom {
        return Ok(vec![root]);
    }
    let android_library = manifest.targets.android.as_ref().is_some_and(|a| a.config.kind == "library");
    let mut kinds = Vec::new();
    for target in manifest.targets.enabled() {
        match target {
            TargetKind::Jvm => kinds.push(PlatformKind::Jvm { jvm_version: None }),
            TargetKind::Wasm => {
                let environment = manifest.targets.wasm.as_ref().map(|w| w.environment.platform().to_owned()).unwrap_or_default();
                kinds.push(PlatformKind::Wasm { environment });
            }
            TargetKind::Js => kinds.push(PlatformKind::Js),
            TargetKind::Native => {
                for platform in manifest.targets.native.iter().flat_map(|n| &n.platforms) {
                    let konan_target = kargo_toolchain::native::resolve_platform(platform)?;
                    if !kinds.contains(&PlatformKind::Native { konan_target: konan_target.clone() }) {
                        kinds.push(PlatformKind::Native { konan_target });
                    }
                }
            }
            TargetKind::Android if android_library => kinds.push(PlatformKind::Android),
            TargetKind::Android => {}
        }
    }
    let multiplatform = is_multiplatform(&kinds);
    let mut out = vec![root];
    if multiplatform {
        out.extend(kinds.iter().map(|kind| platform_module(&group, &pkg.name, kind, true)));
    }
    Ok(out)
}

/// Prints every staged module with its files and their SHA-256, for `--dry-run`.
fn list_staged(ctx: &BuildContext, staged: &[StagedModule]) -> Result<()> {
    for module in staged {
        ctx.shell().status("Staged", format!("{}:{}", module.module, module.version));
        for file in module.files.iter().filter(|f| !is_checksum(f)) {
            let path = module.dir.join(file);
            if file.ends_with(".asc") {
                ctx.shell().status("", file);
                continue;
            }
            let sha256 = hash::sha256_file(&path).with_context(|| format!("failed to hash {}", path.display()))?;
            ctx.shell().status("", format!("{file}  sha256:{sha256}"));
        }
    }
    Ok(())
}

/// Whether `name` is a checksum next to another file.
fn is_checksum(name: &str) -> bool {
    [".md5", ".sha1", ".sha256", ".sha512"].iter().any(|ext| name.ends_with(ext))
}

/// Whether `module`'s version is in `repo` already: `Some(true)` with the same files (by SHA-1),
/// `Some(false)` with others, `None` when it is not there.
fn published_already(ctx: &BuildContext, repo: &Repository, module: &StagedModule) -> Result<Option<bool>> {
    let dir = format!("{}/{}", module.module.repo_dir(), module.version);
    let Some(pom) = module.files.iter().find(|f| f.ends_with(".pom")) else { return Ok(None) };
    if ctx.client.read_text_from(repo, &format!("{dir}/{pom}.sha1"))?.is_none()
        && ctx.client.read_text_from(repo, &format!("{dir}/{pom}"))?.is_none()
    {
        return Ok(None);
    }
    // Signatures differ on every run; the files they sign are what must match.
    for file in module.files.iter().filter(|f| !is_checksum(f) && !f.ends_with(".asc")) {
        let local = hash::sha1_file(&module.dir.join(file)).with_context(|| format!("failed to hash {file}"))?;
        let remote = match ctx.client.read_text_from(repo, &format!("{dir}/{file}.sha1"))? {
            Some(text) => text.split_whitespace().next().unwrap_or_default().to_ascii_lowercase(),
            None => return Ok(Some(false)),
        };
        if remote != local {
            ctx.shell().verbose("Changed", format!("{dir}/{file}"));
            return Ok(Some(false));
        }
    }
    Ok(Some(true))
}

/// The targets that are published: every enabled one, but Android only for a library.
fn publishable_targets(ctx: &BuildContext) -> Result<Vec<TargetKind>> {
    let manifest = ctx.ws.manifest();
    let enabled = manifest.targets.enabled();
    let application = manifest.targets.android.as_ref().is_some_and(|a| a.config.kind != "library");
    let targets: Vec<TargetKind> = enabled.iter().copied().filter(|t| *t != TargetKind::Android || !application).collect();
    if application {
        if targets.is_empty() {
            bail!(
                "`{}` is an Android application, which `kargo publish` does not publish; set `kind = \"library\"` under \
                 [target.android] to publish an AAR, or use `kargo package` for the APK and App Bundle",
                manifest.package.name
            );
        }
        ctx.shell().warn("the android target is an application and is not published; only Android libraries are");
    }
    Ok(targets)
}

/// The fields Maven Central requires in every POM, checked before anything is built.
fn check_central(ws: &Workspace, sign: bool, bom: bool) -> Result<()> {
    let manifest = ws.manifest();
    let pkg = &manifest.package;
    let mut missing = Vec::new();
    if pkg.description.is_none() {
        missing.push("`description`");
    }
    if pkg.license.is_none() {
        missing.push("`license` (an SPDX expression such as \"Apache-2.0\")");
    }
    if pkg.authors.is_empty() {
        missing.push("`authors`");
    }
    if pkg.repository.is_none() {
        missing.push("`repository` (the URL of the source repository)");
    }
    if !missing.is_empty() {
        bail!("Maven Central requires {} in [package] of {}", missing.join(", "), ws.manifest_path().display());
    }
    if kargo_maven::coord::is_snapshot(&pkg.version) {
        bail!("Maven Central does not take SNAPSHOT versions (`{}`); publish a release version", pkg.version);
    }
    if !sign {
        bail!("Maven Central requires signed files; remove `sign = false` from [publish]");
    }
    // A BOM is only a POM, which needs neither.
    if !bom && (manifest.publish.javadoc == Javadoc::Off || !manifest.publish.sources) {
        bail!("Maven Central requires sources and javadoc jars; remove `sources = false` and `javadoc = false` from [publish]");
    }
    Ok(())
}

/// Which target each platform of `kinds` belongs to (Native has one platform per Konan target).
fn targets_per_platform(targets: &[TargetKind], kinds: &[PlatformKind]) -> Vec<TargetKind> {
    kinds
        .iter()
        .map(|k| match k {
            PlatformKind::Jvm { .. } => TargetKind::Jvm,
            PlatformKind::Wasm { .. } => TargetKind::Wasm,
            PlatformKind::Js => TargetKind::Js,
            PlatformKind::Native { .. } => TargetKind::Native,
            PlatformKind::Android => TargetKind::Android,
        })
        .filter(|t| targets.contains(t))
        .collect()
}

/// The directory a platform's own sources get in its sources jar, as Gradle names it (`jvmMain`).
fn platform_source_root(kind: &PlatformKind) -> &'static str {
    match kind {
        PlatformKind::Jvm { .. } => "jvmMain",
        PlatformKind::Wasm { .. } => "wasmMain",
        PlatformKind::Js => "jsMain",
        PlatformKind::Native { .. } => "nativeMain",
        PlatformKind::Android => "androidMain",
    }
}

/// Compiles every target and collects what each platform publishes (artifacts and dependencies).
fn build_platforms(ctx: &BuildContext, targets: &[TargetKind], jars: &Path) -> Result<Vec<PlatformPublication>> {
    let manifest = ctx.ws.manifest();
    let pkg = &manifest.package;
    let mut platforms = Vec::new();
    for &target in targets {
        if target == TargetKind::Android {
            platforms.push(android_platform(ctx)?);
            continue;
        }
        let setup = ctx.setup(target)?;
        match target {
            TargetKind::Jvm => {
                let jvm = build_jvm(ctx)?;
                // The jar `kargo build` writes points its Class-Path at lib/; a library's must not.
                let jar = jars.join(format!("{}-{}.jar", pkg.name, pkg.version));
                let jar_manifest = JarManifest {
                    main_class: None,
                    class_path: Vec::new(),
                    implementation_title: Some(pkg.name.clone()),
                    implementation_version: Some(pkg.version.clone()),
                    ..Default::default()
                };
                let bundled = write_library_jar(ctx, &jar, &jvm, &jar_manifest)?;
                let settings = manifest.targets.jvm.as_ref().context("the JVM target is not enabled")?;
                let bytecode = settings.target_for(ctx.jvm_jdk()?.version)?;
                let jvm_version = bytecode.strip_prefix("1.").unwrap_or(&bytecode).parse().ok();
                let classpaths = jvm_classpaths(ctx, &setup)?;
                let mut dependencies =
                    published_dependencies(ctx, &setup.main, &setup.boms, target, &[&classpaths.compile, &classpaths.runtime])?;
                // What the jar bundles is not a dependency of the library anymore.
                dependencies.retain(|dep| {
                    ![&classpaths.compile, &classpaths.runtime]
                        .iter()
                        .filter_map(|r| r.node(&dep.module))
                        .any(|node| node.files.iter().any(|f| bundled.contains(f)))
                });
                platforms.push(PlatformPublication {
                    kind: PlatformKind::Jvm { jvm_version },
                    artifact: jar,
                    extra: Vec::new(),
                    sources: None,
                    javadoc: None,
                    dependencies,
                    main_class: jvm.main_class.clone(),
                    web_resources: None,
                });
            }
            TargetKind::Wasm => {
                let build =
                    compile_wasm(ctx, false)?.context("there are no Wasm sources to publish; add them under `src/` or `src-wasm/`")?;
                let resolution = wasm_resolution(ctx, &setup, false)?;
                let web_resources =
                    crate::web_resources::archive(ctx.ws, &jars.join(format!("{}-{}-web-resources.zip", pkg.name, pkg.version)))?;
                platforms.push(PlatformPublication {
                    kind: PlatformKind::Wasm { environment: build.environment.platform().to_owned() },
                    artifact: build.klib,
                    extra: Vec::new(),
                    sources: None,
                    javadoc: None,
                    dependencies: published_dependencies(ctx, &setup.main, &setup.boms, target, &[&resolution])?,
                    main_class: None,
                    web_resources,
                });
            }
            TargetKind::Js => {
                let build =
                    compile_js(ctx, false)?.context("there are no Kotlin/JS sources to publish; add them under `src/` or `src-js/`")?;
                let resolution = super::build::js_resolution(ctx, &setup, false)?;
                platforms.push(PlatformPublication {
                    kind: PlatformKind::Js,
                    artifact: build.klib,
                    extra: Vec::new(),
                    sources: None,
                    javadoc: None,
                    dependencies: published_dependencies(ctx, &setup.main, &setup.boms, target, &[&resolution])?,
                    main_class: None,
                    web_resources: None,
                });
            }
            TargetKind::Native => {
                let interops = native_cinterop_defs(ctx)?;
                for konan_target in native_platforms(ctx)? {
                    let klib = native_klib(ctx, &konan_target)?
                        .context("there are no Native sources to publish; add them under `src/` or `src-native/`")?;
                    let bindings = native_cinterop(ctx, &konan_target)?;
                    let extra =
                        interops.iter().zip(bindings).map(|((interop, _), klib)| (format!("cinterop-{}", interop.name), klib)).collect();
                    let resolution = native_resolution(ctx, &setup, &konan_target, false)?;
                    platforms.push(PlatformPublication {
                        kind: PlatformKind::Native { konan_target },
                        artifact: klib,
                        extra,
                        sources: None,
                        javadoc: None,
                        dependencies: published_dependencies(ctx, &setup.main, &setup.boms, target, &[&resolution])?,
                        main_class: None,
                        web_resources: None,
                    });
                }
            }
            TargetKind::Android => unreachable!("published above"),
        }
    }
    Ok(platforms)
}

/// The AAR of an Android library's release build, which Gradle assembles, with its dependencies as
/// `kargo.toml` declares them: Gradle resolves Android dependencies, so they keep their declared versions.
fn android_platform(ctx: &BuildContext) -> Result<PlatformPublication> {
    let manifest = ctx.ws.manifest();
    let aar = crate::gradle::release_aar(ctx)?;
    let contributions = crate::builtins::contributions(manifest, TargetKind::Android, ctx.ws.root())?;
    let (boms, main): (Vec<Dependency>, Vec<Dependency>) =
        merge_deps(&manifest.dependencies_for(TargetKind::Android), &contributions.dependencies).into_iter().partition(|d| d.bom);
    Ok(PlatformPublication {
        kind: PlatformKind::Android,
        artifact: aar,
        extra: Vec::new(),
        sources: None,
        javadoc: None,
        dependencies: published_dependencies(ctx, &main, &boms, TargetKind::Android, &[])?,
        main_class: None,
        web_resources: None,
    })
}

/// The dependencies a platform's POM and `.module` declare: the package's compile and runtime
/// dependencies (with the framework libraries) at the versions resolved for the build, its BOMs,
/// and its path dependencies, which are published under their own coordinates.
fn published_dependencies(
    ctx: &BuildContext,
    main: &[Dependency],
    boms: &[Dependency],
    target: TargetKind,
    resolutions: &[&Resolution],
) -> Result<Vec<PublishedDependency>> {
    let mut out = Vec::new();
    for dep in main {
        // Like Gradle's `implementation`, a dependency that is not part of the API (`exported`) is
        // only needed at runtime by consumers.
        let scope = match dep.scope {
            Scope::Compile if dep.exported => DependencyScope::Compile,
            Scope::Compile => DependencyScope::Runtime,
            Scope::Runtime => DependencyScope::Runtime,
            Scope::Provided | Scope::Processor | Scope::Ksp => continue,
        };
        let node = resolutions.iter().find_map(|r| r.node(&dep.module));
        let Some(version) = node.map(|n| n.version.clone()).or_else(|| dep.version.clone()) else {
            ctx.shell().warn(format!("`{}` has no version to publish; it is left out of the POM", dep.module));
            continue;
        };
        out.push(PublishedDependency {
            module: dep.module.clone(),
            pom_module: pom_module(resolutions, &dep.module),
            version,
            scope,
            classifier: dep.classifier.clone(),
            extension: dep.extension.clone(),
            exclusions: dep.exclusions.clone(),
            transitive: dep.transitive,
            bom: false,
        });
    }
    for bom in boms {
        let Some(version) = bom.version.clone() else { continue };
        out.push(PublishedDependency {
            module: bom.module.clone(),
            pom_module: bom.module.clone(),
            version,
            scope: DependencyScope::Compile,
            classifier: None,
            extension: None,
            exclusions: Vec::new(),
            transitive: true,
            bom: true,
        });
    }
    let ws = ctx.ws;
    for (dep, index) in ws.path_dependencies().iter().filter(|(d, _)| d.applies_to(target, false)) {
        let scope = match dep.scope {
            Scope::Compile if dep.exported => DependencyScope::Compile,
            Scope::Compile => DependencyScope::Runtime,
            Scope::Runtime => DependencyScope::Runtime,
            _ => continue,
        };
        let other = ctx.project.package(*index).manifest();
        let group = other.package.group.clone().with_context(|| format!("`{}` has no `group` in [package]", other.package.name))?;
        let module = ModuleId::new(&group, &other.package.name);
        // What `other` publishes: an Android application is left out.
        let other_targets: Vec<TargetKind> = other
            .targets
            .enabled()
            .into_iter()
            .filter(|t| *t != TargetKind::Android || other.targets.android.as_ref().is_some_and(|a| a.config.kind == "library"))
            .collect();
        let single = other_targets == [TargetKind::Jvm] || other_targets == [TargetKind::Android];
        // An Android library uses the Android module of a package that has one, else the JVM one.
        let kind = match target {
            TargetKind::Android if !other_targets.contains(&TargetKind::Android) => Some(PlatformKind::Jvm { jvm_version: None }),
            _ => platform_kind_of(ctx, target),
        };
        let pom_module = match kind {
            Some(kind) if !single => platform_module(&group, &other.package.name, &kind, true),
            _ => module.clone(),
        };
        out.push(PublishedDependency {
            module,
            pom_module,
            version: other.package.version.clone(),
            scope,
            classifier: None,
            extension: None,
            exclusions: Vec::new(),
            transitive: true,
            bom: false,
        });
    }
    Ok(out)
}

/// The platform kind a path dependency is consumed as on `target` (Native picks its first platform).
fn platform_kind_of(ctx: &BuildContext, target: TargetKind) -> Option<PlatformKind> {
    let manifest = ctx.ws.manifest();
    match target {
        TargetKind::Jvm => Some(PlatformKind::Jvm { jvm_version: None }),
        TargetKind::Wasm => manifest.targets.wasm.as_ref().map(|w| PlatformKind::Wasm { environment: w.environment.platform().to_owned() }),
        TargetKind::Js => manifest.targets.js.is_some().then_some(PlatformKind::Js),
        TargetKind::Native => native_platforms(ctx).ok()?.into_iter().next().map(|konan_target| PlatformKind::Native { konan_target }),
        TargetKind::Android => Some(PlatformKind::Android),
    }
}

/// The module a POM names for `module`. A multiplatform library resolves to a root module that
/// only redirects to its platform module (`kotlinx-coroutines-core` to `-jvm`, at the same
/// version); Maven cannot follow that redirect, so the POM names the platform module.
fn pom_module(resolutions: &[&Resolution], module: &ModuleId) -> ModuleId {
    let Some(node) = resolutions.iter().find_map(|r| r.node(module)) else { return module.clone() };
    match node.dependencies.as_slice() {
        [(target, _)]
            if node.files.is_empty() && target.group == module.group && target.artifact.starts_with(&format!("{}-", module.artifact)) =>
        {
            target.clone()
        }
        _ => module.clone(),
    }
}

/// Zips the Kotlin and Java sources of `dirs` (each under its prefix) into `<label>-sources.jar`.
fn sources_jar(jars: &Path, label: &str, dirs: &[(&str, PathBuf)]) -> Result<PathBuf> {
    let staging = jars.join(format!("sources-{label}"));
    remove_dir_all_if_exists(&staging)?;
    std::fs::create_dir_all(&staging).with_context(|| format!("failed to create {}", staging.display()))?;
    for (prefix, dir) in dirs {
        for file in collect_files(dir, &["kt", "java"]) {
            let rel = file.strip_prefix(dir).unwrap_or(&file);
            let dest = if prefix.is_empty() { staging.join(rel) } else { staging.join(prefix).join(rel) };
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
            }
            std::fs::copy(&file, &dest).with_context(|| format!("failed to copy {}", file.display()))?;
        }
    }
    let jar = jars.join(format!("{label}-sources.jar"));
    write_jar(&jar, &staging, &JarManifest::default(), &[])?;
    Ok(jar)
}

/// A javadoc jar holding only a manifest, which is all Maven Central checks for.
fn empty_javadoc_jar(ctx: &BuildContext, jars: &Path) -> Result<PathBuf> {
    let jar = jars.join("javadoc.jar");
    let manifest = JarManifest { implementation_title: Some(ctx.ws.manifest().package.name.clone()), ..Default::default() };
    write_jar(&jar, &jars.join("javadoc-empty"), &manifest, &[])?;
    Ok(jar)
}

/// A javadoc jar with the HTML documentation Dokka generates for `targets`.
fn dokka_javadoc_jar(ctx: &BuildContext, targets: &[TargetKind], jars: &Path) -> Result<PathBuf> {
    let html = super::doc::generate(ctx, targets)
        .context("failed to generate the javadoc jar; set `javadoc = \"empty\"` in [publish] to skip Dokka")?;
    let jar = jars.join("javadoc.jar");
    write_jar(&jar, &html, &JarManifest::default(), &[])?;
    Ok(jar)
}

/// Signs every staged file (with `signer`) and writes the checksums Maven repositories expect:
/// `.md5`, `.sha1`, `.sha256` and `.sha512` for each file (not for the signatures).
fn finish_files(ctx: &BuildContext, staged: Vec<StagedModule>, signer: Option<&Signer>) -> Result<Vec<StagedModule>> {
    if let Some(_signer) = signer {
        let count: usize = staged.iter().map(|m| m.files.len()).sum();
        ctx.shell().status("Signing", format!("{count} files"));
    }
    let mut out = Vec::new();
    for mut module in staged {
        let mut extra = Vec::new();
        for name in &module.files {
            let path = module.dir.join(name);
            if let Some(signer) = signer {
                signer.sign(&path)?;
                extra.push(format!("{name}.asc"));
            }
            let sums = hash::checksums_file(&path).with_context(|| format!("failed to hash {}", path.display()))?;
            for (ext, sum) in sums.files() {
                let sidecar = module.dir.join(format!("{name}.{ext}"));
                std::fs::write(&sidecar, sum).with_context(|| format!("failed to write {}", sidecar.display()))?;
                extra.push(format!("{name}.{ext}"));
            }
        }
        // Artifacts and their signatures and checksums first, the POM and `.module` they describe last.
        let (metadata, artifacts): (Vec<String>, Vec<String>) =
            module.files.drain(..).chain(extra).partition(|f| f.contains(".pom") || f.contains(".module"));
        module.files = artifacts.into_iter().chain(metadata).collect();
        out.push(module);
    }
    Ok(out)
}

/// Uploads the staged repository to the Central Portal as one bundle and follows its validation.
fn publish_to_central(ctx: &BuildContext, repo_dir: &Path, work: &Path, coordinates: &str, files: usize) -> Result<()> {
    let manifest = ctx.ws.manifest();
    let bundle = work.join("central-bundle.zip");
    central::write_bundle(repo_dir, &bundle)?;
    let portal = Portal::from_env(&ctx.client)?;
    let kind = match manifest.publish.central_publishing {
        CentralPublishing::Manual => PublishingType::Manual,
        CentralPublishing::Automatic => PublishingType::Automatic,
    };
    ctx.shell().status("Uploading", format!("{coordinates} to Maven Central ({files} files)"));
    let id = portal.upload(&bundle, coordinates, kind)?;
    ctx.shell().status("Validating", format!("deployment {id}"));
    portal.wait(&id, kind, ctx.shell())?;
    match kind {
        PublishingType::Manual => {
            ctx.shell().status("Validated", format!("{coordinates} (deployment {id})"));
            ctx.shell()
                .note(format!("release it at {}, or set `central-publishing = \"automatic\"` in [publish]", central::DEPLOYMENTS_PAGE));
        }
        PublishingType::Automatic => {
            ctx.shell().status("Published", format!("{coordinates} to Maven Central"));
            ctx.shell().note("Maven Central can take up to 30 minutes to make a new release available");
        }
    }
    Ok(())
}

/// What `kargo package` writes for a library: a jar of its own classes (no dependencies, no
/// `Class-Path`) and a sources jar, in `target/<profile>/package`. Returns the jar.
/// The jar of a library: its classes, or with `[package.shade]` also the dependencies, shaded. Returns the
/// dependency jars it bundles.
fn write_library_jar(ctx: &BuildContext, jar: &Path, jvm: &super::build::JvmBuild, manifest: &JarManifest) -> Result<Vec<PathBuf>> {
    let Some(shade) = super::pkg_shade(ctx) else {
        write_jar(jar, &jvm.classes, manifest, &[])?;
        return Ok(Vec::new());
    };
    let deps: Vec<PathBuf> = jvm.runtime_classpath.iter().filter(|p| p.extension().is_some_and(|e| e == "jar")).cloned().collect();
    super::shade_jar(ctx, jar, jvm, manifest, &deps, &shade)
}

pub fn package_library(ctx: &BuildContext, jvm: &super::build::JvmBuild) -> Result<PathBuf> {
    let ws = ctx.ws;
    let pkg = &ws.manifest().package;
    let dir = ws.profile_dir(&ctx.profile).join("package");
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let jar = dir.join(format!("{}-{}.jar", pkg.name, pkg.version));
    ctx.shell().status("Packaging", format!("{} v{} ({})", pkg.name, pkg.version, jar.display()));
    let manifest = JarManifest {
        main_class: None,
        class_path: Vec::new(),
        implementation_title: Some(pkg.name.clone()),
        implementation_version: Some(pkg.version.clone()),
        ..Default::default()
    };
    write_library_jar(ctx, &jar, jvm, &manifest)?;
    let work = ws.profile_dir(&ctx.profile).join("package-work");
    let mut all: Vec<(String, PathBuf)> = Vec::new();
    for d in ws.common_source_dirs() {
        all.push((String::new(), d));
    }
    for d in ws.target_source_dirs(TargetKind::Jvm) {
        all.push((String::new(), d));
    }
    for (g, _) in ws.manifest().source_groups_for(TargetKind::Jvm) {
        for d in ws.group_source_dirs(&g.name) {
            all.push((String::new(), d));
        }
    }
    let refs: Vec<(&str, PathBuf)> = all.iter().map(|(p, d)| (p.as_str(), d.clone())).collect();
    let mut dirs = refs;
    dirs.extend(crate::generate::dirs(ws, &ctx.profile, TargetKind::Jvm, GeneratedKind::Sources).into_iter().map(|dir| ("", dir)));
    let sources = sources_jar(&work, "jvm", &dirs)?;
    let dest = dir.join(format!("{}-{}-sources.jar", pkg.name, pkg.version));
    std::fs::copy(&sources, &dest).with_context(|| format!("failed to write {}", dest.display()))?;
    remove_dir_all_if_exists(&work)?;
    Ok(jar)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platforms_map_back_to_targets() {
        let kinds = [
            PlatformKind::Jvm { jvm_version: None },
            PlatformKind::Native { konan_target: "linux_x64".into() },
            PlatformKind::Native { konan_target: "mingw_x64".into() },
        ];
        assert_eq!(
            targets_per_platform(&[TargetKind::Jvm, TargetKind::Native], &kinds),
            [TargetKind::Jvm, TargetKind::Native, TargetKind::Native]
        );
    }

    #[test]
    fn poms_name_the_platform_module_of_multiplatform_dependencies() {
        let core = ModuleId::new("org.jetbrains.kotlinx", "kotlinx-coroutines-core");
        let jvm = ModuleId::new("org.jetbrains.kotlinx", "kotlinx-coroutines-core-jvm");
        let node = |module: &ModuleId, files: Vec<PathBuf>, deps: Vec<(ModuleId, Option<String>)>| kargo_maven::resolver::ResolvedNode {
            module: module.clone(),
            version: "1.11.0".into(),
            files,
            dependencies: deps,
        };
        let resolution = Resolution {
            roots: vec![core.clone()],
            nodes: vec![
                node(&core, Vec::new(), vec![(jvm.clone(), Some("1.11.0".into()))]),
                node(&jvm, vec![PathBuf::from("core-jvm.jar")], Vec::new()),
            ],
        };
        assert_eq!(pom_module(&[&resolution], &core), jvm);
        assert_eq!(pom_module(&[&resolution], &jvm), jvm);
        let other = ModuleId::new("org.acme", "lib");
        assert_eq!(pom_module(&[&resolution], &other), other);
    }
}
