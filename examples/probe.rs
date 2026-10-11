use kargo::{config::Config, maven::*, shell::*, toolchain::*};
fn main() -> anyhow::Result<()> {
    let config = Config::from_env(Shell::new(Verbosity::Normal))?;
    let client = MavenClient::new(&config, vec![Repository::maven_central()]);
    let c = KotlinCompiler::resolve(&client, "2.4.21")?;
    println!("{}", kargo::util::fs::join_paths(&c.classpath));
    let r = Resolver::new(&client, Platform::Wasm("js".into())).resolve(&[DependencyRequest::new("org.jetbrains.kotlin:kotlin-stdlib".parse()?, Some("2.4.21".into()))])?;
    eprintln!("{:?}", r.files());
    Ok(())
}
