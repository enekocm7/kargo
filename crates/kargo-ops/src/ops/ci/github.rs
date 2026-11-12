//! The GitHub Actions writer of `kargo init-ci`.

use super::{CHECKS, CiPlan, JDK_VERSION, JUNIT_REPORTS, NODE_VERSION, PACKAGE};

/// The setup action of this repository.
const ACTION: &str = "enekocm7/kargo/setup-kargo@master";

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
}
