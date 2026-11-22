// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! Per-arrival variables, so each arrival of one request sends values of
//! its own.
//!
//! The plan's `[vars.<name>]` tables are named as `{{name}}` or
//! `{{name.field}}` in a request's path, header values and body and in an
//! HL7 v2 message file.
//!
//! ```toml
//! [load]
//! seed = 7
//!
//! [vars.patient]
//! kind = "file"                 # a row of a JSON Lines or CSV file
//! path = "perf/patients.jsonl"
//! order = "random"              # or "cycle", the default: row after row
//!
//! [vars.n]
//! kind = "counter"              # start, start + step, ...
//! start = 1
//! width = 7                     # zero-padded to seven digits
//!
//! [[requests]]
//! group = "feed"
//! method = "PUT"
//! path = "/fhir/Patient?identifier=urn%3Aoid%3A2.999.1.1%7C{{patient.id}}"
//! status = 200
//! body = '{"resourceType":"Patient","identifier":[{"value":"{{patient.id}}"}]}'
//! ```
//!
//! The kinds are `counter`, `random` (an integer drawn evenly from `min` to
//! `max`), `uuid`, `list` (one of `values`) and `file`. Each arrival draws
//! one value of every variable its request names, so two places of one
//! request that name a variable carry the same value. A variable's sequence
//! is fixed by the plan's `[load] seed` and its own name, and continues
//! across the phases sent to one server, so a run is reproducible and a
//! later phase sends values an earlier one did not. The values are drawn on
//! the schedule's thread before the arrival's intended instant
//! ([`crate::load`]), so drawing them moves no send. A value is inserted as
//! written. No specification governs the format: our own design (#412).

mod data;
pub(crate) mod render;
mod rng;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::mllp::Message;
use crate::plan::Plan;
use data::{Format, Table};
use render::Renderer;
use rng::Rng;

/// One variable of the plan's `[vars]` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Var {
    /// `start`, then `start + step`, and so on.
    Counter(CounterVar),
    /// An integer drawn evenly from `min` to `max`, both included.
    Random(RandomVar),
    /// A version 4 UUID drawn from the seeded sequence, in its hyphenated
    /// lower-case form.
    Uuid(UuidVar),
    /// One of a list of values.
    List(ListVar),
    /// One row of a data file, named by its fields.
    File(FileVar),
}

/// A `counter` variable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CounterVar {
    /// The first value.
    pub start: u64,
    /// The step from one value to the next, at least one.
    pub step: u64,
    /// The digits a value is padded to with leading zeros.
    pub width: usize,
}

impl Default for CounterVar {
    fn default() -> Self {
        Self {
            start: 0,
            step: 1,
            width: 0,
        }
    }
}

/// A `random` variable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RandomVar {
    /// The least value.
    pub min: u64,
    /// The largest value.
    pub max: u64,
    /// The digits a value is padded to with leading zeros.
    #[serde(default)]
    pub width: usize,
}

/// A `uuid` variable, which takes no setting.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UuidVar {}

/// A `list` variable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListVar {
    /// The values, at least one.
    pub values: Vec<String>,
    /// The order they are drawn in.
    #[serde(default)]
    pub order: Order,
}

/// A `file` variable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileVar {
    /// The data file, `.jsonl` or `.csv`, read when the plan is.
    pub path: PathBuf,
    /// The order its rows are drawn in.
    #[serde(default)]
    pub order: Order,
}

/// The order a `list` or `file` variable draws in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Order {
    /// One after the other, from the first again after the last.
    #[default]
    Cycle,
    /// Each drawn evenly from all, from the seeded sequence.
    Random,
}

impl Var {
    fn kind(&self) -> &'static str {
        match self {
            Self::Counter(_) => "counter",
            Self::Random(_) => "random",
            Self::Uuid(_) => "uuid",
            Self::List(_) => "list",
            Self::File(_) => "file",
        }
    }

    /// Checks the settings of the variable.
    ///
    /// # Errors
    ///
    /// The reason, for a zero step, a `min` above `max`, an empty list, a
    /// listed value with a control character, or a data file that is not
    /// `.jsonl` or `.csv`.
    pub(crate) fn check(&self) -> Result<(), String> {
        match self {
            Self::Counter(counter) if counter.step == 0 => {
                Err("step must be at least 1".to_owned())
            }
            Self::Random(random) if random.min > random.max => {
                Err(format!("min {} lies above max {}", random.min, random.max))
            }
            Self::List(list) if list.values.is_empty() => Err("values is empty".to_owned()),
            Self::List(list) if list.values.iter().any(|v| v.chars().any(char::is_control)) => {
                Err("a value holds a control character".to_owned())
            }
            Self::File(file) if Format::of(&file.path).is_none() => Err(format!(
                "the data file {} is not .jsonl or .csv",
                file.path.display()
            )),
            _ => Ok(()),
        }
    }
}

