//! Exports the target triple Kargo is built for, so `kargo self update` fetches the matching
//! release archive, and the commit and date of the build, which `kargo info` and `kargo report`
//! show and `kargo report --open` puts in the issue.

use std::process::Command;

fn main() {
    let target = std::env::var("TARGET").expect("cargo sets TARGET for build scripts");
    println!("cargo:rustc-env=KARGO_TARGET={target}");
    println!("cargo:rustc-env=KARGO_COMMIT={}", commit());
    println!("cargo:rustc-env=KARGO_BUILD_DATE={}", build_date());
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
}

/// The short commit Kargo is built from, or `unknown` without a Git checkout (a crates.io build).
fn commit() -> String {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new("git").args(["rev-parse", "--short=7", "HEAD"]).current_dir(root).env("GIT_TERMINAL_PROMPT", "0").output();
    match output {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        _ => "unknown".to_owned(),
    }
}

/// Today, in UTC, or the time of `SOURCE_DATE_EPOCH` for a reproducible build: `YYYY-MM-DD`.
fn build_date() -> String {
    let seconds = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or_else(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default());
    let (year, month, day) = civil_from_days((seconds / 86_400) as i64);
    format!("{year:04}-{month:02}-{day:02}")
}

/// The calendar date of a day count since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
