//! Adds the development server account (scripts/devserver/run.sh) to a data
//! directory, so the app can be started against it without clicking through
//! onboarding.
//!
//! cargo run -p envelope-core --example dev_account -- /tmp/envelope-dev

use envelope_core::model::*;
use envelope_core::{Engine, EventSink, Paths};

struct Quiet;
impl EventSink for Quiet {
    fn emit(&self, _: EngineEvent) {}
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).ok_or("usage: dev_account <dir>")?);
    // SAFETY: single-threaded at this point.
    unsafe { std::env::set_var("ENVELOPE_SECRETS", "file") };
    let engine = Engine::start(
        Paths {
            data: dir.join("data"),
            cache: dir.join("cache"),
        },
        Quiet,
    )?;
    if engine
        .accounts()?
        .iter()
        .any(|a| a.email == "alex@envelope.test")
    {
        println!("account already present");
        return Ok(());
    }
    let account = engine
        .add_password_account(NewAccount {
            email: "alex@envelope.test".into(),
            name: "Alex Morgan".into(),
            provider: "generic".into(),
            password: "password".into(),
            config: AccountConfig {
                imap: Some(ServerConfig {
                    host: "127.0.0.1".into(),
                    port: 1993,
                    security: Security::Tls,
                    username: String::new(),
                }),
                smtp: Some(ServerConfig {
                    host: "127.0.0.1".into(),
                    port: 1587,
                    security: Security::None,
                    username: String::new(),
                }),
                caldav_url: Some("http://127.0.0.1:5232/".into()),
                accept_invalid_certs: true,
                ..Default::default()
            },
        })
        .await?;
    println!("added account {} ({})", account.id, account.email);
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    engine.shutdown();
    Ok(())
}
