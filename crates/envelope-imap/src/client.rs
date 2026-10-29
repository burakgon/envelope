//! The IMAP client: connection management, command execution and the
//! high-level commands a mail client needs.

use std::future::Future;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use imap_proto::Response;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;

use crate::error::{Error, Result};
use crate::stream::{self, Security, Stream};
use crate::types::{
    Code, FetchItem, ListEntry, MailboxStatus, Resp, Status, Untagged, convert, uid_set_string,
};
use crate::utf7;

/// Connection settings.
#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub accept_invalid_certs: bool,
    /// Timeout for connecting and for each individual read/write.
    pub timeout: Duration,
}

impl Config {
    pub fn new(host: impl Into<String>, port: u16, security: Security) -> Self {
        Config {
            host: host.into(),
            port,
            security,
            accept_invalid_certs: false,
            timeout: Duration::from_secs(60),
        }
    }
}

/// Credentials used to log in.
#[derive(Clone)]
pub enum Auth {
    /// `LOGIN` / `AUTHENTICATE PLAIN` with a password.
    Password { user: String, password: String },
    /// `AUTHENTICATE XOAUTH2` (Gmail, Outlook) with an OAuth2 access token.
    XOAuth2 { user: String, token: String },
    /// `AUTHENTICATE OAUTHBEARER` (RFC 7628).
    OAuthBearer { user: String, token: String },
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Auth::Password { user, .. } => write!(f, "Password({user})"),
            Auth::XOAuth2 { user, .. } => write!(f, "XOAuth2({user})"),
            Auth::OAuthBearer { user, .. } => write!(f, "OAuthBearer({user})"),
        }
    }
}

/// Result of a completed command.
#[derive(Debug, Default)]
pub struct Done {
    pub untagged: Vec<Untagged>,
    pub code: Option<Code>,
    pub text: String,
}

/// State reported by `SELECT` / `EXAMINE`.
#[derive(Debug, Clone, Default)]
pub struct Selected {
    pub mailbox: String,
    pub exists: u32,
    pub recent: u32,
    pub uid_validity: Option<u32>,
    pub uid_next: Option<u32>,
    pub highest_modseq: Option<u64>,
    /// The server explicitly reported `NOMODSEQ` (no CONDSTORE for this mailbox).
    pub no_modseq: bool,
    pub flags: Vec<String>,
    pub permanent_flags: Vec<String>,
    pub read_only: bool,
    /// UIDs reported vanished through QRESYNC.
    pub vanished: Vec<u32>,
    /// Flag changes reported through QRESYNC.
    pub changed: Vec<FetchItem>,
}

/// Parameters for a QRESYNC-enabled `SELECT`.
#[derive(Debug, Clone)]
pub struct QresyncParams {
    pub uid_validity: u32,
    pub modseq: u64,
    /// Optional set of known UIDs (`1:500`) to narrow the VANISHED reply.
    pub known_uids: Option<String>,
}

/// Why an `IDLE` ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleEnd {
    /// The server pushed mailbox changes.
    Changed,
    /// The maximum idle time elapsed without changes.
    Timeout,
    /// The caller asked to stop idling.
    Interrupted,
}

/// One part of a command; literals are sent with proper synchronization.
pub enum Part<'a> {
    Text(&'a str),
    Owned(String),
    Literal(&'a [u8]),
    OwnedLiteral(Vec<u8>),
}

/// An authenticated or not-yet-authenticated IMAP connection.
pub struct Client {
    stream: Stream,
    rbuf: Vec<u8>,
    rstart: usize,
    scan: usize,
    next_tag: u32,
    io_timeout: Duration,
    caps: Vec<String>,
    enabled: Vec<String>,
    selected: Option<String>,
    unsolicited: Vec<Untagged>,
    bye: Option<String>,
    host: String,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("host", &self.host)
            .field("selected", &self.selected)
            .finish()
    }
}

