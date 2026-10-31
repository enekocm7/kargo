// From `kargo metadata` to the IDE's project model, as plain data. The plan lists the modules, their source
// roots and dependencies, and the libraries to create; KargoProjectImporter applies it. Keeping the two apart
// makes the mapping testable without an IDE.

package io.github.enekocm7.kargo.project

/** The kind of root a directory is in a module. */
enum class RootKind { SOURCE, TEST, RESOURCE, TEST_RESOURCE }

/** The Kotlin platform a module is analyzed for. */
enum class PlatformKind { JVM, NATIVE, WASM_JS, WASM_WASI }

/**
 * What the IDE's Kotlin plugin should treat a library as. NPM is the directory of an installed npm package (JavaScript
 * and `.d.ts` files, no klibs): a plain library with no Kotlin library kind.
 */
enum class LibraryKind { JVM, NATIVE, WASM_JS, WASM_WASI, NPM }

enum class DependencyScope { COMPILE, RUNTIME, TEST }

class RootSpec(val path: String, val kind: RootKind, val generated: Boolean = false) {
    override fun equals(other: Any?) = other is RootSpec && path == other.path && kind == other.kind && generated == other.generated

    override fun hashCode() = 31 * (31 * path.hashCode() + kind.hashCode()) + generated.hashCode()

    override fun toString() = (if (generated) "generated " else "") + "$kind $path"
}

/** One content root of a module with the source roots below it. */
data class ContentSpec(val root: String, val roots: List<RootSpec>, val excluded: List<String>)

/** A library: the jars (or klibs) of one Maven artifact, with its sources. */
data class LibrarySpec(val name: String, val kind: LibraryKind, val classes: List<String>, val sources: List<String>)

/** A dependency of a module on a library of the plan or on another module of the plan. */
data class DependencySpec(val library: String?, val module: String?, val scope: DependencyScope)

/**
 * A module of the IDE's project. `platforms` has one entry for a module analyzed for one platform; a package
 * with several targets gets a `common` module that the target modules refine (`dependsOn`).
 */
data class ModuleSpec(
    val name: String,
    val contents: List<ContentSpec>,
    val platform: PlatformKind,
    /** The Native target (`mingw_x64`) of a Native module. */
    val nativeTarget: String?,
    val dependsOn: List<String>,
    /** Whether the module is part of a package with several targets, so `expect` and `actual` are allowed. */
    val multiplatform: Boolean,
    val dependencies: List<DependencySpec>,
    val jdkHome: String?,
    val jvmTarget: String?,
    val classesDir: String?,
    val testClassesDir: String?,
    val languageVersion: String?,
    val apiVersion: String?,
    val optIn: List<String>,
    /** Compiler arguments other than the language, API and opt-in ones. */
    val extraArgs: List<String>,
    val pluginClasspath: List<String>,
    val pluginOptions: List<String>,
    /** The package this module's sources belong to, and its directory. */
    val packageName: String,
    val packageRoot: String,
    /** The target this module analyzes (`jvm`, `wasm`, `native`), or null for the shared `common` module. */
    val target: String?,
)

data class ImportPlan(val modules: List<ModuleSpec>, val libraries: List<LibrarySpec>) {
    companion object {
        const val LIBRARY_PREFIX = "Kargo: "
    }
}

/** The bits of the file system the planner needs, replaceable in tests. */
interface PlanFiles {
    fun exists(path: String): Boolean

    /** The names of the subdirectories of `path`, sorted. */
    fun subdirectories(path: String): List<String>
}

object DiskFiles : PlanFiles {
    override fun exists(path: String) = java.io.File(path).exists()

    override fun subdirectories(path: String): List<String> =
        java.io.File(path).listFiles { f -> f.isDirectory }?.map { it.name }?.sorted().orEmpty()
}

object KargoImportPlanner {
    private val TARGET_ORDER = listOf("jvm", "native", "wasm")

    fun plan(metadata: KargoMetadata, files: PlanFiles = DiskFiles): ImportPlan {
        val libraries = LinkedHashMap<String, LibrarySpec>()
        val modules = ArrayList<ModuleSpec>()
        val packages = metadata.packages
        val targetsOf = packages.associate { pkg -> pkg.name to analyzable(pkg).map { it.kind } }
        for (pkg in packages) {
            val targets = analyzable(pkg)
            if (targets.isEmpty()) continue
            val context = Context(metadata, pkg, targetsOf, files, libraries)
            if (targets.size == 1) {
                modules += context.single(targets.single())
            } else {
                modules += context.common(targets)
                for (target in targets) modules += context.targetModule(target)
            }
        }
        return ImportPlan(modules, libraries.values.toList())
    }

