// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The `[usage_report]` section, and the CI detection read from the process
//! environment.

use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The collector every product reports to unless configured otherwise.
pub const ENDPOINT: &str = "https://report.ferropulse.eu/v1/report";

/// The variables whose value `true` or `1` marks a CI run, which sends
/// nothing.
pub const CI_VARIABLES: [&str; 2] = ["GITHUB_ACTIONS", "CI"];

/// The `[usage_report]` section.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct UsageReportSection {
    /// The off switch: `false` sends nothing.
    pub enabled: bool,
    /// The collector.
    pub endpoint: Endpoint,
    /// The directory the instance id lives in; without one nothing is sent,
    /// since every restart would count as a new installation.
    pub state_dir: Option<PathBuf>,
    /// How the product is deployed.
    pub deployment: Deployment,
}

impl Default for UsageReportSection {
    fn default() -> Self {
        Self {
            enabled: true,
            endpoint: Endpoint::default(),
            state_dir: None,
            deployment: Deployment::Unknown,
        }
    }
}

/// How the product is deployed, as the envelope names it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Deployment {
    /// The Helm chart.
    Helm,
    /// Docker Compose.
    Compose,
    /// The release binary.
    Binary,
    /// Not stated.
    #[default]
    Unknown,
}

/// The collector's URL, checked when the configuration is read: `https`, or
/// `http` to a loopback host, and never with userinfo.
///
/// # Examples
///
/// ```
/// use ferrohealth_usage_report::section::Endpoint;
///
/// assert!(Endpoint::new("https://report.ferropulse.eu/v1/report").is_ok());
/// assert!(Endpoint::new("http://127.0.0.1:9/v1/report").is_ok());
/// assert!(Endpoint::new("http://report.example/v1/report").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint(url::Url);

/// A collector URL outside the rule.
#[derive(Debug, thiserror::Error)]
#[error("the collector URL is not https, or http to a loopback host, without userinfo")]
pub struct EndpointError;

impl Endpoint {
    /// Returns `url` as the collector.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError`] when `url` does not parse, carries userinfo,
    /// or is neither `https` nor `http` to a loopback host.
    pub fn new(url: &str) -> Result<Self, EndpointError> {
        let url = url::Url::parse(url).map_err(|_unparsed| EndpointError)?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err(EndpointError);
        }
        let admitted = match url.scheme() {
            "https" => url.host().is_some(),
            "http" => match url.host() {
                Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
                Some(url::Host::Ipv4(address)) => address.is_loopback(),
                Some(url::Host::Ipv6(address)) => address.is_loopback(),
                None => false,
            },
            _ => false,
        };
        if admitted {
            Ok(Self(url))
        } else {
            Err(EndpointError)
        }
    }

    /// Returns the URL.
    #[must_use]
    pub const fn url(&self) -> &url::Url {
        &self.0
    }
}

impl Default for Endpoint {
    #[expect(
        clippy::expect_used,
        reason = "ENDPOINT is an https constant the admission rule takes, which a test asserts"
    )]
    fn default() -> Self {
        Self::new(ENDPOINT).expect("the default collector should be admitted")
    }
}

impl<'de> Deserialize<'de> for Endpoint {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let url = String::deserialize(deserializer)?;
        Self::new(&url).map_err(serde::de::Error::custom)
    }
}

impl Serialize for Endpoint {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.0.as_str())
    }
}

/// Returns the CI variable that is set to `true` or `1`, read through
/// `lookup`, `GITHUB_ACTIONS` first.
#[must_use]
pub fn ci_of(lookup: impl Fn(&str) -> Option<String>) -> Option<&'static str> {
    CI_VARIABLES.into_iter().find(|name| {
        lookup(name).is_some_and(|value| {
            let value = value.trim();
            value.eq_ignore_ascii_case("true") || value == "1"
        })
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ferrohealth_config::error::ConfigError;
    use ferrohealth_config::loader::Loader;

    use super::{ENDPOINT, Endpoint, UsageReportSection, ci_of};

    #[derive(Debug, Default, serde::Deserialize)]
    #[serde(default, deny_unknown_fields)]
    struct Config {
        usage_report: UsageReportSection,
    }

    fn load(text: &str) -> Result<UsageReportSection, ConfigError> {
        Loader::new("FERROSYS")?
            .load_sources::<Config>(Some(text), None, &BTreeMap::new())
            .map(|loaded| loaded.into_parts().0.usage_report)
    }

    #[test]
    fn the_section_is_on_by_default_and_reports_to_ferropulse() {
        let section = load("").expect("an empty file loads");
        assert!(section.enabled, "the report is on by default");
        assert_eq!(ENDPOINT, section.endpoint.url().as_str());
        assert!(section.state_dir.is_none());
        let off = load("[usage_report]\nenabled = false\nstate_dir = \"/var/lib/ferrosys\"\n")
            .expect("the section loads");
        assert!(!off.enabled, "the off switch is a key");
    }

    #[test]
    fn a_collector_outside_the_rule_is_refused_at_load() {
        let error = load("[usage_report]\nendpoint = \"http://report.example/v1/report\"\n")
            .expect_err("plain http to a remote host");
        assert!(
            matches!(&error, ConfigError::Invalid { key: Some(key), .. } if key == "usage_report.endpoint"),
            "{error:?}"
        );
    }

    #[test]
    fn only_https_or_loopback_http_without_credentials_is_admitted() {
        for (url, admitted) in [
            ("https://report.ferropulse.eu/v1/report", true),
            ("http://127.0.0.1:9/v1/report", true),
            ("http://[::1]:9/v1/report", true),
            ("http://localhost:9/v1/report", true),
            ("http://report.example/v1/report", false),
            ("https://user:pw@report.example/v1/report", false),
            ("ftp://report.example/", false),
            ("not a url", false),
        ] {
            assert_eq!(admitted, Endpoint::new(url).is_ok(), "{url}");
        }
    }

    #[test]
    fn ci_is_detected_from_either_variable_and_true_or_one_only() {
        let lookup = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            }
        };
        assert_eq!(None, ci_of(lookup(&[])));
        assert_eq!(
            Some("GITHUB_ACTIONS"),
            ci_of(lookup(&[("GITHUB_ACTIONS", "TRUE")]))
        );
        assert_eq!(Some("CI"), ci_of(lookup(&[("CI", "1")])));
        assert_eq!(
            Some("GITHUB_ACTIONS"),
            ci_of(lookup(&[("CI", "true"), ("GITHUB_ACTIONS", "true")]))
        );
        assert_eq!(
            None,
            ci_of(lookup(&[("CI", "false"), ("GITHUB_ACTIONS", "0")]))
        );
    }
}
