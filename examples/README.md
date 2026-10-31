# Examples

Complete Kargo projects you can copy, each with its own `kargo.toml`, sources, tests and a README with the commands to run it. Copy one out of the repository and run it:

```sh
cp -r examples/ktor-server ~/ktor-server
cd ~/ktor-server
kargo run
```

| Example | Shows | Targets |
| --- | --- | --- |
| [`hello-jvm`](hello-jvm/) | The smallest project: an application and a test | JVM |
| [`ktor-server`](ktor-server/) | A Ktor server with kotlinx.serialization | JVM |
| [`spring-boot`](spring-boot/) | A Spring Boot web application | JVM |
| [`compose-desktop`](compose-desktop/) | A Compose Multiplatform desktop window | JVM |
| [`compose-multiplatform`](compose-multiplatform/) | One shared `App()` on desktop and in the browser | JVM, Wasm |
| [`compose-html`](compose-html/) | Compose HTML rendering DOM elements | JS |
| [`android`](android/) | An Android app, with no Gradle files | Android |
| [`kotlin-wasm`](kotlin-wasm/) | A browser app on Kotlin/Wasm | Wasm |
| [`kotlin-js`](kotlin-js/) | A Kotlin/JS application | JS |
| [`kotlin-native`](kotlin-native/) | A native executable | Native |
| [`multiplatform-library`](multiplatform-library/) | A serializable library for three targets | JVM, JS, Wasm |
| [`workspace`](workspace/) | Two packages joined by a path dependency | JVM |

## Keeping them current

Every example except `workspace` is exactly what `kargo new` writes for the command listed in `tests/examples.rs`, and none of them pins a version: the Kotlin version, the framework versions and the JVM target are Kargo's defaults. `cargo test --test examples` fails when a template changes and an example no longer matches it; refresh them with:

```sh
KARGO_UPDATE_EXAMPLES=1 cargo test --test examples
```

The `Examples` workflow (`.github/workflows/examples.yml`) builds and tests the examples with the real compilers.
