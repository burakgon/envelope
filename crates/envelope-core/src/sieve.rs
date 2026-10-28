//! Automatic replies on IMAP servers through ManageSieve (RFC 5804): a
//! `vacation` rule (RFC 5230) in the active Sieve script.
//!
//! The rule lives in a marked block at the top of the script, so filters the
//! user keeps in the same script are left alone. The block's first line is a
//! comment carrying the settings as JSON, which is how they are read back.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::error::{Error, Result};

/// The standard ManageSieve port.
pub const PORT: u16 = 4190;
const TIMEOUT: Duration = Duration::from_secs(15);
const BEGIN: &str = "# BEGIN Envelope automatic reply";
const END: &str = "# END Envelope automatic reply";
const SETTINGS: &str = "# envelope:";
/// Name of the script created when the server has no active one.
const SCRIPT: &str = "envelope";

/// An automatic reply as kept in the Sieve script.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vacation {
    /// Off keeps the settings in the script (for next time) without the rule.
    pub enabled: bool,
    pub subject: String,
    pub message: String,
    /// First and last day (inclusive) of the reply, in the server's time zone.
    pub start: Option<NaiveDate>,
    pub end: Option<NaiveDate>,
    /// The user's addresses: mail sent to any of them is answered.
    pub addresses: Vec<String>,
}

// ----- script editing ------------------------------------------------------------

fn quote(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A tag for the block's markers, from its content, so that no text inside
/// the block (the reply message) can look like its end.
fn tag(settings: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(settings.as_bytes())
        .iter()
        .take(4)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The marked block holding the rule.
fn block(v: &Vacation) -> String {
    let settings = serde_json::to_string(v).unwrap_or_default();
    let tag = tag(&settings);
    let end_line = format!("{END} {tag}\n");
    let mut out = format!("{BEGIN} {tag} (managed by Envelope)\n{SETTINGS} {settings}\n");
    if !v.enabled {
        out.push_str(&end_line);
        return out;
    }
    let scheduled = v.start.is_some() || v.end.is_some();
    out.push_str(if scheduled {
        "require [\"vacation\", \"date\", \"relational\"];\n"
    } else {
        "require [\"vacation\"];\n"
    });
    let mut action = String::from("vacation :days 4");
    if !v.subject.trim().is_empty() {
        action.push_str(&format!(" :subject {}", quote(v.subject.trim())));
    }
    if !v.addresses.is_empty() {
        let list: Vec<String> = v.addresses.iter().map(|a| quote(a)).collect();
        action.push_str(&format!(" :addresses [{}]", list.join(", ")));
    }
    action.push_str(&format!(" {};", quote(&v.message)));
    if scheduled {
        let mut tests = Vec::new();
        if let Some(start) = v.start {
            tests.push(format!(
                "currentdate :value \"ge\" \"date\" \"{}\"",
                start.format("%Y-%m-%d")
            ));
        }
        if let Some(end) = v.end {
            tests.push(format!(
                "currentdate :value \"le\" \"date\" \"{}\"",
                end.format("%Y-%m-%d")
            ));
        }
        out.push_str(&format!(
            "if allof ({}) {{\n    {action}\n}}\n",
            tests.join(", ")
        ));
    } else {
        out.push_str(&action);
        out.push('\n');
    }
    out.push_str(&end_line);
    out
}

/// Where Envelope's block starts (at the start of a line) and its tag.
fn find_block(script: &str) -> Option<(usize, String)> {
    let mut from = 0;
    while let Some(i) = script[from..].find(BEGIN).map(|i| from + i) {
        if i == 0 || script.as_bytes()[i - 1] == b'\n' {
            let tag = script[i + BEGIN.len()..]
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string();
            return Some((i, tag));
        }
        from = i + BEGIN.len();
    }
    None
}

/// The script without Envelope's block.
fn without_block(script: &str) -> String {
    let Some((begin, tag)) = find_block(script) else {
        return script.to_string();
    };
    let end_line = format!("\n{END} {tag}");
    let Some(end) = script[begin..]
        .find(&end_line)
        .map(|i| begin + i + end_line.len())
    else {
        return script.to_string();
    };
    let rest = script[end..]
        .strip_prefix("\r\n")
        .or_else(|| script[end..].strip_prefix('\n'))
        .unwrap_or(&script[end..]);
    format!("{}{rest}", &script[..begin])
}

/// The settings kept in Envelope's block, if the script has one.
pub fn read(script: &str) -> Option<Vacation> {
    let (begin, _) = find_block(script)?;
    let line = script[begin..].lines().nth(1)?;
    serde_json::from_str(line.trim().strip_prefix(SETTINGS)?.trim()).ok()
}

/// Whether the script has a `vacation` action of its own (one set up in
/// webmail, say): Sieve allows only one.
fn has_vacation(script: &str) -> bool {
    let bytes = script.as_bytes();
    let mut word = String::new();
    let mut i = 0;
    let mut found = false;
    let mut check = |word: &mut String| {
        if word.eq_ignore_ascii_case("vacation") {
            found = true;
        }
        word.clear();
    };
    while i < bytes.len() {
        match bytes[i] {
            b'#' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                check(&mut word);
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = script[i + 2..]
                    .find("*/")
                    .map_or(bytes.len(), |e| i + 2 + e + 2);
                check(&mut word);
                continue;
            }
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                check(&mut word);
            }
            c if c.is_ascii_alphanumeric() || c == b'_' || c == b':' => word.push(c as char),
            _ => check(&mut word),
        }
        i += 1;
    }
    check(&mut word);
    found
}

