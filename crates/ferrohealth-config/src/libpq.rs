// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! A database URL composed from PostgreSQL's client environment.
//!
//! libpq reads the connection parameters a URL would carry from `PGHOST`,
//! `PGPORT`, `PGDATABASE`, `PGUSER`, `PGPASSWORD` and `PGSSLMODE`
//! (PostgreSQL 18, "Environment Variables",
//! <https://www.postgresql.org/docs/18/libpq-envars.html>), and managed
//! PostgreSQL integrations inject that set in place of a URL. [`url`] turns
//! it into the `postgres://` URL a product's database key holds, for
//! [`crate::loader::Loader::fallback_composed`].

use std::collections::BTreeMap;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

/// Every variable [`url`] reads.
pub const VARIABLES: &[&str] = &[
    "PGHOST_UNPOOLED",
    "PGHOST",
    "PGPORT",
    "PGDATABASE",
    "PGUSER",
    "PGPASSWORD",
    "PGSSLMODE",
];

// NOTE: RFC 3986 §3.2.1 and §2.3; only the unreserved characters stay as written in the userinfo.
const USERINFO: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Returns the `postgres://` URL the libpq variables in `environment`
/// describe, or `None` without a host.
///
/// The host is `PGHOST_UNPOOLED` when set, the direct endpoint a managed
/// service offers beside its pooled `PGHOST`, because a transaction pooler
/// does not keep the session state a migration or a `search_path` needs.
/// The user and the password are percent-encoded; a password without a user
/// is not written, as libpq reads no password then. Without `PGDATABASE` the
/// URL names no database, so the client takes its default, the user name.
///
/// ```
/// use std::collections::BTreeMap;
///
/// let mut environment = BTreeMap::new();
/// environment.insert("PGHOST".to_owned(), "db.example".to_owned());
/// environment.insert("PGUSER".to_owned(), "app".to_owned());
/// environment.insert("PGPASSWORD".to_owned(), "p@ss word".to_owned());
/// environment.insert("PGDATABASE".to_owned(), "records".to_owned());
/// environment.insert("PGSSLMODE".to_owned(), "require".to_owned());
/// let expected = "postgres://app:p%40ss%20word@db.example/records?sslmode=require";
/// assert_eq!(Some(expected.to_owned()), ferrohealth_config::libpq::url(&environment));
/// ```
#[must_use]
pub fn url(environment: &BTreeMap<String, String>) -> Option<String> {
    let host = environment
        .get("PGHOST_UNPOOLED")
        .or_else(|| environment.get("PGHOST"))?;
    let mut url = String::from("postgres://");
    if let Some(user) = environment.get("PGUSER") {
        url.extend(utf8_percent_encode(user, USERINFO));
        if let Some(password) = environment.get("PGPASSWORD") {
            url.push(':');
            url.extend(utf8_percent_encode(password, USERINFO));
        }
        url.push('@');
    }
    url.push_str(host);
    if let Some(port) = environment.get("PGPORT") {
        url.push(':');
        url.push_str(port);
    }
    if let Some(database) = environment.get("PGDATABASE") {
        url.push('/');
        url.extend(utf8_percent_encode(database, USERINFO));
    }
    if let Some(mode) = environment.get("PGSSLMODE") {
        url.push_str("?sslmode=");
        url.extend(utf8_percent_encode(mode, USERINFO));
    }
    Some(url)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::url;

    fn environment(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn no_host_gives_no_url() {
        assert_eq!(None, url(&environment(&[("PGUSER", "app")])));
    }

    #[test]
    fn the_direct_host_wins_over_the_pooled_one() {
        let set = environment(&[
            ("PGHOST", "pooler.example"),
            ("PGHOST_UNPOOLED", "direct.example"),
            ("PGPORT", "5433"),
        ]);
        assert_eq!(Some("postgres://direct.example:5433".to_owned()), url(&set));
    }

    #[test]
    fn a_password_without_a_user_is_not_written() {
        let set = environment(&[("PGHOST", "db"), ("PGPASSWORD", "secret")]);
        assert_eq!(Some("postgres://db".to_owned()), url(&set));
    }

    #[test]
    fn the_userinfo_is_percent_encoded() {
        let set = environment(&[
            ("PGHOST", "db"),
            ("PGUSER", "a:b@c"),
            ("PGPASSWORD", "/?#[]"),
        ]);
        assert_eq!(
            Some("postgres://a%3Ab%40c:%2F%3F%23%5B%5D@db".to_owned()),
            url(&set)
        );
    }
}
