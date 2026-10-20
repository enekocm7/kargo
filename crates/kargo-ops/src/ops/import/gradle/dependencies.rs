//! The `dependencies {}` blocks of a Gradle build: configurations and the notations they take.

use std::collections::HashMap;

use kargo_manifest::TargetKind;

use super::catalog::Catalog;
use super::dsl::{Arg, Scope as Vars, Stmt, Tok, matching, normalize_alias};
use super::sources::{self, SetKind};
use crate::ops::import::model::{Dependency, Scope};

/// Where a dependency applies: to the main or to the test code, of every target or of one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Placement {
    pub target: Option<TargetKind>,
    pub test: bool,
}

/// What a build knows that dependency notations refer to.
pub struct Context<'a> {
    pub vars: &'a Vars,
    pub catalogs: &'a [Catalog],
    pub kotlin_version: Option<&'a str>,
    /// The names of the JVM targets of a multiplatform project (`jvm("desktop")` is `desktop`).
    pub jvm_targets: &'a [String],
    /// Gradle project paths (`:core`, `:libs:util`) to their directories.
    pub projects: &'a HashMap<String, String>,
}

/// What a dependency notation names.
#[derive(Debug)]
pub enum Notation {
    Libraries(Vec<Dependency>),
    /// A project of the build, by its Gradle path.
    Project(String),
    /// `compose.runtime`, `compose.material3`, ...: what the Compose plugin provides.
    Compose,
    Npm(String, String),
    /// A notation Kargo has nothing for, with what to tell the user.
    Unsupported(String),
}

/// What a configuration name means.
#[derive(Debug, PartialEq)]
pub enum Configuration {
    Dependency {
        placement: Option<Placement>,
        scope: Scope,
        exported: bool,
    },
    /// `kapt`, which Kargo does not run.
    Kapt,
    /// A configuration without an equivalent (`androidTestImplementation`), with the reason.
    Unsupported(String),
}

const BASES: &[&str] = &[
    "implementation",
    "api",
    "compileOnly",
    "compileOnlyApi",
    "runtimeOnly",
    "annotationProcessor",
    "ksp",
    "kapt",
    "testFixturesImplementation",
];

/// The meaning of the configuration `name` in a `dependencies {}` block, `None` when it is not one.
pub fn configuration(name: &str, jvm_targets: &[String]) -> Option<Configuration> {
    let (prefix, base) = BASES.iter().find_map(|base| {
        if name == *base {
            return Some(("", *base));
        }
        let capital = format!("{}{}", base[..1].to_ascii_uppercase(), &base[1..]);
        name.strip_suffix(&capital).filter(|p| !p.is_empty()).map(|p| (p, *base))
    })?;
    if base == "kapt" {
        return Some(Configuration::Kapt);
    }
    if base == "testFixturesImplementation" || prefix == "testFixtures" {
        return Some(Configuration::Unsupported("test fixtures".to_owned()));
    }
    let (scope, exported) = match base {
        "api" => (Scope::Compile, true),
        "compileOnly" | "compileOnlyApi" => (Scope::Provided, false),
        "runtimeOnly" => (Scope::Runtime, false),
        "annotationProcessor" => (Scope::Processor, false),
        "ksp" => (Scope::Ksp, false),
        _ => (Scope::Compile, false),
    };
    let placement = match prefix {
        "" => None,
        "test" => Some(Placement { target: None, test: true }),
        "debug" | "release" => return Some(Configuration::Unsupported(format!("the build type configuration `{name}`"))),
        set => match sources::classify(set, jvm_targets) {
            Some(info) if info.kind == SetKind::Supported => Some(Placement { target: info.target, test: info.test }),
            _ => return Some(Configuration::Unsupported(format!("the configuration `{name}`"))),
        },
    };
    Some(Configuration::Dependency { placement, scope, exported })
}

