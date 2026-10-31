package com.example.springboot

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
