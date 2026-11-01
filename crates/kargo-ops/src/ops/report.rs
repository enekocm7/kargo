//! `kargo report`: a redacted Markdown (or `.zip`) bundle for a bug report, and the machine
//! facts `kargo info` adds: the build of Kargo, the JDK and toolchains in use, the sizes of the
//! Kargo home and the cache, the compiler daemons and the environment variables Kargo read.
//!
//! Nothing is uploaded. The redaction rules are explicit: the home directory becomes `~`, the
//! user and machine names are replaced, URLs lose their credentials and query strings, only the
//! allow-listed environment variables are included, and the values of keys such as `password` or
//! `token` in `kargo.toml` become `***`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use kargo_manifest::{JdkRequest, Manifest};
use kargo_toolchain::{GradleDistribution, Jdk, LanguageServer, NativeToolchain, graalvm, jdks};

use crate::lockfile::Lockfile;
use crate::ops::context::jdk_needs;
use crate::ops::doctor::{self, ProjectState};
use crate::workspace::Project;

/// The commit Kargo was built from, exported by `build.rs` (`unknown` in a crates.io build).
pub const COMMIT: &str = env!("KARGO_COMMIT");
/// The date Kargo was built, `YYYY-MM-DD` (`SOURCE_DATE_EPOCH` when it is set).
pub const BUILD_DATE: &str = env!("KARGO_BUILD_DATE");

/// The environment variables Kargo reads that may appear in a report. Credentials (`NPM_TOKEN`,
/// the signing key, repository passwords) are deliberately not here.
const ALLOWED_ENV: &[&str] = &[
    "KARGO_HOME",
    "KARGO_TARGET_DIR",
    "KARGO_JOBS",
    "KARGO_PLATFORM",
    "KARGO_AUTO_INSTALL",
    "KARGO_OFFLINE",
    "KARGO_VENDORED",
    "KARGO_REQUIRE_CHECKSUMS",
    "KARGO_SYSTEM_GRADLE",
    "KARGO_DAEMON",
    "KARGO_INCREMENTAL",
    "KARGO_NATIVE_CACHE",
    "KARGO_COMPILER_JVM_OPTS",
    "KARGO_BROWSER",
    "KARGO_BROWSER_ARGS",
    "KARGO_BROWSER_TIMEOUT",
    "KARGO_ANDROID_DEVICE",
    "KARGO_NO_UPDATE_CHECK",
    "KARGO_RELEASES_URL",
    "KARGO_OSV_URL",
    "KARGO_MAVEN_LOCAL",
    "KARGO_MAVEN_CENTRAL_URL",
    "KARGO_MAVEN_SEARCH_URL",
    "KARGO_INSTALL_ROOT",
    "KARGO_GPG",
    "JAVA_HOME",
    "GRAALVM_HOME",
    "ANDROID_HOME",
    "ANDROID_SDK_ROOT",
    "KONAN_DATA_DIR",
    "SOURCE_DATE_EPOCH",
    "NPM_CONFIG_REGISTRY",
    "NPM_CONFIG_USERCONFIG",
    "BROWSER",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "NO_COLOR",
    "CLICOLOR_FORCE",
    "CI",
    "TERM",
    "SHELL",
];

/// `kargo 0.1.5 (abc1234, built 2026-10-31, x86_64-unknown-linux-gnu)`.
pub fn version_line() -> String {
    format!("{} ({}, built {}, {})", env!("CARGO_PKG_VERSION"), COMMIT, BUILD_DATE, crate::ops::self_update::TARGET)
}

/// Replaces what may not be shared in a `kargo.toml`: the values of secret keys and the
/// credentials and query strings of URLs.
#[cfg(test)]
fn redact_manifest(text: &str) -> String {
    Redactor::new().text(&mask_secrets(text))
}

