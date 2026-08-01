#![deny(unsafe_code)]

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use clap::Parser;
use hsum::config::ManagedPaths;
use hsum::model::{EmbeddingOptions, LocalTextEmbedding, ModelStore};
use serde::Serialize;
use sha2::{Digest, Sha256};

const SCHEMA_VERSION: &str = "hsum.model-portability.v1";
const DEFAULT_MODEL: &str = "bge-small-en-v1-5-fp32";
const FASTEMBED_VERSION: &str = "5.17.4";
const INPUTS: &[u8] = include_bytes!("../benches/model_portability/inputs.json");
const MAX_INITIALIZE_MS: f64 = 10_000.0;
const MAX_INTERACTIVE_P95_MS: f64 = 500.0;
const MAX_BATCH_P95_MS: f64 = 1_500.0;
const MAX_PEAK_RSS_DELTA_BYTES: u64 = 1_073_741_824;

#[derive(Debug, Parser)]
#[command(
    name = "model-portability",
    about = "Verify hSUM model bytes and measure FastEmbed CPU portability"
)]
struct Args {
    #[arg(long, default_value = DEFAULT_MODEL)]
    model: String,
    #[arg(long, default_value_t = NonZeroUsize::new(3).unwrap())]
    warmups: NonZeroUsize,
    #[arg(long, default_value_t = NonZeroUsize::new(20).unwrap())]
    iterations: NonZeroUsize,
    #[arg(long, default_value_t = NonZeroUsize::new(512).unwrap())]
    max_length: NonZeroUsize,
    #[arg(long, default_value_t = NonZeroUsize::new(4).unwrap())]
    threads: NonZeroUsize,
    #[arg(long)]
    output: Option<PathBuf>,
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("model portability probe failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> Result<bool, Box<dyn std::error::Error>> {
    let inputs: Vec<String> = serde_json::from_slice(INPUTS)?;
    if inputs.len() < 8 || inputs.iter().any(String::is_empty) {
        return Err("the fixed portability corpus must contain eight nonempty documents".into());
    }

    let paths = ManagedPaths::from_environment()?;
    let store = ModelStore::new(paths.model_cache_dir());
    let memory_sampler = RssSampler::start();

    let started = Instant::now();
    let artifact = store.verify_embedding_artifact(&args.model)?;
    let verify_ms = elapsed_ms(started);

    let started = Instant::now();
    let bytes = artifact.read()?;
    let verified_bytes = bytes.bytes();
    let read_ms = elapsed_ms(started);

    let started = Instant::now();
    let mut model = bytes.initialize(EmbeddingOptions::new(args.max_length, args.threads))?;
    let initialize_ms = elapsed_ms(started);

    let interactive = measure_workload(
        &mut model,
        "interactive_query",
        &inputs[..1],
        args.warmups,
        args.iterations,
    )?;
    let batch = measure_workload(
        &mut model,
        "ingest_batch_8",
        &inputs[..8],
        args.warmups,
        args.iterations,
    )?;

    let memory = memory_sampler.stop();
    let checks = Checks {
        correct_dimension: model.dimension() == 384,
        deterministic_outputs: interactive.deterministic && batch.deterministic,
        initialize_within_limit: initialize_ms <= MAX_INITIALIZE_MS,
        interactive_p95_within_limit: interactive.latency_ms.p95 <= MAX_INTERACTIVE_P95_MS,
        batch_p95_within_limit: batch.latency_ms.p95 <= MAX_BATCH_P95_MS,
        peak_rss_delta_within_limit: memory
            .peak_delta_bytes
            .is_some_and(|bytes| bytes <= MAX_PEAK_RSS_DELTA_BYTES),
    };
    let passed = checks.all();
    let report = Report {
        schema_version: SCHEMA_VERSION,
        passed,
        qualification: "development_portability_probe",
        platform: Platform::detect(),
        checkout: Checkout::detect(),
        runtime: Runtime {
            backend: "FastEmbed",
            fastembed_version: FASTEMBED_VERSION,
            execution_provider: "CPU",
            network_model_loading: false,
        },
        model: Model {
            id: model.id().to_owned(),
            manifest_sha256: model.fingerprint().to_string(),
            dimension: model.dimension(),
            verified_bytes,
            pooling: "cls",
        },
        configuration: Configuration {
            max_length: args.max_length.get(),
            intra_threads: args.threads.get(),
            warmups: args.warmups.get(),
            iterations: args.iterations.get(),
            fixed_input_sha256: hex::encode(Sha256::digest(INPUTS)),
        },
        stages_ms: Stages {
            verify: verify_ms,
            read_verified_bytes: read_ms,
            initialize: initialize_ms,
        },
        memory,
        workloads: vec![interactive, batch],
        thresholds: Thresholds {
            max_initialize_ms: MAX_INITIALIZE_MS,
            max_interactive_p95_ms: MAX_INTERACTIVE_P95_MS,
            max_batch_p95_ms: MAX_BATCH_P95_MS,
            max_peak_rss_delta_bytes: MAX_PEAK_RSS_DELTA_BYTES,
        },
        checks,
    };
    let json = serde_json::to_vec_pretty(&report)?;
    if let Some(output) = args.output {
        write_new_report(&output, &json)?;
        eprintln!("wrote model portability report to {}", output.display());
    } else {
        std::io::stdout().write_all(&json)?;
        std::io::stdout().write_all(b"\n")?;
    }
    Ok(passed)
}

fn measure_workload(
    model: &mut LocalTextEmbedding,
    name: &'static str,
    inputs: &[String],
    warmups: NonZeroUsize,
    iterations: NonZeroUsize,
) -> Result<Workload, Box<dyn std::error::Error>> {
    let batch_size = NonZeroUsize::new(inputs.len()).ok_or("workload input cannot be empty")?;
    let mut expected_digest = None;
    for _ in 0..warmups.get() {
        let embeddings = model.embed(inputs, batch_size)?;
        expected_digest.get_or_insert_with(|| embedding_digest(&embeddings));
    }

    let mut samples = Vec::with_capacity(iterations.get());
    let mut deterministic = true;
    let mut output_digest = String::new();
    let mut min_norm = f32::INFINITY;
    let mut max_norm = f32::NEG_INFINITY;
    for _ in 0..iterations.get() {
        let started = Instant::now();
        let embeddings = model.embed(inputs, batch_size)?;
        samples.push(elapsed_ms(started));
        let digest = embedding_digest(&embeddings);
        deterministic &= expected_digest
            .as_ref()
            .is_some_and(|expected| expected == &digest);
        output_digest = digest;
        for embedding in &embeddings {
            let norm = embedding
                .iter()
                .map(|value| value * value)
                .sum::<f32>()
                .sqrt();
            min_norm = min_norm.min(norm);
            max_norm = max_norm.max(norm);
        }
    }
    let latency_ms = Latency::from_samples(&samples);
    let throughput_documents_per_second =
        (inputs.len() as f64 * 1_000.0) / latency_ms.mean.max(f64::EPSILON);
    Ok(Workload {
        name,
        documents_per_call: inputs.len(),
        latency_ms,
        throughput_documents_per_second,
        output_sha256: output_digest,
        deterministic,
        min_l2_norm: min_norm,
        max_l2_norm: max_norm,
    })
}

fn embedding_digest(embeddings: &[Vec<f32>]) -> String {
    let mut hasher = Sha256::new();
    for embedding in embeddings {
        hasher.update((embedding.len() as u64).to_le_bytes());
        for component in embedding {
            hasher.update(component.to_le_bytes());
        }
    }
    hex::encode(hasher.finalize())
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1_000.0
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

#[derive(Debug, Serialize)]
struct Report {
    schema_version: &'static str,
    passed: bool,
    qualification: &'static str,
    platform: Platform,
    checkout: Checkout,
    runtime: Runtime,
    model: Model,
    configuration: Configuration,
    stages_ms: Stages,
    memory: Memory,
    workloads: Vec<Workload>,
    thresholds: Thresholds,
    checks: Checks,
}

#[derive(Debug, Serialize)]
struct Platform {
    os: &'static str,
    arch: &'static str,
    os_version: Option<String>,
    cpu: Option<String>,
    logical_cpus: Option<usize>,
}

impl Platform {
    fn detect() -> Self {
        Self {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            os_version: command_output("uname", &["-sr"]),
            cpu: cpu_description(),
            logical_cpus: thread::available_parallelism().ok().map(NonZeroUsize::get),
        }
    }
}

#[derive(Debug, Serialize)]
struct Checkout {
    git_revision: Option<String>,
    dirty: Option<bool>,
    hsum_version: &'static str,
}

impl Checkout {
    fn detect() -> Self {
        Self {
            git_revision: command_output("git", &["rev-parse", "HEAD"]),
            dirty: Command::new("git")
                .args(["status", "--porcelain"])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| !output.stdout.is_empty()),
            hsum_version: env!("CARGO_PKG_VERSION"),
        }
    }
}

#[derive(Debug, Serialize)]
struct Runtime {
    backend: &'static str,
    fastembed_version: &'static str,
    execution_provider: &'static str,
    network_model_loading: bool,
}

#[derive(Debug, Serialize)]
struct Model {
    id: String,
    manifest_sha256: String,
    dimension: u32,
    verified_bytes: u64,
    pooling: &'static str,
}

#[derive(Debug, Serialize)]
struct Configuration {
    max_length: usize,
    intra_threads: usize,
    warmups: usize,
    iterations: usize,
    fixed_input_sha256: String,
}

#[derive(Debug, Serialize)]
struct Stages {
    verify: f64,
    read_verified_bytes: f64,
    initialize: f64,
}

#[derive(Debug, Serialize)]
struct Memory {
    baseline_rss_bytes: Option<u64>,
    peak_rss_bytes: Option<u64>,
    peak_delta_bytes: Option<u64>,
    final_rss_bytes: Option<u64>,
    sample_interval_ms: u64,
}

#[derive(Debug, Serialize)]
struct Workload {
    name: &'static str,
    documents_per_call: usize,
    latency_ms: Latency,
    throughput_documents_per_second: f64,
    output_sha256: String,
    deterministic: bool,
    min_l2_norm: f32,
    max_l2_norm: f32,
}

#[derive(Debug, Serialize)]
struct Latency {
    min: f64,
    mean: f64,
    p50: f64,
    p95: f64,
    max: f64,
}

impl Latency {
    fn from_samples(samples: &[f64]) -> Self {
        let mut ordered = samples.to_vec();
        ordered.sort_by(f64::total_cmp);
        let mean = ordered.iter().sum::<f64>() / ordered.len() as f64;
        Self {
            min: ordered[0],
            mean,
            p50: percentile(&ordered, 0.50),
            p95: percentile(&ordered, 0.95),
            max: ordered[ordered.len() - 1],
        }
    }
}

fn percentile(ordered: &[f64], percentile: f64) -> f64 {
    let rank = (ordered.len() as f64 * percentile).ceil() as usize;
    let index = rank.clamp(1, ordered.len()) - 1;
    ordered[index]
}

#[derive(Debug, Serialize)]
struct Thresholds {
    max_initialize_ms: f64,
    max_interactive_p95_ms: f64,
    max_batch_p95_ms: f64,
    max_peak_rss_delta_bytes: u64,
}

#[derive(Debug, Serialize)]
struct Checks {
    correct_dimension: bool,
    deterministic_outputs: bool,
    initialize_within_limit: bool,
    interactive_p95_within_limit: bool,
    batch_p95_within_limit: bool,
    peak_rss_delta_within_limit: bool,
}

impl Checks {
    fn all(&self) -> bool {
        self.correct_dimension
            && self.deterministic_outputs
            && self.initialize_within_limit
            && self.interactive_p95_within_limit
            && self.batch_p95_within_limit
            && self.peak_rss_delta_within_limit
    }
}

struct RssSampler {
    baseline: Option<u64>,
    peak: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl RssSampler {
    const INTERVAL: Duration = Duration::from_millis(20);

    fn start() -> Self {
        let baseline = current_rss_bytes();
        let peak = Arc::new(AtomicU64::new(baseline.unwrap_or(0)));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_peak = Arc::clone(&peak);
        let thread_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                if let Some(sample) = current_rss_bytes() {
                    thread_peak.fetch_max(sample, Ordering::Relaxed);
                }
                thread::sleep(Self::INTERVAL);
            }
        });
        Self {
            baseline,
            peak,
            stop,
            handle: Some(handle),
        }
    }

    fn stop(mut self) -> Memory {
        let final_rss_bytes = current_rss_bytes();
        if let Some(final_rss) = final_rss_bytes {
            self.peak.fetch_max(final_rss, Ordering::Relaxed);
        }
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        let observed_peak = self.peak.load(Ordering::Relaxed);
        let peak_rss_bytes = (observed_peak > 0).then_some(observed_peak);
        Memory {
            baseline_rss_bytes: self.baseline,
            peak_rss_bytes,
            peak_delta_bytes: self
                .baseline
                .zip(peak_rss_bytes)
                .map(|(baseline, peak)| peak.saturating_sub(baseline)),
            final_rss_bytes,
            sample_interval_ms: Self::INTERVAL.as_millis() as u64,
        }
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
    kibibytes.checked_mul(1_024)
}

#[cfg(target_os = "macos")]
fn current_rss_bytes() -> Option<u64> {
    let pid = std::process::id().to_string();
    let kibibytes = command_output("ps", &["-o", "rss=", "-p", &pid])?
        .parse::<u64>()
        .ok()?;
    kibibytes.checked_mul(1_024)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn current_rss_bytes() -> Option<u64> {
    None
}

#[cfg(target_os = "macos")]
fn cpu_description() -> Option<String> {
    command_output("sysctl", &["-n", "machdep.cpu.brand_string"])
}

#[cfg(target_os = "linux")]
fn cpu_description() -> Option<String> {
    let cpuinfo = fs::read_to_string("/proc/cpuinfo").ok()?;
    cpuinfo.lines().find_map(|line| {
        line.strip_prefix("model name")
            .and_then(|value| value.split_once(':'))
            .map(|(_, value)| value.trim().to_owned())
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn cpu_description() -> Option<String> {
    None
}

fn command_output(command: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(command).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}
