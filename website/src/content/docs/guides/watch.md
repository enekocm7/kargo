---
title: Watch mode
description: Restart a program, rerun tests or run any kargo command whenever the project's files change.
---

`kargo run --watch` rebuilds and restarts the program when you save a file. `kargo test --watch` reruns the tests, and `kargo watch` runs any kargo command you give it. Press Ctrl+C to stop.

```sh
kargo run --watch
kargo test --watch MathTest
kargo watch -x "test -t jvm" -x run
```

## Restart a program on change

`kargo run --watch` builds the program, starts it and waits. When a watched file changes, it builds again:

- If the build succeeds, the program that is running stops and the new one starts.
- If the build fails, the compiler errors are printed and the running program keeps running. The next successful build replaces it.

```console
$ kargo run --watch
     Watching 3 files for changes (press Ctrl+C to stop)
   Compiling watchdemo v0.1.0 (jvm)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 11.44s
     Running `java com.example.watchdemo.MainKt`
Hello, Kotlin!
     Changed src/Main.kt
   Compiling watchdemo v0.1.0 (jvm)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.68s
     Running `java com.example.watchdemo.MainKt`
Hi, Kotlin!
```

Arguments after `--` go to the program on every start: `kargo run --watch -- --port 9000`. `--bin`, `--main` and `--example` work the same way.

On Unix, a program is first sent SIGTERM and given five seconds to exit, so a server can close its sockets and run its shutdown hooks. Only then is it killed. On Windows it is terminated at once.

`--watch` starts JVM, Native and Wasm programs the same way (Wasm programs run on Node.js). It does not run Android apps, which run inside Gradle, or Wasm browser apps, which `kargo run` serves until Ctrl+C. `--watch` conflicts with `--debug`, because a debugger would have to attach again after every restart.

## Rerun tests on change

`kargo test --watch [filter]` compiles and runs the tests, then runs them again after every change. It takes the same options as [`kargo test`](../../reference/commands/#kargo-test-t), except `--list` and `--debug`. A failing test does not stop watching:

```console
$ kargo test --watch
     Watching 3 files for changes (press Ctrl+C to stop)
...
[         1 tests successful      ]
     Changed tests/GreetingTest.kt
...
error: test failed
```

## Run any command on change

`kargo watch` runs a kargo command once and again after every change. Without a command it runs `build`:

```sh
kargo watch                          # kargo build
kargo watch check                    # kargo check
kargo watch -x "test -t jvm" -x run  # two commands, in order
```

Each `-x` is a command line split on spaces, so `-x "test -t jvm"` runs `kargo test -t jvm`. Quotes are not interpreted, so an argument with a space cannot be given this way. When a command fails, the commands after it wait for the next change.

`kargo watch` waits for each command to finish before it watches again. A command that does not end, such as `run`, therefore keeps it from seeing changes. For a program that should restart on change, use `kargo run --watch`.

## What is watched

For every package of the project, members and path dependencies included, Kargo watches:

- its `kargo.toml`, the workspace's `kargo.toml` and `kargo.lock`;
- its source and test directories: `src/`, `src-<target>/`, `tests/`, `tests-<target>/`, `test-fixtures/`, `test-fixtures-<target>/` and `examples/`;
- `resources/`, `test-resources/` and `android/`;
- `interop/`, the `.def` files of `[target.native.cinterop]` and the C sources and include directories of `[target.native.c-sources]`;
- its `.env` files: `.env`, `.env.<profile>` and their `.local` variants;
- the files that the `inputs` of its [`[[generate]]` steps](../code-generation/) match, so a change to a `.proto` file generates the code again.

Directories that do not exist yet are watched too, so the first file added to one is seen. Other files are not watched. A change to a file outside these places does not start a run.

Some files are left out even inside those directories: anything under `target/`, anything whose name starts with a dot (such as `.git` and editor swap files), anything matched by `.gitignore` in the project root, and anything matched by `--ignore`. `--ignore` takes a gitignore-style pattern and can be repeated:

```sh
kargo run --watch --ignore "*.generated.kt" --ignore "generated/"
```

`--clear` clears the terminal before each run. It only has an effect when Kargo's status output goes to a terminal.

:::note
`--clear` and `--ignore` need `--watch`. `kargo watch` takes them directly.
:::

## How changes are found

Kargo checks the files about twice a second instead of subscribing to file system events. Polling behaves the same on Windows, macOS and Linux, and it notices editors that save by renaming a file. A file is read again only when its size or modification time changes, and a rewrite with the same contents does not start a run.

Saving often comes as a burst of writes. Kargo waits until the files have been still for a quarter of a second, then runs once. A change made while a run is in progress starts the next run as soon as this one ends. Kargo's own writes to `kargo.lock` during a run do not count as changes.

## Limits

- **Android and Wasm browser apps** are not restarted. Run them with `kargo run` again after a change.
- **Windows and native programs:** Windows does not let a running executable be replaced. While a native program runs, the next build that links it fails with the linker's error, and the running program keeps running. Stop watching with Ctrl+C, then start `kargo run --watch` again. This is not tested on every setup; if you hit it, report it.
- **Changes are seen up to half a second late**, because of the polling. Every watched file is read once when watching starts, so a large `resources/` folder makes the start slower.
- **A program that writes into a watched directory** restarts itself: each file it writes is a change. Write such files somewhere else, or leave them out with `--ignore`.
- **`kargo watch` waits for each command.** A command that keeps running blocks the watch. Use `kargo run --watch` for programs.