/// The JDK the project builds with, and where it came from: `JAVA_HOME`, `PATH`, a download for
/// a pinned `[toolchain] jdk`, or a Kargo-managed JDK as there is no system one.
pub fn jdk_summary(config: &kargo_util::config::Config, manifest: Option<&Manifest>) -> String {
    let Some(m) = manifest else {
        return match Jdk::find() {
            Ok(Some(jdk)) => format!("JDK {} at {} ({})", jdk.version, jdk.home.display(), system_jdk_origin()),
            Ok(None) => "no JDK on JAVA_HOME or PATH".to_owned(),
            Err(e) => format!("no usable JDK: {e}"),
        };
    };
    match &m.toolchain.jdk {
        JdkRequest::System => match Jdk::find() {
            Ok(Some(jdk)) => format!("JDK {} at {} ({})", jdk.version, jdk.home.display(), system_jdk_origin()),
            Ok(None) => match jdks::find_fallback(config, jdk_needs(m)) {
                Ok(Some(managed)) => {
                    format!("JDK {} at {} (downloaded, as there is no system JDK)", managed.jdk().version, managed.jdk().home.display())
                }
                _ => "no JDK on JAVA_HOME or PATH; the first build downloads one".to_owned(),
            },
            Err(e) => format!("no usable JDK: {e}"),
        },
        request => match jdks::find_installed(config, request) {
            Ok(Some(managed)) => format!(
                "JDK {} at {} (pinned `[toolchain] jdk = \"{request}\"`, downloaded)",
                managed.jdk().version,
                managed.jdk().home.display()
            ),
            _ => format!("pinned `[toolchain] jdk = \"{request}\"`, not installed yet (downloaded on first build)"),
        },
    }
}

fn system_jdk_origin() -> &'static str {
    if let Some(home) = std::env::var_os("JAVA_HOME").filter(|h| !h.is_empty()) {
        let java = PathBuf::from(&home).join("bin").join(if cfg!(windows) { "java.exe" } else { "java" });
        if java.is_file() {
            return "JAVA_HOME";
        }
    }
    "PATH"
}

/// The toolchains installed under the Kargo home, as one line.
pub fn toolchains_summary(config: &kargo_util::config::Config) -> String {
    let mut found: Vec<String> = Vec::new();
    found.extend(jdks::installed(config).unwrap_or_default().iter().map(|j| format!("JDK {}", j.release)));
    found.extend(graalvm::installed(config).unwrap_or_default().iter().map(|g| format!("GraalVM {}", g.release())));
    found.extend(NativeToolchain::installed(config).unwrap_or_default().iter().map(|n| format!("Kotlin/Native {}", n.version)));
    found.extend(GradleDistribution::installed(config).unwrap_or_default().iter().map(|g| format!("Gradle {}", g.version)));
    found.extend(LanguageServer::installed(config).unwrap_or_default().iter().map(|l| format!("{} {}", l.kind.title(), l.version)));
    if found.is_empty() { format!("none in {}", config.toolchains_dir().display()) } else { found.join(", ") }
}

/// The compiler daemons running now, as one line.
pub fn daemons_summary(config: &kargo_util::config::Config) -> String {
    match kargo_compiler::daemon::running(config.home()).len() {
        0 => "none running".to_owned(),
        1 => "1 running (`kargo daemon stop` stops it)".to_owned(),
        n => format!("{n} running (`kargo daemon stop` stops them)"),
    }
}

/// The size of a directory tree, in bytes; `None` when it does not exist.
pub fn directory_size(path: &Path) -> Option<u64> {
    if !path.exists() {
        return None;
    }
    Some(
        walkdir::WalkDir::new(path)
            .into_iter()
            .flatten()
            .filter(|e| e.file_type().is_file())
            .filter_map(|e| e.metadata().ok().map(|m| m.len()))
            .sum(),
    )
}

/// The allow-listed environment variables that are set, with their values redacted.
pub fn environment() -> Vec<(String, String)> {
    let redactor = Redactor::new();
    ALLOWED_ENV
        .iter()
        .filter_map(|name| {
            let value = std::env::var_os(name)?;
            if value.is_empty() {
                return None;
            }
            Some(((*name).to_owned(), redactor.text(&value.to_string_lossy())))
        })
        .collect()
}

/// A report for a bug report: the Markdown, the files a `.zip` holds, and what it contains.
#[derive(Debug)]
pub struct Bundle {
    pub markdown: String,
    /// Extra files for the `.zip`: name and contents.
    pub files: Vec<(String, String)>,
    /// What the report contains, for the message the command prints.
    pub contents: Vec<String>,
}

