//! Measures initial sync speed and query latency against the development
//! server's large mailbox (see scripts/devserver/seed_large.py):
//!
//! python3 scripts/devserver/seed_large.py .devserver/home/sam@envelope.test/Maildir 50000
//! cargo run --release -p envelope-core --example bench_sync -- 50000

use std::time::{Duration, Instant};

use envelope_core::model::*;
use envelope_core::{Engine, EventSink, Paths};

struct Quiet;
impl EventSink for Quiet {
    fn emit(&self, _: EngineEvent) {}
}

fn inbox(offset: i64, limit: i64, search: Option<&str>) -> ThreadQuery {
    ThreadQuery {
        mailbox: Mailbox::Unified {
            role: FolderRole::Inbox,
        },
        search: search.map(str::to_owned),
        filter: ListFilter::default(),
        offset,
        limit,
    }
}

fn time<T>(label: &str, runs: u32, mut f: impl FnMut() -> T) -> T {
    let mut out = f();
    let start = Instant::now();
    for _ in 0..runs {
        out = f();
    }
    let each = start.elapsed() / runs;
    println!("  {label:<44} {:>8.2} ms", each.as_secs_f64() * 1000.0);
    out
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // SAFETY: single-threaded at this point.
    unsafe { std::env::set_var("ENVELOPE_SECRETS", "file") };
    let dir = std::env::temp_dir().join(format!("envelope-bench-{}", std::process::id()));
    let engine = Engine::start(
        Paths {
            data: dir.join("data"),
            cache: dir.join("cache"),
        },
        Quiet,
    )?;
    let expected: i64 = std::env::args()
        .nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or(50_000);
    let started = Instant::now();
    engine
        .add_password_account(NewAccount {
            email: "sam@envelope.test".into(),
            name: "Sam Rivera".into(),
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
        .await?;

    println!("initial sync of sam@envelope.test");
    let mut first_page = None;
    let mut last = -1;
    let db = rusqlite::Connection::open_with_flags(
        dir.join("data/envelope.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let mut mark = 0;
    let mut stable_since = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let page = engine.threads(&inbox(0, 50, None))?;
        let synced: i64 = db
            .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
            .unwrap_or(0);
        if first_page.is_none() && page.items.len() >= 50 {
            first_page = Some(started.elapsed());
            println!(
                "  first screen of threads after {:>8.2} s",
                started.elapsed().as_secs_f64()
            );
        }
        if synced != last {
            last = synced;
            stable_since = Instant::now();
            if synced >= mark {
                mark += 10_000;
                println!(
                    "  {synced:>6} / {expected} messages after {:>6.2} s",
                    started.elapsed().as_secs_f64()
                );
            }
        }
        let idle = matches!(
            engine.accounts()?.first().map(|a| a.sync.clone()),
            Some(SyncState::Idle)
        );
        if (synced >= expected && idle) || stable_since.elapsed() > Duration::from_secs(30) {
            println!(
                "  all {synced} messages ({} threads) after {:>6.2} s",
                page.total,
                started.elapsed().as_secs_f64()
            );
            break;
        }
    }

    println!(
        "queries ({} threads in the inbox)",
        engine.threads(&inbox(0, 1, None))?.total
    );
    time("inbox, first 100 threads", 50, || {
        engine.threads(&inbox(0, 100, None)).unwrap()
    });
    time("inbox, 100 threads at offset 10 000", 50, || {
        engine.threads(&inbox(10_000, 100, None)).unwrap()
    });
    let unread = ThreadQuery {
        filter: ListFilter {
            unread: true,
            ..Default::default()
        },
        ..inbox(0, 100, None)
    };
    time("inbox, unread only", 50, || {
        engine.threads(&unread).unwrap()
    });
    let hits = time("search \"invoice\"", 20, || {
        engine.threads(&inbox(0, 100, Some("invoice"))).unwrap()
    });
    println!("    ({} matching threads)", hits.total);
    time("search \"from:priya roadmap\"", 20, || {
        engine
            .threads(&inbox(0, 100, Some("from:priya roadmap")))
            .unwrap()
    });
    time("search \"deploy server friday\"", 20, || {
        engine
            .threads(&inbox(0, 100, Some("deploy server friday")))
            .unwrap()
    });
    let big = engine
        .threads(&inbox(0, 500, None))?
        .items
        .into_iter()
        .max_by_key(|t| t.message_count)
        .map(|t| t.id);
    if let Some(id) = big {
        time("open a conversation (headers)", 50, || {
            engine.thread(id).unwrap()
        });
    }
    engine.shutdown();
    if std::env::var_os("ENVELOPE_BENCH_KEEP").is_some() {
        println!("kept {}", dir.display());
    } else {
        let _ = std::fs::remove_dir_all(&dir);
    }
    Ok(())
}
