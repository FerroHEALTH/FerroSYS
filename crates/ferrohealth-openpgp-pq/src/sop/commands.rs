// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The subcommands of `ferrohealth-sop` (draft-dkg-openpgp-stateless-cli-16
//! §5): each takes its parsed arguments and standard input, and returns
//! what it writes to standard output.

use std::ffi::OsString;
use std::fmt::Write as _;

use ferrohealth_openpgp_pq::certificate::{self, Certificate};
use ferrohealth_openpgp_pq::encoding::{self, Encoding};
use ferrohealth_openpgp_pq::error::Error;
use ferrohealth_openpgp_pq::key::{Key, Signer};
use ferrohealth_openpgp_pq::mode::Mode;
use ferrohealth_openpgp_pq::signature;
use pgp::composed::{Deserializable as _, SignedSecretKey};
use pgp::types::Password;

use crate::sop::date::{self, Bound};
use crate::sop::exit::{Exit, Failure};
use crate::sop::input;

/// The draft this implementation targets, with the `~` of §5.1.1 that
/// marks an incomplete implementation.
const SOP_SPEC: &str = "~draft-dkg-openpgp-stateless-cli-16";
/// The profile of `generate-key`, the only one (§5.1.2, §7.12).
const PROFILE: &str = "default: ML-DSA-65+Ed25519 (RFC 9980 algorithm ID 30), a version 6 signing key with no subkeys; security, performance and compatibility are aliases";
/// The profile names `generate-key` accepts: the default and the reserved
/// aliases of §6.4.
const PROFILE_NAMES: [&str; 4] = ["default", "security", "performance", "compatibility"];

/// The `version` subcommand (§5.1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VersionInfo {
    /// The implementation name and version.
    Plain,
    /// The underlying OpenPGP toolkit.
    Backend,
    /// The name and version, then the toolkit and the draft.
    Extended,
    /// The targeted draft.
    SopSpec,
    /// The sopv version, which this implementation does not cover.
    Sopv,
}

pub(crate) fn version(info: VersionInfo) -> Result<Vec<u8>, Failure> {
    let name = concat!("ferrohealth-sop ", env!("CARGO_PKG_VERSION"));
    let backend = "rPGP (pgp crate) 0.21 with aws-lc-rs 1.19";
    let text = match info {
        VersionInfo::Plain => format!("{name}\n"),
        VersionInfo::Backend => format!("{backend}\n"),
        VersionInfo::Extended => format!(
            "{name}\n{backend}\nML-DSA-65+Ed25519 (RFC 9980) version 6 keys and signatures only\n"
        ),
        VersionInfo::SopSpec => format!(
            "{SOP_SPEC}\n\nThis implementation offers version, list-profiles, generate-key, extract-cert, sign and verify, for ML-DSA-65+Ed25519 keys only. Its keys sign and do not decrypt.\n"
        ),
        VersionInfo::Sopv => {
            return Err(Failure::new(
                Exit::UnsupportedOption,
                "this implementation does not cover a sopv version: it has no inline-verify",
            ));
        }
    };
    Ok(text.into_bytes())
}

/// The `list-profiles` subcommand (§5.1.2).
pub(crate) fn list_profiles(subcommand: &str) -> Result<Vec<u8>, Failure> {
    if subcommand == "generate-key" {
        return Ok(format!("{PROFILE}\n").into_bytes());
    }
    Err(Failure::new(
        Exit::UnsupportedProfile,
        format!("{subcommand} takes no --profile"),
    ))
}

/// The arguments of `generate-key` (§5.2.1).
#[derive(Debug)]
pub(crate) struct GenerateKey {
    pub(crate) encoding: Encoding,
    pub(crate) password: Option<OsString>,
    pub(crate) profile: Option<String>,
    pub(crate) user_ids: Vec<OsString>,
}

