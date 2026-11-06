// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The transactions of the server's database, read from
//! `pg_stat_database` and witnessed in `pg_stat_activity`.
//!
//! The count is `xact_commit` plus `xact_rollback` of the database the URL
//! names (PostgreSQL 18, "The Cumulative Statistics System",
//! `pg_stat_database`). A backend reports its counts when it goes idle, at
//! most once a second while busy and within ten seconds once idle, so a
//! phase waits for its settle time before its last reading. A backend
//! reports its transaction count only with statistics it has pending, so a
//! transaction that reads or writes no relation (`SELECT 1`) is counted
//! only when that backend next touches one; a loop that polls a table, such
//! as an outbox sender, is counted within a second (PostgreSQL 18 source,
//! `pgstat.c` `pgstat_report_stat`). Each reading is one transaction of its
//! own, so the tool adds at most two to a phase.
//!
//! The idle phase also watches the database's other client backends in
//! `pg_stat_activity` ([`Transactions::witness`]), whose `xact_start` and
//! `query_start` a backend sets as each transaction and statement starts,
//! with no statistics pending. A transaction is witnessed when a sample
//! shows one that started after the last one witnessed for its backend,
//! so the count is a lower bound: at most one per backend per sample, with
//! none counted twice. The watcher has a connection of its own whose
//! statement reads no relation, so its own transactions stay out of
//! `pg_stat_database` until the run ends. Reading another role's
//! backends takes that role, a superuser or `pg_read_all_stats`.
//!
//! The connections are plain: the database is the one the CI job starts
//! beside the server.

use std::collections::BTreeMap;
use std::time::Duration;

use ferrohealth_secret::secret_url::SecretUrl;
use thiserror::Error;
use tokio::time::{Instant, sleep};
use tokio_postgres::{Client, NoTls, Statement};

/// How often the idle phase samples `pg_stat_activity`.
pub(crate) const WITNESS_INTERVAL: Duration = Duration::from_millis(50);

/// The sample of the database's other client backends: the server's time
/// and, per backend, when its transaction and its statement started, each
/// in microseconds since the epoch. It scans a function and no relation.
const SAMPLE: &str = "SELECT (extract(epoch FROM clock_timestamp()) * 1000000)::bigint, \
     a.pid, (extract(epoch FROM a.xact_start) * 1000000)::bigint, \
     (extract(epoch FROM a.query_start) * 1000000)::bigint \
     FROM (VALUES (1)) AS one (x) \
     LEFT JOIN pg_stat_get_activity(NULL) AS a \
     ON a.datid = $1 AND a.backend_type = 'client backend' AND a.pid <> ALL ($2)";

/// A database that cannot be reached or read.
#[derive(Debug, Error)]
pub enum DatabaseError {
    /// The connection failed.
    #[error("cannot connect to the database {url}")]
    Connect {
        /// The URL, its credentials redacted.
        url: String,
        /// The cause.
        #[source]
        source: tokio_postgres::Error,
    },
    /// A statistics query, of `pg_stat_database` or `pg_stat_activity`,
    /// failed.
    #[error("cannot read the database's statistics")]
    Query(#[source] tokio_postgres::Error),
    /// The statistics hold no row for the database.
    #[error("pg_stat_database holds no row for the database {0}")]
    NoRow(String),
    /// The count read is negative, which `pg_stat_database` never holds.
    #[error("pg_stat_database counts {0} transactions")]
    Negative(i64),
}

/// The connections that count the transactions of one database.
#[derive(Debug)]
pub struct Transactions {
    client: Client,
    name: String,
    /// The watcher's own connection, kept open for the whole run.
    watcher: Client,
    /// The prepared [`SAMPLE`].
    sample: Statement,
    /// The database's object id.
    oid: u32,
    /// The process ids of the tool's two backends.
    own: Vec<i32>,
}

impl Transactions {
    /// Connects to the database at `url`, once to read its statistics and
    /// once to watch its activity.
    ///
    /// # Errors
    ///
    /// [`DatabaseError::Connect`] when a connection fails, and
    /// [`DatabaseError::Query`] when the database cannot be named or the
    /// watcher's statement cannot be prepared.
    pub async fn connect(url: &SecretUrl) -> Result<Self, DatabaseError> {
        let client = open(url).await?;
        let row = client
            .query_one(
                "SELECT current_database(), oid, pg_backend_pid() FROM pg_database \
                 WHERE datname = current_database()",
                &[],
            )
            .await
            .map_err(DatabaseError::Query)?;
        let name: String = row.try_get(0).map_err(DatabaseError::Query)?;
        let oid: u32 = row.try_get(1).map_err(DatabaseError::Query)?;
        let reader: i32 = row.try_get(2).map_err(DatabaseError::Query)?;
        let watcher = open(url).await?;
        let sample = watcher
            .prepare(SAMPLE)
            .await
            .map_err(DatabaseError::Query)?;
        // NOTE: PostgreSQL 18, "Statistics Functions": pg_stat_force_next_flush
        // reports the preparation's statistics now, before any phase reads.
        let row = watcher
            .query_one("SELECT pg_backend_pid(), pg_stat_force_next_flush()", &[])
            .await
            .map_err(DatabaseError::Query)?;
        let watching: i32 = row.try_get(0).map_err(DatabaseError::Query)?;
        Ok(Self {
            client,
            name,
            watcher,
            sample,
            oid,
            own: vec![reader, watching],
        })
    }

