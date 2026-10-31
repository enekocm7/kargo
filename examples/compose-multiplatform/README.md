# Compose Multiplatform

One shared `App()` in `src/`, run as a desktop window (`src-jvm/`) and in the browser on Wasm (`src-wasm/`).

```sh
kargo run -t jvm
kargo run -t wasm
kargo test
```

Guide: [Compose](https://enekocm7.github.io/kargo/guides/compose/)

This project is a copy of what `kargo new` generates (see the [examples index](../README.md)), so it tracks Kargo's current default versions.