/// Where `require` commands end: the block goes after the script's own,
/// since Sieve wants every `require` before the first other command.
fn after_requires(script: &str) -> usize {
    let bytes = script.as_bytes();
    let mut i = 0;
    let mut end = 0;
    loop {
        // Skip white space and comments.
        while i < bytes.len() {
            match bytes[i] {
                b' ' | b'\t' | b'\r' | b'\n' => i += 1,
                b'#' => {
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                }
                b'/' if bytes.get(i + 1) == Some(&b'*') => {
                    i = script[i + 2..]
                        .find("*/")
                        .map_or(bytes.len(), |e| i + 2 + e + 2);
                }
                _ => break,
            }
        }
        if !script[i..].starts_with("require") {
            return end;
        }
        // Up to the terminating semicolon, outside strings.
        let mut in_string = false;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' if in_string => i += 1,
                b'"' => in_string = !in_string,
                b';' if !in_string => break,
                _ => {}
            }
            i += 1;
        }
        i = (i + 1).min(bytes.len());
        // A comment after the require stays with it; a command on the same
        // line does not.
        let mut j = i;
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
            j += 1;
        }
        match bytes.get(j) {
            Some(b'#') => {
                while j < bytes.len() && bytes[j] != b'\n' {
                    j += 1;
                }
                i = (j + 1).min(bytes.len());
            }
            Some(b'\r') if bytes.get(j + 1) == Some(&b'\n') => i = j + 2,
            Some(b'\n') => i = j + 1,
            None => i = j,
            _ => {}
        }
        end = i;
    }
}

/// The script with Envelope's block replaced by `vacation` (or removed).
pub fn apply(script: &str, vacation: Option<&Vacation>) -> String {
    let script = without_block(script);
    let Some(v) = vacation else {
        return script;
    };
    let at = after_requires(&script);
    let mut head = script[..at].to_string();
    if !head.is_empty() && !head.ends_with('\n') {
        head.push('\n');
    }
    format!("{head}{}{}", block(v), &script[at..])
}

// ----- ManageSieve client --------------------------------------------------------

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Atom(String),
    Str(String),
}

/// The final line of a response.
#[derive(Debug)]
struct Status {
    ok: bool,
    text: String,
}

fn err(e: impl std::fmt::Display) -> Error {
    Error::Network(format!("ManageSieve: {e}"))
}

