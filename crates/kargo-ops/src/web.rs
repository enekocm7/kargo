//! A small HTTP server for Kotlin/Wasm and Kotlin/JS in the browser. `kargo run` uses it to
//! serve the build output (`kargo run --watch` also reloads the open pages after each
//! build); `kargo test` serves the page that runs the tests and receives the console
//! output that page reports back.
//!
//! It listens on the loopback interface only and serves files from a few
//! directories, so it needs no dependencies and no configuration. A site can forward the
//! requests below some paths to other HTTP servers ([`Site::proxy`]), such as the API of the app.

use std::ffi::OsStr;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use kargo_manifest::ProxyRoute;
use kargo_util::http::{Network, TlsStream};
use kargo_util::process;

/// URL prefix of the endpoints the test page reports to: `stdout`, `stderr`, `returned` and `failed`.
pub const REPORT_PREFIX: &str = "/__kargo/";
/// A request to this endpoint is answered once [`Server::ask_to_quit`] is called, which is when the page closes its window.
pub const QUIT_PATH: &str = "/__kargo/quit";

/// The endpoint a page served with [`Site::live_reload`] listens to (server-sent events): it is told
/// the current generation on connecting and again whenever [`Server::reload`] is called.
pub const RELOAD_PATH: &str = "/__kargo/reload";

/// Added to the HTML pages of a [`Site::live_reload`] site. The page reloads when the generation it is told differs from
/// the first one it saw, so a reload that happened while it was reconnecting is not missed.
const RELOAD_SCRIPT: &str = concat!(
    "<script>(()=>{const s=new EventSource(\"/__kargo/reload\");let g;",
    "s.onmessage=e=>{if(g===undefined)g=e.data;else if(e.data!==g)location.reload()}})()</script>"
);

/// How long a page waits for a reload before the server writes a comment to keep the connection open, which also
/// notices a page that is gone.
const KEEP_ALIVE: Duration = Duration::from_secs(5);

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
    live_reload: bool,
    proxies: Vec<ProxyRoute>,
    network: Network,
}

impl Site {
    /// A site that serves `root` at `/`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { mounts: vec![("/".to_owned(), root.into())], live_reload: false, proxies: Vec::new(), network: Network::default() }
    }

    /// Forwards the requests below the prefix of each of `routes` to the server it names, in the order given, so the
    /// longest prefix goes first. `network` has the certificate authorities `https://` targets are checked against.
    pub fn proxy(mut self, routes: &[ProxyRoute], network: &Network) -> Self {
        self.proxies = routes.to_vec();
        self.network = network.clone();
        self
    }

    /// The route that takes a request for `path`.
    fn route(&self, path: &str) -> Option<&ProxyRoute> {
        self.proxies.iter().find(|r| path.strip_prefix(r.prefix.as_str()).is_some_and(|rest| rest.is_empty() || rest.starts_with('/')))
    }

    /// Adds a script to the HTML pages that reloads them when [`Server::reload`] is called.
    pub fn live_reload(mut self) -> Self {
        self.live_reload = true;
        self
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
    TcpListener::bind((Ipv4Addr::LOCALHOST, port)).with_context(|| {
        format!(
            "failed to listen on 127.0.0.1:{port}; is something else using it? choose another port with `--port` or `port` in kargo.toml"
        )
    })
}

/// What the connection threads share with the [`Server`].
#[derive(Default)]
struct Shared {
    stopped: AtomicBool,
    quit: Mutex<bool>,
    quit_asked: Condvar,
    /// Changes with every [`Server::reload`]; starts at a value that differs between servers, so that a page
    /// that outlives one server reloads when it finds the next on the same port.
    generation: Mutex<u64>,
    reloaded: Condvar,
}

impl Shared {
    fn ask_to_quit(&self) {
        *self.quit.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.quit_asked.notify_all();
    }

