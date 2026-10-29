//! A fake `adb`, `emulator` and `avdmanager` for the offline Android tests.
//!
//! The test compiles this file once and copies the executable into a fake SDK under the names of the tools (and as
//! `gradle`, which only records the `ANDROID_SERIAL` it was given). The name it is started as picks the tool (`fake-android avdmanager ...` is how the Windows `avdmanager.bat` calls it). The state
//! is a directory named by `FAKE_ANDROID_STATE`:
//!
//! - `devices`: one device per line, `serial|state|manufacturer|model|api|avd`.
//! - `log`: every call, one line each, `tool arg arg ...`.
//! - `running`: exists while the app runs (`am start` creates it, `am force-stop` and the end of `logcat` remove it).
//! - `am-fails`, `crash`, `emulator-fails`: switches for the failure cases.
//! - `no-default`: `am start` only resolves an activity started by name (`-n`), as with a launcher filter without a DEFAULT category.
//! - `FAKE_LINT_SARIF` (environment): the SARIF report that `gradle lint<Variant>` writes to `build/reports`; `FAKE_LINT_FAIL` makes that task fail.
//! - `no-resolve`: `cmd package resolve-activity` fails (an old device). `no-launcher`: no activity has the LAUNCHER category.

use std::io::Read;
use std::path::PathBuf;

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let exe = std::env::args().next().unwrap_or_default();
    let stem = std::path::Path::new(&exe).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let tool = if stem.starts_with("fake-android") { args.remove(0) } else { stem };
    let state = PathBuf::from(std::env::var("FAKE_ANDROID_STATE").expect("FAKE_ANDROID_STATE"));
    let mut log = std::fs::read_to_string(state.join("log")).unwrap_or_default();
    log.push_str(&format!("{tool} {}\n", args.join(" ")));
    std::fs::write(state.join("log"), log).unwrap();
    match tool.as_str() {
        "adb" => adb(&state, args),
        "emulator" => emulator(&state, args),
        "avdmanager" => avdmanager(args),
        "gradle" => {
            let mut log = std::fs::read_to_string(state.join("log")).unwrap_or_default();
            log.push_str(&format!("gradle-env ANDROID_SERIAL={}\n", std::env::var("ANDROID_SERIAL").unwrap_or_else(|_| "none".into())));
            std::fs::write(state.join("log"), log).unwrap();
            gradle_lint(&args);
        }
        other => panic!("unknown tool {other}"),
    }
}

/// A lint task writes the report in `FAKE_LINT_SARIF` (when set) the way AGP does, and fails when `FAKE_LINT_FAIL` is set.
fn gradle_lint(args: &[String]) {
    let Some(task) = args.iter().find_map(|a| a.strip_prefix("lint")).filter(|v| !v.is_empty() && !v.starts_with('-')) else {
        return;
    };
    let project = args.iter().position(|a| a == "--project-dir").and_then(|i| args.get(i + 1)).expect("--project-dir");
    let variant = format!("{}{}", task[..1].to_lowercase(), &task[1..]);
    if let Ok(sarif) = std::env::var("FAKE_LINT_SARIF") {
        let reports = PathBuf::from(project).join("build/reports");
        std::fs::create_dir_all(&reports).unwrap();
        std::fs::write(reports.join(format!("lint-results-{variant}.sarif")), sarif).unwrap();
        std::fs::write(reports.join(format!("lint-results-{variant}.html")), "<html/>").unwrap();
    }
    if std::env::var_os("FAKE_LINT_FAIL").is_some() {
        std::process::exit(1);
    }
}

type Device = Vec<String>;

fn devices(state: &std::path::Path) -> Vec<Device> {
    std::fs::read_to_string(state.join("devices"))
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.split('|').map(str::to_owned).collect())
        .collect()
}

fn save(state: &std::path::Path, devices: &[Device]) {
    let text: Vec<String> = devices.iter().map(|d| d.join("|")).collect();
    std::fs::write(state.join("devices"), text.join("\n") + "\n").unwrap();
}

