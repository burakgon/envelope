//! Accounts set up in other mail programs on this computer — Thunderbird,
//! Evolution, Geary and GNOME Online Accounts — offered when adding an
//! account. Addresses, names and servers are taken over; passwords are not
//! (each program keeps them encrypted its own way), so the user signs in.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::engine::Engine;
use crate::error::Result;
use crate::model::{Security, ServerConfig};

/// How an account found elsewhere is added here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FoundKind {
    /// IMAP and SMTP with a password.
    Imap,
    /// Sign-in with Google.
    Google,
    /// Sign-in with Microsoft.
    Microsoft,
    /// Exchange Web Services with a password.
    Exchange,
}

/// An account set up in another mail program.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FoundAccount {
    /// The program it is set up in: "Thunderbird", "Evolution", "Geary" or
    /// "GNOME Online Accounts".
    pub source: String,
    pub email: String,
    /// The sender name, when the program has one.
    pub name: String,
    pub kind: FoundKind,
    /// Incoming and outgoing servers, when the program stores them.
    pub imap: Option<ServerConfig>,
    pub smtp: Option<ServerConfig>,
    /// Exchange: the server's EWS address, when known.
    pub ews_url: Option<String>,
}

impl Engine {
    /// Accounts set up in other mail programs on this computer that are not
    /// added here yet.
    pub fn accounts_elsewhere(&self) -> Result<Vec<FoundAccount>> {
        let Some(home) = dirs::home_dir() else {
            return Ok(Vec::new());
        };
        let config = dirs::config_dir().unwrap_or_else(|| home.join(".config"));
        let added: HashSet<String> = self
            .accounts()?
            .into_iter()
            .map(|a| a.email.to_lowercase())
            .collect();
        Ok(find(&home, &config)
            .into_iter()
            .filter(|a| !added.contains(&a.email.to_lowercase()))
            .collect())
    }
}

/// The accounts set up in the other mail programs of a user whose home and
/// configuration folders are `home` and `config` (`~/.config`): each address
/// once, in the order found.
pub fn find(home: &Path, config: &Path) -> Vec<FoundAccount> {
    let mut seen = HashSet::new();
    thunderbird(home, config)
        .into_iter()
        .chain(evolution(config))
        .chain(geary(config))
        .chain(gnome(config))
        .filter(|a| a.email.contains('@') && seen.insert(a.email.to_lowercase()))
        .collect()
}

/// The sign-in a mail server's host name calls for.
fn kind_of(host: &str) -> FoundKind {
    let host = host.trim().to_ascii_lowercase();
    let under = |domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
    if under("gmail.com") || under("googlemail.com") {
        FoundKind::Google
    } else if under("office365.com") || under("outlook.com") || under("hotmail.com") {
        FoundKind::Microsoft
    } else {
        FoundKind::Imap
    }
}

/// A server with the port its security calls for when none is given.
fn server(
    host: &str,
    port: Option<u16>,
    security: Security,
    username: &str,
    smtp: bool,
) -> Option<ServerConfig> {
    let host = host.trim();
    if host.is_empty() {
        return None;
    }
    let port = port.filter(|p| *p > 0).unwrap_or(match (smtp, security) {
        (false, Security::Tls) => 993,
        (false, _) => 143,
        (true, Security::Tls) => 465,
        (true, Security::StartTls) => 587,
        (true, Security::None) => 25,
    });
    Some(ServerConfig {
        host: host.to_owned(),
        port,
        security,
        username: username.trim().to_owned(),
    })
}

/// An account from its address, name and servers, signing in the way its
/// incoming server calls for.
fn found(
    source: &str,
    email: &str,
    name: &str,
    imap: Option<ServerConfig>,
    smtp: Option<ServerConfig>,
) -> FoundAccount {
    let kind = imap.as_ref().map_or(FoundKind::Imap, |s| kind_of(&s.host));
    let (imap, smtp) = match kind {
        FoundKind::Imap => (imap, smtp),
        _ => (None, None),
    };
    FoundAccount {
        source: source.to_owned(),
        email: email.trim().to_owned(),
        name: name.trim().to_owned(),
        kind,
        imap,
        smtp,
        ews_url: None,
    }
}

// ----- key files (Evolution, Geary, GNOME, Thunderbird's profiles.ini) ------------------

/// Sections of a key file (`[Section]` and `key=value` lines), values unescaped.
#[derive(Debug, Default)]
struct KeyFile(HashMap<String, HashMap<String, String>>);

