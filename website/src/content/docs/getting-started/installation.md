---
title: Installation
description: Install the Kargo CLI and the tools it relies on.
---

Kargo is a single binary written in Rust. Install it, then make sure a JDK is available (or let Kargo download one).

## 1. Install Kargo

Kargo ships as a single prebuilt binary. You don't need Rust. Pick the method that fits your machine.

### The install script

On Linux and macOS:

```sh
curl -fsSL https://enekocm7.github.io/kargo/install.sh | sh
```

On Windows, in PowerShell:

```powershell
irm https://enekocm7.github.io/kargo/install.ps1 | iex
```

The scripts download the archive for your platform from the [GitHub release](https://github.com/enekocm7/kargo/releases), check its SHA-256 against the release's `SHA256SUMS`, and put `kargo` in `~/.kargo/bin` (`%USERPROFILE%\.kargo\bin` on Windows). Then they offer to add that directory to your `PATH`: `install.sh` appends a marked block to your shell startup files (`~/.profile`, and `~/.bashrc`, `~/.bash_profile`, `~/.zshenv` or fish's `conf.d` when you use them), `install.ps1` edits your user `PATH`. Neither touches `PATH` without asking; when there is no terminal to ask on (a CI job), they leave it alone.

Both read options from the environment, because `irm | iex` cannot take arguments:

| Option | `install.sh` | `install.ps1` |
|---|---|---|
| Install a version instead of the latest | `KARGO_VERSION=0.3.0` or `--version 0.3.0` | `$env:KARGO_VERSION = '0.3.0'` |
| Leave `PATH` alone | `--no-modify-path` | `$env:KARGO_NO_MODIFY_PATH = '1'` |
| Edit `PATH` without asking | `--yes` | `$env:KARGO_YES = '1'` |
| Install somewhere else than `~/.kargo` | `KARGO_HOME=/opt/kargo` | `$env:KARGO_HOME = 'D:\kargo'` |

To pass flags through the pipe, use `sh -s --`:

```sh
curl -fsSL https://enekocm7.github.io/kargo/install.sh | KARGO_VERSION=0.3.0 sh -s -- --no-modify-path
```

The scripts install builds for Linux (x86_64 and aarch64, glibc 2.35 or newer, or a static musl build on other distributions and Alpine), macOS (Intel and Apple Silicon) and Windows (x64 and arm64).

### Package managers

These use this repository directly:

| Tool | Command |
|---|---|
| Nix | `nix run github:enekocm7/kargo`, or `nix profile install github:enekocm7/kargo` (builds the flake in the repository from source; it has not been tested with a Nix build yet) |
| `cargo binstall` | `cargo binstall kargo-cli` (downloads the release archive instead of building, so it works once a release is published) |

Every release also builds the files that Homebrew, Scoop, winget and the AUR need (the `packages` artifact of the release workflow, rendered from `packaging/` by `packaging/render.sh`). They are submitted to those registries by hand, so Kargo is **not yet listed** in them; use the install script until it is. Once listed, the commands will be `brew install enekocm7/tap/kargo`, `scoop install kargo` after adding its bucket, `winget install enekocm7.kargo` and `yay -S kargo-bin`. SDKMAN! needs a vendor account and is not set up.

Kargo installed by a package manager is updated by that package manager, and `kargo self update` points you to its command instead of replacing the binary.

### From a downloaded archive

Every [release](https://github.com/enekocm7/kargo/releases) has these files, where `<target>` is one of `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc` or `aarch64-pc-windows-msvc`:

| File | Contents |
|---|---|
| `kargo-<version>-<target>.tar.gz` (`.zip` on Windows) | A folder of the same name holding `kargo` (`kargo.exe`) and the README |
| `SHA256SUMS` | The SHA-256 of every archive |
| `VERSION` | The version, so `releases/latest/download/VERSION` always names the newest release |

Unpack the archive and put `kargo` on your `PATH`. To verify what you downloaded, check the checksum, then the build provenance that the release workflow attests with GitHub:

```sh
sha256sum --check --ignore-missing SHA256SUMS
gh attestation verify kargo-0.3.0-x86_64-unknown-linux-gnu.tar.gz --repo enekocm7/kargo
```

### From source

If you have Rust, Cargo can build Kargo. You need **Rust 1.89 or newer**; install it with [rustup](https://rustup.rs).

```sh
cargo install --locked kargo-cli
```

This builds Kargo and puts the `kargo` binary in `~/.cargo/bin`, which rustup adds to your `PATH`. From a local clone:

```sh
git clone https://github.com/enekocm7/kargo
cd kargo
cargo install --locked --path .
```

### Updating

```sh
kargo self update
```

downloads the latest release, verifies its checksum and replaces the running binary (`kargo self update --version 0.3.0` installs a particular version, `--check` only tells you whether there is a newer one). Kargo installed from source with Cargo is updated by running `cargo install` again, and one installed by a package manager by that package manager. See [`kargo self`](../../reference/commands/#kargo-self).

Kargo looks for a newer release when you run `kargo --version`, at most once a day, and prints a note on stderr when there is one. `KARGO_NO_UPDATE_CHECK=1` turns that off; it never runs offline, in CI, or when stderr is not a terminal.

### Check that it works

```console
$ kargo --version
kargo 0.1.0
$ kargo --help
Cargo for Kotlin: a build tool and package manager
...
```

## 2. Get a JDK

The Kotlin compiler runs on the JVM, so Kargo needs a **JDK 11 or newer** for every target. JVM tests need **JDK 17 or newer**, because `kargo test` runs the JUnit 6 launcher on the project's JDK. You don't have to install one yourself.

**Use the JDK you already have.** Kargo looks at `JAVA_HOME`, then at `java` on your `PATH`.

```console
$ java -version
openjdk version "21.0.5" 2024-10-15 LTS
```

**Let Kargo download one.** On a machine with no JDK on `JAVA_HOME` or `PATH`, the first command that needs one (`kargo build`, `run`, `test`, ...) downloads an [Eclipse Temurin](https://adoptium.net) JDK 21, verifies its checksum and keeps it under `~/.kargo/toolchains`, so `kargo new app && cd app && kargo run` works on a fresh machine. To use the same JDK everywhere, whatever is installed, pin it in the project instead:

```toml title="kargo.toml"
[toolchain]
jdk = "21"
```

`KARGO_AUTO_INSTALL=0` turns automatic downloads off.

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
└── bin/          kargo itself (install script), kargo-<name> subcommand plugins and tools from kargo install
```

## Uninstalling

```sh
kargo self uninstall            # removes the binary and the PATH entries the install scripts added
kargo self uninstall --purge    # also deletes ~/.kargo: toolchains, the Maven cache, installed tools
```

Kargo installed with Cargo is removed with `cargo uninstall kargo`, and one installed by a package manager with that package manager. Without `--purge`, `~/.kargo` stays, so a later install finds its downloaded toolchains.

## Next steps

Continue with the [Quick start](../quick-start/).
