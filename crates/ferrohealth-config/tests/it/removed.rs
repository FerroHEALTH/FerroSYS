// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The shared refusal of a retired key, and the outbound TLS section that
//! moved here from ferrohealth-server.

use std::collections::BTreeMap;

use ferrohealth_config::loader::Loader;
use ferrohealth_config::removed::{Removals, Removed, Retired, stale};
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

/// The keys [`Server`] retired, in the release `0.3.0` of this fixture.
const RETIRED: [Retired; 2] = [
    Retired::replaced(
        "server.request_timeout_ms",
        "server.request_timeout, a duration such as `30s`",
        "0.3.0",
    ),
    Retired::removed(
        "server.swagger_ui",
        "the product serves no API browser",
        "0.3.0",
    ),
];

fn removals(tree: &Tree) -> Removals {
    let [request_timeout_ms, swagger_ui] = &RETIRED;
    let mut removals = Removals::new();
    removals.check(tree.server.request_timeout_ms, request_timeout_ms);
    removals.check(tree.server.swagger_ui, swagger_ui);
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
fn a_refusal_is_stale_once_the_version_moves_past_its_release() {
    assert!(
        stale(&RETIRED, "0.3.0").is_empty(),
        "the release that retired them"
    );
    let past: Vec<&str> = stale(&RETIRED, "0.3.1")
        .into_iter()
        .map(Retired::key)
        .collect();
    assert_eq!(vec!["server.request_timeout_ms", "server.swagger_ui"], past);
    assert_eq!(2, stale(&RETIRED, "0.4.0").len(), "a minor release");
    assert_eq!("0.3.0", RETIRED[0].since());
}

#[test]
fn a_product_whose_version_moves_at_the_cut_names_the_release_it_prepares() {
    // The workspace still carries 0.2.9 while 0.3.0, the release that
    // retired the keys, is prepared.
    let workspace = "0.2.9";
    let preparing = "0.3.0";
    assert_eq!(
        2,
        stale(&RETIRED, workspace).len(),
        "the workspace version names the release before"
    );
    assert!(
        stale(&RETIRED, preparing).is_empty(),
        "the release being prepared keeps its refusals"
    );
    assert!(
        stale(&RETIRED, "0.3.0").is_empty(),
        "the release pull request moves the version to the prepared release"
    );
    let next: Vec<&str> = stale(&RETIRED, "0.3.1")
        .into_iter()
        .map(Retired::key)
        .collect();
    assert_eq!(
        vec!["server.request_timeout_ms", "server.swagger_ui"],
        next,
        "the next release prepared after the cut names them"
    );
}

#[test]
fn a_key_of_a_repeated_table_is_named_under_it() {
    let tree = load("[server]\nrequest_timeout_ms = 1\n");
    let mut removals = Removals::new();
    removals.check_under(
        tree.server.request_timeout_ms,
        &Retired::replaced("request_timeout_ms", "request_timeout", "0.3.0"),
        "auth.issuer[0]",
    );
    let refusal = removals.finish().expect_err("set");
    assert_eq!(
        "auth.issuer[0].request_timeout_ms",
        refusal.removals()[0].key()
    );
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

#[test]
fn the_higher_of_two_minimums_is_their_max() {
    assert!(TlsVersion::Tls12 < TlsVersion::Tls13);
    assert_eq!(TlsVersion::Tls13, TlsVersion::Tls12.max(TlsVersion::Tls13));
}
