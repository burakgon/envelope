use std::path::Path;

use super::*;

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn imap(host: &str, port: u16, security: Security, username: &str) -> Option<ServerConfig> {
    Some(ServerConfig {
        host: host.into(),
        port,
        security,
        username: username.into(),
    })
}

#[test]
fn finds_accounts_of_other_mail_programs() {
    let home =
        std::env::temp_dir().join(format!("envelope-import-{}", uuid::Uuid::new_v4().simple()));
    let config = home.join(".config");

    // Thunderbird: the installation's default profile first; POP, local
    // folders and the same address in a Flatpak profile are left out.
    write(
        &home,
        ".thunderbird/profiles.ini",
        "[Install4F96D1932A9F858E]\nDefault=main.default-release\nLocked=1\n\n[Profile1]\nName=old\nIsRelative=1\nPath=old.default\n\n[Profile0]\nName=default-release\nIsRelative=1\nPath=main.default-release\nDefault=1\n",
    );
    write(
        &home,
        ".thunderbird/main.default-release/prefs.js",
        r#"// Mozilla User Preferences
user_pref("mail.accountmanager.accounts", "account1,account2,account3,account4");
user_pref("mail.account.account1.identities", "id1");
user_pref("mail.account.account1.server", "server1");
user_pref("mail.account.account2.identities", "id2");
user_pref("mail.account.account2.server", "server2");
user_pref("mail.account.account3.server", "server3");
user_pref("mail.account.account4.identities", "id4");
user_pref("mail.account.account4.server", "server4");
user_pref("mail.identity.id1.fullName", "Alex \"Al\" Doe");
user_pref("mail.identity.id1.useremail", "alex@example.com");
user_pref("mail.identity.id1.smtpServer", "smtp1");
user_pref("mail.identity.id2.useremail", "alex.doe@gmail.com");
user_pref("mail.identity.id4.useremail", "old@pop.example");
user_pref("mail.server.server1.hostname", "mail.example.com");
user_pref("mail.server.server1.port", 993);
user_pref("mail.server.server1.socketType", 3);
user_pref("mail.server.server1.type", "imap");
user_pref("mail.server.server1.userName", "alex");
user_pref("mail.server.server2.authMethod", 10);
user_pref("mail.server.server2.hostname", "imap.gmail.com");
user_pref("mail.server.server2.socketType", 3);
user_pref("mail.server.server2.type", "imap");
user_pref("mail.server.server3.type", "none");
user_pref("mail.server.server4.hostname", "pop.example");
user_pref("mail.server.server4.type", "pop3");
user_pref("mail.smtpserver.smtp1.hostname", "smtp.example.com");
user_pref("mail.smtpserver.smtp1.try_ssl", 2);
user_pref("mail.smtpserver.smtp1.username", "alex");
"#,
    );
    write(
        &home,
        ".thunderbird/old.default/prefs.js",
        r#"user_pref("mail.accountmanager.accounts", "account1");
user_pref("mail.account.account1.identities", "id1");
user_pref("mail.account.account1.server", "server1");
user_pref("mail.identity.id1.useremail", "ALEX@example.com");
user_pref("mail.server.server1.hostname", "imap.other.example");
user_pref("mail.server.server1.type", "imap");
"#,
    );

    // Evolution: an IMAP account with its identity and SMTP transport; a
    // disabled one is left out.
    write(
        &config,
        "evolution/sources/1a.source",
        "[Data Source]\nDisplayName=work@evolution.example\nEnabled=true\n\n[Mail Account]\nBackendName=imapx\nIdentityUid=1b\n\n[Authentication]\nHost=imap.evolution.example\nPort=143\nUser=work\n\n[Security]\nMethod=starttls-on-standard-port\n",
    );
    write(
        &config,
        "evolution/sources/1b.source",
        "[Data Source]\nDisplayName=work@evolution.example\n\n[Mail Identity]\nAddress=work@evolution.example\nName=Work\\sMe\n\n[Mail Submission]\nTransportUid=1c\n",
    );
    write(
        &config,
        "evolution/sources/1c.source",
        "[Data Source]\nDisplayName=work@evolution.example\n\n[Mail Transport]\nBackendName=smtp\n\n[Authentication]\nHost=smtp.evolution.example\nPort=465\nUser=work\n\n[Security]\nMethod=ssl-on-alternate-port\n",
    );
    write(
        &config,
        "evolution/sources/2a.source",
        "[Data Source]\nEnabled=false\n\n[Mail Account]\nBackendName=imapx\n\n[Authentication]\nHost=imap.disabled.example\n",
    );

    // Geary: an account with its own servers, and one with Outlook.
    write(
        &config,
        "geary/account_01/geary.ini",
        "[Metadata]\nversion=1\n\n[Account]\nsender_mailboxes=Geary User <geary@mail.example>;\nservice_provider=other\n\n[Incoming]\nhost=imap.mail.example\nport=993\ntransport_security=transport\nlogin=geary\n\n[Outgoing]\nhost=smtp.mail.example\nport=587\ntransport_security=start-tls\ncredentials=use-incoming\n",
    );
    write(
        &config,
        "geary/account_02/geary.ini",
        "[Account]\nsender_mailboxes=hotmail.user@outlook.com;\nservice_provider=outlook\n",
    );

    // GNOME Online Accounts: Google, IMAP with a port in the host, and an
    // account with mail turned off.
    write(
        &config,
        "goa-1.0/accounts.conf",
        "[Account account_1_0]\nProvider=google\nIdentity=gnome.user@gmail.com\nPresentationIdentity=gnome.user@gmail.com\nMailEnabled=true\n\n[Account account_2_0]\nProvider=imap_smtp\nIdentity=gnome@imap.example\nEmailAddress=gnome@imap.example\nName=Gnome User\nImapHost=imap.imap.example:1993\nImapUserName=gnome\nImapUseSsl=true\nSmtpHost=smtp.imap.example\nSmtpUserName=gnome\nSmtpUseTls=true\n\n[Account account_3_0]\nProvider=google\nIdentity=calendar.only@gmail.com\nMailEnabled=false\n",
    );

    let found = find(&home, &config);
    let _ = std::fs::remove_dir_all(&home);
    let brief: Vec<(&str, &str, FoundKind)> = found
        .iter()
        .map(|a| (a.source.as_str(), a.email.as_str(), a.kind))
        .collect();
    assert_eq!(
        brief,
        [
            ("Thunderbird", "alex@example.com", FoundKind::Imap),
            ("Thunderbird", "alex.doe@gmail.com", FoundKind::Google),
            ("Evolution", "work@evolution.example", FoundKind::Imap),
            ("Geary", "geary@mail.example", FoundKind::Imap),
            ("Geary", "hotmail.user@outlook.com", FoundKind::Microsoft),
            (
                "GNOME Online Accounts",
                "gnome.user@gmail.com",
                FoundKind::Google
            ),
            (
                "GNOME Online Accounts",
                "gnome@imap.example",
                FoundKind::Imap
            ),
        ]
    );

    let alex = &found[0];
    assert_eq!(alex.name, "Alex \"Al\" Doe");
    assert_eq!(
        alex.imap,
        imap("mail.example.com", 993, Security::Tls, "alex")
    );
    // No port: the one STARTTLS submission uses.
    assert_eq!(
        alex.smtp,
        imap("smtp.example.com", 587, Security::StartTls, "alex")
    );
    // Google signs in with Google: no servers or password.
    assert_eq!(found[1].imap, None);

    let work = &found[2];
    assert_eq!(work.name, "Work Me");
    assert_eq!(
        work.imap,
        imap("imap.evolution.example", 143, Security::StartTls, "work")
    );
    assert_eq!(
        work.smtp,
        imap("smtp.evolution.example", 465, Security::Tls, "work")
    );

    let geary = &found[3];
    assert_eq!(geary.name, "Geary User");
    assert_eq!(
        geary.imap,
        imap("imap.mail.example", 993, Security::Tls, "geary")
    );
    // The outgoing server signs in like the incoming one.
    assert_eq!(
        geary.smtp,
        imap("smtp.mail.example", 587, Security::StartTls, "geary")
    );

    let gnome = &found[6];
    assert_eq!(
        gnome.imap,
        imap("imap.imap.example", 1993, Security::Tls, "gnome")
    );
    assert_eq!(
        gnome.smtp,
        imap("smtp.imap.example", 587, Security::StartTls, "gnome")
    );
}