/// Builds the report. `state` is what `kargo doctor` checks in the current directory.
pub fn build(
    config: &kargo_util::config::Config,
    state: &ProjectState<'_>,
    include_lock: bool,
    is_builtin_command: &dyn Fn(&str) -> bool,
) -> Bundle {
    let redactor = Redactor::new();
    let project = match state {
        ProjectState::Loaded(project) => Some(*project),
        _ => None,
    };
    let manifest_path = match state {
        ProjectState::Loaded(project) => Some(project.root().join(kargo_manifest::MANIFEST_NAME)),
        ProjectState::Broken { manifest, .. } => Some(manifest.clone()),
        ProjectState::None => None,
    };
    let manifest = manifest_path.as_ref().and_then(|path| std::fs::read_to_string(path).ok());
    // A workspace's members have their own manifests, which the root one does not contain.
    let member_manifests: Vec<(String, String)> = match state {
        ProjectState::Loaded(project) if project.is_workspace() => project
            .members()
            .iter()
            .filter_map(|&i| {
                let member = project.package(i);
                let path = member.manifest_path();
                if Some(&path) == manifest_path.as_ref() {
                    return None;
                }
                let text = std::fs::read_to_string(&path).ok()?;
                Some((member.name().to_owned(), text))
            })
            .collect(),
        _ => Vec::new(),
    };
    let target_dir = project.map(|p| p.target_dir().to_path_buf());
    let last_error = target_dir.as_ref().and_then(|dir| std::fs::read_to_string(kargo_util::capture::last_error_path(dir)).ok());
    let lockfile = project.and_then(|p| std::fs::read_to_string(p.lockfile_path()).ok());

    let mut markdown = String::new();
    let _ = writeln!(markdown, "# Kargo bug report\n");
    let _ = writeln!(
        markdown,
        "Written by `kargo report` on {}. Nothing in it was uploaded; read it before you paste it into an issue.\n",
        timestamp()
    );

    let mut contents = vec!["versions and platform".to_owned()];
    markdown.push_str("## Versions\n\n");
    let _ = writeln!(markdown, "- kargo: {}", version_line());
    let _ = writeln!(
        markdown,
        "- os: {} ({}), {} CPUs",
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
    );
    let kotlin = project.map(|p| p.package(p.current().unwrap_or(p.members()[0])).manifest().kotlin.version.clone());
    if let Some(kotlin) = &kotlin {
        let _ = writeln!(markdown, "- kotlin: {kotlin}");
    }
    let manifest_loaded = project.map(|p| p.package(p.current().unwrap_or(p.members()[0])).manifest());
    let _ = writeln!(markdown, "- jdk: {}", jdk_summary(config, manifest_loaded));
    markdown.push('\n');

    markdown.push_str("## Locations\n\n");
    let home = config.home();
    let cache = home.join("cache");
    let _ = writeln!(markdown, "- KARGO_HOME: {} ({})", path(home), size_label(home));
    let _ = writeln!(markdown, "- cache: {} ({})", path(&cache), size_label(&cache));
    let _ = writeln!(markdown, "- toolchains: {} ({})", path(&config.toolchains_dir()), size_label(&config.toolchains_dir()));
    if let Some(dir) = &target_dir {
        let _ = writeln!(markdown, "- target dir: {} ({})", path(dir), size_label(dir));
    }
    markdown.push('\n');
    contents.push("the locations and sizes of KARGO_HOME, the cache and the toolchains".to_owned());

    markdown.push_str("## Toolchains\n\n");
    let _ = writeln!(markdown, "- installed: {}", toolchains_summary(config));
    let _ = writeln!(markdown, "- compiler daemons: {}", daemons_summary(config));
    markdown.push('\n');
    contents.push("the installed toolchains and the compiler daemons".to_owned());

    let environment = environment();
    if !environment.is_empty() {
        markdown.push_str("## Environment\n\n");
        markdown.push_str("Only the variables Kargo reads from its allow list are here.\n\n");
        for (name, value) in &environment {
            let _ = writeln!(markdown, "- `{name}` = {value}");
        }
        markdown.push('\n');
        contents.push("the environment variables Kargo reads (an allow list; no credentials)".to_owned());
    }

    let mut files = Vec::new();
    if let Some(manifest) = &manifest {
        let redacted = redactor.text(&mask_secrets(manifest));
        markdown.push_str("## kargo.toml\n\n");
        markdown.push_str("```toml\n");
        markdown.push_str(redacted.trim_end());
        markdown.push_str("\n```\n\n");
        files.push(("kargo.toml".to_owned(), redacted.clone()));
        contents.push("kargo.toml, with credentials, tokens and URL query strings removed".to_owned());
        for (name, text) in &member_manifests {
            let redacted = redactor.text(&mask_secrets(text));
            let _ = writeln!(markdown, "### members/{name}/kargo.toml\n");
            markdown.push_str("```toml\n");
            markdown.push_str(redacted.trim_end());
            markdown.push_str("\n```\n\n");
            files.push((format!("members/{name}/kargo.toml"), redacted));
        }
    }

    markdown.push_str("## Last error\n\n");
    match &last_error {
        Some(text) => {
            let redacted = redactor.text(&mask_secrets(text));
            markdown.push_str("```text\n");
            markdown.push_str(redacted.trim_end());
            markdown.push_str("\n```\n\n");
            files.push(("last-error.log".to_owned(), redacted));
            contents.push("the output of the last failed command".to_owned());
        }
        None => markdown.push_str("No failed `build`, `check`, `test` or `run` is recorded for this project.\n\n"),
    }

    let doctor_state = match state {
        ProjectState::None => ProjectState::None,
        ProjectState::Loaded(project) => ProjectState::Loaded(project),
        ProjectState::Broken { manifest, error } => ProjectState::Broken { manifest: manifest.clone(), error },
    };
    let doctor_report = doctor::diagnose(config, doctor_state, is_builtin_command);
    markdown.push_str("## kargo doctor\n\n");
    markdown.push_str("```text\n");
    markdown.push_str(redactor.text(&mask_secrets(&doctor::render(&doctor_report))).trim_end());
    markdown.push_str("\n```\n\n");
    contents.push("the checks of kargo doctor".to_owned());

    markdown.push_str("## kargo.lock\n\n");
    match (&lockfile, project.map(Project::lockfile_path)) {
        (Some(text), Some(path)) => {
            let hash = kargo_util::hash::sha256_bytes(text.as_bytes());
            let _ = writeln!(markdown, "- file: {}", redactor.text(&path.display().to_string()));
            let _ = writeln!(markdown, "- sha256: {hash}");
            contents.push("the hash of kargo.lock".to_owned());
            if include_lock {
                markdown.push('\n');
                match Lockfile::load(&path) {
                    Ok(lock) => {
                        markdown.push_str("| module | version |\n|---|---|\n");
                        for (module, versions) in lock.module_versions() {
                            let _ = writeln!(markdown, "| {module} | {} |", versions.iter().cloned().collect::<Vec<_>>().join(", "));
                        }
                        for (key, tool) in &lock.npm_tools {
                            let _ = writeln!(markdown, "| {key} (npm) | {} |", tool.version);
                        }
                        markdown.push('\n');
                        contents.push("every version kargo.lock resolved".to_owned());
                    }
                    Err(e) => {
                        let _ = writeln!(markdown, "\nThe versions are not readable: {e}\n");
                    }
                }
            }
        }
        _ => markdown.push_str("There is no kargo.lock.\n\n"),
    }

    markdown.push_str("---\n\n");
    let _ =
        writeln!(markdown, "Report the problem at {ISSUES_URL}, or run `kargo report --open` in the project to open a prefilled issue.");

    files.insert(0, (REPORT_FILE.to_owned(), markdown.clone()));
    Bundle { markdown, files, contents }
}

