//! `kargo new` / `kargo init`: project scaffolding.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::builtins::FRAMEWORK_NAMES;
use kargo_manifest::{DEFAULT_KOTLIN_VERSION, MANIFEST_NAME, TargetKind, validate_package_name};

#[derive(Debug, Clone)]
pub struct NewOptions {
    pub name: String,
    pub group: String,
    /// A library instead of an application.
    pub lib: bool,
    pub targets: Vec<TargetKind>,
    pub frameworks: Vec<String>,
    pub kotlin_version: String,
    /// Initialize a git repository (if `git` is available and not already inside one).
    pub vcs: bool,
}

impl NewOptions {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            group: "com.example".to_owned(),
            lib: false,
            targets: vec![TargetKind::Jvm],
            frameworks: Vec::new(),
            kotlin_version: DEFAULT_KOTLIN_VERSION.to_owned(),
            vcs: true,
        }
    }

    fn has(&self, framework: &str) -> bool {
        self.frameworks.iter().any(|f| f == framework)
    }

    /// The Kotlin package of the generated code, e.g. `com.example.my_app`.
    /// Segments that are Kotlin or Java keywords get a `_` suffix (`class` becomes `class_`).
    pub fn package(&self) -> String {
        let ident: String = self.name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
        self.group.split('.').chain([ident.as_str()]).map(package_segment).collect::<Vec<_>>().join(".")
    }

    fn validate(&self) -> Result<()> {
        validate_package_name(&self.name)?;
        if self.group.split('.').any(|s| s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')) {
            bail!("invalid group `{}` (expected something like `com.example`)", self.group);
        }
        if self.targets.is_empty() {
            bail!("at least one target is required");
        }
        for f in &self.frameworks {
            if !FRAMEWORK_NAMES.contains(&f.as_str()) {
                bail!("unknown framework `{f}` (available: {})", FRAMEWORK_NAMES.join(", "));
            }
        }
        let jvm_only = ["spring-boot", "lombok"];
        let non_jvm = self.targets.iter().any(|t| !matches!(t, TargetKind::Jvm | TargetKind::Android));
        for f in jvm_only {
            if self.has(f) && non_jvm {
                bail!("`{f}` only supports the jvm and android targets");
            }
        }
        if self.has("compose") && self.targets.iter().any(|t| matches!(t, TargetKind::Wasm | TargetKind::Native)) {
            bail!("`compose` is supported on the jvm (desktop) and android targets");
        }
        if self.has("spring-boot") && self.has("ktor") {
            bail!("pick either `spring-boot` or `ktor`, not both");
        }
        if self.has("spring-boot") && self.lib {
            bail!("`spring-boot` projects are applications; drop --lib");
        }
        Ok(())
    }
}

/// Kotlin hard keywords plus Java keywords and literals: none of them can be a
/// package segment in the generated Kotlin (or, with Lombok and Android, Java) code.
const RESERVED_WORDS: &[&str] = &[
    "abstract",
    "as",
    "assert",
    "boolean",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extends",
    "false",
    "final",
    "finally",
    "float",
    "for",
    "fun",
    "goto",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "int",
    "interface",
    "is",
    "long",
    "native",
    "new",
    "null",
    "object",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "short",
    "static",
    "strictfp",
    "super",
    "switch",
    "synchronized",
    "this",
    "throw",
    "throws",
    "transient",
    "true",
    "try",
    "typealias",
    "typeof",
    "val",
    "var",
    "void",
    "volatile",
    "when",
    "while",
];

/// Escapes a package segment that is a reserved word by appending `_`.
fn package_segment(segment: &str) -> String {
    if RESERVED_WORDS.contains(&segment) { format!("{segment}_") } else { segment.to_owned() }
}

/// A file to write, relative to the project root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateFile {
    pub path: PathBuf,
    pub contents: String,
}

fn file(path: &str, contents: impl Into<String>) -> TemplateFile {
    TemplateFile { path: PathBuf::from(path), contents: contents.into() }
}