/// The `ksp` family: `kspJvm`, `kspTest` and the like, which are not in [`BASES`] with a suffix.
pub fn ksp_configuration(name: &str, jvm_targets: &[String]) -> Option<Configuration> {
    let rest = name.strip_prefix("ksp").filter(|r| !r.is_empty() && r.chars().next().is_some_and(char::is_uppercase))?;
    let placement = if rest == "Test" {
        Placement { target: None, test: true }
    } else {
        let set = format!("{}Main", lower_first(rest));
        let info = sources::classify(&set, jvm_targets).filter(|i| i.kind == SetKind::Supported)?;
        Placement { target: info.target, test: false }
    };
    Some(Configuration::Dependency { placement: Some(placement), scope: Scope::Ksp, exported: false })
}

fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|c| c.to_ascii_lowercase().to_string() + chars.as_str()).unwrap_or_default()
}

/// The notation `tokens` spell, with the trailing `{ ... }` block of the declaration.
pub fn notation(ctx: &Context, args: &[Arg], block: Option<&[Stmt]>) -> Notation {
    let named = |name: &str| args.iter().find(|a| a.name.as_deref() == Some(name));
    if let (Some(group), Some(artifact)) = (named("group"), named("name")) {
        let text = |a: &Arg| ctx.vars.eval(&a.value);
        let (Some(group), Some(artifact)) = (text(group), text(artifact)) else {
            return Notation::Unsupported("a dependency whose group or name is not a plain string".to_owned());
        };
        let version = named("version").and_then(text);
        let mut dep = Dependency::new(group, artifact, version);
        dep.classifier = named("classifier").and_then(text);
        dep.extension = named("ext").and_then(text);
        return finish(dep, block);
    }
    if let Some(module) = named("module").and_then(|a| ctx.vars.eval(&a.value)) {
        return from_coordinates(ctx, &module, block);
    }
    let Some(first) = args.first() else { return Notation::Unsupported("an empty dependency declaration".to_owned()) };
    from_tokens(ctx, &first.value, block)
}

fn from_tokens(ctx: &Context, tokens: &[Tok], block: Option<&[Stmt]>) -> Notation {
    match tokens {
        [Tok::Str(text)] => match ctx.vars.template(text) {
            Some(text) => from_coordinates(ctx, &text, block),
            None => {
                // A version variable that is not known: keep the module and drop the version.
                let without_version = text.split(':').take(2).collect::<Vec<_>>().join(":");
                match ctx.vars.template(&without_version) {
                    Some(module) if module.contains(':') => {
                        let mut notation = from_coordinates(ctx, &module, block);
                        if let Notation::Libraries(deps) = &mut notation {
                            deps.iter_mut().for_each(|d| d.version = None);
                        }
                        notation
                    }
                    _ => Notation::Unsupported(format!("`{text}`, which uses a variable Kargo cannot read")),
                }
            }
        },
        [Tok::Ident(function), Tok::Punct('('), inner @ .., Tok::Punct(')')] => call(ctx, function, inner, block),
        [Tok::Ident(first), Tok::Punct('.'), ..] => path(ctx, tokens, first, block),
        [Tok::Ident(name)] if ctx.vars.vars.contains_key(name) => match ctx.vars.eval(tokens) {
            Some(text) => from_coordinates(ctx, &text, block),
            None => Notation::Unsupported(format!("`{name}`")),
        },
        _ => Notation::Unsupported(format!("`{}`", render(tokens))),
    }
}