/// Where `kargo report` writes a Markdown report or a `.zip`, and how big the message may be.
pub const REPORT_FILE: &str = "kargo-report.md";
pub const ZIP_FILE: &str = "kargo-report.zip";
/// The page a report is pasted into.
pub const ISSUES_URL: &str = "https://github.com/enekocm7/kargo/issues/new";
/// The GitHub title of a prefilled issue.
pub const ISSUE_TITLE: &str = "Bug report: ";

/// Writes the report as Markdown, or as a `.zip` with the Markdown and the logs.
pub fn write(path: &Path, bundle: &Bundle, zip: bool) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    }
    if zip {
        let file = std::fs::File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, contents) in &bundle.files {
            writer.start_file(name.as_str(), options).with_context(|| format!("failed to write {name}"))?;
            std::io::Write::write_all(&mut writer, contents.as_bytes()).with_context(|| format!("failed to write {name}"))?;
        }
        writer.finish().with_context(|| format!("failed to finish {}", path.display()))?;
    } else {
        std::fs::write(path, &bundle.markdown).with_context(|| format!("failed to write {}", path.display()))?;
    }
    Ok(())
}

/// A directory's size for `kargo info`: `1.2 GiB`, or `not created yet`.
pub fn size_label(path: &Path) -> String {
    match directory_size(path) {
        Some(bytes) => kargo_toolchain::archive::human_size(bytes),
        None => "not created yet".to_owned(),
    }
}

