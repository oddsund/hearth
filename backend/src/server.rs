//! Hearth's window is a browser app window (Edge `--app` on Windows) showing pages this server
//! hands out on 127.0.0.1. Other programs and web pages on the PC can reach that port too, so:
//! - a window gets in only through a one-time launch link that Hearth opens itself; the link
//!   is swapped for a session cookie (HttpOnly, SameSite=Strict) that is new every start;
//! - the Host header must say 127.0.0.1 or localhost (stops DNS rebinding);
//! - API calls must be same-origin JSON POSTs (stops other sites' forms and scripts).
//!
//! The UI calls `POST /api/<command>` with its arguments as JSON, and gets live progress from
//! `GET /api/events` (server-sent events). The server exits once no window has been open for a
//! little while and nothing is running, like closing the app used to.

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderMap, Method, StatusCode, Uri};
use axum::response::Response;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub type Events = tokio::sync::broadcast::Sender<String>;
pub type Reply = Pin<Box<dyn Future<Output = Result<serde_json::Value, String>> + Send>>;
pub type Handler = Arc<dyn Fn(String, serde_json::Value) -> Reply + Send + Sync>;

pub const DEFAULT_PORT: u16 = 47820;
/// How long a launch link works. It's used within a second or two of being made.
const LAUNCH_TTL: Duration = Duration::from_secs(60);
/// Exit after this long with no window open and nothing running...
const IDLE_EXIT: Duration = Duration::from_secs(30);
/// ...but give the first window time to appear.
const STARTUP_GRACE: Duration = Duration::from_secs(90);

const INDEX: &str = include_str!("../../ui/index.html");
const APP_JS: &str = include_str!("../../ui/app.js");
const STYLE: &str = include_str!("../../ui/style.css");
const ICON: &[u8] = include_bytes!("../icons/icon.png");

const CSP: &str = "default-src 'self'; img-src 'self' data:; object-src 'none'; base-uri 'none'; \
                   form-action 'none'; frame-ancestors 'none'";

/// Send an event to every open window.
pub fn emit(ev: &Events, event: &str, payload: impl serde::Serialize) {
    if let Ok(s) = serde_json::to_string(&serde_json::json!({ "event": event, "payload": payload })) {
        let _ = ev.send(s); // Err = no window is listening
    }
}

struct Shared {
    port: u16,
    /// Session cookie value.
    session: String,
    launch_keys: Mutex<Vec<(String, Instant)>>,
    /// Open event streams, i.e. open windows.
    windows: AtomicUsize,
    /// API calls still running.
    calls: AtomicUsize,
    events: Events,
    handler: Handler,
}

#[derive(Clone)]
pub struct Server(Arc<Shared>);

fn random_hex() -> String {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).expect("no random numbers from the OS");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Compare secrets without leaking how much matched through timing.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl Server {
    pub fn new(port: u16, events: Events, handler: Handler) -> Self {
        Server(Arc::new(Shared {
            port,
            session: random_hex(),
            launch_keys: Mutex::new(Vec::new()),
            windows: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
            events,
            handler,
        }))
    }

    /// A link that lets one window in.
    pub fn launch_url(&self) -> String {
        let key = random_hex();
        let mut keys = self.0.launch_keys.lock().unwrap_or_else(|e| e.into_inner());
        keys.retain(|(_, t)| t.elapsed() < LAUNCH_TTL);
        keys.push((key.clone(), Instant::now()));
        format!("http://127.0.0.1:{}/launch?k={key}", self.0.port)
    }

    pub fn open_window(&self) {
        crate::browser::open(&self.launch_url());
    }

    /// Serve until no window has been open for a while.
    pub async fn run(self, listener: tokio::net::TcpListener) -> std::io::Result<()> {
        let s = self.0.clone();
        let idle = async move {
            let start = Instant::now();
            let mut quiet_since: Option<Instant> = None;
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                if s.windows.load(SeqCst) > 0 || s.calls.load(SeqCst) > 0 {
                    quiet_since = None;
                    continue;
                }
                let since = *quiet_since.get_or_insert_with(Instant::now);
                if start.elapsed() > STARTUP_GRACE && since.elapsed() > IDLE_EXIT {
                    break;
                }
            }
        };
        let app = axum::Router::new().fallback(handle).with_state(self.0);
        axum::serve(listener, app).with_graceful_shutdown(idle).await
    }
}

/// Counts something (an open window, a running call) for as long as it lives.
struct Counted(Arc<Shared>, fn(&Shared) -> &AtomicUsize);

impl Counted {
    fn new(s: Arc<Shared>, which: fn(&Shared) -> &AtomicUsize) -> Self {
        which(&s).fetch_add(1, SeqCst);
        Counted(s, which)
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        (self.1)(&self.0).fetch_sub(1, SeqCst);
    }
}

fn windows(s: &Shared) -> &AtomicUsize {
    &s.windows
}

fn calls(s: &Shared) -> &AtomicUsize {
    &s.calls
}

fn respond(status: StatusCode, content_type: &str, body: impl Into<Body>) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_SECURITY_POLICY, CSP)
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::REFERRER_POLICY, "no-referrer")
        .header(header::CACHE_CONTROL, "no-store")
        .body(body.into())
        .expect("valid response")
}

fn json(status: StatusCode, v: &serde_json::Value) -> Response {
    respond(status, "application/json", v.to_string())
}

