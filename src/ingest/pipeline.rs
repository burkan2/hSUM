use serde_json::Value;
use serde_json_canonicalizer::to_vec;
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

use crate::domain::Sha256Digest;

pub struct SnapshotRevision<'a> {
    pub body: &'a [u8],
    pub source_uri: &'a str,
    pub title: &'a str,
    pub metadata: &'a Value,
    pub source_updated_at: Option<&'a str>,
}

pub fn body_sha256(body: &[u8]) -> Sha256Digest {
    Sha256Digest::of_bytes(body)
}

pub fn revision_sha256(snapshot: &SnapshotRevision<'_>) -> Result<Sha256Digest, RevisionHashError> {
    if !snapshot.metadata.is_object() {
        return Err(RevisionHashError::MetadataNotObject);
    }

    let metadata = to_vec(snapshot.metadata)?;
    let normalized_timestamp = snapshot
        .source_updated_at
        .map(|value| {
            OffsetDateTime::parse(value, &Rfc3339)?
                .to_offset(UtcOffset::UTC)
                .format(&Rfc3339)
                .map_err(RevisionHashError::TimestampFormat)
        })
        .transpose()?;
    let timestamp = to_vec(&normalized_timestamp)?;

    let mut hasher = Sha256::new();
    hasher.update(b"hsum.snapshot.v1\0");
    update_frame(&mut hasher, 1, snapshot.body);
    update_frame(&mut hasher, 2, snapshot.source_uri.as_bytes());
    update_frame(&mut hasher, 3, snapshot.title.as_bytes());
    update_frame(&mut hasher, 4, &metadata);
    update_frame(&mut hasher, 5, &timestamp);
    Ok(Sha256Digest::from_bytes(hasher.finalize().into()))
}

fn update_frame(hasher: &mut Sha256, tag: u8, bytes: &[u8]) {
    hasher.update([tag]);
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

#[derive(Debug, Error)]
pub enum RevisionHashError {
    #[error("metadata cannot be represented as RFC 8785 canonical JSON")]
    CanonicalJson(#[from] serde_json::Error),
    #[error("source timestamp is not valid RFC 3339")]
    InvalidTimestamp(#[from] time::error::Parse),
    #[error("normalized source timestamp could not be formatted")]
    TimestampFormat(#[from] time::error::Format),
    #[error("snapshot metadata must be a JSON object")]
    MetadataNotObject,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture<'a>(
        body: &'a [u8],
        metadata: &'a Value,
        timestamp: Option<&'a str>,
    ) -> SnapshotRevision<'a> {
        SnapshotRevision {
            body,
            source_uri: "repo://src/lib.rs",
            title: "lib.rs",
            metadata,
            source_updated_at: timestamp,
        }
    }

    #[test]
    fn body_hash_ignores_metadata() {
        assert_eq!(body_sha256(b"same"), body_sha256(b"same"));
        assert_ne!(body_sha256(b"same"), body_sha256(b"changed"));
    }

    #[test]
    fn snapshot_v1_hash_matches_the_frozen_bom_crlf_unicode_fixture() {
        let body = b"\xef\xbb\xbf# Source\r\ncaf\xc3\xa9\r\n";
        let metadata = json!({"z": "é", "a": {"two": 2, "one": 1}});
        let snapshot = SnapshotRevision {
            body,
            source_uri: "repo://notes/source.md",
            title: "Source",
            metadata: &metadata,
            source_updated_at: Some("2026-07-20T03:00:00+03:00"),
        };

        assert_eq!(
            body_sha256(body).to_string(),
            "ae19817deddc3d68a1b0826fbf04266fdf844ad28d590ef07d370f31a1a5bb33"
        );
        assert_eq!(
            revision_sha256(&snapshot).unwrap().to_string(),
            "e3591b43a6bf8aa739b4e87aeac57e7113476f8e2e12617a7bcab464cc943962"
        );
    }

    #[test]
    fn metadata_key_order_does_not_change_revision_identity() {
        let left = json!({"z": 1, "a": {"two": 2, "one": 1}});
        let right = json!({"a": {"one": 1, "two": 2}, "z": 1});
        assert_eq!(
            revision_sha256(&fixture(b"body", &left, None)).unwrap(),
            revision_sha256(&fixture(b"body", &right, None)).unwrap()
        );
    }

    #[test]
    fn every_snapshot_input_changes_revision_identity() {
        let metadata = json!({"team": "search"});
        let base = fixture(b"body", &metadata, Some("2026-07-20T00:00:00Z"));
        let base_hash = revision_sha256(&base).unwrap();

        let changed_body = fixture(b"BODY", &metadata, base.source_updated_at);
        assert_ne!(base_hash, revision_sha256(&changed_body).unwrap());

        let changed_uri = SnapshotRevision {
            source_uri: "repo://src/main.rs",
            ..base
        };
        assert_ne!(base_hash, revision_sha256(&changed_uri).unwrap());

        let changed_title = SnapshotRevision {
            title: "main.rs",
            ..base
        };
        assert_ne!(base_hash, revision_sha256(&changed_title).unwrap());

        let changed_metadata_value = json!({"team": "storage"});
        let changed_metadata = fixture(base.body, &changed_metadata_value, base.source_updated_at);
        assert_ne!(base_hash, revision_sha256(&changed_metadata).unwrap());

        let changed_timestamp = fixture(base.body, base.metadata, Some("2026-07-20T00:00:01Z"));
        assert_ne!(base_hash, revision_sha256(&changed_timestamp).unwrap());
    }

    #[test]
    fn equivalent_offsets_normalize_to_the_same_revision_identity() {
        let metadata = json!({});
        let utc = fixture(b"body", &metadata, Some("2026-07-20T00:00:00Z"));
        let offset = fixture(b"body", &metadata, Some("2026-07-20T03:00:00+03:00"));
        assert_eq!(
            revision_sha256(&utc).unwrap(),
            revision_sha256(&offset).unwrap()
        );
    }

    #[test]
    fn invalid_timestamp_is_rejected() {
        let metadata = json!({});
        assert!(matches!(
            revision_sha256(&fixture(b"body", &metadata, Some("not-a-timestamp"))),
            Err(RevisionHashError::InvalidTimestamp(_))
        ));
    }

    #[test]
    fn metadata_must_be_an_object() {
        let metadata = json!(["not", "an", "object"]);
        assert!(matches!(
            revision_sha256(&fixture(b"body", &metadata, None)),
            Err(RevisionHashError::MetadataNotObject)
        ));
    }
}
