---
title: Installing command-line tools
description: Put a Kotlin command-line tool on your PATH with kargo install, from a project, a git repository or Maven, and remove it with kargo uninstall.
---

`kargo install` builds a Kotlin program (or downloads a published one) and puts a launcher for it on your `PATH`, the way `cargo install` does for Rust tools. After that, the tool runs from any directory with no `java -jar` and no wrapper script of your own.

```console
$ kargo install --path .                      # the project in the current directory
$ kargo install --git https://github.com/user/tool --tag v1.2
$ kargo install com.example:tool              # a published application, newest version
$ kargo install com.example:tool:1.2.0        # a specific version
$ kargo install --list                        # what is installed
$ kargo uninstall tool
```

## Where things go

Launchers go to `~/.kargo/bin` (`$KARGO_HOME/bin`), the directory `kargo-<name>` [plugins](../tasks-and-plugins/) already live in. A program's files go to `~/.kargo/install/<name>/<version>/`, and `~/.kargo/install/.installed.toml` records what was installed, where from, and which launchers belong to it.

Put `~/.kargo/bin` on your `PATH` once. When it isn't there, `kargo install` says so after it finishes and shows how:

```console
$ kargo install --path .
   Compiling tool v0.1.0 (jvm)
    Finished `release` profile [optimized] target(s) in 9.12s
  Installing /home/me/.kargo/bin/tool
   Installed package `tool v0.1.0` (executable `tool`)
warning: /home/me/.kargo/bin is not in your PATH; add it to run the installed programs
note: add `export PATH="/home/me/.kargo/bin:$PATH"` to your shell's startup file
```

`--root <dir>` (or [`KARGO_INSTALL_ROOT`](../../reference/environment/)) installs below `<dir>/bin` and `<dir>/install` instead, for a CI job or a directory you manage yourself. Pass the same root to `--list`, `uninstall` and `--force`.

## What gets installed

Projects, from `--path` or `--git`, are built with the `release` profile. The program installed is:

| Target | What you get |
|---|---|
| JVM (the default when it is enabled) | The self-contained jar, as `kargo package` builds it, and a launcher that runs it with `java -jar`. The launcher names the JDK the build used by its path, so changing `JAVA_HOME` later does not break the tool. |
| JVM with `--format jlink` | A runtime image (see [distributing applications](../distributing/#runtime-images)) and a launcher for it. No Java is needed to run it. |
| Native (`--target native`, or when it is the only one) | The release executable for this machine, copied to `~/.kargo/bin` as it is. No JVM is involved. A Native target needs `"host"` in its `platforms`. |

The launcher is a shell script on Linux and macOS and a `.cmd` file on Windows, and passes its arguments and exit code through.

A project with several programs installs every entry point: its own `main` function and each [`[[bin]]`](../multiple-programs/) with a JVM main class (or a native entry point, for the Native target). `--bin <name>` (repeatable) installs only those:

```console
$ kargo install --path . --bin migrate
```

`kargo.lock` is used when the source has one. `--locked` fails the install if it is out of date. In a workspace, `-p <member>` picks the member to install.

## From git

```console
$ kargo install --git https://github.com/user/tool              # the default branch
$ kargo install --git https://github.com/user/tool --tag v1.2
$ kargo install --git https://github.com/user/tool --branch next
$ kargo install --git https://github.com/user/tool --rev 1a2b3c4
```

Kargo clones the repository with `git` (it must be on your `PATH`) into a temporary directory, builds the project at its root and removes the clone. The commit is recorded in `.installed.toml` and shown by `--list`.

## From Maven

`group:artifact[:version]` installs an application published to Maven Central, or to a repository you name with `--repository <url>` (repeatable, looked up before Central):

```console
$ kargo install com.example:tool --repository https://repo.example.com/releases
```

Kargo resolves the module and its runtime dependencies for the JVM, copies the jars into `~/.kargo/install/<artifact>/<version>/lib/` and writes a launcher that runs the main class. It finds the class in the `Main-Class` of the jar's manifest, or in the `kargo.main-class` property of the POM, which [`kargo publish`](../publishing/) writes for a package with a `main` function. A library that names no class cannot be installed. The launcher uses the JDK Kargo finds through `JAVA_HOME` or `PATH`.

## Upgrading, replacing and removing

Installing a program that is installed already fails, so a tool is never replaced by accident. `--force` replaces it:

```console
$ kargo install --path . --force
```

Naming an installed program installs it again from where it came from, which upgrades a tool installed from the newest version of a Maven module, a branch or a path:

```console
$ kargo install --force tool
```

A Maven version, a git tag or a `--rev` that you pinned stays pinned. The other versions of a program are deleted once the new one is in place.

`kargo install` also refuses to overwrite a launcher it did not write, and a launcher that belongs to another installed program; remove that file or pass `--force` for the first.

```console
$ kargo install --list
tool v0.1.0 (/home/me/tool):
    tool
    migrate
$ kargo uninstall tool
     Removed package `tool v0.1.0`
```

`kargo uninstall` removes the launchers, the program's files and its entry in `.installed.toml`.
