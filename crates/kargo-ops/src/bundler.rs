//! Bundling browser apps and npm libraries (`bundler = "bun" | "rollup"` in `[target.wasm]` and `[target.js]`): the
//! module the Kotlin compiler wrote, the npm packages it imports and its other JavaScript become one tree-shaken,
//! minified bundle with code splitting and content-hashed names, next to the `.wasm` file (hashed too) that its glue
//! loads. A library keeps its names: it is written in each of its `formats` (`<name>.mjs`, `<name>.cjs`,
//! `<name>.umd.js`) with the npm packages it depends on left as imports, for the apps that install it to bundle.
//!
//! Both bundlers run a script Kargo writes (`bundle_bun.mjs`, `bundle_rollup.mjs`) with a JSON file of
//! options, so the project needs no `package.json` and no configuration of its own. Bun is the one on
//! `PATH` or a pinned download; Rollup and its plugins are pinned npm packages installed like the TypeScript
//! compiler. The bundle goes to `target/<profile>/<target>-bundle`, the directory `kargo run` serves and the
//! size report counts; the compiler's output stays where it was.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::ops::context::BuildContext;
use kargo_manifest::{Bundler, Bundling, LibraryFormat, Manifest, TargetKind};
use kargo_util::fs::remove_dir_all_if_exists;
use kargo_util::hash::{Fingerprint, sha256_file};
use kargo_util::process::{self, find_on_path};

const BUN_SCRIPT: &str = include_str!("bundle_bun.mjs");
const ROLLUP_SCRIPT: &str = include_str!("bundle_rollup.mjs");

/// The Rollup release and plugins Kargo installs for `bundler = "rollup"`.
const ROLLUP_PACKAGES: [(&str, &str); 4] = [
    ("rollup", "4.67.2"),
    ("@rollup/plugin-commonjs", "29.0.3"),
    ("@rollup/plugin-node-resolve", "16.0.3"),
    ("@rollup/plugin-terser", "1.0.2"),
];

/// Records what the last bundler run was made from, so an unchanged build is not bundled again.
const STAMP: &str = "stamp";

/// What a bundler made of a browser app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    /// The site: the bundle, the `.wasm` file and (once the page is assembled) the page and the resources.
    pub dir: PathBuf,
    /// The file name of the entry module, which the page loads.
    pub entry: String,
    /// The `.wasm` file in `dir`, for a Wasm app.
    pub wasm: Option<PathBuf>,
    /// For a library: the file of each format, in `dir`.
    pub formats: Vec<(LibraryFormat, String)>,
}

/// The module to bundle.
pub struct Input<'a> {
    pub target: TargetKind,
    /// The module's name: the compiler wrote `<name>.mjs`.
    pub name: &'a str,
    pub module: &'a Path,
    /// The directory of the compiler's output, which the module and its other modules are in.
    pub dir: &'a Path,
    /// The `.wasm` file the module loads.
    pub wasm: Option<&'a Path>,
    /// Whether the module runs in a browser, as opposed to Node.js.
    pub browser: bool,
    /// For a library: its npm package name and the npm packages it depends on.
    pub library: Option<LibraryInput<'a>>,
}

/// What a library bundle needs to know besides the module.
pub struct LibraryInput<'a> {
    /// The name the package is published under, which the UMD global is derived from.
    pub package: &'a str,
    /// The npm packages the library depends on: they stay imports, for the apps that install it to bundle.
    pub dependencies: Vec<String>,
}

/// The bundler settings of `target` when this build bundles: a bundler is chosen, and the profile is one that bundles
/// (`optimize`, unless `bundle` says otherwise).
pub fn active(manifest: &Manifest, target: TargetKind, optimize: bool) -> Option<&Bundling> {
    configured(manifest, target).filter(|b| b.applies(optimize))
}

