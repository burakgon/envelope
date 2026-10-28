//! SMTP submission.

use std::time::Duration;

use lettre::address::Envelope;
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};

use crate::error::{Error, Result};
use crate::model::{Security, ServerConfig};

/// SMTP credentials.
pub enum SmtpAuth {
    Password { user: String, password: String },
    XOAuth2 { user: String, token: String },
}

fn transport(
    server: &ServerConfig,
    auth: &SmtpAuth,
    accept_invalid_certs: bool,
) -> Result<AsyncSmtpTransport<Tokio1Executor>> {
    let tls_params = || {
        TlsParameters::builder(server.host.clone())
            .dangerous_accept_invalid_certs(accept_invalid_certs)
            .build_rustls()
            .map_err(|e| Error::Smtp(e.to_string()))
    };
    let tls = match server.security {
        Security::Tls => Tls::Wrapper(tls_params()?),
        Security::StartTls => Tls::Required(tls_params()?),
        Security::None => Tls::None,
    };
    let (credentials, mechanisms) = match auth {
        SmtpAuth::Password { user, password } => (
            Credentials::new(user.clone(), password.clone()),
            vec![Mechanism::Plain, Mechanism::Login],
        ),
        SmtpAuth::XOAuth2 { user, token } => (
            Credentials::new(user.clone(), token.clone()),
            vec![Mechanism::Xoauth2],
        ),
    };
    Ok(
        AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(server.host.clone())
            .port(server.port)
            .tls(tls)
            .credentials(credentials)
            .authentication(mechanisms)
            .timeout(Some(Duration::from_secs(60)))
            .build(),
    )
}

fn map_err(e: lettre::transport::smtp::Error) -> Error {
    let text = e.to_string();
    let lower = text.to_ascii_lowercase();
    if lower.contains("authentication") || lower.contains("535") || lower.contains("534") {
        Error::Auth(format!("The SMTP server rejected the login: {text}"))
    } else if e.is_transient()
        || lower.contains("connection")
        || lower.contains("timed out")
        || lower.contains("network")
    {
        Error::Network(text)
    } else {
        Error::Smtp(text)
    }
}

/// Sends a rendered message.
pub async fn send(
    server: &ServerConfig,
    auth: &SmtpAuth,
    accept_invalid_certs: bool,
    from: &str,
    recipients: &[String],
    mime: &[u8],
) -> Result<()> {
    let transport = transport(server, auth, accept_invalid_certs)?;
    let from = from
        .parse()
        .map_err(|_| Error::Invalid(format!("invalid sender address {from}")))?;
    let mut to = Vec::with_capacity(recipients.len());
    for r in recipients {
        to.push(
            r.parse()
                .map_err(|_| Error::Invalid(format!("invalid recipient address {r}")))?,
        );
    }
    let envelope = Envelope::new(Some(from), to).map_err(|e| Error::Invalid(e.to_string()))?;
    transport.send_raw(&envelope, mime).await.map_err(map_err)?;
    Ok(())
}

/// Connects and authenticates without sending anything.
pub async fn test(
    server: &ServerConfig,
    auth: &SmtpAuth,
    accept_invalid_certs: bool,
) -> Result<()> {
    let transport = transport(server, auth, accept_invalid_certs)?;
    match transport.test_connection().await {
        Ok(true) => Ok(()),
        Ok(false) => Err(Error::Network(format!(
            "could not connect to {}",
            server.host
        ))),
        Err(e) => Err(map_err(e)),
    }
}
