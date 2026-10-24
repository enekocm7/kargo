//! A small HTTP server for Kotlin/Wasm in the browser. `kargo run` uses it to
//! serve the build output; `kargo test` serves the page that runs the tests
//! and receives the console output that page reports back.
//!
//! It listens on the loopback interface only and serves files from a few
//! directories, so it needs no dependencies and no configuration.

use std::ffi::OsStr;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};

use kargo_util::process;

/// URL prefix of the endpoints the test page reports to: `stdout`, `stderr`, `returned` and `failed`.
pub const REPORT_PREFIX: &str = "/__kargo/";
/// A request to this endpoint is answered once [`Server::ask_to_quit`] is called, which is when the page closes its window.
pub const QUIT_PATH: &str = "/__kargo/quit";

/// The largest request body accepted, which is plenty for console output.
const MAX_BODY: usize = 16 * 1024 * 1024;
const MAX_HEADERS: usize = 100;

/// What the test page tells Kargo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageEvent {
    /// `console.log` and `console.info` output.
    Stdout(String),
    /// `console.warn`, `console.error` output and uncaught errors.
    Stderr(String),
    /// The test entry point returned; asynchronous tests may still be running.
    Returned,
    /// The page could not run the tests.
    Failed(String),
}

/// The directories a site is made of: each is served below a URL prefix.
#[derive(Debug, Clone)]
pub struct Site {
    mounts: Vec<(String, PathBuf)>,
}

impl Site {
    /// A site that serves `root` at `/`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { mounts: vec![("/".to_owned(), root.into())] }
    }

    /// Also serves `dir` below `prefix` (`/node_modules/`), taking precedence over files at `/`.
    pub fn mount(mut self, prefix: &str, dir: impl Into<PathBuf>) -> Self {
        let prefix = format!("/{}/", prefix.trim_matches('/')).replace("//", "/");
        self.mounts.push((prefix, dir.into()));
        self
    }

    /// The file or directory a request path names, or `None` for a path that leaves its mount.
    fn resolve(&self, url_path: &str) -> Option<PathBuf> {
        let decoded = percent_decode(url_path.split(['?', '#']).next().unwrap_or_default())?;
        let (prefix, dir) = self.mounts.iter().filter(|(prefix, _)| decoded.starts_with(prefix.as_str())).max_by_key(|(p, _)| p.len())?;
        let mut path = dir.clone();
        for segment in decoded[prefix.len()..].split('/') {
            match segment {
                "" | "." => {}
                // Neither leaving the directory, nor naming a drive or a stream on Windows, nor reaching
                // the hidden files next to the module (the compiler's `.fingerprint/`, a browser profile).
                s if s.starts_with('.') || s.contains(['\\', ':', '\0']) => return None,
                s => path.push(s),
            }
        }
        Some(path)
    }
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Opens a file or URL in the browser named by `$BROWSER`, else the system default.
pub fn open_in_browser(target: &OsStr) -> Result<()> {
    let mut cmd = match std::env::var_os("BROWSER").filter(|b| !b.is_empty()) {
        Some(browser) => Command::new(browser),
        None if cfg!(windows) => {
            let mut c = Command::new("rundll32");
            c.arg("url.dll,FileProtocolHandler");
            c
        }
        None if cfg!(target_os = "macos") => Command::new("open"),
        None => Command::new("xdg-open"),
    };
    cmd.arg(target);
    cmd.spawn().with_context(|| {
        format!("failed to open {} in a browser (`{}`); open it manually", Path::new(target).display(), process::describe(&cmd))
    })?;
    Ok(())
}

/// Listens on `127.0.0.1:port`; port 0 picks a free one.
pub fn bind(port: u16) -> Result<TcpListener> {
    TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .with_context(|| format!("failed to listen on 127.0.0.1:{port}; is something else using it? choose another port with `--port`"))
}

/// What the connection threads share with the [`Server`].
#[derive(Default)]
struct Shared {
    stopped: AtomicBool,
    quit: Mutex<bool>,
    quit_asked: Condvar,
}

