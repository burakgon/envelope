//! Authentication: HTTP Basic, HTTP Digest (RFC 7616 / RFC 2617) and OAuth 2
//! bearer tokens.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use md5::Md5;
use sha2::{Digest as _, Sha256};

use crate::util::hex;

/// Supplies OAuth 2 access tokens for [`Auth::Bearer`].
///
/// Implementations are expected to cache the token and refresh it when it
/// expires. When the server rejects a token with `401 Unauthorized`, the
/// client calls [`invalidate`](TokenSource::invalidate) and retries the
/// request once with a freshly obtained token.
#[async_trait]
pub trait TokenSource: Send + Sync {
    /// Returns a currently valid access token, refreshing it if needed.
    /// The error string is surfaced as [`DavError::TokenSource`](crate::DavError::TokenSource).
    async fn access_token(&self) -> Result<String, String>;

    /// Marks the most recently returned token as rejected so that the next
    /// call to [`access_token`](TokenSource::access_token) refreshes it.
    async fn invalidate(&self);
}

/// How the client authenticates.
#[derive(Clone, Default)]
pub enum Auth {
    /// No credentials.
    #[default]
    None,
    /// Username and password.
    ///
    /// They are sent as HTTP Basic credentials, pre-emptively over HTTPS and
    /// to loopback hosts, otherwise only once the server asks for them. When
    /// the server challenges with HTTP Digest (older Baïkal / sabre/dav
    /// setups), the client switches to Digest automatically (MD5, MD5-sess,
    /// SHA-256, SHA-256-sess; `qop=auth`, `auth-int` or none) and keeps using
    /// it for that server.
    Basic {
        /// The user name.
        username: String,
        /// The password (for iCloud, Yahoo and Fastmail an app-specific
        /// password).
        password: String,
    },
    /// OAuth 2 bearer tokens (for example Google CalDAV). Tokens are only
    /// sent over HTTPS or to loopback hosts, never in clear text.
    Bearer(Arc<dyn TokenSource>),
}

impl Auth {
    /// Username/password credentials (see [`Auth::Basic`]).
    pub fn basic(username: impl Into<String>, password: impl Into<String>) -> Self {
        Auth::Basic {
            username: username.into(),
            password: password.into(),
        }
    }

    /// Bearer tokens obtained from `source`.
    pub fn bearer(source: impl TokenSource + 'static) -> Self {
        Auth::Bearer(Arc::new(source))
    }
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Auth::None => f.write_str("None"),
            Auth::Basic { username, .. } => f
                .debug_struct("Basic")
                .field("username", username)
                .field("password", &"<redacted>")
                .finish(),
            Auth::Bearer(_) => f.write_str("Bearer(<token source>)"),
        }
    }
}

/// `Authorization` header value for HTTP Basic.
pub(crate) fn basic_header(username: &str, password: &str) -> String {
    let encoded =
        base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
    format!("Basic {encoded}")
}

/// One challenge from a `WWW-Authenticate` header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Challenge {
    /// Lower-cased scheme name (`basic`, `digest`, `bearer`, ...).
    pub(crate) scheme: String,
    /// Parameters with lower-cased names.
    pub(crate) params: Vec<(String, String)>,
}

impl Challenge {
    fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Parses the values of all `WWW-Authenticate` headers of a response
/// (RFC 7235 §4.1). A single header may carry several comma-separated
/// challenges, and parameter values may be quoted strings containing commas.
pub(crate) fn parse_challenges<'a>(headers: impl IntoIterator<Item = &'a str>) -> Vec<Challenge> {
    let mut challenges = Vec::new();
    for header in headers {
        parse_challenge_header(header, &mut challenges);
    }
    challenges
}

