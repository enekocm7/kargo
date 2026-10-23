//! `kargo preview`: renders the `@Preview` composables of a Compose Desktop app to PNG files, for docs,
//! code review and CI. The previews are found in the JVM sources (see [`crate::compose::preview`]),
//! the app is built, and a small renderer compiled against it draws each one off screen.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::build::{BuildOptions, build, existing_sources, jvm_classpaths, jvm_compile_classpath};
use super::context::BuildContext;
use crate::compose::preview::{self, Preview};
use crate::workspace::ProfileKind;
use kargo_compiler::SourceSet;
use kargo_compiler::jvm::JvmCompilation;
use kargo_manifest::TargetKind;
use kargo_util::fs::join_paths;
use kargo_util::process;

#[derive(Debug, Clone)]
pub struct PreviewOptions {
    pub profile: ProfileKind,
    /// The previews to render, by function or qualified name; empty renders all of them.
    pub names: Vec<String>,
    /// Where the PNG files go; `None` is `target/preview`.
    pub out_dir: Option<PathBuf>,
    pub width: u32,
    pub height: u32,
    /// Pixels per density-independent pixel.
    pub density: f32,
}

/// The previews of the project's JVM sources, without building anything.
pub fn list(ctx: &BuildContext) -> Result<Vec<Preview>> {
    if !crate::compose::enabled(ctx.ws.manifest()) {
        bail!("`kargo preview` renders Compose previews; enable Compose with `[frameworks] compose = true`");
    }
    if !ctx.ws.manifest().targets.enabled().contains(&TargetKind::Jvm) {
        bail!("`kargo preview` renders on the desktop and needs the jvm target; add it with `kargo target add jvm`");
    }
    Ok(preview::find(&existing_sources(ctx, TargetKind::Jvm, false, &["kt"])?.kotlin_files()))
}

/// Renders the chosen previews and returns the PNG files.
pub fn render(ctx: &BuildContext, opts: &PreviewOptions) -> Result<Vec<PathBuf>> {
    let all = list(ctx)?;
    let chosen: Vec<&Preview> = if opts.names.is_empty() {
        all.iter().collect()
    } else {
        let mut chosen = Vec::new();
        for name in &opts.names {
            let found: Vec<&Preview> = all.iter().filter(|p| p.matches(name)).collect();
            match found.as_slice() {
                [] => bail!("no preview is named `{name}`; `kargo preview --list` shows them"),
                [one] => chosen.push(*one),
                many => {
                    let names: Vec<String> = many.iter().map(|p| p.qualified_name()).collect();
                    bail!("`{name}` names several previews ({}); give the qualified name", names.join(", "))
                }
            }
        }
        chosen
    };
    if chosen.is_empty() {
        bail!(
            "the JVM sources have no previews: annotate a top-level @Composable function without parameters with \
             @Preview (androidx.compose.ui.tooling.preview.Preview)"
        );
    }

    let results = build(ctx, &BuildOptions { targets: vec![TargetKind::Jvm], profile: opts.profile.clone(), ..Default::default() })?;
    let jvm = results.jvm.context("the JVM build produced no classes to preview")?;
    let renderer = compile_renderer(ctx, &jvm.classes)?;

    let out_dir = opts.out_dir.clone().unwrap_or_else(|| ctx.ws.work_dir("preview"));
    let files: Vec<PathBuf> = chosen.iter().map(|p| out_dir.join(format!("{}.png", p.qualified_name()))).collect();
    let mut classpath = vec![renderer, jvm.classes.clone()];
    classpath.extend(jvm.runtime_classpath.iter().cloned());
    let jdk = ctx.jdk()?;
    let mut cmd = Command::new(jdk.java());
    cmd.arg("-Djava.awt.headless=true").args(jdk.default_run_args());
    let joined = join_paths(&classpath);
    if joined.len() >= process::LONG_CLASSPATH {
        let argfile = ctx.ws.output_dir(&ctx.profile, TargetKind::Jvm).join("preview.args");
        process::write_argfile(&argfile, &["-cp".to_owned(), process::classpath_for_argfile(&classpath)])?;
        cmd.arg(format!("@{}", process::slash(&argfile)));
    } else {
        cmd.arg("-cp").arg(joined);
    }
    // The scene's size is in pixels; the options give it in density-independent pixels.
    let pixels = |dp: u32| ((dp as f32 * opts.density).round() as u32).max(1).to_string();
    cmd.arg(preview::RENDERER_MAIN).arg(pixels(opts.width)).arg(pixels(opts.height)).arg(opts.density.to_string());
    for (p, file) in chosen.iter().zip(&files) {
        cmd.arg(format!("{}|{}|{}", p.class, p.function, file.display()));
    }
    cmd.current_dir(ctx.ws.root());
    ctx.shell().status("Rendering", format!("{} preview{}", chosen.len(), if chosen.len() == 1 { "" } else { "s" }));
    ctx.shell().verbose("Command", process::describe(&cmd));
    let output = cmd.output().context("failed to start the preview renderer")?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    for line in stderr.lines().filter(|l| l.starts_with("failed ")) {
        ctx.shell().error(line.trim_start_matches("failed ").to_owned());
    }
    if !output.status.success() {
        let detail = if stderr.lines().any(|l| l.starts_with("failed ")) { String::new() } else { format!(": {}", stderr.trim()) };
        bail!("some previews could not be rendered{detail}");
    }
    Ok(files)
}

/// Compiles the renderer against the app's classes and its compile classpath, with the Compose
/// compiler, into `target/<profile>/jvm/preview`.
fn compile_renderer(ctx: &BuildContext, classes: &std::path::Path) -> Result<PathBuf> {
    let manifest = ctx.ws.manifest();
    let jvm = manifest.targets.jvm.as_ref().context("the JVM target is not enabled")?;
    let setup = ctx.setup(TargetKind::Jvm)?;
    let classpaths = jvm_classpaths(ctx, &setup)?;
    let mut classpath = vec![classes.to_path_buf()];
    classpath.extend(jvm_compile_classpath(ctx, &classpaths)?);
    let jdk = ctx.jvm_jdk()?;
    let compiler = ctx.kotlin_compiler()?;
    let plugins = ctx.plugin_jars(&setup.contributions.plugins)?;
    let jvm_target = jvm.target_for(jdk.version)?;

    let dir = ctx.ws.output_dir(&ctx.profile, TargetKind::Jvm).join("preview");
    let source = dir.join("src").join("KargoPreview.kt");
    if std::fs::read_to_string(&source).ok().as_deref() != Some(preview::RENDERER) {
        std::fs::create_dir_all(source.parent().expect("a parent"))?;
        std::fs::write(&source, preview::RENDERER)?;
    }
    let output = dir.join("classes");
    let module = format!("{}-preview", manifest.package.name);
    let unit = JvmCompilation {
        name: &module,
        sources: SourceSet::new(Vec::new(), vec![source], TargetKind::Jvm.as_str()),
        classpath,
        friend_paths: Vec::new(),
        output: output.clone(),
        jvm_target: &jvm_target,
        plugins: &plugins,
        kotlin_args: &[],
        javac_args: &[],
        processor_path: &[],
        resources: Vec::new(),
    };
    let fp_dir = ctx.fingerprint_dir(TargetKind::Jvm).join("preview");
    std::fs::create_dir_all(&fp_dir)?;
    unit.compile(ctx.shell(), jdk, compiler, &fp_dir, &|| ctx.shell().status("Compiling", "the preview renderer"))?;
    Ok(output)
}