    fn generation(&self) -> u64 {
        *self.generation.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn reload(&self) {
        *self.generation.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        self.reloaded.notify_all();
    }

    /// Blocks until the generation is not `seen` any more, the server stops or `timeout` passes; returns the generation.
    fn wait_for_reload(&self, seen: u64, timeout: Duration) -> u64 {
        let deadline = Instant::now() + timeout;
        let mut generation = self.generation.lock().unwrap_or_else(PoisonError::into_inner);
        while *generation == seen && !self.stopped.load(Ordering::Relaxed) {
            let Some(left) = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) else { break };
            generation =
                self.reloaded.wait_timeout(generation, left.min(Duration::from_millis(100))).unwrap_or_else(PoisonError::into_inner).0;
        }
        *generation
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
        let start = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        let shared = Arc::new(Shared { generation: Mutex::new(start), ..Shared::default() });
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

    /// Reloads the pages of a [`Site::live_reload`] site that are open.
    pub fn reload(&self) {
        self.shared.reload();
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
    /// Every header as sent, for the requests that are forwarded.
    headers: Vec<(String, String)>,
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
    let (mut length, mut host, mut accept_encoding, mut headers) = (0usize, None, None, Vec::new());
    for _ in 0..MAX_HEADERS {
        line.clear();
        if reader.read_line(&mut line)? == 0 || line.trim_end().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_owned(), value.trim().to_owned()));
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
    Ok(Some(Request { method, target, host, accept_encoding, headers, body }))
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
    if request.method == "GET" && site.live_reload && path == RELOAD_PATH {
        return stream_reloads(stream, shared);
    }
    if let Some(route) = site.route(path) {
        return forward(stream, &request, route, &site.network);
    }
    match request.method.as_str() {
        "GET" | "HEAD" => {
            let head = request.method == "HEAD";
            match read_file(site, &request.target, request.accept_encoding.as_deref()) {
                Some((file, body, encoding)) => {
                    let kind = content_type(&file);
                    let body = if site.live_reload && encoding.is_none() && kind.starts_with("text/html") {
                        with_reload_script(body)
                    } else {
                        body
                    };
                    respond_encoding(stream, "200 OK", kind, &body, head, encoding)
                }
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

/// How long Kargo waits to reach a server it forwards to, and for it to say something.
const UPSTREAM_CONNECT: Duration = Duration::from_secs(5);
const UPSTREAM_IDLE: Duration = Duration::from_secs(600);
/// How long a tunnel waits for one side before it looks at the other.
const TUNNEL_POLL: Duration = Duration::from_millis(10);

/// A connection to a server a request is forwarded to.
enum Upstream {
    Plain(TcpStream),
    Tls(Box<TlsStream>),
}

impl Upstream {
    fn socket(&self) -> &TcpStream {
        match self {
            Upstream::Plain(tcp) => tcp,
            Upstream::Tls(tls) => &tls.sock,
        }
    }
}

impl Read for Upstream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Upstream::Plain(tcp) => tcp.read(buf),
            Upstream::Tls(tls) => tls.read(buf),
        }
    }
}

impl Write for Upstream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Upstream::Plain(tcp) => tcp.write(buf),
            Upstream::Tls(tls) => tls.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Upstream::Plain(tcp) => tcp.flush(),
            Upstream::Tls(tls) => tls.flush(),
        }
    }
}

/// The headers of a request that are for the connection to Kargo, not for the server it is forwarded to. A WebSocket
/// upgrade keeps `Connection` and `Upgrade`, which are what it asks for.
fn is_hop_header(name: &str, upgrade: bool) -> bool {
    if upgrade && (name.eq_ignore_ascii_case("connection") || name.eq_ignore_ascii_case("upgrade")) {
        return false;
    }
    ["host", "connection", "keep-alive", "upgrade", "te", "trailer", "transfer-encoding", "content-length", "expect"]
        .iter()
        .any(|h| name.eq_ignore_ascii_case(h))
        || name.to_ascii_lowercase().starts_with("proxy-")
}

/// Whether `request` asks to switch to the WebSocket protocol.
fn is_websocket_upgrade(request: &Request) -> bool {
    let has = |name: &str, token: &str| {
        request.headers.iter().any(|(n, v)| n.eq_ignore_ascii_case(name) && v.split(',').any(|t| t.trim().eq_ignore_ascii_case(token)))
    };
    request.method == "GET" && has("upgrade", "websocket") && has("connection", "upgrade")
}

/// The request line target the server of `route` gets for `target`: the path as it is, or with the prefix replaced by the
/// path of the route's URL.
fn upstream_target(route: &ProxyRoute, target: &str) -> String {
    match &route.base {
        Some(base) => format!("{base}{}", &target[route.prefix.len()..]),
        None => target.to_owned(),
    }
}

