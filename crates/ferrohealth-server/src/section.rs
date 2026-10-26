// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The `[server]` section of a product's configuration.

use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The `[server]` section.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerSection {
    /// The socket address the listener binds.
    pub listen: SocketAddr,
    /// The path every route is served under: empty, or starting with `/`
    /// and not ending with one.
    pub base_path: BasePath,
    /// How long one request may take before the server answers `408`.
    pub request_timeout_ms: u64,
    /// The largest request body the server reads before answering `413`.
    pub body_limit_bytes: usize,
    /// How long readiness stays withdrawn before the listener closes, so an
    /// orchestrator stops sending requests first.
    pub drain_delay_ms: u64,
    /// How long the requests in flight may take once the listener closed.
    pub shutdown_timeout_ms: u64,
    /// How many requests the server serves at once; past it a request is
    /// answered `503`. Unset, there is no bound.
    pub max_concurrent_requests: Option<NonZeroUsize>,
    /// The seconds a refused request is told to wait (`Retry-After`).
    pub retry_after_s: u64,
    /// The listener's TLS; unset, the listener speaks plain HTTP behind a
    /// proxy that terminates TLS.
    pub tls: Option<TlsSection>,
}

/// The `[server.tls]` table: the certificate chain and the private key the
/// listener serves, each in PEM.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TlsSection {
    /// The certificate chain, the leaf first.
    pub certificate_file: std::path::PathBuf,
    /// The private key of the leaf.
    pub key_file: std::path::PathBuf,
}

impl Default for ServerSection {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], 8080)),
            base_path: BasePath::default(),
            request_timeout_ms: 30_000,
            body_limit_bytes: 1024 * 1024,
            drain_delay_ms: 0,
            shutdown_timeout_ms: 10_000,
            max_concurrent_requests: None,
            retry_after_s: 1,
            tls: None,
        }
    }
}

impl ServerSection {
    /// The request timeout.
    #[must_use]
    pub const fn request_timeout(&self) -> Duration {
        Duration::from_millis(self.request_timeout_ms)
    }

    /// The drain delay.
    #[must_use]
    pub const fn drain_delay(&self) -> Duration {
        Duration::from_millis(self.drain_delay_ms)
    }

    /// The bound on the drain.
    #[must_use]
    pub const fn shutdown_timeout(&self) -> Duration {
        Duration::from_millis(self.shutdown_timeout_ms)
    }
}

/// The path every route is served under, checked when the configuration is
/// read: empty, or `/` followed by segments with no trailing `/`.
///
/// # Examples
///
/// ```
/// use ferrohealth_server::section::BasePath;
///
/// assert_eq!("/ferropix", BasePath::new("/ferropix")?.as_str());
/// assert_eq!("/ferropix/health", BasePath::new("/ferropix")?.join("/health"));
/// assert!(BasePath::new("ferropix").is_err());
/// assert!(BasePath::new("/ferropix/").is_err());
/// # Ok::<(), ferrohealth_server::section::BasePathError>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BasePath(String);

/// A base path outside the form.
#[derive(Debug, thiserror::Error)]
#[error("the base path is not empty, or `/` followed by segments with no trailing `/`")]
pub struct BasePathError;

impl BasePath {
    /// Returns `path` as a base path.
    ///
    /// # Errors
    ///
    /// Returns [`BasePathError`] unless `path` is empty or starts with `/`,
    /// does not end with `/`, holds no empty segment and only the characters
    /// a path segment allows unescaped (RFC 3986 §3.3).
    pub fn new(path: impl Into<String>) -> Result<Self, BasePathError> {
        let path = path.into();
        if path.is_empty() {
            return Ok(Self(path));
        }
        let Some(rest) = path.strip_prefix('/') else {
            return Err(BasePathError);
        };
        let segments_are_valid = rest.split('/').all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-._~!$&'()*+,;=:@".contains(c))
        });
        if segments_are_valid {
            Ok(Self(path))
        } else {
            Err(BasePathError)
        }
    }

    /// Returns the path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns `route` under the base path.
    #[must_use]
    pub fn join(&self, route: &str) -> String {
        format!("{}{route}", self.0)
    }
}

impl<'de> Deserialize<'de> for BasePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let path = String::deserialize(deserializer)?;
        Self::new(path).map_err(serde::de::Error::custom)
    }
}

impl Serialize for BasePath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ferrohealth_config::error::ConfigError;
    use ferrohealth_config::loader::Loader;

    use super::{BasePath, ServerSection};

    #[derive(Debug, Default, serde::Deserialize)]
    #[serde(default, deny_unknown_fields)]
    struct Config {
        server: ServerSection,
    }

    fn load(text: &str) -> Result<ServerSection, ConfigError> {
        Loader::new("FERROSYS")?
            .load_sources::<Config>(Some(text), None, &BTreeMap::new())
            .map(|loaded| loaded.into_parts().0.server)
    }

    #[test]
    fn the_section_defaults_and_reads_its_keys() {
        assert_eq!(
            ServerSection::default(),
            load("").expect("an empty file loads")
        );
        let section = load(
            "[server]\nlisten = \"0.0.0.0:9090\"\nbase_path = \"/ferrosys\"\nmax_concurrent_requests = 64\n",
        )
        .expect("the section loads");
        assert_eq!("0.0.0.0:9090", section.listen.to_string());
        assert_eq!("/ferrosys", section.base_path.as_str());
        assert_eq!(
            Some(64),
            section
                .max_concurrent_requests
                .map(std::num::NonZeroUsize::get)
        );
        assert_eq!(
            std::time::Duration::from_secs(10),
            section.shutdown_timeout()
        );
    }

    #[test]
    fn a_bad_listen_address_base_path_or_zero_bound_is_refused_at_load() {
        for (text, key) in [
            ("[server]\nlisten = \"not an address\"\n", "server.listen"),
            ("[server]\nbase_path = \"/ferrosys/\"\n", "server.base_path"),
            (
                "[server]\nmax_concurrent_requests = 0\n",
                "server.max_concurrent_requests",
            ),
        ] {
            let error = load(text).expect_err("the value is refused");
            assert!(
                matches!(&error, ConfigError::Invalid { key: Some(found), .. } if found == key),
                "{text}: {error:?}"
            );
        }
    }

    #[test]
    fn a_base_path_is_empty_or_slash_separated_segments() {
        assert!(BasePath::new("").is_ok());
        assert!(BasePath::new("/a/b-c").is_ok());
        for bad in ["a", "/", "/a/", "//a", "/a b", "/a?b", "/a#b", "/ä"] {
            assert!(BasePath::new(bad).is_err(), "{bad:?} is refused");
        }
    }
}
