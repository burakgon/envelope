//! Automatic replies through ManageSieve against the development server
//! (Dovecot with Pigeonhole): the reply goes into the user's active script
//! next to their own filters, reads back, turns off and on again.
//!
//! Run: ENVELOPE_TEST_IMAP=1 cargo test -p envelope-core --test live_sieve -- --nocapture

use chrono::NaiveDate;
use envelope_core::auto_reply::{Audience, AutoReply};
use envelope_core::model::*;
use envelope_core::sieve;
use envelope_core::{Engine, EventSink, Paths};

struct Quiet;
impl EventSink for Quiet {
    fn emit(&self, _: EngineEvent) {}
}

const FILTERS: &str = "require [\"fileinto\"];\nif header :contains \"subject\" \"[list]\" { fileinto \"Archive\"; }\n";

async fn server() -> sieve::Client {
    let mut client = sieve::Client::connect("127.0.0.1", 14190, true, true)
        .await
        .expect("ManageSieve on 127.0.0.1:14190 (scripts/devserver/run.sh, needs Pigeonhole)");
    client
        .login("alex@envelope.test", "password")
        .await
        .unwrap();
    client
}

async fn active_script() -> String {
    let mut client = server().await;
    let (_, active) = client.scripts().await.unwrap();
    let script = client
        .get(&active.expect("an active script"))
        .await
        .unwrap();
    client.logout().await;
    script
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_replies_through_managesieve() {
    if std::env::var("ENVELOPE_TEST_IMAP").is_err() {
        return;
    }
    unsafe { std::env::set_var("ENVELOPE_SECRETS", "file") };

    // The user already filters mail with a script of their own.
    let mut client = server().await;
    client.put("filters", FILTERS).await.unwrap();
    client.activate("filters").await.unwrap();
    client.logout().await;

    let dir = std::env::temp_dir().join(format!("envelope-sieve-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let engine = Engine::start(
        Paths {
            data: dir.join("d"),
            cache: dir.join("c"),
        },
        Quiet,
    )
    .unwrap();
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
                accept_invalid_certs: true,
                ..Default::default()
            },
        })
        .await
        .unwrap();

    let info = engine.auto_reply(account.id).await.unwrap();
    assert!(!info.reply.enabled);
    assert!(info.subject);
    assert_eq!(info.audiences, [Audience::Everyone]);

    let away = AutoReply {
        enabled: true,
        start: NaiveDate::from_ymd_opt(2026, 11, 2),
        end: NaiveDate::from_ymd_opt(2026, 11, 6),
        subject: "Out of office".into(),
        message: "I'm away until Monday, November 9.\nFor anything urgent, call Sam.".into(),
        audience: Audience::Everyone,
    };
    engine
        .set_auto_reply(account.id, away.clone())
        .await
        .unwrap();
    let script = active_script().await;
    println!("{script}");
    assert!(script.starts_with("require [\"fileinto\"];\n# BEGIN Envelope automatic reply"));
    assert!(script.contains(
        "vacation :days 4 :subject \"Out of office\" :addresses [\"alex@envelope.test\"]"
    ));
    assert!(
        script.ends_with("if header :contains \"subject\" \"[list]\" { fileinto \"Archive\"; }\n")
    );
    assert_eq!(engine.auto_reply(account.id).await.unwrap().reply, away);

    // Off keeps the text for next time; the user's filters stay as they were.
    let off = AutoReply {
        enabled: false,
        ..away.clone()
    };
    engine
        .set_auto_reply(account.id, off.clone())
        .await
        .unwrap();
    let script = active_script().await;
    assert!(!script.contains("vacation :days"), "{script}");
    assert!(script.contains("fileinto \"Archive\""));
    assert_eq!(engine.auto_reply(account.id).await.unwrap().reply, off);

    // Without dates the reply stays on until turned off.
    let always = AutoReply {
        start: None,
        end: None,
        ..away
    };
    engine
        .set_auto_reply(account.id, always.clone())
        .await
        .unwrap();
    assert!(
        active_script()
            .await
            .contains("require [\"vacation\"];\nvacation :days 4")
    );
    assert_eq!(engine.auto_reply(account.id).await.unwrap().reply, always);

    // An impossible period is refused before anything is written.
    let backwards = AutoReply {
        start: NaiveDate::from_ymd_opt(2026, 11, 6),
        end: NaiveDate::from_ymd_opt(2026, 11, 2),
        ..always
    };
    assert!(engine.set_auto_reply(account.id, backwards).await.is_err());

    let mut client = server().await;
    client.put("filters", FILTERS).await.unwrap();
    client.logout().await;
    engine.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
