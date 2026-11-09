// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The revocation check against a generated CA chain with a CRL and an OCSP
//! responder: every certificate the peer presents is checked before any
//! data is sent, a revoked one or one whose source cannot be reached fails
//! the connection, and a good one passes (RFC 5280 §6.3, RFC 6960).

mod front;

use std::time::Duration;

use ferrohealth_config::tls::TlsClientSection;
use ferrohealth_config::tls::TlsVersion;
use ferrohealth_secret::secret::Secret;
use ferrohealth_server::revocation::{
    RevocationCheck, RevocationError, RevocationSetupError, StatusError,
};
use ferrohealth_server::tls_client::TlsClient;
use url::Url;

use self::front::{Front, Source, Standing};

/// The budget of one check.
const WITHIN: Duration = Duration::from_secs(5);

/// The check over the front's trust roots, presenting its client identity.
fn check(front: &Front) -> RevocationCheck {
    RevocationCheck::from_pem(
        front.trust_roots(),
        Some(&Secret::new(front.client_identity())),
        TlsVersion::Tls13,
    )
    .expect("the harness material reads")
}

/// The front's `/ping` URL.
fn ping(front: &Front) -> Url {
    Url::parse(&format!("{}/ping", front.origin())).expect("a URL")
}

/// Clears the front and asks it once, returning the answer's status.
async fn cleared_and_asked(front: &Front) -> http::StatusCode {
    let check = check(front);
    let url = ping(front);
    check.clear(&url, WITHIN).await.expect("the chain clears");
    check
        .http_client()
        .expect("the client builds")
        .get(url)
        .send()
        .await
        .expect("the front answers")
        .status()
}

#[tokio::test]
async fn a_good_certificate_by_its_crl_passes() {
    let front = Front::start(Source::Crl, Standing::Good).await;
    assert_eq!(http::StatusCode::OK, cleared_and_asked(&front).await);
    assert_eq!(1, front.requests(), "the request reached the front");
}

#[tokio::test]
async fn a_good_certificate_by_its_ocsp_responder_passes() {
    let front = Front::start(Source::Ocsp, Standing::Good).await;
    assert_eq!(http::StatusCode::OK, cleared_and_asked(&front).await);
    assert_eq!(1, front.requests(), "the request reached the front");
}

#[tokio::test]
async fn every_certificate_of_the_chain_is_checked() {
    let front = Front::start(Source::Crl, Standing::Good).await;
    check(&front)
        .clear(&ping(&front), WITHIN)
        .await
        .expect("the chain clears");
    assert_eq!(
        2,
        front.status_requests().await,
        "the server certificate's CRL and the intermediate's, the root unchecked"
    );
}

#[tokio::test]
async fn a_good_status_is_kept_and_not_asked_again() {
    let front = Front::start(Source::Ocsp, Standing::Good).await;
    let check = check(&front);
    check
        .clear(&ping(&front), WITHIN)
        .await
        .expect("the chain clears");
    check
        .clear(&ping(&front), WITHIN)
        .await
        .expect("the chain clears again");
    assert_eq!(
        2,
        front.status_requests().await,
        "the second check answers from the kept statuses"
    );
}

#[tokio::test]
async fn a_certificate_its_crl_revokes_fails_and_nothing_is_sent() {
    let front = Front::start(Source::Crl, Standing::Revoked).await;
    let checked = check(&front).clear(&ping(&front), WITHIN).await;
    assert!(
        matches!(checked, Err(RevocationError::Revoked { depth: 0 })),
        "a revoked certificate fails: {checked:?}"
    );
    assert_eq!(0, front.requests(), "nothing was sent");
}

#[tokio::test]
async fn a_certificate_its_ocsp_responder_revokes_fails_and_nothing_is_sent() {
    let front = Front::start(Source::Ocsp, Standing::Revoked).await;
    let checked = check(&front).clear(&ping(&front), WITHIN).await;
    assert!(
        matches!(checked, Err(RevocationError::Revoked { depth: 0 })),
        "a revoked certificate fails: {checked:?}"
    );
    assert_eq!(0, front.requests(), "nothing was sent");
}

