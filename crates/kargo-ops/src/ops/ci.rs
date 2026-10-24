//! `kargo init-ci`: writes a CI workflow for the project, with the steps of the CI guide and the
//! setup that `kargo.toml` implies (a JDK unless `[toolchain] jdk` is set, Node for Wasm, an OS
//! matrix for Kotlin/Native).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::workspace::Project;
use kargo_manifest::{JdkRequest, TargetKind};

/// The setup action of this repository.
const ACTION: &str = "enekocm7/kargo/setup-kargo@master";

/// The CI systems `kargo init-ci` can write a workflow for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiSystem {
    Github,
}

impl CiSystem {
    /// Where the workflow goes, relative to the project root.
    pub fn path(self) -> &'static str {
        match self {
            CiSystem::Github => ".github/workflows/ci.yml",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CiOptions {
    pub system: CiSystem,
    /// Replace an existing workflow.
    pub force: bool,
    /// Publish the JUnit reports of the tests in the run summary.
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

    /// The steps the plan leaves out, with the reason.
    pub fn left_out(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.setup_java {
            out.push("actions/setup-java: every package pins a JDK with `[toolchain] jdk`".to_owned());
        }
        if !self.setup_node {
            out.push("actions/setup-node: no Wasm or JS target".to_owned());
        }
        if !self.os_matrix {
            out.push("OS matrix: no Kotlin/Native target (the job runs on ubuntu-latest)".to_owned());
        }
        if !self.junit {
            out.push("JUnit report: pass `--junit` to add it".to_owned());
        }
        out
    }

    /// The text of the GitHub Actions workflow, installing Kargo `version`.
    pub fn github_workflow(&self, version: &str) -> String {
        let mut y = String::from("name: CI\n\non:\n  push:\n  pull_request:\n\njobs:\n  ci:\n");
        if self.os_matrix {
            y.push_str("    strategy:\n      fail-fast: false\n      matrix:\n        os: [ubuntu-latest, macos-latest, windows-latest]\n");
            y.push_str("    runs-on: ${{ matrix.os }}\n");
        } else {
            y.push_str("    runs-on: ubuntu-latest\n");
        }
        y.push_str("    steps:\n      - uses: actions/checkout@v4\n");
        if self.setup_java {
            y.push_str("      - uses: actions/setup-java@v4\n        with:\n          distribution: temurin\n          java-version: 21\n");
        }
        if self.setup_node {
            y.push_str("      - uses: actions/setup-node@v4\n        with:\n          node-version: 22\n");
        }
        y.push_str(&format!("      - uses: {ACTION}\n        with:\n          version: {version}\n"));
        for step in ["fmt --check", "lint", "check --tests --locked", "build --locked"] {
            y.push_str(&format!("      - run: kargo {step}\n"));
        }
        if self.junit {
            y.push_str("      - run: kargo test --locked --report junit\n");
            y.push_str("      - uses: mikepenz/action-junit-report@v5\n        if: always()\n");
            y.push_str("        with:\n          report_paths: target/*/*/test-reports/*.xml\n");
        } else {
            y.push_str("      - run: kargo test --locked\n");
        }
        y.push_str("      - run: kargo package --release --locked\n");
        y
    }
}

/// Writes the workflow for the project.
pub fn init_ci(project: &Project, opts: &CiOptions) -> Result<CiReport> {
    let plan = CiPlan::from_project(project, opts.junit);
    let path: PathBuf = Path::new(project.root()).join(opts.system.path());
    if path.exists() && !opts.force {
        bail!("`{}` already exists; pass `--force` to overwrite it", path.display());
    }
    let text = match opts.system {
        CiSystem::Github => plan.github_workflow(env!("CARGO_PKG_VERSION")),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    }
    std::fs::write(&path, text).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(CiReport { path, left_out: plan.left_out() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jvm_plan_has_java_and_no_matrix() {
        let plan = CiPlan { setup_java: true, ..CiPlan::default() };
        let y = plan.github_workflow("1.2.3");
        assert!(y.contains("setup-java") && !y.contains("setup-node") && !y.contains("matrix"));
        assert!(y.contains("version: 1.2.3"));
    }

    #[test]
    fn native_and_wasm_add_matrix_and_node() {
        let plan = CiPlan { setup_node: true, os_matrix: true, junit: true, ..CiPlan::default() };
        let y = plan.github_workflow("1.2.3");
        assert!(y.contains("setup-node") && y.contains("matrix.os") && y.contains("--report junit"));
        assert!(!y.contains("setup-java"));
    }
}
