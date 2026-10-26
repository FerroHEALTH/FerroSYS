// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The fields a product adds to its request line, and the line that carries
//! them.
//!
//! `tracing` names an event's fields at its callsite, so a line with fields
//! a product names at run time goes through a callsite built for that set of
//! names: one per level and set, built once and kept for the process. The
//! names are `&'static str` the product writes in its code, so the sets are
//! few.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{LazyLock, Mutex, OnceLock, PoisonError};

use tracing::callsite::{Callsite, Identifier};
use tracing::field::{Field, FieldSet, Value};
use tracing::metadata::Kind;
use tracing::subscriber::Interest;
use tracing::{Event, Level, Metadata};

use super::TARGET;

/// The most fields a product adds to one line.
pub const MAX_FIELDS: usize = 8;

/// The longest field value, in bytes.
pub const MAX_TOKEN_LENGTH: usize = 128;

/// The longest field name, in bytes.
pub const MAX_NAME_LENGTH: usize = 32;

/// The fields the request log writes itself, which no product field may
/// take.
pub const RESERVED: [&str; 8] = [
    "message",
    "method",
    "route",
    "status",
    "latency_ms",
    "query",
    "request_id",
    "client_named",
];

/// The length of the value set of a line: the request log's fields and the
/// most a product adds.
const SLOTS: usize = RESERVED.len() + MAX_FIELDS;

/// A field value the product vouches for: from 1 to [`MAX_TOKEN_LENGTH`]
/// visible ASCII characters, with no space and no control character, such
/// as a client id, an identifier domain or a code system URI.
///
/// The shape check keeps a line parseable; that the value names no patient
/// is the product's promise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// Returns `value` as a token.
    ///
    /// # Errors
    ///
    /// Returns [`FieldError::Value`] when `value` is empty, longer than
    /// [`MAX_TOKEN_LENGTH`], or holds a character outside visible ASCII.
    pub fn new(value: impl Into<String>) -> Result<Self, FieldError> {
        let value = value.into();
        let shaped = !value.is_empty()
            && value.len() <= MAX_TOKEN_LENGTH
            && value.bytes().all(|byte| byte.is_ascii_graphic());
        if shaped {
            Ok(Self(value))
        } else {
            Err(FieldError::Value)
        }
    }

    /// Returns the value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A product field the request line cannot carry.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum FieldError {
    /// The name is empty, longer than [`MAX_NAME_LENGTH`], or holds a
    /// character outside `a` to `z`, `0` to `9`, `_` and `.`.
    #[error("the line field name {name:?} is not a lower-case identifier")]
    Name {
        /// The name.
        name: &'static str,
    },
    /// The name is one the request log writes itself ([`RESERVED`]).
    #[error("the line field name {name:?} is the request log's own")]
    Reserved {
        /// The name.
        name: &'static str,
    },
    /// The line holds [`MAX_FIELDS`] product fields already.
    #[error("the line holds {MAX_FIELDS} product fields already")]
    Full,
    /// The value is not a [`Token`].
    #[error("the line field value is not a short visible-ASCII token")]
    Value,
}

/// The fields a handler adds to its request's line, each a name the product
/// writes in its code and a [`Token`].
///
/// Put it in the response's extensions; the request log reads it when the
/// response passes and writes every field as a field of the line, in the
/// order the product added them. The fields reach the line only, never the
/// span.
///
/// # Examples
///
/// ```
/// use ferrohealth_telemetry::request_log::fields::{LineFields, Token};
///
/// let fields = LineFields::new()
///     .with("caller", Token::new("ferrofed-console")?)?
///     .with("domain", Token::new("2.16.840.1.113883.2.4.6.3")?)?;
/// assert_eq!(2, fields.len());
/// # Ok::<(), ferrohealth_telemetry::request_log::fields::FieldError>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineFields(Vec<(&'static str, Token)>);

impl LineFields {
    /// No fields.
    #[must_use]
    pub const fn new() -> Self {
        Self(Vec::new())
    }

    /// Returns these fields with `name` set to `value`.
    ///
    /// # Errors
    ///
    /// As [`LineFields::insert`].
    pub fn with(mut self, name: &'static str, value: Token) -> Result<Self, FieldError> {
        self.insert(name, value)?;
        Ok(self)
    }

