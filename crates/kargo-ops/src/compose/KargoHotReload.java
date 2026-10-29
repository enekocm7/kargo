// The Java agent behind `kargo run --hot-reload`. Kargo builds it into a jar the first time it is
// needed and starts the program with `-javaagent:<jar>=<state directory>`. It works for any JVM
// program; a Compose Desktop app also gets its composition rebuilt.
//
// The state directory holds:
//   classes  the class directories to watch, one per line
//   request  a number Kargo writes after each successful build
//   result   "<number> ok <classes>" or "<number> restart <reason>", the agent's answer
//   agent.log what the agent did, for when a reload does not do what you expect
//
// On each new request the agent redefines the loaded classes whose class files changed. When the
// program uses Compose, that happens between Compose's HotReloader.saveStateAndDispose and
// loadStateAndCompose on the UI thread, so the composition is rebuilt with the new code and keeps
// its remembered state; any other program is redefined where the agent runs, and the code that
// runs next uses the new methods. Classes that are not
// loaded yet need nothing: the class loader reads the new file when they are first used. A change
// the JVM cannot apply in place (a new field or method without JetBrains Runtime) answers
// "restart", and Kargo starts the program again.
package kargo.hotreload;

import java.awt.EventQueue;
import java.io.IOException;
import java.lang.instrument.ClassDefinition;
import java.lang.instrument.Instrumentation;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.stream.Stream;

public final class KargoHotReload {
    /** Whether the program uses Compose, whose composition has to be saved and rebuilt around a reload. */
    private static final boolean COMPOSE = composePresent();

    private final Instrumentation instrumentation;
    private final Path state;
    private final List<Path> classDirs = new ArrayList<>();
    /** The size and modification time of every class file, as of the last reload. */
    private Map<Path, String> stamps = new HashMap<>();

    private KargoHotReload(Instrumentation instrumentation, Path state) throws IOException {
        this.instrumentation = instrumentation;
        this.state = state;
        for (String line : Files.readAllLines(state.resolve("classes"), StandardCharsets.UTF_8)) {
            if (!line.isBlank()) classDirs.add(Paths.get(line.trim()));
        }
        stamps = scan();
    }

    public static void premain(String args, Instrumentation instrumentation) throws IOException {
        KargoHotReload agent = new KargoHotReload(instrumentation, Paths.get(args));
        Thread thread = new Thread(agent::watch, "kargo-hot-reload");
        thread.setDaemon(true);
        thread.start();
    }

    private void watch() {
        String last = read("request");
        while (true) {
            try {
                Thread.sleep(100);
            } catch (InterruptedException e) {
                return;
            }
            String request = read("request");
            if (request.isEmpty() || request.equals(last)) continue;
            last = request;
            log("request " + request);
            String answer;
            try {
                answer = "ok " + reload();
            } catch (Throwable t) {
                String reason = t.getMessage() == null ? t.getClass().getName() : t.getClass().getSimpleName() + ": " + t.getMessage();
                answer = "restart " + reason.replace('\n', ' ');
            }
            log("answer " + answer);
            write("result", request + " " + answer);
        }
    }

