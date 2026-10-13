@echo off
rem Starts Kargo's Kotlin language server: LSP over stdin and stdout.
rem JAVA_HOME selects the JDK (17 or newer); KARGO_LSP_JVM_OPTS holds the JVM options.
setlocal
set "KARGO_SERVER_HOME=%~dp0.."
if defined JAVA_HOME (set "JAVA=%JAVA_HOME%\bin\java.exe") else (set "JAVA=java")
if not defined KARGO_LSP_JVM_OPTS set "KARGO_LSP_JVM_OPTS=-Xmx2g -XX:+UseG1GC -XX:SoftRefLRUPolicyMSPerMB=50"
"%JAVA%" %KARGO_LSP_JVM_OPTS% -cp "%KARGO_SERVER_HOME%\lib\*" io.github.enekocm7.kargo.server.MainKt %*
