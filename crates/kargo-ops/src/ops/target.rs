//! `kargo target add`, `remove` and `list`: enable and disable targets in an existing project.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::new::{NewOptions, android_files, target_section};
use crate::workspace::Workspace;
use kargo_manifest::edit::ManifestEditor;
use kargo_manifest::{Manifest, TargetKind};
use kargo_util::shell::Shell;

/// What `kargo target add` changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AddedTargets {
    /// The targets written to `kargo.toml`, in the order given.
    pub targets: Vec<TargetKind>,
    /// Whether `[target.jvm]` was written to keep an implicit JVM project building for the JVM.
    pub materialized_jvm: bool,
    /// The files scaffolded for the new targets, relative to the package root.
    pub created: Vec<PathBuf>,
}

/// `kargo target add`: appends `[target.<name>]` with the defaults `kargo new` writes and scaffolds
/// the files a target needs, never overwriting one. `lib` picks a library over an application for
/// Native and Android; `None` infers it from the sources (a `main` function makes an application).
pub fn add(shell: &Shell, ws: &Workspace, targets: &[TargetKind], lib: Option<bool>) -> Result<AddedTargets> {
    let mut editor = ManifestEditor::load(&ws.manifest_path())?;
    for &target in targets {
        if ws.manifest().targets.is_enabled(target) {
            shell.note(format!("the {target} target is already enabled"));
        }
    }
    let Some(edit) = edit(ws, ws.manifest(), &mut editor, targets, lib)? else {
        return Ok(AddedTargets::default());
    };
    let TargetEdit { targets: new_targets, lib, materialized_jvm, manifest } = edit;
    editor.save()?;
    let mut added = AddedTargets { materialized_jvm, ..AddedTargets::default() };
    if added.materialized_jvm {
        shell.status("Adding", "jvm target");
        shell.note("kargo.toml had no [target] section, which means jvm only; [target.jvm] keeps the jvm target enabled");
    }
    for &target in &new_targets {
        let kind = match target {
            TargetKind::Native if lib => " (library)",
            TargetKind::Native => " (executable)",
            TargetKind::Android if lib => " (library)",
            TargetKind::Android => " (application)",
            _ => "",
        };
        shell.status("Adding", format!("{target} target{kind}"));
    }

    added.created = scaffold_targets(ws, &manifest, &new_targets, lib)?;
    for path in &added.created {
        shell.note(format!("created {}", path.to_string_lossy().replace('\\', "/")));
    }
    for &target in &new_targets {
        if let Err(e) = crate::builtins::contributions(&manifest, target, ws.root()) {
            shell.warn(format!("the {target} target will not build with the enabled frameworks: {e}"));
        }
    }
    if !manifest.dependencies.is_empty() && new_targets.iter().any(|t| *t != TargetKind::Android) {
        let names: Vec<&str> = new_targets.iter().map(|t| t.as_str()).collect();
        shell.note(format!(
            "every dependency in [dependencies] must publish a variant for {}; move JVM-only ones to [target.jvm.dependencies]",
            names.join(" and ")
        ));
    }
    match new_targets.as_slice() {
        [one] => shell.note(format!("run `kargo build --target {one}` to build it")),
        _ => shell.note("run `kargo build` to build every target"),
    }
    added.targets = new_targets;
    Ok(added)
}

/// The `kargo.toml` edit of `kargo target add`.
#[derive(Debug, Clone)]
pub struct TargetEdit {
    /// The targets added, without the ones already enabled.
    pub targets: Vec<TargetKind>,
    /// Whether they were added as libraries.
    pub lib: bool,
    /// Whether `[target.jvm]` was written to keep an implicit JVM project building for the JVM.
    pub materialized_jvm: bool,
    /// The edited manifest.
    pub manifest: Manifest,
}

/// Adds the `[target.<name>]` tables of `kargo target add` to `editor`, which holds `manifest`
/// (the package's `kargo.toml`, possibly with unsaved changes). Writes nothing. Returns `None`
/// when every target is already enabled, and fails when the result would not be valid.
pub fn edit(
    ws: &Workspace,
    manifest: &Manifest,
    editor: &mut ManifestEditor,
    targets: &[TargetKind],
    lib: Option<bool>,
) -> Result<Option<TargetEdit>> {
    let mut new_targets: Vec<TargetKind> = Vec::new();
    for &target in targets {
        if !manifest.targets.is_enabled(target) && !new_targets.contains(&target) {
            new_targets.push(target);
        }
    }
    if new_targets.is_empty() {
        return Ok(None);
    }
    let lib = lib.unwrap_or_else(|| !has_main(ws, manifest));
    let opts = new_options(manifest, lib);
    let mut materialized_jvm = false;
    if editor.targets().is_empty() {
        // Without a [target] section the project is implicitly JVM; a [target.<other>] table alone
        // would turn the JVM target off.
        editor.add_target(TargetKind::Jvm, &target_section(TargetKind::Jvm, lib, &opts.package()))?;
        materialized_jvm = true;
    }
    for &target in &new_targets {
        editor.add_target(target, &target_section(target, lib, &opts.package()))?;
    }
    let manifest = ws.check_manifest(&editor.contents()).context("the edited manifest is invalid; nothing was written")?;
    Ok(Some(TargetEdit { targets: new_targets, lib, materialized_jvm, manifest }))
}