fn parse_challenge_header(header: &str, out: &mut Vec<Challenge>) {
    let mut chars = header.char_indices().peekable();
    // Index into `out` of the challenge currently collecting parameters.
    let mut current: Option<usize> = None;

    loop {
        // Skip separators.
        while chars
            .next_if(|&(_, c)| c == ',' || c.is_whitespace())
            .is_some()
        {}
        let Some(&(start, _)) = chars.peek() else {
            break;
        };
        // Read a token.
        let mut end = start;
        while let Some((idx, ch)) =
            chars.next_if(|&(_, c)| !(c == ',' || c == '=' || c.is_whitespace()))
        {
            end = idx + ch.len_utf8();
        }
        let token = &header[start..end];
        while chars.next_if(|&(_, c)| c == ' ' || c == '\t').is_some() {}

        if chars.next_if(|&(_, c)| c == '=').is_some() {
            while chars.next_if(|&(_, c)| c == ' ' || c == '\t').is_some() {}
            let value = if chars.next_if(|&(_, c)| c == '"').is_some() {
                let mut value = String::new();
                while let Some((_, ch)) = chars.next() {
                    match ch {
                        '"' => break,
                        '\\' => {
                            if let Some((_, escaped)) = chars.next() {
                                value.push(escaped);
                            }
                        }
                        _ => value.push(ch),
                    }
                }
                value
            } else {
                let mut value = String::new();
                while let Some((_, ch)) = chars.next_if(|&(_, c)| c != ',' && !c.is_whitespace()) {
                    value.push(ch);
                }
                value
            };
            if let Some(challenge) = current.and_then(|index| out.get_mut(index)) {
                challenge.params.push((token.to_ascii_lowercase(), value));
            }
        } else if token.is_empty() {
            // A stray character that cannot start a token; skip it.
            chars.next();
        } else {
            out.push(Challenge {
                scheme: token.to_ascii_lowercase(),
                params: Vec::new(),
            });
            current = Some(out.len() - 1);
        }
    }
}

/// Hash algorithm of a Digest challenge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DigestAlgorithm {
    Md5,
    Md5Sess,
    Sha256,
    Sha256Sess,
}

impl DigestAlgorithm {
    fn parse(value: Option<&str>) -> Option<Self> {
        match value.map(str::to_ascii_uppercase).as_deref() {
            None | Some("MD5") => Some(DigestAlgorithm::Md5),
            Some("MD5-SESS") => Some(DigestAlgorithm::Md5Sess),
            Some("SHA-256") => Some(DigestAlgorithm::Sha256),
            Some("SHA-256-SESS") => Some(DigestAlgorithm::Sha256Sess),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            DigestAlgorithm::Md5 => "MD5",
            DigestAlgorithm::Md5Sess => "MD5-sess",
            DigestAlgorithm::Sha256 => "SHA-256",
            DigestAlgorithm::Sha256Sess => "SHA-256-sess",
        }
    }

    fn is_session(self) -> bool {
        matches!(self, DigestAlgorithm::Md5Sess | DigestAlgorithm::Sha256Sess)
    }

    fn strength(self) -> u8 {
        match self {
            DigestAlgorithm::Md5 | DigestAlgorithm::Md5Sess => 1,
            DigestAlgorithm::Sha256 | DigestAlgorithm::Sha256Sess => 2,
        }
    }

    fn hash(self, data: &[u8]) -> String {
        match self {
            DigestAlgorithm::Md5 | DigestAlgorithm::Md5Sess => hex(&Md5::digest(data)),
            DigestAlgorithm::Sha256 | DigestAlgorithm::Sha256Sess => hex(&Sha256::digest(data)),
        }
    }
}

/// Quality of protection selected for a Digest exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Qop {
    Auth,
    AuthInt,
}

/// A parsed Digest challenge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DigestChallenge {
    pub(crate) realm: String,
    pub(crate) nonce: String,
    pub(crate) opaque: Option<String>,
    pub(crate) algorithm: DigestAlgorithm,
    /// Whether the server named the algorithm explicitly (it is then echoed).
    pub(crate) algorithm_explicit: bool,
    pub(crate) qop: Option<Qop>,
    pub(crate) stale: bool,
    pub(crate) userhash: bool,
}

impl DigestChallenge {
    fn from_challenge(challenge: &Challenge) -> Option<Self> {
        if challenge.scheme != "digest" {
            return None;
        }
        let nonce = challenge.param("nonce")?.to_owned();
        let algorithm_param = challenge.param("algorithm");
        let algorithm = DigestAlgorithm::parse(algorithm_param)?;
        let qop = match challenge.param("qop") {
            None => None,
            Some(list) => {
                let options: Vec<String> = list
                    .split(',')
                    .map(|option| option.trim().to_ascii_lowercase())
                    .collect();
                if options.iter().any(|option| option == "auth") {
                    Some(Qop::Auth)
                } else if options.iter().any(|option| option == "auth-int") {
                    Some(Qop::AuthInt)
                } else {
                    // Only unknown protection levels offered.
                    return None;
                }
            }
        };
        let flag = |name: &str| {
            challenge
                .param(name)
                .is_some_and(|value| value.eq_ignore_ascii_case("true"))
        };
        Some(DigestChallenge {
            realm: challenge.param("realm").unwrap_or_default().to_owned(),
            nonce,
            opaque: challenge.param("opaque").map(str::to_owned),
            algorithm,
            algorithm_explicit: algorithm_param.is_some(),
            qop,
            stale: flag("stale"),
            userhash: flag("userhash"),
        })
    }