/// The bundler settings of `target`, whatever the profile.
pub fn configured(manifest: &Manifest, target: TargetKind) -> Option<&Bundling> {
    match target {
        TargetKind::Wasm => manifest.targets.wasm.as_ref()?.bundling.as_ref(),
        TargetKind::Js => manifest.targets.js.as_ref()?.bundling.as_ref(),
        _ => None,
    }
}

/// Bundles `input` and returns the directory with the result.
pub fn bundle(ctx: &BuildContext, input: &Input) -> Result<Bundle> {
    let bundling = configured(ctx.ws.manifest(), input.target).context("no bundler is configured")?;
    let optimize = ctx.ws.profile(&ctx.profile)?.optimize;
    let source_maps = bundling.wants_source_maps(optimize);
    let profile_dir = ctx.ws.profile_dir(&ctx.profile);
    let work = profile_dir.join("bundle-work").join(input.target.as_str());
    let assets = work.join("assets");
    let out = profile_dir.join(format!("{}-bundle", input.target));

    // The `.wasm` file of an app gets the hash of its contents in its name, which the glue is told about before it is
    // bundled. A library keeps the names it has, which its consumers' bundlers hash.
    let hashed = input.library.is_none();
    let wasm = input.wasm.map(|file| wasm_name(input.name, file, hashed)).transpose()?;
    let replace = match &wasm {
        Some((from, to, _)) if from != to => vec![serde_json::json!({ "dir": input.dir, "from": from, "to": to })],
        _ => Vec::new(),
    };
    let mut external = bundling.external.clone();
    let library = match &input.library {
        Some(library) => {
            external.extend(library.dependencies.iter().cloned());
            external.sort();
            external.dedup();
            let name = global_name(bundling, library.package);
            if bundling.formats.contains(&LibraryFormat::Umd) {
                let missing: Vec<&str> =
                    external.iter().map(String::as_str).filter(|package| !bundling.globals.contains_key(*package)).collect();
                if !missing.is_empty() {
                    bail!(
                        "the `umd` format needs the global variable of each npm package it leaves out, but `globals` has none for {}; \
                         add them under [target.{}.globals] (`{} = \"{}\"`)",
                        missing.join(", "),
                        input.target,
                        missing[0],
                        global_name_for(missing[0])
                    );
                }
            }
            let globals: BTreeMap<&String, &String> = bundling.globals.iter().collect();
            let formats: Vec<&str> = bundling.formats.iter().map(|f| f.as_str()).collect();
            Some(serde_json::json!({ "formats": formats, "name": name, "globals": globals }))
        }
        None => None,
    };
    let options = serde_json::json!({
        "input": input.module,
        "outDir": assets,
        "manifest": work.join("manifest.json"),
        "minify": optimize,
        "sourceMaps": source_maps,
        "platform": if input.browser { "browser" } else { "node" },
        "external": external,
        "replace": replace,
        "library": library,
    });
    let stamp = stamp(input, &options, bundling.bundler);
    let stamp_path = work.join(STAMP);
    let manifest_path = work.join("manifest.json");
    let cached = std::fs::read_to_string(&stamp_path).is_ok_and(|s| s == stamp) && manifest_path.is_file() && assets.is_dir();
    if !cached {
        let _ = std::fs::remove_file(&stamp_path);
        remove_dir_all_if_exists(&assets)?;
        std::fs::create_dir_all(&work)?;
        std::fs::write(work.join("options.json"), serde_json::to_string_pretty(&options).expect("a JSON value serializes"))?;
        run(ctx, input, bundling.bundler, &work)?;
        std::fs::write(&stamp_path, stamp)?;
    }
    let (entry, files) = read_entry(&manifest_path, &assets, &bundling.formats)?;

    remove_dir_all_if_exists(&out)?;
    kargo_util::fs::copy_dir(&assets, &out).context("failed to copy the bundle")?;
    let wasm = match (input.wasm, wasm) {
        (Some(file), Some((_, to, _))) => {
            let dest = out.join(to);
            std::fs::copy(file, &dest).with_context(|| format!("failed to copy {}", file.display()))?;
            Some(dest)
        }
        _ => None,
    };
    if input.library.is_some() {
        copy_declarations(input, &out, &bundling.formats)?;
    }
    let formats = if input.library.is_some() { files } else { Vec::new() };
    let bundle = Bundle { dir: out, entry, wasm, formats };
    if !cached {
        let size = std::fs::metadata(bundle.dir.join(&bundle.entry)).map(|m| m.len()).unwrap_or(0);
        let files = if bundle.formats.len() > 1 {
            bundle.formats.iter().map(|(_, file)| file.as_str()).collect::<Vec<_>>().join(", ")
        } else {
            bundle.entry.clone()
        };
        ctx.shell().status("Bundled", format!("{files} ({})", crate::wasm_output::human_size(size)));
    }
    Ok(bundle)
}

