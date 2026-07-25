use crate::domain::Sha256Digest;
use crate::ingest::ChunkKind;

pub const APPLICATION_ID: i32 = i32::from_be_bytes(*b"HSUM");
pub const SCHEMA_VERSION: u32 = 1;

pub(crate) const MIGRATION_0001: &str = include_str!("../../migrations/0001_alpha1.sql");

const PIPELINE_DESCRIPTOR: &str = concat!(
    "hsum.pipeline.v1\n",
    "filesystem=v1:extensions=md,markdown,txt,rs,py,ts,tsx,js,jsx,go,java,kt,kts,c,h,cpp,hpp,cc,hh,cxx,rb,cs,swift,php,scala,sh,bash,sql:symlinks=never\n",
    "connector_key=platform-raw-relative-bytes:no-unicode-or-case-normalization\n",
    "source_uri=repo-v1:rfc3986-unreserved:slash-preserved:uppercase-percent\n",
    "snapshot_hash=sha256-length-framed-rfc8785-utc\n",
    "content=original-utf8:line-endings-preserved:nul-rejected\n",
    "chunker=bytes-v1:target-1200:max-1800:overlap-max-180:bom-search-start-3\n",
    "chunk_layout_key=sha256-domain-separated:chunk-kind+settings\n",
    "fts=unicode61:remove_diacritics-0:tokenchars-_-.:/\n",
    "literals=case-sensitive:max-64:bytes-2..128\n",
    "quote_bloom=byte-trigram:bits-4096:hashes-4:sha256-double-hash-be\n",
    "embedding=none\n",
);

pub fn schema_checksum() -> Sha256Digest {
    Sha256Digest::of_bytes(MIGRATION_0001.as_bytes())
}

pub fn pipeline_fingerprint() -> Sha256Digest {
    Sha256Digest::of_bytes(PIPELINE_DESCRIPTOR.as_bytes())
}

pub fn pipeline_descriptor() -> &'static str {
    PIPELINE_DESCRIPTOR
}

pub fn chunker_fingerprint(kind: ChunkKind) -> Sha256Digest {
    let descriptor = format!(
        "hsum.chunk-layout.v1\0kind={}\ntarget=1200\nmax=1800\noverlap-max=180\n",
        kind.as_str()
    );
    Sha256Digest::of_bytes(descriptor.as_bytes())
}

pub(crate) fn chunk_kind_for_fingerprint(fingerprint: Sha256Digest) -> Option<ChunkKind> {
    ChunkKind::ALL
        .into_iter()
        .find(|kind| chunker_fingerprint(*kind) == fingerprint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_1_pipeline_fingerprint_is_frozen() {
        // Bumped when Tier 1 language extensions were added. Changing this value
        // invalidates every existing index: doctor reports a generation
        // pipeline-fingerprint failure until the index is re-ingested.
        assert_eq!(
            pipeline_fingerprint().to_string(),
            "ff465d499d3e917ab4f468dc6dec0cbf9b343cbbab661190f808c92d79bccf5b"
        );
    }

    #[test]
    fn layout_fingerprints_are_kind_specific_and_recognized() {
        let markdown = chunker_fingerprint(ChunkKind::Markdown);
        let rust = chunker_fingerprint(ChunkKind::Rust);
        assert_ne!(markdown, rust);
        assert_eq!(
            markdown.to_string(),
            "071dbd9f11a74207b9b15860c8220ebd5810d5e7fa0449ee4e5d1f011031c812"
        );
        assert_eq!(
            chunk_kind_for_fingerprint(markdown),
            Some(ChunkKind::Markdown)
        );
        assert_eq!(chunk_kind_for_fingerprint(rust), Some(ChunkKind::Rust));
    }
}