/// The directory `kargo report` writes into: the target directory of the project, else
/// `reports/` in the Kargo home.
pub fn output_dir(config: &kargo_util::config::Config, project: Option<&Project>) -> PathBuf {
    match project {
        Some(project) => project.target_dir().join(".kargo"),
        None => config.home().join("reports"),
    }
}

/// A prefilled GitHub issue URL. The body is the Markdown, cut when the URL would be too long.
pub fn issue_url(markdown: &str) -> String {
    let title = format!("{}kargo {}", ISSUE_TITLE, env!("CARGO_PKG_VERSION"));
    let body = format!(
        "{markdown}\n\n<!-- Make sure no secret slipped through before submitting. -->\n\n\
         <!-- To get the full report as a file next time: `kargo report` writes target/.kargo/kargo-report.md. -->"
    );
    let body = truncate_on_char(&body, 5_000);
    format!("{ISSUES_URL}?title={}&body={}", encode(&title), encode(body))
}

fn truncate_on_char(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((at, _)) => &text[..at],
        None => text,
    }
}

/// Percent-encodes everything but the unreserved characters, as a URL query value needs.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

fn path(path: &Path) -> String {
    Redactor::new().text(&path.display().to_string())
}

/// The current UTC time, `YYYY-MM-DDTHH:MM:SSZ`.
fn timestamp() -> String {
    let seconds = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default();
    kargo_util::capture::iso8601(seconds)
}

/// Replaces what may not be shared: the home directory, the user and machine names, and the
/// credentials and query strings of URLs.
#[derive(Debug, Clone, Default)]
struct Redactor {
    home: Option<String>,
    user: Option<String>,
    host: Option<String>,
}

impl Redactor {
    fn new() -> Self {
        let home = dirs::home_dir().map(|p| p.to_string_lossy().into_owned());
        let user = std::env::var("USER")
            .ok()
            .or_else(|| std::env::var("USERNAME").ok())
            .or_else(|| home.as_deref().and_then(|h| Path::new(h).file_name()).map(|n| n.to_string_lossy().into_owned()));
        let host = std::env::var("COMPUTERNAME")
            .ok()
            .or_else(|| std::env::var("HOSTNAME").ok())
            .or_else(|| std::fs::read_to_string("/etc/hostname").ok().map(|h| h.trim().to_owned()))
            .filter(|h| !h.is_empty());
        Self { home, user, host }
    }

    #[cfg(test)]
    fn with(home: &str, user: &str, host: &str) -> Self {
        Self { home: Some(home.to_owned()), user: Some(user.to_owned()), host: Some(host.to_owned()) }
    }

    fn text(&self, text: &str) -> String {
        let mut out = redact_urls(text);
        if let Some(home) = self.home.as_deref().filter(|h| h.len() > 1) {
            out = out.replace(home, "~");
            let forward = home.replace('\\', "/");
            if forward != home {
                out = out.replace(&forward, "~");
            }
        }
        if let Some(user) = self.user.as_deref().filter(|u| u.len() >= 3) {
            out = out.replace(user, "<user>");
        }
        if let Some(host) = self.host.as_deref().filter(|h| h.len() >= 3) {
            out = out.replace(host, "<host>");
        }
        out
    }
}

