---
title: Project templates
description: Create projects from your own templates with kargo new --template, from a git repository, a directory or a short name, with variables, prompts and conditional files.
---

`kargo new` and `kargo init` have built-in templates (`--lib`, `-t`, `--with`). A team can have its own: a company starter, a Ktor service with CI and Docker files, a library skeleton with publishing set up. A template is a directory of plain files, and `kargo new --template` copies it into a new project, filling in the names and answers.

```sh
kargo new billing --template https://github.com/acme/service-template.git
kargo new billing --template ./templates/service
kargo new billing --template company/service      # a short name, see below
```

Kargo asks for the variables the template declares, writes the files, and prints the template's message. Nothing is written until every answer is valid and the generated `kargo.toml` parses.

## A first template

```text
service-template/
├── kargo-template.toml
├── kargo.toml
├── Dockerfile
└── src/
    └── {{package_path}}/
        └── Main.kt
```

```toml title="kargo-template.toml"
[template]
name = "company-service"
description = "An HTTP service with our CI"
message = "Next: cd {{name}} && kargo run"

[variables.docker]
prompt = "Add a Dockerfile?"
default = false

[variables.owner]
prompt = "Owning team:"

[[conditional]]
path = "Dockerfile"
when = "docker"

[[steps]]
builtin = "git-init"
```

```toml title="kargo.toml"
[package]
name = "{{name}}"
version = "0.1.0"
group = "{{group}}"

[frameworks]
ktor = { server = "netty" }
```

```kotlin title="src/{{package_path}}/Main.kt"
package {{package}}

fun main() = println("{{name | pascal}} is owned by {{owner}}")
```

```console
$ kargo new billing --template ./service-template
   Using template `company-service` from ./service-template
Add a Dockerfile? (y/n) [n] y
Owning team: payments
     Created `billing` from the template `company-service`
Next: cd billing && kargo run
```

`kargo-template.toml` itself is not copied. A `.git` directory in the template never is either.

## Placeholders

A `{{variable}}` in a file's contents **and** in the name of a file or directory is replaced by the variable's value. A directory name can hold several path segments: `{{package_path}}` becomes `com/example/billing`.

A filter changes the value: `{{name | pascal}}`.

| Filter | `my-app` becomes | |
|---|---|---|
| `lower`, `upper` | `my-app`, `MY-APP` | |
| `pascal` | `MyApp` | Words are split at anything that is not a letter or digit, and before an uppercase letter that follows a lowercase one |
| `camel` | `myApp` | |
| `snake`, `kebab` | `my_app`, `my-app` | |
| `package` | `myapp` | The segments of a dotted name made into Kotlin package segments, as `kargo new` does |
| `path` | | Dots become slashes |

Text that is not a placeholder is left alone, so other template languages and CI files work as they are: `${{ github.workflow }}`, `{{ .Name }}` and an unterminated `{{` stay as written. A placeholder with a name the template does not declare is an error that names the file. To write a literal `{{name}}`, put a backslash before it: `\{{name}}`. For a whole directory of such files, such as GitHub Actions workflows, list it under `raw` (below).

Files that are not UTF-8 text (images, jars) are copied byte for byte.

## Variables

These variables always exist:

| Variable | Value |
|---|---|
| `name` | The package name: `--name`, or the name of the directory |
| `group` | `--group`, else the template's default, else `com.example` |
| `package` | The Kotlin package, `<group>.<name>` lowercased with only letters and digits, as in [`kargo new`](../../reference/commands/#kargo-new) (`my-app` in `com.example` is `com.example.myapp`) |
| `package_path` | `package` with slashes: `com/example/myapp` |
| `kotlin` | `--kotlin`, else the template's default, else Kargo's default Kotlin version |

The package name is checked as `kargo new` checks it, and the group and package must be valid names (no Kotlin or Java keywords).

Declare your own under `[variables.<key>]`. A key is lowercase letters, digits and `_`, starting with a letter.

| Key | Meaning |
|---|---|
| `prompt` | The question. Defaults to the key |
| `type` | `string`, `bool` or `choice`. Inferred from `default` (a boolean is `bool`) and `choices` |
| `default` | The default answer. A string default may use placeholders of the variables before it: `default = "{{name}}-service"` |
| `choices` | The allowed values of a `choice` |

A variable with no `default` has to be answered. `group`, `kotlin` and `package` may be declared too, to ask for them or to give a different default:

```toml
[variables.group]
prompt = "Maven group"
default = "com.acme"
```

`name` and `package_path` cannot be declared.

### Answers

For each variable, in the order of the file, Kargo takes:

1. the value of `--define key=value` (`-D`), checked against the type: `true`, `false`, `yes`, `no`, `y`, `n`, `1`, `0`, `on` and `off` for a `bool`, one of `choices` for a `choice`;
2. otherwise, on a terminal, the answer to the prompt (Enter takes the default; a `choice` is a list you move through with the arrow keys, with the default first);
3. otherwise the default, or an error that says to pass `--define key=<value>`.

`--defaults` (or `--yes`) skips the prompts and takes the defaults. Without a terminal, in CI or a script, nothing is asked either, so the same command works there:

```sh
kargo new billing --template company/service --defaults -D owner=payments -D docker=true --group com.acme
```

`--define` accepts the keys of the template and `group`, `kotlin` and `package`, and Kargo suggests the closest name for a typo. `--name` sets the name.

## Files

By default every file of the template is copied. `[files]` and `[[conditional]]` choose.

```toml
[files]
include = ["src/", "kargo.toml", "*.md"]   # only these (default: everything)
exclude = ["docs/internal/", "*.bak"]        # never these
raw = [".github/**"]                         # copy these without replacing placeholders

[[conditional]]
path = "docker/"                             # a file, a directory or a pattern
when = "docker"                              # only when this holds
```

Patterns follow `.gitignore` rules, relative to the template (`docker/` is a directory, `*.md` any Markdown file, `!` negates). A `when` condition is one of:

| Condition | True when |
|---|---|
| `docker` | the variable is `true`, or a string that is not empty or `false` |
| `!docker` | it is not |
| `ci == github`, `ci == "github"` | the variable has that value |
| `ci != none` | it has not |

A condition may only name a declared variable or one of the built-in ones. Empty directories are not created, and a file or directory whose name comes out empty is left out: `{{docker_dir}}/compose.yml` is skipped when `docker_dir` is empty. Two files that would be written to the same path are an error. A file name never leaves the project: a name that renders to `..`, a path starting with a slash, or one with `\` or `:` is refused. Symbolic links in a template are refused too, since templates are plain files.

On Linux and macOS a file keeps its executable bit.

## Steps after the files

```toml
[[steps]]
builtin = "git-init"          # git init, unless --no-vcs, a git repository is around, or the project joins a workspace

[[steps]]
builtin = "fetch"             # kargo fetch in the new project

[[steps]]
run = ["npm", "install"]      # a command and its arguments, no shell
when = "frontend"             # steps take a `when` as files do
```

`git-init` and `fetch` are Kargo's own and always allowed. A `run` step starts any program, so it needs your explicit consent: without `--allow-scripts` Kargo lists the commands and stops before writing anything.

```console
$ kargo new billing --template company/service --defaults
error: the template runs commands after creating the project:
  npm install
read them, and pass --allow-scripts to run them
```

Read the commands, then add `--allow-scripts`. The arguments of a `run` step may use placeholders. Steps run in the new project in order, and the first one that fails stops the rest; the project is kept.

## Where templates come from

`--template` takes one of:

| Source | Example |
|---|---|
| A git URL | `https://github.com/acme/service-template.git`, `git@github.com:acme/service-template.git` |
| A git URL with a revision | `https://github.com/acme/service-template.git#v2` (a branch, tag or commit) |
| A directory | `./templates/service`, `~/templates/service`, `/opt/templates/service` |
| A short name | `company/service` |

`--rev <rev>` picks the branch, tag or commit of a git source and replaces a `#rev` in the URL. The default is the repository's default branch.

Kargo clones a repository into `templates/git/` of the Kargo home the first time and fetches before each use, so a template is up to date; with `--offline` it uses the copy it already has, and fails if there is none. Cloning needs `git` on the `PATH`. A private repository works as `git clone` does for you (SSH keys, credential helpers).

A plain word such as `service` is looked up as a short name first, and then as a directory of that name in the current directory. A directory that is not a template (no `kargo-template.toml`) is an error.

### Short names

Map names to sources in `templates.toml` in the Kargo home (`~/.kargo/templates.toml`, or `$KARGO_HOME`):

```toml title="~/.kargo/templates.toml"
[templates]
"company/service" = "https://github.com/acme/service-template.git#v2"
lib = "~/templates/lib"
tools = { git = "https://github.com/acme/templates.git", rev = "main", subdir = "tools", description = "CLI tools" }
local = { path = "../shared/templates/local" }
```

An entry is a string (a git URL or a path, as for `--template`) or a table with `git` (and `rev`, `subdir`) or `path`, and an optional `description`. `subdir` is the template's directory inside a repository that holds several. Relative paths are relative to the Kargo home. `kargo template list` shows the names:

```console
$ kargo template list
company/service  https://github.com/acme/service-template.git#v2
lib              /home/me/templates/lib
tools            https://github.com/acme/templates.git#main (tools)  CLI tools
```

## Workspaces and `kargo init`

Inside a [workspace](../workspaces/), a project made from a template becomes a member, as with the built-in templates, and no new git repository is created.

`kargo init --template` writes into an existing directory and keeps the files that are already there, as `kargo init` does. A template that writes a `kargo.toml` is refused in a directory that has one.

## Checks

Before the first file is written, `kargo new --template` checks that:

- the template's `kargo-template.toml` has only known keys, its variables, conditions and steps make sense and every `when` names a declared variable;
- the name, group and package are valid;
- every answer fits its type;
- each placeholder in a file or file name has a value and each file lands inside the project;
- the `kargo.toml` it generates parses (a workspace member can inherit from its workspace);
- commands to run are allowed.

An error leaves no directory behind.

## Publishing a template

A template is a git repository (or a folder in one) with a `kargo-template.toml` at its root. Tag a release and users pin it with `#v1` or `--rev v1`. Give it a short name in the team's `templates.toml`, which you can distribute with your dotfiles or the build image. Try changes without committing them by pointing `--template` at the working directory.

:::note
`kargo new --lib`, `-t` and `--with` still use Kargo's built-in templates. They cannot be combined with `--template`.
:::

See [`kargo new`](../../reference/commands/#kargo-new) and [`kargo template`](../../reference/commands/#kargo-template) for the options.