/// Checks a variable's name: a lower-case letter, then lower-case letters,
/// digits and `_`.
///
/// # Errors
///
/// The reason, for any other name.
pub(crate) fn check_name(name: &str) -> Result<(), String> {
    if is_name(name.as_bytes()) {
        Ok(())
    } else {
        Err(format!(
            "the variable {name:?} must be a lower-case letter followed by lower-case letters, digits and '_'"
        ))
    }
}

fn is_name(name: &[u8]) -> bool {
    name.first().is_some_and(u8::is_ascii_lowercase)
        && name
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
}

fn is_field_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

/// A plan's variables that cannot be used.
#[derive(Debug, Error)]
pub enum TemplateError {
    /// A data file could not be read.
    #[error("cannot read the data file {path}")]
    Read {
        /// The file.
        path: String,
        /// The cause.
        #[source]
        source: std::io::Error,
    },
    /// A data file holds a row that cannot be used.
    #[error("the data file {path} is refused at line {line}: {reason}")]
    Data {
        /// The file.
        path: String,
        /// The line.
        line: usize,
        /// Why.
        reason: String,
    },
    /// A placeholder names no variable, or names it the wrong way.
    #[error("{place}: {reason}")]
    Placeholder {
        /// Where: the request or message and its part.
        place: String,
        /// Why.
        reason: String,
    },
    /// A variable no request or message names.
    #[error("the variable {0:?} fills no placeholder of the plan")]
    Unused(String),
    /// A counter ran past the largest value it can hold.
    #[error("the counter {0:?} ran past 18446744073709551615")]
    Overflow(String),
}

/// One `{{name}}` or `{{name.field}}` of a text, or the text between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Piece<'a> {
    Text(&'a [u8]),
    Slot {
        name: &'a str,
        field: Option<&'a str>,
    },
}

/// The pieces of `text`, every `{{` opening a placeholder.
fn pieces(text: &[u8]) -> Result<Vec<Piece<'_>>, String> {
    let mut pieces = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.windows(2).position(|pair| pair == b"{{") {
        let (before, tail) = rest.split_at_checked(open).unwrap_or((rest, &[]));
        let Some((name, field, after)) = slot(tail.get(2..).unwrap_or_default()) else {
            let at = text.len().saturating_sub(tail.len());
            return Err(format!(
                "the '{{{{' at byte {at} opens no placeholder {{{{name}}}} or {{{{name.field}}}}"
            ));
        };
        if !before.is_empty() {
            pieces.push(Piece::Text(before));
        }
        pieces.push(Piece::Slot { name, field });
        rest = after;
    }
    if !rest.is_empty() {
        pieces.push(Piece::Text(rest));
    }
    Ok(pieces)
}

/// The name, the field and the text after the closing `}}` of the
/// placeholder whose `{{` came just before `inner`.
fn slot(inner: &[u8]) -> Option<(&str, Option<&str>, &[u8])> {
    let length = inner
        .iter()
        .position(|b| !(b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_'))
        .unwrap_or(inner.len());
    let (name, rest) = inner.split_at_checked(length)?;
    if !is_name(name) {
        return None;
    }
    let (field, rest) = match rest.strip_prefix(b".") {
        Some(tail) => {
            let length = tail
                .iter()
                .position(|b| !is_field_byte(*b))
                .unwrap_or(tail.len());
            let (field, rest) = tail.split_at_checked(length)?;
            if field.is_empty() {
                return None;
            }
            (Some(std::str::from_utf8(field).ok()?), rest)
        }
        None => (None, rest),
    };
    let rest = rest.strip_prefix(b"}}")?;
    Some((std::str::from_utf8(name).ok()?, field, rest))
}

/// Checks the placeholders of `text` against the declared `vars`: each
/// names a variable, with a field when it is a `file` variable and with
/// none otherwise.
///
/// # Errors
///
/// The reason, for a `{{` that opens no placeholder or a placeholder that
/// names no variable or names it the wrong way.
pub(crate) fn check_placeholders(text: &str, vars: &BTreeMap<String, Var>) -> Result<(), String> {
    for piece in pieces(text.as_bytes())? {
        if let Piece::Slot { name, field } = piece {
            slot_var(name, field, vars)?;
        }
    }
    Ok(())
}

fn slot_var<'a>(
    name: &str,
    field: Option<&str>,
    vars: &'a BTreeMap<String, Var>,
) -> Result<&'a Var, String> {
    let var = vars
        .get(name)
        .ok_or_else(|| format!("{{{{{name}}}}} names no variable of [vars]"))?;
    match (var, field) {
        (Var::File(_), None) => Err(format!(
            "{{{{{name}}}}} names a file variable, whose placeholders name a field: {{{{{name}.<field>}}}}"
        )),
        (Var::File(_), Some(_)) | (_, None) => Ok(var),
        (other, Some(field)) => Err(format!(
            "{{{{{name}.{field}}}}} names a field of a {} variable, which has none",
            other.kind()
        )),
    }
}

