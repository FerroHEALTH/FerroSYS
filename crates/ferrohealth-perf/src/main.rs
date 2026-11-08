// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! `ferrohealth-perf`: the performance gate a product's CI job runs.
//!
//! `run` measures a server and judges it against the baseline; `ab`
//! measures the base and the head of a change side by side and judges the
//! head over the base; `compare` judges a saved report again and contacts
//! nothing. Each prints the
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
use ferrohealth_perf::ab::{AbOptions, AbReport, ArmValues, DEFAULT_PAIRS, Server};
use ferrohealth_perf::baseline::Baselines;
use ferrohealth_perf::compare::judge_file;
use ferrohealth_perf::plan::Plan;
use ferrohealth_perf::process::{Target, container_cgroup};
use ferrohealth_perf::report::{Report, ReportError};
use ferrohealth_perf::run::{DEFAULT_RUNS, RunOptions, run};
use ferrohealth_perf::summary;
use ferrohealth_perf::values::Values;
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
    /// Measures the head of a change and its base side by side on this
    /// host and judges the head over the base.
    Ab(AbArgs),
    /// Judges a saved report against the baseline, or a saved A/B report
    /// again; contacts no server.
    Compare(CompareArgs),
    /// Merges the baselines several `--update` jobs wrote into the
    /// committed one, so each host shape's noise floor holds the spread
    /// between runners; contacts no server.
    Merge(MergeArgs),
}

#[derive(Debug, Args)]
struct MergeArgs {
    /// The committed baseline file the entries are merged into.
    #[arg(long, value_name = "PATH", default_value = "perf/baseline.toml")]
    baseline: PathBuf,
    /// The baseline files the jobs wrote, one or more.
    #[arg(value_name = "JOB", required = true)]
    jobs: Vec<PathBuf>,
}

#[derive(Debug, Args)]
struct Output {
    /// The baseline file, one entry per host shape.
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
    /// The server's MLLP listener, `host:port`, in place of the plan's
    /// `[mllp] address`.
    #[arg(long, value_name = "HOST:PORT")]
    mllp_address: Option<String>,
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
    /// Writes the baseline entry of the runner's host shape from a run that
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
struct AbArgs {
    /// The plan: limits, load, tolerances and request set.
    #[arg(long, value_name = "PATH", default_value = "perf/plan.toml")]
    plan: PathBuf,
    /// The base server's base URL.
    #[arg(long, value_name = "URL")]
    base_url: String,
    /// The base server's process id.
    #[arg(long, value_name = "PID", group = "base", required_unless_present_any = ["base_cgroup", "base_container"])]
    base_pid: Option<u32>,
    /// The base server's cgroup v2 directory.
    #[arg(long, value_name = "DIR", group = "base")]
    base_cgroup: Option<PathBuf>,
    /// The base server's container id, full or leading.
    #[arg(long, value_name = "ID", group = "base")]
    base_container: Option<String>,
    /// The base server's PostgreSQL database, whose transactions are
    /// counted.
    #[arg(
        long,
        value_name = "URL",
        env = "FERROHEALTH_PERF_BASE_DATABASE_URL",
        hide_env_values = true
    )]
    base_database_url: Option<String>,
    /// The base server's MLLP listener, `host:port`.
    #[arg(long, value_name = "HOST:PORT")]
    base_mllp_address: Option<String>,
    /// The head server's base URL.
    #[arg(long, value_name = "URL")]
    head_url: String,
    /// The head server's process id.
    #[arg(long, value_name = "PID", group = "head", required_unless_present_any = ["head_cgroup", "head_container"])]
    head_pid: Option<u32>,
    /// The head server's cgroup v2 directory.
    #[arg(long, value_name = "DIR", group = "head")]
    head_cgroup: Option<PathBuf>,
    /// The head server's container id, full or leading.
    #[arg(long, value_name = "ID", group = "head")]
    head_container: Option<String>,
    /// The head server's PostgreSQL database, whose transactions are
    /// counted and held to the idle limit.
    #[arg(
        long,
        value_name = "URL",
        env = "FERROHEALTH_PERF_HEAD_DATABASE_URL",
        hide_env_values = true
    )]
    head_database_url: Option<String>,
    /// The head server's MLLP listener, `host:port`.
    #[arg(long, value_name = "HOST:PORT")]
    head_mllp_address: Option<String>,
    /// Where the cgroup v2 hierarchy is mounted.
    #[arg(long, value_name = "DIR", default_value = "/sys/fs/cgroup")]
    cgroup_root: PathBuf,
    /// The pairs of load phases, one per server each, in alternating order:
    /// an even count of at least two.
    #[arg(long, value_name = "N", default_value_t = DEFAULT_PAIRS, value_parser = even_pairs)]
    pairs: u32,
    /// A value of the base's plan, `NAME=VALUE`, filling each `@NAME@` of
    /// its request paths, header values, bodies and message files; repeat
    /// it for each name.
    #[arg(long = "base-var", value_name = "NAME=VALUE")]
    base_vars: Vec<String>,
    /// A value of the head's plan, as `--base-var` is of the base's.
    #[arg(long = "head-var", value_name = "NAME=VALUE")]
    head_vars: Vec<String>,
    /// The directory the JSON reports are written to.
    #[arg(long, value_name = "DIR", default_value = "target/perf")]
    out_dir: PathBuf,
    /// A file the Markdown summary is also written to.
    #[arg(long, value_name = "PATH")]
    summary: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct CompareArgs {
    /// The saved JSON report.
    #[arg(long, value_name = "PATH")]
    report: PathBuf,
    #[command(flatten)]
    output: Output,
}

