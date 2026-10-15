---
title: Kargo's Kotlin server
description: The Kotlin language server built into kargo lsp, the default outside IntelliJ IDEA - what it gives you, how it reads your project and the standard library, and where it stops.
---

`kargo lsp` comes with its own Kotlin language server. It is written in Rust and built into the `kargo` binary, so there is nothing to download and no JDK to run it on, and it starts at once. It is the default Kotlin server for every editor that runs `kargo lsp`: Zed, Neovim, Helix, Emacs and any other LSP client. See [Editor integration](../editors/) to set one up.

```sh
kargo lsp    # Kargo's Kotlin server, kargo.toml support, runnables and check on save
```

IntelliJ IDEA and Android Studio don't use it. The [Kargo plugin](../editors/#intellij-idea-and-android-studio) keeps the IDE's own Kotlin support and runs `kargo lsp --server none` for `kargo.toml` only.

## What it does

| Feature | What you get |
|---|---|
| Completion | Members after `.`, `?.` and `::`, extension functions included; everything in scope otherwise; `import` paths. Declarations that need an import come with it |
| Hover | The declaration's signature, its package or class, and its KDoc as Markdown. Inferred types of variables, such as `val names: List<String>` |
| Go to definition | Into your project, a path dependency, or the sources of the standard library, the JDK and libraries. Go to declaration and go to type definition too |
| Find references, highlights | Every use in the project, with the declaration |
| Rename | Every reference in the project, and the declarations that must change with it: `expect` and `actual` declarations, overrides, named arguments |
| Outline | Document symbols for the file, workspace symbols for the project |
| Errors | From [check on save](../editors/#check-on-save), which is on with this server: `kargo check` runs when you save a file and its errors for every target appear in the editor |

### Completion

```kotlin title="src/Main.kt"
fun main() {
    val names = listOf("Ada", "Grace")
    names.filter { it.length > 3 }.forEach { println(it.up|) }
    //                                                  ↑ uppercase, uppercaseChar, ...
}
```

After a `.` you get the members of the receiver's type and its supertypes, and the extension functions and properties that apply to it. Without a receiver, you get what is in scope, nearest first:

- local variables and parameters, and `it` in a lambda;
- the members of `this`: the enclosing class (and its companion object), the receiver of an extension function, and the receiver of a lambda such as `apply { }`, `with(x) { }` or `buildString { }`;
- the file's declarations, its imports, its package, star imports and the packages Kotlin imports by default (`kotlin`, `kotlin.collections`, `kotlin.io`, `kotlin.text` and the rest);
- keywords.

Once you have typed a letter, classes and functions from other packages are offered too, with an edit that adds their `import`. Choosing a function inserts its parentheses and, in an editor that supports snippets, puts the cursor between them, or in the braces of a function that takes one lambda.

To know the receiver's type, the server infers the types completion needs: literals, calls of generic functions (`listOf("a")` is a `List<String>`), lambda parameters from the function the lambda is passed to (`it` in `forEach`, `map` or `let`), lambda results (`map { it.length }` is a `List<Int>`), `for` loop variables, destructuring (`for ((key, value) in map)`), `by lazy { }`, `if` expressions and the operators.

### Rename

Rename works on declarations of your project: classes, functions, properties, parameters and local variables. It changes every reference in every file of the project, including imports and string templates (`"$name"`), and the declarations that must keep the same name: the `actual` declarations of an `expect` one (and the other way around), the overrides of a member and the member it overrides, and named arguments (`greet(name = "x")`) of a parameter. A name that isn't a plain identifier, such as a keyword, gets backquotes.

Declarations of the standard library and of other libraries can't be renamed; the editor says so before you type a name.

## How it reads your project

The server reads your project the way `kargo build` compiles it, from [`kargo metadata`](../editors/#importing-a-project): every package of a [workspace](../workspaces/), with the source, test and test-fixture roots of each target and its examples. It writes no files: no `workspace.json`, no `kls-classpath`.

Java files in those roots are read too, so Kotlin code sees the classes of your `.java` files. The server doesn't edit them: see [Limits](#limits).

A file sees the declarations Kargo compiles it with:

| Code in | Sees |
|---|---|
| `src/` | `src/` |
| `src-<target>/` | `src/` and `src-<target>/` |
| `tests/` | `src/` and `tests/` |
| `tests-<target>/` | `src/`, `src-<target>/`, `tests/` and `tests-<target>/` |
| `examples/` | `src/` |

A package also sees the main code of its [path dependencies](../workspaces/), and theirs. Test fixtures count as test code. A Kotlin file outside the source roots, open in the editor, sees the main code of every package.

So in a [multiplatform](../multiplatform/) package, common code in `src/` completes and navigates to the `expect` declarations, and `src-jvm/` sees both sides. Code in `src-wasm/` and `src-native/` gets the same completion, navigation and rename as JVM code, against your sources and the standard library.

The server notices new, changed and deleted `.kt` files, from the editor and on disk. When `kargo.toml` or `kargo.lock` change, it reads the project's layout again.

## Libraries

The **standard library** comes from its sources jar at the project's [`[kotlin] version`](../../reference/manifest/#kotlin), and so does `kotlin-test`, for tests. Kargo downloads them on first use from the project's repositories into its Maven cache, like any other artifact, and extracts them under `~/.kargo/cache/lsp-sources`, so go to definition opens real files. The JVM-only parts of the standard library are offered when a package has a JVM or Android target.

Offline before that first download (`kargo --offline lsp`), a built-in outline of the most used parts of the standard library stands in, with its documentation. The whole library replaces it on the next start with network access.

The **JDK** comes from the sources in its `lib/src.zip`. It is the JDK the project builds with: the installed one that `[toolchain] jdk` asks for, else the one on `JAVA_HOME` or the path. The server doesn't download a JDK; `kargo build` does. The JDK's classes are read when a name first needs them, so `java.io.File` and `System` complete, with their Javadoc, and go to definition opens the source file. A JDK without `lib/src.zip` leaves its classes unknown.

Kotlin's view of Java types matches the compiler's: `java.lang.String` is `String`, `int` is `Int`, `java.util.List` is Kotlin's `MutableList`, and so on. `java.lang` is imported into every Kotlin file on the JVM, as it is by the compiler.

The Kotlin classes for `String`, `CharSequence`, the collections and the maps also show the Java methods of their counterpart, so `"abc".chars()`, `codePointAt(0)` and a list's `stream()` complete. A Java method is left out when Kotlin already declares or offers a member of that name: `lines()` is Kotlin's extension, not Java's `Stream`-returning method, and `length` is Kotlin's property. Static Java methods don't complete on an instance. The server extends only these classes this way: `Int`, `Any` and the other Kotlin classes don't show their Java counterparts' methods.

**Dependencies** come from their sources jars, once they are in the cache. Download them with:

```sh
kargo fetch --sources
```

The server then resolves the project's dependencies in the background, as `kargo metadata` does, and reads the sources jars it finds, Java files included. Restart the editor's language server after fetching new sources. Libraries without a sources jar stay unknown to it: their classes don't complete.

## Limits

The server reads declarations and infers types as far as completion and navigation need. It is not the Kotlin compiler:

- It reports no errors of its own. They come from `kargo check` on save, which compiles the code for real. Turn it off with `{ "checkOnSave": false }` in the editor's `initializationOptions`; see [Check on save](../editors/#check-on-save).
- It reads Java but doesn't edit it. Java files get no completion, hover, rename or references of their own, and a Java declaration can't be renamed from Kotlin: the server refuses, because the Java code that uses it wouldn't change. References to a Kotlin declaration from Java code aren't searched.
- Classes from binary-only libraries (jars without a sources jar) are unknown, so their members don't complete.
- Smart casts (`if (x is Foo) x.`), the branches of `when` and `try`, and some overloads chosen by argument types are not followed, so completion may miss members of such expressions.
- It has no formatting, quick fixes, signature help, inlay hints or semantic highlighting. Use [`kargo fmt`](../formatting-and-linting/) to format code.

For the full analysis of the Kotlin compiler, use IntelliJ IDEA with the Kargo plugin, or run JetBrains' Kotlin LSP behind `kargo lsp` with `--server kotlin-lsp`. See [Choosing the Kotlin server](../editors/#choosing-the-kotlin-server).
