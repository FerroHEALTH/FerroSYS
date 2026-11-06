// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! A mutual-TLS front whose server certificate a test can revoke.
//!
//! The chain is generated at start: a root CA, an intermediate CA it signs,
//! and the front's server certificate for `127.0.0.1` the intermediate
//! signs. The intermediate names the root's CRL in its CRL distribution
//! point. The server certificate names the one revocation source the
//! [`Source`] a test picks: the intermediate's CRL, the intermediate's OCSP
//! responder in its Authority Information Access extension, either at an
//! address no connection reaches, or none. A status server beside the front
//! serves both CRLs and answers OCSP requests (RFC 6960) signed by the
//! intermediate, with the server certificate's status the [`Standing`] the
//! test picks. Every key is generated at run time and written nowhere. No
//! specification governs the harness: our own design.

use std::collections::BTreeSet;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use der::asn1::{BitString, GeneralizedTime, Ia5String, ObjectIdentifier};
use der::{Decode, Encode};
use rcgen::{
    BasicConstraints, CertificateParams, CertificateRevocationListParams, CrlDistributionPoint,
    CrlIssuingDistributionPoint, CrlScope, CustomExtension, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyIdMethod, KeyPair, RevocationReason, RevokedCertParams, SerialNumber,
    SigningKey as _, date_time_ymd,
};
use rustls::RootCertStore;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::WebPkiClientVerifier;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Respond, ResponseTemplate};
use x509_cert::Certificate;
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::ext::pkix::{AccessDescription, AuthorityInfoAccessSyntax};
use x509_cert::name::Name;
use x509_cert::spki::AlgorithmIdentifierOwned;
use x509_ocsp::{
    BasicOcspResponse, CertStatus, OcspGeneralizedTime, OcspRequest, OcspResponse, ResponderId,
    ResponseData, RevokedInfo, SingleResponse, Version,
};

/// The serial number of the front's server certificate.
const SERVER_SERIAL: u64 = 0x21;

/// `id-pe-authorityInfoAccess` (RFC 5280 §4.2.2.1).
const AIA: [u64; 9] = [1, 3, 6, 1, 5, 5, 7, 1, 1];

/// `id-ad-ocsp` (RFC 5280 §4.2.2.1).
const AD_OCSP: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.48.1");

/// `ecdsa-with-SHA256` (RFC 5758 §3.2), the algorithm of the generated keys.
const ECDSA_WITH_SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.2");

/// The one revocation source the front's server certificate names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Source {
    /// The intermediate's CRL, in the CRL distribution point.
    Crl,
    /// The intermediate's OCSP responder, in the Authority Information
    /// Access extension.
    Ocsp,
    /// The intermediate's CRL, whose `nextUpdate` has passed.
    StaleCrl,
    /// The intermediate's CRL, carrying an issuing distribution point, a
    /// critical extension (RFC 5280 §5.2.5).
    PartitionedCrl,
    /// A CRL distribution point no connection reaches.
    UnreachableCrl,
    /// An OCSP responder no connection reaches.
    UnreachableOcsp,
    /// The intermediate's OCSP responder, whose answer carries trailing
    /// data inside the `ResponseData` of its `BasicOCSPResponse`.
    TrailingOcsp,
    /// No source at all.
    None,
}

/// What the revocation sources say of the front's server certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Standing {
    /// Not revoked.
    Good,
    /// Revoked, for key compromise.
    Revoked,
    /// Unknown to the OCSP responder.
    Unknown,
}

/// A generated CA, its certificate and the issuer that signs with its key.
struct Authority {
    certificate: rcgen::Certificate,
    issuer: Issuer<'static, KeyPair>,
}

/// The root, the intermediate and the client identity the root issues.
pub(super) struct Pki {
    root: Authority,
    intermediate: Authority,
    client_identity: String,
}

impl Pki {
    /// A chain whose intermediate names an unreachable root CRL; enough for
    /// a test that opens no connection.
    pub(super) fn generate() -> Self {
        Self::with("http://127.0.0.1:9")
    }

