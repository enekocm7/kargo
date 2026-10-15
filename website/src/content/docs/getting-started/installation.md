---
title: Installation
description: Install the Kargo CLI and the tools it relies on.
---

Kargo is a single binary written in Rust. Install it, then make sure a JDK is available (or let Kargo download one).

## 1. Install Kargo

Kargo is installed from source with Cargo, Rust's package manager. If you don't have Rust yet, install it with [rustup](https://rustup.rs). Kargo needs **Rust 1.89 or newer**.

```sh
cargo install --locked --git https://github.com/enekocm7/kargo kargo
```

This builds Kargo and puts the `kargo` binary in `~/.cargo/bin`, which rustup adds to your `PATH`.

### From a local clone

```sh
git clone https://github.com/enekocm7/kargo
cd kargo
cargo install --locked --path .
```

### Updating

Run the same `cargo install` command again. Cargo replaces the old binary.

### Check that it works

```console
$ kargo --version
kargo 0.1.0
$ kargo --help
Cargo for Kotlin: a build tool and package manager
...
```

## 2. Get a JDK

The Kotlin compiler runs on the JVM, so Kargo needs a **JDK 11 or newer** for every target. JVM tests need **JDK 17 or newer**, because `kargo test` runs the JUnit 6 launcher on the project's JDK. You have two options.

**Use the JDK you already have.** Kargo looks at `JAVA_HOME`, then at `java` on your `PATH`.

```console
$ java -version
openjdk version "21.0.5" 2024-10-15 LTS
```

**Let Kargo download one.** Add a `[toolchain]` section to a project and Kargo will download that [Eclipse Temurin](https://adoptium.net) JDK on first use, verify its checksum and keep it under `~/.kargo/toolchains`:

```toml title="kargo.toml"
[toolchain]
jdk = "21"
```

See [Managing JDKs](../../guides/toolchains/) for details.

## 3. Optional tools

You only need these for some targets:

| You want to... | You need |
|---|---|
| Run or test **WebAssembly** programs | [Node.js](https://nodejs.org) (`node` on `PATH`). Compiling Wasm doesn't need it, unless a dependency needs [npm packages](../../guides/wasm/#npm-packages); then npm, pnpm or bun must be on `PATH` too. |
| Test a Wasm **browser** app | Chrome, Chromium, Edge, Brave or Firefox (Kargo runs it without a window; see [Testing in a browser](../../guides/wasm/#testing-in-a-browser)). `kargo run` only needs a browser to look at the page. |
| Make release **Wasm** modules smaller | [Binaryen](https://github.com/WebAssembly/binaryen)'s `wasm-opt` on `PATH`. Without it, release builds still remove dead code. |
| Build for **Android** | The [Android SDK](https://developer.android.com/studio) (set `ANDROID_HOME`). Kargo downloads Gradle itself. |
| Run on an **Android** device | `adb` from the SDK platform-tools, plus a device or emulator |
| Build **Kotlin/Native** | Nothing. Kargo downloads Kotlin/Native and its LLVM toolchain on the first native build. |
| `kargo new` creating a git repository | `git` (optional; pass `--no-vcs` to skip) |

## 4. Shell completions (optional)

`kargo completions <shell>` prints a completion script for `bash`, `zsh`, `fish`, `powershell` or `elvish`. Load it from your shell's startup file:

| Shell | Add to | Line |
|---|---|---|
| bash | `~/.bashrc` | `source <(kargo completions bash)` |
| zsh | `~/.zshrc` | `source <(kargo completions zsh)` |
| fish | `~/.config/fish/config.fish` | `kargo completions fish \| source` |
| PowerShell | `$PROFILE` | `kargo completions powershell \| Out-String \| Invoke-Expression` |
| elvish | `~/.config/elvish/rc.elv` | `eval (kargo completions elvish \| slurp)` |

Besides commands and flags, <kbd>Tab</kbd> completes values that depend on where you are: the current project's [tasks](../../guides/tasks-and-plugins/#tasks), `kargo-*` plugins, targets for `-t`, framework names for `--with` and `--framework`, and installed toolchains for `kargo toolchain remove`. The script asks the `kargo` on your `PATH` for them, so it keeps working when you update Kargo.

## Where Kargo stores things

Everything Kargo downloads goes to `~/.kargo` (override with `KARGO_HOME`):

```text
~/.kargo/
├── cache/        Maven artifacts and metadata
├── toolchains/   JDKs, Kotlin/Native distributions and their LLVM dependencies
└── bin/          kargo-<name> subcommand plugins
```

To uninstall, run `cargo uninstall kargo` and delete `~/.kargo`.

## Next steps

Continue with the [Quick start](../quick-start/).
