// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The run path every FerroHEALTH binary shares: the command line parsed,
//! the runtime started, the product's job run on it, and a failure written
//! with its whole cause chain.
//!
//! A product keeps its own parser and every subcommand body; [`Run`] holds
//! what is the same around them:
//!
//! ```
//! use std::process::ExitCode;
//!
//! use clap::{Parser, Subcommand};
//! use ferrohealth_server::cli::{Common, ConfigArgs};
//! use ferrohealth_server::run::{EXIT_CONFIG, Failure, Run};
//!
//! #[derive(Debug, Parser)]
//! struct Cli {
//!     #[command(flatten)]
//!     config: ConfigArgs,
//!     #[command(subcommand)]
//!     command: Command,
//! }
//!
//! #[derive(Debug, Subcommand)]
//! enum Command {
//!     #[command(flatten)]
//!     Common(Common),
//!     /// A job of the product's own.
//!     Migrate,
//! }
//!
//! let code = Run::new("ferrosys").run(["ferrosys", "migrate"], |cli: Cli| async move {
//!     match cli.command {
//!         Command::Migrate => Ok(ExitCode::SUCCESS),
//!         Command::Common(_) => {
//!             let refused = std::io::Error::other("server.listen is not an address");
//!             Err(Failure::from(refused).with_context("cannot start").with_code(EXIT_CONFIG))
//!         }
//!     }
//! });
//! assert_eq!(ExitCode::SUCCESS, code);
//! ```
//!
//! No specification governs this: our own design, with the exit codes of
//! clap and of `sysexits(3)`.

use std::error::Error;
use std::ffi::OsString;
use std::fmt::{self, Write as _};
use std::io::{self, Write};
use std::num::NonZeroU8;
use std::process::ExitCode;

use clap::Parser;

/// The exit code of a configuration the binary refuses to start on.
///
/// `EX_CONFIG` from `sysexits(3)`
/// (<https://man.freebsd.org/cgi/man.cgi?query=sysexits>), so an
/// orchestrator tells a bad configuration from a failure to serve.
pub const EXIT_CONFIG: NonZeroU8 = NonZeroU8::MIN.saturating_add(77);

/// The exit code of a failure that names no code of its own.
pub const EXIT_FAILURE: NonZeroU8 = NonZeroU8::MIN;

/// The run path of one binary: its name in every line it writes, and the
/// runtime its job runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub struct Run {
    name: &'static str,
    thread_stack_size: Option<usize>,
}

impl Run {
    /// The run path of the binary `name`, which opens every line it writes
    /// to standard error.
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            thread_stack_size: None,
        }
    }

    /// Gives every runtime thread, the workers and the blocking pool alike,
    /// a stack of `bytes` in the place of tokio's default.
    pub const fn thread_stack_size(mut self, bytes: usize) -> Self {
        self.thread_stack_size = Some(bytes);
        self
    }

    /// Parses `args` as `C`, runs `dispatch` on a multi-threaded runtime and
    /// returns the process exit code.
    ///
    /// `args` is the whole argument vector, the program name included, so
    /// clap reports usage under the right name. Help and the version go to
    /// standard output and exit 0; a usage error goes to standard error and
    /// exits with clap's code. The runtime has every driver on. The exit
    /// code is the one `dispatch` returns, or, for a [`Failure`], the line
    /// `<name>: <context>: <cause chain>` on standard error and the
    /// failure's code. A runtime that cannot start is such a failure, with
    /// the context `cannot start the runtime` and exit 1.
    ///
    /// The job must start no runtime of its own: tokio refuses one inside
    /// another.
    pub fn run<C, I, T, D, F>(self, args: I, dispatch: D) -> ExitCode
    where
        C: Parser,
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
        D: FnOnce(C) -> F,
        F: Future<Output = Result<ExitCode, Failure>>,
    {
        self.run_to(C::try_parse_from(args), dispatch, &mut io::stderr())
    }

    /// [`Run::run`] over the parse of its command line, with the failure
    /// line written to `stderr`.
    fn run_to<C, D, F>(
        self,
        parsed: Result<C, clap::Error>,
        dispatch: D,
        stderr: &mut dyn Write,
    ) -> ExitCode
    where
        D: FnOnce(C) -> F,
        F: Future<Output = Result<ExitCode, Failure>>,
    {
        let parsed = match parsed {
            Ok(parsed) => parsed,
            Err(error) => return usage(&error),
        };
        let runtime = match self.runtime() {
            Ok(runtime) => runtime,
            Err(error) => {
                let failure = Failure::from(error).with_context("cannot start the runtime");
                return self.report(&failure, stderr);
            }
        };
        match runtime.block_on(dispatch(parsed)) {
            Ok(code) => code,
            Err(failure) => self.report(&failure, stderr),
        }
    }

    /// The multi-threaded runtime with every driver on.
    fn runtime(self) -> io::Result<tokio::runtime::Runtime> {
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        builder.enable_all();
        if let Some(bytes) = self.thread_stack_size {
            builder.thread_stack_size(bytes);
        }
        builder.build()
    }

    /// Writes `failure` to `stderr` and returns its exit code.
    fn report(self, failure: &Failure, stderr: &mut dyn Write) -> ExitCode {
        let code = ExitCode::from(failure.code.get());
        // NOTE: no specification governs this: our own design; a standard
        // error that refuses the line leaves the exit code as the report.
        match writeln!(stderr, "{}: {failure}", self.name) {
            Ok(()) | Err(_) => code,
        }
    }
}

