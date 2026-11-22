// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! Drawing one arrival's values and filling its request or message with
//! them.

use std::collections::BTreeSet;
use std::sync::MutexGuard;

use super::rng::Rng;
use super::{Drawn, Draws, Order, Part, Source, Template, TemplateError, Templates};
use crate::mllp::Message;
use crate::plan::Request;

/// What one load phase draws its values with.
#[derive(Debug)]
pub(crate) struct Renderer<'a> {
    pub(super) templates: &'a Templates,
    pub(super) draws: MutexGuard<'a, Draws>,
}

impl Renderer<'_> {
    /// One value of each variable in `uses`, by variable.
    fn draw(&mut self, uses: &BTreeSet<usize>) -> Result<Vec<Option<Drawn>>, TemplateError> {
        let mut drawn = vec![None; self.templates.sources.len()];
        for var in uses {
            let (Some(source), Some((count, rng)), Some(slot)) = (
                self.templates.sources.get(*var),
                self.draws.states.get_mut(*var),
                drawn.get_mut(*var),
            ) else {
                continue;
            };
            let overflow = || {
                TemplateError::Overflow(self.templates.names.get(*var).cloned().unwrap_or_default())
            };
            let pick = |rng: &mut Rng, order: Order, length: usize, count: u64| -> usize {
                let length = u64::try_from(length).unwrap_or(u64::MAX);
                let index = match order {
                    Order::Cycle => count.checked_rem(length),
                    Order::Random => rng.below(length),
                };
                index.and_then(|i| usize::try_from(i).ok()).unwrap_or(0)
            };
            *slot = Some(match source {
                Source::Counter(counter) => {
                    let value = counter
                        .step
                        .checked_mul(*count)
                        .and_then(|offset| counter.start.checked_add(offset))
                        .ok_or_else(overflow)?;
                    Drawn::Text(format!("{value:0width$}", width = counter.width))
                }
                Source::Random(random) => {
                    let span = random.max.saturating_sub(random.min).checked_add(1);
                    let offset = match span {
                        Some(span) => rng.below(span).unwrap_or(0),
                        None => rng.next_u64(),
                    };
                    let value = random.min.saturating_add(offset);
                    Drawn::Text(format!("{value:0width$}", width = random.width))
                }
                Source::Uuid => {
                    let high = rng.next_u64().to_be_bytes();
                    let low = rng.next_u64().to_be_bytes();
                    let mut bytes = [0_u8; 16];
                    for (byte, value) in bytes.iter_mut().zip(high.iter().chain(&low)) {
                        *byte = *value;
                    }
                    Drawn::Text(
                        uuid::Builder::from_random_bytes(bytes)
                            .into_uuid()
                            .to_string(),
                    )
                }
                Source::List(list) => {
                    let index = pick(rng, list.order, list.values.len(), *count);
                    Drawn::Text(list.values.get(index).cloned().unwrap_or_default())
                }
                Source::File { table, order } => {
                    Drawn::Row(pick(rng, *order, table.rows(), *count))
                }
            });
            *count = count.saturating_add(1);
        }
        Ok(drawn)
    }

    fn render(&self, template: &Template, drawn: &[Option<Drawn>]) -> Vec<u8> {
        let mut out = Vec::new();
        for part in &template.parts {
            match part {
                Part::Text(text) => out.extend_from_slice(text),
                Part::Slot { var, column } => {
                    match (drawn.get(*var), column, self.templates.sources.get(*var)) {
                        (Some(Some(Drawn::Text(text))), None, _) => {
                            out.extend_from_slice(text.as_bytes());
                        }
                        (
                            Some(Some(Drawn::Row(row))),
                            Some(column),
                            Some(Source::File { table, .. }),
                        ) => {
                            out.extend_from_slice(
                                table.get(*row, *column).unwrap_or_default().as_bytes(),
                            );
                        }
                        _ => {}
                    }
                }
            }
        }
        out
    }

    fn render_text(&self, template: &Template, drawn: &[Option<Drawn>]) -> String {
        // NOTE: a template is UTF-8 cut at ASCII braces and every value is UTF-8, so the bytes are UTF-8.
        String::from_utf8_lossy(&self.render(template, drawn)).into_owned()
    }

    /// Request `position` of the plan with its placeholders filled from
    /// fresh draws, or `None` when it holds none.
    ///
    /// # Errors
    ///
    /// [`TemplateError::Overflow`] for a counter past its largest value.
    pub(crate) fn request(
        &mut self,
        position: usize,
        request: &Request,
    ) -> Result<Option<Request>, TemplateError> {
        let Some(Some(template)) = self.templates.requests.get(position) else {
            return Ok(None);
        };
        let drawn = self.draw(&template.uses)?;
        let mut filled = request.clone();
        if let Some(path) = &template.path {
            filled.path = self.render_text(path, &drawn);
        }
        for (name, value) in &template.headers {
            if let Some(header) = filled.headers.get_mut(name) {
                *header = self.render_text(value, &drawn);
            }
        }
        if let Some(body) = &template.body {
            filled.body = Some(self.render_text(body, &drawn));
        }
        Ok(Some(filled))
    }

    /// Message `index` of the plan with its placeholders filled from fresh
    /// draws, or `None` when it holds none.
    ///
    /// # Errors
    ///
    /// [`TemplateError::Overflow`] for a counter past its largest value.
    pub(crate) fn message(&mut self, index: usize) -> Result<Option<Message>, TemplateError> {
        let Some(Some(template)) = self.templates.messages.get(index) else {
            return Ok(None);
        };
        let drawn = self.draw(&template.uses)?;
        Ok(Some(Message::from_parts(
            self.render(&template.before, &drawn),
            self.render(&template.after, &drawn),
        )))
    }
}
