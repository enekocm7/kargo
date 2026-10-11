//! Shared state for one Kargo invocation that builds something.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result, bail};

use crate::builtins::{self, Contributions, PluginSpec};
use crate::lockfile::Lockfile;
use crate::workspace::{ProfileKind, Workspace};
use kargo_compiler::ResolvedPlugin;
use kargo_manifest::{Dependency, Scope, TargetKind};
use kargo_maven::{DependencyRequest, Exclusion, MavenClient, Platform, Resolution, Resolver};
use kargo_toolchain::{Jdk, KotlinCompiler, NativeToolchain};
use kargo_util::config::Config;
use kargo_util::hash::Fingerprint;
use kargo_util::shell::Shell;

/// Minimum JDK supported by the Kotlin compiler.
pub const MIN_JDK: u32 = 11;

pub struct BuildContext<'a> {
    pub config: &'a Config,
    pub ws: &'a Workspace,
    pub profile: ProfileKind,
    pub client: MavenClient,
    /// Fail instead of updating `kargo.lock` (`--locked`).
    pub locked: bool,
    lock: Mutex<Lockfile>,
    lock_dirty: AtomicBool,
    jdk: OnceLock<Jdk>,
    compiler: OnceLock<KotlinCompiler>,
    native: OnceLock<NativeToolchain>,
}

/// The dependency configuration of one target.
#[derive(Debug, Clone)]
pub struct TargetSetup {
    pub target: TargetKind,
    pub contributions: Contributions,
    /// Main dependencies (manifest first, then framework-provided).
    pub main: Vec<Dependency>,
    pub dev: Vec<Dependency>,
    pub boms: Vec<Dependency>,
}

impl TargetSetup {
    pub fn requests(&self, scopes: &[Scope], include_dev: bool) -> Vec<DependencyRequest> {
        let mut deps: Vec<&Dependency> = self.main.iter().filter(|d| scopes.contains(&d.scope)).collect();
        if include_dev {
            deps.extend(self.dev.iter());
        }
        let mut out: Vec<DependencyRequest> = Vec::new();
        for d in deps {
            if out.iter().any(|r| r.module == d.module && r.classifier == d.classifier) {
                continue;
            }
            out.push(to_request(d));
        }
        out
    }
}

pub fn to_request(d: &Dependency) -> DependencyRequest {
    DependencyRequest {
        module: d.module.clone(),
        version: d.version.clone(),
        classifier: d.classifier.clone(),
        extension: d.extension.clone(),
        exclusions: d.exclusions.iter().map(|e| Exclusion::new(&e.group, &e.artifact)).collect(),
        force: d.force,
        transitive: d.transitive,
    }
}

/// Appends `extra` dependencies unless the user already declared the module.
fn merge_deps(user: &[Dependency], extra: &[Dependency]) -> Vec<Dependency> {
    let mut out: Vec<Dependency> = user.to_vec();
    for d in extra {
        if !out.iter().any(|u| u.module == d.module && u.classifier == d.classifier) {
            out.push(d.clone());
        }
    }
    out
}

impl<'a> BuildContext<'a> {
    pub fn new(config: &'a Config, ws: &'a Workspace, profile: ProfileKind) -> Result<Self> {
        let lock = Lockfile::load(&ws.lockfile_path())?;
        Ok(Self {
            config,
            ws,
            profile,
            client: MavenClient::new(config, ws.manifest().repositories.clone()),
            locked: false,
            lock: Mutex::new(lock),
            lock_dirty: AtomicBool::new(false),
            jdk: OnceLock::new(),
            compiler: OnceLock::new(),
            native: OnceLock::new(),
        })
    }

    pub fn shell(&self) -> &Shell {
        self.config.shell()
    }

    pub fn jdk(&self) -> Result<&Jdk> {
        if let Some(jdk) = self.jdk.get() {
            return Ok(jdk);
        }
        let request = &self.ws.manifest().toolchain.jdk;
        let jdk = kargo_toolchain::jdks::resolve(self.config, request)?;
        jdk.require_version(MIN_JDK)?;
        self.shell().verbose("Using", format!("JDK {} at {}", jdk.version, jdk.home.display()));
        Ok(self.jdk.get_or_init(|| jdk))
    }

    pub fn kotlin_compiler(&self) -> Result<&KotlinCompiler> {
        if let Some(c) = self.compiler.get() {
            return Ok(c);
        }
        let compiler = KotlinCompiler::resolve(&self.client, &self.ws.manifest().kotlin.version)?;
        Ok(self.compiler.get_or_init(|| compiler))
    }

    pub fn native_toolchain(&self) -> Result<&NativeToolchain> {
        if let Some(t) = self.native.get() {
            return Ok(t);
        }
        let toolchain = NativeToolchain::ensure(self.config, &self.client, &self.ws.manifest().kotlin.version)?;
        Ok(self.native.get_or_init(|| toolchain))
    }

    pub fn setup(&self, target: TargetKind) -> Result<TargetSetup> {
        let manifest = self.ws.manifest();
        let contributions = builtins::contributions(manifest, target, self.ws.root())?;
        let user = manifest.dependencies_for(target);
        let all = merge_deps(&user, &contributions.dependencies);
        let (boms, main): (Vec<_>, Vec<_>) = all.into_iter().partition(|d| d.bom);
        let (dev_boms, dev): (Vec<_>, Vec<_>) =
            merge_deps(&manifest.dev_dependencies, &contributions.dev_dependencies).into_iter().partition(|d| d.bom);
        let mut boms = boms;
        boms.extend(dev_boms);
        Ok(TargetSetup { target, contributions, main, dev, boms })
    }

