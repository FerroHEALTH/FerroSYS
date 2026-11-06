// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The certificates of a presented chain paired with their issuers, and the
//! revocation sources each one names: its CRL distribution points (RFC 5280
//! §4.2.1.13) and its OCSP responders (RFC 5280 §4.2.2.1).

use aws_lc_rs::digest::{SHA256, digest};
use der::asn1::ObjectIdentifier;
use der::oid::AssociatedOid;
use der::{Decode, Encode};
use url::Url;
use x509_cert::Certificate;
use x509_cert::ext::pkix::name::{DistributionPointName, GeneralName};
use x509_cert::ext::pkix::{AuthorityInfoAccessSyntax, CrlDistributionPoints};
use x509_cert::name::Name;

use super::{RevocationError, StatusSource, signature};

/// `id-ad-ocsp` (RFC 5280 §4.2.2.1).
const AD_OCSP: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.48.1");

/// One certificate of a chain and the certificate that issued it.
#[derive(Debug)]
pub(super) struct Link<'a> {
    /// The certificate's position, 0 for the peer's own.
    pub(super) depth: usize,
    /// The certificate.
    pub(super) subject: &'a Certificate,
    /// Its issuer, presented or a trust root.
    pub(super) issuer: &'a Certificate,
}

impl Link<'_> {
    /// The SHA-256 of the certificate and its issuer, the key a status is
    /// kept under.
    pub(super) fn key(&self) -> Result<[u8; 32], der::Error> {
        let mut both = self.subject.to_der()?;
        both.extend(self.issuer.to_der()?);
        let mut key = [0_u8; 32];
        key.copy_from_slice(digest(&SHA256, &both).as_ref());
        Ok(key)
    }
}

/// Every certificate of `presented` from the peer's own up to the trust
/// root, each with its issuer; the root itself is not checked.
pub(super) fn links<'a>(
    presented: &'a [Certificate],
    anchors: &'a [Certificate],
) -> Result<Vec<Link<'a>>, RevocationError> {
    let mut links = Vec::new();
    let mut current = presented.first().ok_or(RevocationError::NoChain)?;
    for depth in 0..=presented.len() {
        if anchors.contains(current) {
            return Ok(links);
        }
        let anchor = anchors.iter().find(|anchor| issued(current, anchor));
        if let Some(issuer) = anchor {
            links.push(Link {
                depth,
                subject: current,
                issuer,
            });
            return Ok(links);
        }
        let issuer = presented
            .iter()
            .filter(|candidate| !std::ptr::eq(*candidate, current))
            .find(|candidate| issued(current, candidate))
            .ok_or(RevocationError::NoIssuer { depth })?;
        links.push(Link {
            depth,
            subject: current,
            issuer,
        });
        current = issuer;
    }
    Err(RevocationError::NoIssuer {
        depth: presented.len(),
    })
}

/// Whether `issuer` names `subject`'s issuer and its key verifies
/// `subject`'s signature.
fn issued(subject: &Certificate, issuer: &Certificate) -> bool {
    // NOTE: RFC 5280 §6.1: a candidate whose name or key does not fit is not the
    // issuer; the chain fails as NoIssuer when none fits.
    same_name(
        subject.tbs_certificate().issuer(),
        issuer.tbs_certificate().subject(),
    ) && signature::verify_certificate(subject, issuer).is_ok()
}

/// Whether `a` and `b` are the same name, compared as DER.
pub(super) fn same_name(a: &Name, b: &Name) -> bool {
    match (a.to_der(), b.to_der()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The CRL distribution points and then the OCSP responders `certificate`
/// names, each an `http` or `https` URL; an `ldap` point is left out.
pub(super) fn sources(certificate: &Certificate) -> Result<Vec<StatusSource>, der::Error> {
    let mut crls = Vec::new();
    let mut responders = Vec::new();
    for extension in certificate
        .tbs_certificate()
        .extensions()
        .into_iter()
        .flatten()
    {
        let value = extension.extn_value.as_bytes();
        if extension.extn_id == CrlDistributionPoints::OID {
            for point in CrlDistributionPoints::from_der(value)?.0 {
                // NOTE: RFC 5280 §6.3.3 (b)(1) and (d): the check reads direct CRLs that
                // cover every reason, so an indirect or partial point is no source here.
                if point.crl_issuer.is_some() || point.reasons.is_some() {
                    continue;
                }
                if let Some(DistributionPointName::FullName(names)) = point.distribution_point {
                    crls.extend(names.iter().filter_map(web).map(StatusSource::Crl));
                }
            }
        } else if extension.extn_id == AuthorityInfoAccessSyntax::OID {
            for access in AuthorityInfoAccessSyntax::from_der(value)?.0 {
                if access.access_method == AD_OCSP
                    && let Some(url) = web(&access.access_location)
                {
                    responders.push(StatusSource::Ocsp(url));
                }
            }
        }
    }
    crls.extend(responders);
    Ok(crls)
}

/// The `http` or `https` URL `name` is, if it is one.
fn web(name: &GeneralName) -> Option<Url> {
    // NOTE: RFC 5280 §4.2.1.13 and §4.2.2.1: no specification governs which schemes
    // are fetched (our own design); a name not http or https is legitimately no source.
    let GeneralName::UniformResourceIdentifier(uri) = name else {
        return None;
    };
    Url::parse(uri.as_str())
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"))
}
