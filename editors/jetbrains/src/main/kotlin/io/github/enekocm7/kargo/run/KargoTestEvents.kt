// The test tree for `kargo test`: Kargo's `--message-format json` messages become the IDE's
// test events, and test names lead back to their sources.

package io.github.enekocm7.kargo.run

import com.google.gson.JsonObject
import com.google.gson.JsonParser
import com.intellij.execution.Executor
import com.intellij.execution.Location
import com.intellij.execution.PsiLocation
import com.intellij.execution.testframework.TestConsoleProperties
import com.intellij.execution.testframework.sm.SMCustomMessagesParsing
import com.intellij.execution.testframework.sm.runner.OutputToGeneralTestEventsConverter
import com.intellij.execution.testframework.sm.runner.SMTRunnerConsoleProperties
import com.intellij.execution.testframework.sm.runner.SMTestLocator
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.Key
import com.intellij.psi.PsiElement
import com.intellij.psi.PsiManager
import com.intellij.psi.search.FilenameIndex
import com.intellij.psi.search.GlobalSearchScope
import com.intellij.execution.testframework.sm.ServiceMessageBuilder
import jetbrains.buildServer.messages.serviceMessages.ServiceMessageVisitor

class KargoTestConsoleProperties(configuration: KargoRunConfiguration, executor: Executor) :
    SMTRunnerConsoleProperties(configuration, "Kargo", executor), SMCustomMessagesParsing {

    init {
        isIdBasedTestTree = true
    }

    override fun createTestEventsConverter(testFrameworkName: String, consoleProperties: TestConsoleProperties): OutputToGeneralTestEventsConverter =
        KargoTestEventsConverter(testFrameworkName, consoleProperties)

    override fun getTestLocator(): SMTestLocator = KargoTestLocator
}

/**
 * Translates Kargo's JSON lines into TeamCity service messages for the IDE's test runner. Kargo
 * reports tests, not classes, so a class node is opened at its first test and closed when its
 * target's `test-summary` arrives. Node ids carry the package and target, since one run can test
 * several of both.
 */
class KargoTestEventsConverter(testFrameworkName: String, consoleProperties: TestConsoleProperties) :
    OutputToGeneralTestEventsConverter(testFrameworkName, consoleProperties) {

    private val translator = KargoTestTranslator()

    override fun processServiceMessages(text: String, outputType: Key<*>, visitor: ServiceMessageVisitor): Boolean {
        val messages = translator.translate(text) ?: return super.processServiceMessages(text, outputType, visitor)
        for (message in messages) super.processServiceMessages(message, outputType, visitor)
        return true
    }
}

/** The part of [KargoTestEventsConverter] that doesn't need the IDE, so it can be unit tested. */
class KargoTestTranslator {
    /** Open class nodes per `package/target`, by node id. */
    private val openSuites = mutableMapOf<String, MutableSet<String>>()
    private val startedTests = mutableSetOf<String>()

    /** The service messages for one line of output; `null` when it isn't one of Kargo's JSON messages. */
    fun translate(line: String): List<String>? {
        val trimmed = line.trim()
        if (!trimmed.startsWith("{")) return null
        val json = runCatching { JsonParser.parseString(trimmed).asJsonObject }.getOrNull() ?: return null
        return when (json.string("reason")) {
            "test-started" -> started(json)
            "test-finished" -> finished(json)
            "test-summary" -> summary(json)
            "compiler-message" -> listOfNotNull(json.string("rendered")?.let { output(it + "\n") })
            "build-finished", "test-discovered" -> emptyList()
            else -> null
        }
    }

    private fun scope(json: JsonObject) = "${json.string("package") ?: ""}/${json.string("target") ?: ""}"

    private fun testId(json: JsonObject) = "T:${scope(json)}:${json.string("id") ?: "${json.string("suite")}.${json.string("name")}"}"

    /** Opens the node of the test's class (or parameterized test) if needed, and returns its id. */
    private fun suite(json: JsonObject, messages: MutableList<String>): String {
        val suite = json.string("suite") ?: "tests"
        val parent = json.string("parent-id") ?: suite
        val id = "S:${scope(json)}:$parent"
        if (openSuites.getOrPut(scope(json)) { linkedSetOf() }.add(id)) {
            val simple = suite.substringAfterLast('.').replace('$', '.')
            val isClass = json.string("parent-id")?.let { it.endsWith("[class:$suite]") || it.endsWith("[nested-class:${suite.substringAfterLast('$')}]") } ?: true
            val name = if (isClass) simple else "$simple.${json.string("name")}"
            val target = json.string("target")
            messages += ServiceMessageBuilder("testSuiteStarted")
                .addAttribute("name", if (target != null && target != "jvm") "$name ($target)" else name)
                .addAttribute("nodeId", id)
                .addAttribute("parentNodeId", "0")
                .addAttribute("locationHint", "${KargoTestLocator.PROTOCOL}://$suite")
                .toString()
        }
        return id
    }

