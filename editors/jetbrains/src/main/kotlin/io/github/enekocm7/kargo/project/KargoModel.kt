// What the IDE needs to know about a Kargo project, read from `kargo metadata`: one module per
// package, with its source roots, JDK and libraries. No IDE classes here, so it is easy to test.

package io.github.enekocm7.kargo.project

import com.google.gson.JsonElement
import com.google.gson.JsonObject
import com.google.gson.JsonParser
import java.io.File

/** A jar of a package's classpath, with its sources jar when Kargo has downloaded it. */
data class KargoLibrary(val name: String, val classes: String, val sources: String?, val test: Boolean)

/** A package of the project, as one IDE module. */
data class KargoModule(
    val name: String,
    val root: String,
    val sourceRoots: List<String>,
    val testRoots: List<String>,
    val resourceRoots: List<String>,
    val testResourceRoots: List<String>,
    val excluded: List<String>,
    val libraries: List<KargoLibrary>,
    /** The packages this one depends on by path, each with whether only its tests do. */
    val moduleDependencies: List<Pair<String, Boolean>>,
    val target: String,
)

data class KargoProject(val fingerprint: String?, val jdkHome: String?, val modules: List<KargoModule>) {
    companion object {
        /**
         * Reads `kargo metadata`. Each package becomes a module with the roots and classpath of its JVM
         * target, or of its first other target when it has none (those have no classpath: the IDE can't read klibs).
         */
        fun parse(json: String): KargoProject {
            val root = JsonParser.parseString(json).asJsonObject
            val modules = root.array("packages").map { module(root, it.asJsonObject) }
            return KargoProject(root.string("fingerprint"), root.obj("toolchain")?.obj("jdk")?.string("home"), modules)
        }

        private fun module(project: JsonObject, pkg: JsonObject): KargoModule {
            val targets = pkg.array("targets").map { it.asJsonObject }
            val target = targets.firstOrNull { it.string("kind") == "jvm" }
                ?: targets.firstOrNull { it.string("kind") != "android" }
                ?: targets.firstOrNull()
            val root = pkg.string("root").orEmpty()
            val classpath = target?.array("dependencies")?.map { it.asJsonObject }?.firstOrNull { it.string("platform") == "jvm" }
            val compile = classpath?.array("compile").orEmpty().map { it.asJsonObject }
            val test = classpath?.array("test").orEmpty().map { it.asJsonObject }
            val compilePaths = compile.mapNotNull { it.string("path") }.toSet()
            val compilePackages = compile.mapNotNull { it.string("package") }.toSet()
            val libraries = compile.mapNotNull { library(it, false) } +
                test.filter { it.string("path") !in compilePaths }.mapNotNull { library(it, true) }
            val packages = compile.mapNotNull { it.string("package") }.map { it to false } +
                test.mapNotNull { it.string("package") }.filter { it !in compilePackages }.map { it to true }
            return KargoModule(
                name = pkg.string("name").orEmpty(),
                root = root,
                sourceRoots = target?.strings("source-roots").orEmpty(),
                // `examples/` is compiled against the package's sources.
                testRoots = target?.strings("test-roots").orEmpty() + File(root, "examples").path,
                resourceRoots = target?.strings("resource-roots").orEmpty(),
                testResourceRoots = target?.strings("test-resource-roots").orEmpty(),
                excluded = listOfNotNull(project.string("target-dir")),
                libraries = libraries,
                moduleDependencies = packages.distinct(),
                target = target?.string("kind").orEmpty(),
            )
        }

        private fun library(entry: JsonObject, test: Boolean): KargoLibrary? {
            val path = entry.string("path") ?: return null
            return KargoLibrary("Kargo: ${entry.string("module")}:${entry.string("version")}", path, entry.string("sources"), test)
        }

        private fun JsonObject.string(name: String): String? = get(name)?.takeIf { !it.isJsonNull }?.asString
        private fun JsonObject.obj(name: String): JsonObject? = get(name)?.takeIf { it.isJsonObject }?.asJsonObject
        private fun JsonObject.array(name: String): List<JsonElement> = get(name)?.takeIf { it.isJsonArray }?.asJsonArray?.toList().orEmpty()
        private fun JsonObject.strings(name: String): List<String> = array(name).map { it.asString }
    }
}