/// Writes the files `kargo target add` scaffolds for `targets` in the package of `manifest`,
/// never overwriting one. Returns them relative to the package root.
pub fn scaffold_targets(ws: &Workspace, manifest: &Manifest, targets: &[TargetKind], lib: bool) -> Result<Vec<PathBuf>> {
    let opts = new_options(manifest, lib);
    let mut created = Vec::new();
    for &target in targets {
        created.extend(scaffold(ws, &opts, target)?);
    }
    Ok(created)
}

/// The options `kargo new` would use for this package, for the target defaults and templates.
fn new_options(manifest: &Manifest, lib: bool) -> NewOptions {
    let mut opts = NewOptions::new(manifest.package.name.clone());
    if let Some(group) = &manifest.package.group {
        opts.group = group.clone();
    }
    opts.lib = lib;
    if manifest.frameworks.compose.is_some() {
        opts.frameworks.push("compose".to_owned());
    }
    opts
}

/// Whether the sources of the enabled targets declare a `main` function (or the manifest names one).
fn has_main(ws: &Workspace, m: &Manifest) -> bool {
    if m.targets.jvm.as_ref().is_some_and(|j| j.main_class.is_some()) || !m.bins.is_empty() {
        return true;
    }
    let mut files = kargo_util::fs::collect_files(&ws.common_sources(), &["kt"]);
    for target in m.targets.enabled() {
        files.extend(kargo_util::fs::collect_files(&ws.target_sources(target), &["kt"]));
    }
    !kargo_compiler::mainclass::find_entry_points(&files).is_empty()
}

/// Writes the files `target` needs that do not exist yet: the Android manifest and launcher
/// activity, or an empty `src-<target>/` for target-specific sources. Returns them relative to the root.
fn scaffold(ws: &Workspace, opts: &NewOptions, target: TargetKind) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if target == TargetKind::Android {
        let greeting = declares_greeting(&ws.common_sources(), &opts.package());
        files.extend(android_files(opts, greeting).into_iter().map(|f| (f.path, f.contents)));
    }
    let dir = PathBuf::from(target.source_dir_name());
    if !files.iter().any(|(path, _)| path.starts_with(&dir)) {
        files.push((dir.join(".gitkeep"), String::new()));
    }
    let mut created = Vec::new();
    for (relative, contents) in files {
        let path = ws.root().join(&relative);
        let parent = path.parent().expect("scaffolded files are in a directory");
        // An existing directory of target sources needs no placeholder.
        if path.exists() || (relative.ends_with(".gitkeep") && parent.exists()) {
            continue;
        }
        std::fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
        std::fs::write(&path, contents).with_context(|| format!("failed to write {}", path.display()))?;
        created.push(relative);
    }
    Ok(created)
}

/// Whether a source file in `dir` declares `fun greeting` in `package`, which the template activity calls.
fn declares_greeting(dir: &Path, package: &str) -> bool {
    kargo_util::fs::collect_files(dir, &["kt"]).iter().any(|file| {
        std::fs::read_to_string(file)
            .is_ok_and(|src| src.contains("fun greeting(") && src.lines().any(|l| l.trim() == format!("package {package}")))
    })
}

