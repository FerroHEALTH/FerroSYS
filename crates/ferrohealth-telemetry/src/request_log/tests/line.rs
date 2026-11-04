// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The request line: what it names, the fields a handler adds, the route
//! namer, the layer and the panic line.

use std::sync::Arc;

use crate::request_log::fields::{FieldError, LineFields, MAX_FIELDS, Token};
use crate::request_log::{RequestId, RequestLog, RouteNamer, UNMATCHED, method_label, middleware};
use crate::section::{Format, TelemetrySection};
use crate::telemetry::{Console, Service, Telemetry};
use axum::Router;
use axum::body::Body;
use axum::routing::get;
use tower::ServiceExt as _;
use tracing_subscriber::layer::SubscriberExt as _;

use super::{Capture, Count, app, get_request, json_lines, request};

#[tokio::test]
async fn the_line_names_the_template_and_the_server_id_and_nothing_a_client_wrote() {
    let capture = Capture::default();
    let section = TelemetrySection {
        format: Format::Json,
        ..TelemetrySection::default()
    };
    let telemetry =
        Telemetry::new(&section, Service::new("ferrosys", "0.0.0")).expect("the telemetry builds");
    let subscriber = tracing_subscriber::registry().with(
        telemetry
            .layers(&section, &Console::new(false, None), capture.clone())
            .expect("the layers build")
            .layers,
    );
    let count = Arc::new(Count::default());
    let log = RequestLog::new(&["_count".to_owned()]).with_recorder(count.clone());
    let _default = tracing::subscriber::set_default(subscriber);
    let response = app(log)
        .oneshot(request())
        .await
        .expect("the request is served");
    assert_eq!(http::StatusCode::OK, response.status());
    let text = capture.text();
    let line: serde_json::Value = serde_json::from_str(text.trim_end()).expect("one JSON line");
    assert_eq!("/fhir/Patient/{id}", line["route"]);
    assert_eq!("GET", line["method"]);
    assert_eq!(200, line["status"]);
    assert_eq!("_count=10", line["query"]);
    assert_eq!("srv-1", line["request_id"]);
    assert_eq!(true, line["client_named"]);
    for leaked in ["999911120", "Qz7query", "Qz7client", "Qz7body", "4bf92f35"] {
        assert!(!text.contains(leaked), "the line carries {leaked}: {text}");
    }
    assert_eq!(
        vec![("/fhir/Patient/{id}".to_owned(), 200)],
        *count.0.lock().expect("the list")
    );
}

#[test]
fn the_fields_a_handler_adds_are_named_fields_of_the_line() {
    let app = |log: RequestLog| {
        Router::new()
            .route(
                "/pix/feed",
                get(|| async {
                    let added = LineFields::new()
                        .with("caller", Token::new("ferropix-test").expect("a token"))
                        .and_then(|added| {
                            added.with(
                                "domain",
                                Token::new("2.16.840.1.113883.2.4.6.3").expect("a token"),
                            )
                        })
                        .expect("the fields are allowed");
                    let mut response = axum::response::IntoResponse::into_response((
                        http::StatusCode::CONFLICT,
                        "Qz7body",
                    ));
                    response.extensions_mut().insert(added);
                    response
                }),
            )
            .layer(axum::middleware::from_fn_with_state(log, middleware))
    };
    let (status, lines) = json_lines(
        RequestLog::default(),
        app,
        get_request("/pix/feed?id=Qz7query"),
    );
    assert_eq!(http::StatusCode::CONFLICT, status);
    let [line] = lines.as_slice() else {
        panic!("one line: {lines:?}");
    };
    assert_eq!("ferropix-test", line["caller"]);
    assert_eq!("2.16.840.1.113883.2.4.6.3", line["domain"]);
    assert_eq!("/pix/feed", line["route"]);
    assert_eq!(409, line["status"]);
    assert_eq!("request", line["message"]);
    assert_eq!("WARN", line["level"], "a client error is a warning");
    assert_eq!("ferrohealth_telemetry::request", line["target"]);
    let text = line.to_string();
    assert!(!text.contains("Qz7"), "{text}");
}

#[test]
fn a_line_field_is_a_lower_case_name_and_a_short_visible_token() {
    let token = || Token::new("x").expect("a token");
    assert_eq!(
        Err(FieldError::Reserved { name: "route" }),
        LineFields::new().with("route", token())
    );
    assert_eq!(
        Err(FieldError::Name { name: "Caller" }),
        LineFields::new().with("Caller", token())
    );
    assert_eq!(
        Err(FieldError::Name { name: "" }),
        LineFields::new().with("", token())
    );
    for value in ["", "two words", "line\nbreak", "é", &"x".repeat(129)] {
        assert_eq!(Err(FieldError::Value), Token::new(value), "{value:?}");
    }
    assert!(Token::new("http://snomed.info/sct").is_ok());
    let names = ["a", "b", "c", "d", "e", "f", "g", "h", "i"];
    let mut full = LineFields::new();
    for name in names.iter().take(MAX_FIELDS) {
        full.insert(name, token()).expect("room for the field");
    }
    assert_eq!(Err(FieldError::Full), full.insert("i", token()));
    full.insert("a", Token::new("again").expect("a token"))
        .expect("a known name is replaced");
    assert_eq!(Some(("a", "again")), full.iter().next());
    assert_eq!(MAX_FIELDS, full.len());
}

struct Operations;

impl RouteNamer for Operations {
    fn route(&self, method: &http::Method, path: &str) -> Option<String> {
        (method == http::Method::GET && path.starts_with("/v1/ehr/"))
            .then(|| "/v1/ehr/{ehr_id}".to_owned())
    }
}

