// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! What `TlsClient::platform_config` builds: the configuration, and the
//! revocation check a product clears the endpoints of the table's roots
//! with. No specification governs this: our own design.

use rustls::ClientConfig;

use crate::revocation::RevocationCheck;

/// The platform configuration of one table, its revocation check, and the
/// roots it trusts with no revocation check.
///
/// `Debug` shows whether a check and unchecked roots are present.
pub struct PlatformTls {
    config: ClientConfig,
    check: Option<RevocationCheck>,
    unchecked: Option<(String, String)>,
}

impl std::fmt::Debug for PlatformTls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlatformTls")
            .field("check", &self.check.is_some())
            .field("unchecked", &self.unchecked)
            .finish_non_exhaustive()
    }
}

impl PlatformTls {
    /// The configuration `config`, the check `check` of the roots of
    /// `trust_roots_file`, and the table key and subjects of the roots of
    /// `trust_roots_without_revocation_file`.
    pub(crate) const fn new(
        config: ClientConfig,
        check: Option<RevocationCheck>,
        unchecked: Option<(String, String)>,
    ) -> Self {
        Self {
            config,
            check,
            unchecked,
        }
    }

    /// The rustls configuration.
    #[must_use]
    pub const fn config(&self) -> &ClientConfig {
        &self.config
    }

    /// The rustls configuration, taken.
    #[must_use]
    pub fn into_config(self) -> ClientConfig {
        self.config
    }

    /// The revocation check of the roots of `trust_roots_file`, or `None`
    /// when the table names none. A chain to one of those roots opens a
    /// connection only once this check cleared it, so a product clears every
    /// endpoint that reaches them with [`crate::revocation::Clearing`].
    #[must_use]
    pub const fn check(&self) -> Option<&RevocationCheck> {
        self.check.as_ref()
    }

    /// The line a product's startup banner names the roots trusted with no
    /// revocation check by: the table key and their subjects, or `None` when
    /// the table names none.
    #[must_use]
    pub fn unchecked_roots(&self) -> Option<String> {
        self.unchecked
            .as_ref()
            .map(|(key, names)| format!("{key}: no revocation check for {names}"))
    }

    /// `builder` over this configuration, so reqwest verifies as
    /// [`PlatformTls::config`] does and presents its identity.
    pub fn apply(&self, builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
        builder.tls_backend_preconfigured(self.config.clone())
    }
}