/// Copies the TypeScript declarations the compiler wrote (`<name>.d.mts`) into a library's `out`, and as `<name>.d.cts` for
/// the formats a `require` loads.
fn copy_declarations(input: &Input, out: &Path, formats: &[LibraryFormat]) -> Result<()> {
    let declarations = input.dir.join(format!("{}.d.mts", input.name));
    if !declarations.is_file() {
        return Ok(());
    }
    std::fs::copy(&declarations, out.join(format!("{}.d.mts", input.name))).context("failed to copy the TypeScript declarations")?;
    if formats.iter().any(|f| *f != LibraryFormat::Esm) {
        std::fs::copy(&declarations, out.join(format!("{}.d.cts", input.name))).context("failed to copy the TypeScript declarations")?;
    }
    Ok(())
}

/// The global variable a UMD library puts itself in: `global-name`, else the package name (without its scope) in PascalCase.
fn global_name(bundling: &Bundling, package: &str) -> String {
    bundling.global_name.clone().unwrap_or_else(|| global_name_for(package.rsplit('/').next().unwrap_or(package)))
}

/// `left-pad` as a JavaScript name: `LeftPad`.
fn global_name_for(package: &str) -> String {
    let name: String = package
        .trim_start_matches('@')
        .rsplit('/')
        .next()
        .unwrap_or(package)
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map(|first| first.to_ascii_uppercase().to_string() + chars.as_str()).unwrap_or_default()
        })
        .collect();
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) { format!("_{name}") } else { name }
}

/// `(from, to, hash)`: the name the module has for the `.wasm` file, the name it gets (hashed when `hashed`), and the hash.
fn wasm_name(name: &str, file: &Path, hashed: bool) -> Result<(String, String, String)> {
    let hash = sha256_file(file).with_context(|| format!("failed to read {}", file.display()))?[..12].to_owned();
    let from = format!("{name}.wasm");
    let to = if hashed { format!("{name}.{hash}.wasm") } else { from.clone() };
    Ok((from, to, hash))
}

/// What the bundle is made of: the modules and `.wasm` files of the compiler's output, the options and the bundler.
fn stamp(input: &Input, options: &serde_json::Value, bundler: Bundler) -> String {
    let mut fingerprint = Fingerprint::new();
    fingerprint.str(env!("CARGO_PKG_VERSION")).str(bundler.as_str()).str(&options.to_string());
    for file in crate::wasm_output::deliverables(input.dir) {
        if matches!(file.extension().and_then(|e| e.to_str()), Some("mjs" | "js" | "cjs" | "wasm" | "json")) {
            fingerprint.str(&process::slash(file.strip_prefix(input.dir).unwrap_or(&file)));
            fingerprint.str(&sha256_file(&file).unwrap_or_default());
        }
    }
    // The installed npm packages decide what is bundled too.
    if let Some(modules) = input.dir.parent().map(|p| p.join("node_modules")).filter(|m| m.is_dir()).or_else(|| {
        let own = input.dir.join("node_modules");
        own.is_dir().then_some(own)
    }) {
        fingerprint.str(&std::fs::read_to_string(modules.join(".kargo-package.json")).unwrap_or_default());
    }
    fingerprint.finish()
}