fn adb(state: &std::path::Path, mut args: Vec<String>) {
    let mut serial = None;
    if args.first().map(String::as_str) == Some("-s") {
        args.remove(0);
        serial = Some(args.remove(0));
    }
    let all = devices(state);
    let device = serial.as_ref().and_then(|s| all.iter().find(|d| &d[0] == s)).cloned();
    if serial.is_some() && device.is_none() {
        eprintln!("adb: device '{}' not found", serial.unwrap());
        std::process::exit(1);
    }
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["version"] => println!("Android Debug Bridge version 1.0.41\nVersion 36.0.0-fake"),
        ["devices", ..] => {
            println!("List of devices attached");
            for (i, d) in all.iter().enumerate() {
                let state_text = if d[1] == "no permissions" {
                    "no permissions (user in plugdev group; are your udev rules wrong?); see [http://developer.android.com/tools/device.html]".to_owned()
                } else {
                    d[1].clone()
                };
                println!("{:<22} {} product:fake model:{} device:fake transport_id:{}", d[0], state_text, d[3].replace(' ', "_"), i + 1);
            }
            println!();
        }
        ["connect", address] => {
            if address.contains("bad") {
                println!("failed to connect to '{address}': Connection refused");
            } else {
                let mut all = all;
                all.push(vec![address.to_string(), "device".into(), "Google".into(), "Pixel 7".into(), "34".into(), String::new()]);
                save(state, &all);
                println!("connected to {address}");
            }
        }
        ["disconnect", ..] => {
            let kept: Vec<Device> = all.into_iter().filter(|d| !d[0].contains(':')).collect();
            save(state, &kept);
            println!("disconnected everything");
        }
        ["pair", address, code] => {
            if *code == "123456" {
                println!("Successfully paired to {address} [guid=adb-fake]");
            } else {
                println!("Failed: Wrong password or connection was dropped");
            }
        }
        ["emu", "avd", "name"] => {
            let device = device.expect("emu needs -s");
            println!("{}\nOK", device[5]);
        }
        ["emu", "kill"] => {
            let serial = serial.unwrap();
            save(state, &all.into_iter().filter(|d| d[0] != serial).collect::<Vec<_>>());
            println!("OK: killing emulator, bye bye");
        }
        ["shell", "getprop", name] => {
            let device = device.expect("shell needs -s");
            let value = match *name {
                "ro.build.version.sdk" => device[4].clone(),
                "ro.product.manufacturer" => device[2].clone(),
                "ro.product.model" => device[3].clone(),
                "sys.boot_completed" => "1".to_owned(),
                _ => String::new(),
            };
            println!("{value}");
        }
        // The launcher filter of the app has no DEFAULT category: it resolves with `resolve-activity`, but an implicit intent does not.
        ["shell", "cmd", "package", "resolve-activity", ..] => {
            if state.join("no-resolve").exists() {
                eprintln!("Can't find service: package");
                std::process::exit(1);
            } else if state.join("no-launcher").exists() {
                println!("No activity found");
            } else {
                println!("priority=0 preferredOrder=0 match=0x108000 specificIndex=-1 isDefault=false\ncom.example.droid/.MainActivity");
            }
        }
        ["shell", "am", "start", rest @ ..] => {
            let by_name = rest.contains(&"-n");
            if (state.join("no-default").exists() || state.join("no-launcher").exists()) && !by_name {
                println!("Starting: Intent {{ act=android.intent.action.MAIN cat=[android.intent.category.LAUNCHER] pkg=com.example.droid }}\nError: Activity not started, unable to resolve Intent {{ act=android.intent.action.MAIN cat=[android.intent.category.LAUNCHER] flg=0x10000000 xflg=0x4 pkg=com.example.droid }}");
            } else if state.join("am-fails").exists() {
                println!("Starting: Intent {{ act=android.intent.action.MAIN }}\nError type 3\nError: Activity class {{com.example.droid/com.example.droid.Missing}} does not exist.");
            } else {
                std::fs::write(state.join("running"), "1").unwrap();
                println!("Starting: Intent {{ act=android.intent.action.MAIN cat=[android.intent.category.LAUNCHER] pkg=com.example.droid }}\nStatus: ok\nLaunchState: COLD\nActivity: com.example.droid/.MainActivity\nTotalTime: 123\nWaitTime: 130\nComplete");
            }
        }
        ["shell", "am", "force-stop", ..] => {
            let _ = std::fs::remove_file(state.join("running"));
        }
        ["shell", "pidof", _] => {
            if state.join("running").exists() {
                println!("4242");
            } else {
                std::process::exit(1);
            }
        }
        ["logcat", rest @ ..] => {
            println!("--- logcat {}", rest.join(" "));
            println!("05-01 12:00:00.000  4242  4242 I Hello: the app says hi");
            if state.join("crash").exists() {
                println!("05-01 12:00:01.000  4242  4242 E AndroidRuntime: FATAL EXCEPTION: main");
            }
            // The app is gone by the time the next process check runs.
            let _ = std::fs::remove_file(state.join("running"));
            std::thread::sleep(std::time::Duration::from_secs(30));
        }
        _ => {}
    }
}

fn emulator(state: &std::path::Path, args: Vec<String>) {
    if state.join("emulator-fails").exists() {
        eprintln!("PANIC: Missing emulator engine program for 'x86' CPU.");
        std::process::exit(1);
    }
    let name = args.iter().position(|a| a == "-avd").and_then(|i| args.get(i + 1)).expect("-avd NAME").clone();
    let mut all = devices(state);
    let serial = "emulator-5554".to_owned();
    all.push(vec![serial.clone(), "device".into(), "Google".into(), "sdk_gphone64".into(), "35".into(), name]);
    save(state, &all);
    // An emulator runs until it is killed.
    for _ in 0..200 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if !devices(state).iter().any(|d| d[0] == serial) {
            return;
        }
    }
}

fn avdmanager(args: Vec<String>) {
    let mut stdin = String::new();
    let _ = std::io::stdin().read_to_string(&mut stdin);
    let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    assert_eq!(&args[..2], ["create", "avd"], "{args:?}");
    let (name, package) = (value("--name").expect("--name"), value("--package").expect("--package"));
    assert_eq!(stdin.trim(), "no", "avdmanager asks about a custom hardware profile");
    let home = PathBuf::from(std::env::var("ANDROID_AVD_HOME").expect("ANDROID_AVD_HOME"));
    let ini = home.join(format!("{name}.ini"));
    if ini.exists() && !args.iter().any(|a| a == "--force") {
        eprintln!("Error: Android Virtual Device '{name}' already exists.");
        std::process::exit(1);
    }
    let dir = home.join(format!("{name}.avd"));
    std::fs::create_dir_all(&dir).unwrap();
    // system-images;android-35;google_apis;x86_64
    let parts: Vec<&str> = package.split(';').collect();
    std::fs::write(&ini, format!("avd.ini.encoding=UTF-8\npath={}\ntarget={}\n", dir.display(), parts[1])).unwrap();
    std::fs::write(
        dir.join("config.ini"),
        format!(
            "image.sysdir.1=system-images/{}/{}/{}/\nhw.device.name={}\n",
            parts[1],
            parts[2],
            parts[3],
            value("--device").unwrap_or_else(|| "pixel".into())
        ),
    )
    .unwrap();
    println!("done");
}
