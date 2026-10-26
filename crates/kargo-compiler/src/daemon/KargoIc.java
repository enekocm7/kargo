// Incremental Kotlin/JVM compilation through the Kotlin Build Tools API, run
// inside the Kargo compiler daemon. Kargo compiles this file against the
// `kotlin-build-tools-api` jar of the project's Kotlin version and puts it on
// the daemon's classpath next to `kotlin-build-tools-impl`.
//
// Arguments (from the daemon's `kotlin-ic` command):
//   --working-dir <dir>   incremental caches of this compilation unit
//   --dest <dir>          output directory
//   --root <dir>          project root, optional (makes caches relocatable)
//   --build-dir <dir>     module build directory, optional
//   --source <file>       every source file (repeated)
//   --modified <file>     sources changed since the last successful build (repeated)
//   --removed <file>      sources removed since then (repeated)
//   --unknown-changes     rebuild everything and start new caches
//   --snapshot <entry> <file> <class|member>
//                         classpath entry and its snapshot, created when missing (repeated)
//   --                    the rest are compiler arguments

import java.io.File;
import java.io.PrintStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.nio.file.StandardCopyOption;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.HashSet;
import java.util.List;
import java.util.Set;

import org.jetbrains.kotlin.buildtools.api.BaseCompilationOperation;
import org.jetbrains.kotlin.buildtools.api.CompilationResult;
import org.jetbrains.kotlin.buildtools.api.CompilerMessageRenderer;
import org.jetbrains.kotlin.buildtools.api.KotlinLogger;
import org.jetbrains.kotlin.buildtools.api.KotlinToolchains;
import org.jetbrains.kotlin.buildtools.api.SourcesChanges;
import org.jetbrains.kotlin.buildtools.api.jvm.ClassSnapshotGranularity;
import org.jetbrains.kotlin.buildtools.api.jvm.ClasspathEntrySnapshot;
import org.jetbrains.kotlin.buildtools.api.jvm.JvmPlatformToolchain;
import org.jetbrains.kotlin.buildtools.api.jvm.JvmSnapshotBasedIncrementalCompilationConfiguration;
import org.jetbrains.kotlin.buildtools.api.jvm.operations.JvmClasspathSnapshottingOperation;
import org.jetbrains.kotlin.buildtools.api.jvm.operations.JvmCompilationOperation;

public final class KargoIc {
    private static volatile KotlinToolchains toolchains;

    private static KotlinToolchains toolchains() {
        if (toolchains == null) {
            synchronized (KargoIc.class) {
                if (toolchains == null) {
                    toolchains = KotlinToolchains.loadImplementation(KargoIc.class.getClassLoader());
                }
            }
        }
        return toolchains;
    }

