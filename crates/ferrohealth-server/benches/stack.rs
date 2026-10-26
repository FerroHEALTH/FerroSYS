// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! What a request costs in `stack::app` before a handler does any work.
//!
//! `floor/axum` is a bare router to a handler that answers a constant;
//! `floor/stack` is the same handler inside `stack::app` with the default
//! `[server]` section and the default request log; `floor/stack_unmatched`
//! asks for a path no route matches, so the fallback answers; and
//! `floor/stack_metrics` feeds the request log's duration histogram too. The
//! router is built once and cloned per request because `oneshot` consumes
//! it, as a product's own floor bench calls it.
#![allow(
    clippy::expect_used,
    missing_docs,
    reason = "a benchmark harness fails loud, like a test binary"
)]

use axum::Router;
use axum::body::Body;
use axum::routing::get;
use criterion::{Criterion, criterion_group, criterion_main};
use ferrohealth_health::lifecycle::Lifecycle;
use ferrohealth_health::registry::Registry;
use ferrohealth_server::health::Health;
use ferrohealth_server::section::ServerSection;
use ferrohealth_server::stack::{self, StackPolicy};
use ferrohealth_telemetry::request_log::RequestLog;
use ferrohealth_telemetry::section::TelemetrySection;
use ferrohealth_telemetry::telemetry::{Service, Telemetry};
use tower::ServiceExt as _;

fn product() -> Router {
    Router::new().route("/empty", get(async || ""))
}

fn health() -> Health {
    let lifecycle = Lifecycle::default();
    lifecycle.booted();
    Health::new(Registry::default(), lifecycle)
}

fn floor(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let section = ServerSection::default();
    let bare = product();
    let served = stack::app(product(), health(), &section, StackPolicy::default());
    let telemetry = Telemetry::new(
        &TelemetrySection::default(),
        Service::new("ferrosys", "bench"),
    )
    .expect("the default telemetry builds");
    let metered = stack::app(
        product(),
        health(),
        &section,
        StackPolicy {
            request_log: RequestLog::default().with_metrics(&telemetry.meter("bench")),
            ..StackPolicy::default()
        },
    );
    let call = |router: Router, uri: &str| {
        let request = http::Request::get(uri)
            .body(Body::empty())
            .expect("request");
        runtime.block_on(async {
            let response = router.oneshot(request).await.expect("response");
            axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body")
        })
    };

    let mut group = c.benchmark_group("floor");
    group.bench_function("axum", |b| b.iter(|| call(bare.clone(), "/empty")));
    group.bench_function("stack", |b| b.iter(|| call(served.clone(), "/empty")));
    group.bench_function("stack_unmatched", |b| {
        b.iter(|| call(served.clone(), "/no-such-route"));
    });
    group.bench_function("stack_metrics", |b| {
        b.iter(|| call(metered.clone(), "/empty"));
    });
    group.finish();
}

criterion_group!(benches, floor);
criterion_main!(benches);
