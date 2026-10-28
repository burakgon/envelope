//! Outlook master categories (`/me/outlook/masterCategories`).

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::client::{GraphClient, GraphRequest};
use crate::error::GraphError;
use crate::serde_util::{graph_enum, null_default};

graph_enum! {
    /// Color preset of an Outlook category. Outlook decides the actual
    /// color; [`CategoryColor::hex`] gives a close approximation.
    pub enum CategoryColor {
        /// No color.
        NoColor = "none",
        /// Red.
        Preset0 = "preset0",
        /// Orange.
        Preset1 = "preset1",
        /// Brown.
        Preset2 = "preset2",
        /// Yellow.
        Preset3 = "preset3",
        /// Green.
        Preset4 = "preset4",
        /// Teal.
        Preset5 = "preset5",
        /// Olive.
        Preset6 = "preset6",
        /// Blue.
        Preset7 = "preset7",
        /// Purple.
        Preset8 = "preset8",
        /// Cranberry.
        Preset9 = "preset9",
        /// Steel.
        Preset10 = "preset10",
        /// Dark steel.
        Preset11 = "preset11",
        /// Gray.
        Preset12 = "preset12",
        /// Dark gray.
        Preset13 = "preset13",
        /// Black.
        Preset14 = "preset14",
        /// Dark red.
        Preset15 = "preset15",
        /// Dark orange.
        Preset16 = "preset16",
        /// Dark brown.
        Preset17 = "preset17",
        /// Dark yellow.
        Preset18 = "preset18",
        /// Dark green.
        Preset19 = "preset19",
        /// Dark teal.
        Preset20 = "preset20",
        /// Dark olive.
        Preset21 = "preset21",
        /// Dark blue.
        Preset22 = "preset22",
        /// Dark purple.
        Preset23 = "preset23",
        /// Dark cranberry.
        Preset24 = "preset24",
    }
    unknown = NoColor;
    default = NoColor;
}

/// Color names and approximate CSS colors of the presets, in preset order.
/// Microsoft documents only the names; the hex values approximate what
/// Outlook renders.
const PRESETS: [(&str, &str); 25] = [
    ("Red", "#e74856"),
    ("Orange", "#ff8c00"),
    ("Brown", "#ab7b4f"),
    ("Yellow", "#fce100"),
    ("Green", "#47b347"),
    ("Teal", "#00b7c3"),
    ("Olive", "#8c9a2b"),
    ("Blue", "#3a96dd"),
    ("Purple", "#8764b8"),
    ("Cranberry", "#c50f1f"),
    ("Steel", "#8ba3b6"),
    ("DarkSteel", "#4a5e6d"),
    ("Gray", "#a0a0a0"),
    ("DarkGray", "#696969"),
    ("Black", "#1f1f1f"),
    ("DarkRed", "#750b1c"),
    ("DarkOrange", "#ca5010"),
    ("DarkBrown", "#6b4527"),
    ("DarkYellow", "#c19c00"),
    ("DarkGreen", "#0b6a0b"),
    ("DarkTeal", "#005b70"),
    ("DarkOlive", "#5a6417"),
    ("DarkBlue", "#004e8c"),
    ("DarkPurple", "#401b6c"),
    ("DarkCranberry", "#6e0811"),
];

impl CategoryColor {
    fn preset_index(self) -> Option<usize> {
        CategoryColor::ALL
            .iter()
            .position(|c| *c == self)
            .and_then(|i| i.checked_sub(1))
    }

    /// The Outlook color name (`Red`, `DarkSteel`, …), or `None`.
    pub fn name(self) -> &'static str {
        self.preset_index()
            .and_then(|i| PRESETS.get(i))
            .map_or("None", |(name, _)| name)
    }

    /// Approximate CSS color (`#rrggbb`), or `None` for [`CategoryColor::NoColor`].
    pub fn hex(self) -> Option<&'static str> {
        self.preset_index()
            .and_then(|i| PRESETS.get(i))
            .map(|(_, hex)| *hex)
    }
}

/// A category from the user's master category list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutlookCategory {
    /// Category id.
    pub id: String,
    /// The category name, which is also the value stored on items. It cannot
    /// be changed after creation.
    #[serde(default, deserialize_with = "null_default")]
    pub display_name: String,
    /// Color preset.
    #[serde(default, deserialize_with = "null_default")]
    pub color: CategoryColor,
}

impl GraphClient {
    /// The user's master category list.
    pub async fn master_categories(&self) -> Result<Vec<OutlookCategory>, GraphError> {
        self.get_all(
            &GraphRequest::get(["me", "outlook", "masterCategories"]),
            None,
        )
        .await
    }

    /// Adds a category to the master list.
    pub async fn create_category(
        &self,
        display_name: &str,
        color: CategoryColor,
    ) -> Result<OutlookCategory, GraphError> {
        let request = GraphRequest::post(["me", "outlook", "masterCategories"])
            .json(&json!({ "displayName": display_name, "color": color }))?;
        self.send_json(&request).await
    }

    /// Changes the color of a category (the only writable property).
    pub async fn set_category_color(
        &self,
        category_id: &str,
        color: CategoryColor,
    ) -> Result<(), GraphError> {
        let request = GraphRequest::patch(["me", "outlook", "masterCategories", category_id])
            .json(&json!({ "color": color }))?;
        self.send_empty(&request).await
    }

    /// Removes a category from the master list (items keep the name).
    pub async fn delete_category(&self, category_id: &str) -> Result<(), GraphError> {
        let request = GraphRequest::delete(["me", "outlook", "masterCategories", category_id]);
        self.send_empty(&request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_names_and_colors() {
        assert_eq!(CategoryColor::Preset0.name(), "Red");
        assert_eq!(CategoryColor::Preset0.hex(), Some("#e74856"));
        assert_eq!(CategoryColor::Preset11.name(), "DarkSteel");
        assert_eq!(CategoryColor::Preset24.name(), "DarkCranberry");
        assert_eq!(CategoryColor::NoColor.hex(), None);
        assert_eq!(CategoryColor::NoColor.name(), "None");
        assert_eq!(CategoryColor::ALL.len(), PRESETS.len() + 1);
        for color in CategoryColor::ALL.iter().skip(1) {
            let hex = color.hex().unwrap_or_default();
            assert!(hex.len() == 7 && hex.starts_with('#'), "{color}: {hex}");
        }
    }

    #[test]
    fn parses_documented_casing() {
        let category: OutlookCategory = serde_json::from_str(
            r#"{"id":"1","displayName":"Project expenses","color":"Preset9"}"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(category.color, CategoryColor::Preset9);
        assert_eq!(category.color.name(), "Cranberry");
    }
}
