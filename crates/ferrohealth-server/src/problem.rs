// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! Problem details for every error body a product writes outside a FHIR
//! route, as RFC 9457 defines them (<https://www.rfc-editor.org/rfc/rfc9457>).
//!
//! A [`Problem`] carries the members of RFC 9457 §3.1 (`type`, `title`,
//! `status`, `detail`, `instance`) and the extension members of §3.2, and
//! answers as `application/problem+json` ([`MEDIA_TYPE`]) with the status it
//! names. A product defines its problem types once in a [`ProblemTypes`]
//! registry under a base URI it owns and documents, so each `type` resolves
//! to the page that explains it (§4). A problem with no type of its own is
//! [`ProblemType::about_blank`], whose title is the status phrase (§4.2.1).
//!
//! The answer is `application/problem+json` whatever the request's
//! `Accept` names: a problem details document is JSON, so a client that
//! asks for `application/json` reads it, and HTTP lets a server answer a
//! type the client did not list (RFC 9457 §3, RFC 9110 §12.5.1).
//!
//! [`refusal_body`] and [`panic_body`] are the opt-in for the stack's own
//! answers: a product that sets them in its `StackPolicy` writes the `408`,
//! `413`, `429`, `503` and the panic's `500` as problem details. A product
//! that has not moved keeps its own bodies, and `refusal::keep_status`.
//!
//! Neither body carries the request's path, a panic's message or anything
//! a client sent, because a path can name a patient and a message can
//! carry a value or a statement (§5): the detail is a fixed sentence and
//! the `instance` is the server's request id as a `urn:uuid:` URI. What a
//! product writes into its own problems is the product's to vet.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;

use axum::response::{IntoResponse, Response};
use http::{HeaderValue, StatusCode, header};
use serde_json::{Map, Value};

use crate::panic::PanicBody;
use crate::refusal::RefusalBody;

/// The media type of a problem details JSON document (RFC 9457 §3).
pub const MEDIA_TYPE: &str = "application/problem+json";

/// The problem type that adds nothing to the status code, and the value of
/// an absent `type` (RFC 9457 §4.2.1).
pub const ABOUT_BLANK: &str = "about:blank";

/// The members RFC 9457 §3.1 defines, which no extension may take.
const MEMBERS: [&str; 5] = ["type", "title", "status", "detail", "instance"];

/// The detail of a panicked request, the same for every panic.
pub const INTERNAL: &str = "the server could not answer this request";

/// Why a problem type, a registry or an extension member was refused.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProblemError {
    /// The registry's base is not an absolute `http` or `https` URI ending
    /// in `/` or `#`.
    #[error("the problem type base is not an absolute http or https URI ending in / or #")]
    Base,
    /// A slug is not lower-case ASCII letters, digits and `-`, starting with
    /// a letter.
    #[error("the problem type slug {0:?} is not lower-case letters, digits and -")]
    Slug(String),
    /// The registry already defines a type under the slug.
    #[error("the problem type slug {0:?} is defined twice")]
    Duplicate(String),
    /// The status of a problem type is not an error status (`4xx` or `5xx`).
    #[error("the problem type status {0} is not a 4xx or 5xx status")]
    Status(u16),
    /// A title is empty.
    #[error("a problem type title is empty")]
    Title,
    /// An extension member's name does not start with a letter, holds a
    /// character other than a letter, a digit or `_`, or is shorter than
    /// three characters (RFC 9457 §4).
    #[error("the extension member name {0:?} is not three or more letters, digits and _")]
    ExtensionName(String),
    /// An extension member takes the name of a member RFC 9457 §3.1
    /// defines.
    #[error("the extension member name {0:?} is a member RFC 9457 defines")]
    Reserved(String),
    /// An extension member's value does not serialize to JSON.
    #[error("the extension member {name:?} does not serialize to JSON")]
    Value {
        /// The member's name.
        name: String,
        /// Why the value does not serialize.
        #[source]
        source: serde_json::Error,
    },
}

