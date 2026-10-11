use kargo::{config::Config, maven::*, shell::*, toolchain::*};
fn main() -> anyhow::Result<()> {
    let config = Config::from_env(Shell::new(Verbosity::Quiet))?;
    let client = MavenClient::new(&config, vec![Repository::maven_central()]);
    let what = std::env::args().nth(1).unwrap_or_default();
    if what == "wasm" {
        let r = Resolver::new(&client, Platform::Wasm("js".into())).resolve(&[
            DependencyRequest::new("org.jetbrains.kotlin:kotlin-stdlib-wasm-js".parse()?, Some("2.4.21".into())),
            DependencyRequest::new("org.jetbrains.kotlin:kotlin-test-wasm-js".parse()?, Some("2.4.21".into()))])?;
        println!("{}", kargo::util::fs::join_paths(&r.files()));
    } else {
        let c = KotlinCompiler::resolve(&client, "2.4.21")?;
        println!("{}", kargo::util::fs::join_paths(&c.classpath));
    }
    Ok(())
}
