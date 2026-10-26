// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! A value whose `Debug` and `Display` show none of it.
//!
//! Every FerroHEALTH type that holds clinical content or a patient
//! identifier renders through [`Redacted`], or by hand with the same rule,
//! so a `{:?}` in a log line or an error message shows the shape of a value
//! and never the value. The wrapper deliberately implements no `Serialize`:
//! a clinical payload is serialized where it is sent, through the inner
//! value, never by accident through a log.

use std::fmt;

use crate::REDACTED;

/// A value whose renderings name its type and print [`REDACTED`] in place of
/// the value.
///
/// `Debug` shows `Redacted<T>(***)` with the type's name as
/// [`std::any::type_name`] gives it, so a log line still says what kind of
/// value was there; `Display` shows [`REDACTED`]. The
/// value is reached through [`Redacted::expose`] or [`Redacted::into_inner`].
///
/// # Examples
///
/// ```
/// use ferrohealth_secret::redacted::Redacted;
///
/// #[derive(Debug)]
/// struct Lookup {
///     system: &'static str,
///     value: Redacted<String>,
/// }
///
/// let lookup = Lookup {
///     system: "urn:oid:2.16.840.1.113883.2.4.6.3",
///     value: Redacted::new("999911120".to_owned()),
/// };
/// let shown = format!("{lookup:?}");
/// assert!(!shown.contains("999911120"));
/// assert!(shown.contains("Redacted<") && shown.contains("String>(***)"));
/// assert_eq!(lookup.value.expose(), "999911120");
/// ```
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Redacted<T>(T);

impl<T> Redacted<T> {
    /// Wraps `value`.
    #[must_use]
    pub const fn new(value: T) -> Self {
        Self(value)
    }

    /// Returns the value, for the one place it is used.
    ///
    /// Never log or format the result.
    #[must_use]
    pub const fn expose(&self) -> &T {
        &self.0
    }

    /// Returns the value mutably, for the one place it is changed.
    #[must_use]
    pub const fn expose_mut(&mut self) -> &mut T {
        &mut self.0
    }

    /// Unwraps the value.
    #[must_use]
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> From<T> for Redacted<T> {
    fn from(value: T) -> Self {
        Self(value)
    }
}

impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Redacted<{}>({REDACTED})", std::any::type_name::<T>())
    }
}

impl<T> fmt::Display for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

#[cfg(test)]
mod tests {
    use super::Redacted;
    use crate::REDACTED;

    const IDENTIFIER: &str = "999911120";

    #[test]
    fn no_rendering_shows_the_value() {
        let value = Redacted::new(IDENTIFIER.to_owned());
        for shown in [
            format!("{value:?}"),
            format!("{value:#?}"),
            format!("{value}"),
        ] {
            assert!(
                !shown.contains(IDENTIFIER),
                "a rendering shows what it must hide"
            );
            assert!(
                shown.contains(REDACTED),
                "a rendering lacks what it must show"
            );
        }
    }

    #[test]
    fn debug_names_the_type() {
        assert_eq!("Redacted<u32>(***)", format!("{:?}", Redacted::new(7_u32)));
        assert_eq!("***", Redacted::new(7_u32).to_string());
    }

    #[test]
    fn a_nested_value_stays_hidden() {
        #[derive(Debug)]
        #[expect(
            dead_code,
            reason = "the fields are read only through Debug, which is the point"
        )]
        struct Observation {
            code: &'static str,
            text: Redacted<Vec<String>>,
        }
        let observation = Observation {
            code: "8480-6",
            text: Redacted::new(vec!["Systolic 182, patient Jansen".to_owned()]),
        };
        let shown = format!("{observation:#?}");
        assert!(
            !shown.contains("Jansen"),
            "a rendering shows what it must hide"
        );
        assert!(
            shown.contains("8480-6"),
            "a rendering lacks what it must show"
        );
    }

    #[test]
    fn the_value_is_reached_on_purpose() {
        let mut value = Redacted::from(IDENTIFIER.to_owned());
        assert_eq!(IDENTIFIER, value.expose());
        value.expose_mut().push('0');
        assert_eq!("9999111200", value.clone().into_inner());
        assert_eq!(value, Redacted::new("9999111200".to_owned()));
        assert_eq!(Redacted::<String>::default().into_inner(), "");
    }
}
