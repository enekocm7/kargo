---
title: Automated dependency updates
description: Keep kargo.toml and kargo.lock current with a scheduled GitHub Actions workflow or Renovate.
---

`kargo outdated` and `kargo upgrade` ([Keeping dependencies up to date](../dependencies/#keeping-dependencies-up-to-date)) update a project when you run them. To get a pull request whenever a dependency has a new version, run them on a schedule, or teach Renovate to read `kargo.toml`.

## A scheduled workflow

`kargo init-ci --updates` writes `.github/workflows/kargo-updates.yml`:

```sh
kargo init-ci --updates
```

Every Monday at 06:00 UTC, and when you start it by hand from the Actions tab, the workflow:

1. runs `kargo upgrade -p <name> --format json` for each package of the project, which raises the versions in `kargo.toml` to the newest compatible ones;
2. runs `kargo update`, so `kargo.lock` follows;
3. runs `kargo outdated -p <name> --format json`, which lists the newer versions `kargo upgrade` left alone;
4. opens (or updates) the pull request `kargo/dependency-updates` with [peter-evans/create-pull-request](https://github.com/peter-evans/create-pull-request). Its description is a table of what moved, and a second table of the newer versions `kargo upgrade` left alone: the incompatible ones, which `kargo upgrade --incompatible <name>` raises, and the dependencies inherited from `[workspace.dependencies]`, which are raised in the workspace manifest.

When nothing changed, no pull request is opened. The workflow installs Kargo with the [`setup-kargo` action](../ci/#the-setup-kargo-action) and, like the CI workflow, pins the version of the Kargo that wrote it. Edit the `cron` line to change the schedule.

:::note
For the pull request to be opened, enable **Allow GitHub Actions to create and approve pull requests** in the repository's *Settings → Actions → General*. A pull request opened with the default `GITHUB_TOKEN` does not start your CI workflow; pass a personal access token or a GitHub App token as `token:` to `create-pull-request` if CI should run on it.
:::

The command refuses to replace an existing workflow unless you pass `--force`. `--updates` writes this workflow instead of the CI workflow, so run `kargo init-ci` separately for that one.

## Machine-readable output

`kargo outdated` and `kargo upgrade` take `--format json` and print one JSON object on stdout; progress and notes stay on stderr.

```console
$ kargo outdated --format json
{
  "outdated": [
    {
      "compatible": "1.14.11",
      "current": "1.13.0",
      "ecosystem": "maven",
      "latest": "1.14.11",
      "module": "io.mockk:mockk",
      "name": "io.mockk:mockk",
      "section": "dev-dependencies"
    }
  ],
  "package": {
    "name": "app",
    "version": "1.0.0"
  }
}
```

```console
$ kargo upgrade --format json
{
  "dry-run": false,
  "package": {
    "name": "app",
    "version": "1.0.0"
  },
  "upgraded": [
    {
      "ecosystem": "maven",
      "from": "1.13.0",
      "module": "io.mockk:mockk",
      "name": "io.mockk:mockk",
      "section": "dev-dependencies",
      "to": "1.14.11"
    }
  ]
}
```

| Field | Meaning |
|---|---|
| `name` | The key in `kargo.toml`; for an npm package its name |
| `module` | `group:artifact`; for an npm package its name |
| `ecosystem` | `maven` or `npm` |
| `section` | The table the dependency is declared in, such as `dependencies` or `target.wasm.npm-dependencies` |
| `current`, `compatible`, `latest` | In `outdated`: the declared version and the newer ones, `null` when there is none |
| `from`, `to` | In `upgraded`: the version before and after |
| `dry-run` | `true` when `--dry-run` left `kargo.toml` unchanged |

With `jq`, `kargo upgrade --format json | jq -r '.upgraded[] | "\(.name) \(.from) -> \(.to)"'` gives one line per change.

## One group or one dependency per run

Name the dependencies to upgrade, or a Maven group with `--group` (`-g`, repeatable). Everything else stays as it is, which gives small pull requests that are easy to review and revert:

```sh
kargo upgrade io.mockk:mockk              # one dependency
kargo upgrade --group org.jetbrains.kotlinx   # every dependency of a group
kargo upgrade -g io.ktor -g io.mockk --incompatible
```

`--group` fails when no declared dependency belongs to the group. npm packages have no group; name them instead.

## Renovate

[Renovate](https://docs.renovatebot.com/) has no manager for `kargo.toml`, but its [regex custom manager](https://docs.renovatebot.com/modules/manager/regex/) can read the dependency tables and look the versions up on Maven and npm. Put this in `renovate.json` at the root of the repository:

```json title="renovate.json"
{
  "$schema": "https://docs.renovatebot.com/renovate-schema.json",
  "customManagers": [
    {
      "customType": "regex",
      "managerFilePatterns": ["/(^|/)kargo[.]toml$/"],
      "datasourceTemplate": "maven",
      "matchStrings": [
        "\"(?<depName>[\\w.\\-]+:[\\w.\\-]+)\"\\s*=\\s*\"(?<currentValue>[^\"]+)\"",
        "\"(?<depName>[\\w.\\-]+:[\\w.\\-]+)\"\\s*=\\s*\\{[^}]*?\\bversion\\s*=\\s*\"(?<currentValue>[^\"]+)\"",
        "\\bcoords\\s*=\\s*\"(?<depName>[\\w.\\-]+:[\\w.\\-]+):(?<currentValue>[^\":]+)\"",
        "\\bcoords\\s*=\\s*\"(?<depName>[\\w.\\-]+:[\\w.\\-]+)\"\\s*,\\s*version\\s*=\\s*\"(?<currentValue>[^\"]+)\""
      ]
    },
    {
      "customType": "regex",
      "managerFilePatterns": ["/(^|/)kargo[.]toml$/"],
      "datasourceTemplate": "maven",
      "depNameTemplate": "{{{group}}}:{{{artifact}}}",
      "matchStrings": [
        "\\bgroup\\s*=\\s*\"(?<group>[^\"]+)\"\\s*,\\s*artifact\\s*=\\s*\"(?<artifact>[^\"]+)\"\\s*,\\s*version\\s*=\\s*\"(?<currentValue>[^\"]+)\""
      ]
    },
    {
      "customType": "regex",
      "managerFilePatterns": ["/(^|/)kargo[.]toml$/"],
      "datasourceTemplate": "npm",
      "matchStringsStrategy": "recursive",
      "matchStrings": [
        "\\[target\\.(?:wasm|js)\\.(?:dev-)?npm-dependencies\\][^\\[]*",
        "\"?(?<depName>@?[\\w.\\-/]+)\"?\\s*=\\s*\"(?<currentValue>[^\"]+)\""
      ]
    }
  ],
  "postUpgradeTasks": {
    "commands": ["kargo update"],
    "fileFilters": ["**/kargo.lock"],
    "executionMode": "branch"
  }
}
```

The first manager reads the Maven forms of the [`[dependencies]`](../../reference/manifest/#dependencies-and-dev-dependencies) tables — and of `[dev-dependencies]`, the per-target tables, `[workspace.dependencies]` and `[profile.<name>.dependencies]`, since it does not care which table an entry is in:

```toml
"io.mockk:mockk" = "1.14.11"
"io.ktor:ktor-client-core" = { version = "3.2.3", scope = "runtime" }
guava = { coords = "com.google.guava:guava:33.0.0-jre" }
widget = { coords = "com.acme:widget", version = "1.0.0" }
```

The second reads `{ group = "g", artifact = "a", version = "1" }` with the keys in that order, and the third the entries of the npm tables (`[target.wasm.npm-dependencies]`, `[target.js.npm-dependencies]` and their `dev-` versions). Entries without a `version` (managed by a BOM) and `path` dependencies match nothing.

`postUpgradeTasks` runs `kargo update` in the branch of each update, so `kargo.lock` follows `kargo.toml` and `kargo build --locked` passes on the pull request.

Things to know:

- **`postUpgradeTasks` needs a self-hosted Renovate** with Kargo on its `PATH` and the command allowed in the bot's configuration: `"allowedCommands": ["^kargo update$"]`. The hosted Mend Renovate app does not run commands. Without `postUpgradeTasks`, Renovate still edits `kargo.toml`, and the next `kargo build` without `--locked` rewrites the lockfile.
- A dependency from a repository other than Maven Central needs `registryUrls` on the Maven managers, with the URL from [`[repositories]`](../../reference/manifest/#repositories).
- Version ranges (`[0.6,0.7)`) are matched like any other version string; add a `packageRules` entry with `"enabled": false` for the ones you want to leave alone.
- The regexes look at each line, not at the table, so a `"a:b" = "c"` key anywhere else in `kargo.toml` (for example in `[env]`) is read as a dependency. Keep such keys out of the file or exclude them with `ignoreDeps`.
- The scheduled workflow above and Renovate do the same job; use one.

## Dependabot

Dependabot only supports the ecosystems GitHub ships and has no plugin system, so it cannot read `kargo.toml` yet. Use the scheduled workflow or Renovate.