impl Shared {
    fn ask_to_quit(&self) {
        *self.quit.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.quit_asked.notify_all();
    }

    /// Blocks until the page is asked to quit or the server stops.
    fn wait_for_quit(&self) {
        let mut quit = self.quit.lock().unwrap_or_else(PoisonError::into_inner);
        while !*quit && !self.stopped.load(Ordering::Relaxed) {
            quit = self.quit_asked.wait_timeout(quit, Duration::from_millis(100)).unwrap_or_else(PoisonError::into_inner).0;
        }
    }
}

/// A running server, stopped when dropped.
pub struct Server {
    addr: SocketAddr,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    /// Starts serving `site` on `listener`. With `events`, the endpoints below
    /// [`REPORT_PREFIX`] accept what the page reports; without it they don't exist.
    pub fn start(listener: TcpListener, site: Site, events: Option<Sender<PageEvent>>) -> Result<Server> {
        let addr = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let shared = Arc::new(Shared::default());
        let thread = std::thread::spawn({
            let shared = shared.clone();
            let site = Arc::new(site);
            move || {
                while !shared.stopped.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let (site, events, shared) = (site.clone(), events.clone(), shared.clone());
                            std::thread::spawn(move || {
                                let _ = handle(stream, &site, events.as_ref(), &shared, addr.port());
                            });
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(5)),
                        Err(_) => std::thread::sleep(Duration::from_millis(50)),
                    }
                }
            }
        });
        Ok(Server { addr, shared, thread: Some(thread) })
    }

    /// Answers the page's request to [`QUIT_PATH`], which makes it close its window. A browser
    /// that Kargo cannot stop by ending the process it started is stopped this way.
    pub fn ask_to_quit(&self) {
        self.shared.ask_to_quit();
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// The address of the site's root.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port())
    }

    /// Serves until the process ends (`kargo run` stops with Ctrl+C).
    pub fn wait(mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::Relaxed);
        self.shared.quit_asked.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Request {
    method: String,
    target: String,
    host: Option<String>,
    accept_encoding: Option<String>,
    body: Vec<u8>,
}

fn read_request(stream: &TcpStream) -> io::Result<Option<Request>> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else { return Ok(None) };
    let (method, target) = (method.to_owned(), target.to_owned());
    let (mut length, mut host, mut accept_encoding) = (0usize, None, None);
    for _ in 0..MAX_HEADERS {
        line.clear();
        if reader.read_line(&mut line)? == 0 || line.trim_end().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            match name.trim().to_ascii_lowercase().as_str() {
                "content-length" => length = value.trim().parse().unwrap_or(0),
                "host" => host = Some(value.trim().to_owned()),
                "accept-encoding" => accept_encoding = Some(value.trim().to_ascii_lowercase()),
                _ => {}
            }
        }
    }
    let mut body = vec![0; length.min(MAX_BODY)];
    reader.read_exact(&mut body)?;
    Ok(Some(Request { method, target, host, accept_encoding, body }))
}

fn handle(mut stream: TcpStream, site: &Site, events: Option<&Sender<PageEvent>>, shared: &Shared, port: u16) -> io::Result<()> {
    let result = answer(&mut stream, site, events, shared, port);
    // Closing a socket abruptly can reset the connection before the browser has read the response
    // (Windows does): say that nothing more is coming, then wait for the client to finish.
    let _ = stream.shutdown(Shutdown::Write);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
    let _ = io::copy(&mut stream, &mut io::sink());
    result
}

