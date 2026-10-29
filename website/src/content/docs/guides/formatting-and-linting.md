---
title: Formatting and linting
description: Format Kotlin code with kargo fmt (ktlint) and find code smells with kargo lint (detekt), locally and in CI.
---

`kargo fmt` formats your Kotlin code with [ktlint](https://pinterest.github.io/ktlint/), and `kargo lint` analyzes it with [detekt](https://detekt.dev/). They are Kargo's `cargo fmt` and `cargo clippy`. Kargo downloads both tools from Maven Central and runs them on your JDK, so you don't install anything and every machine on the team runs the same version.

```console
$ kargo fmt
  Formatting my-app v0.1.0 (ktlint 1.8.0)
    Finished ktlint in 1.30s
$ kargo lint
     Linting my-app v0.1.0 (detekt 1.23.8)
    Finished detekt in 1.81s
```

Projects created by `kargo new` already pass both.

## Which files

Both commands look at every `.kt` and `.kts` file in `src/`, `src-<target>/`, `tests/` and `tests-<target>/`, for all targets whether or not they are enabled. `target/` and other directories are never touched.

## Formatting with `kargo fmt`

`kargo fmt` rewrites files in place. ktlint fixes what it can; if a problem has no automatic fix (some naming rules, for example), ktlint lists it and `kargo fmt` exits with code 101.

To check without changing anything, as in CI, pass `--check`:

```console
$ kargo fmt --check
    Checking formatting of my-app v0.1.0 (ktlint 1.8.0)
src/Helper.kt:3:4: Single space expected after the fun keyword (standard:fun-keyword-spacing)
src/Helper.kt:3:5: Unnecessary long whitespace (standard:no-multi-spaces)

Summary error count (descending) by rule:
  standard:fun-keyword-spacing: 1
  standard:no-multi-spaces: 1
error: formatting check failed; `kargo fmt` fixes what ktlint can correct automatically
```

`--check` exits with code 101 if ktlint reports anything. Problems are printed to stdout, relative to the project root.

### Configuring ktlint

ktlint follows the [ktlint_official](https://pinterest.github.io/ktlint/latest/rules/code-styles/) code style by default, which puts every parameter of a long signature on its own line. To use `intellij_idea` or `android_studio` instead, set `code-style` in `[fmt]`:

```toml title="kargo.toml"
[fmt]
code-style = "intellij_idea"
```

`kargo fmt` and the editor's *Format Document* both pass it to ktlint. Line length and individual rules go in an `.editorconfig` file at the project root, which ktlint finds on its own (it can set the style too, as `ktlint_code_style`):

```ini title=".editorconfig"
root = true

[*.{kt,kts}]
ktlint_code_style = intellij_idea
max_line_length = 120
ktlint_standard_no-wildcard-imports = disabled
```

ktlint cannot replace a wildcard import (`import three.*`) with the imports it stands for, so `kargo fmt` reports it as a problem it can't fix. Either replace it by hand (your editor's *organize imports* action does it) or allow wildcards with `ktlint_standard_no-wildcard-imports = disabled`, as above.

See [ktlint's configuration docs](https://pinterest.github.io/ktlint/latest/rules/configuration-ktlint/) for every property.

Skip files, such as generated code, and pin the ktlint version in `kargo.toml`:

```toml title="kargo.toml"
[fmt]
exclude = ["src/generated/**"]
ktlint = "1.8.0"
```

| Key | Default | Meaning |
|---|---|---|
| `code-style` | `.editorconfig`, else `ktlint_official` | ktlint code style: `ktlint_official`, `intellij_idea` or `android_studio` |
| `exclude` | `[]` | Globs, relative to the project root, of files ktlint skips |
| `ktlint` | `1.8.0` | Exact ktlint version |

### Formatting from your editor

[Kargo's Kotlin server](../kotlin-server/#formatting), the default behind `kargo lsp`, formats the open file with this same ktlint: *Format Document* in your editor gives what `kargo fmt` would write, with your `.editorconfig` and `[fmt]` settings, even for text you haven't saved. A file that `[fmt] exclude` leaves out is left alone. IntelliJ IDEA keeps its own formatter, and `--server kotlin-lsp` and `--server kls` format with their own.

Format on save is your editor's setting; turn it on and the editor asks the server on each save:

| Editor | Setting |
|---|---|
| Neovim | `vim.lsp.buf.format({ async = false })` from a `BufWritePre` autocommand |
| VS Code | `"[kotlin]": { "editor.formatOnSave": true }` |
| Zed | `"languages": { "Kotlin": { "format_on_save": "on" } }` |
| Helix | `auto-format = true` for `kotlin` in `languages.toml` |

ktlint is a JVM program, so a format takes about a second. It runs on its own thread: the editor stays responsive, and text you changed in the meantime is not overwritten. The first format downloads ktlint (and a JDK, if `[toolchain]` asks for one you haven't installed); `kargo fetch --tools` downloads ktlint ahead of time. Only whole files are formatted, and `.kts` scripts are not.

## Linting with `kargo lint`

`kargo lint` runs detekt's default rule set: complexity, naming, potential bugs, style and more. It exits with code 101 if detekt reports issues:

```console
$ kargo lint
     Linting my-app v0.1.0 (detekt 1.23.8)
/home/me/my-app/src/Helper.kt:3:5: helper is returning a constant. Prefer declaring a constant instead. [FunctionOnlyReturningConstant]

Analysis failed with 1 weighted issues.
error: detekt found issues; fix them, suppress them with `@Suppress`, or record them with `kargo lint --create-baseline`
```

Silence a single finding with `@Suppress("FunctionOnlyReturningConstant")` on the declaration.

### Configuring detekt

Put a `detekt.yml` at the project root. Kargo finds it automatically (no `[lint]` section needed) and passes it to detekt on top of detekt's [default configuration](https://github.com/detekt/detekt/blob/v1.23.8/detekt-core/src/main/resources/default-detekt-config.yml), so the file only needs the rules you change:

```yaml title="detekt.yml"
style:
  MagicNumber:
    active: false
  MaxLineLength:
    maxLineLength: 140
complexity:
  LongMethod:
    threshold: 80
```

Kargo turns off one rule for you: `InvalidPackageDeclaration`, because Kargo projects keep sources directly in `src/` instead of in directories that mirror the package. Turn it back on in your `detekt.yml` if you lay out your sources by package.

It also skips `UnusedPrivateProperty` on declarations annotated with `@JsModule` or `@JsNonModule`, whose constructor parameters describe a JavaScript signature. detekt cannot skip a rule by the `external` modifier alone, so for other `external` declarations use `@Suppress("UnusedPrivateProperty")`, or list their files in the rule's `excludes` in your `detekt.yml`. If you set `ignoreAnnotated` for that rule yourself, your list replaces Kargo's.

A Kotlin/JS or Wasm project with scene or layout code often triggers many `MagicNumber` findings. Turn that rule off, as in the example above, or raise `ignoreNumbers`.

To use other files or pin the version, add a `[lint]` section:

```toml title="kargo.toml"
[lint]
config = ["config/detekt.yml"]
baseline = "config/detekt-baseline.xml"
detekt = "1.23.8"
```

| Key | Default | Meaning |
|---|---|---|
| `config` | `["detekt.yml"]` if that file exists | detekt configuration files, relative to the project root. Later files override earlier ones |
| `baseline` | `detekt-baseline.xml` | Baseline file, relative to the project root. It is used only if it exists, unless you set this key; then it must exist |
| `detekt` | `1.23.8` | Exact detekt version |

### Adopting detekt in an existing project

A codebase that has never been linted usually has many findings. Record them in a baseline, so `kargo lint` only fails on new issues:

```console
$ kargo lint --create-baseline
     Linting my-app v0.1.0 (detekt 1.23.8)
     Created baseline detekt-baseline.xml
    Finished detekt in 1.78s
```

Commit `detekt-baseline.xml`. Later runs of `kargo lint` ignore the issues it lists. Fix them over time and run `--create-baseline` again to shrink the file.

The baseline records the issues left after your `detekt.yml` is applied, so change the configuration first and create the baseline afterwards: a rule you turn off later simply leaves stale entries, and a rule you turn on reports its findings as new issues. `--create-baseline` replaces the whole file.

## In CI

Add both commands to your workflow. They fail the job with exit code 101:

```yaml
      - run: kargo fmt --check
      - run: kargo lint
```

See [Continuous integration](../ci/).

## How it works

ktlint and detekt are downloaded on first use (the `ktlint-cli` and `detekt-cli` jars with the `all` classifier) and cached under `~/.kargo/cache`, like the Kotlin compiler. `kargo fetch --tools` downloads them ahead of time. After that they work with `--offline`. They run on the JDK from [`[toolchain]`](../toolchains/), or `JAVA_HOME`.

Run `kargo fmt -v` or `kargo lint -v` to see the exact command line. Kargo's own detekt settings are written to `target/lint/kargo-detekt.yml`.

:::note
`kargo fmt` and `kargo lint` are built-in commands, so a [task](../tasks-and-plugins/) named `fmt` or `lint` no longer runs as `kargo fmt` or `kargo lint`. Run it with `kargo task fmt` instead; Kargo [warns you](../../reference/warnings/#tasks-hidden-by-built-in-commands) when such a task exists.
:::
