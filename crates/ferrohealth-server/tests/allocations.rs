// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The heap allocations of one request through the stack.

// NOTE: no specification governs this: our own design; `allocation-counter` counts in
// the whole binary's allocator, so this test runs alone in its own binary.
#![cfg(test)]

use axum::Router;
use axum::body::Body;
use axum::routing::get;
use ferrohealth_health::lifecycle::Lifecycle;
use ferrohealth_health::registry::Registry;
use http::StatusCode;
use tower::ServiceExt as _;

use ferrohealth_server::health::Health;
use ferrohealth_server::section::{BasePath, ServerSection};
use ferrohealth_server::stack::{Stack, StackPolicy};

// NOTE: #428; the figure ferrohealth-server 0.12.0 measured for this request.
const CEILING: u64 = 57;

async fn send(app: Router) -> StatusCode {
    let response = app
        .oneshot(
            http::Request::builder()
                .uri("/ferrosys/fhir/Patient/1")
                .body(Body::empty())
                .expect("the request builds"),
        )
        .await
        .expect("the request is served");
    let status = response.status();
    axum::body::to_bytes(response.into_body(), 4096)
        .await
        .expect("the body");
    status
}

#[test]
fn one_admitted_request_allocates_no_more_than_the_ceiling() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("the runtime");
    let lifecycle = Lifecycle::default();
    lifecycle.booted();
    let section = ServerSection {
        base_path: BasePath::new("/ferrosys").expect("a base path"),
        ..ServerSection::default()
    };
    let product = Router::new().route("/fhir/Patient/{id}", get(|| async { "ok" }));
    let app = Stack::new(
        product,
        Health::new(Registry::default(), lifecycle),
        &section,
        StackPolicy::default(),
    )
    .into_router();
    assert_eq!(StatusCode::OK, runtime.block_on(send(app.clone())));
    let app = app.clone();
    let mut status = None;
    let allocations =
        allocation_counter::measure(|| status = Some(runtime.block_on(send(app)))).count_total;
    let status = status.expect("the request ran");
    assert_eq!(StatusCode::OK, status);
    assert!(
        allocations <= CEILING,
        "one request allocated {allocations} times, above the ceiling of {CEILING}"
    );
}
