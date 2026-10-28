//! Known mail providers and automatic server discovery.
//!
//! Discovery order: built-in presets by domain, MX records (catches Google
//! Workspace / Microsoft 365 on custom domains), the Thunderbird ISP database,
//! the domain's own autoconfig file, an on-premises Exchange server answering
//! Autodiscover (probed without credentials), RFC 6186 SRV records and
//! finally common host name guesses.

use std::time::Duration;

use crate::model::{AccountKind, AuthMethod, Discovery, Security, ServerConfig};

/// A built-in provider definition.
#[derive(Debug, Clone, Copy)]
pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    pub kind: AccountKind,
    pub auth: AuthMethod,
    pub domains: &'static [&'static str],
    pub mx: &'static [&'static str],
    pub imap: Option<(&'static str, u16, Security)>,
    pub smtp: Option<(&'static str, u16, Security)>,
    pub caldav: Option<&'static str>,
    pub carddav: Option<&'static str>,
    pub password_hint: Option<&'static str>,
    pub help_url: Option<&'static str>,
}

pub const PRESETS: &[Preset] = &[
    Preset {
        id: "gmail",
        name: "Gmail",
        kind: AccountKind::Gmail,
        auth: AuthMethod::OAuthGoogle,
        domains: &["gmail.com", "googlemail.com"],
        mx: &["google.com", "googlemail.com"],
        imap: Some(("imap.gmail.com", 993, Security::Tls)),
        smtp: Some(("smtp.gmail.com", 465, Security::Tls)),
        caldav: None,
        carddav: None,
        password_hint: Some("With 2-Step Verification on, you can also use an app password."),
        help_url: Some("https://myaccount.google.com/apppasswords"),
    },
    Preset {
        id: "outlook",
        name: "Outlook",
        kind: AccountKind::Microsoft,
        auth: AuthMethod::OAuthMicrosoft,
        domains: &[
            "outlook.com",
            "hotmail.com",
            "live.com",
            "msn.com",
            "passport.com",
            "outlook.de",
            "outlook.fr",
            "outlook.es",
            "outlook.it",
            "outlook.com.tr",
            "hotmail.co.uk",
            "hotmail.de",
            "hotmail.fr",
            "hotmail.it",
            "hotmail.es",
            "live.co.uk",
            "live.de",
            "live.fr",
            "live.nl",
        ],
        mx: &["protection.outlook.com", "outlook.com", "hotmail.com"],
        imap: Some(("outlook.office365.com", 993, Security::Tls)),
        smtp: Some(("smtp.office365.com", 587, Security::StartTls)),
        caldav: None,
        carddav: None,
        password_hint: None,
        help_url: None,
    },
    Preset {
        id: "icloud",
        name: "iCloud",
        kind: AccountKind::Imap,
        auth: AuthMethod::Password,
        domains: &["icloud.com", "me.com", "mac.com"],
        mx: &["mail.icloud.com"],
        imap: Some(("imap.mail.me.com", 993, Security::Tls)),
        smtp: Some(("smtp.mail.me.com", 587, Security::StartTls)),
        caldav: Some("https://caldav.icloud.com"),
        carddav: Some("https://contacts.icloud.com"),
        password_hint: Some(
            "iCloud needs an app-specific password. Create one at account.apple.com → Sign-In and Security.",
        ),
        help_url: Some("https://account.apple.com/account/manage"),
    },
    Preset {
        id: "yahoo",
        name: "Yahoo Mail",
        kind: AccountKind::Imap,
        auth: AuthMethod::Password,
        domains: &[
            "yahoo.com",
            "ymail.com",
            "rocketmail.com",
            "yahoo.co.uk",
            "yahoo.de",
            "yahoo.fr",
            "yahoo.it",
            "yahoo.es",
            "yahoo.com.tr",
            "yahoo.co.jp",
            "yahoo.ca",
            "yahoo.com.au",
            "yahoo.co.in",
        ],
        mx: &["yahoodns.net"],
        imap: Some(("imap.mail.yahoo.com", 993, Security::Tls)),
        smtp: Some(("smtp.mail.yahoo.com", 465, Security::Tls)),
        caldav: Some("https://caldav.calendar.yahoo.com"),
        carddav: Some("https://carddav.address.yahoo.com"),
        password_hint: Some(
            "Yahoo needs an app password. Generate one in Account security → App passwords.",
        ),
        help_url: Some("https://login.yahoo.com/account/security"),
    },
    Preset {
        id: "aol",
        name: "AOL",
        kind: AccountKind::Imap,
        auth: AuthMethod::Password,
        domains: &["aol.com", "aim.com"],
        mx: &[],
        imap: Some(("imap.aol.com", 993, Security::Tls)),
        smtp: Some(("smtp.aol.com", 465, Security::Tls)),
        caldav: None,
        carddav: None,
        password_hint: Some("AOL needs an app password from your account security page."),
        help_url: Some("https://login.aol.com/account/security"),
    },
    Preset {
        id: "fastmail",
        name: "Fastmail",
        kind: AccountKind::Imap,
        auth: AuthMethod::Password,
        domains: &[
            "fastmail.com",
            "fastmail.fm",
            "fastmail.net",
            "messagingengine.com",
        ],
        mx: &["messagingengine.com"],
        imap: Some(("imap.fastmail.com", 993, Security::Tls)),
        smtp: Some(("smtp.fastmail.com", 465, Security::Tls)),
        caldav: Some("https://caldav.fastmail.com/dav/"),
        carddav: Some("https://carddav.fastmail.com/dav/"),
        password_hint: Some("Create an app password in Fastmail Settings → Privacy & Security."),
        help_url: Some("https://app.fastmail.com/settings/security/devicekeys"),
    },
    Preset {
        id: "proton",
        name: "Proton Mail (Bridge)",
        kind: AccountKind::Imap,
        auth: AuthMethod::Password,
        domains: &["proton.me", "protonmail.com", "protonmail.ch", "pm.me"],
        mx: &["protonmail.ch"],
        imap: Some(("127.0.0.1", 1143, Security::StartTls)),
        smtp: Some(("127.0.0.1", 1025, Security::StartTls)),
        caldav: None,
        carddav: None,
        password_hint: Some(
            "Proton Mail works through Proton Mail Bridge. Use the password shown in the Bridge app.",
        ),
        help_url: Some("https://proton.me/mail/bridge"),
    },
    Preset {
        id: "zoho",
        name: "Zoho Mail",
        kind: AccountKind::Imap,
        auth: AuthMethod::Password,
        domains: &["zoho.com", "zohomail.com", "zoho.eu"],
        mx: &["zoho.com", "zoho.eu"],
        imap: Some(("imap.zoho.com", 993, Security::Tls)),
        smtp: Some(("smtp.zoho.com", 465, Security::Tls)),
        caldav: Some("https://calendar.zoho.com"),
        carddav: None,
        password_hint: Some(
            "If two-factor authentication is on, use an application-specific password.",
        ),
        help_url: None,
    },
    Preset {
        id: "gmx",
        name: "GMX",
        kind: AccountKind::Imap,
        auth: AuthMethod::Password,
        domains: &["gmx.com", "gmx.net", "gmx.de", "gmx.at", "gmx.ch"],
        mx: &["gmx.net", "gmx.com"],
        imap: Some(("imap.gmx.net", 993, Security::Tls)),
        smtp: Some(("mail.gmx.net", 587, Security::StartTls)),
        caldav: None,
        carddav: None,
        password_hint: Some("Enable IMAP access in the GMX web settings first."),
        help_url: None,
    },
    Preset {
        id: "webde",
        name: "WEB.DE",
        kind: AccountKind::Imap,
        auth: AuthMethod::Password,
        domains: &["web.de"],
        mx: &["web.de"],
        imap: Some(("imap.web.de", 993, Security::Tls)),
        smtp: Some(("smtp.web.de", 587, Security::StartTls)),
        caldav: None,
        carddav: None,
        password_hint: Some("Enable IMAP access in the WEB.DE web settings first."),
        help_url: None,
    },
    Preset {
        id: "yandex",
        name: "Yandex Mail",
        kind: AccountKind::Imap,
        auth: AuthMethod::Password,
        domains: &["yandex.com", "yandex.ru", "ya.ru", "yandex.com.tr"],
        mx: &["yandex.net", "yandex.ru"],
        imap: Some(("imap.yandex.com", 993, Security::Tls)),
        smtp: Some(("smtp.yandex.com", 465, Security::Tls)),
        caldav: Some("https://caldav.yandex.ru"),
        carddav: Some("https://carddav.yandex.ru"),
        password_hint: Some("Yandex needs an app password for mail clients."),
        help_url: None,
    },
    Preset {
        id: "mailru",
        name: "Mail.ru",
        kind: AccountKind::Imap,
        auth: AuthMethod::Password,
        domains: &["mail.ru", "inbox.ru", "list.ru", "bk.ru"],
        mx: &["mail.ru"],
        imap: Some(("imap.mail.ru", 993, Security::Tls)),
        smtp: Some(("smtp.mail.ru", 465, Security::Tls)),
        caldav: None,
        carddav: None,
        password_hint: Some("Mail.ru needs an app password for mail clients."),
        help_url: None,
    },
    Preset {
        id: "qq",
        name: "QQ Mail",
        kind: AccountKind::Imap,
        auth: AuthMethod::Password,
        domains: &["qq.com", "foxmail.com"],
        mx: &["qq.com"],
        imap: Some(("imap.qq.com", 993, Security::Tls)),
        smtp: Some(("smtp.qq.com", 465, Security::Tls)),
        caldav: None,
        carddav: None,
        password_hint: Some("Use the authorization code generated in QQ Mail settings."),
        help_url: None,
    },
    Preset {
        id: "netease",
        name: "NetEase Mail",
        kind: AccountKind::Imap,
        auth: AuthMethod::Password,
        domains: &["163.com", "126.com", "yeah.net"],
        mx: &["163.com", "netease.com"],
        imap: Some(("imap.163.com", 993, Security::Tls)),
        smtp: Some(("smtp.163.com", 465, Security::Tls)),
        caldav: None,
        carddav: None,
        password_hint: Some("Use the client authorization code from NetEase Mail settings."),
        help_url: None,
    },
];

pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

fn server(s: (&str, u16, Security)) -> ServerConfig {
    ServerConfig {
        host: s.0.to_string(),
        port: s.1,
        security: s.2,
        username: String::new(),
    }
}

fn from_preset(email: &str, p: &Preset) -> Discovery {
    Discovery {
        email: email.to_string(),
        provider: p.id.to_string(),
        provider_name: p.name.to_string(),
        kind: p.kind,
        auth: p.auth,
        imap: p.imap.map(server),
        smtp: p.smtp.map(server),
        caldav_url: p.caldav.map(str::to_string),
        carddav_url: p.carddav.map(str::to_string),
        password_hint: p.password_hint.map(str::to_string),
        password_help_url: p.help_url.map(str::to_string),
    }
}

/// An on-premises Exchange server (Exchange Web Services, signed in with the
/// Windows password).
fn exchange_discovery(email: &str) -> Discovery {
    Discovery {
        email: email.to_string(),
        provider: "exchange".into(),
        provider_name: "Exchange".into(),
        kind: AccountKind::Exchange,
        auth: AuthMethod::Password,
        imap: None,
        smtp: None,
        caldav_url: None,
        carddav_url: None,
        password_hint: Some(
            "Use the password you sign in to your work computer or Outlook with.".into(),
        ),
        password_help_url: None,
    }
}

fn domain_of(email: &str) -> Option<String> {
    email
        .rsplit_once('@')
        .map(|(_, d)| d.trim().to_ascii_lowercase())
        .filter(|d| d.contains('.'))
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(6))
        .user_agent(concat!("Envelope/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_default()
}

/// Detects provider and server settings for an address.
pub async fn discover(email: &str) -> Discovery {
    let email = email.trim();
    let generic = |imap: Option<ServerConfig>, smtp: Option<ServerConfig>| Discovery {
        email: email.to_string(),
        provider: "generic".into(),
        provider_name: "IMAP".into(),
        kind: AccountKind::Imap,
        auth: AuthMethod::Password,
        imap,
        smtp,
        caldav_url: None,
        carddav_url: None,
        password_hint: None,
        password_help_url: None,
    };
    let Some(domain) = domain_of(email) else {
        return generic(None, None);
    };
    if let Some(p) = PRESETS
        .iter()
        .find(|p| p.domains.contains(&domain.as_str()))
    {
        return from_preset(email, p);
    }

    let mx_hosts = mx_lookup(&domain).await;
    for host in &mx_hosts {
        if let Some(p) = PRESETS.iter().find(|p| {
            p.mx.iter()
                .any(|m| host == m || host.ends_with(&format!(".{m}")))
        }) {
            let mut d = from_preset(email, p);
            if p.kind == AccountKind::Imap {
                // Custom domain hosted by the provider: same servers.
                d.provider = p.id.to_string();
            }
            return d;
        }
    }

    // Exchange shows itself on `autodiscover.<domain>` (checked without
    // credentials, alongside the autoconfig lookups).
    let (autoconfigured, exchange) = tokio::join!(
        autoconfig(email, &domain, &mx_hosts),
        crate::sync::ews::exchange_hint(&domain),
    );
    if exchange == crate::sync::ews::ExchangeHint::Microsoft365
        && let Some(p) = preset("outlook")
    {
        return from_preset(email, p);
    }
    if let Some((imap, smtp)) = autoconfigured {
        let mut d = generic(imap, smtp);
        d.caldav_url = Some(format!("https://{domain}"));
        return d;
    }
    if exchange == crate::sync::ews::ExchangeHint::OnPremises {
        return exchange_discovery(email);
    }
    if let Some((imap, smtp)) = srv_lookup(&domain).await {
        return generic(imap, smtp);
    }
    let imap = Some(ServerConfig {
        host: format!("imap.{domain}"),
        port: 993,
        security: Security::Tls,
        username: String::new(),
    });
    let smtp = Some(ServerConfig {
        host: format!("smtp.{domain}"),
        port: 465,
        security: Security::Tls,
        username: String::new(),
    });
    generic(imap, smtp)
}

async fn resolver() -> Option<hickory_resolver::TokioResolver> {
    hickory_resolver::TokioResolver::builder_tokio()
        .ok()?
        .build()
        .ok()
}

async fn mx_lookup(domain: &str) -> Vec<String> {
    let Some(r) = resolver().await else {
        return Vec::new();
    };
    let Ok(Ok(lookup)) = tokio::time::timeout(Duration::from_secs(4), r.mx_lookup(domain)).await
    else {
        return Vec::new();
    };
    let mut hosts: Vec<(u16, String)> = lookup
        .answers()
        .iter()
        .filter_map(|rec| match &rec.data {
            hickory_resolver::proto::rr::RData::MX(mx) => Some((
                mx.preference,
                mx.exchange
                    .to_string()
                    .trim_end_matches('.')
                    .to_ascii_lowercase(),
            )),
            _ => None,
        })
        .collect();
    hosts.sort();
    hosts.into_iter().map(|(_, h)| h).collect()
}

async fn srv_lookup(domain: &str) -> Option<(Option<ServerConfig>, Option<ServerConfig>)> {
    let r = resolver().await?;
    let srv = |name: String| {
        let r = r.clone();
        async move {
            let lookup = tokio::time::timeout(Duration::from_secs(3), r.srv_lookup(name))
                .await
                .ok()?
                .ok()?;
            lookup.answers().iter().find_map(|rec| match &rec.data {
                hickory_resolver::proto::rr::RData::SRV(s) => {
                    let host = s.target.to_string().trim_end_matches('.').to_string();
                    (!host.is_empty() && host != ".").then_some((host, s.port))
                }
                _ => None,
            })
        }
    };
    let (imaps, imap, subs, sub) = tokio::join!(
        srv(format!("_imaps._tcp.{domain}")),
        srv(format!("_imap._tcp.{domain}")),
        srv(format!("_submissions._tcp.{domain}")),
        srv(format!("_submission._tcp.{domain}")),
    );
    let imap = imaps
        .map(|(h, p)| ServerConfig {
            host: h,
            port: p,
            security: Security::Tls,
            username: String::new(),
        })
        .or_else(|| {
            imap.map(|(h, p)| ServerConfig {
                host: h,
                port: p,
                security: Security::StartTls,
                username: String::new(),
            })
        });
    let smtp = subs
        .map(|(h, p)| ServerConfig {
            host: h,
            port: p,
            security: Security::Tls,
            username: String::new(),
        })
        .or_else(|| {
            sub.map(|(h, p)| ServerConfig {
                host: h,
                port: p,
                security: Security::StartTls,
                username: String::new(),
            })
        });
    (imap.is_some() || smtp.is_some()).then_some((imap, smtp))
}

/// Thunderbird-style autoconfig (ISPDB and the domain's own config file).
async fn autoconfig(
    email: &str,
    domain: &str,
    mx_hosts: &[String],
) -> Option<(Option<ServerConfig>, Option<ServerConfig>)> {
    let client = http();
    let enc = percent_encoding::utf8_percent_encode(email, percent_encoding::NON_ALPHANUMERIC)
        .to_string();
    let mut urls = vec![
        format!("https://autoconfig.thunderbird.net/v1.1/{domain}"),
        format!("https://autoconfig.{domain}/mail/config-v1.1.xml?emailaddress={enc}"),
        format!("https://{domain}/.well-known/autoconfig/mail/config-v1.1.xml?emailaddress={enc}"),
    ];
    if let Some(mx) = mx_hosts.first() {
        let parts: Vec<&str> = mx.split('.').collect();
        if parts.len() >= 2 {
            let base = parts[parts.len() - 2..].join(".");
            if base != domain {
                urls.push(format!("https://autoconfig.thunderbird.net/v1.1/{base}"));
            }
        }
    }
    let fetches = urls.into_iter().map(|u| {
        let client = client.clone();
        async move {
            let resp = client.get(&u).send().await.ok()?;
            if !resp.status().is_success() {
                return None;
            }
            let text = resp.text().await.ok()?;
            parse_autoconfig(&text, email)
        }
    });
    let results = futures::future::join_all(fetches).await;
    results.into_iter().flatten().next()
}

/// Parses a Thunderbird autoconfig XML document.
pub fn parse_autoconfig(
    xml: &str,
    email: &str,
) -> Option<(Option<ServerConfig>, Option<ServerConfig>)> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut imap: Option<ServerConfig> = None;
    let mut smtp: Option<ServerConfig> = None;
    let mut current: Option<(String, ServerConfig, bool)> = None; // (type, server, has_password_auth)
    let mut field = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = e.local_name().as_ref().to_string();
                if name == "incomingServer" || name == "outgoingServer" {
                    let ty = e
                        .attributes()
                        .flatten()
                        .find(|a| a.key.as_ref() == "type")
                        .map(|a| a.value.to_string())
                        .unwrap_or_default();
                    current = Some((ty, ServerConfig::default(), false));
                }
                field = name;
            }
            Ok(Event::Text(t)) => {
                let text = t.xml10_content().trim().to_string();
                if let Some((_, s, password)) = current.as_mut() {
                    match field.as_str() {
                        "hostname" => s.host = text,
                        "port" => s.port = text.parse().unwrap_or(0),
                        "socketType" => {
                            s.security = match text.as_str() {
                                "SSL" => Security::Tls,
                                "STARTTLS" => Security::StartTls,
                                _ => Security::None,
                            }
                        }
                        "username" => {
                            let local = email.split('@').next().unwrap_or("");
                            let domain = email.split('@').nth(1).unwrap_or("");
                            s.username = text
                                .replace("%EMAILADDRESS%", email)
                                .replace("%EMAILLOCALPART%", local)
                                .replace("%EMAILDOMAIN%", domain);
                        }
                        "authentication" if text.starts_with("password") => *password = true,
                        _ => {}
                    }
                }
            }
            Ok(Event::End(e)) => {
                let name = e.local_name().as_ref().to_string();
                if (name == "incomingServer" || name == "outgoingServer")
                    && let Some((ty, mut s, _)) = current.take()
                {
                    if s.username == email {
                        s.username = String::new();
                    }
                    if ty == "imap" && imap.is_none() && !s.host.is_empty() {
                        imap = Some(s);
                    } else if ty == "smtp" && smtp.is_none() && !s.host.is_empty() {
                        smtp = Some(s);
                    }
                }
                field.clear();
            }
            Ok(Event::Eof) => break,
            Err(_) => return None,
            _ => {}
        }
    }
    imap.as_ref()?;
    Some((imap, smtp))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autoconfig_xml() {
        let xml = r#"<?xml version="1.0"?>
<clientConfig version="1.1"><emailProvider id="example.com">
<incomingServer type="pop3"><hostname>pop.example.com</hostname><port>995</port><socketType>SSL</socketType></incomingServer>
<incomingServer type="imap"><hostname>mail.example.com</hostname><port>993</port><socketType>SSL</socketType>
<username>%EMAILLOCALPART%</username><authentication>password-cleartext</authentication></incomingServer>
<outgoingServer type="smtp"><hostname>smtp.example.com</hostname><port>587</port><socketType>STARTTLS</socketType>
<username>%EMAILADDRESS%</username></outgoingServer></emailProvider></clientConfig>"#;
        let (imap, smtp) = parse_autoconfig(xml, "jo@example.com").unwrap();
        let imap = imap.unwrap();
        assert_eq!(imap.host, "mail.example.com");
        assert_eq!(imap.username, "jo");
        let smtp = smtp.unwrap();
        assert_eq!(smtp.security, Security::StartTls);
        assert_eq!(smtp.username, "");
    }

    #[tokio::test]
    async fn preset_domains() {
        let d = discover("someone@icloud.com").await;
        assert_eq!(d.provider, "icloud");
        assert!(d.caldav_url.is_some());
        let d = discover("x@hotmail.com").await;
        assert_eq!(d.kind, AccountKind::Microsoft);
    }
}
