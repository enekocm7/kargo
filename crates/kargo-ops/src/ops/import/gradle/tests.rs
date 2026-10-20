use super::*;
use kargo_manifest::Manifest;

fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, text) in files {
        let p = dir.path().join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    dir
}

fn manifest_of(package: &Package, workspace_kotlin: Option<&str>) -> String {
    let text = package.manifest(workspace_kotlin);
    Manifest::parse(&text).unwrap_or_else(|e| panic!("{e:#}\n{text}"));
    text
}

fn moves(package: &Package) -> Vec<String> {
    package
        .moves
        .iter()
        .map(|m| format!("{} -> {}", m.from.to_string_lossy().replace('\\', "/"), m.to.to_string_lossy().replace('\\', "/")))
        .collect()
}

const KTOR_BUILD: &str = r#"
val kotlin_version: String by project
val logback_version: String by project
val ktor_version: String by project

plugins {
    kotlin("jvm") version "2.0.20"
    id("io.ktor.plugin") version "3.0.0"
    id("org.jetbrains.kotlin.plugin.serialization") version "2.0.20"
}

group = "com.example"
version = "0.0.1"

application {
    mainClass.set("io.ktor.server.netty.EngineMain")

    val isDevelopment: Boolean = project.ext.has("development")
    applicationDefaultJvmArgs = listOf("-Dio.ktor.development=$isDevelopment")
}

repositories {
    mavenCentral()
    maven { url = uri("https://maven.pkg.jetbrains.space/public/p/ktor/eap") }
}

dependencies {
    implementation("io.ktor:ktor-server-core-jvm")
    implementation("io.ktor:ktor-serialization-kotlinx-json-jvm")
    implementation("io.ktor:ktor-server-content-negotiation-jvm")
    implementation("io.ktor:ktor-server-netty-jvm")
    implementation("ch.qos.logback:logback-classic:$logback_version")
    implementation("io.ktor:ktor-server-config-yaml")
    testImplementation("io.ktor:ktor-server-test-host-jvm")
    testImplementation("org.jetbrains.kotlin:kotlin-test-junit:$kotlin_version")
}
"#;

#[test]
fn a_ktor_project_becomes_the_ktor_framework() {
    let dir = project(&[
        ("settings.gradle.kts", "rootProject.name = \"ktor-sample\"\n"),
        ("build.gradle.kts", KTOR_BUILD),
        ("gradle.properties", "kotlin_version=2.0.20\nktor_version=3.0.0\nlogback_version=1.4.14\nkotlin.code.style=official\n"),
        ("src/main/kotlin/Application.kt", ""),
        ("src/main/resources/application.yaml", ""),
        ("src/test/kotlin/ApplicationTest.kt", ""),
    ]);
    let imported = read(dir.path()).unwrap();
    assert!(!imported.workspace);
    let package = &imported.packages[0];
    assert_eq!((package.name.as_str(), package.version.as_str(), package.group.as_deref()), ("ktor-sample", "0.0.1", Some("com.example")));
    assert_eq!(package.kotlin.version.as_deref(), Some("2.0.20"));
    assert_eq!(package.main_class.as_deref(), Some("io.ktor.server.netty.EngineMain"));
    let ktor = package.frameworks.ktor.as_ref().expect("ktor");
    assert_eq!((ktor.version.as_deref(), ktor.server.as_deref(), ktor.json), (Some("3.0.0"), Some("netty"), true));
    assert_eq!(ktor.server_plugins, ["content-negotiation", "config-yaml"]);
    assert!(package.frameworks.serialization.is_some());
    let deps: Vec<_> = package.dependencies.iter().map(|d| (d.coords(), d.version.clone())).collect();
    assert_eq!(deps, [("ch.qos.logback:logback-classic".to_owned(), Some("1.4.14".to_owned()))]);
    assert!(package.dev_dependencies.is_empty(), "{:?}", package.dev_dependencies);
    assert_eq!(package.repositories.len(), 1);
    assert_eq!(package.repositories[0].url, "https://maven.pkg.jetbrains.space/public/p/ktor/eap");
    let text = manifest_of(package, None);
    assert!(text.contains("main-class = \"io.ktor.server.netty.EngineMain\""), "{text}");
    assert!(
        text.contains(
            "ktor = { version = \"3.0.0\", server = \"netty\", server-plugins = [\"content-negotiation\", \"config-yaml\"], json = true }"
        ),
        "{text}"
    );
    assert_eq!(moves(package)[..3], ["src/main/kotlin -> src", "src/main/java -> src", "src/test/kotlin -> tests"]);
}