pub(crate) fn generate_key(arguments: &GenerateKey) -> Result<Vec<u8>, Failure> {
    if let Some(profile) = &arguments.profile
        && !PROFILE_NAMES.contains(&profile.as_str())
    {
        return Err(Failure::new(
            Exit::UnsupportedProfile,
            format!("generate-key has no profile {profile}"),
        ));
    }
    let mut user_ids = Vec::with_capacity(arguments.user_ids.len());
    for user_id in &arguments.user_ids {
        let Some(text) = user_id.to_str() else {
            return Err(Failure::new(Exit::ExpectedText, "a USERID is not UTF-8"));
        };
        if text.is_empty() {
            return Err(Failure::new(Exit::BadData, "an empty USERID is refused"));
        }
        user_ids.push(text);
    }
    let password = arguments
        .password
        .as_deref()
        .map(|name| input::read(name).and_then(|bytes| input::new_password(&bytes)))
        .transpose()?;
    let mut key =
        Key::generate(&user_ids).map_err(|error| Failure::from_error("generate-key", &error))?;
    if let Some(password) = password {
        key.lock(&Password::from(password.as_str()))
            .map_err(|error| Failure::from_error("generate-key", &error))?;
    }
    key.encode(arguments.encoding)
        .map_err(|error| Failure::from_error("generate-key", &error))
}

/// The `extract-cert` subcommand (§5.2.4): every key on standard input.
pub(crate) fn extract_cert(keys: &[u8], encoding: Encoding) -> Result<Vec<u8>, Failure> {
    let keys = read_keys("KEYS on standard input", keys)?;
    let certificates: Vec<Certificate> = keys.iter().map(|key| key.certificate().clone()).collect();
    certificate::encode_all(&certificates, encoding)
        .map_err(|error| Failure::from_error("extract-cert", &error))
}

/// The arguments of `sign` (§5.4.1).
#[derive(Debug)]
pub(crate) struct Sign {
    pub(crate) encoding: Encoding,
    pub(crate) mode: Mode,
    pub(crate) passwords: Vec<OsString>,
    pub(crate) keys: Vec<OsString>,
}

pub(crate) fn sign(arguments: &Sign, data: &[u8]) -> Result<Vec<u8>, Failure> {
    if arguments.mode == Mode::Text && std::str::from_utf8(data).is_err() {
        return Err(Failure::new(
            Exit::ExpectedText,
            "--as=text needs UTF-8 DATA",
        ));
    }
    let mut passwords = Vec::new();
    for name in &arguments.passwords {
        passwords.extend(input::password_variants(&input::read(name)?));
    }
    let mut signatures = Vec::new();
    for name in &arguments.keys {
        let label = name.to_string_lossy();
        for key in read_keys(&label, &input::read(name)?)? {
            let signer = unlock(&label, &key, &passwords)?;
            let detached = signer
                .sign(data, arguments.mode)
                .map_err(|error| Failure::from_error(&label, &error))?;
            signatures.push(detached);
        }
    }
    encoding::encode(&signatures, arguments.encoding)
        .map_err(|error| Failure::from_error("sign", &error))
}

/// The arguments of `verify` (§5.4.2).
#[derive(Debug)]
pub(crate) struct Verify {
    pub(crate) not_before: Option<String>,
    pub(crate) not_after: Option<String>,
    pub(crate) signatures: OsString,
    pub(crate) certs: Vec<OsString>,
}

