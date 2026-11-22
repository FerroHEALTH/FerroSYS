// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The data file of a `file` variable: rows of named fields, from JSON Lines
//! (one object a line, each value a string, a number or a boolean) or from
//! CSV (RFC 4180: a header row naming the fields, then one record a line).
//!
//! Every row holds every field, and no value holds a control character, so
//! a value fits a header field and an HL7 v2 segment as written. The cells
//! are kept a column at a time, one string and its offsets each, so a file
//! of a million rows costs about its own size. No specification governs the
//! JSON Lines shape: our own design.

use std::collections::BTreeMap;
use std::path::Path;

/// The rows of a data file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Table {
    names: Vec<String>,
    columns: Vec<Column>,
    rows: usize,
}

/// One field of every row: the values one after the other, and where each
/// ends.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Column {
    text: String,
    ends: Vec<usize>,
}

impl Column {
    fn push(&mut self, value: &str) {
        self.text.push_str(value);
        self.ends.push(self.text.len());
    }

    fn get(&self, row: usize) -> Option<&str> {
        let start = match row.checked_sub(1) {
            Some(before) => *self.ends.get(before)?,
            None => 0,
        };
        self.text.get(start..*self.ends.get(row)?)
    }
}

/// The format of a data file, from its extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Format {
    JsonLines,
    Csv,
}

impl Format {
    /// The format of the file at `path`: `.jsonl` or `.csv`.
    pub(super) fn of(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()? {
            "jsonl" => Some(Self::JsonLines),
            "csv" => Some(Self::Csv),
            _ => None,
        }
    }
}

impl Table {
    /// The table of `text` in `format`.
    ///
    /// # Errors
    ///
    /// The line and the reason, for a row that does not parse, lacks a
    /// field, holds another or holds a control character, and for a file
    /// with no row.
    pub(super) fn parse(text: &str, format: Format) -> Result<Self, (usize, String)> {
        let rows = match format {
            Format::JsonLines => json_lines(text)?,
            Format::Csv => csv(text)?,
        };
        let Some((_, first)) = rows.first() else {
            return Err((1, "the file holds no row".to_owned()));
        };
        let names: Vec<String> = first.keys().cloned().collect();
        let mut columns = vec![Column::default(); names.len()];
        for (line, row) in &rows {
            if row.len() != names.len() || !names.iter().all(|name| row.contains_key(name)) {
                return Err((
                    *line,
                    "the row does not hold the first row's fields".to_owned(),
                ));
            }
            for (column, value) in columns.iter_mut().zip(row.values()) {
                if value.chars().any(char::is_control) {
                    return Err((*line, "a value holds a control character".to_owned()));
                }
                column.push(value);
            }
        }
        Ok(Self {
            names,
            columns,
            rows: rows.len(),
        })
    }

    /// The rows.
    pub(super) fn rows(&self) -> usize {
        self.rows
    }

    /// The column of the field `name`.
    pub(super) fn column(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n == name)
    }

    /// The value of `column` in `row`.
    pub(super) fn get(&self, row: usize, column: usize) -> Option<&str> {
        self.columns.get(column)?.get(row)
    }
}

/// One CSV record: the line it starts on and its fields.
type Record = (usize, Vec<String>);

/// One row: its line and its fields by name.
type Row = (usize, BTreeMap<String, String>);

fn json_lines(text: &str) -> Result<Vec<Row>, (usize, String)> {
    let mut rows = Vec::new();
    for (line, content) in (1_usize..).zip(text.lines()) {
        if content.trim().is_empty() {
            continue;
        }
        let object: serde_json::Map<String, serde_json::Value> = serde_json::from_str(content)
            .map_err(|error| (line, format!("not a JSON object: {error}")))?;
        let mut row = BTreeMap::new();
        for (name, value) in object {
            let text = match value {
                serde_json::Value::String(text) => text,
                serde_json::Value::Number(number) => number.to_string(),
                serde_json::Value::Bool(flag) => flag.to_string(),
                _ => {
                    return Err((
                        line,
                        format!("the value of {name:?} is not a string, number or boolean"),
                    ));
                }
            };
            row.insert(name, text);
        }
        rows.push((line, row));
    }
    Ok(rows)
}

