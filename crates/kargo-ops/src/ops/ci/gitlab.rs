//! The GitLab CI writer of `kargo init-ci`.
//!
//! GitLab has no setup action, so the pipeline installs Kargo with `install.sh` or `install.ps1` and
//! caches the Kargo home inside the project directory, the only place GitLab caches from. The steps
//! are in the hidden job `.kargo`, the setup of each shell in `.unix` and `.windows`, and every job
//! extends two of them.

use super::{CHECKS, CiPlan, JDK_VERSION, JUNIT_REPORTS, NODE_VERSION, PACKAGE};

/// The image of the Linux job when the project has no Wasm or JS target: it has `curl` and `tar`.
const BASE_IMAGE: &str = "buildpack-deps:stable-curl";

/// The runner tags of GitLab.com's hosted macOS and Windows runners.
const MACOS_TAG: &str = "saas-macos-medium-m1";
const WINDOWS_TAG: &str = "saas-windows-medium-amd64";

/// The text of `.gitlab-ci.yml`, installing Kargo `version`.
pub(super) fn pipeline(plan: &CiPlan, version: &str) -> String {
    let mut y = String::from("variables:\n  KARGO_HOME: $CI_PROJECT_DIR/.kargo\n\n");
    y.push_str(".kargo:\n  cache:\n    key:\n      prefix: kargo-$CI_JOB_NAME\n      files:\n        - kargo.lock\n        - kargo.toml\n");
    y.push_str("    paths:\n      - .kargo/\n  script:\n");
    for step in CHECKS.iter().copied().chain([plan.test_command(), PACKAGE]) {
        y.push_str(&format!("    - kargo {step}\n"));
    }
    if plan.junit {
        y.push_str(&format!("  artifacts:\n    when: always\n    reports:\n      junit: {JUNIT_REPORTS}\n"));
    }

    y.push_str(&format!(
        "\n.unix:\n  before_script:\n    - curl -fsSL https://enekocm7.github.io/kargo/install.sh | KARGO_VERSION={version} sh -s -- --no-modify-path\n"
    ));
    y.push_str("    - export PATH=\"$KARGO_HOME/bin:$PATH\"\n");
    if plan.setup_java {
        y.push_str(&format!("    - kargo toolchain install jdk {JDK_VERSION}\n"));
    }
    y.push_str("  after_script:\n    - rm -rf \"$KARGO_HOME/daemon\"\n");

    let image = if plan.setup_node { format!("node:{NODE_VERSION}") } else { BASE_IMAGE.to_owned() };
    if !plan.os_matrix {
        y.push_str(&format!("\nci:\n  extends: [.kargo, .unix]\n  image: {image}\n"));
        return y;
    }

    y.push_str(&format!(
        "\n.windows:\n  before_script:\n    - $env:KARGO_VERSION = '{version}'; $env:KARGO_NO_MODIFY_PATH = '1'; irm https://enekocm7.github.io/kargo/install.ps1 | iex\n"
    ));
    y.push_str("    - $env:PATH = \"$env:KARGO_HOME\\bin;$env:PATH\"\n");
    if plan.setup_java {
        y.push_str(&format!("    - kargo toolchain install jdk {JDK_VERSION}\n"));
    }
    y.push_str("  after_script:\n    - Remove-Item -Recurse -Force -ErrorAction SilentlyContinue \"$env:KARGO_HOME\\daemon\"\n");

    y.push_str(&format!("\nlinux:\n  extends: [.kargo, .unix]\n  image: {image}\n"));
    y.push_str(&format!("\nmacos:\n  extends: [.kargo, .unix]\n  tags: [{MACOS_TAG}]\n"));
    y.push_str(&format!("\nwindows:\n  extends: [.kargo, .windows]\n  tags: [{WINDOWS_TAG}]\n"));
    y
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jvm_pipeline_is_one_linux_job() {
        let y = pipeline(&CiPlan { setup_java: true, ..CiPlan::default() }, "1.2.3");
        assert_eq!(
            y,
            "\
variables:
  KARGO_HOME: $CI_PROJECT_DIR/.kargo

.kargo:
  cache:
    key:
      prefix: kargo-$CI_JOB_NAME
      files:
        - kargo.lock
        - kargo.toml
    paths:
      - .kargo/
  script:
    - kargo fmt --check
    - kargo lint
    - kargo check --tests --locked
    - kargo build --locked
    - kargo test --locked
    - kargo package --release --locked

.unix:
  before_script:
    - curl -fsSL https://enekocm7.github.io/kargo/install.sh | KARGO_VERSION=1.2.3 sh -s -- --no-modify-path
    - export PATH=\"$KARGO_HOME/bin:$PATH\"
    - kargo toolchain install jdk 21
  after_script:
    - rm -rf \"$KARGO_HOME/daemon\"

ci:
  extends: [.kargo, .unix]
  image: buildpack-deps:stable-curl
"
        );
    }

    #[test]
    fn a_pinned_jdk_needs_no_install_and_junit_is_a_report() {
        let y = pipeline(&CiPlan { junit: true, ..CiPlan::default() }, "1.2.3");
        assert!(!y.contains("toolchain install"));
        assert!(y.contains("    - kargo test --locked --report junit\n"));
        assert!(y.contains("  artifacts:\n    when: always\n    reports:\n      junit: target/*/*/test-reports*/*.xml\n"));
    }

    #[test]
    fn every_pipeline_is_yaml_with_the_jobs_it_extends() {
        for plan in [CiPlan::default(), CiPlan { setup_java: true, setup_node: true, os_matrix: true, junit: true }] {
            let doc: serde_yaml_ng::Value = serde_yaml_ng::from_str(&pipeline(&plan, "1.2.3")).unwrap();
            let jobs = doc.as_mapping().unwrap();
            for (name, job) in jobs {
                for parent in job.get("extends").and_then(|e| e.as_sequence()).into_iter().flatten() {
                    assert!(jobs.contains_key(parent), "{name:?} extends a missing {parent:?}");
                }
            }
        }
    }

    #[test]
    fn wasm_runs_on_the_node_image() {
        let y = pipeline(&CiPlan { setup_node: true, ..CiPlan::default() }, "1.2.3");
        assert!(y.contains("  image: node:22\n") && !y.contains("buildpack-deps"));
    }

    #[test]
    fn native_has_a_job_per_os() {
        let y = pipeline(&CiPlan { setup_java: true, os_matrix: true, ..CiPlan::default() }, "1.2.3");
        assert!(y.contains(
            "
linux:
  extends: [.kargo, .unix]
  image: "
        ));
        assert!(y.contains(
            "
macos:
  extends: [.kargo, .unix]
  tags: [saas-macos-medium-m1]
"
        ));
        assert!(y.ends_with(
            "
windows:
  extends: [.kargo, .windows]
  tags: [saas-windows-medium-amd64]
"
        ));
        assert!(!y.contains(
            "
ci:"
        ));
        let windows = y.split(".windows:").nth(1).unwrap().split("\n\n").next().unwrap();
        assert!(
            windows.contains("install.ps1")
                && windows.contains("KARGO_VERSION = '1.2.3'")
                && windows.contains("kargo toolchain install jdk 21")
        );
    }
}
