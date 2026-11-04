// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! Which query parameters a request line logs, and how much of each value.

use super::{MAX_VALUE_LENGTH, Settings};

/// Returns the allowed `key=value` pairs of `query` whose value has its
/// shape, each value cut to [`MAX_VALUE_LENGTH`] characters, joined by
/// spaces.
pub(super) fn allowed_parameters(query: &str, settings: &Settings) -> String {
    let mut out = String::new();
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        if !settings
            .logged_query_parameters
            .iter()
            .any(|name| name == key)
        {
            continue;
        }
        let shape = settings
            .shapes
            .get(key)
            .copied()
            .unwrap_or(settings.default_shape);
        let Some(value) = shape.admit(value) else {
            continue;
        };
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(key);
        out.push('=');
        out.extend(value.chars().take(MAX_VALUE_LENGTH));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::allowed_parameters;
    use crate::request_log::{MAX_VALUE_LENGTH, RequestLog, ValueShape};

    fn logged(log: &RequestLog, query: &str) -> String {
        allowed_parameters(query, &log.settings)
    }

    #[test]
    fn nothing_is_logged_when_the_allowlist_is_empty() {
        assert_eq!(
            "",
            logged(&RequestLog::default(), "subject_id=1234&_count=10")
        );
    }

    #[test]
    fn only_an_allowed_parameter_contributes_and_its_value_is_cut() {
        let count = RequestLog::new(&["_count".to_owned()]);
        assert_eq!("_count=10", logged(&count, "subject_id=1234&_count=10"));
        let long = "x".repeat(MAX_VALUE_LENGTH + 10);
        assert_eq!(
            format!("_count={}", "x".repeat(MAX_VALUE_LENGTH)),
            logged(&count, &format!("_count={long}"))
        );
        assert_eq!("", logged(&count, "_count"));
    }

    #[test]
    fn a_value_of_the_wrong_shape_is_left_out_with_its_key() {
        let paging =
            RequestLog::new(&["offset".to_owned(), "fetch".to_owned(), "_count".to_owned()])
                .with_query_shape("offset", ValueShape::Digits { max: 10 })
                .with_query_shape("fetch", ValueShape::Digits { max: 10 });
        assert_eq!(
            "offset=20 fetch=10 _count=Qz7text",
            logged(&paging, "offset=20&fetch=10&_count=Qz7text&subject=1")
        );
        for value in [
            "SYNTHETIC-1",
            "",
            "12345678901",
            "-1",
            "+1",
            "1 0",
            "%31",
            "１",
        ] {
            assert_eq!(
                "fetch=5",
                logged(&paging, &format!("offset={value}&fetch=5")),
                "{value:?}"
            );
        }
        let digits = RequestLog::new(&["offset".to_owned(), "_count".to_owned()])
            .with_default_query_shape(ValueShape::Digits { max: 3 })
            .with_query_shape("_count", ValueShape::Text);
        assert_eq!(
            "offset=123 _count=Qz7text",
            logged(&digits, "offset=123&_count=Qz7text")
        );
        assert_eq!("", logged(&digits, "offset=1234"));
        assert_eq!(
            "",
            logged(
                &RequestLog::default().with_query_shape("offset", ValueShape::Digits { max: 10 }),
                "offset=20"
            ),
            "a shape admits no name on its own"
        );
    }
}