/// A problem type: the URI that identifies it, its title and the status it
/// is used with (RFC 9457 §4).
///
/// A product's types come from its [`ProblemTypes`]; a problem with no
/// semantics beyond its status uses [`ProblemType::about_blank`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProblemType {
    uri: Cow<'static, str>,
    slug: Option<String>,
    title: Option<Cow<'static, str>>,
    status: StatusCode,
}

impl ProblemType {
    /// The `about:blank` type of `status`, titled with its status phrase,
    /// such as `Not Found` for `404` (RFC 9457 §4.2.1).
    ///
    /// A status with no registered phrase has no title.
    #[must_use]
    pub fn about_blank(status: StatusCode) -> Self {
        Self {
            uri: Cow::Borrowed(ABOUT_BLANK),
            slug: None,
            title: status.canonical_reason().map(Cow::Borrowed),
            status,
        }
    }

    /// The URI that identifies the type, the `type` member.
    #[must_use]
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// The slug the registry defined the type under; `None` for
    /// `about:blank`.
    #[must_use]
    pub fn slug(&self) -> Option<&str> {
        self.slug.as_deref()
    }

    /// The short summary of the type, the `title` member.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// The status the type is used with, the `status` member.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// Whether the type is `about:blank`.
    #[must_use]
    pub fn is_about_blank(&self) -> bool {
        self.uri == ABOUT_BLANK
    }

    /// A problem of this type, to which the occurrence's detail and
    /// extension members are added.
    #[must_use]
    pub fn problem(&self) -> Problem {
        Problem::of(self)
    }
}

/// A product's problem types, each under the base URI the product owns, so
/// every `type` resolves to the page that documents it (RFC 9457 §4).
///
/// The type of the slug `aql-syntax` under the base
/// `https://example.org/book/problems.html#` is
/// `https://example.org/book/problems.html#aql-syntax`. The registry refuses
/// a slug defined twice and lists its types in slug order, so a product can
/// draw its documentation and its API description from the one list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProblemTypes {
    base: String,
    types: BTreeMap<String, ProblemType>,
}

impl ProblemTypes {
    /// An empty registry under `base`, an absolute `http` or `https` URI
    /// ending in `/` or `#`.
    ///
    /// # Errors
    ///
    /// [`ProblemError::Base`] when `base` is not such a URI.
    pub fn new(base: impl Into<String>) -> Result<Self, ProblemError> {
        let base = base.into();
        let rest = base
            .strip_prefix("https://")
            .or_else(|| base.strip_prefix("http://"))
            .ok_or(ProblemError::Base)?;
        let host_present = rest.chars().next().is_some_and(|c| c != '/' && c != '#');
        let printable = rest.chars().all(|c| c.is_ascii_graphic());
        let ends = rest.ends_with('/') || rest.ends_with('#');
        let one_fragment = base.matches('#').count() <= 1;
        if host_present && printable && ends && one_fragment {
            Ok(Self {
                base,
                types: BTreeMap::new(),
            })
        } else {
            Err(ProblemError::Base)
        }
    }

    /// The base URI every type of the registry is under.
    #[must_use]
    pub fn base(&self) -> &str {
        &self.base
    }

    /// Defines the type `slug`, titled `title`, used with `status`, and
    /// returns it.
    ///
    /// # Errors
    ///
    /// [`ProblemError::Slug`] when `slug` is not lower-case ASCII letters,
    /// digits and `-` starting with a letter, [`ProblemError::Duplicate`]
    /// when the registry already defines it, [`ProblemError::Title`] for an
    /// empty title and [`ProblemError::Status`] for a status outside `4xx`
    /// and `5xx`.
    pub fn define(
        &mut self,
        slug: &str,
        title: impl Into<Cow<'static, str>>,
        status: StatusCode,
    ) -> Result<ProblemType, ProblemError> {
        let legal = slug.chars().next().is_some_and(|c| c.is_ascii_lowercase())
            && slug
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !legal {
            return Err(ProblemError::Slug(slug.to_owned()));
        }
        if self.types.contains_key(slug) {
            return Err(ProblemError::Duplicate(slug.to_owned()));
        }
        let title = title.into();
        if title.trim().is_empty() {
            return Err(ProblemError::Title);
        }
        if !(status.is_client_error() || status.is_server_error()) {
            return Err(ProblemError::Status(status.as_u16()));
        }
        let defined = ProblemType {
            uri: Cow::Owned(format!("{}{slug}", self.base)),
            slug: Some(slug.to_owned()),
            title: Some(title),
            status,
        };
        self.types.insert(slug.to_owned(), defined.clone());
        Ok(defined)
    }

