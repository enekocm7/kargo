//! Helpers shared by the integration tests.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use assert_cmd::Command;

/// A scratch project directory with an isolated Kargo home.
pub struct Project {
    pub dir: tempfile::TempDir,
    pub home: tempfile::TempDir,
}

impl Project {
    pub fn new() -> Self {
        Self { dir: tempfile::tempdir().unwrap(), home: tempfile::tempdir().unwrap() }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn file(&self, rel: &str, contents: &str) -> &Self {
        let p = self.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, contents).unwrap();
        self
    }

    pub fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.path().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
    }

    /// `kargo` running in the project with an isolated home directory. Toolchains are never
    /// installed automatically (`KARGO_AUTO_INSTALL=0`), so a machine without Java needs no network.
    pub fn kargo(&self) -> Command {
        let mut cmd = Command::cargo_bin("kargo").unwrap();
        cmd.current_dir(self.path())
            .env("KARGO_HOME", self.home.path())
            .env_remove("KARGO_TARGET_DIR")
            .env_remove("KARGO_JOBS")
            .env("NO_COLOR", "1")
            .env("KARGO_AUTO_INSTALL", "0");
        cmd
    }
}

/// An on-disk Maven repository.
pub struct LocalRepo {
    pub dir: tempfile::TempDir,
}

impl LocalRepo {
    pub fn new() -> Self {
        Self { dir: tempfile::tempdir().unwrap() }
    }

    pub fn url(&self) -> String {
        format!("file:///{}", self.dir.path().display().to_string().replace('\\', "/").trim_start_matches('/'))
    }

    pub fn write(&self, rel: &str, contents: &str) {
        let p = self.dir.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, contents).unwrap();
    }

    /// Publishes `group:artifact:version` with `(group:artifact, version)` dependencies.
    pub fn publish(&self, coord: &str, version: &str, deps: &[(&str, &str)]) -> &Self {
        let (group, artifact) = coord.split_once(':').unwrap();
        let dir = format!("{}/{artifact}", group.replace('.', "/"));
        self.write(&format!("{dir}/{version}/{artifact}-{version}.pom"), &pom(group, artifact, version, deps));
        self.write(&format!("{dir}/{version}/{artifact}-{version}.jar"), &format!("jar {coord} {version}"));
        self.list_version(&dir, version)
    }

    /// Publishes the `classifier` jar of a module already published with [`LocalRepo::publish`].
    pub fn publish_classifier(&self, coord: &str, version: &str, classifier: &str) -> &Self {
        let (group, artifact) = coord.split_once(':').unwrap();
        let rel = format!("{}/{artifact}/{version}/{artifact}-{version}-{classifier}.jar", group.replace('.', "/"));
        self.write(&rel, &format!("jar {coord} {version} {classifier}"));
        self
    }

    /// Publishes a SNAPSHOT the way `mvn deploy` stores it: timestamped files in the `1.0-SNAPSHOT`
    /// directory (`stamp` is `20240101.120000-1`) and the `maven-metadata.xml` that points at them.
    /// Publishing again moves the metadata to the new build and leaves the old files in place.
    pub fn publish_snapshot(&self, coord: &str, version: &str, stamp: &str, deps: &[(&str, &str)]) -> &Self {
        let (group, artifact) = coord.split_once(':').unwrap();
        let dir = format!("{}/{artifact}", group.replace('.', "/"));
        let file_version = format!("{}-{stamp}", version.strip_suffix("-SNAPSHOT").unwrap());
        let (timestamp, number) = stamp.split_once('-').unwrap();
        let updated = timestamp.replace('.', "");
        self.write(&format!("{dir}/{version}/{artifact}-{file_version}.pom"), &pom(group, artifact, version, deps));
        self.write(&format!("{dir}/{version}/{artifact}-{file_version}.jar"), &format!("jar {coord} {file_version}"));
        let entry = |extension: &str| {
            format!(
                "<snapshotVersion><extension>{extension}</extension><value>{file_version}</value><updated>{updated}</updated></snapshotVersion>"
            )
        };
        self.write(
            &format!("{dir}/{version}/maven-metadata.xml"),
            &format!(
                "<metadata><groupId>{group}</groupId><artifactId>{artifact}</artifactId><version>{version}</version><versioning>\
                 <snapshot><timestamp>{timestamp}</timestamp><buildNumber>{number}</buildNumber></snapshot><lastUpdated>{updated}</lastUpdated>\
                 <snapshotVersions>{}{}</snapshotVersions></versioning></metadata>",
                entry("jar"),
                entry("pom")
            ),
        );
        self.list_version(&dir, version)
    }

    /// Adds `version` to the module's version list.
    fn list_version(&self, dir: &str, version: &str) -> &Self {
        let meta_path = self.dir.path().join(format!("{dir}/maven-metadata.xml"));
        let mut versions: Vec<String> = std::fs::read_to_string(&meta_path)
            .map(|s| s.split("<version>").skip(1).map(|p| p.split('<').next().unwrap().to_owned()).collect())
            .unwrap_or_default();
        if !versions.iter().any(|v| v == version) {
            versions.push(version.to_owned());
        }
        let body: String = versions.iter().map(|v| format!("<version>{v}</version>")).collect();
        self.write(
            &format!("{dir}/maven-metadata.xml"),
            &format!("<metadata><versioning><versions>{body}</versions></versioning></metadata>"),
        );
        self
    }
}

