//! A tiny scriptable HTTP/1.1 server for end-to-end tests.
//!
//! Unlike general-purpose mock servers it can stream a body in chunks of a
//! chosen size, stall, keep a stream open with keep-alive comments, and report
//! when the client disconnects (to verify cancellation).

#![allow(dead_code)]

pub mod fixtures;

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use envelope_ai::{AiClient, ClientOptions, ProviderConfig, ProviderKind};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

/// A request as received by the server.
#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    /// Path including the query string.
    pub path: String,
    /// Header names are lowercased.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).expect("request body is JSON")
    }
}

/// What happens after the scripted chunks were written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum After {
    /// Finish the chunked body and close.
    Close,
    /// Keep sending SSE keep-alive comments until the client disconnects.
    KeepAlive,
    /// Send nothing more and wait for the client to disconnect.
    Stall,
}

/// A scripted response.
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub chunks: Vec<Vec<u8>>,
    pub chunk_delay: Duration,
    pub head_delay: Duration,
    pub after: After,
}

impl Reply {
    pub fn new(status: u16, content_type: &str, body: &[u8]) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), content_type.into())],
            chunks: vec![body.to_vec()],
            chunk_delay: Duration::ZERO,
            head_delay: Duration::ZERO,
            after: After::Close,
        }
    }

    /// A 200 `text/event-stream` response.
    pub fn sse(body: &str) -> Self {
        Self::new(200, "text/event-stream", body.as_bytes())
    }

    /// A JSON response with the given status.
    pub fn json(status: u16, body: &str) -> Self {
        Self::new(status, "application/json", body.as_bytes())
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Re-splits the body into chunks of `size` bytes (each flushed separately).
    pub fn chunked(mut self, size: usize) -> Self {
        let body: Vec<u8> = self.chunks.concat();
        self.chunks = body.chunks(size.max(1)).map(<[u8]>::to_vec).collect();
        self.chunk_delay = Duration::from_millis(1);
        self
    }

    pub fn head_delay(mut self, delay: Duration) -> Self {
        self.head_delay = delay;
        self
    }

    pub fn then(mut self, after: After) -> Self {
        self.after = after;
        self
    }
}

struct Route {
    method: String,
    path: String,
    replies: VecDeque<Reply>,
}

#[derive(Default)]
struct State {
    routes: Mutex<Vec<Route>>,
    requests: Mutex<Vec<Recorded>>,
    disconnects: AtomicUsize,
    disconnected: Notify,
}

impl State {
    fn mark_disconnect(&self) {
        self.disconnects.fetch_add(1, Ordering::SeqCst);
        self.disconnected.notify_waiters();
        self.disconnected.notify_one();
    }
}

pub struct MockServer {
    pub addr: SocketAddr,
    state: Arc<State>,
}

impl MockServer {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let state = Arc::new(State::default());
        let accept_state = Arc::clone(&state);
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                tokio::spawn(handle(socket, Arc::clone(&accept_state)));
            }
        });
        Self { addr, state }
    }

    /// `http://127.0.0.1:PORT{path}`
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    /// Registers replies for `method path` (query string ignored). Replies are
    /// used in order; the last one repeats.
    pub fn route(&self, method: &str, path: &str, replies: Vec<Reply>) {
        self.state.routes.lock().unwrap().push(Route {
            method: method.into(),
            path: path.into(),
            replies: replies.into(),
        });
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.state.requests.lock().unwrap().clone()
    }

    pub fn disconnects(&self) -> usize {
        self.state.disconnects.load(Ordering::SeqCst)
    }

    /// Waits until at least one client disconnected mid-response.
    pub async fn wait_for_disconnect(&self, timeout: Duration) -> bool {
        let wait = async {
            loop {
                if self.disconnects() > 0 {
                    return;
                }
                let notified = self.state.disconnected.notified();
                if self.disconnects() > 0 {
                    return;
                }
                let _ = tokio::time::timeout(Duration::from_millis(20), notified).await;
            }
        };
        tokio::time::timeout(timeout, wait).await.is_ok()
    }
}