pub fn manifest(opts: &NewOptions) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "[package]");
    let _ = writeln!(s, "name = \"{}\"", opts.name);
    let _ = writeln!(s, "version = \"0.1.0\"");
    let _ = writeln!(s, "group = \"{}\"", opts.group);
    // The Kotlin version and JVM target fall back to Kargo's defaults, so they are
    // only written when the user asked for something else.
    if opts.kotlin_version != DEFAULT_KOTLIN_VERSION {
        let _ = writeln!(s, "\n[kotlin]\nversion = \"{}\"", opts.kotlin_version);
    }
    // No [target] section at all means a plain JVM project.
    let jvm_only = opts.targets == [TargetKind::Jvm];
    for target in &opts.targets {
        match target {
            TargetKind::Jvm => {
                if !jvm_only {
                    let _ = writeln!(s, "\n[target.jvm]");
                }
            }
            TargetKind::Wasm => {
                let _ = writeln!(s, "\n[target.wasm]\nenvironment = \"js\"");
            }
            TargetKind::Native => {
                let kind = if opts.lib { "library" } else { "executable" };
                let _ = writeln!(s, "\n[target.native]\nplatforms = [\"host\"]\nkind = \"{kind}\"");
            }
            TargetKind::Android => {
                let kind = if opts.lib { "library" } else { "application" };
                let _ = writeln!(s, "\n[target.android]\nnamespace = \"{}\"\nkind = \"{kind}\"\nmin-sdk = 24", opts.package());
            }
        }
    }
    if !opts.frameworks.is_empty() {
        let _ = writeln!(s, "\n[frameworks]");
        for f in &opts.frameworks {
            match f.as_str() {
                "ktor" => {
                    let _ = writeln!(s, "ktor = {{ server = \"netty\" }}");
                }
                "spring-boot" => {
                    let _ = writeln!(s, "spring-boot = {{ starters = [\"web\"] }}");
                }
                other => {
                    let _ = writeln!(s, "{other} = true");
                }
            }
        }
    }
    s
}

fn gitignore() -> String {
    "/target\n/.kotlin\n/.gradle\n*.iml\n.idea/\n".to_owned()
}

/// Whether `src/Main.kt` comes from a framework's own template rather than the plain one.
fn framework_main(opts: &NewOptions) -> bool {
    opts.has("spring-boot") || opts.has("ktor") || (opts.has("compose") && opts.targets.contains(&TargetKind::Jvm))
}

/// The `@Serializable` class of the serialization template, in its own file so
/// the file name matches the class (as detekt and the Kotlin conventions expect).
fn serializable_source(opts: &NewOptions) -> String {
    let pkg = opts.package();
    format!("package {pkg}\n\nimport kotlinx.serialization.Serializable\n\n@Serializable\ndata class Greeting(\n    val name: String,\n)\n")
}

fn main_source(opts: &NewOptions) -> String {
    let pkg = opts.package();
    if opts.has("spring-boot") {
        return format!(
            r#"package {pkg}

import org.springframework.boot.autoconfigure.SpringBootApplication
import org.springframework.boot.runApplication
import org.springframework.web.bind.annotation.GetMapping
import org.springframework.web.bind.annotation.RequestParam
import org.springframework.web.bind.annotation.RestController

@SpringBootApplication
class Application

@RestController
class GreetingController {{
    @GetMapping("/")
    fun greet(
        @RequestParam(defaultValue = "World") name: String,
    ) = greeting(name)
}}

fun greeting(name: String) = "Hello, $name!"

fun main(args: Array<String>) {{
    runApplication<Application>(args = args)
}}
"#
        );
    }
    if opts.has("ktor") {
        return format!(
            r#"package {pkg}

import io.ktor.server.engine.embeddedServer
import io.ktor.server.netty.Netty
import io.ktor.server.response.respondText
import io.ktor.server.routing.get
import io.ktor.server.routing.routing

fun greeting(name: String) = "Hello, $name!"

fun main() {{
    embeddedServer(Netty, port = 8080) {{
        routing {{
            get("/") {{ call.respondText(greeting("Ktor")) }}
        }}
    }}.start(wait = true)
}}
"#
        );
    }
    if opts.has("compose") && opts.targets.contains(&TargetKind::Jvm) {
        return format!(
            r#"package {pkg}

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.window.Window
import androidx.compose.ui.window.application

fun greeting(name: String) = "Hello, $name!"

fun main() =
    application {{
        Window(onCloseRequest = ::exitApplication, title = "{name}") {{
            MaterialTheme {{
                var clicks by remember {{ mutableStateOf(0) }}
                Column(
                    modifier = Modifier.fillMaxSize(),
                    verticalArrangement = Arrangement.Center,
                    horizontalAlignment = Alignment.CenterHorizontally,
                ) {{
                    Text(greeting("Compose"))
                    Button(onClick = {{ clicks++ }}) {{ Text("Clicked $clicks times") }}
                }}
            }}
        }}
    }}
"#,
            name = opts.name
        );
    }
    if opts.lib {
        return format!("package {pkg}\n\n/** Returns a friendly greeting. */\nfun greeting(name: String): String = \"Hello, $name!\"\n");
    }
    let mut imports = String::new();
    let mut body = String::from("    println(greeting(\"Kotlin\"))\n");
    if opts.has("serialization") {
        imports.push_str("import kotlinx.serialization.encodeToString\nimport kotlinx.serialization.json.Json\n\n");
        body.push_str("    println(Json.encodeToString(Greeting(\"Kotlin\")))\n");
    }
    format!("package {pkg}\n\n{imports}fun greeting(name: String): String = \"Hello, $name!\"\n\nfun main() {{\n{body}}}\n")
}