    private fun started(json: JsonObject): List<String> {
        val messages = mutableListOf<String>()
        val parent = suite(json, messages)
        val id = testId(json)
        if (startedTests.add(id)) messages += testStarted(json, id, parent)
        return messages
    }

    private fun testStarted(json: JsonObject, id: String, parent: String): String =
        ServiceMessageBuilder.testStarted(json.string("display-name") ?: json.string("name") ?: "test")
            .addAttribute("nodeId", id)
            .addAttribute("parentNodeId", parent)
            .addAttribute("locationHint", "${KargoTestLocator.PROTOCOL}://${json.string("suite")}/${json.string("name")}")
            .toString()

    private fun finished(json: JsonObject): List<String> {
        val messages = mutableListOf<String>()
        val parent = suite(json, messages)
        val id = testId(json)
        val name = json.string("display-name") ?: json.string("name") ?: "test"
        if (startedTests.add(id)) messages += testStarted(json, id, parent)
        json.string("stdout")?.let { messages += ServiceMessageBuilder("testStdOut").addAttribute("nodeId", id).addAttribute("out", it).toString() }
        json.string("stderr")?.let { messages += ServiceMessageBuilder("testStdErr").addAttribute("nodeId", id).addAttribute("out", it).toString() }
        when (json.string("outcome")) {
            "failed" -> messages += ServiceMessageBuilder.testFailed(name)
                .addAttribute("nodeId", id)
                .addAttribute("message", json.string("message") ?: "")
                .addAttribute("details", json.string("details") ?: "")
                .toString()
            "ignored" -> messages += ServiceMessageBuilder.testIgnored(name)
                .addAttribute("nodeId", id)
                .addAttribute("message", json.string("message") ?: "")
                .toString()
        }
        val finished = ServiceMessageBuilder.testFinished(name).addAttribute("nodeId", id)
        json.get("duration-ms")?.takeIf { it.isJsonPrimitive }?.let { finished.addAttribute("duration", it.asLong.toString()) }
        messages += finished.toString()
        return messages
    }

    private fun summary(json: JsonObject): List<String> = openSuites.remove(scope(json)).orEmpty().map { id ->
        ServiceMessageBuilder("testSuiteFinished").addAttribute("nodeId", id).toString()
    }

    private fun output(text: String) = ServiceMessageBuilder("message").addAttribute("text", text).addAttribute("status", "NORMAL").toString()

    private fun JsonObject.string(key: String): String? = get(key)?.takeIf { it.isJsonPrimitive }?.asString
}

/**
 * Finds a test from its `kargo://package.Class[/method]` location hint. Kargo's sources are not in
 * the IDE's Kotlin indexes, so this looks for the class declaration in the project's `.kt` files.
 */
object KargoTestLocator : SMTestLocator {
    const val PROTOCOL = "kargo"

    override fun getLocation(protocol: String, path: String, project: Project, scope: GlobalSearchScope): List<Location<*>> {
        if (protocol != PROTOCOL) return emptyList()
        val element = find(project, path) ?: return emptyList()
        return listOf(PsiLocation(element))
    }

    private fun find(project: Project, path: String): PsiElement? {
        val suite = path.substringBefore('/')
        val method = path.substringAfter('/', "").takeIf { it.isNotEmpty() }
        val packageName = suite.substringBeforeLast('.', "")
        val className = suite.substringAfterLast('.').substringAfterLast('$')
        val classPattern = Regex("""\b(?:class|object)\s+${Regex.escape(className)}\b""")
        val packagePattern = Regex("""^\s*package\s+([\w.]+)""", RegexOption.MULTILINE)
        val psiManager = PsiManager.getInstance(project)
        for (file in FilenameIndex.getAllFilesByExt(project, "kt", GlobalSearchScope.projectScope(project))) {
            val text = psiManager.findFile(file)?.text ?: continue
            if ((packagePattern.find(text)?.groupValues?.get(1) ?: "") != packageName) continue
            val classMatch = classPattern.find(text) ?: continue
            val offset = method?.let { Regex("""\bfun\s+(?:`${Regex.escape(it)}`|${Regex.escape(it)})\b""").find(text, classMatch.range.first)?.range?.first }
                ?: classMatch.range.first
            return psiManager.findFile(file)?.findElementAt(offset)
        }
        return null
    }
}