async fn handle(mut socket: TcpStream, state: Arc<State>) {
    let Some(request) = read_request(&mut socket).await else {
        return;
    };
    let path_only = request
        .path
        .split('?')
        .next()
        .unwrap_or_default()
        .to_owned();
    let method = request.method.clone();
    state.requests.lock().unwrap().push(request);
    let reply = {
        let mut routes = state.routes.lock().unwrap();
        routes
            .iter_mut()
            .find(|r| r.method == method && r.path == path_only)
            .and_then(|route| {
                if route.replies.len() > 1 {
                    route.replies.pop_front()
                } else {
                    route.replies.front().cloned()
                }
            })
    };
    let reply = reply.unwrap_or_else(|| Reply::new(404, "text/plain", b"Not Found"));
    respond(&mut socket, &reply, &state).await;
}

async fn read_request(socket: &mut TcpStream) -> Option<Recorded> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 8192];
    let head_end = loop {
        if let Some(pos) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split(' ');
    let method = request_line.next()?.to_owned();
    let path = request_line.next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let length = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buffer[head_end..].to_vec();
    while body.len() < length {
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    Some(Recorded {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

/// Sleeps for `duration` unless the client disconnects first (returns true then).
async fn wait_or_disconnect(socket: &mut TcpStream, duration: Duration) -> bool {
    let mut probe = [0_u8; 64];
    tokio::select! {
        read = socket.read(&mut probe) => matches!(read, Ok(0) | Err(_)),
        () = tokio::time::sleep(duration) => false,
    }
}

async fn write_chunk(socket: &mut TcpStream, data: &[u8]) -> std::io::Result<()> {
    socket
        .write_all(format!("{:x}\r\n", data.len()).as_bytes())
        .await?;
    socket.write_all(data).await?;
    socket.write_all(b"\r\n").await?;
    socket.flush().await
}

async fn respond(socket: &mut TcpStream, reply: &Reply, state: &State) {
    if !reply.head_delay.is_zero() && wait_or_disconnect(socket, reply.head_delay).await {
        state.mark_disconnect();
        return;
    }
    let mut head = format!("HTTP/1.1 {} Mock\r\n", reply.status);
    for (name, value) in &reply.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("transfer-encoding: chunked\r\nconnection: close\r\n\r\n");
    if socket.write_all(head.as_bytes()).await.is_err() {
        state.mark_disconnect();
        return;
    }
    for chunk in &reply.chunks {
        if chunk.is_empty() {
            continue;
        }
        if write_chunk(socket, chunk).await.is_err() {
            state.mark_disconnect();
            return;
        }
        if !reply.chunk_delay.is_zero() && wait_or_disconnect(socket, reply.chunk_delay).await {
            state.mark_disconnect();
            return;
        }
    }
    match reply.after {
        After::Close => {
            let _ = socket.write_all(b"0\r\n\r\n").await;
            let _ = socket.flush().await;
            let _ = socket.shutdown().await;
        }
        After::KeepAlive => loop {
            if wait_or_disconnect(socket, Duration::from_millis(20)).await
                || write_chunk(socket, b": keep-alive\n\n").await.is_err()
            {
                state.mark_disconnect();
                return;
            }
        },
        After::Stall => loop {
            if wait_or_disconnect(socket, Duration::from_secs(3600)).await {
                state.mark_disconnect();
                return;
            }
        },
    }
}

/// An HTTP client that never uses a system proxy.
pub fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("http client")
}

/// An [`AiClient`] for the mock server.
pub fn client(kind: ProviderKind, base_url: String, key: Option<&str>, model: &str) -> AiClient {
    client_with(kind, base_url, key, model, ClientOptions::default())
}

pub fn client_with(
    kind: ProviderKind,
    base_url: String,
    key: Option<&str>,
    model: &str,
    options: ClientOptions,
) -> AiClient {
    let config = ProviderConfig::new(kind, base_url, key.map(str::to_owned), model);
    AiClient::with_http_client(config, http(), options).expect("valid config")
}