/// Quotes a string as an IMAP `astring`, or returns `None` if it needs a literal.
pub fn quote(s: &str) -> Option<String> {
    if s.bytes()
        .any(|b| b == b'\r' || b == b'\n' || b == 0 || b >= 0x80)
    {
        return None;
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        if ch == '"' || ch == '\\' {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('"');
    Some(out)
}

fn literal_len(line: &[u8]) -> Option<usize> {
    let line = line.strip_suffix(b"}")?;
    let open = line.iter().rposition(|&b| b == b'{')?;
    let inner = &line[open + 1..];
    let digits = inner.strip_suffix(b"+").unwrap_or(inner);
    if digits.is_empty() || digits.len() > 12 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

/// Best-effort interpretation of a frame `imap-proto` could not parse, so a
/// single odd server response never desynchronizes the connection.
fn fallback_parse(frame: &[u8]) -> Resp {
    let line_end = frame
        .iter()
        .position(|&b| b == b'\r' || b == b'\n')
        .unwrap_or(frame.len());
    let line = String::from_utf8_lossy(&frame[..line_end]).to_string();
    if let Some(rest) = line.strip_prefix('+') {
        return Resp::Continue {
            text: rest.trim().to_string(),
        };
    }
    let mut words = line.splitn(3, ' ');
    let first = words.next().unwrap_or_default().to_string();
    let second = words.next().unwrap_or_default().to_ascii_uppercase();
    let rest = words.next().unwrap_or_default().to_string();
    let status = match second.as_str() {
        "OK" => Some(Status::Ok),
        "NO" => Some(Status::No),
        "BAD" => Some(Status::Bad),
        "BYE" => Some(Status::Bye),
        "PREAUTH" => Some(Status::PreAuth),
        _ => None,
    };
    if first == "*" {
        tracing::debug!(response = %line.chars().take(200).collect::<String>(), "unparsed untagged response");
        return match status {
            Some(status) => Resp::Untagged(Untagged::Status {
                status,
                code: None,
                text: rest,
            }),
            None => Resp::Untagged(Untagged::Other),
        };
    }
    Resp::Tagged {
        tag: first,
        status: status.unwrap_or(Status::Bad),
        code: None,
        text: rest,
    }
}

/// Servers may answer one FETCH with several responses for the same message
/// (for example Dovecot adds `$HasAttachment` keywords while parsing and
/// reports the flag change separately). Merge them per sequence number.
fn merge_fetches(items: impl IntoIterator<Item = FetchItem>) -> Vec<FetchItem> {
    let mut out: Vec<FetchItem> = Vec::new();
    let mut index: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    for item in items {
        match index.get(&item.seq) {
            Some(&i) => {
                let target = &mut out[i];
                macro_rules! take {
                    ($($field:ident),*) => { $( if item.$field.is_some() { target.$field = item.$field; } )* };
                }
                take!(
                    uid,
                    flags,
                    modseq,
                    internal_date,
                    size,
                    header,
                    body,
                    bodystructure,
                    gm_msgid,
                    gm_thrid,
                    gm_labels,
                    email_id,
                    thread_id
                );
                target.sections.extend(item.sections);
            }
            None => {
                index.insert(item.seq, out.len());
                out.push(item);
            }
        }
    }
    out
}

impl Client {
    /// Connects, performs TLS (implicit or STARTTLS) and reads the greeting.
    pub async fn connect(config: &Config) -> Result<Client> {
        let tcp = timeout(
            config.timeout,
            stream::tcp_connect(&config.host, config.port),
        )
        .await
        .map_err(|_| Error::Timeout)??;
        let stream = match config.security {
            Security::Tls => timeout(
                config.timeout,
                stream::tls_wrap(tcp, &config.host, config.accept_invalid_certs),
            )
            .await
            .map_err(|_| Error::Timeout)??,
            _ => Stream::Plain(tcp),
        };
        let mut client = Client {
            stream,
            rbuf: Vec::with_capacity(64 * 1024),
            rstart: 0,
            scan: 0,
            next_tag: 0,
            io_timeout: config.timeout,
            caps: Vec::new(),
            enabled: Vec::new(),
            selected: None,
            unsolicited: Vec::new(),
            bye: None,
            host: config.host.clone(),
        };
        client.read_greeting().await?;

        if config.security == Security::StartTls {
            if client.caps.is_empty() {
                client.capability().await?;
            }
            if !client.has_cap("STARTTLS") {
                return Err(Error::Unsupported("STARTTLS"));
            }
            client.run("STARTTLS", &[Part::Text("STARTTLS")]).await?;
            client.caps.clear();
            let Stream::Plain(tcp) = std::mem::replace(&mut client.stream, Stream::Detached) else {
                return Err(Error::Protocol("STARTTLS on an encrypted stream".into()));
            };
            client.stream = timeout(
                config.timeout,
                stream::tls_wrap(tcp, &config.host, config.accept_invalid_certs),
            )
            .await
            .map_err(|_| Error::Timeout)??;
            client.rbuf.clear();
            client.rstart = 0;
            client.scan = 0;
        }
        if client.caps.is_empty() {
            client.capability().await?;
        }
        Ok(client)
    }

    async fn read_greeting(&mut self) -> Result<()> {
        match self.read_resp().await? {
            Resp::Untagged(Untagged::Status { status, code, text }) => match status {
                Status::Ok | Status::PreAuth => {
                    if let Some(Code::Capabilities(c)) = code {
                        self.caps = c;
                    }
                    Ok(())
                }
                Status::Bye => Err(Error::Bye(text)),
                _ => Err(Error::Protocol(format!("unexpected greeting: {text}"))),
            },
            other => Err(Error::Protocol(format!("unexpected greeting: {other:?}"))),
        }
    }

    /// Server capabilities (upper-cased).
    pub fn capabilities(&self) -> &[String] {
        &self.caps
    }

    /// Whether the server advertises a capability (case-insensitive).
    pub fn has_cap(&self, cap: &str) -> bool {
        self.caps.iter().any(|c| c.eq_ignore_ascii_case(cap))
    }

    /// Whether an extension was turned on with `ENABLE`.
    pub fn is_enabled(&self, cap: &str) -> bool {
        self.enabled.iter().any(|c| c.eq_ignore_ascii_case(cap))
    }

    /// Whether this is a Gmail server (X-GM-EXT-1).
    pub fn is_gmail(&self) -> bool {
        self.has_cap("X-GM-EXT-1")
    }

    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// Takes untagged responses that arrived unsolicited during earlier commands
    /// (new mail `EXISTS`, `EXPUNGE`, flag changes from other clients, …).
    pub fn take_unsolicited(&mut self) -> Vec<Untagged> {
        std::mem::take(&mut self.unsolicited)
    }

    fn mailbox_parts(&self, name: &str) -> Vec<Part<'static>> {
        let encoded = if self.is_enabled("UTF8=ACCEPT") {
            name.to_string()
        } else {
            utf7::encode(name)
        };
        match quote(&encoded) {
            Some(q) => vec![Part::Owned(q)],
            None => vec![Part::OwnedLiteral(encoded.into_bytes())],
        }
    }

    // ----- low level I/O -------------------------------------------------

    fn frame_end(&mut self) -> Option<usize> {
        let mut pos = self.scan.max(self.rstart);
        loop {
            let line_start = pos;
            let Some(off) = self.rbuf[pos..].iter().position(|&b| b == b'\n') else {
                self.scan = line_start;
                return None;
            };
            let nl = pos + off;
            let mut le = nl;
            if le > line_start && self.rbuf[le - 1] == b'\r' {
                le -= 1;
            }
            if let Some(n) = literal_len(&self.rbuf[line_start..le]) {
                let lit_end = nl + 1 + n;
                if lit_end > self.rbuf.len() {
                    self.scan = line_start;
                    // Make sure the buffer can take the whole literal in one go.
                    self.rbuf.reserve(lit_end - self.rbuf.len());
                    return None;
                }
                pos = lit_end;
                continue;
            }
            self.scan = nl + 1;
            return Some(nl + 1 - self.rstart);
        }
    }

    /// Reads one complete response from the server.
    pub async fn read_resp(&mut self) -> Result<Resp> {
        loop {
            if let Some(len) = self.frame_end() {
                let frame = &self.rbuf[self.rstart..self.rstart + len];
                let resp = match Response::parse(frame) {
                    Ok((_, r)) => convert(r),
                    Err(_) => fallback_parse(frame),
                };
                self.rstart += len;
                if self.rstart >= self.rbuf.len() {
                    self.rbuf.clear();
                    self.rstart = 0;
                    self.scan = 0;
                }
                return Ok(resp);
            }
            if self.rstart > 0 && self.rstart >= self.rbuf.len() / 2 {
                self.rbuf.drain(..self.rstart);
                self.scan -= self.rstart;
                self.rstart = 0;
            }
            if self.rbuf.capacity() - self.rbuf.len() < 16 * 1024 {
                self.rbuf.reserve(64 * 1024);
            }
            let n = timeout(self.io_timeout, self.stream.read_buf(&mut self.rbuf))
                .await
                .map_err(|_| Error::Timeout)??;
            if n == 0 {
                return Err(match self.bye.take() {
                    Some(text) => Error::Bye(text),
                    None => Error::Closed,
                });
            }
        }
    }

    async fn write(&mut self, data: &[u8]) -> Result<()> {
        timeout(self.io_timeout, async {
            self.stream.write_all(data).await?;
            self.stream.flush().await
        })
        .await
        .map_err(|_| Error::Timeout)??;
        Ok(())
    }

    fn tag(&mut self) -> String {
        self.next_tag += 1;
        format!("E{}", self.next_tag)
    }

    /// Runs a command made of text and literal parts and waits for completion.
    pub async fn run(&mut self, name: &str, parts: &[Part<'_>]) -> Result<Done> {
        let tag = self.tag();
        let mut untagged = Vec::new();
        let mut out: Vec<u8> = Vec::with_capacity(256);
        out.extend_from_slice(tag.as_bytes());
        out.push(b' ');
        for part in parts {
            match part {
                Part::Text(t) => out.extend_from_slice(t.as_bytes()),
                Part::Owned(t) => out.extend_from_slice(t.as_bytes()),
                Part::Literal(_) | Part::OwnedLiteral(_) => {
                    let data: &[u8] = match part {
                        Part::Literal(d) => d,
                        Part::OwnedLiteral(d) => d,
                        _ => &[],
                    };
                    let non_sync = self.has_cap("LITERAL+")
                        || (self.has_cap("LITERAL-") && data.len() <= 4096);
                    if non_sync {
                        out.extend_from_slice(format!("{{{}+}}\r\n", data.len()).as_bytes());
                        out.extend_from_slice(data);
                    } else {
                        out.extend_from_slice(format!("{{{}}}\r\n", data.len()).as_bytes());
                        self.write(&out).await?;
                        out.clear();
                        loop {
                            match self.read_resp().await? {
                                Resp::Continue { .. } => break,
                                Resp::Untagged(u) => untagged.push(u),
                                Resp::Tagged {
                                    tag: t,
                                    status,
                                    text,
                                    ..
                                } if t == tag => {
                                    return Err(Self::status_error(name, status, text));
                                }
                                Resp::Tagged { .. } => {}
                            }
                        }
                        self.write(data).await?;
                    }
                }
            }
        }
        out.extend_from_slice(b"\r\n");
        self.write(&out).await?;
        self.finish(&tag, name, untagged).await
    }

    /// Convenience for commands without literals.
    pub async fn run_text(&mut self, command: &str) -> Result<Done> {
        let name = command.split(' ').next().unwrap_or(command).to_string();
        self.run(&name, &[Part::Text(command)]).await
    }

    fn status_error(name: &str, status: Status, text: String) -> Error {
        match status {
            Status::No => Error::No {
                command: name.to_string(),
                text,
            },
            Status::Bad => Error::Bad {
                command: name.to_string(),
                text,
            },
            Status::Bye => Error::Bye(text),
            _ => Error::Protocol(format!("{name}: unexpected status {status:?}")),
        }
    }

    async fn finish(&mut self, tag: &str, name: &str, mut untagged: Vec<Untagged>) -> Result<Done> {
        loop {
            match self.read_resp().await? {
                Resp::Tagged {
                    tag: t,
                    status,
                    code,
                    text,
                } if t == tag => {
                    return match status {
                        Status::Ok => Ok(Done {
                            untagged,
                            code,
                            text,
                        }),
                        other => Err(Self::status_error(name, other, text)),
                    };
                }
                Resp::Tagged { .. } => {}
                Resp::Untagged(Untagged::Status {
                    status: Status::Bye,
                    text,
                    ..
                }) => {
                    self.bye = Some(text);
                }
                Resp::Untagged(Untagged::Capabilities(c))
                    if !name.eq_ignore_ascii_case("ENABLE") =>
                {
                    self.caps = c;
                }
                Resp::Untagged(u) => untagged.push(u),
                Resp::Continue { .. } => {}
            }
        }
    }

    /// Splits off untagged responses a command did not ask for so the sync
    /// layer can react to them later.
    fn stash<F: Fn(&Untagged) -> bool>(
        &mut self,
        untagged: Vec<Untagged>,
        keep: F,
    ) -> Vec<Untagged> {
        let mut kept = Vec::new();
        for u in untagged {
            if keep(&u) {
                kept.push(u);
            } else if matches!(
                u,
                Untagged::Exists(_)
                    | Untagged::Expunge(_)
                    | Untagged::Vanished { .. }
                    | Untagged::Fetch(_)
                    | Untagged::Recent(_)
            ) {
                self.unsolicited.push(u);
            }
        }
        kept
    }

    // ----- session commands ----------------------------------------------

    /// Refreshes the capability list.
    pub async fn capability(&mut self) -> Result<&[String]> {
        let done = self.run_text("CAPABILITY").await?;
        for u in done.untagged {
            if let Untagged::Capabilities(c) = u {
                self.caps = c;
            }
        }
        Ok(&self.caps)
    }

    /// Authenticates with the best mechanism for the given credentials.
    pub async fn authenticate(&mut self, auth: &Auth) -> Result<()> {
        let result = match auth {
            Auth::Password { user, password } => {
                if self.has_cap("AUTH=PLAIN") && !self.has_cap("LOGINDISABLED") {
                    let payload = format!("\0{user}\0{password}");
                    self.sasl("PLAIN", payload.as_bytes()).await
                } else {
                    self.login(user, password).await
                }
            }
            Auth::XOAuth2 { user, token } => {
                let payload = format!("user={user}\x01auth=Bearer {token}\x01\x01");
                self.sasl("XOAUTH2", payload.as_bytes()).await
            }
            Auth::OAuthBearer { user, token } => {
                let payload = format!(
                    "n,a={},\x01host={}\x01auth=Bearer {token}\x01\x01",
                    user.replace(',', "=2C").replace('=', "=3D"),
                    self.host
                );
                self.sasl("OAUTHBEARER", payload.as_bytes()).await
            }
        };
        match result {
            Ok(done) => {
                if let Some(Code::Capabilities(c)) = done.code {
                    self.caps = c;
                } else {
                    self.capability().await?;
                }
                Ok(())
            }
            Err(Error::No { text, .. }) | Err(Error::Bad { text, .. }) => {
                Err(Error::AuthFailed(text))
            }
            Err(e) => Err(e),
        }
    }

    async fn login(&mut self, user: &str, password: &str) -> Result<Done> {
        let mut parts = vec![Part::Text("LOGIN ")];
        let (u, p);
        match quote(user) {
            Some(q) => {
                u = q;
                parts.push(Part::Owned(u.clone()));
            }
            None => parts.push(Part::Literal(user.as_bytes())),
        }
        parts.push(Part::Text(" "));
        match quote(password) {
            Some(q) => {
                p = q;
                parts.push(Part::Owned(p.clone()));
            }
            None => parts.push(Part::Literal(password.as_bytes())),
        }
        self.run("LOGIN", &parts).await
    }

    async fn sasl(&mut self, mechanism: &str, payload: &[u8]) -> Result<Done> {
        let tag = self.tag();
        let encoded = B64.encode(payload);
        let ir = self.has_cap("SASL-IR");
        let cmd = if ir {
            format!("{tag} AUTHENTICATE {mechanism} {encoded}\r\n")
        } else {
            format!("{tag} AUTHENTICATE {mechanism}\r\n")
        };
        self.write(cmd.as_bytes()).await?;
        let mut sent_payload = ir;
        let mut untagged = Vec::new();
        loop {
            match self.read_resp().await? {
                Resp::Continue { .. } => {
                    if sent_payload {
                        // Server sent an error challenge (XOAUTH2); answer with an
                        // empty response so it can finish with a tagged NO.
                        self.write(b"\r\n").await?;
                    } else {
                        self.write(format!("{encoded}\r\n").as_bytes()).await?;
                        sent_payload = true;
                    }
                }
                Resp::Tagged {
                    tag: t,
                    status,
                    code,
                    text,
                } if t == tag => {
                    return match status {
                        Status::Ok => Ok(Done {
                            untagged,
                            code,
                            text,
                        }),
                        other => Err(Self::status_error("AUTHENTICATE", other, text)),
                    };
                }
                Resp::Tagged { .. } => {}
                Resp::Untagged(Untagged::Capabilities(c)) => self.caps = c,
                Resp::Untagged(u) => untagged.push(u),
            }
        }
    }

    /// Sends the `ID` command (some providers, e.g. NetEase, require it).
    pub async fn id(&mut self, name: &str, version: &str) -> Result<()> {
        if !self.has_cap("ID") {
            return Ok(());
        }
        let cmd = format!(
            "ID (\"name\" {} \"version\" {} \"vendor\" \"Envelope\")",
            quote(name).unwrap_or_else(|| "\"Envelope\"".into()),
            quote(version).unwrap_or_else(|| "\"0\"".into())
        );
        self.run_text(&cmd).await.map(|_| ())
    }

    /// Turns on extensions (`ENABLE QRESYNC UTF8=ACCEPT`), returning those enabled.
    pub async fn enable(&mut self, caps: &[&str]) -> Result<Vec<String>> {
        if !self.has_cap("ENABLE") {
            return Ok(Vec::new());
        }
        let wanted: Vec<&str> = caps.iter().copied().filter(|c| self.has_cap(c)).collect();
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        let done = self
            .run_text(&format!("ENABLE {}", wanted.join(" ")))
            .await?;
        for u in done.untagged {
            if let Untagged::Capabilities(c) = u {
                for cap in c {
                    if !self.is_enabled(&cap) {
                        self.enabled.push(cap);
                    }
                }
            }
        }
        // CONDSTORE is implied by QRESYNC.
        if self.is_enabled("QRESYNC") && !self.is_enabled("CONDSTORE") {
            self.enabled.push("CONDSTORE".into());
        }
        Ok(self.enabled.clone())
    }

    pub async fn noop(&mut self) -> Result<Vec<Untagged>> {
        let done = self.run_text("NOOP").await?;
        Ok(done.untagged)
    }

    pub async fn logout(&mut self) -> Result<()> {
        let _ = self.run_text("LOGOUT").await;
        let _ = self.stream.shutdown().await;
        Ok(())
    }

    // ----- mailbox commands ----------------------------------------------

    /// Lists all mailboxes. Uses `RETURN (SPECIAL-USE)` when available and,
    /// with LIST-STATUS, returns per-mailbox status in the same round trip.
    pub async fn list(
        &mut self,
        with_status: bool,
    ) -> Result<(Vec<ListEntry>, Vec<MailboxStatus>)> {
        let extended = self.has_cap("LIST-EXTENDED");
        let mut ret: Vec<String> = Vec::new();
        if extended && self.has_cap("SPECIAL-USE") {
            ret.push("SPECIAL-USE".into());
        }
        if with_status && self.has_cap("LIST-STATUS") {
            let mut items = vec!["MESSAGES", "UNSEEN", "UIDNEXT", "UIDVALIDITY"];
            if self.has_cap("CONDSTORE") || self.has_cap("QRESYNC") {
                items.push("HIGHESTMODSEQ");
            }
            ret.push(format!("STATUS ({})", items.join(" ")));
        }
        let cmd = if ret.is_empty() {
            "LIST \"\" \"*\"".to_string()
        } else {
            format!("LIST \"\" \"*\" RETURN ({})", ret.join(" "))
        };
        let done = match self.run_text(&cmd).await {
            Ok(d) => d,
            Err(Error::Bad { .. }) if !ret.is_empty() => self.run_text("LIST \"\" \"*\"").await?,
            Err(e) => return Err(e),
        };
        let mut entries = Vec::new();
        let mut statuses = Vec::new();
        for u in done.untagged {
            match u {
                Untagged::List(l) => entries.push(l),
                Untagged::MailboxStatus(s) => statuses.push(s),
                _ => {}
            }
        }
        Ok((entries, statuses))
    }

    /// `STATUS` for one mailbox.
    pub async fn status(&mut self, mailbox: &str) -> Result<MailboxStatus> {
        let mut items = vec!["MESSAGES", "UNSEEN", "UIDNEXT", "UIDVALIDITY"];
        if self.has_cap("CONDSTORE") || self.has_cap("QRESYNC") {
            items.push("HIGHESTMODSEQ");
        }
        let mut parts = vec![Part::Text("STATUS ")];
        parts.extend(self.mailbox_parts(mailbox));
        let tail = format!(" ({})", items.join(" "));
        parts.push(Part::Owned(tail));
        let done = self.run("STATUS", &parts).await?;
        for u in done.untagged {
            if let Untagged::MailboxStatus(mut s) = u {
                s.mailbox = mailbox.to_string();
                return Ok(s);
            }
        }
        Err(Error::Protocol("STATUS returned no data".into()))
    }

    /// Selects a mailbox read-write, using QRESYNC/CONDSTORE when possible.
    pub async fn select(
        &mut self,
        mailbox: &str,
        qresync: Option<&QresyncParams>,
    ) -> Result<Selected> {
        self.select_impl("SELECT", mailbox, qresync).await
    }

    /// Selects a mailbox read-only.
    pub async fn examine(&mut self, mailbox: &str) -> Result<Selected> {
        self.select_impl("EXAMINE", mailbox, None).await
    }

    async fn select_impl(
        &mut self,
        verb: &str,
        mailbox: &str,
        qresync: Option<&QresyncParams>,
    ) -> Result<Selected> {
        let mut parts = vec![Part::Owned(format!("{verb} "))];
        parts.extend(self.mailbox_parts(mailbox));
        if let (Some(q), true) = (qresync, self.is_enabled("QRESYNC")) {
            let known = q
                .known_uids
                .as_ref()
                .map(|k| format!(" {k}"))
                .unwrap_or_default();
            parts.push(Part::Owned(format!(
                " (QRESYNC ({} {}{known}))",
                q.uid_validity, q.modseq
            )));
        } else if self.has_cap("CONDSTORE") && !self.is_enabled("CONDSTORE") {
            parts.push(Part::Text(" (CONDSTORE)"));
        }
        self.selected = None;
        let done = self.run(verb, &parts).await?;
        let mut sel = Selected {
            mailbox: mailbox.to_string(),
            read_only: matches!(done.code, Some(Code::ReadOnly)),
            ..Default::default()
        };
        for u in done.untagged {
            match u {
                Untagged::Exists(n) => sel.exists = n,
                Untagged::Recent(n) => sel.recent = n,
                Untagged::Flags(f) => sel.flags = f,
                Untagged::Status { code, text, .. } => match code {
                    Some(Code::UidValidity(v)) => sel.uid_validity = Some(v),
                    Some(Code::UidNext(v)) => sel.uid_next = Some(v),
                    Some(Code::HighestModSeq(v)) => sel.highest_modseq = Some(v),
                    Some(Code::PermanentFlags(f)) => sel.permanent_flags = f,
                    _ => {
                        if text.to_ascii_uppercase().starts_with("[NOMODSEQ]") {
                            sel.no_modseq = true;
                        }
                    }
                },
                Untagged::Vanished { uids, .. } => {
                    for r in uids {
                        if r.end().saturating_sub(*r.start()) < 5_000_000 {
                            sel.vanished.extend(r);
                        }
                    }
                }
                Untagged::Fetch(f) => sel.changed.push(*f),
                _ => {}
            }
        }
        self.selected = Some(mailbox.to_string());
        Ok(sel)
    }

    /// Closes the selected mailbox without expunging (`UNSELECT`, or `CLOSE` fallback
    /// is avoided since it expunges).
    pub async fn unselect(&mut self) -> Result<()> {
        if self.selected.is_none() {
            return Ok(());
        }
        if self.has_cap("UNSELECT") {
            self.run_text("UNSELECT").await?;
        }
        self.selected = None;
        Ok(())
    }

    pub async fn create(&mut self, mailbox: &str) -> Result<()> {
        let mut parts = vec![Part::Text("CREATE ")];
        parts.extend(self.mailbox_parts(mailbox));
        self.run("CREATE", &parts).await.map(|_| ())
    }

    pub async fn delete(&mut self, mailbox: &str) -> Result<()> {
        let mut parts = vec![Part::Text("DELETE ")];
        parts.extend(self.mailbox_parts(mailbox));
        self.run("DELETE", &parts).await.map(|_| ())
    }

    pub async fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        let mut parts = vec![Part::Text("RENAME ")];
        parts.extend(self.mailbox_parts(from));
        parts.push(Part::Text(" "));
        parts.extend(self.mailbox_parts(to));
        self.run("RENAME", &parts).await.map(|_| ())
    }

    pub async fn subscribe(&mut self, mailbox: &str) -> Result<()> {
        let mut parts = vec![Part::Text("SUBSCRIBE ")];
        parts.extend(self.mailbox_parts(mailbox));
        self.run("SUBSCRIBE", &parts).await.map(|_| ())
    }

    // ----- message commands ----------------------------------------------

    /// `UID FETCH <set> (<items>)`.
    pub async fn uid_fetch(&mut self, set: &str, items: &str) -> Result<Vec<FetchItem>> {
        let done = self.run_text(&format!("UID FETCH {set} ({items})")).await?;
        Ok(self.collect_fetch(done.untagged))
    }

    /// `UID FETCH <set> (<items>) (CHANGEDSINCE <modseq> [VANISHED])`.
    /// Returns the changed messages and, with QRESYNC, the vanished UIDs.
    pub async fn uid_fetch_changed(
        &mut self,
        set: &str,
        items: &str,
        modseq: u64,
    ) -> Result<(Vec<FetchItem>, Vec<u32>)> {
        let vanished = if self.is_enabled("QRESYNC") {
            " VANISHED"
        } else {
            ""
        };
        let done = self
            .run_text(&format!(
                "UID FETCH {set} ({items}) (CHANGEDSINCE {modseq}{vanished})"
            ))
            .await?;
        let mut gone = Vec::new();
        let mut fetches = Vec::new();
        for u in done.untagged {
            match u {
                Untagged::Fetch(f) => fetches.push(*f),
                Untagged::Vanished { uids, .. } => {
                    for r in uids {
                        if r.end().saturating_sub(*r.start()) < 5_000_000 {
                            gone.extend(r);
                        }
                    }
                }
                other => {
                    let _ = self.stash(vec![other], |_| false);
                }
            }
        }
        Ok((merge_fetches(fetches), gone))
    }

    fn collect_fetch(&mut self, untagged: Vec<Untagged>) -> Vec<FetchItem> {
        let kept = self.stash(untagged, |u| matches!(u, Untagged::Fetch(_)));
        merge_fetches(kept.into_iter().filter_map(|u| match u {
            Untagged::Fetch(f) => Some(*f),
            _ => None,
        }))
    }

    /// `UID SEARCH <criteria>`.
    pub async fn uid_search(&mut self, criteria: &str) -> Result<Vec<u32>> {
        let charset = if criteria.is_ascii() {
            ""
        } else {
            "CHARSET UTF-8 "
        };
        let done = self
            .run_text(&format!("UID SEARCH {charset}{criteria}"))
            .await?;
        let mut out = Vec::new();
        for u in self.stash(done.untagged, |u| matches!(u, Untagged::Search(_))) {
            if let Untagged::Search(v) = u {
                out.extend(v);
            }
        }
        Ok(out)
    }

    /// Gmail's server-side search (`X-GM-RAW`), using Gmail search syntax.
    pub async fn gmail_search(&mut self, query: &str) -> Result<Vec<u32>> {
        let mut parts = vec![Part::Text("UID SEARCH ")];
        if !query.is_ascii() {
            parts.push(Part::Text("CHARSET UTF-8 "));
        }
        parts.push(Part::Text("X-GM-RAW "));
        let quoted = quote(query);
        match &quoted {
            Some(q) => parts.push(Part::Text(q)),
            None => parts.push(Part::Literal(query.as_bytes())),
        }
        let done = self.run("SEARCH", &parts).await?;
        let mut out = Vec::new();
        for u in self.stash(done.untagged, |u| matches!(u, Untagged::Search(_))) {
            if let Untagged::Search(v) = u {
                out.extend(v);
            }
        }
        Ok(out)
    }

    /// `UID STORE <set> <op> (<flags>)`, e.g. op = `+FLAGS.SILENT`.
    pub async fn uid_store(&mut self, uids: &[u32], op: &str, flags: &[&str]) -> Result<()> {
        if uids.is_empty() {
            return Ok(());
        }
        self.uid_store_set(&uid_set_string(uids), op, flags).await
    }

    /// `UID STORE` on a UID set written out, such as `1:500`.
    pub async fn uid_store_set(&mut self, set: &str, op: &str, flags: &[&str]) -> Result<()> {
        let done = self
            .run_text(&format!("UID STORE {set} {op} ({})", flags.join(" ")))
            .await?;
        let _ = self.stash(done.untagged, |_| false);
        Ok(())
    }

    /// Adds or removes Gmail labels (`X-GM-LABELS`).
    pub async fn gmail_store_labels(
        &mut self,
        uids: &[u32],
        add: bool,
        labels: &[&str],
    ) -> Result<()> {
        if uids.is_empty() || labels.is_empty() {
            return Ok(());
        }
        let set = uid_set_string(uids);
        let list: Vec<String> = labels
            .iter()
            .map(|l| {
                if l.starts_with('\\') {
                    l.to_string()
                } else {
                    let enc = if self.is_enabled("UTF8=ACCEPT") {
                        l.to_string()
                    } else {
                        utf7::encode(l)
                    };
                    quote(&enc).unwrap_or(enc)
                }
            })
            .collect();
        let op = if add {
            "+X-GM-LABELS.SILENT"
        } else {
            "-X-GM-LABELS.SILENT"
        };
        let done = self
            .run_text(&format!("UID STORE {set} {op} ({})", list.join(" ")))
            .await?;
        let _ = self.stash(done.untagged, |_| false);
        Ok(())
    }

    /// Copies messages; returns `(source, dest)` UID pairs when the server reports COPYUID.
    pub async fn uid_copy(&mut self, uids: &[u32], dest: &str) -> Result<Vec<(u32, u32)>> {
        let set = uid_set_string(uids);
        let mut parts = vec![Part::Owned(format!("UID COPY {set} "))];
        parts.extend(self.mailbox_parts(dest));
        let done = self.run("COPY", &parts).await?;
        Ok(Self::copyuid_pairs(&done))
    }

    fn copyuid_pairs(done: &Done) -> Vec<(u32, u32)> {
        let mut codes: Vec<&Code> = done.code.iter().collect();
        for u in &done.untagged {
            if let Untagged::Status { code: Some(c), .. } = u {
                codes.push(c);
            }
        }
        for c in codes {
            if let Code::CopyUid { source, dest, .. } = c
                && source.len() == dest.len()
            {
                return source.iter().copied().zip(dest.iter().copied()).collect();
            }
        }
        Vec::new()
    }

    /// Moves messages (`UID MOVE`, or COPY + delete + expunge fallback).
    pub async fn uid_move(&mut self, uids: &[u32], dest: &str) -> Result<Vec<(u32, u32)>> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        if self.has_cap("MOVE") {
            let set = uid_set_string(uids);
            let mut parts = vec![Part::Owned(format!("UID MOVE {set} "))];
            parts.extend(self.mailbox_parts(dest));
            let done = self.run("MOVE", &parts).await?;
            let pairs = Self::copyuid_pairs(&done);
            let _ = self.stash(done.untagged, |_| false);
            return Ok(pairs);
        }
        let pairs = self.uid_copy(uids, dest).await?;
        self.uid_store(uids, "+FLAGS.SILENT", &["\\Deleted"])
            .await?;
        self.uid_expunge(uids).await?;
        Ok(pairs)
    }

    /// Permanently removes messages marked `\Deleted` (only the given UIDs when
    /// UIDPLUS is available).
    pub async fn uid_expunge(&mut self, uids: &[u32]) -> Result<()> {
        self.uid_expunge_set(&uid_set_string(uids)).await
    }

    /// Permanently removes the messages marked `\Deleted` in a UID set written
    /// out, such as `1:500` (all of them without UIDPLUS).
    pub async fn uid_expunge_set(&mut self, set: &str) -> Result<()> {
        let done = if self.has_cap("UIDPLUS") {
            self.run_text(&format!("UID EXPUNGE {set}")).await?
        } else {
            self.run_text("EXPUNGE").await?
        };
        let _ = self.stash(done.untagged, |_| false);
        Ok(())
    }

    /// Appends a message; returns `(uid_validity, uid)` when APPENDUID is reported.
    pub async fn append(
        &mut self,
        mailbox: &str,
        flags: &[&str],
        date: Option<chrono::DateTime<chrono::FixedOffset>>,
        message: &[u8],
    ) -> Result<Option<(u32, u32)>> {
        let mut parts = vec![Part::Text("APPEND ")];
        parts.extend(self.mailbox_parts(mailbox));
        let mut meta = String::new();
        if !flags.is_empty() {
            meta.push_str(&format!(" ({})", flags.join(" ")));
        }
        if let Some(d) = date {
            meta.push_str(&format!(" \"{}\"", d.format("%d-%b-%Y %H:%M:%S %z")));
        }
        meta.push(' ');
        parts.push(Part::Owned(meta));
        parts.push(Part::Literal(message));
        let done = self.run("APPEND", &parts).await?;
        if let Some(Code::AppendUid { uid_validity, uids }) = &done.code
            && let Some(uid) = uids.first()
        {
            return Ok(Some((*uid_validity, *uid)));
        }
        Ok(None)
    }

    /// Waits for server push notifications with `IDLE`.
    ///
    /// Returns when the server reports changes, when `max` elapses (servers drop
    /// idle connections after ~30 minutes, so keep this below that), or when
    /// `interrupt` completes.
    pub async fn idle<F: Future<Output = ()>>(
        &mut self,
        max: Duration,
        interrupt: F,
    ) -> Result<(IdleEnd, Vec<Untagged>)> {
        if !self.has_cap("IDLE") {
            return Err(Error::Unsupported("IDLE"));
        }
        let tag = self.tag();
        self.write(format!("{tag} IDLE\r\n").as_bytes()).await?;
        let mut untagged = Vec::new();
        loop {
            match self.read_resp().await? {
                Resp::Continue { .. } => break,
                Resp::Tagged {
                    tag: t,
                    status,
                    text,
                    ..
                } if t == tag => return Err(Self::status_error("IDLE", status, text)),
                Resp::Untagged(u) => untagged.push(u),
                Resp::Tagged { .. } => {}
            }
        }

        let saved_timeout = self.io_timeout;
        // While idling, reads may legitimately block for a long time.
        self.io_timeout = max + Duration::from_secs(60);
        tokio::pin!(interrupt);
        let deadline = tokio::time::sleep(max);
        tokio::pin!(deadline);
        let end = loop {
            tokio::select! {
                resp = self.read_resp() => {
                    match resp {
                        Ok(Resp::Untagged(Untagged::Status { status: Status::Bye, text, .. })) => {
                            self.io_timeout = saved_timeout;
                            return Err(Error::Bye(text));
                        }
                        Ok(Resp::Untagged(u)) => {
                            let interesting = matches!(
                                u,
                                Untagged::Exists(_)
                                    | Untagged::Expunge(_)
                                    | Untagged::Vanished { .. }
                                    | Untagged::Fetch(_)
                            );
                            untagged.push(u);
                            if interesting {
                                break IdleEnd::Changed;
                            }
                        }
                        Ok(_) => {}
                        Err(e) => {
                            self.io_timeout = saved_timeout;
                            return Err(e);
                        }
                    }
                }
                _ = &mut deadline => break IdleEnd::Timeout,
                _ = &mut interrupt => break IdleEnd::Interrupted,
            }
        };
        self.io_timeout = saved_timeout;
        self.write(b"DONE\r\n").await?;
        let done = self.finish(&tag, "IDLE", Vec::new()).await?;
        untagged.extend(done.untagged);
        Ok((end, untagged))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_detection() {
        assert_eq!(literal_len(b"* 1 FETCH (BODY[] {123}"), Some(123));
        assert_eq!(literal_len(b"A1 APPEND x {5+}"), Some(5));
        assert_eq!(literal_len(b"* OK done"), None);
        assert_eq!(literal_len(b"* 1 FETCH (X {})"), None);
        assert_eq!(literal_len(b"{abc}"), None);
    }

    #[test]
    fn quoting() {
        assert_eq!(quote("INBOX").as_deref(), Some("\"INBOX\""));
        assert_eq!(quote("a\"b\\c").as_deref(), Some("\"a\\\"b\\\\c\""));
        assert_eq!(quote("Öğe"), None);
        assert_eq!(quote("a\r\nb"), None);
    }

    #[test]
    fn fallback() {
        match fallback_parse(b"E7 NO [WEIRD stuff] nope\r\n") {
            Resp::Tagged { tag, status, .. } => {
                assert_eq!(tag, "E7");
                assert_eq!(status, Status::No);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(
            fallback_parse(b"+ idling\r\n"),
            Resp::Continue { .. }
        ));
    }
}
