// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The `[telemetry]` section of a product's configuration.

use std::fmt;
use std::path::PathBuf;

use ferrohealth_secret::secret_url::SecretUrl;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tracing_subscriber::EnvFilter;

/// The filter a product runs with when its configuration names none: this
/// product's lines, with the HTTP stack's own crates at `warn`.
pub const DEFAULT_FILTER: &str = "info,hyper=warn,tower=warn,h2=warn";

/// The `[telemetry]` section.
///
/// Every key has a default, so the section may be left out; with no
/// `[telemetry.otlp]` table nothing is exported.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TelemetrySection {
    /// The console rendering.
    pub format: Format,
    /// The `tracing` filter directive of the console.
    pub filter: Filter,
    /// The query parameters whose values the request log may carry; empty,
    /// it carries none.
    pub logged_query_parameters: Vec<String>,
    /// The deployment environment, `deployment.environment.name` on the
    /// resource, such as `production`.
    pub environment: Option<String>,
    /// The OTLP collector, when spans and metrics are exported.
    pub otlp: Option<OtlpSection>,
}

/// The console rendering a deployment asks for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// `pretty` on a terminal, `json` otherwise, so a container writes
    /// machine lines with no configuration.
    #[default]
    Auto,
    /// One JSON object per line.
    Json,
    /// Lines for a person.
    Pretty,
}

/// The rendering once `auto` is decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rendering {
    /// One JSON object per line.
    Json,
    /// Lines for a person.
    Pretty,
}

impl Format {
    /// Decides `auto` from whether stdout is a terminal, which the caller
    /// passes so a test fixes it.
    #[must_use]
    pub const fn resolve(self, stdout_is_terminal: bool) -> Rendering {
        match self {
            Self::Pretty => Rendering::Pretty,
            Self::Auto if stdout_is_terminal => Rendering::Pretty,
            Self::Json | Self::Auto => Rendering::Json,
        }
    }
}

/// A `tracing` filter directive that parses, checked when the configuration
/// is read.
///
/// A directive the filter parser refuses is refused at load, naming no part
/// of it, so a product never starts with a filter it would have to replace.
///
/// # Examples
///
/// ```
/// use ferrohealth_telemetry::section::Filter;
///
/// assert!(Filter::new("info,ferrosys=debug").is_ok());
/// assert!(Filter::new("info,=,,").is_err());
/// assert_eq!("info,hyper=warn,tower=warn,h2=warn", Filter::default().as_str());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter(String);

/// A filter directive the parser refuses.
#[derive(Debug, thiserror::Error)]
#[error("the log filter does not parse")]
pub struct FilterError {
    #[source]
    source: tracing_subscriber::filter::ParseError,
}

impl Filter {
    /// Returns `directive` as a filter.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError`] when the filter parser refuses it.
    pub fn new(directive: impl Into<String>) -> Result<Self, FilterError> {
        let directive = directive.into();
        EnvFilter::try_new(&directive).map_err(|source| FilterError { source })?;
        Ok(Self(directive))
    }

    /// Returns the directive.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the filter the console applies.
    pub(crate) fn env_filter(&self) -> Result<EnvFilter, FilterError> {
        EnvFilter::try_new(&self.0).map_err(|source| FilterError { source })
    }
}

impl Default for Filter {
    fn default() -> Self {
        Self(DEFAULT_FILTER.to_owned())
    }
}

impl fmt::Display for Filter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Filter {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let directive = String::deserialize(deserializer)?;
        Self::new(directive).map_err(|_refused| {
            serde::de::Error::custom("the log filter does not parse as a tracing directive")
        })
    }
}

impl Serialize for Filter {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

/// The `[telemetry.otlp]` table: where and how spans and metrics are sent.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct OtlpSection {
    /// The collector's OTLP/gRPC endpoint; `https://` speaks TLS. It may
    /// carry a credential, so it renders masked.
    pub endpoint: SecretUrl,
    /// Whether the product's spans are exported.
    pub traces: bool,
    /// Whether the metrics are pushed.
    pub metrics: bool,
    /// The share of the server's traces that are sampled.
    pub sample_ratio: SampleRatio,
    /// A PEM file of trust roots the collector's certificate may chain to,
    /// beside the platform's.
    pub trust_roots_file: Option<PathBuf>,
    /// A PEM file holding the client certificate chain and its key, for
    /// mutual TLS with the collector.
    pub client_identity_file: Option<PathBuf>,
}

impl Default for OtlpSection {
    fn default() -> Self {
        Self {
            endpoint: SecretUrl::default(),
            traces: true,
            metrics: true,
            sample_ratio: SampleRatio::ALL,
            trust_roots_file: None,
            client_identity_file: None,
        }
    }
}

/// The share of traces that are sampled: a finite number from `0.0`, none,
/// to `1.0`, every one.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct SampleRatio(f64);

impl SampleRatio {
    /// Every trace is sampled.
    pub const ALL: Self = Self(1.0);

