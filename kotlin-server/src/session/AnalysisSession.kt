// A Kotlin Analysis API session in standalone mode, built from the project model: one source
// module per model module, one library module per library, and the JDK.
//
// This and `model` are the only places that touch the Analysis API's project structure, which
// changes between Kotlin versions.
package io.github.enekocm7.kargo.server.session

import com.intellij.openapi.Disposable
import com.intellij.openapi.util.Disposer
import io.github.enekocm7.kargo.server.documents.Document
import io.github.enekocm7.kargo.server.documents.normalize
import io.github.enekocm7.kargo.server.model.CompilerSettings
import io.github.enekocm7.kargo.server.model.Module
import io.github.enekocm7.kargo.server.model.ProjectModel
import org.jetbrains.kotlin.analysis.api.KaSession
import org.jetbrains.kotlin.analysis.api.analyze
import org.jetbrains.kotlin.analysis.api.analyzeCopy
import org.jetbrains.kotlin.analysis.api.projectStructure.KaDanglingFileResolutionMode
import org.jetbrains.kotlin.analysis.api.projectStructure.KaLibraryModule
import org.jetbrains.kotlin.analysis.api.projectStructure.KaModule
import org.jetbrains.kotlin.analysis.api.projectStructure.KaSourceModule
import org.jetbrains.kotlin.analysis.api.standalone.StandaloneAnalysisAPISession
import org.jetbrains.kotlin.analysis.api.standalone.buildStandaloneAnalysisAPISession
import org.jetbrains.kotlin.analysis.project.structure.builder.KtModuleProviderBuilder
import org.jetbrains.kotlin.analysis.project.structure.builder.buildKtLibraryModule
import org.jetbrains.kotlin.analysis.project.structure.builder.buildKtSdkModule
import org.jetbrains.kotlin.analysis.project.structure.builder.buildKtSourceModule
import org.jetbrains.kotlin.config.AnalysisFlags
import org.jetbrains.kotlin.config.ApiVersion
import org.jetbrains.kotlin.config.JvmTarget
import org.jetbrains.kotlin.config.LanguageFeature
import org.jetbrains.kotlin.config.LanguageVersion
import org.jetbrains.kotlin.config.LanguageVersionSettings
import org.jetbrains.kotlin.config.LanguageVersionSettingsImpl
import org.jetbrains.kotlin.platform.TargetPlatform
import org.jetbrains.kotlin.platform.jvm.JvmPlatforms
import org.jetbrains.kotlin.platform.konan.NativePlatforms
import org.jetbrains.kotlin.platform.wasm.WasmPlatforms
import org.jetbrains.kotlin.psi.KtFile
import org.jetbrains.kotlin.psi.KtPsiFactory
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths

class AnalysisSession private constructor(
    val model: ProjectModel,
    private val session: StandaloneAnalysisAPISession,
    private val disposable: Disposable,
) : AutoCloseable {
    /** The project's Kotlin files by path. */
    private val files: Map<Path, KtFile> = session.modulesWithFiles.values
        .flatten()
        .filterIsInstance<KtFile>()
        .mapNotNull { file -> file.virtualFile?.path?.let { normalize(Paths.get(it)) to file } }
        .toMap()

    val fileCount: Int get() = files.size

    fun file(path: Path): KtFile? = files[path]

    /**
     * Runs [action] on the open document's text: on the file itself when it is unchanged, else on
     * a copy that stands in for it. `null` when the document is in no module.
     */
    fun <R> analyzeDocument(document: Document, action: KaSession.(KtFile) -> R): R? {
        val original = document.path?.let(files::get) ?: return null
        if (original.text == document.text) {
            return analyze(original) { action(original) }
        }
        val copy = KtPsiFactory.contextual(original).createFile(original.name, document.text)
        copy.originalFile = original
        return analyzeCopy(copy, KaDanglingFileResolutionMode.PREFER_SELF) { action(copy) }
    }

    override fun close() {
        Disposer.dispose(disposable)
    }

    companion object {
        fun build(model: ProjectModel): AnalysisSession {
            val disposable = Disposer.newDisposable("kargo-kotlin-server")
            try {
                val session = buildStandaloneAnalysisAPISession(disposable) {
                    buildKtModuleProvider {
                        platform = JvmPlatforms.defaultJvmPlatform
                        ModuleGraph(model, this).addAll()
                    }
                }
                return AnalysisSession(model, session, disposable)
            } catch (e: Throwable) {
                Disposer.dispose(disposable)
                throw e
            }
        }
    }
}

