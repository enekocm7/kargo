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
| Completion | Members after `.`, `?.` and `::`, extension functions included; everything in scope otherwise; `import` paths. Declarations that need an import come with it. Annotation classes after `@`, and the arguments of an annotation inside its parentheses |
| Signature help | Typing `(` or `,` in a call shows every overload of the function or constructor, the one that fits the arguments first, with the parameter you are at highlighted and its `@param` text. Named arguments (`f(b = `) pick their parameter, and a `vararg` stays active |
| Hover | The declaration's signature, its package or class, and its KDoc as Markdown. Inferred types of variables, such as `val names: List<String>`. The same for annotations and their arguments |
| Go to definition | Into your project, a path dependency, the sources of the standard library, the JDK and libraries, or stubs of libraries without sources. Go to declaration and go to type definition too |
| Find references, highlights | Every use in the project, with the declaration |
| Rename | Every reference in the project, and the declarations that must change with it: `expect` and `actual` declarations, overrides, named arguments |
| Outline | Document symbols for the file, workspace symbols for the project |
| Semantic highlighting | Every identifier classified by what it resolves to: class, interface, enum, enum member, type parameter, function, method, property, parameter, local variable or package. The modifiers `declaration`, `readonly` (`val`, parameters), `static` (members of objects and companions), `deprecated` and `defaultLibrary` (the standard library and other libraries) come with it. Whole file and range requests; the result is kept until the next change |
| Folding | Bodies, lambdas and multi-line argument lists, raw strings, the import block, KDoc and block comments, and runs of `//` comments |
| Selection | Expand the selection from an identifier to its dotted call chain, the brackets around it, the statement, the declarations around it (function, class) and the file |
| Formatting | The whole file, formatted by ktlint with the project's `.editorconfig` and `[fmt]` settings, as `kargo fmt` does. See [Formatting](#formatting) |
| Code actions | Import an unresolved name, organize imports, add the dependency an import needs, implement abstract members, add missing `actual` declarations, and quick fixes for `kargo check` errors. See [Code actions](#code-actions) |
| Diagnostics as you type | Unclosed or unmatched brackets, unterminated strings and comments, a `class`, `interface` or `typealias` without a name (errors), unused imports and local variables that are never used (warnings, shown greyed out). Decided from the file alone, so they appear between saves |
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

Once you have typed a letter, classes and functions from other packages are offered too, with an edit that adds their `import`. Choosing a function inserts its parentheses and, in an editor that supports snippets, puts the cursor between them, or in the braces of a function that takes one lambda. Choosing `if`, `while`, `for` or `when` inserts the scaffold of its condition the same way (`if ($0)`, `for ($1 in $2)`, `when ($0) {}`), and the other keywords insert plain text.

Annotations complete on their own. After `@` you get annotation classes only: those in scope, and once you have typed a letter, the others with an `import` added. `@Rest` offers Spring Web's `RestController`, for example. Inside the parentheses of an annotation you get the arguments it takes that you haven't given yet, as `defaultValue = `. The arguments of a Java annotation are the elements of its `@interface`, and those of an annotation written in Kotlin are its constructor parameters. Hover and go to definition work on annotations on classes, functions, parameters and properties, and on their arguments.

To know the receiver's type, the server infers the types completion needs: literals, calls of generic functions (`listOf("a")` is a `List<String>`), lambda parameters from the function the lambda is passed to (`it` in `forEach`, `map` or `let`), lambda results (`map { it.length }` is a `List<Int>`), `for` loop variables, destructuring (`for ((key, value) in map)`), delegated properties (`by lazy { }`, `by map`, `Delegates.observable(...)`, or any delegate with a `getValue`), `if`, `when` and `try` expressions (the type their branches share), the operators (`+`, `-`, `*`, `/`, `%`, `..` and unary `-` through the `operator fun` of the left operand's type), function references and lambdas as values (`String::length`, `::foo`, `val f = { x: Int -> x }`), function-type aliases as lambda parameter types, the upper bounds of type parameters (`fun <T : CharSequence> f(x: T)`), and `object : Foo { ... }` expressions with their own members.

### Rename

Rename works on declarations of your project: classes, functions, properties, parameters and local variables. It changes every reference in every file of the project, including imports and string templates (`"$name"`), and the declarations that must keep the same name: the `actual` declarations of an `expect` one (and the other way around), the overrides of a member and the member it overrides, and named arguments (`greet(name = "x")`) of a parameter. A name that isn't a plain identifier, such as a keyword, gets backquotes.

Declarations of the standard library and of other libraries can't be renamed; the editor says so before you type a name.

### Formatting

