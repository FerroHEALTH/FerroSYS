// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! `ferrohealth-sop` against draft-dkg-openpgp-stateless-cli-16: each
//! subcommand, and each return code of §8 it gives.

use std::ffi::OsStr;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use pgp::composed::{ArmorOptions, KeyType, SecretKeyParamsBuilder};
use pgp::types::KeyVersion;
use rand::rngs::OsRng;

use crate::support::{A3_CERTIFICATE, A3_MESSAGE, A3_SIGNATURE};

/// Runs the binary in `dir` with `args`, `stdin` on standard input and
/// `env` added to its environment.
fn sop_in(dir: &Path, args: &[&OsStr], stdin: &[u8], env: &[(&str, &str)]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ferrohealth-sop"))
        .args(args)
        .current_dir(dir)
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

fn sop_env(dir: &Path, args: &[&str], stdin: &[u8], env: &[(&str, &str)]) -> Output {
    let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    sop_in(dir, &args, stdin, env)
}

fn sop(dir: &Path, args: &[&str], stdin: &[u8]) -> Output {
    sop_env(dir, args, stdin, &[])
}

fn code(output: &Output) -> i32 {
    output.status.code().unwrap()
}

fn ok(output: Output) -> Vec<u8> {
    assert_eq!(
        code(&output),
        0,
        "expected success, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "nothing on standard error on success (§8)"
    );
    output.stdout
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// A generated key and its certificate, as files in `dir`.
fn alice(dir: &Path) -> (String, String) {
    let key = ok(sop(
        dir,
        &["generate-key", "Alice <alice@example.org>"],
        b"",
    ));
    write(dir, "alice.key", &key);
    let cert = ok(sop(dir, &["extract-cert"], &key));
    write(dir, "alice.cert", &cert);
    ("alice.key".into(), "alice.cert".into())
}

#[test]
fn version_answers_each_form_of_section_5_1_1() {
    let dir = tempfile::tempdir().unwrap();
    let plain = String::from_utf8(ok(sop(dir.path(), &["version"], b""))).unwrap();
    assert_eq!(
        plain,
        concat!("ferrohealth-sop ", env!("CARGO_PKG_VERSION"), "\n"),
        "name and version"
    );
    let spec = String::from_utf8(ok(sop(dir.path(), &["version", "--sop-spec"], b""))).unwrap();
    assert!(
        spec.starts_with("~draft-dkg-openpgp-stateless-cli-16\n"),
        "targeted draft, marked incomplete"
    );
    let extended = String::from_utf8(ok(sop(dir.path(), &["version", "--extended"], b""))).unwrap();
    assert!(
        extended.starts_with(&plain),
        "the first line matches the plain form"
    );
    ok(sop(dir.path(), &["version", "--backend"], b""));
    let sopv = sop(dir.path(), &["version", "--sopv"], b"");
    assert_eq!(code(&sopv), 37, "no sopv coverage: UNSUPPORTED_OPTION");
    assert!(sopv.stdout.is_empty(), "nothing on standard output");
    assert_eq!(
        code(&sop(
            dir.path(),
            &["version", "--backend", "--extended"],
            b""
        )),
        83,
        "INCOMPATIBLE_OPTIONS"
    );
}

#[test]
fn the_command_line_is_refused_with_the_section_8_codes() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        code(&sop(dir.path(), &["encrypt"], b"")),
        69,
        "UNSUPPORTED_SUBCOMMAND"
    );
    assert_eq!(
        code(&sop(dir.path(), &["sign", "--micalg-out=x", "k"], b"")),
        37,
        "UNSUPPORTED_OPTION"
    );
    assert_eq!(
        code(&sop(dir.path(), &["sign"], b"")),
        19,
        "MISSING_ARG: no KEYS"
    );
    assert_eq!(
        code(&sop(dir.path(), &["verify", "sig"], b"")),
        19,
        "MISSING_ARG: no CERTS"
    );
    assert_eq!(
        code(&sop(dir.path(), &["sign", "absent.key"], b"")),
        61,
        "MISSING_INPUT"
    );
}

