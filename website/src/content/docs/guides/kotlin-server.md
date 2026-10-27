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
| Completion | Members after `.`, `?.` and `::`, extension functions included; everything in scope otherwise, ranked by the expected type; named arguments; `override` stubs. Declarations that need an import come with it. Beyond expressions: the `package` of the file's directory, `import` paths, the cases a `when` is missing, annotations and their use-site targets and arguments, `$name` in strings, supertypes, types and type parameters, labels and lambda parameter names. See [Completion outside expressions](#completion-outside-expressions) |
| Signature help | Typing `(` or `,` in a call shows every overload of the function or constructor, the one that fits the arguments first, with the parameter you are at highlighted and its `@param` text. Named arguments (`f(b = `) pick their parameter, and a `vararg` stays active |
| Hover | The declaration's signature, its package or class, and its KDoc as Markdown, with `@see` names as links and `@sample` functions shown. Hover on a name in a KDoc comment shows what it refers to. Inferred types of variables, such as `val names: List<String>`. The same for annotations and their arguments |
| Go to definition | Into your project, a path dependency, the sources of the standard library, the JDK and libraries, or stubs of libraries without sources. Go to declaration and go to type definition too. A name in a KDoc comment (`[Name]`, `@see`) goes to its declaration |
| Find references, highlights | Every use in the project, with the declaration, and the names in KDoc comments that refer to it |
| Rename | Every reference in the project, and what must change with it: `expect` and `actual` declarations, overrides, named arguments, KDoc links and tags, labels, the file named after a class, packages and the entry points in `kargo.toml` |
| Outline | Document symbols for the file, workspace symbols for the project, and for its libraries once the query has three characters. See [Protocol details](#protocol-details) |
| Semantic highlighting | Every identifier classified by what it resolves to: class, interface, enum, enum member, type parameter, function, method, property, parameter, local variable or package. The modifiers `declaration`, `readonly` (`val`, parameters), `static` (members of objects and companions), `deprecated` and `defaultLibrary` (the standard library and other libraries) come with it. Whole file and range requests; the result is kept until the next change |
| Inlay hints | The inferred type after `val`, `var`, `for` and lambda parameter names (`val names`: `: List<String>`), left out when the initializer shows it (a literal or a constructor call) or the type is not fully known; the parameter name before literal arguments (`greet(name: "a", loud: true)`); and the implicit `it: T` and `this: T` at the start of a lambda. See [Inlay hints](#inlay-hints) |
| Folding | Bodies, lambdas and multi-line argument lists, raw strings, the import block, KDoc and block comments, and runs of `//` comments |
| Selection | Expand the selection from an identifier to its dotted call chain, the brackets around it, the statement, the declarations around it (function, class) and the file |
| Formatting | The whole file, formatted by ktlint with the project's `.editorconfig` and `[fmt]` settings, as `kargo fmt` does. See [Formatting](#formatting) |
| Code actions | Import an unresolved name, organize imports, add the dependency an import needs, implement abstract members, add the branches a `when` is missing, add missing `actual` declarations, generate the KDoc of a declaration, quick fixes for `kargo check` errors, and quick fixes for its own unused-code and deprecation diagnostics. See [Code actions](#code-actions) |
| Refactorings | Extract a variable or a function, inline a variable or a function, move or remove a parameter, and move a declaration to another package or a file of its own. See [Refactorings](#refactorings) |
| Diagnostics as you type | Unclosed or unmatched brackets, unterminated strings and comments, a `class`, `interface` or `typealias` without a name (errors), unused code (warnings, shown greyed out: see [Unused code and deprecated usages](#unused-code-and-deprecated-usages)), deprecated usages (shown struck through), and [operators](#operators) that can't work. Decided from the file and the project's declarations, so they appear between saves |
| Errors | From [check on save](../editors/#check-on-save), which is on with this server: `kargo check` runs when you save a file and its errors for every target appear in the editor |

### Inlay hints

```kotlin title="src/Main.kt"
fun main() {
    val names = listOf("Ada", "Grace")      // names: List<String>
    greet("Ada", true)                      // greet(name: "Ada", loud: true)
    names.forEach { println(it) }           // { it: String -> ... }
}
```

Every kind can be switched off with `initializationOptions`, either nested (`{ "inlayHints": { "types": false } }`) or flat (`{ "inlayHints.types": false }`). All are on by default.

| Option | Shows |
|---|---|
| `inlayHints.types` | The type of `val`/`var`, `for` variables and lambda parameters without a declared type |
| `inlayHints.parameterNames` | The parameter name before a literal argument (number, character, string without templates, `true`, `false`, `null`) that is passed by position |
| `inlayHints.implicitIt` | `it: T` after the `{` of a lambda that declares no parameters |
| `inlayHints.implicitReceivers` | `this: T` after the `{` of a lambda with a receiver, such as `buildString { }` |
| `inlayHints.operators` | The `operator fun` of your project that an operator calls, after the operator (`a + b` shows `plus(o: Money)`). Off by default |

Only fully known types are shown, and a type is left out when the initializer already says it, as IntelliJ does.

### Unused code and deprecated usages

Your editor greys out code nothing uses, and strikes through the names that are deprecated:

- **Unused code** (warnings, greyed out): imports that nothing names, local `val`s and `var`s that are never read, `private` functions, properties and classes whose name appears nowhere else in their file, parameters of `private` and local functions that the body doesn't read, and the statements after a `return`, `throw`, `break`, `continue` or `TODO()` in the same block.
- **Deprecated usages** (hints, struck through): a use of a declaration marked `@Deprecated`, in your project or a library. The message is the one you wrote, with the `ReplaceWith` when there is one. A `DeprecationLevel.ERROR` declaration is an error here, and a `HIDDEN` one is left to `kargo check`, which reports it as unresolved.

The server is conservative. A name counts as used when it appears anywhere else in the file, so a declaration that shares its name with something else is never reported. Declarations that are used by name from outside the code the server reads are left alone: annotated ones (`@Test`, `@Serializable`, `@JvmStatic`, ...), `operator` and `infix` functions, `override`s, `main`, `componentN` functions, the JVM's serialization hooks (`readObject`, `writeReplace`, `serialVersionUID`, ...) and names that start with `_`. Public declarations are never reported as unused, since other code in the project or your dependencies may use them.

Turn them off with `initializationOptions`: `{ "unusedCode": false }` for the unused code, `{ "deprecated": false }` for the deprecated usages. Both are on by default.

The [code actions](#code-actions) remove an unused import or variable and replace a deprecated call with its `ReplaceWith`. A deprecation that `kargo check` reports too is shown once, with the compiler's message.

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

The members the compiler generates are offered too, and hover marks them as generated: `copy`, `componentN`, `toString`, `equals` and `hashCode` of a data class (and the last three of a value class), `entries`, `values()` and `valueOf(value)` after the name of an enum class (and by their plain names inside it), and `name` and `ordinal` on its entries. Go to definition on one of them goes to its class, and they can't be renamed. A lambda passed to a `fun interface` (`Handler { it.length }`, or as the argument of a function that takes a `Handler`) gets the types of the interface's one method, and `invoke` is offered after a value of a function type. A class annotated `@Serializable` (what [`serialization`](../frameworks/) adds) gets `serializer()`, so `Person.serializer()` resolves; `Json.encodeToString` itself needs the kotlinx.serialization sources, which the server reads from the dependency's sources jar. The parameters of a function type annotated `@Composable (Int) -> Unit` are kept for the lambda passed to it. With [`lombok`](../frameworks/), the getters, setters, constructors (`@AllArgsConstructor`, `@NoArgsConstructor`, `@RequiredArgsConstructor`, and the ones `@Data` and `@Value` add) and builders (`@Builder`: `builder()`, one method per field, `build()`) of annotated Java classes are offered in Kotlin and hover marks them as generated by Lombok. Hover on a class annotated `@Component`, `@Service`, `@Configuration`, `@SpringBootApplication` or another annotation of Spring's `allopen` preset notes that the plugin opens it.

Once you have typed a letter, classes and functions from other packages are offered too, with an edit that adds their `import`. Choosing a function inserts its parentheses and, in an editor that supports snippets, puts the cursor between them, or in the braces of a function that takes one lambda. Choosing `if`, `while`, `for` or `when` inserts the scaffold of its condition the same way (`if ($0)`, `for ($1 in $2)`, `when ($0) {}`), and the other keywords insert plain text.

Completion also looks at where you are typing:

- **Expected type.** After `val x: String = `, after `return` in a function that declares its return type, as an argument (`take(` for `fun take(n: Int)`) and after `name = ` of a named argument or an assignment, the values and calls whose type fits come first, ahead of the rest of their group. Among items of the same group, your project's declarations come before the libraries'.
- **Named arguments.** Inside the parentheses of a call you are offered `name = ` for the parameters that no argument gives yet. Once an argument is positional, the parameters before it are left out.
- **Overrides.** In a class body, typing the start of a member's name or `override ` offers the open and abstract members of the supertypes that the class doesn't override yet, and inserts a stub with the full signature: `override fun area(scale: Double): Double { TODO("Not yet implemented") }` for an abstract member, a call of `super` for an open one. Type arguments of the supertype are filled in.
- **Keywords.** Only the keywords that are valid at the cursor are offered: declaration keywords in a class body and at the top of a file (`constructor` and `init` in classes, `companion object` where the class has none yet), `if`, `return` and the other expression keywords after `=`, `else` after an `if` that has none, `is` and `in` in `when` branches, and `as`, `is` and `in` after an expression.
- **Trailing lambdas.** A function whose last parameter is a function type is inserted with its lambda, with the names the function type gives its parameters (`(index: Int, value: String) -> Unit` inserts `{ index, value -> $0 }`), or names made from their types when it gives none: `repeatIt(${1:times}) { $0 }`.

- **Recently used.** A name you inserted in one edit lately (a completion you accepted, a paste) comes first among the items of its group.
- **Postfix templates.** After `expr.`, with an editor that supports snippets: `val`, `var`, `return`, `println`, `null` and `nn` (an `if` on null or not null) at the start of a statement, and `not`, `if` and `while` after a `Boolean`. They replace `expr.` and what you typed with the template.

The same request for the same position in an unchanged project is answered from the last result, so an editor that asks again on every keystroke of a prefix it already has costs nothing. The list is marked incomplete while the prefix is empty, since declarations that need an import are offered only once you have typed a letter.

Declarations marked `@PublishedApi` and the library package `kotlin.internal` are left out of every list.

Annotations complete on their own. After `@` you get annotation classes only: those in scope, and once you have typed a letter, the others with an `import` added. `@Rest` offers Spring Web's `RestController`, for example. Inside the parentheses of an annotation you get the arguments it takes that you haven't given yet, as `defaultValue = `. The arguments of a Java annotation are the elements of its `@interface`, and those of an annotation written in Kotlin are its constructor parameters. Hover and go to definition work on annotations on classes, functions, parameters and properties, and on their arguments.

### Completion outside expressions

Where Kotlin code is not an expression, completion offers what fits there, and nothing where you are writing a new name:

```kotlin title="src/app/shapes/Area.kt"
package |                                 // app.shapes

fun area(s: Shape) = when (s) {
    is Circle -> 3.14 * s.r * s.r
    |                                     // is Square, Empty
}
```

| Where | You get |
|---|---|
| `package ` | The package that matches the file's directory under its source root: `app.shapes` for `src/app/shapes/Area.kt`. It replaces the segments you typed |
| `import ` | Packages for each segment, then the classes, functions and properties of the package, nested classes after a class, and `*`. After a complete path, `as`; the alias itself is a new name |
| A `when` branch | With a subject that is an enum, a `sealed` class or interface, or a `Boolean`, the cases no branch names yet: `Color.GREEN`, `is Circle` for a class, `Empty` for an object, `true`, and `null` when the subject is nullable. A case from another package comes with its `import`. `else` is offered only when it adds something: never twice, and not once every case is covered |
| `@` | Annotation classes, and the use-site targets `field:`, `get:`, `set:`, `param:`, `setparam:`, `property:`, `receiver:` and `delegate:`, with `file:` before the package. After a target (`@get:`), annotation classes only |
| `"$` in a string | The values in scope (locals, parameters, properties) and `this`, not functions or classes. `${ }` takes any expression |
| `class Foo :` | Interfaces, and classes that are `open`, `abstract` or `sealed`, with their constructor call: `Base($0)`, or `Base()` when it takes nothing. After `interface Foo :`, interfaces only |
| After `fun` | The receiver type: types only. After `fun String.` comes the new function's name, so nothing |
| A type, after `:` or `List<` | Types and the type parameters in scope: `T` in `fun <T> f(x: T)` |
| `return@`, `break@`, `continue@`, `this@`, `super@` | The labels in scope: lambdas by their label or the function they are passed to (`return@forEach`), loops for `break` and `continue`, and for `this@` the enclosing classes, extension functions and lambdas with a receiver (`this@buildString`) |
| `call { ` | Names for the lambda's parameters from the function type, as `string, int ->` |
| `val `, `class `, `fun <`, `catch (` | Nothing: a new name. In a parameter list, only the modifiers that fit before the name: `val`, `var`, `vararg` and the visibilities in a class's primary constructor, `vararg`, `noinline` and `crossinline` in a function |

The `package` suggestion comes from the source roots of `kargo metadata`, so a file outside them, or directly in one, gets none.

To know the receiver's type, the server infers the types completion needs: literals, calls of generic functions (`listOf("a")` is a `List<String>`), lambda parameters from the function the lambda is passed to (`it` in `forEach`, `map` or `let`), lambda results (`map { it.length }` is a `List<Int>`), `for` loop variables, destructuring (`for ((key, value) in map)`), delegated properties (see [Delegation](#delegation)), `if`, `when` and `try` expressions (the type their branches share), the operators (see [Operators](#operators)), function references and lambdas as values (`String::length`, `::foo`, `val f = { x: Int -> x }`), function-type aliases as lambda parameter types, the upper bounds of type parameters (`fun <T : CharSequence> f(x: T)`), and `object : Foo { ... }` expressions with their own members.

### Newer and rarer syntax

The names in these places resolve, complete and navigate like the ones in an ordinary function:

- **Context parameters and receivers.** `context(logger: Logger) fun f()` puts `logger` in scope in the body, and `context(Scope) fun g()` makes the members of `Scope` available without a receiver, as an extension receiver does; `this@Scope` is that receiver.
- **Destructuring in lambdas.** `{ (a, _): Pair<A, B> -> }` types `a` from the written type (without one, from the function the lambda is passed to), and `_` for an unused parameter or component doesn't shift the others.
- **Lambdas outside function bodies.** Lambdas in the default value of a parameter (`block: (Int) -> Int = { it + n }`), in the constructor arguments of a supertype (`: Base({ name.length })`), in a delegate (`: Runnable by lazy { name }`) and in the arguments of an enum entry have their own scopes: they see the primary constructor's parameters and the parameters before them, and `it` gets its type from the declared function type or the supertype's constructor.
- **Accessors.** A getter with a body (`val x get() { return "s" }`) gives the property its type from the first `return` that is the getter's own, and so does the type written after `get()`. Inside `get` and `set`, `field` has the property's type, and the setter's parameter does too.
- **Types.** A `where` clause over several lines bounds the type parameters of functions, properties and classes, and a parameter with several bounds (`fun <T> f(x: T) where T : CharSequence, T : Comparable<T>`) offers the members of all of them. Definitely non-nullable types (`T & Any`, also as `List<T & Any>` or `(T & Any)?`) are not nullable whatever `T`'s bound, and annotated or `suspend` function types (`@Composable () -> Unit`, `suspend String.() -> Unit`, `(suspend () -> Unit).run()`) work as lambda parameter types and receivers. A star projection (`List<*>`) reads as `Any?`, and `out` and `in` variance is read without being checked: `List<out Number>` offers the members of `Number`. Extension functions on a type parameter (`fun <T> T.keep(): T`, `fun <T : Comparable<T>> List<T>.best(): T`) bind it from the receiver. The element of a `buildList { add(x) }` is the type of the first `add(x)` in its body, and a `reified` type parameter takes the type written in the call (`make<String>()`).
- **Constructors.** The arguments of `: this(...)` and `: super(...)` and every `init` block see the constructor's parameters.
- **Scripts and file annotations.** A [Kargo script](../scripts/) is a `.kts` file outside a project, or one whose shebang runs `kargo script`. Its top-level statements share one scope, and `args` is the array of its arguments. Its `@file:DependsOn` dependencies are loaded for that script alone (see [Libraries](#libraries)). `@file:JvmName("Name")` and `@file:JvmMultifileClass` are read, so the name of the class a file's top-level declarations are in is known. Gradle's `build.gradle.kts` and `settings.gradle.kts` belong to the Kotlin plugin, and the server leaves them alone.

Strings and comments that are hard to read are handled as the compiler does: `"$"` and `"${'$'}"`, raw strings with templates over several lines, a backslash at the end of a line, and nested block comments. Whatever you type, the parser finishes: a test edits real sources at random and checks that it never panics or hangs.

### Operators

Kotlin's operators are `operator fun`s that the syntax calls, and the server follows all of them, in your code, in extensions and in the standard library and other libraries:

| Syntax | Calls |
|---|---|
| `a + b`, `a - b`, `a * b`, `a / b`, `a % b` | `plus`, `minus`, `times`, `div`, `rem` (the deprecated `mod` when there is no `rem`) |
| `a..b`, `a..<b` | `rangeTo`, `rangeUntil` |
| `x in a`, `x !in a` | `a.contains(x)` |
| `+a`, `-a`, `!a` | `unaryPlus`, `unaryMinus`, `not` |
| `a++`, `--a` | `inc`, `dec` |
| `a[i, j]`, `a[i] = v` | `get`, `set` |
| `a(args)`, `a { }` | `invoke`, on a value that is no function type |
| `a += b` | `plusAssign`, else `a = a + b` with `plus` |
| `a < b`, `a == b` | `compareTo`, `equals` |
| `for (x in a)` | `iterator`, then `hasNext` and `next` |
| `val (x, y) = a` | `component1`, `component2`, ... |
| `val x by d` | `getValue`, `setValue`, `provideDelegate` |

- **Types.** The type of `a + b`, `-a`, `a[i]`, `a()`, `a++` and of the variables of a `for` loop or a destructuring comes from the function the operator calls, with the overload chosen by the type of the right operand (`a + 1` and `a + b` can differ). A member wins over an extension of the same fit.
- **Go to definition and hover** on the operator token (`+`, `[`, `in`, `by`, ...) jump to and show the `operator fun` it calls.
- **Find references** of an `operator fun` lists the operators that call it next to the uses by name: `a + b` for `plus`, `a[i]` for `get`, the `in` of a `for` loop for `iterator`, and the variable of `val (x, y) = a` for `component1`.
- **Rename** of an `operator fun` is refused when syntax calls it: a new name would break `a + b`. Remove the `operator` modifier or the uses first.
- **Completion** offers every operator function after `operator fun` and `operator fun Type.`, with a snippet for its signature (`plus(other: T): T`), including `componentN`, `plusAssign` and the other augmented assignments, `iterator`, `getValue`, `setValue` and `provideDelegate`.

- **Diagnostics.** For operators on classes of your project, the server reports an error when the function an operator calls lacks the `operator` modifier, when the class has no such function, and when none takes the right operand's type. It stays quiet for library types and for classes with a supertype it doesn't know, so `kargo check` remains the authority; the editor may show both messages.
- **Inlay hints.** With `inlayHints.operators` on, each operator that calls a function of your project shows it (see [Inlay hints](#inlay-hints)).
- **Position.** A pointer on an operator's first character (`a[0]`, `a + b`) lands on the operator, not on the name before it, when the operator calls an `operator fun`.

### Delegation

A property declared with `by` gets its type from the delegate's `getValue`, with the delegate's type arguments put in: `by lazy { }`, `by lazy(LazyThreadSafetyMode.NONE) { }`, `by lazy(::compute)`, `by map` (the map's value type), `by Delegates.observable(0) { ... }`, `by Delegates.notNull<String>()`, a delegate class or object of your own, an extension `getValue` (`by remember { mutableStateOf(0) }` is the `T` of `State<T>`), and `by ::other` or `by this::other`, which take the type of the other property. A delegate with a `provideDelegate` gives `getValue` the type that `provideDelegate` returns. A declared type (`val x: Foo by inject()`) wins over the inferred one. Local, top-level and member properties work the same.

The server reports an error on the `by` of a property whose delegate is a class of your project without a `getValue` (or without a `setValue` for a `var`), whose `getValue` lacks `operator`, or whose type is not the declared one (`val n: Int by Cell()` where `getValue` returns `String`). Delegates from libraries and classes with a supertype the server doesn't know are left to `kargo check`.

Hover on the property shows the type, hover and go to definition on `by` show the `getValue` or `provideDelegate` it calls (see [Operators](#operators)), and [inlay hints](#inlay-hints) show the inferred type.

A class that delegates an interface (`class A(b: B) : Iface by b`, also `by B()` and several `by` clauses) has all of that interface's members for completion, hover and go to definition. The `Implement members` action doesn't ask for what is delegated, and `override` completion lists the delegated members marked `implemented by delegation to b`, with a stub that forwards to `b`.

### Implementations and hierarchies

Go to implementation goes from an interface, abstract class or `open` class to the classes of your project that extend it, from a member to its overrides in them, and from an `expect` declaration to its `actual` declarations.

The call hierarchy shows who calls a function or constructor of your project (found through its references, so overrides count) and which functions and constructors its body calls. The type hierarchy shows the direct supertypes of a class, library ones included, and the classes of your project that name it as a supertype. Java files are read for declarations only, so calls made from them aren't listed.

### KDoc completion

Inside a `/** */` comment, `@` offers the block tags (`@param`, `@return`, `@constructor`, `@receiver`, `@property`, `@throws`, `@exception`, `@sample`, `@see`, `@author`, `@since`, `@suppress`) with a snippet for what follows. After `@param` and `@property` it offers the parameters, properties and type parameters of the documented declaration. In a link (`[`) and after `@see`, `@throws`, `@exception` and `@sample` it offers the declarations in scope, and the members after a dot (`[User.`) or the contents of a package. After `@throws` and `@exception` it offers first the exceptions the body throws (`throw Name(...)`), as *thrown in the body*.

### KDoc links and hints

Hover, go to definition, find references and highlights work on the names in a KDoc comment: a link (`[Name]`, `[Type.member]`), the name after `@see`, `@throws`, `@exception` and `@sample`, and the name of a `@param` or `@property` tag, which refers to the parameter or property. Hover shows a `@see` name as a link to its declaration and a `@sample` name as the body of the function it names.

A name in a KDoc comment that resolves to nothing gets a hint, not an error, since the compiler decides what is wrong: a `@param` or `@property` name that is not a parameter or property of the declaration, and a link, `@see`, `@throws` or `@sample` name that is neither a declaration nor a package. A name from a library the project doesn't depend on yet shows up as a hint too.

### Rename

Rename works on declarations of your project: classes, functions, properties, parameters and local variables. It changes every reference in every file of the project, including imports and string templates (`"$name"`), and the declarations that must keep the same name: the `actual` declarations of an `expect` one (and the other way around), the overrides of a member and the member it overrides, and named arguments (`greet(name = "x")`) of a parameter. A name that isn't a plain identifier, such as a keyword, gets backquotes.

Declarations of the standard library and of other libraries can't be renamed; the editor says so before you type a name.

Rename also changes what isn't Kotlin code:

- **KDoc.** Links (`[User]`, `[User.name]`, `[app.User]`, `@see User`, `@throws`, `@exception`, `@sample`) and the names in `@param` and `@property` tags (also `@param[name]`) follow the declaration they name. Brackets in code spans (`a[i]`) are not links. You can start a rename on a name inside a KDoc comment too.
- **Labels.** Renaming `loop@` renames the `break@loop` and `continue@loop` of that loop (and not those of another loop with the same label), and the same for `return@label` on a lambda. Renaming a function also renames the `return@forEach`-style references to it, and renaming a class the `this@Outer` ones.
- **Type parameters.** Renaming `T` in `class Box<T>` changes its uses in the class, the `@param T` tag and `[T]` links, and leaves a member that declares its own `T`.
- **The file of a class.** Renaming `User` in `User.kt` also renames the file to `Person.kt`, when the editor takes `rename` operations in a workspace edit. Set `renameFiles` to `false` in the editor's `initializationOptions` to turn that off. The other way round works too: when the editor sends `workspace/willRenameFiles`, renaming `User.kt` to `Person.kt` renames the class `User`. A file with `expect` and `actual` declarations (`User.kt`, `User.jvm.kt`) keeps its target suffix.
- **Packages.** Renaming a segment of a package name (in a `package` directive, an import or a qualified name) renames it in every `package` directive, import, qualified reference and KDoc link of the project, and moves the directory of the package (`src/app/model/` to `src/app/domain/`) when the directory holds only that package. Set `movePackageDirectories` to `false` to leave the directories. Renaming the directory in the editor renames the package, through `workspace/willRenameFiles`. A package that exists already can't be the new name.
- **`kargo.toml`.** The entry points name Kotlin declarations: `[target.jvm] main-class`, `[target.native] entry-point` and the `main` and `entry-point` of `[[bin]]`. Renaming the function `main`, the class a `[[bin]]` names, a package, or the file whose top-level `main` the entry point names (`Main.kt` is the class `MainKt`, unless the file has `@file:JvmName`) updates them, keeping the file's formatting and comments. The `path` of a `[[bin]]` follows a renamed file.

With an editor that takes `documentChanges`, the edit carries the version of each open document, so a rename made against text that has changed since is rejected by the editor instead of corrupting the file. Otherwise it is a plain `changes` map, without the file and directory renames.

Finding references and renaming search the whole project on a worker thread, so other requests keep answering while they run. An editor that shows `$/progress` gets a cancellable progress bar, and `$/cancelRequest` stops the search. If you edit a file while a search runs, the request fails with `ContentModified` and the editor asks again.

### Protocol details

- **Cancellation.** References, rename, `workspace/willRenameFiles` and `workspace/symbol` run on a worker thread, so the other requests keep answering, and `$/cancelRequest` stops them. A cancelled request fails with `RequestCancelled`.
- **Progress.** An editor that shows `$/progress` sees *Loading Kotlin project* while Kargo reads your sources and indexes the standard library and the dependencies, then *Fetching dependency sources* while sources jars download. Until the standard library is indexed, the first requests wait up to five seconds and then answer with what is loaded.
- **Workspace symbols.** The project's symbols come first, then those of the libraries when the query has at least three characters, 500 at most. An editor that supports `workspaceSymbol/resolve` (it lists `location.range` in `workspace.symbol.resolveSupport`) gets symbols without a range and asks for it when you pick one.
- **Position encoding.** When the editor lists `utf-8` in `general.positionEncodings`, columns count bytes, which needs no conversion; when it lists `utf-32` (and not `utf-8`), they count characters; otherwise they count UTF-16 units, as the protocol's default.
- **Renamed and deleted files.** The server asks for `workspace/didRenameFiles` and `workspace/didDeleteFiles` on Kotlin and Java files and on folders, so a file you rename, move or delete in the editor leaves the index at once (and its new path is read), without waiting for the file watcher or the next scan.
- **Status.** With `{ "status": true }` in `initializationOptions`, the server sends a `kargo/status` notification when it starts and stops indexing the project, for an editor's status bar. See [Status and logs](../editors/#status-and-logs).
- **Editors that don't watch files.** Without dynamic registration of `workspace/didChangeWatchedFiles` (some Emacs and Helix versions), the server looks for new, changed and deleted source files itself: on every save, and every five seconds. A saved `kargo.toml` reloads the project layout the same way.
- **Several projects in one window.** Every workspace folder gets its own project, found at the folder's Kargo project root. A request about a file goes to the project that contains it, and workspace symbols search all of them. Folders added or removed with `workspace/didChangeWorkspaceFolders` are loaded or dropped without a restart. Files outside every folder are served by the first project.

### Typing aids

- **On-type formatting** (`textDocument/onTypeFormatting`). After Enter, the new line gets the indentation of the line above, one level deeper after a line that ends with `{`, `(`, `[` or `->`. Inside a `/** */` or `/* */` comment it starts with ` * `, and a comment you just opened gets its ` */`. A `}` typed first on its line moves under the line of its `{`. A `"""` that opens a raw string nothing closes gets its closing `"""`, and a `{`, `(` or `[` nothing closes gets its closing bracket, for editors that don't pair them (Zed and Helix without auto-pairs). Editors that pair brackets themselves already close them, and then nothing is added. The indentation unit is the editor's `tabSize` and `insertSpaces`.
- **Linked editing** (`textDocument/linkedEditingRange`). With the cursor on a type parameter, its uses in the same signature (`fun <T> T.map(f: (T) -> R): List<T>`) and its `@param T` and `[T]` in the declaration's KDoc change together as you type. Inside the body, use [rename](#rename), which reaches everything.
- **Document links** (`textDocument/documentLink`). URLs (`http://` and `https://`) in comments, KDoc and strings are links, and so is a string that names a file that exists, relative to the file's directory, its package's directory or that package's `resources/`: `"resources/app.properties"`, or `getResource("/app.properties")`.
- **Inline values** (`textDocument/inlineValue`). While a [debugging session](../editors/#debugging) is stopped, editors that support it show the value of each parameter and local variable of the function next to its uses, from the start of the function to the stopped line. The values come from the debug adapter; the server tells the editor which names to look up.

### Formatting

*Format Document* (and format on save, where your editor has it) formats the open file with the same ktlint that [`kargo fmt`](../formatting-and-linting/) runs, so the editor and the command never disagree. The text of the editor goes through ktlint, so it formats what you see, saved or not, and ktlint reads the rules from the `.editorconfig` files above the file and the [`[fmt]`](../../reference/manifest/#fmt) settings of the package it belongs to. A file that `[fmt] exclude` leaves out is not changed.

The code action *Run `kargo fmt` on file* (kind `source.format`) does the same from the light bulb, for editors that have no *Format Document* or call it differently: the server runs ktlint and applies the edit with `workspace/applyEdit`. It is offered on open files when the editor supports `workspace/applyEdit`, through the `kargo/formatFile` command.

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
| Add remaining branches | A `when` over an enum, a `sealed` type or a `Boolean` whose branches don't name every case | Writes `Case -> TODO("Not yet implemented")` for each missing case, before the `else` branch if there is one, with the imports they need. With one case missing, it is *Add the missing branch `<case>`* |
| Add `actual` for `<name>` | An `expect` declaration in `src/` that a target of the package lacks | Writes the `actual` declaration, with `TODO()` bodies, to `src-<target>/` at the common file's path with the target in its name (`Platform.kt` gets `Platform.jvm.kt`): appended to that file, or in a new one when the editor can create files. *To every target* does them all at once |
| Add `override` | `kargo check`'s "hides member of supertype and needs an 'override' modifier" | Adds `override` before `fun`, `val` or `var` |
| Generate KDoc | A function, constructor or class without a comment, on its first line | Writes a comment with a `@param` for each parameter, a `@return` unless the function returns `Unit`, and a `@throws` for each exception its body throws (`throw Name(...)`). Nothing is written for a declaration with nothing to document |
| Remove variable | `kargo check`'s "Variable 'x' is never used" | Removes the declaration. An initializer that may have side effects (a call) stays as a statement. A destructured component or loop variable is renamed to `_` instead |
| Remove unused import | The server's own *unused import* warning | Removes the import's line |
| Remove variable `x` | The server's own *variable `x` is never used* warning, when `kargo check` doesn't report the same variable | Removes the declaration, as above. Destructured and delegated variables get no fix here |
| Replace with `<ReplaceWith>` | The server's own deprecation hint or error for a call | Replaces the call with the `ReplaceWith` expression, each argument taking the place of the parameter it is passed to (in parentheses when it is an expression). Not offered for a qualified call (`a.old()`), a call with a trailing lambda, a bare name of a function, or a `ReplaceWith` that names imports, where the expression may not mean the same thing |

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

### Refactorings

The server also offers refactorings, which editors list under *Refactor...* (`refactor.*` code actions). Each one is a single edit across files, so your editor's undo takes it back at once.

| Refactoring | Offered on | Does |
|---|---|---|
| Extract variable | A selected expression inside a block or lambda (`refactor.extract`) | Declares `val <name> = <expression>` before its statement, with the type when the server knows it fully (and the imports it needs), and puts the name in its place. The name comes from what the expression calls or reads (`user.name` gives `name`, `getUser()` gives `user`), else from its type. *Replace all N occurrences* also replaces the equal expressions of the block, when they read the same names and nothing between them calls or assigns anything |
| Extract function | Selected whole statements in a function's body (`refactor.extract`) | Moves them to a `private fun extracted(...)` after that function, in the same class or file and with its receiver and `suspend`. The locals they read become its parameters, and the one local they declare that the code after them uses becomes its result: `val total = extracted(items, prefix)` |
| Inline variable `<name>` | A local `val`, where it is declared or used (`refactor.inline`) | Puts its initializer in place of every use, in parentheses where precedence needs them and as `${...}` in a string template, and removes the declaration |
| Inline function `<name>` | A top-level function whose body is one expression (`= expr`, or `{ return expr }`), where it is declared or called (`refactor.inline`) | Puts the body in place of every call, with the arguments in place of the parameters, adds the imports the body needs where it is called, and removes the function and its imports. A `Unit` function whose body is one statement is inlined where it is called as a statement |
| Move parameter `<name>` left / right | A parameter of a function (`refactor.rewrite`) | Swaps it with its neighbor in the function, its overrides and its `expect`/`actual` counterparts, and in every call. A positional argument that would end up before a named one gets its name |
| Remove parameter `<name>` | A parameter that no body of the function or its overrides uses (`refactor.rewrite`) | Removes it from the declarations and its argument from every call |
| Move `<name>` to package `<package>` | The header of a top-level class, object, interface, function, property or type alias (`refactor.move`), for each other package of its source set (the first 12 by name) | Moves it to that package's directory, into `<Name>.kt` for a class or type alias and into a file of the same name as its own otherwise (appended when that file exists), with the imports it needs. Imports of it and names qualified with its package change, and the files that used it without an import get one |
| Move `<name>` to `<Name>.kt` | The header of a top-level class or type alias in a file with other declarations (`refactor.move`) | Moves it to a file of its own in the same directory and package |

Selecting `items.size` in `println(items.size * 2)` and choosing *Extract variable* gives:

```kotlin title="src/Main.kt"
val size: Int = items.size
println(size * 2)
```

A refactoring never changes what code does without telling you, so it is only offered when the server understands everything it rewrites:

- Every name in the code it moves must resolve. Selected code that reads something the server can't resolve gets no *Extract*, and a declaration that uses a `private` declaration of its file is not moved.
- Every use it rewrites must be one it knows how to change: a call with its arguments in parentheses, an import, a name qualified with a package. A function used as a value (`::square`), called with a trailing lambda in the place that changes, or named in a Java file of the project is not inlined or changed.
- Side effects stay where they were. An expression is extracted only where it always runs, once: not from the right of `&&`, `||` or `?:`, a branch of `if` or `when`, or a loop. One with calls also needs nothing else with side effects to run before it in its statement. A variable whose initializer calls something is inlined only into its one use in the next statement. An argument with calls is inlined only into a body that uses it once, unconditionally and in the same order; a parameter is removed, and two arguments swapped, only when that drops or reorders no call.
- Extract function refuses statements that `return`, `break` or `continue` out of themselves, assign a local of the function, declare more than one local that is used after them, or read a local that a check before them may smart cast.

Renaming is not one of them: use your editor's *Rename*, which also renames files, packages and `kargo.toml` entry points (see the table above).

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

A package also sees the main code of its [path dependencies](../workspaces/), and theirs, without its `internal` declarations: those are visible only inside their own package, in all its source sets. Test fixtures count as test code. With [Compose resources](../compose/#resources), the generated `Res` class counts as code in `src/`. `kargo metadata` writes it when the server loads the project, and every build updates it. A Kotlin file outside the source roots, open in the editor, sees the main code of every package.

So in a [multiplatform](../multiplatform/) package, common code in `src/` completes and navigates to the `expect` declarations, and `src-jvm/` sees both sides. Code in `src-wasm/`, `src-js/` and `src-native/` gets the same completion, navigation and rename as JVM code, against your sources and the standard library.

A Kargo script is not part of the project's sources, as Kargo does not compile it: other files don't see its declarations, and it doesn't count as a source of the project. The server notices new, changed and deleted `.kt` files, from the editor and on disk. When `kargo.toml` or `kargo.lock` change, it reads the project's layout again.

An edit updates only the index entries of the edited file, so typing stays fast in projects with thousands of files. Adding or deleting a file, or changing a file's `package`, rebuilds the project index. Looking up the scope or the declaration at a position is a binary search or a hash lookup, so large generated files resolve in linear time.

## Libraries


The server parses the sources of each library once. It stores the parsed files in a `.kargo-parsed` file in the library's folder under `~/.kargo/cache/lsp-sources`, and the next start reads them from there instead of parsing again (about 7 times faster for the standard library). Delete the file to parse again; a file from another Kargo version is ignored.

Each library's folder is written in a private staging folder and moved into place once it holds every Kotlin and Java file of its jar, so servers loading at once (one per workspace folder, or several editors) never leave a partial one. A `.kargo-complete` file records how many source files the folder has; a folder with files missing, or one made by an older Kargo, is extracted again on the next load. To start over anyway, stop the server, delete `~/.kargo/cache/lsp-sources` (or just the folder of one library), and start it again.

Libraries are seen per target, as the compiler sees them. Common code in `src/` compiles for every target of its package, so it sees only the libraries that every one of them has. `src-jvm/` sees the JVM's libraries, `src-wasm/` the Wasm ones and `src-js/` the JS ones. Test code also sees the libraries that only the test classpath has, such as `[dev-dependencies]`, and main code doesn't. A Kotlin file outside the source roots sees the libraries of every platform.

The **standard library** comes from its sources jar at the project's [`[kotlin] version`](../../reference/manifest/#kotlin), and so does `kotlin-test`, for tests. Kargo downloads them on first use from the project's repositories into its Maven cache, like any other artifact, and extracts them under `~/.kargo/cache/lsp-sources`, so go to definition opens real files. The server loads the standard library of each platform the project targets: the JVM's sources for `jvm` and `android`, and `kotlin-stdlib-wasm-js` (or `kotlin-stdlib-wasm-wasi` for `environment = "wasi"`) for `wasm`, and `kotlin-stdlib-js` for `js`, each with its `kotlin-test` for tests. Native code takes the parts of the Wasm one that Native shares. The JVM-only parts are offered to JVM code alone.

Offline before that first download (`kargo --offline lsp`), a built-in outline of the most used parts of the standard library stands in, with its documentation. The whole library replaces it on the next start with network access.

The **JDK** comes from the sources in its `lib/src.zip`. It is the JDK the project builds with: the installed one that `[toolchain] jdk` asks for, else the one on `JAVA_HOME` or the path. The server doesn't download a JDK; `kargo build` does. The JDK's classes are read when a name first needs them, so `java.io.File` and `System` complete, with their Javadoc, and go to definition opens the source file. A JDK without `lib/src.zip` leaves its classes unknown.

Kotlin's view of Java types matches the compiler's: `java.lang.String` is `String`, `int` is `Int`, `java.util.List` is Kotlin's `MutableList`, and so on. `java.lang` is imported into every Kotlin file on the JVM, as it is by the compiler.

The Kotlin classes for `String`, `CharSequence`, the collections and the maps also show the Java methods of their counterpart, so `"abc".chars()`, `codePointAt(0)` and a list's `stream()` complete. A Java method is left out when Kotlin already declares or offers a member of that name: `lines()` is Kotlin's extension, not Java's `Stream`-returning method, and `length` is Kotlin's property. Static Java methods don't complete on an instance. The server extends only these classes this way: `Int`, `Any` and the other Kotlin classes don't show their Java counterparts' methods.

**Dependencies** are resolved in the background, as `kargo metadata` does, and read whether or not they publish sources:

- A dependency whose **sources jar** is in the cache is read from it, Java files included, so hover shows its KDoc and Javadoc. The server downloads the missing sources jars itself, as `kargo fetch --sources` does: once the dependencies are resolved, it fetches the sources jar of every library that lacks one, in parallel and reported with `$/progress` ("Fetching dependency sources"), and then reads those libraries again from their sources. Until then, and for libraries that publish no sources, the compiled form below stands in, documented from the library's Javadoc jar when it publishes one (see [below](#documentation-from-javadoc-jars)).
- A dependency **without a sources jar** is read from its compiled form, for the targets whose classpath has it, as [above](#libraries) describes.
  - A **jar** is read from its class files. A class compiled from Kotlin keeps its Kotlin declarations in its metadata, so extension functions, nullability, `suspend`, default arguments, type aliases and companion objects come out as written. Any other class is read as Java: its public and protected fields, methods, constructors and nested classes, with generic types and, when the jar was compiled with `javac -parameters`, parameter names (otherwise `arg0`, `arg1`, ...).
  - A **klib** (the Wasm, JS and Native variants of a dependency) is read from its Kotlin metadata the same way.
- A **cinterop klib** (the C bindings a Native library publishes next to its own klib, such as `mylib-mingwx64-1.0-cinterop-mylib.klib`) is always read from its metadata, even when the library has a sources jar: cinterop generates those declarations from C headers, so no sources jar contains them. Hover names it `<artifact> <version> (cinterop)`.
- For Native code, the **Kotlin/Native standard library** of the distribution that builds the project is read from its klib for what the standard library's sources don't give Native code: `kotlinx.cinterop` (`CValue`, `memScoped`, `cValue`, `ExperimentalForeignApi`, ...), `kotlin.native`, `kotlin.concurrent.AtomicInt` and the like. Its internal packages and every declaration the sources already have are left out, so `println` still shows its documentation. Hover names it `Kotlin/Native stdlib`.
- For Wasm and JS code, the **npm packages** of [`[target.wasm.npm-dependencies]`](../wasm/#kotlin-declarations-for-npm-packages) and [`[target.js.npm-dependencies]`](../js/#npm-packages) are read from the Kotlin declarations Kargo generates from their TypeScript declarations. When the server opens a project, it installs the packages and generates the declarations if they changed, as a build does, so `express()` completes, shows the package's documentation in hover and goes to its declaration before the first build. Hover names them `npm`.
- Your own **TypeScript modules** in [`js/`](../wasm/#your-own-javascript-and-typescript) are read from the Kotlin declarations Kargo generates for them, for Wasm and JS code. The server writes them when it opens the project and again whenever a `.ts` or `.mts` file there is saved, created or deleted (it asks the editor to watch those files; an editor without file watchers gets them on save, through the rescan), without a build and without running tsc: only the declaration files whose text changed are rewritten, the libraries take them in, and the diagnostics of the open files are published again. Write `export function sum(a: number, b: number): number` in `js/sum.ts`, save, and `sum(1, 2)` resolves in Kotlin with completion and hover. Hover names them `js`.
- For Native code, the **platform libraries** of the Kotlin/Native distribution that builds the project (`platform.posix`, `platform.zlib`, and on macOS and iOS the Apple frameworks) are read for the first platform of [`[target.native] platforms`](../../reference/manifest/#targetnative).
- A [Kargo script](../scripts/) has its **own dependencies**: the `@file:DependsOn` coordinates and `@file:Repository` URLs in its header are resolved in the background, with the same mirrors as a project, and their libraries are read as above. Only that script sees them; the project's files don't, and the script still sees the project's libraries and the standard library. They are resolved again when the header changes, and the `fetchSources` setting below applies to them too. The libraries of `@file:Framework` are not loaded for a script yet.

A library without sources is written out once as stub files under `~/.kargo/cache/lsp-sources`: one Kotlin file per package, and one Java file for its Java classes. Go to definition opens these files, which show each declaration's signature, `...` for default values, and no bodies. Their documentation comes from the library's Javadoc jar (see below), or from the klib itself when it was compiled with its KDoc. They are generated, so edits to them change nothing. Private and internal declarations are left out, and so are those deprecated with `level = DeprecationLevel.HIDDEN`.

### Documentation from Javadoc jars

A library that publishes no sources jar often still publishes a `-javadoc.jar`. When the server looks for a sources jar and finds none, it downloads the Javadoc jar instead (so does `kargo fetch --sources`), and its pages become the doc comments of the stubs, so hover, completion and signature help show them:

- The description as Markdown: paragraphs, lists, code spans and `<pre>` blocks.
- `@param`, `@return`, `@throws` and `@see`, from the tags the page lists.
- The code of `@sample`s, from Dokka's pages.
- For Java methods compiled without `-parameters`, the real parameter names in place of `arg0`, `arg1`, ...

Two layouts are read: the standard doclet's (javadoc from JDK 8 on, and Dokka's `javadoc` format, which copies it), and Dokka's HTML format, which many Kotlin libraries publish as their Javadoc jar. A page is matched to a declaration by its class, name and parameter types; Kotlin top-level declarations are found on Dokka's package pages or on the page of their file class (`UtilsKt`). A klib compiled with its KDoc keeps its own documentation.

The pages are read once, when the stubs are generated, and the documented stubs are cached under `~/.kargo/cache/lsp-sources` with the jar and the Javadoc jar in their key, so a large library isn't read again in the next session. Like sources jars, Javadoc jars are not recorded in `kargo.lock`, and `kargo metadata` lists them in the `javadoc` field of a library without sources.

The standard library and `kotlin-test` come from their sources jars or the outline above, not from their compiled jars or klibs; only the Native-only parts of the Kotlin/Native standard library come from its klib, as described above.

When `kargo.toml` or `kargo.lock` change, after `kargo add` for example, the server resolves the dependencies again, fetches the new ones' sources jars and loads the libraries again, without a restart. Files of libraries that didn't change are not parsed again. A library whose repositories publish no sources jar is not asked for one again until the language server restarts, nor is one whose Javadoc jar is already in the cache; a download that failed is retried on the next change.

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

- It reports only syntax problems, unused code and deprecated usages of its own (see the table above and [Unused code and deprecated usages](#unused-code-and-deprecated-usages)). It doesn't report unresolved references or type errors. Those come from `kargo check` on save, which compiles the code for real. Turn it off with `{ "checkOnSave": false }` in the editor's `initializationOptions`; see [Check on save](../editors/#check-on-save).
- It reads Java but doesn't edit it. Java files get no completion, hover, rename or references of their own, and a Java declaration can't be renamed from Kotlin: the server refuses, because the Java code that uses it wouldn't change. References to a Kotlin declaration from Java code aren't searched.
- Libraries without sources are documented only when they publish a Javadoc jar in the standard doclet's or Dokka's HTML layout (see [Documentation from Javadoc jars](#documentation-from-javadoc-jars)); the JDK has no fallback to its online Javadoc when it has no `src.zip`. The compiler's context-sensitive features are not all followed: a property's accessors don't see its `context(...)`, and name-based destructuring (`(val a, val b)`) is read as positional.
- Kotlin/Native code gets the standard library's parts that its Wasm sources jar shares with Native, plus the Native-only declarations from the distribution's klib, which have no documentation in hover.
- Smart casts of parameters and `val`s are followed: inside `if (x is Foo)` (and after `&&`), in the branch of `when (x) { is Foo -> }` or `when { x is Foo -> }`, and after `if (x !is Foo) return`, `x as Foo` or `require(x is Foo)`. Null checks narrow the same way (`x != null`, `if (x == null) return`, `x!!`, `x ?: return`). A check against a type that is neither a subtype nor a supertype of the declared one gives both, so `if (x is CharSequence)` on a `T : Comparable<T>` offers the members of both. Smart casts of `var`s and properties are not followed.
- Nullability is tracked: `x.` on a nullable value offers only the extensions of nullable types (`isNullOrEmpty`, `let`, ...), `x?.` offers the members of the unwrapped type, and the result of a safe call is nullable again. Types from Java count as non-null.
- Operators are found by their syntax, with each operand's type taken from the expression around it: a long expression with infix calls or lambdas inside can leave an operator unresolved, and a `for` loop's `hasNext` and `next` are looked up on the type that `iterator()` returns. `a { }` is an `invoke` only when `a` is a value of a class type; a function-typed value is a plain call.
- Delegates: a type argument that only the expected type decides (`val x by viewModels()` without a declared type) stays unknown, so completion after `x.` is empty; write `viewModels<Foo>()` or `val x: Foo by viewModels()`. Renaming a `getValue` is refused like any operator function.
- The cases of a `when` over a `sealed` type are its direct subclasses in your project, and in the file that declares it when it comes from a library. A branch counts as covering a case when it names it, so a branch on a nested sealed class (`is Shape.Round` for `Circle` and `Oval`) leaves them offered.
- Overloads are chosen by the arguments' types (subtypes, the type arguments by their variance, so `List<String>` fits `List<out CharSequence>` but not a `MutableList<CharSequence>`, nullability, named and default arguments, and the parameter count of lambdas); when an argument's type is unknown and several overloads fit, go to definition lists them all. A type argument is not inferred from the expected type of a call (`foo(emptyList())`), so completion may miss members of such expressions. A `when` or `try` whose branches have nothing in common but `Any` completes as `Any`.
- Its [code actions](#code-actions) and [refactorings](#refactorings) are the ones listed there; it has no other quick fixes, and refactorings such as adding a parameter, moving to a package that doesn't exist yet, or converting between expression and block bodies are not there yet. Semantic highlighting leaves keywords, literals and comments to the editor's own grammar. [Formatting](#formatting) is ktlint's: whole files only.

For the full analysis of the Kotlin compiler, use IntelliJ IDEA with the Kargo plugin, or run JetBrains' Kotlin LSP behind `kargo lsp` with `--server kotlin-lsp`. See [Choosing the Kotlin server](../editors/#choosing-the-kotlin-server).
