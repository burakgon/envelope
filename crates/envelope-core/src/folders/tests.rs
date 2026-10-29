//! Folder management against a real temporary database (the fixture is the
//! mail tests'), with the server side in the Graph, Exchange and live IMAP
//! tests.

use super::*;
use crate::mail_api::mailbox_tests::{fixture, folder};

#[test]
fn names_and_places_of_new_folders() {
    assert_eq!(
        clean_name("  Clients ", Some("/")).ok().as_deref(),
        Some("Clients")
    );
    assert!(clean_name("   ", Some("/")).is_err());
    assert!(
        clean_name("a/b", Some("/")).is_err(),
        "the delimiter makes a path"
    );
    assert!(clean_name("v1.2", Some("/")).is_ok());
    assert!(clean_name("v1.2", Some(".")).is_err());

    let rows = |paths: &[&str]| -> Vec<FolderRow> {
        paths
            .iter()
            .enumerate()
            .map(|(i, p)| FolderRow {
                id: i as i64 + 1,
                account_id: 1,
                remote_id: p.to_string(),
                name: p.to_string(),
                path: p.to_string(),
                parent_id: None,
                role: None,
                kind: "folder".into(),
                delimiter: Some(".".into()),
                selectable: true,
                color: None,
                sync_state: Default::default(),
            })
            .collect()
    };
    // Courier-style servers keep every folder below INBOX: new ones go there too.
    let courier = rows(&["INBOX", "INBOX.Sent", "INBOX.Trash", "INBOX.Archive"]);
    assert_eq!(imap_top_prefix(&courier, ".", false), "INBOX.");
    assert_eq!(imap_parent(&courier, "INBOX.Clients", "."), Some(1));
    let dovecot = rows(&["INBOX", "Sent", "Trash"]);
    assert_eq!(imap_top_prefix(&dovecot, "/", false), "");
    assert_eq!(imap_parent(&dovecot, "Clients", "/"), None);
    assert_eq!(imap_top_prefix(&courier, ".", true), "", "never in Gmail");

    let mut taken = rows(&["Work", "Clients"]);
    taken[1].parent_id = Some(1);
    assert!(check_free(&taken, Some(1), "folder", "clients", None).is_err());
    assert!(check_free(&taken, None, "folder", "clients", None).is_ok());
    assert!(check_free(&taken, Some(1), "label", "clients", None).is_ok());
    assert!(
        check_free(&taken, Some(1), "folder", "Clients", Some(2)).is_ok(),
        "itself"
    );
}

#[test]
fn outlook_colors_round_trip() {
    for color in CategoryColor::ALL.iter().skip(1) {
        let hex = color.hex().unwrap_or_default();
        assert_eq!(nearest_preset(Some(hex)), *color, "{hex}");
    }
    assert_eq!(nearest_preset(Some("#e64a55")), CategoryColor::Preset0);
    assert_eq!(nearest_preset(None), CategoryColor::NoColor);
    assert_eq!(nearest_preset(Some("pink")), CategoryColor::NoColor);
    assert_eq!(preset_number(CategoryColor::NoColor), -1);
    assert_eq!(preset_number(CategoryColor::Preset0), 0);
    assert_eq!(preset_number(CategoryColor::Preset7), 7);
}

#[tokio::test]
async fn a_renamed_imap_folder_takes_its_subfolders_and_queued_changes_along() {
    let fx = fixture();
    let (_, thread) = fx.message("INBOX", 1, "a@x", None, "ann@example.com", "Hi", 1_000, 0);
    let account = fx.account;
    let work = fx.row("Work").expect("Work");
    let clients = fx.row("Work/Clients").expect("Clients");
    // A move into the subfolder still waiting in the undo window.
    fx.engine
        .move_threads(
            &[thread],
            Some(fx.id("INBOX")),
            &MoveTarget::Folder { id: clients.id },
        )
        .expect("move");
    // A label change as Gmail queues them, by path.
    fx.engine
        .db
        .write(|tx| {
            misc::queue_op(
                tx,
                account,
                &PendingOp::Labels {
                    message_ids: vec![1],
                    add: vec!["Work/Clients".into()],
                    remove: vec!["Workshop".into()],
                },
                0,
            )
        })
        .expect("queue");
    fx.engine
        .db
        .write(|tx| {
            mail::rehome_folder(tx, work.id, "Projects", "Projects", "Projects", None, true)?;
            misc::rename_folder_in_ops(tx, account, "Work", "Projects", "/")
        })
        .expect("rename");
    assert!(fx.row("Work").is_none());
    let moved = fx.row("Projects/Clients").expect("subfolder follows");
    assert_eq!(moved.id, clients.id, "same folder, same messages");
    assert_eq!(moved.parent_id, Some(work.id));
    assert_eq!(fx.row("Projects").map(|f| f.name), Some("Projects".into()));
    let dest = fx.ops().into_iter().find_map(|op| match op {
        PendingOp::Move { dest_folder, .. } => Some(dest_folder),
        _ => None,
    });
    assert_eq!(dest.as_deref(), Some("Projects/Clients"));
    let labels = fx.ops().into_iter().find_map(|op| match op {
        PendingOp::Labels { add, remove, .. } => Some((add, remove)),
        _ => None,
    });
    assert_eq!(
        labels,
        Some((vec!["Projects/Clients".into()], vec!["Workshop".into()])),
        "a name that only starts the same stays"
    );
}

