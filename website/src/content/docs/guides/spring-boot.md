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
    fun greet(@RequestParam(defaultValue = "World") name: String) = greeting(name)
}

fun greeting(name: String) = "Hello, $name!"

fun main(args: Array<String>) {
    runApplication<Application>(*args)
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

## Packaging

```sh
kargo package --release
java -jar target/release/jvm/greeter-0.1.0-all.jar
```