impl KeyFile {
    fn parse(text: &str) -> KeyFile {
        let mut sections: HashMap<String, HashMap<String, String>> = HashMap::new();
        let mut current = String::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                current = name.to_owned();
                sections.entry(current.clone()).or_default();
            } else if let Some((key, value)) = line.split_once('=') {
                sections
                    .entry(current.clone())
                    .or_default()
                    .insert(key.trim().to_owned(), unescape(value.trim()));
            }
        }
        KeyFile(sections)
    }

    fn read(path: &Path) -> Option<KeyFile> {
        std::fs::read_to_string(path)
            .ok()
            .map(|t| KeyFile::parse(&t))
    }

    fn get(&self, section: &str, key: &str) -> Option<&str> {
        self.0
            .get(section)?
            .get(key)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    fn has(&self, section: &str) -> bool {
        self.0.contains_key(section)
    }

    fn port(&self, section: &str, key: &str) -> Option<u16> {
        self.get(section, key)?.parse().ok()
    }

    fn yes(&self, section: &str, key: &str) -> bool {
        self.get(section, key) == Some("true")
    }
}

/// The escapes of key file values (`\s`, `\n`, `\t`, `\r`, `\\`).
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

// ----- Thunderbird ----------------------------------------------------------------------

/// Thunderbird's profile folders (the default ones first) in its usual,
/// Flatpak and Snap locations.
fn thunderbird_profiles(home: &Path, config: &Path) -> Vec<PathBuf> {
    let roots = [
        home.join(".thunderbird"),
        home.join(".var/app/org.mozilla.Thunderbird/.thunderbird"),
        home.join("snap/thunderbird/common/.thunderbird"),
        config.join("thunderbird"),
    ];
    let mut out = Vec::new();
    for root in roots {
        let Some(ini) = KeyFile::read(&root.join("profiles.ini")) else {
            continue;
        };
        let mut defaults = Vec::new();
        let mut others = Vec::new();
        let mut names: Vec<&String> = ini.0.keys().collect();
        names.sort();
        for name in names {
            let at = |path: &str, relative: bool| {
                if relative {
                    root.join(path)
                } else {
                    PathBuf::from(path)
                }
            };
            if name.starts_with("Install") {
                // The profile an installation of Thunderbird opens.
                if let Some(path) = ini.get(name, "Default") {
                    defaults.push(at(path, !path.starts_with('/')));
                }
            } else if name.starts_with("Profile")
                && let Some(path) = ini.get(name, "Path")
            {
                let dir = at(path, ini.get(name, "IsRelative") != Some("0"));
                if ini.get(name, "Default") == Some("1") {
                    defaults.push(dir);
                } else {
                    others.push(dir);
                }
            }
        }
        for dir in defaults.into_iter().chain(others) {
            if !out.contains(&dir) {
                out.push(dir);
            }
        }
    }
    out
}

/// The preferences of a Thunderbird profile (`user_pref("name", value);`).
fn parse_prefs(text: &str) -> HashMap<String, serde_json::Value> {
    text.lines()
        .filter_map(|line| {
            let inner = line.trim().strip_prefix("user_pref(")?.strip_suffix(");")?;
            // A key and a value, written as JSON is.
            match serde_json::from_str::<serde_json::Value>(&format!("[{inner}]")).ok()? {
                serde_json::Value::Array(mut pair) if pair.len() == 2 => {
                    let value = pair.pop()?;
                    Some((pair.pop()?.as_str()?.to_owned(), value))
                }
                _ => None,
            }
        })
        .collect()
}

/// Thunderbird's security of a server (`socketType`, `try_ssl`).
fn thunderbird_security(value: Option<i64>) -> Security {
    match value {
        Some(3) => Security::Tls,
        Some(1 | 2) => Security::StartTls,
        _ => Security::None,
    }
}