/// One connection's reading and writing, before or after TLS.
struct Conn<S> {
    io: BufReader<S>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Conn<S> {
    async fn command_status(&mut self, line: &str) -> Result<(Vec<Vec<Token>>, Status)> {
        let io = self.io.get_mut();
        tokio::time::timeout(TIMEOUT, async {
            io.write_all(line.as_bytes()).await?;
            io.write_all(b"\r\n").await?;
            io.flush().await
        })
        .await
        .map_err(|_| err("timed out"))?
        .map_err(err)?;
        self.response().await
    }

    async fn command(&mut self, line: &str) -> Result<Vec<Vec<Token>>> {
        let (lines, status) = self.command_status(line).await?;
        if !status.ok {
            return Err(err(status.text));
        }
        Ok(lines)
    }

    /// Reads data lines up to the closing `OK`, `NO` or `BYE`.
    async fn response(&mut self) -> Result<(Vec<Vec<Token>>, Status)> {
        let mut lines = Vec::new();
        loop {
            let line = tokio::time::timeout(TIMEOUT, self.line())
                .await
                .map_err(|_| err("timed out"))??;
            if let Some(Token::Atom(word)) = line.first() {
                let word = word.to_ascii_uppercase();
                if word == "OK" || word == "NO" || word == "BYE" {
                    let text = line
                        .iter()
                        .skip(1)
                        .filter_map(|t| match t {
                            Token::Str(s) => Some(s.as_str()),
                            Token::Atom(_) => None,
                        })
                        .next_back()
                        .unwrap_or_default()
                        .to_string();
                    return Ok((
                        lines,
                        Status {
                            ok: word == "OK",
                            text,
                        },
                    ));
                }
            }
            lines.push(line);
        }
    }

    /// Reads one response line, literals included.
    async fn line(&mut self) -> Result<Vec<Token>> {
        let mut tokens = Vec::new();
        loop {
            let mut raw = Vec::new();
            let n = self.io.read_until(b'\n', &mut raw).await.map_err(err)?;
            if n == 0 {
                return Err(err("connection closed"));
            }
            let text = String::from_utf8_lossy(&raw);
            let text = text.trim_end_matches(['\r', '\n']);
            let Some(size) = tokenize(text, &mut tokens)? else {
                return Ok(tokens);
            };
            let mut data = vec![0; size];
            self.io.read_exact(&mut data).await.map_err(err)?;
            tokens.push(Token::Str(String::from_utf8_lossy(&data).into_owned()));
            // The line goes on after the literal.
        }
    }

    /// The capability lines up to the closing `OK`.
    async fn capabilities(&mut self) -> Result<Vec<(String, String)>> {
        let (lines, status) = self.response().await?;
        if !status.ok {
            return Err(err(status.text));
        }
        Ok(lines
            .into_iter()
            .filter_map(|line| {
                let mut it = line.into_iter();
                let name = match it.next()? {
                    Token::Str(s) | Token::Atom(s) => s,
                };
                let value = match it.next() {
                    Some(Token::Str(s) | Token::Atom(s)) => s,
                    None => String::new(),
                };
                Some((name, value))
            })
            .collect())
    }
}

fn find<'a>(capabilities: &'a [(String, String)], name: &str) -> Option<&'a str> {
    capabilities
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// A ManageSieve session.
pub struct Client {
    conn: Conn<Box<dyn Io>>,
    capabilities: Vec<(String, String)>,
}

impl Client {
    /// Connects, switching to TLS when the server offers it. Without TLS the
    /// password would cross the network in the clear, so that is refused
    /// unless `allow_plain` (a local or explicitly unencrypted server).
    pub async fn connect(
        host: &str,
        port: u16,
        accept_invalid_certs: bool,
        allow_plain: bool,
    ) -> Result<Client> {
        let tcp = tokio::time::timeout(TIMEOUT, TcpStream::connect((host, port)))
            .await
            .map_err(|_| err("timed out"))?
            .map_err(err)?;
        let mut plain = Conn {
            io: BufReader::new(tcp),
        };
        let capabilities = plain.capabilities().await?;
        if find(&capabilities, "STARTTLS").is_none() {
            if !allow_plain {
                return Err(err("the server does not offer an encrypted connection"));
            }
            let conn = Conn {
                io: BufReader::new(Box::new(plain.io.into_inner()) as Box<dyn Io>),
            };
            return Ok(Client { conn, capabilities });
        }
        plain.command("STARTTLS").await?;
        if !plain.io.buffer().is_empty() {
            return Err(err("unexpected data before TLS"));
        }
        let config: Arc<rustls::ClientConfig> =
            envelope_imap::stream::tls_config(accept_invalid_certs)
                .map_err(|e| Error::Network(e.to_string()))?;
        let name = rustls_pki_types::ServerName::try_from(host.to_string()).map_err(err)?;
        let tls = tokio::time::timeout(
            TIMEOUT,
            tokio_rustls::TlsConnector::from(config).connect(name, plain.io.into_inner()),
        )
        .await
        .map_err(|_| err("TLS timed out"))?
        .map_err(err)?;
        let mut conn = Conn {
            io: BufReader::new(Box::new(tls) as Box<dyn Io>),
        };
        let capabilities = conn.capabilities().await?;
        Ok(Client { conn, capabilities })
    }

