extern crate hsum;
// Diagnostic only: current SQL/ranker, read-only disposable database.
mod search {
 #[path="/Users/b.k./Desktop/hSUM/src/search/positions.rs"] mod positions;
 #[path="/Users/b.k./Desktop/hSUM/src/search/ranking.rs"] mod ranking;
 pub fn install(c: &rusqlite::Connection) { ranking::install(c).unwrap(); }
}
use rusqlite::{Connection,OpenFlags,params};
use std::time::Instant;
const PASSAGE_SCOPE_JOINS: &str = "
    JOIN project_sources AS ps
      ON ps.source_id = ap.source_id
     AND ps.removed_at IS NULL
    JOIN document_heads AS dh
      ON dh.document_id = ap.document_id
     AND dh.document_version_id = ap.document_version_id
     AND dh.state = 'active'
    JOIN document_versions AS dv
      ON dv.id = ap.document_version_id
     AND dv.document_id = ap.document_id";
fn lexical_ranked_ctes() -> String {
    let eligible = |input: &str| {
        format!(
            "SELECT ap.id, candidates.lexical_score,
                ap.source_id AS tie_source, ap.document_id AS tie_document,
                c.start_byte AS tie_byte
         FROM {input} AS candidates
         CROSS JOIN active_passages AS ap ON ap.id = candidates.id
         {PASSAGE_SCOPE_JOINS}
         JOIN chunks AS c INDEXED BY chunks_order_idx ON c.id = ap.chunk_id
         WHERE ps.project_id = ?1"
        )
    };
    let frontier = eligible("frontier");
    let remainder = eligible("remainder");
    // Score once without per-match metadata joins. Include every tie at the
    // global Kth score, then apply full project/head/version/chunk eligibility.
    // If >=K eligible rows survive, every unseen score is strictly worse.
    // Otherwise include the entire remaining eligible set before LIMIT/OFFSET.
    // No boundary (fewer than K matches) means all scores are in the frontier.
    // Metadata is capped at K; SQLite's score spool still scales with matches.
    format!(
        "scores AS MATERIALIZED (
             SELECT passages_fts.rowid AS id,
                    hsum_bm25(passages_fts,
                        CASE WHEN typeof(sizes.sz)='blob' AND length(sizes.sz)<=27
                             THEN sizes.sz ELSE NULL END) AS lexical_score
             FROM passages_fts
             LEFT JOIN main.passages_fts_docsize AS sizes ON sizes.id=passages_fts.rowid
             WHERE passages_fts MATCH ?2
         ), boundary AS (
             SELECT lexical_score FROM scores ORDER BY lexical_score
             LIMIT 1 OFFSET (?3 + ?4 - 1)
         ), frontier AS MATERIALIZED (
             SELECT * FROM scores
             WHERE (SELECT lexical_score FROM boundary) IS NULL
                OR lexical_score <= (SELECT lexical_score FROM boundary)
         ), eligible_frontier AS MATERIALIZED (
             {frontier}
             ORDER BY lexical_score, tie_source, tie_document, tie_byte
             LIMIT (?3 + ?4)
         ), remainder AS MATERIALIZED (
             SELECT * FROM scores
             WHERE (SELECT count(*) FROM eligible_frontier) < (?3 + ?4)
               AND lexical_score > (SELECT lexical_score FROM boundary)
         ), ranked AS MATERIALIZED (
             SELECT id, lexical_score FROM (
                 SELECT * FROM eligible_frontier
                 UNION ALL
                 {remainder}
             )
             ORDER BY lexical_score, tie_source, tie_document, tie_byte
             LIMIT ?3 OFFSET ?4
         )"
    )
}