/** Adds the model's modules to a module provider, each after the modules it depends on. */
private class ModuleGraph(private val model: ProjectModel, private val provider: KtModuleProviderBuilder) {
    private val libraries = HashMap<String, KaLibraryModule>()
    private val modules = HashMap<String, KaSourceModule>()
    private val building = HashSet<String>()

    private val sdk: KaLibraryModule? = model.jdk?.let { jdk ->
        provider.addModule(
            provider.buildKtSdkModule {
                platform = JvmPlatforms.defaultJvmPlatform
                libraryName = "JDK ${jdk.version ?: ""}".trim()
                addBinaryRootsFromJdkHome(jdk.home, false)
            },
        )
    }

    fun addAll() {
        model.modules.forEach { module(it.id) }
    }

    private fun library(id: String): KaLibraryModule? {
        libraries[id]?.let { return it }
        val library = model.library(id)?.takeIf { Files.exists(it.path) } ?: return null
        val module = provider.addModule(
            provider.buildKtLibraryModule {
                platform = platformOf(listOf(library.platform), null)
                libraryName = library.id
                addBinaryRoot(library.path)
            },
        )
        libraries[id] = module
        return module
    }

    /** The source module for a model module, built once; `null` for a cycle or an unknown id. */
    private fun module(id: String): KaSourceModule? {
        modules[id]?.let { return it }
        val module = model.module(id) ?: return null
        if (!building.add(id)) return null
        val dependsOn = module.dependsOn.mapNotNull(::module)
        val friends = module.friends.mapNotNull(::module)
        val regular = module.moduleIds.mapNotNull(::module)
        val libraries = module.libraryIds.mapNotNull(::library)
        val targetPlatform = platformOf(module.platforms, module.compiler.jvmTarget)
        val built = provider.addModule(
            provider.buildKtSourceModule {
                moduleName = module.id
                platform = targetPlatform
                languageVersionSettings = languageSettings(module.compiler)
                addSourceRoots(module.sourceRoots.filter(Files::isDirectory))
                dependsOn.forEach(::addDependsOnDependency)
                friends.forEach(::addFriendDependency)
                (regular + libraries).forEach(::addRegularDependency)
                if (module.analyzesJvm() && sdk != null) addRegularDependency(sdk)
            },
        )
        modules[id] = built
        building.remove(id)
        return built
    }

    private fun Module.analyzesJvm(): Boolean = platforms.isEmpty() || platforms.any { it == "jvm" }
}

/** The platform of a module or library from the model's platform names. */
internal fun platformOf(names: List<String>, jvmTarget: String?): TargetPlatform {
    val simple = names.flatMap { name ->
        when {
            name == "jvm" -> (jvmTarget?.let(JvmTarget::fromString)?.let(JvmPlatforms::jvmPlatformByTargetVersion) ?: JvmPlatforms.defaultJvmPlatform)
            name == "wasm-js" -> WasmPlatforms.wasmJs
            name == "wasm-wasi" -> WasmPlatforms.wasmWasi
            name == "native" -> NativePlatforms.unspecifiedNativePlatform
            name.startsWith("native:") -> NativePlatforms.nativePlatformByTargetNames(listOf(name.removePrefix("native:")))
            else -> JvmPlatforms.defaultJvmPlatform
        }.componentPlatforms
    }.toSet()
    return if (simple.isEmpty()) JvmPlatforms.defaultJvmPlatform else TargetPlatform(simple)
}

/** The language version, API version, opt-ins and features of a module. */
internal fun languageSettings(settings: CompilerSettings): LanguageVersionSettings {
    val language = settings.languageVersion?.let(LanguageVersion::fromVersionString) ?: LanguageVersion.LATEST_STABLE
    val api = settings.apiVersion?.let(ApiVersion::parse) ?: ApiVersion.createByLanguageVersion(language)
    val features = if (settings.multiplatform) mapOf(LanguageFeature.MultiPlatformProjects to LanguageFeature.State.ENABLED) else emptyMap()
    return LanguageVersionSettingsImpl(language, api, mapOf(AnalysisFlags.optIn to settings.optIn), features)
}
