// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The transactions of the server's database, read from
//! `pg_stat_database`.
//!
//! The count is `xact_commit` plus `xact_rollback` of the database the URL
//! names (PostgreSQL 18, "The Cumulative Statistics System",
//! `pg_stat_database`). A backend reports its counts when it goes idle, at
//! most once a second while busy and within ten seconds once idle, so a
//! phase waits for its settle time before its last reading. Each reading is
//! one transaction of its own, so the tool adds at most two to a phase.
//!
//! The connection is plain: the database is the one the CI job starts
//! beside the server.

use ferrohealth_secret::secret_url::SecretUrl;
use thiserror::Error;
use tokio_postgres::{Client, NoTls};

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
    /// The statistics query failed.
    #[error("cannot read pg_stat_database")]
    Query(#[source] tokio_postgres::Error),
    /// The statistics hold no row for the database.
    #[error("pg_stat_database holds no row for the database {0}")]
    NoRow(String),
    /// The count read is negative, which `pg_stat_database` never holds.
    #[error("pg_stat_database counts {0} transactions")]
    Negative(i64),
}

/// A connection that reads the transaction count of one database.
#[derive(Debug)]
pub struct Transactions {
    client: Client,
    name: String,
}

impl Transactions {
    /// Connects to the database at `url`.
    ///
    /// # Errors
    ///
    /// [`DatabaseError::Connect`] when the connection fails, and
    /// [`DatabaseError::Query`] when the database name cannot be read.
    pub async fn connect(url: &SecretUrl) -> Result<Self, DatabaseError> {
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
        let row = client
            .query_one("SELECT current_database()", &[])
            .await
            .map_err(DatabaseError::Query)?;
        let name: String = row.try_get(0).map_err(DatabaseError::Query)?;
        Ok(Self { client, name })
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
}