    /// A chain whose intermediate names the root CRL under `base`.
    fn with(base: &str) -> Self {
        let root_key = KeyPair::generate().expect("a root key");
        let mut root_params = CertificateParams::new(Vec::<String>::new()).expect("parameters");
        root_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        root_params
            .distinguished_name
            .push(DnType::CommonName, "FerroSYS harness root CA");
        let root = Authority {
            certificate: root_params.self_signed(&root_key).expect("the root"),
            issuer: Issuer::new(root_params, root_key),
        };
        let key = KeyPair::generate().expect("an intermediate key");
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("parameters");
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params
            .distinguished_name
            .push(DnType::CommonName, "FerroSYS harness intermediate CA");
        params.serial_number = Some(SerialNumber::from(0x11_u64));
        crl_point(&mut params, &format!("{base}/root.crl"));
        let intermediate = Authority {
            certificate: params
                .signed_by(&key, &root.issuer)
                .expect("the intermediate"),
            issuer: Issuer::new(params, key),
        };
        let client_key = KeyPair::generate().expect("a client key");
        let mut client_params = CertificateParams::new(Vec::<String>::new()).expect("parameters");
        client_params
            .distinguished_name
            .push(DnType::CommonName, "harness client");
        client_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        let client = client_params
            .signed_by(&client_key, &root.issuer)
            .expect("the client certificate");
        Self {
            root,
            intermediate,
            client_identity: format!("{}{}", client.pem(), client_key.serialize_pem()),
        }
    }

    /// The root CA certificate, PEM.
    pub(super) fn root_pem(&self) -> String {
        self.root.certificate.pem()
    }

    /// The client certificate and its key, PEM.
    pub(super) fn client_identity(&self) -> &str {
        &self.client_identity
    }
}

/// A running front over the revocable chain, with its status server.
pub(super) struct Front {
    origin: String,
    roots: String,
    client_identity: String,
    requests: Arc<AtomicUsize>,
    status: MockServer,
}