    /// Returns `ratio` as a sample ratio, or `None` when it is not a finite
    /// number from `0.0` to `1.0`.
    #[must_use]
    pub fn new(ratio: f64) -> Option<Self> {
        (ratio.is_finite() && (0.0..=1.0).contains(&ratio)).then_some(Self(ratio))
    }

    /// Returns the ratio.
    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }
}

impl Default for SampleRatio {
    fn default() -> Self {
        Self::ALL
    }
}

impl PartialEq for SampleRatio {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl<'de> Deserialize<'de> for SampleRatio {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let ratio = f64::deserialize(deserializer)?;
        Self::new(ratio).ok_or_else(|| {
            serde::de::Error::custom("the sample ratio is not a number from 0.0 to 1.0")
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ferrohealth_config::error::ConfigError;
    use ferrohealth_config::loader::Loader;

    use super::{DEFAULT_FILTER, Filter, Format, Rendering, SampleRatio, TelemetrySection};

    #[derive(Debug, Default, serde::Deserialize)]
    #[serde(default, deny_unknown_fields)]
    struct Config {
        telemetry: TelemetrySection,
    }

    fn load(text: &str) -> Result<TelemetrySection, ConfigError> {
        Loader::new("FERROSYS")?
            .load_sources::<Config>(Some(text), None, &BTreeMap::new())
            .map(|loaded| loaded.into_parts().0.telemetry)
    }

    #[test]
    fn auto_follows_the_terminal_and_an_explicit_format_does_not() {
        assert_eq!(Rendering::Pretty, Format::Auto.resolve(true));
        assert_eq!(Rendering::Json, Format::Auto.resolve(false));
        assert_eq!(Rendering::Json, Format::Json.resolve(true));
        assert_eq!(Rendering::Pretty, Format::Pretty.resolve(false));
    }

    #[test]
    fn the_section_defaults_to_auto_the_default_filter_and_no_export() {
        let section = load("").expect("an empty file loads");
        assert_eq!(TelemetrySection::default(), section);
        assert_eq!(DEFAULT_FILTER, section.filter.as_str());
        assert!(section.otlp.is_none(), "nothing is exported by default");
    }

    #[test]
    fn an_invalid_filter_is_refused_at_load_without_quoting_it() {
        let error =
            load("[telemetry]\nfilter = \"info,=,,Qz7\"\n").expect_err("the filter does not parse");
        assert!(
            matches!(&error, ConfigError::Invalid { key: Some(key), .. } if key == "telemetry.filter"),
            "{error:?}"
        );
        assert!(
            !error.to_string().contains("Qz7"),
            "the refusal quotes no part of the directive"
        );
    }

    #[test]
    fn a_valid_filter_loads_as_written() {
        let section =
            load("[telemetry]\nfilter = \"warn,ferrosys=debug\"\n").expect("the filter parses");
        assert_eq!("warn,ferrosys=debug", section.filter.to_string());
        assert_eq!("warn", Filter::new("warn").expect("warn parses").as_str());
    }

    #[test]
    fn the_otlp_table_loads_with_its_defaults_and_masks_its_endpoint() {
        let section =
            load("[telemetry.otlp]\nendpoint = \"https://u:Qz7@otel.example.org:4317\"\n")
                .expect("the table loads");
        let otlp = section.otlp.expect("the table is present");
        assert!(
            otlp.traces && otlp.metrics,
            "both signals are on by default"
        );
        assert_eq!(SampleRatio::ALL, otlp.sample_ratio);
        assert_eq!(
            "https://***@otel.example.org:4317",
            otlp.endpoint.to_string()
        );
    }

    #[test]
    fn a_sample_ratio_outside_zero_to_one_is_refused() {
        for ratio in ["1.5", "-0.1", "nan", "inf"] {
            let error = load(&format!(
                "[telemetry.otlp]\nendpoint = \"http://127.0.0.1:4317\"\nsample_ratio = {ratio}\n"
            ))
            .expect_err("the ratio is out of range");
            assert!(
                matches!(&error, ConfigError::Invalid { key: Some(key), .. } if key == "telemetry.otlp.sample_ratio"),
                "{ratio}: {error:?}"
            );
        }
        assert_eq!(Some(0.25), SampleRatio::new(0.25).map(SampleRatio::get));
    }

    #[test]
    fn the_environment_loads_as_written() {
        let section = load("[telemetry]\nenvironment = \"staging\"\n").expect("the key loads");
        assert_eq!(Some("staging"), section.environment.as_deref());
        assert_eq!(None, load("").expect("an empty file loads").environment);
    }

    #[test]
    fn an_unknown_format_is_refused_with_the_closest_one() {
        let error = load("[telemetry]\nformat = \"jsno\"\n").expect_err("jsno is no format");
        assert!(
            matches!(&error, ConfigError::Invalid { suggestion: Some(suggestion), .. } if suggestion == "json"),
            "{error:?}"
        );
    }
}