fn call(ctx: &Context, function: &str, inner: &[Tok], block: Option<&[Stmt]>) -> Notation {
    let args = split(inner);
    match function {
        "platform" | "enforcedPlatform" => match args.first().map(|a| from_tokens(ctx, &a.value, None)) {
            Some(Notation::Libraries(mut deps)) => {
                deps.iter_mut().for_each(|d| d.bom = true);
                Notation::Libraries(deps)
            }
            Some(other) => other,
            None => Notation::Unsupported("an empty platform()".to_owned()),
        },
        "project" => {
            let path = args.iter().find(|a| a.name.as_deref().is_none_or(|n| n == "path")).and_then(|a| ctx.vars.eval(&a.value));
            match path {
                Some(path) => Notation::Project(path),
                None => Notation::Unsupported("a project() whose path is not a plain string".to_owned()),
            }
        }
        "kotlin" => match args.first().and_then(|a| a.string()) {
            Some(module) => {
                let mut dep = Dependency::new("org.jetbrains.kotlin", format!("kotlin-{module}"), ctx.kotlin_version.map(str::to_owned));
                dep.exclude.clear();
                finish(dep, block)
            }
            None => Notation::Unsupported("a kotlin() module that is not a plain string".to_owned()),
        },
        "npm" => match (args.first().and_then(|a| ctx.vars.eval(&a.value)), args.get(1).and_then(|a| ctx.vars.eval(&a.value))) {
            (Some(name), Some(version)) => Notation::Npm(name, version),
            _ => Notation::Unsupported("an npm() package whose name or version is not a plain string".to_owned()),
        },
        "files" | "fileTree" => {
            Notation::Unsupported("`files(...)` and `fileTree(...)`: put the jars in a Maven repository or `maven-local`".to_owned())
        }
        "gradleApi" | "localGroovy" | "gradleTestKit" => Notation::Unsupported(format!("`{function}()`, which only Gradle plugins use")),
        other => Notation::Unsupported(format!("`{other}(...)`")),
    }
}

/// `libs.ktor.server.core`, `libs.bundles.ktor`, `compose.runtime`, `projects.core.util`.
fn path(ctx: &Context, tokens: &[Tok], first: &str, block: Option<&[Stmt]>) -> Notation {
    let mut segments: Vec<String> = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        match &tokens[i] {
            Tok::Ident(name) => {
                segments.push(name.clone());
                i += 1;
                if matches!(tokens.get(i), Some(Tok::Punct('('))) {
                    i = matching(tokens, i).map_or(tokens.len(), |close| close + 1);
                    if matches!(segments.last().map(String::as_str), Some("get" | "asProvider" | "toString")) {
                        segments.pop();
                    }
                }
            }
            Tok::Punct('.') => i += 1,
            _ => return Notation::Unsupported(format!("`{}`", render(tokens))),
        }
    }
    if first == "compose" {
        return Notation::Compose;
    }
    if first == "projects" {
        let wanted = normalize_alias(&segments[1..].join("."));
        let found =
            ctx.projects.keys().find(|path| normalize_alias(&path.trim_start_matches(':').replace(':', ".")).eq_ignore_ascii_case(&wanted));
        return match found {
            Some(path) => Notation::Project(path.clone()),
            None => {
                Notation::Unsupported(format!("`{}`, a project accessor Kargo could not match to an included project", segments.join(".")))
            }
        };
    }
    let Some(catalog) = ctx.catalogs.iter().find(|c| c.name == first) else {
        return Notation::Unsupported(format!("`{}`", segments.join(".")));
    };
    let alias = normalize_alias(&segments[1..].join("."));
    if let Some(bundle) = alias.strip_prefix("bundles.") {
        let members: Vec<Dependency> = catalog
            .bundles
            .get(bundle)
            .into_iter()
            .flatten()
            .filter_map(|member| catalog.libraries.get(member))
            .map(|lib| Dependency::new(&lib.group, &lib.artifact, lib.version.clone()))
            .collect();
        return if members.is_empty() {
            Notation::Unsupported(format!("the bundle `{}`", segments.join(".")))
        } else {
            Notation::Libraries(members)
        };
    }
    match catalog.libraries.get(&alias) {
        Some(lib) => finish(Dependency::new(&lib.group, &lib.artifact, lib.version.clone()), block),
        None => Notation::Unsupported(format!("`{}`, which is not in the version catalog `{}`", segments.join("."), catalog.name)),
    }
}

