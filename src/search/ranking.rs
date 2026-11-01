//! Unit-weight BM25 for the fixed, three-column `passages_fts` schema.
//!
//! Uses SQLite's documented FTS5 extension API and docsize format:
//! <https://www.sqlite.org/fts5.html>. The formula and operation order match
//! SQLite's public-domain fts5_aux.c. Phrase-local iteration avoids merging
//! positions, and a bounded, query-local token-length array avoids random
//! docsize lookups. Nothing is cached across snapshots or connections.

use std::ffi::c_void;
use std::ptr;

use rusqlite::{Connection, ffi};

const MAX_CACHED_ROWID: i64 = 2_000_000; // At most 8 MB per active FTS cursor.
const MIN_CACHED_ROWS: i64 = 4_096;

fn sqlite_error(code: i32) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(ffi::Error::new(code), None)
}

fn code(error: rusqlite::Error) -> i32 {
    error
        .sqlite_error()
        .map_or(ffi::SQLITE_CORRUPT, |error| error.extended_code)
}

fn check(code: i32) -> Result<(), i32> {
    if code == ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(code)
    }
}

// Each docsize blob has precisely three SQLite varints. Reject truncated,
// overflowing and extra counts before installing any cache. Never truncate ids.
fn token_total(bytes: &[u8]) -> Result<i32, i32> {
    let mut offset = 0;
    let mut total = 0_i32;
    for _ in 0..3 {
        let mut value = 0_u64;
        for index in 0..9 {
            let byte = *bytes.get(offset).ok_or(ffi::SQLITE_CORRUPT)?;
            offset += 1;
            if index == 8 {
                value = (value << 8) | u64::from(byte);
                break;
            }
            value = (value << 7) | u64::from(byte & 0x7f);
            if byte & 0x80 == 0 {
                break;
            }
        }
        total = total
            .checked_add(i32::try_from(value).map_err(|_| ffi::SQLITE_CORRUPT)?)
            .ok_or(ffi::SQLITE_CORRUPT)?;
    }
    if offset != bytes.len() {
        return Err(ffi::SQLITE_CORRUPT);
    }
    Ok(total)
}

fn load_lengths(
    connection: &Connection,
    rows: i64,
    mut poll: impl FnMut() -> Result<(), i32>,
) -> Result<Vec<i32>, i32> {
    if rows < MIN_CACHED_ROWS {
        return Ok(Vec::new());
    }
    let maximum: Option<i64> = connection
        .query_row("SELECT max(id) FROM main.passages_fts_docsize", [], |row| {
            row.get(0)
        })
        .map_err(code)?;
    let Some(maximum) = maximum else {
        return Ok(Vec::new());
    };
    if !(1..=MAX_CACHED_ROWID).contains(&maximum) || maximum / 4 > rows {
        return Ok(Vec::new());
    }
    let size = usize::try_from(maximum + 1).map_err(|_| ffi::SQLITE_TOOBIG)?;
    let mut lengths = Vec::new();
    lengths
        .try_reserve_exact(size)
        .map_err(|_| ffi::SQLITE_NOMEM)?;
    lengths.resize(size, -1);
    let mut statement = connection
        .prepare(
            "SELECT id, CASE WHEN typeof(sz)='blob' AND length(sz)<=27 THEN sz ELSE NULL END
         FROM main.passages_fts_docsize",
        )
        .map_err(code)?;
    let mut cursor = statement.query([]).map_err(code)?;
    while let Some(row) = cursor.next().map_err(code)? {
        poll()?;
        let id: i64 = row.get(0).map_err(code)?;
        // Negative rowids can be valid FTS rows; they use xColumnSize below.
        if id < 0 {
            continue;
        }
        let id = usize::try_from(id).map_err(|_| ffi::SQLITE_CORRUPT)?;
        let value = row.get_ref(1).map_err(code)?;
        // An unrelated malformed row must not poison a selective query. Leave
        // its sentinel in place so SQLite validates it if it is actually scored.
        let length = value
            .as_blob()
            .ok()
            .and_then(|bytes| token_total(bytes).ok())
            .unwrap_or(-1);
        *lengths.get_mut(id).ok_or(ffi::SQLITE_CORRUPT)? = length;
    }
    Ok(lengths)
}

