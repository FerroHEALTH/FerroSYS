// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The shared refusal of a retired key, and the outbound TLS section that
//! moved here from ferrohealth-server.

use std::collections::BTreeMap;

use ferrohealth_config::loader::Loader;
use ferrohealth_config::removed::{Removals, Removed};
use ferrohealth_config::tls::{TlsClientSection, TlsVersion, TlsVersionError};
use serde::{Deserialize, Serialize};

/// A section with two retired keys beside the one that replaced them.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Server {
    request_timeout: Option<String>,
    request_timeout_ms: Removed,
    swagger_ui: Removed,
}

/// The tree around [`Server`].
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Tree {
    server: Server,
    tls: TlsClientSection,
}

#[expect(
    clippy::expect_used,
    reason = "test fixture over a literal prefix and a file each test writes"
)]
fn load(file: &str) -> Tree {
    Loader::new("REMOVED")
        .expect("the prefix")
        .load_sources::<Tree>(Some(file), None, &BTreeMap::new())
        .expect("a retired key loads, to be refused after")
        .into_parts()
        .0
}

fn removals(tree: &Tree) -> Removals {
    let mut removals = Removals::new();
    removals.replaced(
        tree.server.request_timeout_ms,
        "server.request_timeout_ms",
        "server.request_timeout, a duration such as `30s`",
    );
    removals.retired(
        tree.server.swagger_ui,
        "server.swagger_ui",
        "the product serves no API browser",
    );
    removals
}

#[test]
fn every_retired_key_set_is_refused_at_once_with_its_replacement() {
    let tree = load("[server]\nrequest_timeout_ms = 30000\nswagger_ui = { path = \"/ui\" }\n");
    let refusal = removals(&tree).finish().expect_err("two retired keys");
    assert_eq!(2, refusal.removals().len());
    assert_eq!("server.request_timeout_ms", refusal.removals()[0].key());
    assert_eq!(
        Some("server.request_timeout, a duration such as `30s`"),
        refusal.removals()[0].replacement()
    );
    assert_eq!(None, refusal.removals()[1].replacement());
    assert_eq!(
        "server.request_timeout_ms was replaced by server.request_timeout, a duration such as `30s`\n\
         server.swagger_ui was removed with no replacement: the product serves no API browser",
        refusal.to_string()
    );
}

#[test]
fn a_refusal_never_shows_the_value() {
    let tree = load("[server]\nrequest_timeout_ms = \"Qz7sentinel\"\n");
    let refusal = removals(&tree).finish().expect_err("a retired key");
    assert!(!refusal.to_string().contains("Qz7sentinel"), "{refusal}");
    assert!(!format!("{tree:?}").contains("Qz7sentinel"));
}

#[test]
fn an_unset_retired_key_passes_and_renders_as_absent() {
    let tree = load("[server]\nrequest_timeout = \"30s\"\n");
    let removals = removals(&tree);
    assert!(removals.is_empty());
    assert!(removals.finish().is_ok());
    let rendered = toml::to_string(&tree).expect("the rendering");
    assert!(!rendered.contains("request_timeout_ms"), "{rendered}");
    assert!(!rendered.contains("swagger_ui"), "{rendered}");
}

#[test]
fn removals_gathered_by_sections_join() {
    let tree = load("[server]\nrequest_timeout_ms = 1\n");
    let mut all = Removals::new();
    all.extend(removals(&tree));
    all.extend(removals(&tree));
    assert_eq!(2, all.removals().len());
}

#[test]
fn the_outbound_tls_section_reads_its_keys_and_masks_the_identity() {
    let tree = load(
        "[tls]\nclient_identity = \"Qz7sentinel\"\ntrust_roots_file = \"/etc/roots.pem\"\nmin_version = \"1.2\"\n",
    );
    assert_eq!(TlsVersion::Tls12, tree.tls.min_version);
    assert!(tree.tls.names_material());
    assert!(!format!("{:?}", tree.tls).contains("Qz7sentinel"));
    let rendered = toml::to_string(&tree).expect("the rendering");
    assert!(!rendered.contains("Qz7sentinel"), "{rendered}");
    assert!(!TlsClientSection::default().names_material());
    assert_eq!(TlsVersion::Tls13, TlsClientSection::default().min_version);
}

#[test]
fn a_deprecated_tls_version_is_refused_naming_the_minimum() {
    assert_eq!(
        Err(TlsVersionError::Deprecated("1.1")),
        TlsVersion::try_from("1.1".to_owned())
    );
    let error = Loader::new("REMOVED")
        .expect("the prefix")
        .load_sources::<Tree>(
            Some("[tls]\nmin_version = \"1.0\"\n"),
            None,
            &BTreeMap::new(),
        )
        .expect_err("TLS 1.0");
    let shown = error.to_string();
    assert!(shown.contains("RFC 8996"), "{shown}");
    assert!(shown.contains("`1.2` or `1.3`"), "{shown}");
}
