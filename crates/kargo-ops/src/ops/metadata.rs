//! `kargo metadata`: a versioned JSON description of the project for editors, language
//! servers and other tools, so they never have to interpret `kargo.toml` themselves.
//!
//! The output lists every package of the project with its source, test and resource roots,
//! output directories, compiler arguments and plugins, entry points and tasks, and, unless
//! `--no-deps` is given, the resolved classpath (or klib path) of every target and scope.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Value, json};

use super::build::{ALL_SCOPES, existing_sources, native_entry_point, native_platforms, wasm_resolution};
use super::context::{BuildContext, TargetSetup};
use crate::builtins;
use crate::npm;
use crate::workspace::{Project, Workspace};
use kargo_compiler::common_kotlin_args;
use kargo_compiler::mainclass::find_jvm_main_class;
use kargo_manifest::{GeneratedKind, GeneratorAction, Scope, TargetKind};
use kargo_maven::{Platform, Resolution};
use kargo_util::hash::Fingerprint;

/// The version of the output format. It changes only when a field is removed or changes its
/// meaning; new fields can appear at any time.
pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Default)]
pub struct MetadataOptions {
    /// Leave out the resolved dependencies and the JDK (`--no-deps`): nothing is downloaded.
    pub no_deps: bool,
}

/// The files whose contents change the metadata: every package's `kargo.toml` and `kargo.lock`.
/// Editors watch them and import the project again when one changes.
pub fn watched_files(project: &Project) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    if project.is_workspace() {
        files.push(project.root().join(kargo_manifest::MANIFEST_NAME));
    }
    files.extend(project.packages().iter().map(Workspace::manifest_path));
    files.push(project.lockfile_path());
    files
}

/// A hash of the [`watched_files`]' contents (and of Kargo's version): the metadata is the same
/// as long as it is the same, so an editor can compare it before importing the project again.
pub fn fingerprint(project: &Project) -> String {
    let mut fp = Fingerprint::new();
    fp.str(env!("CARGO_PKG_VERSION"));
    for file in watched_files(project) {
        fp.str(&file.to_string_lossy());
        fp.str(&std::fs::read_to_string(&file).unwrap_or_default());
    }
    fp.finish()
}

/// The `kargo metadata` document. `ctx` is the context of the project's first member; every
/// package is described through a context of its own that shares its lockfile and toolchains.
pub fn metadata(ctx: &BuildContext, opts: &MetadataOptions) -> Result<Value> {
    let project = ctx.project;
    let packages = (0..project.packages().len()).map(|i| package(&ctx.for_package(i), opts)).collect::<Result<Vec<_>>>()?;
    // The fingerprint covers kargo.lock as this command leaves it.
    ctx.save_lockfile()?;
    let manifest = ctx.ws.manifest();
    let jdk = if opts.no_deps {
        None
    } else {
        match ctx.jdk() {
            Ok(jdk) => Some(jdk),
            Err(e) => {
                ctx.shell().warn(format!("no JDK for the project's metadata: {e:#}"));
                None
            }
        }
    };
    let native = kargo_toolchain::NativeToolchain::find(ctx.config, &manifest.kotlin.version)?;
    let profile = ctx.profile.name();
    Ok(json!({
        "version": FORMAT_VERSION,
        "kargo-version": env!("CARGO_PKG_VERSION"),
        "fingerprint": fingerprint(project),
        "watch": watched_files(project).iter().map(|p| path(p)).collect::<Vec<_>>(),
        "root": path(project.root()),
        "workspace": project.is_workspace(),
        "members": project.members().iter().map(|&i| project.package(i).name()).collect::<Vec<_>>(),
        "default-members": project.default_members().iter().map(|&i| project.package(i).name()).collect::<Vec<_>>(),
        "lockfile": path(&project.lockfile_path()),
        "target-dir": path(project.target_dir()),
        "profile": profile,
        "resolved": !opts.no_deps,
        "toolchain": {
            "kotlin": manifest.kotlin.version,
            "jdk": {
                "request": manifest.toolchain.jdk.to_string(),
                "home": jdk.map(|j| path(&j.home)),
                "version": jdk.map(|j| j.version),
            },
            "kotlin-native": native.map(|n| json!({ "version": n.version, "home": path(&n.home) })),
        },
        "packages": packages,
    }))
}

fn path(p: &Path) -> String {
    p.display().to_string()
}

