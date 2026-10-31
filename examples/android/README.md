# Android app

An Android application. Kargo generates the Gradle project under `target/android`, so there are no Gradle files here. It needs the Android SDK; `kargo toolchain install android-sdk` downloads one.

```sh
kargo build
kargo run -t android --device emulator-5554
kargo test
```

Guide: [Android](https://enekocm7.github.io/kargo/guides/android/)

This project is a copy of what `kargo new` generates (see the [examples index](../README.md)), so it tracks Kargo's current default versions.
