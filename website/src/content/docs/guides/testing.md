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
kargo test --release       # with the release profile
```

`kargo test` exits with status `101` if a test fails, so it works in scripts and CI.

On the JVM every class in `tests/` and `tests-jvm/` is scanned, whatever its name: a class called `T` or a Kotest spec called `UserSpec` runs just like `UserTest`. If test sources compiled but no test was found, or the filter matched nothing, `kargo test` reports an error and exits with `101` instead of silently passing.

## Filtering tests

The filter keeps the tests whose fully qualified `package.Class.method` name contains it, on the JVM, Native and Android:

| Filter | Runs |
|---|---|
| `CalculatorTest` | every test in classes whose name contains `CalculatorTest` |
| `com.example.calc` | every test in that package |
| `CalculatorTest.adds` (or `CalculatorTest#adds`) | that test method |
| `adds` | every test method whose name contains `adds` |

On the JVM, a filter that matches a test class name selects whole classes, which works with every JUnit Platform engine. A filter that matches no class name is matched against method names, which only finds tests of method-based engines such as JUnit Jupiter (what `kotlin.test` uses), not Kotest specs.

## What runs where

| Target | Test runner | Default test libraries |
|---|---|---|
| JVM | JUnit Platform | `kotlin-test-junit5`, `junit-jupiter` |
| Wasm | Node.js | `kotlin-test-wasm-js` / `kotlin-test-wasm-wasi` |
| Native | the native test binary | built into Kotlin/Native |
| Android | Gradle local unit tests (`testDebugUnitTest`) | `kotlin-test-junit`, `junit` |

Test filters are not supported on Wasm; all tests run.

## JVM-only test libraries

Because JVM tests run on the JUnit Platform, JUnit 5 annotations and libraries like [MockK](https://mockk.io), [AssertJ](https://assertj.github.io/doc/) or [Kotest assertions](https://kotest.io) work. Add them as dev-dependencies for the JVM:

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

## Hooks

Run something before or after tests with a [task or hook plugin](../tasks-and-plugins/) attached to `pre-test` or `post-test`. After tests, hooks receive `KARGO_TEST_RESULT`.
