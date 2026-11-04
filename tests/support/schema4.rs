use std::path::Path;

use rusqlite::Connection;
use sha2::{Digest, Sha256};

pub fn downgrade_to_schema_4(path: &Path) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "DROP INDEX chunks_order_idx;
         DELETE FROM schema_migrations WHERE version = 5;
         UPDATE index_meta SET value = CAST('4' AS BLOB) WHERE key = 'schema_version';
         PRAGMA user_version = 4;",
        )
        .unwrap();
    let migrations = [
        (1_u32, include_str!("../../migrations/0001_alpha1.sql")),
        (
            2_u32,
            include_str!("../../migrations/0002_jsonl_sources.sql"),
        ),
        (3_u32, include_str!("../../migrations/0003_maintenance.sql")),
        (
            4_u32,
            include_str!("../../migrations/0004_vector_storage.sql"),
        ),
    ];
    let mut hasher = Sha256::new();
    hasher.update(b"hsum.schema-chain.v1\0");
    let released = Connection::open_in_memory().unwrap();
    for (version, sql) in migrations {
        hasher.update(version.to_be_bytes());
        hasher.update((sql.len() as u64).to_be_bytes());
        hasher.update(sql.as_bytes());
        released.execute_batch(sql).unwrap();
    }
    connection
        .execute(
            "UPDATE index_meta SET value = ?1 WHERE key = 'schema_checksum'",
            [hasher.finalize().as_slice()],
        )
        .unwrap();
    assert_eq!(schema_rows(&connection), schema_rows(&released));
}

fn schema_rows(connection: &Connection) -> Vec<(String, String, String, String)> {
    connection
        .prepare(
            "SELECT type, name, tbl_name, COALESCE(sql, '')
         FROM sqlite_schema ORDER BY type, name, tbl_name",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}
