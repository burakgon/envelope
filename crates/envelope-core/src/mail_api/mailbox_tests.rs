//! Engine actions on mail against a real temporary database. The fixture is
//! shared with the folder tests.

use std::sync::Arc;

use parking_lot::Mutex;

use super::*;
use crate::engine::{EventSink, Paths};

#[derive(Clone, Default)]
pub(crate) struct Events(Arc<Mutex<Vec<EngineEvent>>>);

impl EventSink for Events {
    fn emit(&self, event: EngineEvent) {
        self.0.lock().push(event);
    }
}

#[tokio::test]
async fn sync_failures_are_counted_until_a_sync_works() {
    let dir = std::env::temp_dir().join(format!("envelope-sync-{}", uuid::Uuid::new_v4().simple()));
    let events = Events::default();
    let engine = Engine::start(
        Paths {
            data: dir.join("data"),
            cache: dir.join("cache"),
        },
        events.clone(),
    )
    .expect("engine");
    // The count needs no account row.
    let id = 7;
    let failures = |s: SyncState| match s {
        SyncState::Offline { failures, .. } | SyncState::Error { failures, .. } => failures,
        _ => 0,
    };
    engine.set_sync_state(id, SyncState::Connecting);
    engine.set_sync_state(id, SyncState::failed("connection refused".into(), true));
    engine.set_sync_state(id, SyncState::Connecting);
    engine.set_sync_state(id, SyncState::failed("connection refused".into(), true));
    assert_eq!(failures(engine.sync_state(id)), 2);
    assert!(
        matches!(engine.sync_state(id), SyncState::Offline { ref message, .. } if message == "connection refused")
    );
    engine.set_sync_state(id, SyncState::Idle);
    engine.set_sync_state(id, SyncState::failed("bad certificate".into(), false));
    assert_eq!(
        failures(engine.sync_state(id)),
        1,
        "a sync that worked starts over"
    );
    // The interface hears about every failed attempt.
    let reported = events
        .0
        .lock()
        .iter()
        .filter(|e| {
            matches!(
                e,
                EngineEvent::SyncStatus {
                    sync: SyncState::Offline { .. } | SyncState::Error { .. },
                    ..
                }
            )
        })
        .count();
    assert_eq!(reported, 3);
    let json = serde_json::to_value(engine.sync_state(id)).expect("json");
    assert_eq!(
        json,
        serde_json::json!({ "state": "error", "message": "bad certificate", "failures": 1 })
    );
    engine.shutdown();
    let _ = std::fs::remove_dir_all(dir);
}