/// Replaces every URL in `text` with its credentials and query string removed.
fn redact_urls(text: &str) -> String {
    const START: &str = "(\"'<[{|,;`";
    const END: &str = ")\"'>]}|,;`";
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    while let Some(found) = text[pos..].find("://") {
        let scheme_end = pos + found;
        let uri_start = text[..scheme_end].rfind(|c: char| c.is_whitespace() || START.contains(c)).map(|i| i + 1).unwrap_or(0);
        let rest = &text[scheme_end + 3..];
        let end = rest.find(|c: char| c.is_whitespace() || END.contains(c)).map(|i| scheme_end + 3 + i).unwrap_or(text.len());
        out.push_str(&text[pos..uri_start]);
        out.push_str(&redact_url(&text[uri_start..end]));
        pos = end;
    }
    out.push_str(&text[pos..]);
    out
}

/// `https://user:secret@host:8443/path?token=x` becomes `https://***@host:8443/path`.
fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else { return url.to_owned() };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let userinfo = authority.rfind('@').map(|at| &authority[..at]);
    let host = &authority[authority.rfind('@').map(|at| at + 1).unwrap_or(0)..];
    let path = &rest[authority_end..];
    let path = path.split(['?', '#']).next().unwrap_or("");
    match userinfo {
        Some(_) => format!("{scheme}://***@{host}{path}"),
        None => format!("{scheme}://{host}{path}"),
    }
}

/// Replaces the values of keys such as `password` or `token` with `***`, in plain TOML text.
/// `kargo.toml` may be broken, so this works line by line instead of parsing it.
fn mask_secrets(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut skipping: Option<&'static str> = None;
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if let Some(delimiter) = skipping {
            // The rest of a multiline secret value; its first line already holds `***`.
            if line.contains(delimiter) {
                skipping = None;
            }
            continue;
        }
        let (masked, opened) = mask_line(line);
        out.push_str(&masked);
        skipping = opened;
    }
    out
}

const TRIPLE_DOUBLE: &str = "\"\"\"";
const TRIPLE_SINGLE: &str = "'''";

/// Masks the secret values of one line. The second value is the delimiter of a multiline secret
/// value that continues on the next lines.
fn mask_line(line: &str) -> (String, Option<&'static str>) {
    let mut out = String::with_capacity(line.len());
    let mut opened = None;
    let mut pos = 0;
    while pos < line.len() {
        let ch = line[pos..].chars().next().unwrap_or_default();
        if ch == '"' || ch == '\'' {
            // A value of a key that is not secret: copy it as it is, so a `=` inside it is not a key.
            let end = quoted_end(line, pos, ch);
            out.push_str(&line[pos..end]);
            pos = end;
            continue;
        }
        if ch != '=' {
            out.push(ch);
            pos += ch.len_utf8();
            continue;
        }
        let before = line[..pos].trim_end();
        let key_start = before.rfind(|c: char| c.is_whitespace() || c == '{' || c == ',' || c == '[').map(|i| i + 1).unwrap_or(0);
        let key = before[key_start..].trim_matches(['"', '\'']);
        out.push('=');
        pos += 1;
        if !is_secret_key(key) {
            continue;
        }
        let after = &line[pos..];
        let spaces = after.len() - after.trim_start().len();
        out.push_str(&after[..spaces]);
        pos += spaces;
        let value = &line[pos..];
        if value.starts_with(TRIPLE_DOUBLE) || value.starts_with(TRIPLE_SINGLE) {
            let delimiter = if value.starts_with(TRIPLE_DOUBLE) { TRIPLE_DOUBLE } else { TRIPLE_SINGLE };
            out.push_str(delimiter);
            out.push_str("***");
            out.push_str(delimiter);
            match value[3..].find(delimiter) {
                Some(at) => pos += 3 + at + 3,
                None => {
                    opened = Some(delimiter);
                    pos = line.len();
                }
            }
        } else if value.starts_with('"') || value.starts_with('\'') {
            let quote = value.chars().next().unwrap_or('"');
            match quoted_end_at(value, quote) {
                Some(len) => {
                    out.push(quote);
                    out.push_str("***");
                    out.push(quote);
                    pos += len;
                }
                None => {
                    out.push_str("***");
                    pos = line.len();
                }
            }
        } else {
            // A number, a boolean or another bare value.
            let end = value.find([',', '}', '#']).unwrap_or(value.len());
            out.push_str("***");
            pos += end;
        }
    }
    (out, opened)
}

