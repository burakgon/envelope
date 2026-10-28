//! Texts shown outside the web view (notifications, the tray menu) in the
//! interface language. The interface sends its translations once it has
//! loaded; until then, and for anything missing, English is used.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

fn table() -> &'static RwLock<HashMap<String, String>> {
    static TABLE: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();
    TABLE.get_or_init(Default::default)
}

/// Replaces the translations.
pub fn set(strings: HashMap<String, String>) {
    if let Ok(mut t) = table().write() {
        *t = strings;
    }
}

/// The text for `key`, or `english` when there is no translation.
pub fn get(key: &str, english: &str) -> String {
    table()
        .read()
        .ok()
        .and_then(|t| t.get(key).filter(|s| !s.is_empty()).cloned())
        .unwrap_or_else(|| english.to_owned())
}

/// [`get`] with `{name}` placeholders filled in.
pub fn fill(key: &str, english: &str, args: &[(&str, &str)]) -> String {
    let mut text = get(key, english);
    for (name, value) in args {
        text = text.replace(&format!("{{{name}}}"), value);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falls_back_to_english_and_fills_placeholders() {
        assert_eq!(get("native.test.missing", "Archive"), "Archive");
        set(HashMap::from([
            (
                "native.test.count".to_string(),
                "{count} neue Nachrichten".to_string(),
            ),
            ("native.test.empty".to_string(), String::new()),
        ]));
        assert_eq!(
            fill(
                "native.test.count",
                "{count} new messages",
                &[("count", "3")]
            ),
            "3 neue Nachrichten"
        );
        assert_eq!(get("native.test.empty", "Open"), "Open");
        set(HashMap::new());
    }
}
