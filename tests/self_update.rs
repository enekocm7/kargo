//! `kargo self update`, `kargo self uninstall`, `kargo --version` and `[toolchain] kargo`, against release directories on disk.
//!
//! Each test copies the `kargo` under test into a scratch folder first, so updating or removing "Kargo" never touches the
//! binary cargo built.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::{Project, manifest};
use predicates::prelude::*;

/// The target the `kargo` under test was built for.
fn host(p: &Project) -> String {
    let out = p.kargo().args(["-v", "--version"]).output().unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("host: ").map(str::to_owned))
        .expect("`kargo -vV` prints the host")
}

fn file_url(dir: &Path) -> String {
    format!("file:///{}", dir.display().to_string().replace('\\', "/").trim_start_matches('/'))
}

/// Publishes `version` in `dir`, in the layout `kargo self update` reads, with a "binary" that is just `payload`. `sum`
/// replaces the archive's real checksum when given.
fn publish(p: &Project, dir: &Path, version: &str, payload: &str, sum: Option<&str>) {
    let target = host(p);
    let windows = target.contains("windows");
    let exe = if windows { "kargo.exe" } else { "kargo" };
    let folder = format!("kargo-{version}-{target}");
    let name = format!("{folder}.{}", if windows { "zip" } else { "tar.gz" });
    let downloads = dir.join("download").join(format!("v{version}"));
    std::fs::create_dir_all(&downloads).unwrap();
    let archive = downloads.join(&name);
    if windows {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        zip.start_file(format!("{folder}/{exe}"), zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(payload.as_bytes()).unwrap();
        zip.finish().unwrap();
    } else {
        let gz = flate2::write::GzEncoder::new(std::fs::File::create(&archive).unwrap(), flate2::Compression::default());
        let mut tar = tar::Builder::new(gz);
        let mut header = tar::Header::new_gnu();
        header.set_size(payload.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        tar.append_data(&mut header, format!("{folder}/{exe}"), payload.as_bytes()).unwrap();
        tar.into_inner().unwrap().finish().unwrap();
    }
    let real = kargo_util::hash::sha256_bytes(&std::fs::read(&archive).unwrap());
    std::fs::write(downloads.join("SHA256SUMS"), format!("{}  {name}\n", sum.unwrap_or(&real))).unwrap();
    std::fs::create_dir_all(dir.join("latest/download")).unwrap();
    std::fs::write(dir.join("latest/download/VERSION"), format!("{version}\n")).unwrap();
}

/// Copies the `kargo` under test into `dir`, where `self` commands may replace or remove it.
fn install_copy(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let exe = dir.join(if cfg!(windows) { "kargo.exe" } else { "kargo" });
    std::fs::copy(assert_cmd::cargo::cargo_bin("kargo"), &exe).unwrap();
    exe
}

/// Runs the copy at `exe` with the Kargo home of `p` and the releases in `releases`.
fn run(exe: &Path, p: &Project, releases: &Path, args: &[&str]) -> Output {
    Command::new(exe)
        .args(args)
        .env("KARGO_HOME", p.home.path())
        .env("KARGO_RELEASES_URL", file_url(releases))
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn update_replaces_the_binary_after_verifying_it() {
    let p = Project::new();
    let releases = tempfile::tempdir().unwrap();
    publish(&p, releases.path(), "99.0.0", "the new kargo", None);
    let exe = install_copy(&p.home.path().join("bin"));

    let check = run(&exe, &p, releases.path(), &["self", "update", "--check"]);
    assert!(check.status.success());
    assert!(stderr(&check).contains("kargo 99.0.0"), "{}", stderr(&check));
    assert_ne!(std::fs::read(&exe).unwrap(), b"the new kargo", "--check changes nothing");

    let update = run(&exe, &p, releases.path(), &["self", "update"]);
    assert!(update.status.success() && stderr(&update).contains("Updated"), "{}", stderr(&update));
    assert_eq!(std::fs::read(&exe).unwrap(), b"the new kargo");
    let staging = p.home.path().join("tmp");
    assert!(!staging.exists() || std::fs::read_dir(staging).unwrap().next().is_none(), "the download is cleaned up");
}

#[test]
fn update_refuses_an_archive_with_the_wrong_checksum() {
    let p = Project::new();
    let releases = tempfile::tempdir().unwrap();
    publish(&p, releases.path(), "99.0.0", "tampered", Some(&"0".repeat(64)));
    let exe = install_copy(&p.home.path().join("bin"));
    let before = std::fs::read(&exe).unwrap();

    let out = run(&exe, &p, releases.path(), &["self", "update"]);
    assert_eq!(out.status.code(), Some(101));
    assert!(stderr(&out).contains("checksum mismatch"), "{}", stderr(&out));
    assert_eq!(std::fs::read(&exe).unwrap(), before, "the binary is untouched");
}

#[test]
fn update_knows_when_it_is_up_to_date_and_reports_missing_releases() {
    let p = Project::new();
    let releases = tempfile::tempdir().unwrap();
    publish(&p, releases.path(), "0.1.0", "same", None);
    let exe = install_copy(&p.home.path().join("bin"));

    let out = run(&exe, &p, releases.path(), &["self", "update"]);
    assert!(out.status.success() && stderr(&out).contains("Up to date"), "{}", stderr(&out));

    let out = run(&exe, &p, releases.path(), &["self", "update", "--version", "98.0.0"]);
    assert_eq!(out.status.code(), Some(101));
    assert!(stderr(&out).contains("failed to find release v98.0.0"), "{}", stderr(&out));

    let out = run(&exe, &p, releases.path(), &["self", "update", "--version", "latest"]);
    assert!(stderr(&out).contains("is not a version like 0.3.0"), "{}", stderr(&out));
}

#[test]
fn package_manager_installs_are_left_alone() {
    let p = Project::new();
    let releases = tempfile::tempdir().unwrap();
    publish(&p, releases.path(), "99.0.0", "new", None);
    let cellar = tempfile::tempdir().unwrap();
    let exe = install_copy(&cellar.path().join("Cellar/kargo/0.1.0/bin"));
    for args in [["self", "update"], ["self", "uninstall"]] {
        let out = run(&exe, &p, releases.path(), &args);
        assert_eq!(out.status.code(), Some(101));
        let text = stderr(&out);
        assert!(text.contains("installed by Homebrew") && text.contains("`brew upgrade kargo`"), "{text}");
    }
    assert!(exe.is_file());
    // `--check` is only a question, so it works for them too.
    let out = run(&exe, &p, releases.path(), &["self", "update", "--check"]);
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn uninstall_removes_the_binary_and_with_purge_the_home() {
    let p = Project::new();
    let releases = tempfile::tempdir().unwrap();
    let kargo_home = p.home.path().join("kargo-home");
    let exe = install_copy(&kargo_home.join("bin"));
    std::fs::create_dir_all(kargo_home.join("cache")).unwrap();
    let uninstall = |exe: &Path, args: &[&str]| {
        Command::new(exe)
            .args(["self", "uninstall"])
            .args(args)
            .env("KARGO_HOME", &kargo_home)
            .env("KARGO_RELEASES_URL", file_url(releases.path()))
            .output()
            .unwrap()
    };

    let out = uninstall(&exe, &[]);
    assert!(out.status.success() && stderr(&out).contains("kept"), "{}", stderr(&out));
    assert!(!exe.exists());
    assert!(kargo_home.join("cache").is_dir(), "only --purge removes the home");

    let exe = install_copy(&kargo_home.join("bin"));
    let out = uninstall(&exe, &["--purge"]);
    assert!(out.status.success(), "{}", stderr(&out));
    // Windows finishes deleting the folder of the running binary after the process exits.
    for _ in 0..40 {
        if !kargo_home.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    assert!(!kargo_home.exists());
}

#[test]
fn version_flags_print_the_version_and_host() {
    let p = Project::new();
    // The version of this crate, which is the binary's.
    let version = env!("CARGO_PKG_VERSION");
    p.kargo().arg("--version").assert().success().stdout(predicate::str::starts_with(format!("kargo {version}\n")));
    p.kargo().arg("-V").assert().success().stdout(predicate::str::starts_with(format!("kargo {version}\n")));
    p.kargo()
        .arg("-vV")
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("release: {version}")))
        .stdout(predicate::str::contains("host: "));
    // `kargo version` is the same output as a command.
    p.kargo().arg("version").assert().success().stdout(format!("kargo {version}\n"));
    p.kargo()
        .args(["-v", "version"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("release: {version}")))
        .stdout(predicate::str::contains("host: "));
    // No notice when stderr is not a terminal, even when a newer release exists.
    let releases = tempfile::tempdir().unwrap();
    publish(&p, releases.path(), "99.0.0", "new", None);
    p.kargo().arg("--version").env("KARGO_RELEASES_URL", file_url(releases.path())).assert().success().stderr("");
}

#[test]
fn toolchain_kargo_requires_a_minimum_kargo_version() {
    let p = Project::new();
    p.file("kargo.toml", &manifest("app", "[toolchain]\nkargo = \">=0.1\"\n"));
    p.kargo().arg("metadata").assert().success();

    p.file("kargo.toml", &manifest("app", "[toolchain]\nkargo = \">=99.0\"\n"));
    p.kargo()
        .arg("metadata")
        .assert()
        .failure()
        .stderr(predicate::str::contains("needs kargo >=99.0"))
        .stderr(predicate::str::contains("kargo self update"));

    // `kargo self` never reads the project, so a Kargo that is too old can still update itself.
    let releases = tempfile::tempdir().unwrap();
    publish(&p, releases.path(), "0.1.0", "same", None);
    let exe = install_copy(&p.home.path().join("bin"));
    let out = Command::new(&exe)
        .args(["self", "update"])
        .current_dir(p.path())
        .env("KARGO_HOME", p.home.path())
        .env("KARGO_RELEASES_URL", file_url(releases.path()))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
}