#[test]
fn a_route_namer_names_a_request_no_route_matched() {
    let app = |log: RequestLog| {
        Router::new()
            .route("/known", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(log, middleware))
    };
    let count = Arc::new(Count::default());
    let named = RequestLog::default()
        .with_route_namer(Arc::new(Operations))
        .with_recorder(count.clone());
    let (status, lines) = json_lines(named.clone(), app, get_request("/v1/ehr/999911120"));
    assert_eq!(http::StatusCode::NOT_FOUND, status);
    assert_eq!(
        Some("/v1/ehr/{ehr_id}"),
        lines.first().and_then(|line| line["route"].as_str())
    );
    assert!(!format!("{lines:?}").contains("999911120"));
    let (_, lines) = json_lines(named, app, get_request("/elsewhere"));
    assert_eq!(
        Some(UNMATCHED),
        lines.first().and_then(|line| line["route"].as_str())
    );
    let (_, lines) = json_lines(RequestLog::default(), app, get_request("/v1/ehr/1"));
    assert_eq!(
        Some(UNMATCHED),
        lines.first().and_then(|line| line["route"].as_str())
    );
    assert_eq!(
        vec![("/v1/ehr/{ehr_id}".to_owned(), 404), ("-".to_owned(), 404)],
        *count.0.lock().expect("the list"),
        "the recorders see the named route too"
    );
}

#[test]
fn the_layer_writes_the_line_the_middleware_writes() {
    let routes = || {
        Router::new()
            .route("/fhir/Patient/{id}", get(|| async { "ok" }))
            .route("/broken", get(|| async { http::StatusCode::BAD_GATEWAY }))
    };
    let with_id = |router: Router| {
        router.layer(axum::middleware::map_request(
            |mut request: axum::extract::Request| async move {
                request
                    .extensions_mut()
                    .insert(RequestId::new("srv-1", false));
                request
            },
        ))
    };
    let by_middleware = |log: RequestLog| {
        with_id(routes().layer(axum::middleware::from_fn_with_state(log, middleware)))
    };
    let by_layer = |log: RequestLog| with_id(routes().layer(log));
    let without_latency = |mut lines: Vec<serde_json::Value>| {
        for line in &mut lines {
            if let Some(line) = line.as_object_mut() {
                line.remove("latency_ms");
                line.remove("timestamp");
            }
        }
        lines
    };
    for uri in ["/fhir/Patient/7?_count=3&name=Qz7", "/broken", "/elsewhere"] {
        let count = Arc::new(Count::default());
        let log = RequestLog::new(&["_count".to_owned()]).with_recorder(count.clone());
        let (status, expected) = json_lines(log.clone(), by_middleware, get_request(uri));
        let (got_status, got) = json_lines(log, by_layer, get_request(uri));
        assert_eq!(status, got_status, "{uri}");
        assert_eq!(1, expected.len(), "{uri}: {expected:?}");
        assert_eq!(without_latency(expected), without_latency(got), "{uri}");
        let seen = count.0.lock().expect("the list");
        assert_eq!(2, seen.len(), "{uri}: both feed the recorder");
        assert_eq!(seen.first(), seen.last(), "{uri}");
    }
}

#[test]
fn a_panic_inside_a_request_names_its_server_id_and_nothing_else_of_it() {
    let boom = || {
        Router::new().route(
            "/fhir/Patient/{id}",
            get(|| async {
                let caught = std::panic::catch_unwind(|| {
                    panic!("patient Qz7sentinel has no EHR");
                });
                if caught.is_ok() { "kept" } else { "caught" }
            }),
        )
    };
    let with_id = |router: Router| {
        router.layer(axum::middleware::map_request(
            |mut request: axum::extract::Request| async move {
                request
                    .extensions_mut()
                    .insert(RequestId::new("srv-1", true));
                request
            },
        ))
    };
    let by_middleware = |log: RequestLog| {
        with_id(boom().layer(axum::middleware::from_fn_with_state(log, middleware)))
    };
    let by_layer = |log: RequestLog| with_id(boom().layer(log));
    let without_id = |log: RequestLog| boom().layer(log);
    let panic_line = |lines: &[serde_json::Value]| {
        lines
            .iter()
            .find(|line| line["target"] == crate::panic::TARGET)
            .cloned()
            .expect("a panic line")
    };
    let _hook = crate::panic::HOOK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    crate::panic::install_hook();
    let request = || {
        http::Request::builder()
            .uri("/fhir/Patient/999911120?subject=Qz7query")
            .header("x-request-id", "Qz7client")
            .body(Body::empty())
            .expect("the request builds")
    };
    let apps: [&dyn Fn(RequestLog) -> Router; 2] = [&by_middleware, &by_layer];
    for app in apps {
        let (status, lines) = json_lines(RequestLog::default(), app, request());
        assert_eq!(http::StatusCode::OK, status);
        let line = panic_line(&lines);
        assert_eq!("srv-1", line["request_id"], "{lines:?}");
        let text = format!("{line}");
        for leaked in ["Qz7sentinel", "999911120", "Qz7query", "Qz7client"] {
            assert!(
                !text.contains(leaked),
                "the panic line carries {leaked}: {text}"
            );
        }
    }
    let (_, lines) = json_lines(RequestLog::default(), without_id, request());
    let line = panic_line(&lines);
    assert!(
        line.get("request_id").is_none(),
        "a request with no id names none: {line}"
    );
    let _default = std::panic::take_hook();
}

#[test]
fn a_method_outside_the_known_set_is_other() {
    assert_eq!("GET", method_label(&http::Method::GET));
    assert_eq!(
        "QUERY",
        method_label(&http::Method::from_bytes(b"QUERY").expect("a method"))
    );
    assert_eq!(
        "_OTHER",
        method_label(&http::Method::from_bytes(b"SYNTHETIC").expect("a method"))
    );
}