fn test_source(opts: &NewOptions) -> String {
    let pkg = opts.package();
    format!(
        r#"package {pkg}

import kotlin.test.Test
import kotlin.test.assertEquals

class GreetingTest {{
    @Test
    fun greetsByName() {{
        assertEquals("Hello, Kargo!", greeting("Kargo"))
    }}
}}
"#
    )
}

fn android_files(opts: &NewOptions) -> Vec<TemplateFile> {
    let pkg = opts.package();
    let mut files = Vec::new();
    if opts.lib {
        files.push(file(
            "android/AndroidManifest.xml",
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<manifest xmlns:android=\"http://schemas.android.com/apk/res/android\" />\n",
        ));
        return files;
    }
    files.push(file(
        "android/AndroidManifest.xml",
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<manifest xmlns:android="http://schemas.android.com/apk/res/android">
    <application
        android:label="{name}"
        android:theme="@android:style/Theme.Material.Light.NoActionBar">
        <activity
            android:name=".MainActivity"
            android:exported="true">
            <intent-filter>
                <action android:name="android.intent.action.MAIN" />
                <category android:name="android.intent.category.LAUNCHER" />
            </intent-filter>
        </activity>
    </application>
</manifest>
"#,
            name = opts.name
        ),
    ));
    let activity = if opts.has("compose") {
        format!(
            r#"package {pkg}

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text

class MainActivity : ComponentActivity() {{
    override fun onCreate(savedInstanceState: Bundle?) {{
        super.onCreate(savedInstanceState)
        setContent {{
            MaterialTheme {{ Text(greeting("Android")) }}
        }}
    }}
}}
"#
        )
    } else {
        format!(
            r#"package {pkg}

import android.app.Activity
import android.os.Bundle
import android.widget.TextView

class MainActivity : Activity() {{
    override fun onCreate(savedInstanceState: Bundle?) {{
        super.onCreate(savedInstanceState)
        setContentView(TextView(this).apply {{ text = greeting("Android") }})
    }}
}}
"#
        )
    };
    files.push(file("src-android/MainActivity.kt", activity));
    files
}

/// All files of a new project.
pub fn template(opts: &NewOptions) -> Result<Vec<TemplateFile>> {
    opts.validate()?;
    let mut files = vec![file(MANIFEST_NAME, manifest(opts)), file(".gitignore", gitignore())];

    let android_only = opts.targets == [TargetKind::Android];
    let mut main = main_source(opts);
    if android_only && !opts.lib {
        // Android apps start from an Activity, not `main`.
        let lib_opts = NewOptions { lib: true, ..opts.clone() };
        main = main_source(&lib_opts);
    }
    files.push(file("src/Main.kt", main));
    if opts.has("serialization") && !framework_main(opts) {
        files.push(file("src/Greeting.kt", serializable_source(opts)));
    }
    files.push(file("tests/GreetingTest.kt", test_source(opts)));
    if opts.targets.contains(&TargetKind::Android) {
        files.extend(android_files(opts));
    }
    if opts.has("lombok") {
        files.push(file("lombok.config", "config.stopBubbling = true\nlombok.addLombokGeneratedAnnotation = true\n"));
        let pkg = opts.package();
        files.push(file(
            "src/Person.java",
            format!("package {pkg};\n\nimport lombok.Data;\n\n@Data\npublic class Person {{\n    private String name;\n    private int age;\n}}\n"),
        ));
    }
    Ok(files)
}