    /// Sieve extensions the server supports.
    pub fn extensions(&self) -> Vec<String> {
        find(&self.capabilities, "SIEVE")
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_ascii_lowercase)
            .collect()
    }

    /// Signs in with SASL PLAIN.
    pub async fn login(&mut self, user: &str, password: &str) -> Result<()> {
        let sasl = find(&self.capabilities, "SASL")
            .unwrap_or_default()
            .to_ascii_uppercase();
        if !sasl.split_whitespace().any(|m| m == "PLAIN") {
            return Err(err("the server does not accept passwords (SASL PLAIN)"));
        }
        let token =
            base64::engine::general_purpose::STANDARD.encode(format!("\0{user}\0{password}"));
        let (_, status) = self
            .conn
            .command_status(&format!("AUTHENTICATE \"PLAIN\" {}", quote(&token)))
            .await?;
        if !status.ok {
            return Err(Error::Auth(if status.text.is_empty() {
                "the mail server refused the password".into()
            } else {
                status.text
            }));
        }
        Ok(())
    }

    /// The scripts on the server and which one is active.
    pub async fn scripts(&mut self) -> Result<(Vec<String>, Option<String>)> {
        let lines = self.conn.command("LISTSCRIPTS").await?;
        let mut names = Vec::new();
        let mut active = None;
        for line in lines {
            let mut it = line.into_iter();
            if let Some(Token::Str(name)) = it.next() {
                if matches!(it.next(), Some(Token::Atom(a)) if a.eq_ignore_ascii_case("ACTIVE")) {
                    active = Some(name.clone());
                }
                names.push(name);
            }
        }
        Ok((names, active))
    }

    pub async fn get(&mut self, name: &str) -> Result<String> {
        let lines = self
            .conn
            .command(&format!("GETSCRIPT {}", quote(name)))
            .await?;
        lines
            .into_iter()
            .flatten()
            .find_map(|t| match t {
                Token::Str(s) => Some(s),
                Token::Atom(_) => None,
            })
            .ok_or_else(|| err("empty GETSCRIPT response"))
    }

    pub async fn put(&mut self, name: &str, script: &str) -> Result<()> {
        let command = format!(
            "PUTSCRIPT {} {{{}+}}\r\n{script}",
            quote(name),
            script.len()
        );
        let (_, status) = self.conn.command_status(&command).await?;
        if !status.ok {
            return Err(Error::Invalid(format!(
                "The mail server did not accept the automatic reply: {}",
                status.text
            )));
        }
        Ok(())
    }

    pub async fn activate(&mut self, name: &str) -> Result<()> {
        self.conn
            .command(&format!("SETACTIVE {}", quote(name)))
            .await?;
        Ok(())
    }

    pub async fn logout(mut self) {
        let _ = self.conn.command_status("LOGOUT").await;
    }
}

