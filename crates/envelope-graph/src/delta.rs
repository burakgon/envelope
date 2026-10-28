//! Delta-query plumbing shared by mail, calendar and contacts sync.
//!
//! A delta *round* starts with an initial request (or a stored
//! `@odata.deltaLink`) and continues through `@odata.nextLink` pages until a
//! page carries a new `@odata.deltaLink`. Every page can be persisted on its
//! own: store the items, apply the removals, then store the returned
//! [`DeltaCursor`]. If the process stops mid-round, resuming from the stored
//! `Next` cursor continues where it left off.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client::{Collection, GraphClient, GraphRequest};
use crate::error::GraphError;

/// Where a delta round stands after a page.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "state", content = "url", rename_all = "camelCase")]
pub enum DeltaCursor {
    /// More changes are pending in this round: fetch this
    /// `@odata.nextLink` next.
    Next(String),
    /// The round is complete. Persist this `@odata.deltaLink`; fetching it
    /// later returns everything that changed since.
    Done(String),
}

impl DeltaCursor {
    /// The URL to request next.
    pub fn url(&self) -> &str {
        match self {
            DeltaCursor::Next(url) | DeltaCursor::Done(url) => url,
        }
    }

    /// True when the round is complete (`@odata.deltaLink`).
    pub fn is_done(&self) -> bool {
        matches!(self, DeltaCursor::Done(_))
    }
}

/// One page of a delta query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeltaPage<T> {
    /// Created or updated items.
    pub items: Vec<T>,
    /// Ids reported under `@removed`: deleted, moved out of the synced
    /// collection, or (for calendar views) changed outside the window.
    pub removed_ids: Vec<String>,
    /// How to continue.
    pub next: DeltaCursor,
}

impl<T> DeltaPage<T> {
    /// Converts the items, keeping removals and cursor.
    pub fn map<U>(self, f: impl FnMut(T) -> U) -> DeltaPage<U> {
        DeltaPage {
            items: self.items.into_iter().map(f).collect(),
            removed_ids: self.removed_ids,
            next: self.next,
        }
    }

    pub(crate) fn try_map<U, E>(self, f: impl FnMut(T) -> Result<U, E>) -> Result<DeltaPage<U>, E> {
        Ok(DeltaPage {
            items: self.items.into_iter().map(f).collect::<Result<_, _>>()?,
            removed_ids: self.removed_ids,
            next: self.next,
        })
    }
}

impl GraphClient {
    /// Fetches one delta page and separates `@removed` entries from items.
    pub(crate) async fn delta_page_raw<T: DeserializeOwned>(
        &self,
        request: &GraphRequest,
    ) -> Result<DeltaPage<T>, GraphError> {
        let collection: Collection<Value> = self.send_json(request).await?;
        split_delta(collection)
    }
}

pub(crate) fn split_delta<T: DeserializeOwned>(
    collection: Collection<Value>,
) -> Result<DeltaPage<T>, GraphError> {
    let next = match (collection.next_link, collection.delta_link) {
        (Some(next), _) => DeltaCursor::Next(next),
        (None, Some(delta)) => DeltaCursor::Done(delta),
        (None, None) => {
            return Err(GraphError::Decode(
                "delta response has neither @odata.nextLink nor @odata.deltaLink".into(),
            ));
        }
    };
    let mut items = Vec::new();
    let mut removed_ids = Vec::new();
    for value in collection.value {
        if value.get("@removed").is_some() {
            match value.get("id").and_then(Value::as_str) {
                Some(id) => removed_ids.push(id.to_owned()),
                None => tracing::warn!("ignoring @removed entry without id"),
            }
            continue;
        }
        let item =
            serde_json::from_value(value).map_err(|e| GraphError::decode("delta item", e))?;
        items.push(item);
    }
    Ok(DeltaPage {
        items,
        removed_ids,
        next,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Item {
        id: String,
    }

    fn collection(json: &str) -> Collection<Value> {
        serde_json::from_str(json).unwrap_or_else(|e| panic!("fixture: {e}"))
    }

    #[test]
    fn splits_removed_entries() {
        let page: DeltaPage<Item> = split_delta(collection(
            r##"{
              "@odata.deltaLink": "https://graph.microsoft.com/v1.0/me/x/delta?$deltatoken=abc",
              "value": [
                {"id": "a"},
                {"@odata.type": "#microsoft.graph.message", "id": "b", "@removed": {"reason": "deleted"}}
              ]
            }"##,
        ))
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(page.items, vec![Item { id: "a".into() }]);
        assert_eq!(page.removed_ids, vec!["b".to_owned()]);
        assert!(page.next.is_done());
    }

    #[test]
    fn missing_links_is_an_error() {
        let result: Result<DeltaPage<Item>, _> = split_delta(collection(r#"{"value": []}"#));
        assert!(matches!(result, Err(GraphError::Decode(_))));
    }

    #[test]
    fn cursor_serializes_with_state_tag() {
        let cursor = DeltaCursor::Next("https://x/y".into());
        let json = serde_json::to_string(&cursor).unwrap_or_default();
        assert_eq!(json, r#"{"state":"next","url":"https://x/y"}"#);
        let back: DeltaCursor =
            serde_json::from_str(&json).unwrap_or(DeltaCursor::Done(String::new()));
        assert_eq!(back, cursor);
    }
}