fn rust_frontier(c: &Connection, project: &[u8]) -> Vec<(i64,u64)> {
 let expression="\"immutable\" AND \"citation\" AND \"evidence\"";
 let mut scores=c.prepare("SELECT passages_fts.rowid,hsum_bm25(passages_fts,CASE WHEN typeof(sizes.sz)='blob' AND length(sizes.sz)<=27 THEN sizes.sz ELSE NULL END) FROM passages_fts LEFT JOIN main.passages_fts_docsize AS sizes ON sizes.id=passages_fts.rowid WHERE passages_fts MATCH ?1").unwrap().query_map([expression],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,f64>(1)?))).unwrap().collect::<Result<Vec<_>,_>>().unwrap();
 if scores.is_empty(){return Vec::new();}
 let kth=(scores.len().min(500))-1;
 let cutoff=scores.select_nth_unstable_by(kth,|a,b|a.1.total_cmp(&b.1)).1.1;
 let score_map:std::collections::HashMap<i64,f64>=scores.iter().copied().collect();
 let mut ids:Vec<i64>=scores.iter().filter(|v|v.1<=cutoff).map(|v|v.0).collect();
 let sql=format!("SELECT ap.id,ap.source_id,ap.document_id,c.start_byte FROM json_each(?2) AS j CROSS JOIN active_passages AS ap ON ap.id=j.value {} JOIN chunks AS c INDEXED BY chunks_order_idx ON c.id=ap.chunk_id WHERE ps.project_id=?1",PASSAGE_SCOPE_JOINS);
 let query=|ids:&[i64]| {
 c.prepare(&sql).unwrap().query_map(params![project,serde_json::to_string(ids).unwrap()],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,Vec<u8>>(1)?,r.get::<_,Vec<u8>>(2)?,r.get::<_,i64>(3)?))).unwrap().collect::<Result<Vec<_>,_>>().unwrap()
 };
 let mut eligible=query(&ids);
 if eligible.len()<500 {ids=scores.iter().filter(|v|v.1>cutoff).map(|v|v.0).collect();eligible.extend(query(&ids));}
 eligible.sort_unstable_by(|a,b|score_map[&a.0].total_cmp(&score_map[&b.0]).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)).then(a.3.cmp(&b.3)));
 eligible.into_iter().take(500).map(|v|(v.0,score_map[&v.0].to_bits())).collect()
}

fn main() {
 let path=std::env::args().nth(1).unwrap();
 if std::env::args().nth(2).as_deref()==Some("optimize-disposable") {
  let c=Connection::open(&path).unwrap();
  let started=Instant::now(); c.execute_batch("INSERT INTO passages_fts(passages_fts) VALUES('optimize'); PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
  println!("optimize-ms {:.3}",started.elapsed().as_secs_f64()*1000.0);
  return;
 }
 let mut expected=None;
 for (label,pragma) in [("default-before",""),("rust-frontier","RUST"),("default-after","")] {
  for pass in 0..10 {
   let c=Connection::open_with_flags(&path,OpenFlags::SQLITE_OPEN_READ_ONLY|OpenFlags::SQLITE_OPEN_NO_MUTEX).unwrap();
   if pragma!="RUST" {c.execute_batch(pragma).unwrap();} c.execute_batch("BEGIN").unwrap(); search::install(&c);
   let project:Vec<u8>=c.query_row("SELECT project_id FROM project_sources LIMIT 1",[],|r|r.get(0)).unwrap();
   let sql=format!("WITH {} SELECT id,lexical_score FROM ranked",lexical_ranked_ctes());
   let started=Instant::now();
   let rows=if pragma=="RUST" { rust_frontier(&c,&project) } else { c.prepare(&sql).unwrap().query_map(params![project,"\"immutable\" AND \"citation\" AND \"evidence\"",500,0],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,f64>(1)?.to_bits()))).unwrap().collect::<Result<Vec<_>,_>>().unwrap() };
   println!("{} {} {:.3}",label,pass,started.elapsed().as_secs_f64()*1000.0);
   if let Some(ref old)=expected {assert_eq!(&rows,old);} else {expected=Some(rows);}
  }
 }
}