    /**
     * Runs one incremental compilation; returns a process-style exit code. Compiler messages go to
     * `out`, the incremental compiler's own failures (it falls back to a full rebuild) to `log`.
     */
    public static int run(String[] args, PrintStream out, PrintStream log) throws Exception {
        Path workingDir = null;
        Path dest = null;
        Path root = null;
        Path buildDir = null;
        List<Path> sources = new ArrayList<>();
        List<File> modified = new ArrayList<>();
        List<File> removed = new ArrayList<>();
        boolean unknown = false;
        List<String[]> snapshots = new ArrayList<>();
        List<String> compilerArgs = new ArrayList<>();
        for (int i = 0; i < args.length; i++) {
            switch (args[i]) {
                case "--working-dir": workingDir = Paths.get(args[++i]); break;
                case "--dest": dest = Paths.get(args[++i]); break;
                case "--root": root = Paths.get(args[++i]); break;
                case "--build-dir": buildDir = Paths.get(args[++i]); break;
                case "--source": sources.add(Paths.get(args[++i])); break;
                case "--modified": modified.add(new File(args[++i])); break;
                case "--removed": removed.add(new File(args[++i])); break;
                case "--unknown-changes": unknown = true; break;
                case "--snapshot": snapshots.add(new String[] {args[++i], args[++i], args[++i]}); break;
                case "--": compilerArgs.addAll(Arrays.asList(args).subList(i + 1, args.length)); i = args.length; break;
                default: throw new IllegalArgumentException("unknown argument " + args[i]);
            }
        }

        if (unknown) {
            // New caches. The daemon holds this unit's lock, so no earlier compilation still has them open.
            deleteRecursively(workingDir);
        }

        KotlinToolchains kotlin = toolchains();
        JvmPlatformToolchain jvm = JvmPlatformToolchain.from(kotlin);
        Logger logger = new Logger(out, log);
        int code;
        try (KotlinToolchains.BuildSession session = kotlin.createBuildSession()) {
            List<Path> snapshotFiles = new ArrayList<>();
            for (String[] s : snapshots) {
                Path file = Paths.get(s[1]);
                if (!Files.isRegularFile(file)) {
                    JvmClasspathSnapshottingOperation.Builder op = jvm.classpathSnapshottingOperationBuilder(Paths.get(s[0]));
                    op.set(JvmClasspathSnapshottingOperation.GRANULARITY,
                            s[2].equals("member") ? ClassSnapshotGranularity.CLASS_MEMBER_LEVEL : ClassSnapshotGranularity.CLASS_LEVEL);
                    ClasspathEntrySnapshot snapshot = session.executeOperation(op.build(), kotlin.createInProcessExecutionPolicy(), logger);
                    Files.createDirectories(file.getParent());
                    Path tmp = file.resolveSibling(file.getFileName() + "." + Thread.currentThread().getId() + ".tmp");
                    snapshot.saveSnapshot(tmp);
                    Files.move(tmp, file, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE);
                }
                snapshotFiles.add(file);
            }

            JvmCompilationOperation.Builder op = jvm.jvmCompilationOperationBuilder(sources, dest);
            op.getCompilerArguments().applyArgumentStrings(compilerArgs);
            SourcesChanges changes = unknown ? SourcesChanges.Unknown.INSTANCE : new SourcesChanges.Known(modified, removed);
            JvmSnapshotBasedIncrementalCompilationConfiguration.Builder ic =
                    op.snapshotBasedIcConfigurationBuilder(workingDir, changes, snapshotFiles, workingDir.resolve("shrunk-classpath-snapshot.bin"));
            if (root != null) {
                ic.set(JvmSnapshotBasedIncrementalCompilationConfiguration.ROOT_PROJECT_DIR, root);
            }
            if (buildDir != null) {
                ic.set(JvmSnapshotBasedIncrementalCompilationConfiguration.MODULE_BUILD_DIR, buildDir);
            }
            Set<Path> outputs = new HashSet<>(Arrays.asList(dest, workingDir));
            ic.set(JvmSnapshotBasedIncrementalCompilationConfiguration.OUTPUT_DIRS, outputs);
            ic.set(JvmSnapshotBasedIncrementalCompilationConfiguration.BACKUP_CLASSES, true);
            ic.set(JvmSnapshotBasedIncrementalCompilationConfiguration.KEEP_IC_CACHES_IN_MEMORY, true);
            op.set(JvmCompilationOperation.INCREMENTAL_COMPILATION, ic.build());
            op.set(BaseCompilationOperation.COMPILER_MESSAGE_RENDERER, new PlainRenderer());

            CompilationResult result = session.executeOperation(op.build(), kotlin.createInProcessExecutionPolicy(), logger);
            switch (result) {
                case COMPILATION_SUCCESS: code = 0; break;
                case COMPILATION_ERROR: code = 1; break;
                case COMPILATION_OOM_ERROR: throw new OutOfMemoryError("the Kotlin compiler ran out of memory");
                default: code = 2; break;
            }
        }
        if (logger.storageConflict) {
            // The caches of an earlier compilation are still registered in this JVM, and every later
            // one would fail the same way; the daemon retires on this message.
            throw new IllegalStateException("incremental compilation fell back: a storage is already registered");
        }
        return code;
    }