    /** Redefines the loaded classes whose files changed and recomposes; returns how many were redefined. */
    private int reload() throws Exception {
        Map<Path, String> now = scan();
        Map<String, Path> changed = new HashMap<>();
        for (Map.Entry<Path, String> entry : now.entrySet()) {
            if (!entry.getValue().equals(stamps.get(entry.getKey()))) {
                changed.put(className(entry.getKey()), entry.getKey());
            }
        }
        List<ClassDefinition> definitions = new ArrayList<>();
        for (Class<?> loaded : instrumentation.getAllLoadedClasses()) {
            Path file = changed.get(loaded.getName());
            if (file != null && instrumentation.isModifiableClass(loaded)) {
                definitions.add(new ClassDefinition(loaded, Files.readAllBytes(file)));
            }
        }
        stamps = now;
        log(changed.size() + " class files changed, " + definitions.size() + " of them loaded");
        if (definitions.isEmpty()) return 0;
        Throwable[] failure = new Throwable[1];
        Runnable swap = () -> {
            Object token = null;
            try {
                if (COMPOSE) {
                    token = hotReloader("saveStateAndDispose", new Object());
                    log("saved the composition state");
                }
                instrumentation.redefineClasses(definitions.toArray(new ClassDefinition[0]));
                log("redefined the classes");
            } catch (Throwable t) {
                failure[0] = t;
            } finally {
                if (token != null) {
                    try {
                        hotReloader("loadStateAndCompose", token);
                    } catch (Throwable t) {
                        if (failure[0] == null) failure[0] = t;
                    }
                }
            }
        };
        if (!COMPOSE || EventQueue.isDispatchThread() || java.awt.GraphicsEnvironment.isHeadless()) {
            swap.run();
        } else {
            EventQueue.invokeAndWait(swap);
        }
        if (failure[0] != null) {
            Throwable t = failure[0];
            if (t instanceof java.lang.reflect.InvocationTargetException && t.getCause() != null) t = t.getCause();
            throw t instanceof Exception ? (Exception) t : new RuntimeException(t);
        }
        return definitions.size();
    }

    private static boolean composePresent() {
        try {
            Class.forName("androidx.compose.runtime.HotReloader", false, ClassLoader.getSystemClassLoader());
            return true;
        } catch (ClassNotFoundException | LinkageError e) {
            return false;
        }
    }

    /** Calls a private function of Compose's `HotReloader.Companion`, which IDE tooling calls the same way. */
    private static Object hotReloader(String name, Object argument) throws Exception {
        Class<?> reloader = Class.forName("androidx.compose.runtime.HotReloader", true, ClassLoader.getSystemClassLoader());
        Field field = reloader.getDeclaredField("Companion");
        field.setAccessible(true);
        Object companion = field.get(null);
        Method method = companion.getClass().getDeclaredMethod(name, Object.class);
        method.setAccessible(true);
        return method.invoke(companion, argument);
    }

    private Map<Path, String> scan() throws IOException {
        Map<Path, String> out = new HashMap<>();
        for (Path dir : classDirs) {
            if (!Files.isDirectory(dir)) continue;
            try (Stream<Path> files = Files.walk(dir)) {
                for (Path file : (Iterable<Path>) files.filter(f -> f.toString().endsWith(".class"))::iterator) {
                    out.put(file, Files.size(file) + "@" + Files.getLastModifiedTime(file).toMillis());
                }
            }
        }
        return out;
    }

    private String className(Path file) {
        for (Path dir : classDirs) {
            if (file.startsWith(dir)) {
                String relative = dir.relativize(file).toString().replace(file.getFileSystem().getSeparator(), ".");
                return relative.substring(0, relative.length() - ".class".length());
            }
        }
        return "";
    }

    private void log(String line) {
        try {
            byte[] bytes = (java.time.LocalTime.now() + " " + line + System.lineSeparator()).getBytes(StandardCharsets.UTF_8);
            Files.write(state.resolve("agent.log"), bytes, java.nio.file.StandardOpenOption.CREATE, java.nio.file.StandardOpenOption.APPEND);
        } catch (IOException ignored) {
            // The log is only for people.
        }
    }

    private String read(String name) {
        try {
            return new String(Files.readAllBytes(state.resolve(name)), StandardCharsets.UTF_8).trim();
        } catch (IOException e) {
            return "";
        }
    }

    private void write(String name, String text) {
        try {
            Path temp = state.resolve(name + ".tmp");
            Files.write(temp, text.getBytes(StandardCharsets.UTF_8));
            Files.move(temp, state.resolve(name), java.nio.file.StandardCopyOption.REPLACE_EXISTING);
        } catch (IOException ignored) {
            // Kargo times out and restarts the program.
        }
    }
}