/// The entry module the bundler wrote (and, for a library, the file of each of its `formats`).
fn read_entry(manifest: &Path, assets: &Path, formats: &[LibraryFormat]) -> Result<(String, Vec<(LibraryFormat, String)>)> {
    let text = std::fs::read_to_string(manifest).with_context(|| format!("failed to read {}", manifest.display()))?;
    let json = serde_json::from_str::<serde_json::Value>(&text).unwrap_or_default();
    let exists = |file: &str| assets.join(file).is_file();
    let Some(entry) = json["entry"].as_str().filter(|e| exists(e)) else {
        bail!("the bundler wrote no entry module into {}", assets.display());
    };
    let mut files = Vec::new();
    if let Some(written) = json["formats"].as_object() {
        for format in formats {
            let Some(file) = written.get(format.as_str()).and_then(|f| f.as_str()).filter(|f| exists(f)) else {
                bail!("the bundler wrote no {} file into {}", format.as_str(), assets.display());
            };
            files.push((*format, file.to_owned()));
        }
    }
    Ok((entry.to_owned(), files))
}

/// Runs the bundler's script over `work/options.json`.
fn run(ctx: &BuildContext, input: &Input, bundler: Bundler, work: &Path) -> Result<()> {
    let options = work.join("options.json");
    let mut cmd = match bundler {
        Bundler::Bun => {
            let script = work.join("bundle.mjs");
            std::fs::write(&script, BUN_SCRIPT)?;
            let mut cmd = Command::new(bun(ctx)?);
            cmd.arg("run").arg(script);
            cmd
        }
        Bundler::Rollup => {
            let dir = install_rollup(ctx, input.target)?;
            let script = dir.join("kargo-bundle.mjs");
            std::fs::write(&script, ROLLUP_SCRIPT)?;
            let mut cmd = Command::new(kargo_toolchain::node()?);
            cmd.arg(script).current_dir(dir);
            cmd
        }
    };
    cmd.arg(options);
    if bundler == Bundler::Bun {
        cmd.current_dir(work);
    }
    ctx.shell().status("Bundling", format!("{}.mjs with {bundler}", input.name));
    ctx.shell().verbose("Running", format!("`{}`", process::describe(&cmd)));
    let output = cmd.output().with_context(|| format!("failed to start {bundler}"))?;
    if !output.status.success() {
        let log = format!("{}\n{}", String::from_utf8_lossy(&output.stdout).trim(), String::from_utf8_lossy(&output.stderr).trim());
        bail!(
            "{bundler} failed with {} while bundling {}.mjs; set `bundle = false` in kargo.toml to serve the modules as the compiler wrote them\n\n{}",
            process::describe_status(output.status),
            input.name,
            log.trim()
        );
    }
    let log = String::from_utf8_lossy(&output.stderr);
    if !log.trim().is_empty() {
        ctx.shell().verbose("Output", log.trim());
    }
    Ok(())
}

/// The `bun` on `PATH`, else the pinned one (downloaded the first time).
fn bun(ctx: &BuildContext) -> Result<PathBuf> {
    match find_on_path("bun") {
        Some(exe) => Ok(exe),
        None => kargo_toolchain::bun::ensure(ctx.config).context("bundler = \"bun\" needs Bun; install it and put `bun` on PATH"),
    }
}

/// Installs Rollup and its plugins (apart from the project's npm packages) and returns the directory that holds them.
fn install_rollup(ctx: &BuildContext, target: TargetKind) -> Result<PathBuf> {
    let deps: BTreeMap<String, String> =
        ROLLUP_PACKAGES.iter().map(|(name, version)| ((*name).to_owned(), (*version).to_owned())).collect();
    let dir = ctx.ws.work_dir("rollup");
    crate::npm::install_tool(ctx, target, &deps, &dir)
        .context("bundler = \"rollup\" needs Rollup, which Kargo installs with npm, pnpm or bun")?;
    Ok(dir)
}