/// Parses `--pairs`: an even count of at least two, so the head runs first in
/// as many pairs as the base and a drift over the job leans on neither (#283).
fn even_pairs(value: &str) -> Result<u32, String> {
    let pairs: u32 = value
        .parse()
        .map_err(|error| format!("{value} is not a count of pairs: {error}"))?;
    if pairs >= 2 && pairs.is_multiple_of(2) {
        Ok(pairs)
    } else {
        Err(format!(
            "{pairs} is not an even count of at least 2: an odd count runs the head second in \
             most of the pairs, so a drift over the job leans against it"
        ))
    }
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
        Command::Ab(args) => ab_command(args),
        Command::Compare(args) => compare_command(&args),
        Command::Merge(args) => merge_command(&args),
    }
}

fn merge_command(args: &MergeArgs) -> Result<u8, Box<dyn Error>> {
    let into = Baselines::read(&args.baseline)?;
    let mut inputs = Vec::new();
    for job in &args.jobs {
        let file = Baselines::read(job)?
            .ok_or_else(|| format!("the job baseline {} does not exist", job.display()))?;
        inputs.push(file);
    }
    let written = jiff::Timestamp::now().to_string();
    let (file, merged) =
        ferrohealth_perf::baseline::merge(into, &inputs, env!("CARGO_PKG_VERSION"), &written)?;
    file.write(&args.baseline)?;
    println!("merged into {}:", args.baseline.display());
    for model in merged {
        println!("- the host {} over {} jobs", model.model, model.jobs);
    }
    Ok(0)
}

fn run_command(args: RunArgs) -> Result<u8, Box<dyn Error>> {
    let plan = Plan::read(&args.plan)?;
    let target = target(args.pid, args.cgroup, args.container, &args.cgroup_root)?;
    let options = RunOptions {
        plan,
        target,
        base_url: SecretUrl::new(args.base_url),
        mllp_address: args.mllp_address,
        database_url: args.database_url.map(SecretUrl::new),
        baseline: args.output.baseline.clone(),
        out_dir: args.out_dir,
        update: args.update,
        runs: args.runs.unwrap_or(DEFAULT_RUNS),
    };
    let result = runtime()?.block_on(run(&options))?;
    publish(&result.summary, args.output.summary.as_deref())?;
    println!("report: {}", result.report_path.display());
    Ok(result.verdict.outcome.code())
}

/// The server a process id, a cgroup or a container id names.
fn target(
    pid: Option<u32>,
    cgroup: Option<PathBuf>,
    container: Option<String>,
    cgroup_root: &Path,
) -> Result<Target, Box<dyn Error>> {
    Ok(match (pid, cgroup, container) {
        (Some(pid), _, _) => Target::Pid(pid),
        (_, Some(dir), _) => Target::Cgroup(dir),
        (_, _, Some(id)) => Target::Cgroup(container_cgroup(cgroup_root, &id)?),
        (None, None, None) => return Err("give a process id, a cgroup or a container".into()),
    })
}

fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
}

fn ab_command(args: AbArgs) -> Result<u8, Box<dyn Error>> {
    let values = ArmValues {
        base: Values::parse(&args.base_vars).map_err(|e| format!("--base-var: {e}"))?,
        head: Values::parse(&args.head_vars).map_err(|e| format!("--head-var: {e}"))?,
    };
    let options = AbOptions {
        plan: Plan::read(&args.plan)?,
        base: Server {
            target: target(
                args.base_pid,
                args.base_cgroup,
                args.base_container,
                &args.cgroup_root,
            )?,
            url: SecretUrl::new(args.base_url),
            database_url: args.base_database_url.map(SecretUrl::new),
            mllp_address: args.base_mllp_address,
        },
        head: Server {
            target: target(
                args.head_pid,
                args.head_cgroup,
                args.head_container,
                &args.cgroup_root,
            )?,
            url: SecretUrl::new(args.head_url),
            database_url: args.head_database_url.map(SecretUrl::new),
            mllp_address: args.head_mllp_address,
        },
        pairs: args.pairs,
        out_dir: args.out_dir,
    };
    let result = runtime()?.block_on(ferrohealth_perf::ab::run_with(&options, &values))?;
    publish(&result.summary, args.summary.as_deref())?;
    println!("report: {}", result.report_path.display());
    Ok(result.verdict.outcome.code())
}

fn compare_command(args: &CompareArgs) -> Result<u8, Box<dyn Error>> {
    let report = match Report::read(&args.report) {
        Ok(report) => report,
        // NOTE: no specification governs this: our own design; a file that is no run report is read as an A/B report, and its own error names it when it is neither.
        Err(ReportError::Parse { .. }) => {
            let report = AbReport::read(&args.report)?;
            let values = ferrohealth_perf::ab::recorded_values(&args.report)?;
            let verdict = ferrohealth_perf::ab::judge(&report)?;
            publish(
                &summary::ab_markdown_with_values(&report, &verdict, &values),
                args.output.summary.as_deref(),
            )?;
            return Ok(verdict.outcome.code());
        }
        Err(error) => return Err(error.into()),
    };
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
