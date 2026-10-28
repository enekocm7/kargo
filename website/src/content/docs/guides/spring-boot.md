---
title: Spring Boot
description: Build a Spring Boot application in Kotlin without Gradle or Maven.
---

Kargo's `spring-boot` framework sets up everything the Spring Boot Gradle plugin and Kotlin Spring plugins would.

## Create the project

```sh
kargo new greeter -w spring-boot
cd greeter
kargo run
```

```toml title="kargo.toml"
[frameworks]
spring-boot = { starters = ["web"] }
```

The generated app has one REST endpoint:

```kotlin title="src/Main.kt"
package com.example.greeter

import org.springframework.boot.autoconfigure.SpringBootApplication
import org.springframework.boot.runApplication
import org.springframework.web.bind.annotation.GetMapping
import org.springframework.web.bind.annotation.RequestParam
import org.springframework.web.bind.annotation.RestController

@SpringBootApplication
class Application

@RestController
class GreetingController {
    @GetMapping("/")
    fun greet(
        @RequestParam(defaultValue = "World") name: String,
    ) = greeting(name)
}

fun greeting(name: String) = "Hello, $name!"

fun main(args: Array<String>) {
    runApplication<Application>(args = args)
}
```

```console
$ curl 'localhost:8080/?name=Kargo'
Hello, Kargo!
```

## Starters

Each entry in `starters` adds `org.springframework.boot:spring-boot-starter-<name>`. Versions come from the Spring Boot BOM, so you never write them:

```toml
[frameworks]
spring-boot = { version = "4.1.1", starters = ["web", "data-jpa", "validation", "actuator"] }

[dependencies]
"com.h2database:h2" = { scope = "runtime" }       # version managed by the Spring Boot BOM
"org.postgresql:postgresql" = { scope = "runtime" }
```

### Renamed starters

Spring Boot 4 renamed some starters, and the old names don't exist in its BOM. `spring-boot-starter-aop` is now `spring-boot-starter-aspectj`, so `starters = ["aop"]` is an error that names the replacement before any download starts:

```console
$ kargo build
error: the Spring Boot starter `aop` of `[frameworks] spring-boot` does not exist in Spring Boot 4.1.1; it is called `aspectj` there (`starters = ["aspectj"]`)
```

Kargo checks the starters of `starters` and the `org.springframework.boot:spring-boot-starter-*` coordinates in your dependency tables against the Spring Boot version, in both directions: on Spring Boot 3 it is `aop`, and `aspectj` is the error. Starters Kargo doesn't know about are passed on as they are, so a misspelled one still fails as a missing artifact. Look for it among the `spring-boot-starter-*` artifacts on [Maven Central](https://central.sonatype.com/namespace/org.springframework.boot).

## JPA entities

With `data-jpa` (or `jpa = true`), Kargo enables the `noarg` and `allopen` compiler plugins for `@Entity`, `@MappedSuperclass` and `@Embeddable`, so Kotlin classes work as JPA entities:

```kotlin
import jakarta.persistence.*
import org.springframework.data.jpa.repository.JpaRepository

@Entity
class Book(
    var title: String,
    var author: String,
    @Id @GeneratedValue var id: Long? = null,
)

interface BookRepository : JpaRepository<Book, Long> {
    fun findByAuthor(author: String): List<Book>
}
```

## Configuration

`resources/application.properties` (or `application.yml`) is on the classpath:

```properties title="resources/application.properties"
server.port=8081
spring.jpa.hibernate.ddl-auto=update
```

## Testing

`spring-boot-starter-test` is added to the tests automatically:

```kotlin title="tests-jvm/ApplicationTest.kt"
package com.example.greeter

import org.junit.jupiter.api.Test
import org.springframework.beans.factory.annotation.Autowired
import org.springframework.boot.test.context.SpringBootTest
import kotlin.test.assertEquals

@SpringBootTest
class ApplicationTest {
    @Autowired lateinit var controller: GreetingController

    @Test
    fun greets() {
        assertEquals("Hello, Spring!", controller.greet("Spring"))
    }
}
```

### Test slices in Spring Boot 4

Spring Boot 4 split its test auto-configuration into one module per technology and moved the annotations to new packages. `spring-boot-starter-test` only has the core (`@SpringBootTest`), so `@WebMvcTest`, `@AutoConfigureMockMvc` and `@DataJpaTest` fail to compile until you add the test starter that matches each starter you use. The Spring Boot BOM manages their versions, so add them to `[dev-dependencies]` without one:

```toml title="kargo.toml"
[frameworks]
spring-boot = { starters = ["webmvc", "data-jpa"] }

[dev-dependencies]
"org.springframework.boot:spring-boot-starter-webmvc-test" = {}
"org.springframework.boot:spring-boot-starter-data-jpa-test" = {}
```

The test starter of `<name>` is `spring-boot-starter-<name>-test`:

| Starter you use | Add to `[dev-dependencies]`                   | Annotations it brings                                                                     |
| --------------- | --------------------------------------------- | ----------------------------------------------------------------------------------------- |
| `web`, `webmvc` | `spring-boot-starter-webmvc-test`             | `org.springframework.boot.webmvc.test.autoconfigure.WebMvcTest`, `AutoConfigureMockMvc`   |
| `webflux`       | `spring-boot-starter-webflux-test`            | `org.springframework.boot.webflux.test.autoconfigure.WebFluxTest`                         |
| `data-jpa`      | `spring-boot-starter-data-jpa-test`           | `org.springframework.boot.data.jpa.test.autoconfigure.DataJpaTest`                        |
| `jdbc`          | `spring-boot-starter-jdbc-test`               | `org.springframework.boot.jdbc.test.autoconfigure.JdbcTest`                               |
| `validation`    | `spring-boot-starter-validation-test`         |                                                                                           |
| `security`      | `spring-boot-starter-security-test`           |                                                                                           |

There is a test starter for most other starters too (for example `actuator`). Without a matching one, look for the starter's `-test` artifact on [Maven Central](https://central.sonatype.com/namespace/org.springframework.boot). A test with MockMvc then looks like this:

```kotlin title="tests-jvm/GreetingControllerTest.kt"
import org.junit.jupiter.api.Test
import org.springframework.beans.factory.annotation.Autowired
import org.springframework.boot.webmvc.test.autoconfigure.AutoConfigureMockMvc
import org.springframework.boot.test.context.SpringBootTest
import org.springframework.test.web.servlet.MockMvc
import org.springframework.test.web.servlet.get

@SpringBootTest
@AutoConfigureMockMvc
class GreetingControllerTest {
    @Autowired lateinit var mockMvc: MockMvc

    @Test
    fun greets() {
        mockMvc.get("/?name=Kargo").andExpect { status { isOk() } }
    }
}
```

:::note
These packages and modules are new in Spring Boot 4. On Spring Boot 3 (`version = "3.x"`), `spring-boot-starter-test` already includes the slices and they live under `org.springframework.boot.test.autoconfigure...`, for example `org.springframework.boot.test.autoconfigure.web.servlet.AutoConfigureMockMvc`. Copying a Boot 3 example into a Boot 4 project is the usual cause of "unresolved reference" errors in tests.
:::

## Packaging

```sh
kargo package --release
java -jar target/release/jvm/greeter-0.1.0-all.jar
```