fn thunderbird_accounts(prefs: &HashMap<String, serde_json::Value>) -> Vec<FoundAccount> {
    let text = |key: String| prefs.get(&key).and_then(|v| v.as_str()).unwrap_or_default();
    let int = |key: String| prefs.get(&key).and_then(serde_json::Value::as_i64);
    let mut out = Vec::new();
    for account in text("mail.accountmanager.accounts".into()).split(',') {
        let account = account.trim();
        let incoming = text(format!("mail.account.{account}.server"));
        if account.is_empty() || incoming.is_empty() {
            continue;
        }
        let at = |key: &str| format!("mail.server.{incoming}.{key}");
        let identity = text(format!("mail.account.{account}.identities"))
            .split(',')
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned();
        let email = text(format!("mail.identity.{identity}.useremail"));
        let name = text(format!("mail.identity.{identity}.fullName"));
        match text(at("type")) {
            "imap" => {
                let security = thunderbird_security(int(at("socketType")));
                let port = int(at("port")).and_then(|p| u16::try_from(p).ok());
                let imap = server(
                    text(at("hostname")),
                    port,
                    security,
                    text(at("userName")),
                    false,
                );
                // The identity's outgoing server, or the default one.
                let smtp_key = Some(text(format!("mail.identity.{identity}.smtpServer")))
                    .filter(|k| !k.is_empty())
                    .unwrap_or_else(|| text("mail.smtp.defaultserver".into()));
                let smtp = ["mail.smtpserver", "mail.outgoingserver"]
                    .iter()
                    .find_map(|prefix| {
                        let at = |key: &str| format!("{prefix}.{smtp_key}.{key}");
                        let security = thunderbird_security(int(at("try_ssl")));
                        let port = int(at("port")).and_then(|p| u16::try_from(p).ok());
                        server(
                            text(at("hostname")),
                            port,
                            security,
                            text(at("username")),
                            true,
                        )
                    });
                out.push(found("Thunderbird", email, name, imap, smtp));
            }
            // Thunderbird's own Exchange accounts.
            "ews" => out.push(FoundAccount {
                ews_url: Some(text(at("ews_url")).to_owned()).filter(|u| !u.is_empty()),
                kind: FoundKind::Exchange,
                ..found("Thunderbird", email, name, None, None)
            }),
            // POP, local folders, news and feeds are not mail accounts Envelope keeps.
            _ => {}
        }
    }
    out
}

fn thunderbird(home: &Path, config: &Path) -> Vec<FoundAccount> {
    thunderbird_profiles(home, config)
        .iter()
        .filter_map(|dir| std::fs::read_to_string(dir.join("prefs.js")).ok())
        .flat_map(|prefs| thunderbird_accounts(&parse_prefs(&prefs)))
        .collect()
}

// ----- Evolution --------------------------------------------------------------------------

/// Evolution's security methods (`[Security] Method`).
fn evolution_security(source: &KeyFile) -> Security {
    match source.get("Security", "Method") {
        Some("ssl-on-alternate-port") => Security::Tls,
        Some("starttls-on-standard-port") => Security::StartTls,
        _ => Security::None,
    }
}

fn evolution(config: &Path) -> Vec<FoundAccount> {
    let Ok(dir) = std::fs::read_dir(config.join("evolution/sources")) else {
        return Vec::new();
    };
    let mut sources: Vec<(String, KeyFile)> = dir
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let uid = path.file_stem()?.to_str()?.to_owned();
            (path.extension()? == "source").then_some(())?;
            Some((uid, KeyFile::read(&path)?))
        })
        .collect();
    sources.sort_by(|a, b| a.0.cmp(&b.0));
    let by_uid: HashMap<&str, &KeyFile> = sources.iter().map(|(u, s)| (u.as_str(), s)).collect();
    let mut out = Vec::new();
    for (_, source) in &sources {
        if !source.has("Mail Account") || source.get("Data Source", "Enabled") == Some("false") {
            continue;
        }
        let identity = source
            .get("Mail Account", "IdentityUid")
            .and_then(|uid| by_uid.get(uid));
        let email = identity
            .and_then(|i| i.get("Mail Identity", "Address"))
            .unwrap_or_default();
        let name = identity
            .and_then(|i| i.get("Mail Identity", "Name"))
            .unwrap_or_default();
        match source.get("Mail Account", "BackendName") {
            Some("imapx" | "imap") => {
                let imap = server(
                    source.get("Authentication", "Host").unwrap_or_default(),
                    source.port("Authentication", "Port"),
                    evolution_security(source),
                    source.get("Authentication", "User").unwrap_or_default(),
                    false,
                );
                let transport = identity
                    .and_then(|i| i.get("Mail Submission", "TransportUid"))
                    .and_then(|uid| by_uid.get(uid))
                    .filter(|t| t.get("Mail Transport", "BackendName") == Some("smtp"));
                let smtp = transport.and_then(|t| {
                    server(
                        t.get("Authentication", "Host").unwrap_or_default(),
                        t.port("Authentication", "Port"),
                        evolution_security(t),
                        t.get("Authentication", "User").unwrap_or_default(),
                        true,
                    )
                });
                out.push(found("Evolution", email, name, imap, smtp));
            }
            Some("ews") => out.push(FoundAccount {
                ews_url: source.get("Ews Backend", "Hosturl").map(str::to_owned),
                kind: FoundKind::Exchange,
                ..found(
                    "Evolution",
                    source
                        .get("Ews Backend", "Email")
                        .filter(|_| email.is_empty())
                        .unwrap_or(email),
                    name,
                    None,
                    None,
                )
            }),
            _ => {}
        }
    }
    out
}

// ----- Geary --------------------------------------------------------------------------------

