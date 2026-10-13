// The project model `kargo lsp` sends in `initializationOptions.kargo.project` and in
// `kargo/projectChanged` (see crates/kargo-lsp/src/model.rs and the website's "LSP project
// model" page).
package io.github.enekocm7.kargo.server.model

import com.google.gson.JsonArray
import com.google.gson.JsonElement
import com.google.gson.JsonObject
import java.nio.file.Path
import java.nio.file.Paths

/** The model versions this server reads. */
const val MODEL_VERSION = 1

data class ProjectModel(
    val root: String,
    val targetDir: Path?,
    val resolved: Boolean,
    val jdk: Jdk?,
    val kotlinVersion: String?,
    val libraries: List<Library>,
    val modules: List<Module>,
) {
    private val librariesById = libraries.associateBy { it.id }
    private val modulesById = modules.associateBy { it.id }

    fun library(id: String): Library? = librariesById[id]

    fun module(id: String): Module? = modulesById[id]

    /** Where the server logs: `target/lsp/kotlin-server.log`. */
    val logFile: Path? get() = targetDir?.resolve("lsp")?.resolve("kotlin-server.log")

    companion object {
        /** Parses a model, or fails with a message for the user. */
        fun parse(json: JsonObject): ProjectModel {
            val version = json.int("modelVersion")
            require(version == MODEL_VERSION) {
                "kargo lsp sent project model version $version; this server reads version $MODEL_VERSION, so update Kargo and the server together"
            }
            return ProjectModel(
                root = json.string("root") ?: "",
                targetDir = json.string("targetDir")?.let(Paths::get),
                resolved = json.bool("resolved"),
                jdk = json.obj("jdk")?.let { jdk -> jdk.string("home")?.let { Jdk(Paths.get(it), jdk.int("version")) } },
                kotlinVersion = json.obj("kotlin")?.string("projectVersion"),
                libraries = json.objects("libraries").map(Library::parse),
                modules = json.objects("modules").map(Module::parse),
            )
        }
    }
}

data class Jdk(val home: Path, val version: Int?)

data class Library(
    val id: String,
    /** `jar`, `klib`, or another file extension. */
    val kind: String,
    val path: Path,
    val sources: Path?,
    val sha256: String?,
    /** `jvm`, `wasm-js`, `wasm-wasi`, `native` or `native:<platform>`. */
    val platform: String,
) {
    companion object {
        fun parse(json: JsonObject) = Library(
            id = json.string("id") ?: "",
            kind = json.string("kind") ?: "jar",
            path = Paths.get(json.string("path") ?: ""),
            sources = json.string("sources")?.let(Paths::get),
            sha256 = json.string("sha256"),
            platform = json.string("platform") ?: "jvm",
        )
    }
}

data class Module(
    val id: String,
    val packageName: String,
    val sourceSet: String,
    val scope: String,
    val platforms: List<String>,
    val sourceRoots: List<Path>,
    val dependsOn: List<String>,
    val friends: List<String>,
    /** `lib:<id>` and `module:<id>`, in order. */
    val dependencies: List<String>,
    val compiler: CompilerSettings,
) {
    val libraryIds: List<String> get() = dependencies.filter { it.startsWith("lib:") }.map { it.removePrefix("lib:") }
    val moduleIds: List<String> get() = dependencies.filter { it.startsWith("module:") }.map { it.removePrefix("module:") }

    companion object {
        fun parse(json: JsonObject) = Module(
            id = json.string("id") ?: "",
            packageName = json.string("package") ?: "",
            sourceSet = json.string("sourceSet") ?: "",
            scope = json.string("scope") ?: "main",
            platforms = json.strings("platform"),
            sourceRoots = json.strings("sourceRoots").map { Paths.get(it) },
            dependsOn = json.strings("dependsOn"),
            friends = json.strings("friends"),
            dependencies = json.strings("dependencies"),
            compiler = CompilerSettings.parse(json.obj("compiler") ?: JsonObject()),
        )
    }
}

data class CompilerPlugin(val id: String, val module: String?, val options: List<Pair<String, String>>)

data class CompilerSettings(
    val languageVersion: String?,
    val apiVersion: String?,
    val optIn: List<String>,
    val multiplatform: Boolean,
    val jvmTarget: String?,
    val freeArgs: List<String>,
    val plugins: List<CompilerPlugin>,
) {
    companion object {
        fun parse(json: JsonObject) = CompilerSettings(
            languageVersion = json.string("languageVersion"),
            apiVersion = json.string("apiVersion"),
            optIn = json.strings("optIn"),
            multiplatform = json.bool("multiplatform"),
            jvmTarget = json.string("jvmTarget"),
            freeArgs = json.strings("freeArgs"),
            plugins = json.objects("plugins").map { plugin ->
                CompilerPlugin(
                    id = plugin.string("id") ?: "",
                    module = plugin.string("module"),
                    options = plugin.objects("options").map { (it.string("name") ?: "") to (it.string("value") ?: "") },
                )
            },
        )
    }
}

private fun JsonObject.value(key: String): JsonElement? = get(key)?.takeUnless { it.isJsonNull }

private fun JsonObject.string(key: String): String? = value(key)?.takeIf { it.isJsonPrimitive }?.asString

private fun JsonObject.int(key: String): Int? = value(key)?.takeIf { it.isJsonPrimitive && it.asJsonPrimitive.isNumber }?.asInt

private fun JsonObject.bool(key: String): Boolean = value(key)?.takeIf { it.isJsonPrimitive && it.asJsonPrimitive.isBoolean }?.asBoolean ?: false

private fun JsonObject.obj(key: String): JsonObject? = value(key)?.takeIf { it.isJsonObject }?.asJsonObject

private fun JsonObject.array(key: String): JsonArray = value(key)?.takeIf { it.isJsonArray }?.asJsonArray ?: JsonArray()

private fun JsonObject.strings(key: String): List<String> = array(key).filter { it.isJsonPrimitive }.map { it.asString }

private fun JsonObject.objects(key: String): List<JsonObject> = array(key).filter { it.isJsonObject }.map { it.asJsonObject }