    /// Picks the strongest supported Digest challenge among `challenges`.
    pub(crate) fn select(challenges: &[Challenge]) -> Option<Self> {
        challenges
            .iter()
            .filter_map(DigestChallenge::from_challenge)
            .max_by_key(|challenge| challenge.algorithm.strength())
    }
}

/// Digest state for one server: the current challenge and nonce counter.
#[derive(Debug, Clone)]
pub(crate) struct DigestSession {
    pub(crate) challenge: DigestChallenge,
    nonce_count: u32,
}

/// What is needed to compute a Digest response for one request.
pub(crate) struct DigestRequest<'a> {
    pub(crate) username: &'a str,
    pub(crate) password: &'a str,
    pub(crate) method: &'a str,
    /// The request target (`path?query`).
    pub(crate) uri: &'a str,
    pub(crate) body: &'a [u8],
    pub(crate) cnonce: &'a str,
}

impl DigestSession {
    pub(crate) fn new(challenge: DigestChallenge) -> Self {
        DigestSession {
            challenge,
            nonce_count: 0,
        }
    }

    /// Computes the `Authorization` header for one request and advances the
    /// nonce counter.
    pub(crate) fn authorization(&mut self, request: &DigestRequest<'_>) -> String {
        self.nonce_count = self.nonce_count.wrapping_add(1);
        let challenge = &self.challenge;
        let algorithm = challenge.algorithm;
        let nc = format!("{:08x}", self.nonce_count);

        let mut ha1 = algorithm.hash(
            format!(
                "{}:{}:{}",
                request.username, challenge.realm, request.password
            )
            .as_bytes(),
        );
        if algorithm.is_session() {
            ha1 =
                algorithm.hash(format!("{ha1}:{}:{}", challenge.nonce, request.cnonce).as_bytes());
        }
        let ha2 = match challenge.qop {
            Some(Qop::AuthInt) => algorithm.hash(
                format!(
                    "{}:{}:{}",
                    request.method,
                    request.uri,
                    algorithm.hash(request.body)
                )
                .as_bytes(),
            ),
            _ => algorithm.hash(format!("{}:{}", request.method, request.uri).as_bytes()),
        };
        let qop_name = match challenge.qop {
            Some(Qop::Auth) => Some("auth"),
            Some(Qop::AuthInt) => Some("auth-int"),
            None => None,
        };
        let response = match qop_name {
            Some(qop) => algorithm.hash(
                format!(
                    "{ha1}:{}:{nc}:{}:{qop}:{ha2}",
                    challenge.nonce, request.cnonce
                )
                .as_bytes(),
            ),
            None => algorithm.hash(format!("{ha1}:{}:{ha2}", challenge.nonce).as_bytes()),
        };

        let username = if challenge.userhash {
            algorithm.hash(format!("{}:{}", request.username, challenge.realm).as_bytes())
        } else {
            request.username.to_owned()
        };

        let mut header = format!(
            "Digest username={}, realm={}, nonce={}, uri={}",
            quote(&username),
            quote(&challenge.realm),
            quote(&challenge.nonce),
            quote(request.uri),
        );
        if challenge.algorithm_explicit {
            header.push_str(", algorithm=");
            header.push_str(algorithm.name());
        }
        if let Some(qop) = qop_name {
            header.push_str(&format!(
                ", qop={qop}, nc={nc}, cnonce={}",
                quote(request.cnonce)
            ));
        }
        header.push_str(&format!(", response={}", quote(&response)));
        if let Some(opaque) = &challenge.opaque {
            header.push_str(&format!(", opaque={}", quote(opaque)));
        }
        if challenge.userhash {
            header.push_str(", userhash=true");
        }
        header
    }
}

