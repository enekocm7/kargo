//! Shared state for one Kargo invocation that builds something.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result, bail};

use crate::builtins::{self, Contributions, PluginSpec};
use crate::env::{EnvOptions, ProjectEnv};
use crate::lockfile::Lockfile;
use crate::workspace::{ProfileKind, Project, Workspace};
use kargo_compiler::ResolvedPlugin;
use kargo_manifest::{Dependency, JsEnvironment, Manifest, Scope, TargetKind, TestSuite, WasmEnvironment};
use kargo_maven::resolver::ResolvedNode;
use kargo_maven::{Artifact, DependencyRequest, Exclusion, Explanation, MavenClient, ModuleId, Platform, Repository, Resolution, Resolver};
use kargo_toolchain::jdks::JdkNeeds;
use kargo_toolchain::{Jdk, KotlinCompiler, NativeToolchain};
use kargo_util::config::Config;
use kargo_util::flock::FileLock;
use kargo_util::hash::Fingerprint;
use kargo_util::shell::Shell;

/// Minimum JDK supported by the Kotlin compiler.
pub const MIN_JDK: u32 = 11;

/// The `kargo.lock` key of the Kotlin compiler's classpath, which every package of a project shares.
pub const COMPILER_KEY: &str = "tools/kotlin-compiler";

/// What a project needs of a JDK that Kargo installs for it when the machine has none: new
/// enough for its `jvm-target`, and not newer than its Kotlin compiler runs on.
pub fn jdk_needs(manifest: &Manifest) -> JdkNeeds {
    let jvm_target = manifest.targets.jvm.as_ref().and_then(|jvm| jvm.required_jdk());
    JdkNeeds {
        min: jvm_target.map_or(JdkNeeds::default().min, |t| t.max(JdkNeeds::default().min)),
        max: kargo_toolchain::kotlin::max_compiler_jdk(&manifest.kotlin.version),
    }
}

/// The build of one package. Path dependencies are built through contexts of their own
/// ([`BuildContext::for_package`]) that share the lockfile and toolchains.
pub struct BuildContext<'a> {
    pub config: &'a Config,
    pub project: &'a Project,
    /// The package being built.
    pub ws: &'a Workspace,
    /// Index of `ws` in the project.
    pub package: usize,
    pub profile: ProfileKind,
    pub client: Arc<MavenClient>,
    /// Fail instead of updating `kargo.lock` (`--locked`).
    pub locked: bool,
    /// Never write `kargo.lock` (`kargo update --dry-run`).
    pub dry_run: bool,
    /// Set by a selective `kargo update`: which locked versions to keep.
    pub update: Option<UpdatePlan>,
    shared: Arc<Shared>,
    /// Only the context a command created writes `kargo.lock` when it is dropped.
    owner: bool,
}

/// State shared by every package built in one invocation.
struct Shared {
    lock: Mutex<Lockfile>,
    lock_dirty: AtomicBool,
    jdk: Lazy<Jdk>,
    compiler: Lazy<KotlinCompiler>,
    native: Lazy<NativeToolchain>,
    /// KSP tool classpaths by `kargo.lock` key and KSP version (members of a workspace may pick different ones).
    ksp: Mutex<HashMap<String, Vec<PathBuf>>>,
    /// Artifacts of path dependencies built in this invocation, by package, target and platform. Each
    /// entry has its own lock, so packages building at the same time build a shared dependency once.
    built: Mutex<HashMap<String, Arc<Mutex<Option<BuiltArtifact>>>>>,
    /// The build-directory lock, held until [`BuildContext::unlock`] or drop.
    build_lock: Mutex<Option<FileLock>>,
    /// The packages the command builds, when it builds several ([`BuildContext::select`]).
    selected: Mutex<Vec<usize>>,
    /// Held while Gradle runs: the generated Android builds of one invocation share the build
    /// directories of their library subprojects.
    gradle: Mutex<()>,
}

/// The outcome of building a path dependency: its artifact, or why it failed.
type BuiltArtifact = std::result::Result<Option<PathBuf>, String>;

/// A package reached through path dependencies, with the scope it has for the consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalDependency {
    pub package: usize,
    pub scope: Scope,
}

/// The scope a dependency's own dependency (`inner`) gets for the consumer, if any. These are
/// Maven's rules: a provided dependency is not passed on, and a runtime one stays runtime.
pub fn combine_scopes(outer: Scope, inner: Scope) -> Option<Scope> {
    match (outer, inner) {
        // Annotation and KSP processors only run for the package that declares them.
        (_, Scope::Provided | Scope::Processor | Scope::Ksp) => None,
        (Scope::Provided, _) => Some(Scope::Provided),
        (Scope::Processor | Scope::Ksp, _) => None,
        (Scope::Runtime, _) | (_, Scope::Runtime) => Some(Scope::Runtime),
        (Scope::Compile, Scope::Compile) => Some(Scope::Compile),
    }
}

/// The scope a path dependency's own dependency (`inner`) gets for the consumer, which reaches the
/// path dependency with scope `outer`. Like Gradle's `api` and `implementation`: an `exported`
/// dependency is part of the path dependency's API and keeps Maven's scope ([`combine_scopes`]);
/// any other one is only needed to run it, so the consumer gets it at runtime (and not at all
/// through a `provided` dependency).
pub fn inherited_scope(outer: Scope, inner: Scope, exported: bool) -> Option<Scope> {
    match combine_scopes(outer, inner)? {
        Scope::Compile if !exported => Some(Scope::Runtime),
        Scope::Provided if !exported => None,
        scope => Some(scope),
    }
}

/// A value initialized at most once even when targets build in parallel, so two
/// threads never download or install the same toolchain at the same time.
struct Lazy<T> {
    cell: OnceLock<T>,
    init: Mutex<()>,
}

impl<T> Lazy<T> {
    fn new() -> Self {
        Self { cell: OnceLock::new(), init: Mutex::new(()) }
    }

    fn get_or_try_init(&self, init: impl FnOnce() -> Result<T>) -> Result<&T> {
        if let Some(v) = self.cell.get() {
            return Ok(v);
        }
        let _guard = self.init.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = self.cell.get() {
            return Ok(v);
        }
        let value = init()?;
        Ok(self.cell.get_or_init(|| value))
    }
}

/// A selective `kargo update`: every module keeps its locked version except `modules`, which are
/// re-resolved, or set to `precise` when given.
#[derive(Debug, Clone)]
pub struct UpdatePlan {
    /// The lockfile before the update.
    pub previous: Lockfile,
    pub modules: Vec<ModuleId>,
    pub precise: Option<String>,
}

