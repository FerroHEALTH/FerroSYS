// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! `ferrohealth-perf`: the performance gate a product's CI job runs.
//!
//! `run` measures a server and judges it against the baseline; `compare`
//! judges a saved report again and contacts nothing. Both print the
//! Markdown summary, append it to `$GITHUB_STEP_SUMMARY` when that is set,
//! and exit 0 for a pass, 1 for a breach and 2 for a run that cannot be
//! judged or a harness error.
#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a binary writes its summary to stdout and its errors to stderr"
)]

use std::error::Error;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use ferrohealth_perf::baseline::Baselines;
use ferrohealth_perf::compare::judge_file;
use ferrohealth_perf::plan::Plan;
use ferrohealth_perf::process::{Target, container_cgroup};
use ferrohealth_perf::report::Report;
use ferrohealth_perf::run::{DEFAULT_RUNS, RunOptions, run};
use ferrohealth_perf::summary;
use ferrohealth_secret::secret_url::SecretUrl;

/// The exit code of a harness or setup error.
const HARNESS: u8 = 2;

/// The performance gate of the FerroHEALTH servers.
#[derive(Debug, Parser)]
#[command(name = "ferrohealth-perf", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Measures a running server and judges it against the baseline.
    Run(RunArgs),
    /// Judges a saved report against the baseline; contacts no server.
    Compare(CompareArgs),
}

#[derive(Debug, Args)]
struct Output {
    /// The baseline file, one entry per CPU model.
    #[arg(long, value_name = "PATH", default_value = "perf/baseline.toml")]
    baseline: PathBuf,
    /// A file the Markdown summary is also written to.
    #[arg(long, value_name = "PATH")]
    summary: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct RunArgs {
    /// The plan: limits, load and request set.
    #[arg(long, value_name = "PATH", default_value = "perf/plan.toml")]
    plan: PathBuf,
    /// The server's process id.
    #[arg(long, value_name = "PID", group = "server", required_unless_present_any = ["cgroup", "container"])]
    pid: Option<u32>,
    /// The server's cgroup v2 directory.
    #[arg(long, value_name = "DIR", group = "server")]
    cgroup: Option<PathBuf>,
    /// The server's container id, full or leading, found under the cgroup
    /// root.
    #[arg(long, value_name = "ID", group = "server")]
    container: Option<String>,
    /// Where the cgroup v2 hierarchy is mounted.
    #[arg(long, value_name = "DIR", default_value = "/sys/fs/cgroup")]
    cgroup_root: PathBuf,
    /// The server's base URL.
    #[arg(long, value_name = "URL")]
    base_url: String,
    /// The server's PostgreSQL database, whose transactions are counted.
    #[arg(
        long,
        value_name = "URL",
        env = "FERROHEALTH_PERF_DATABASE_URL",
        hide_env_values = true
    )]
    database_url: Option<String>,
    /// The directory the JSON reports are written to.
    #[arg(long, value_name = "DIR", default_value = "target/perf")]
    out_dir: PathBuf,
    /// Writes the baseline entry of the runner's CPU model from a run that
    /// passes: the median of `--runs` load phases, with their spread as the
    /// noise floor.
    #[arg(long)]
    update: bool,
    /// The load phases `--update` measures, each after its own warm-up.
    #[arg(long, value_name = "N", requires = "update", value_parser = clap::value_parser!(u32).range(2..))]
    runs: Option<u32>,
    #[command(flatten)]
    output: Output,
}

#[derive(Debug, Args)]
struct CompareArgs {
    /// The saved JSON report.
    #[arg(long, value_name = "PATH")]
    report: PathBuf,
    #[command(flatten)]
    output: Output,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(cli.command) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            let mut text = format!("ferrohealth-perf: {error}");
            let mut cause = error.source();
            while let Some(inner) = cause {
                text.push_str(": ");
                text.push_str(&inner.to_string());
                cause = inner.source();
            }
            eprintln!("{text}");
            ExitCode::from(HARNESS)
        }
    }
}

fn dispatch(command: Command) -> Result<u8, Box<dyn Error>> {
    match command {
        Command::Run(args) => run_command(args),
        Command::Compare(args) => compare_command(&args),
    }
}

fn run_command(args: RunArgs) -> Result<u8, Box<dyn Error>> {
    let plan = Plan::read(&args.plan)?;
    let target = match (args.pid, args.cgroup, args.container) {
        (Some(pid), _, _) => Target::Pid(pid),
        (_, Some(dir), _) => Target::Cgroup(dir),
        (_, _, Some(id)) => Target::Cgroup(container_cgroup(&args.cgroup_root, &id)?),
        (None, None, None) => return Err("give --pid, --cgroup or --container".into()),
    };
    let options = RunOptions {
        plan,
        target,
        base_url: SecretUrl::new(args.base_url),
        database_url: args.database_url.map(SecretUrl::new),
        baseline: args.output.baseline.clone(),
        out_dir: args.out_dir,
        update: args.update,
        runs: args.runs.unwrap_or(DEFAULT_RUNS),
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(run(&options))?;
    publish(&result.summary, args.output.summary.as_deref())?;
    println!("report: {}", result.report_path.display());
    Ok(result.verdict.outcome.code())
}

fn compare_command(args: &CompareArgs) -> Result<u8, Box<dyn Error>> {
    let report = Report::read(&args.report)?;
    let baseline = Baselines::read(&args.output.baseline)?;
    let verdict = judge_file(&report, baseline.as_ref(), false)?;
    publish(
        &summary::markdown(&report, &verdict),
        args.output.summary.as_deref(),
    )?;
    Ok(verdict.outcome.code())
}

/// Prints the summary, writes it to `path`, and appends it to the step
/// summary of a GitHub Actions job.
fn publish(text: &str, path: Option<&Path>) -> Result<(), Box<dyn Error>> {
    print!("{text}");
    if let Some(path) = path {
        std::fs::write(path, text)?;
    }
    if let Some(step) = std::env::var_os("GITHUB_STEP_SUMMARY").filter(|s| !s.is_empty()) {
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(step)?;
        file.write_all(text.as_bytes())?;
    }
    Ok(())
}
