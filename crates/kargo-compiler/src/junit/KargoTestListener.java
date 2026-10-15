// A JUnit Platform listener that reports tests to Kargo as JSON lines on stdout,
// each prefixed with `##kargo `. The JUnit console launcher registers it through
// META-INF/services when its directory is on `--class-path`.
//
// With `-Dkargo.test.list=true` (and the launcher in dry-run mode) it prints one
// `test-discovered` line per test; otherwise `test-started` and `test-finished`
// lines as tests run. Kargo adds the package and target before passing them on.

import java.io.FileDescriptor;
import java.io.FileOutputStream;
import java.io.PrintStream;
import java.io.PrintWriter;
import java.io.StringWriter;
import java.nio.charset.StandardCharsets;
import java.util.Map;
import java.util.Optional;
import java.util.concurrent.ConcurrentHashMap;

import org.junit.platform.engine.TestExecutionResult;
import org.junit.platform.engine.TestSource;
import org.junit.platform.engine.reporting.ReportEntry;
import org.junit.platform.engine.support.descriptor.ClassSource;
import org.junit.platform.engine.support.descriptor.MethodSource;
import org.junit.platform.launcher.TestExecutionListener;
import org.junit.platform.launcher.TestIdentifier;
import org.junit.platform.launcher.TestPlan;

public final class KargoTestListener implements TestExecutionListener {
    /** The process's real stdout: JUnit's output capture replaces `System.out` while tests run. */
    private static final PrintStream OUT = new PrintStream(new FileOutputStream(FileDescriptor.out), true, StandardCharsets.UTF_8);

    private final boolean list = Boolean.getBoolean("kargo.test.list");
    private final Map<String, Long> started = new ConcurrentHashMap<>();
    private final Map<String, StringBuilder> stdout = new ConcurrentHashMap<>();
    private final Map<String, StringBuilder> stderr = new ConcurrentHashMap<>();
    private volatile TestPlan plan;

    @Override
    public void testPlanExecutionStarted(TestPlan testPlan) {
        plan = testPlan;
        if (!list) {
            return;
        }
        for (TestIdentifier root : testPlan.getRoots()) {
            discover(root);
        }
    }

    private void discover(TestIdentifier id) {
        // A parameterized test or test factory is a container until it runs; list it as one test.
        boolean template = id.isContainer() && id.getSource().orElse(null) instanceof MethodSource;
        if (id.isTest() || template) {
            emit(fields("test-discovered", id));
            if (template) {
                return;
            }
        }
        for (TestIdentifier child : plan.getChildren(id)) {
            discover(child);
        }
    }

    @Override
    public void testPlanExecutionFinished(TestPlan testPlan) {
        // Printed last: a run that ends without it stopped early, such as when a test called `System.exit`.
        if (!list) {
            emit(new StringBuilder("{\"reason\":\"run-finished\""));
        }
    }

    @Override
    public void executionStarted(TestIdentifier id) {
        if (list || !id.isTest()) {
            return;
        }
        started.put(id.getUniqueId(), System.nanoTime());
        emit(fields("test-started", id));
    }

    @Override
    public void executionSkipped(TestIdentifier id, String reason) {
        if (list) {
            return;
        }
        if (!id.isTest()) {
            // Report the tests of a skipped class one by one.
            for (TestIdentifier child : plan.getChildren(id)) {
                executionSkipped(child, reason);
            }
            return;
        }
        StringBuilder json = fields("test-finished", id);
        json.append(",\"outcome\":\"ignored\",\"duration-ms\":0");
        if (reason != null && !reason.isEmpty()) {
            json.append(",\"message\":").append(quote(reason));
        }
        emit(json);
    }

    @Override
    public void reportingEntryPublished(TestIdentifier id, ReportEntry entry) {
        // What a test printed, when the launcher captures output.
        for (Map.Entry<String, String> e : entry.getKeyValuePairs().entrySet()) {
            Map<String, StringBuilder> target = e.getKey().equals("stdout") ? stdout : e.getKey().equals("stderr") ? stderr : null;
            if (target != null) {
                target.computeIfAbsent(id.getUniqueId(), k -> new StringBuilder()).append(e.getValue());
            }
        }
    }