/// `group:artifact[:version[:classifier]][@extension]`.
fn from_coordinates(ctx: &Context, text: &str, block: Option<&[Stmt]>) -> Notation {
    let _ = ctx;
    let (coordinates, extension) = match text.rsplit_once('@') {
        Some((c, e)) => (c, Some(e.to_owned())),
        None => (text, None),
    };
    let parts: Vec<&str> = coordinates.split(':').collect();
    if parts.len() < 2 || parts.len() > 4 || parts.iter().take(2).any(|p| p.is_empty()) {
        return Notation::Unsupported(format!("`{text}`, which is not group:artifact:version"));
    }
    let version = parts.get(2).filter(|v| !v.is_empty()).map(|v| (*v).to_owned());
    let mut dep = Dependency::new(parts[0], parts[1], version);
    dep.classifier = parts.get(3).map(|c| (*c).to_owned());
    dep.extension = extension.filter(|e| e != "jar");
    finish(dep, block)
}

/// Applies the `{ exclude(...) }` block of a declaration.
fn finish(mut dep: Dependency, block: Option<&[Stmt]>) -> Notation {
    for stmt in block.unwrap_or_default() {
        match stmt.name().as_deref() {
            Some("exclude") => {
                let args = stmt.args();
                let named = |name: &str| args.iter().find(|a| a.name.as_deref() == Some(name)).and_then(|a| a.string());
                let (group, module) = (named("group"), named("module"));
                let positional: Vec<&str> = args.iter().filter(|a| a.name.is_none()).filter_map(|a| a.string()).collect();
                match (group, module, positional.as_slice()) {
                    (Some(g), Some(m), _) => dep.exclude.push(format!("{g}:{m}")),
                    (Some(g), None, _) => dep.exclude.push(format!("{g}:*")),
                    (None, Some(m), _) => dep.exclude.push(format!("*:{m}")),
                    (None, None, [g, m]) => dep.exclude.push(format!("{g}:{m}")),
                    _ => {}
                }
            }
            Some("version") => {
                // `version { strictly("1.0") }`
                if let Some(version) = stmt.block.iter().flatten().find_map(|s| match s.name().as_deref() {
                    Some("strictly" | "require" | "prefer") => s.args().first().and_then(|a| a.string().map(str::to_owned)),
                    _ => None,
                }) {
                    dep.version = Some(version);
                }
            }
            _ => {}
        }
        let transitive_off = matches!(stmt.name().as_deref(), Some("isTransitive" | "transitive"))
            && stmt.assigned().is_some_and(|t| matches!(t, [Tok::Ident(v)] if v == "false"));
        if transitive_off {
            dep.non_transitive = true;
        }
    }
    dep.exclude.retain(|e| !e.starts_with("*:"));
    Notation::Libraries(vec![dep])
}

fn split(tokens: &[Tok]) -> Vec<Arg> {
    // A call's argument list is the same shape as a statement's.
    let mut wrapped = vec![Tok::Ident("f".to_owned()), Tok::Punct('(')];
    wrapped.extend(tokens.iter().cloned());
    wrapped.push(Tok::Punct(')'));
    Stmt { tokens: wrapped, block: None }.args()
}