/// Prints clap's answer to a command line it did not run and returns its
/// exit code: 0 for help and the version, clap's usage code otherwise.
fn usage(error: &clap::Error) -> ExitCode {
    let code = error.exit_code();
    if error.print().is_err() {
        return ExitCode::FAILURE;
    }
    u8::try_from(code).map_or(ExitCode::FAILURE, ExitCode::from)
}

/// A job that failed: its error, the line that says what was being done,
/// and the exit code.
///
/// Any [`Error`] converts into one with `?` and exits 1. Neither the
/// rendering nor the `Debug` form reads anything of the error but the
/// `Display` of each cause in its chain.
pub struct Failure {
    error: Box<dyn Error>,
    context: Option<String>,
    code: NonZeroU8,
}

impl<E: Error + 'static> From<E> for Failure {
    fn from(error: E) -> Self {
        Self::boxed(error)
    }
}

impl Failure {
    /// A failure over an error already boxed, such as an `anyhow::Error`
    /// or a message.
    #[must_use]
    pub fn boxed(error: impl Into<Box<dyn Error>>) -> Self {
        Self {
            error: error.into(),
            context: None,
            code: EXIT_FAILURE,
        }
    }

    /// Opens the rendered line with `context`, such as `cannot start`.
    #[must_use]
    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = Some(context.into());
        self
    }

    /// Exits with `code` in the place of 1, such as [`EXIT_CONFIG`].
    #[must_use]
    pub const fn with_code(mut self, code: NonZeroU8) -> Self {
        self.code = code;
        self
    }

    /// The exit code the failure ends the process with.
    #[must_use]
    pub const fn code(&self) -> NonZeroU8 {
        self.code
    }

    /// The error the failure carries.
    #[must_use]
    pub fn error(&self) -> &(dyn Error + 'static) {
        self.error.as_ref()
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(context) = &self.context {
            write!(f, "{context}: ")?;
        }
        f.write_str(&chain(self.error.as_ref()))
    }
}

impl fmt::Debug for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Failure")
            .field("error", &chain(self.error.as_ref()))
            .field("context", &self.context)
            .field("code", &self.code)
            .finish()
    }
}