struct QueryData {
    average_length: f64,
    idf: Vec<f64>,
    lengths: Option<Vec<i32>>,
    scored: usize,
    rows: i64,
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Statistics,
    Positions,
    Lengths,
}

#[cfg(test)]
thread_local! {
    static INTERRUPT_PHASE: std::cell::Cell<Option<Phase>> = const { std::cell::Cell::new(None) };
    pub(super) static INSTALLATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// The only raw-pointer boundary is SQLite's synchronous auxiliary-function API.
// SQLite supplies live callback pointers, and owns each Box after xSetAuxdata.
// All Rust unwinding is caught before returning through the C ABI.
#[allow(unsafe_code)]
mod extension {
    use super::*;

    unsafe extern "C" {
        fn hsum_bm25_idf(rows: i64, hits: i64) -> f64;
        fn hsum_bm25_accumulate(
            score: f64,
            idf: f64,
            frequency: f64,
            length: f64,
            average_length: f64,
        ) -> f64;
    }

    pub(in crate::search) fn install(connection: &Connection) -> rusqlite::Result<()> {
        // SAFETY: the connection outlives this call; the temporary statement is
        // always finalized. The FTS5 API pointer remains owned by SQLite.
        unsafe {
            let db = connection.handle();
            let mut statement = ptr::null_mut();
            let mut api: *mut ffi::fts5_api = ptr::null_mut();
            check(ffi::sqlite3_prepare_v2(
                db,
                c"SELECT fts5(?1)".as_ptr(),
                -1,
                &mut statement,
                ptr::null_mut(),
            ))
            .map_err(sqlite_error)?;
            let result = check(ffi::sqlite3_bind_pointer(
                statement,
                1,
                (&mut api as *mut *mut ffi::fts5_api).cast(),
                c"fts5_api_ptr".as_ptr(),
                None,
            ))
            .and_then(|()| match ffi::sqlite3_step(statement) {
                ffi::SQLITE_ROW => Ok(()),
                error => Err(error),
            });
            let finalized = ffi::sqlite3_finalize(statement);
            result
                .and_then(|()| check(finalized))
                .map_err(sqlite_error)?;
            let api_ref = api
                .as_ref()
                .ok_or_else(|| sqlite_error(ffi::SQLITE_ERROR))?;
            let create = api_ref
                .xCreateFunction
                .ok_or_else(|| sqlite_error(ffi::SQLITE_ERROR))?;
            check(create(
                api,
                c"hsum_bm25".as_ptr(),
                ptr::null_mut(),
                Some(rank),
                None,
            ))
            .map_err(sqlite_error)?;
            #[cfg(test)]
            INSTALLATIONS.with(|count| count.set(count.get() + 1));
            Ok(())
        }
    }

    unsafe extern "C" fn destroy(data: *mut c_void) {
        // SAFETY: xSetAuxdata calls this exactly once for this Box, including
        // when installing it fails. QueryData has no panicking destructor.
        unsafe {
            drop(Box::from_raw(data.cast::<QueryData>()));
        }
    }

    struct HitCount {
        count: i64,
        db: *mut ffi::sqlite3,
    }

    unsafe fn poll(db: *mut ffi::sqlite3, _phase: Phase) -> Result<(), i32> {
        // SAFETY: called only with the live handle from the current callback.
        unsafe {
            #[cfg(test)]
            INTERRUPT_PHASE.with(|phase| {
                if phase.get() == Some(_phase) {
                    phase.set(None);
                    ffi::sqlite3_interrupt(db);
                }
            });
            if ffi::sqlite3_is_interrupted(db) != 0 {
                Err(ffi::SQLITE_INTERRUPT)
            } else {
                Ok(())
            }
        }
    }

    unsafe extern "C" fn count_hit(
        _api: *const ffi::Fts5ExtensionApi,
        _fts: *mut ffi::Fts5Context,
        data: *mut c_void,
    ) -> i32 {
        // SAFETY: xQueryPhrase calls synchronously with our stack HitCount.
        unsafe {
            let hits = &mut *data.cast::<HitCount>();
            if hits.count % 256 == 0
                && let Err(error) = poll(hits.db, Phase::Statistics)
            {
                return error;
            }
            match hits.count.checked_add(1) {
                Some(count) => {
                    hits.count = count;
                    ffi::SQLITE_OK
                }
                None => ffi::SQLITE_TOOBIG,
            }
        }
    }

    unsafe fn score(
        api: &ffi::Fts5ExtensionApi,
        fts: *mut ffi::Fts5Context,
        db: *mut ffi::sqlite3,
    ) -> Result<f64, i32> {
        // SAFETY: all calls are synchronous on SQLite's callback thread. The
        // temporary rusqlite wrapper is nonowning; it and its statements are
        // dropped before the callback returns. It never changes connection
        // settings or acquires an interrupt handle. Auxdata belongs to this
        // FTS cursor and cannot escape its snapshot.
        unsafe {
            if api.iVersion < 2 {
                return Err(ffi::SQLITE_ERROR);
            }
            let mut data = api.xGetAuxdata.ok_or(ffi::SQLITE_ERROR)?(fts, 0).cast::<QueryData>();
            if data.is_null() {
                let phrases = api.xPhraseCount.ok_or(ffi::SQLITE_ERROR)?(fts);
                if !(0..=4096).contains(&phrases)
                    || api.xColumnCount.ok_or(ffi::SQLITE_ERROR)?(fts) != 3
                {
                    return Err(ffi::SQLITE_ERROR);
                }
                let mut rows = 0;
                let mut tokens = 0;
                check(api.xRowCount.ok_or(ffi::SQLITE_ERROR)?(fts, &mut rows))?;
                check(api.xColumnTotalSize.ok_or(ffi::SQLITE_ERROR)?(
                    fts,
                    -1,
                    &mut tokens,
                ))?;
                if rows <= 0 || tokens <= 0 {
                    return Err(ffi::SQLITE_CORRUPT);
                }
                let mut idf = Vec::new();
                idf.try_reserve_exact(phrases as usize)
                    .map_err(|_| ffi::SQLITE_NOMEM)?;
                for phrase in 0..phrases {
                    let mut hits = HitCount { count: 0, db };
                    check(api.xQueryPhrase.ok_or(ffi::SQLITE_ERROR)?(
                        fts,
                        phrase,
                        (&mut hits as *mut HitCount).cast(),
                        Some(count_hit),
                    ))?;
                    if hits.count > rows {
                        return Err(ffi::SQLITE_CORRUPT);
                    }
                    idf.push(hsum_bm25_idf(rows, hits.count));
                }
                let set = api.xSetAuxdata.ok_or(ffi::SQLITE_ERROR)?;
                data = Box::into_raw(Box::new(QueryData {
                    average_length: tokens as f64 / rows as f64,
                    idf,
                    lengths: None,
                    scored: 0,
                    rows,
                }));
                check(set(fts, data.cast(), Some(destroy)))?;
            }
            let data = &mut *data;
            // Only a broad query pays for the length cache. Exact phrase paging
            // and selective identifiers continue using native per-row lookups.
            if data.scored == MIN_CACHED_ROWS as usize && data.lengths.is_none() {
                let borrowed = Connection::from_handle(db).map_err(code)?;
                data.lengths = Some(load_lengths(&borrowed, data.rows, || {
                    poll(db, Phase::Lengths)
                })?);
            }
            data.scored = data.scored.saturating_add(1);
            let rowid = api.xRowid.ok_or(ffi::SQLITE_ERROR)?(fts);
            let mut length = usize::try_from(rowid)
                .ok()
                .and_then(|id| data.lengths.as_ref().and_then(|lengths| lengths.get(id)))
                .copied()
                .unwrap_or(-1);
            if length < 0 {
                check(api.xColumnSize.ok_or(ffi::SQLITE_ERROR)?(
                    fts,
                    -1,
                    &mut length,
                ))?;
            }
            if length < 0 {
                return Err(ffi::SQLITE_CORRUPT);
            }
            let mut score = 0.0;
            for (phrase, idf) in data.idf.iter().enumerate() {
                let mut iterator = ffi::Fts5PhraseIter {
                    a: ptr::null(),
                    b: ptr::null(),
                };
                let mut column = 0;
                let mut offset = 0;
                let mut frequency = 0_u64;
                check(api.xPhraseFirst.ok_or(ffi::SQLITE_ERROR)?(
                    fts,
                    phrase as i32,
                    &mut iterator,
                    &mut column,
                    &mut offset,
                ))?;
                while column >= 0 {
                    if frequency.is_multiple_of(256) {
                        poll(db, Phase::Positions)?;
                    }
                    frequency += 1;
                    api.xPhraseNext.ok_or(ffi::SQLITE_ERROR)?(
                        fts,
                        &mut iterator,
                        &mut column,
                        &mut offset,
                    );
                }
                let frequency = frequency as f64;
                score = hsum_bm25_accumulate(
                    score,
                    *idf,
                    frequency,
                    length as f64,
                    data.average_length,
                );
            }
            if !score.is_finite() {
                return Err(ffi::SQLITE_CORRUPT);
            }
            Ok(-score)
        }
    }

    unsafe extern "C" fn rank(
        api: *const ffi::Fts5ExtensionApi,
        fts: *mut ffi::Fts5Context,
        context: *mut ffi::sqlite3_context,
        argc: i32,
        _argv: *mut *mut ffi::sqlite3_value,
    ) {
        // SAFETY: SQLite supplies these live pointers for the duration of the
        // callback. No Rust panic is allowed to cross the C boundary.
        unsafe {
            let result = std::panic::catch_unwind(|| {
                if argc != 0 {
                    return Err(ffi::SQLITE_MISUSE);
                }
                score(&*api, fts, ffi::sqlite3_context_db_handle(context))
            })
            .unwrap_or(Err(ffi::SQLITE_ERROR));
            match result {
                Ok(value) => ffi::sqlite3_result_double(context, value),
                Err(error) => ffi::sqlite3_result_error_code(context, error),
            }
        }
    }
}

pub(super) use extension::install;

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(count: i64) -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE VIRTUAL TABLE passages_fts USING fts5(title, source_uri, content);
             BEGIN;",
            )
            .unwrap();
        for row in 1..=count {
            connection
                .execute(
                    "INSERT INTO passages_fts(rowid,title,source_uri,content) VALUES(?1,?2,?3,?4)",
                    rusqlite::params![
                        row * 2,
                        format!("evidence title {}", row % 17),
                        format!("repo://citation/{row}"),
                        format!(
                            "{} {} immutable citation café",
                            "evidence ".repeat((row % 11) as usize),
                            "alpha beta ".repeat((row % 7) as usize)
                        )
                    ],
                )
                .unwrap();
        }
        connection.execute_batch("COMMIT").unwrap();
        install(&connection).unwrap();
        connection
    }

    fn assert_scores(connection: &Connection) {
        for expression in [
            "evidence",
            "evidence AND citation",
            "evidence OR title",
            "evidence NOT alpha",
            "\"alpha beta\"",
            "NEAR(evidence citation, 5)",
            "title:evidence",
            "café",
            "cit*",
            "evidence AND evidence",
        ] {
            assert_eq!(
                scores(connection, "bm25", expression).unwrap(),
                scores(connection, "hsum_bm25", expression).unwrap(),
                "{expression}"
            );
        }
    }

    fn scores(
        connection: &Connection,
        function: &str,
        expression: &str,
    ) -> rusqlite::Result<Vec<(i64, u64)>> {
        // Separate statements: evaluating native bm25 first on a shared FTS
        // cursor could mask a custom iterator bug by populating native caches.
        connection.prepare(&format!("SELECT rowid,{function}(passages_fts) FROM passages_fts WHERE passages_fts MATCH ?1"))?
            .query_map([expression], |row| Ok((row.get(0)?, row.get::<_,f64>(1)?.to_bits())))?
            .collect()
    }

    #[test]
    fn token_lengths_validate_all_varint_boundaries() {
        assert_eq!(token_total(&[0, 1, 127]), Ok(128));
        assert_eq!(
            token_total(&[0x81, 0, 0xff, 0x7f, 0x81, 0x80, 0]),
            Ok(128 + 16383 + 16384)
        );
        assert_eq!(
            token_total(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 1, 0, 0]),
            Ok(1)
        );
        for bytes in [
            vec![],
            vec![1, 2],
            vec![1, 2, 3, 4],
            vec![0x80],
            vec![0xff; 9],
            vec![0x87, 0xff, 0xff, 0xff, 0x7f, 1, 0],
        ] {
            assert_eq!(token_total(&bytes), Err(ffi::SQLITE_CORRUPT), "{bytes:?}");
        }
    }

    #[test]
    fn interrupted_auxiliary_phases_release_state_and_allow_next_query() {
        let connection = fixture(4500);
        for phase in [Phase::Statistics, Phase::Positions, Phase::Lengths] {
            INTERRUPT_PHASE.with(|value| value.set(Some(phase)));
            let error = scores(&connection, "hsum_bm25", "evidence").unwrap_err();
            assert_eq!(
                error.sqlite_error_code(),
                Some(rusqlite::ErrorCode::OperationInterrupted)
            );
            INTERRUPT_PHASE.with(|value| assert!(value.get().is_none(), "phase was not exercised"));
            assert_eq!(
                scores(&connection, "bm25", "evidence").unwrap(),
                scores(&connection, "hsum_bm25", "evidence").unwrap()
            );
        }
    }

    #[test]
    fn malformed_cached_sizes_fall_back_to_native_row_validation() {
        let connection = fixture(4500);
        let original: Vec<u8> = connection
            .query_row(
                "SELECT sz FROM passages_fts_docsize WHERE id=9000",
                [],
                |row| row.get(0),
            )
            .unwrap();
        for value in [
            vec![],
            vec![0x80],
            vec![1, 2],
            vec![1, 2, 3, 4],
            vec![0xff; 9],
            vec![1; 28],
        ] {
            connection
                .execute(
                    "UPDATE passages_fts_docsize SET sz=?1 WHERE id=9000",
                    [value],
                )
                .unwrap();
            let native = scores(&connection, "bm25", "evidence");
            let optimized = scores(&connection, "hsum_bm25", "evidence");
            match (native, optimized) {
                (Ok(native), Ok(optimized)) => assert_eq!(native, optimized),
                (Err(native), Err(optimized)) => {
                    assert_eq!(native.sqlite_error_code(), optimized.sqlite_error_code())
                }
                _ => panic!("custom and native corruption disposition differs"),
            }
            // A selective query that does not score this row still works.
            assert_eq!(
                scores(&connection, "bm25", "source_uri:4499").unwrap(),
                scores(&connection, "hsum_bm25", "source_uri:4499").unwrap()
            );
        }
        connection
            .execute(
                "UPDATE passages_fts_docsize SET sz=?1 WHERE id=9000",
                [original],
            )
            .unwrap();
        connection
            .execute("DELETE FROM passages_fts_docsize WHERE id=9000", [])
            .unwrap();
        assert_eq!(
            scores(&connection, "bm25", "evidence")
                .unwrap_err()
                .sqlite_error_code(),
            scores(&connection, "hsum_bm25", "evidence")
                .unwrap_err()
                .sqlite_error_code()
        );
    }

    #[test]
    fn scores_match_builtin_for_small_and_cached_indexes() {
        for count in [100, 4500] {
            let connection = fixture(count);
            assert_scores(&connection);
            connection
                .execute("DELETE FROM passages_fts WHERE rowid % 6 = 0", [])
                .unwrap();
            connection
                .execute(
                    "UPDATE passages_fts SET content='new evidence evidence' WHERE rowid % 10 = 0",
                    [],
                )
                .unwrap();
            assert_scores(&connection);
        }
    }

    #[test]
    fn sparse_and_large_row_ids_use_builtin_lengths() {
        let connection = fixture(4096);
        connection
            .execute(
                "INSERT INTO passages_fts(rowid,content) VALUES(?1,'evidence citation')",
                [i64::MAX],
            )
            .unwrap();
        assert_scores(&connection);
    }
}