*Format Document* (and format on save, where your editor has it) formats the open file with the same ktlint that [`kargo fmt`](../formatting-and-linting/) runs, so the editor and the command never disagree. The text of the editor goes through ktlint, so it formats what you see, saved or not, and ktlint reads the rules from the `.editorconfig` files above the file and the [`[fmt]`](../../reference/manifest/#fmt) settings of the package it belongs to. A file that `[fmt] exclude` leaves out is not changed.

The server answers with a single edit that covers only the lines that changed, so the cursor, folds and undo history stay where they were. ktlint runs on the package's JDK on a separate thread: the first format after the editor starts waits while Kargo downloads ktlint (and a JDK, if `[toolchain] jdk` asks for one that isn't installed), and each one after that takes a second or so, during which completion and the other requests keep working. Offline (`kargo --offline lsp`), it works once ktlint and the JDK are in Kargo's cache, as `kargo fmt` does. If you type while it runs, the editor is told the document changed and the result is dropped; format again.

A file with a syntax error can't be formatted. The editor shows ktlint's message (`Not a valid Kotlin file (1:7 expecting ')')`) and the text stays as it is. Problems ktlint can't fix by itself, such as a file name that isn't PascalCase, don't stop the rest of the file from being formatted; [`kargo fmt`](../formatting-and-linting/) lists them.

Only whole files are formatted: ktlint has no way to format a range, so *Format Selection* is not offered. Formatting covers `.kt` files; `.kts` scripts are not formatted by the server. To format on save, turn on your editor's own setting: see [Formatting from your editor](../formatting-and-linting/#formatting-from-your-editor).

### Code actions

The server offers code actions in Kotlin files: the light bulb, or *Quick Fix* and *Source Action* in your editor.