/// One part of a compiled text: bytes as written, or the value of a
/// variable (of one field of its row, for a `file` variable).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Text(Vec<u8>),
    Slot { var: usize, column: Option<usize> },
}

/// A text with its placeholders resolved to variables.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Template {
    parts: Vec<Part>,
}

/// The value one arrival drew of one variable.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Drawn {
    Text(String),
    Row(usize),
}

/// A variable, ready to draw from.
#[derive(Debug)]
enum Source {
    Counter(CounterVar),
    Random(RandomVar),
    Uuid,
    List(ListVar),
    File { table: Table, order: Order },
}

/// Where each variable of a plan is in its sequence.
#[derive(Debug)]
pub(crate) struct Draws {
    states: Vec<(u64, Rng)>,
}

/// The templated parts of one request: each `None` is sent as written.
#[derive(Debug)]
struct RequestTemplate {
    path: Option<Template>,
    headers: BTreeMap<String, Template>,
    body: Option<Template>,
    uses: BTreeSet<usize>,
}

/// The templated halves of one HL7 v2 message, around its MSH-10.
#[derive(Debug)]
struct MessageTemplate {
    before: Template,
    after: Template,
    uses: BTreeSet<usize>,
}

/// A plan's variables, compiled against its requests and messages, with
/// where each variable is in its sequence.
#[derive(Debug)]
pub(crate) struct Templates {
    names: Vec<String>,
    sources: Vec<Source>,
    requests: Vec<Option<RequestTemplate>>,
    messages: Vec<Option<MessageTemplate>>,
    draws: Mutex<Draws>,
}

/// Resolves placeholders to variables and columns.
struct Resolver<'a> {
    vars: &'a BTreeMap<String, Var>,
    names: &'a [String],
    sources: &'a [Source],
    used: BTreeSet<usize>,
}

impl Resolver<'_> {
    /// The template of `text`, or `None` when it holds no placeholder.
    fn compile(&mut self, text: &[u8], place: &str) -> Result<Option<Template>, TemplateError> {
        let refused = |reason: String| TemplateError::Placeholder {
            place: place.to_owned(),
            reason,
        };
        let pieces = pieces(text).map_err(refused)?;
        if pieces.iter().all(|piece| matches!(piece, Piece::Text(_))) {
            return Ok(None);
        }
        let mut parts = Vec::with_capacity(pieces.len());
        for piece in pieces {
            parts.push(match piece {
                Piece::Text(text) => Part::Text(text.to_vec()),
                Piece::Slot { name, field } => {
                    slot_var(name, field, self.vars).map_err(refused)?;
                    let var = self.names.iter().position(|n| n == name).ok_or_else(|| {
                        refused(format!("{{{{{name}}}}} names no variable of [vars]"))
                    })?;
                    let column = match (self.sources.get(var), field) {
                        (Some(Source::File { table, .. }), Some(field)) => {
                            Some(table.column(field).ok_or_else(|| {
                                refused(format!(
                                    "the data file of {name:?} holds no field {field:?}"
                                ))
                            })?)
                        }
                        _ => None,
                    };
                    self.used.insert(var);
                    Part::Slot { var, column }
                }
            });
        }
        Ok(Some(Template { parts }))
    }
}

fn uses<'a>(templates: impl IntoIterator<Item = &'a Template>) -> BTreeSet<usize> {
    templates
        .into_iter()
        .flat_map(|template| &template.parts)
        .filter_map(|part| match part {
            Part::Slot { var, .. } => Some(*var),
            Part::Text(_) => None,
        })
        .collect()
}

