//! The Kotlin compiler, fetched from Maven Central as `kotlin-compiler-embeddable`
//! and run on the detected JDK.

use std::path::PathBuf;

use anyhow::{Context, Result};

use kargo_maven::{Artifact, DependencyRequest, MavenClient, ModuleId, Platform, Resolver};

pub const KOTLIN_GROUP: &str = "org.jetbrains.kotlin";
pub const JVM_COMPILER_MAIN: &str = "org.jetbrains.kotlin.cli.jvm.K2JVMCompiler";
pub const JS_COMPILER_MAIN: &str = "org.jetbrains.kotlin.cli.js.K2JSCompiler";

#[derive(Debug, Clone)]
pub struct KotlinCompiler {
    pub version: String,
    pub classpath: Vec<PathBuf>,
}

impl KotlinCompiler {
    pub fn resolve(client: &MavenClient, version: &str) -> Result<Self> {
        let resolver = Resolver::new(client, Platform::Jvm);
        let root = DependencyRequest::new(ModuleId::new(KOTLIN_GROUP, "kotlin-compiler-embeddable"), Some(version.to_owned()));
        let resolution = resolver.resolve(&[root]).with_context(|| format!("failed to obtain the Kotlin {version} compiler"))?;
        Ok(Self { version: version.to_owned(), classpath: resolution.files() })
    }
}

/// Fetches a single jar (no transitive dependencies), e.g. a compiler plugin.
pub fn fetch_jar(client: &MavenClient, group: &str, artifact: &str, version: &str) -> Result<PathBuf> {
    let artifact = Artifact::new(ModuleId::new(group, artifact), version);
    client.fetch_required(&artifact.repo_path())
}

/// Fetches a Kotlin compiler plugin matching the compiler version.
pub fn fetch_kotlin_plugin(client: &MavenClient, artifact: &str, kotlin_version: &str) -> Result<PathBuf> {
    fetch_jar(client, KOTLIN_GROUP, artifact, kotlin_version)
        .with_context(|| format!("failed to fetch Kotlin compiler plugin `{artifact}` {kotlin_version}"))
}
