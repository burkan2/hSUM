#![deny(unsafe_code)]

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use clap::Parser;
use rusqlite::{Connection, ErrorCode, TransactionBehavior, params};
use serde::Serialize;

const REPORT_SCHEMA: &str = "hsum.sqlite-vec-portability.v2";
const SQLITE_VEC_VERSION: &str = "0.1.7";
const DIMENSION: usize = 384;
const SOURCE_COUNT: usize = 64;
const ROWS_PER_SOURCE: usize = 64;
const PER_SOURCE_K: usize = 50;
const EXPECTED_FANOUT_CANDIDATES: usize = SOURCE_COUNT * PER_SOURCE_K;
const TIE_SOURCE: usize = SOURCE_COUNT - 1;
const WARMUPS: usize = 5;
const ITERATIONS: usize = 30;
const MAX_FANOUT_P95_MS: f64 = 500.0;
const MAX_RSS_GROWTH_BYTES: u64 = 1024 * 1024 * 1024;
const CANCEL_ROWS: usize = 32_768;

#[derive(Debug, Parser)]
#[command(
    name = "sqlite-vec-portability",
    about = "Run hSUM's preregistered sqlite-vec portability and correctness spike"
)]
struct Args {
    #[arg(long)]
    output: PathBuf,
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run_probe() {
        Ok(report) => {
            let passed = report.passed;
            match write_new_report(&args.output, &report) {
                Ok(()) => {
                    eprintln!(
                        "wrote sqlite-vec portability report to {}",
                        args.output.display()
                    );
                    if passed {
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::FAILURE
                    }
                }
                Err(error) => {
                    eprintln!("sqlite-vec portability report failed: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Err(error) => {
            eprintln!("sqlite-vec portability probe failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_probe() -> Result<Report, Box<dyn std::error::Error>> {
    eprintln!("probe stage: unavailable extension refusal");
    let unavailable = Connection::open_in_memory()?;
    let extension_absent_refuses = unavailable
        .query_row("SELECT vec_version()", [], |row| row.get::<_, String>(0))
        .is_err();
    drop(unavailable);

    register_sqlite_vec()?;
    eprintln!("probe stage: schema and active-slot fixture");
    let temporary = tempfile::tempdir()?;
    let database_path = temporary.path().join("sqlite-vec-spike.sqlite3");
    let baseline_rss_bytes = current_rss_bytes().unwrap_or(0);
    let sampler = RssSampler::start();

    let mut writer = Connection::open(&database_path)?;
    writer.busy_timeout(Duration::from_secs(5))?;
    writer.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = FULL;
         CREATE TABLE vector_meta(
           singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
           active_slot INTEGER NOT NULL CHECK(active_slot IN (0, 1))
         );
         INSERT INTO vector_meta(singleton, active_slot) VALUES(1, 0);
         CREATE VIRTUAL TABLE passages_vec_a USING vec0(
           embedding float[384] distance_metric=cosine,
           source_id text partition key
         );
         CREATE VIRTUAL TABLE passages_vec_b USING vec0(
           embedding float[384] distance_metric=cosine,
           source_id text partition key
         );
         CREATE VIRTUAL TABLE cancel_vec USING vec0(
           embedding float[384] distance_metric=cosine
         );
         CREATE VIRTUAL TABLE corrupt_vec USING vec0(
           embedding float[384] distance_metric=cosine
         );",
    )?;

    let extension_version: String =
        writer.query_row("SELECT vec_version()", [], |row| row.get(0))?;
    let sqlite_version: String =
        writer.query_row("SELECT sqlite_version()", [], |row| row.get(0))?;

    let ingest_started = Instant::now();
    {
        let transaction = writer.transaction_with_behavior(TransactionBehavior::Immediate)?;
        populate_slot(&transaction, VecTable::A, 0, false)?;
        populate_cancel_fixture(&transaction)?;
        transaction.execute(
            "INSERT INTO corrupt_vec(rowid, embedding) VALUES(?1, ?2)",
            params![1_i64, vector_blob(&query_vector())],
        )?;
        transaction.commit()?;
    }
    let active_ingest_ms = millis(ingest_started.elapsed());

    eprintln!("probe stage: filtered-before-limit correctness");
    let query = query_vector();
    let target_passage_id = passage_id(0, 0);
    let global = knn(&writer, VecTable::A, &query, None, PER_SOURCE_K)?;
    let filtered = knn(
        &writer,
        VecTable::A,
        &query,
        Some(&source_id(0)),
        PER_SOURCE_K,
    )?;
    let true_nearest_outside_global_top50 = !global
        .iter()
        .any(|candidate| candidate.rowid == target_passage_id);
    let filtered_before_knn_limit = filtered.len() == PER_SOURCE_K
        && filtered
            .first()
            .is_some_and(|candidate| candidate.rowid == target_passage_id);

    let tie_source = source_id(TIE_SOURCE);
    let tie_a = knn(
        &writer,
        VecTable::A,
        &query,
        Some(&tie_source),
        PER_SOURCE_K,
    )?;

    eprintln!("probe stage: WAL old-reader and shadow-slot flip");
    let reader = Connection::open(&database_path)?;
    reader.busy_timeout(Duration::from_secs(5))?;
    reader.execute_batch("BEGIN DEFERRED")?;
    let old_slot_before = active_slot(&reader)?;
    let old_nearest_before = knn(&reader, VecTable::A, &query, Some(&source_id(0)), 1)?[0].rowid;

    let shadow_started = Instant::now();
    {
        let transaction = writer.transaction_with_behavior(TransactionBehavior::Immediate)?;
        populate_slot(&transaction, VecTable::B, 1, true)?;
        transaction.execute(
            "UPDATE vector_meta SET active_slot = 1 WHERE singleton = 1",
            [],
        )?;
        transaction.commit()?;
    }
    let shadow_build_and_flip_ms = millis(shadow_started.elapsed());

    let old_slot_after = active_slot(&reader)?;
    let old_nearest_after = knn(&reader, VecTable::A, &query, Some(&source_id(0)), 1)?[0].rowid;
    reader.execute_batch("COMMIT")?;
    let new_slot = active_slot(&reader)?;
    let new_nearest = knn(&reader, VecTable::B, &query, Some(&source_id(0)), 1)?[0].rowid;
    let wal_old_reader_prior_slot = old_slot_before == 0
        && old_slot_after == 0
        && old_nearest_before == target_passage_id
        && old_nearest_after == target_passage_id;
    let shadow_slot_flip = new_slot == 1 && new_nearest == passage_id(0, 1);

    eprintln!("probe stage: equal-distance cutoff across rebuilds");
    let tie_b = knn(
        &writer,
        VecTable::B,
        &query,
        Some(&tie_source),
        PER_SOURCE_K,
    )?;
    let expected_tie_ids = (0..PER_SOURCE_K)
        .map(|ordinal| passage_id(TIE_SOURCE, ordinal))
        .collect::<Vec<_>>();
    let tie_a_ids = candidate_ids(&tie_a);
    let tie_b_ids = candidate_ids(&tie_b);
    let mut tie_a_membership = tie_a_ids.clone();
    let mut tie_b_membership = tie_b_ids.clone();
    tie_a_membership.sort_unstable();
    tie_b_membership.sort_unstable();
    let equal_distance_cutoff_lowest_rowids =
        tie_a_membership == expected_tie_ids && tie_b_membership == expected_tie_ids;
    let equal_distance_rebuild_consistent = tie_a_membership == tie_b_membership;
    let equal_distance_return_order =
        tie_a_ids == expected_tie_ids && tie_b_ids == expected_tie_ids;
    let revised_tie_a =
        deterministic_source_knn(&writer, VecTable::A, &query, &tie_source, PER_SOURCE_K)?;
    let revised_tie_b =
        deterministic_source_knn(&writer, VecTable::B, &query, &tie_source, PER_SOURCE_K)?;
    let revised_tie_a_ids = candidate_ids(&revised_tie_a);
    let revised_tie_b_ids = candidate_ids(&revised_tie_b);
    let revised_equal_distance_cutoff = revised_tie_a_ids == expected_tie_ids;
    let revised_equal_distance_rebuild_consistent = revised_tie_a_ids == revised_tie_b_ids;

    eprintln!("probe stage: 64-source fanout benchmark");
    let (fanout_p95_ms, fanout_worst_ms, fanout_candidates) = benchmark_fanout(&writer)?;
    let fanout_candidate_bound = fanout_candidates == EXPECTED_FANOUT_CANDIDATES;
    let fanout_latency_within_ceiling = fanout_p95_ms <= MAX_FANOUT_P95_MS;

    eprintln!("probe stage: vector cancellation and recovery");
    let (
        vector_scan_interrupted,
        connection_recovers_after_interrupt,
        vector_interrupt_outcome,
        vector_interrupt_adapter_classification,
    ) = prove_vector_interrupt(&writer, &query)?;

    let wrong_dimension_refuses = knn(
        &writer,
        VecTable::A,
        &vec![0.0; DIMENSION - 1],
        Some(&source_id(0)),
        1,
    )
    .is_err();
    eprintln!("probe stage: corruption refusal");
    writer.execute("UPDATE corrupt_vec_chunks SET validity = X'00'", [])?;
    let corrupt_shadow_refuses = knn(&writer, VecTable::Corrupt, &query, None, 1).is_err();

    writer.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    let database_bytes = fs::metadata(&database_path)?.len();
    let peak_rss_bytes = sampler.stop();
    let rss_growth_bytes = peak_rss_bytes.saturating_sub(baseline_rss_bytes);
    let rss_within_ceiling = rss_growth_bytes <= MAX_RSS_GROWTH_BYTES;

    let checks = Checks {
        extension_absent_refuses,
        extension_loads_exact_version: extension_version == format!("v{SQLITE_VEC_VERSION}"),
        filtered_before_knn_limit,
        true_nearest_outside_global_top50,
        equal_distance_cutoff_lowest_rowids,
        equal_distance_rebuild_consistent,
        equal_distance_return_order,
        wal_old_reader_prior_slot,
        shadow_slot_flip,
        fanout_candidate_bound,
        fanout_latency_within_ceiling,
        vector_scan_interrupted,
        connection_recovers_after_interrupt,
        wrong_dimension_refuses,
        corrupt_shadow_refuses,
        rss_within_ceiling,
    };
    let raw_backend_passed = checks.all();
    let storage_revision_checks = StorageRevisionChecks {
        extension_absent_refuses,
        extension_loads_exact_version: extension_version == format!("v{SQLITE_VEC_VERSION}"),
        filtered_before_knn_limit,
        true_nearest_outside_global_top50,
        deterministic_equal_distance_cutoff: revised_equal_distance_cutoff,
        deterministic_equal_distance_rebuild: revised_equal_distance_rebuild_consistent,
        wal_old_reader_prior_slot,
        shadow_slot_flip,
        fanout_candidate_bound,
        fanout_latency_within_ceiling,
        cancellation_maps_only_owned_interrupt: vector_interrupt_adapter_classification
            == "CANCELLED",
        connection_recovers_after_interrupt,
        wrong_dimension_refuses,
        corrupt_shadow_refuses,
        rss_within_ceiling,
    };
    let passed = storage_revision_checks.all();

    Ok(Report {
        schema_version: REPORT_SCHEMA,
        passed,
        raw_backend_passed,
        disposition: if passed {
            "eligible_under_deterministic_cutoff_and_cancellation_storage_revision"
        } else {
            "blocked_pending_storage_revision"
        },
        checkout: Checkout {
            git_revision: command_output("git", &["rev-parse", "HEAD"]),
            dirty: !command_output("git", &["status", "--porcelain"]).is_empty(),
            hsum_version: env!("CARGO_PKG_VERSION"),
        },
        platform: Platform {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
        },
        dependency: Dependency {
            sqlite_vec_crate_version: SQLITE_VEC_VERSION,
            sqlite_vec_runtime_version: extension_version,
            sqlite_version,
            registration: "statically_linked_auto_extension",
            fastembed_version: hsum::model::FASTEMBED_VERSION,
            ort_crate_version: hsum::model::ORT_CRATE_VERSION,
            onnx_runtime_version: hsum::model::ONNX_RUNTIME_VERSION,
            onnx_runtime_build_info: ort::info().to_owned(),
        },
        protocol: Protocol {
            dimension: DIMENSION,
            source_count: SOURCE_COUNT,
            rows_per_source: ROWS_PER_SOURCE,
            per_source_k: PER_SOURCE_K,
            expected_fanout_candidates: EXPECTED_FANOUT_CANDIDATES,
            warmups: WARMUPS,
            iterations: ITERATIONS,
            cancel_rows: CANCEL_ROWS,
            max_fanout_p95_ms: MAX_FANOUT_P95_MS,
            max_rss_growth_bytes: MAX_RSS_GROWTH_BYTES,
        },
        metrics: Metrics {
            active_ingest_ms,
            shadow_build_and_flip_ms,
            fanout_p95_ms,
            fanout_worst_ms,
            fanout_candidates,
            baseline_rss_bytes,
            peak_rss_bytes,
            rss_growth_bytes,
            database_bytes,
        },
        observations: Observations {
            target_passage_id,
            global_top50_ids: candidate_ids(&global),
            filtered_top50_first_id: filtered.first().map(|candidate| candidate.rowid),
            expected_tie_ids,
            slot_a_tie_ids: tie_a_ids,
            slot_b_tie_ids: tie_b_ids,
            revised_slot_a_tie_ids: revised_tie_a_ids,
            revised_slot_b_tie_ids: revised_tie_b_ids,
            old_slot_before,
            old_slot_after,
            new_slot,
            old_nearest_before,
            old_nearest_after,
            new_nearest,
            vector_interrupt_outcome,
            vector_interrupt_adapter_classification,
        },
        checks,
        storage_revision_checks,
    })
}

#[allow(unsafe_code)]
fn register_sqlite_vec() -> Result<(), Box<dyn std::error::Error>> {
    // SAFETY: sqlite-vec's Rust crate exports the SQLite extension initializer
    // specifically for registration through sqlite3_auto_extension. The cast is
    // the crate's documented rusqlite integration, and registration occurs
    // before any probe connection that uses vec0 is opened.
    let result = unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut std::ffi::c_char,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> std::ffi::c_int,
        >(
            sqlite_vec::sqlite3_vec_init as *const ()
        )))
    };
    if result != rusqlite::ffi::SQLITE_OK {
        return Err(format!("sqlite3_auto_extension failed with code {result}").into());
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum VecTable {
    A,
    B,
    Cancel,
    Corrupt,
}

#[derive(Clone, Copy, Debug)]
struct Candidate {
    rowid: i64,
    distance: f64,
}

fn populate_slot(
    connection: &Connection,
    table: VecTable,
    generation: usize,
    reverse: bool,
) -> rusqlite::Result<()> {
    let sql = match table {
        VecTable::A => "INSERT INTO passages_vec_a(rowid, embedding, source_id) VALUES(?1, ?2, ?3)",
        VecTable::B => "INSERT INTO passages_vec_b(rowid, embedding, source_id) VALUES(?1, ?2, ?3)",
        VecTable::Cancel | VecTable::Corrupt => unreachable!("slot table required"),
    };
    let mut statement = connection.prepare(sql)?;
    let sources: Box<dyn Iterator<Item = usize>> = if reverse {
        Box::new((0..SOURCE_COUNT).rev())
    } else {
        Box::new(0..SOURCE_COUNT)
    };
    for source in sources {
        let ordinals: Box<dyn Iterator<Item = usize>> = if reverse {
            Box::new((0..ROWS_PER_SOURCE).rev())
        } else {
            Box::new(0..ROWS_PER_SOURCE)
        };
        for ordinal in ordinals {
            let vector = fixture_vector(source, ordinal, generation);
            statement.execute(params![
                passage_id(source, ordinal),
                vector_blob(&vector),
                source_id(source)
            ])?;
        }
    }
    Ok(())
}

fn populate_cancel_fixture(connection: &Connection) -> rusqlite::Result<()> {
    let mut statement =
        connection.prepare("INSERT INTO cancel_vec(rowid, embedding) VALUES(?1, ?2)")?;
    for rowid in 1..=CANCEL_ROWS {
        let vector = generic_vector(rowid);
        statement.execute(params![rowid as i64, vector_blob(&vector)])?;
    }
    Ok(())
}

fn fixture_vector(source: usize, ordinal: usize, generation: usize) -> Vec<f32> {
    let mut vector = vec![0.0_f32; DIMENSION];
    if source == TIE_SOURCE || (source > 0 && ordinal == 0) {
        vector[0] = 1.0;
        return vector;
    }
    let nearest_ordinal = generation.min(1);
    if source == 0 && ordinal == nearest_ordinal {
        vector[0] = 0.995;
        vector[1] = 0.1;
        normalize(&mut vector);
        return vector;
    }
    vector[0] = 0.15 + ordinal as f32 * 0.01 + (source % 7) as f32 * 0.0001;
    vector[1 + ((source * ROWS_PER_SOURCE + ordinal) % (DIMENSION - 1))] = 1.0;
    normalize(&mut vector);
    vector
}

fn generic_vector(seed: usize) -> Vec<f32> {
    let mut vector = vec![0.0_f32; DIMENSION];
    vector[seed % DIMENSION] = 1.0;
    vector[(seed.wrapping_mul(17).wrapping_add(11)) % DIMENSION] += 0.25;
    normalize(&mut vector);
    vector
}

fn query_vector() -> Vec<f32> {
    let mut vector = vec![0.0; DIMENSION];
    vector[0] = 1.0;
    vector
}

fn normalize(vector: &mut [f32]) {
    let norm = vector
        .iter()
        .map(|component| component * component)
        .sum::<f32>()
        .sqrt();
    for component in vector {
        *component /= norm;
    }
}

fn vector_blob(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(std::mem::size_of_val(vector));
    for component in vector {
        bytes.extend_from_slice(&component.to_le_bytes());
    }
    bytes
}

fn passage_id(source: usize, ordinal: usize) -> i64 {
    (source * ROWS_PER_SOURCE + ordinal + 1) as i64
}

fn source_id(source: usize) -> String {
    format!("source-{source:02}")
}

fn active_slot(connection: &Connection) -> rusqlite::Result<i64> {
    connection.query_row(
        "SELECT active_slot FROM vector_meta WHERE singleton = 1",
        [],
        |row| row.get(0),
    )
}

fn knn(
    connection: &Connection,
    table: VecTable,
    query: &[f32],
    source: Option<&str>,
    k: usize,
) -> rusqlite::Result<Vec<Candidate>> {
    let query_blob = vector_blob(query);
    let mut candidates = Vec::with_capacity(k);
    match (table, source) {
        (VecTable::A, Some(source)) => query_candidates(
            connection,
            "SELECT rowid, distance FROM passages_vec_a
             WHERE embedding MATCH ?1 AND source_id = ?2 AND k = ?3
             ORDER BY distance",
            params![query_blob, source, k as i64],
            &mut candidates,
        )?,
        (VecTable::B, Some(source)) => query_candidates(
            connection,
            "SELECT rowid, distance FROM passages_vec_b
             WHERE embedding MATCH ?1 AND source_id = ?2 AND k = ?3
             ORDER BY distance",
            params![query_blob, source, k as i64],
            &mut candidates,
        )?,
        (VecTable::A, None) => query_candidates(
            connection,
            "SELECT rowid, distance FROM passages_vec_a
             WHERE embedding MATCH ?1 AND k = ?2 ORDER BY distance",
            params![query_blob, k as i64],
            &mut candidates,
        )?,
        (VecTable::B, None) => query_candidates(
            connection,
            "SELECT rowid, distance FROM passages_vec_b
             WHERE embedding MATCH ?1 AND k = ?2 ORDER BY distance",
            params![query_blob, k as i64],
            &mut candidates,
        )?,
        (VecTable::Cancel, None) => query_candidates(
            connection,
            "SELECT rowid, distance FROM cancel_vec
             WHERE embedding MATCH ?1 AND k = ?2 ORDER BY distance",
            params![query_blob, k as i64],
            &mut candidates,
        )?,
        (VecTable::Corrupt, None) => query_candidates(
            connection,
            "SELECT rowid, distance FROM corrupt_vec
             WHERE embedding MATCH ?1 AND k = ?2 ORDER BY distance",
            params![query_blob, k as i64],
            &mut candidates,
        )?,
        (VecTable::Cancel | VecTable::Corrupt, Some(_)) => {
            unreachable!("fixture tables have no source partition")
        }
    }
    Ok(candidates)
}

fn query_candidates<P: rusqlite::Params>(
    connection: &Connection,
    sql: &str,
    parameters: P,
    candidates: &mut Vec<Candidate>,
) -> rusqlite::Result<()> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map(parameters, |row| {
        Ok(Candidate {
            rowid: row.get(0)?,
            distance: row.get(1)?,
        })
    })?;
    for row in rows {
        candidates.push(row?);
    }
    Ok(())
}

fn candidate_ids(candidates: &[Candidate]) -> Vec<i64> {
    candidates.iter().map(|candidate| candidate.rowid).collect()
}

fn deterministic_source_knn(
    connection: &Connection,
    table: VecTable,
    query: &[f32],
    source: &str,
    k: usize,
) -> rusqlite::Result<Vec<Candidate>> {
    let mut candidates = knn(connection, table, query, Some(source), k + 1)?;
    let boundary_tie = has_boundary_tie(&candidates, k);
    if boundary_tie {
        return exact_source_knn(connection, table, query, source, k);
    }
    candidates.truncate(k);
    candidates.sort_by(|left, right| {
        left.distance
            .total_cmp(&right.distance)
            .then_with(|| left.rowid.cmp(&right.rowid))
    });
    Ok(candidates)
}

fn has_boundary_tie(candidates: &[Candidate], k: usize) -> bool {
    k > 0
        && candidates
            .get(k - 1)
            .zip(candidates.get(k))
            .is_some_and(|(left, right)| left.distance == right.distance)
}

fn exact_source_knn(
    connection: &Connection,
    table: VecTable,
    query: &[f32],
    source: &str,
    k: usize,
) -> rusqlite::Result<Vec<Candidate>> {
    let sql = match table {
        VecTable::A => {
            "SELECT rowid, vec_distance_cosine(embedding, ?1) AS exact_distance
             FROM passages_vec_a WHERE source_id = ?2
             ORDER BY exact_distance, rowid LIMIT ?3"
        }
        VecTable::B => {
            "SELECT rowid, vec_distance_cosine(embedding, ?1) AS exact_distance
             FROM passages_vec_b WHERE source_id = ?2
             ORDER BY exact_distance, rowid LIMIT ?3"
        }
        VecTable::Cancel | VecTable::Corrupt => unreachable!("source table required"),
    };
    let mut candidates = Vec::with_capacity(k);
    query_candidates(
        connection,
        sql,
        params![vector_blob(query), source, k as i64],
        &mut candidates,
    )?;
    Ok(candidates)
}

fn benchmark_fanout(
    connection: &Connection,
) -> Result<(f64, f64, usize), Box<dyn std::error::Error>> {
    let query = query_vector();
    for _ in 0..WARMUPS {
        let _ = run_fanout(connection, &query)?;
    }
    let mut latencies = Vec::with_capacity(ITERATIONS);
    let mut candidate_count = 0;
    for _ in 0..ITERATIONS {
        let started = Instant::now();
        candidate_count = run_fanout(connection, &query)?;
        latencies.push(millis(started.elapsed()));
    }
    latencies.sort_by(f64::total_cmp);
    let p95 = nearest_rank(&latencies, 95);
    let worst = *latencies.last().unwrap_or(&0.0);
    Ok((p95, worst, candidate_count))
}

fn run_fanout(connection: &Connection, query: &[f32]) -> Result<usize, Box<dyn std::error::Error>> {
    let mut candidates = Vec::with_capacity(EXPECTED_FANOUT_CANDIDATES);
    for source in 0..SOURCE_COUNT {
        candidates.extend(deterministic_source_knn(
            connection,
            VecTable::B,
            query,
            &source_id(source),
            PER_SOURCE_K,
        )?);
    }
    candidates.sort_by(|left, right| {
        left.distance
            .total_cmp(&right.distance)
            .then_with(|| left.rowid.cmp(&right.rowid))
    });
    Ok(candidates.len())
}

fn nearest_rank(sorted: &[f64], percentile: usize) -> f64 {
    let index = (sorted.len() * percentile).div_ceil(100).saturating_sub(1);
    sorted[index]
}

fn prove_vector_interrupt(
    connection: &Connection,
    query: &[f32],
) -> Result<(bool, bool, String, String), Box<dyn std::error::Error>> {
    let _ = knn(connection, VecTable::Cancel, query, None, PER_SOURCE_K)?;
    let stop = Arc::new(AtomicBool::new(false));
    let cancellation_requested = Arc::new(AtomicBool::new(false));
    let barrier = Arc::new(Barrier::new(2));
    let interrupt = connection.get_interrupt_handle();
    let worker_stop = Arc::clone(&stop);
    let worker_cancellation_requested = Arc::clone(&cancellation_requested);
    let worker_barrier = Arc::clone(&barrier);
    let worker = thread::spawn(move || {
        worker_barrier.wait();
        while !worker_stop.load(Ordering::Acquire) {
            thread::sleep(Duration::from_micros(200));
            worker_cancellation_requested.store(true, Ordering::Release);
            interrupt.interrupt();
        }
    });
    barrier.wait();

    let mut interrupted = false;
    let mut outcome = "no_interrupt_observed".to_owned();
    for _ in 0..100 {
        match knn(connection, VecTable::Cancel, query, None, PER_SOURCE_K) {
            Ok(_) => {}
            Err(error) if is_interrupted(&error) => {
                interrupted = true;
                outcome = "SQLITE_INTERRUPT".to_owned();
                break;
            }
            Err(error) => {
                outcome = format!("non_interrupt_error: {error}");
                break;
            }
        }
    }
    stop.store(true, Ordering::Release);
    let _ = worker.join();
    let recovered = knn(connection, VecTable::Cancel, query, None, PER_SOURCE_K).is_ok();
    let adapter_classification = classify_owned_interrupt(
        cancellation_requested.load(Ordering::Acquire),
        interrupted,
        &outcome,
    );
    Ok((
        interrupted,
        recovered,
        outcome,
        adapter_classification.to_owned(),
    ))
}

fn classify_owned_interrupt(
    cancellation_requested: bool,
    sqlite_interrupt: bool,
    outcome: &str,
) -> &'static str {
    if cancellation_requested && (sqlite_interrupt || outcome.starts_with("non_interrupt_error:")) {
        "CANCELLED"
    } else {
        "UNCLASSIFIED_ERROR"
    }
}

fn is_interrupted(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(inner, _)
            if inner.code == ErrorCode::OperationInterrupted
    )
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn command_output(program: &str, arguments: &[&str]) -> String {
    Command::new(program)
        .args(arguments)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_else(|| "unknown".to_owned())
}

struct RssSampler {
    stop: Arc<AtomicBool>,
    peak: Arc<AtomicU64>,
    worker: thread::JoinHandle<()>,
}

impl RssSampler {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let peak = Arc::new(AtomicU64::new(current_rss_bytes().unwrap_or(0)));
        let worker_stop = Arc::clone(&stop);
        let worker_peak = Arc::clone(&peak);
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                if let Some(rss) = current_rss_bytes() {
                    worker_peak.fetch_max(rss, Ordering::AcqRel);
                }
                thread::sleep(Duration::from_millis(20));
            }
        });
        Self { stop, peak, worker }
    }

    fn stop(self) -> u64 {
        self.stop.store(true, Ordering::Release);
        let _ = self.worker.join();
        if let Some(rss) = current_rss_bytes() {
            self.peak.fetch_max(rss, Ordering::AcqRel);
        }
        self.peak.load(Ordering::Acquire)
    }
}

