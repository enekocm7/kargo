# Kargo for Emacs

Runs [Kargo](https://github.com/enekocm7/kargo) from Emacs 29.1 or newer: `kargo lsp` through Eglot or lsp-mode,
`kargo build`, `check`, `run` and `test` through `compile` with errors you can jump to, tests with a results buffer,
build errors in Flymake, debugging with Dape, and a Transient menu.

| File | Does |
|---|---|
| `kargo.el` | The commands, runnables, `kargo-mode`, `project.el` and Projectile |
| `kargo-messages.el` | Reads the JSON messages of `--message-format json` |
| `kargo-test.el` | Runs a test, a class or a file, and lists the results |
| `kargo-eglot.el` | `kargo lsp` for Eglot |
| `lsp-kargo.el` | `kargo lsp` for lsp-mode |
| `kargo-flymake.el` | Build errors in Flymake |
| `kargo-dape.el` | Debugging through `kargo dap` with Dape |
| `kargo-transient.el` | A menu of all commands |

Installation and usage are in the [Emacs section of the editor guide](https://enekocm7.github.io/kargo/guides/editors/#emacs).

Run the tests with:

```sh
emacs -Q --batch -L . -l test/kargo-tests.el -f ert-run-tests-batch-and-exit
```