    /// The type defined under `slug`.
    #[must_use]
    pub fn get(&self, slug: &str) -> Option<&ProblemType> {
        self.types.get(slug)
    }

    /// Every type of the registry, in slug order.
    pub fn iter(&self) -> impl Iterator<Item = &ProblemType> {
        self.types.values()
    }
}

/// One occurrence of a problem: an RFC 9457 problem details object, which
/// answers as `application/problem+json` with its status.
///
/// Its `Debug` rendering names the type, the status and the extension
/// members, never the detail or a member's value, which may carry what a
/// client sent.
#[derive(Clone, PartialEq)]
pub struct Problem {
    kind: ProblemType,
    detail: Option<String>,
    instance: Option<String>,
    extensions: BTreeMap<String, Value>,
}

impl Problem {
    /// An `about:blank` problem of `status`, titled with its status phrase
    /// (RFC 9457 §4.2.1).
    #[must_use]
    pub fn new(status: StatusCode) -> Self {
        Self::of(&ProblemType::about_blank(status))
    }

    /// A problem of `kind`, with its URI, title and status.
    #[must_use]
    pub fn of(kind: &ProblemType) -> Self {
        Self {
            kind: kind.clone(),
            detail: None,
            instance: None,
            extensions: BTreeMap::new(),
        }
    }

    /// Sets the `detail` member: what helps the client correct this
    /// occurrence, never debugging information (RFC 9457 §3.1.4, §5).
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Sets the `instance` member, a URI reference that identifies this
    /// occurrence (RFC 9457 §3.1.5).
    #[must_use]
    pub fn with_instance(mut self, instance: impl Into<String>) -> Self {
        self.instance = Some(instance.into());
        self
    }

    /// Sets the `instance` member to the server's request id as a
    /// `urn:uuid:` URI (RFC 9562 §4), which a client quotes to the operator
    /// and the request log names; an id that is not a UUID sets nothing.
    #[must_use]
    pub fn with_request_id(mut self, request_id: &str) -> Self {
        if let Ok(id) = uuid::Uuid::try_parse(request_id) {
            self.instance = Some(id.urn().to_string());
        }
        self
    }

    /// Adds the extension member `name` with `value` (RFC 9457 §3.2),
    /// replacing one of the same name.
    ///
    /// # Errors
    ///
    /// [`ProblemError::ExtensionName`] when `name` is not three or more
    /// ASCII letters, digits and `_` starting with a letter (RFC 9457 §4),
    /// [`ProblemError::Reserved`] when it is a member §3.1 defines, and
    /// [`ProblemError::Value`] when `value` does not serialize to JSON.
    pub fn with_extension(
        mut self,
        name: &str,
        value: impl serde::Serialize,
    ) -> Result<Self, ProblemError> {
        if MEMBERS.contains(&name) {
            return Err(ProblemError::Reserved(name.to_owned()));
        }
        let legal = name.len() >= 3
            && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !legal {
            return Err(ProblemError::ExtensionName(name.to_owned()));
        }
        let value = serde_json::to_value(value).map_err(|source| ProblemError::Value {
            name: name.to_owned(),
            source,
        })?;
        self.extensions.insert(name.to_owned(), value);
        Ok(self)
    }

    /// The problem's type.
    #[must_use]
    pub fn kind(&self) -> &ProblemType {
        &self.kind
    }

