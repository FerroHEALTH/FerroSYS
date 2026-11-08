// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! Plan values: an upper-case name between at signs, `@READ_ID@`, in a
//! request's path, header value or body, or in an HL7 v2 message file,
//! filled per run.
//!
//! An A/B run fills each arm's copy of the plan with that arm's values,
//! because a product whose seeded data gets a server-assigned id cannot
//! name the base's and the head's in one plan. Every placeholder needs a
//! value and every value a placeholder, so a misspelt name refuses the run
//! before any load. A name is an upper-case letter followed by upper-case
//! letters, digits and `_`; a value is inserted as written and is not
//! searched again. No specification governs the format: our own design
//! (#286).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::plan::{Plan, PlanError};

/// The values one run fills its plan with, by name.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Values {
    values: BTreeMap<String, String>,
}

impl fmt::Debug for Values {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.values.keys()).finish()
    }
}

/// Values that cannot fill a plan.
#[derive(Debug, Error)]
pub enum ValuesError {
    /// A value is not given as `NAME=VALUE` with a valid name.
    #[error(
        "{0:?} is not NAME=VALUE with NAME an upper-case letter followed by upper-case letters, \
         digits and '_'"
    )]
    Pair(String),
    /// A name is given twice.
    #[error("@{0}@ is given more than once")]
    Repeated(String),
    /// A placeholder of the plan has no value.
    #[error("the plan's placeholder @{0}@ has no value")]
    Missing(String),
    /// A value fills no placeholder of the plan.
    #[error("the value of {0} fills no placeholder of the plan")]
    Unused(String),
    /// An HL7 v2 message file of the plan cannot be read.
    #[error("cannot read the HL7 v2 message {path}")]
    Read {
        /// The file.
        path: String,
        /// The cause.
        #[source]
        source: std::io::Error,
    },
    /// The filled plan is refused.
    #[error("the filled plan is refused")]
    Plan(#[source] PlanError),
}

impl Values {
    /// Values from `NAME=VALUE` pairs.
    ///
    /// # Errors
    ///
    /// [`ValuesError::Pair`] for a pair without `=` or with a bad name, and
    /// [`ValuesError::Repeated`] for a name given twice.
    pub fn parse<S: AsRef<str>>(pairs: &[S]) -> Result<Self, ValuesError> {
        let mut values = BTreeMap::new();
        for pair in pairs {
            let pair = pair.as_ref();
            let (name, value) = pair
                .split_once('=')
                .filter(|(name, _)| is_name(name.as_bytes()))
                .ok_or_else(|| ValuesError::Pair(pair.to_owned()))?;
            if values.insert(name.to_owned(), value.to_owned()).is_some() {
                return Err(ValuesError::Repeated(name.to_owned()));
            }
        }
        Ok(Self { values })
    }