fn source(var: &Var) -> Result<Source, TemplateError> {
    Ok(match var {
        Var::Counter(counter) => Source::Counter(counter.clone()),
        Var::Random(random) => Source::Random(random.clone()),
        Var::Uuid(_) => Source::Uuid,
        Var::List(list) => Source::List(list.clone()),
        Var::File(file) => {
            let shown = file.path.display().to_string();
            let text =
                std::fs::read_to_string(&file.path).map_err(|source| TemplateError::Read {
                    path: shown.clone(),
                    source,
                })?;
            let format = Format::of(&file.path).ok_or_else(|| TemplateError::Data {
                path: shown.clone(),
                line: 0,
                reason: "not .jsonl or .csv".to_owned(),
            })?;
            let table =
                Table::parse(&text, format).map_err(|(line, reason)| TemplateError::Data {
                    path: shown,
                    line,
                    reason,
                })?;
            Source::File {
                table,
                order: file.order,
            }
        }
    })
}

impl Templates {
    /// Compiles the variables of `plan` against its requests and `messages`,
    /// the plan's HL7 v2 messages in order, reading every data file.
    ///
    /// # Errors
    ///
    /// [`TemplateError::Read`] or [`TemplateError::Data`] for a data file
    /// that cannot be used, [`TemplateError::Placeholder`] for a placeholder
    /// that names no variable, names it the wrong way or names a field its
    /// data file lacks, and [`TemplateError::Unused`] for a variable nothing
    /// names.
    pub(crate) fn compile(plan: &Plan, messages: &[&Message]) -> Result<Self, TemplateError> {
        let names: Vec<String> = plan.vars.keys().cloned().collect();
        let sources = plan
            .vars
            .values()
            .map(source)
            .collect::<Result<Vec<_>, _>>()?;
        let mut resolver = Resolver {
            vars: &plan.vars,
            names: &names,
            sources: &sources,
            used: BTreeSet::new(),
        };
        let mut requests = Vec::with_capacity(plan.requests.len());
        for (index, request) in plan.requests.iter().enumerate() {
            let path = resolver.compile(
                request.path.as_bytes(),
                &format!("request {index}, its path"),
            )?;
            let mut headers = BTreeMap::new();
            for (name, value) in &request.headers {
                let place = format!("request {index}, its header {name:?}");
                if let Some(template) = resolver.compile(value.as_bytes(), &place)? {
                    headers.insert(name.clone(), template);
                }
            }
            let body = match &request.body {
                Some(body) => {
                    resolver.compile(body.as_bytes(), &format!("request {index}, its body"))?
                }
                None => None,
            };
            let used = uses(path.iter().chain(&body).chain(headers.values()));
            requests.push((!used.is_empty()).then_some(RequestTemplate {
                path,
                headers,
                body,
                uses: used,
            }));
        }
        let mut compiled = Vec::with_capacity(messages.len());
        for (index, message) in messages.iter().enumerate() {
            let place = format!("message {index}");
            let before = resolver.compile(message.before(), &place)?;
            let after = resolver.compile(message.after(), &place)?;
            let used = uses(before.iter().chain(&after));
            let whole = |part: Option<Template>, text: &[u8]| {
                part.unwrap_or_else(|| Template {
                    parts: vec![Part::Text(text.to_vec())],
                })
            };
            compiled.push((!used.is_empty()).then(|| MessageTemplate {
                before: whole(before, message.before()),
                after: whole(after, message.after()),
                uses: used,
            }));
        }
        if let Some(unused) = names
            .iter()
            .enumerate()
            .find(|(index, _)| !resolver.used.contains(index))
            .map(|(_, name)| name.clone())
        {
            return Err(TemplateError::Unused(unused));
        }
        let states = names
            .iter()
            .map(|name| (0, Rng::new(plan.load.seed, name)))
            .collect();
        Ok(Self {
            names,
            sources,
            requests,
            messages: compiled,
            draws: Mutex::new(Draws { states }),
        })
    }

    /// The renderer one load phase draws with, holding the sequences until
    /// it is dropped.
    pub(crate) fn renderer(&self) -> Renderer<'_> {
        Renderer {
            templates: self,
            draws: self.draws.lock().unwrap_or_else(PoisonError::into_inner),
        }
    }
}

#[cfg(test)]
mod tests;
