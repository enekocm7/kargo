//! `kargo vendor`: copies every Maven file a build of the project needs into a directory laid out
//! as a Maven repository (`group/artifact/version/...`, with POMs, Gradle module metadata,
//! `maven-metadata.xml` and `.sha1` checksums), so a `--vendored` build resolves from it alone.
//!
//! What a build needs is what [`super::fetch`] downloads. It runs twice: once as `kargo fetch`
//! (which brings `kargo.lock` up to date), then again with every locked version pinned and the
//! lock bypassed, so the resolver reads every POM and `.module` file again. Every file the
//! Maven client served in those runs, and every file of `kargo.lock` (checked against its
//! SHA-256), is copied. Android dependencies are resolved by Gradle and are not vendored.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::context::BuildContext;
use kargo_manifest::TargetKind;
use kargo_util::hash;

/// The Maven modules that make up the toolchains, vendored only with `--toolchains`: the Kotlin
/// compiler (with the Build Tools API and the daemon) and the Kotlin/Native distribution.
const TOOLCHAIN_MODULES: &[&str] = &[
    "org/jetbrains/kotlin/kotlin-compiler-embeddable/",
    "org/jetbrains/kotlin/kotlin-build-tools-impl/",
    "org/jetbrains/kotlin/kotlin-daemon-embeddable/",
    "org/jetbrains/kotlin/kotlin-native-prebuilt/",
];

#[derive(Debug, Clone, Default)]
pub struct VendorOptions {
    /// The directory to write, absolute.
    pub dir: PathBuf,
    /// Also copy the Kotlin compiler and Kotlin/Native distributions.
    pub toolchains: bool,
    /// Remove files of the directory that the project no longer needs.
    pub sync: bool,
}

/// What `kargo vendor` did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VendorSummary {
    /// Files in the vendored repository (not counting checksums).
    pub files: usize,
    /// Files copied because they were missing or different.
    pub copied: usize,
    /// Files removed by `--sync`.
    pub removed: usize,
}

/// The targets of a package `kargo vendor` fetches: all enabled ones but Android.
fn vendored_targets(ctx: &BuildContext) -> Vec<TargetKind> {
    ctx.ws.manifest().targets.enabled().into_iter().filter(|t| *t != TargetKind::Android).collect()
}

/// Downloads what every package of the project needs, as `kargo fetch` does. Run once with a
/// plain context and once with one that re-resolves at the locked versions (see the module docs).
pub fn fetch(ctx: &BuildContext, toolchains: bool) -> Result<()> {
    for package in 0..ctx.project.packages().len() {
        let ctx = ctx.for_package(package);
        let targets = vendored_targets(&ctx);
        if ctx.ws.manifest().targets.is_enabled(TargetKind::Android) {
            ctx.shell().warn(format!(
                "`{}`: the Android target's dependencies are resolved by Gradle and are not vendored",
                ctx.ws.manifest().package.name
            ));
        }
        if targets.is_empty() {
            continue;
        }
        super::fetch(&ctx, &targets, false)?;
        if toolchains && targets.contains(&TargetKind::Native) {
            // An installed distribution is not downloaded again by a build, so ask for it here.
            let artifact = kargo_toolchain::NativeToolchain::distribution(&ctx.ws.manifest().kotlin.version)?;
            ctx.client.fetch_required(&artifact.repo_path())?;
        }
    }
    Ok(())
}

/// Reads the version list (`maven-metadata.xml`) of every locked module, so a version range can
/// be resolved again from the vendored repository after the manifest changes.
pub fn fetch_metadata(ctx: &BuildContext) -> Result<()> {
    let modules: BTreeSet<String> = ctx.lockfile().module_versions().into_keys().collect();
    let modules: Vec<kargo_maven::ModuleId> = modules.iter().filter_map(|m| m.parse().ok()).collect();
    for result in kargo_maven::par_map_io(&modules, |module| ctx.client.versions(module)) {
        result?;
    }
    Ok(())
}

