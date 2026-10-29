use super::*;

async fn read_all(text: &[u8]) -> Vec<String> {
    let path =
        std::env::temp_dir().join(format!("envelope-mbox-{}", uuid::Uuid::new_v4().simple()));
    std::fs::write(&path, text).unwrap();
    let mut messages = Messages::open(&path).await.unwrap();
    let mut out = Vec::new();
    while let Some(m) = messages.next().await.unwrap() {
        out.push(String::from_utf8_lossy(&m).into_owned());
    }
    let _ = std::fs::remove_file(&path);
    out
}

#[tokio::test]
async fn splits_mbox_files_and_unescapes_from_lines() {
    let mbox = b"From - Tue Oct 29 10:00:00 2026\n\
Message-ID: <one@example.com>\n\
Subject: First\n\
\n\
Hello.\n\
>From the archives: a line that started with From.\n\
>>From stays quoted once.\n\
\n\
From MAILER-DAEMON Wed Oct 30 09:00:00 2026\n\
Subject: Second\n\
Content-Type: text/plain; charset=iso-8859-1\n\
\n\
Gr\xfc\xdfe aus K\xf6ln.\n\
From a line in a body (no blank line before it), not a new message.\n\
\n";
    let messages = read_all(mbox).await;
    assert_eq!(messages.len(), 2, "{messages:#?}");
    assert_eq!(
        messages[0],
        "Message-ID: <one@example.com>\r\nSubject: First\r\n\r\nHello.\r\nFrom the archives: a line that started with From.\r\n>From stays quoted once.\r\n"
    );
    // 8-bit text survives, and a "From " line inside a body stays in it.
    assert!(messages[1].starts_with("Subject: Second\r\n"));
    assert!(messages[1].contains("Gr\u{fffd}\u{fffd}e aus K\u{fffd}ln."));
    assert!(
        messages[1]
            .ends_with("From a line in a body (no blank line before it), not a new message.\r\n")
    );
}

#[tokio::test]
async fn a_saved_message_is_one_message() {
    let eml = b"From: Ada <ada@example.com>\nSubject: Saved\n\nFrom here on, text.\n";
    let messages = read_all(eml).await;
    assert_eq!(
        messages,
        ["From: Ada <ada@example.com>\r\nSubject: Saved\r\n\r\nFrom here on, text.\r\n"]
    );
}

#[test]
fn reads_the_message_id_and_date() {
    let (id, date) = identity(
        b"Message-ID: <abc@example.com>\r\nDate: Tue, 29 Oct 2026 10:00:00 +0100\r\n\r\nx",
    );
    assert_eq!(id.as_deref(), Some("abc@example.com"));
    assert_eq!(
        date.map(|d| d.to_rfc3339()).as_deref(),
        Some("2026-10-29T10:00:00+01:00")
    );
}