fn answer(stream: &mut TcpStream, site: &Site, events: Option<&Sender<PageEvent>>, shared: &Shared, port: u16) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let Some(request) = read_request(stream)? else { return Ok(()) };

    // A page on another site could point its own name at 127.0.0.1 to read what is served here.
    let local = |host: &str| [format!("127.0.0.1:{port}"), format!("localhost:{port}"), format!("[::1]:{port}")].iter().any(|h| h == host);
    if !request.host.as_deref().is_some_and(local) {
        return respond(stream, "403 Forbidden", "text/plain", b"forbidden host", false);
    }

    let path = request.target.split(['?', '#']).next().unwrap_or_default();
    if request.method == "GET" && events.is_some() && path == QUIT_PATH {
        shared.wait_for_quit();
        return respond(stream, "204 No Content", "text/plain", b"", false);
    }
    match request.method.as_str() {
        "GET" | "HEAD" => {
            let head = request.method == "HEAD";
            match read_file(site, &request.target, request.accept_encoding.as_deref()) {
                Some((file, body, encoding)) => respond_encoding(stream, "200 OK", content_type(&file), &body, head, encoding),
                None => respond(stream, "404 Not Found", "text/plain", b"not found", head),
            }
        }
        "POST" => {
            let event = path.strip_prefix(REPORT_PREFIX).and_then(|name| {
                let text = String::from_utf8_lossy(&request.body).into_owned();
                match name {
                    "stdout" => Some(PageEvent::Stdout(text)),
                    "stderr" => Some(PageEvent::Stderr(text)),
                    "returned" => Some(PageEvent::Returned),
                    "failed" => Some(PageEvent::Failed(text)),
                    _ => None,
                }
            });
            match (event, events) {
                (Some(event), Some(events)) => {
                    let _ = events.send(event);
                    respond(stream, "204 No Content", "text/plain", b"", false)
                }
                _ => respond(stream, "404 Not Found", "text/plain", b"not found", false),
            }
        }
        _ => respond(stream, "405 Method Not Allowed", "text/plain", b"method not allowed", false),
    }
}

/// The file a GET request names (`index.html` for a directory) and its contents.
fn read_file(site: &Site, target: &str, accept_encoding: Option<&str>) -> Option<(PathBuf, Vec<u8>, Option<&'static str>)> {
    let mut file = site.resolve(target)?;
    if file.is_dir() {
        file.push("index.html");
    }
    if let Some(encodings) = accept_encoding {
        for (encoding, suffix) in [("br", "br"), ("gzip", "gz")] {
            if encodings.split(',').any(|item| {
                let mut parts = item.trim().split(';');
                parts.next() == Some(encoding)
                    && !parts.any(|p| p.trim().eq_ignore_ascii_case("q=0") || p.trim().eq_ignore_ascii_case("q=0.0"))
            }) {
                let compressed = file.with_file_name(format!("{}.{}", file.file_name()?.to_string_lossy(), suffix));
                if let Ok(body) = std::fs::read(compressed) {
                    return Some((file, body, Some(if encoding == "br" { "br" } else { "gzip" })));
                }
            }
        }
    }
    let body = std::fs::read(&file).ok()?;
    Some((file, body, None))
}

fn respond(stream: &mut TcpStream, status: &str, content_type: &str, body: &[u8], head_only: bool) -> io::Result<()> {
    respond_encoding(stream, status, content_type, body, head_only, None)
}

fn respond_encoding(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
    head_only: bool,
    encoding: Option<&str>,
) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n{}{}Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len(),
        encoding.map(|e| format!("Content-Encoding: {e}\r\n")).unwrap_or_default(),
        encoding.map(|_| "Vary: Accept-Encoding\r\n").unwrap_or_default(),
    )?;
    if !head_only {
        stream.write_all(body)?;
    }
    stream.flush()
}

