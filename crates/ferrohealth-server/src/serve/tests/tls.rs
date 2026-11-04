// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The TLS listener: the pinned healthcheck, the reload of the pair the
//! product keeps, and the refusal of a missing or empty file.

use std::time::Duration;

use axum::Router;
use ferrohealth_health::document::Phase;
use ferrohealth_health::lifecycle::Lifecycle;
use ferrohealth_health::registry::Registry;
use tokio::net::TcpListener;

use crate::health::Health;
use crate::healthcheck::{Outcome, check};
use crate::section::ServerSection;
use crate::serve::Drained;
use crate::stack::{StackPolicy, app};

use super::certificate;

#[tokio::test]
async fn a_tls_listener_serves_the_pinned_healthcheck_and_drains() {
    let certificate = certificate();
    let config = std::sync::Arc::new(
        crate::tls::ReloadableTls::new(&certificate.section).expect("the TLS files read"),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("the address");
    let lifecycle = Lifecycle::default();
    let app = app(
        Router::new(),
        Health::new(Registry::default(), lifecycle.clone()),
        &ServerSection::default(),
        StackPolicy::default(),
    );
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let signal = async move {
        let _received = stopped.await;
    };
    let served = tokio::spawn(crate::serve::serve_tls(
        listener,
        app,
        config,
        lifecycle.clone(),
        signal,
        Duration::ZERO,
        Duration::from_secs(5),
    ));
    let pinned = crate::tls::HealthcheckTls::read(&certificate.section).expect("the pin reads");
    assert_eq!(
        Outcome::Healthy,
        crate::healthcheck::check_tls(address, "", Duration::from_secs(5), &pinned).await
    );
    let plain = check(address, "", Duration::from_secs(2)).await;
    assert!(
        !matches!(plain, Outcome::Healthy),
        "plain HTTP to a TLS listener is no answer: {plain:?}"
    );
    let other = crate::tls::HealthcheckTls::read(&certificate_section_of_another())
        .expect("another pin reads");
    let refused = crate::healthcheck::check_tls(address, "", Duration::from_secs(5), &other).await;
    assert!(
        matches!(refused, Outcome::Failed(ref reason) if reason.contains("TLS")),
        "{refused:?}"
    );
    stop.send(()).expect("the server listens for the signal");
    let drained = served
        .await
        .expect("the task ends")
        .expect("the server serves");
    assert_eq!(Drained::Complete, drained);
    assert_eq!(Phase::Draining, lifecycle.phase());
}

fn certificate_section_of_another() -> crate::section::TlsSection {
    let other = certificate();
    let dir = tempfile::tempdir().expect("a directory").keep();
    let certificate_file = dir.join("other.pem");
    std::fs::copy(&other.section.certificate_file, &certificate_file)
        .expect("the certificate is copied");
    crate::section::TlsSection::new(certificate_file, dir.join("unused.key"))
}

#[tokio::test]
async fn a_listener_serves_the_tls_the_product_keeps_and_reloads() {
    let certificate = certificate();
    let kept = std::sync::Arc::new(
        crate::tls::ReloadableTls::of(&certificate.section, crate::tls_keys!("metrics.tls"))
            .expect("the TLS files read"),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("the address");
    let lifecycle = Lifecycle::default();
    lifecycle.booted();
    let app = app(
        Router::new(),
        Health::new(Registry::default(), lifecycle.clone()),
        &ServerSection::default(),
        StackPolicy::default(),
    );
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let signal = async move {
        let _received = stopped.await;
    };
    let section = ServerSection {
        drain_delay_ms: 0,
        ..ServerSection::default()
    };
    let reloaded = std::sync::Arc::clone(&kept);
    let served = tokio::spawn(async move {
        crate::serve::run_all(
            vec![
                crate::serve::Listener::new(listener, app)
                    .with_reloadable_tls(std::sync::Arc::clone(&kept)),
            ],
            &section,
            lifecycle,
            signal,
        )
        .await
    });
    let first = crate::tls::HealthcheckTls::read(&certificate.section).expect("the pin reads");
    assert_eq!(
        Outcome::Healthy,
        crate::healthcheck::check_tls(address, "", Duration::from_secs(5), &first).await
    );
    let renewed = crate::serve::tests::certificate();
    std::fs::copy(
        &renewed.section.certificate_file,
        &certificate.section.certificate_file,
    )
    .expect("the renewed certificate is written");
    std::fs::copy(&renewed.section.key_file, &certificate.section.key_file)
        .expect("the renewed key is written");
    reloaded.reload().expect("the renewed pair reads");
    let second = crate::tls::HealthcheckTls::read(&certificate.section).expect("the pin reads");
    assert_eq!(
        Outcome::Healthy,
        crate::healthcheck::check_tls(address, "", Duration::from_secs(5), &second).await,
        "the next handshake serves the renewed certificate"
    );
    stop.send(()).expect("the server listens for the signal");
    served
        .await
        .expect("the task ends")
        .expect("the server serves");
}

#[test]
fn a_kept_tls_names_its_own_keys_and_a_failed_reload_keeps_the_running_pair() {
    let certificate = certificate();
    let missing = crate::section::TlsSection {
        certificate_file: "/nonexistent/server.pem".into(),
        ..certificate.section.clone()
    };
    let error = crate::tls::ReloadableTls::of(&missing, crate::tls_keys!("metrics.tls"))
        .expect_err("the file is missing");
    assert!(
        error
            .to_string()
            .starts_with("metrics.tls.certificate_file "),
        "{error}"
    );
    assert_eq!(
        "server.tls.key_file",
        crate::tls::TlsKeys::SERVER.key_file()
    );
    let kept = crate::tls::ReloadableTls::of(&certificate.section, crate::tls_keys!("admin.tls"))
        .expect("the TLS files read");
    let running = kept.current();
    std::fs::write(&certificate.section.key_file, "not a key").expect("the key is broken");
    let refused = kept.reload().expect_err("a broken key does not read");
    assert!(refused.to_string().contains("admin.tls.key"), "{refused}");
    assert!(
        std::sync::Arc::ptr_eq(&running, &kept.current()),
        "the running configuration stays"
    );
}

#[test]
fn a_tls_file_that_is_missing_or_holds_no_pem_is_refused() {
    let certificate = certificate();
    let missing = crate::section::TlsSection {
        certificate_file: "/nonexistent/server.pem".into(),
        ..certificate.section.clone()
    };
    assert!(matches!(
        crate::tls::server_config(&missing),
        Err(crate::tls::TlsError::Read { .. })
    ));
    let swapped = crate::section::TlsSection::new(
        certificate.section.key_file.clone(),
        certificate.section.certificate_file.clone(),
    );
    assert!(matches!(
        crate::tls::server_config(&swapped),
        Err(crate::tls::TlsError::Pem { .. })
    ));
}