fn package(ctx: &BuildContext, opts: &MetadataOptions) -> Result<Value> {
    let ws = ctx.ws;
    let m = ws.manifest();
    let project = ctx.project;
    let path_dependencies: Vec<Value> = ws
        .path_dependencies()
        .iter()
        .map(|(dep, index)| {
            json!({
                "name": dep.name,
                "package": project.package(*index).name(),
                "scope": dep.scope.as_str(),
                "dev": dep.dev,
                "target": dep.target.map(TargetKind::as_str),
                "exported": dep.exported,
                "test-fixtures": dep.test_fixtures,
            })
        })
        .collect();
    let tasks: Vec<Value> = m
        .tasks
        .values()
        .map(|t| json!({ "name": t.name, "description": t.description, "run": t.platform_run(), "depends-on": t.depends_on }))
        .collect();
    let examples: Vec<Value> = ws
        .examples()
        .into_iter()
        .map(|name| json!({ "name": name, "path": path(&ws.examples_dir().join(format!("{name}.kt"))) }))
        .collect();
    let targets = m.targets.enabled().into_iter().map(|t| target(ctx, t, opts)).collect::<Result<Vec<_>>>()?;
    Ok(json!({
        "name": m.package.name,
        "version": m.package.version,
        "group": m.package.group,
        "description": m.package.description,
        "member": project.members().contains(&ctx.package),
        "root": path(ws.root()),
        "manifest": path(&ws.manifest_path()),
        "lockfile": path(&ws.lockfile_path()),
        "kotlin": {
            "version": m.kotlin.version,
            "language-version": m.kotlin.language_version,
            "api-version": m.kotlin.api_version,
            "opt-in": m.kotlin.opt_in,
            "warnings-as-errors": m.kotlin.warnings_as_errors,
        },
        "frameworks": builtins::enabled_frameworks(m),
        "path-dependencies": path_dependencies,
        "bins": m
            .bins
            .iter()
            .map(|b| json!({ "name": b.name, "main": b.main, "entry-point": b.entry_point, "path": b.path.as_ref().map(|p| path(&ws.root().join(p))) }))
            .collect::<Vec<_>>(),
        "examples": examples,
        "generators": m.generators.iter().map(|g| generator(ctx, g)).collect::<Vec<_>>(),
        "tasks": tasks,
        "targets": targets,
    }))
}

/// A `[[generate]]` step: what it is, what it reads and where it writes.
fn generator(ctx: &BuildContext, g: &kargo_manifest::Generator) -> Value {
    let (how, detail) = match &g.action {
        GeneratorAction::Run { run, windows_run, .. } => ("run", json!(windows_run.as_ref().filter(|_| cfg!(windows)).unwrap_or(run))),
        GeneratorAction::Tool { module, version, .. } => ("tool", json!(format!("{module}:{version}"))),
        GeneratorAction::BuildConfig(_) => ("builtin", json!("build-config")),
        GeneratorAction::Protobuf(_) => ("builtin", json!("protobuf")),
    };
    json!({
        "name": g.name,
        "kind": g.kind.as_str(),
        "targets": g.targets.iter().map(|t| t.as_str()).collect::<Vec<_>>(),
        "inputs": g.inputs,
        "after": g.after,
        "roots": crate::generate::roots(ctx.ws, &ctx.profile, g).iter().map(|d| path(d)).collect::<Vec<_>>(),
        how: detail,
        "output-dir": path(&crate::generate::output_dir(ctx.ws, &ctx.profile, &g.name)),
    })
}