#[test]
fn a_spring_boot_project_becomes_the_spring_boot_framework() {
    let build = r#"
plugins {
    kotlin("jvm") version "1.9.24"
    kotlin("plugin.spring") version "1.9.24"
    kotlin("plugin.jpa") version "1.9.24"
    id("org.springframework.boot") version "3.3.1"
    id("io.spring.dependency-management") version "1.1.5"
}

group = "com.example"
version = "0.0.1-SNAPSHOT"

java {
    toolchain {
        languageVersion = JavaLanguageVersion.of(21)
    }
}

repositories {
    mavenCentral()
}

dependencies {
    implementation("org.springframework.boot:spring-boot-starter-web")
    implementation("org.springframework.boot:spring-boot-starter-data-jpa")
    implementation("com.fasterxml.jackson.module:jackson-module-kotlin")
    implementation("org.jetbrains.kotlin:kotlin-reflect")
    runtimeOnly("org.postgresql:postgresql")
    testImplementation("org.springframework.boot:spring-boot-starter-test")
    testImplementation("org.jetbrains.kotlin:kotlin-test-junit5")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

kotlin {
    compilerOptions {
        freeCompilerArgs.addAll("-Xjsr305=strict")
    }
}

tasks.withType<Test> {
    useJUnitPlatform()
}
"#;
    let dir = project(&[("settings.gradle.kts", "rootProject.name = \"demo\"\n"), ("build.gradle.kts", build)]);
    let imported = read(dir.path()).unwrap();
    let package = &imported.packages[0];
    let spring = package.frameworks.spring_boot.as_ref().expect("spring boot");
    assert_eq!(spring.version.as_deref(), Some("3.3.1"));
    assert_eq!(spring.starters, ["web", "data-jpa"]);
    assert_eq!(package.jvm_target.as_deref(), Some("21"));
    assert_eq!(package.jdk.as_deref(), Some("21"));
    assert!(package.kotlin.compiler_args.is_empty());
    assert!(package.plugins.is_empty(), "the framework sets up all-open and no-arg: {:?}", package.plugins);
    let text = manifest_of(package, None);
    assert!(text.contains("[toolchain]\njdk = \"21\""), "{text}");
    assert!(text.contains("\"org.postgresql:postgresql\" = { scope = \"runtime\" }"), "{text}");
    assert!(text.contains("\"org.junit.platform:junit-platform-launcher\" = { scope = \"runtime\" }"), "{text}");
    assert!(!text.contains("jackson") && !text.contains("kotlin-reflect") && !text.contains("starter-test"), "{text}");
}

#[test]
fn a_multiplatform_project_gets_its_targets_and_source_folders() {
    let build = r#"
plugins {
    kotlin("multiplatform") version "2.0.20"
}

group = "org.example"
version = "1.0"

kotlin {
    jvm {
        compilerOptions { jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17) }
    }
    wasmJs {
        browser()
        binaries.executable()
    }
    linuxX64 {
        binaries {
            executable {
                entryPoint = "org.example.main"
            }
        }
    }
    macosArm64()
    iosArm64()

    sourceSets {
        commonMain.dependencies {
            implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.8.1")
            implementation(libs.okio)
        }
        commonTest.dependencies {
            implementation(kotlin("test"))
        }
        val jvmMain by getting {
            dependencies {
                implementation("com.squareup.okhttp3:okhttp:4.12.0")
            }
        }
        val wasmJsMain by getting {
            dependencies {
                implementation(npm("left-pad", "1.3.0"))
            }
        }
    }
}
"#;
    let dir = project(&[
        ("settings.gradle.kts", "rootProject.name = \"shared\"\n"),
        ("build.gradle.kts", build),
        ("gradle/libs.versions.toml", "[libraries]\nokio = \"com.squareup.okio:okio:3.9.0\"\n"),
        ("src/commonMain/kotlin/A.kt", ""),
        ("src/commonMain/resources/r.txt", ""),
        ("src/commonTest/kotlin/ATest.kt", ""),
        ("src/jvmMain/kotlin/B.kt", ""),
        ("src/jvmTest/kotlin/BTest.kt", ""),
        ("src/wasmJsMain/kotlin/C.kt", ""),
        ("src/linuxX64Main/kotlin/D.kt", ""),
        ("src/iosMain/kotlin/E.kt", ""),
    ]);
    let imported = read(dir.path()).unwrap();
    let package = &imported.packages[0];
    let kinds: Vec<_> = package.targets.iter().map(|t| t.kind).collect();
    assert_eq!(kinds, [TargetKind::Jvm, TargetKind::Wasm, TargetKind::Native]);
    let text = manifest_of(package, None);
    assert!(text.contains("[target.jvm]\njvm-target = \"17\""), "{text}");
    assert!(text.contains("[target.wasm]\nenvironment = \"browser\""), "{text}");
    assert!(text.contains("[target.native]\nplatforms = [\"linuxX64\", \"macosArm64\", \"iosArm64\"]\nkind = \"executable\"\nentry-point = \"org.example.main\""), "{text}");
    assert!(text.contains("coroutines = \"1.8.1\""), "{text}");
    assert!(text.contains("\"com.squareup.okio:okio\" = \"3.9.0\""), "{text}");
    assert!(text.contains("[target.jvm.dependencies]\n\"com.squareup.okhttp3:okhttp\" = \"4.12.0\""), "{text}");
    assert!(text.contains("[target.wasm.npm-dependencies]\nleft-pad = \"1.3.0\""), "{text}");
    let moves = moves(package);
    for expected in [
        "src/commonMain/kotlin -> src",
        "src/commonTest/kotlin -> tests",
        "src/jvmMain/kotlin -> src-jvm",
        "src/jvmTest/kotlin -> tests-jvm",
        "src/wasmJsMain/kotlin -> src-wasm",
        "src/linuxX64Main/kotlin -> src-native",
        "src/iosMain/kotlin -> src-native",
        "src/commonMain/resources -> resources",
    ] {
        assert!(moves.iter().any(|m| m == expected), "{expected} in {moves:?}");
    }
    assert!(imported.notes.iter().any(|n| !n.translated && n.message.contains("src-native")), "{:?}", imported.notes);
}

