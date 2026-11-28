// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The `private_key_jwt` client assertion of a request (RFC 7523 §2.2): a
//! fresh one appended to every send's form body, against a server that
//! verifies each and records its `jti`, and the plans that are refused.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

use aws_lc_rs::signature::{
    ECDSA_P256_SHA256_FIXED, ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, UnparsedPublicKey,
};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ferrohealth_perf::credentials::{Credentials, JWT_BEARER};
use ferrohealth_perf::load::{Endpoints, drive};
use ferrohealth_perf::plan::{Plan, PlanError};
use tokio::time::Instant;
use url::Url;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use crate::bindings::write;

const LOAD: &str = "[load]\nrate = 10.0\nwarmup_secs = 0\nduration_secs = 2\n";

/// A token endpoint that verifies every client assertion against the client
/// key and records its `jti`: `200` for a valid one, `401` for any other.
#[derive(Clone)]
struct AssertionChecker {
    public: Vec<u8>,
    jtis: Arc<Mutex<BTreeSet<String>>>,
    refusals: Arc<Mutex<Vec<String>>>,
    // NOTE: no specification governs this: our own design; wiremock names the request URL without its host, so the checker holds the server's base.
    base: String,
}

impl AssertionChecker {
    fn check(&self, request: &Request) -> Result<(), String> {
        let form: Vec<(String, String)> = url::form_urlencoded::parse(&request.body)
            .into_owned()
            .collect();
        let field = |name: &str| {
            form.iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
                .ok_or(format!("no {name}"))
        };
        if field("grant_type")? != "client_credentials" || field("scope")? != "system/Patient.rs" {
            return Err("the request's own fields are lost".to_owned());
        }
        if field("client_assertion_type")? != JWT_BEARER {
            return Err("another client_assertion_type".to_owned());
        }
        let jwt = field("client_assertion")?;
        let [head, claims, signature]: [&str; 3] = jwt
            .split('.')
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|_parts| "not three parts".to_owned())?;
        let decode = |part: &str| -> Result<serde_json::Value, String> {
            let bytes = URL_SAFE_NO_PAD.decode(part).map_err(|e| e.to_string())?;
            serde_json::from_slice(&bytes).map_err(|e| e.to_string())
        };
        let (head, body) = (decode(head)?, decode(claims)?);
        if head["alg"] != "ES256" || head["typ"] != "JWT" || head["kid"] != "perf-1" {
            return Err(format!("header {head}"));
        }
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|e| e.to_string())?;
        let signed = jwt
            .rsplit_once('.')
            .map(|(input, _)| input)
            .unwrap_or_default();
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, &self.public)
            .verify(signed.as_bytes(), &signature)
            .map_err(|_bad| "the signature does not verify".to_owned())?;
        let audience = format!("{}{}", self.base, request.url.path());
        if body["iss"] != "perf-backend"
            || body["sub"] != "perf-backend"
            || body["aud"] != audience.as_str()
        {
            return Err(format!("claims {body} against {audience}"));
        }
        let now = jiff::Timestamp::now().as_second();
        let (iat, exp) = (
            body["iat"].as_i64().unwrap_or_default(),
            body["exp"].as_i64().unwrap_or_default(),
        );
        // NOTE: SMART App Launch 2.2.0, Client Authentication: Asymmetric: exp no more than five minutes ahead.
        if (now - iat).abs() > 5 || exp <= now || exp - iat > 300 {
            return Err(format!("times {body}"));
        }
        let jti = body["jti"].as_str().unwrap_or_default().to_owned();
        if jti.is_empty() || !self.jtis.lock().unwrap().insert(jti) {
            return Err("a jti is missing or spent twice".to_owned());
        }
        Ok(())
    }
}

impl Respond for AssertionChecker {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        match self.check(request) {
            Ok(()) => ResponseTemplate::new(200),
            Err(reason) => {
                self.refusals.lock().unwrap().push(reason.clone());
                ResponseTemplate::new(401).set_body_string(reason)
            }
        }
    }
}