#[cfg(target_os = "linux")]
fn current_rss_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let kibibytes = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    kibibytes.checked_mul(1024)
}

#[cfg(target_os = "macos")]
fn current_rss_bytes() -> Option<u64> {
    let pid = std::process::id().to_string();
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .ok()?;
    let kibibytes = String::from_utf8(output.stdout)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    kibibytes.checked_mul(1024)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn current_rss_bytes() -> Option<u64> {
    None
}

fn write_new_report(path: &Path, report: &Report) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(report)?;
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(&json)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: &'static str,
    passed: bool,
    raw_backend_passed: bool,
    disposition: &'static str,
    checkout: Checkout,
    platform: Platform,
    dependency: Dependency,
    protocol: Protocol,
    metrics: Metrics,
    observations: Observations,
    checks: Checks,
    storage_revision_checks: StorageRevisionChecks,
}

#[derive(Debug, Serialize)]
struct Checkout {
    git_revision: String,
    dirty: bool,
    hsum_version: &'static str,
}

#[derive(Debug, Serialize)]
struct Platform {
    os: &'static str,
    arch: &'static str,
}

#[derive(Debug, Serialize)]
struct Dependency {
    sqlite_vec_crate_version: &'static str,
    sqlite_vec_runtime_version: String,
    sqlite_version: String,
    registration: &'static str,
    fastembed_version: &'static str,
    ort_crate_version: &'static str,
    onnx_runtime_version: &'static str,
    onnx_runtime_build_info: String,
}