#[test]
fn list_profiles_names_the_one_generate_key_profile() {
    let dir = tempfile::tempdir().unwrap();
    let profiles =
        String::from_utf8(ok(sop(dir.path(), &["list-profiles", "generate-key"], b""))).unwrap();
    assert_eq!(profiles.lines().count(), 1, "one profile");
    assert!(
        profiles.starts_with("default: ML-DSA-65+Ed25519"),
        "the default"
    );
    assert_eq!(
        code(&sop(dir.path(), &["list-profiles", "sign"], b"")),
        89,
        "UNSUPPORTED_PROFILE"
    );
}

#[test]
fn generate_key_makes_an_armored_key_and_checks_its_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let key = String::from_utf8(ok(sop(
        dir.path(),
        &["generate-key", "--signing-only", "A <a@example.org>"],
        b"",
    )))
    .unwrap();
    assert!(
        key.starts_with("-----BEGIN PGP PRIVATE KEY BLOCK-----"),
        "armored by default (§5)"
    );
    let binary = ok(sop(dir.path(), &["generate-key", "--no-armor"], b""));
    assert!(!binary.starts_with(b"-----"), "--no-armor");
    for alias in ["default", "security", "performance", "compatibility"] {
        ok(sop(
            dir.path(),
            &["generate-key", &format!("--profile={alias}")],
            b"",
        ));
    }
    assert_eq!(
        code(&sop(
            dir.path(),
            &["generate-key", "--profile=rfc4880"],
            b""
        )),
        89,
        "UNSUPPORTED_PROFILE"
    );
    assert_eq!(
        code(&sop(dir.path(), &["generate-key", ""], b"")),
        41,
        "an empty USERID: BAD_DATA"
    );
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let not_utf8 = OsStr::from_bytes(b"\xff\xfe");
        let output = sop_in(
            dir.path(),
            &[OsStr::new("generate-key"), not_utf8],
            b"",
            &[],
        );
        assert_eq!(code(&output), 53, "EXPECTED_TEXT");
    }
}

#[test]
fn extract_cert_writes_certificates_and_refuses_a_certificate_as_keys() {
    let dir = tempfile::tempdir().unwrap();
    let (_, cert) = alice(dir.path());
    let cert = std::fs::read_to_string(dir.path().join(cert)).unwrap();
    assert!(
        cert.starts_with("-----BEGIN PGP PUBLIC KEY BLOCK-----"),
        "a certificate"
    );
    assert_eq!(
        code(&sop(dir.path(), &["extract-cert"], cert.as_bytes())),
        41,
        "BAD_DATA"
    );
}

#[test]
fn sign_then_verify_round_trips_and_a_changed_message_has_no_signature() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = alice(dir.path());
    let signature = ok(sop(dir.path(), &["sign", &key], b"message"));
    assert!(
        signature.starts_with(b"-----BEGIN PGP SIGNATURE-----"),
        "armored"
    );
    write(dir.path(), "message.sig", &signature);

    let verified = String::from_utf8(ok(sop(
        dir.path(),
        &["verify", "message.sig", &cert],
        b"message",
    )))
    .unwrap();
    let fields: Vec<&str> = verified.trim_end().splitn(5, ' ').collect();
    assert_eq!(fields.len(), 5, "four fields and the JSON (§7.10)");
    assert!(
        fields[0].ends_with('Z') && fields[0].len() == 20,
        "UTC to the second: {}",
        fields[0]
    );
    assert_eq!(fields[1].len(), 64, "the v6 fingerprint in hex");
    assert_eq!(fields[1], fields[1].to_uppercase(), "upper-case hex");
    assert_eq!(fields[1], fields[2], "signed by the primary key");
    assert_eq!(fields[3], "mode:binary", "binary by default");
    assert_eq!(
        fields[4], r#"{"signers":["alice.cert"]}"#,
        "the CERTS argument"
    );

    let changed = sop(dir.path(), &["verify", "message.sig", &cert], b"massage");
    assert_eq!(code(&changed), 3, "NO_SIGNATURE");
    assert!(changed.stdout.is_empty(), "no VERIFICATIONS");
    assert!(
        !changed.stderr.is_empty(),
        "a reason on standard error (§13.1.1)"
    );
}

