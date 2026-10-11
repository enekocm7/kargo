// The Kargo compiler daemon: a JVM that stays running between builds and runs
// the Kotlin compiler (and javac) in-process, so a build pays neither JVM
// startup nor a cold JIT. Kargo compiles this file with the JDK's javac on
// first use and talks to it over a loopback socket.
//
// Protocol (all integers big-endian, strings are u32 length + UTF-8 bytes):
//   request:  u32 PROTOCOL, string token, string command, u32 n, n * string argument
//             commands: `kotlin` (first argument is the compiler main class), `javac`, `stop`
//   response: frames of u8 kind + payload: 'o' u32 length + bytes (compiler output),
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
import java.util.Arrays;
import java.util.concurrent.atomic.AtomicInteger;

public final class KargoDaemon {
    static final int PROTOCOL = 1;
    /** Exit code telling the client to run the tool as a separate process instead. */
    static final int UNSUPPORTED = -1000;

    /** Where `System.out`/`System.err` of the current request (and threads it starts) go. */
    static final InheritableThreadLocal<OutputStream> SINK = new InheritableThreadLocal<>();
    static final AtomicInteger ACTIVE = new AtomicInteger();
    static volatile long lastUse = System.currentTimeMillis();

    public static void main(String[] args) throws Exception {
        Path info = Paths.get(args[0]);
        long idleMillis = Long.parseLong(args[1]) * 1000L;
        // Keeps the compiler's application environment alive between compilations, like the Kotlin daemon.
        System.setProperty("kotlin.environment.keepalive", "true");
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
            if (!owner || System.currentTimeMillis() - lastUse > idleMillis) {
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
            if (command.equals("stop")) {
                exit(out, 0);
                System.exit(0);
            }
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
            exit(out, code);
        } catch (IOException e) {
            // The client went away (e.g. Ctrl-C); nothing to report to.
        } finally {
            lastUse = System.currentTimeMillis();
            ACTIVE.decrementAndGet();
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
        if (command.equals("javac")) {
            javax.tools.JavaCompiler javac = javax.tools.ToolProvider.getSystemJavaCompiler();
            if (javac == null) {
                return UNSUPPORTED;
            }
            return javac.run(null, output, output, arguments);
        }
        return UNSUPPORTED;
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
