---
title: A REST API with Ktor
description: Build a small JSON REST API with Ktor and kotlinx.serialization, test it, and package it.
---

This guide builds a small to-do list API with [Ktor](https://ktor.io), a lightweight Kotlin web framework.

## Create the project

```sh
kargo new todo-api -w ktor,serialization
cd todo-api
```

```toml title="kargo.toml"
[frameworks]
ktor = { server = "netty" }
serialization = true
```

Because `serialization` is enabled too, Kargo also adds Ktor's JSON support and the content-negotiation plugin.

## Write the API

```kotlin title="src/Main.kt"
package com.example.todo_api

import io.ktor.http.HttpStatusCode
import io.ktor.serialization.kotlinx.json.json
import io.ktor.server.application.Application
import io.ktor.server.application.install
import io.ktor.server.engine.embeddedServer
import io.ktor.server.netty.Netty
import io.ktor.server.plugins.contentnegotiation.ContentNegotiation
import io.ktor.server.request.receive
import io.ktor.server.response.respond
import io.ktor.server.routing.*
import kotlinx.serialization.Serializable
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicInteger

@Serializable
data class Todo(val id: Int = 0, val title: String, val done: Boolean = false)

class TodoStore {
    private val todos = ConcurrentHashMap<Int, Todo>()
    private val nextId = AtomicInteger(1)

    fun all(): List<Todo> = todos.values.sortedBy { it.id }
    fun get(id: Int): Todo? = todos[id]
    fun add(todo: Todo): Todo = todo.copy(id = nextId.getAndIncrement()).also { todos[it.id] = it }
    fun delete(id: Int): Boolean = todos.remove(id) != null
}

fun Application.module(store: TodoStore = TodoStore()) {
    install(ContentNegotiation) { json() }
    routing {
        route("/todos") {
            get { call.respond(store.all()) }
            post {
                val created = store.add(call.receive<Todo>())
                call.respond(HttpStatusCode.Created, created)
            }
            get("/{id}") {
                val todo = call.parameters["id"]?.toIntOrNull()?.let(store::get)
                if (todo == null) call.respond(HttpStatusCode.NotFound) else call.respond(todo)
            }
            delete("/{id}") {
                val deleted = call.parameters["id"]?.toIntOrNull()?.let(store::delete) ?: false
                call.respond(if (deleted) HttpStatusCode.NoContent else HttpStatusCode.NotFound)
            }
        }
    }
}

fun main() {
    embeddedServer(Netty, port = 8080) { module() }.start(wait = true)
}
```

## Run it

```sh
kargo run
```

In another terminal:

```console
$ curl -X POST localhost:8080/todos -H 'Content-Type: application/json' -d '{"title":"Try Kargo"}'
{"id":1,"title":"Try Kargo","done":false}
$ curl localhost:8080/todos
[{"id":1,"title":"Try Kargo","done":false}]
```

## Test it

`ktor-server-test-host` is already on the test classpath. It runs your module in memory, without opening a port:

```kotlin title="tests/TodoApiTest.kt"
package com.example.todo_api

import io.ktor.client.request.*
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.HttpStatusCode
import io.ktor.http.contentType
import io.ktor.server.testing.testApplication
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class TodoApiTest {
    @Test
    fun createsAndListsTodos() = testApplication {
        application { module() }

        val created = client.post("/todos") {
            contentType(ContentType.Application.Json)
            setBody("""{"title":"Write tests"}""")
        }
        assertEquals(HttpStatusCode.Created, created.status)

        val list = client.get("/todos").bodyAsText()
        assertTrue("Write tests" in list)
    }

    @Test
    fun unknownTodoIs404() = testApplication {
        application { module() }
        assertEquals(HttpStatusCode.NotFound, client.get("/todos/42").status)
    }
}
```

```sh
kargo test
```

## Ship it

```sh
kargo package --release
java -jar target/release/jvm/todo-api-0.1.0-all.jar
```

A minimal container image:

```dockerfile title="Dockerfile"
FROM eclipse-temurin:21-jre
COPY target/release/jvm/todo-api-0.1.0-all.jar /app.jar
EXPOSE 8080
ENTRYPOINT ["java", "-jar", "/app.jar"]
```

## More Ktor plugins

Add server plugins by name:

```toml
[frameworks]
ktor = { server = "netty", server-plugins = ["cors", "call-logging", "status-pages", "auth"] }
```

Each name `x` adds `io.ktor:ktor-server-x` at the same Ktor version. Clients work the same way with `client` and `client-plugins`.
