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

`--locked` makes the build fail if `kargo.lock` is out of date, so nobody forgets to commit it. `kargo fmt --check` and `kargo lint` fail the job on unformatted code and detekt issues; see [Formatting and linting](../formatting-and-linting/).

The first build of the job starts a [compiler daemon](../build-performance/#the-compiler-daemon) that the later steps reuse, so `check`, `build`, `test` and `package` each skip JVM startup. Leave `~/.kargo/daemon` out of the cache: it only holds that run's daemon state and large class-data archives. Set `KARGO_DAEMON=false` if your CI doesn't allow background processes.

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

### Wasm

Add Node.js so Wasm tests can run. It also provides npm, which Kargo uses to install [npm packages](../wasm/#npm-packages) for Wasm dependencies (pnpm and bun work too):

```yaml
      - uses: actions/setup-node@v4
        with:
          node-version: 22
```

## Other CI systems

The steps are the same everywhere:

1. Install Rust, then `cargo install --locked --git https://github.com/enekocm7/kargo kargo`.
2. Provide a JDK, or pin one with `[toolchain] jdk`.
3. Cache `~/.kargo` (or `$KARGO_HOME`), except its `daemon/` directory.
4. Run `kargo test --locked`.

Kargo exits with status `101` on build and test failures, like Cargo.

## Useful flags

| Flag | Why in CI |
|---|---|
| `--locked` | Fail instead of silently changing `kargo.lock` |
| `--offline` | Make sure a step uses only what's cached |
| `-q`, `--quiet` | Less log noise |
| `-v`, `--verbose` | Print the exact compiler commands when debugging |