    /// Whether no value is given.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// The value of `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    /// The names and values, by name.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.values.iter().map(|(n, v)| (n.as_str(), v.as_str()))
    }

    /// `text` with every placeholder replaced by its value.
    ///
    /// # Errors
    ///
    /// [`ValuesError::Missing`] for a placeholder with no value.
    pub fn fill_bytes(&self, text: &[u8]) -> Result<Vec<u8>, ValuesError> {
        let mut out = Vec::with_capacity(text.len());
        let mut rest = text;
        while let Some((before, name, after)) = next_placeholder(rest) {
            out.extend_from_slice(before);
            let name = String::from_utf8_lossy(name);
            let value = self
                .get(&name)
                .ok_or_else(|| ValuesError::Missing(name.into_owned()))?;
            out.extend_from_slice(value.as_bytes());
            rest = after;
        }
        out.extend_from_slice(rest);
        Ok(out)
    }

    fn fill(&self, text: &str) -> Result<String, ValuesError> {
        let filled = self.fill_bytes(text.as_bytes())?;
        // NOTE: a placeholder is ASCII between two ASCII at signs, so replacing it with UTF-8 keeps UTF-8.
        Ok(String::from_utf8_lossy(&filled).into_owned())
    }

    /// `plan` with every placeholder of its requests filled, after checking
    /// that every placeholder of its requests and message files has a value
    /// and every value fills one. The message files themselves are filled
    /// when they are read for sending ([`crate::load::Endpoints::filled`]).
    ///
    /// # Errors
    ///
    /// [`ValuesError::Missing`], [`ValuesError::Unused`],
    /// [`ValuesError::Read`] for a message file that cannot be read, and
    /// [`ValuesError::Plan`] for a filled request the plan's checks refuse.
    pub fn fill_plan(&self, plan: &Plan) -> Result<Plan, ValuesError> {
        let mut found = BTreeSet::new();
        for request in &plan.requests {
            found.extend(placeholders(request.path.as_bytes()));
            for value in request.headers.values() {
                found.extend(placeholders(value.as_bytes()));
            }
            if let Some(body) = &request.body {
                found.extend(placeholders(body.as_bytes()));
            }
        }
        for message in &plan.messages {
            found.extend(placeholders(&read(&message.file)?));
        }
        if let Some(missing) = found.iter().find(|name| self.get(name).is_none()) {
            return Err(ValuesError::Missing(missing.clone()));
        }
        if let Some(unused) = self.values.keys().find(|name| !found.contains(*name)) {
            return Err(ValuesError::Unused(unused.clone()));
        }
        let mut filled = plan.clone();
        for request in &mut filled.requests {
            request.path = self.fill(&request.path)?;
            for value in request.headers.values_mut() {
                *value = self.fill(value)?;
            }
            if let Some(body) = &mut request.body {
                *body = self.fill(body)?;
            }
        }
        filled.check().map_err(ValuesError::Plan)?;
        Ok(filled)
    }
}

/// The bytes of the file at `path`.
pub(crate) fn read(path: &Path) -> Result<Vec<u8>, ValuesError> {
    std::fs::read(path).map_err(|source| ValuesError::Read {
        path: path.display().to_string(),
        source,
    })
}

/// The names of the placeholders in `text`.
#[must_use]
pub fn placeholders(text: &[u8]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut rest = text;
    while let Some((_, name, after)) = next_placeholder(rest) {
        names.insert(String::from_utf8_lossy(name).into_owned());
        rest = after;
    }
    names
}

fn is_name(name: &[u8]) -> bool {
    name.first().is_some_and(u8::is_ascii_uppercase)
        && name
            .iter()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || *b == b'_')
}