/// Connects to the server of `route`, trying every address its name has (`localhost` may name an IPv6 address first, where a
/// server listening on IPv4 only is not), and starts TLS for an `https://` target.
fn connect(route: &ProxyRoute, network: &Network) -> Result<Upstream, String> {
    let host = route.host.trim_start_matches('[').trim_end_matches(']');
    let addrs = (host, route.port).to_socket_addrs().map_err(|_| "the host is unknown".to_owned())?;
    let mut last = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, UPSTREAM_CONNECT) {
            Ok(tcp) => {
                tcp.set_read_timeout(Some(UPSTREAM_IDLE)).map_err(|e| e.to_string())?;
                tcp.set_write_timeout(Some(UPSTREAM_CONNECT)).map_err(|e| e.to_string())?;
                if !route.tls {
                    return Ok(Upstream::Plain(tcp));
                }
                let tls = kargo_util::http::tls_connect(network, host, tcp, !route.insecure).map_err(|e| format!("{e:#}"))?;
                return Ok(Upstream::Tls(Box::new(tls)));
            }
            Err(e) => last = Some(e),
        }
    }
    Err(last.map_or_else(|| "the host has no address".to_owned(), |e| e.to_string()))
}

fn is_idle(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
}

/// Sends `request` to the server of `route` and relays what it answers, as it comes. The connection is closed after one
/// exchange (`Connection: close`), so the response ends when the server closes it and needs no parsing; a server that is
/// not there is a 502. A WebSocket upgrade is not closed: once the server has answered, the two sides talk through Kargo
/// until one of them hangs up.
fn forward(stream: &mut TcpStream, request: &Request, route: &ProxyRoute, network: &Network) -> io::Result<()> {
    let bad_gateway = |stream: &mut TcpStream, why: String| {
        respond(stream, "502 Bad Gateway", "text/plain", format!("could not reach {}:{}: {why}", route.host, route.port).as_bytes(), false)
    };
    let mut upstream = match connect(route, network) {
        Ok(upstream) => upstream,
        Err(why) => return bad_gateway(stream, why),
    };
    let upgrade = is_websocket_upgrade(request);
    // The port is left out when it is the scheme's own, as a browser would.
    let default_port = if route.tls { 443 } else { 80 };
    let host = if route.port == default_port { route.host.clone() } else { format!("{}:{}", route.host, route.port) };
    let mut head = format!("{} {} HTTP/1.1\r\nHost: {host}\r\n", request.method, upstream_target(route, &request.target));
    if !upgrade {
        head.push_str("Connection: close\r\n");
    }
    for (name, value) in request.headers.iter().filter(|(name, _)| !is_hop_header(name, upgrade)) {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    if !request.body.is_empty() {
        head.push_str(&format!("Content-Length: {}\r\n", request.body.len()));
    }
    head.push_str("\r\n");
    if let Err(e) = upstream.write_all(head.as_bytes()).and_then(|()| upstream.write_all(&request.body)).and_then(|()| upstream.flush()) {
        return bad_gateway(stream, e.to_string());
    }
    if upgrade {
        return tunnel(stream, &mut upstream);
    }
    let mut buf = [0u8; 16 * 1024];
    loop {
        match upstream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => stream.write_all(&buf[..n])?,
            // A TLS server that closes without saying so ends its response too.
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
    }
    stream.flush()
}

/// Passes bytes between the page and the server, both ways, until either side closes.
fn tunnel(client: &mut TcpStream, upstream: &mut Upstream) -> io::Result<()> {
    client.set_read_timeout(Some(TUNNEL_POLL))?;
    upstream.socket().set_read_timeout(Some(TUNNEL_POLL))?;
    let mut buf = [0u8; 16 * 1024];
    loop {
        match client.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                upstream.write_all(&buf[..n])?;
                upstream.flush()?;
            }
            Err(e) if is_idle(&e) => {}
            Err(e) => return Err(e),
        }
        match upstream.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                client.write_all(&buf[..n])?;
                client.flush()?;
            }
            Err(e) if is_idle(&e) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        }
    }
}

