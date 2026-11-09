// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The JSON text of a received report, read with a repeated member
//! refused.
//!
//! RFC 8259 §4 leaves an object with a repeated name to the reader, and a
//! plain `serde_json::Value` keeps the last value. A report whose closed
//! members could be stated twice would let the check read one value and a
//! later reader another, so the repeat is refused, naming its pointer.

use std::cell::RefCell;
use std::fmt;

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};

use super::{Refusal, Rule, pointer, refuse};

/// Parses `body` into a JSON value, refusing a repeated member.
pub(super) fn parse(body: &[u8]) -> Result<Value, Refusal> {
    let repeated = RefCell::new(None);
    let mut deserializer = serde_json::Deserializer::from_slice(body);
    let parsed = Node {
        at: String::new(),
        repeated: &repeated,
    }
    .deserialize(&mut deserializer)
    .and_then(|value| deserializer.end().map(|()| value));
    parsed.map_err(|source| match repeated.into_inner() {
        Some(at) => refuse(&at, Rule::Repeated),
        None => refuse("", Rule::Json(source)),
    })
}

/// One value of the text at the pointer `at`; the pointer of the first
/// repeated member goes to `repeated`.
struct Node<'a> {
    at: String,
    repeated: &'a RefCell<Option<String>>,
}

impl Node<'_> {
    /// The node of the member or item `name` below this one.
    fn child(&self, name: &str) -> Self {
        Node {
            at: pointer(&self.at, name),
            repeated: self.repeated,
        }
    }
}

impl<'de> DeserializeSeed<'de> for Node<'_> {
    type Value = Value;

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Node<'_> {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Value, E> {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("a number JSON cannot carry"))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(value))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut items: A) -> Result<Value, A::Error> {
        let mut array = Vec::new();
        while let Some(item) = items.next_element_seed(self.child(&array.len().to_string()))? {
            array.push(item);
        }
        Ok(Value::Array(array))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut members: A) -> Result<Value, A::Error> {
        let mut object = Map::new();
        while let Some(name) = members.next_key::<String>()? {
            let child = self.child(&name);
            if object.contains_key(&name) {
                let at = child.at;
                self.repeated.replace(Some(at));
                return Err(de::Error::custom("a repeated member"));
            }
            let value = members.next_value_seed(child)?;
            object.insert(name, value);
        }
        Ok(Value::Object(object))
    }
}

#[cfg(test)]
mod tests {
    use super::parse;
    use crate::received::Rule;

    #[test]
    fn a_repeated_member_is_refused_at_its_pointer() {
        let refusal = parse(br#"{"status": {"a": true, "a": false}}"#).expect_err("refused");
        assert_eq!("/status/a", refusal.member());
        assert!(matches!(refusal.rule(), Rule::Repeated), "{refusal}");
    }

    #[test]
    fn text_after_the_value_and_broken_text_are_not_json() {
        for body in [&b"{} {}"[..], b"{", b""] {
            let refusal = parse(body).expect_err("refused");
            assert!(matches!(refusal.rule(), Rule::Json(_)), "{refusal}");
            assert_eq!("", refusal.member());
        }
    }

    #[test]
    fn an_escaped_name_keeps_its_pointer_form() {
        let refusal = parse(br#"{"a/b~": {"x": 1, "x": 2}}"#).expect_err("refused");
        assert_eq!("/a~1b~0/x", refusal.member());
    }
}