    /// The status the problem answers with, the `status` member.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.kind.status
    }

    /// The `detail` member.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    /// The `instance` member.
    #[must_use]
    pub fn instance(&self) -> Option<&str> {
        self.instance.as_deref()
    }

    /// The extension member `name`.
    #[must_use]
    pub fn extension(&self, name: &str) -> Option<&Value> {
        self.extensions.get(name)
    }

    /// The problem details object, with `type` and `status` always present
    /// and the extension members in name order.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut object = Map::new();
        object.insert("type".to_owned(), Value::from(self.kind.uri()));
        if let Some(title) = self.kind.title() {
            object.insert("title".to_owned(), Value::from(title));
        }
        object.insert("status".to_owned(), Value::from(self.status().as_u16()));
        if let Some(detail) = &self.detail {
            object.insert("detail".to_owned(), Value::from(detail.as_str()));
        }
        if let Some(instance) = &self.instance {
            object.insert("instance".to_owned(), Value::from(instance.as_str()));
        }
        for (name, value) in &self.extensions {
            object.insert(name.clone(), value.clone());
        }
        Value::Object(object)
    }
}

impl std::fmt::Debug for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Problem")
            .field("type", &self.kind.uri())
            .field("status", &self.status().as_u16())
            .field("extensions", &self.extensions.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl serde::Serialize for Problem {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_json().serialize(serializer)
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        // NOTE: RFC 9457 §3.1.2, the response carries the same status as the member.
        let mut response = (self.status(), self.to_json().to_string()).into_response();
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static(MEDIA_TYPE));
        response
    }
}

/// The refusal body that writes the stack's refusals (`408`, `413`, `429`,
/// `503`, and a console's cross-site refusal) as `about:blank` problems:
/// the stack's reason as the detail and the request id as the instance.
///
/// The refusals are conditions of any resource, which RFC 9457 §4 leaves to
/// the status code, so they carry no type of a product's.
#[must_use]
pub fn refusal_body() -> RefusalBody {
    Arc::new(|_uri, request_id, status, reason| {
        Problem::new(status)
            .with_detail(reason)
            .with_request_id(request_id)
            .into_response()
    })
}

/// The panic body that writes a panicked request as an `about:blank` `500`
/// problem with the fixed detail [`INTERNAL`] and the request id as the
/// instance, never the panic's message.
#[must_use]
pub fn panic_body() -> PanicBody {
    Arc::new(|_uri, request_id| {
        Problem::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_detail(INTERNAL)
            .with_request_id(request_id)
            .into_response()
    })
}

#[cfg(test)]
mod tests {
    use axum::response::IntoResponse as _;
    use http::{StatusCode, header};
    use serde_json::json;

    use super::{MEDIA_TYPE, Problem, ProblemError, ProblemType, ProblemTypes};

    fn registry() -> ProblemTypes {
        ProblemTypes::new("https://example.org/book/problems.html#").expect("the base")
    }

    #[test]
    fn an_about_blank_problem_is_titled_with_the_status_phrase() {
        let problem = Problem::new(StatusCode::NOT_FOUND);
        assert_eq!(
            json!({"type": "about:blank", "title": "Not Found", "status": 404}),
            problem.to_json()
        );
        assert!(problem.kind().is_about_blank());
    }

    #[test]
    fn a_status_without_a_phrase_has_no_title() {
        let status = StatusCode::from_u16(499).expect("a status");
        assert_eq!(
            json!({"type": "about:blank", "status": 499}),
            Problem::new(status).to_json()
        );
    }

    #[test]
    fn a_registered_type_resolves_under_the_base() {
        let mut types = registry();
        let syntax = types
            .define(
                "aql-syntax",
                "The AQL query does not parse",
                StatusCode::BAD_REQUEST,
            )
            .expect("the type");
        assert_eq!(
            "https://example.org/book/problems.html#aql-syntax",
            syntax.uri()
        );
        assert_eq!(Some("aql-syntax"), syntax.slug());
        assert_eq!(Some(&syntax), types.get("aql-syntax"));
        let problem = syntax
            .problem()
            .with_detail("the query ends inside a WHERE clause")
            .with_extension("position", json!({"line": 1, "column": 42}))
            .expect("the member");
        assert_eq!(
            json!({
                "type": "https://example.org/book/problems.html#aql-syntax",
                "title": "The AQL query does not parse",
                "status": 400,
                "detail": "the query ends inside a WHERE clause",
                "position": {"line": 1, "column": 42},
            }),
            problem.to_json()
        );
    }

