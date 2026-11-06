// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! What the integration tests share: the planted processes, the test
//! database and a short plan.
//!
//! A planted process is this test binary run again with one test selected,
//! [`crate::plant::planted_process`], and `FERROHEALTH_PERF_PLANT` naming
//! what it does: `quiet` sleeps, `spin` burns one core, and `commit`
//! polls an empty outbox table against `FERROHEALTH_PERF_PLANT_DATABASE`
//! in a loop, one transaction per poll. Each is killed when its guard drops, so a test stops only the
//! process it started.

use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

/// The variable that names what a planted process does.
pub(crate) const PLANT: &str = "FERROHEALTH_PERF_PLANT";

/// The variable that hands a `commit` plant its database.
pub(crate) const PLANT_DATABASE: &str = "FERROHEALTH_PERF_PLANT_DATABASE";

/// The variable that hands the tests a PostgreSQL superuser URL.
pub(crate) const DATABASE: &str = "FERROHEALTH_PERF_TEST_DATABASE_URL";

/// A process this test started, killed on drop.
#[derive(Debug)]
pub(crate) struct Planted(Child);

impl Planted {
    /// Starts a planted process doing `what`, with `database` for `commit`.
    pub(crate) fn start(what: &str, database: Option<&str>) -> Self {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "plant::planted_process",
                "--exact",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env(PLANT, what)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(url) = database {
            command.env(PLANT_DATABASE, url);
        }
        Self(command.spawn().unwrap())
    }

    /// Its process id.
    pub(crate) fn pid(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for Planted {
    fn drop(&mut self) {
        let _kill: std::io::Result<()> = self.0.kill();
        let _wait: std::io::Result<std::process::ExitStatus> = self.0.wait();
    }
}

/// The superuser URL of the test PostgreSQL, or `None` off CI when none is
/// given. On CI (`CI` set) a missing URL fails the test, so the database
/// cases never pass there without running.
pub(crate) fn database_url() -> Option<String> {
    match std::env::var(DATABASE) {
        Ok(url) if !url.is_empty() => Some(url),
        _ => {
            assert!(
                std::env::var_os("CI").is_none(),
                "{DATABASE} must name the test PostgreSQL on CI"
            );
            eprintln!("{DATABASE} is unset: this case needs a PostgreSQL and has nothing to do");
            None
        }
    }
}

static SEQUENCE: AtomicU32 = AtomicU32::new(0);

/// Creates a database of its own for one test and returns its URL, so the
/// transactions of other tests never reach its `pg_stat_database` row.
pub(crate) async fn fresh_database(admin: &str) -> String {
    let name = format!(
        "perf_{}_{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let (client, connection) = tokio_postgres::connect(admin, tokio_postgres::NoTls)
        .await
        .unwrap();
    let task = tokio::spawn(connection);
    client
        .batch_execute(&format!("CREATE DATABASE {name}"))
        .await
        .unwrap();
    drop(client);
    task.await.unwrap().unwrap();
    let mut url = url::Url::parse(admin).unwrap();
    url.set_path(&format!("/{name}"));
    url.to_string()
}

/// A plan of short phases over `requests`, TOML `[[requests]]` tables.
pub(crate) fn plan(requests: &str) -> String {
    format!(
        "[idle]\nduration_secs = 2\nsettle_secs = 1\n\
         [load]\nrate = 50.0\nwarmup_secs = 1\nduration_secs = 3\nmax_lag_ms = 50\n\
         settle_secs = 1\n{requests}"
    )
}

/// One request table.
pub(crate) fn request(group: &str, path: &str, status: u16) -> String {
    format!(
        "[[requests]]\ngroup = \"{group}\"\nmethod = \"GET\"\npath = \"{path}\"\nstatus = {status}\n"
    )
}