/// Geary's security (`transport_security`).
fn geary_security(value: Option<&str>) -> Security {
    match value {
        Some("start-tls") => Security::StartTls,
        Some("none") => Security::None,
        _ => Security::Tls,
    }
}

/// The first mailbox of Geary's `sender_mailboxes` ("Alex Doe <alex@example.com>;…").
fn geary_sender(value: &str) -> (String, String) {
    let first = value.split(';').next().unwrap_or_default().trim();
    match first.rsplit_once('<') {
        Some((name, rest)) => (
            rest.trim_end_matches('>').trim().to_owned(),
            name.trim().trim_matches('"').to_owned(),
        ),
        None => (first.to_owned(), String::new()),
    }
}

fn geary(config: &Path) -> Vec<FoundAccount> {
    let Ok(dir) = std::fs::read_dir(config.join("geary")) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = dir
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("account_"))
        })
        .collect();
    dirs.sort();
    let mut out = Vec::new();
    for dir in dirs {
        let Some(ini) = KeyFile::read(&dir.join("geary.ini")) else {
            continue;
        };
        let (email, name) =
            geary_sender(ini.get("Account", "sender_mailboxes").unwrap_or_default());
        let account = match ini.get("Account", "service_provider") {
            Some("gmail") => FoundAccount {
                kind: FoundKind::Google,
                ..found("Geary", &email, &name, None, None)
            },
            Some("outlook") => FoundAccount {
                kind: FoundKind::Microsoft,
                ..found("Geary", &email, &name, None, None)
            },
            // Other providers' servers are found again from the address.
            Some("other") | None => {
                let imap = server(
                    ini.get("Incoming", "host").unwrap_or_default(),
                    ini.port("Incoming", "port"),
                    geary_security(ini.get("Incoming", "transport_security")),
                    ini.get("Incoming", "login").unwrap_or_default(),
                    false,
                );
                let login = match ini.get("Outgoing", "credentials") {
                    Some("use-incoming") => ini.get("Incoming", "login"),
                    _ => ini.get("Outgoing", "login"),
                };
                let smtp = server(
                    ini.get("Outgoing", "host").unwrap_or_default(),
                    ini.port("Outgoing", "port"),
                    geary_security(ini.get("Outgoing", "transport_security")),
                    login.unwrap_or_default(),
                    true,
                );
                found("Geary", &email, &name, imap, smtp)
            }
            Some(_) => found("Geary", &email, &name, None, None),
        };
        out.push(account);
    }
    out
}

// ----- GNOME Online Accounts ---------------------------------------------------------------

/// A host with an optional `:port`.
fn host_port(value: &str) -> (&str, Option<u16>) {
    match value.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => (host, port.parse().ok()),
        _ => (value, None),
    }
}

fn gnome(config: &Path) -> Vec<FoundAccount> {
    const SOURCE: &str = "GNOME Online Accounts";
    let Some(file) = KeyFile::read(&config.join("goa-1.0/accounts.conf")) else {
        return Vec::new();
    };
    let mut names: Vec<&String> = file
        .0
        .keys()
        .filter(|n| n.starts_with("Account "))
        .collect();
    names.sort();
    let mut out = Vec::new();
    for section in names {
        let get = |key: &str| file.get(section, key).unwrap_or_default();
        if get("MailEnabled") == "false" {
            continue;
        }
        let email = [
            get("EmailAddress"),
            get("PresentationIdentity"),
            get("Identity"),
        ]
        .into_iter()
        .find(|e| e.contains('@'))
        .unwrap_or_default();
        let name = get("Name");
        let account = match get("Provider") {
            "google" => FoundAccount {
                kind: FoundKind::Google,
                ..found(SOURCE, email, name, None, None)
            },
            "ms_graph" | "windows_live" => FoundAccount {
                kind: FoundKind::Microsoft,
                ..found(SOURCE, email, name, None, None)
            },
            "exchange" => FoundAccount {
                kind: FoundKind::Exchange,
                ..found(SOURCE, email, name, None, None)
            },
            "imap_smtp" => {
                let security = |prefix: &str| {
                    if file.yes(section, &format!("{prefix}UseSsl")) {
                        Security::Tls
                    } else if file.yes(section, &format!("{prefix}UseTls")) {
                        Security::StartTls
                    } else {
                        Security::None
                    }
                };
                let (host, port) = host_port(get("ImapHost"));
                let imap = server(host, port, security("Imap"), get("ImapUserName"), false);
                let (host, port) = host_port(get("SmtpHost"));
                let smtp = server(host, port, security("Smtp"), get("SmtpUserName"), true);
                found(SOURCE, email, name, imap, smtp)
            }
            _ => continue,
        };
        out.push(account);
    }
    out
}

#[cfg(test)]
mod tests;
