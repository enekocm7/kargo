//! The Kargo extension for Zed.
//!
//! - **Language server**: runs `kargo lsp` for Kotlin files and `kargo.toml` in a worktree with a
//!   `kargo.toml` at its root, so Kotlin code is analyzed with the project's dependencies.
//! - **Debugger**: the `Kargo` debug adapter runs `kargo dap`, and the `kargo` locator turns a
//!   `kargo run` or `kargo test` task into a debug session, so the run buttons and tasks Kargo
//!   writes with `kargo ide --zed` can also be debugged.
//! - **Assistant context**: the `/kargo-metadata` slash command inserts the project's layout,
//!   targets and `kargo.toml`.

mod metadata;

use zed_extension_api::{
    self as zed, DebugAdapterBinary, DebugConfig, DebugRequest, DebugScenario, DebugTaskDefinition, LanguageServerId, SlashCommand,
    SlashCommandOutput, SlashCommandOutputSection, StartDebuggingRequestArguments, StartDebuggingRequestArgumentsRequest, TaskTemplate,
    Worktree,
    serde_json::{self, Value, json},
    settings::LspSettings,
};

/// The name of the language server and of its settings (`lsp.kargo`).
const SERVER: &str = "kargo";
/// The debug adapter's name.
const ADAPTER: &str = "Kargo";

const NOT_INSTALLED: &str = "Kargo is not installed: install it (https://enekocm7.github.io/kargo/getting-started/installation/) \
or set its path in `lsp.kargo.binary.path` in Zed's settings";

struct KargoExtension;

impl KargoExtension {
    /// The `kargo` to run: `lsp.kargo.binary.path`, else the one on the worktree's `PATH`.
    fn kargo(worktree: &Worktree) -> Result<String, String> {
        let configured = LspSettings::for_worktree(SERVER, worktree).ok().and_then(|s| s.binary).and_then(|b| b.path);
        configured.or_else(|| worktree.which("kargo")).ok_or_else(|| NOT_INSTALLED.to_owned())
    }

    fn is_kargo_project(worktree: &Worktree) -> bool {
        worktree.read_text_file("kargo.toml").is_ok()
    }
}

impl zed::Extension for KargoExtension {
    fn new() -> Self {
        KargoExtension
    }

    fn language_server_command(&mut self, _id: &LanguageServerId, worktree: &Worktree) -> zed::Result<zed::Command> {
        if !Self::is_kargo_project(worktree) {
            return Err("not a Kargo project: there is no kargo.toml at the root of this folder".to_owned());
        }
        let settings = LspSettings::for_worktree(SERVER, worktree).unwrap_or_default();
        let binary = settings.binary.as_ref();
        let mut args = vec!["lsp".to_owned()];
        args.extend(binary.and_then(|b| b.arguments.clone()).unwrap_or_default());
        let mut env = worktree.shell_env();
        env.extend(binary.and_then(|b| b.env.clone()).unwrap_or_default());
        Ok(zed::Command { command: Self::kargo(worktree)?, args, env })
    }

    fn language_server_initialization_options(&mut self, _id: &LanguageServerId, worktree: &Worktree) -> zed::Result<Option<Value>> {
        let configured = LspSettings::for_worktree(SERVER, worktree).ok().and_then(|s| s.initialization_options);
        Ok(Some(initialization_options(configured)))
    }

    fn get_dap_binary(
        &mut self,
        adapter_name: String,
        config: DebugTaskDefinition,
        user_provided_debug_adapter_path: Option<String>,
        worktree: &Worktree,
    ) -> Result<DebugAdapterBinary, String> {
        if adapter_name != ADAPTER {
            return Err(format!("unknown debug adapter `{adapter_name}`"));
        }
        let configuration: Value = serde_json::from_str(&config.config).map_err(|e| format!("invalid debug configuration: {e}"))?;
        let request = self.dap_request_kind(adapter_name, configuration)?;
        let command = match user_provided_debug_adapter_path {
            Some(path) => path,
            None => Self::kargo(worktree)?,
        };
        Ok(DebugAdapterBinary {
            command: Some(command),
            arguments: vec!["dap".to_owned()],
            envs: worktree.shell_env(),
            cwd: Some(worktree.root_path()),
            connection: None,
            request_args: StartDebuggingRequestArguments { configuration: config.config, request },
        })
    }

    fn dap_request_kind(&mut self, _adapter_name: String, config: Value) -> Result<StartDebuggingRequestArgumentsRequest, String> {
        match config.get("request").and_then(Value::as_str) {
            Some("launch") => Ok(StartDebuggingRequestArgumentsRequest::Launch),
            Some("attach") => Ok(StartDebuggingRequestArgumentsRequest::Attach),
            _ => Err("a Kargo debug configuration needs `\"request\": \"launch\"` or `\"attach\"`".to_owned()),
        }
    }

    fn dap_config_to_scenario(&mut self, config: DebugConfig) -> Result<DebugScenario, String> {
        let configuration = match config.request {
            DebugRequest::Launch(launch) => {
                let mut args = launch.args;
                // `kargo run ...` typed as the program, or the Kargo arguments alone.
                if !is_kargo(&launch.program) {
                    args.insert(0, launch.program);
                }
                let mut configuration = json!({ "request": "launch", "args": args });
                if let Some(cwd) = launch.cwd {
                    configuration["cwd"] = json!(cwd);
                }
                if !launch.envs.is_empty() {
                    configuration["env"] = env_object(&launch.envs);
                }
                configuration
            }
            DebugRequest::Attach(_) => {
                return Err(
                    "Kargo attaches to a JVM by port: add a debug.json scenario with `\"request\": \"attach\"` and `\"port\"`".to_owned()
                );
            }
        };
        Ok(DebugScenario {
            label: config.label,
            adapter: ADAPTER.to_owned(),
            build: None,
            config: configuration.to_string(),
            tcp_connection: None,
        })
    }