    fn inputs_fingerprint(&self, platform: &Platform, roots: &[DependencyRequest], boms: &[Dependency]) -> String {
        let mut fp = Fingerprint::new();
        fp.str(kargo_maven::resolver::RESOLVER_VERSION);
        fp.str(&platform.to_string());
        for r in roots {
            fp.str(&format!("{r:?}"));
        }
        fp.str("--boms");
        for b in boms {
            fp.str(&format!("{}:{:?}", b.module, b.version));
        }
        fp.str("--repos");
        for repo in self.client.repositories() {
            fp.str(&repo.url);
        }
        fp.finish()
    }

    /// Resolves `roots` for `platform`, reusing `kargo.lock` when possible.
    pub fn resolve(&self, key: &str, platform: Platform, roots: &[DependencyRequest], boms: &[Dependency]) -> Result<Resolution> {
        let inputs = self.inputs_fingerprint(&platform, roots, boms);
        if !self.config.refresh() {
            let lock = self.lock.lock().expect("lock poisoned");
            if let Some(res) = lock.restore(key, &inputs, &self.client)? {
                return Ok(res);
            }
        }
        if self.locked {
            bail!("kargo.lock needs to be updated for `{key}` but --locked was passed");
        }
        self.shell().verbose("Resolving", key);
        let mut resolver = Resolver::new(&self.client, platform);
        for bom in boms {
            let version = bom.version.as_deref().context("BOM without version")?;
            resolver.import_bom(&bom.module, version)?;
        }
        let resolution = resolver.resolve(roots).with_context(|| format!("failed to resolve dependencies for `{key}`"))?;
        self.lock.lock().expect("lock poisoned").record(key, &inputs, &resolution, &self.client)?;
        self.lock_dirty.store(true, Ordering::SeqCst);
        Ok(resolution)
    }

    /// Fetches compiler plugin jars.
    pub fn plugin_jars(&self, plugins: &[PluginSpec]) -> Result<Vec<ResolvedPlugin>> {
        let kotlin = &self.ws.manifest().kotlin.version;
        plugins
            .iter()
            .map(|p| {
                let version = p.version.as_deref().unwrap_or(kotlin);
                let jar = kargo_toolchain::kotlin::fetch_jar(&self.client, &p.group, &p.artifact, version)
                    .with_context(|| format!("failed to fetch compiler plugin {}:{}:{version}", p.group, p.artifact))?;
                Ok(ResolvedPlugin { jar, options: builtins::plugin_option_args(p) })
            })
            .collect()
    }

    /// A single tool jar (e.g. the JUnit console launcher), without dependencies.
    pub fn tool_jar(&self, group: &str, artifact: &str, version: &str) -> Result<PathBuf> {
        kargo_toolchain::kotlin::fetch_jar(&self.client, group, artifact, version)
    }

    /// Persists `kargo.lock` if any resolution changed it.
    pub fn save_lockfile(&self) -> Result<()> {
        if self.lock_dirty.load(Ordering::SeqCst) {
            let lock = self.lock.lock().expect("lock poisoned");
            if lock.save(&self.ws.lockfile_path())? {
                self.shell().verbose("Updated", "kargo.lock");
            }
        }
        Ok(())
    }

    /// Replaces the lockfile content (used by `kargo update`).
    pub fn reset_lockfile(&self, keep: Option<&[String]>) {
        let mut lock = self.lock.lock().expect("lock poisoned");
        match keep {
            Some(keys) => lock.retain(keys),
            None => lock.resolutions.clear(),
        }
        self.lock_dirty.store(true, Ordering::SeqCst);
    }

    pub fn fingerprint_dir(&self, target: TargetKind) -> PathBuf {
        self.ws.output_dir(self.profile, target).join(".fingerprint")
    }
}

impl Drop for BuildContext<'_> {
    fn drop(&mut self) {
        // Keep successful resolutions even if a later step (e.g. compilation) failed.
        if let Err(e) = self.save_lockfile() {
            self.shell().warn(format!("failed to write kargo.lock: {e:#}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_dependencies_override_framework_ones() {
        let user = vec![Dependency::new("a:b".parse().unwrap(), Some("2".into()))];
        let extra =
            vec![Dependency::new("a:b".parse().unwrap(), Some("1".into())), Dependency::new("c:d".parse().unwrap(), Some("1".into()))];
        let merged = merge_deps(&user, &extra);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].version.as_deref(), Some("2"));
    }

    #[test]
    fn requests_filter_by_scope() {
        let mut provided = Dependency::new("p:p".parse().unwrap(), Some("1".into()));
        provided.scope = Scope::Provided;
        let mut runtime = Dependency::new("r:r".parse().unwrap(), Some("1".into()));
        runtime.scope = Scope::Runtime;
        let setup = TargetSetup {
            target: TargetKind::Jvm,
            contributions: Contributions::default(),
            main: vec![Dependency::new("c:c".parse().unwrap(), Some("1".into())), provided, runtime],
            dev: vec![Dependency::new("t:t".parse().unwrap(), Some("1".into()))],
            boms: vec![],
        };
        let names = |r: Vec<DependencyRequest>| r.into_iter().map(|r| r.module.artifact).collect::<Vec<_>>();
        assert_eq!(names(setup.requests(&[Scope::Compile, Scope::Provided], false)), ["c", "p"]);
        assert_eq!(names(setup.requests(&[Scope::Compile, Scope::Runtime], false)), ["c", "r"]);
        assert_eq!(names(setup.requests(&[Scope::Compile, Scope::Runtime, Scope::Provided], true)), ["c", "p", "r", "t"]);
    }
}
