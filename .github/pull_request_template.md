## What and why

<!-- What does this change, and why? Link the issue it fixes, for example "Fixes #123". For a user-visible change, show the command and its output before and after. -->

## How it was tested

<!-- The tests you added or ran. A bug fix needs a test that fails without the fix. -->

## Checklist

- [ ] `cargo fmt --all` has been run, and `cargo clippy --workspace --all-targets` adds no warnings.
- [ ] `cargo test --workspace` passes. It needs no network and no JDK.
- [ ] New behavior has a test: in `tests/cli.rs` for the CLI (offline, with `tests/common`), next to the code for units, or in `tests/e2e.rs` behind `require_e2e!()` when it needs a real compiler.
- [ ] The documentation is updated in this pull request: the matching page in `website/src/content/docs/` (and `README.md` only for a headline change), plus `website/public/schemas/kargo.json` and its copy `crates/kargo-lsp/schema/kargo.json` for a `kargo.toml` key. If the change needs no documentation (a refactor, an internal test, performance work with no visible effect), say so here: <!-- reason -->
- [ ] `bun run build` passes in `website/` (only if `website/` changed).
- [ ] Changes to the output of `--message-format json`, `kargo metadata`, `kargo lsp` or `kargo dap` keep the editor plugins in `editors/` working.
- [ ] Changes to compiler drivers, frameworks or toolchains were run with the end-to-end tests (`KARGO_E2E=1 cargo test --test e2e <names>`), or I need a maintainer to run them.

## Notes for the reviewer

<!-- Anything that is not obvious from the diff: trade-offs, things left out on purpose, follow-up work. Delete this section if there is nothing to add. -->