/// Splits a response line into tokens. Returns the size of a literal that
/// ends the line (`{12}`), which follows on the next bytes.
fn tokenize(line: &str, tokens: &mut Vec<Token>) -> Result<Option<usize>> {
    let mut chars = line.char_indices().peekable();
    while let Some(&(i, c)) = chars.peek() {
        match c {
            ' ' | '(' | ')' => {
                chars.next();
            }
            '"' => {
                chars.next();
                let mut s = String::new();
                let mut closed = false;
                while let Some((_, c)) = chars.next() {
                    match c {
                        '\\' => {
                            if let Some((_, n)) = chars.next() {
                                s.push(n);
                            }
                        }
                        '"' => {
                            closed = true;
                            break;
                        }
                        c => s.push(c),
                    }
                }
                if !closed {
                    return Err(err("unterminated string"));
                }
                tokens.push(Token::Str(s));
            }
            '{' => {
                let rest = &line[i + 1..];
                let close = rest.find('}').ok_or_else(|| err("bad literal"))?;
                let size = rest[..close]
                    .trim_end_matches('+')
                    .parse()
                    .map_err(|_| err("bad literal"))?;
                return Ok(Some(size));
            }
            _ => {
                let mut s = String::new();
                while let Some(&(_, c)) = chars.peek() {
                    if c == ' ' || c == '(' || c == ')' {
                        break;
                    }
                    s.push(c);
                    chars.next();
                }
                tokens.push(Token::Atom(s));
            }
        }
    }
    Ok(None)
}

// ----- reading and writing the reply -----------------------------------------------

/// Where to reach the server and how to sign in.
pub struct Login<'a> {
    pub host: &'a str,
    pub port: u16,
    pub user: &'a str,
    pub password: &'a str,
    pub accept_invalid_certs: bool,
    pub allow_plain: bool,
}

async fn open(login: &Login<'_>) -> Result<Client> {
    let mut client = Client::connect(
        login.host,
        login.port,
        login.accept_invalid_certs,
        login.allow_plain,
    )
    .await?;
    if !client.extensions().iter().any(|e| e == "vacation") {
        return Err(Error::Invalid(
            "This mail server does not support automatic replies.".into(),
        ));
    }
    client.login(login.user, login.password).await?;
    Ok(client)
}

/// The automatic reply kept by Envelope, if the server has one.
pub async fn load(login: &Login<'_>) -> Result<Option<Vacation>> {
    let mut client = open(login).await?;
    let (_, active) = client.scripts().await?;
    let found = match active {
        Some(name) => read(&client.get(&name).await?),
        None => None,
    };
    client.logout().await;
    Ok(found)
}

