//! Small serde helpers for lenient decoding of Google responses.

use serde::{Deserialize, Deserializer};

/// Decodes `null` as `T::default()`. Combine with `#[serde(default)]` so a
/// missing field is accepted too.
pub(crate) fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// Returns `Some(s)` unless `s` is empty or whitespace.
pub(crate) fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.trim().is_empty())
}