impl UpdatePlan {
    /// The versions to pin in the resolution `key`, each with the locked build when it is a SNAPSHOT
    /// (`1.0-20240101.120000-1`). Locked versions are kept only while the resolution's inputs are
    /// unchanged; otherwise the manifest changed and it resolves afresh.
    fn pins(&self, key: &str, inputs: &str) -> Vec<(ModuleId, String, Option<String>)> {
        let mut pins: Vec<(ModuleId, String, Option<String>)> = Vec::new();
        if let Some(locked) = self.previous.resolutions.get(key).filter(|r| r.inputs == inputs) {
            for pkg in &locked.packages {
                if let Ok(module) = pkg.module.parse::<ModuleId>()
                    && !self.modules.contains(&module)
                {
                    pins.push((module, pkg.version.clone(), pkg.build_file_version()));
                }
            }
        }
        if let Some(precise) = &self.precise {
            pins.extend(self.modules.iter().map(|m| (m.clone(), precise.clone(), None)));
        }
        pins
    }
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
            deps.extend(self.dev.iter().filter(|d| scopes.contains(&d.scope)));
        }
        let mut out: Vec<DependencyRequest> = Vec::new();
        for d in deps {
            let request = to_request(d);
            if !out.iter().any(|r| r.same_artifact(&request)) {
                out.push(request);
            }
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

/// Adds the dependencies a path dependency brings to `deps`, whose first `own` entries the consumer
/// declared itself (and which win). A module that two path dependencies bring with different scopes
/// is needed both to compile and at runtime.
fn merge_inherited(deps: &mut Vec<Dependency>, own: usize, inherited: Vec<Dependency>) {
    for d in inherited {
        match deps.iter().position(|u| u.same_artifact(&d)) {
            None => deps.push(d),
            Some(i) if i >= own && !d.bom && deps[i].scope != d.scope => deps[i].scope = Scope::Compile,
            Some(_) => {}
        }
    }
}

/// Adds the dependencies of the selected profile to `user`; a profile's dependency replaces the
/// one with the same artifact, as the profile is the more specific declaration.
fn with_profile_dependencies(mut user: Vec<Dependency>, profile: &[Dependency]) -> Vec<Dependency> {
    user.retain(|d| !profile.iter().any(|p| p.same_artifact(d)));
    user.extend(profile.iter().cloned());
    user
}

/// Appends `extra` dependencies unless the user already declared that artifact of the module.
pub(crate) fn merge_deps(user: &[Dependency], extra: &[Dependency]) -> Vec<Dependency> {
    let mut out: Vec<Dependency> = user.to_vec();
    for d in extra {
        if !out.iter().any(|u| u.same_artifact(d)) {
            out.push(d.clone());
        }
    }
    out
}

impl<'a> BuildContext<'a> {
    /// The context for building `package` of `project`; it owns `kargo.lock`.
    pub fn new(config: &'a Config, project: &'a Project, package: usize, profile: ProfileKind) -> Result<Self> {
        kargo_compiler::daemon::configure(config.home());
        project.package(package).profile(&profile)?;
        // Taken before reading kargo.lock so a concurrent process's write is seen.
        let build_lock = project.package(package).lock(config.shell())?;
        let lock = Lockfile::load(&project.lockfile_path())?;
        Ok(Self {
            config,
            project,
            ws: project.package(package),
            package,
            profile,
            client: Arc::new(MavenClient::new(config, project.repositories())),
            locked: false,
            dry_run: false,
            update: None,
            shared: Arc::new(Shared {
                lock: Mutex::new(lock),
                lock_dirty: AtomicBool::new(false),
                jdk: Lazy::new(),
                compiler: Lazy::new(),
                native: Lazy::new(),
                ksp: Mutex::new(HashMap::new()),
                built: Mutex::new(HashMap::new()),
                build_lock: Mutex::new(Some(build_lock)),
                selected: Mutex::new(Vec::new()),
                gradle: Mutex::new(()),
            }),
            owner: true,
        })
    }

    /// A context for another package of the project, sharing the lockfile, toolchains and settings.
    pub fn for_package(&self, package: usize) -> BuildContext<'a> {
        BuildContext {
            config: self.config,
            project: self.project,
            ws: self.project.package(package),
            package,
            profile: self.profile.clone(),
            client: self.client.clone(),
            locked: self.locked,
            dry_run: self.dry_run,
            update: self.update.clone(),
            shared: self.shared.clone(),
            owner: false,
        }
    }

    /// Records the packages this invocation builds, so a package can leave work to another one that
    /// does it anyway (an Android library is built by the Gradle run of the Android package that uses it).
    pub fn select(&self, packages: &[usize]) {
        *self.shared.selected.lock().unwrap_or_else(|e| e.into_inner()) = packages.to_vec();
    }

    /// The packages this invocation builds: the ones given to [`BuildContext::select`], else this package.
    pub fn selected(&self) -> Vec<usize> {
        let selected = self.shared.selected.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if selected.is_empty() { vec![self.package] } else { selected }
    }

    /// Runs `f` while no other package of this invocation runs Gradle.
    pub fn with_gradle<T>(&self, f: impl FnOnce() -> T) -> T {
        let _guard = self.shared.gradle.lock().unwrap_or_else(|e| e.into_inner());
        f()
    }

    /// The artifact of a path dependency built earlier in this invocation, or builds it with `build`.
    /// While one thread builds it, the others asking for the same `key` wait for its result.
    pub fn built_artifact(&self, key: String, build: impl FnOnce() -> Result<Option<PathBuf>>) -> Result<Option<PathBuf>> {
        let entry = self.shared.built.lock().expect("lock poisoned").entry(key).or_default().clone();
        let mut slot = entry.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(built) = &*slot {
            return built.clone().map_err(|e| anyhow::anyhow!(e));
        }
        let built = build();
        *slot = Some(built.as_ref().map(Clone::clone).map_err(|e| format!("{e:#}")));
        built
    }

    /// Every package this one reaches through path dependencies that apply to `target`, with its
    /// effective scope; with `dev`, the ones only its dev-dependencies reach.
    pub fn local_dependencies(&self, target: TargetKind, dev: bool) -> Result<Vec<LocalDependency>> {
        let mut out: Vec<LocalDependency> = Vec::new();
        let mut queue: VecDeque<(usize, usize, Scope)> = self
            .ws
            .path_dependencies()
            .iter()
            .filter(|(dep, _)| dep.applies_to(target, dev))
            .map(|(dep, index)| (self.package, *index, dep.scope))
            .collect();
        while let Some((consumer, package, scope)) = queue.pop_front() {
            let pkg = self.project.package(package);
            if !pkg.manifest().targets.is_enabled(target) {
                bail!(
                    "`{}` depends on the path dependency `{}`, which does not enable the {target} target; add [target.{target}] to {}",
                    self.project.package(consumer).name(),
                    pkg.name(),
                    pkg.manifest_path().display()
                );
            }
            let scope = match out.iter_mut().find(|d| d.package == package) {
                // Reached again: widen the scope when it is needed both to compile and at runtime.
                Some(existing) if existing.scope == scope || existing.scope == Scope::Compile => continue,
                Some(existing) => {
                    existing.scope = Scope::Compile;
                    Scope::Compile
                }
                None => {
                    out.push(LocalDependency { package, scope });
                    scope
                }
            };
            for (dep, child) in pkg.path_dependencies().iter().filter(|(dep, _)| dep.applies_to(target, false)) {
                if let Some(s) = inherited_scope(scope, dep.scope, dep.exported) {
                    queue.push_back((package, *child, s));
                }
            }
        }
        // A package's tests may use a package that depends on it; it is never its own dependency.
        out.retain(|d| d.package != self.package);
        if dev {
            let main = self.local_dependencies(target, false)?;
            out.retain(|d| !main.iter().any(|m| m.package == d.package));
        }
        Ok(out)
    }

    pub fn shell(&self) -> &Shell {
        self.config.shell()
    }

    /// The environment of the package for this profile, for a command that starts programs or
    /// tasks: `[env]`, the `.env` files and the shell. Fails when a required variable is not set.
    pub fn project_env(&self, opts: &EnvOptions) -> Result<ProjectEnv> {
        self.resolve_env(opts)?.ensure_set()
    }

    /// The environment of the package for this profile, with unset required variables left in
    /// [`ProjectEnv::missing`] for the caller to judge. Builds and their hooks use this.
    pub fn resolve_env(&self, opts: &EnvOptions) -> Result<ProjectEnv> {
        crate::env::resolve(self.ws, self.profile.name(), opts)
    }

    /// Whether the programs of `target` start with the project environment. Android installs an
    /// app that gets no environment, and a Wasm or JS browser app runs in a browser, which can't read it.
    pub fn program_env_applies(&self, target: TargetKind) -> bool {
        match target {
            TargetKind::Jvm | TargetKind::Native => true,
            TargetKind::Wasm => self.ws.manifest().targets.wasm.as_ref().is_none_or(|w| w.environment != WasmEnvironment::Browser),
            TargetKind::Js => self.ws.manifest().targets.js.as_ref().is_none_or(|j| j.environment != JsEnvironment::Browser),
            TargetKind::Android => false,
        }
    }

    pub fn jdk(&self) -> Result<&Jdk> {
        self.shared.jdk.get_or_try_init(|| {
            let manifest = self.ws.manifest();
            let jdk = kargo_toolchain::jdks::resolve_for(self.config, &manifest.toolchain.jdk, jdk_needs(manifest))?;
            jdk.require_version(MIN_JDK)?;
            self.shell().verbose("Using", format!("JDK {} at {}", jdk.version, jdk.home.display()));
            Ok(jdk)
        })
    }

    /// The JDK that runs the Kotlin compiler, checked against the Kotlin version
    /// so an unsupported combination fails with a clear message instead of a
    /// compiler stack trace.
    pub fn compiler_jdk(&self) -> Result<&Jdk> {
        let jdk = self.jdk()?;
        kargo_toolchain::kotlin::check_compiler_jdk(&self.ws.manifest().kotlin.version, jdk)?;
        Ok(jdk)
    }

    /// The JDK that compiles the JVM target: the `compiler_jdk`, once it is known to be new
    /// enough for `jvm-target`. Checked here so a bad target fails before anything downloads.
    pub fn jvm_jdk(&self) -> Result<&Jdk> {
        let jdk = self.compiler_jdk()?;
        if let Some(jvm) = &self.ws.manifest().targets.jvm {
            jvm.target_for(jdk.version)?;
        }
        Ok(jdk)
    }

    /// The Kotlin compiler, with its classpath locked in `kargo.lock` as `tools/kotlin-compiler`. Packages built
    /// together share one Kotlin version, so the whole project shares the entry.
    pub fn kotlin_compiler(&self) -> Result<&KotlinCompiler> {
        self.shared.compiler.get_or_try_init(|| {
            use kargo_toolchain::kotlin::{compiler_roots, has_build_tools};
            let version = &self.ws.manifest().kotlin.version;
            // A locked classpath tells by itself whether it has the Build Tools API (incremental compilation).
            for build_tools in [true, false] {
                if let Some(locked) = self.restore(COMPILER_KEY, &Platform::Jvm, &compiler_roots(version, build_tools), &[])? {
                    return Ok(KotlinCompiler::new(version, locked.files(), build_tools));
                }
            }
            // Without the Build Tools API (not published, or offline), compile non-incrementally.
            if !self.locked
                && has_build_tools(&self.client, version)
                && let Ok(resolution) = self.resolve_at(COMPILER_KEY, Platform::Jvm, &compiler_roots(version, true), &[])
            {
                return Ok(KotlinCompiler::new(version, resolution.files(), true));
            }
            let resolution = self
                .resolve_at(COMPILER_KEY, Platform::Jvm, &compiler_roots(version, false), &[])
                .with_context(|| format!("failed to obtain the Kotlin {version} compiler"))?;
            Ok(KotlinCompiler::new(version, resolution.files(), false))
        })
    }

    pub fn native_toolchain(&self) -> Result<&NativeToolchain> {
        self.shared.native.get_or_try_init(|| NativeToolchain::ensure(self.config, &self.client, &self.ws.manifest().kotlin.version))
    }

    /// The KSP2 command-line tool's classpath (`symbol-processing-aa-embeddable` and its dependencies), locked in
    /// `kargo.lock` as `tools/ksp`.
    pub fn ksp_tool(&self) -> Result<Vec<PathBuf>> {
        let version = builtins::ksp_version(self.ws.manifest());
        let key = self.tool_key("ksp");
        // Held while resolving, so parallel targets never download KSP twice.
        let mut tools = self.shared.ksp.lock().unwrap_or_else(|e| e.into_inner());
        let cached = format!("{key}@{version}");
        if let Some(classpath) = tools.get(&cached) {
            return Ok(classpath.clone());
        }
        let root =
            DependencyRequest::new(ModuleId::new("com.google.devtools.ksp", "symbol-processing-aa-embeddable"), Some(version.to_owned()));
        let resolution = self.resolve_at(&key, Platform::Jvm, &[root], &[]).with_context(|| format!("failed to obtain KSP {version}"))?;
        let classpath = resolution.files();
        tools.insert(cached, classpath.clone());
        Ok(classpath)
    }

    /// The `kargo.lock` key of the tool `name`, `tools/<name>`: one for each package, whose settings pick the
    /// tools' versions, but shared by its profiles.
    pub fn tool_key(&self, name: &str) -> String {
        self.ws.lock_key(&format!("tools/{name}"))
    }

    /// Resolves the classpath of a tool the build runs, such as Dokka's plugins, locked in `kargo.lock` under
    /// [`BuildContext::tool_key`] like a dependency resolution.
    pub fn resolve_tool(&self, name: &str, roots: &[DependencyRequest]) -> Result<Resolution> {
        self.resolve_at(&self.tool_key(name), Platform::Jvm, roots, &[])
    }

    /// The resolutions of the build's tools that `kargo.lock` holds for this package, by key: the Kotlin
    /// compiler, `tools/*` (KSP, the JUnit launcher, compiler plugins, Dokka, ...), `generate/*` and the
    /// benchmark generator. Only the tools some build already used are there.
    pub fn locked_tools(&self) -> Vec<(String, crate::lockfile::LockedResolution)> {
        let prefix = self.ws.lock_key("");
        let lock = self.shared.lock.lock().expect("lock poisoned");
        lock.resolutions
            .iter()
            .filter(|(key, _)| {
                let own = key.strip_prefix(prefix.as_str()).unwrap_or("");
                key.as_str() == COMPILER_KEY
                    || (key.starts_with(prefix.as_str())
                        && (own.starts_with("tools/") || own.starts_with("generate/") || own == "jvm/bench-tools"))
            })
            .map(|(key, locked)| (key.clone(), locked.clone()))
            .collect()
    }

    pub fn setup(&self, target: TargetKind) -> Result<TargetSetup> {
        self.setup_for_suite(target, None)
    }

    /// The dependency configuration of `target`, with the Maven dependencies of the test
    /// suite `suite` added to the test classpath (`None` is the default `unit` suite).
    /// A suite's dependencies replace the ones with the same artifact, as the suite is
    /// the most specific declaration.
    pub fn setup_for_suite(&self, target: TargetKind, suite: Option<&TestSuite>) -> Result<TargetSetup> {
        let manifest = self.ws.manifest();
        let mut contributions = builtins::contributions(manifest, target, self.ws.root())?;
        if let Some(compose) = &manifest.frameworks.compose
            && let Some(plugin) = contributions.plugins.iter_mut().find(|p| p.id.as_deref() == Some(builtins::COMPOSE_PLUGIN_ID))
        {
            plugin.options.extend(builtins::compose_compiler_options(compose, self.ws.root(), &self.ws.work_dir("compose"), target));
        }
        let user = with_profile_dependencies(manifest.dependencies_for(target), &self.ws.profile(&self.profile)?.dependencies);
        let mut all = merge_deps(&user, &contributions.dependencies);
        let mut dev_all = merge_deps(&manifest.dev_dependencies, &contributions.dev_dependencies);
        if let Some(suite) = suite {
            dev_all = merge_deps(&suite.dependencies, &dev_all);
            // The suite's own declaration wins over a dev-dependency of the same artifact.
            // `merge_deps` keeps the first occurrence, so put the suite's first.
            let mut reordered = suite.dependencies.clone();
            for d in dev_all {
                if !reordered.iter().any(|u| u.same_artifact(&d)) {
                    reordered.push(d);
                }
            }
            dev_all = reordered;
        }
        let (own, dev_own) = (all.len(), dev_all.len());
        // Path dependencies bring their Maven dependencies (declared and framework ones) along:
        // the `exported` ones (and the frameworks' libraries) with Maven's scope rules, the others
        // at runtime. What the consumer declares itself comes first and wins.
        for dev in [false, true] {
            for local in self.local_dependencies(target, dev)? {
                let pkg = self.project.package(local.package);
                let theirs = builtins::contributions(pkg.manifest(), target, pkg.root())?.dependencies;
                let inherited: Vec<Dependency> = merge_deps(&pkg.manifest().dependencies_for(target), &theirs)
                    .into_iter()
                    .filter_map(|mut d| {
                        if !d.bom {
                            d.scope = inherited_scope(local.scope, d.scope, d.exported)?;
                        }
                        Some(d)
                    })
                    .collect();
                if dev {
                    merge_inherited(&mut dev_all, dev_own, inherited);
                } else {
                    merge_inherited(&mut all, own, inherited);
                }
            }
        }
        // The suite's own path dependencies bring their Maven dependencies too.
        if let Some(suite) = suite {
            for path in &suite.path_dependencies {
                let index = self
                    .ws
                    .path_dependencies()
                    .iter()
                    .find(|(d, _)| d.name == path.name)
                    .map(|(_, i)| *i)
                    .or_else(|| self.project.packages().iter().position(|p| p.name() == path.name));
                let Some(index) = index else { continue };
                let pkg = self.project.package(index);
                let theirs = builtins::contributions(pkg.manifest(), target, pkg.root())?.dependencies;
                let inherited: Vec<Dependency> = merge_deps(&pkg.manifest().dependencies_for(target), &theirs)
                    .into_iter()
                    .filter_map(|mut d| {
                        if !d.bom {
                            d.scope = inherited_scope(path.scope, d.scope, d.exported)?;
                        }
                        Some(d)
                    })
                    .collect();
                merge_inherited(&mut dev_all, dev_own, inherited);
                // The suite's direct Maven dependencies of the path package itself are handled
                // through `local_dependencies_for_suite` artifacts; their versions come from the lock.
            }
        }
        // Test fixtures compile against their package's dev-dependencies, which the tests using them need too.
        for (_, index) in self.ws.path_dependencies().iter().filter(|(d, _)| d.test_fixtures && d.applies_to(target, true)) {
            let pkg = self.project.package(*index);
            let theirs = builtins::contributions(pkg.manifest(), target, pkg.root())?.dev_dependencies;
            merge_inherited(&mut dev_all, dev_own, merge_deps(&pkg.manifest().dev_dependencies, &theirs));
        }
        if let Some(suite) = suite {
            for path in &suite.path_dependencies {
                if !path.test_fixtures {
                    continue;
                }
                let Some(index) = self.project.packages().iter().position(|p| p.name() == path.name) else { continue };
                let pkg = self.project.package(index);
                let theirs = builtins::contributions(pkg.manifest(), target, pkg.root())?.dev_dependencies;
                merge_inherited(&mut dev_all, dev_own, merge_deps(&pkg.manifest().dev_dependencies, &theirs));
            }
        }
        let (boms, main): (Vec<_>, Vec<_>) = all.into_iter().partition(|d| d.bom);
        let (dev_boms, dev): (Vec<_>, Vec<_>) = dev_all.into_iter().partition(|d| d.bom);
        let mut boms = boms;
        boms.extend(dev_boms);
        Ok(TargetSetup { target, contributions, main, dev, boms })
    }

    /// Every package the suite's tests reach through path dependencies: the main and dev
    /// ones, plus the suite's own `path` dependencies, with their transitive main dependencies.
    pub fn local_dependencies_for_suite(&self, target: TargetKind, suite: Option<&TestSuite>) -> Result<Vec<LocalDependency>> {
        let mut out = self.local_dependencies(target, false)?;
        out.extend(self.local_dependencies(target, true)?);
        let Some(suite) = suite else {
            return Ok(out);
        };
        for path in &suite.path_dependencies {
            let Some(index) = self.project.packages().iter().position(|p| p.name() == path.name) else {
                anyhow::bail!(
                    "test suite `{}` depends on `{}` through `path`, which is not part of this project; add it with a `path` dependency",
                    suite.name,
                    path.name
                );
            };
            if out.iter().any(|d| d.package == index) {
                continue;
            }
            out.push(LocalDependency { package: index, scope: path.scope });
            // The suite package's own main dependencies, as `local_dependencies` would add them.
            for local in self.project.package(index).path_dependencies().iter().filter(|(d, _)| d.applies_to(target, false)) {
                if let Some(s) = inherited_scope(path.scope, local.0.scope, local.0.exported)
                    && !out.iter().any(|d| d.package == local.1)
                {
                    out.push(LocalDependency { package: local.1, scope: s });
                }
            }
        }
        out.retain(|d| d.package != self.package);
        Ok(out)
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
        // The declared repositories, so a mirror or `--vendored` keeps the lock.
        for repo in self.client.declared_repositories() {
            fp.str(&repo.url);
            // Only rules change the fingerprint, so the locks of projects without them stay valid.
            if !repo.content.is_unrestricted() {
                fp.str(&format!("{:?}", repo.content));
            }
        }
        fp.finish()
    }

    /// The resolution `key` of this profile. A profile with dependencies of its own resolves them with
    /// the others (`jvm/runtime@staging`); other profiles keep sharing the entries of the default ones.
    fn profile_resolution(&self, key: &str) -> Result<String> {
        let profile = self.ws.profile(&self.profile)?;
        Ok(if profile.dependencies.is_empty() { key.to_owned() } else { format!("{key}@{}", self.profile.name()) })
    }

    /// Resolves `roots` for `platform`, reusing `kargo.lock` when possible.
    pub fn resolve(&self, key: &str, platform: Platform, roots: &[DependencyRequest], boms: &[Dependency]) -> Result<Resolution> {
        self.resolve_at(&self.ws.lock_key(&self.profile_resolution(key)?), platform, roots, boms)
    }

    /// Resolves `roots` again, without `kargo.lock` and without downloading artifacts, to say why each module
    /// of the resolution `key` has its version (`kargo why`).
    pub fn explain(&self, key: &str, platform: Platform, roots: &[DependencyRequest], boms: &[Dependency]) -> Result<Explanation> {
        let mut resolver = Resolver::new(&self.client, platform);
        for bom in boms {
            let version = bom.version.as_deref().context("BOM without version")?;
            resolver.import_bom(&bom.module, version)?;
        }
        let (_, explanation) = resolver.explain(roots).with_context(|| format!("failed to resolve dependencies for `{key}`"))?;
        Ok(explanation)
    }

    /// The resolution locked under the full `kargo.lock` key `key`, if it is there for these inputs (and not
    /// `--refresh`), with every artifact checked against its locked SHA-256.
    fn restore(&self, key: &str, platform: &Platform, roots: &[DependencyRequest], boms: &[Dependency]) -> Result<Option<Resolution>> {
        if self.config.refresh() {
            return Ok(None);
        }
        let inputs = self.inputs_fingerprint(platform, roots, boms);
        let lock = self.shared.lock.lock().expect("lock poisoned");
        // Under --locked, cached artifacts are hashed rather than trusted by their stamps.
        lock.restore(key, &inputs, &self.client, !self.locked)
    }

    /// [`BuildContext::resolve`] under the full `kargo.lock` key `key`.
    fn resolve_at(&self, key: &str, platform: Platform, roots: &[DependencyRequest], boms: &[Dependency]) -> Result<Resolution> {
        let inputs = self.inputs_fingerprint(&platform, roots, boms);
        if let Some(res) = self.restore(key, &platform, roots, boms)? {
            return Ok(res);
        }
        if self.locked {
            bail!(
                "kargo.lock needs to be updated for `{key}` but --locked was passed; run `kargo update` (or `kargo fetch`) without --locked and commit kargo.lock"
            );
        }
        self.shell().verbose("Resolving", key);
        let mut resolver = Resolver::new(&self.client, platform);
        if let Some(plan) = &self.update {
            for (module, version, build) in plan.pins(key, &inputs) {
                match build {
                    Some(build) => resolver.pin_build(module, &version, &build),
                    None => resolver.pin(module, &version),
                }
            }
        }
        for bom in boms {
            let version = bom.version.as_deref().context("BOM without version")?;
            resolver.import_bom(&bom.module, version)?;
        }
        let resolution = resolver.resolve(roots).with_context(|| format!("failed to resolve dependencies for `{key}`"))?;
        // Verifying the artifacts takes network requests and hashing, so the lockfile is not held meanwhile.
        let previous = self.shared.lock.lock().expect("lock poisoned").resolutions.get(key).cloned();
        let locked = Lockfile::lock_resolution(previous.as_ref(), &inputs, &resolution, &self.client)?;
        self.shared.lock.lock().expect("lock poisoned").resolutions.insert(key.to_owned(), locked);
        self.shared.lock_dirty.store(true, Ordering::SeqCst);
        Ok(resolution)
    }

    /// Pins files that are fetched apart from a resolution, such as the web resources of Wasm libraries, under
    /// `key` in `kargo.lock`, the way [`BuildContext::resolve`] pins a resolution's artifacts. `inputs` names what
    /// is fetched (`group:artifact:version` and the file); `fetch` downloads it when the lock does not match, as
    /// one node per module. Nothing is locked, and an old entry is dropped, when `inputs` is empty.
    pub fn lock_files(&self, key: &str, inputs: &[String], fetch: impl FnOnce() -> Result<Resolution>) -> Result<Resolution> {
        self.lock_files_at(&self.ws.lock_key(&self.profile_resolution(key)?), inputs, fetch)
    }

    /// [`BuildContext::lock_files`] under the full `kargo.lock` key `key`.
    fn lock_files_at(&self, key: &str, inputs: &[String], fetch: impl FnOnce() -> Result<Resolution>) -> Result<Resolution> {
        if inputs.is_empty() {
            let mut lock = self.shared.lock.lock().expect("lock poisoned");
            if lock.resolutions.contains_key(key) {
                if self.locked {
                    bail!(
                        "kargo.lock needs to be updated for `{key}` but --locked was passed; run `kargo update` without --locked and commit kargo.lock"
                    );
                }
                lock.resolutions.remove(key);
                self.shared.lock_dirty.store(true, Ordering::SeqCst);
            }
            return Ok(Resolution::default());
        }
        let mut fp = Fingerprint::new();
        for input in inputs {
            fp.str(input);
        }
        fp.str("--repos");
        for repo in self.client.declared_repositories() {
            fp.str(&repo.url);
            // Only rules change the fingerprint, so the locks of projects without them stay valid.
            if !repo.content.is_unrestricted() {
                fp.str(&format!("{:?}", repo.content));
            }
        }
        let fingerprint = fp.finish();
        if !self.config.refresh() {
            let lock = self.shared.lock.lock().expect("lock poisoned");
            if let Some(res) = lock.restore(key, &fingerprint, &self.client, !self.locked)? {
                return Ok(res);
            }
        }
        if self.locked {
            bail!(
                "kargo.lock needs to be updated for `{key}` but --locked was passed; run `kargo update` (or `kargo fetch`) without --locked and commit kargo.lock"
            );
        }
        let fetched = fetch()?;
        let previous = self.shared.lock.lock().expect("lock poisoned").resolutions.get(key).cloned();
        let locked = Lockfile::lock_resolution(previous.as_ref(), &fingerprint, &fetched, &self.client)?;
        self.shared.lock.lock().expect("lock poisoned").resolutions.insert(key.to_owned(), locked);
        self.shared.lock_dirty.store(true, Ordering::SeqCst);
        Ok(fetched)
    }

    /// Fetches compiler plugin jars, each locked in `kargo.lock` like a [`BuildContext::tool_jar`].
    pub fn plugin_jars(&self, plugins: &[PluginSpec]) -> Result<Vec<ResolvedPlugin>> {
        let kotlin = &self.ws.manifest().kotlin.version;
        plugins
            .iter()
            .map(|p| {
                let version = p.version.as_deref().unwrap_or(kotlin);
                let jar = self
                    .tool_jar(&p.group, &p.artifact, version)
                    .with_context(|| format!("failed to fetch compiler plugin {}:{}:{version}", p.group, p.artifact))?;
                Ok(ResolvedPlugin { jar, options: builtins::plugin_option_args(p) })
            })
            .collect()
    }

    /// A single tool jar (e.g. the JUnit console launcher), without dependencies, locked in `kargo.lock` as
    /// `tools/<artifact>`.
    pub fn tool_jar(&self, group: &str, artifact: &str, version: &str) -> Result<PathBuf> {
        self.locked_tool_file(Artifact::new(ModuleId::new(group, artifact), version))
    }

    /// A single tool jar with a Maven classifier (e.g. JaCoCo's `runtime` agent or `nodeps`
    /// CLI), locked in `kargo.lock` as `tools/<artifact>` like [`BuildContext::tool_jar`].
    pub fn tool_classified_jar(&self, group: &str, artifact: &str, classifier: &str, version: &str) -> Result<PathBuf> {
        self.locked_tool_file(Artifact::new(ModuleId::new(group, artifact), version).with_classifier(Some(classifier.to_owned())))
    }

    /// R8 is published only to Google's Maven repository, so use a dedicated client while still
    /// recording the verified artifact in the project's lockfile.
    pub fn r8_jar(&self, version: &str) -> Result<PathBuf> {
        let client = MavenClient::new(self.config, vec![Repository::new("google", "https://dl.google.com/dl/android/maven2")]);
        let artifact = Artifact::new(ModuleId::new("com.android.tools", "r8"), version);
        self.locked_tool_file_from(&client, artifact)
    }

    /// Fetches `artifact` and pins it in `kargo.lock` under the tool key of its artifact name.
    fn locked_tool_file(&self, artifact: Artifact) -> Result<PathBuf> {
        self.locked_tool_file_from(&self.client, artifact)
    }

    fn locked_tool_file_from(&self, client: &MavenClient, artifact: Artifact) -> Result<PathBuf> {
        let key = self.tool_key(&artifact.module.artifact);
        let rel = client.artifact_path(&artifact)?;
        let inputs = [format!("{}:{} {rel}", artifact.module, artifact.version)];
        let locked = self.lock_files_at(&key, &inputs, || {
            let file = client.fetch_verified(&rel)?;
            let node = ResolvedNode {
                module: artifact.module.clone(),
                version: artifact.version.clone(),
                files: vec![file],
                dependencies: Vec::new(),
            };
            Ok(Resolution { roots: Vec::new(), nodes: vec![node] })
        })?;
        locked.files().into_iter().next().with_context(|| format!("kargo.lock lists no file for `{key}`; run `kargo update`"))
    }

    /// The `kargo.lock` key of the npm package `name` of a build tool, `npm-tools/<name>`.
    pub fn npm_tool_key(&self, name: &str) -> String {
        self.ws.lock_key(&format!("npm-tools/{name}"))
    }

    /// The version of the npm package of a build tool that `request` (what `kargo.toml` asks for) stands for, pinned
    /// in `kargo.lock` under [`BuildContext::npm_tool_key`]: `resolve` runs only when the lock has no entry for this
    /// request (or with `--refresh`), and `--locked` refuses to make one.
    pub fn lock_npm_tool(&self, name: &str, request: &str, resolve: impl FnOnce() -> Result<String>) -> Result<String> {
        let key = self.npm_tool_key(name);
        if !self.config.refresh()
            && let Some(tool) = self.shared.lock.lock().expect("lock poisoned").npm_tools.get(&key).filter(|t| t.request == request)
        {
            return Ok(tool.version.clone());
        }
        if self.locked {
            bail!(
                "kargo.lock needs to be updated for `{key}` but --locked was passed; run `kargo update` (or `kargo fetch`) without --locked and commit kargo.lock"
            );
        }
        let version = resolve()?;
        let locked = crate::lockfile::LockedNpmTool { request: request.to_owned(), version: version.clone() };
        self.shared.lock.lock().expect("lock poisoned").npm_tools.insert(key, locked);
        self.shared.lock_dirty.store(true, Ordering::SeqCst);
        Ok(version)
    }

    /// A copy of the lockfile as currently resolved.
    pub fn lockfile(&self) -> Lockfile {
        self.shared.lock.lock().expect("lock poisoned").clone()
    }

    /// Fetches the self-contained (`-all` classifier) jar of a command-line tool such as ktlint or detekt, locked
    /// in `kargo.lock` as `tools/<artifact>`.
    pub fn tool_fat_jar(&self, group: &str, artifact: &str, version: &str) -> Result<PathBuf> {
        self.locked_tool_file(Artifact::new(ModuleId::new(group, artifact), version).with_classifier(Some("all".to_owned())))
    }

    /// Persists `kargo.lock` if any resolution changed it.
    pub fn save_lockfile(&self) -> Result<()> {
        if self.shared.lock_dirty.load(Ordering::SeqCst) && !self.dry_run {
            let lock = self.shared.lock.lock().expect("lock poisoned");
            if lock.save(&self.project.lockfile_path())? {
                self.shell().verbose("Updated", "kargo.lock");
            }
        }
        Ok(())
    }

    /// Replaces the lockfile content (used by `kargo update`).
    pub fn reset_lockfile(&self, keep: Option<&[String]>) {
        let mut lock = self.shared.lock.lock().expect("lock poisoned");
        match keep {
            Some(keys) => lock.retain(keys),
            None => {
                lock.resolutions.clear();
                lock.npm_tools.clear();
            }
        }
        self.shared.lock_dirty.store(true, Ordering::SeqCst);
    }

    /// Puts back the npm tools of an earlier lockfile after [`BuildContext::reset_lockfile`]: a selective `kargo update`
    /// keeps the TypeScript compiler it had.
    pub fn keep_npm_tools(&self, tools: &std::collections::BTreeMap<String, crate::lockfile::LockedNpmTool>) {
        self.shared.lock.lock().expect("lock poisoned").npm_tools.extend(tools.clone());
    }

    /// Saves `kargo.lock` and releases the build-directory lock, so a long-running
    /// program started by `kargo run` does not block other Kargo commands.
    pub fn unlock(&self) -> Result<()> {
        self.save_lockfile()?;
        self.shared.build_lock.lock().expect("lock poisoned").take();
        Ok(())
    }

    /// The path of the held build-directory lock, if any.
    pub fn build_lock_path(&self) -> Option<PathBuf> {
        self.shared.build_lock.lock().expect("lock poisoned").as_ref().map(|l| l.path().to_path_buf())
    }

    pub fn fingerprint_dir(&self, target: TargetKind) -> PathBuf {
        self.ws.output_dir(&self.profile, target).join(".fingerprint")
    }
}

