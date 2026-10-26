// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! URLs whose renderings never show a credential.
//!
//! A URL may carry a user name and password in its userinfo (RFC 3986
//! §3.2.1) and a credential in its query, so both are replaced by
//! [`REDACTED`] wherever a URL is shown. [`SecretUrl`] holds a URL the
//! product connects with; [`RedactedUrl`] renders any URL text the same way,
//! for a value the product logs but does not own. This is the one copy of
//! the rule in the family.

use std::fmt;

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::REDACTED;

/// A URL or connection string kept as written, whose renderings never show
/// a credential.
///
/// The text is a [`SecretString`], zeroed on drop. [`SecretUrl::expose`]
/// returns it verbatim for connecting with it; `Debug`, `Display` and
/// `Serialize` show [`SecretUrl::redacted`]. It deserializes from a plain
/// string.
///
/// # Examples
///
/// ```
/// use ferrohealth_secret::secret_url::SecretUrl;
///
/// let url = SecretUrl::new("postgres://ferrosys:Qz7pw@db.example.org:5432/ferrosys");
/// assert_eq!(url.to_string(), "postgres://***@db.example.org:5432/ferrosys");
/// assert_eq!(url.expose(), "postgres://ferrosys:Qz7pw@db.example.org:5432/ferrosys");
/// ```
#[derive(Clone, Default)]
pub struct SecretUrl(SecretString);

impl SecretUrl {
    /// Creates a URL holding `url` verbatim.
    #[must_use]
    pub fn new(url: impl Into<String>) -> Self {
        Self(SecretString::from(url.into()))
    }

    /// Returns the URL as written, credentials included, for connecting with
    /// it.
    ///
    /// Never log or format the result.
    #[must_use]
    pub fn expose(&self) -> &str {
        self.0.expose_secret()
    }

    /// Whether the URL is the empty string.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.expose_secret().is_empty()
    }

    /// Returns the URL with every part that may carry a credential replaced
    /// by [`REDACTED`], as [`RedactedUrl`] renders it.
    #[must_use]
    pub fn redacted(&self) -> String {
        redact(self.expose())
    }
}

impl From<SecretString> for SecretUrl {
    fn from(url: SecretString) -> Self {
        Self(url)
    }
}

impl fmt::Debug for SecretUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.redacted(), f)
    }
}

impl fmt::Display for SecretUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.redacted())
    }
}

impl PartialEq for SecretUrl {
    fn eq(&self, other: &Self) -> bool {
        self.expose() == other.expose()
    }
}

impl Eq for SecretUrl {}

impl<'de> Deserialize<'de> for SecretUrl {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer).map(Self::new)
    }
}

impl Serialize for SecretUrl {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.redacted())
    }
}

/// A view of URL text that renders with its userinfo and its query
/// redacted, for a URL the product shows but does not hold as a
/// [`SecretUrl`].
///
/// `Debug` quotes the redacted text, `Display` writes it bare.
///
/// The userinfo becomes `***@` and the query `?***`; the scheme, the host,
/// the port, the path and the fragment show as written. Text with no `://`
/// (a libpq key/value connection string, which carries its password outside
/// any userinfo) shows as [`REDACTED`] whole. The text is never decoded, so
/// it is redacted as written.
///
/// # Examples
///
/// ```
/// use ferrohealth_secret::secret_url::RedactedUrl;
///
/// let shown = RedactedUrl("https://user:Qz7pw@pix.example.org/fhir?_page=Qz7");
/// assert_eq!(shown.to_string(), "https://***@pix.example.org/fhir?***");
/// assert_eq!(format!("{shown:?}"), "\"https://***@pix.example.org/fhir?***\"");
/// ```
#[derive(Clone, Copy)]
pub struct RedactedUrl<'a>(pub &'a str);

impl fmt::Debug for RedactedUrl<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&redact(self.0), f)
    }
}

impl fmt::Display for RedactedUrl<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&redact(self.0))
    }
}

/// Returns `url` with its userinfo and its query replaced by [`REDACTED`].
///
/// The userinfo is everything after `://` up to an `@`, since a host holds no
/// `@` (RFC 3986 §3.2). When the text parses as a URL (the WHATWG URL
/// Standard, as `url` and every client built on it read it), the authority
/// ends at its first `/`, `?` or `#`, so an `@` after it is in the path or
/// the query and shows as written. When it does not parse, the userinfo runs
/// to its last `@`, so a password holding a `/`, a `?` or a `#` is still
/// found.
fn redact(url: &str) -> String {
    if url.is_empty() {
        return String::new();
    }
    let Some((scheme, rest)) = url.split_once("://") else {
        return REDACTED.to_owned();
    };
    let at = if ::url::Url::parse(url).is_ok() {
        let authority = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        rest.get(..authority).and_then(|text| text.rfind('@'))
    } else {
        rest.rfind('@')
    };
    let (userinfo, rest) = match at {
        Some(at) => (
            format!("{REDACTED}@"),
            rest.get(at + 1..).unwrap_or_default(),
        ),
        None => (String::new(), rest),
    };
    let end = rest.find(['?', '#']).unwrap_or(rest.len());
    let (hierarchy, tail) = rest.split_at_checked(end).unwrap_or((rest, ""));
    let tail = match tail.strip_prefix('?') {
        Some(query) => {
            let fragment = query
                .find('#')
                .and_then(|at| query.get(at..))
                .unwrap_or_default();
            format!("?{REDACTED}{fragment}")
        }
        None => tail.to_owned(),
    };
    format!("{scheme}://{userinfo}{hierarchy}{tail}")
}

#[cfg(test)]
mod tests {
    use secrecy::SecretString;

    use super::{RedactedUrl, SecretUrl, redact};
    use crate::REDACTED;