    /// The name of the database read.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The committed and rolled-back transactions of the database since its
    /// statistics were last reset.
    ///
    /// # Errors
    ///
    /// [`DatabaseError::Query`] when the query fails,
    /// [`DatabaseError::NoRow`] when the statistics hold no row for it, and
    /// [`DatabaseError::Negative`] for a negative count.
    pub async fn count(&self) -> Result<u64, DatabaseError> {
        let row = self
            .client
            .query_opt(
                "SELECT (xact_commit + xact_rollback)::bigint FROM pg_stat_database \
                 WHERE datname = $1",
                &[&self.name],
            )
            .await
            .map_err(DatabaseError::Query)?
            .ok_or_else(|| DatabaseError::NoRow(self.name.clone()))?;
        let count: i64 = row.try_get(0).map_err(DatabaseError::Query)?;
        u64::try_from(count).map_err(|_negative| DatabaseError::Negative(count))
    }

    /// The transactions the database's other client backends were seen to
    /// start while `pg_stat_activity` was sampled every `every` for
    /// `duration`: a lower bound that counts the transactions which touch
    /// no relation as well.
    ///
    /// # Errors
    ///
    /// [`DatabaseError::Query`] when a sample cannot be read.
    pub async fn witness(&self, duration: Duration, every: Duration) -> Result<u64, DatabaseError> {
        let started = Instant::now();
        let mut witness = Witness::default();
        loop {
            let rows = self
                .watcher
                .query(&self.sample, &[&self.oid, &self.own])
                .await
                .map_err(DatabaseError::Query)?;
            let mut now = None;
            let mut backends = Vec::with_capacity(rows.len());
            for row in &rows {
                now = Some(row.try_get::<_, i64>(0).map_err(DatabaseError::Query)?);
                if let Some(pid) = row
                    .try_get::<_, Option<i32>>(1)
                    .map_err(DatabaseError::Query)?
                {
                    backends.push((
                        pid,
                        Backend {
                            xact_start: row.try_get(2).map_err(DatabaseError::Query)?,
                            query_start: row.try_get(3).map_err(DatabaseError::Query)?,
                        },
                    ));
                }
            }
            if let Some(now) = now {
                witness.observe(now, backends);
            }
            let Some(left) = duration.checked_sub(started.elapsed()) else {
                break;
            };
            if left.is_zero() {
                break;
            }
            sleep(every.min(left)).await;
        }
        Ok(witness.count)
    }
}

/// Opens a plain connection to `url`, its driver on a task of its own.
async fn open(url: &SecretUrl) -> Result<Client, DatabaseError> {
    let (client, connection) =
        tokio_postgres::connect(url.expose(), NoTls)
            .await
            .map_err(|source| DatabaseError::Connect {
                url: url.redacted(),
                source,
            })?;
    // The connection future ends when the client is dropped; its error then
    // surfaces through the next query of the client.
    drop(tokio::spawn(connection));
    Ok(client)
}

/// One client backend as a sample shows it, its times in microseconds
/// since the epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Backend {
    /// When its transaction started, while it is in one.
    xact_start: Option<i64>,
    /// When its last statement started.
    query_start: Option<i64>,
}

/// What the samples know of one backend: the last sample of it, and the
/// start of the last transaction witnessed or seen before the window.
#[derive(Debug, Clone, Copy)]
struct Seen {
    last: Backend,
    mark: i64,
}