impl Front {
    /// Starts a front on a free loopback port whose server certificate
    /// names `source` and stands as `standing` says.
    pub(super) async fn start(source: Source, standing: Standing) -> Self {
        let status = MockServer::start().await;
        let base = status.uri();
        let pki = Pki::with(&base);
        let unreachable = unreachable().await;
        let server_key = KeyPair::generate().expect("a server key");
        let mut params =
            CertificateParams::new(vec![Ipv4Addr::LOCALHOST.to_string()]).expect("parameters");
        params
            .distinguished_name
            .push(DnType::CommonName, "harness peer");
        params.serial_number = Some(SerialNumber::from(SERVER_SERIAL));
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        match source {
            Source::Crl | Source::StaleCrl | Source::PartitionedCrl => {
                crl_point(&mut params, &format!("{base}/intermediate.crl"));
            }
            Source::UnreachableCrl => {
                crl_point(&mut params, &format!("{unreachable}/intermediate.crl"));
            }
            Source::Ocsp | Source::TrailingOcsp => {
                ocsp_point(&mut params, &format!("{base}/ocsp"));
            }
            Source::UnreachableOcsp => ocsp_point(&mut params, &format!("{unreachable}/ocsp")),
            Source::None => {}
        }
        let server = params
            .signed_by(&server_key, &pki.intermediate.issuer)
            .expect("the server certificate");

        let revoked = match standing {
            Standing::Revoked => BTreeSet::from([SERVER_SERIAL]),
            Standing::Good | Standing::Unknown => BTreeSet::new(),
        };
        let known = match standing {
            Standing::Unknown => BTreeSet::new(),
            Standing::Good | Standing::Revoked => BTreeSet::from([SERVER_SERIAL]),
        };
        serve_crl(
            &status,
            "/root.crl",
            &pki.root.issuer,
            Crl::new(BTreeSet::new()),
        )
        .await;
        let mut intermediate_crl = Crl::new(revoked.clone());
        match source {
            Source::StaleCrl => intermediate_crl.next_update = 2021,
            Source::PartitionedCrl => intermediate_crl.partitioned = true,
            _ => {}
        }
        serve_crl(
            &status,
            "/intermediate.crl",
            &pki.intermediate.issuer,
            intermediate_crl,
        )
        .await;
        let responder = Responder {
            subject: Certificate::from_der(pki.intermediate.certificate.der())
                .expect("the intermediate decodes")
                .tbs_certificate()
                .subject()
                .clone(),
            issuer: pki.intermediate.issuer,
            known,
            revoked,
            trailing: source == Source::TrailingOcsp,
        };
        Mock::given(method("POST"))
            .and(path("/ocsp"))
            .respond_with(responder)
            .mount(&status)
            .await;

        let chain = vec![
            server.der().clone(),
            pki.intermediate.certificate.der().clone(),
        ];
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(server_key.serialize_der()));
        let requests = Arc::new(AtomicUsize::new(0));
        let origin = listen(chain, key, pki.root.certificate.der(), &requests).await;
        Self {
            origin,
            roots: pki.root.certificate.pem(),
            client_identity: pki.client_identity,
            requests,
            status,
        }
    }

    /// The `https://127.0.0.1:<port>` origin of the front.
    pub(super) fn origin(&self) -> &str {
        &self.origin
    }

    /// The root CA certificate, PEM, a client trusts the front by.
    pub(super) fn trust_roots(&self) -> &str {
        &self.roots
    }

    /// The client certificate and its key, PEM, which the front admits.
    pub(super) fn client_identity(&self) -> &str {
        &self.client_identity
    }

    /// How many requests reached the front over a completed handshake.
    pub(super) fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    /// How many CRL and OCSP requests the status server answered.
    pub(super) async fn status_requests(&self) -> usize {
        self.status
            .received_requests()
            .await
            .expect("the status server records")
            .len()
    }
}

/// Serves TLS over `chain` and `key` on a free loopback port, requiring a
/// client certificate `root` issued, and answers `200 pong` to every
/// request that arrives, counting it in `requests`.
async fn listen(
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    root: &CertificateDer<'static>,
    requests: &Arc<AtomicUsize>,
) -> String {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut roots = RootCertStore::empty();
    roots.add(root.clone()).expect("the root is an anchor");
    let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider.clone())
        .build()
        .expect("the client verifier");
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("the versions")
        .with_client_cert_verifier(verifier)
        .with_single_cert(chain, key)
        .expect("the server configuration");
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("a port");
    let port = listener.local_addr().expect("the address").port();
    let requests = Arc::clone(requests);
    tokio::spawn(async move {
        while let Ok((stream, _peer)) = listener.accept().await {
            let acceptor = acceptor.clone();
            let requests = Arc::clone(&requests);
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(stream).await else {
                    return;
                };
                let mut buffer = [0_u8; 4096];
                if matches!(tls.read(&mut buffer).await, Ok(read) if read > 0) {
                    requests.fetch_add(1, Ordering::SeqCst);
                    let answered = tls
                        .write_all(
                            b"HTTP/1.1 200 OK\r\ncontent-length: 4\r\nconnection: close\r\n\r\npong",
                        )
                        .await;
                    if answered.is_ok() {
                        let _closed = tls.shutdown().await;
                    }
                }
            });
        }
    });
    format!("https://127.0.0.1:{port}")
}

/// An `http://127.0.0.1:<port>` base on a port no listener holds.
async fn unreachable() -> String {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("a port");
    let port = listener.local_addr().expect("the address").port();
    drop(listener);
    format!("http://127.0.0.1:{port}")
}

/// Names `url` as the one CRL distribution point of `params`.
fn crl_point(params: &mut CertificateParams, url: &str) {
    params.crl_distribution_points = vec![CrlDistributionPoint {
        uris: vec![url.to_owned()],
    }];
}

