//! `kargo init-ci`: writes a CI workflow for the project, with the steps of the CI guide and the
//! setup that `kargo.toml` implies (a JDK unless `[toolchain] jdk` is set, Node for Wasm, an OS
//! matrix for Kotlin/Native). [`CiPlan`] holds what the project needs; each CI system has a writer
//! module that turns it into text.

mod github;
mod gitlab;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::workspace::Project;
use kargo_manifest::{JdkRequest, TargetKind};

/// The JDK major version a workflow installs when a package does not pin one.
const JDK_VERSION: &str = "21";

/// The Node.js major version a workflow installs for Wasm and JS targets.
const NODE_VERSION: &str = "22";

/// The `kargo` commands that run before the tests.
const CHECKS: [&str; 4] = ["fmt --check", "lint", "check --tests --locked", "build --locked"];

/// The `kargo` command that runs last.
const PACKAGE: &str = "package --release --locked";

/// Where `kargo test --report junit` writes the reports, as a glob from the project root.
const JUNIT_REPORTS: &str = "target/*/*/test-reports*/*.xml";

/// The CI systems `kargo init-ci` can write a workflow for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiSystem {
    Github,
    Gitlab,
}

impl CiSystem {
    /// Where the workflow goes, relative to the project root.
    pub fn path(self) -> &'static str {
        match self {
            CiSystem::Github => ".github/workflows/ci.yml",
            CiSystem::Gitlab => ".gitlab-ci.yml",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CiOptions {
    pub system: CiSystem,
    /// Replace an existing workflow.
    pub force: bool,
    /// Publish the JUnit reports of the tests in the CI's test summary.
    pub junit: bool,
}

/// What `init_ci` wrote.
#[derive(Debug, Clone)]
pub struct CiReport {
    pub path: PathBuf,
    /// Steps the project does not need, each with the reason.
    pub left_out: Vec<String>,
}

/// What the workflow depends on, read from the manifests of the project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CiPlan {
    /// Some package does not pin a JDK in `[toolchain]`.
    pub setup_java: bool,
    pub setup_node: bool,
    pub os_matrix: bool,
    pub junit: bool,
}

impl CiPlan {
    pub fn from_project(project: &Project, junit: bool) -> Self {
        let mut plan = CiPlan { junit, ..CiPlan::default() };
        for package in project.packages() {
            let manifest = package.manifest();
            plan.setup_java |= manifest.toolchain.jdk == JdkRequest::System;
            plan.setup_node |= manifest.targets.is_enabled(TargetKind::Wasm) || manifest.targets.is_enabled(TargetKind::Js);
            plan.os_matrix |= manifest.targets.is_enabled(TargetKind::Native);
        }
        plan
    }

    /// The steps the plan leaves out of the workflow of `system`, with the reason.
    pub fn left_out(&self, system: CiSystem) -> Vec<String> {
        let (java, node, single_job) = match system {
            CiSystem::Github => ("actions/setup-java", "actions/setup-node", "ubuntu-latest"),
            CiSystem::Gitlab => ("`kargo toolchain install jdk`", "the Node.js image", "a Linux runner"),
        };
        let mut out = Vec::new();
        if !self.setup_java {
            out.push(format!("{java}: every package pins a JDK with `[toolchain] jdk`"));
        }
        if !self.setup_node {
            out.push(format!("{node}: no Wasm or JS target"));
        }
        if !self.os_matrix {
            out.push(format!("OS matrix: no Kotlin/Native target (the job runs on {single_job})"));
        }
        if !self.junit {
            out.push("JUnit report: pass `--junit` to add it".to_owned());
        }
        out
    }

    /// The `kargo` command that runs the tests.
    fn test_command(&self) -> &'static str {
        if self.junit { "test --locked --report junit" } else { "test --locked" }
    }
}

/// Writes the workflow for the project.
pub fn init_ci(project: &Project, opts: &CiOptions) -> Result<CiReport> {
    let plan = CiPlan::from_project(project, opts.junit);
    let path: PathBuf = Path::new(project.root()).join(opts.system.path());
    if path.exists() && !opts.force {
        bail!("`{}` already exists; pass `--force` to overwrite it", path.display());
    }
    let version = env!("CARGO_PKG_VERSION");
    let text = match opts.system {
        CiSystem::Github => github::workflow(&plan, version),
        CiSystem::Gitlab => gitlab::pipeline(&plan, version),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    }
    std::fs::write(&path, text).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(CiReport { path, left_out: plan.left_out(opts.system) })
}
