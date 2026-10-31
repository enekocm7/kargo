// `kargo metadata` as plain Kotlin data: the project model Kargo describes for editors. Only the fields the
// import needs are read; the format only ever adds fields (see FORMAT_VERSION in kargo-ops' metadata.rs).

package io.github.enekocm7.kargo.project

import com.google.gson.JsonArray
import com.google.gson.JsonElement
import com.google.gson.JsonObject
import com.google.gson.JsonParser

/** The newest `kargo metadata` format version this plugin understands. */
const val SUPPORTED_METADATA_VERSION = 1

class KargoMetadata(
    val version: Int,
    val root: String,
    val fingerprint: String?,
    /** The files whose change makes the metadata stale: every `kargo.toml` and the `kargo.lock`. */
    val watch: List<String>,
    val kotlinVersion: String?,
    val jdkHome: String?,
    val nativeHome: String?,
    val packages: List<KargoPackage>,
    /** Whether the dependencies were resolved (`--no-deps` leaves them out). */
    val resolved: Boolean,
) {
    companion object {
        fun parse(json: String): KargoMetadata {
            val doc = JsonParser.parseString(json).asJsonObject
            val toolchain = doc.obj("toolchain")
            return KargoMetadata(
                version = doc.int("version") ?: 0,
                root = doc.string("root").orEmpty(),
                fingerprint = doc.string("fingerprint"),
                watch = doc.strings("watch"),
                kotlinVersion = toolchain?.string("kotlin"),
                jdkHome = toolchain?.obj("jdk")?.string("home"),
                nativeHome = toolchain?.obj("kotlin-native")?.string("home"),
                packages = doc.objs("packages").map(KargoPackage::parse),
                resolved = doc.bool("resolved") ?: true,
            )
        }
    }
}

class KargoPackage(
    val name: String,
    val root: String,
    val manifest: String,
    val member: Boolean,
    val languageVersion: String?,
    val apiVersion: String?,
    val optIn: List<String>,
    val targets: List<KargoTarget>,
) {
    companion object {
        fun parse(json: JsonObject): KargoPackage {
            val kotlin = json.obj("kotlin")
            return KargoPackage(
                name = json.string("name").orEmpty(),
                root = json.string("root").orEmpty(),
                manifest = json.string("manifest").orEmpty(),
                member = json.bool("member") ?: true,
                languageVersion = kotlin?.string("language-version"),
                apiVersion = kotlin?.string("api-version"),
                optIn = kotlin?.strings("opt-in").orEmpty(),
                targets = json.objs("targets").map(KargoTarget::parse),
            )
        }
    }
}

/** One enabled target (`jvm`, `wasm`, `native` or `android`) of a package. */
class KargoTarget(
    val kind: String,
    val sourceRoots: List<String>,
    val testRoots: List<String>,
    val resourceRoots: List<String>,
    val testResourceRoots: List<String>,
    val classesDir: String?,
    val testClassesDir: String?,
    val outputDir: String?,
    val jvmTarget: String?,
    val compilerArgs: List<String>,
    val compilerPlugins: List<KargoCompilerPlugin>,
    /** The Native platforms (`mingw_x64`, ...) the target is built for. */
    val platforms: List<String>,
    /** Resolved dependencies per platform; null when they weren't resolved (`--no-deps`, Android). */
    val dependencies: List<KargoPlatformDependencies>?,
    /** The npm packages a Wasm target runs with, where they are installed; empty for other targets. */
    val npm: List<KargoNpmPackage>,
    /** The Kotlin declarations generated from the npm packages' TypeScript declarations, or null without them. */
    val npmDeclarations: String?,
    /** The Kotlin declarations generated from the package's own TypeScript modules (`js/`), or null without them. */
    val jsDeclarations: String?,
) {
    companion object {
        fun parse(json: JsonObject) = KargoTarget(
            kind = json.string("kind").orEmpty(),
            sourceRoots = json.strings("source-roots"),
            testRoots = json.strings("test-roots"),
            resourceRoots = json.strings("resource-roots"),
            testResourceRoots = json.strings("test-resource-roots"),
            classesDir = json.string("classes-dir"),
            testClassesDir = json.string("test-classes-dir"),
            outputDir = json.string("output-dir"),
            jvmTarget = json.string("jvm-target"),
            compilerArgs = json.strings("compiler-args"),
            compilerPlugins = json.objs("compiler-plugins").map(KargoCompilerPlugin::parse),
            platforms = json.strings("platforms"),
            dependencies = json.get("dependencies")?.takeIf { it.isJsonArray }?.asJsonArray?.map { KargoPlatformDependencies.parse(it.asJsonObject) },
            npm = json.objs("npm").map(KargoNpmPackage::parse),
            npmDeclarations = json.string("npm-declarations"),
            jsDeclarations = json.string("js-declarations"),
        )
    }
}