fn pom(group: &str, artifact: &str, version: &str, deps: &[(&str, &str)]) -> String {
    let deps_xml: String = deps
        .iter()
        .map(|(c, v)| {
            let (g, a) = c.split_once(':').unwrap();
            format!("<dependency><groupId>{g}</groupId><artifactId>{a}</artifactId><version>{v}</version></dependency>")
        })
        .collect();
    format!(
        "<project><groupId>{group}</groupId><artifactId>{artifact}</artifactId><version>{version}</version><dependencies>{deps_xml}</dependencies></project>"
    )
}

/// A canned OSV server on a loopback port, for `kargo audit` (`KARGO_OSV_URL`): answers
/// `/v1/querybatch` and `/v1/vulns/<id>` from `advisories`, each affecting the listed
/// `(package, version)` pairs, and counts the requests.
pub struct OsvServer {
    pub url: String,
    pub requests: Arc<AtomicUsize>,
}

impl OsvServer {
    pub fn start(advisories: Vec<(serde_json::Value, Vec<(&str, &str)>)>) -> Self {
        let advisories: Vec<(serde_json::Value, Vec<(String, String)>)> = advisories
            .into_iter()
            .map(|(v, affected)| (v, affected.into_iter().map(|(n, v)| (n.to_owned(), v.to_owned())).collect()))
            .collect();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                counter.fetch_add(1, Ordering::SeqCst);
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                let path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
                let mut length = 0;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header.trim_end().is_empty() {
                        break;
                    }
                    if let Some((k, v)) = header.split_once(':')
                        && k.eq_ignore_ascii_case("content-length")
                    {
                        length = v.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let (status, answer) = if path == "/v1/querybatch" {
                    let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    let results: Vec<serde_json::Value> = request["queries"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|q| {
                            let (name, version) = (q["package"]["name"].as_str().unwrap(), q["version"].as_str().unwrap());
                            let vulns: Vec<serde_json::Value> = advisories
                                .iter()
                                .filter(|(_, affected)| affected.iter().any(|(n, v)| n == name && v == version))
                                .map(|(v, _)| serde_json::json!({ "id": v["id"], "modified": v["modified"] }))
                                .collect();
                            if vulns.is_empty() { serde_json::json!({}) } else { serde_json::json!({ "vulns": vulns }) }
                        })
                        .collect();
                    (200, serde_json::json!({ "results": results }).to_string())
                } else if let Some(id) = path.strip_prefix("/v1/vulns/") {
                    match advisories.iter().find(|(v, _)| v["id"] == id) {
                        Some((v, _)) => (200, v.to_string()),
                        None => (404, "{}".to_owned()),
                    }
                } else {
                    (404, "{}".to_owned())
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} OK
Content-Type: application/json
Content-Length: {}
Connection: close

{answer}",
                    answer.len()
                );
            }
        });
        Self { url, requests }
    }

    pub fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

pub fn manifest(name: &str, extra: &str) -> String {
    format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n{extra}")
}

/// Whether end-to-end tests that need a JDK and network should run.
pub fn e2e_enabled() -> bool {
    std::env::var_os("KARGO_E2E").is_some()
}

pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat", ""] } else { &[""] };
    std::env::split_paths(&path).find_map(|d| exts.iter().map(|e| d.join(format!("{name}{e}"))).find(|p| p.is_file()))
}

/// The fake `adb`, `emulator`, `avdmanager` and `gradle` of `tests/fixtures/fake_android.rs`, compiled once per test run.
fn fake_android_exe() -> &'static Path {
    static EXE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    EXE.get_or_init(|| {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("fake-android");
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join(format!("fake-android-{}{}", std::process::id(), std::env::consts::EXE_SUFFIX));
        let source = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fake_android.rs");
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let out = std::process::Command::new(rustc)
            .args(["--edition", "2021", "-o"])
            .arg(&exe)
            .arg(source)
            .output()
            .expect("failed to run rustc");
        assert!(out.status.success(), "could not compile the fake Android tools:\n{}", String::from_utf8_lossy(&out.stderr));
        exe
    })
}