    #[test]
    fn the_registry_lists_its_types_in_slug_order_and_refuses_a_second_definition() {
        let mut types = registry();
        types
            .define(
                "version-conflict",
                "The version is not the latest",
                StatusCode::PRECONDITION_FAILED,
            )
            .expect("the type");
        types
            .define(
                "aql-syntax",
                "The AQL query does not parse",
                StatusCode::BAD_REQUEST,
            )
            .expect("the type");
        assert!(
            matches!(types.define("aql-syntax", "Again", StatusCode::BAD_REQUEST), Err(ProblemError::Duplicate(ref got)) if got == "aql-syntax")
        );
        let slugs: Vec<_> = types.iter().filter_map(ProblemType::slug).collect();
        assert_eq!(vec!["aql-syntax", "version-conflict"], slugs);
    }

    #[test]
    fn a_registry_refuses_an_illegal_base_slug_title_or_status() {
        for base in [
            "ftp://example.org/",
            "https://",
            "https:///problems/",
            "https://example.org/problems",
            "https://example.org/a page/",
            "https://example.org/#a#",
            "/problems/",
        ] {
            assert!(
                matches!(ProblemTypes::new(base), Err(ProblemError::Base)),
                "{base}"
            );
        }
        assert!(ProblemTypes::new("http://localhost:8080/problems/").is_ok());
        let mut types = registry();
        for slug in ["", "Aql", "1aql", "aql_syntax", "aql syntax", "aql#x"] {
            assert!(
                matches!(types.define(slug, "A title", StatusCode::BAD_REQUEST), Err(ProblemError::Slug(ref got)) if got == slug),
                "{slug}"
            );
        }
        assert!(matches!(
            types.define("empty", " ", StatusCode::BAD_REQUEST),
            Err(ProblemError::Title)
        ));
        assert!(matches!(
            types.define("fine", "Fine", StatusCode::OK),
            Err(ProblemError::Status(200))
        ));
    }

    #[test]
    fn an_extension_member_name_follows_rfc_9457_section_4() {
        for name in ["type", "title", "status", "detail", "instance"] {
            assert!(
                matches!(Problem::new(StatusCode::BAD_REQUEST).with_extension(name, 1), Err(ProblemError::Reserved(ref got)) if got == name),
                "{name}"
            );
        }
        for name in ["ab", "1abc", "_abc", "a-bc", "a bc", "caf\u{e9}"] {
            assert!(
                matches!(Problem::new(StatusCode::BAD_REQUEST).with_extension(name, 1), Err(ProblemError::ExtensionName(ref got)) if got == name),
                "{name}"
            );
        }
        let problem = Problem::new(StatusCode::PRECONDITION_FAILED)
            .with_extension("current_version", "8849182c::local.ehrbase.org::2")
            .expect("the member");
        assert_eq!(
            Some(&json!("8849182c::local.ehrbase.org::2")),
            problem.extension("current_version")
        );
    }

    #[test]
    fn a_problem_answers_its_status_as_problem_json() {
        let response = Problem::new(StatusCode::CONFLICT)
            .with_instance("/ehr/1/composition/2")
            .into_response();
        assert_eq!(StatusCode::CONFLICT, response.status());
        assert_eq!(
            Some(MEDIA_TYPE),
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
        );
    }

    #[test]
    fn the_request_id_is_the_instance_as_a_urn() {
        let problem = Problem::new(StatusCode::REQUEST_TIMEOUT)
            .with_request_id("67e55044-10b1-426f-9247-bb680e5fe0c8");
        assert_eq!(
            Some("urn:uuid:67e55044-10b1-426f-9247-bb680e5fe0c8"),
            problem.instance()
        );
        let free_text = Problem::new(StatusCode::REQUEST_TIMEOUT).with_request_id("patient 12");
        assert_eq!(None, free_text.instance());
    }

    #[test]
    fn debug_names_no_detail_and_no_member_value() {
        let problem = Problem::new(StatusCode::UNPROCESSABLE_ENTITY)
            .with_detail("patient Qz7sentinel has no EHR")
            .with_extension("path", "/content[Qz7member]")
            .expect("the member");
        let debug = format!("{problem:?}");
        assert!(!debug.contains("Qz7"), "{debug}");
        assert!(debug.contains("path"), "{debug}");
    }
}