/// Writes the automatic reply into the active script.
pub async fn save(login: &Login<'_>, vacation: &Vacation) -> Result<()> {
    let mut client = open(login).await?;
    if vacation.enabled && (vacation.start.is_some() || vacation.end.is_some()) {
        let ext = client.extensions();
        if !ext.iter().any(|e| e == "date") || !ext.iter().any(|e| e == "relational") {
            return Err(Error::Invalid(
                "This mail server cannot limit automatic replies to certain days.".into(),
            ));
        }
    }
    let (_, active) = client.scripts().await?;
    match active {
        Some(name) => {
            let script = client.get(&name).await?;
            if vacation.enabled && has_vacation(&without_block(&script)) {
                client.logout().await;
                return Err(Error::Invalid(
                    "Your mail server already sends an automatic reply that was set up elsewhere, for example in webmail. Turn that one off there first.".into(),
                ));
            }
            let updated = apply(&script, Some(vacation));
            if updated != script {
                client.put(&name, &updated).await?;
            }
        }
        None if vacation.enabled => {
            client.put(SCRIPT, &apply("", Some(vacation))).await?;
            client.activate(SCRIPT).await?;
        }
        None => {}
    }
    client.logout().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn away() -> Vacation {
        Vacation {
            enabled: true,
            subject: "Out of office".into(),
            message: "Back on \"Monday\".\nUrgent? Call me.".into(),
            start: NaiveDate::from_ymd_opt(2026, 11, 2),
            end: NaiveDate::from_ymd_opt(2026, 11, 9),
            addresses: vec!["alex@example.com".into()],
        }
    }

    #[test]
    fn writes_a_dated_rule_after_the_users_requires() {
        let user = "require [\"fileinto\"]; # filing\r\nrequire \"imap4flags\";\n\
                    if header :contains \"subject\" \"[list]\" { fileinto \"Lists\"; }\n";
        let script = apply(user, Some(&away()));
        assert!(script.starts_with(
            "require [\"fileinto\"]; # filing\r\nrequire \"imap4flags\";\n# BEGIN Envelope automatic reply"
        ), "{script}");
        assert!(script.contains("require [\"vacation\", \"date\", \"relational\"];\n"));
        assert!(script.contains(
            "if allof (currentdate :value \"ge\" \"date\" \"2026-11-02\", currentdate :value \"le\" \"date\" \"2026-11-09\") {\n    \
             vacation :days 4 :subject \"Out of office\" :addresses [\"alex@example.com\"] \
             \"Back on \\\"Monday\\\".\nUrgent? Call me.\";\n}\n"
        ), "{script}");
        assert!(script.contains("\n# END Envelope automatic reply "));
        assert!(
            script.ends_with(
                "\nif header :contains \"subject\" \"[list]\" { fileinto \"Lists\"; }\n"
            )
        );
        assert_eq!(read(&script), Some(away()));

        // Changing the reply replaces the block; removing it restores the script.
        let undated = Vacation {
            start: None,
            end: None,
            ..away()
        };
        let changed = apply(&script, Some(&undated));
        assert_eq!(changed.matches("# BEGIN Envelope").count(), 1);
        assert!(changed.contains("require [\"vacation\"];\nvacation :days 4"));
        assert_eq!(read(&changed), Some(undated));
        // Off keeps the text but no rule.
        let off = Vacation {
            enabled: false,
            ..away()
        };
        let disabled = apply(&changed, Some(&off));
        assert!(!disabled.contains("vacation :days") && !disabled.contains("require [\"vacation"));
        assert_eq!(read(&disabled), Some(off));
        assert_eq!(apply(&disabled, None), user);
    }

    #[test]
    fn the_message_cannot_end_the_block() {
        let tricky = Vacation {
            message: "Away.\n# END Envelope automatic reply\nBack soon.".into(),
            ..away()
        };
        let user = "require \"fileinto\";\nkeep;\n";
        let script = apply(user, Some(&tricky));
        assert_eq!(read(&script), Some(tricky.clone()));
        assert_eq!(apply(&script, None), user);
    }

    #[test]
    fn notices_vacation_rules_set_up_elsewhere() {
        assert!(has_vacation(
            "require [\"vacation\"];\nvacation :days 7 \"Away\";\n"
        ));
        assert!(has_vacation(
            "require \"vacation\"; if true { Vacation \"x\"; }"
        ));
        // Mentions in strings and comments are not rules.
        assert!(!has_vacation(
            "require [\"vacation\", \"fileinto\"]; # vacation later\nfileinto \"vacation\";"
        ));
        assert!(!has_vacation("/* vacation */ keep;"));
        // Envelope's own block is set aside before checking.
        let ours = apply("keep;\n", Some(&away()));
        assert!(has_vacation(&ours));
        assert!(!has_vacation(&without_block(&ours)));
    }

    #[test]
    fn finds_the_end_of_the_requires() {
        assert_eq!(after_requires(""), 0);
        assert_eq!(after_requires("keep;\n"), 0);
        // A command on the same line as the require stays after the block.
        let one_line = "require \"fileinto\"; if true { fileinto \"X\"; }\n";
        assert_eq!(
            &one_line[after_requires(one_line)..],
            " if true { fileinto \"X\"; }\n"
        );
        let script = apply(one_line, Some(&away()));
        let block_at = script.find("# BEGIN").unwrap();
        assert!(block_at < script.find("if true").unwrap());
        let s = "/* rules */\nrequire [\"a\", \"b;c\"];\n# x\nkeep;";
        assert_eq!(&s[after_requires(s)..], "# x\nkeep;");
    }

    #[test]
    fn tokenizes_responses() {
        let mut tokens = Vec::new();
        assert_eq!(
            tokenize("\"SIEVE\" \"fileinto vacation\"", &mut tokens).unwrap(),
            None
        );
        assert_eq!(
            tokens,
            [
                Token::Str("SIEVE".into()),
                Token::Str("fileinto vacation".into())
            ]
        );
        tokens.clear();
        assert_eq!(
            tokenize("NO (QUOTA) \"Too \\\"big\\\"\"", &mut tokens).unwrap(),
            None
        );
        assert_eq!(
            tokens,
            [
                Token::Atom("NO".into()),
                Token::Atom("QUOTA".into()),
                Token::Str("Too \"big\"".into())
            ]
        );
        tokens.clear();
        assert_eq!(tokenize("{42}", &mut tokens).unwrap(), Some(42));
    }
}