#[tokio::test]
async fn deleting_a_folder_removes_its_subfolders_and_their_mail() {
    let fx = fixture();
    let (_, thread) = fx.message(
        "Work/Clients",
        1,
        "a@x",
        None,
        "ann@example.com",
        "Plan",
        1_000,
        0,
    );
    fx.message(
        "INBOX",
        2,
        "b@x",
        None,
        "bob@example.com",
        "Lunch",
        2_000,
        0,
    );
    let work = fx.id("Work");
    fx.engine
        .db
        .write(|tx| mail::delete_folder_tree(tx, work))
        .expect("delete");
    assert!(fx.row("Work").is_none());
    assert!(fx.row("Work/Clients").is_none());
    assert!(
        fx.engine.thread(thread).is_err(),
        "its only copy went with it"
    );
    assert_eq!(fx.list(Mailbox::Folder { id: fx.id("INBOX") }).len(), 1);
}

#[tokio::test]
async fn emptying_trash_deletes_everything_in_it_and_below_it() {
    let fx = fixture();
    let (_, a) = fx.message("Trash", 7, "a@x", None, "ann@example.com", "Old", 1_000, 1);
    let (_, b) = fx.message(
        "Trash/Old",
        3,
        "b@x",
        None,
        "bob@example.com",
        "Older",
        500,
        1,
    );
    let (_, keep) = fx.message("INBOX", 9, "c@x", None, "cat@example.com", "Keep", 2_000, 0);
    let trash = fx.id("Trash");
    // The server had more than this computer downloaded.
    let mut state = fx.row("Trash").expect("trash").sync_state;
    state.uid_next = Some(42);
    fx.engine
        .db
        .write(|tx| mail::set_folder_sync_state(tx, trash, &state))
        .expect("state");
    assert!(
        fx.engine.empty_folder(fx.id("INBOX")).is_err(),
        "only Trash and Spam"
    );

    assert_eq!(fx.engine.empty_folder(trash).expect("empty"), 2);
    assert!(fx.list(Mailbox::Folder { id: trash }).is_empty());
    assert!(fx.engine.thread(a).is_err() && fx.engine.thread(b).is_err());
    assert!(fx.row("Trash/Old").is_none(), "folders inside go too");
    assert!(fx.engine.thread(keep).is_ok());
    let row = fx.row("Trash").expect("Trash stays");
    let view = fx.engine.folder_view(row.id).expect("view");
    assert_eq!((view.total_count, view.unread_count), (0, 0));
    assert_eq!(
        fx.ops(),
        vec![PendingOp::EmptyFolder {
            folder: "Trash".into(),
            max_uid: Some(41),
            subfolders: vec!["Trash/Old".into()],
        }]
    );
}

#[tokio::test]
async fn label_colors_stay_on_this_computer_for_imap() {
    let fx = fixture();
    let work = fx.id("Work");
    fx.engine
        .set_folder_color(work, Some("#47b347".into()))
        .await
        .expect("color");
    assert_eq!(
        fx.row("Work").and_then(|f| f.color).as_deref(),
        Some("#47b347")
    );
    // The next listing (which has no colors) keeps it.
    let account = fx.account;
    fx.engine
        .db
        .write(|tx| mail::upsert_folder(tx, account, &folder("Work", None, None), None).map(drop))
        .expect("listing");
    assert_eq!(
        fx.row("Work").and_then(|f| f.color).as_deref(),
        Some("#47b347")
    );
    fx.engine.set_folder_color(work, None).await.expect("clear");
    assert_eq!(fx.row("Work").and_then(|f| f.color), None);
}

#[tokio::test]
async fn special_folders_are_protected() {
    let fx = fixture();
    for path in ["INBOX", "Sent", "Trash", "Junk", "Archive"] {
        let id = fx.id(path);
        assert!(
            fx.engine.rename_folder(id, "Other").await.is_err(),
            "{path}"
        );
        assert!(fx.engine.delete_folder(id).await.is_err(), "{path}");
        assert!(
            fx.engine
                .move_folder(id, Some(fx.id("Work")))
                .await
                .is_err(),
            "{path}"
        );
    }
    // Nothing reached a server (there is none): the refusals come first.
    assert!(fx.row("INBOX").is_some());
    let work = fx.id("Work");
    let clients = fx.id("Work/Clients");
    assert!(
        fx.engine.move_folder(work, Some(clients)).await.is_err(),
        "not inside itself"
    );
    assert!(
        fx.engine
            .create_folder(fx.account, None, "Tags", true, None)
            .await
            .is_err(),
        "IMAP has no labels"
    );
}
