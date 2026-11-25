// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! `ferrohealth-sop`: a subset of the Stateless OpenPGP command line
//! (draft-dkg-openpgp-stateless-cli-16) over ML-DSA-65+Ed25519 keys.
//!
//! `version`, `list-profiles`, `generate-key`, `extract-cert`, `sign` and
//! `verify` follow §5 of the draft. Output goes to standard output, the
//! reason for a failure to standard error, and the process returns the code
//! of §8 for the failure's mnemonic.

mod sop;

use std::ffi::OsString;
use std::io::Write as _;
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use ferrohealth_openpgp_pq::encoding::Encoding;
use ferrohealth_openpgp_pq::mode::Mode;

use crate::sop::commands::{self, GenerateKey, Sign, Verify, VersionInfo};
use crate::sop::exit::{Exit, Failure};
use crate::sop::input;

/// Stateless OpenPGP over ML-DSA-65+Ed25519 (RFC 9980) keys.
#[derive(Debug, Parser)]
#[command(name = "ferrohealth-sop", disable_version_flag = true)]
struct Cli {
    /// Emit more verbose output (§4.1).
    #[arg(long, global = true)]
    debug: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Version information (§5.1.1).
    Version(VersionArgs),
    /// The profiles of a subcommand (§5.1.2).
    ListProfiles {
        /// The subcommand whose profiles to list.
        subcommand: String,
    },
    /// Generate a secret key (§5.2.1).
    GenerateKey(GenerateKeyArgs),
    /// Extract the certificate of each secret key on standard input (§5.2.4).
    ExtractCert {
        /// Write binary packets.
        #[arg(long)]
        no_armor: bool,
    },
    /// Make one detached signature per key over standard input (§5.4.1).
    Sign(SignArgs),
    /// Verify detached signatures over standard input (§5.4.2).
    Verify(VerifyArgs),
}

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("info").multiple(false)))]
#[expect(
    clippy::struct_excessive_bools,
    reason = "§5.1.1 defines four mutually exclusive flags, which the group keeps to one"
)]
struct VersionArgs {
    /// The underlying OpenPGP toolkit.
    #[arg(long, group = "info")]
    backend: bool,
    /// Several lines of version information.
    #[arg(long, group = "info")]
    extended: bool,
    /// The draft this implementation targets.
    #[arg(long, group = "info")]
    sop_spec: bool,
    /// The sopv version covered.
    #[arg(long, group = "info")]
    sopv: bool,
}

#[derive(Debug, Args)]
struct GenerateKeyArgs {
    /// Write binary packets.
    #[arg(long)]
    no_armor: bool,
    /// Lock the key with the password read from PASSWORD.
    #[arg(long, value_name = "PASSWORD")]
    with_key_password: Option<OsString>,
    /// The profile; `list-profiles generate-key` lists them.
    #[arg(long)]
    profile: Option<String>,
    /// Make a signing-only key, which every key of this implementation is.
    #[arg(long)]
    signing_only: bool,
    /// The User IDs.
    #[arg(value_name = "USERID")]
    user_ids: Vec<OsString>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum As {
    Binary,
    Text,
}

#[derive(Debug, Args)]
struct SignArgs {
    /// Write binary packets.
    #[arg(long)]
    no_armor: bool,
    /// What the signature covers.
    #[arg(long = "as", value_enum, default_value = "binary")]
    mode: As,
    /// A PASSWORD that may unlock a locked key; repeatable.
    #[arg(long, value_name = "PASSWORD")]
    with_key_password: Vec<OsString>,
    /// The KEYS to sign with.
    #[arg(value_name = "KEYS", required = true)]
    keys: Vec<OsString>,
}

#[derive(Debug, Args)]
struct VerifyArgs {
    /// Refuse signatures made before this DATE (`-` for no limit).
    #[arg(long, value_name = "DATE")]
    not_before: Option<String>,
    /// Refuse signatures made after this DATE (`-` for no limit, `now` by default).
    #[arg(long, value_name = "DATE")]
    not_after: Option<String>,
    /// The SIGNATURES to verify.
    #[arg(value_name = "SIGNATURES")]
    signatures: OsString,
    /// The CERTS that may have made them.
    #[arg(value_name = "CERTS", required = true)]
    certs: Vec<OsString>,
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => return usage(&error),
    };
    if cli.debug {
        // NOTE: §4.1, the arguments name inputs and never hold their contents, so they may be shown.
        let _unreported = writeln!(
            std::io::stderr().lock(),
            "ferrohealth-sop: {:?}",
            cli.command
        );
    }
    match run(cli.command) {
        Ok(output) => match std::io::stdout().lock().write_all(&output) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(&Failure::new(
                Exit::UnspecifiedFailure,
                format!("writing standard output: {error}"),
            )),
        },
        Err(failure) => fail(&failure),
    }
}

fn run(command: Command) -> Result<Vec<u8>, Failure> {
    match command {
        Command::Version(arguments) => commands::version(if arguments.backend {
            VersionInfo::Backend
        } else if arguments.extended {
            VersionInfo::Extended
        } else if arguments.sop_spec {
            VersionInfo::SopSpec
        } else if arguments.sopv {
            VersionInfo::Sopv
        } else {
            VersionInfo::Plain
        }),
        Command::ListProfiles { subcommand } => commands::list_profiles(&subcommand),
        Command::GenerateKey(arguments) => commands::generate_key(&GenerateKey {
            encoding: encoding(arguments.no_armor),
            password: arguments.with_key_password,
            profile: arguments.profile,
            user_ids: arguments.user_ids,
        }),
        Command::ExtractCert { no_armor } => {
            commands::extract_cert(&input::stdin()?, encoding(no_armor))
        }
        Command::Sign(arguments) => commands::sign(
            &Sign {
                encoding: encoding(arguments.no_armor),
                mode: match arguments.mode {
                    As::Binary => Mode::Binary,
                    As::Text => Mode::Text,
                },
                passwords: arguments.with_key_password,
                keys: arguments.keys,
            },
            &input::stdin()?,
        ),
        Command::Verify(arguments) => commands::verify(
            &Verify {
                not_before: arguments.not_before,
                not_after: arguments.not_after,
                signatures: arguments.signatures,
                certs: arguments.certs,
            },
            &input::stdin()?,
        ),
    }
}

fn encoding(no_armor: bool) -> Encoding {
    if no_armor {
        Encoding::Binary
    } else {
        Encoding::Armored
    }
}

/// Answers a command line clap refused with the code §5 and §8 give it.
fn usage(error: &clap::Error) -> ExitCode {
    let exit = match error.kind() {
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
            return match error.print() {
                Ok(()) => ExitCode::SUCCESS,
                Err(_unwritable) => ExitCode::from(Exit::UnspecifiedFailure.code()),
            };
        }
        ErrorKind::InvalidSubcommand => Exit::UnsupportedSubcommand,
        ErrorKind::MissingRequiredArgument
        | ErrorKind::MissingSubcommand
        | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => Exit::MissingArg,
        ErrorKind::ArgumentConflict => Exit::IncompatibleOptions,
        _ => Exit::UnsupportedOption,
    };
    // NOTE: a closed standard error leaves the exit code as the only report, which the caller still reads.
    let _unreported = error.print();
    ExitCode::from(exit.code())
}

fn fail(failure: &Failure) -> ExitCode {
    // NOTE: a closed standard error leaves the exit code as the only report, which the caller still reads.
    let _unreported = writeln!(std::io::stderr().lock(), "ferrohealth-sop: {failure}");
    ExitCode::from(failure.exit().code())
}