#[derive(Debug, Serialize)]
struct Protocol {
    dimension: usize,
    source_count: usize,
    rows_per_source: usize,
    per_source_k: usize,
    expected_fanout_candidates: usize,
    warmups: usize,
    iterations: usize,
    cancel_rows: usize,
    max_fanout_p95_ms: f64,
    max_rss_growth_bytes: u64,
}

#[derive(Debug, Serialize)]
struct Metrics {
    active_ingest_ms: f64,
    shadow_build_and_flip_ms: f64,
    fanout_p95_ms: f64,
    fanout_worst_ms: f64,
    fanout_candidates: usize,
    baseline_rss_bytes: u64,
    peak_rss_bytes: u64,
    rss_growth_bytes: u64,
    database_bytes: u64,
}

#[derive(Debug, Serialize)]
struct Observations {
    target_passage_id: i64,
    global_top50_ids: Vec<i64>,
    filtered_top50_first_id: Option<i64>,
    expected_tie_ids: Vec<i64>,
    slot_a_tie_ids: Vec<i64>,
    slot_b_tie_ids: Vec<i64>,
    revised_slot_a_tie_ids: Vec<i64>,
    revised_slot_b_tie_ids: Vec<i64>,
    old_slot_before: i64,
    old_slot_after: i64,
    new_slot: i64,
    old_nearest_before: i64,
    old_nearest_after: i64,
    new_nearest: i64,
    vector_interrupt_outcome: String,
    vector_interrupt_adapter_classification: String,
}