    /** The targets the IDE can analyze: Android is Gradle's, and a target must be known to the importer. */
    private fun analyzable(pkg: KargoPackage): List<KargoTarget> =
        pkg.targets.filter { it.kind in TARGET_ORDER }.sortedBy { TARGET_ORDER.indexOf(it.kind) }

    /** The name of the module of `package` that holds the code of `target`. */
    fun moduleName(packageName: String, target: String, targets: List<String>): String =
        if (targets.size <= 1) packageName else "$packageName.$target"

    private class Context(
        val metadata: KargoMetadata,
        val pkg: KargoPackage,
        val targetsOf: Map<String, List<String>>,
        val files: PlanFiles,
        val libraries: MutableMap<String, LibrarySpec>,
    ) {
        private val root = norm(pkg.root)
        private val excluded = listOf("$root/target")

        /**
         * The content entries of a module: the roots in the package directory, and each root inside `target/` (the
         * output of `[[generate]]` steps) as an entry of its own, since a root inside the excluded `target/` would be
         * left out of the index.
         */
        private fun contents(roots: List<RootSpec>): List<ContentSpec> {
            val (generated, own) = roots.partition { it.path.startsWith("$root/target/") }
            return listOf(ContentSpec(root, own, excluded)) + generated.map { ContentSpec(it.path, listOf(it), emptyList()) }
        }

        fun single(target: KargoTarget): ModuleSpec {
            val roots = target.sourceRoots.map { RootSpec(norm(it), RootKind.SOURCE) } +
                target.testRoots.map { RootSpec(norm(it), RootKind.TEST) } +
                target.resourceRoots.map { RootSpec(norm(it), RootKind.RESOURCE) } +
                target.testResourceRoots.map { RootSpec(norm(it), RootKind.TEST_RESOURCE) } +
                npmDeclarations(target) +
                examples()
            return module(pkg.name, contents(roots), target, dependsOn = emptyList(), target = target.kind)
        }

        /** The shared module: `src/` and `tests/`, analyzed like the first target (JVM, else Native, else Wasm). */
        fun common(targets: List<KargoTarget>): ModuleSpec {
            val first = targets.first()
            val shared = sharedRoots(targets)
            val roots = shared.sources.map { RootSpec(it, RootKind.SOURCE) } + shared.tests.map { RootSpec(it, RootKind.TEST) } + examples()
            return module("${pkg.name}.common", contents(roots), first, dependsOn = emptyList(), target = null)
        }

        fun targetModule(target: KargoTarget): ModuleSpec {
            val shared = sharedRoots(pkg.targets.filter { it.kind in targetsOf.getValue(pkg.name) })
            val contents = ArrayList<ContentSpec>()
            fun add(paths: List<String>, kind: RootKind, sharedPaths: List<String>) {
                for (path in paths.map(::norm).filter { it !in sharedPaths }) {
                    // The output of `[[generate]]` steps is in `target/`, and only exists once a build or `kargo generate` ran.
                    if (files.exists(path) || path.startsWith("$root/target/")) contents += ContentSpec(path, listOf(RootSpec(path, kind)), emptyList())
                }
            }
            add(target.sourceRoots, RootKind.SOURCE, shared.sources)
            add(target.testRoots, RootKind.TEST, shared.tests)
            add(target.resourceRoots, RootKind.RESOURCE, emptyList())
            add(target.testResourceRoots, RootKind.TEST_RESOURCE, emptyList())
            // The Kotlin declarations generated for `npm-dependencies` are main code of the Wasm module.
            // They sit under `target/`, so they are kept even before the first build creates them.
            for (declarations in npmDeclarations(target)) {
                if (declarations.path !in contents.flatMap { it.roots }.map { it.path }) {
                    contents += ContentSpec(declarations.path, listOf(declarations), emptyList())
                }
            }
            return module(moduleName(pkg.name, target.kind, targetsOf.getValue(pkg.name)), contents, target, dependsOn = listOf("${pkg.name}.common"), target = target.kind)
        }

        /**
         * The Kotlin declarations generated for a Wasm target's `npm-dependencies`, as a generated source
         * root: kept even before the first build creates the directory, so the IDE picks the files up when
         * they appear. Never on the common module or the test roots.
         */
        private fun npmDeclarations(target: KargoTarget): List<RootSpec> {
            if (target.kind != "wasm") return emptyList()
            val dir = target.npmDeclarations?.let(::norm) ?: return emptyList()
            // Unit tests don't parse `kargo.toml` and can hand any directory; the shape is what matters.
            return listOf(RootSpec(dir, RootKind.SOURCE, generated = true))
        }

        /** `examples/` is compiled against the package's sources, so the IDE sees it as test code. */
        private fun examples() = listOf(RootSpec("$root/examples", RootKind.TEST))

        /** The source and test directories every target of the package compiles: `src/` and `tests/`. */
        fun sharedRoots(targets: List<KargoTarget>): Shared {
            fun common(select: (KargoTarget) -> List<String>): List<String> {
                val lists = targets.map { t -> select(t).map(::norm) }
                return lists.first().filter { path -> lists.all { path in it } }
            }
            return Shared(common { it.sourceRoots }, common { it.testRoots })
        }

        private fun module(name: String, contents: List<ContentSpec>, source: KargoTarget, dependsOn: List<String>, target: String?): ModuleSpec {
            val platform = platformOf(source)
            val dependencies = dependencies(source, platform)
            val nativeTarget = if (platform.first == PlatformKind.NATIVE) platform.second else null
            val (language, api, optIn, extra) = splitArgs(source.compilerArgs)
            return ModuleSpec(
                name = name,
                contents = contents,
                platform = platform.first,
                nativeTarget = nativeTarget,
                dependsOn = dependsOn,
                multiplatform = targetsOf.getValue(pkg.name).size > 1,
                dependencies = dependencies,
                jdkHome = if (platform.first == PlatformKind.JVM) metadata.jdkHome else null,
                jvmTarget = source.jvmTarget,
                classesDir = source.classesDir?.let(::norm),
                testClassesDir = source.testClassesDir?.let(::norm),
                languageVersion = language ?: pkg.languageVersion,
                apiVersion = api ?: pkg.apiVersion,
                optIn = optIn.ifEmpty { pkg.optIn },
                extraArgs = extra,
                pluginClasspath = source.compilerPlugins.mapNotNull { it.jar }.map(::norm),
                pluginOptions = source.compilerPlugins.flatMap { it.options },
                packageName = pkg.name,
                packageRoot = root,
                target = target,
            )
        }

        /** The platform the IDE analyzes `target` for, with the matching entry of its dependencies. */
        private fun platformOf(target: KargoTarget): Pair<PlatformKind, String?> = when (target.kind) {
            "jvm" -> PlatformKind.JVM to null
            "native" -> PlatformKind.NATIVE to (target.dependencies?.firstOrNull()?.platform?.removePrefix("native-") ?: target.platforms.firstOrNull())
            else -> {
                val platform = target.dependencies?.firstOrNull()?.platform
                (if (platform == "wasm-wasi") PlatformKind.WASM_WASI else PlatformKind.WASM_JS) to null
            }
        }

        private fun dependencies(target: KargoTarget, platform: Pair<PlatformKind, String?>): List<DependencySpec> {
            val result = LinkedHashMap<String, DependencySpec>()
            fun add(spec: DependencySpec) {
                val key = spec.library ?: "module:${spec.module}"
                val existing = result[key]
                // Compile beats runtime beats test when a library is on several classpaths.
                if (existing == null || spec.scope.ordinal < existing.scope.ordinal) result[key] = spec
            }
            // A Native target built for several platforms is analyzed for the first.
            val entry = target.dependencies?.firstOrNull()
            if (entry != null) {
                val libKind = when (platform.first) {
                    PlatformKind.JVM -> LibraryKind.JVM
                    PlatformKind.NATIVE -> LibraryKind.NATIVE
                    PlatformKind.WASM_JS -> LibraryKind.WASM_JS
                    PlatformKind.WASM_WASI -> LibraryKind.WASM_WASI
                }
                fun scoped(list: List<KargoDependency>, scope: DependencyScope) {
                    for (library in group(list, libKind)) add(DependencySpec(library.name, null, scope))
                    for (local in list.mapNotNull { it.localPackage }.distinct()) {
                        val targets = targetsOf[local] ?: continue
                        if (target.kind in targets) add(DependencySpec(null, moduleName(local, target.kind, targets), scope))
                    }
                }
                scoped(entry.compile, DependencyScope.COMPILE)
                scoped(entry.runtime, DependencyScope.RUNTIME)
                scoped(entry.test, DependencyScope.TEST)
            }
            if (platform.first == PlatformKind.NATIVE) {
                for (library in nativeDistribution(platform.second)) add(DependencySpec(library.name, null, DependencyScope.COMPILE))
            }
            if (target.kind == "wasm") {
                for (npm in target.npm) {
                    // Installed by `kargo build` (or `kargo fetch`); until then there is no directory to describe.
                    val directory = npm.directory?.let(::norm)?.takeIf { files.exists(it) } ?: continue
                    val label = npm.version?.let { "${npm.name}@$it" } ?: npm.name
                    val spec = LibrarySpec("${ImportPlan.LIBRARY_PREFIX}npm $label", LibraryKind.NPM, listOf(directory), emptyList())
                    val library = libraries.putIfAbsent(spec.name, spec) ?: spec
                    add(DependencySpec(library.name, null, if (npm.test) DependencyScope.TEST else DependencyScope.RUNTIME))
                }
            }
            return result.values.toList()
        }

        /** The artifacts of `list` as libraries of the plan: one per module and version. */
        private fun group(list: List<KargoDependency>, kind: LibraryKind): List<LibrarySpec> {
            val grouped = LinkedHashMap<String, MutableList<KargoDependency>>()
            for (dependency in list) {
                if (dependency.module == null || dependency.path == null) continue
                grouped.getOrPut("${dependency.module}:${dependency.version}") { ArrayList() } += dependency
            }
            return grouped.map { (key, entries) ->
                // An artifact can come with more than one file (a Native library and its cinterop klib).
                val sources = entries.mapNotNull { it.sources }.map(::norm).distinct()
                val spec = LibrarySpec("${ImportPlan.LIBRARY_PREFIX}$key", kind, entries.mapNotNull { it.path }.map(::norm).distinct(), sources)
                libraries.putIfAbsent(spec.name, spec) ?: spec
            }
        }

        /** The standard library and the platform libraries of the Kotlin/Native distribution, one library per klib. */
        private fun nativeDistribution(nativeTarget: String?): List<LibrarySpec> {
            val home = metadata.nativeHome?.let(::norm) ?: return emptyList()
            val version = metadata.kotlinVersion ?: ""
            val klibs = ArrayList<Pair<String, String>>()
            if (files.exists("$home/klib/common/stdlib")) klibs += "Kotlin/Native $version stdlib" to "$home/klib/common/stdlib"
            if (nativeTarget != null) {
                for (name in files.subdirectories("$home/klib/platform/$nativeTarget")) {
                    klibs += "Kotlin/Native $version $nativeTarget ${name.removePrefix("org.jetbrains.kotlin.native.platform.")}" to "$home/klib/platform/$nativeTarget/$name"
                }
            }
            return klibs.map { (name, path) ->
                val spec = LibrarySpec("${ImportPlan.LIBRARY_PREFIX}$name", LibraryKind.NATIVE, listOf(path), emptyList())
                libraries.putIfAbsent(spec.name, spec) ?: spec
            }
        }
    }

    class Shared(val sources: List<String>, val tests: List<String>)

    private data class SplitArgs(val language: String?, val api: String?, val optIn: List<String>, val extra: List<String>)

    /** Separates the compiler arguments that have fields in the IDE's Kotlin settings from the rest. */
    private fun splitArgs(args: List<String>): SplitArgs {
        var language: String? = null
        var api: String? = null
        val optIn = ArrayList<String>()
        val extra = ArrayList<String>()
        for (arg in args) {
            when {
                arg.startsWith("-language-version=") -> language = arg.substringAfter('=')
                arg.startsWith("-api-version=") -> api = arg.substringAfter('=')
                arg.startsWith("-opt-in=") -> optIn += arg.substringAfter('=')
                else -> extra += arg
            }
        }
        return SplitArgs(language, api, optIn, extra)
    }
}

/** A path with forward slashes, the form the IDE's file system uses. */
fun norm(path: String): String = path.replace('\\', '/').trimEnd('/').ifEmpty { "/" }
