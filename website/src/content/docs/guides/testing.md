---
title: Testing
description: Write and run tests with kotlin.test on every target.
---

Tests live in `tests/` (common to all targets) and `tests-<target>/` (one target only). They can use everything in `src/`, including `internal` declarations.

## Writing tests

[`kotlin.test`](https://kotlinlang.org/api/core/kotlin-test/) works on every target and is set up for you:

```kotlin title="tests/CalculatorTest.kt"
package com.example.calc

import kotlin.test.*

class CalculatorTest {
    @Test
    fun adds() {
        assertEquals(4, add(2, 2))
    }

    @Test
    fun rejectsDivisionByZero() {
        assertFailsWith<ArithmeticException> { divide(1, 0) }
    }

    @Test
    fun parsesNumbers() {
        assertNull("abc".toIntOrNull())
        assertTrue(isEven(2), "2 should be even")
    }
}
```

`@BeforeTest` and `@AfterTest` run around every test.

## Running tests

```sh
kargo test                 # all enabled targets
kargo test -t jvm          # one target
kargo test Calculator      # only tests whose package.Class.method contains "Calculator"
kargo test Calculator.adds # one test
kargo test --no-run        # compile tests without running them
kargo test --no-fail-fast  # test every target even if one fails
kargo check --tests        # type-check tests without linking (fastest; see kargo check)
kargo test --release       # with the release profile
kargo test --profile staging # with a custom profile: its jvm-args reach the test JVM too
```

`kargo test` exits with status `101` if a test fails, so it works in scripts and CI.

Native and Wasm test lines show how long each test took, as in `test MathTest.adds ... ok (12ms)`, and their result line ends with the total time of the run (`finished in 1.50s`). The JVM report ends with the launcher's own total instead. With `--message-format json` each test's `duration-ms` is in its [result message](../../reference/machine-readable-output/#test-results), and `--report junit` writes the time of every test.

With several targets enabled, the tests of every target compile at the same time; they then run one target after another, so each target's results stay together. See [Build performance](../build-performance/).

On the JVM every class in `tests/` and `tests-jvm/` is scanned, whatever its name: a class called `T` or a Kotest spec called `UserSpec` runs just like `UserTest`. If test sources compiled but no test was found, or the filter matched nothing, `kargo test` reports an error and exits with `101` instead of silently passing.

## Rerunning tests on change

`kargo test --watch` runs the tests again after every change to the project's files, until you press Ctrl+C. A failing test does not stop it. See [Watch mode](../watch/).

## Testing every target

By default `kargo test` stops at the first target whose tests fail or don't compile, like `cargo test`: the tests of later targets still compile, but don't run. It notes the targets it skipped:

```console
$ kargo test
...
note: skipped the native target after a failure; pass `--no-fail-fast` to test every target
error: test failed
```

Pass `--no-fail-fast` to test the remaining targets anyway. Each target's tests run, errors are reported per target, and a table at the end shows how every target went:

```console
$ kargo test --no-fail-fast
...
test results by target:
    jvm      ok
    wasm     error (could not compile or run the tests)
    native   ok
error: test failed
```

`kargo test` still exits with `101` if any target failed. To make this the default, for example for CI, set it in `kargo.toml`:

```toml title="kargo.toml"
[test]
fail-fast = false
```

`kargo build --keep-going` and `kargo check --keep-going` do the same for builds: every target is built and each failure is reported.

## Test reports for CI

CI systems such as GitHub Actions, GitLab and Jenkins show per-test results from JUnit XML files. Pass `--report junit` and Kargo writes them next to each target's build output, in addition to the usual console report:

```console
$ kargo test --report junit
     Running JVM tests (JUnit Platform)
...
    Reported JUnit XML in target/debug/jvm/test-reports
```

| Target | Report files in `target/<profile>/<target>/test-reports/` |
|---|---|
| JVM | Written by the JUnit console launcher: one `TEST-<engine>.xml` per test engine, such as `TEST-junit-jupiter.xml` |
| Wasm | `TEST-wasm.xml`, built from the test runner's results |
| Native | `TEST-native.xml`, built from the test runner's results |
| Android | Gradle's `TEST-<class>.xml` files, copied from `target/android/build/test-results` |

Each report has one `<testsuite>` per test class, with each test's time, failure message and stack trace, and what it printed. The directory is cleared at the start of every run, so it never holds results from an earlier one. See [CI](../ci/#test-results) for publishing the reports.

When a Kotlin/Native test fails, the failure list shows the assertion message first, then the stack trace without the Kotlin/Native runtime's own frames (they end with `... N runtime frames omitted`), so the first frame is yours.

## Filtering tests

The filter keeps the tests whose fully qualified `package.Class.method` name contains it, on the JVM, Native and Android:

| Filter | Runs |
|---|---|
| `CalculatorTest` | every test in classes whose name contains `CalculatorTest` |
| `com.example.calc` | every test in that package |
| `CalculatorTest.adds` (or `CalculatorTest#adds`) | that test method |
| `adds` | every test method whose name contains `adds` |

On the JVM, a filter that matches a test class name selects whole classes, which works with every JUnit Platform engine. A filter that matches no class name is matched against method names, which only finds tests of method-based engines such as JUnit Jupiter (what `kotlin.test` uses), not Kotest specs.

## Tags

JUnit's `@Tag` separates slow or integration tests from the rest. `--include-tag` keeps only the tests with a tag, and `--exclude-tag` skips them. Both can be repeated, and both combine with a filter:

```sh
$ kargo test --exclude-tag slow          # everything but the slow tests
$ kargo test --include-tag integration   # only the integration tests
```

```kotlin title="tests/CalculatorTest.kt"
import kotlin.test.Test
import org.junit.jupiter.api.Tag

class CalculatorTest {
    @Test
    fun adds() { /* ... */ }

    @Test
    @Tag("slow")
    fun loadsTheFullTable() { /* ... */ }
}
```

Tags apply to JVM tests. Kargo says so when another target runs with them, and that target's tests run unfiltered. When no test matches, the run fails and says the filters matched nothing.

## Timeouts

Two limits in `kargo.toml` stop a test that hangs or runs too long:

```toml title="kargo.toml"
[test]
timeout = "5m"         # the whole test run
test-timeout = "30s"   # each JVM test
```

`timeout` limits the whole run. When it passes, Kargo kills the test process and fails with `JVM tests timed out after 5m; the whole test run is limited by [test] timeout`. The clock starts when the tests start running, so compiling them doesn't count. It applies to the JVM, Native and Wasm targets; Android tests run under Gradle, which Kargo doesn't limit.

`test-timeout` limits each JVM test with JUnit Jupiter's `junit.jupiter.execution.timeout.default`. A test that runs longer fails, and the rest of the run goes on. It applies to tests run by JUnit Jupiter, which includes `kotlin.test` on the JVM. Other targets have no per-test limit, and Kargo says so when they run.

Both take a whole number and a unit: `500ms`, `30s`, `5m` or `1h`.

## Rerunning failures

`kargo test --failed` runs only the JVM test classes that had a failing test in the last run, so a long suite can be fixed one class at a time:

```sh
$ kargo test          # some tests fail
$ kargo test --failed # runs the classes that failed, and nothing else
```

Every JVM run records its failing classes in `target/<profile>/jvm/test-failures`, replacing the previous record. A run that passed records none, and `--failed` then says nothing failed and runs nothing. Without a record, `--failed` runs every test and says so. A run that didn't finish, such as one that was killed, records nothing for the same reason.

The rerun works on whole test classes, not single methods, so a class with a failing test runs in full. `--failed` applies to the JVM target only; other targets run all their tests.

## When a test run stops early

A test that calls `System.exit`, or a JVM that runs out of memory, ends the test process before every test has run. Kargo reports that rather than leaving you to look for a failing assertion:

```console
$ kargo test
error: test JVM exited with code 5 before finishing
```

In the terminal report, exit code 1 is the same as a failing test, so such a run is reported as failed tests. With `--message-format json`, Kargo's listener reports the end of the run, so a run that stops early with exit code 1 is reported as stopped early as well.

## Listing tests

`--list` compiles the tests and prints them without running any:

```console
$ kargo test --list
   Compiling app v0.1.0 (jvm)
   Compiling app v0.1.0 (jvm, tests)
jvm: com.example.app.GreetingTest.greetsByName
jvm: com.example.app.MathTest.adds
      Listed 2 jvm tests
```

A filter narrows the list the way it narrows a run. `--format json` prints one JSON object per test instead, with its id, class, name, display name, source file and line, for editors and other tools; see [Test discovery](../../reference/machine-readable-output/#test-discovery).

JVM and Native tests can be listed. Wasm and Android tests can't be listed yet; `--list` says so and moves on.

## Test results as JSON

Editors that show a test explorer can follow a run test by test:

```sh
kargo test --message-format json
```

Each test prints a `test-started` and a `test-finished` JSON line on stdout as it runs, with its outcome, duration, failure message and stack trace, and what it printed. Each target ends with a `test-summary` line. The usual report goes to stderr. See [Test results](../../reference/machine-readable-output/#test-results) for the fields.

## Debugging tests

`--debug` starts the JVM that runs the tests suspended, waiting for a debugger to attach over JDWP on port 5005 of `127.0.0.1` (`--debug=<port>` for another one). Combine it with a filter to stop in one test:

```console
$ kargo test --debug CalculatorTest.adds
     Running JVM tests (JUnit Platform)
note: the test JVM waits for a debugger: attach one over JDWP to 127.0.0.1 on port 5005
Listening for transport dt_socket at address: 5005
```

Attach your editor's debugger as described in [Debugging](../editors/#debugging). `--debug` only debugs JVM tests, so a project with several targets needs `-t jvm`.

Editors that use the [`kargo dap`](../editors/#debug-adapter) debug adapter do this for you: a launch with `"args": ["test", "CalculatorTest.adds"]` runs the test under the debugger and attaches.

## What runs where

| Target | Test runner | Default test libraries |
|---|---|---|
| JVM | JUnit 6 on the JUnit Platform | `kotlin-test-junit5`, `junit-jupiter` |
| Wasm | Node.js, or a headless browser with `environment = "browser"` | `kotlin-test-wasm-js` / `kotlin-test-wasm-wasi` |
| Native | the native test binary | built into Kotlin/Native |
| Android | Gradle local unit tests (`testDebugUnitTest`) | `kotlin-test-junit`, `junit` |

Test filters are not supported on Wasm; all tests run.

JVM tests run on JUnit 6, which needs JDK 17 or newer, because `kargo test` starts its launcher on the project's JDK. If the JDK that builds your project is older, pin a newer one with [`[toolchain] jdk`](../toolchains/).

With `environment = "browser"`, Kargo serves a test page, starts Chrome, Chromium, Edge, Brave or Firefox without a window to load it, and prints the results the page reports, so output, failures and `--report junit` look the same as on Node.js. See [Testing in a browser](../wasm/#testing-in-a-browser).

## JVM-only test libraries

Because JVM tests run on JUnit 6, JUnit Jupiter annotations and libraries like [MockK](https://mockk.io), [AssertJ](https://assertj.github.io/doc/) or [Kotest assertions](https://kotest.io) work. Add them as dev-dependencies for the JVM:

```sh
kargo add io.mockk:mockk --dev
```

```kotlin title="tests-jvm/ServiceTest.kt"
import io.mockk.every
import io.mockk.mockk
import kotlin.test.Test
import kotlin.test.assertEquals

class ServiceTest {
    @Test
    fun usesTheRepository() {
        val repo = mockk<UserRepository>()
        every { repo.find(1) } returns User(1, "Ada")
        assertEquals("Ada", UserService(repo).nameOf(1))
    }
}
```

## Coroutines

With the `coroutines` [framework](../frameworks/) enabled, `kotlinx-coroutines-test` is added to JVM tests:

```kotlin
import kotlinx.coroutines.test.runTest

@Test
fun loadsData() = runTest {
    assertEquals(3, repository.loadAll().size)
}
```

## Test resources

Files in `test-resources/` are on the classpath of JVM tests:

```kotlin
val text = javaClass.getResource("/fixtures/sample.json")!!.readText()
```

## Test fixtures

Test helpers that the tests of several packages share (fakes, builders, sample data) go in `test-fixtures/` (and JVM-only ones in `test-fixtures-jvm/`). They compile against the package and its dev-dependencies, and are on the classpath of the package's own JVM tests and of every package that depends on it with `test-fixtures = true`:

```toml title="kargo.toml"
[dev-dependencies]
mathlib = { path = "../mathlib", test-fixtures = true }
```

See [Test fixtures](../workspaces/#test-fixtures).

## Hooks

Run something before or after tests with a [task or hook plugin](../tasks-and-plugins/) attached to `pre-test` or `post-test`. After tests, hooks receive `KARGO_TEST_RESULT`.
