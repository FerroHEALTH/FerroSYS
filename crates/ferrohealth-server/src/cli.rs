// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The subcommands and the arguments every FerroHEALTH binary shares.
//!
//! A product flattens [`ConfigArgs`] into its parser and [`Common`] into its
//! own subcommand enum, and adds its jobs beside them:
//!
//! ```
//! use clap::{Parser, Subcommand};
//! use ferrohealth_server::cli::{Common, ConfigArgs, ConfigCommand};
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
//! let cli = Cli::parse_from(["ferrosys", "config", "check", "--set", "server.listen=0.0.0.0:80"]);
//! assert!(matches!(cli.command, Command::Common(Common::Config { command: ConfigCommand::Check })));
//! assert_eq!(1, cli.config.sets.len());
//! ```

use std::path::PathBuf;

use clap::{Args, Subcommand};

/// `--config` and `--set`, which every subcommand takes.
///
/// `--config` and the `--set` pairs feed
/// `ferrohealth_config::loader::Loader::load_with`, which applies the pairs
/// last, over the file and the environment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Args)]
pub struct ConfigArgs {
    /// The TOML configuration file; unset, the product's `_CONFIG` variable
    /// names it.
    #[arg(long, value_name = "PATH", global = true)]
    pub config: Option<PathBuf>,
    /// One configuration key set over the file and the environment, as
    /// `key=value`; repeatable.
    #[arg(long = "set", value_name = "KEY=VALUE", value_parser = key_value, global = true)]
    pub sets: Vec<(String, String)>,
}

/// One `--set` pair: the key before the first `=`, the value after it.
fn key_value(text: &str) -> Result<(String, String), String> {
    match text.split_once('=') {
        Some((key, value)) if !key.trim().is_empty() => {
            Ok((key.trim().to_owned(), value.to_owned()))
        }
        _ => Err("expected key=value, such as server.listen=0.0.0.0:8080".to_owned()),
    }
}

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

    use super::{Common, ConfigArgs, ConfigCommand};

    #[derive(Debug, Parser)]
    struct Cli {
        #[command(flatten)]
        config: ConfigArgs,
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

    #[test]
    fn config_and_set_parse_before_or_after_the_subcommand() {
        let cli = Cli::try_parse_from([
            "x",
            "--config",
            "/etc/ferrosys.toml",
            "serve",
            "--set",
            "server.listen=0.0.0.0:80",
            "--set",
            "a.b=c=d",
        ])
        .expect("the arguments parse");
        assert_eq!(
            ConfigArgs {
                config: Some("/etc/ferrosys.toml".into()),
                sets: vec![
                    ("server.listen".to_owned(), "0.0.0.0:80".to_owned()),
                    ("a.b".to_owned(), "c=d".to_owned()),
                ],
            },
            cli.config
        );
        assert_eq!(
            ConfigArgs::default(),
            Cli::try_parse_from(["x", "serve"])
                .expect("no arguments")
                .config
        );
        for bad in ["novalue", "=value"] {
            assert!(
                Cli::try_parse_from(["x", "serve", "--set", bad]).is_err(),
                "{bad:?} is refused"
            );
        }
    }
}