/// The transactions a run of samples witnessed.
#[derive(Debug, Default)]
struct Witness {
    backends: BTreeMap<i32, Seen>,
    /// The server's time at the first sample.
    start: Option<i64>,
    count: u64,
}

impl Witness {
    /// Takes the sample `backends`, read at the server's time `now`.
    fn observe(&mut self, now: i64, backends: Vec<(i32, Backend)>) {
        let first = self.start.is_none();
        let start = *self.start.get_or_insert(now);
        let mut next = BTreeMap::new();
        for (pid, current) in backends {
            let seen = match self.backends.get(&pid) {
                Some(seen) => self.step(seen.last, current, seen.mark),
                None if first => Seen {
                    last: current,
                    mark: current.xact_start.max(current.query_start).unwrap_or(start),
                },
                None => {
                    let unseen = Backend {
                        xact_start: None,
                        query_start: None,
                    };
                    self.step(unseen, current, start)
                }
            };
            next.insert(pid, seen);
        }
        self.backends = next;
    }

    /// Counts the transaction `current` shows started after `mark`: the
    /// one it is in, or, when it was in none at `previous` and is in none
    /// now, the one its newer statement ran in.
    fn step(&mut self, previous: Backend, current: Backend, mark: i64) -> Seen {
        let started = match (current.xact_start, previous.xact_start, current.query_start) {
            (Some(xact), _, _) if xact > mark => Some(xact),
            (None, None, Some(query)) if query > mark => Some(query),
            _ => None,
        };
        if started.is_some() {
            self.count = self.count.saturating_add(1);
        }
        Seen {
            last: current,
            mark: started.unwrap_or(mark),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Backend, Witness};

    fn idle(query: i64) -> Backend {
        Backend {
            xact_start: None,
            query_start: Some(query),
        }
    }

    fn within(xact: i64, query: i64) -> Backend {
        Backend {
            xact_start: Some(xact),
            query_start: Some(query),
        }
    }

    #[test]
    fn a_backend_at_rest_is_never_counted() {
        let mut witness = Witness::default();
        for now in [100, 150, 200] {
            witness.observe(now, vec![(7, idle(10))]);
        }
        assert_eq!(0, witness.count);
    }

    #[test]
    fn a_statement_loop_is_counted_once_per_sample() {
        let mut witness = Witness::default();
        witness.observe(100, vec![(7, idle(90))]);
        witness.observe(150, vec![(7, idle(140))]);
        witness.observe(200, vec![(7, within(195, 195))]);
        witness.observe(250, vec![(7, within(245, 245))]);
        assert_eq!(3, witness.count);
    }

    #[test]
    fn a_transaction_seen_twice_is_counted_once() {
        let mut witness = Witness::default();
        witness.observe(100, vec![(7, idle(90))]);
        // NOTE: PostgreSQL 18 source, postgres.c exec_simple_query: the statement
        // shows as running before its transaction starts, at the same instant.
        witness.observe(150, vec![(7, idle(140))]);
        witness.observe(200, vec![(7, within(140, 140))]);
        witness.observe(250, vec![(7, within(140, 245))]);
        assert_eq!(1, witness.count);
    }

    #[test]
    fn the_commit_of_a_seen_transaction_is_not_another() {
        let mut witness = Witness::default();
        witness.observe(100, vec![(7, idle(90))]);
        witness.observe(150, vec![(7, within(120, 145))]);
        witness.observe(200, vec![(7, idle(170))]);
        assert_eq!(1, witness.count);
    }

    #[test]
    fn a_backend_mid_transaction_at_the_start_is_not_counted() {
        let mut witness = Witness::default();
        witness.observe(100, vec![(7, within(50, 95))]);
        witness.observe(150, vec![(7, within(50, 140))]);
        assert_eq!(0, witness.count);
        witness.observe(200, vec![(7, within(180, 180))]);
        assert_eq!(1, witness.count);
    }

    #[test]
    fn a_backend_that_connects_in_the_window_is_counted() {
        let mut witness = Witness::default();
        witness.observe(100, Vec::new());
        witness.observe(150, vec![(9, idle(120))]);
        witness.observe(200, vec![(9, idle(120)), (11, idle(90))]);
        assert_eq!(1, witness.count, "a statement before the window is not");
    }
}
