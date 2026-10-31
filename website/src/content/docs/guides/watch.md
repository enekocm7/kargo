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

`--watch` starts JVM, Native, Wasm and JS programs the same way (Wasm and JS programs run on Node.js). It does not run Android apps, which run inside Gradle. Wasm and JS browser apps are [served and reloaded](#reload-a-browser-app-on-change) instead of restarted. `--watch` conflicts with `--debug`, because a debugger would have to attach again after every restart.

For a JVM program, `kargo run --hot` watches the same way but does not restart it: it swaps the changed classes into the running one, which keeps its state. See [Hot reload a JVM program](#hot-reload-a-jvm-program).

## Hot reload a JVM program

A restart loses the program's state and takes seconds. `kargo run --hot` (`--hot-reload` for short: both are the same flag) keeps the JVM process alive and swaps the code that changed into it:

```console
$ kargo run --hot
     Watching 3 files for changes (press Ctrl+C to stop)
   Compiling watchdemo v0.1.0 (jvm)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 11.44s
     Running `java com.example.watchdemo.MainKt`
tick 4812 first
tick 4812 first
     Changed src/Main.kt
   Compiling watchdemo v0.1.0 (jvm)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.68s
    Reloaded 1 class in 38 ms
tick 4812 second
```

It watches and rebuilds like [`--watch`](#restart-a-program-on-change), with the same options (`--clear`, `--ignore`), and a build that fails leaves the program as it was. After a successful build, Kargo tells a small Java agent that the program starts with (`-javaagent`) to redefine the loaded classes whose files changed, using the JVM's class redefinition API (the one debuggers use for "hot swap"). The next call of a changed function runs the new body in the same process, so fields, caches, connections and threads stay as they are. The line `Reloaded 3 classes in 180 ms` says what happened.

What reloads depends on the JVM:

| Change | Standard JDK | JetBrains Runtime |
| --- | --- | --- |
| The body of a function, a lambda or an accessor, and a new class it uses | Reloads | Reloads |
| A new function, lambda or field, or a changed signature or supertype | Restarts | Reloads |
| A new dependency, a changed JVM option or `kargo.toml` edit that changes the command line | Restarts | Restarts |

When a change cannot be applied in place, Kargo says why and starts the program again, the way `--watch` does:

```console
        note: restarting the app, as the change cannot be reloaded: UnsupportedOperationException: class redefinition failed: attempted to add a method
     Running `java com.example.watchdemo.MainKt`
```

The program runs on the JDK of your project. If that JDK is [JetBrains Runtime](https://github.com/JetBrains/JetBrainsRuntime), Kargo turns on its enhanced class redefinition (`-XX:+AllowEnhancedClassRedefinition`); set `[toolchain] jdk` or `JAVA_HOME` to one to get it. A Compose Desktop app is the exception: Kargo downloads JetBrains Runtime for it, see [Compose hot reload](../compose/#hot-reload).

A running program does not run code again just because its class changed. A value computed once at startup, such as a route table, a `val` in an `object` or a bean wired when the application started, keeps its old value until the program restarts; press Ctrl+C and run it again when that matters. Servers built on a framework mostly call into your handlers on each request, so their handlers reload; see [Ktor](../ktor/#reload-while-you-edit) and [Spring Boot](../spring-boot/#reload-while-you-edit).

The program runs from a copy of its classes in `target/<profile>/jvm/hot-reload/`, which Kargo updates only after a build succeeds (the compiler rewrites `classes/` while a program is loading from it). `agent.log` there records each reload.

`--hot` works for the jvm target and the project's own program (`--main` and `--bin` too), not for an `--example`. It conflicts with `--debug`. It also does not reload code that comes from jars, such as a path dependency's classes.

## Reload a browser app on change

For a Wasm or JS browser app, `kargo run --watch` builds the app, serves it and keeps serving it. After each successful build, the pages that are open in a browser reload by themselves:

```console
$ kargo run -t js --watch --open
     Watching 4 files for changes (press Ctrl+C to stop)
   Compiling hello v0.1.0 (js)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 9.12s
     Serving target/debug/js at http://127.0.0.1:8080/ (reloading on change, press Ctrl+C to stop)
     Changed src/Main.kt
   Compiling hello v0.1.0 (js)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.85s
   Reloading the open pages
```

The server starts once, so the address stays the same, and `--port` and `--open` work as in [`kargo run`](../../reference/commands/#kargo-run-r) (`--open` opens the page once, not after every build). Kargo watches the Kotlin sources, the TypeScript and JavaScript modules in `js/`, the resources and `kargo.toml`, as for any other program.

A build that fails prints its errors and leaves the page as it is; the next successful build reloads it. The pages listen to a server-sent events stream on `/__kargo/reload`, which the served HTML pages get a small script for. Kargo adds the script to the pages it serves and not to the files in `target/`, and only with `--watch`.

With a [`bundler`](../wasm/#bundling) and `bundle = true`, each build bundles the app again (an unchanged build does not) and the open pages reload onto the new bundle, so you develop against what a release serves. The [dev server's proxy](../wasm/#forwarding-api-requests) works with `--watch` too.

The page reloads completely, so its state is lost. [Hot reload](#hot-reload-a-jvm-program) swaps code without a restart, but it is for the JVM.

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

- **Android apps** are installed again and their log followed, but the app is not reloaded in place.
- **Browser apps** reload the whole page; the state of the page is lost.
- **Windows and native programs:** Windows does not let a running executable be replaced. While a native program runs, the next build that links it fails with the linker's error, and the running program keeps running. Stop watching with Ctrl+C, then start `kargo run --watch` again. This is not tested on every setup; if you hit it, report it.
- **Changes are seen up to half a second late**, because of the polling. Every watched file is read once when watching starts, so a large `resources/` folder makes the start slower.
- **A program that writes into a watched directory** restarts itself: each file it writes is a change. Write such files somewhere else, or leave them out with `--ignore`.
- **`kargo watch` waits for each command.** A command that keeps running blocks the watch. Use `kargo run --watch` for programs.