/// The `Content-Type` of a file. Browsers refuse to run `.wasm` that is not `application/wasm`
/// (when streaming it) and ES modules that are not JavaScript.
fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("wasm") => "application/wasm",
        Some("txt" | "md" | "ts" | "mts") => "text/plain; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("mp3") => "audio/mpeg",
        Some("ogg") => "audio/ogg",
        Some("wav") => "audio/wav",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    /// Sends a raw request and returns the whole response.
    fn exchange(port: u16, request: &str) -> String {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .unwrap_or_else(|e| panic!("{e} reading the response to {request:?}, got {} bytes", response.len()));
        String::from_utf8_lossy(&response).into_owned()
    }

    fn get(server: &Server, path: &str) -> String {
        exchange(server.port(), &format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n", server.port()))
    }

    fn site(dir: &Path) -> Site {
        std::fs::create_dir_all(dir.join("root/sub")).unwrap();
        std::fs::create_dir_all(dir.join("modules/pkg")).unwrap();
        std::fs::write(dir.join("root/index.html"), "<h1>home</h1>").unwrap();
        std::fs::write(dir.join("root/app.wasm"), [0, 0x61, 0x73, 0x6d]).unwrap();
        std::fs::write(dir.join("root/app.wasm.br"), [1, 2, 3]).unwrap();
        std::fs::write(dir.join("root/app.mjs"), "export {}").unwrap();
        std::fs::write(dir.join("root/sub/index.html"), "<h1>sub</h1>").unwrap();
        std::fs::write(dir.join("root/with space.txt"), "spaced").unwrap();
        std::fs::write(dir.join("modules/pkg/index.js"), "export default 1").unwrap();
        std::fs::write(dir.join("secret.txt"), "secret").unwrap();
        std::fs::create_dir_all(dir.join("root/.fingerprint")).unwrap();
        std::fs::write(dir.join("root/.fingerprint/app.args"), "hidden").unwrap();
        Site::new(dir.join("root")).mount("node_modules", dir.join("modules"))
    }

    #[test]
    fn serves_files_with_their_content_types() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(bind(0).unwrap(), site(dir.path()), None).unwrap();

        let home = get(&server, "/");
        assert!(home.starts_with("HTTP/1.1 200 OK"), "{home}");
        assert!(home.contains("Content-Type: text/html; charset=utf-8") && home.ends_with("<h1>home</h1>"), "{home}");
        assert!(get(&server, "/sub/").ends_with("<h1>sub</h1>"));
        assert!(get(&server, "/app.wasm").contains("Content-Type: application/wasm"));
        let compressed = exchange(
            server.port(),
            &format!("GET /app.wasm HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAccept-Encoding: br, gzip\r\n\r\n", server.port()),
        );
        assert!(
            compressed.contains("Content-Type: application/wasm")
                && compressed.contains("Content-Encoding: br")
                && compressed.contains("Vary: Accept-Encoding")
                && compressed.ends_with("\u{1}\u{2}\u{3}"),
            "{compressed:?}"
        );
        let refused = exchange(
            server.port(),
            &format!("GET /app.wasm HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAccept-Encoding: br;q=0, gzip;q=0\r\n\r\n", server.port()),
        );
        assert!(!refused.contains("Content-Encoding:") && refused.ends_with("\0asm"), "{refused:?}");
        assert!(get(&server, "/app.mjs").contains("Content-Type: text/javascript"));
        assert!(get(&server, "/with%20space.txt?x=1").ends_with("spaced"));
        assert!(get(&server, "/node_modules/pkg/index.js").ends_with("export default 1"));
        assert!(get(&server, "/").contains("Cache-Control: no-store"));

        let head = exchange(server.port(), &format!("HEAD /app.mjs HTTP/1.1\r\nHost: localhost:{}\r\n\r\n", server.port()));
        assert!(head.starts_with("HTTP/1.1 200 OK") && head.contains("Content-Length: 9") && !head.contains("export"), "{head}");
    }

    #[test]
    fn refuses_what_is_not_part_of_the_site() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(bind(0).unwrap(), site(dir.path()), None).unwrap();

        assert!(get(&server, "/missing.js").starts_with("HTTP/1.1 404"));
        // Hidden files (the compiler's `.fingerprint/`, a browser profile) are not part of the site.
        for hidden in ["/.fingerprint/app.args", "/.fingerprint/", "/sub/.git"] {
            let response = get(&server, hidden);
            assert!(response.starts_with("HTTP/1.1 404") && !response.contains("hidden"), "{hidden}: {response}");
        }
        for escape in [
            "/../secret.txt",
            "/%2e%2e/secret.txt",
            "/sub/../../secret.txt",
            "/..%5csecret.txt",
            "/C:/Windows/win.ini",
            "/node_modules/../secret.txt",
        ] {
            let response = get(&server, escape);
            assert!(response.starts_with("HTTP/1.1 404"), "{escape}: {response}");
            assert!(!response.contains("secret\n") && !response.ends_with("secret"), "{escape}: {response}");
        }
        // Only the loopback names are accepted as the host.
        let rebound = exchange(server.port(), "GET / HTTP/1.1\r\nHost: evil.example:80\r\n\r\n");
        assert!(rebound.starts_with("HTTP/1.1 403"), "{rebound}");
        assert!(exchange(server.port(), "GET / HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 403"));
        let put = exchange(server.port(), &format!("PUT / HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n", server.port()));
        assert!(put.starts_with("HTTP/1.1 405"), "{put}");
        // Nothing receives reports unless the caller asked for them.
        let post = exchange(
            server.port(),
            &format!("POST /__kargo/stdout HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Length: 2\r\n\r\nhi", server.port()),
        );
        assert!(post.starts_with("HTTP/1.1 404"), "{post}");
    }

    #[test]
    fn collects_what_the_page_reports() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        let server = Server::start(bind(0).unwrap(), site(dir.path()), Some(tx)).unwrap();
        let post = |name: &str, body: &str| {
            exchange(
                server.port(),
                &format!(
                    "POST /__kargo/{name} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Length: {}\r\n\r\n{body}",
                    server.port(),
                    body.len()
                ),
            )
        };

        assert!(post("stdout", "##teamcity[testStarted name='a']\n").starts_with("HTTP/1.1 204"));
        post("stderr", "oops\n");
        post("returned", "");
        post("failed", "boom");
        assert!(post("unknown", "x").starts_with("HTTP/1.1 404"));

        let wait = Duration::from_secs(5);
        assert_eq!(rx.recv_timeout(wait).unwrap(), PageEvent::Stdout("##teamcity[testStarted name='a']\n".to_owned()));
        assert_eq!(rx.recv_timeout(wait).unwrap(), PageEvent::Stderr("oops\n".to_owned()));
        assert_eq!(rx.recv_timeout(wait).unwrap(), PageEvent::Returned);
        assert_eq!(rx.recv_timeout(wait).unwrap(), PageEvent::Failed("boom".to_owned()));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn the_quit_request_is_answered_when_the_page_is_asked_to_quit() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, _events) = mpsc::channel();
        let server = Server::start(bind(0).unwrap(), site(dir.path()), Some(tx)).unwrap();
        let port = server.port();
        let (answered, response) = mpsc::channel();
        std::thread::spawn(move || {
            let request = format!("GET {QUIT_PATH} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n");
            let _ = answered.send(exchange(port, &request));
        });
        assert!(response.recv_timeout(Duration::from_millis(300)).is_err(), "the page is not asked to quit yet");
        server.ask_to_quit();
        let response = response.recv_timeout(Duration::from_secs(5)).expect("answered once asked to quit");
        assert!(response.starts_with("HTTP/1.1 204"), "{response}");

        // Without a receiver for reports there is no such endpoint.
        let plain = Server::start(bind(0).unwrap(), site(dir.path()), None).unwrap();
        assert!(get(&plain, QUIT_PATH).starts_with("HTTP/1.1 404"));
    }

    #[test]
    fn dropping_the_server_releases_a_waiting_page() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, _events) = mpsc::channel();
        let server = Server::start(bind(0).unwrap(), site(dir.path()), Some(tx)).unwrap();
        let port = server.port();
        let waiting = std::thread::spawn(move || exchange(port, &format!("GET {QUIT_PATH} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n")));
        std::thread::sleep(Duration::from_millis(200));
        drop(server);
        assert!(waiting.join().unwrap().starts_with("HTTP/1.1 204"));
    }

    #[test]
    fn stops_listening_when_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(bind(0).unwrap(), site(dir.path()), None).unwrap();
        let port = server.port();
        drop(server);
        assert!(TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_err());
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("/a%20b/%C3%A9").as_deref(), Some("/a b/é"));
        assert_eq!(percent_decode("/100%"), None);
        assert_eq!(percent_decode("/%zz"), None);
        assert_eq!(percent_decode("/%ff"), None, "not UTF-8");
    }
}