    fn dap_locator_create_scenario(
        &mut self,
        _locator_name: String,
        build_task: TaskTemplate,
        resolved_label: String,
        _debug_adapter_name: String,
    ) -> Option<DebugScenario> {
        let configuration = locate(&build_task.command, &build_task.args, build_task.cwd.as_deref(), &build_task.env)?;
        Some(DebugScenario {
            label: format!("Debug {}", resolved_label.trim_start_matches("Debug ")),
            adapter: ADAPTER.to_owned(),
            build: None,
            config: configuration.to_string(),
            tcp_connection: None,
        })
    }

    fn run_dap_locator(&mut self, _locator_name: String, _build_task: TaskTemplate) -> Result<DebugRequest, String> {
        Err("the kargo locator starts debug sessions without a build step".to_owned())
    }

    fn run_slash_command(
        &self,
        command: SlashCommand,
        _args: Vec<String>,
        worktree: Option<&Worktree>,
    ) -> Result<SlashCommandOutput, String> {
        if command.name != "kargo-metadata" {
            return Err(format!("unknown slash command `{}`", command.name));
        }
        let worktree = worktree.ok_or("open a Kargo project to use /kargo-metadata")?;
        if !Self::is_kargo_project(worktree) {
            return Err("not a Kargo project: there is no kargo.toml at the root of this folder".to_owned());
        }
        let root = worktree.root_path();
        let manifest = format!("{root}/kargo.toml");
        let output = zed::process::Command::new(Self::kargo(worktree)?)
            .args(["--manifest-path", manifest.as_str(), "metadata", "--no-deps"])
            .envs(worktree.shell_env())
            .output()?;
        if output.status != Some(0) {
            return Err(format!("`kargo metadata` failed: {}", String::from_utf8_lossy(&output.stderr).trim()));
        }
        let metadata: Value = serde_json::from_slice(&output.stdout).map_err(|e| format!("unexpected `kargo metadata` output: {e}"))?;
        let text = metadata::describe(&metadata, |path| worktree.read_text_file(path).ok());
        let range = zed::Range { start: 0, end: text.len() as u32 };
        Ok(SlashCommandOutput { sections: vec![SlashCommandOutputSection { range, label: "Kargo project".to_owned() }], text })
    }
}

/// The `kargo lsp` initialization options: the user's, over Kargo's defaults for Zed.
fn initialization_options(configured: Option<Value>) -> Value {
    let mut options = json!({ "checkOnSave": true });
    if let (Some(Value::Object(configured)), Some(defaults)) = (configured, options.as_object_mut()) {
        defaults.extend(configured);
    }
    options
}

/// Whether `program` names the `kargo` executable.
fn is_kargo(program: &str) -> bool {
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    name == "kargo" || name == "kargo.exe"
}

/// The debug configuration of a `kargo run` or `kargo test` task, or `None` for any other task.
fn locate(command: &str, args: &[String], cwd: Option<&str>, env: &[(String, String)]) -> Option<Value> {
    // A task written as one command line, such as `kargo run --bin server`.
    let words: Vec<String> = command.split_whitespace().map(str::to_owned).collect();
    let (program, mut all) = words.split_first().map(|(p, rest)| (p.clone(), rest.to_vec()))?;
    all.extend(args.iter().cloned());
    if !is_kargo(&program) {
        return None;
    }
    // Global options come first: `kargo -q run`.
    let subcommand = all.iter().position(|a| !a.starts_with('-'))?;
    if !matches!(all[subcommand].as_str(), "run" | "r" | "test" | "t") {
        return None;
    }
    let mut configuration = json!({ "request": "launch", "args": all });
    if let Some(cwd) = cwd {
        configuration["cwd"] = json!(cwd);
    }
    if !env.is_empty() {
        configuration["env"] = env_object(env);
    }
    Some(configuration)
}

fn env_object(env: &[(String, String)]) -> Value {
    Value::Object(env.iter().map(|(k, v)| (k.clone(), json!(v))).collect())
}

zed::register_extension!(KargoExtension);

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    #[test]
    fn debugs_kargo_run_and_test_tasks() {
        let config = locate("kargo", &strings(&["run", "--bin", "server"]), Some("$ZED_WORKTREE_ROOT"), &[]).unwrap();
        assert_eq!(config, json!({ "request": "launch", "args": ["run", "--bin", "server"], "cwd": "$ZED_WORKTREE_ROOT" }));
        let config = locate("kargo test", &strings(&["com.example.MathTest"]), None, &[("A".into(), "1".into())]).unwrap();
        assert_eq!(config, json!({ "request": "launch", "args": ["test", "com.example.MathTest"], "env": { "A": "1" } }));
        assert!(locate("/usr/local/bin/kargo", &strings(&["-q", "t"]), None, &[]).is_some());
        assert!(locate("kargo", &strings(&["build"]), None, &[]).is_none());
        assert!(locate("cargo", &strings(&["run"]), None, &[]).is_none());
        assert!(locate("kargo", &[], None, &[]).is_none());
    }

    #[test]
    fn turns_on_check_on_save_unless_the_user_turns_it_off() {
        assert_eq!(initialization_options(None), json!({ "checkOnSave": true }));
        let options = initialization_options(Some(json!({ "checkOnSave": false, "checkTargets": ["wasm"] })));
        assert_eq!(options, json!({ "checkOnSave": false, "checkTargets": ["wasm"] }));
    }

    #[test]
    fn recognizes_the_kargo_executable() {
        assert!(is_kargo("kargo"));
        assert!(is_kargo("C:\\tools\\kargo.exe"));
        assert!(!is_kargo("kargo-fmt"));
    }
}