fn target(ctx: &BuildContext, kind: TargetKind, opts: &MetadataOptions) -> Result<Value> {
    let ws = ctx.ws;
    let m = ws.manifest();
    let roots = |dirs: &[PathBuf]| dirs.iter().map(|d| path(d)).collect::<Vec<_>>();
    // What `[[generate]]` steps write, listed whether or not a build has run them yet.
    let with_generated = |mut dirs: Vec<PathBuf>, generated: GeneratedKind| {
        dirs.extend(crate::generate::dirs(ws, &ctx.profile, kind, generated));
        dirs
    };
    let mut out = json!({
        "kind": kind.as_str(),
        "source-roots": roots(&with_generated(vec![ws.common_sources(), ws.target_sources(kind)], GeneratedKind::Sources)),
        // Test fixtures and benchmarks compile like tests, so editors treat them as test sources too.
        "test-roots": roots(&with_generated(
            vec![
                ws.common_tests(),
                ws.target_tests(kind),
                ws.common_test_fixtures(),
                ws.target_test_fixtures(kind),
                ws.common_benches(),
                ws.target_benches(kind),
            ],
            GeneratedKind::TestSources,
        )),
        // The output directories of the `[[generate]]` steps this target uses, by what they hold.
        "generated-roots": GeneratedKind::ALL
            .iter()
            .flat_map(|&k| m.generators.iter().filter(move |g| g.kind == k && g.applies_to(kind)).map(move |g| (k, g)))
            .flat_map(|(k, g)| {
                // Whether every target of the package gets the same files, so that they are shared code.
                let common = m.targets.enabled().iter().all(|t| g.applies_to(*t));
                crate::generate::roots(ws, &ctx.profile, g)
                    .into_iter()
                    .map(move |dir| json!({ "name": g.name, "kind": k.as_str(), "dir": path(&dir), "common": common }))
            })
            .collect::<Vec<_>>(),
        "test-fixture-roots": roots(&[ws.common_test_fixtures(), ws.target_test_fixtures(kind)]),
        "bench-roots": roots(&[ws.common_benches(), ws.target_benches(kind)]),
        "output-dir": path(&ws.output_dir(&ctx.profile, kind)),
    });
    if kind == TargetKind::Android {
        // Gradle compiles Android code and resolves its dependencies.
        out["dependencies"] = Value::Null;
        return Ok(out);
    }
    let setup = ctx.setup(kind)?;
    out["compiler-args"] = json!(common_kotlin_args(m, ws.profile(&ctx.profile)?, &setup.contributions.compiler_args));
    out["compiler-plugins"] = compiler_plugins(ctx, &setup, opts)?;
    // The `Res` class of `composeResources/`, written now so that editors see it before a build.
    let dir = super::build::compose_resources_out(ws, &ctx.profile, kind);
    out["compose-resources"] = match crate::compose::resources::generate(ws.root(), m, &dir)? {
        Some(generated) => json!({
            "package": crate::compose::resources::package(m),
            "source-root": path(generated.source.parent().unwrap_or(&dir)),
            "resource-root": path(&generated.resources),
        }),
        None => Value::Null,
    };
    match kind {
        TargetKind::Jvm => {
            let jvm = m.targets.jvm.as_ref().expect("the JVM target is enabled");
            // The default `jvm-target` is lowered to an older JDK, as the build does.
            let jdk = if opts.no_deps { None } else { ctx.jdk().ok() };
            let jvm_target = jdk.and_then(|jdk| jvm.target_for(jdk.version).ok()).unwrap_or_else(|| jvm.jvm_target.clone());
            let kotlin_files = existing_sources(ctx, kind, false, &["kt", "java"])?.kotlin_files();
            let main_class = match &jvm.main_class {
                Some(main) => Some(main.clone()),
                None => find_jvm_main_class(&kotlin_files, "").ok(),
            };
            // The `@Preview` composables `kargo preview` renders.
            out["previews"] = if crate::compose::enabled(m) {
                json!(crate::compose::preview::find(&kotlin_files)
                    .iter()
                    .map(|p| json!({ "name": p.qualified_name(), "function": p.function, "class": p.class, "path": path(&p.file), "line": p.line }))
                    .collect::<Vec<_>>())
            } else {
                json!([])
            };
            let output = ws.output_dir(&ctx.profile, kind);
            out["resource-roots"] = json!(roots(&with_generated(vec![ws.resources()], GeneratedKind::Resources)));
            out["test-resource-roots"] = json!(roots(&[ws.test_resources()]));
            out["classes-dir"] = json!(path(&output.join("classes")));
            out["test-classes-dir"] = json!(path(&output.join("test-classes")));
            out["jvm-target"] = json!(jvm_target);
            out["main-class"] = json!(main_class);
            out["jvm-args"] = json!(jvm.jvm_args);
            out["javac-args"] = json!(super::build::jvm_javac_args(&setup, jvm));
        }
        TargetKind::Wasm => {
            let wasm = m.targets.wasm.as_ref().expect("the Wasm target is enabled");
            out["environment"] = json!(wasm.environment.as_str());
            out["wasm-kind"] = json!(wasm.kind.as_str());
            out["module-name"] = json!(wasm.module_name(&m.package));
            out["npm"] = if opts.no_deps { Value::Null } else { Value::Array(npm_packages(ctx, kind, &setup)?) };
            // The Kotlin declarations generated for `[target.wasm.npm-dependencies]`, once a build wrote them.
            out["npm-declarations"] = npm_declarations(ws, ctx, kind, &wasm.npm_dependencies);
            out["js-sources"] = json!(roots(&crate::js_sources::dirs(ws, kind)));
            out["js-declarations"] = js_declarations(ws, ctx, kind);
        }
        TargetKind::Js => {
            let js = m.targets.js.as_ref().expect("the JS target is enabled");
            out["environment"] = json!(js.environment.as_str());
            out["js-kind"] = json!(js.kind.as_str());
            out["module-kind"] = json!(js.module_kind.as_str());
            out["module-name"] = json!(js.module_name(&m.package));
            out["npm"] = if opts.no_deps { Value::Null } else { Value::Array(npm_packages(ctx, kind, &setup)?) };
            out["npm-declarations"] = npm_declarations(ws, ctx, kind, &js.npm_dependencies);
            out["js-sources"] = json!(roots(&crate::js_sources::dirs(ws, kind)));
            out["js-declarations"] = js_declarations(ws, ctx, kind);
        }
        TargetKind::Native => {
            let native = m.targets.native.as_ref().expect("the Native target is enabled");
            let entry_point = native_entry_point(native, &existing_sources(ctx, kind, false, &["kt"])?).ok().flatten();
            out["native-kind"] = json!(native.kind.as_str());
            out["platforms"] = json!(native_platforms(ctx)?);
            out["entry-point"] = json!(entry_point);
        }
        TargetKind::Android => unreachable!("handled above"),
    }
    out["dependencies"] = if opts.no_deps { Value::Null } else { Value::Array(dependencies(ctx, kind, &setup)?) };
    Ok(out)
}