    /// Sets `name` to `value`, replacing the value a field of that name
    /// held.
    ///
    /// # Errors
    ///
    /// Returns [`FieldError::Name`] for a name that is not a lower-case
    /// identifier, [`FieldError::Reserved`] for a name the request log
    /// writes itself, and [`FieldError::Full`] for a new name when the line
    /// holds [`MAX_FIELDS`] fields.
    pub fn insert(&mut self, name: &'static str, value: Token) -> Result<(), FieldError> {
        let shaped = !name.is_empty()
            && name.len() <= MAX_NAME_LENGTH
            && name.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'.'
            });
        if !shaped {
            return Err(FieldError::Name { name });
        }
        if RESERVED.contains(&name) {
            return Err(FieldError::Reserved { name });
        }
        if let Some(slot) = self.0.iter_mut().find(|(held, _)| *held == name) {
            slot.1 = value;
            return Ok(());
        }
        if self.0.len() >= MAX_FIELDS {
            return Err(FieldError::Full);
        }
        self.0.push((name, value));
        Ok(())
    }

    /// Returns how many fields there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the fields in the order they were added.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &str)> + '_ {
        self.0.iter().map(|(name, value)| (*name, value.as_str()))
    }
}

/// The request log's own values of one line, in the order of [`RESERVED`]
/// after the message.
pub(super) struct Core<'a> {
    pub(super) method: &'a str,
    pub(super) route: &'a str,
    pub(super) status: u16,
    pub(super) latency_ms: f64,
    pub(super) query: &'a str,
    pub(super) request_id: &'a str,
    pub(super) client_named: bool,
}

/// Writes the line of `core` and `fields` at `level`.
pub(super) fn write(level: Level, core: &Core<'_>, fields: &LineFields) {
    let callsite = callsite(level, fields);
    let metadata = callsite.metadata();
    tracing::dispatcher::get_default(|dispatch| {
        if !dispatch.enabled(metadata) {
            return;
        }
        let set = metadata.fields();
        let all: Vec<Field> = set.iter().collect();
        let Some(first) = all.first() else {
            return;
        };
        let message = "request";
        let mut values: Vec<&dyn Value> = vec![
            &message,
            &core.method,
            &core.route,
            &core.status,
            &core.latency_ms,
            &core.query,
            &core.request_id,
            &core.client_named,
        ];
        let tokens: Vec<&str> = fields.iter().map(|(_, value)| value).collect();
        values.extend(tokens.iter().map(|token| -> &dyn Value { token }));
        let slots: [(&Field, Option<&dyn Value>); SLOTS] =
            std::array::from_fn(|slot| match (all.get(slot), values.get(slot)) {
                (Some(field), Some(value)) => (field, Some(*value)),
                _ => (first, None),
            });
        dispatch.event(&Event::new(metadata, &set.value_set(&slots)));
    });
}

/// A callsite of the request line for one level and one set of field names.
struct LineCallsite {
    metadata: OnceLock<Metadata<'static>>,
}

impl Callsite for LineCallsite {
    fn set_interest(&self, _interest: Interest) {
        // NOTE: no specification governs this: our own design; the line asks the
        // dispatcher at every write, so a cached interest is never read.
    }

    #[expect(
        clippy::expect_used,
        reason = "callsite() sets the metadata before the callsite leaves it"
    )]
    fn metadata(&self) -> &Metadata<'_> {
        self.metadata
            .get()
            .expect("the line callsite's metadata should have been set at its creation")
    }
}

/// The key of a line callsite: the level's rank and the product's names.
type Key = (u8, Vec<&'static str>);

/// The line callsites built so far.
static CALLSITES: LazyLock<Mutex<BTreeMap<Key, &'static LineCallsite>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

/// Returns the callsite of the line at `level` with `fields`' names,
/// building and registering it on first use.
fn callsite(level: Level, fields: &LineFields) -> &'static LineCallsite {
    let names: Vec<&'static str> = fields.iter().map(|(name, _)| name).collect();
    let rank = match level {
        Level::ERROR => 0,
        Level::WARN => 1,
        _ => 2,
    };
    let mut callsites = CALLSITES.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(callsite) = callsites.get(&(rank, names.clone())) {
        return callsite;
    }
    let all: Vec<&'static str> = RESERVED
        .iter()
        .copied()
        .chain(names.iter().copied())
        .collect();
    let all: &'static [&'static str] = Box::leak(all.into_boxed_slice());
    let callsite: &'static LineCallsite = Box::leak(Box::new(LineCallsite {
        metadata: OnceLock::new(),
    }));
    let _set: Result<(), Metadata<'static>> = callsite.metadata.set(Metadata::new(
        "request",
        TARGET,
        level,
        Some(file!()),
        Some(line!()),
        Some(module_path!()),
        FieldSet::new(all, Identifier(callsite)),
        Kind::EVENT,
    ));
    tracing::callsite::register(callsite);
    callsites.insert((rank, names), callsite);
    callsite
}