/// Names `url` as the one OCSP responder of `params`, in a non-critical
/// Authority Information Access extension.
fn ocsp_point(params: &mut CertificateParams, url: &str) {
    let access = AuthorityInfoAccessSyntax(vec![AccessDescription {
        access_method: AD_OCSP,
        access_location: GeneralName::UniformResourceIdentifier(
            Ia5String::new(url).expect("an IA5 URL"),
        ),
    }]);
    params
        .custom_extensions
        .push(CustomExtension::from_oid_content(
            &AIA,
            access.to_der().expect("the extension encodes"),
        ));
}

/// The shape of one served CRL.
struct Crl {
    revoked: BTreeSet<u64>,
    /// The year, on 1 January of which, the CRL stops being valid.
    next_update: i32,
    partitioned: bool,
}

impl Crl {
    /// A CRL valid from 2020 to 2099 listing the serial numbers `revoked`.
    fn new(revoked: BTreeSet<u64>) -> Self {
        Self {
            revoked,
            next_update: 2099,
            partitioned: false,
        }
    }
}

/// Serves at `at` the CRL `issuer` signs, shaped as `crl` says.
async fn serve_crl(status: &MockServer, at: &str, issuer: &Issuer<'static, KeyPair>, crl: Crl) {
    let crl = CertificateRevocationListParams {
        this_update: date_time_ymd(2020, 1, 1),
        next_update: date_time_ymd(crl.next_update, 1, 1),
        crl_number: SerialNumber::from(1_u64),
        issuing_distribution_point: crl.partitioned.then(|| CrlIssuingDistributionPoint {
            distribution_point: CrlDistributionPoint {
                uris: vec![format!("{}{at}", status.uri())],
            },
            scope: Some(CrlScope::UserCertsOnly),
        }),
        revoked_certs: crl
            .revoked
            .iter()
            .map(|serial| RevokedCertParams {
                serial_number: SerialNumber::from(*serial),
                revocation_time: date_time_ymd(2021, 1, 1),
                reason_code: Some(RevocationReason::KeyCompromise),
                invalidity_date: None,
            })
            .collect(),
        key_identifier_method: KeyIdMethod::Sha256,
    }
    .signed_by(issuer)
    .expect("the CRL");
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/pkix-crl")
                .set_body_bytes(crl.der().to_vec()),
        )
        .mount(status)
        .await;
}

/// The OCSP responder of the intermediate: it signs each answer with the
/// intermediate's own key, so it is named by the intermediate's subject.
struct Responder {
    subject: Name,
    issuer: Issuer<'static, KeyPair>,
    known: BTreeSet<u64>,
    revoked: BTreeSet<u64>,
    /// Whether the answer carries trailing data inside its `ResponseData`.
    trailing: bool,
}

impl Respond for Responder {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        // NOTE: RFC 6960 §2.3: a request that does not decode is answered
        // malformedRequest; the harness answers a 400, which the check reads as no status.
        self.answer(&request.body).map_or_else(
            || ResponseTemplate::new(400),
            |body| {
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/ocsp-response")
                    .set_body_bytes(body)
            },
        )
    }
}