/// `kargo fetch` and `kargo update`: gets the bundler of `target`, so an offline build finds it.
pub fn fetch(ctx: &BuildContext, target: TargetKind) -> Result<()> {
    let Some(bundling) = configured(ctx.ws.manifest(), target) else { return Ok(()) };
    let fetched = match bundling.bundler {
        Bundler::Bun => bun(ctx).map(|_| ()),
        Bundler::Rollup => install_rollup(ctx, target).map(|_| ()),
    };
    if let Err(e) = fetched {
        ctx.shell().warn(format!("{} was not fetched: {e:#}", bundling.bundler));
    }
    Ok(())
}

impl Bundle {
    /// `html` with the module the compiler wrote (`<name>.mjs`) replaced by the entry module of the bundle. Returns
    /// whether the page loaded that module at all.
    pub fn rewrite_page(&self, html: &str, name: &str) -> (String, bool) {
        let module = format!("{name}.mjs");
        (html.replace(&module, &self.entry), html.contains(&module))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_loads_the_entry_module() {
        let bundle = Bundle { dir: "out".into(), entry: "app-AbC123.js".into(), wasm: None, formats: Vec::new() };
        let page = "<script type=\"module\" src=\"./app.mjs\"></script>\n<script>import('/app.mjs')</script>";
        let (html, loaded) = bundle.rewrite_page(page, "app");
        assert!(loaded);
        assert_eq!(html, "<script type=\"module\" src=\"./app-AbC123.js\"></script>\n<script>import('/app-AbC123.js')</script>");
        let (unchanged, loaded) = bundle.rewrite_page("<p>no script</p>", "app");
        assert!(!loaded && unchanged == "<p>no script</p>");
    }

    #[test]
    fn the_wasm_file_is_named_by_its_contents() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("app.wasm");
        std::fs::write(&file, b"wasm bytes").unwrap();
        let (from, to, hash) = wasm_name("app", &file, true).unwrap();
        assert_eq!(from, "app.wasm");
        assert_eq!(hash.len(), 12);
        assert_eq!(to, format!("app.{hash}.wasm"));
        std::fs::write(&file, b"other bytes").unwrap();
        assert_ne!(wasm_name("app", &file, true).unwrap().1, to);
        assert_eq!(wasm_name("app", &file, false).unwrap().1, "app.wasm", "a library keeps the name");
    }

    #[test]
    fn only_a_bundler_that_applies_bundles() {
        let parse = |toml: &str| Manifest::parse(&format!("[package]\nname = \"web\"\nversion = \"0.1.0\"\n{toml}")).unwrap();
        let plain = parse("[target.wasm]\nenvironment = \"browser\"\n");
        assert!(configured(&plain, TargetKind::Wasm).is_none() && active(&plain, TargetKind::Wasm, true).is_none());
        let bundled = parse("[target.js]\nenvironment = \"browser\"\nbundler = \"bun\"\n");
        assert!(active(&bundled, TargetKind::Js, true).is_some(), "release builds bundle");
        assert!(active(&bundled, TargetKind::Js, false).is_none() && configured(&bundled, TargetKind::Js).is_some());
        assert!(active(&bundled, TargetKind::Wasm, true).is_none());
        let always = parse("[target.js]\nenvironment = \"browser\"\nbundler = \"rollup\"\nbundle = true\n");
        assert!(active(&always, TargetKind::Js, false).is_some());
    }

