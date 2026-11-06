// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The status a complete, direct CRL gives one certificate (RFC 5280 §5
//! and §6.3.3): issued by the certificate's issuer, signed by its key, with
//! a `nextUpdate` that has not passed.

use der::oid::AssociatedOid;
use der::{Decode, Encode};
use x509_cert::crl::CertificateList;
use x509_cert::ext::pkix::KeyUsage;

use super::chain::{Link, same_name};
use super::{SignatureError, Standing, StatusError, signature};

/// The clock skew, in seconds, a `thisUpdate` in the future is allowed (no
/// specification governs it: our own design).
pub(super) const SKEW: u64 = 60;

/// What the DER CRL `list` says of `link`'s certificate at the Unix second
/// `now`.
pub(super) fn standing(list: &[u8], link: &Link<'_>, now: u64) -> Result<Standing, StatusError> {
    let list = CertificateList::from_der(list).map_err(StatusError::Decode)?;
    let tbs = &list.tbs_cert_list;
    let issuer = link.issuer.tbs_certificate();
    if !same_name(&tbs.issuer, issuer.subject()) || !signs_crls(link)? {
        return Err(StatusError::WrongIssuer);
    }
    if tbs.signature != list.signature_algorithm {
        return Err(StatusError::Signature(SignatureError::Invalid));
    }
    let signed = tbs.to_der().map_err(StatusError::Decode)?;
    let signature = list
        .signature
        .as_bytes()
        .ok_or(StatusError::Signature(SignatureError::Invalid))?;
    signature::verify(
        issuer.subject_public_key_info(),
        &list.signature_algorithm,
        &signed,
        signature,
    )
    .map_err(StatusError::Signature)?;
    // NOTE: RFC 5280 §5.1.2.5: conforming CRL issuers MUST include nextUpdate, and
    // §6.3.3 (a) needs it, so a CRL with no nextUpdate gives no status.
    let next = tbs.next_update.ok_or(StatusError::Stale)?;
    let next = next.to_unix_duration().as_secs();
    let this = tbs.this_update.to_unix_duration().as_secs();
    if this > now.saturating_add(SKEW) || now >= next {
        return Err(StatusError::Stale);
    }
    let critical = tbs
        .crl_extensions
        .iter()
        .flatten()
        .chain(
            tbs.revoked_certificates
                .iter()
                .flatten()
                .flat_map(|entry| entry.crl_entry_extensions.iter().flatten()),
        )
        .any(|extension| extension.critical);
    // NOTE: RFC 5280 §5.2 and §5.3: a CRL with a critical extension the application
    // cannot process MUST NOT be used; this check processes none.
    if critical {
        return Err(StatusError::Unprocessable);
    }
    let serial = link.subject.tbs_certificate().serial_number();
    let revoked = tbs
        .revoked_certificates
        .iter()
        .flatten()
        .any(|entry| entry.serial_number == *serial);
    Ok(if revoked {
        Standing::Revoked
    } else {
        Standing::Good { until: next }
    })
}

/// Whether `link`'s issuer may sign CRLs: a key usage, when it states one,
/// carries `cRLSign` (RFC 5280 §4.2.1.3).
fn signs_crls(link: &Link<'_>) -> Result<bool, StatusError> {
    for extension in link
        .issuer
        .tbs_certificate()
        .extensions()
        .into_iter()
        .flatten()
    {
        if extension.extn_id == KeyUsage::OID {
            let usage =
                KeyUsage::from_der(extension.extn_value.as_bytes()).map_err(StatusError::Decode)?;
            return Ok(usage.crl_sign());
        }
    }
    Ok(true)
}