#[test]
fn exchange_accounts_keep_their_server_address() {
    let home =
        std::env::temp_dir().join(format!("envelope-import-{}", uuid::Uuid::new_v4().simple()));
    let config = home.join(".config");
    write(
        &home,
        ".thunderbird/profiles.ini",
        "[Profile0]\nName=default\nIsRelative=1\nPath=p.default\nDefault=1\n",
    );
    write(
        &home,
        ".thunderbird/p.default/prefs.js",
        r#"user_pref("mail.accountmanager.accounts", "account1");
user_pref("mail.account.account1.identities", "id1");
user_pref("mail.account.account1.server", "server1");
user_pref("mail.identity.id1.useremail", "jane@corp.example");
user_pref("mail.server.server1.type", "ews");
user_pref("mail.server.server1.ews_url", "https://mail.corp.example/EWS/Exchange.asmx");
"#,
    );
    write(
        &config,
        "evolution/sources/e.source",
        "[Data Source]\nDisplayName=Work\n\n[Mail Account]\nBackendName=ews\n\n[Ews Backend]\nEmail=john@corp.example\nHosturl=https://owa.corp.example/EWS/Exchange.asmx\n",
    );
    let found = find(&home, &config);
    let _ = std::fs::remove_dir_all(&home);
    assert_eq!(found.len(), 2);
    assert!(found.iter().all(|a| a.kind == FoundKind::Exchange));
    assert_eq!(
        found[0].ews_url.as_deref(),
        Some("https://mail.corp.example/EWS/Exchange.asmx")
    );
    assert_eq!(found[1].email, "john@corp.example");
    assert_eq!(
        found[1].ews_url.as_deref(),
        Some("https://owa.corp.example/EWS/Exchange.asmx")
    );
}

#[test]
fn nothing_is_found_without_other_programs() {
    let home =
        std::env::temp_dir().join(format!("envelope-import-{}", uuid::Uuid::new_v4().simple()));
    assert_eq!(find(&home, &home.join(".config")), []);
}