#[test]
fn several_projects_become_a_workspace() {
    let dir = project(&[
        ("settings.gradle.kts", "rootProject.name = \"shop\"\ninclude(\":core\", \":app\")\n"),
        (
            "build.gradle.kts",
            "plugins {\n    kotlin(\"jvm\") version \"2.0.20\" apply false\n}\nallprojects {\n    group = \"com.acme\"\n    version = \"1.0\"\n    repositories { mavenCentral() }\n}\n",
        ),
        (
            "gradle/libs.versions.toml",
            "[versions]\nslf4j = \"2.0.13\"\n[libraries]\nslf4j-api = { module = \"org.slf4j:slf4j-api\", version.ref = \"slf4j\" }\n",
        ),
        ("core/build.gradle.kts", "plugins { kotlin(\"jvm\") }\ndependencies {\n    api(libs.slf4j.api)\n}\n"),
        (
            "app/build.gradle.kts",
            "plugins {\n    kotlin(\"jvm\")\n    application\n}\ndependencies {\n    implementation(project(\":core\"))\n    testImplementation(kotlin(\"test\"))\n}\napplication { mainClass.set(\"app.MainKt\") }\n",
        ),
        ("core/src/main/kotlin/Core.kt", ""),
        ("app/src/main/kotlin/Main.kt", ""),
    ]);
    let imported = read(dir.path()).unwrap();
    assert!(imported.workspace);
    let names: Vec<_> = imported.packages.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["core", "app"]);
    let core = &imported.packages[0];
    assert_eq!(core.group.as_deref(), Some("com.acme"));
    assert_eq!(core.kotlin.version.as_deref(), Some("2.0.20"), "declared by the root script");
    assert_eq!(core.dependencies[0].coords(), "org.slf4j:slf4j-api");
    assert!(core.dependencies[0].exported, "api(...)");
    let app = &imported.packages[1];
    assert_eq!(app.main_class.as_deref(), Some("app.MainKt"));
    let dep = &app.dependencies[0];
    assert_eq!(
        dep.path.as_ref().map(|p| (p.name.as_str(), p.dir.to_string_lossy().replace('\\', "/"))),
        Some(("core", "../core".to_owned()))
    );
    manifest_of(app, Some("2.0.20"));
    assert_eq!(moves(app)[0], "app/src/main/kotlin -> app/src");
}