/// The compiler plugins of a target (frameworks' and `[plugins]`), with their jars unless `--no-deps`.
fn compiler_plugins(ctx: &BuildContext, setup: &TargetSetup, opts: &MetadataOptions) -> Result<Value> {
    let plugins = &setup.contributions.plugins;
    let jars = if opts.no_deps { Vec::new() } else { ctx.plugin_jars(plugins)? };
    let kotlin = &ctx.ws.manifest().kotlin.version;
    Ok(plugins
        .iter()
        .enumerate()
        .map(|(i, p)| {
            json!({
                "module": format!("{}:{}", p.group, p.artifact),
                "version": p.version.as_deref().unwrap_or(kotlin),
                "id": p.id,
                "options": builtins::plugin_option_args(p),
                "jar": jars.get(i).map(|j| path(&j.jar)),
            })
        })
        .collect())
}

/// The classpaths (JVM) or klib paths (Wasm, JS, Native) of a target, one entry per platform.
/// Path dependencies are listed by package name: editors map them to that package's roots.
fn dependencies(ctx: &BuildContext, kind: TargetKind, setup: &TargetSetup) -> Result<Vec<Value>> {
    let locals = |scopes: &[Scope], dev: bool| -> Result<Vec<Value>> {
        let mut deps = ctx.local_dependencies(kind, false)?;
        if dev {
            deps.extend(ctx.local_dependencies(kind, true)?);
        }
        Ok(deps
            .iter()
            .filter(|d| scopes.contains(&d.scope))
            .map(|d| json!({ "package": ctx.project.package(d.package).name(), "scope": d.scope.as_str() }))
            .collect())
    };
    let entries = |locals: Vec<Value>, res: &Resolution| -> Vec<Value> {
        let mut out = locals;
        out.extend(maven_entries(ctx, res));
        out
    };
    let compile_scopes = [Scope::Compile, Scope::Provided];
    let runtime_scopes = [Scope::Compile, Scope::Runtime];
    Ok(match kind {
        TargetKind::Jvm => {
            let classpaths = super::build::jvm_classpaths(ctx, setup)?;
            let test = ctx.resolve("jvm/test", Platform::Jvm, &setup.requests(&ALL_SCOPES, true), &setup.boms)?;
            vec![json!({
                "platform": "jvm",
                "compile": entries(locals(&compile_scopes, false)?, &classpaths.compile),
                "runtime": entries(locals(&runtime_scopes, false)?, &classpaths.runtime),
                "test": entries(locals(&ALL_SCOPES, true)?, &test),
            })]
        }
        TargetKind::Wasm => {
            let env = ctx.ws.manifest().targets.wasm.as_ref().map(|w| w.environment).unwrap_or_default();
            let main = super::build::wasm_resolution(ctx, setup, false)?;
            let test = super::build::wasm_resolution(ctx, setup, true)?;
            vec![klib_entry(
                &format!("wasm-{}", env.platform()),
                entries(locals(&ALL_SCOPES, false)?, &main),
                entries(locals(&ALL_SCOPES, true)?, &test),
            )]
        }
        TargetKind::Js => {
            let main = super::build::js_resolution(ctx, setup, false)?;
            let test = super::build::js_resolution(ctx, setup, true)?;
            vec![klib_entry("js", entries(locals(&ALL_SCOPES, false)?, &main), entries(locals(&ALL_SCOPES, true)?, &test))]
        }
        TargetKind::Native => native_platforms(ctx)?
            .iter()
            .map(|p| -> Result<Value> {
                let main = super::build::native_resolution(ctx, setup, p, false)?;
                let test = super::build::native_resolution(ctx, setup, p, true)?;
                Ok(klib_entry(
                    &format!("native-{p}"),
                    entries(locals(&ALL_SCOPES, false)?, &main),
                    entries(locals(&ALL_SCOPES, true)?, &test),
                ))
            })
            .collect::<Result<_>>()?,
        TargetKind::Android => Vec::new(),
    })
}