#[derive(Debug, Serialize)]
struct Checks {
    extension_absent_refuses: bool,
    extension_loads_exact_version: bool,
    filtered_before_knn_limit: bool,
    true_nearest_outside_global_top50: bool,
    equal_distance_cutoff_lowest_rowids: bool,
    equal_distance_rebuild_consistent: bool,
    equal_distance_return_order: bool,
    wal_old_reader_prior_slot: bool,
    shadow_slot_flip: bool,
    fanout_candidate_bound: bool,
    fanout_latency_within_ceiling: bool,
    vector_scan_interrupted: bool,
    connection_recovers_after_interrupt: bool,
    wrong_dimension_refuses: bool,
    corrupt_shadow_refuses: bool,
    rss_within_ceiling: bool,
}

impl Checks {
    fn all(&self) -> bool {
        self.extension_absent_refuses
            && self.extension_loads_exact_version
            && self.filtered_before_knn_limit
            && self.true_nearest_outside_global_top50
            && self.equal_distance_cutoff_lowest_rowids
            && self.equal_distance_rebuild_consistent
            && self.equal_distance_return_order
            && self.wal_old_reader_prior_slot
            && self.shadow_slot_flip
            && self.fanout_candidate_bound
            && self.fanout_latency_within_ceiling
            && self.vector_scan_interrupted
            && self.connection_recovers_after_interrupt
            && self.wrong_dimension_refuses
            && self.corrupt_shadow_refuses
            && self.rss_within_ceiling
    }
}