fn render(tokens: &[Tok]) -> String {
    tokens
        .iter()
        .map(|t| match t {
            Tok::Ident(s) | Tok::Num(s) => s.clone(),
            Tok::Str(s) => format!("\"{s}\""),
            Tok::Punct(c) => c.to_string(),
            Tok::Newline => " ".to_owned(),
        })
        .collect::<Vec<_>>()
        .join("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::import::gradle::dsl::parse;

    fn deps(src: &str, catalog: Option<&str>) -> Vec<(String, Notation)> {
        let mut vars = Vars::default();
        vars.vars.insert("ktor_version".into(), "3.0.0".into());
        let catalogs: Vec<Catalog> = catalog.map(|c| Catalog::parse("libs", c).unwrap()).into_iter().collect();
        let projects = HashMap::from([(":core".to_owned(), "core".to_owned()), (":libs:util".to_owned(), "libs/util".to_owned())]);
        let ctx = Context { vars: &vars, catalogs: &catalogs, kotlin_version: Some("2.0.20"), jvm_targets: &[], projects: &projects };
        parse(src).iter().map(|s| (s.name().unwrap(), notation(&ctx, &s.args(), s.block.as_deref()))).collect()
    }

    #[test]
    fn configurations() {
        let c = |name: &str| configuration(name, &[]);
        assert_eq!(c("implementation"), Some(Configuration::Dependency { placement: None, scope: Scope::Compile, exported: false }));
        assert_eq!(c("api"), Some(Configuration::Dependency { placement: None, scope: Scope::Compile, exported: true }));
        assert_eq!(c("runtimeOnly"), Some(Configuration::Dependency { placement: None, scope: Scope::Runtime, exported: false }));
        assert_eq!(
            c("testImplementation"),
            Some(Configuration::Dependency {
                placement: Some(Placement { target: None, test: true }),
                scope: Scope::Compile,
                exported: false
            })
        );
        assert_eq!(
            c("jvmMainImplementation"),
            Some(Configuration::Dependency {
                placement: Some(Placement { target: Some(TargetKind::Jvm), test: false }),
                scope: Scope::Compile,
                exported: false
            })
        );
        assert_eq!(c("kapt"), Some(Configuration::Kapt));
        assert!(matches!(c("androidTestImplementation"), Some(Configuration::Unsupported(_))));
        assert_eq!(c("println"), None);
        assert!(matches!(ksp_configuration("kspJvm", &[]), Some(Configuration::Dependency { scope: Scope::Ksp, .. })));
    }

    #[test]
    fn notations() {
        let found = deps(
            r#"
implementation("io.ktor:ktor-server-core-jvm:$ktor_version")
implementation("com.example:no-version")
implementation(platform("org.springframework.boot:spring-boot-dependencies:3.3.1"))
implementation(project(":libs:util"))
implementation(kotlin("test"))
implementation(group = "g", name = "a", version = "2")
implementation("g:a:1.0:natives@zip")
implementation("g:a:1.0") { exclude(group = "x", module = "y") }
implementation(compose.runtime)
implementation(files("libs/a.jar"))
implementation("io.ktor:ktor-client:$unknown")
"#,
            None,
        );
        let library = |n: &Notation| match n {
            Notation::Libraries(d) => d[0].clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(library(&found[0].1).version.as_deref(), Some("3.0.0"));
        assert_eq!(library(&found[1].1).version, None);
        assert!(library(&found[2].1).bom);
        assert!(matches!(&found[3].1, Notation::Project(p) if p == ":libs:util"));
        assert_eq!(library(&found[4].1).coords(), "org.jetbrains.kotlin:kotlin-test");
        assert_eq!(library(&found[4].1).version.as_deref(), Some("2.0.20"));
        assert_eq!(library(&found[5].1).coords(), "g:a");
        let natives = library(&found[6].1);
        assert_eq!((natives.classifier.as_deref(), natives.extension.as_deref()), (Some("natives"), Some("zip")));
        assert_eq!(library(&found[7].1).exclude, ["x:y"]);
        assert!(matches!(found[8].1, Notation::Compose));
        assert!(matches!(found[9].1, Notation::Unsupported(_)));
        let unknown = library(&found[10].1);
        assert_eq!((unknown.coords().as_str(), unknown.version.as_deref()), ("io.ktor:ktor-client", None));
    }

    #[test]
    fn catalog_accessors_and_bundles() {
        let catalog = r#"
[versions]
ktor = "3.0.0"
[libraries]
ktor-server-core = { module = "io.ktor:ktor-server-core", version.ref = "ktor" }
logback = "ch.qos.logback:logback-classic:1.5.6"
[bundles]
server = ["ktor-server-core", "logback"]
"#;
        let found = deps(
            "implementation(libs.ktor.server.core)\nimplementation(libs.bundles.server)\nimplementation(libs.missing)\nimplementation(libs.logback.get())\n",
            Some(catalog),
        );
        assert!(matches!(&found[0].1, Notation::Libraries(d) if d[0].version.as_deref() == Some("3.0.0")));
        assert!(matches!(&found[1].1, Notation::Libraries(d) if d.len() == 2));
        assert!(matches!(&found[2].1, Notation::Unsupported(_)));
        assert!(matches!(&found[3].1, Notation::Libraries(d) if d[0].artifact == "logback-classic"));
    }
}
