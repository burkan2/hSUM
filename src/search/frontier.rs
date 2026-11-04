//! Bounded request-local lexical ranking; larger inputs use the existing SQL.

use std::cmp::Ordering;
use std::collections::{BTreeSet, BinaryHeap};

use rusqlite::{Connection, params, types::ValueRef};

use super::SearchError;
use super::retrieval::PASSAGE_SCOPE_JOINS;

// The score buffer is at most 2 MiB. Additional storage is a 500-score heap,
// 500 fixed-width eligible keys, and one 1024-ID batch (including its JSON).
const MAX_SCORE_ROWS: usize = 131_072;
const BATCH_ROWS: usize = 1024;
const MAX_RANKED_ROWS: usize = 500;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Phase {
    Scoring,
    Selection,
    Metadata,
}

#[derive(Clone, Copy, Debug)]
struct Score(f64);

impl PartialEq for Score {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Score {}
impl PartialOrd for Score {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Score {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

#[derive(Clone, Copy)]
struct ScoredRow {
    id: i64,
    score: Score,
}

struct ScoredMatches {
    rows: Vec<ScoredRow>,
    cutoff: Option<Score>,
}

#[derive(Clone, Copy)]
struct Eligible {
    row: ScoredRow,
    source: [u8; 16],
    document: [u8; 16],
    start: i64,
}

impl PartialEq for Eligible {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Eligible {}
impl PartialOrd for Eligible {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Eligible {
    fn cmp(&self, other: &Self) -> Ordering {
        self.row
            .score
            .cmp(&other.row.score)
            .then(self.source.cmp(&other.source))
            .then(self.document.cmp(&other.document))
            .then(self.start.cmp(&other.start))
    }
}

pub(super) fn rank(
    connection: &Connection,
    project: &[u8],
    expression: &str,
    k: usize,
    check: &mut impl FnMut(Phase) -> Result<(), SearchError>,
) -> Result<Option<Vec<(i64, f64)>>, SearchError> {
    rank_with_limit(connection, project, expression, k, MAX_SCORE_ROWS, check)
}

fn rank_with_limit(
    connection: &Connection,
    project: &[u8],
    expression: &str,
    k: usize,
    row_limit: usize,
    check: &mut impl FnMut(Phase) -> Result<(), SearchError>,
) -> Result<Option<Vec<(i64, f64)>>, SearchError> {
    check(Phase::Scoring)?;
    if k == 0 || k > MAX_RANKED_ROWS {
        return Ok(None);
    }
    let count: i64 =
        connection.query_row("SELECT count(*) FROM main.passages_fts_docsize", [], |r| {
            r.get(0)
        })?;
    let Ok(capacity) = usize::try_from(count) else {
        return Ok(None);
    };
    if capacity > row_limit.min(MAX_SCORE_ROWS) {
        return Ok(None);
    }
    let Some(ScoredMatches {
        rows: scores,
        cutoff,
    }) = collect_scores(connection, expression, k, capacity, check)?
    else {
        return Ok(None);
    };
    check(Phase::Selection)?;
    let Some(cutoff) = cutoff else {
        return Ok(Some(Vec::new()));
    };
    let mut eligible = BTreeSet::new();
    if !collect_eligible(
        connection,
        project,
        scores.iter(),
        |row| row.score <= cutoff,
        k,
        &mut eligible,
        check,
    )? {
        return Ok(None);
    }
    // Every unseen score is strictly worse. Only an underfilled eligible
    // frontier needs the rest of the already-scored matches.
    if eligible.len() < k
        && !collect_eligible(
            connection,
            project,
            scores.iter(),
            |row| row.score > cutoff,
            k,
            &mut eligible,
            check,
        )?
    {
        return Ok(None);
    }
    check(Phase::Selection)?;
    Ok(Some(
        eligible
            .into_iter()
            .map(|entry| (entry.row.id, entry.row.score.0))
            .collect(),
    ))
}

fn collect_scores(
    connection: &Connection,
    expression: &str,
    k: usize,
    capacity: usize,
    check: &mut impl FnMut(Phase) -> Result<(), SearchError>,
) -> Result<Option<ScoredMatches>, SearchError> {
    let mut scores: Vec<ScoredRow> = Vec::new();
    let mut best: BinaryHeap<Score> = BinaryHeap::new();
    if capacity > MAX_SCORE_ROWS
        || scores.try_reserve_exact(capacity).is_err()
        || scores.capacity() > MAX_SCORE_ROWS
        || best.try_reserve_exact(k).is_err()
        || best.capacity() > MAX_RANKED_ROWS
    {
        return Ok(None);
    }
    let mut statement = connection.prepare(
        "SELECT passages_fts.rowid, hsum_bm25(passages_fts,
                CASE WHEN typeof(sizes.sz)='blob' AND length(sizes.sz)<=27
                     THEN sizes.sz ELSE NULL END)
         FROM passages_fts
         LEFT JOIN main.passages_fts_docsize AS sizes ON sizes.id=passages_fts.rowid
         WHERE passages_fts MATCH ?1 ORDER BY passages_fts.rowid",
    )?;
    let mut rows = statement.query([expression])?;
    while let Some(row) = rows.next()? {
        if scores.len().is_multiple_of(256) {
            check(Phase::Scoring)?;
        }
        // A stale/corrupt count must never grow the bounded allocation.
        if scores.len() == capacity {
            return Ok(None);
        }
        let score = row.get::<_, f64>(1)?;
        if !score.is_finite() {
            return Err(SearchError::NonFiniteScore);
        }
        let value = ScoredRow {
            id: row.get(0)?,
            score: Score(score),
        };
        if best.len() < k {
            best.push(value.score);
        } else if let Some(mut worst) = best.peek_mut()
            && value.score < *worst
        {
            *worst = value.score;
        }
        scores.push(value);
    }
    check(Phase::Scoring)?;
    Ok(Some(ScoredMatches {
        rows: scores,
        cutoff: best.peek().copied(),
    }))
}

fn collect_eligible<'a>(
    connection: &Connection,
    project: &[u8],
    mut scores: impl Iterator<Item = &'a ScoredRow>,
    include: impl Fn(&ScoredRow) -> bool,
    k: usize,
    eligible: &mut BTreeSet<Eligible>,
    check: &mut impl FnMut(Phase) -> Result<(), SearchError>,
) -> Result<bool, SearchError> {
    let sql = format!(
        "SELECT j.key,ap.id,
           CASE WHEN typeof(ap.source_id)='blob' AND length(ap.source_id)=16 THEN ap.source_id END,
           CASE WHEN typeof(ap.document_id)='blob' AND length(ap.document_id)=16 THEN ap.document_id END,
           CASE WHEN typeof(c.start_byte)='integer' THEN c.start_byte END
         FROM json_each(?2) AS j
         CROSS JOIN active_passages AS ap ON ap.id=j.value
         {PASSAGE_SCOPE_JOINS}
         JOIN chunks AS c INDEXED BY chunks_order_idx ON c.id=ap.chunk_id
         WHERE ps.project_id=?1");
    let mut statement = connection.prepare(&sql)?;
    loop {
        check(Phase::Metadata)?;
        let mut batch = Vec::with_capacity(BATCH_ROWS);
        for (index, row) in scores.by_ref().enumerate() {
            if index.is_multiple_of(256) {
                check(Phase::Metadata)?;
            }
            if include(row) {
                batch.push(*row);
            }
            if batch.len() == BATCH_ROWS {
                break;
            }
        }
        if batch.is_empty() {
            break;
        }
        let ids: Vec<i64> = batch.iter().map(|row| row.id).collect();
        let json = serde_json::to_string(&ids)
            .map_err(|_| SearchError::Corrupt("lexical candidate serialization"))?;
        let mut rows = statement.query(params![project, json])?;
        let mut examined = 0_usize;
        while let Some(row) = rows.next()? {
            if examined.is_multiple_of(256) {
                check(Phase::Metadata)?;
            }
            examined += 1;
            let index = usize::try_from(row.get::<_, i64>(0)?)
                .map_err(|_| SearchError::Corrupt("lexical candidate index"))?;
            let Some(score) = batch.get(index) else {
                return Err(SearchError::Corrupt("lexical candidate index"));
            };
            if row.get::<_, i64>(1)? != score.id {
                return Err(SearchError::Corrupt("lexical candidate identity"));
            }
            let (ValueRef::Blob(source), ValueRef::Blob(document), ValueRef::Integer(start)) =
                (row.get_ref(2)?, row.get_ref(3)?, row.get_ref(4)?)
            else {
                return Ok(false);
            };
            let (Ok(source), Ok(document)) = (source.try_into(), document.try_into()) else {
                return Ok(false);
            };
            if start < 0 {
                return Ok(false);
            }
            let entry = Eligible {
                row: *score,
                source,
                document,
                start,
            };
            if eligible.len() == k && eligible.last().is_some_and(|worst| entry > *worst) {
                continue;
            }
            // SQL does not promise the encounter order for completely equal
            // keys. Preserve the established query for these ambiguous cases.
            if !eligible.insert(entry) {
                return Ok(false);
            }
            if eligible.len() > k {
                eligible.pop_last();
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{Connection, params};

    const PROJECT: [u8; 16] = [1; 16];

    fn fixture(count: usize) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE VIRTUAL TABLE passages_fts USING fts5(title,source_uri,content);
             CREATE TABLE active_passages(id INTEGER PRIMARY KEY,source_id,document_id,document_version_id,chunk_id);
             CREATE TABLE project_sources(source_id,project_id,removed_at);
             CREATE TABLE document_heads(document_id PRIMARY KEY,document_version_id,state);
             CREATE TABLE document_versions(id PRIMARY KEY,document_id);
             CREATE TABLE chunks(id INTEGER PRIMARY KEY,start_byte);
             CREATE INDEX chunks_order_idx ON chunks(id,start_byte);",
        ).unwrap();
        c.execute("INSERT INTO project_sources VALUES(?1,?1,NULL)", [PROJECT])
            .unwrap();
        c.execute_batch("BEGIN").unwrap();
        for n in 1..=count {
            let id = n as i64;
            // Reverse tie order relative to FTS rowid.
            let document = ((count - n + 1) as u128).to_be_bytes();
            c.execute(
                "INSERT INTO passages_fts(rowid,content) VALUES(?1,'shared evidence')",
                [id],
            )
            .unwrap();
            c.execute(
                "INSERT INTO active_passages VALUES(?1,?2,?3,?1,?1)",
                params![id, PROJECT, document],
            )
            .unwrap();
            c.execute(
                "INSERT INTO document_heads VALUES(?1,?2,'active')",
                params![document, id],
            )
            .unwrap();
            c.execute(
                "INSERT INTO document_versions VALUES(?1,?2)",
                params![id, document],
            )
            .unwrap();
            c.execute("INSERT INTO chunks VALUES(?1,0)", [id]).unwrap();
        }
        c.execute_batch("COMMIT").unwrap();
        super::super::ranking::install(&c).unwrap();
        c
    }

    fn baseline(c: &Connection, k: usize) -> Vec<(i64, u64)> {
        c.prepare("SELECT ap.id,bm25(passages_fts) AS score
          FROM passages_fts CROSS JOIN active_passages ap ON ap.id=passages_fts.rowid
          JOIN project_sources ps ON ps.source_id=ap.source_id AND ps.removed_at IS NULL
          JOIN document_heads dh ON dh.document_id=ap.document_id AND dh.document_version_id=ap.document_version_id AND dh.state='active'
          JOIN document_versions dv ON dv.id=ap.document_version_id AND dv.document_id=ap.document_id
          JOIN chunks c ON c.id=ap.chunk_id
          WHERE ps.project_id=?1 AND passages_fts MATCH 'shared'
          ORDER BY score,ap.source_id,ap.document_id,c.start_byte LIMIT ?2").unwrap()
            .query_map(params![PROJECT,k as i64], |r| Ok((r.get(0)?,r.get::<_,f64>(1)?.to_bits()))).unwrap()
            .collect::<Result<Vec<_>,_>>().unwrap()
    }

    fn optimized(c: &Connection) -> Option<Vec<(i64, u64)>> {
        rank(c, &PROJECT, "shared", 500, &mut |_| Ok(()))
            .unwrap()
            .map(|rows| {
                rows.into_iter()
                    .map(|(id, score)| (id, score.to_bits()))
                    .collect()
            })
    }

    #[test]
    fn cardinalities_and_cutoff_ties_match_native_bits_and_order() {
        for count in [0, 49, 50, 51, 499, 500, 501, 1400] {
            let c = fixture(count);
            assert_eq!(optimized(&c), Some(baseline(&c, 500)), "count={count}");
        }
    }

    #[test]
    fn foreign_removed_inactive_and_replaced_heads_match_scope() {
        let c = fixture(1400);
        c.execute(
            "INSERT INTO project_sources VALUES(?1,?1,NULL)",
            [[2_u8; 16]],
        )
        .unwrap();
        c.execute(
            "INSERT INTO project_sources VALUES(?1,?2,'removed')",
            params![[3_u8; 16], PROJECT],
        )
        .unwrap();
        c.execute(
            "UPDATE active_passages SET source_id=?1 WHERE id<=300",
            [[2_u8; 16]],
        )
        .unwrap();
        c.execute(
            "UPDATE active_passages SET source_id=?1 WHERE id>300 AND id<=600",
            [[3_u8; 16]],
        )
        .unwrap();
        c.execute(
            "UPDATE passages_fts SET content='shared' WHERE rowid<=600",
            [],
        )
        .unwrap();
        c.execute_batch("UPDATE document_heads SET state='inactive' WHERE document_version_id%13=0;
                         UPDATE document_heads SET document_version_id=document_version_id+2000 WHERE document_version_id%17=0;").unwrap();
        assert_eq!(optimized(&c), Some(baseline(&c, 500)));
    }

    #[test]
    fn malformed_tie_and_ambiguous_complete_keys_fall_back() {
        let c = fixture(51);
        c.execute("UPDATE chunks SET start_byte='invalid' WHERE id=1", [])
            .unwrap();
        assert!(optimized(&c).is_none());
        c.execute("UPDATE chunks SET start_byte=0 WHERE id=1", [])
            .unwrap();
        c.execute_batch("UPDATE active_passages SET document_id=(SELECT document_id FROM active_passages WHERE id=1),document_version_id=1 WHERE id=2;").unwrap();
        assert!(optimized(&c).is_none());
    }

    #[test]
    fn row_limit_boundary_falls_back_without_truncating() {
        let c = fixture(51);
        let result = rank_with_limit(&c, &PROJECT, "shared", 50, 50, &mut |_| Ok(())).unwrap();
        assert!(result.is_none());
        let result = rank_with_limit(&c, &PROJECT, "shared", 50, 51, &mut |_| Ok(()))
            .unwrap()
            .unwrap();
        assert_eq!(
            result
                .into_iter()
                .map(|(id, s)| (id, s.to_bits()))
                .collect::<Vec<_>>(),
            baseline(&c, 50)
        );
        assert!(
            collect_scores(&c, "shared", 50, 1, &mut |_| Ok(()))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            std::mem::size_of::<ScoredRow>() * MAX_SCORE_ROWS,
            2 * 1024 * 1024
        );
    }

    #[test]
    fn persistent_cancellation_in_each_phase_propagates_and_recovers() {
        let c = fixture(501);
        for stop in [Phase::Scoring, Phase::Selection, Phase::Metadata] {
            let result = rank(&c, &PROJECT, "shared", 500, &mut |phase| {
                if phase == stop {
                    Err(super::super::SearchError::DeadlineExceeded)
                } else {
                    Ok(())
                }
            });
            assert!(matches!(
                result,
                Err(super::super::SearchError::DeadlineExceeded)
            ));
            assert_eq!(optimized(&c), Some(baseline(&c, 500)));
        }
    }

    #[test]
    fn cancellation_after_work_started_never_returns_partial_ranking() {
        let c = fixture(1400);
        for (stop, after) in [
            (Phase::Scoring, 3),
            (Phase::Selection, 2),
            (Phase::Metadata, 4),
            (Phase::Metadata, 7),
        ] {
            let mut seen = 0;
            let mut cancelled = false;
            let result = rank(&c, &PROJECT, "shared", 500, &mut |phase| {
                if phase == stop {
                    seen += 1;
                    cancelled |= seen >= after;
                }
                if cancelled {
                    Err(SearchError::DeadlineExceeded)
                } else {
                    Ok(())
                }
            });
            assert!(cancelled, "phase={stop:?}");
            assert!(matches!(result, Err(SearchError::DeadlineExceeded)));
            assert_eq!(optimized(&c), Some(baseline(&c, 500)));
        }
    }

    #[test]
    fn signed_extreme_row_ids_survive_json_and_preserve_score_bits() {
        let c = fixture(5);
        for (old, id) in [(1, i64::MIN), (2, -1), (3, 0), (4, i64::MAX)] {
            c.execute(
                "UPDATE passages_fts SET rowid=?1 WHERE rowid=?2",
                params![id, old],
            )
            .unwrap();
            c.execute(
                "UPDATE active_passages SET id=?1 WHERE id=?2",
                params![id, old],
            )
            .unwrap();
        }
        assert_eq!(optimized(&c), Some(baseline(&c, 500)));
    }
}