#[test]
fn text_mode_needs_utf8_and_verifies_across_line_endings() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = alice(dir.path());
    assert_eq!(
        code(&sop(dir.path(), &["sign", "--as=text", &key], b"\xff")),
        53,
        "EXPECTED_TEXT"
    );
    let signature = ok(sop(dir.path(), &["sign", "--as=text", &key], b"one\ntwo\n"));
    write(dir.path(), "text.sig", &signature);
    let verified = String::from_utf8(ok(sop(
        dir.path(),
        &["verify", "text.sig", &cert],
        b"one\r\ntwo\r\n",
    )))
    .unwrap();
    assert!(verified.contains(" mode:text "), "text mode");
}

#[test]
fn the_verification_window_bounds_the_signatures() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = alice(dir.path());
    write(
        dir.path(),
        "m.sig",
        &ok(sop(dir.path(), &["sign", &key], b"m")),
    );
    ok(sop(
        dir.path(),
        &[
            "verify",
            "--not-before=-",
            "--not-after=20991231T000000Z",
            "m.sig",
            &cert,
        ],
        b"m",
    ));
    let future = sop(
        dir.path(),
        &[
            "verify",
            "--not-before=2099-01-01T00:00:00Z",
            "m.sig",
            &cert,
        ],
        b"m",
    );
    assert_eq!(code(&future), 3, "made before --not-before");
    let past = sop(
        dir.path(),
        &[
            "verify",
            "--not-after=2000-01-01T00:00:00+00:00",
            "m.sig",
            &cert,
        ],
        b"m",
    );
    assert_eq!(code(&past), 3, "made after --not-after");
    assert_eq!(
        code(&sop(
            dir.path(),
            &["verify", "--not-after=yesterday", "m.sig", &cert],
            b"m"
        )),
        41,
        "BAD_DATA"
    );
}

#[test]
fn certs_with_secret_material_or_another_algorithm_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (key, _) = alice(dir.path());
    write(
        dir.path(),
        "m.sig",
        &ok(sop(dir.path(), &["sign", &key], b"m")),
    );
    assert_eq!(
        code(&sop(dir.path(), &["verify", "m.sig", &key], b"m")),
        41,
        "a key as CERTS: BAD_DATA (§7.2)"
    );

    let mut params = SecretKeyParamsBuilder::default();
    params
        .key_type(KeyType::Ed25519)
        .version(KeyVersion::V6)
        .can_sign(true)
        .primary_user_id("Classical <c@example.org>".into());
    let classical = params.build().unwrap().generate(OsRng).unwrap();
    let armored = classical
        .to_public_key()
        .to_armored_string(ArmorOptions::default())
        .unwrap();
    write(dir.path(), "classical.cert", armored.as_bytes());
    assert_eq!(
        code(&sop(
            dir.path(),
            &["verify", "m.sig", "classical.cert"],
            b"m"
        )),
        13,
        "UNSUPPORTED_ASYMMETRIC_ALGO"
    );
    let secret = classical
        .to_armored_string(ArmorOptions::default())
        .unwrap();
    write(dir.path(), "classical.key", secret.as_bytes());
    assert_eq!(
        code(&sop(dir.path(), &["sign", "classical.key"], b"m")),
        13,
        "a classical key cannot sign here"
    );
    assert_eq!(
        code(&sop(dir.path(), &["sign", "m.sig"], b"m")),
        41,
        "a signature as KEYS: BAD_DATA"
    );
}

