//! A minimal scripted HTTP/1.1 server used to exercise the client end to end
//! (authentication, redirects, error statuses) without a real DAV server.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// A request received by the mock server.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    /// Request target as sent (`/path?query`).
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Request {
    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The path part of the target.
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or_default()
    }

    /// `http://host:port` as seen in the `Host` header.
    pub fn origin(&self) -> String {
        format!("http://{}", self.header("host").unwrap_or("127.0.0.1"))
    }
}

/// A scripted response.
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Reply {
    pub fn new(status: u16) -> Self {
        Reply {
            status,
            headers: Vec::new(),
            body: String::new(),
        }
    }

    /// `207 Multi-Status` with an XML body.
    pub fn multistatus(body: impl Into<String>) -> Self {
        Reply::new(207)
            .header("Content-Type", "application/xml; charset=utf-8")
            .body(body)
    }

    /// A redirect to `location`.
    pub fn redirect(status: u16, location: &str) -> Self {
        Reply::new(status).header("Location", location)
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    pub fn body(mut self, body: impl Into<String>) -> Self {
        self.body = body.into();
        self
    }
}

type Handler = dyn Fn(&Request) -> Reply + Send + Sync;

/// A running mock server; stops when dropped.
pub struct MockServer {
    pub addr: SocketAddr,
    requests: Arc<Mutex<Vec<Request>>>,
    task: JoinHandle<()>,
}

impl MockServer {
    /// Starts a server on an ephemeral loopback port answering every request
    /// with `handler`.
    pub async fn start(handler: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> MockServer {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock server");
        let addr = listener.local_addr().expect("local addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let handler: Arc<Handler> = Arc::new(handler);
        let recorded = Arc::clone(&requests);
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let handler = Arc::clone(&handler);
                let recorded = Arc::clone(&recorded);
                tokio::spawn(async move {
                    let _ = serve_connection(stream, handler, recorded).await;
                });
            }
        });
        MockServer {
            addr,
            requests,
            task,
        }
    }

    /// `http://127.0.0.1:port`.
    pub fn origin(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Absolute URL for `path` on this server.
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.origin())
    }

    /// Every request received so far, in order.
    pub fn requests(&self) -> Vec<Request> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve_connection(
    stream: TcpStream,
    handler: Arc<Handler>,
    recorded: Arc<Mutex<Vec<Request>>>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);
    loop {
        let mut request_line = String::new();
        if reader.read_line(&mut request_line).await? == 0 {
            return Ok(());
        }
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_owned();
        let target = parts.next().unwrap_or_default().to_owned();

        let mut headers = Vec::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).await? == 0 {
                return Ok(());
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                headers.push((name.trim().to_owned(), value.trim().to_owned()));
            }
        }
        let length = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = vec![0u8; length];
        reader.read_exact(&mut body).await?;

        let request = Request {
            method,
            target,
            headers,
            body: String::from_utf8_lossy(&body).into_owned(),
        };
        recorded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request.clone());
        let reply = handler(&request);

        let mut response = format!("HTTP/1.1 {} {}\r\n", reply.status, reason(reply.status));
        for (name, value) in &reply.headers {
            response.push_str(&format!("{name}: {value}\r\n"));
        }
        response.push_str(&format!("Content-Length: {}\r\n\r\n", reply.body.len()));
        response.push_str(&reply.body);
        reader.get_mut().write_all(response.as_bytes()).await?;
        reader.get_mut().flush().await?;
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        207 => "Multi-Status",
        301 => "Moved Permanently",
        302 => "Found",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        412 => "Precondition Failed",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        507 => "Insufficient Storage",
        _ => "Status",
    }
}

/// Loads a fixture from `tests/fixtures`.
pub fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("cannot read {path}: {err}"))
}