#[test]
fn what_cannot_be_translated_is_listed() {
    let build = r#"
plugins {
    kotlin("jvm") version "2.0.20"
    kotlin("kapt") version "2.0.20"
    id("com.github.johnrengelman.shadow") version "8.1.1"
    id("com.example.custom-plugin")
    `maven-publish`
}
dependencies {
    implementation(files("libs/local.jar"))
    kapt("com.google.dagger:dagger-compiler:2.51")
    implementation(libs.missing.thing)
}
tasks.register("generate") { }
"#;
    let dir = project(&[
        (
            "settings.gradle.kts",
            "rootProject.name = \"sample\"
",
        ),
        ("build.gradle.kts", build),
    ]);
    let imported = read(dir.path()).unwrap();
    let manual: Vec<_> = imported.notes.iter().filter(|n| !n.translated).map(|n| n.message.as_str()).collect();
    let mentions = |what: &str| manual.iter().any(|m| m.contains(what));
    assert!(mentions("kapt"), "{manual:?}");
    assert!(mentions("shadow"), "{manual:?}");
    assert!(mentions("com.example.custom-plugin"), "{manual:?}");
    assert!(mentions("maven-publish"), "{manual:?}");
    assert!(mentions("files(...)"), "{manual:?}");
    assert!(mentions("libs.missing.thing"), "{manual:?}");
    assert!(mentions("generate"), "{manual:?}");
    assert!(imported.notes.iter().filter(|n| !n.translated).all(|n| n.docs.is_some()));
}

#[test]
fn groovy_scripts_are_read_too() {
    let build = r#"
plugins {
    id 'org.jetbrains.kotlin.jvm' version '1.9.22'
    id 'application'
}

group 'com.example'
version '1.2.3'

repositories {
    mavenCentral()
    maven { url 'https://jitpack.io' }
}

dependencies {
    implementation 'com.github.User:Repo:1.0'
    implementation group: 'org.slf4j', name: 'slf4j-api', version: '2.0.13'
    testImplementation 'org.jetbrains.kotlin:kotlin-test'
}

application {
    mainClassName = 'com.example.MainKt'
}
"#;
    let dir = project(&[("settings.gradle", "rootProject.name = 'groovy-app'\n"), ("build.gradle", build)]);
    let imported = read(dir.path()).unwrap();
    let package = &imported.packages[0];
    assert_eq!((package.name.as_str(), package.version.as_str(), package.group.as_deref()), ("groovy-app", "1.2.3", Some("com.example")));
    assert_eq!(package.kotlin.version.as_deref(), Some("1.9.22"));
    assert_eq!(package.main_class.as_deref(), Some("com.example.MainKt"));
    assert_eq!(package.repositories[0].url, "https://jitpack.io");
    let deps: Vec<_> = package.dependencies.iter().map(Dependency::coords).collect();
    assert_eq!(deps, ["com.github.User:Repo", "org.slf4j:slf4j-api"]);
    manifest_of(package, None);
}

#[test]
fn an_android_application_gets_its_target_and_layout() {
    let build = r#"
plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "com.example.app"
    compileSdk = 34

    defaultConfig {
        applicationId = "com.example.app"
        minSdk = 26
        targetSdk = 34
        versionCode = 3
        versionName = "1.2"
    }

    buildTypes {
        release {
            isMinifyEnabled = true
        }
    }
}

dependencies {
    implementation("androidx.core:core-ktx:1.13.1")
    androidTestImplementation("androidx.test:runner:1.5.2")
}
"#;
    let dir = project(&[
        ("settings.gradle.kts", "rootProject.name = \"droid\"\n"),
        ("build.gradle.kts", build),
        ("src/main/AndroidManifest.xml", "<manifest/>"),
        ("src/main/res/values/strings.xml", ""),
        ("src/main/kotlin/MainActivity.kt", ""),
    ]);
    let imported = read(dir.path()).unwrap();
    let package = &imported.packages[0];
    let text = manifest_of(package, None);
    assert!(
        text.contains("[target.android]\nnamespace = \"com.example.app\"\nkind = \"application\"\nmin-sdk = 26\ntarget-sdk = 34\ncompile-sdk = 34\nversion-code = 3\nversion-name = \"1.2\"\nminify = true"),
        "{text}"
    );
    let moves = moves(package);
    assert!(moves.contains(&"src/main/AndroidManifest.xml -> android/AndroidManifest.xml".to_owned()), "{moves:?}");
    assert!(moves.contains(&"src/main/res -> android/res".to_owned()), "{moves:?}");
    assert!(imported.notes.iter().any(|n| !n.translated && n.message.contains("androidTestImplementation")), "{:?}", imported.notes);
}

#[test]
fn a_directory_without_a_kotlin_plugin_is_not_a_project() {
    let dir = project(&[("build.gradle.kts", "plugins { java }\n")]);
    let err = read(dir.path()).unwrap_err();
    assert!(err.to_string().contains("no Gradle project applies a Kotlin plugin"), "{err}");
}