#[tokio::test]
async fn an_unknown_ocsp_status_fails_closed() {
    let front = Front::start(Source::Ocsp, Standing::Unknown).await;
    let checked = check(&front).clear(&ping(&front), WITHIN).await;
    assert!(
        matches!(checked, Err(RevocationError::Unavailable { depth: 0, .. })),
        "an unknown status fails closed: {checked:?}"
    );
    assert_eq!(0, front.requests(), "nothing was sent");
}

#[tokio::test]
async fn a_crl_past_its_next_update_fails_closed() {
    let front = Front::start(Source::StaleCrl, Standing::Good).await;
    let checked = check(&front).clear(&ping(&front), WITHIN).await;
    assert!(
        matches!(
            checked,
            Err(RevocationError::Unavailable {
                depth: 0,
                source: StatusError::Stale
            })
        ),
        "a stale CRL gives no status: {checked:?}"
    );
    assert_eq!(0, front.requests(), "nothing was sent");
}

#[tokio::test]
async fn a_crl_with_a_critical_extension_it_does_not_process_fails_closed() {
    let front = Front::start(Source::PartitionedCrl, Standing::Good).await;
    let checked = check(&front).clear(&ping(&front), WITHIN).await;
    assert!(
        matches!(
            checked,
            Err(RevocationError::Unavailable {
                depth: 0,
                source: StatusError::Unprocessable
            })
        ),
        "a CRL with an unprocessed critical extension gives no status: {checked:?}"
    );
    assert_eq!(0, front.requests(), "nothing was sent");
}

// NOTE: X.690 §8.1.1, a DER value is exactly its tag, length and contents, so an
// OCSP answer with data after the last field of a nested SEQUENCE does not decode.
#[tokio::test]
async fn an_ocsp_answer_with_nested_trailing_data_fails_closed() {
    let front = Front::start(Source::TrailingOcsp, Standing::Good).await;
    let checked = check(&front).clear(&ping(&front), WITHIN).await;
    assert!(
        matches!(
            &checked,
            Err(RevocationError::Unavailable {
                depth: 0,
                source: StatusError::Decode(error)
            }) if matches!(error.kind(), der::ErrorKind::TrailingData { .. })
        ),
        "an answer with nested trailing data gives no status: {checked:?}"
    );
    assert_eq!(0, front.requests(), "nothing was sent");
}

#[tokio::test]
async fn an_unreachable_crl_fails_closed() {
    let front = Front::start(Source::UnreachableCrl, Standing::Good).await;
    let checked = check(&front).clear(&ping(&front), WITHIN).await;
    assert!(
        matches!(checked, Err(RevocationError::Unavailable { depth: 0, .. })),
        "an unreachable source fails closed: {checked:?}"
    );
    assert_eq!(0, front.requests(), "nothing was sent");
}

#[tokio::test]
async fn an_unreachable_ocsp_responder_fails_closed() {
    let front = Front::start(Source::UnreachableOcsp, Standing::Good).await;
    let checked = check(&front).clear(&ping(&front), WITHIN).await;
    assert!(
        matches!(checked, Err(RevocationError::Unavailable { depth: 0, .. })),
        "an unreachable source fails closed: {checked:?}"
    );
    assert_eq!(0, front.requests(), "nothing was sent");
}

#[tokio::test]
async fn a_certificate_that_names_no_source_fails_closed() {
    let front = Front::start(Source::None, Standing::Good).await;
    let checked = check(&front).clear(&ping(&front), WITHIN).await;
    assert!(
        matches!(checked, Err(RevocationError::NoSource { depth: 0 })),
        "a certificate with no source fails closed: {checked:?}"
    );
    assert_eq!(0, front.requests(), "nothing was sent");
}

#[tokio::test]
async fn the_client_sends_nothing_over_a_chain_no_check_cleared() {
    let front = Front::start(Source::Crl, Standing::Good).await;
    let refused = check(&front)
        .http_client()
        .expect("the client builds")
        .get(ping(&front))
        .send()
        .await;
    assert!(
        refused.is_err(),
        "an unchecked chain is refused: {refused:?}"
    );
    assert_eq!(0, front.requests(), "nothing was sent");
}