/// Fetches the self-contained (`-all` classifier) jar of a command-line tool, for callers that
/// have no [`BuildContext`] (and so don't take the lock on `target/`).
pub fn fat_jar(client: &MavenClient, group: &str, artifact: &str, version: &str) -> Result<PathBuf> {
    let jar = kargo_maven::Artifact::new(kargo_maven::ModuleId::new(group, artifact), version).with_classifier(Some("all".to_owned()));
    client.fetch_required(&jar.repo_path())
}

impl Drop for BuildContext<'_> {
    fn drop(&mut self) {
        // Keep successful resolutions even if a later step (e.g. compilation) failed.
        if !self.owner {
            return;
        }
        if let Err(e) = self.save_lockfile() {
            self.shell().warn(format!("failed to write kargo.lock: {e:#}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Project;

    #[test]
    fn profile_dependencies_replace_the_ones_with_the_same_artifact() {
        let dep = |coord: &str, version: &str| Dependency::new(coord.parse().unwrap(), Some(version.to_owned()));
        let user = vec![dep("g:a", "1"), dep("g:b", "1")];
        let merged = with_profile_dependencies(user, &[dep("g:a", "2"), dep("g:c", "1")]);
        let versions: Vec<_> = merged.iter().map(|d| (d.module.artifact.as_str(), d.version.as_deref().unwrap())).collect();
        assert_eq!(versions, [("b", "1"), ("a", "2"), ("c", "1")]);
    }

    #[test]
    fn exported_dependencies_keep_their_scope() {
        use Scope::*;
        assert_eq!(inherited_scope(Compile, Compile, true), Some(Compile));
        assert_eq!(inherited_scope(Compile, Compile, false), Some(Runtime));
        assert_eq!(inherited_scope(Compile, Runtime, true), Some(Runtime));
        assert_eq!(inherited_scope(Provided, Compile, true), Some(Provided));
        assert_eq!(inherited_scope(Provided, Compile, false), None);
        assert_eq!(inherited_scope(Runtime, Compile, false), Some(Runtime));
        assert_eq!(inherited_scope(Compile, Provided, true), None);
    }

    #[test]
    fn scopes_combine_like_maven() {
        use Scope::*;
        assert_eq!(combine_scopes(Compile, Compile), Some(Compile));
        assert_eq!(combine_scopes(Compile, Runtime), Some(Runtime));
        assert_eq!(combine_scopes(Runtime, Compile), Some(Runtime));
        assert_eq!(combine_scopes(Provided, Compile), Some(Provided));
        assert_eq!(combine_scopes(Provided, Runtime), Some(Provided));
        assert_eq!(combine_scopes(Compile, Provided), None);
        // A dependency's KSP processors stay with it.
        assert_eq!(combine_scopes(Compile, Ksp), None);
        assert_eq!(combine_scopes(Ksp, Compile), None);
    }

    #[test]
    fn path_dependencies_bring_their_dependencies_with_scopes() {
        use kargo_util::shell::{Shell, Verbosity};
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let write = |rel: &str, text: &str| {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                path,
                format!(
                    "[package]
name = \"{}\"
version = \"0.1.0\"
{text}",
                    rel.split('/').next().unwrap()
                ),
            )
            .unwrap();
        };
        write(
            "app/kargo.toml",
            "[dependencies]
lib = { path = \"../lib\" }
tool = { path = \"../tool\", scope = \"provided\" }
\"g:shared\" = \"2.0\"
             [dev-dependencies]
fixtures = { path = \"../fixtures\" }
[target.wasm]
",
        );
        write(
            "lib/kargo.toml",
            "[dependencies]
core = { path = \"../core\", scope = \"runtime\" }
\"g:shared\" = \"1.0\"
\"g:lib\" = { version = \"1.0\", exported = true }
\"g:impl\" = \"1.0\"
             \"g:hidden\" = { version = \"1.0\", scope = \"provided\" }
",
        );
        write(
            "core/kargo.toml",
            "[dependencies]
\"g:core\" = \"1.0\"
",
        );
        write(
            "tool/kargo.toml",
            "[dependencies]
\"g:tool\" = { version = \"1.0\", exported = true }
\"g:tool-impl\" = \"1.0\"
",
        );
        write(
            "fixtures/kargo.toml",
            "[dependencies]
\"g:fixture\" = \"1.0\"
lib = { path = \"../lib\" }
",
        );

        let project = Project::load(&dir.path().join("app/kargo.toml")).unwrap();
        let config = Config::new(home.path().to_path_buf(), Shell::new(Verbosity::Quiet));
        let ctx = BuildContext::new(&config, &project, 0, ProfileKind::Dev).unwrap();
        let name = |d: &LocalDependency| (project.package(d.package).name().to_owned(), d.scope);
        let main: Vec<_> = ctx.local_dependencies(TargetKind::Jvm, false).unwrap().iter().map(name).collect();
        assert_eq!(main, [("lib".to_owned(), Scope::Compile), ("tool".to_owned(), Scope::Provided), ("core".to_owned(), Scope::Runtime)]);
        // `lib` is already a main dependency, so the dev closure only adds `fixtures`.
        let dev: Vec<_> = ctx.local_dependencies(TargetKind::Jvm, true).unwrap().iter().map(name).collect();
        assert_eq!(dev, [("fixtures".to_owned(), Scope::Compile)]);

        let setup = ctx.setup(TargetKind::Jvm).unwrap();
        let find = |deps: &[Dependency], artifact: &str| deps.iter().find(|d| d.module.artifact == artifact).cloned();
        // The consumer's own declaration wins over the dependency's.
        assert_eq!(find(&setup.main, "shared").unwrap().version.as_deref(), Some("2.0"));
        // Only what `lib` exports is there to compile against; the rest comes at runtime.
        assert_eq!(find(&setup.main, "lib").unwrap().scope, Scope::Compile);
        assert_eq!(find(&setup.main, "impl").unwrap().scope, Scope::Runtime);
        assert_eq!(find(&setup.main, "core").unwrap().scope, Scope::Runtime);
        assert_eq!(find(&setup.main, "tool").unwrap().scope, Scope::Provided);
        assert!(find(&setup.main, "tool-impl").is_none(), "a provided dependency brings only its API");
        assert!(find(&setup.main, "hidden").is_none(), "provided dependencies are not passed on");
        assert!(find(&setup.dev, "fixture").is_some());

        let err = format!("{:#}", ctx.setup(TargetKind::Wasm).unwrap_err());
        assert!(err.contains("`app` depends on the path dependency `lib`, which does not enable the wasm target"), "{err}");
    }

    #[test]
    fn npm_tools_are_pinned_in_the_lockfile() {
        use kargo_util::shell::{Shell, Verbosity};
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("kargo.toml"), "[package]\nname = \"app\"\nversion = \"0.1.0\"\n").unwrap();
        let project = Project::load(&dir.path().join("kargo.toml")).unwrap();
        let config = Config::new(home.path().to_path_buf(), Shell::new(Verbosity::Quiet));
        let ctx = BuildContext::new(&config, &project, 0, ProfileKind::Dev).unwrap();

        let resolved = std::cell::Cell::new(0);
        let resolve = |version: &str| {
            resolved.set(resolved.get() + 1);
            Ok(version.to_owned())
        };
        assert_eq!(ctx.lock_npm_tool("typescript", "latest", || resolve("7.0.2")).unwrap(), "7.0.2");
        assert_eq!(ctx.lock_npm_tool("typescript", "latest", || resolve("8.0.0")).unwrap(), "7.0.2", "the lock wins over the registry");
        assert_eq!(resolved.get(), 1);
        assert_eq!(ctx.lock_npm_tool("typescript", "^5", || resolve("5.9.3")).unwrap(), "5.9.3", "another request is resolved again");
        ctx.save_lockfile().unwrap();
        let text = std::fs::read_to_string(dir.path().join("kargo.lock")).unwrap();
        assert!(text.contains("[npm-tool.\"npm-tools/typescript\"]") && text.contains("version = \"5.9.3\""), "{text}");

        // A new run reads it back, and `--locked` refuses what the lock lacks.
        drop(ctx);
        let mut ctx = BuildContext::new(&config, &project, 0, ProfileKind::Dev).unwrap();
        ctx.locked = true;
        assert_eq!(ctx.lock_npm_tool("typescript", "^5", || unreachable!()).unwrap(), "5.9.3");
        let err = format!("{:#}", ctx.lock_npm_tool("typescript", "latest", || unreachable!()).unwrap_err());
        assert!(err.contains("kargo.lock needs to be updated for `npm-tools/typescript`"), "{err}");
        // `kargo update` forgets it.
        ctx.reset_lockfile(None);
        assert!(ctx.lockfile().npm_tools.is_empty());
    }

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
        let mut processor = Dependency::new("ap:ap".parse().unwrap(), Some("1".into()));
        processor.scope = Scope::Processor;
        let mut test_processor = Dependency::new("tp:tp".parse().unwrap(), Some("1".into()));
        test_processor.scope = Scope::Processor;
        let mut ksp = Dependency::new("k:k".parse().unwrap(), Some("1".into()));
        ksp.scope = Scope::Ksp;
        let mut test_ksp = Dependency::new("tk:tk".parse().unwrap(), Some("1".into()));
        test_ksp.scope = Scope::Ksp;
        let setup = TargetSetup {
            target: TargetKind::Jvm,
            contributions: Contributions::default(),
            main: vec![Dependency::new("c:c".parse().unwrap(), Some("1".into())), provided, runtime, processor, ksp],
            dev: vec![Dependency::new("t:t".parse().unwrap(), Some("1".into())), test_processor, test_ksp],
            boms: vec![],
        };
        let names = |r: Vec<DependencyRequest>| r.into_iter().map(|r| r.module.artifact).collect::<Vec<_>>();
        assert_eq!(names(setup.requests(&[Scope::Compile, Scope::Provided], false)), ["c", "p"]);
        assert_eq!(names(setup.requests(&[Scope::Compile, Scope::Runtime], false)), ["c", "r"]);
        assert_eq!(names(setup.requests(&[Scope::Compile, Scope::Runtime, Scope::Provided], true)), ["c", "p", "r", "t"]);
        // Processors never land on a classpath, only on the processor path.
        assert_eq!(names(setup.requests(&[Scope::Processor], false)), ["ap"]);
        assert_eq!(names(setup.requests(&[Scope::Processor], true)), ["ap", "tp"]);
        // KSP processors get their own path too.
        assert_eq!(names(setup.requests(&[Scope::Ksp], false)), ["k"]);
        assert_eq!(names(setup.requests(&[Scope::Ksp], true)), ["k", "tk"]);
    }
}
