// The parts of the import that talk to the IDE's Kotlin plugin: library kinds and the Kotlin facet that tells
// the Kotlin analysis which platform a module is and how it is compiled. They are not public API of the Kotlin
// plugin, so everything that touches them lives here, and the importer survives them failing.

package io.github.enekocm7.kargo.project

import com.intellij.facet.FacetManager
import com.intellij.openapi.module.Module
import com.intellij.openapi.roots.libraries.PersistentLibraryKind
import org.jetbrains.kotlin.cli.common.arguments.K2JVMCompilerArguments
import org.jetbrains.kotlin.config.CompilerSettings
import org.jetbrains.kotlin.idea.base.platforms.KotlinNativeLibraryKind
import org.jetbrains.kotlin.idea.base.platforms.KotlinWasmJsLibraryKind
import org.jetbrains.kotlin.idea.base.platforms.KotlinWasmWasiLibraryKind
import org.jetbrains.kotlin.idea.facet.KotlinFacet
import org.jetbrains.kotlin.idea.facet.KotlinFacetType
import org.jetbrains.kotlin.idea.facet.initializeIfNeeded
import org.jetbrains.kotlin.platform.TargetPlatform
import org.jetbrains.kotlin.platform.jvm.JvmPlatforms
import org.jetbrains.kotlin.platform.konan.NativePlatforms
import org.jetbrains.kotlin.platform.wasm.WasmPlatforms

object KotlinSupport {
    /** The library kind the Kotlin plugin analyzes a library of `kind` with; JVM libraries need none. */
    fun libraryKind(kind: LibraryKind): PersistentLibraryKind<*>? = when (kind) {
        LibraryKind.JVM -> null
        LibraryKind.NATIVE -> KotlinNativeLibraryKind
        LibraryKind.WASM_JS -> KotlinWasmJsLibraryKind
        LibraryKind.WASM_WASI -> KotlinWasmWasiLibraryKind
    }

    fun targetPlatform(spec: ModuleSpec): TargetPlatform = when (spec.platform) {
        PlatformKind.JVM -> JvmPlatforms.defaultJvmPlatform
        PlatformKind.NATIVE -> spec.nativeTarget?.let { NativePlatforms.nativePlatformByTargetNames(listOf(it)) } ?: NativePlatforms.unspecifiedNativePlatform
        PlatformKind.WASM_JS -> WasmPlatforms.wasmJs
        PlatformKind.WASM_WASI -> WasmPlatforms.wasmWasi
    }

    /** Gives `module` a Kotlin facet for `spec`, replacing the settings of the one it has. */
    fun configureFacet(module: Module, spec: ModuleSpec) {
        val manager = FacetManager.getInstance(module)
        val model = manager.createModifiableModel()
        var facet = KotlinFacet.get(module)
        if (facet == null) {
            facet = manager.createFacet(KotlinFacetType.INSTANCE, "Kotlin", null) as KotlinFacet
            model.addFacet(facet)
        }
        val settings = facet.configuration.settings
        val platform = targetPlatform(spec)
        settings.useProjectSettings = false
        settings.initializeIfNeeded(module, null, platform)
        settings.targetPlatform = platform
        settings.dependsOnModuleNames = spec.dependsOn
        settings.isHmppEnabled = spec.dependsOn.isNotEmpty()
        settings.compilerSettings = CompilerSettings().apply { additionalArguments = spec.extraArgs.joinToString(" ") { quote(it) } }
        settings.compilerArguments?.let { arguments ->
            arguments.languageVersion = spec.languageVersion
            arguments.apiVersion = spec.apiVersion
            arguments.multiPlatform = spec.multiplatform
            // Empty arrays, not null: the argument classes of newer Kotlin plugins don't take null.
            arguments.optIn = spec.optIn.toTypedArray()
            arguments.pluginClasspaths = spec.pluginClasspath.toTypedArray()
            arguments.pluginOptions = spec.pluginOptions.toTypedArray()
            if (arguments is K2JVMCompilerArguments) arguments.jvmTarget = spec.jvmTarget
        }
        settings.updateMergedArguments()
        model.commit()
    }

    private fun quote(arg: String) = if (arg.any { it.isWhitespace() }) "\"$arg\"" else arg
}
