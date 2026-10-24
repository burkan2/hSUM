use std::collections::BTreeSet;
use std::fmt;
use std::str;

use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
use serde_json_canonicalizer::to_string as to_canonical_json;
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

pub const MAX_JSONL_ID_BYTES: usize = 512;
pub const MAX_JSONL_SOURCE_URI_BYTES: usize = 4 * 1024;
pub const MAX_JSONL_TITLE_BYTES: usize = 4 * 1024;
pub const MAX_JSONL_CONTENT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_JSONL_METADATA_BYTES: usize = 16 * 1024;
pub const MAX_JSONL_NESTING_DEPTH: usize = 32;
pub const MAX_JSONL_COLLECTION_MEMBERS: usize = 10_000;

/// Bounds one encoded record before JSON string decoding. Six bytes per
/// decoded byte covers the longest single-code-unit `\uXXXX` representation;
/// the remaining allowance covers field names and structural whitespace.
pub const MAX_JSONL_LINE_BYTES: usize = 16 * 1024 * 1024;

const ALLOWED_FIELDS: [&str; 7] = [
    "content",
    "deleted",
    "id",
    "metadata",
    "source_uri",
    "title",
    "updated_at",
];

#[derive(Clone, Debug, PartialEq)]
pub struct JsonlRecord {
    id: String,
    source_uri: String,
    content: String,
    title: Option<String>,
    updated_at: Option<String>,
    metadata: Value,
    metadata_json: String,
}

impl JsonlRecord {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn source_uri(&self) -> &str {
        &self.source_uri
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn updated_at(&self) -> Option<&str> {
        self.updated_at.as_deref()
    }

    pub fn metadata(&self) -> &Value {
        &self.metadata
    }

