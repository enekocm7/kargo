//! The GitHub Actions writer of `kargo init-ci`.

use super::{CHECKS, CiPlan, JDK_VERSION, JUNIT_REPORTS, NODE_VERSION, PACKAGE};

/// The setup action of this repository.
const ACTION: &str = "enekocm7/kargo/setup-kargo@master";

/// The `--updates` workflow; `{version}` is the Kargo version and `{packages}` the packages to upgrade.
const UPDATES_WORKFLOW: &str = r#"name: Dependency updates

on:
  schedule:
    - cron: "0 6 * * 1"
  workflow_dispatch:

permissions:
  contents: write
  pull-requests: write

jobs:
  update:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: {action}
        with:
          version: {version}
      - name: Upgrade the dependencies and the lockfile
        run: |
          mkdir -p "$RUNNER_TEMP/kargo"
          for package in {packages}; do
            kargo upgrade -p "$package" --format json > "$RUNNER_TEMP/kargo/upgraded-$package.json"
          done
          kargo update
          for package in {packages}; do
            kargo outdated -p "$package" --format json > "$RUNNER_TEMP/kargo/held-back-$package.json"
          done
      - name: Describe the update
        run: |
          {
            echo 'Raised by `kargo upgrade` to the newest compatible versions, with `kargo.lock` refreshed by `kargo update`.'
            echo
            echo '| Package | Dependency | From | To |'
            echo '| --- | --- | --- | --- |'
            jq -rs '.[] | .package.name as $p | .upgraded[] | "| \($p) | `\(.name)` | \(.from) | \(.to) |"' "$RUNNER_TEMP"/kargo/upgraded-*.json
          } > "$RUNNER_TEMP/kargo/body.md"
          held=$(jq -rs '.[] | .package.name as $p | .outdated[] | "| \($p) | `\(.name)` | \(.current) | \(.latest) |"' "$RUNNER_TEMP"/kargo/held-back-*.json)
          if [ -n "$held" ]; then
            {
              echo
              echo 'Newer versions that `kargo upgrade` left alone:'
              echo
              echo '| Package | Dependency | Current | Latest |'
              echo '| --- | --- | --- | --- |'
              echo "$held"
              echo
              echo 'An incompatible one is raised with `kargo upgrade --incompatible <name>`; one inherited with `workspace = true`, in the workspace manifest.'
            } >> "$RUNNER_TEMP/kargo/body.md"
          fi
      - uses: peter-evans/create-pull-request@v7
        with:
          branch: kargo/dependency-updates
          delete-branch: true
          commit-message: Update dependencies
          title: Update dependencies
          body-path: ${{ runner.temp }}/kargo/body.md
"#;

/// The text of the workflow `.github/workflows/ci.yml`, installing Kargo `version`.
pub(super) fn workflow(plan: &CiPlan, version: &str) -> String {
    let mut y = String::from("name: CI\n\non:\n  push:\n  pull_request:\n\njobs:\n  ci:\n");
    if plan.os_matrix {
        y.push_str("    strategy:\n      fail-fast: false\n      matrix:\n        os: [ubuntu-latest, macos-latest, windows-latest]\n");
        y.push_str("    runs-on: ${{ matrix.os }}\n");
    } else {
        y.push_str("    runs-on: ubuntu-latest\n");
    }
    y.push_str("    steps:\n      - uses: actions/checkout@v4\n");
    if plan.setup_java {
        y.push_str(&format!(
            "      - uses: actions/setup-java@v4\n        with:\n          distribution: temurin\n          java-version: {JDK_VERSION}\n"
        ));
    }
    if plan.setup_node {
        y.push_str(&format!("      - uses: actions/setup-node@v4\n        with:\n          node-version: {NODE_VERSION}\n"));
    }
    y.push_str(&format!("      - uses: {ACTION}\n        with:\n          version: {version}\n"));
    for step in CHECKS {
        y.push_str(&format!("      - run: kargo {step}\n"));
    }
    y.push_str(&format!("      - run: kargo {}\n", plan.test_command()));
    if plan.junit {
        y.push_str("      - uses: mikepenz/action-junit-report@v5\n        if: always()\n");
        y.push_str(&format!("        with:\n          report_paths: {JUNIT_REPORTS}\n"));
    }
    y.push_str(&format!("      - run: kargo {PACKAGE}\n"));
    y
}

/// The text of the dependency update workflow, which upgrades each of `packages`.
pub(super) fn updates_workflow(version: &str, packages: &[&str]) -> String {
    UPDATES_WORKFLOW.replace("{action}", ACTION).replace("{version}", version).replace("{packages}", &packages.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jvm_plan_has_java_and_no_matrix() {
        let y = workflow(&CiPlan { setup_java: true, ..CiPlan::default() }, "1.2.3");
        assert!(y.contains("setup-java") && !y.contains("setup-node") && !y.contains("matrix"));
        assert!(y.contains("version: 1.2.3"));
    }

    #[test]
    fn native_and_wasm_add_matrix_and_node() {
        let y = workflow(&CiPlan { setup_node: true, os_matrix: true, junit: true, ..CiPlan::default() }, "1.2.3");
        assert!(y.contains("setup-node") && y.contains("matrix.os") && y.contains("--report junit"));
        assert!(!y.contains("setup-java"));
    }

    #[test]
    fn updates_workflow_upgrades_every_package() {
        let y = updates_workflow("1.2.3", &["app", "lib"]);
        assert!(y.contains("version: 1.2.3") && y.contains(&format!("uses: {ACTION}")));
        assert_eq!(y.matches("for package in app lib; do").count(), 2);
        assert!(!y.contains("{packages}") && !y.contains("{version}") && !y.contains("{action}"));
    }
}
