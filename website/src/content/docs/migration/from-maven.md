---
title: Migrating from Maven
description: Translate a pom.xml into kargo.toml.
---

Kargo reads the same Maven repositories and understands the same POMs and BOMs, so moving from Maven is mostly copying coordinates.

:::tip
`kargo import` does this translation for you: it writes the `kargo.toml` from your `pom.xml` (parents, BOMs and modules included), moves the sources and builds the result. See [Importing a Maven or Gradle project](../../guides/importing/). The rest of this page explains what it does and where Maven and Kargo differ.
:::

## Example

```xml title="pom.xml"
<project>
  <groupId>com.example</groupId>
  <artifactId>billing</artifactId>
  <version>2.0.0</version>

  <properties>
    <kotlin.version>2.4.21</kotlin.version>
    <maven.compiler.release>17</maven.compiler.release>
  </properties>

  <dependencyManagement>
    <dependencies>
      <dependency>
        <groupId>com.fasterxml.jackson</groupId>
        <artifactId>jackson-bom</artifactId>
        <version>2.19.0</version>
        <type>pom</type>
        <scope>import</scope>
      </dependency>
    </dependencies>
  </dependencyManagement>

  <dependencies>
    <dependency>
      <groupId>org.jetbrains.kotlin</groupId>
      <artifactId>kotlin-stdlib</artifactId>
      <version>${kotlin.version}</version>
    </dependency>
    <dependency>
      <groupId>com.fasterxml.jackson.module</groupId>
      <artifactId>jackson-module-kotlin</artifactId>
    </dependency>
    <dependency>
      <groupId>org.postgresql</groupId>
      <artifactId>postgresql</artifactId>
      <version>42.7.5</version>
      <scope>runtime</scope>
    </dependency>
    <dependency>
      <groupId>jakarta.servlet</groupId>
      <artifactId>jakarta.servlet-api</artifactId>
      <version>6.1.0</version>
      <scope>provided</scope>
    </dependency>
    <dependency>
      <groupId>com.example</groupId>
      <artifactId>legacy-client</artifactId>
      <version>1.4</version>
      <exclusions>
        <exclusion>
          <groupId>commons-logging</groupId>
          <artifactId>commons-logging</artifactId>
        </exclusion>
      </exclusions>
    </dependency>
    <dependency>
      <groupId>org.jetbrains.kotlin</groupId>
      <artifactId>kotlin-test-junit5</artifactId>
      <version>${kotlin.version}</version>
      <scope>test</scope>
    </dependency>
  </dependencies>

  <repositories>
    <repository>
      <id>company</id>
      <url>https://repo.example.com/maven</url>
    </repository>
  </repositories>
</project>
```

```toml title="kargo.toml"
[package]
name = "billing"
version = "2.0.0"
group = "com.example"

[kotlin]
version = "2.4.21"                 # kotlin-stdlib is added automatically

[target.jvm]
jvm-target = "17"

[dependencies]
"com.fasterxml.jackson:jackson-bom" = { version = "2.19.0", bom = true }
"com.fasterxml.jackson.module:jackson-module-kotlin" = {}
"org.postgresql:postgresql" = { version = "42.7.5", scope = "runtime" }
"jakarta.servlet:jakarta.servlet-api" = { version = "6.1.0", scope = "provided" }
"com.example:legacy-client" = { version = "1.4", exclude = ["commons-logging:commons-logging"] }

[dev-dependencies]                 # kotlin-test-junit5 is added automatically

[repositories]
company = { url = "https://repo.example.com/maven", username-env = "REPO_USER", password-env = "REPO_PASS" }
```

Credentials that lived in `~/.m2/settings.xml` become environment variables named by `username-env` and `password-env`. The `<mirrors>` and `<proxies>` of `settings.xml` become `[mirrors]` and `[http] proxy` in `~/.kargo/config.toml` (see [Offline and corporate builds](../../guides/offline-and-mirrors/)).

## Directories

| Maven | Kargo |
|---|---|
| `src/main/kotlin`, `src/main/java` | `src/` |
| `src/main/resources` | `resources/` |
| `src/test/kotlin`, `src/test/java` | `tests/` |
| `src/test/resources` | `test-resources/` |
| `target/` | `target/` (different layout) |

## Reference: Maven → Kargo

| Maven | Kargo |
|---|---|
| `<scope>compile</scope>` (default) | `[dependencies]` |
| `<scope>runtime</scope>` | `scope = "runtime"` |
| `<scope>provided</scope>` | `scope = "provided"` |
| `maven-compiler-plugin` `<annotationProcessorPaths>` | `scope = "processor"` |
| `maven-compiler-plugin` `<compilerArgs>` | `[target.jvm] javac-args` |
| `<scope>test</scope>` | `[dev-dependencies]` |
| `<type>pom</type><scope>import</scope>` | `bom = true` |
| `<exclusions>` | `exclude = ["group:artifact"]` |
| `<classifier>` | `classifier = "..."` |
| `<type>` | `extension = "..."` |
| `<optional>true</optional>` | not needed; leave it out of consumers |
| `<properties>` for versions | write versions directly, or use a BOM |
| `<parent>` (e.g. `spring-boot-starter-parent`) | the matching BOM, or `[frameworks] spring-boot` |
| `kotlin-maven-plugin` `<compilerPlugins>` | `[frameworks]` or `[plugins.<name>]` |
| `<args>` of `kotlin-maven-plugin` | `[kotlin] compiler-args` |
| `maven-shade-plugin` / `maven-assembly-plugin` | `kargo package` |
| `exec-maven-plugin` | `kargo run` or `[tasks]` |
| `maven-surefire-plugin` | `kargo test` (JUnit Platform) |
| `mvn dependency:tree` | `kargo tree` |
| `mvn dependency:tree -Dincludes=g:a -Dverbose` | `kargo why g:a` or `kargo tree --invert g:a` |
| `dokka-maven-plugin` / `mvn javadoc:javadoc` | `kargo doc` |
| `ktlint-maven-plugin`, `spotless-maven-plugin` | `kargo fmt` / `kargo fmt --check` |
| `detekt-maven-plugin` | `kargo lint` |
| `mvn -o` | `kargo --offline` |
| `mvn versions:display-dependency-updates` | `kargo outdated` |
| `mvn versions:use-latest-releases` | `kargo upgrade --incompatible` |
| `mvn install` | `kargo publish --local`; other projects read it with `[repositories] local = "maven-local"` |
| `mvn deploy` (`<distributionManagement>`) | `kargo publish --repository <name>`, a `[repositories]` entry |
| `maven-source-plugin`, `maven-javadoc-plugin`, `maven-gpg-plugin`, `central-publishing-maven-plugin` | built into `kargo publish`; see [Publishing a library](../../guides/publishing/) |
| `<name>`, `<description>`, `<url>`, `<licenses>`, `<developers>`, `<scm>` | `name`, `description`, `homepage`, `license`, `authors`, `repository` in `[package]` |

## What's different

- **Conflict resolution.** Maven picks the *nearest* version in the tree; Kargo picks the *highest*, like Gradle. Usually this means fewer runtime `NoSuchMethodError`s, but check `kargo tree --duplicates` after migrating and use `force = true` if you need a specific version.
- **Lockfile.** Kargo always writes `kargo.lock` with checksums. Commit it.
- **Multi-module builds.** `<modules>` becomes a [workspace](../../guides/workspaces/) with `members`, a dependency on a sibling module becomes `core = { path = "../core" }`, and `<dependencyManagement>` versions can move to `[workspace.dependencies]`. There is no parent POM inheritance of other settings.