| Action | Offered on | Does |
|---|---|---|
| Import `<name>` | A class, object or call that doesn't resolve, or a name that `kargo check` reports as `Unresolved reference` | Adds the import, in sorted place among the others. Each declaration of that name your project and its libraries have is offered, and the only one is preferred. An extension function called on a receiver (`"a".myExt()`) is found too |
| Organize imports | Anywhere (a *Source Action*, `source.organizeImports`) | Removes unused, duplicate and same-package imports and sorts the rest as IntelliJ IDEA and [`kargo fmt`](../formatting-and-linting/) do: other packages, then `java`, `javax`, `kotlin`, then aliased imports. Star imports and imports of operator functions (`getValue`, `plus`, `component1`, ...) stay. Not offered when a comment sits among the imports |
| Add `<group:artifact>` to kargo.toml | An `import` of a well-known library that no dependency provides, such as `kotlinx.coroutines`, `kotlinx.serialization`, `kotlinx.datetime` or `io.ktor` | Runs [`kargo/addDependency`](../editors/#commands-for-editor-plugins), which adds the library at its latest version as `kargo add` does: to `[dev-dependencies]` from test code, to `[target.<target>.dependencies]` from `src-<target>/` |
| Implement members | The header of a class or object that doesn't implement the abstract members of its supertypes | Writes an `override` with a `TODO()` body for each one, with the supertype's type arguments filled in (`Repo<User>` gives `fun find(id: Int): User`) and the imports they need |
| Add `actual` for `<name>` | An `expect` declaration in `src/` that a target of the package lacks | Writes the `actual` declaration, with `TODO()` bodies, to `src-<target>/` at the common file's path with the target in its name (`Platform.kt` gets `Platform.jvm.kt`): appended to that file, or in a new one when the editor can create files. *To every target* does them all at once |
| Add `override` | `kargo check`'s "hides member of supertype and needs an 'override' modifier" | Adds `override` before `fun`, `val` or `var` |
| Remove variable | `kargo check`'s "Variable 'x' is never used" | Removes the declaration. An initializer that may have side effects (a call) stays as a statement. A destructured component or loop variable is renamed to `_` instead |

With the cursor on `class Users : Repo<User> {}`, where `Repo<T>` declares `fun find(id: Int): T` and `val size: Int`, *Implement 2 members* gives:

```kotlin title="src/app/Users.kt"
class Users : Repo<User> {
    override fun find(id: Int): User {
        TODO("Not yet implemented")
    }

    override val size: Int
        get() = TODO("Not yet implemented")
}
```

Imports, the implemented members and the `actual` declarations come from the server's own analysis, so they work in every target's code. The quick fixes for compiler messages need [check on save](../editors/#check-on-save), which is on with this server.

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

A package also sees the main code of its [path dependencies](../workspaces/), and theirs. Test fixtures count as test code. With [Compose resources](../compose/#resources), the generated `Res` class counts as code in `src/`. `kargo metadata` writes it when the server loads the project, and every build updates it. A Kotlin file outside the source roots, open in the editor, sees the main code of every package.

So in a [multiplatform](../multiplatform/) package, common code in `src/` completes and navigates to the `expect` declarations, and `src-jvm/` sees both sides. Code in `src-wasm/` and `src-native/` gets the same completion, navigation and rename as JVM code, against your sources and the standard library.

The server notices new, changed and deleted `.kt` files, from the editor and on disk. When `kargo.toml` or `kargo.lock` change, it reads the project's layout again.

## Libraries

Libraries are seen per target, as the compiler sees them. Common code in `src/` compiles for every target of its package, so it sees only the libraries that every one of them has. `src-jvm/` sees the JVM's libraries, and `src-wasm/` the Wasm ones. Test code also sees the libraries that only the test classpath has, such as `[dev-dependencies]`, and main code doesn't. A Kotlin file outside the source roots sees the libraries of every platform.

The **standard library** comes from its sources jar at the project's [`[kotlin] version`](../../reference/manifest/#kotlin), and so does `kotlin-test`, for tests. Kargo downloads them on first use from the project's repositories into its Maven cache, like any other artifact, and extracts them under `~/.kargo/cache/lsp-sources`, so go to definition opens real files. The server loads the standard library of each platform the project targets: the JVM's sources for `jvm` and `android`, and `kotlin-stdlib-wasm-js` (or `kotlin-stdlib-wasm-wasi` for `environment = "wasi"`) for `wasm`, each with its `kotlin-test` for tests. Native code takes the parts of the Wasm one that Native shares. The JVM-only parts are offered to JVM code alone.

Offline before that first download (`kargo --offline lsp`), a built-in outline of the most used parts of the standard library stands in, with its documentation. The whole library replaces it on the next start with network access.

The **JDK** comes from the sources in its `lib/src.zip`. It is the JDK the project builds with: the installed one that `[toolchain] jdk` asks for, else the one on `JAVA_HOME` or the path. The server doesn't download a JDK; `kargo build` does. The JDK's classes are read when a name first needs them, so `java.io.File` and `System` complete, with their Javadoc, and go to definition opens the source file. A JDK without `lib/src.zip` leaves its classes unknown.

Kotlin's view of Java types matches the compiler's: `java.lang.String` is `String`, `int` is `Int`, `java.util.List` is Kotlin's `MutableList`, and so on. `java.lang` is imported into every Kotlin file on the JVM, as it is by the compiler.

The Kotlin classes for `String`, `CharSequence`, the collections and the maps also show the Java methods of their counterpart, so `"abc".chars()`, `codePointAt(0)` and a list's `stream()` complete. A Java method is left out when Kotlin already declares or offers a member of that name: `lines()` is Kotlin's extension, not Java's `Stream`-returning method, and `length` is Kotlin's property. Static Java methods don't complete on an instance. The server extends only these classes this way: `Int`, `Any` and the other Kotlin classes don't show their Java counterparts' methods.

**Dependencies** are resolved in the background, as `kargo metadata` does, and read whether or not they publish sources:

- A dependency whose **sources jar** is in the cache is read from it, Java files included, so hover shows its KDoc and Javadoc. The server downloads the missing sources jars itself, as `kargo fetch --sources` does: once the dependencies are resolved, it fetches the sources jar of every library that lacks one, in parallel and reported with `$/progress` ("Fetching dependency sources"), and then reads those libraries again from their sources. Until then, and for libraries that publish no sources, the compiled form below stands in.
- A dependency **without a sources jar** is read from its compiled form, for the targets whose classpath has it, as [above](#libraries) describes.
  - A **jar** is read from its class files. A class compiled from Kotlin keeps its Kotlin declarations in its metadata, so extension functions, nullability, `suspend`, default arguments, type aliases and companion objects come out as written. Any other class is read as Java: its public and protected fields, methods, constructors and nested classes, with generic types and, when the jar was compiled with `javac -parameters`, parameter names (otherwise `arg0`, `arg1`, ...).
  - A **klib** (the Wasm and Native variants of a dependency) is read from its Kotlin metadata the same way.
- A **cinterop klib** (the C bindings a Native library publishes next to its own klib, such as `mylib-mingwx64-1.0-cinterop-mylib.klib`) is always read from its metadata, even when the library has a sources jar: cinterop generates those declarations from C headers, so no sources jar contains them. Hover names it `<artifact> <version> (cinterop)`.
- For Native code, the **Kotlin/Native standard library** of the distribution that builds the project is read from its klib for what the standard library's sources don't give Native code: `kotlinx.cinterop` (`CValue`, `memScoped`, `cValue`, `ExperimentalForeignApi`, ...), `kotlin.native`, `kotlin.concurrent.AtomicInt` and the like. Its internal packages and every declaration the sources already have are left out, so `println` still shows its documentation. Hover names it `Kotlin/Native stdlib`.
- For Wasm code, the **npm packages** of [`[target.wasm.npm-dependencies]`](../wasm/#kotlin-declarations-for-npm-packages) are read from the Kotlin declarations Kargo generates from their TypeScript declarations. When the server opens a project, it installs the packages and generates the declarations if they changed, as a build does, so `express()` completes, shows the package's documentation in hover and goes to its declaration before the first build. Hover names them `npm`.
- For Native code, the **platform libraries** of the Kotlin/Native distribution that builds the project (`platform.posix`, `platform.zlib`, and on macOS and iOS the Apple frameworks) are read for the first platform of [`[target.native] platforms`](../../reference/manifest/#targetnative).

A library without sources is written out once as stub files under `~/.kargo/cache/lsp-sources`: one Kotlin file per package, and one Java file for its Java classes. Go to definition opens these files, which show each declaration's signature, `...` for default values, and no bodies or documentation (a klib compiled with its KDoc keeps it). They are generated, so edits to them change nothing. Private and internal declarations are left out, and so are those deprecated with `level = DeprecationLevel.HIDDEN`.

The standard library and `kotlin-test` come from their sources jars or the outline above, not from their compiled jars or klibs; only the Native-only parts of the Kotlin/Native standard library come from its klib, as described above.

When `kargo.toml` or `kargo.lock` change, after `kargo add` for example, the server resolves the dependencies again, fetches the new ones' sources jars and loads the libraries again, without a restart. Files of libraries that didn't change are not parsed again. A library whose repositories publish no sources jar is not asked for one again until the language server restarts; a download that failed is retried on the next change.

Offline (`kargo --offline lsp`), only sources jars already in Kargo's cache or in a repository on disk are read. To keep the server from downloading sources jars, on a metered network for example, set `fetchSources` to `false` in the editor's `initializationOptions`:

```lua title="init.lua"
vim.lsp.config('kargo', {
  cmd = { 'kargo', 'lsp' },
  filetypes = { 'kotlin', 'toml' },
  root_markers = { 'kargo.lock', 'kargo.toml' },
  init_options = { fetchSources = false },
})
```

Then run `kargo fetch --sources` yourself when you want them; the server reads them on the next change to `kargo.toml` or `kargo.lock`, or the next start.

## Limits

The server reads declarations and infers types as far as completion and navigation need. It is not the Kotlin compiler:

- It reports only syntax problems and unused code of its own (see the table above). It doesn't report unresolved references or type errors. Those come from `kargo check` on save, which compiles the code for real. Turn it off with `{ "checkOnSave": false }` in the editor's `initializationOptions`; see [Check on save](../editors/#check-on-save).
- It reads Java but doesn't edit it. Java files get no completion, hover, rename or references of their own, and a Java declaration can't be renamed from Kotlin: the server refuses, because the Java code that uses it wouldn't change. References to a Kotlin declaration from Java code aren't searched.
- Libraries without sources have no documentation in hover, and a type parameter with several bounds keeps only the first.
- Kotlin/Native code gets the standard library's parts that its Wasm sources jar shares with Native, plus the Native-only declarations from the distribution's klib, which have no documentation in hover.
- Smart casts of parameters and `val`s are followed: inside `if (x is Foo)` (and after `&&`), in the branch of `when (x) { is Foo -> }` or `when { x is Foo -> }`, and after `if (x !is Foo) return`, `x as Foo` or `require(x is Foo)`. Null checks narrow the same way (`x != null`, `if (x == null) return`, `x!!`, `x ?: return`). Smart casts of `var`s and properties are not followed.
- Nullability is tracked: `x.` on a nullable value offers only the extensions of nullable types (`isNullOrEmpty`, `let`, ...), `x?.` offers the members of the unwrapped type, and the result of a safe call is nullable again. Types from Java count as non-null.
- Overloads are chosen by the arguments' types (subtypes, nullability, named and default arguments, and the parameter count of lambdas); when an argument's type is unknown and several overloads fit, go to definition lists them all. A type argument is not inferred from the expected type of a call (`foo(emptyList())`), so completion may miss members of such expressions. A `when` or `try` whose branches have nothing in common but `Any` completes as `Any`.
- Its [code actions](#code-actions) are the ones listed there; it has no other quick fixes or refactorings, and no inlay hints. Semantic highlighting leaves keywords, literals and comments to the editor's own grammar. [Formatting](#formatting) is ktlint's: whole files only.

For the full analysis of the Kotlin compiler, use IntelliJ IDEA with the Kargo plugin, or run JetBrains' Kotlin LSP behind `kargo lsp` with `--server kotlin-lsp`. See [Choosing the Kotlin server](../editors/#choosing-the-kotlin-server).
