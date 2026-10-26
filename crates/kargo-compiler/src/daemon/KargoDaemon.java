// The Kargo compiler daemon: a JVM that stays running between builds and runs
// the Kotlin compiler (and javac) in-process, so a build pays neither JVM
// startup nor a cold JIT. Kargo compiles this file with the JDK's javac on
// first use and talks to it over a loopback socket.
//
// Protocol (all integers big-endian, strings are u32 length + UTF-8 bytes):
//   request:  u32 PROTOCOL, string token, string command, u32 n, n * string argument
//             commands: `kotlin` (first argument is the compiler main class), `javac`,
//             `kotlin-ic` (incremental compilation, see KargoIc.java), `stop`
//   response: frames of u8 kind + payload: 'a' (no payload, sent once the request is read), 'o' u32 length + bytes (compiler output),
//             'x' i32 exit code (last frame).

import java.io.BufferedOutputStream;
import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.io.PrintStream;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.net.InetAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.nio.file.StandardCopyOption;
import java.nio.file.attribute.PosixFilePermissions;
import java.security.MessageDigest;
import java.security.SecureRandom;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.Locale;
import java.util.TreeSet;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.locks.ReentrantLock;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicBoolean;

public final class KargoDaemon {
    static final int PROTOCOL = 2;
    /** Exit code telling the client to run the tool as a separate process instead. */
    static final int UNSUPPORTED = -1000;
    /** Exit code for an incremental compilation that failed for reasons other than compile errors. */
    static final int IC_FAILED = -1001;

    /** Where `System.out`/`System.err` of the current request (and threads it starts) go. */
    static final InheritableThreadLocal<OutputStream> SINK = new InheritableThreadLocal<>();
    static final AtomicInteger ACTIVE = new AtomicInteger();
    /** One lock per output directory, so a request waits for an orphaned compilation writing to the same one. */
    static final ConcurrentHashMap<String, ReentrantLock> OUTPUT_LOCKS = new ConcurrentHashMap<>();
    static final boolean WINDOWS = System.getProperty("os.name", "").toLowerCase(Locale.ROOT).startsWith("windows");
    static volatile long lastUse = System.currentTimeMillis();
    /**
     * Set when the compiler left incremental caches registered that nothing will close; every
     * later compilation of that unit would fail, so the daemon exits once it is idle.
     */
    static volatile boolean retiring;
    /** The daemon's log (its original stderr), for problems that are not the client's output. */
    static PrintStream log = System.err;

    public static void main(String[] args) throws Exception {
        Path info = Paths.get(args[0]);
        long idleMillis = Long.parseLong(args[1]) * 1000L;
        // Keeps the compiler's application environment alive between compilations, like the Kotlin daemon.
        System.setProperty("kotlin.environment.keepalive", "true");
        log = System.err;
        System.setOut(new PrintStream(new Router(System.out), true));
        System.setErr(new PrintStream(new Router(System.err), true));

        ServerSocket server = new ServerSocket(0, 50, InetAddress.getLoopbackAddress());
        byte[] secret = new byte[24];
        new SecureRandom().nextBytes(secret);
        String token = hex(secret);
        String content = server.getLocalPort() + " " + token + " " + ProcessHandle.current().pid() + "\n";
        writeInfo(info, content);

        Thread watchdog = new Thread(() -> watch(info, content, idleMillis), "kargo-daemon-watchdog");
        watchdog.setDaemon(true);
        watchdog.start();

        while (true) {
            Socket socket = server.accept();
            // The compiler recurses deeply on large expressions; match the `-Xss8m` of a standalone run.
            Thread worker = new Thread(null, () -> serve(socket, token), "kargo-compile", 16L << 20);
            worker.setDaemon(true);
            worker.start();
        }
    }

    /** Exits when idle for too long, or when another daemon (or nobody) owns the info file. */
    static void watch(Path info, String content, long idleMillis) {
        while (true) {
            try {
                Thread.sleep(2000);
            } catch (InterruptedException e) {
                return;
            }
            if (ACTIVE.get() > 0) {
                continue;
            }
            boolean owner;
            try {
                owner = new String(Files.readAllBytes(info), StandardCharsets.UTF_8).equals(content);
            } catch (IOException e) {
                owner = false;
            }
            if (!owner || retiring || System.currentTimeMillis() - lastUse > idleMillis) {
                if (owner) {
                    // Leave no info file behind for a port another program may take.
                    try {
                        Files.deleteIfExists(info);
                    } catch (IOException e) {
                        // The next client finds the stale file by its handshake.
                    }
                }
                System.exit(0);
            }
        }
    }