#[tokio::test]
async fn a_revoked_chain_stays_refused_by_the_client() {
    let front = Front::start(Source::Ocsp, Standing::Revoked).await;
    let check = check(&front);
    let url = ping(&front);
    let checked = check.clear(&url, WITHIN).await;
    assert!(checked.is_err(), "the chain is revoked: {checked:?}");
    let refused = check
        .http_client()
        .expect("the client builds")
        .get(url)
        .send()
        .await;
    assert!(refused.is_err(), "a revoked chain is refused: {refused:?}");
    assert_eq!(0, front.requests(), "nothing was sent");
}

#[tokio::test]
async fn an_endpoint_that_is_no_https_url_is_refused() {
    let front = Front::start(Source::Crl, Standing::Good).await;
    let plain = Url::parse("http://127.0.0.1:9/ping").expect("a URL");
    let checked = check(&front).clear(&plain, WITHIN).await;
    assert!(
        matches!(checked, Err(RevocationError::Endpoint)),
        "an http endpoint is refused: {checked:?}"
    );
}

/// The front's roots and identity, written to files and read as a table.
fn tls_client(front: &Front, dir: &std::path::Path) -> TlsClient {
    let roots = dir.join("roots.pem");
    let identity = dir.join("identity.pem");
    std::fs::write(&roots, front.trust_roots()).expect("the roots are written");
    std::fs::write(&identity, front.client_identity()).expect("the identity is written");
    let section = TlsClientSection {
        client_identity_file: Some(identity),
        trust_roots_file: Some(roots),
        ..TlsClientSection::default()
    };
    TlsClient::read("xcpd.tls", &section).expect("the material reads")
}

#[tokio::test]
async fn a_check_built_from_a_tls_client_clears_and_presents_its_identity() {
    let front = Front::start(Source::Crl, Standing::Good).await;
    let dir = tempfile::tempdir().expect("a directory");
    let check = RevocationCheck::from_tls_client(&tls_client(&front, dir.path()))
        .expect("the check builds");
    let url = ping(&front);
    check.clear(&url, WITHIN).await.expect("the chain clears");
    let status = check
        .http_client()
        .expect("the client builds")
        .get(url)
        .send()
        .await
        .expect("the front answers")
        .status();
    assert_eq!(
        http::StatusCode::OK,
        status,
        "the front admits the identity"
    );
    assert_eq!(1, front.requests(), "the request reached the front");
}

#[tokio::test]
async fn a_check_built_from_a_tls_client_refuses_a_revoked_chain() {
    let front = Front::start(Source::Ocsp, Standing::Revoked).await;
    let dir = tempfile::tempdir().expect("a directory");
    let check = RevocationCheck::from_tls_client(&tls_client(&front, dir.path()))
        .expect("the check builds");
    assert!(check.clear(&ping(&front), WITHIN).await.is_err());
    assert_eq!(0, front.requests(), "nothing reached the front");
}

#[test]
fn a_tls_client_without_roots_is_refused() {
    let client =
        TlsClient::read("xcpd.tls", &TlsClientSection::default()).expect("nothing to read");
    assert!(matches!(
        RevocationCheck::from_tls_client(&client),
        Err(RevocationSetupError::NoRoots)
    ));
}

#[test]
fn material_that_does_not_read_is_refused() {
    let identity = Secret::new("-----BEGIN PRIVATE KEY-----\nQz7\n-----END PRIVATE KEY-----\n");
    assert!(
        matches!(
            RevocationCheck::from_pem("", Some(&identity), TlsVersion::Tls13),
            Err(RevocationSetupError::NoRoots)
        ),
        "no roots are refused"
    );
}

#[test]
fn debug_never_shows_the_key() {
    let pki = front::Pki::generate();
    let check = RevocationCheck::from_pem(
        &pki.root_pem(),
        Some(&Secret::new(pki.client_identity())),
        TlsVersion::Tls12,
    )
    .expect("the material reads");
    let rendered = format!("{check:?}");
    assert!(
        rendered.contains("roots: 1") && !rendered.contains("PRIVATE KEY"),
        "{rendered}"
    );
}
