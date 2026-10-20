---
title: Benchmarking
description: Measure the speed of JVM code with JMH, compare runs against a baseline, and keep results in CI.
---

Timing code in a test or in `main` gives misleading numbers on the JVM: the JIT compiler has not warmed up, the garbage collector adds noise, and code whose result is never used is removed. `kargo bench` runs your benchmarks with [JMH](https://github.com/openjdk/jmh), the benchmark harness of the OpenJDK, using the annotations of [kotlinx-benchmark](https://github.com/Kotlin/kotlinx-benchmark). There is no Gradle plugin and no annotation processing to set up.

:::note
Benchmarks run on the JVM target. Kotlin/Native and Wasm benchmarks are not supported yet: `kargo bench -t native` and `-t wasm` say so and stop.
:::

## Write a benchmark

Benchmarks live in `benches/` (common to all targets, but only the JVM target runs them) and `benches-jvm/`. They are compiled against your `src/` code, like tests, so they can use `internal` declarations and your dev-dependencies. They are not part of `kargo build`, `kargo test`, `kargo package` or `kargo publish`.

```kotlin title="benches/ParseBench.kt"
package com.example.parser

import kotlinx.benchmark.Benchmark
import kotlinx.benchmark.Scope
import kotlinx.benchmark.State

@State(Scope.Benchmark)
class ParseBench {
    private val input = "1 + 2 * (3 - 4)".repeat(100)

    @Benchmark
    fun parse(): Ast = Parser(input).parse()
}
```

JMH calls each `@Benchmark` function many times and measures the time. Keep the setup in the class, not in the function. The value a function returns is consumed for you, so the JIT compiler can't drop the work. A benchmark that returns nothing can pass results to a `Blackhole` parameter instead.

To start from a sample, create a project with `kargo new --bench`, or add the framework to an existing one:

```sh
kargo new fast --bench              # a project with benches/SampleBench.kt
kargo add --framework benchmark     # [frameworks] benchmark = true, and a sample if benches/ is empty
```

The `benchmark` [framework](../frameworks/#benchmark) puts kotlinx-benchmark and JMH on the benchmarks' classpath, so your editor resolves the annotations. `kargo bench` works without it: it adds the same libraries itself.

The annotations you will use most:

| Annotation | Meaning |
|---|---|
| `@State(Scope.Benchmark)` | Marks the class that holds the benchmarks and their data. `Scope.Thread` gives each thread its own instance |
| `@Benchmark` | A function to measure |
| `@Setup` / `@TearDown` | Run before and after the benchmarks of the class (`Level.Trial`, `Level.Iteration` or `Level.Invocation`) |
| `@Param("10", "1000")` | Runs every benchmark of the class once for each value of this `var` |
| `@BenchmarkMode`, `@OutputTimeUnit` | What to measure and in which unit, for one class or function |
| `@Warmup`, `@Measurement`, `@Fork` | Iterations and forks, for one class or function |
| `Blackhole` | A parameter of a benchmark function to `consume(...)` results with |

Kargo compiles benchmark classes with the Kotlin `allopen` plugin for `@State`, because JMH generates subclasses of them and Kotlin classes are final.

## Run benchmarks

```console
$ kargo bench
   Compiling fast v0.1.0 (jvm)
   Compiling fast v0.1.0 (jvm, benches)
  Generating fast v0.1.0 (jvm, JMH harness)
   Compiling fast v0.1.0 (jvm, JMH harness)
     Running benchmarks (JMH 1.37)
Benchmarking com.example.fast.SampleBench.sum
Benchmarking com.example.fast.SampleBench.sumOfSquares

benchmark                                  mode   score  error
com.example.fast.SampleBench.sum           avgt  400 ns  ± 17.5 ns
com.example.fast.SampleBench.sumOfSquares  avgt  367 ns  ± 9.48 ns
    Finished `release` profile [optimized] target(s) in 36.63s
```

Benchmarks always build with the `release` profile, or with `[profile.bench]` when your `kargo.toml` defines one (see [Profiles](../../reference/manifest/#profilename)). `--release` and `--profile <name>` choose another one. A filter selects benchmarks by name:

```sh
kargo bench Parse           # benchmarks whose package.Class.method contains "Parse"
kargo bench ParseBench#parse # one method of one class
kargo bench -t jvm          # the JVM target (the only one that runs benchmarks)
kargo bench --no-run        # compile the benchmarks and generate the harness only
```

JMH runs each benchmark in separate JVMs ("forks") so that one benchmark's JIT decisions do not leak into the next, and measures after a warmup. The time this takes is set in `[bench]`, or with flags.

## Settings

Set the iterations, forks, mode and unit in [`[bench]`](../../reference/manifest/#bench):

```toml title="kargo.toml"
[bench]
warmup = "5x1s"      # 5 warmup iterations of 1 second
iterations = "5x1s"  # 5 measured iterations of 1 second
forks = 2            # JVMs per benchmark; 0 runs in the launcher's JVM
mode = "avgt"        # thrpt, avgt, sample, ss or all
time-unit = "us"     # ns, us, ms, s, m, h or d
jvm-args = ["-Xmx2g"]
```

The flags `--warmup`, `--iterations`, `--forks`, `--mode` and `--time-unit` take the same values and win over `kargo.toml`. A setting is taken from the first of these that sets it:

1. the command-line flag,
2. `[bench]` in `kargo.toml`,
3. the JMH annotation on the benchmark (`@Warmup`, `@Measurement`, `@Fork`, `@BenchmarkMode` or `@OutputTimeUnit`, found in the sources of `benches/`),
4. Kargo's default: `warmup = "3x1s"`, `iterations = "5x1s"`, `forks = 2`, `mode = "avgt"`.

Kargo's defaults are much shorter than JMH's own (10-second iterations, 5 forks), so a run takes seconds per benchmark. For results you want to rely on, raise `iterations` and `forks`.

When you set no time unit, each score is shown in whichever unit reads best (`1.20 ms`, `85.0 ns`, `4.50 Mops/s`). With `time-unit` (or `@OutputTimeUnit`) scores use that unit.

Environment variables from `[env]` and `.env` files reach the benchmark JVMs as they do for `kargo run` and `kargo test`; `--env-file` and `--no-env-file` work too.

## Compare runs with a baseline

Save a run under a name, change your code, and run again with `--baseline`:

```console
$ kargo bench --save main
...
       Saved baseline `main` in target/bench/main.json

$ kargo bench --baseline main
...
com.example.fast.SampleBench.sum           400 ns -> 497 ns  (+24.1%, p < 0.05)
com.example.fast.SampleBench.sumOfSquares  367 ns -> 439 ns  (+19.8%, p < 0.05)

0 faster, 2 slower, 0 unchanged
```

Baselines are JMH's JSON results in `target/bench/<name>.json` (`target/bench/<member>/` in a workspace). `--save` replaces a baseline of the same name; `--save` and `--baseline` together compare first, then save. Benchmarks are matched by name, mode and `@Param` values; ones the baseline lacks are shown as `not in the baseline`, and baseline entries that did not run are left out.

Computers are noisy, so Kargo does not call every difference a change. It compares the scores of all measured iterations of both runs with Welch's t-test. When the p-value is below 0.05, the line shows `p < 0.05` and counts as faster or slower (green or red in a terminal). Otherwise it reads `no significant change` with the p-value. When a run has too few iterations to test, a difference counts when the error intervals of the two scores don't overlap.

For time per operation (`avgt`, `sample`, `ss`) a smaller score is faster; for throughput (`thrpt`) a larger one is.

Compare on the same machine, with the same JDK and settings, and close other programs while you measure.

## Output formats

`--format json` prints JMH's JSON results on stdout, the format other JMH tools read, and moves the report to stderr:

```sh
kargo bench --format json > results.json
```

Each entry has the benchmark, its parameters, the score with its error, and the score of every iteration (`rawData`).

`--verbose` shows JMH's own output while it runs. If JMH fails, for example because a benchmark threw an exception, its whole output is printed and `kargo bench` exits with `101`.

## Continuous integration

Run a benchmark job on a machine that does nothing else, upload the JSON, and compare with the baseline of the main branch:

```sh
kargo bench --baseline main --format json > bench.json
```

`kargo bench` does not fail when a benchmark gets slower; read the JSON or the report to decide. See [Continuous integration](../ci/).