    static void deleteRecursively(Path dir) throws java.io.IOException {
        if (!Files.exists(dir)) {
            return;
        }
        try (java.util.stream.Stream<Path> walk = Files.walk(dir)) {
            List<Path> paths = new ArrayList<>();
            walk.forEach(paths::add);
            for (int i = paths.size() - 1; i >= 0; i--) {
                Files.deleteIfExists(paths.get(i));
            }
        }
    }

    /** The format of the standalone compiler: `path:line:column: severity: message`, the line and a caret. */
    static final class PlainRenderer implements CompilerMessageRenderer {
        @Override
        public String render(Severity severity, String message, SourceLocation location) {
            String level = severity.name().toLowerCase();
            // Like the compiler's own renderer: "Unresolved reference" -> "unresolved reference", "JVM ..." stays.
            if (message.length() > 1 && Character.isUpperCase(message.charAt(0)) && !Character.isUpperCase(message.charAt(1))) {
                message = Character.toLowerCase(message.charAt(0)) + message.substring(1);
            }
            if (location == null || location.getPath() == null || location.getPath().isEmpty()) {
                return level + ": " + message;
            }
            StringBuilder sb = new StringBuilder(location.getPath());
            if (location.getLine() > 0) {
                sb.append(':').append(location.getLine());
                if (location.getColumn() > 0) {
                    sb.append(':').append(location.getColumn());
                }
            }
            sb.append(": ").append(level).append(": ").append(message);
            String content = location.getLineContent();
            if (content != null && !content.isEmpty() && location.getColumn() > 0 && location.getColumn() <= content.length() + 1) {
                sb.append('\n').append(content).append('\n');
                for (int i = 0; i < location.getColumn() - 1; i++) {
                    sb.append(' ');
                }
                // Underline the whole range when it ends on the same line, like the compiler does.
                int width = location.getLineEnd() == location.getLine() && location.getColumnEnd() > location.getColumn()
                        ? location.getColumnEnd() - location.getColumn()
                        : 1;
                for (int i = 0; i < width; i++) {
                    sb.append('^');
                }
            }
            return sb.toString();
        }
    }

    /**
     * Prints compiler diagnostics. The incremental compiler's own progress notes are dropped, and
     * its failures (after which it rebuilds everything itself) go to the daemon log: they are not
     * problems in the user's code.
     */
    static final class Logger implements KotlinLogger {
        private final PrintStream out;
        private final PrintStream log;
        /** Whether a failure was IntelliJ's "Storage for [...] is already registered". */
        volatile boolean storageConflict;

        Logger(PrintStream out, PrintStream log) {
            this.out = out;
            this.log = log;
        }

        /** Whether `message` is the incremental compiler's report of its own failure, not a diagnostic. */
        static boolean isFallback(String message) {
            // The compiler's text is a margin-trimmed block; drop a margin it may have kept.
            String text = message.trim();
            while (text.startsWith("|")) {
                text = text.substring(1).trim();
            }
            return text.startsWith("Incremental compilation was attempted but failed")
                    || text.startsWith("Incremental compilation failed")
                    || text.startsWith("Falling back to non-incremental compilation");
        }

        private void print(String message, Throwable cause) {
            PrintStream target = out;
            if (message != null && isFallback(message)) {
                target = log;
                if (message.contains("is already registered")) {
                    storageConflict = true;
                }
            }
            target.println(message);
            if (cause != null) {
                cause.printStackTrace(target);
            }
        }

        @Override
        public boolean isDebugEnabled() {
            return false;
        }

        @Override
        public void error(String message, Throwable cause) {
            print(message, cause);
        }

        @Override
        public void warn(String message, Throwable cause) {
            print(message, null);
        }

        @Override
        public void warn(String message) {
            print(message, null);
        }

        @Override
        public void info(String message) {
        }

        @Override
        public void debug(String message) {
        }

        @Override
        public void lifecycle(String message) {
        }
    }
}