/// An Android SDK of fake tools (`adb`, `emulator`, `avdmanager`), a fake JDK, a fake `gradle` and a device list that the
/// tools read, for tests of `kargo run -t android` and `kargo android` that need no emulator.
pub struct FakeAndroid {
    pub sdk: PathBuf,
    pub state: PathBuf,
    pub avds: PathBuf,
    pub jdk: PathBuf,
    pub bin: PathBuf,
    _dir: tempfile::TempDir,
}

impl FakeAndroid {
    /// `devices` lists them as `serial|state|manufacturer|model|api|avd`, one per line.
    pub fn new(devices: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let (sdk, state, avds, jdk, bin) = (root.join("sdk"), root.join("state"), root.join("avds"), root.join("jdk"), root.join("bin"));
        for d in [&state, &avds, &bin, &jdk.join("bin")] {
            std::fs::create_dir_all(d).unwrap();
        }
        let exe = fake_android_exe();
        let copy = |to: PathBuf| {
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::copy(exe, to).unwrap();
        };
        let ext = std::env::consts::EXE_SUFFIX;
        copy(sdk.join("platform-tools").join(format!("adb{ext}")));
        copy(sdk.join("emulator").join(format!("emulator{ext}")));
        copy(bin.join(format!("gradle{ext}")));
        let avdmanager = sdk.join("cmdline-tools/latest/bin");
        std::fs::create_dir_all(&avdmanager).unwrap();
        if cfg!(windows) {
            std::fs::write(avdmanager.join("avdmanager.bat"), format!("@\"{}\" avdmanager %*\r\n", exe.display())).unwrap();
        } else {
            copy(avdmanager.join("avdmanager"));
        }
        let abi = if cfg!(target_arch = "aarch64") { "arm64-v8a" } else { "x86_64" };
        for package in [
            "platforms/android-36",
            "build-tools/36.0.0",
            "platform-tools",
            "emulator",
            "cmdline-tools/latest",
            &format!("system-images/android-35/google_apis/{abi}"),
        ] {
            std::fs::write(sdk.join(package).join("package.xml"), "<package/>").unwrap_or_else(|_| {
                std::fs::create_dir_all(sdk.join(package)).unwrap();
                std::fs::write(sdk.join(package).join("package.xml"), "<package/>").unwrap();
            });
        }
        // `Jdk::find` reads the version from the `release` file, so the fake `java` never runs.
        std::fs::write(jdk.join("release"), "JAVA_VERSION=\"21.0.5\"\n").unwrap();
        std::fs::write(jdk.join("bin").join(format!("java{ext}")), "").unwrap();
        let fake = Self { sdk, state, avds, jdk, bin, _dir: dir };
        fake.set_devices(devices);
        fake
    }

    pub fn set_devices(&self, devices: &str) {
        std::fs::write(self.state.join("devices"), devices).unwrap();
    }

    /// A virtual device on disk, as `avdmanager` would have made it.
    pub fn add_avd(&self, name: &str, api: u32) {
        let dir = self.avds.join(format!("{name}.avd"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(self.avds.join(format!("{name}.ini")), format!("path={}\ntarget=android-{api}\n", dir.display())).unwrap();
        std::fs::write(
            dir.join("config.ini"),
            format!("image.sysdir.1=system-images/android-{api}/google_apis/x86_64/\nhw.device.name=pixel_8\n"),
        )
        .unwrap();
    }

    /// A switch of the fake tools (`am-fails`, `crash`, `emulator-fails`).
    pub fn switch(&self, name: &str) {
        std::fs::write(self.state.join(name), "").unwrap();
    }

    /// Every call the tools got, one per line.
    pub fn log(&self) -> String {
        std::fs::read_to_string(self.state.join("log")).unwrap_or_default()
    }

    /// Points `kargo` at the fake SDK, JDK and Gradle, and away from the real ones.
    pub fn apply(&self, cmd: &mut Command) {
        let path = std::env::join_paths(
            std::iter::once(self.bin.clone()).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())),
        )
        .unwrap();
        cmd.env("ANDROID_HOME", &self.sdk)
            .env_remove("ANDROID_SDK_ROOT")
            .env("ANDROID_AVD_HOME", &self.avds)
            .env_remove("KARGO_ANDROID_DEVICE")
            .env("JAVA_HOME", &self.jdk)
            .env("PATH", path)
            .env("FAKE_ANDROID_STATE", &self.state);
    }
}