    /// Bundles a made-up Kotlin/Wasm output with the `bun` on `PATH` (the test is skipped without one): the glue loads a
    /// `.wasm` file, imports an npm package and splits off a module.
    #[test]
    fn bun_bundles_the_output_of_the_compiler() {
        use crate::workspace::{ProfileKind, Project};
        use kargo_util::config::Config;
        use kargo_util::shell::{Shell, Verbosity};

        if find_on_path("bun").is_none() {
            eprintln!("skipping: bun is not on PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("kargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[target.wasm]\nenvironment = \"browser\"\nbundler = \"bun\"\nexternal = [\"outside\"]\n",
        )
        .unwrap();
        let wasm_dir = dir.path().join("target/debug/wasm");
        std::fs::create_dir_all(&wasm_dir).unwrap();
        std::fs::create_dir_all(dir.path().join("target/debug/node_modules/left-pad")).unwrap();
        std::fs::write(dir.path().join("target/debug/node_modules/left-pad/package.json"), r#"{"name":"left-pad","main":"index.js"}"#)
            .unwrap();
        std::fs::write(dir.path().join("target/debug/node_modules/left-pad/index.js"), "module.exports = (s, n) => String(s).padStart(n);")
            .unwrap();
        std::fs::write(
            wasm_dir.join("app.mjs"),
            "import pad from 'left-pad';\nimport 'outside';\nconst wasm = new URL('./app.wasm', import.meta.url).href;\nconsole.log(pad('x', 4), wasm);\nexport const more = () => import('./lazy.mjs');\n",
        )
        .unwrap();
        std::fs::write(wasm_dir.join("lazy.mjs"), "export const lazy = 'lazy';\n").unwrap();
        std::fs::write(wasm_dir.join("app.wasm"), b"\0asm").unwrap();

        let project = Project::load(&dir.path().join("kargo.toml")).unwrap();
        let config = Config::new(home.path().to_path_buf(), Shell::new(Verbosity::Quiet));
        let ctx = BuildContext::new(&config, &project, 0, ProfileKind::Dev).unwrap();
        let input = Input {
            target: TargetKind::Wasm,
            name: "app",
            module: &wasm_dir.join("app.mjs"),
            dir: &wasm_dir,
            wasm: Some(&wasm_dir.join("app.wasm")),
            browser: true,
            library: None,
        };
        let bundle = bundle(&ctx, &input).unwrap();
        assert_eq!(bundle.dir, dir.path().join("target/debug/wasm-bundle"));
        assert!(bundle.entry.starts_with("app-") && bundle.entry.ends_with(".js"), "{}", bundle.entry);
        let code = std::fs::read_to_string(bundle.dir.join(&bundle.entry)).unwrap();
        let wasm = bundle.wasm.clone().unwrap();
        let wasm_name = wasm.file_name().unwrap().to_string_lossy().into_owned();
        assert!(wasm.is_file() && wasm_name.starts_with("app.") && wasm_name != "app.wasm", "{wasm_name}");
        assert!(code.contains(&wasm_name) && !code.contains("'./app.wasm'"), "the glue loads the hashed file:\n{code}");
        assert!(code.contains("padStart") && !code.contains("\"left-pad\""), "the package is bundled:\n{code}");
        assert!(code.contains("outside"), "`external` stays an import:\n{code}");
        // A development build keeps readable code and has a source map; the dynamic import is a chunk of its own.
        assert!(code.contains("more") && bundle.dir.join(format!("{}.map", bundle.entry)).is_file());
        assert!(std::fs::read_dir(&bundle.dir).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with("chunk-")));

        // Unchanged output is not bundled again; a change is.
        let again = self::bundle(&ctx, &input).unwrap();
        assert_eq!(again, bundle);
        std::fs::write(wasm_dir.join("lazy.mjs"), "export const lazy = 'changed';\n").unwrap();
        let changed = self::bundle(&ctx, &input).unwrap();
        assert!(
            std::fs::read_dir(&changed.dir)
                .unwrap()
                .flatten()
                .any(|e| std::fs::read_to_string(e.path()).is_ok_and(|c| c.contains("changed")))
        );
    }

    /// Bundles a made-up Kotlin/JS library with the `bun` on `PATH` (the test is skipped without one): it keeps its names
    /// and is written in the ES module and CommonJS formats, with its declarations.
    #[test]
    fn bun_bundles_a_library_in_its_formats() {
        use crate::workspace::{ProfileKind, Project};
        use kargo_util::config::Config;
        use kargo_util::shell::{Shell, Verbosity};

        if find_on_path("bun").is_none() {
            eprintln!("skipping: bun is not on PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("kargo.toml"),
            "[package]\nname = \"greeter\"\nversion = \"0.1.0\"\n[target.js]\nkind = \"library\"\n\
             bundler = \"bun\"\nformats = [\"esm\", \"cjs\"]\n",
        )
        .unwrap();
        let js_dir = dir.path().join("target/debug/js");
        std::fs::create_dir_all(&js_dir).unwrap();
        std::fs::create_dir_all(dir.path().join("target/debug/node_modules/left-pad")).unwrap();
        std::fs::write(dir.path().join("target/debug/node_modules/left-pad/package.json"), r#"{"name":"left-pad","main":"index.js"}"#)
            .unwrap();
        std::fs::write(dir.path().join("target/debug/node_modules/left-pad/index.js"), "module.exports = (s, n) => String(s).padStart(n);")
            .unwrap();
        std::fs::write(js_dir.join("greeter.mjs"), "import pad from 'left-pad';\nexport const greet = (n) => pad(n, 4) + n;\n").unwrap();
        std::fs::write(js_dir.join("greeter.d.mts"), "export declare function greet(name: string): string;\n").unwrap();

        let project = Project::load(&dir.path().join("kargo.toml")).unwrap();
        let config = Config::new(home.path().to_path_buf(), Shell::new(Verbosity::Quiet));
        let ctx = BuildContext::new(&config, &project, 0, ProfileKind::Dev).unwrap();
        let input = Input {
            target: TargetKind::Js,
            name: "greeter",
            module: &js_dir.join("greeter.mjs"),
            dir: &js_dir,
            wasm: None,
            browser: false,
            library: Some(LibraryInput { package: "@acme/greeter", dependencies: Vec::new() }),
        };
        let bundle = bundle(&ctx, &input).unwrap();
        assert_eq!(bundle.formats, [(LibraryFormat::Esm, "greeter.mjs".to_owned()), (LibraryFormat::Cjs, "greeter.cjs".to_owned())]);
        let code = std::fs::read_to_string(bundle.dir.join("greeter.mjs")).unwrap();
        assert!(code.contains("padStart") && !code.contains("\"left-pad\""), "the package is bundled:\n{code}");
        assert!(bundle.wasm.is_none());
        assert!(bundle.dir.join("greeter.cjs").is_file());
        assert!(bundle.dir.join("greeter.d.mts").is_file() && bundle.dir.join("greeter.d.cts").is_file());
    }

    #[test]
    fn libraries_get_a_javascript_global_name() {
        assert_eq!(global_name_for("left-pad"), "LeftPad");
        assert_eq!(global_name_for("@acme/left-pad"), "LeftPad");
        assert_eq!(global_name_for("2fa"), "_2fa");
    }

    #[test]
    fn the_stamp_follows_the_modules_and_options() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("app.mjs"), "export {}").unwrap();
        std::fs::write(dir.path().join("app.wasm"), "wasm").unwrap();
        let input = Input {
            target: TargetKind::Wasm,
            name: "app",
            module: &dir.path().join("app.mjs"),
            dir: dir.path(),
            wasm: None,
            browser: true,
            library: None,
        };
        let options = serde_json::json!({ "minify": true });
        let first = stamp(&input, &options, Bundler::Bun);
        assert_eq!(first, stamp(&input, &options, Bundler::Bun));
        assert_ne!(first, stamp(&input, &options, Bundler::Rollup));
        assert_ne!(first, stamp(&input, &serde_json::json!({ "minify": false }), Bundler::Bun));
        std::fs::write(dir.path().join("app.mjs"), "export const a = 1").unwrap();
        assert_ne!(first, stamp(&input, &options, Bundler::Bun));
        std::fs::write(dir.path().join("notes.txt"), "not a module").unwrap();
        let second = stamp(&input, &options, Bundler::Bun);
        std::fs::write(dir.path().join("notes.txt"), "still not a module").unwrap();
        assert_eq!(second, stamp(&input, &options, Bundler::Bun), "other files do not matter");
    }
}
