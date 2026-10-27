---
title: Continuous integration
description: Build and test Kargo projects on GitHub Actions and other CI systems.
---

Kargo is a single binary, so CI is simple: install it, cache `~/.kargo`, and run `kargo test --locked`.

## Generating a workflow

`kargo init-ci` writes `.github/workflows/ci.yml` for the project:

```sh
kargo init-ci            # add --junit for the test report step
```

It runs `fmt --check`, `lint`, `check --tests --locked`, `build --locked`, `test --locked` and `package --release --locked`, and reads `kargo.toml` for the setup around them:

- `actions/setup-java` only when a package does not set `[toolchain] jdk`. With `--locked`, Kargo doesn't download a JDK for a project on the system JDK by itself, so the job needs one on `PATH` or a pinned one.
- `actions/setup-node` when a package has a Wasm or JS target.
- An `ubuntu-latest`, `macos-latest` and `windows-latest` matrix when a package has a Kotlin/Native target.
- `--junit` runs `kargo test --report junit` and publishes the reports (see [Test results](#test-results)).

The command prints each step it left out and why. It refuses to replace an existing workflow unless you pass `--force`; `--system` picks the CI system, and `github` is the only one so far.

### The setup-kargo action

The workflow installs Kargo with the `setup-kargo` action of this repository. It installs the prebuilt release (`version` defaults to the latest; the generated workflow pins the version of the Kargo that wrote it), puts `kargo` on `PATH`, and restores and saves the cache of `~/.kargo` (or `$KARGO_HOME`) without `daemon/`, keyed on the runner OS, `kargo.lock` and `kargo.toml`. It works on Linux, macOS and Windows runners.

```yaml
      - uses: enekocm7/kargo/setup-kargo@master
        with:
          version: 0.1.0   # optional; `cache: 'false'` turns the cache off
```

Pin the action to a tag or commit if you want it to change only when you decide. The workflow below is what you write by hand without the action.

## GitHub Actions

```yaml title=".github/workflows/ci.yml"
name: CI
on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - uses: actions/setup-java@v4      # skip this if kargo.toml pins a [toolchain] jdk
        with:
          distribution: temurin
          java-version: 21

      - name: Cache Kargo
        uses: actions/cache@v4
        with:
          path: |
            ~/.kargo
            !~/.kargo/daemon
          key: kargo-${{ runner.os }}-${{ hashFiles('kargo.lock', 'kargo.toml') }}
          restore-keys: kargo-${{ runner.os }}-

      - name: Install Kargo
        run: |
          curl -fsSL https://enekocm7.github.io/kargo/install.sh | KARGO_VERSION=0.1.0 sh -s -- --no-modify-path
          echo "$HOME/.kargo/bin" >> "$GITHUB_PATH"

      - run: kargo fmt --check
      - run: kargo lint
      - run: kargo check --tests --locked
      - run: kargo build --locked
      - run: kargo test --locked
      - run: kargo package --release --locked

      - uses: actions/upload-artifact@v4
        with:
          name: jar
          path: target/release/jvm/*-all.jar
```

`--locked` makes the build fail if `kargo.lock` is out of date, so nobody forgets to commit it. It covers the Kotlin compiler, its plugins and the JUnit launcher too, which `kargo.lock` pins as [build tools](../../reference/lockfile/#build-tools); run `kargo fetch --tools` before committing if CI runs `kargo doc`, `fmt` or `lint` with `--locked`. With `--locked`, Kargo also refuses to send [repository credentials](../dependencies/#repositories) over plain `http://`. Add `--require-checksums` (or set `KARGO_REQUIRE_CHECKSUMS=1` for the job) to also fail on a newly resolved artifact whose repository publishes no checksum. A Kargo upgrade that changes how dependencies resolve also counts as out of date: build once locally without `--locked` and commit the new lockfile (see [`kargo.lock`](../../reference/lockfile/)). `kargo fmt --check` and `kargo lint` fail the job on unformatted code and detekt issues; see [Formatting and linting](../formatting-and-linting/).

If the project has [`[[generate]]` steps](../code-generation/), `kargo build` and the other commands run them first, on the CI machine's own `target/`. Run `kargo generate` on its own to find a broken generator before anything compiles.

The first build of the job starts a [compiler daemon](../build-performance/#the-compiler-daemon) that the later steps reuse, so `check`, `build`, `test` and `package` each skip JVM startup. Leave `~/.kargo/daemon` out of the cache: it only holds that run's daemon state and large class-data archives. Set `KARGO_DAEMON=false` if your CI doesn't allow background processes.

### Test results

`kargo test --report junit` writes JUnit XML reports to `target/<profile>/<target>/test-reports/` (see [Test reports for CI](../testing/#test-reports-for-ci)). Upload them so failed tests show up by name in the run summary, for example with a JUnit report action:

```yaml
      - run: kargo test --locked --report junit
      - uses: mikepenz/action-junit-report@v5
        if: always()
        with:
          report_paths: target/*/*/test-reports/*.xml
```

`if: always()` publishes the results when tests fail, which is when you need them. In GitLab, list the same paths under `artifacts: reports: junit:`; in Jenkins, pass them to the `junit` step.

### Benchmarks

`kargo bench --format json` prints JMH's JSON results on stdout, and `--save <name>` / `--baseline <name>` keep and compare results in `target/bench/` (see [Benchmarking](../benchmarking/)). Benchmarks need a quiet machine: run them in a job of their own, not next to tests, and compare only runs from the same kind of runner.

To compare a change with the main branch, save a baseline on `main` and restore it on pull requests, for example with the CI's cache:

```yaml
      - uses: actions/cache@v4
        with:
          path: target/bench
          key: bench-${{ runner.os }}-${{ github.run_id }}
          restore-keys: bench-${{ runner.os }}-
      # On the main branch:
      - run: kargo bench --locked --save main
        if: github.ref == 'refs/heads/main'
      # On pull requests:
      - run: kargo bench --locked --baseline main --format json > bench.json
        if: github.event_name == 'pull_request'
```

`kargo bench` exits with `101` when a benchmark fails or throws, and `0` when one just got slower, so decide from `bench.json` (or the report on stderr) whether a slowdown fails the build.

### Security advisories

`kargo audit` checks every dependency against the [OSV](https://osv.dev) advisory database and exits with `101` when one has an advisory of `high` severity or worse (see [`kargo audit`](../../reference/commands/#kargo-audit)). Run it in a job of its own, so a new advisory fails that job and not your tests, and on a schedule as well, since advisories appear without any change on your side:

```yaml title=".github/workflows/audit.yml"
name: Audit
on:
  push:
  pull_request:
  schedule:
    - cron: "0 6 * * 1"     # every Monday

jobs:
  audit:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: enekocm7/kargo/setup-kargo@master
      - run: kargo audit --locked --tooling
```

`--locked` audits exactly what `kargo.lock` pins. `--deny <severity>` (or [`[audit] deny`](../../reference/manifest/#audit)) moves the bar, and `--format json` gives the findings to other tools. An advisory that does not affect you goes in [`[audit] ignore`](../../reference/manifest/#audit) with a reason, and an `until` date so it is looked at again. This is the only step that contacts `api.osv.dev`; with `--offline` it audits from the advisories cached under `~/.kargo/advisories`.

### Several operating systems

Kotlin/Native and Compose Desktop produce OS-specific output. Use a matrix:

```yaml
jobs:
  test:
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
    runs-on: ${{ matrix.os }}
```

### Native executables and installers

[`kargo package --format jlink`, `native-image` and `installer`](../distributing/) build for the OS they run on, so build each platform's on its own runner:

```yaml
jobs:
  release:
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      # ... cache and install Kargo as above; pin [toolchain] jdk and graalvm in kargo.toml
      - run: kargo package --release --locked --format native-image
      - run: kargo package --release --locked --format installer
      - uses: actions/upload-artifact@v4
        with:
          name: app-${{ matrix.os }}
          path: |
            target/release/jvm/package/*
            !target/release/jvm/package/*-installer
```

With `type = ["msi", "dmg", "deb"]` in [`[package.installer]`](../../reference/manifest/#packageinstaller), each runner builds its own OS's installer from the same `kargo.toml`. For a [Compose Desktop](../compose/#distributing) application, run `kargo package --release --locked --desktop` instead.

To sign and notarize the macOS installer, import the Developer ID certificate into the runner's keychain in an earlier step and pass the notarization credentials as secrets, under the names `notarize` reads:

```yaml
      - run: kargo package --release --locked --desktop
        env:
          APPLE_ID: ${{ secrets.APPLE_ID }}
          APPLE_APP_PASSWORD: ${{ secrets.APPLE_APP_PASSWORD }}
```

GitHub's Windows runners come with Visual Studio, which `native-image` needs. `jpackage` needs the [WiX Toolset](https://wixtoolset.org) for `.msi` files: install it in a step if the runner image doesn't list it. The Linux runners have `dpkg-deb` and `fakeroot` for `.deb` files. Add `~/.kargo/toolchains/graalvm` to the cache only if downloading GraalVM on each run is too slow: it is several hundred megabytes.

### Kotlin/Native cache

Debug builds of a native target link a precompiled [standard library cache](../native/#build-speed) that Kotlin/Native builds on first use, which takes one to two minutes per platform. It lives under `~/.kargo/toolchains/kotlin-native/<version>/klib/cache`, so the `~/.kargo` cache above keeps it between runs. If your CI starts from an empty home every time, building it costs more than it saves; set `KARGO_NATIVE_CACHE=false` for those jobs.

Kotlin/Native also downloads several hundred megabytes of LLVM and sysroots for each platform, into `~/.kargo/toolchains/konan`. Run `kargo toolchain install` as a setup step to download them for every platform the runner can build before the build starts (and before `pre-build` tasks that use the bundled clang); the `~/.kargo` cache keeps them too.

### Jobs that share a home directory

Jobs on one runner that share `~/.kargo` can run in parallel. Kargo locks each toolchain while it installs it, so when two jobs need the same JDK, the second waits for the first instead of deleting or repeating its download. See [Parallel installs and retries](../toolchains/#parallel-installs-and-retries).

### Wasm and JS

Add Node.js so Wasm and JS tests can run. It also provides npm, which Kargo uses to install [npm packages](../wasm/#npm-packages) for Wasm dependencies (pnpm and bun work too):

```yaml
      - uses: actions/setup-node@v4
        with:
          node-version: 22
```

Tests of a Wasm or JS project with `environment = "browser"` run in a headless browser instead. GitHub's `ubuntu-latest`, `macos-latest` and `windows-latest` runners come with Chrome, which Kargo finds. On a container image, install a browser and pass the flags it needs through `KARGO_BROWSER_ARGS`, for example `--no-sandbox` when running as root. See [Testing in a browser](../wasm/#testing-in-a-browser).

For release builds, Kargo downloads its pinned Binaryen `wasm-opt` into the toolchain cache and verifies its SHA-256 automatically. CI needs no global npm install. See [Wasm release builds](../wasm/#release-builds) for optimization presets, compression and size budgets.

### Android

With [`[toolchain] android-sdk`](../android/#android-sdk), Kargo installs the Android SDK itself, but only after someone accepts Google's license terms, and a CI job has no terminal to ask on. Accept them in a step before the build:

```yaml
      - run: kargo toolchain install android-sdk --accept-licenses
      - run: kargo build -t android --release
```

Without it, the build stops with a message that names this command. The SDK lands in `~/.kargo/toolchains/android-sdk`, which the `~/.kargo` cache keeps, with the acceptance.

### Publishing releases

`kargo publish` uploads a library from CI, with the Maven Central token and the signing key in secrets. See [Publishing from CI](../publishing/#publishing-from-ci) for a complete workflow.

## Other CI systems

The steps are the same everywhere:

1. Install Kargo: `curl -fsSL https://enekocm7.github.io/kargo/install.sh | KARGO_VERSION=<version> sh -s -- --no-modify-path`, then put `~/.kargo/bin` on `PATH` (on Windows, `$env:KARGO_VERSION = '<version>'; irm https://enekocm7.github.io/kargo/install.ps1 | iex`). Pin the version so a new release cannot change your build, and set `[toolchain] kargo = ">=<version>"` in `kargo.toml` so a developer's older Kargo says so.
2. Provide a JDK, or pin one with `[toolchain] jdk`. Without either, Kargo downloads Temurin 21 itself, but not with `--locked` or `--offline`: there it stops and names the `kargo toolchain install jdk` command to run (see [Without a JDK](../toolchains/#without-a-jdk)).
3. Cache `~/.kargo` (or `$KARGO_HOME`), except its `daemon/` directory.
4. Run `kargo test --locked`, adding `--report junit` if the CI shows JUnit XML results.

Kargo exits with status `101` on build and test failures, like Cargo.

`kargo run` (and `kargo script` and `kargo-<name>` plugins) pass the program's exit code through unchanged, as a full 32-bit value on Windows, so `System.exit(1000)` or a crash status such as `0xC0000005` survives. On Linux and macOS a program killed by a signal exits with `128 + n`, as in a shell (`139` for SIGSEGV, `137` for SIGKILL), and Kargo prints `process terminated by signal 11 (SIGSEGV)`. Because `101` is also what Kargo itself uses for build and test failures, a wrapper that must tell "the program returned 101" from "the build failed" should run `kargo build` first and then `kargo run`.

## Useful flags

| Flag | Why in CI |
|---|---|
| `--locked` | Fail instead of silently changing `kargo.lock`, and check every cached artifact against it |
| `--offline` | Make sure a step uses only what's cached |
| `KARGO_AUTO_INSTALL=0` | Fail instead of downloading a toolchain the cache doesn't have (a JDK, Kotlin/Native, Gradle), with the `kargo toolchain install` command to run. See [What installs automatically](../toolchains/#what-installs-automatically) |
| `kargo fetch` | Download everything in a step of its own (add `--tools` for the doc, fmt and lint tools), so the steps after it can use `--offline` |
| `--vendored` | Build from a committed `vendor/` that [`kargo vendor`](../offline-and-mirrors/#vendoring-dependencies) wrote, without the public repositories |
| `~/.kargo/config.toml` | Send downloads through your company's [mirrors and proxy](../offline-and-mirrors/) |
| `kargo test --no-fail-fast` | Test every target and print a result per target instead of stopping at the first failure (or set [`[test] fail-fast = false`](../../reference/manifest/#test)) |
| `KARGO_JOBS=1` or `-j 1` | Build one workspace member and one target at a time, in the same order on every run, so logs compare line by line. A small number also keeps a runner with little memory from swapping; the default already counts the free memory. See [Limiting parallelism](../build-performance/#limiting-parallelism) |
| `kargo test -j <n>` | Test `<n>` workspace members at the same time (one test JVM each) instead of one after another |
| `kargo build --keep-going` | Build every target (and, in a workspace, every member whose dependencies built) and report all failures, not just the first |
| `kargo test --report junit` | Write JUnit XML reports for the CI's test summary |
| `kargo audit` | Fail on dependencies with known security advisories. See [Security advisories](#security-advisories) |
| `[test] timeout` | Stop a hung test run and fail it, rather than waiting for the CI's own job limit. See [Timeouts](../testing/#timeouts) |
| `kargo bench --format json` | JMH's results as JSON on stdout, for tracking performance. See [Benchmarks](#benchmarks) |
| `kargo test --failed` | Rerun only the JVM test classes that failed in the last run, on a retry job |
| `--message-format json` | Compiler diagnostics as JSON, to annotate them on the changed lines. See [Machine-readable output](../../reference/machine-readable-output/) |
| `-q`, `--quiet` | Less log noise |
| `-v`, `--verbose` | Print the exact compiler commands when debugging |
