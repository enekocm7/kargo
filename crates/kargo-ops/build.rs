//! Exports the target triple Kargo is built for, so `kargo self update` fetches the matching release archive.

fn main() {
    let target = std::env::var("TARGET").expect("cargo sets TARGET for build scripts");
    println!("cargo:rustc-env=KARGO_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
}