/// Copies what the fetches served (through `ctx`'s client and `earlier`, another client's
/// [`kargo_maven::MavenClient::served_files`]), and every file of `kargo.lock`, into `opts.dir`.
pub fn write(ctx: &BuildContext, opts: &VendorOptions, earlier: BTreeMap<String, PathBuf>) -> Result<VendorSummary> {
    let mut files: BTreeMap<String, PathBuf> = earlier;
    files.extend(ctx.client.served_files());
    // The toolchains, unless asked for. A project that depends on one of their modules keeps its locked files.
    files.retain(|rel, _| !rel.ends_with(".sha1") && (opts.toolchains || !TOOLCHAIN_MODULES.iter().any(|m| rel.starts_with(m))));
    for pkg in ctx.lockfile().resolutions.values().flat_map(|r| &r.packages) {
        for file in &pkg.files {
            // A resolution `kargo fetch` does not restore (another profile's) may not be cached yet.
            let cached = ctx.client.fetch_required(&file.path)?;
            let actual = hash::sha256_file(&cached).with_context(|| format!("failed to hash {}", cached.display()))?;
            if actual != file.sha256 {
                bail!(
                    "checksum mismatch for {} (locked sha256 {}, cached {actual}); delete {} so it is downloaded again, \
                     or run `kargo update` to re-lock it if the lock entry is wrong",
                    file.path,
                    file.sha256,
                    cached.display()
                );
            }
            files.insert(file.path.clone(), cached);
        }
    }

    std::fs::create_dir_all(&opts.dir).with_context(|| format!("failed to create {}", opts.dir.display()))?;
    let mut summary = VendorSummary { files: files.len(), ..Default::default() };
    let mut written: BTreeSet<PathBuf> = BTreeSet::new();
    for (rel, cached) in &files {
        let dest = rel.split('/').fold(opts.dir.clone(), |p, seg| p.join(seg));
        let sha1 = hash::sha1_file(cached).with_context(|| format!("failed to hash {}", cached.display()))?;
        let current = dest.is_file() && hash::sha1_file(&dest).is_ok_and(|s| s == sha1);
        if !current {
            kargo_util::fs::write_atomic(&dest, |f| std::io::copy(&mut std::fs::File::open(cached)?, f).map(|_| ()))
                .with_context(|| format!("failed to copy {} to {}", cached.display(), dest.display()))?;
            summary.copied += 1;
        }
        let sidecar = dest.with_file_name(format!("{}.sha1", dest.file_name().unwrap_or_default().to_string_lossy()));
        if std::fs::read_to_string(&sidecar).ok().as_deref().map(str::trim) != Some(sha1.as_str()) {
            kargo_util::fs::write_string_atomic(&sidecar, &format!("{sha1}\n"))?;
        }
        written.insert(dest);
        written.insert(sidecar);
    }
    if opts.sync {
        summary.removed = remove_unlisted(&opts.dir, &written)?;
    }
    Ok(summary)
}

/// Removes the files under `dir` that are not in `keep`, and the directories that end up empty.
fn remove_unlisted(dir: &Path, keep: &BTreeSet<PathBuf>) -> Result<usize> {
    let mut removed = 0;
    let mut dirs = Vec::new();
    for entry in walkdir::WalkDir::new(dir).min_depth(1) {
        let entry = entry.with_context(|| format!("failed to read {}", dir.display()))?;
        if entry.file_type().is_dir() {
            dirs.push(entry.into_path());
        } else if !keep.contains(entry.path()) {
            std::fs::remove_file(entry.path()).with_context(|| format!("failed to remove {}", entry.path().display()))?;
            if !entry.file_name().to_string_lossy().ends_with(".sha1") {
                removed += 1;
            }
        }
    }
    // Deepest first, so a parent is empty once its children are gone.
    for dir in dirs.iter().rev() {
        let _ = std::fs::remove_dir(dir);
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_removes_unlisted_files_and_empty_directories() {
        let dir = tempfile::tempdir().unwrap();
        let file = |rel: &str| {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "x").unwrap();
            path
        };
        let kept = file("org/a/a/1.0/a-1.0.jar");
        file("org/b/b/1.0/b-1.0.jar");
        file("org/b/b/1.0/b-1.0.jar.sha1");
        let removed = remove_unlisted(dir.path(), &BTreeSet::from([kept.clone()])).unwrap();
        assert_eq!(removed, 1, "checksums are not counted");
        assert!(kept.is_file());
        assert!(!dir.path().join("org/b").exists());
    }
}