/// A klib platform: the same libraries compile and link the program.
fn klib_entry(platform: &str, main: Vec<Value>, test: Vec<Value>) -> Value {
    json!({ "platform": platform, "compile": main, "runtime": main, "test": test })
}

/// Where the Kotlin declarations generated for the npm packages of `target` go once a build wrote
/// them, or `null` when it has no `npm-dependencies` (or its Kotlin is too old for them on Kotlin/JS).
fn npm_declarations(ws: &Workspace, ctx: &BuildContext, target: TargetKind, deps: &BTreeMap<String, String>) -> Value {
    if deps.is_empty() || !npm::declarations_supported(ws.manifest(), target) {
        return Value::Null;
    }
    json!(path(&npm::declarations_dir(ws, &ctx.profile, target)))
}

/// Where the Kotlin declarations of the package's own TypeScript modules go, written by a build or
/// `kargo lsp`; `null` without `js-sources` directories.
fn js_declarations(ws: &Workspace, ctx: &BuildContext, target: TargetKind) -> Value {
    if crate::js_sources::dirs(ws, target).is_empty() || !npm::declarations_supported(ws.manifest(), target) {
        return Value::Null;
    }
    json!(path(&crate::js_sources::declarations_dir(ws, &ctx.profile, target)))
}

/// The npm packages of the Wasm or JS target: the ones its klibs and `[target.<target>.npm-dependencies]` need,
/// and where `kargo build` and `kargo test` install them. Path dependencies are not built to read their klibs
/// here, so their packages are missing.
fn npm_packages(ctx: &BuildContext, target: TargetKind, setup: &TargetSetup) -> Result<Vec<Value>> {
    let (main, test, main_dir) = if target == TargetKind::Js {
        let main = super::build::js_resolution(ctx, setup, false)?.files();
        let test = super::build::js_resolution(ctx, setup, true)?.files();
        (main, test, ctx.ws.output_dir(&ctx.profile, target))
    } else {
        (wasm_resolution(ctx, setup, false)?.files(), wasm_resolution(ctx, setup, true)?.files(), ctx.ws.profile_dir(&ctx.profile))
    };
    let main = npm::dependencies(ctx, target, &main)?;
    let test = npm::dependencies(ctx, target, &test)?;
    let test_dir = ctx.ws.output_dir(&ctx.profile, target).join("test").join("node_modules");
    Ok(npm_listing(&main, &test, &main_dir.join("node_modules"), &test_dir))
}

/// The packages of the program, installed in `main_dir`, and of its tests. `kargo test` installs every
/// test package into `test_dir` only when they differ from the program's, so then the ones the program
/// already has at the same version are left out: they are listed once, from `main_dir`.
fn npm_listing(main: &BTreeMap<String, String>, test: &BTreeMap<String, String>, main_dir: &Path, test_dir: &Path) -> Vec<Value> {
    let mut out = npm_entries(main.iter(), main_dir, false);
    if test != main {
        let extra = test.iter().filter(|(name, version)| main.get(*name) != Some(*version));
        out.extend(npm_entries(extra, test_dir, true));
    }
    out
}