/// A fresh P-256 key: its PKCS#8 PEM and its public point.
fn p256() -> (String, Vec<u8>) {
    let document = EcdsaKeyPair::generate_pkcs8(
        &ECDSA_P256_SHA256_FIXED_SIGNING,
        &aws_lc_rs::rand::SystemRandom::new(),
    )
    .unwrap();
    let pair =
        EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, document.as_ref()).unwrap();
    let pem = format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
        STANDARD.encode(document.as_ref())
    );
    (
        pem,
        aws_lc_rs::signature::KeyPair::public_key(&pair)
            .as_ref()
            .to_vec(),
    )
}

const FORM: &str = "headers = { content-type = \"application/x-www-form-urlencoded\" }\n";

#[tokio::test(flavor = "multi_thread")]
async fn every_send_carries_a_fresh_client_assertion_that_verifies() {
    let server = MockServer::start().await;
    let (pem, public) = p256();
    let checker = AssertionChecker {
        public,
        jtis: Arc::new(Mutex::new(BTreeSet::new())),
        refusals: Arc::new(Mutex::new(Vec::new())),
        base: server.uri(),
    };
    Mock::given(wiremock::matchers::any())
        .respond_with(checker.clone())
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let key = write(dir.path(), "client.pem", &pem);
    let text = format!(
        "{LOAD}[[requests]]\ngroup = \"token\"\nmethod = \"POST\"\npath = \"/token\"\nstatus = 200\n\
         body = 'grant_type=client_credentials&scope=system%2FPatient.rs'\n{FORM}\
         client_assertion = {{ client_id = \"perf-backend\", key_file = '{key}', kid = \"perf-1\" }}\n"
    );
    let plan_path = write(dir.path(), "plan.toml", &text);
    let plan = Plan::read(Path::new(&plan_path)).unwrap();
    let credentials = Credentials::load(&plan).unwrap();
    let outcome = drive(
        &plan,
        &Endpoints::new(&plan, Url::parse(&server.uri()).unwrap(), None).unwrap(),
        &reqwest::Client::new(),
        &credentials,
        Instant::now(),
    )
    .await
    .unwrap();
    assert!(
        outcome.failures.is_empty(),
        "{:?}: {:?}",
        outcome.failures,
        checker.refusals.lock().unwrap().first()
    );
    assert_eq!(20, outcome.sent);
    assert_eq!(
        20,
        checker.jtis.lock().unwrap().len(),
        "a new jti for every send"
    );
}

#[test]
fn a_client_assertion_plan_that_cannot_be_used_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (pem, _) = p256();
    let key = write(dir.path(), "client.pem", &pem);
    let not_a_key = write(dir.path(), "not-a-key.pem", "nothing here\n");
    let request = |headers: &str, table: &str| {
        format!(
            "{LOAD}[[requests]]\ngroup = \"token\"\nmethod = \"POST\"\npath = \"/token\"\nstatus = 200\n\
             body = 'grant_type=client_credentials'\n{headers}client_assertion = {table}\n"
        )
    };
    let read = |text: String| Plan::read(Path::new(&write(dir.path(), "plan.toml", &text)));
    let no_form = request("", &format!("{{ client_id = \"c\", key_file = '{key}' }}"));
    assert!(
        matches!(read(no_form), Err(PlanError::Invalid(reason)) if reason.contains("content-type")),
        "a body that is not a form"
    );
    let no_client = request(FORM, &format!("{{ client_id = \"\", key_file = '{key}' }}"));
    assert!(
        matches!(read(no_client), Err(PlanError::Invalid(_))),
        "no client id"
    );
    for table in [
        format!("{{ client_id = \"c\", key_file = '{not_a_key}' }}"),
        format!(
            "{{ client_id = \"c\", key_file = '{}' }}",
            dir.path().join("absent.pem").display()
        ),
        format!("{{ client_id = \"c\", key_file = '{key}', algorithm = \"HS256\" }}"),
        format!("{{ client_id = \"c\", key_file = '{key}', algorithm = \"RS256\" }}"),
    ] {
        assert!(
            matches!(read(request(FORM, &table)), Err(PlanError::Credential(_))),
            "{table}"
        );
    }
}
