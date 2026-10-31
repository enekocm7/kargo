package com.example.ktorserver

import io.ktor.server.engine.embeddedServer
import io.ktor.server.netty.Netty
import io.ktor.server.response.respondText
import io.ktor.server.routing.get
import io.ktor.server.routing.routing

fun greeting(name: String) = "Hello, $name!"

fun main() {
    embeddedServer(Netty, port = 8080) {
        routing {
            get("/") { call.respondText(greeting("Ktor")) }
        }
    }.start(wait = true)
}