/// The text before the first placeholder of `text`, its name, and the text
/// after it.
fn next_placeholder(text: &[u8]) -> Option<(&[u8], &[u8], &[u8])> {
    let mut from: usize = 0;
    loop {
        let open = from.checked_add(text.get(from..)?.iter().position(|b| *b == b'@')?)?;
        let after_open = open.checked_add(1)?;
        let tail = text.get(after_open..)?;
        let length = tail
            .iter()
            .position(|b| !(b.is_ascii_uppercase() || b.is_ascii_digit() || *b == b'_'))
            .unwrap_or(tail.len());
        let name = tail.get(..length)?;
        if is_name(name) && tail.get(length) == Some(&b'@') {
            let before = text.get(..open)?;
            let after = tail.get(length.checked_add(1)?..)?;
            return Some((before, name, after));
        }
        from = after_open;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAN: &str = "[[requests]]\ngroup = \"read\"\nmethod = \"GET\"\n\
                        path = \"/fhir/Condition/@READ_ID@\"\nstatus = 200\n\
                        headers = { authorization = \"Bearer @TOKEN@\" }\n\
                        [[requests]]\ngroup = \"create\"\nmethod = \"POST\"\npath = \"/fhir/x\"\n\
                        status = 201\nbody = '{\"id\":\"@READ_ID@\",\"mail\":\"a@b.org\"}'\n";

    fn values(pairs: &[&str]) -> Values {
        Values::parse(pairs).unwrap()
    }

    #[test]
    fn placeholders_are_upper_case_names_between_at_signs() {
        let names = placeholders(b"@A@ @B_2@ a@b.org @lower@ @@ @9X@ @C@D@ @E");
        assert_eq!(
            vec!["A", "B_2", "C"],
            names.iter().map(String::as_str).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_value_is_inserted_once_and_never_searched_again() {
        let filled = values(&["A=@B@", "B=x"]).fill_bytes(b"<@A@|@B@>").unwrap();
        assert_eq!(b"<@B@|x>".as_slice(), filled.as_slice());
    }

    #[test]
    fn a_pair_needs_an_equals_sign_and_a_name() {
        for bad in ["READ_ID", "read_id=1", "=1", "9A=1", "A B=1"] {
            assert!(
                matches!(Values::parse(&[bad]), Err(ValuesError::Pair(_))),
                "{bad}"
            );
        }
        assert_eq!(Some("a=b"), values(&["A=a=b"]).get("A"));
        assert_eq!(Some(""), values(&["A="]).get("A"));
        assert!(matches!(
            Values::parse(&["A=1", "A=2"]),
            Err(ValuesError::Repeated(name)) if name == "A"
        ));
    }

    #[test]
    fn a_plan_is_filled_in_its_paths_headers_and_bodies() {
        let plan = Plan::parse(PLAN).unwrap();
        let filled = values(&["READ_ID=c-1", "TOKEN=t"])
            .fill_plan(&plan)
            .unwrap();
        assert_eq!("/fhir/Condition/c-1", filled.requests[0].path);
        assert_eq!("Bearer t", filled.requests[0].headers["authorization"]);
        assert_eq!(
            Some("{\"id\":\"c-1\",\"mail\":\"a@b.org\"}"),
            filled.requests[1].body.as_deref()
        );
        assert_eq!(plan.load, filled.load);
    }

    #[test]
    fn a_placeholder_without_a_value_and_a_value_without_a_placeholder_are_refused() {
        let plan = Plan::parse(PLAN).unwrap();
        assert!(matches!(
            values(&["READ_ID=c-1"]).fill_plan(&plan),
            Err(ValuesError::Missing(name)) if name == "TOKEN"
        ));
        assert!(matches!(
            values(&["READ_ID=c-1", "TOKEN=t", "SEARCH=s"]).fill_plan(&plan),
            Err(ValuesError::Unused(name)) if name == "SEARCH"
        ));
    }

    #[test]
    fn a_filled_request_the_plan_refuses_is_refused() {
        let plan = Plan::parse(PLAN).unwrap();
        assert!(matches!(
            values(&["READ_ID=c-1", "TOKEN=a\nb"]).fill_plan(&plan),
            Err(ValuesError::Plan(_))
        ));
    }

    #[test]
    fn a_placeholder_of_a_message_file_needs_a_value() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.hl7");
        std::fs::write(
            &file,
            "MSH|^~\\&|@SENDER@|F|R|F|20260101||ORU^R01|1|P|2.5\r",
        )
        .unwrap();
        let text = format!(
            "[mllp]\naddress = \"127.0.0.1:1\"\n[[messages]]\ngroup = \"oru\"\nfile = {:?}\n",
            file.display().to_string()
        );
        let plan = Plan::parse(&text).unwrap();
        assert!(matches!(
            Values::default().fill_plan(&plan),
            Err(ValuesError::Missing(name)) if name == "SENDER"
        ));
        assert!(values(&["SENDER=PERFLAB"]).fill_plan(&plan).is_ok());
    }

    #[test]
    fn debug_names_the_values_and_never_shows_them() {
        let shown = format!("{:?}", values(&["TOKEN=secret"]));
        assert!(
            shown.contains("TOKEN") && !shown.contains("secret"),
            "{shown}"
        );
    }
}
