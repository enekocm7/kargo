---
title: API documentation
description: Generate HTML API documentation from your KDoc comments with kargo doc and Dokka.
---

`kargo doc` turns the KDoc comments in your sources into an HTML site, like `cargo doc` does for Rust. It runs [Dokka](https://kotlinlang.org/docs/dokka-introduction.html), the official Kotlin documentation engine, without Gradle (except to resolve the classpath of the [Android target](#android)).

```kotlin title="src/Main.kt"
package com.example.mylib

/** Returns a friendly greeting. */
fun greeting(name: String): String = "Hello, $name!"
```

```console
$ kargo doc --open
 Documenting mylib v0.1.0 (jvm)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 7.21s
   Generated /home/me/mylib/target/debug/doc/index.html
     Opening /home/me/mylib/target/debug/doc/index.html
```

The site is written to `target/debug/doc/`. Open `index.html` in any browser, or pass `--open` to open it for you. `kargo doc` replaces the directory on every run, so pages for deleted code don't linger.

## What gets documented

`kargo doc` documents public and protected declarations of your main sources (not tests). Comments use the [KDoc syntax](https://kotlinlang.org/docs/kotlin-doc.html): Markdown, `[links]` to other declarations, and block tags such as `@param`, `@return` and `@throws`. Java files on the JVM are documented too.

Types from your dependencies, the Kotlin standard library and the JDK are resolved against the same libraries `kargo build` uses. Standard library and JDK types link to their online documentation. JDK links point to the Java version of your `jvm-target`.

## Multiplatform projects

`kargo doc` documents every enabled target. Pick some with `--target`:

```sh
kargo doc                 # every enabled target
kargo doc --target jvm    # only the JVM API
```

With one target, `src/` and `src-<target>/` form a single source set. With several, `src/` becomes a `common` source set that the `src-jvm/`, `src-wasm/`, `src-native/` and `src-android/` source sets depend on. Each declaration is then shown with the platforms it is available on, and an `expect` declaration is shown together with its `actual` implementations.

```console
$ kargo doc
 Documenting mp v0.1.0 (common, jvm, wasm)
```

On Native, the API is documented for the first platform in `[target.native] platforms`. Documenting Native downloads the Kotlin/Native distribution, like a Native build does.

Common code is analysed against the Wasm or Native libraries when one of those targets is enabled. In a project with only JVM and Android targets, types from dependencies used in `src/` show up unresolved; move that code to `src-jvm/` and `src-android/`, or document one target with `--target`.

## Android

Android sources in `src/` and `src-android/` are documented like the others. The difference is where the classpath comes from: Android dependencies are resolved by Gradle, so `kargo doc` first runs a small task in the generated Gradle project under `target/android`. It collects `android.jar` for your `compile-sdk` and the classes of every dependency, with AARs unpacked by the Android Gradle Plugin.

```console
$ kargo doc
   Resolving android classpath (gradle)
 Documenting droid v0.1.0 (android)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 7.72s
   Generated /home/me/droid/target/debug/doc/index.html
```

This needs the same setup as `kargo build --target android`: the Android SDK and `android/AndroidManifest.xml` (see [Android](../android/)). Android platform types such as `Context` and `View` link to the [Android reference](https://developer.android.com/reference/kotlin/).

## Configuring Dokka

Everything is optional. Add a `[doc]` section to `kargo.toml`:

```toml title="kargo.toml"
[doc]
includes = ["docs/module.md"]
source-link = "https://github.com/me/mylib/blob/main"
external-links = ["https://kotlinlang.org/api/kotlinx.coroutines/"]
# dokka = "2.2.0"
```

| Key | Default | Meaning |
|---|---|---|
| `includes` | `[]` | Markdown files with module and package documentation, relative to the project root |
| `source-link` | none | Base URL of your source tree. Each declaration gets a link to its source line. |
| `external-links` | `[]` | Documentation sites of dependencies. Their types become links. |
| `dokka` | `2.2.0` | Dokka version to use |

### Module and package pages

An `includes` file uses Dokka's format: a `# Module <name>` heading for the front page, and a `# Package <name>` heading for each package page.

```markdown title="docs/module.md"
# Module mylib

Greetings for every occasion.

# Package com.example.mylib

The whole public API.
```

### Source links

`source-link` is the URL that matches your project root. Kargo appends the file path and `#L<line>`, so `src/Main.kt` line 4 links to `https://github.com/me/mylib/blob/main/src/Main.kt#L4`. GitHub, GitLab and most code browsers use this format.

### Links to dependency docs

Each entry in `external-links` must be the root of a site generated by Dokka or Javadoc, with a `package-list` file in it. For example, `https://kotlinlang.org/api/kotlinx.coroutines/` turns `Flow` into a link to the coroutines documentation.

## Offline

Dokka is downloaded from Maven Central on first use and cached under `~/.kargo/cache`, like the Kotlin compiler. With `--offline`, `kargo doc` uses the cached copy and doesn't download the `package-list` files of external sites. Types from those sites are then shown without links.

## Troubleshooting

Dokka's warnings are printed as they happen. Run `kargo doc -v` to see Dokka's full log and the exact command line. Dokka's configuration is written to `target/debug/dokka/dokka.json`.
