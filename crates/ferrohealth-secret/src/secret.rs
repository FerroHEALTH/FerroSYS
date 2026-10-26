// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! A secret string that never renders itself.

use std::fmt;

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::REDACTED;

/// A credential that never renders itself.
///
/// The value is a [`SecretString`], zeroed on drop. `Debug`, `Display` and
/// `Serialize` all show [`REDACTED`]; it deserializes from a plain string,
/// and the value is reached only through [`Secret::expose`].
///
/// # Examples
///
/// ```
/// use ferrohealth_secret::secret::Secret;
///
/// let token = Secret::new("Qz7-token");
/// assert_eq!(format!("{token:?}"), "\"***\"");
/// assert_eq!(token.to_string(), "***");
/// assert_eq!(token.expose(), "Qz7-token");
/// ```
#[derive(Clone)]
pub struct Secret(SecretString);

impl Default for Secret {
    fn default() -> Self {
        Self::new(String::new())
    }
}

impl Secret {
    /// Creates a secret holding `value`.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(SecretString::from(value.into()))
    }

    /// Returns the value, for the one place a request or a connection is
    /// composed from it.
    ///
    /// Never log or format the result.
    #[must_use]
    pub fn expose(&self) -> &str {
        self.0.expose_secret()
    }

    /// Returns the value still wrapped, for a client that takes a
    /// [`SecretString`] and composes its request from that.
    #[must_use]
    pub fn to_secret_string(&self) -> SecretString {
        self.0.clone()
    }

    /// Whether the secret is the empty string.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.expose_secret().is_empty()
    }
}

impl From<SecretString> for Secret {
    fn from(value: SecretString) -> Self {
        Self(value)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(REDACTED, f)
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl PartialEq for Secret {
    fn eq(&self, other: &Self) -> bool {
        self.expose() == other.expose()
    }
}

impl Eq for Secret {}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer).map(Self::new)
    }
}

impl Serialize for Secret {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(REDACTED)
    }
}

/// A secret on its one trip to the server that stores it.
///
/// A console sends a secret an operator typed to its server in the body of
/// one request. `Debug` and `Display` show [`REDACTED`]; `Serialize` writes
/// the value itself, because that request is the one place it travels.
/// Nothing else serializes one, and the server never answers, stores in
/// memory beyond the write, or logs it.
///
/// # Examples
///
/// ```
/// use ferrohealth_secret::secret::WriteOnly;
///
/// let value = WriteOnly::new("Qz7-client-secret");
/// assert_eq!(format!("{value:?}"), "\"***\"");
/// assert_eq!(serde_json::to_string(&value)?, "\"Qz7-client-secret\"");
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Clone, Default, PartialEq, Eq)]
pub struct WriteOnly(Secret);

impl WriteOnly {
    /// Creates the value `value`.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(Secret::new(value))
    }

    /// Returns the value, for the one place it is written.
    ///
    /// Never log or format the result.
    #[must_use]
    pub fn expose(&self) -> &str {
        self.0.expose()
    }

    /// Whether the value is the empty string, as a form field left empty
    /// gives it.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the value as the [`Secret`] the server keeps.
    #[must_use]
    pub fn into_secret(self) -> Secret {
        self.0
    }
}

impl From<SecretString> for WriteOnly {
    fn from(value: SecretString) -> Self {
        Self(Secret::from(value))
    }
}

impl fmt::Debug for WriteOnly {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl fmt::Display for WriteOnly {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl<'de> Deserialize<'de> for WriteOnly {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Secret::deserialize(deserializer).map(Self)
    }
}

impl Serialize for WriteOnly {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.0.expose())
    }
}

#[cfg(test)]
mod tests {
    use secrecy::{ExposeSecret, SecretString};

    use super::Secret;
    use crate::REDACTED;

    const SENTINEL: &str = "Qz7sentinel";

    #[test]
    fn a_secret_renders_as_the_placeholder_everywhere() {
        let secret = Secret::new(SENTINEL);
        for shown in [
            format!("{secret:?}"),
            format!("{secret:#?}"),
            format!("{secret}"),
            serde_json::to_string(&secret).expect("a secret serializes"),
            toml::Value::try_from(&secret)
                .expect("a secret serializes")
                .to_string(),
        ] {
            assert!(
                !shown.contains(SENTINEL),
                "a rendering shows what it must hide"
            );
            assert!(
                shown.contains(REDACTED),
                "a rendering lacks what it must show"
            );
        }
        assert_eq!(SENTINEL, secret.expose());
    }

    #[test]
    fn a_struct_holding_a_secret_renders_without_it() {
        #[derive(Debug, serde::Serialize)]
        struct Section {
            user: String,
            password: Secret,
        }
        let section = Section {
            user: "ferrosys".to_owned(),
            password: Secret::new(SENTINEL),
        };
        let shown = [
            format!("{section:?}"),
            serde_json::to_string(&section).expect("the section serializes"),
        ];
        for text in shown {
            assert!(
                !text.contains(SENTINEL),
                "a rendering shows what it must hide"
            );
            assert!(
                text.contains("ferrosys"),
                "a rendering lacks what it must show"
            );
        }
    }

    #[test]
    fn a_secret_reads_from_a_plain_string() {
        let table: toml::Table = toml::from_str("token = \"Qz7sentinel\"").expect("toml");
        let secret: Secret = table
            .get("token")
            .cloned()
            .expect("the key")
            .try_into()
            .expect("a string reads as a secret");
        assert_eq!(SENTINEL, secret.expose());
        let secret: Secret = serde_json::from_str("\"Qz7sentinel\"").expect("json");
        assert_eq!(SENTINEL, secret.expose());
    }

    #[test]
    fn a_secret_refuses_what_is_not_a_string() {
        assert!(
            serde_json::from_str::<Secret>("42").is_err(),
            "a number is no secret"
        );
        assert!(
            serde_json::from_str::<Secret>("null").is_err(),
            "null is no secret"
        );
    }

    #[test]
    fn a_secret_knows_when_it_is_empty() {
        assert!(Secret::new("").is_empty(), "the empty string is empty");
        assert!(
            !Secret::new(SENTINEL).is_empty(),
            "the sentinel is not empty"
        );
    }

    #[test]
    fn a_secret_converts_from_and_to_a_secret_string() {
        let secret = Secret::from(SecretString::from(SENTINEL));
        assert_eq!(SENTINEL, secret.expose());
        assert_eq!(SENTINEL, secret.to_secret_string().expose_secret());
    }

    #[test]
    fn a_write_only_value_hides_in_renderings_and_travels_in_its_body() {
        let value = super::WriteOnly::new(SENTINEL);
        for shown in [format!("{value:?}"), format!("{value}")] {
            assert!(
                !shown.contains(SENTINEL),
                "a rendering shows what it must hide"
            );
        }
        assert_eq!(
            format!("\"{SENTINEL}\""),
            serde_json::to_string(&value).expect("the value serializes")
        );
        let read: super::WriteOnly = serde_json::from_str("\"Qz7sentinel\"").expect("JSON");
        assert_eq!(value, read);
        assert!(
            super::WriteOnly::default().is_empty(),
            "an empty field is empty"
        );
        assert_eq!(SENTINEL, value.clone().into_secret().expose());
        assert_eq!(
            SENTINEL,
            super::WriteOnly::from(SecretString::from(SENTINEL)).expose()
        );
    }

    #[test]
    fn secrets_compare_by_value() {
        assert_eq!(Secret::new(SENTINEL), Secret::new(SENTINEL));
        assert_ne!(Secret::new(SENTINEL), Secret::new("other"));
    }
}