    const SENTINEL: &str = "Qz7sentinel";

    #[test]
    fn a_url_with_userinfo_renders_without_it() {
        let raw = "postgres://ferrosys:Qz7sentinel@db.example.org:5432/ferrosys";
        let url = SecretUrl::new(raw);
        let redacted = "postgres://***@db.example.org:5432/ferrosys";
        assert_eq!(redacted, url.redacted());
        assert_eq!(redacted, url.to_string());
        assert_eq!(format!("{redacted:?}"), format!("{url:?}"));
        assert_eq!(
            format!("\"{redacted}\""),
            serde_json::to_string(&url).expect("a URL serializes")
        );
        assert_eq!(
            toml::Value::String(redacted.to_owned()),
            toml::Value::try_from(&url).expect("a URL serializes")
        );
        assert_eq!(raw, url.expose());
    }

    #[test]
    fn no_rendering_of_a_secret_url_carries_the_credential() {
        let url = SecretUrl::new("https://u:Qz7sentinel@cdr.example.org/?token=Qz7sentinel");
        for shown in [
            format!("{url:?}"),
            format!("{url:#?}"),
            format!("{url}"),
            serde_json::to_string(&url).expect("a URL serializes"),
            format!("{:?}", RedactedUrl(url.expose())),
            format!("{}", RedactedUrl(url.expose())),
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
    }

    #[test]
    fn every_part_that_may_carry_a_credential_is_redacted() {
        for (raw, redacted) in [
            (
                "https://Qz7user@pix.example.org/fhir",
                "https://***@pix.example.org/fhir",
            ),
            (
                "https://Qz7user:Qz7password@pix.example.org:8443/fhir/Patient/$ihe-pix",
                "https://***@pix.example.org:8443/fhir/Patient/$ihe-pix",
            ),
            (
                "https://u:p@ss@pix.example.org/",
                "https://***@pix.example.org/",
            ),
            (
                "https://u:p@pix.example.org?x=1",
                "https://***@pix.example.org?***",
            ),
            (
                "postgres://db.example.org/ferrosys?password=Qz7sentinel#f",
                "postgres://db.example.org/ferrosys?***#f",
            ),
            ("host=db.example.org password=Qz7sentinel", REDACTED),
            ("urn:uuid:1e7f2a52-0d1c-4b0f-9a52-5d3c1c0e2f11", REDACTED),
            ("u:Qz7sentinel@db.example.org/x", REDACTED),
        ] {
            assert_eq!(redacted, redact(raw), "{raw}");
        }
    }

    #[test]
    fn a_password_holding_a_delimiter_is_redacted() {
        for (raw, redacted) in [
            ("https://u:p/x@host", "https://***@host"),
            (
                "postgres://ferrosys:Qz7/sentinel@db.example.org:5432/ferrosys",
                "postgres://***@db.example.org:5432/ferrosys",
            ),
            (
                "postgres://ferrosys:Qz7?example@db.example.org/ferrosys?sslmode=require",
                "postgres://***@db.example.org/ferrosys?***",
            ),
            (
                "https://u:Qz7#sentinel@cdr.example.org/openehr",
                "https://***@cdr.example.org/openehr",
            ),
        ] {
            assert_eq!(redacted, redact(raw), "{raw}");
        }
    }

    #[test]
    fn an_at_sign_after_the_authority_is_not_userinfo() {
        for (raw, redacted) in [
            (
                "https://directory.example.org/fhir/Endpoint/@handle",
                "https://directory.example.org/fhir/Endpoint/@handle",
            ),
            (
                "https://pix.example.org/fhir/a@b/Patient",
                "https://pix.example.org/fhir/a@b/Patient",
            ),
            (
                "https://pix.example.org/fhir/Patient?email=someone@example.org",
                "https://pix.example.org/fhir/Patient?***",
            ),
            (
                "https://pix.example.org/fhir#section@b",
                "https://pix.example.org/fhir#section@b",
            ),
        ] {
            assert_eq!(redacted, redact(raw), "{raw}");
        }
    }

    #[test]
    fn text_that_does_not_parse_is_redacted_to_its_last_at_sign() {
        assert_eq!(
            "https://***@handle",
            redact("https://cdr example.org/path/@handle")
        );
    }

    #[test]
    fn multi_byte_text_is_redacted_on_character_boundaries() {
        assert_eq!(
            "https://***@zürich.example.org/fhir?***",
            redact("https://müller:pässwörd@zürich.example.org/fhir?näme=Jürgen")
        );
    }

    #[test]
    fn a_url_without_a_credential_renders_as_written() {
        for raw in [
            "https://pix.example.org/fhir/Patient/$ihe-pix",
            "https://directory.example.org/fhir/Endpoint/@handle",
            "http://127.0.0.1:4317/",
            "",
        ] {
            assert_eq!(raw, redact(raw), "{raw}");
            assert_eq!(raw, SecretUrl::new(raw).redacted(), "{raw}");
        }
        assert!(SecretUrl::default().is_empty(), "the default URL is empty");
        assert!(
            !SecretUrl::new("http://127.0.0.1/").is_empty(),
            "a URL is not empty"
        );
    }

    #[test]
    fn a_secret_url_reads_from_a_plain_string_and_compares_by_value() {
        let url: SecretUrl =
            serde_json::from_str("\"https://u:p@cdr.example.org/\"").expect("json");
        assert_eq!("https://u:p@cdr.example.org/", url.expose());
        assert_eq!(
            url,
            SecretUrl::from(SecretString::from("https://u:p@cdr.example.org/"))
        );
        assert_ne!(url, SecretUrl::new("https://cdr.example.org/"));
        assert!(
            serde_json::from_str::<SecretUrl>("7").is_err(),
            "a number is no URL"
        );
    }
}
