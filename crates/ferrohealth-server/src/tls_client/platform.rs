// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The platform's verifier, built once per process for each set of extra
//! roots (#372).
//!
//! On Linux, building a `rustls_platform_verifier::Verifier` reads and
//! parses the whole system root store, which takes tens of milliseconds, so
//! a product that resolves its configuration repeatedly would pay it on
//! every resolve. The system store does not change while the process runs,
//! so each verifier is built on first use and shared. No specification
//! governs this: our own design.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use rustls::crypto::CryptoProvider;
use rustls::pki_types::CertificateDer;
use rustls_platform_verifier::Verifier;

/// A set of extra roots, each in DER, sorted, so two lists of the same
/// roots in another order share one verifier.
type RootSet = Vec<Vec<u8>>;

/// The verifiers built in this process, by the set of extra roots each
/// trusts beside the platform's.
fn built() -> &'static Mutex<BTreeMap<RootSet, Arc<Verifier>>> {
    static BUILT: OnceLock<Mutex<BTreeMap<RootSet, Arc<Verifier>>>> = OnceLock::new();
    BUILT.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// The platform's verifier that trusts `roots` beside the platform's own,
/// over `provider`, built on its first use in this process and shared after.
///
/// The lock is held while a verifier is built, so two callers never both
/// read the system store for the same set.
///
/// # Errors
///
/// Returns the error of `Verifier::new_with_extra_roots` when it refuses an
/// extra root; nothing is kept for that set.
pub(super) fn verifier(
    roots: &[CertificateDer<'static>],
    provider: Arc<CryptoProvider>,
) -> Result<Arc<Verifier>, rustls::Error> {
    let mut key: RootSet = roots.iter().map(|root| root.as_ref().to_vec()).collect();
    key.sort_unstable();
    key.dedup();
    let mut built = built().lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(verifier) = built.get(&key) {
        return Ok(Arc::clone(verifier));
    }
    let verifier = Arc::new(Verifier::new_with_extra_roots(
        roots.iter().cloned(),
        provider,
    )?);
    built.insert(key, Arc::clone(&verifier));
    Ok(verifier)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rustls::pki_types::CertificateDer;

    use super::verifier;

    /// A self-signed root of its own name, in DER.
    fn root(name: &str) -> CertificateDer<'static> {
        let key = rcgen::KeyPair::generate().expect("a key");
        let params = rcgen::CertificateParams::new(vec![name.to_owned()]).expect("the params");
        params
            .self_signed(&key)
            .expect("a certificate")
            .der()
            .clone()
    }

    #[test]
    fn one_set_of_extra_roots_shares_one_verifier_and_another_gets_its_own() {
        let provider = crate::tls::provider();
        let (a, b) = (root("a.example"), root("b.example"));
        let first = verifier(&[a.clone(), b.clone()], Arc::clone(&provider)).expect("a verifier");
        let again = verifier(&[b.clone(), a.clone()], Arc::clone(&provider)).expect("a verifier");
        assert!(
            Arc::ptr_eq(&first, &again),
            "the same roots, in any order, read the system store once"
        );
        let other = verifier(&[a], Arc::clone(&provider)).expect("a verifier");
        assert!(
            !Arc::ptr_eq(&first, &other),
            "another set of roots gets a verifier of its own"
        );
        let none = verifier(&[], Arc::clone(&provider)).expect("a verifier");
        let none_again = verifier(&[], provider).expect("a verifier");
        assert!(
            Arc::ptr_eq(&none, &none_again),
            "no extra roots share one too"
        );
    }
}
