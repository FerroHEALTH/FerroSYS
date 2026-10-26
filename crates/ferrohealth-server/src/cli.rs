// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The subcommands every FerroHEALTH binary shares.
//!
//! A product flattens [`Common`] into its own subcommand enum and adds its
//! jobs beside it:
//!
//! ```
//! use clap::{Parser, Subcommand};
//! use ferrohealth_server::cli::{Common, ConfigCommand};
//!
//! #[derive(Debug, Parser)]
//! struct Cli {
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
//! let cli = Cli::parse_from(["ferrosys", "config", "check"]);
//! assert!(matches!(cli.command, Command::Common(Common::Config { command: ConfigCommand::Check })));
//! ```

use clap::Subcommand;

/// `serve`, `config check` and `healthcheck`.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Common {
    /// Serves until the process is asked to stop.
    Serve,
    /// Works on the configuration.
    Config {
        /// The job to run.
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Asks the server running beside this process whether it is ready, and
    /// exits 0 only when it is.
    Healthcheck,
}

/// The `config` jobs.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum ConfigCommand {
    /// Reads the configuration, reports every refusal with its key, and
    /// prints the running tree with every secret redacted.
    Check,
}

#[cfg(test)]
mod tests {
    use clap::{Parser, Subcommand};

    use super::{Common, ConfigCommand};

    #[derive(Debug, Parser)]
    struct Cli {
        #[command(subcommand)]
        command: Command,
    }

    #[derive(Debug, Subcommand)]
    enum Command {
        #[command(flatten)]
        Common(Common),
        Migrate,
    }

    #[test]
    fn the_shared_subcommands_sit_beside_the_product_s_own() {
        for (args, want) in [
            (vec!["x", "serve"], Some(Common::Serve)),
            (vec!["x", "healthcheck"], Some(Common::Healthcheck)),
            (
                vec!["x", "config", "check"],
                Some(Common::Config {
                    command: ConfigCommand::Check,
                }),
            ),
            (vec!["x", "migrate"], None),
        ] {
            let cli = Cli::try_parse_from(&args).expect("the arguments parse");
            let got = match cli.command {
                Command::Common(common) => Some(common),
                Command::Migrate => None,
            };
            assert_eq!(want, got, "{args:?}");
        }
        assert!(
            Cli::try_parse_from(["x", "config"]).is_err(),
            "config needs its job"
        );
    }
}