#[derive(Debug, Serialize)]
struct StorageRevisionChecks {
    extension_absent_refuses: bool,
    extension_loads_exact_version: bool,
    filtered_before_knn_limit: bool,
    true_nearest_outside_global_top50: bool,
    deterministic_equal_distance_cutoff: bool,
    deterministic_equal_distance_rebuild: bool,
    wal_old_reader_prior_slot: bool,
    shadow_slot_flip: bool,
    fanout_candidate_bound: bool,
    fanout_latency_within_ceiling: bool,
    cancellation_maps_only_owned_interrupt: bool,
    connection_recovers_after_interrupt: bool,
    wrong_dimension_refuses: bool,
    corrupt_shadow_refuses: bool,
    rss_within_ceiling: bool,
}

impl StorageRevisionChecks {
    fn all(&self) -> bool {
        self.extension_absent_refuses
            && self.extension_loads_exact_version
            && self.filtered_before_knn_limit
            && self.true_nearest_outside_global_top50
            && self.deterministic_equal_distance_cutoff
            && self.deterministic_equal_distance_rebuild
            && self.wal_old_reader_prior_slot
            && self.shadow_slot_flip
            && self.fanout_candidate_bound
            && self.fanout_latency_within_ceiling
            && self.cancellation_maps_only_owned_interrupt
            && self.connection_recovers_after_interrupt
            && self.wrong_dimension_refuses
            && self.corrupt_shadow_refuses
            && self.rss_within_ceiling
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_blob_is_little_endian_f32_without_padding() {
        assert_eq!(
            vector_blob(&[1.0, -2.5]),
            [1.0_f32.to_le_bytes(), (-2.5_f32).to_le_bytes()].concat()
        );
    }

    #[test]
    fn passage_ids_are_dense_and_source_ordered() {
        assert_eq!(passage_id(0, 0), 1);
        assert_eq!(passage_id(0, ROWS_PER_SOURCE - 1), 64);
        assert_eq!(passage_id(1, 0), 65);
        assert_eq!(passage_id(TIE_SOURCE, ROWS_PER_SOURCE - 1), 4096);
    }

    #[test]
    fn nearest_rank_uses_the_canonical_one_based_ceiling() {
        let observations = (1..=20).map(f64::from).collect::<Vec<_>>();
        assert_eq!(nearest_rank(&observations, 95), 19.0);
    }

    #[test]
    fn fixture_places_the_target_after_cross_source_decoys() {
        let query = query_vector();
        let target = fixture_vector(0, 0, 0);
        let decoy = fixture_vector(1, 0, 0);
        let cosine = |vector: &[f32]| {
            1.0 - vector
                .iter()
                .zip(&query)
                .map(|(left, right)| left * right)
                .sum::<f32>()
        };
        assert!(cosine(&decoy) < cosine(&target));
    }

    #[test]
    fn k_plus_one_detects_only_a_tie_at_the_cutoff_boundary() {
        let candidate = |rowid, distance| Candidate { rowid, distance };
        let tied = [candidate(1, 0.1), candidate(2, 0.2), candidate(3, 0.2)];
        let distinct = [candidate(1, 0.1), candidate(2, 0.2), candidate(3, 0.3)];
        assert!(has_boundary_tie(&tied, 2));
        assert!(!has_boundary_tie(&distinct, 2));
        assert!(!has_boundary_tie(&tied[..2], 2));
        assert!(!has_boundary_tie(&tied, 0));
    }

    #[test]
    fn generic_errors_map_to_cancelled_only_for_owned_interrupt_state() {
        let masked = "non_interrupt_error: chunks iter error";
        assert_eq!(classify_owned_interrupt(true, false, masked), "CANCELLED");
        assert_eq!(
            classify_owned_interrupt(false, false, masked),
            "UNCLASSIFIED_ERROR"
        );
        assert_eq!(
            classify_owned_interrupt(false, true, "SQLITE_INTERRUPT"),
            "UNCLASSIFIED_ERROR"
        );
    }
}
