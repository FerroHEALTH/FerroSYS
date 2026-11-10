// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The loader: the fixtures every test of it shares.

mod arrays;
mod defaults;
mod refusals;
mod sources;

use std::collections::BTreeMap;

use ferrohealth_secret::secret::Secret;

use ferrohealth_config::error::ConfigError;
use ferrohealth_config::loader::Loader;

const SENTINEL: &str = "Qz7sentinel";

#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
    spec_profile: String,
    server: Server,
    telemetry: Telemetry,
    cdr: Option<Cdr>,
    pixm: Option<Pixm>,
    auth: Option<Auth>,
}

#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
struct Pixm {
    manager: Vec<Manager>,
}

#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
struct Auth {
    issuer: Vec<Issuer>,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Issuer {
    url: String,
    audience: Option<String>,
    #[serde(default)]
    timeout_secs: u64,
    client_secret: Option<Secret>,
    client_secret_file: Option<std::path::PathBuf>,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Manager {
    url: String,
    method: Method,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
enum Method {
    Get,
    Post,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
struct Server {
    listen: String,
    body_limit_bytes: usize,
    tls: bool,
    origins: Vec<String>,
}

impl Default for Server {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:8080".to_owned(),
            body_limit_bytes: 1024,
            tls: false,
            origins: Vec::new(),
        }
    }
}

#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
struct Telemetry {
    format: Format,
    filter: String,
}

#[derive(Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
enum Format {
    #[default]
    Auto,
    Json,
    Pretty,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Cdr {
    url: String,
    user: Option<String>,
    password: Option<Secret>,
    password_file: Option<std::path::PathBuf>,
}

#[expect(clippy::expect_used, reason = "test fixture over a literal prefix")]
fn loader() -> Loader {
    Loader::new("FERROSYS")
        .expect("the prefix is valid")
        .alias("RUST_LOG", "telemetry.filter")
        .alias("FERROSYS_LOG_FORMAT", "telemetry.format")
}

fn environment(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

#[expect(clippy::expect_used, reason = "test fixture over literal defaults")]
fn product_defaults() -> Loader {
    let product = Server {
        listen: "0.0.0.0:8080".to_owned(),
        body_limit_bytes: 64 << 20,
        tls: true,
        origins: vec!["https://product.example".to_owned()],
    };
    loader()
        .defaults("server", &product)
        .expect("the defaults serialize")
}

#[expect(
    clippy::panic,
    reason = "a test helper fails its test on a refusal of another kind"
)]
fn refusals(error: &ConfigError) -> Vec<(Option<String>, Option<String>, Option<String>)> {
    let one = |error: &ConfigError| match error {
        ConfigError::Invalid {
            key,
            variable,
            suggestion,
            ..
        } => (key.clone(), variable.clone(), suggestion.clone()),
        other => panic!("an invalid key: {other:?}"),
    };
    match error {
        ConfigError::Refusals { refusals } => refusals.iter().map(one).collect(),
        other => vec![one(other)],
    }
}
