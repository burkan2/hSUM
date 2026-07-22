use std::fmt;
use std::str::FromStr;

use thiserror::Error;

use super::{ByteSpan, DocumentId, IndexId, Sha256Digest, SourceId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Citation {
    pub index_id: IndexId,
    pub source_id: SourceId,
    pub document_id: DocumentId,
    pub revision: Sha256Digest,
    pub span: ByteSpan,
}

impl fmt::Display for Citation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "hsum://v1/{}/{}/{}?rev={}#bytes={}-{}",
            self.index_id,
            self.source_id,
            self.document_id,
            self.revision,
            self.span.start(),
            self.span.end()
        )
    }
}

impl FromStr for Citation {
    type Err = CitationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let remainder = value.strip_prefix("hsum://v1/").ok_or(CitationError)?;
        let (path_and_query, fragment) = remainder.split_once('#').ok_or(CitationError)?;
        if fragment.contains('#') {
            return Err(CitationError);
        }
        let (path, revision) = path_and_query.split_once("?rev=").ok_or(CitationError)?;
        if revision.contains('?') || revision.contains('&') {
            return Err(CitationError);
        }

        let mut segments = path.split('/');
        let index_id = segments
            .next()
            .ok_or(CitationError)?
            .parse()
            .map_err(|_| CitationError)?;
        let source_id = segments
            .next()
            .ok_or(CitationError)?
            .parse()
            .map_err(|_| CitationError)?;
        let document_id = segments
            .next()
            .ok_or(CitationError)?
            .parse()
            .map_err(|_| CitationError)?;
        if segments.next().is_some() {
            return Err(CitationError);
        }

        let revision = revision.parse().map_err(|_| CitationError)?;
        let bytes = fragment.strip_prefix("bytes=").ok_or(CitationError)?;
        let (start, end) = bytes.split_once('-').ok_or(CitationError)?;
        if end.contains('-') {
            return Err(CitationError);
        }
        let start = parse_canonical_u64(start)?;
        let end = parse_canonical_u64(end)?;
        let span = ByteSpan::new(start, end).map_err(|_| CitationError)?;

        let citation = Self {
            index_id,
            source_id,
            document_id,
            revision,
            span,
        };
        if citation.to_string() != value {
            return Err(CitationError);
        }
        Ok(citation)
    }
}

fn parse_canonical_u64(value: &str) -> Result<u64, CitationError> {
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return Err(CitationError);
    }
    value.parse().map_err(|_| CitationError)
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("malformed hsum://v1 citation")]
pub struct CitationError;

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn fixture() -> Citation {
        Citation {
            index_id: IndexId::from_uuid(
                Uuid::parse_str("018f47f0-9d9a-7a63-b4cc-8d6f2c8a44af").unwrap(),
            ),
            source_id: SourceId::from_uuid(
                Uuid::parse_str("018f47f0-a032-7978-a41a-10d768f60755").unwrap(),
            ),
            document_id: DocumentId::from_uuid(
                Uuid::parse_str("018f47f0-a82c-76e0-8cc8-8922a657d632").unwrap(),
            ),
            revision: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
                .parse()
                .unwrap(),
            span: ByteSpan::new(120, 480).unwrap(),
        }
    }

    #[test]
    fn citation_round_trips_exactly() {
        let citation = fixture();
        let text = citation.to_string();
        assert_eq!(
            text,
            "hsum://v1/018f47f0-9d9a-7a63-b4cc-8d6f2c8a44af/018f47f0-a032-7978-a41a-10d768f60755/018f47f0-a82c-76e0-8cc8-8922a657d632?rev=ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad#bytes=120-480"
        );
        assert_eq!(text.parse::<Citation>().unwrap(), citation);
    }

    #[test]
    fn malformed_variants_fail_closed() {
        let canonical = fixture().to_string();
        for malformed in [
            canonical.replacen("hsum://", "http://", 1),
            canonical.replacen("/v1/", "/v2/", 1),
            canonical.replacen("?rev=", "?revision=", 1),
            canonical.replacen("#bytes=120-480", "#bytes=480-120", 1),
            format!("{canonical}&extra=true"),
        ] {
            assert!(malformed.parse::<Citation>().is_err(), "{malformed}");
        }
    }
}
