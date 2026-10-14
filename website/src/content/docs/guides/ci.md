---
title: Continuous integration
description: Build and test Kargo projects on GitHub Actions and other CI systems.
---

Kargo is a single binary, so CI is simple: install it, cache `~/.kargo`, and run `kargo test --locked`.

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
            ~/.cargo/bin/kargo
            ~/.kargo
            !~/.kargo/daemon
          key: kargo-${{ runner.os }}-${{ hashFiles('kargo.lock', 'kargo.toml') }}
          restore-keys: kargo-${{ runner.os }}-

      - name: Install Kargo
        run: command -v kargo || cargo install --locked --git https://github.com/enekocm7/kargo kargo

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

`--locked` makes the build fail if `kargo.lock` is out of date, so nobody forgets to commit it. A Kargo upgrade that changes how dependencies resolve also counts as out of date: build once locally without `--locked` and commit the new lockfile (see [`kargo.lock`](../../reference/lockfile/)). `kargo fmt --check` and `kargo lint` fail the job on unformatted code and detekt issues; see [Formatting and linting](../formatting-and-linting/).

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

### Kotlin/Native cache

Debug builds of a native target link a precompiled [standard library cache](../native/#build-speed) that Kotlin/Native builds on first use, which takes one to two minutes per platform. It lives under `~/.kargo/toolchains/kotlin-native/<version>/klib/cache`, so the `~/.kargo` cache above keeps it between runs. If your CI starts from an empty home every time, building it costs more than it saves; set `KARGO_NATIVE_CACHE=false` for those jobs.

### Wasm

Add Node.js so Wasm tests can run. It also provides npm, which Kargo uses to install [npm packages](../wasm/#npm-packages) for Wasm dependencies (pnpm and bun work too):

```yaml
      - uses: actions/setup-node@v4
        with:
          node-version: 22
```

Tests of a Wasm project with `environment = "browser"` run in a headless browser instead. GitHub's `ubuntu-latest`, `macos-latest` and `windows-latest` runners come with Chrome, which Kargo finds. On a container image, install a browser and pass the flags it needs through `KARGO_BROWSER_ARGS`, for example `--no-sandbox` when running as root. See [Testing in a browser](../wasm/#testing-in-a-browser).

For release builds, Binaryen's `wasm-opt` makes the Wasm module much smaller; `npm install -g binaryen` puts it on `PATH` (see [Release builds](../wasm/#release-builds)).

### Publishing releases

`kargo publish` uploads a library from CI, with the Maven Central token and the signing key in secrets. See [Publishing from CI](../publishing/#publishing-from-ci) for a complete workflow.

## Other CI systems

The steps are the same everywhere:

1. Install Rust, then `cargo install --locked --git https://github.com/enekocm7/kargo kargo`.
2. Provide a JDK, or pin one with `[toolchain] jdk`.
3. Cache `~/.kargo` (or `$KARGO_HOME`), except its `daemon/` directory.
4. Run `kargo test --locked`, adding `--report junit` if the CI shows JUnit XML results.

Kargo exits with status `101` on build and test failures, like Cargo.

## Useful flags

| Flag | Why in CI |
|---|---|
| `--locked` | Fail instead of silently changing `kargo.lock` |
| `--offline` | Make sure a step uses only what's cached |
| `kargo fetch` | Download everything in a step of its own (add `--tools` for the doc, fmt and lint tools), so the steps after it can use `--offline` |
| `kargo test --no-fail-fast` | Test every target and print a result per target instead of stopping at the first failure (or set [`[test] fail-fast = false`](../../reference/manifest/#test)) |
| `kargo build --keep-going` | Build every target and report all failures, not just the first |
| `kargo test --report junit` | Write JUnit XML reports for the CI's test summary |
| `--message-format json` | Compiler diagnostics as JSON, to annotate them on the changed lines. See [Machine-readable output](../../reference/machine-readable-output/) |
| `-q`, `--quiet` | Less log noise |
| `-v`, `--verbose` | Print the exact compiler commands when debugging |