fn page(status: StatusCode, msg: &str) -> Response {
    let html = format!(
        "<!doctype html><meta charset=utf-8><title>Hearth</title>\
         <body style=\"font:15px system-ui,sans-serif;padding:24px\"><p>{msg}</p>"
    );
    respond(status, "text/html; charset=utf-8", html)
}

fn header_str<'a>(h: &'a HeaderMap, name: header::HeaderName) -> &'a str {
    h.get(name).and_then(|v| v.to_str().ok()).unwrap_or("")
}

fn session_cookie(h: &HeaderMap) -> Option<&str> {
    h.get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|c| c.trim().strip_prefix("hearth="))
}

async fn handle(State(s): State<Arc<Shared>>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    let host = header_str(&headers, header::HOST);
    let ours = |h: &str, scheme: &str| {
        h == format!("{scheme}127.0.0.1:{}", s.port) || h == format!("{scheme}localhost:{}", s.port)
    };
    if !ours(host, "") {
        return page(StatusCode::FORBIDDEN, "Wrong address.");
    }
    let path = uri.path();

    if method == Method::GET && path == "/launch" {
        let key = uri.query().and_then(|q| q.strip_prefix("k=")).unwrap_or("");
        let valid = {
            let mut keys = s.launch_keys.lock().unwrap_or_else(|e| e.into_inner());
            keys.retain(|(_, t)| t.elapsed() < LAUNCH_TTL);
            match keys.iter().position(|(k, _)| same(k, key)) {
                Some(i) => {
                    keys.remove(i); // one window per link
                    true
                }
                None => false,
            }
        };
        if !valid {
            return page(StatusCode::FORBIDDEN, "This link has been used or has expired. Open Hearth again.");
        }
        return Response::builder()
            .status(StatusCode::SEE_OTHER)
            .header(header::LOCATION, "/")
            .header(header::SET_COOKIE, format!("hearth={}; Path=/; HttpOnly; SameSite=Strict", s.session))
            .header(header::CACHE_CONTROL, "no-store")
            .body(Body::empty())
            .expect("valid response");
    }

    if !session_cookie(&headers).is_some_and(|c| same(c, &s.session)) {
        let msg = "This window belongs to a Hearth that has stopped. Close it and open Hearth again.";
        return if path.starts_with("/api/") {
            json(StatusCode::UNAUTHORIZED, &serde_json::Value::String(msg.into()))
        } else {
            page(StatusCode::FORBIDDEN, "Open Hearth from the Start menu (or run <code>hearth</code>) to get a window.")
        };
    }

    match (&method, path) {
        (&Method::GET, "/" | "/index.html") => respond(StatusCode::OK, "text/html; charset=utf-8", INDEX),
        (&Method::GET, "/app.js") => respond(StatusCode::OK, "text/javascript; charset=utf-8", APP_JS),
        (&Method::GET, "/style.css") => respond(StatusCode::OK, "text/css; charset=utf-8", STYLE),
        (&Method::GET, "/icon.png") => respond(StatusCode::OK, "image/png", ICON),
        (&Method::GET, "/api/events") => events(s),
        (&Method::POST, p) if p.starts_with("/api/") => {
            // Browsers always send Origin on POST, and other sites can't send JSON here without
            // a CORS preflight, which this server never answers.
            let same_origin = ours(header_str(&headers, header::ORIGIN), "http://");
            let is_json = header_str(&headers, header::CONTENT_TYPE).starts_with("application/json");
            if !same_origin || !is_json {
                return json(StatusCode::FORBIDDEN, &"Refused".into());
            }
            let cmd = p["/api/".len()..].to_string();
            api(s, cmd, body).await
        }
        _ => page(StatusCode::NOT_FOUND, "Not found."),
    }
}

async fn api(s: Arc<Shared>, cmd: String, body: Bytes) -> Response {
    let args: serde_json::Value = if body.is_empty() {
        serde_json::Value::Null
    } else {
        match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => return json(StatusCode::BAD_REQUEST, &format!("Bad request: {e}").into()),
        }
    };
    let busy = Counted::new(s.clone(), calls);
    let reply = (s.handler)(cmd, args);
    // Spawned, so an install finishes even if its window closes partway.
    let done = tokio::spawn(async move {
        let _busy = busy;
        reply.await
    });
    match done.await {
        Ok(Ok(v)) => json(StatusCode::OK, &v),
        Ok(Err(e)) => json(StatusCode::BAD_REQUEST, &e.into()),
        Err(e) => json(StatusCode::INTERNAL_SERVER_ERROR, &format!("Hearth hit an internal error: {e}").into()),
    }
}

/// Server-sent events: one `data:` line of JSON ({event, payload}) per event.
fn events(s: Arc<Shared>) -> Response {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::convert::Infallible>>(64);
    let mut sub = s.events.subscribe();
    let open = Counted::new(s, windows);
    tokio::spawn(async move {
        let _open = open;
        // Reconnect quickly if the stream drops.
        if tx.send(Ok(Bytes::from_static(b"retry: 2000\n\n"))).await.is_err() {
            return;
        }
        // Pings keep the connection alive and notice a closed window.
        let mut ping = tokio::time::interval(Duration::from_secs(10));
        loop {
            let msg = tokio::select! {
                m = sub.recv() => match m {
                    Ok(m) => format!("data: {m}\n\n"),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
                _ = ping.tick() => ": ping\n\n".to_string(),
                _ = tx.closed() => break,
            };
            if tx.send(Ok(Bytes::from(msg))).await.is_err() {
                break;
            }
        }
    });
    let mut r = respond(StatusCode::OK, "text/event-stream", Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx)));
    r.headers_mut().insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-cache"));
    r
}