pub(crate) fn verify(arguments: &Verify, data: &[u8]) -> Result<Vec<u8>, Failure> {
    let not_before = date::parse(arguments.not_before.as_deref().unwrap_or("-"))?;
    let not_after = date::parse(arguments.not_after.as_deref().unwrap_or("now"))?;
    let signatures = signature::read_all(&input::read(&arguments.signatures)?)
        .map_err(|error| Failure::from_error("SIGNATURES", &error))?;
    let mut reasons = Vec::new();
    let mut certificates = Vec::new();
    for name in &arguments.certs {
        let label = name.to_string_lossy().into_owned();
        let bytes = input::read(name)?;
        if holds_secret_key(&bytes) {
            return Err(Failure::new(
                Exit::BadData,
                format!("{label} holds secret key material where CERTS are expected"),
            ));
        }
        match Certificate::read_all(&bytes) {
            Ok(read) => certificates.extend(
                read.into_iter()
                    .map(|certificate| (label.clone(), certificate)),
            ),
            Err(error @ (Error::Revoked | Error::CannotSign | Error::NoDirectKeySignature)) => {
                reasons.push(format!("{label}: {error}"));
            }
            Err(error) => return Err(Failure::from_error(&label, &error)),
        }
    }

    let mut out = String::new();
    for detached in &signatures {
        let mut signers: Vec<&str> = Vec::new();
        let mut verified = None;
        for (label, certificate) in &certificates {
            match certificate.verify(&detached.signature, data) {
                Ok(verification)
                    if within(verification.created().as_secs(), not_before, not_after) =>
                {
                    if !signers.contains(&label.as_str()) {
                        signers.push(label);
                    }
                    verified.get_or_insert((verification, certificate.fingerprint()));
                }
                Ok(_) => reasons.push(format!(
                    "{label}: the signature lies outside the --not-before and --not-after window"
                )),
                Err(error) => reasons.push(format!("{label}: {}", crate::sop::exit::chain(&error))),
            }
        }
        if let Some((verification, fingerprint)) = verified {
            let mode = match verification.mode() {
                Mode::Binary => "binary",
                Mode::Text => "text",
            };
            let json = serde_json::json!({ "signers": signers });
            writeln!(
                out,
                "{} {fingerprint:X} {fingerprint:X} mode:{mode} {json}",
                date::render(verification.created().as_secs())?
            )
            .map_err(|error| Failure::new(Exit::UnspecifiedFailure, error.to_string()))?;
        }
    }
    if out.is_empty() {
        reasons.insert(0, "no acceptable signature found".to_owned());
        return Err(Failure::new(Exit::NoSignature, reasons.join("\n")));
    }
    Ok(out.into_bytes())
}

/// Whether `seconds` lies in the window (§5.4.2).
fn within(seconds: u32, not_before: Bound, not_after: Bound) -> bool {
    let time = i64::from(seconds);
    let after_start = match not_before {
        Bound::Open => true,
        Bound::At(start) => time >= start.as_second(),
    };
    let before_end = match not_after {
        Bound::Open => true,
        Bound::At(end) => time <= end.as_second(),
    };
    after_start && before_end
}

/// Reads KEYS: a certificate where keys are expected is bad data (§7.3).
fn read_keys(label: &str, bytes: &[u8]) -> Result<Vec<Key>, Failure> {
    Key::read_all(bytes).map_err(|error| match error {
        Error::Missing { .. } | Error::Packet(_) => Failure::new(
            Exit::BadData,
            format!("{label} holds no OpenPGP secret key"),
        ),
        other => Failure::from_error(label, &other),
    })
}

/// Unlocks `key`, trying each password variant on a locked key (§5.4.1).
fn unlock(label: &str, key: &Key, passwords: &[Vec<u8>]) -> Result<Signer, Failure> {
    if !key.is_locked() {
        return key
            .unlock(&Password::empty())
            .map_err(|error| Failure::from_error(label, &error));
    }
    for password in passwords {
        match key.unlock(&Password::from(password.as_slice())) {
            Ok(signer) => return Ok(signer),
            Err(Error::Locked(_)) => {}
            Err(error) => return Err(Failure::from_error(label, &error)),
        }
    }
    Err(Failure::new(
        Exit::KeyIsProtected,
        format!("{label} is locked and no --with-key-password unlocks it"),
    ))
}

/// Whether `bytes` hold a transferable secret key of any algorithm.
fn holds_secret_key(bytes: &[u8]) -> bool {
    // NOTE: a parse failure means the input is no secret key, which is the answer this probe asks for.
    SignedSecretKey::from_reader_many(bytes)
        .is_ok_and(|(mut keys, _headers)| keys.next().is_some_and(|key| key.is_ok()))
}
