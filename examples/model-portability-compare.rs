#![deny(unsafe_code)]

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const REPORT_SCHEMA: &str = "hsum.model-portability.v2";
const COMPARISON_SCHEMA: &str = "hsum.model-numerical-compatibility.v1";
const MAX_COMPONENT_ABS_DELTA: f64 = 1e-5;
const MAX_VECTOR_L2_DELTA: f64 = 2e-4;
const MAX_COSINE_DISTANCE: f64 = 1e-6;
const MAX_PAIRWISE_DISTANCE_DELTA: f64 = 2e-6;

#[derive(Debug, Parser)]
#[command(
    name = "model-portability-compare",
    about = "Compare two hSUM native embedding portability reports numerically"
)]
struct Args {
    left: PathBuf,
    right: PathBuf,
    #[arg(long)]
    output: Option<PathBuf>,
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("model portability comparison failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> Result<bool, Box<dyn std::error::Error>> {
    let left: Report = serde_json::from_slice(&fs::read(&args.left)?)?;
    let right: Report = serde_json::from_slice(&fs::read(&args.right)?)?;
    let comparison = compare(&left, &right)?;
    let passed = comparison.passed;
    let json = serde_json::to_vec_pretty(&comparison)?;
    if let Some(output) = args.output {
        write_new_report(&output, &json)?;
        eprintln!(
            "wrote model numerical compatibility report to {}",
            output.display()
        );
    } else {
        std::io::stdout().write_all(&json)?;
        std::io::stdout().write_all(b"\n")?;
    }
    Ok(passed)
}

fn compare(left: &Report, right: &Report) -> Result<Comparison, ComparisonError> {
    validate_report(left)?;
    validate_report(right)?;
    if left.platform == right.platform {
        return Err(ComparisonError::SamePlatform(left.platform.clone()));
    }

    let left_provenance = comparable_provenance(&left.provenance)?;
    let right_provenance = comparable_provenance(&right.provenance)?;
    let provenance_compatible = left_provenance == right_provenance;
    let protocol_compatible = left.checkout == right.checkout
        && left.model == right.model
        && left.configuration == right.configuration
        && left.qualification == right.qualification
        && left.thresholds == right.thresholds;
    let supported_platform_pair = is_supported_platform_pair(&left.platform, &right.platform);
    if left.workloads.len() != right.workloads.len() {
        return Err(ComparisonError::WorkloadCount {
            left: left.workloads.len(),
            right: right.workloads.len(),
        });
    }

    let mut workloads = Vec::with_capacity(left.workloads.len());
    let mut max_component_abs_delta = 0.0_f64;
    let mut max_vector_l2_delta = 0.0_f64;
    let mut max_cosine_distance = 0.0_f64;
    let mut all_components = 0_u64;
    let mut squared_component_delta_sum = 0.0_f64;

    for (left_workload, right_workload) in left.workloads.iter().zip(&right.workloads) {
        let metrics = compare_workload(left_workload, right_workload)?;
        max_component_abs_delta = max_component_abs_delta.max(metrics.max_component_abs_delta);
        max_vector_l2_delta = max_vector_l2_delta.max(metrics.max_vector_l2_delta);
        max_cosine_distance = max_cosine_distance.max(metrics.max_cosine_distance);
        all_components = all_components
            .checked_add(metrics.component_count)
            .ok_or(ComparisonError::Overflow)?;
        squared_component_delta_sum += metrics.squared_component_delta_sum;
        workloads.push(metrics.into_report());
    }

    let left_batch = batch_workload(left)?;
    let right_batch = batch_workload(right)?;
    let ordering = compare_ordering(left_batch, right_batch)?;
    let component_rms_delta = (squared_component_delta_sum / all_components as f64).sqrt();
    let thresholds = Thresholds {
        max_component_abs_delta: MAX_COMPONENT_ABS_DELTA,
        max_vector_l2_delta: MAX_VECTOR_L2_DELTA,
        max_cosine_distance: MAX_COSINE_DISTANCE,
        max_pairwise_distance_delta: MAX_PAIRWISE_DISTANCE_DELTA,
        require_identical_ordering: true,
    };
    let checks = Checks {
        source_reports_passed: left.passed && right.passed,
        source_protocol_compatible: protocol_compatible,
        supported_platform_pair,
        provenance_compatible,
        component_delta_within_tolerance: max_component_abs_delta <= MAX_COMPONENT_ABS_DELTA,
        vector_l2_delta_within_tolerance: max_vector_l2_delta <= MAX_VECTOR_L2_DELTA,
        cosine_distance_within_tolerance: max_cosine_distance <= MAX_COSINE_DISTANCE,
        pairwise_distance_delta_within_tolerance: ordering.max_pairwise_distance_delta
            <= MAX_PAIRWISE_DISTANCE_DELTA,
        deterministic_ordering_identical: ordering.identical,
    };
    let passed = checks.all();

    Ok(Comparison {
        schema_version: COMPARISON_SCHEMA,
        passed,
        left: left.platform.clone(),
        right: right.platform.clone(),
        thresholds,
        metrics: Metrics {
            component_count: all_components,
            max_component_abs_delta,
            component_rms_delta,
            max_vector_l2_delta,
            max_cosine_distance,
            max_pairwise_distance_delta: ordering.max_pairwise_distance_delta,
            minimum_adjacent_distance_gap: ordering.minimum_adjacent_distance_gap,
            ordering_mismatches: ordering.mismatches,
        },
        workloads,
        checks,
    })
}

fn validate_report(report: &Report) -> Result<(), ComparisonError> {
    if report.schema_version != REPORT_SCHEMA {
        return Err(ComparisonError::Schema(report.schema_version.clone()));
    }
    if report.workloads.len() != 2
        || report
            .workloads
            .iter()
            .filter(|workload| workload.name == "interactive_query")
            .count()
            != 1
        || report
            .workloads
            .iter()
            .filter(|workload| workload.name == "ingest_batch_8")
            .count()
            != 1
        || report
            .workloads
            .iter()
            .find(|workload| workload.name == "interactive_query")
            .is_none_or(|workload| workload.documents_per_call != 1)
        || report
            .workloads
            .iter()
            .find(|workload| workload.name == "ingest_batch_8")
            .is_none_or(|workload| workload.documents_per_call != 8)
    {
        return Err(ComparisonError::WorkloadProtocol);
    }
    Ok(())
}

fn is_supported_platform_pair(left: &Platform, right: &Platform) -> bool {
    matches!(
        (
            left.os.as_str(),
            left.arch.as_str(),
            right.os.as_str(),
            right.arch.as_str()
        ),
        ("linux", "x86_64", "macos", "aarch64") | ("macos", "aarch64", "linux", "x86_64")
    )
}

fn comparable_provenance(provenance: &Value) -> Result<Value, ComparisonError> {
    let mut provenance = provenance
        .as_object()
        .cloned()
        .ok_or(ComparisonError::ProvenanceShape)?;
    provenance.remove("target_os");
    provenance.remove("target_arch");
    Ok(Value::Object(provenance))
}

fn compare_workload(left: &Workload, right: &Workload) -> Result<WorkloadMetrics, ComparisonError> {
    if left.name != right.name {
        return Err(ComparisonError::WorkloadName {
            left: left.name.clone(),
            right: right.name.clone(),
        });
    }
    if left.reference_embeddings.is_empty()
        || right.reference_embeddings.is_empty()
        || left.documents_per_call != right.documents_per_call
        || left.reference_embeddings.len() != right.reference_embeddings.len()
        || left.reference_embeddings.len() != left.documents_per_call
    {
        return Err(ComparisonError::VectorCount(left.name.clone()));
    }

    let mut max_component_abs_delta = 0.0_f64;
    let mut max_vector_l2_delta = 0.0_f64;
    let mut max_cosine_distance = 0.0_f64;
    let mut component_count = 0_u64;
    let mut squared_component_delta_sum = 0.0_f64;
    for (left_vector, right_vector) in left
        .reference_embeddings
        .iter()
        .zip(&right.reference_embeddings)
    {
        if left_vector.is_empty() || left_vector.len() != right_vector.len() {
            return Err(ComparisonError::Dimension(left.name.clone()));
        }
        let mut vector_squared_delta = 0.0_f64;
        for (&left_component, &right_component) in left_vector.iter().zip(right_vector) {
            if !left_component.is_finite() || !right_component.is_finite() {
                return Err(ComparisonError::NonFinite(left.name.clone()));
            }
            let delta = (left_component - right_component).abs();
            max_component_abs_delta = max_component_abs_delta.max(delta);
            vector_squared_delta += delta * delta;
            squared_component_delta_sum += delta * delta;
            component_count = component_count
                .checked_add(1)
                .ok_or(ComparisonError::Overflow)?;
        }
        max_vector_l2_delta = max_vector_l2_delta.max(vector_squared_delta.sqrt());
        max_cosine_distance = max_cosine_distance.max(cosine_distance(left_vector, right_vector)?);
    }

    Ok(WorkloadMetrics {
        name: left.name.clone(),
        vectors: left.reference_embeddings.len(),
        dimension: left.reference_embeddings[0].len(),
        component_count,
        max_component_abs_delta,
        max_vector_l2_delta,
        max_cosine_distance,
        squared_component_delta_sum,
    })
}

fn cosine_distance(left: &[f64], right: &[f64]) -> Result<f64, ComparisonError> {
    let dot = left.iter().zip(right).map(|(a, b)| a * b).sum::<f64>();
    let left_norm = left.iter().map(|value| value * value).sum::<f64>().sqrt();
    let right_norm = right.iter().map(|value| value * value).sum::<f64>().sqrt();
    if left_norm == 0.0 || right_norm == 0.0 {
        return Err(ComparisonError::ZeroNorm);
    }
    Ok((1.0 - dot / (left_norm * right_norm)).max(0.0))
}

fn batch_workload(report: &Report) -> Result<&Workload, ComparisonError> {
    report
        .workloads
        .iter()
        .find(|workload| workload.name == "ingest_batch_8")
        .ok_or(ComparisonError::MissingBatch)
}

fn compare_ordering(left: &Workload, right: &Workload) -> Result<OrderingMetrics, ComparisonError> {
    if left.reference_embeddings.len() != right.reference_embeddings.len() {
        return Err(ComparisonError::VectorCount(left.name.clone()));
    }
    let left_distances = pairwise_squared_l2(&left.reference_embeddings)?;
    let right_distances = pairwise_squared_l2(&right.reference_embeddings)?;
    let mut max_pairwise_distance_delta = 0.0_f64;
    let mut minimum_adjacent_distance_gap = f64::INFINITY;
    let mut mismatches = 0_u64;

    for (left_row, right_row) in left_distances.iter().zip(&right_distances) {
        for (&left_distance, &right_distance) in left_row.iter().zip(right_row) {
            max_pairwise_distance_delta =
                max_pairwise_distance_delta.max((left_distance - right_distance).abs());
        }
        let left_order = ordered_indices(left_row);
        let right_order = ordered_indices(right_row);
        mismatches += left_order
            .iter()
            .zip(&right_order)
            .filter(|(left, right)| left != right)
            .count() as u64;
        minimum_adjacent_distance_gap = minimum_adjacent_distance_gap
            .min(minimum_adjacent_gap(left_row, &left_order))
            .min(minimum_adjacent_gap(right_row, &right_order));
    }

    Ok(OrderingMetrics {
        identical: mismatches == 0,
        mismatches,
        max_pairwise_distance_delta,
        minimum_adjacent_distance_gap,
    })
}

fn minimum_adjacent_gap(distances: &[f64], order: &[usize]) -> f64 {
    order
        .windows(2)
        .map(|pair| (distances[pair[1]] - distances[pair[0]]).abs())
        .fold(f64::INFINITY, f64::min)
}

fn pairwise_squared_l2(vectors: &[Vec<f64>]) -> Result<Vec<Vec<f64>>, ComparisonError> {
    let mut distances = vec![vec![0.0; vectors.len()]; vectors.len()];
    for (query_index, query) in vectors.iter().enumerate() {
        for (candidate_index, candidate) in vectors.iter().enumerate() {
            if query.len() != candidate.len() {
                return Err(ComparisonError::Dimension("ingest_batch_8".to_owned()));
            }
            distances[query_index][candidate_index] = query
                .iter()
                .zip(candidate)
                .map(|(left, right)| {
                    let delta = left - right;
                    delta * delta
                })
                .sum();
        }
    }
    Ok(distances)
}

fn ordered_indices(distances: &[f64]) -> Vec<usize> {
    let mut indices = (0..distances.len()).collect::<Vec<_>>();
    indices.sort_by(|left, right| {
        distances[*left]
            .total_cmp(&distances[*right])
            .then_with(|| left.cmp(right))
    });
    indices
}

fn write_new_report(path: &Path, json: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(json)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Platform {
    os: String,
    arch: String,
}

#[derive(Debug, Deserialize)]
struct Report {
    schema_version: String,
    passed: bool,
    checkout: Value,
    model: Value,
    configuration: Value,
    qualification: String,
    thresholds: Value,
    platform: Platform,
    provenance: Value,
    workloads: Vec<Workload>,
}

#[derive(Debug, Deserialize)]
struct Workload {
    name: String,
    documents_per_call: usize,
    reference_embeddings: Vec<Vec<f64>>,
}

struct WorkloadMetrics {
    name: String,
    vectors: usize,
    dimension: usize,
    component_count: u64,
    max_component_abs_delta: f64,
    max_vector_l2_delta: f64,
    max_cosine_distance: f64,
    squared_component_delta_sum: f64,
}

impl WorkloadMetrics {
    fn into_report(self) -> WorkloadComparison {
        WorkloadComparison {
            name: self.name,
            vectors: self.vectors,
            dimension: self.dimension,
            max_component_abs_delta: self.max_component_abs_delta,
            max_vector_l2_delta: self.max_vector_l2_delta,
            max_cosine_distance: self.max_cosine_distance,
        }
    }
}

struct OrderingMetrics {
    identical: bool,
    mismatches: u64,
    max_pairwise_distance_delta: f64,
    minimum_adjacent_distance_gap: f64,
}

#[derive(Debug, Serialize)]
struct Comparison {
    schema_version: &'static str,
    passed: bool,
    left: Platform,
    right: Platform,
    thresholds: Thresholds,
    metrics: Metrics,
    workloads: Vec<WorkloadComparison>,
    checks: Checks,
}

#[derive(Debug, Serialize)]
struct Thresholds {
    max_component_abs_delta: f64,
    max_vector_l2_delta: f64,
    max_cosine_distance: f64,
    max_pairwise_distance_delta: f64,
    require_identical_ordering: bool,
}

#[derive(Debug, Serialize)]
struct Metrics {
    component_count: u64,
    max_component_abs_delta: f64,
    component_rms_delta: f64,
    max_vector_l2_delta: f64,
    max_cosine_distance: f64,
    max_pairwise_distance_delta: f64,
    minimum_adjacent_distance_gap: f64,
    ordering_mismatches: u64,
}

#[derive(Debug, Serialize)]
struct WorkloadComparison {
    name: String,
    vectors: usize,
    dimension: usize,
    max_component_abs_delta: f64,
    max_vector_l2_delta: f64,
    max_cosine_distance: f64,
}

#[derive(Debug, Serialize)]
struct Checks {
    source_reports_passed: bool,
    source_protocol_compatible: bool,
    supported_platform_pair: bool,
    provenance_compatible: bool,
    component_delta_within_tolerance: bool,
    vector_l2_delta_within_tolerance: bool,
    cosine_distance_within_tolerance: bool,
    pairwise_distance_delta_within_tolerance: bool,
    deterministic_ordering_identical: bool,
}

impl Checks {
    fn all(&self) -> bool {
        self.source_reports_passed
            && self.source_protocol_compatible
            && self.supported_platform_pair
            && self.provenance_compatible
            && self.component_delta_within_tolerance
            && self.vector_l2_delta_within_tolerance
            && self.cosine_distance_within_tolerance
            && self.pairwise_distance_delta_within_tolerance
            && self.deterministic_ordering_identical
    }
}

#[derive(Debug, thiserror::Error)]
enum ComparisonError {
    #[error("unsupported portability report schema {0}")]
    Schema(String),
    #[error("portability report does not contain the fixed batch-1 and batch-8 protocol")]
    WorkloadProtocol,
    #[error("reports must come from different platforms, both were {0:?}")]
    SamePlatform(Platform),
    #[error("embedding provenance must be a JSON object")]
    ProvenanceShape,
    #[error("workload count differs: left {left}, right {right}")]
    WorkloadCount { left: usize, right: usize },
    #[error("workload names differ: left {left}, right {right}")]
    WorkloadName { left: String, right: String },
    #[error("workload {0} has incompatible vector counts")]
    VectorCount(String),
    #[error("workload {0} has incompatible vector dimensions")]
    Dimension(String),
    #[error("workload {0} contains a non-finite component")]
    NonFinite(String),
    #[error("embedding vector has zero norm")]
    ZeroNorm,
    #[error("ingest_batch_8 workload is missing")]
    MissingBatch,
    #[error("comparison counter overflowed")]
    Overflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(platform: (&str, &str), delta: f64, reverse: bool) -> Report {
        let mut batch = vec![
            vec![1.0, 0.0, 0.0],
            vec![0.8, 0.6, 0.0],
            vec![0.0, 0.0, 1.0],
            vec![0.6, 0.8, 0.0],
            vec![0.0, 0.6, 0.8],
            vec![0.6, 0.0, 0.8],
            vec![0.577, 0.577, 0.577],
            vec![-1.0, 0.0, 0.0],
        ];
        batch[0][1] += delta;
        if reverse {
            batch.swap(1, 2);
        }
        Report {
            schema_version: REPORT_SCHEMA.to_owned(),
            passed: true,
            checkout: serde_json::json!({"git_revision": "fixture", "dirty": false}),
            model: serde_json::json!({"id": "fixture", "dimension": 3}),
            configuration: serde_json::json!({"fixed_input_sha256": "fixture"}),
            qualification: "development_portability_probe".to_owned(),
            thresholds: serde_json::json!({"max_initialize_ms": 10_000}),
            platform: Platform {
                os: platform.0.to_owned(),
                arch: platform.1.to_owned(),
            },
            provenance: serde_json::json!({
                "model_id": "fixture",
                "target_os": platform.0,
                "target_arch": platform.1,
                "target_endianness": "little"
            }),
            workloads: vec![
                Workload {
                    name: "interactive_query".to_owned(),
                    documents_per_call: 1,
                    reference_embeddings: vec![batch[0].clone()],
                },
                Workload {
                    name: "ingest_batch_8".to_owned(),
                    documents_per_call: batch.len(),
                    reference_embeddings: batch,
                },
            ],
        }
    }

    #[test]
    fn identical_cross_platform_vectors_pass_all_contract_checks() {
        let comparison = compare(
            &report(("linux", "x86_64"), 0.0, false),
            &report(("macos", "aarch64"), 0.0, false),
        )
        .unwrap();
        assert!(comparison.passed);
        assert_eq!(comparison.metrics.ordering_mismatches, 0);
        assert_eq!(comparison.metrics.max_component_abs_delta, 0.0);
    }

    #[test]
    fn component_drift_and_ordering_changes_fail_distinct_checks() {
        let drift = compare(
            &report(("linux", "x86_64"), 0.0, false),
            &report(("macos", "aarch64"), 1e-3, false),
        )
        .unwrap();
        assert!(!drift.checks.component_delta_within_tolerance);
        assert!(!drift.passed);

        let reordered = compare(
            &report(("linux", "x86_64"), 0.0, false),
            &report(("macos", "aarch64"), 0.0, true),
        )
        .unwrap();
        assert!(!reordered.checks.deterministic_ordering_identical);
        assert!(!reordered.passed);
    }

    #[test]
    fn provenance_differences_fail_without_hiding_numerical_metrics() {
        let left = report(("linux", "x86_64"), 0.0, false);
        let mut right = report(("macos", "aarch64"), 0.0, false);
        right.provenance["model_id"] = Value::String("other".to_owned());
        let comparison = compare(&left, &right).unwrap();
        assert!(!comparison.checks.provenance_compatible);
        assert!(!comparison.passed);
        assert_eq!(comparison.metrics.max_component_abs_delta, 0.0);
    }

    #[test]
    fn input_or_checkout_differences_fail_the_protocol_gate() {
        let left = report(("linux", "x86_64"), 0.0, false);
        let mut right = report(("macos", "aarch64"), 0.0, false);
        right.configuration["fixed_input_sha256"] = Value::String("other".to_owned());
        let comparison = compare(&left, &right).unwrap();
        assert!(!comparison.checks.source_protocol_compatible);
        assert!(!comparison.passed);
    }

    #[test]
    fn malformed_fixed_workload_is_rejected_without_panicking() {
        let left = report(("linux", "x86_64"), 0.0, false);
        let mut right = report(("macos", "aarch64"), 0.0, false);
        right.workloads[1].reference_embeddings.clear();
        right.workloads[1].documents_per_call = 0;
        assert!(matches!(
            compare(&left, &right),
            Err(ComparisonError::WorkloadProtocol)
        ));
    }
}