/// Where the quoted string starting at `start` ends, or the end of the line.
fn quoted_end(line: &str, start: usize, quote: char) -> usize {
    let rest = &line[start..];
    if let Some(delimiter) = triple(rest, quote) {
        return match rest[3..].find(delimiter) {
            Some(at) => start + 3 + at + 3,
            None => line.len(),
        };
    }
    quoted_end_at(rest, quote).map(|len| start + len).unwrap_or(line.len())
}

/// The length of the quoted value at the start of `value`, both quotes included.
fn quoted_end_at(value: &str, quote: char) -> Option<usize> {
    let mut escaped = false;
    for (i, c) in value[1..].char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == quote {
            return Some(i + 2);
        }
    }
    None
}

fn triple(rest: &str, quote: char) -> Option<&'static str> {
    if quote == '"' && rest.starts_with(TRIPLE_DOUBLE) {
        Some(TRIPLE_DOUBLE)
    } else if quote == '\'' && rest.starts_with(TRIPLE_SINGLE) {
        Some(TRIPLE_SINGLE)
    } else {
        None
    }
}

fn is_secret_key(key: &str) -> bool {
    let key = key.trim().trim_matches(['"', '\'']).to_ascii_lowercase();
    if key.ends_with("-env") || key.ends_with("_env") {
        return false;
    }
    ["password", "passphrase", "secret", "token", "credential", "api-key", "apikey"].iter().any(|w| key.contains(w))
        || matches!(key.as_str(), "key" | "private-key" | "signing-key")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_homes_names_and_urls() {
        let r = Redactor::with("/home/eneko", "eneko", "workstation");
        let text = "built in /home/eneko/app on workstation by eneko; see https://eneko:secret@example.com/repo?token=abc#x";
        assert_eq!(r.text(text), "built in ~/app on <host> by <user>; see https://***@example.com/repo");
        assert_eq!(redact_url("https://repo.example.com/maven"), "https://repo.example.com/maven");
        assert_eq!(redact_url("file:///home/eneko/.m2"), "file:///home/eneko/.m2");
        assert_eq!(redact_url("proxy:3128"), "proxy:3128");
    }

    #[test]
    fn redacts_urls_inside_quotes_and_paths() {
        let r = Redactor::with("/home/eneko", "eneko", "workstation");
        assert_eq!(r.text(r#"url = "https://user:pw@nexus.corp/repo?key=1""#), r#"url = "https://***@nexus.corp/repo""#);
        assert_eq!(r.text("open https://a.example/x) now"), "open https://a.example/x) now");
    }

    #[test]
    fn masks_secret_values_in_manifests() {
        let manifest = r#"[package]
name = "app"
[repositories]
mirror = { url = "https://me:s3cret@nexus.corp/maven?token=x", password = "hunter2", username-env = "TOKEN" }
password = "plain"
api-key = 'abc'
note = "key-value"
"#;
        let redacted = redact_manifest(manifest);
        assert!(redacted.contains("https://***@nexus.corp/maven"), "{redacted}");
        assert!(!redacted.contains("hunter2"), "{redacted}");
        assert!(!redacted.contains("plain"), "{redacted}");
        assert!(!redacted.contains("abc"), "{redacted}");
        assert!(redacted.contains("username-env = \"TOKEN\""), "{redacted}");
        assert!(redacted.contains("note = \"key-value\""), "{redacted}");
        assert!(redacted.contains("name = \"app\""), "{redacted}");
    }

    #[test]
    fn an_unparseable_manifest_is_still_redacted() {
        let text = "password = \"secret\"\nnot toml [\n";
        let redacted = redact_manifest(text);
        assert!(!redacted.contains("secret"), "{redacted}");
        assert!(redacted.contains("not toml ["), "{redacted}");
    }

    #[test]
    fn encodes_issue_urls() {
        let url = issue_url("## Versions\n\n- kargo: 1.0 & more");
        assert!(url.starts_with(ISSUES_URL), "{url}");
        assert!(url.contains("body=%23%23%20Versions"), "{url}");
        assert!(!url.contains('\n'), "{url}");
        assert!(!url.contains(' '), "{url}");
    }
}