fn csv(text: &str) -> Result<Vec<Row>, (usize, String)> {
    let mut records = records(text)?.into_iter();
    let Some((_, header)) = records.next() else {
        return Ok(Vec::new());
    };
    let mut rows = Vec::new();
    for (line, record) in records {
        if record.len() != header.len() {
            return Err((
                line,
                format!("{} fields, the header names {}", record.len(), header.len()),
            ));
        }
        let row: BTreeMap<String, String> = header.iter().cloned().zip(record).collect();
        if row.len() != header.len() {
            return Err((1, "the header names a field twice".to_owned()));
        }
        rows.push((line, row));
    }
    Ok(rows)
}

/// The records of a CSV text with the line each starts on (RFC 4180 §2:
/// fields split by commas, a field in double quotes may hold commas, line
/// breaks and doubled quotes). A blank line holds no record.
fn records(text: &str) -> Result<Vec<Record>, (usize, String)> {
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut line: usize = 1;
    let mut start = line;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match (quoted, c) {
            (true, '"') if chars.peek() == Some(&'"') => {
                chars.next();
                field.push('"');
            }
            (true, '"') => quoted = false,
            (true, other) => {
                if other == '\n' {
                    line = line.saturating_add(1);
                }
                field.push(other);
            }
            (false, '"') if field.is_empty() => quoted = true,
            (false, ',') => record.push(std::mem::take(&mut field)),
            (false, '\r') if chars.peek() == Some(&'\n') => {}
            (false, '\n') => {
                if !(record.is_empty() && field.is_empty()) {
                    record.push(std::mem::take(&mut field));
                    records.push((start, std::mem::take(&mut record)));
                }
                line = line.saturating_add(1);
                start = line;
            }
            (false, '"') => {
                return Err((line, "a double quote inside an unquoted field".to_owned()));
            }
            (false, other) => field.push(other),
        }
    }
    if quoted {
        return Err((start, "a quoted field is not closed".to_owned()));
    }
    if !(record.is_empty() && field.is_empty()) {
        record.push(field);
        records.push((start, record));
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_lines_rows_keep_their_fields() {
        let text = "{\"id\":\"C-0000001\",\"n\":7,\"ok\":true}\n\n{\"n\":8,\"id\":\"C-0000002\",\"ok\":false}\n";
        let table = Table::parse(text, Format::JsonLines).unwrap();
        assert_eq!(2, table.rows());
        let id = table.column("id").unwrap();
        let n = table.column("n").unwrap();
        assert_eq!(Some("C-0000002"), table.get(1, id));
        assert_eq!(Some("7"), table.get(0, n));
        assert_eq!(Some("false"), table.get(1, table.column("ok").unwrap()));
        assert_eq!(None, table.get(2, id));
        assert_eq!(None, table.column("missing"));
    }

    #[test]
    fn csv_takes_quotes_commas_and_line_breaks_inside_quotes() {
        let text = "id,name\r\n1,\"Doe, Jane\"\r\n2,\"say \"\"hi\"\"\"\n\n3,plain\n";
        let table = Table::parse(text, Format::Csv).unwrap();
        assert_eq!(3, table.rows());
        let name = table.column("name").unwrap();
        assert_eq!(Some("Doe, Jane"), table.get(0, name));
        assert_eq!(Some("say \"hi\""), table.get(1, name));
        assert_eq!(Some("plain"), table.get(2, name));
    }

    #[test]
    fn a_bad_row_is_refused_with_its_line() {
        for (text, format, line) in [
            ("{\"a\":\"1\"}\n{\"b\":\"2\"}\n", Format::JsonLines, 2),
            ("{\"a\":\"1\"}\n[1]\n", Format::JsonLines, 2),
            ("{\"a\":null}\n", Format::JsonLines, 1),
            ("{\"a\":\"x\\ny\"}\n", Format::JsonLines, 1),
            ("a,b\n1,2\n3\n", Format::Csv, 3),
            ("a,b\n1,\"2\n", Format::Csv, 2),
            ("a,b\n1,x\"y\n", Format::Csv, 2),
            ("a,a\n1,2\n", Format::Csv, 1),
            ("a\n", Format::Csv, 1),
            ("", Format::JsonLines, 1),
        ] {
            match Table::parse(text, format) {
                Err((at, _)) => assert_eq!(line, at, "{text:?}"),
                Ok(table) => panic!("{text:?} gave {table:?}"),
            }
        }
    }

    #[test]
    fn the_format_comes_from_the_extension() {
        assert_eq!(Some(Format::JsonLines), Format::of(Path::new("a/p.jsonl")));
        assert_eq!(Some(Format::Csv), Format::of(Path::new("p.csv")));
        assert_eq!(None, Format::of(Path::new("p.json")));
        assert_eq!(None, Format::of(Path::new("p")));
    }
}