#[test]
fn a_locked_key_signs_only_with_its_password() {
    let dir = tempfile::tempdir().unwrap();
    let key = ok(sop_env(
        dir.path(),
        &[
            "generate-key",
            "--with-key-password=@ENV:KEY_PASSWORD",
            "Locked <l@example.org>",
        ],
        b"",
        &[("KEY_PASSWORD", "correct horse\n")],
    ));
    write(dir.path(), "locked.key", &key);
    assert_eq!(
        code(&sop(dir.path(), &["sign", "locked.key"], b"m")),
        67,
        "KEY_IS_PROTECTED"
    );
    write(dir.path(), "wrong", b"wrong horse");
    assert_eq!(
        code(&sop(
            dir.path(),
            &["sign", "--with-key-password=wrong", "locked.key"],
            b"m"
        )),
        67,
        "wrong password"
    );
    write(dir.path(), "password", b"correct horse\r\n");
    ok(sop(
        dir.path(),
        &[
            "sign",
            "--with-key-password=wrong",
            "--with-key-password=password",
            "locked.key",
        ],
        b"m",
    ));
    ok(sop(dir.path(), &["extract-cert"], &key));
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        write(dir.path(), "binary", b"\xff\xfe");
        let args: Vec<&OsStr> = vec![
            OsStr::new("generate-key"),
            OsStr::from_bytes(b"--with-key-password=binary"),
        ];
        assert_eq!(
            code(&sop_in(dir.path(), &args, b"", &[])),
            31,
            "PASSWORD_NOT_HUMAN_READABLE"
        );
    }
}

#[test]
fn special_designators_follow_section_7_1() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = alice(dir.path());
    let signature = ok(sop(dir.path(), &["sign", &key], b"m"));
    let signature = String::from_utf8(signature).unwrap();
    ok(sop_env(
        dir.path(),
        &["verify", "@ENV:SIGNATURE", &cert],
        b"m",
        &[("SIGNATURE", &signature)],
    ));
    assert_eq!(
        code(&sop(
            dir.path(),
            &["verify", "@ENV:UNSET_VARIABLE", &cert],
            b"m"
        )),
        61,
        "an unset variable"
    );
    assert_eq!(
        code(&sop(dir.path(), &["verify", "@FD:x", &cert], b"m")),
        71,
        "@FD: with no number"
    );
    assert_eq!(
        code(&sop(dir.path(), &["verify", "@HTTP:x", &cert], b"m")),
        71,
        "UNSUPPORTED_SPECIAL_PREFIX"
    );
    write(dir.path(), "@ENV:SIGNATURE", signature.as_bytes());
    let ambiguous = sop_env(
        dir.path(),
        &["verify", "@ENV:SIGNATURE", &cert],
        b"m",
        &[("SIGNATURE", &signature)],
    );
    assert_eq!(code(&ambiguous), 73, "AMBIGUOUS_INPUT");
}

#[test]
fn several_keys_make_several_signatures_and_each_verifies() {
    let dir = tempfile::tempdir().unwrap();
    let (alice_key, alice_cert) = alice(dir.path());
    let bob_key = ok(sop(
        dir.path(),
        &["generate-key", "Bob <bob@example.org>"],
        b"",
    ));
    write(dir.path(), "bob.key", &bob_key);
    write(
        dir.path(),
        "bob.cert",
        &ok(sop(dir.path(), &["extract-cert"], &bob_key)),
    );
    write(
        dir.path(),
        "both.sig",
        &ok(sop(dir.path(), &["sign", &alice_key, "bob.key"], b"m")),
    );
    let verified = String::from_utf8(ok(sop(
        dir.path(),
        &["verify", "both.sig", &alice_cert, "bob.cert"],
        b"m",
    )))
    .unwrap();
    assert_eq!(verified.lines().count(), 2, "one line per signature");
    assert!(verified.contains(r#"{"signers":["alice.cert"]}"#), "Alice");
    assert!(verified.contains(r#"{"signers":["bob.cert"]}"#), "Bob");
}

#[test]
fn the_rfc_9980_a3_signature_verifies_through_the_command_line() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a3.cert", A3_CERTIFICATE.as_bytes());
    write(dir.path(), "a3.sig", A3_SIGNATURE.as_bytes());
    let verified = String::from_utf8(ok(sop(
        dir.path(),
        &["verify", "a3.sig", "a3.cert"],
        A3_MESSAGE,
    )))
    .unwrap();
    assert!(
        verified.contains(" A3E2E14B6A493FF930FB27321F125E9A6880338BE9FB7DA3AE065EA65793242F "),
        "the A.3 fingerprint: {verified}"
    );
    assert!(
        verified.contains(" mode:text "),
        "A.3.4 is a text signature"
    );
}