impl Responder {
    /// The DER of the signed answer to the OCSP request `body`.
    fn answer(&self, body: &[u8]) -> Option<Vec<u8>> {
        let request = OcspRequest::from_der(body).ok()?;
        let now = jiff::Timestamp::now().as_second().unsigned_abs();
        let at = |seconds: u64| {
            GeneralizedTime::from_unix_duration(Duration::from_secs(seconds))
                .map(OcspGeneralizedTime)
                .expect("a time")
        };
        let responses = request
            .tbs_request
            .request_list
            .into_iter()
            .map(|single| {
                let serial = serial_of(single.req_cert.serial_number.as_bytes());
                let cert_status = match serial {
                    Some(serial) if self.revoked.contains(&serial) => {
                        CertStatus::Revoked(RevokedInfo {
                            revocation_time: at(now - 86_400),
                            revocation_reason: None,
                        })
                    }
                    Some(serial) if self.known.contains(&serial) => CertStatus::good(),
                    _ => CertStatus::unknown(),
                };
                SingleResponse {
                    cert_id: single.req_cert,
                    cert_status,
                    this_update: at(now - 60),
                    next_update: Some(at(now + 3_600)),
                    single_extensions: None,
                }
            })
            .collect();
        let data = ResponseData {
            version: Version::V1,
            responder_id: ResponderId::ByName(self.subject.clone()),
            produced_at: at(now),
            responses,
            response_extensions: None,
        };
        let signature = self
            .issuer
            .key()
            .sign(&data.to_der().ok()?)
            .expect("the answer signs");
        if self.trailing {
            return trailing_answer(&data, &signature);
        }
        let basic = BasicOcspResponse {
            tbs_response_data: data,
            signature_algorithm: AlgorithmIdentifierOwned {
                oid: ECDSA_WITH_SHA256,
                parameters: None,
            },
            signature: BitString::from_bytes(&signature).expect("a bit string"),
            certs: None,
        };
        OcspResponse::successful(basic).ok()?.to_der().ok()
    }
}

/// The DER of a successful answer whose `ResponseData` carries a `NULL`
/// after its last field, inside the `BasicOCSPResponse` the `response`
/// OCTET STRING holds (RFC 6960 §4.2.1): nested trailing data, which DER
/// decoding refuses.
fn trailing_answer(data: &ResponseData, signature: &[u8]) -> Option<Vec<u8>> {
    let data = data.to_der().ok()?;
    let mut fields = contents(&data)?.to_vec();
    fields.extend_from_slice(&[0x05, 0x00]);
    let algorithm = AlgorithmIdentifierOwned {
        oid: ECDSA_WITH_SHA256,
        parameters: None,
    }
    .to_der()
    .ok()?;
    let signature = BitString::from_bytes(signature).ok()?.to_der().ok()?;
    let basic = tlv(0x30, &[tlv(0x30, &fields), algorithm, signature].concat());
    let basic_oid = ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.48.1.1")
        .to_der()
        .ok()?;
    let bytes = tlv(0x30, &[basic_oid, tlv(0x04, &basic)].concat());
    let successful = [0x0a, 0x01, 0x00];
    Some(tlv(0x30, &[&successful[..], &tlv(0xa0, &bytes)].concat()))
}

/// The DER of a value with `tag` and `body`, its length in the short or
/// the long form (X.690 §8.1.3).
fn tlv(tag: u8, body: &[u8]) -> Vec<u8> {
    let short = u8::try_from(body.len())
        .ok()
        .filter(|length| *length <= 0x7f);
    let length = short.map_or_else(
        || {
            let significant: Vec<u8> = body
                .len()
                .to_be_bytes()
                .into_iter()
                .skip_while(|byte| *byte == 0)
                .collect();
            let count = u8::try_from(significant.len()).expect("a length of at most 8 bytes");
            [vec![0x80 | count], significant].concat()
        },
        |length| vec![length],
    );
    [vec![tag], length, body.to_vec()].concat()
}

/// The contents of the DER value `der`, without its tag and length.
fn contents(der: &[u8]) -> Option<&[u8]> {
    let first = *der.get(1)?;
    let skip = match first {
        0..=0x7f => 2,
        long => 2 + usize::from(long & 0x7f),
    };
    der.get(skip..)
}

/// The serial number whose big-endian bytes are `bytes`, when it fits a
/// `u64`.
fn serial_of(bytes: &[u8]) -> Option<u64> {
    let bytes = match bytes {
        [0, rest @ ..] => rest,
        all => all,
    };
    (bytes.len() <= 8).then(|| {
        bytes
            .iter()
            .fold(0_u64, |serial, byte| (serial << 8) | u64::from(*byte))
    })
}