    pub fn metadata_json(&self) -> &str {
        &self.metadata_json
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsonlDeletion {
    id: String,
    source_uri: String,
}

impl JsonlDeletion {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn source_uri(&self) -> &str {
        &self.source_uri
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct JsonlSnapshot {
    records: Vec<JsonlRecord>,
    deletions: Vec<JsonlDeletion>,
}

impl JsonlSnapshot {
    pub fn records(&self) -> &[JsonlRecord] {
        &self.records
    }

    pub fn deletions(&self) -> &[JsonlDeletion] {
        &self.deletions
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty() && self.deletions.is_empty()
    }
}

/// Parses one complete UTF-8 JSONL snapshot. No records are returned unless
/// every nonblank line validates, so callers cannot accidentally apply a valid
/// prefix of a bad source.
pub fn parse_jsonl_snapshot(input: &[u8]) -> Result<JsonlSnapshot, JsonlSnapshotError> {
    str::from_utf8(input).map_err(|error| JsonlSnapshotError::InvalidUtf8 {
        valid_up_to: error.valid_up_to(),
    })?;

    let mut snapshot = JsonlSnapshot::default();
    let mut ids = BTreeSet::new();
    for (index, line) in input.split(|byte| *byte == b'\n').enumerate() {
        let line_number = index + 1;
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if line.len() > MAX_JSONL_LINE_BYTES {
            return Err(JsonlSnapshotError::LineTooLarge {
                line: line_number,
                found: line.len(),
            });
        }

        let value = parse_bounded_value(line).map_err(|source| JsonlSnapshotError::Json {
            line: line_number,
            source,
        })?;
        let object = value
            .as_object()
            .ok_or(JsonlSnapshotError::TopLevelObjectRequired { line: line_number })?;
        if let Some(field) = object
            .keys()
            .find(|field| !ALLOWED_FIELDS.contains(&field.as_str()))
        {
            return Err(JsonlSnapshotError::UnknownField {
                line: line_number,
                field: field.clone(),
            });
        }

        let id = required_string(object, "id", line_number)?;
        validate_nonempty_bounded_display(&id, "id", MAX_JSONL_ID_BYTES, line_number)?;
        if !ids.insert(id.clone()) {
            return Err(JsonlSnapshotError::DuplicateId {
                line: line_number,
                id,
            });
        }

        let source_uri = required_string(object, "source_uri", line_number)?;
        validate_nonempty_bounded_display(
            &source_uri,
            "source_uri",
            MAX_JSONL_SOURCE_URI_BYTES,
            line_number,
        )?;

        match object.get("deleted") {
            Some(Value::Bool(true)) => {
                if object
                    .keys()
                    .any(|field| !matches!(field.as_str(), "id" | "source_uri" | "deleted"))
                {
                    return Err(JsonlSnapshotError::InvalidDeletion { line: line_number });
                }
                snapshot.deletions.push(JsonlDeletion { id, source_uri });
            }
            Some(_) => {
                return Err(JsonlSnapshotError::InvalidDeletion { line: line_number });
            }
            None => {
                let content = required_string(object, "content", line_number)?;
                if content.is_empty() {
                    return Err(JsonlSnapshotError::EmptyField {
                        line: line_number,
                        field: "content",
                    });
                }
                if content.len() > MAX_JSONL_CONTENT_BYTES {
                    return Err(JsonlSnapshotError::FieldTooLarge {
                        line: line_number,
                        field: "content",
                        found: content.len(),
                        maximum: MAX_JSONL_CONTENT_BYTES,
                    });
                }
                if content.contains('\0') {
                    return Err(JsonlSnapshotError::NulContent { line: line_number });
                }

                let title = optional_string(object, "title", line_number)?;
                if let Some(title) = title.as_deref() {
                    validate_bounded_display(title, "title", MAX_JSONL_TITLE_BYTES, line_number)?;
                }

                let updated_at = optional_string(object, "updated_at", line_number)?
                    .map(|value| normalize_timestamp(&value, line_number))
                    .transpose()?;
                let metadata = object
                    .get("metadata")
                    .cloned()
                    .unwrap_or_else(|| Value::Object(Map::new()));
                if !metadata.is_object() {
                    return Err(JsonlSnapshotError::MetadataObjectRequired { line: line_number });
                }
                let metadata_json = to_canonical_json(&metadata).map_err(|source| {
                    JsonlSnapshotError::MetadataSerialization {
                        line: line_number,
                        source,
                    }
                })?;
                if metadata_json.len() > MAX_JSONL_METADATA_BYTES {
                    return Err(JsonlSnapshotError::MetadataTooLarge {
                        line: line_number,
                        found: metadata_json.len(),
                    });
                }

                snapshot.records.push(JsonlRecord {
                    id,
                    source_uri,
                    content,
                    title,
                    updated_at,
                    metadata,
                    metadata_json,
                });
            }
        }
    }
    Ok(snapshot)
}

fn required_string(
    object: &Map<String, Value>,
    field: &'static str,
    line: usize,
) -> Result<String, JsonlSnapshotError> {
    let value = object
        .get(field)
        .ok_or(JsonlSnapshotError::MissingField { line, field })?;
    value
        .as_str()
        .map(str::to_owned)
        .ok_or(JsonlSnapshotError::StringRequired { line, field })
}

fn optional_string(
    object: &Map<String, Value>,
    field: &'static str,
    line: usize,
) -> Result<Option<String>, JsonlSnapshotError> {
    object
        .get(field)
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or(JsonlSnapshotError::StringRequired { line, field })
        })
        .transpose()
}

fn validate_nonempty_bounded_display(
    value: &str,
    field: &'static str,
    maximum: usize,
    line: usize,
) -> Result<(), JsonlSnapshotError> {
    if value.is_empty() {
        return Err(JsonlSnapshotError::EmptyField { line, field });
    }
    validate_bounded_display(value, field, maximum, line)
}

fn validate_bounded_display(
    value: &str,
    field: &'static str,
    maximum: usize,
    line: usize,
) -> Result<(), JsonlSnapshotError> {
    if value.len() > maximum {
        return Err(JsonlSnapshotError::FieldTooLarge {
            line,
            field,
            found: value.len(),
            maximum,
        });
    }
    if value.chars().any(char::is_control) {
        return Err(JsonlSnapshotError::ControlCharacter { line, field });
    }
    Ok(())
}

fn normalize_timestamp(value: &str, line: usize) -> Result<String, JsonlSnapshotError> {
    OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|_| JsonlSnapshotError::InvalidTimestamp { line })?
        .to_offset(UtcOffset::UTC)
        .format(&Rfc3339)
        .map_err(|_| JsonlSnapshotError::InvalidTimestamp { line })
}

fn parse_bounded_value(line: &[u8]) -> Result<Value, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_slice(line);
    let value = BoundedValueSeed { depth: 1 }.deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(value)
}

struct BoundedValueSeed {
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for BoundedValueSeed {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(BoundedValueVisitor { depth: self.depth })
    }
}

struct BoundedValueVisitor {
    depth: usize,
}

impl<'de> Visitor<'de> for BoundedValueVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("JSON number is not finite"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(Value::String(value))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        BoundedValueSeed { depth: self.depth }.deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        if self.depth > MAX_JSONL_NESTING_DEPTH {
            return Err(serde::de::Error::custom("JSON nesting exceeds 32 levels"));
        }
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(BoundedValueSeed {
            depth: self.depth + 1,
        })? {
            if values.len() == MAX_JSONL_COLLECTION_MEMBERS {
                return Err(serde::de::Error::custom(
                    "JSON collection exceeds 10000 members",
                ));
            }
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        if self.depth > MAX_JSONL_NESTING_DEPTH {
            return Err(serde::de::Error::custom("JSON nesting exceeds 32 levels"));
        }
        let mut values = Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if values.len() == MAX_JSONL_COLLECTION_MEMBERS {
                return Err(serde::de::Error::custom(
                    "JSON collection exceeds 10000 members",
                ));
            }
            if values.contains_key(&key) {
                return Err(serde::de::Error::custom("duplicate JSON object key"));
            }
            let value = object.next_value_seed(BoundedValueSeed {
                depth: self.depth + 1,
            })?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}

#[derive(Debug, Error)]
pub enum JsonlSnapshotError {
    #[error("JSONL snapshot is not UTF-8 at byte {valid_up_to}")]
    InvalidUtf8 { valid_up_to: usize },
    #[error("JSONL line {line} exceeds the encoded record limit ({found} bytes)")]
    LineTooLarge { line: usize, found: usize },
    #[error("JSONL line {line} is invalid JSON")]
    Json {
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("JSONL line {line} must contain one object")]
    TopLevelObjectRequired { line: usize },
    #[error("JSONL line {line} contains unknown field {field}")]
    UnknownField { line: usize, field: String },
    #[error("JSONL line {line} is missing required field {field}")]
    MissingField { line: usize, field: &'static str },
    #[error("JSONL line {line} field {field} must be a string")]
    StringRequired { line: usize, field: &'static str },
    #[error("JSONL line {line} field {field} must not be empty")]
    EmptyField { line: usize, field: &'static str },
    #[error("JSONL line {line} field {field} is {found} bytes; maximum is {maximum}")]
    FieldTooLarge {
        line: usize,
        field: &'static str,
        found: usize,
        maximum: usize,
    },
    #[error("JSONL line {line} field {field} contains a disallowed control character")]
    ControlCharacter { line: usize, field: &'static str },
    #[error("JSONL line {line} content contains NUL")]
    NulContent { line: usize },
    #[error("JSONL line {line} has an invalid RFC 3339 updated_at")]
    InvalidTimestamp { line: usize },
    #[error("JSONL line {line} metadata must be an object")]
    MetadataObjectRequired { line: usize },
    #[error("JSONL line {line} metadata is {found} bytes; maximum is 16384")]
    MetadataTooLarge { line: usize, found: usize },
    #[error("JSONL line {line} metadata could not be canonicalized")]
    MetadataSerialization {
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("JSONL line {line} deletion must contain only id, source_uri, and deleted=true")]
    InvalidDeletion { line: usize },
    #[error("JSONL line {line} repeats id {id}")]
    DuplicateId { line: usize, id: String },
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use serde_json::json;

    use super::*;

    #[test]
    fn valid_snapshot_preserves_decoded_content_and_skips_blank_lines() {
        let input = br#"
            {"id":"alpha","source_uri":"runbook://alpha","content":"caf\u00e9\nnext","title":"Alpha","updated_at":"2026-07-20T03:00:00+03:00","metadata":{"z":2,"a":1}}

            {"id":"gone","source_uri":"runbook://gone","deleted":true}
        "#;
        let snapshot = parse_jsonl_snapshot(input).unwrap();
        assert_eq!(snapshot.records().len(), 1);
        assert_eq!(snapshot.deletions().len(), 1);
        let record = &snapshot.records()[0];
        assert_eq!(record.content().as_bytes(), "café\nnext".as_bytes());
        assert_eq!(record.updated_at(), Some("2026-07-20T00:00:00Z"));
        assert_eq!(record.metadata_json(), r#"{"a":1,"z":2}"#);
    }

    #[test]
    fn rejects_duplicate_ids_keys_unknown_fields_and_bad_json() {
        let cases = [
            br#"{"id":"a","source_uri":"u","content":"x"}
                {"id":"a","source_uri":"v","content":"y"}"#
                .as_slice(),
            br#"{"id":"a","id":"b","source_uri":"u","content":"x"}"#.as_slice(),
            br#"{"id":"a","source_uri":"u","content":"x","extra":1}"#.as_slice(),
            br#"{"id":"a","source_uri":"u","content":}"#.as_slice(),
            br#"{"id":"a","source_uri":"u","content":"x","metadata":{"k":1,"k":2}}"#.as_slice(),
        ];
        for input in cases {
            assert!(parse_jsonl_snapshot(input).is_err(), "{input:?}");
        }
    }

    #[test]
    fn rejects_missing_empty_wrong_types_controls_nul_and_invalid_timestamp() {
        let cases = [
            json!({"source_uri":"u","content":"x"}),
            json!({"id":"","source_uri":"u","content":"x"}),
            json!({"id":"a","source_uri":1,"content":"x"}),
            json!({"id":"a\n","source_uri":"u","content":"x"}),
            json!({"id":"a","source_uri":"u","content":"\u{0}"}),
            json!({"id":"a","source_uri":"u","content":"x","updated_at":"today"}),
            json!({"id":"a","source_uri":"u","content":"x","metadata":[]}),
            json!({"id":"a","source_uri":"u","deleted":false}),
            json!({"id":"a","source_uri":"u","deleted":true,"title":"no"}),
        ];
        for value in cases {
            let input = serde_json::to_vec(&value).unwrap();
            assert!(parse_jsonl_snapshot(&input).is_err(), "{value}");
        }
    }

    #[test]
    fn enforces_every_field_and_metadata_limit() {
        for (field, maximum) in [
            ("id", MAX_JSONL_ID_BYTES),
            ("source_uri", MAX_JSONL_SOURCE_URI_BYTES),
            ("title", MAX_JSONL_TITLE_BYTES),
            ("content", MAX_JSONL_CONTENT_BYTES),
        ] {
            let mut value = json!({
                "id":"a",
                "source_uri":"u",
                "content":"x",
                "title":"t"
            });
            value[field] = Value::String("x".repeat(maximum));
            assert!(parse_jsonl_snapshot(&serde_json::to_vec(&value).unwrap()).is_ok());
            value[field] = Value::String("x".repeat(maximum + 1));
            assert!(parse_jsonl_snapshot(&serde_json::to_vec(&value).unwrap()).is_err());
        }

        let fitting = json!({"id":"a","source_uri":"u","content":"x","metadata":{"v":"x".repeat(MAX_JSONL_METADATA_BYTES - 8)}});
        assert!(parse_jsonl_snapshot(&serde_json::to_vec(&fitting).unwrap()).is_ok());
        let oversized = json!({"id":"a","source_uri":"u","content":"x","metadata":{"v":"x".repeat(MAX_JSONL_METADATA_BYTES)}});
        assert!(parse_jsonl_snapshot(&serde_json::to_vec(&oversized).unwrap()).is_err());
    }

    #[test]
    fn enforces_nesting_and_collection_limits() {
        let mut accepted = "0".to_owned();
        for _ in 0..(MAX_JSONL_NESTING_DEPTH - 2) {
            accepted = format!("[{accepted}]");
        }
        let accepted =
            format!(r#"{{"id":"a","source_uri":"u","content":"x","metadata":{{"v":{accepted}}}}}"#);
        let parsed = parse_jsonl_snapshot(accepted.as_bytes());
        assert!(parsed.is_ok(), "{parsed:?}");

        let rejected = accepted.replacen("\"v\":", "\"v\":[", 1) + "]";
        assert!(parse_jsonl_snapshot(rejected.as_bytes()).is_err());

        let accepted_members = (0..MAX_JSONL_COLLECTION_MEMBERS)
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let accepted = format!("[{accepted_members}]");
        assert!(parse_bounded_value(accepted.as_bytes()).is_ok());
        let rejected = format!("[{},{}]", accepted_members, MAX_JSONL_COLLECTION_MEMBERS);
        assert!(parse_bounded_value(rejected.as_bytes()).is_err());
    }

    proptest! {
        #[test]
        fn arbitrary_bytes_never_panic_or_return_duplicate_ids(input in prop::collection::vec(any::<u8>(), 0..32768)) {
            if let Ok(snapshot) = parse_jsonl_snapshot(&input) {
                let ids = snapshot.records().iter().map(JsonlRecord::id)
                    .chain(snapshot.deletions().iter().map(JsonlDeletion::id))
                    .collect::<BTreeSet<_>>();
                prop_assert_eq!(ids.len(), snapshot.records().len() + snapshot.deletions().len());
            }
        }
    }
}
