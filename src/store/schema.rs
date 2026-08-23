use crate::domain::Sha256Digest;
use crate::ingest::ChunkKind;
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

pub const APPLICATION_ID: i32 = i32::from_be_bytes(*b"HSUM");
pub const SCHEMA_VERSION: u32 = 4;

pub(crate) const MIGRATION_0001: &str = include_str!("../../migrations/0001_alpha1.sql");
pub(crate) const MIGRATION_0002: &str = include_str!("../../migrations/0002_jsonl_sources.sql");
pub(crate) const MIGRATION_0003: &str = include_str!("../../migrations/0003_maintenance.sql");
pub(crate) const MIGRATION_0004: &str = include_str!("../../migrations/0004_vector_storage.sql");
pub(crate) const MIGRATIONS: [(u32, &str); 4] = [
    (1, MIGRATION_0001),
    (2, MIGRATION_0002),
    (3, MIGRATION_0003),
    (4, MIGRATION_0004),
];

const PIPELINE_DESCRIPTOR_PREFIX: &str = concat!(
    "connector_key=platform-raw-relative-bytes:no-unicode-or-case-normalization\n",
    "source_uri=repo-v1:rfc3986-unreserved:slash-preserved:uppercase-percent\n",
    "snapshot_hash=sha256-length-framed-rfc8785-utc\n",
    "content=original-utf8:line-endings-preserved:nul-rejected\n",
    "chunker=bytes-v1:target-1200:max-1800:overlap-max-180:bom-search-start-3\n",
    "chunk_layout_key=sha256-domain-separated:chunk-kind+settings\n",
    "fts=unicode61:remove_diacritics-0:tokenchars-_-.:/\n",
    "literals=case-sensitive:max-64:bytes-2..128\n",
    "quote_bloom=byte-trigram:bits-4096:hashes-4:sha256-double-hash-be\n",
);
const PIPELINE_DESCRIPTOR_SUFFIX: &str =
    "jsonl=v1:complete-snapshot:id-identity:plain-text:strict-bounds\n";
static PIPELINE_DESCRIPTOR: OnceLock<String> = OnceLock::new();

pub fn schema_checksum() -> Sha256Digest {
    schema_checksum_through(SCHEMA_VERSION)
}

pub(crate) fn schema_checksum_through(version: u32) -> Sha256Digest {
    if version == 1 {
        return Sha256Digest::of_bytes(MIGRATION_0001.as_bytes());
    }
    let mut hasher = Sha256::new();
    hasher.update(b"hsum.schema-chain.v1\0");
    for (migration_version, sql) in MIGRATIONS {
        if migration_version > version {
            break;
        }
        hasher.update(migration_version.to_be_bytes());
        hasher.update((sql.len() as u64).to_be_bytes());
        hasher.update(sql.as_bytes());
    }
    Sha256Digest::from_bytes(hasher.finalize().into())
}

pub(crate) fn migration_checksum(version: u32) -> Option<Sha256Digest> {
    MIGRATIONS.iter().find_map(|(candidate, sql)| {
        (*candidate == version).then(|| Sha256Digest::of_bytes(sql.as_bytes()))
    })
}

pub fn pipeline_fingerprint() -> Sha256Digest {
    Sha256Digest::of_bytes(pipeline_descriptor().as_bytes())
}

pub fn pipeline_fingerprint_for(
    embedding_profile: &crate::store::vector::IndexEmbeddingProfile,
) -> Sha256Digest {
    Sha256Digest::of_bytes(pipeline_descriptor_for(embedding_profile).as_bytes())
}

pub fn pipeline_descriptor() -> &'static str {
    PIPELINE_DESCRIPTOR
        .get_or_init(|| {
            let extensions = ChunkKind::EXTENSIONS
                .iter()
                .map(|(extension, _)| *extension)
                .collect::<Vec<_>>()
                .join(",");
            format!(
                "hsum.pipeline.v1\nfilesystem=v1:extensions={extensions}:symlinks=never\n\
                 {PIPELINE_DESCRIPTOR_PREFIX}embedding=none\n{PIPELINE_DESCRIPTOR_SUFFIX}"
            )
        })
        .as_str()
}

fn pipeline_descriptor_for(
    embedding_profile: &crate::store::vector::IndexEmbeddingProfile,
) -> String {
    if embedding_profile == &crate::store::vector::IndexEmbeddingProfile::LexicalOnly {
        return pipeline_descriptor().to_owned();
    }
    let extensions = ChunkKind::EXTENSIONS
        .iter()
        .map(|(extension, _)| *extension)
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "hsum.pipeline.v1\nfilesystem=v1:extensions={extensions}:symlinks=never\n\
         {PIPELINE_DESCRIPTOR_PREFIX}{}{PIPELINE_DESCRIPTOR_SUFFIX}",
        embedding_profile.descriptor(),
    )
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
    fn alpha_2_pipeline_fingerprint_is_frozen() {
        // Bumped when Tier 1 language extensions were added. Changing this
        // value makes every existing index unopenable: `inspect_connection`
        // runs on every open (see `IndexDb::open_existing`), so all commands
        // fail with "pipeline fingerprint does not match this binary". Ingest
        // cannot repair it either, because ingest must open the index first.
        // `init --rebuild` validates and replaces the incompatible index.
        assert_eq!(
            pipeline_fingerprint().to_string(),
            "d61e7a913b8d3fd7b58a3eeefb8ac7d4633647ad646c2d2c1a58db6d13ca980d"
        );
    }

    #[test]
    fn published_alpha_1_schema_checksum_is_frozen() {
        assert_eq!(
            schema_checksum_through(1).to_string(),
            "006c34cb6bec7b2312c36edf0cc2dd3bfdb6827bedacecf611efe6adad011e4b"
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
