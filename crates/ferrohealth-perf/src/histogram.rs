// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The latency histogram of a request group, and its encoding in a report.
//!
//! Latencies are recorded in microseconds into an `HdrHistogram` of three
//! significant figures. A report stores each histogram in the V2
//! serialization, base64-wrapped, so a comparison can re-derive every
//! quantile from the report alone
//! (<https://docs.rs/hdrhistogram/7.6.0/hdrhistogram/serialization/>).
//! Percentiles of several histograms are read off their merge, never
//! averaged.

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hdrhistogram::Histogram;
use hdrhistogram::serialization::{Deserializer, Serializer as _, V2Serializer};
use thiserror::Error;

/// The highest latency a histogram tracks, in microseconds: ten minutes. A
/// longer one is recorded at this value.
pub const HIGHEST_US: u64 = 600_000_000;

/// The significant figures every histogram keeps.
pub const SIGNIFICANT_FIGURES: u8 = 3;

/// A histogram that cannot be built, merged, encoded or decoded.
#[derive(Debug, Error)]
pub enum HistogramError {
    /// Two histograms could not be merged.
    #[error("the histograms cannot be merged")]
    Merge(#[source] hdrhistogram::AdditionError),
    /// The V2 serialization failed.
    #[error("the histogram cannot be encoded")]
    Encode(#[source] hdrhistogram::serialization::V2SerializeError),
    /// The text is not base64.
    #[error("the histogram encoding is not base64")]
    Base64(#[source] NotBase64),
    /// The bytes are not a V2 serialization.
    #[error("the histogram encoding does not decode")]
    Decode(#[source] hdrhistogram::serialization::DeserializeError),
}

/// Why a histogram's text is not base64: the decoder's own account, which
/// names the offending symbol or length.
///
/// The base64 decoder stays behind this type, so the crate's API names no
/// type of it and a move of its line is not a break.
#[derive(Debug, Error)]
#[error(transparent)]
pub struct NotBase64(base64::DecodeError);

/// Builds an empty latency histogram over microseconds.
///
/// # Panics
///
/// Never: the bounds are constants the histogram accepts.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "hdrhistogram refuses only a low bound below one, a high bound below twice the low \
              one, or more than five significant figures; the constants are none of these"
)]
pub fn empty() -> Histogram<u64> {
    Histogram::new_with_bounds(1, HIGHEST_US, SIGNIFICANT_FIGURES)
        .expect("the latency bounds should be accepted")
}

/// Microseconds in `duration`, saturating at `u64::MAX`.
#[must_use]
pub fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

/// Records `duration` into `histogram`, clamped to [`HIGHEST_US`].
pub fn record(histogram: &mut Histogram<u64>, duration: Duration) {
    histogram.saturating_record(micros(duration).clamp(1, HIGHEST_US));
}

/// Adds every count of `from` into `into`.
///
/// # Errors
///
/// [`HistogramError::Merge`] when the two do not share bounds.
pub fn merge(into: &mut Histogram<u64>, from: &Histogram<u64>) -> Result<(), HistogramError> {
    into.add(from).map_err(HistogramError::Merge)
}

/// Encodes `histogram` in the V2 serialization, base64-wrapped.
///
/// # Errors
///
/// [`HistogramError::Encode`] when the serializer refuses it.
pub fn encode(histogram: &Histogram<u64>) -> Result<String, HistogramError> {
    let mut bytes = Vec::new();
    V2Serializer::new()
        .serialize(histogram, &mut bytes)
        .map_err(HistogramError::Encode)?;
    Ok(STANDARD.encode(&bytes))
}

/// Decodes a base64-wrapped V2 serialization.
///
/// # Errors
///
/// [`HistogramError::Base64`] or [`HistogramError::Decode`] for text that is
/// not one.
pub fn decode(text: &str) -> Result<Histogram<u64>, HistogramError> {
    let bytes = STANDARD
        .decode(text)
        .map_err(|e| HistogramError::Base64(NotBase64(e)))?;
    Deserializer::new()
        .deserialize(&mut bytes.as_slice())
        .map_err(HistogramError::Decode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_histogram_round_trips_through_its_encoding() {
        let mut histogram = empty();
        for value in [1_u64, 10, 100, 1_000, 10_000] {
            record(&mut histogram, Duration::from_micros(value));
        }
        let decoded = decode(&encode(&histogram).unwrap()).unwrap();
        assert_eq!(histogram, decoded);
        assert!(matches!(
            decode("not base64!"),
            Err(HistogramError::Base64(_))
        ));
        assert!(matches!(decode("AAAA"), Err(HistogramError::Decode(_))));
    }

    #[test]
    fn text_that_is_not_base64_carries_the_decoder_account_as_its_cause() {
        let error = decode("not base64!").unwrap_err();
        let cause = std::error::Error::source(&error).unwrap();
        assert!(cause.downcast_ref::<NotBase64>().is_some());
        assert!(cause.to_string().contains("Invalid symbol"));
    }

    #[test]
    fn a_latency_past_the_bound_is_recorded_at_the_bound() {
        let mut histogram = empty();
        record(&mut histogram, Duration::from_secs(3_600));
        record(&mut histogram, Duration::ZERO);
        assert_eq!(2, histogram.len());
        assert!(
            histogram.max() >= HIGHEST_US,
            "the bound is the largest value"
        );
        assert_eq!(1, histogram.min(), "zero is recorded as one microsecond");
    }

    #[test]
    fn a_merge_counts_both() {
        let mut a = empty();
        let mut b = empty();
        record(&mut a, Duration::from_millis(1));
        record(&mut b, Duration::from_millis(2));
        merge(&mut a, &b).unwrap();
        assert_eq!(2, a.len());
    }
}