/**
 * An npm package of a Wasm target, installed in `directory`. `test` is set when only the tests need it, and
 * then it is installed apart from the program's packages.
 */
class KargoNpmPackage(val name: String, val version: String?, val directory: String?, val test: Boolean) {
    companion object {
        fun parse(json: JsonObject) = KargoNpmPackage(
            name = json.string("name").orEmpty(),
            version = json.string("version"),
            directory = json.string("directory"),
            test = json.bool("test") ?: false,
        )
    }
}

class KargoCompilerPlugin(val id: String?, val jar: String?, val options: List<String>) {
    companion object {
        fun parse(json: JsonObject) = KargoCompilerPlugin(json.string("id"), json.string("jar"), json.strings("options"))
    }
}

/** The classpath (JVM) or klib path (Wasm, Native) of one platform: `jvm`, `wasm-js`, `native-mingw_x64`, ... */
class KargoPlatformDependencies(
    val platform: String,
    val compile: List<KargoDependency>,
    val runtime: List<KargoDependency>,
    val test: List<KargoDependency>,
) {
    companion object {
        fun parse(json: JsonObject) = KargoPlatformDependencies(
            platform = json.string("platform").orEmpty(),
            compile = json.objs("compile").map(KargoDependency::parse),
            runtime = json.objs("runtime").map(KargoDependency::parse),
            test = json.objs("test").map(KargoDependency::parse),
        )
    }
}

/**
 * One entry of a classpath: a downloaded artifact (`module`, `version`, `path`, maybe `sources`), or a path
 * dependency on another package of the project (`localPackage`).
 */
class KargoDependency(
    val module: String?,
    val version: String?,
    val path: String?,
    val sources: String?,
    val localPackage: String?,
) {
    companion object {
        fun parse(json: JsonObject) = KargoDependency(
            module = json.string("module"),
            version = json.string("version"),
            path = json.string("path"),
            sources = json.string("sources"),
            localPackage = json.string("package"),
        )
    }
}

private fun JsonObject.get(name: String): JsonElement? = if (has(name) && !this[name].isJsonNull) this[name] else null

private fun JsonObject.string(name: String): String? = get(name)?.takeIf { it.isJsonPrimitive }?.asString

private fun JsonObject.int(name: String): Int? = get(name)?.takeIf { it.isJsonPrimitive }?.asInt

private fun JsonObject.bool(name: String): Boolean? = get(name)?.takeIf { it.isJsonPrimitive }?.asBoolean

private fun JsonObject.obj(name: String): JsonObject? = get(name)?.takeIf { it.isJsonObject }?.asJsonObject

private fun JsonObject.array(name: String): JsonArray? = get(name)?.takeIf { it.isJsonArray }?.asJsonArray

private fun JsonObject.strings(name: String): List<String> = array(name)?.mapNotNull { if (it.isJsonPrimitive) it.asString else null }.orEmpty()

private fun JsonObject.objs(name: String): List<JsonObject> = array(name)?.filter { it.isJsonObject }?.map { it.asJsonObject }.orEmpty()