fn npm_entries<'a>(packages: impl Iterator<Item = (&'a String, &'a String)>, dir: &Path, test: bool) -> Vec<Value> {
    packages
        .map(|(name, version)| {
            // A scoped name such as `@js-joda/core` is a directory inside `node_modules`.
            let directory = name.split('/').fold(dir.to_path_buf(), |p, part| p.join(part));
            json!({ "name": name, "version": version, "directory": path(&directory), "test": test })
        })
        .collect()
}

/// One entry per downloaded artifact, in classpath order, with its sources jar if it is cached.
fn maven_entries(ctx: &BuildContext, res: &Resolution) -> Vec<Value> {
    let mut out = Vec::new();
    for node in &res.nodes {
        let sources = ctx.client.cached_sources(&node.module, &node.version, &node.files);
        for file in &node.files {
            out.push(json!({
                "module": node.module.to_string(),
                "version": node.version,
                "path": path(file),
                "sources": sources.as_deref().map(path),
            }));
        }
    }
    out
}

/// `kargo fetch --sources`: downloads the sources jar of every resolved module of `targets`,
/// for editors to navigate into libraries. Modules without one are skipped. Returns how many
/// sources jars are now in the cache.
pub fn fetch_sources(ctx: &BuildContext, targets: &[TargetKind]) -> Result<usize> {
    let mut modules: Vec<(kargo_maven::ModuleId, String)> = Vec::new();
    for &target in targets.iter().filter(|t| **t != TargetKind::Android) {
        for configuration in [super::Configuration::Compile, super::Configuration::Runtime, super::Configuration::Test] {
            for (_, res) in super::resolution_for(ctx, target, configuration)? {
                for node in res.nodes.iter().filter(|n| !n.files.is_empty()) {
                    if !modules.iter().any(|(m, v)| *m == node.module && *v == node.version) {
                        modules.push((node.module.clone(), node.version.clone()));
                    }
                }
            }
        }
    }
    let fetched = kargo_maven::par_map_io(&modules, |(module, version)| ctx.client.fetch_sources(module, version));
    let mut found = 0;
    for result in fetched {
        if result?.is_some() {
            found += 1;
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::ProfileKind;
    use kargo_util::config::Config;
    use kargo_util::shell::{Shell, Verbosity};

    /// Writes a module with a POM and a jar (and a sources jar with `sources`) to a Maven repository.
    fn publish(repo: &Path, group: &str, artifact: &str, version: &str, sources: bool) {
        let dir = repo.join(group.replace('.', "/")).join(artifact).join(version);
        std::fs::create_dir_all(&dir).unwrap();
        let pom = format!("<project><groupId>{group}</groupId><artifactId>{artifact}</artifactId><version>{version}</version></project>");
        std::fs::write(dir.join(format!("{artifact}-{version}.pom")), pom).unwrap();
        std::fs::write(dir.join(format!("{artifact}-{version}.jar")), "jar").unwrap();
        if sources {
            std::fs::write(dir.join(format!("{artifact}-{version}-sources.jar")), "sources").unwrap();
        }
    }

    #[test]
    fn describes_classpaths_and_their_sources() {
        let repo = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        publish(repo.path(), "g", "lib", "1.0", true);
        publish(repo.path(), "g", "check", "1.0", false);
        for artifact in ["kotlin-stdlib", "kotlin-test-junit5"] {
            publish(repo.path(), "org.jetbrains.kotlin", artifact, kargo_manifest::DEFAULT_KOTLIN_VERSION, false);
        }
        publish(repo.path(), "org.junit.jupiter", "junit-jupiter", crate::builtins::JUNIT_VERSION, false);
        let url = format!("file:///{}", repo.path().display().to_string().replace('\\', "/").trim_start_matches('/'));
        let manifest = format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\n\"g:lib\" = \"1.0\"\n\
             [dev-dependencies]\n\"g:check\" = \"1.0\"\n[repositories]\nlocal = \"{url}\"\n"
        );
        std::fs::write(dir.path().join("kargo.toml"), manifest).unwrap();
        let project = Project::load(&dir.path().join("kargo.toml")).unwrap();
        let config = Config::new(home.path().to_path_buf(), Shell::new(Verbosity::Quiet)).with_offline(true);
        let ctx = BuildContext::new(&config, &project, 0, ProfileKind::Dev).unwrap();

        let doc = metadata(&ctx, &MetadataOptions { no_deps: true }).unwrap();
        assert_eq!(doc["version"], FORMAT_VERSION);
        assert_eq!(doc["packages"][0]["targets"][0]["dependencies"], Value::Null);

        let doc = metadata(&ctx, &MetadataOptions::default()).unwrap();
        let jvm = &doc["packages"][0]["targets"][0]["dependencies"][0];
        let modules = |scope: &str| -> Vec<String> {
            jvm[scope].as_array().unwrap().iter().map(|e| e["module"].as_str().unwrap().to_owned()).collect()
        };
        assert!(modules("compile").contains(&"g:lib".to_owned()), "{jvm}");
        assert!(!modules("compile").contains(&"g:check".to_owned()), "dev-dependencies are only on the test classpath");
        assert!(modules("test").contains(&"g:check".to_owned()), "{jvm}");
        let lib = jvm["compile"].as_array().unwrap().iter().find(|e| e["module"] == "g:lib").unwrap();
        assert!(lib["path"].as_str().unwrap().ends_with("lib-1.0.jar"), "{lib}");
        assert_eq!(lib["sources"], Value::Null, "sources are only listed once they are downloaded");

        // Only g:lib publishes a sources jar.
        assert_eq!(fetch_sources(&ctx, &[TargetKind::Jvm]).unwrap(), 1);
        let doc = metadata(&ctx, &MetadataOptions::default()).unwrap();
        let jvm = &doc["packages"][0]["targets"][0]["dependencies"][0];
        let lib = jvm["runtime"].as_array().unwrap().iter().find(|e| e["module"] == "g:lib").unwrap();
        assert!(lib["sources"].as_str().unwrap().ends_with("lib-1.0-sources.jar"), "{lib}");
    }

    fn packages(list: &[(&str, &str)]) -> BTreeMap<String, String> {
        list.iter().map(|(name, version)| (name.to_string(), version.to_string())).collect()
    }

    #[test]
    fn npm_packages_are_listed_in_the_directory_they_install_to() {
        let main = packages(&[("date-fns", "4.1.0"), ("@js-joda/core", "3.2.0")]);
        let out = npm_listing(&main, &main, Path::new("build/node_modules"), Path::new("build/test/node_modules"));
        assert_eq!(out.len(), 2, "{out:?}");
        let joda = out.iter().find(|e| e["name"] == "@js-joda/core").unwrap();
        assert_eq!(joda["version"], "3.2.0");
        assert_eq!(joda["test"], false);
        let dir = joda["directory"].as_str().unwrap().replace('\\', "/");
        assert!(dir.ends_with("build/node_modules/@js-joda/core"), "{dir}");
    }

    #[test]
    fn test_packages_are_listed_only_when_they_are_installed_apart() {
        let main = packages(&[("@js-joda/core", "3.2.0")]);
        let same = npm_listing(&main, &main, Path::new("out/node_modules"), Path::new("out/test/node_modules"));
        assert!(same.iter().all(|e| e["test"] == false), "{same:?}");

        let test = packages(&[("@js-joda/core", "3.2.0"), ("mocha", "10.0.0")]);
        let out = npm_listing(&main, &test, Path::new("out/node_modules"), Path::new("out/test/node_modules"));
        let mocha = out.iter().find(|e| e["name"] == "mocha").unwrap();
        assert_eq!(mocha["test"], true);
        assert!(mocha["directory"].as_str().unwrap().replace('\\', "/").ends_with("out/test/node_modules/mocha"));
        // The program's own package stays listed once, in the program's directory.
        assert_eq!(out.iter().filter(|e| e["name"] == "@js-joda/core").count(), 1, "{out:?}");
    }

    #[test]
    fn fingerprint_follows_the_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("kargo.toml");
        std::fs::write(&manifest, "[package]\nname = \"app\"\nversion = \"0.1.0\"\n").unwrap();
        let first = fingerprint(&Project::load(&manifest).unwrap());
        assert_eq!(first, fingerprint(&Project::load(&manifest).unwrap()));
        std::fs::write(&manifest, "[package]\nname = \"app\"\nversion = \"0.2.0\"\n").unwrap();
        assert_ne!(first, fingerprint(&Project::load(&manifest).unwrap()));
    }
}