/// Renders `error` and every cause behind it on one line, each separated
/// by `: `.
///
/// Each cause is read through its `Display` alone, so a type that redacts
/// itself there stays redacted.
#[must_use]
pub fn chain(error: &dyn Error) -> String {
    let mut line = error.to_string();
    let mut cause = error.source();
    while let Some(source) = cause {
        // NOTE: no specification governs this: our own design; writing to a
        // String cannot fail, so the result carries nothing.
        if write!(line, ": {source}").is_err() {
            break;
        }
        cause = source.source();
    }
    line
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::fmt;
    use std::process::ExitCode;

    use clap::{Parser, Subcommand};

    use super::{EXIT_CONFIG, Failure, Run, chain};

    #[derive(Debug, Parser)]
    #[command(name = "ferrosys", version = "1.2.3")]
    struct Cli {
        #[command(subcommand)]
        command: Command,
    }

    #[derive(Debug, Subcommand)]
    enum Command {
        Ok,
        Fail,
        Async,
    }

    /// A cause whose `Debug` holds what its `Display` keeps out.
    struct Refused {
        secret: &'static str,
        source: Option<Box<Refused>>,
    }

    impl fmt::Debug for Refused {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "Refused({})", self.secret)
        }
    }

    impl fmt::Display for Refused {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            if self.source.is_some() {
                f.write_str("the token endpoint refused the client")
            } else {
                f.write_str("401 Unauthorized")
            }
        }
    }

    impl Error for Refused {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            let source: &(dyn Error + 'static) = self.source.as_deref()?;
            Some(source)
        }
    }

    fn refused() -> Refused {
        Refused {
            secret: "s3cr3t-password",
            source: Some(Box::new(Refused {
                secret: "s3cr3t-body",
                source: None,
            })),
        }
    }

    fn run(args: &[&str]) -> (ExitCode, String) {
        let mut stderr = Vec::new();
        let code = Run::new("ferrosys").run_to(
            Cli::try_parse_from(args.iter().copied()),
            |cli: Cli| async move {
                match cli.command {
                    Command::Ok => Ok(ExitCode::from(3)),
                    Command::Fail => Err(Failure::from(refused())
                        .with_context("cannot start")
                        .with_code(EXIT_CONFIG)),
                    Command::Async => {
                        let handle = tokio::spawn(async { 7_u8 });
                        handle.await.map(ExitCode::from).map_err(Failure::from)
                    }
                }
            },
            &mut stderr,
        );
        (code, String::from_utf8(stderr).expect("the line is UTF-8"))
    }

    #[test]
    fn a_usage_error_exits_with_clap_s_code_and_help_with_0() {
        let (code, stderr) = run(&["ferrosys", "unknown"]);
        assert_eq!(ExitCode::from(2), code, "clap's usage code");
        assert!(stderr.is_empty(), "clap writes its own refusal: {stderr}");
        assert_eq!(ExitCode::from(2), run(&["ferrosys"]).0, "a job is required");
        for asked in ["--help", "--version"] {
            assert_eq!(ExitCode::SUCCESS, run(&["ferrosys", asked]).0, "{asked}");
        }
    }

    #[test]
    fn the_dispatch_s_code_is_the_process_s() {
        assert_eq!((ExitCode::from(3), String::new()), run(&["ferrosys", "ok"]));
    }

    #[test]
    fn the_dispatch_runs_on_a_multi_threaded_runtime() {
        assert_eq!(ExitCode::from(7), run(&["ferrosys", "async"]).0);
        let stacked = Run::new("ferrosys").thread_stack_size(4 * 1024 * 1024).run(
            ["ferrosys", "async"],
            |_: Cli| async {
                let handle = tokio::task::spawn_blocking(|| 9_u8);
                handle.await.map(ExitCode::from).map_err(Failure::from)
            },
        );
        assert_eq!(ExitCode::from(9), stacked);
    }

    #[test]
    fn a_failure_prints_every_cause_and_exits_with_its_code() {
        let (code, stderr) = run(&["ferrosys", "fail"]);
        assert_eq!(ExitCode::from(78), code, "EX_CONFIG");
        assert_eq!(
            "ferrosys: cannot start: the token endpoint refused the client: 401 Unauthorized\n",
            stderr
        );
    }

    #[test]
    fn nothing_past_each_cause_s_display_is_rendered() {
        let failure = Failure::from(refused());
        for rendered in [
            failure.to_string(),
            format!("{failure:?}"),
            chain(&refused()),
        ] {
            assert!(!rendered.contains("s3cr3t"), "{rendered}");
            assert!(rendered.contains("401 Unauthorized"), "{rendered}");
        }
        assert_eq!(1, failure.code().get(), "a failure exits 1 unless named");
    }

    #[test]
    fn a_boxed_error_keeps_its_chain() {
        let message = Failure::boxed("the listener is taken").with_context("cannot serve");
        assert_eq!("cannot serve: the listener is taken", message.to_string());
        let boxed: Box<dyn Error> = Box::new(refused());
        assert_eq!(
            "the token endpoint refused the client: 401 Unauthorized",
            Failure::boxed(boxed).to_string()
        );
        assert_eq!(
            "401 Unauthorized",
            Failure::boxed(boxed_root()).error().to_string()
        );
    }

    fn boxed_root() -> Box<dyn Error> {
        Box::new(Refused {
            secret: "s3cr3t",
            source: None,
        })
    }
}