/// Writes a project into `dir`, refusing to overwrite an existing manifest.
pub fn create(dir: &Path, opts: &NewOptions) -> Result<Vec<PathBuf>> {
    let files = template(opts)?;
    if dir.join(MANIFEST_NAME).exists() {
        bail!("`{}` already contains a {MANIFEST_NAME}", dir.display());
    }
    std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let mut written = Vec::new();
    for f in files {
        let path = dir.join(&f.path);
        if path.exists() {
            // `kargo init` in an existing directory keeps the user's files.
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, f.contents).with_context(|| format!("failed to write {}", path.display()))?;
        written.push(path);
    }
    if opts.vcs && !inside_git_repo(dir) {
        if let Some(git) = kargo_util::process::find_on_path("git") {
            let _ = std::process::Command::new(git)
                .arg("init")
                .arg("--quiet")
                .current_dir(dir)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
    Ok(written)
}

/// Whether `dir` is inside an existing git work tree, so `kargo new` does not
/// create a nested repository. `dir` may be relative (`kargo new app` run from a
/// subdirectory of a repository), so the check walks the absolute path.
fn inside_git_repo(dir: &Path) -> bool {
    let abs = dir.canonicalize().or_else(|_| std::path::absolute(dir)).unwrap_or_else(|_| dir.to_path_buf());
    abs.ancestors().any(|d| d.join(".git").exists())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kargo_manifest::Manifest;

    fn opts(name: &str) -> NewOptions {
        NewOptions { vcs: false, ..NewOptions::new(name) }
    }

    #[test]
    fn default_project_has_valid_manifest() {
        let files = template(&opts("hello-world")).unwrap();
        let manifest = &files.iter().find(|f| f.path == Path::new(MANIFEST_NAME)).unwrap().contents;
        let m = Manifest::parse(manifest).unwrap();
        assert_eq!(m.package.name, "hello-world");
        assert_eq!(m.targets.enabled(), [TargetKind::Jvm]);
        assert!(m.tasks.is_empty());
        assert_eq!(m.kotlin.version, DEFAULT_KOTLIN_VERSION);
        assert_eq!(m.targets.jvm.as_ref().unwrap().jvm_target, kargo_manifest::DEFAULT_JVM_TARGET);
        for section in ["[kotlin]", "[target", "[dependencies]", "[dev-dependencies]", "[tasks]"] {
            assert!(!manifest.contains(section), "default manifest should not contain {section}:\n{manifest}");
        }
        let main = &files.iter().find(|f| f.path == Path::new("src/Main.kt")).unwrap().contents;
        assert!(main.contains("package com.example.hello_world"));
        assert!(main.contains("fun main()"));
    }

    #[test]
    fn every_combination_produces_valid_manifests() {
        let combos: &[(&[TargetKind], &[&str], bool)] = &[
            (&[TargetKind::Jvm, TargetKind::Wasm, TargetKind::Native], &["serialization", "coroutines"], false),
            (&[TargetKind::Jvm], &["compose"], false),
            (&[TargetKind::Jvm], &["ktor", "serialization"], false),
            (&[TargetKind::Jvm], &["spring-boot", "lombok"], false),
            (&[TargetKind::Android], &["compose"], false),
            (&[TargetKind::Jvm, TargetKind::Android], &[], true),
            (&[TargetKind::Native], &[], true),
        ];
        for (targets, frameworks, lib) in combos {
            let o = NewOptions {
                targets: targets.to_vec(),
                frameworks: frameworks.iter().map(|s| s.to_string()).collect(),
                lib: *lib,
                ..opts("demo")
            };
            let files = template(&o).unwrap_or_else(|e| panic!("{targets:?} {frameworks:?}: {e}"));
            let m = Manifest::parse(&files[0].contents).unwrap_or_else(|e| panic!("{targets:?} {frameworks:?}: {e:#}"));
            assert_eq!(m.targets.enabled(), *targets);
        }
    }

    #[test]
    fn serializable_class_gets_its_own_file() {
        for lib in [false, true] {
            let files = template(&NewOptions { frameworks: vec!["serialization".into()], lib, ..opts("ser") }).unwrap();
            let class = &files.iter().find(|f| f.path == Path::new("src/Greeting.kt")).unwrap().contents;
            assert!(class.contains("@Serializable\ndata class Greeting("), "{class}");
            let main = &files.iter().find(|f| f.path == Path::new("src/Main.kt")).unwrap().contents;
            assert!(!main.contains("class Greeting"), "{main}");
            assert_eq!(main.contains("Json.encodeToString(Greeting"), !lib, "{main}");
        }
        let ktor = template(&NewOptions { frameworks: vec!["ktor".into(), "serialization".into()], ..opts("k") }).unwrap();
        assert!(!ktor.iter().any(|f| f.path == Path::new("src/Greeting.kt")));
    }

    #[test]
    fn non_default_kotlin_version_is_written() {
        let o = NewOptions { kotlin_version: "2.1.0".into(), ..opts("demo") };
        let m = Manifest::parse(&template(&o).unwrap()[0].contents).unwrap();
        assert_eq!(m.kotlin.version, "2.1.0");
    }

    #[test]
    fn keyword_names_get_an_escaped_package() {
        assert_eq!(opts("class").package(), "com.example.class_");
        assert_eq!(opts("in").package(), "com.example.in_");
        assert_eq!(opts("my-app").package(), "com.example.my_app");
        assert_eq!(NewOptions { group: "org.package".into(), ..opts("fun") }.package(), "org.package_.fun_");
        let files = template(&opts("object")).unwrap();
        let main = &files.iter().find(|f| f.path == Path::new("src/Main.kt")).unwrap().contents;
        assert!(main.starts_with(
            "package com.example.object_
"
        ));
    }

    #[test]
    fn rejects_bad_options() {
        assert!(template(&opts("1bad")).is_err());
        assert!(template(&NewOptions { group: "com..x".into(), ..opts("a") }).is_err());
        assert!(template(&NewOptions { frameworks: vec!["rails".into()], ..opts("a") }).is_err());
        assert!(template(&NewOptions { frameworks: vec!["spring-boot".into()], targets: vec![TargetKind::Native], ..opts("a") }).is_err());
        assert!(template(&NewOptions { frameworks: vec!["compose".into()], targets: vec![TargetKind::Wasm], ..opts("a") }).is_err());
        assert!(template(&NewOptions { targets: vec![], ..opts("a") }).is_err());
    }

    #[test]
    fn android_app_gets_manifest_and_activity() {
        let o = NewOptions { targets: vec![TargetKind::Android], ..opts("droid") };
        let files = template(&o).unwrap();
        assert!(files.iter().any(|f| f.path == Path::new("android/AndroidManifest.xml")));
        assert!(files.iter().any(|f| f.path == Path::new("src-android/MainActivity.kt")));
        let main = &files.iter().find(|f| f.path == Path::new("src/Main.kt")).unwrap().contents;
        assert!(!main.contains("fun main()"), "android-only apps have no main()");
    }

    #[test]
    fn creates_files_and_refuses_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("app");
        let written = create(&project, &opts("app")).unwrap();
        assert!(written.iter().any(|p| p.ends_with("Main.kt")));
        assert!(project.join("tests/GreetingTest.kt").is_file());
        assert!(create(&project, &opts("app")).is_err());
    }

    #[test]
    fn init_keeps_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/Main.kt"), "fun main() = println(\"mine\")").unwrap();
        create(dir.path(), &opts("existing")).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("src/Main.kt")).unwrap(), "fun main() = println(\"mine\")");
        assert!(dir.path().join(MANIFEST_NAME).is_file());
    }
}