    static void writeInfo(Path info, String content) throws IOException {
        Path tmp = info.resolveSibling(info.getFileName() + "." + ProcessHandle.current().pid() + ".tmp");
        Files.deleteIfExists(tmp);
        try {
            Files.createFile(tmp, PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rw-------")));
        } catch (UnsupportedOperationException e) {
            Files.createFile(tmp);
        }
        Files.write(tmp, content.getBytes(StandardCharsets.UTF_8));
        Files.move(tmp, info, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE);
    }

    static void serve(Socket socket, String token) {
        ACTIVE.incrementAndGet();
        try (Socket s = socket) {
            DataInputStream in = new DataInputStream(s.getInputStream());
            DataOutputStream out = new DataOutputStream(new BufferedOutputStream(s.getOutputStream()));
            if (in.readInt() != PROTOCOL) {
                return;
            }
            byte[] given = readBytes(in);
            if (!MessageDigest.isEqual(given, token.getBytes(StandardCharsets.UTF_8))) {
                return;
            }
            String command = readString(in);
            String[] arguments = new String[in.readInt()];
            for (int i = 0; i < arguments.length; i++) {
                arguments[i] = readString(in);
            }
            synchronized (out) {
                out.writeByte('a');
                out.flush();
            }
            if (command.equals("stop")) {
                exit(out, 0);
                System.exit(0);
            }
            // The client sends nothing after the request, so a read that ends means it went away.
            AtomicBoolean gone = new AtomicBoolean();
            Thread watcher = new Thread(() -> {
                try {
                    while (in.read() >= 0) {
                        // ignore stray bytes
                    }
                } catch (IOException e) {
                    // closed
                }
                gone.set(true);
            }, "kargo-client-watcher");
            watcher.setDaemon(true);
            watcher.start();
            if (retiring) {
                // Compilations here can no longer be trusted; the client runs the tool as a process.
                exit(out, UNSUPPORTED);
                return;
            }
            // Sorted, so two requests that share directories take their locks in the same order.
            List<ReentrantLock> locks = new ArrayList<>();
            for (String dir : new TreeSet<>(lockedDirectories(command, arguments))) {
                ReentrantLock lock = OUTPUT_LOCKS.computeIfAbsent(dir, k -> new ReentrantLock());
                lock.lock();
                locks.add(lock);
            }
            try {
                if (gone.get()) {
                    // The client left while waiting for an earlier compilation of the same output.
                    return;
                }
                compileAndReply(command, arguments, out, gone);
            } finally {
                for (ReentrantLock lock : locks) {
                    lock.unlock();
                }
            }
        } catch (IOException e) {
            // The client went away (e.g. Ctrl-C); nothing to report to.
        } finally {
            lastUse = System.currentTimeMillis();
            ACTIVE.decrementAndGet();
        }
    }

    /**
     * The directories a request writes to: its output, and for an incremental compilation its
     * caches, which the compiler registers JVM-wide and fails to open twice.
     */
    static List<String> lockedDirectories(String command, String[] arguments) {
        List<String> flags = command.equals("kotlin-ic") ? List.of("--dest", "--working-dir")
                : command.equals("kotlin") || command.equals("javac") ? List.of("-d")
                : List.of();
        List<String> dirs = new ArrayList<>();
        for (int i = 0; i + 1 < arguments.length; i++) {
            if (arguments[i].equals("--")) {
                break;
            }
            if (flags.contains(arguments[i])) {
                dirs.add(lockKey(arguments[i + 1]));
            }
        }
        return dirs;
    }

    /**
     * One key per directory however a client spells it: `F:\p` and `f:\p` (editors often
     * lowercase the drive letter) are the same directory to the compiler's caches.
     */
    static String lockKey(String dir) {
        Path path;
        try {
            path = Paths.get(dir).toAbsolutePath().normalize();
        } catch (RuntimeException e) {
            return dir;
        }
        // The real path of the deepest existing ancestor, so a directory not created yet keys the same.
        Path existing = path;
        while (existing != null && !Files.exists(existing)) {
            existing = existing.getParent();
        }
        if (existing != null) {
            try {
                path = existing.toRealPath().resolve(existing.relativize(path));
            } catch (IOException | RuntimeException e) {
                // keep the absolute path
            }
        }
        String key = path.toString();
        return WINDOWS ? key.toLowerCase(Locale.ROOT) : key;
    }

    static void compileAndReply(String command, String[] arguments, DataOutputStream out, AtomicBoolean gone) throws IOException {
        {
            FrameStream frames = new FrameStream(out);
            PrintStream output = new PrintStream(new BufferedOutputStream(frames, 8192), false, "UTF-8");
            SINK.set(output);
            int code;
            try {
                code = run(command, arguments, output);
            } catch (OutOfMemoryError e) {
                output.println("error: the Kargo compiler daemon ran out of memory; it will restart on the next build "
                        + "(raise its heap with KARGO_COMPILER_JVM_OPTS, e.g. -Xmx4g)");
                output.flush();
                exit(out, 137);
                System.exit(1);
                return;
            } catch (Throwable e) {
                Throwable cause = e instanceof InvocationTargetException && e.getCause() != null ? e.getCause() : e;
                output.println("error: the compiler crashed inside the Kargo daemon:");
                cause.printStackTrace(output);
                code = 2;
            } finally {
                SINK.remove();
            }
            output.flush();
            if (!gone.get()) {
                exit(out, code);
            }
        }
    }

    static int run(String command, String[] arguments, PrintStream output) throws Exception {
        if (command.equals("kotlin")) {
            Class<?> compilerClass = Class.forName(arguments[0]);
            Object compiler = compilerClass.getDeclaredConstructor().newInstance();
            Class<?> rendererClass = Class.forName("org.jetbrains.kotlin.cli.common.messages.MessageRenderer");
            // Full paths: the client knows its working directory and makes them relative.
            Object renderer = rendererClass.getField("PLAIN_FULL_PATHS").get(null);
            Method exec = compilerClass.getMethod("exec", PrintStream.class, rendererClass, String[].class);
            Object exitCode = exec.invoke(compiler, output, renderer, (Object) Arrays.copyOfRange(arguments, 1, arguments.length));
            return (Integer) exitCode.getClass().getMethod("getCode").invoke(exitCode);
        }
        if (command.equals("kotlin-ic")) {
            Method run;
            try {
                run = Class.forName("KargoIc").getMethod("run", String[].class, PrintStream.class, PrintStream.class);
            } catch (ClassNotFoundException | NoSuchMethodException | LinkageError e) {
                return UNSUPPORTED;
            }
            try {
                return (Integer) run.invoke(null, arguments, output, log);
            } catch (InvocationTargetException e) {
                if (e.getCause() instanceof OutOfMemoryError) {
                    throw (OutOfMemoryError) e.getCause();
                }
                // Not a compile error: the client recompiles everything from scratch instead.
                log.println("incremental compilation failed:");
                e.getCause().printStackTrace(log);
                if (alreadyRegistered(e.getCause())) {
                    log.println("incremental caches are still registered in this JVM; the daemon exits once idle");
                    retiring = true;
                }
                return IC_FAILED;
            }
        }
        if (command.equals("javac")) {
            javax.tools.JavaCompiler javac = javax.tools.ToolProvider.getSystemJavaCompiler();
            if (javac == null) {
                return UNSUPPORTED;
            }
            return javac.run(null, output, output, arguments);
        }
        return UNSUPPORTED;
    }

    /** Whether `e` is IntelliJ's "Storage for [...] is already registered", the sign of caches nobody closed. */
    static boolean alreadyRegistered(Throwable e) {
        for (int depth = 0; e != null && depth < 20; e = e.getCause(), depth++) {
            String message = e.getMessage();
            if (message != null && message.contains("is already registered")) {
                return true;
            }
            for (Throwable s : e.getSuppressed()) {
                if (s.getMessage() != null && s.getMessage().contains("is already registered")) {
                    return true;
                }
            }
        }
        return false;
    }

    static void exit(DataOutputStream out, int code) throws IOException {
        synchronized (out) {
            out.writeByte('x');
            out.writeInt(code);
            out.flush();
        }
    }

    static byte[] readBytes(DataInputStream in) throws IOException {
        int length = in.readInt();
        if (length < 0 || length > (64 << 20)) {
            throw new IOException("bad length");
        }
        byte[] bytes = new byte[length];
        in.readFully(bytes);
        return bytes;
    }

    static String readString(DataInputStream in) throws IOException {
        return new String(readBytes(in), StandardCharsets.UTF_8);
    }

    static String hex(byte[] bytes) {
        StringBuilder sb = new StringBuilder();
        for (byte b : bytes) {
            sb.append(String.format("%02x", b));
        }
        return sb.toString();
    }

    /** Sends everything written to it as output frames. */
    static final class FrameStream extends OutputStream {
        private final DataOutputStream out;

        FrameStream(DataOutputStream out) {
            this.out = out;
        }

        @Override
        public void write(int b) throws IOException {
            write(new byte[] {(byte) b}, 0, 1);
        }

        @Override
        public void write(byte[] b, int off, int len) throws IOException {
            if (len == 0) {
                return;
            }
            synchronized (out) {
                out.writeByte('o');
                out.writeInt(len);
                out.write(b, off, len);
                out.flush();
            }
        }
    }

    /** `System.out`/`System.err` replacement that writes to the current request's output. */
    static final class Router extends OutputStream {
        private final OutputStream fallback;

        Router(OutputStream fallback) {
            this.fallback = fallback;
        }

        private OutputStream target() {
            OutputStream sink = SINK.get();
            return sink != null ? sink : fallback;
        }

        @Override
        public void write(int b) throws IOException {
            try {
                target().write(b);
            } catch (IOException e) {
                fallback.write(b);
            }
        }

        @Override
        public void write(byte[] b, int off, int len) throws IOException {
            try {
                target().write(b, off, len);
            } catch (IOException e) {
                fallback.write(b, off, len);
            }
        }

        @Override
        public void flush() throws IOException {
            try {
                target().flush();
            } catch (IOException e) {
                fallback.flush();
            }
        }
    }
}
