//! Generic iCalendar / vCard component tree.
//!
//! Every calendar object is parsed into this tree first, so properties and
//! subcomponents the crate does not understand (vendor `X-` extensions, newer
//! RFCs, …) survive a parse → edit → serialize round trip untouched.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::text::{escape_text, text_list, unescape_text};
use crate::writer;

/// A property parameter such as `TZID=Europe/Berlin` or `TYPE=WORK,VOICE`.
///
/// Values are stored unquoted and with RFC 6868 caret escapes decoded. A
/// parameter without `=` (vCard 2.1 style `TEL;WORK:`) has no values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Param {
    /// Upper-case parameter name.
    pub name: String,
    /// Parameter values (a parameter may carry a comma-separated list).
    pub values: Vec<String>,
}

impl Param {
    /// A parameter with a single value. The name is upper-cased.
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Param {
            name: name.into().to_ascii_uppercase(),
            values: vec![value.into()],
        }
    }

    /// A parameter with several values. The name is upper-cased.
    pub fn with_values(name: impl Into<String>, values: Vec<String>) -> Self {
        Param {
            name: name.into().to_ascii_uppercase(),
            values,
        }
    }

    /// The first value, if any.
    pub fn value(&self) -> Option<&str> {
        self.values.first().map(String::as_str)
    }
}

/// A content line: `[group.]NAME;PARAM=value:VALUE`.
///
/// `value` holds the raw, still escaped value exactly as it appeared in the
/// source (after line unfolding). Use [`Property::text_value`] for TEXT values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Property {
    /// vCard property group (`item1` in `item1.EMAIL`), if any.
    pub group: Option<String>,
    /// Upper-case property name.
    pub name: String,
    /// Parameters in source order.
    pub params: Vec<Param>,
    /// Raw (escaped) value.
    pub value: String,
}

impl Property {
    /// A property with a raw (already escaped) value. The name is upper-cased.
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Property {
            group: None,
            name: name.into().to_ascii_uppercase(),
            params: Vec::new(),
            value: value.into(),
        }
    }

    /// A property whose value is `text`, escaped as a TEXT value.
    pub fn text(name: impl Into<String>, text: &str) -> Self {
        Property::new(name, escape_text(text))
    }

    /// Builder-style variant of [`Property::set_param`].
    #[must_use]
    pub fn with_param(mut self, name: &str, value: impl Into<String>) -> Self {
        self.set_param(name, value);
        self
    }

    /// The first value of the first parameter called `name` (case-insensitive).
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .and_then(Param::value)
    }

    /// All values of all parameters called `name`, flattened.
    pub fn param_values(&self, name: &str) -> Vec<&str> {
        self.params
            .iter()
            .filter(|p| p.name.eq_ignore_ascii_case(name))
            .flat_map(|p| p.values.iter().map(String::as_str))
            .collect()
    }

    /// True if a parameter called `name` exists (with or without values).
    pub fn has_param(&self, name: &str) -> bool {
        self.params
            .iter()
            .any(|p| p.name.eq_ignore_ascii_case(name))
    }

    /// True if parameter `name` has `value` among its values (both case-insensitive).
    pub fn has_param_value(&self, name: &str, value: &str) -> bool {
        self.param_values(name)
            .iter()
            .any(|v| v.eq_ignore_ascii_case(value))
    }

    /// Replaces every parameter called `name` with a single-valued one.
    pub fn set_param(&mut self, name: &str, value: impl Into<String>) {
        self.set_param_values(name, vec![value.into()]);
    }

    /// Replaces every parameter called `name` with one carrying `values`.
    /// The parameter keeps the position of its first previous occurrence.
    pub fn set_param_values(&mut self, name: &str, values: Vec<String>) {
        let position = self
            .params
            .iter()
            .position(|p| p.name.eq_ignore_ascii_case(name));
        self.remove_param(name);
        let param = Param::with_values(name, values);
        match position {
            Some(index) if index <= self.params.len() => self.params.insert(index, param),
            _ => self.params.push(param),
        }
    }

    /// Removes every parameter called `name`.
    pub fn remove_param(&mut self, name: &str) {
        self.params.retain(|p| !p.name.eq_ignore_ascii_case(name));
    }

    /// The value unescaped as a TEXT value.
    pub fn text_value(&self) -> String {
        unescape_text(&self.value)
    }

    /// The value split as a comma-separated TEXT list (`CATEGORIES`, …).
    pub fn text_values(&self) -> Vec<String> {
        text_list(&self.value)
    }

    /// Serializes this property as one folded content line (CRLF terminated).
    pub fn to_content_line(&self) -> String {
        let mut out = String::new();
        writer::write_property(self, &mut out);
        out
    }
}