/// Formats an HTTP quoted-string.
fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        if ch == '"' || ch == '\\' {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

/// A fresh client nonce.
pub(crate) fn new_cnonce() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiple_challenges_in_one_header() {
        let challenges = parse_challenges([
            r#"Digest realm="Baikal, the CalDAV server", nonce="abc,def", qop="auth,auth-int", algorithm=MD5, Basic realm="x""#,
        ]);
        assert_eq!(challenges.len(), 2);
        assert_eq!(challenges[0].scheme, "digest");
        assert_eq!(
            challenges[0].param("realm"),
            Some("Baikal, the CalDAV server")
        );
        assert_eq!(challenges[0].param("nonce"), Some("abc,def"));
        assert_eq!(challenges[0].param("qop"), Some("auth,auth-int"));
        assert_eq!(challenges[0].param("algorithm"), Some("MD5"));
        assert_eq!(challenges[1].scheme, "basic");
        assert_eq!(challenges[1].param("realm"), Some("x"));
    }

    #[test]
    fn parses_multiple_headers_escapes_and_token68() {
        let challenges = parse_challenges([
            r#"Bearer realm="example", error="invalid_token", error_description="The \"token\" expired""#,
            "Negotiate YIIBhwYGKwYBBQUCoIIBezCCAXegDTALBgkqhkiG9xIBAgKiggFk==",
            "Basic",
        ]);
        let schemes: Vec<&str> = challenges.iter().map(|c| c.scheme.as_str()).collect();
        assert_eq!(schemes, ["bearer", "negotiate", "basic"]);
        assert_eq!(
            challenges[0].param("error_description"),
            Some(r#"The "token" expired"#)
        );
    }

    #[test]
    fn tolerates_garbage() {
        assert!(parse_challenges([""]).is_empty());
        assert!(parse_challenges([", ,"]).is_empty());
        // Parameters before any scheme are ignored rather than panicking.
        assert!(parse_challenges([r#"realm="x""#]).is_empty());
        let challenges = parse_challenges(["Basic realm=\"unterminated"]);
        assert_eq!(challenges[0].param("realm"), Some("unterminated"));
    }

    #[test]
    fn rfc2617_md5_vector() {
        let challenge = DigestChallenge::select(&parse_challenges([
            r#"Digest realm="testrealm@host.com", qop="auth,auth-int", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093", opaque="5ccc069c403ebaf9f0171e9517f40e41""#,
        ]))
        .expect("digest challenge");
        assert_eq!(challenge.qop, Some(Qop::Auth));
        let mut session = DigestSession::new(challenge);
        let header = session.authorization(&DigestRequest {
            username: "Mufasa",
            password: "Circle Of Life",
            method: "GET",
            uri: "/dir/index.html",
            body: b"",
            cnonce: "0a4f113b",
        });
        assert_eq!(
            header,
            r#"Digest username="Mufasa", realm="testrealm@host.com", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093", uri="/dir/index.html", qop=auth, nc=00000001, cnonce="0a4f113b", response="6629fae49393a05397450978507c4ef1", opaque="5ccc069c403ebaf9f0171e9517f40e41""#
        );
        // The nonce counter advances with every request.
        let second = session.authorization(&DigestRequest {
            username: "Mufasa",
            password: "Circle Of Life",
            method: "GET",
            uri: "/dir/index.html",
            body: b"",
            cnonce: "0a4f113b",
        });
        assert!(second.contains("nc=00000002"));
    }

    #[test]
    fn rfc7616_sha256_vector_is_preferred_over_md5() {
        let challenges = parse_challenges([
            r#"Digest realm="http-auth@example.org", qop="auth, auth-int", algorithm=SHA-256, nonce="7ypf/xlj9XXwfDPEoM4URrv/xwf94BcCAzFZH4GiTo0v", opaque="FQhe/qaU925kfnzjCev0ciny7QMkPqMAFRtzCUYo5tdS""#,
            r#"Digest realm="http-auth@example.org", qop="auth, auth-int", algorithm=MD5, nonce="7ypf/xlj9XXwfDPEoM4URrv/xwf94BcCAzFZH4GiTo0v", opaque="FQhe/qaU925kfnzjCev0ciny7QMkPqMAFRtzCUYo5tdS""#,
        ]);
        let challenge = DigestChallenge::select(&challenges).expect("digest challenge");
        assert_eq!(challenge.algorithm, DigestAlgorithm::Sha256);
        let request = DigestRequest {
            username: "Mufasa",
            password: "Circle of Life",
            method: "GET",
            uri: "/dir/index.html",
            body: b"",
            cnonce: "f2/wE4q74E6zIJEtWaHKaf5wv/H5QzzpXusqGemxURZJ",
        };
        let header = DigestSession::new(challenge).authorization(&request);
        assert!(
            header.contains(
                r#"response="753927fa0e85d155564e2e272a28d1802ca10daf4496794697cf8db5856cb6c1""#
            ),
            "{header}"
        );
        assert!(header.contains("algorithm=SHA-256"));

        let md5 = challenges
            .iter()
            .filter_map(DigestChallenge::from_challenge)
            .find(|c| c.algorithm == DigestAlgorithm::Md5)
            .expect("md5 challenge");
        let header = DigestSession::new(md5).authorization(&request);
        assert!(
            header.contains(r#"response="8ca523f5e9506fed4657c9700eebdbec""#),
            "{header}"
        );
    }

    #[test]
    fn legacy_digest_without_qop() {
        let challenge = DigestChallenge::select(&parse_challenges([
            r#"Digest realm="testrealm@host.com", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093""#,
        ]))
        .expect("digest challenge");
        let header = DigestSession::new(challenge).authorization(&DigestRequest {
            username: "Mufasa",
            password: "Circle Of Life",
            method: "GET",
            uri: "/dir/index.html",
            body: b"",
            cnonce: "unused",
        });
        // RFC 2069 response = MD5(HA1:nonce:HA2)
        let ha1 = DigestAlgorithm::Md5.hash(b"Mufasa:testrealm@host.com:Circle Of Life");
        let ha2 = DigestAlgorithm::Md5.hash(b"GET:/dir/index.html");
        let expected = DigestAlgorithm::Md5
            .hash(format!("{ha1}:dcd98b7102dd2f0e8b11d0f600bfb0c093:{ha2}").as_bytes());
        assert!(header.contains(&format!("response=\"{expected}\"")));
        assert!(!header.contains("qop="));
        assert!(!header.contains("algorithm="));
    }

    #[test]
    fn session_variants_auth_int_and_userhash() {
        let challenge = DigestChallenge::select(&parse_challenges([
            r#"Digest realm="r", nonce="n", qop="auth-int", algorithm=MD5-sess, userhash=true, stale=TRUE"#,
        ]))
        .expect("digest challenge");
        assert!(challenge.stale);
        assert!(challenge.userhash);
        assert_eq!(challenge.qop, Some(Qop::AuthInt));
        let header = DigestSession::new(challenge).authorization(&DigestRequest {
            username: "u",
            password: "p",
            method: "PUT",
            uri: "/a.ics",
            body: b"BEGIN:VCALENDAR",
            cnonce: "c",
        });
        let md5 = DigestAlgorithm::Md5;
        let ha1 = md5.hash(format!("{}:n:c", md5.hash(b"u:r:p")).as_bytes());
        let ha2 = md5.hash(format!("PUT:/a.ics:{}", md5.hash(b"BEGIN:VCALENDAR")).as_bytes());
        let expected = md5.hash(format!("{ha1}:n:00000001:c:auth-int:{ha2}").as_bytes());
        assert!(
            header.contains(&format!("response=\"{expected}\"")),
            "{header}"
        );
        assert!(header.contains(&format!("username=\"{}\"", md5.hash(b"u:r"))));
        assert!(header.contains("userhash=true"));
        assert!(header.contains("algorithm=MD5-sess"));
    }

    #[test]
    fn unsupported_digest_variants_are_ignored() {
        assert!(
            DigestChallenge::select(&parse_challenges([
                r#"Digest realm="r", nonce="n", algorithm=SHA-512-256"#
            ]))
            .is_none()
        );
        assert!(
            DigestChallenge::select(&parse_challenges([
                r#"Digest realm="r", nonce="n", qop="auth-conf""#
            ]))
            .is_none()
        );
        assert!(DigestChallenge::select(&parse_challenges([r#"Digest realm="r""#])).is_none());
        assert!(DigestChallenge::select(&parse_challenges([r#"Basic realm="r""#])).is_none());
    }

    #[test]
    fn quoting_and_basic() {
        assert_eq!(quote(r#"a"b\c"#), r#""a\"b\\c""#);
        assert_eq!(
            basic_header("Aladdin", "open sesame"),
            "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ=="
        );
        assert_eq!(new_cnonce().len(), 32);
    }

    #[test]
    fn debug_output_redacts_secrets() {
        let auth = Auth::basic("jane", "hunter2");
        let debug = format!("{auth:?}");
        assert!(debug.contains("jane"));
        assert!(!debug.contains("hunter2"));
    }
}
