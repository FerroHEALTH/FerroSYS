// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The DATE input (§6.1) and the timestamp output (§7.10).

use jiff::Timestamp;

use crate::sop::exit::{Exit, Failure};

/// A bound of the verification window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Bound {
    /// `-`: no limit on this side.
    Open,
    /// A point in time.
    At(Timestamp),
}

/// Parses a DATE: `now`, `-` or an ISO 8601 timestamp with a zone, in the
/// extended or the basic format.
pub(crate) fn parse(text: &str) -> Result<Bound, Failure> {
    match text {
        "-" => Ok(Bound::Open),
        "now" => Ok(Bound::At(Timestamp::now())),
        _ => text.parse::<Timestamp>().map(Bound::At).map_err(|error| {
            Failure::new(
                Exit::BadData,
                format!("{text} is not an ISO 8601 date with a time zone: {error}"),
            )
        }),
    }
}

/// A signature creation time as §7.10 writes it: UTC, to the second, with
/// the `Z` suffix.
pub(crate) fn render(seconds: u32) -> Result<String, Failure> {
    Timestamp::from_second(i64::from(seconds))
        .map(|time| time.strftime("%Y-%m-%dT%H:%M:%SZ").to_string())
        .map_err(|error| Failure::new(Exit::UnspecifiedFailure, error.to_string()))
}