/// Answers a page's request to [`RELOAD_PATH`] with an event stream: the generation now, then the new one after each
/// [`Shared::reload`], until the page goes away or the server stops.
fn stream_reloads(stream: &mut TcpStream, shared: &Shared) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n"
    )?;
    let mut seen = shared.generation();
    write!(stream, "data: {seen}\n\n")?;
    stream.flush()?;
    while !shared.stopped.load(Ordering::Relaxed) {
        let current = shared.wait_for_reload(seen, KEEP_ALIVE);
        if current == seen {
            write!(stream, ": keep-alive\n\n")?;
        } else {
            seen = current;
            write!(stream, "data: {seen}\n\n")?;
        }
        stream.flush()?;
    }
    Ok(())
}

/// `page` with [`RELOAD_SCRIPT`] before its closing `</body>`, else at its end. A page that is not UTF-8 is left alone.
fn with_reload_script(page: Vec<u8>) -> Vec<u8> {
    let text = match String::from_utf8(page) {
        Ok(text) => text,
        Err(not_utf8) => return not_utf8.into_bytes(),
    };
    let at = text.to_ascii_lowercase().rfind("</body>").unwrap_or(text.len());
    format!("{}{RELOAD_SCRIPT}{}", &text[..at], &text[at..]).into_bytes()
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
    fn live_reload_adds_its_script_to_html_pages_only() {
        let dir = tempfile::tempdir().unwrap();
        let live = Server::start(bind(0).unwrap(), site(dir.path()).live_reload(), None).unwrap();
        let home = get(&live, "/");
        let at = home.find("<script>").expect("the script is added");
        assert!(home.contains("/__kargo/reload") && !home.ends_with("<h1>home</h1>") && home[..at].ends_with("<h1>home</h1>"), "{home}");
        assert!(!get(&live, "/app.mjs").contains("EventSource"));
        // Content-Length counts the script.
        let (head, body) = home.split_once("\r\n\r\n").unwrap();
        assert!(head.contains(&format!("Content-Length: {}", body.len())), "{home}");

        std::fs::write(dir.path().join("root/page.html"), "<body>hi</BODY>").unwrap();
        let page = get(&live, "/page.html");
        assert!(page.contains("<body>hi<script>") && page.ends_with("</BODY>"), "{page}");

        let plain = Server::start(bind(0).unwrap(), site(dir.path()), None).unwrap();
        assert!(get(&plain, "/").ends_with("<h1>home</h1>"));
        assert!(get(&plain, RELOAD_PATH).starts_with("HTTP/1.1 404"));
    }

    #[test]
    fn open_pages_are_told_to_reload() {
        use std::io::BufRead;

        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(bind(0).unwrap(), site(dir.path()).live_reload(), None).unwrap();
        let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, server.port())).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        (&stream).write_all(format!("GET {RELOAD_PATH} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n", server.port()).as_bytes()).unwrap();
        let mut reader = BufReader::new(&stream);
        let mut next_event = || loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0, "the stream ended");
            if let Some(data) = line.strip_prefix("data: ") {
                return data.trim().to_owned();
            }
            if line.starts_with("HTTP/1.1") {
                assert!(line.starts_with("HTTP/1.1 200"), "{line}");
            }
        };

        let first = next_event();
        server.reload();
        let second = next_event();
        assert_ne!(first, second);
        server.reload();
        assert_ne!(next_event(), second);
    }

    /// A server that answers one request with `answer(request)`, and the port it listens on.
    fn upstream(answer: impl FnOnce(String) -> String + Send + 'static) -> u16 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            // The head and, for the request below, the 4-byte body.
            while !String::from_utf8_lossy(&request).contains("\r\n\r\n") || (request.starts_with(b"POST") && !request.ends_with(b"data")) {
                let n = stream.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
            }
            stream.write_all(answer(String::from_utf8_lossy(&request).into_owned()).as_bytes()).unwrap();
        });
        port
    }

    fn route(prefix: &str, target: &str) -> ProxyRoute {
        ProxyRoute::parse(prefix, target, false).unwrap()
    }

    #[test]
    fn forwards_the_requests_below_a_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let api = upstream(|request| {
            assert!(request.starts_with("GET /api/users?id=7 HTTP/1.1\r\n"), "{request}");
            assert!(request.contains("Host: localhost:"), "the server sees its own host:\n{request}");
            assert!(request.contains("Connection: close") && request.contains("Accept: application/json"), "{request}");
            assert!(!request.contains("127.0.0.1"), "{request}");
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"users\":[]}".to_owned()
        });
        let site = site(dir.path()).proxy(&[route("/api", &format!("http://localhost:{api}"))], &Network::default());
        let server = Server::start(bind(0).unwrap(), site, None).unwrap();
        let response = exchange(
            server.port(),
            &format!(
                "GET /api/users?id=7 HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAccept: application/json\r\nConnection: keep-alive\r\n\r\n",
                server.port()
            ),
        );
        assert!(response.starts_with("HTTP/1.1 200 OK") && response.ends_with("{\"users\":[]}"), "{response}");
        // What is not below the prefix is still the site's.
        assert!(get(&server, "/app.mjs").ends_with("export {}"));
        assert!(get(&server, "/apiary.txt").starts_with("HTTP/1.1 404"), "`/apiary` is not below `/api`");
    }

    #[test]
    fn the_prefix_is_replaced_by_the_path_of_the_target_and_bodies_go_along() {
        let dir = tempfile::tempdir().unwrap();
        let api = upstream(|request| {
            assert!(request.starts_with("POST /backend/save HTTP/1.1\r\n"), "{request}");
            assert!(request.contains("Content-Length: 4") && request.ends_with("data"), "{request}");
            "HTTP/1.1 201 Created\r\nConnection: close\r\n\r\n".to_owned()
        });
        let site = site(dir.path()).proxy(&[route("/api", &format!("http://localhost:{api}/backend/"))], &Network::default());
        let server = Server::start(bind(0).unwrap(), site, None).unwrap();
        let response = exchange(
            server.port(),
            &format!("POST /api/save HTTP/1.1\r\nHost: localhost:{}\r\nContent-Length: 4\r\n\r\ndata", server.port()),
        );
        assert!(response.starts_with("HTTP/1.1 201 Created"), "{response}");
    }

    #[test]
    fn a_server_that_is_not_there_is_a_bad_gateway() {
        let dir = tempfile::tempdir().unwrap();
        // A port that was just free.
        let closed = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap().local_addr().unwrap().port();
        let site = site(dir.path()).proxy(&[route("/api", &format!("http://127.0.0.1:{closed}"))], &Network::default());
        let server = Server::start(bind(0).unwrap(), site, None).unwrap();
        let response = get(&server, "/api/users");
        assert!(response.starts_with("HTTP/1.1 502 Bad Gateway") && response.contains("could not reach 127.0.0.1"), "{response}");
        // Another site cannot use the proxy through its own host name either.
        let foreign = exchange(server.port(), "GET /api/users HTTP/1.1\r\nHost: evil.example\r\n\r\n");
        assert!(foreign.starts_with("HTTP/1.1 403"), "{foreign}");
    }

    trait Duplex: Read + Write {}
    impl<T: Read + Write> Duplex for T {}

    /// A server that takes one connection, over TLS with a self-signed certificate for `localhost` if `tls`, and
    /// hands it to `handler`. Returns its port.
    fn serve(tls: bool, handler: impl FnOnce(&mut dyn Duplex) + Send + 'static) -> u16 {
        use rustls::pki_types::pem::PemObject;
        use rustls::pki_types::{CertificateDer, PrivateKeyDer};
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut tcp, _) = listener.accept().unwrap();
            if !tls {
                return handler(&mut tcp);
            }
            let certs = vec![CertificateDer::from_pem_slice(include_bytes!("../testdata/localhost-cert.pem")).unwrap()];
            let key = PrivateKeyDer::from_pem_slice(include_bytes!("../testdata/localhost-key.pem")).unwrap();
            let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(certs, key)
                .unwrap();
            let mut stream = rustls::StreamOwned::new(rustls::ServerConnection::new(Arc::new(config)).unwrap(), tcp);
            handler(&mut stream);
            stream.conn.send_close_notify();
            let _ = stream.flush();
        });
        port
    }

    /// The head of a request, up to its blank line.
    fn read_head(stream: &mut dyn Duplex) -> String {
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            if stream.read(&mut byte).unwrap() == 0 {
                break;
            }
            head.push(byte[0]);
        }
        String::from_utf8_lossy(&head).into_owned()
    }

    #[test]
    fn forwards_to_https_servers() {
        let dir = tempfile::tempdir().unwrap();
        let api = serve(true, |stream| {
            let head = read_head(stream);
            assert!(head.starts_with("GET /api/users HTTP/1.1\r\n") && head.contains("Host: localhost:"), "{head}");
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\n[\"ada\"]").unwrap();
        });
        // The certificate is self-signed, so the route has to say it does not mind.
        let insecure = ProxyRoute::parse("/api", &format!("https://localhost:{api}"), true).unwrap();
        let server = Server::start(bind(0).unwrap(), site(dir.path()).proxy(&[insecure], &Network::default()), None).unwrap();
        let response = get(&server, "/api/users");
        assert!(response.starts_with("HTTP/1.1 200 OK") && response.ends_with("[\"ada\"]"), "{response}");
    }

    #[test]
    fn a_certificate_nobody_trusts_is_a_bad_gateway() {
        let dir = tempfile::tempdir().unwrap();
        let api = serve(true, |stream| {
            let _ = stream.read(&mut [0u8; 1]);
        });
        let strict = route("/api", &format!("https://localhost:{api}"));
        let server = Server::start(bind(0).unwrap(), site(dir.path()).proxy(&[strict], &Network::default()), None).unwrap();
        let response = get(&server, "/api/users");
        assert!(response.starts_with("HTTP/1.1 502 Bad Gateway") && response.contains("certificate"), "{response}");
    }

    /// Opens a WebSocket through the proxy to a server that answers in capitals, and talks to it.
    fn websocket_through_the_proxy(tls: bool) {
        let dir = tempfile::tempdir().unwrap();
        let api = serve(tls, |stream| {
            let head = read_head(stream);
            assert!(head.starts_with("GET /socket HTTP/1.1\r\n"), "{head}");
            assert!(
                head.contains("Upgrade: websocket") && head.contains("Connection: Upgrade") && head.contains("Sec-WebSocket-Key: abc"),
                "{head}"
            );
            assert!(!head.contains("Connection: close"), "an upgrade keeps the connection:\n{head}");
            stream.write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n").unwrap();
            let mut word = [0u8; 5];
            stream.read_exact(&mut word).unwrap();
            stream.write_all(&word.to_ascii_uppercase()).unwrap();
            stream.read_exact(&mut word).unwrap();
            stream.write_all(&word.to_ascii_uppercase()).unwrap();
        });
        let scheme = if tls { "wss" } else { "ws" };
        let route = ProxyRoute::parse("/socket", &format!("{scheme}://localhost:{api}"), tls).unwrap();
        let server = Server::start(bind(0).unwrap(), site(dir.path()).proxy(&[route], &Network::default()), None).unwrap();
        let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, server.port())).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        write!(
            client,
            "GET /socket HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: abc\r\nSec-WebSocket-Version: 13\r\n\r\n",
            server.port()
        )
        .unwrap();
        let head = read_head(&mut client);
        assert!(head.starts_with("HTTP/1.1 101 Switching Protocols"), "{head}");
        // Both directions work, and more than once.
        for word in [b"hello", b"world"] {
            client.write_all(word).unwrap();
            let mut answer = [0u8; 5];
            client.read_exact(&mut answer).unwrap();
            assert_eq!(&answer, &word.to_ascii_uppercase().as_slice());
        }
    }

    #[test]
    fn websockets_go_through_the_proxy() {
        websocket_through_the_proxy(false);
    }

    #[test]
    fn websockets_go_through_the_proxy_over_tls() {
        websocket_through_the_proxy(true);
    }

    #[test]
    fn upstream_targets() {
        assert_eq!(upstream_target(&route("/api", "http://h:1"), "/api/x?y=1"), "/api/x?y=1");
        assert_eq!(upstream_target(&route("/api", "http://h:1/backend/"), "/api/x?y=1"), "/backend/x?y=1");
        assert_eq!(upstream_target(&route("/api", "http://h:1/backend"), "/api?y=1"), "/backend?y=1");
        assert!(is_hop_header("Connection", false) && is_hop_header("proxy-authorization", false) && !is_hop_header("Accept", false));
        assert!(!is_hop_header("Upgrade", true) && is_hop_header("Host", true));
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("/a%20b/%C3%A9").as_deref(), Some("/a b/é"));
        assert_eq!(percent_decode("/100%"), None);
        assert_eq!(percent_decode("/%zz"), None);
        assert_eq!(percent_decode("/%ff"), None, "not UTF-8");
    }
}