/// `kargo target remove`: removes `[target.<name>]` with its sub-tables. Target sources stay on disk.
/// Returns the targets removed.
pub fn remove(shell: &Shell, ws: &Workspace, targets: &[TargetKind]) -> Result<Vec<TargetKind>> {
    let mut editor = ManifestEditor::load(&ws.manifest_path())?;
    let enabled = ws.manifest().targets.enabled();
    let mut removed: Vec<TargetKind> = Vec::new();
    for &target in targets {
        if !enabled.contains(&target) {
            shell.note(format!("the {target} target is not enabled"));
        } else if !removed.contains(&target) {
            removed.push(target);
        }
    }
    if removed.is_empty() {
        return Ok(removed);
    }
    if enabled.iter().all(|t| removed.contains(t)) {
        let names: Vec<&str> = removed.iter().map(|t| t.as_str()).collect();
        bail!(
            "cannot remove {}: a project needs at least one target; add another first with `kargo target add <target>`",
            if removed.len() == 1 { format!("the last target ({})", names[0]) } else { format!("every target ({})", names.join(", ")) }
        );
    }
    for &target in &removed {
        editor.remove_target(target)?;
    }
    ws.check_manifest(&editor.contents()).context("the edited manifest is invalid; nothing was written")?;
    editor.save()?;
    for &target in &removed {
        shell.status("Removing", format!("{target} target"));
        let mut kept: Vec<String> = Vec::new();
        let mut dirs = vec![ws.target_sources(target), ws.target_tests(target)];
        if target == TargetKind::Android {
            dirs.push(ws.root().join("android"));
        }
        for dir in dirs.iter().filter(|d| d.exists()) {
            kept.push(format!("{}/", dir.file_name().unwrap_or_default().to_string_lossy()));
        }
        if !kept.is_empty() {
            let them = if kept.len() == 1 { "it" } else { "them" };
            shell.note(format!("kept {} on disk; delete {them} if you no longer need {them}", kept.join(", ")));
        }
    }
    Ok(removed)
}

/// One enabled target and its main settings, as `kargo target list` shows them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetInfo {
    pub target: TargetKind,
    /// The JVM target of a manifest without a [target] section.
    pub implicit: bool,
    pub settings: Vec<(&'static str, String)>,
}

/// `kargo target list`: the enabled targets, then the ones that can still be added.
pub fn list(ws: &Workspace) -> Result<(Vec<TargetInfo>, Vec<TargetKind>)> {
    let implicit = ManifestEditor::load(&ws.manifest_path())?.targets().is_empty();
    let targets = &ws.manifest().targets;
    let mut enabled = Vec::new();
    if let Some(jvm) = &targets.jvm {
        let mut settings = vec![("jvm-target", jvm.jvm_target.clone())];
        if let Some(main) = &jvm.main_class {
            settings.push(("main-class", main.clone()));
        }
        enabled.push(TargetInfo { target: TargetKind::Jvm, implicit, settings });
    }
    if let Some(wasm) = &targets.wasm {
        let settings = vec![("environment", wasm.environment.as_str().to_owned()), ("kind", wasm.kind.as_str().to_owned())];
        enabled.push(TargetInfo { target: TargetKind::Wasm, implicit: false, settings });
    }
    if let Some(native) = &targets.native {
        let settings = vec![("platforms", native.platforms.join(", ")), ("kind", native.kind.as_str().to_owned())];
        enabled.push(TargetInfo { target: TargetKind::Native, implicit: false, settings });
    }
    if let Some(android) = &targets.android {
        let c = &android.config;
        let settings = vec![("namespace", c.namespace.clone()), ("kind", c.kind.clone()), ("min-sdk", c.min_sdk.to_string())];
        enabled.push(TargetInfo { target: TargetKind::Android, implicit: false, settings });
    }
    let available = TargetKind::ALL.into_iter().filter(|t| !targets.is_enabled(*t)).collect();
    Ok((enabled, available))
}

/// The text of `kargo target list`.
pub fn render(enabled: &[TargetInfo], available: &[TargetKind]) -> String {
    let mut out = String::from("Enabled targets:\n");
    for info in enabled {
        let settings: Vec<String> = info.settings.iter().map(|(k, v)| format!("{k} {v}")).collect();
        let implicit = if info.implicit { " (implicit: kargo.toml has no [target] section)" } else { "" };
        out.push_str(&format!("    {:8} {}{implicit}\n", info.target.as_str(), settings.join(", ")));
    }
    if !available.is_empty() {
        out.push_str("Available targets (add with `kargo target add <target>`):\n");
        for target in available {
            out.push_str(&format!("    {}\n", target.as_str()));
        }
    }
    out
}

/// `kargo target list --format json`.
pub fn json(enabled: &[TargetInfo], available: &[TargetKind]) -> serde_json::Value {
    let enabled: Vec<serde_json::Value> = enabled
        .iter()
        .map(|info| {
            let settings: serde_json::Map<String, serde_json::Value> =
                info.settings.iter().map(|(k, v)| ((*k).to_owned(), serde_json::Value::from(v.as_str()))).collect();
            serde_json::json!({ "name": info.target.as_str(), "implicit": info.implicit, "settings": settings })
        })
        .collect();
    serde_json::json!({
        "enabled": enabled,
        "available": available.iter().map(|t| t.as_str()).collect::<Vec<_>>(),
    })
}