    @Override
    public void executionFinished(TestIdentifier id, TestExecutionResult result) {
        if (list) {
            return;
        }
        boolean failed = result.getStatus() != TestExecutionResult.Status.SUCCESSFUL;
        // A failing class (such as a failing `@BeforeAll`) is reported as a failed test too.
        if (!id.isTest() && !(failed && id.isContainer() && id.getSource().isPresent())) {
            return;
        }
        Long start = started.remove(id.getUniqueId());
        long millis = start == null ? 0 : (System.nanoTime() - start) / 1_000_000;
        StringBuilder json = fields("test-finished", id);
        String outcome;
        switch (result.getStatus()) {
            case SUCCESSFUL:
                outcome = "passed";
                break;
            case ABORTED:
                // A failed assumption: the test did not run to the end.
                outcome = "ignored";
                break;
            default:
                outcome = "failed";
        }
        json.append(",\"outcome\":").append(quote(outcome)).append(",\"duration-ms\":").append(millis);
        Optional<Throwable> thrown = result.getThrowable();
        if (thrown.isPresent()) {
            Throwable t = thrown.get();
            String message = t.getMessage() != null ? t.getMessage() : t.getClass().getName();
            StringWriter trace = new StringWriter();
            t.printStackTrace(new PrintWriter(trace));
            json.append(",\"message\":").append(quote(message)).append(",\"details\":").append(quote(trace.toString()));
        }
        StringBuilder out = stdout.remove(id.getUniqueId());
        if (out != null) {
            json.append(",\"stdout\":").append(quote(out.toString()));
        }
        StringBuilder err = stderr.remove(id.getUniqueId());
        if (err != null) {
            json.append(",\"stderr\":").append(quote(err.toString()));
        }
        emit(json);
    }

    /** The opening of a message with the fields that identify the test. */
    private StringBuilder fields(String reason, TestIdentifier id) {
        StringBuilder json = new StringBuilder("{\"reason\":").append(quote(reason));
        json.append(",\"id\":").append(quote(id.getUniqueId()));
        Optional<String> parent = id.getParentId();
        if (parent.isPresent()) {
            json.append(",\"parent-id\":").append(quote(parent.get()));
        }
        TestSource source = id.getSource().orElse(null);
        String name = id.getDisplayName();
        if (source instanceof MethodSource) {
            name = ((MethodSource) source).getMethodName();
        }
        json.append(",\"suite\":").append(quote(suite(id)));
        json.append(",\"name\":").append(quote(name));
        json.append(",\"display-name\":").append(quote(id.getDisplayName()));
        return json;
    }

    /** The class a test belongs to: its own source's, or the nearest enclosing class's. */
    private String suite(TestIdentifier id) {
        for (TestIdentifier current = id; current != null; ) {
            TestSource source = current.getSource().orElse(null);
            if (source instanceof MethodSource) {
                return ((MethodSource) source).getClassName();
            }
            if (source instanceof ClassSource) {
                return ((ClassSource) source).getClassName();
            }
            Optional<String> parent = current.getParentId();
            current = parent.isPresent() && plan != null ? plan.getTestIdentifier(parent.get()) : null;
        }
        return "";
    }

    private static void emit(StringBuilder json) {
        json.append('}');
        synchronized (OUT) {
            // Start on a fresh line in case a test printed without a newline.
            OUT.print("\n##kargo " + json + "\n");
            OUT.flush();
        }
    }

    private static String quote(String text) {
        StringBuilder out = new StringBuilder(text.length() + 2).append('"');
        for (int i = 0; i < text.length(); i++) {
            char c = text.charAt(i);
            switch (c) {
                case '"':
                    out.append("\\\"");
                    break;
                case '\\':
                    out.append("\\\\");
                    break;
                case '\n':
                    out.append("\\n");
                    break;
                case '\r':
                    out.append("\\r");
                    break;
                case '\t':
                    out.append("\\t");
                    break;
                default:
                    if (c < 0x20) {
                        out.append(String.format("\\u%04x", (int) c));
                    } else {
                        out.append(c);
                    }
            }
        }
        return out.append('"').toString();
    }
}