/// A component (`VCALENDAR`, `VEVENT`, `VALARM`, `VCARD`, …) with its
/// properties and nested components, in source order.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Component {
    /// Upper-case component name.
    pub name: String,
    /// Properties in source order.
    pub properties: Vec<Property>,
    /// Nested components in source order.
    pub components: Vec<Component>,
}

impl Component {
    /// An empty component. The name is upper-cased.
    pub fn new(name: impl Into<String>) -> Self {
        Component {
            name: name.into().to_ascii_uppercase(),
            properties: Vec::new(),
            components: Vec::new(),
        }
    }

    /// True if the component is called `name` (case-insensitive).
    pub fn is(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }

    /// The first property called `name`.
    pub fn property(&self, name: &str) -> Option<&Property> {
        self.properties
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
    }

    /// Mutable access to the first property called `name`.
    pub fn property_mut(&mut self, name: &str) -> Option<&mut Property> {
        self.properties
            .iter_mut()
            .find(|p| p.name.eq_ignore_ascii_case(name))
    }

    /// Every property called `name`, in order.
    pub fn properties_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Property> {
        self.properties
            .iter()
            .filter(move |p| p.name.eq_ignore_ascii_case(name))
    }

    /// Raw value of the first property called `name`.
    pub fn value(&self, name: &str) -> Option<&str> {
        self.property(name).map(|p| p.value.as_str())
    }

    /// Unescaped TEXT value of the first property called `name`.
    pub fn text(&self, name: &str) -> Option<String> {
        self.property(name).map(Property::text_value)
    }

    /// Appends a property.
    pub fn push(&mut self, property: Property) {
        self.properties.push(property);
    }

    /// Replaces every property with the same name as `property` by
    /// `property`, keeping the position of the first one; appends otherwise.
    pub fn set(&mut self, property: Property) {
        let name = property.name.clone();
        self.set_all(&name, vec![property]);
    }

    /// Replaces every property called `name` by `properties`, inserted at the
    /// position of the first existing one (or appended if there was none).
    pub fn set_all(&mut self, name: &str, properties: Vec<Property>) {
        let position = self
            .properties
            .iter()
            .position(|p| p.name.eq_ignore_ascii_case(name));
        self.remove_all(name);
        let index = position
            .unwrap_or(self.properties.len())
            .min(self.properties.len());
        for (offset, property) in properties.into_iter().enumerate() {
            self.properties.insert(index + offset, property);
        }
    }

    /// Removes every property called `name`; returns how many were removed.
    pub fn remove_all(&mut self, name: &str) -> usize {
        let before = self.properties.len();
        self.properties
            .retain(|p| !p.name.eq_ignore_ascii_case(name));
        before - self.properties.len()
    }

    /// Nested components called `name`.
    pub fn children<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Component> {
        self.components.iter().filter(move |c| c.is(name))
    }

    /// Serializes the component (and its children) to folded CRLF text.
    pub fn to_ical(&self) -> String {
        let mut out = String::new();
        writer::write_component(self, &mut out);
        out
    }
}

impl fmt::Display for Component {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_ical())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn property_params_are_case_insensitive() {
        let mut p = Property::new("dtstart", "20240101T090000").with_param("tzid", "Europe/Berlin");
        assert_eq!(p.name, "DTSTART");
        assert_eq!(p.param("TZID"), Some("Europe/Berlin"));
        assert!(p.has_param("Tzid"));
        p.set_param("TZID", "Europe/Istanbul");
        assert_eq!(p.param_values("tzid"), vec!["Europe/Istanbul"]);
        p.remove_param("TZID");
        assert!(!p.has_param("TZID"));
    }

    #[test]
    fn set_keeps_position() {
        let mut c = Component::new("vevent");
        c.push(Property::new("UID", "1"));
        c.push(Property::new("SUMMARY", "a"));
        c.push(Property::new("LOCATION", "x"));
        c.push(Property::new("SUMMARY", "b"));
        c.set(Property::text("SUMMARY", "new, text"));
        let names: Vec<_> = c.properties.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["UID", "SUMMARY", "LOCATION"]);
        assert_eq!(c.text("summary").as_deref(), Some("new, text"));
        assert_eq!(c.value("SUMMARY"), Some("new\\, text"));
    }

    #[test]
    fn set_all_appends_when_missing() {
        let mut c = Component::new("VEVENT");
        c.push(Property::new("UID", "1"));
        c.set_all(
            "EXDATE",
            vec![Property::new("EXDATE", "a"), Property::new("EXDATE", "b")],
        );
        assert_eq!(c.properties_named("exdate").count(), 2);
        assert_eq!(c.remove_all("EXDATE"), 2);
    }
}
