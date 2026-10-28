//! Serde helpers shared by the wire types.

use serde::{Deserialize, Deserializer};

/// Deserializes `null` as `T::default()`. Combine with `#[serde(default)]`
/// so that a missing field also becomes the default.
pub(crate) fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// Deserializes a boolean sent as JSON `true`/`false`, as the strings
/// `"true"`/`"false"` (seen in Graph documentation samples) or as `null`
/// (`false`).
pub(crate) fn lenient_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Bool(bool),
        Text(String),
    }
    match Option::<Raw>::deserialize(deserializer)? {
        None => Ok(false),
        Some(Raw::Bool(value)) => Ok(value),
        Some(Raw::Text(text)) => {
            let text = text.trim();
            if text.eq_ignore_ascii_case("true") {
                Ok(true)
            } else if text.eq_ignore_ascii_case("false") || text.is_empty() {
                Ok(false)
            } else {
                Err(serde::de::Error::custom(format!(
                    "expected a boolean, found `{text}`"
                )))
            }
        }
    }
}

/// `skip_serializing_if` helper for numeric fields.
pub(crate) fn is_zero(value: &u32) -> bool {
    *value == 0
}

/// Defines an enum that mirrors a Microsoft Graph string enumeration.
///
/// The generated type serializes to the exact wire value and deserializes
/// case-insensitively (Graph is not always consistent about casing). With
/// `unknown = Variant;` unrecognized values map to that variant instead of
/// failing, which keeps sync robust when Microsoft adds new values;
/// `default = Variant;` additionally implements [`Default`].
macro_rules! graph_enum {
    (@unknown $name:ident, $value:ident, $unknown:ident) => {{
        let _ = $value;
        Ok($name::$unknown)
    }};
    (@unknown $name:ident, $value:ident) => {
        Err(serde::de::Error::custom(format!(
            "unknown {} value `{}`",
            stringify!($name),
            $value
        )))
    };
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident = $wire:literal, )+
        }
        $( unknown = $unknown:ident; )?
        $( default = $default:ident; )?
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        $vis enum $name {
            $( $(#[$vmeta])* $variant, )+
        }

        impl $name {
            /// All variants, in declaration order.
            pub const ALL: &'static [$name] = &[$( $name::$variant, )+];

            /// The value exactly as Microsoft Graph writes it.
            pub fn as_str(self) -> &'static str {
                match self {
                    $( $name::$variant => $wire, )+
                }
            }

            /// Parses a wire value, ignoring ASCII case.
            pub fn parse(value: &str) -> Option<Self> {
                $(
                    if value.eq_ignore_ascii_case($wire) {
                        return Some($name::$variant);
                    }
                )+
                None
            }
        }

        $(
            impl Default for $name {
                fn default() -> Self {
                    $name::$default
                }
            }
        )?

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                match $name::parse(&value) {
                    Some(parsed) => Ok(parsed),
                    None => graph_enum!(@unknown $name, value $(, $unknown)?),
                }
            }
        }
    };
}

pub(crate) use graph_enum;
