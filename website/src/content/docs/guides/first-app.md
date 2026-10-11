---
title: Your first Kotlin app
description: A step-by-step tutorial for people who are new to Kotlin. Build a word counter, test it, and ship it as a jar.
---

This tutorial is for people who have never written Kotlin before. By the end you will have built a small command-line tool that counts the most common words in a text file, written tests for it, and packaged it as a single file you can share.

You'll need Kargo and a JDK. See [Installation](../../getting-started/installation/) if you haven't set them up yet.

:::tip[Editor]
Any editor works. [IntelliJ IDEA](https://www.jetbrains.com/idea/) has the best Kotlin support. [VS Code](https://code.visualstudio.com) with a Kotlin extension is a lighter alternative.
:::

## Step 1: Create the project

```console
$ kargo new wordcount
     Created application `wordcount` package
$ cd wordcount
$ kargo run
Hello, Kotlin!
```

Open `kargo.toml`. This file describes your project:

```toml title="kargo.toml"
[package]
name = "wordcount"        # the project name
version = "0.1.0"
group = "com.example"     # your organisation; also the start of the Kotlin package name
```

That is all a JVM project needs. Kargo fills in the rest with defaults: Kotlin 2.4.21 and JVM bytecode target 17. To change them, or to add libraries, add the matching sections:

```toml title="kargo.toml"
[kotlin]
version = "2.4.21"        # which Kotlin compiler to use

[target.jvm]
jvm-target = "17"         # the Java version the compiled code runs on

[dependencies]            # libraries your code uses (`kargo add` creates this)

[dev-dependencies]        # libraries only your tests use (`kargo add --dev`)
```

## Step 2: Kotlin in five minutes

Replace `src/Main.kt` with this and run it with `kargo run`:

```kotlin title="src/Main.kt"
package com.example.wordcount

fun main() {
    // `val` is a read-only variable; `var` can be reassigned.
    val language = "Kotlin"
    var count = 1
    count += 1

    // String templates: put $name or ${expression} inside a string.
    println("Hello from $language, count is $count")

    // Lists and lambdas.
    val numbers = listOf(3, 1, 4, 1, 5, 9)
    val doubled = numbers.map { it * 2 }
    println("Doubled: $doubled")
    println("Sum of evens: ${numbers.filter { it % 2 == 0 }.sum()}")

    // `if` and `when` are expressions: they return a value.
    val size = if (numbers.size > 5) "long" else "short"
    val description = when (count) {
        0 -> "none"
        1 -> "one"
        else -> "many"
    }
    println("The list is $size and the count is $description")

    // Null safety: a type ending in `?` can be null, and Kotlin makes you handle it.
    val maybe: String? = numbers.firstOrNull { it > 100 }?.toString()
    println("First big number: ${maybe ?: "none"}")
}
```

The most important ideas are `val`/`var`, string templates, lambdas (`{ it * 2 }`), and nullable types (`String?`). The [official Kotlin tour](https://kotlinlang.org/docs/kotlin-tour-welcome.html) covers the rest.

## Step 3: Write the logic

Good programs keep the logic separate from input and output, because that makes the logic easy to test. Create a new file for the counting code:

```kotlin title="src/WordCount.kt"
package com.example.wordcount

/** A word and how many times it appears. */
data class WordFrequency(val word: String, val count: Int)

/** Splits [text] into lowercase words, ignoring punctuation and numbers. */
fun words(text: String): List<String> =
    Regex("[\\p{L}']+")
        .findAll(text.lowercase())
        .map { it.value.trim('\'') }
        .filter { it.isNotEmpty() }
        .toList()

/** Returns the [limit] most frequent words in [text], most frequent first. */
fun topWords(text: String, limit: Int = 10): List<WordFrequency> =
    words(text)
        .groupingBy { it }
        .eachCount()
        .map { (word, count) -> WordFrequency(word, count) }
        .sortedWith(compareByDescending<WordFrequency> { it.count }.thenBy { it.word })
        .take(limit)
```

A few new things here:

- `data class` creates a class with `equals`, `hashCode`, `toString` and `copy` generated for you.
- `limit: Int = 10` is a default argument, so callers can leave it out.
- Functions whose body is a single expression can use `=` instead of `{ return ... }`.

## Step 4: Read a file from the command line

Now wire it up in `main`:

```kotlin title="src/Main.kt"
package com.example.wordcount

import java.io.File
import kotlin.system.exitProcess

fun main(args: Array<String>) {
    if (args.isEmpty()) {
        System.err.println("usage: wordcount <file> [limit]")
        exitProcess(1)
    }
    val file = File(args[0])
    if (!file.isFile) {
        System.err.println("error: ${file.path} is not a file")
        exitProcess(1)
    }
    val limit = args.getOrNull(1)?.toIntOrNull() ?: 10

    for ((index, entry) in topWords(file.readText(), limit).withIndex()) {
        println("${(index + 1).toString().padStart(3)}. ${entry.word.padEnd(15)} ${entry.count}")
    }
}
```

Create a text file and try it. Everything after `--` is passed to your program:

```console
$ echo "the cat and the hat and the bat" > sample.txt
$ kargo run -- sample.txt 3
  1. the             3
  2. and             2
  3. bat             1
```

## Step 5: Test it

Delete `tests/GreetingTest.kt` (it tests the old `greeting` function) and add tests for the new code:

```kotlin title="tests/WordCountTest.kt"
package com.example.wordcount

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class WordCountTest {
    @Test
    fun splitsAndLowercasesWords() {
        assertEquals(listOf("hello", "world", "it's"), words("Hello, WORLD! It's 42."))
    }

    @Test
    fun countsAndSortsByFrequency() {
        val top = topWords("b a b c b a", limit = 2)
        assertEquals(listOf(WordFrequency("b", 3), WordFrequency("a", 2)), top)
    }

    @Test
    fun emptyTextHasNoWords() {
        assertTrue(topWords("").isEmpty())
    }
}
```

```sh
kargo test
```

Run only the test classes whose name contains a string:

```sh
kargo test WordCount
```

If you break `topWords` on purpose (for example, change `compareByDescending` to `compareBy`), `kargo test` shows which assertion failed and exits with an error.

## Step 6: Use a library

Let's add a `--json` flag using [kotlinx.serialization](https://github.com/Kotlin/kotlinx.serialization). It needs both a library and a compiler plugin, which Kargo sets up for you as a [built-in framework](../frameworks/):

```sh
kargo add --framework serialization
```

That adds `serialization = true` to a `[frameworks]` section. Mark the data class as `@Serializable`:

```kotlin title="src/WordCount.kt" ins={3,5}
package com.example.wordcount

import kotlinx.serialization.Serializable

@Serializable
data class WordFrequency(val word: String, val count: Int)
```

And use it in `main`:

```kotlin title="src/Main.kt"
import kotlinx.serialization.json.Json
// ...
    val json = "--json" in args
    val positional = args.filter { !it.startsWith("--") }
    // ... read positional[0] and positional.getOrNull(1) instead of args[0] and args[1]
    val top = topWords(file.readText(), limit)
    if (json) {
        println(Json.encodeToString(top))
        return
    }
```

```console
$ kargo run -- sample.txt 2 --json
[{"word":"the","count":3},{"word":"and","count":2}]
```

Any library on Maven Central can be added the same way with `kargo add group:artifact`. See [Managing dependencies](../dependencies/).

## Step 7: Ship it

Build a release jar that includes all dependencies:

```console
$ kargo package --release
$ java -jar target/release/jvm/wordcount-0.1.0-all.jar sample.txt
```

Anyone with Java installed can run that file.

## Where to go next

- Make it run in the browser or as a native binary: [Multiplatform projects](../multiplatform/)
- Turn it into a web service: [A REST API with Ktor](../ktor/)
- Automate chores like formatting: [Tasks and plugins](../tasks-and-plugins/)
