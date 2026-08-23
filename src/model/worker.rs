use std::collections::{BTreeMap, VecDeque};
use std::env;
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::domain::Sha256Digest;
use crate::store::EMBEDDING_DIMENSION;

use super::{
    EmbeddingInferenceError, EmbeddingOptions, LocalTextEmbedding, ModelError, ModelStore,
};

pub const QUERY_MODEL_WORKERS: usize = 2;
pub const QUERY_MODEL_QUEUE: usize = 8;
pub const MAX_QUERY_EMBEDDING_BYTES: usize = 4_096;
const MAX_WORKER_REQUEST_BYTES: usize = 32 * 1024;
const MAX_WORKER_RESPONSE_BYTES: usize = 64 * 1024;
const WORKER_SCHEMA: &str = "hsum.model-worker.v1";
const WORKER_ARGUMENT: &str = "__model-worker";
const WORKER_CACHE_ARGUMENT: &str = "--cache-root";
const WORKER_HARD_KILL_GRACE: Duration = Duration::from_secs(2);
const WAIT_POLL: Duration = Duration::from_millis(10);
const NORMALIZED_NORM_TOLERANCE: f32 = 0.001;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryEmbeddingRequest {
    model_id: String,
    model_fingerprint: Sha256Digest,
    query: String,
}

impl QueryEmbeddingRequest {
    pub fn new(
        model_id: impl Into<String>,
        model_fingerprint: Sha256Digest,
        query: impl Into<String>,
    ) -> Result<Self, QueryEmbeddingError> {
        let model_id = model_id.into();
        let query = query.into();
        if model_id.is_empty()
            || model_id.len() > 64
            || !model_id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(QueryEmbeddingError::InvalidRequest("model ID"));
        }
        if query.is_empty()
            || query.len() > MAX_QUERY_EMBEDDING_BYTES
            || query.as_bytes().contains(&0)
        {
            return Err(QueryEmbeddingError::InvalidRequest("query bytes"));
        }
        Ok(Self {
            model_id,
            model_fingerprint,
            query,
        })
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    pub const fn model_fingerprint(&self) -> Sha256Digest {
        self.model_fingerprint
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    fn key(&self) -> QueryKey {
        QueryKey {
            model_id: self.model_id.clone(),
            model_fingerprint: self.model_fingerprint,
            query_sha256: Sha256Digest::of_bytes(self.query.as_bytes()),
        }
    }
}

#[derive(Clone, Copy)]
pub struct QueryEmbeddingControl {
    pub deadline: Instant,
    pub cancelled: Option<fn() -> bool>,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum QueryEmbeddingError {
    #[error("query embedding request is invalid: {0}")]
    InvalidRequest(&'static str),
    #[error("the pinned embedding model is not installed")]
    ModelMissing,
    #[error("the pinned embedding model artifact is not verified")]
    ModelUnverified,
    #[error("the embedding model does not match the index pin")]
    ModelIncompatible,
    #[error("the bounded model queue is full")]
    Busy,
    #[error("the private model worker is restarting")]
    Restarting,
    #[error("the query embedding request was cancelled")]
    Cancelled,
    #[error("the query embedding deadline expired")]
    Deadline,
    #[error("the private model worker protocol is invalid")]
    Protocol,
}

pub struct QueryEmbeddingService {
    queue: Arc<WorkerQueue>,
    workers: Vec<JoinHandle<()>>,
}

impl QueryEmbeddingService {
    pub fn new(cache_root: PathBuf) -> Result<Self, QueryEmbeddingError> {
        if !cache_root.is_absolute() {
            return Err(QueryEmbeddingError::InvalidRequest("model cache path"));
        }
        let executable = env::current_exe().map_err(|_| QueryEmbeddingError::Restarting)?;
        let launch = WorkerLaunch {
            program: executable,
            arguments: vec![
                OsString::from(WORKER_ARGUMENT),
                OsString::from(WORKER_CACHE_ARGUMENT),
                cache_root.into_os_string(),
            ],
            hard_kill_grace: WORKER_HARD_KILL_GRACE,
        };
        let factory: WorkerFactory = Arc::new(move || {
            ProcessEmbeddingWorker::spawn(&launch)
                .map(|worker| Box::new(worker) as Box<dyn EmbeddingWorker>)
        });
        Self::with_factory(factory, QUERY_MODEL_WORKERS)
    }

    pub fn embed(
        &self,
        request: QueryEmbeddingRequest,
        control: QueryEmbeddingControl,
    ) -> Result<Vec<f32>, QueryEmbeddingError> {
        checkpoint(control)?;
        let key = request.key();
        let job = {
            let mut state = lock(&self.queue.state)?;
            if let Some(job) = state.in_flight.get(&key).and_then(Weak::upgrade) {
                job
            } else {
                if state.pending.len() >= QUERY_MODEL_QUEUE {
                    return Err(QueryEmbeddingError::Busy);
                }
                let job = Arc::new(QueryJob {
                    key: key.clone(),
                    request,
                    deadline: control.deadline,
                    result: Mutex::new(None),
                    ready: Condvar::new(),
                });
                state.in_flight.insert(key, Arc::downgrade(&job));
                state.pending.push_back(Arc::clone(&job));
                self.queue.available.notify_one();
                job
            }
        };
        wait_for_job(&job, control)
    }

    fn with_factory(
        factory: WorkerFactory,
        worker_count: usize,
    ) -> Result<Self, QueryEmbeddingError> {
        let queue = Arc::new(WorkerQueue {
            state: Mutex::new(QueueState::default()),
            available: Condvar::new(),
            shutdown: AtomicBool::new(false),
        });
        let mut workers = Vec::with_capacity(worker_count);
        for ordinal in 0..worker_count {
            let worker_queue = Arc::clone(&queue);
            let worker_factory = Arc::clone(&factory);
            match thread::Builder::new()
                .name(format!("hsum-model-supervisor-{ordinal}"))
                .spawn(move || worker_loop(worker_queue, worker_factory))
            {
                Ok(worker) => workers.push(worker),
                Err(_) => {
                    queue.shutdown.store(true, Ordering::Release);
                    queue.available.notify_all();
                    for worker in workers {
                        let _ = worker.join();
                    }
                    return Err(QueryEmbeddingError::Restarting);
                }
            }
        }
        Ok(Self { queue, workers })
    }

    #[cfg(test)]
    fn pending(&self) -> usize {
        self.queue
            .state
            .lock()
            .map_or(0, |state| state.pending.len())
    }

    #[cfg(test)]
    fn in_flight_references(&self, request: &QueryEmbeddingRequest) -> usize {
        self.queue.state.lock().map_or(0, |state| {
            state
                .in_flight
                .get(&request.key())
                .and_then(Weak::upgrade)
                .map_or(0, |job| Arc::strong_count(&job).saturating_sub(1))
        })
    }
}

impl Drop for QueryEmbeddingService {
    fn drop(&mut self) {
        self.queue.shutdown.store(true, Ordering::Release);
        self.queue.available.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct QueryKey {
    model_id: String,
    model_fingerprint: Sha256Digest,
    query_sha256: Sha256Digest,
}

struct QueryJob {
    key: QueryKey,
    request: QueryEmbeddingRequest,
    deadline: Instant,
    result: Mutex<Option<Result<Vec<f32>, QueryEmbeddingError>>>,
    ready: Condvar,
}

struct WorkerQueue {
    state: Mutex<QueueState>,
    available: Condvar,
    shutdown: AtomicBool,
}

#[derive(Default)]
struct QueueState {
    pending: VecDeque<Arc<QueryJob>>,
    in_flight: BTreeMap<QueryKey, Weak<QueryJob>>,
}

trait EmbeddingWorker: Send {
    fn execute(
        &mut self,
        request: &QueryEmbeddingRequest,
        deadline: Instant,
        shutdown: &AtomicBool,
    ) -> Result<Vec<f32>, QueryEmbeddingError>;
}

type WorkerFactory =
    Arc<dyn Fn() -> Result<Box<dyn EmbeddingWorker>, QueryEmbeddingError> + Send + Sync + 'static>;

fn worker_loop(queue: Arc<WorkerQueue>, factory: WorkerFactory) {
    let mut worker: Option<Box<dyn EmbeddingWorker>> = None;
    loop {
        let Some(job) = next_job(&queue) else {
            break;
        };
        let result = if Instant::now() >= job.deadline {
            Err(QueryEmbeddingError::Deadline)
        } else {
            if worker.is_none() {
                worker = factory().ok();
            }
            match worker.as_mut() {
                Some(worker) => worker.execute(&job.request, job.deadline, &queue.shutdown),
                None => Err(QueryEmbeddingError::Restarting),
            }
        };
        if matches!(
            result,
            Err(QueryEmbeddingError::Restarting
                | QueryEmbeddingError::Protocol
                | QueryEmbeddingError::Deadline)
        ) {
            worker = None;
        }
        finish_job(&queue, &job, result);
    }
}

fn next_job(queue: &WorkerQueue) -> Option<Arc<QueryJob>> {
    let mut state = queue.state.lock().ok()?;
    loop {
        if let Some(job) = state.pending.pop_front() {
            return Some(job);
        }
        if queue.shutdown.load(Ordering::Acquire) {
            return None;
        }
        state = queue.available.wait(state).ok()?;
    }
}

fn finish_job(queue: &WorkerQueue, job: &QueryJob, result: Result<Vec<f32>, QueryEmbeddingError>) {
    if let Ok(mut slot) = job.result.lock() {
        *slot = Some(result);
        job.ready.notify_all();
    }
    if let Ok(mut state) = queue.state.lock()
        && state
            .in_flight
            .get(&job.key)
            .and_then(Weak::upgrade)
            .is_some_and(|current| std::ptr::eq(current.as_ref(), job))
    {
        state.in_flight.remove(&job.key);
    }
}

fn wait_for_job(
    job: &QueryJob,
    control: QueryEmbeddingControl,
) -> Result<Vec<f32>, QueryEmbeddingError> {
    let mut result = lock(&job.result)?;
    loop {
        if let Some(result) = result.as_ref() {
            return result.clone().and_then(validate_worker_embedding);
        }
        checkpoint(control)?;
        let remaining = control.deadline.saturating_duration_since(Instant::now());
        let wait = remaining.min(WAIT_POLL);
        let (next, _) = job
            .ready
            .wait_timeout(result, wait)
            .map_err(|_| QueryEmbeddingError::Restarting)?;
        result = next;
    }
}

fn checkpoint(control: QueryEmbeddingControl) -> Result<(), QueryEmbeddingError> {
    if control.cancelled.is_some_and(|cancelled| cancelled()) {
        return Err(QueryEmbeddingError::Cancelled);
    }
    if Instant::now() >= control.deadline {
        return Err(QueryEmbeddingError::Deadline);
    }
    Ok(())
}

fn lock<T>(mutex: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, QueryEmbeddingError> {
    mutex.lock().map_err(|_| QueryEmbeddingError::Restarting)
}

fn validate_worker_embedding(embedding: Vec<f32>) -> Result<Vec<f32>, QueryEmbeddingError> {
    if embedding.len() != EMBEDDING_DIMENSION as usize
        || embedding.iter().any(|component| !component.is_finite())
    {
        return Err(QueryEmbeddingError::ModelIncompatible);
    }
    let norm = embedding
        .iter()
        .map(|component| component * component)
        .sum::<f32>()
        .sqrt();
    if !norm.is_finite() || (norm - 1.0).abs() > NORMALIZED_NORM_TOLERANCE {
        return Err(QueryEmbeddingError::ModelIncompatible);
    }
    Ok(embedding)
}

#[derive(Clone)]
struct WorkerLaunch {
    program: PathBuf,
    arguments: Vec<OsString>,
    hard_kill_grace: Duration,
}

struct ProcessEmbeddingWorker {
    child: Child,
    stdin: Option<ChildStdin>,
    responses: Receiver<ReaderEvent>,
    reader: Option<JoinHandle<()>>,
    hard_kill_grace: Duration,
    usable: bool,
}

impl ProcessEmbeddingWorker {
    fn spawn(launch: &WorkerLaunch) -> Result<Self, QueryEmbeddingError> {
        let mut child = Command::new(&launch.program)
            .args(&launch.arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| QueryEmbeddingError::Restarting)?;
        let stdin = child.stdin.take().ok_or(QueryEmbeddingError::Restarting)?;
        let stdout = child.stdout.take().ok_or(QueryEmbeddingError::Restarting)?;
        let (sender, responses) = mpsc::channel();
        let reader = thread::Builder::new()
            .name("hsum-model-worker-reader".to_owned())
            .spawn(move || {
                let mut stdout = BufReader::new(stdout);
                loop {
                    let event = match read_bounded_line(&mut stdout, MAX_WORKER_RESPONSE_BYTES) {
                        Ok(Some(line)) => ReaderEvent::Line(line),
                        Ok(None) => ReaderEvent::Eof,
                        Err(_) => ReaderEvent::Invalid,
                    };
                    let finished = !matches!(event, ReaderEvent::Line(_));
                    if sender.send(event).is_err() || finished {
                        break;
                    }
                }
            })
            .map_err(|_| QueryEmbeddingError::Restarting)?;
        Ok(Self {
            child,
            stdin: Some(stdin),
            responses,
            reader: Some(reader),
            hard_kill_grace: launch.hard_kill_grace,
            usable: true,
        })
    }

    fn terminate(&mut self) {
        self.usable = false;
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl EmbeddingWorker for ProcessEmbeddingWorker {
    fn execute(
        &mut self,
        request: &QueryEmbeddingRequest,
        deadline: Instant,
        shutdown: &AtomicBool,
    ) -> Result<Vec<f32>, QueryEmbeddingError> {
        if !self.usable {
            return Err(QueryEmbeddingError::Restarting);
        }
        let request_id = Uuid::new_v4();
        let wire = WorkerRequest {
            schema_version: WORKER_SCHEMA.to_owned(),
            request_id,
            model_id: request.model_id.clone(),
            model_fingerprint: request.model_fingerprint,
            query: request.query.clone(),
        };
        let mut frame = serde_json::to_vec(&wire).map_err(|_| QueryEmbeddingError::Protocol)?;
        if frame.len() > MAX_WORKER_REQUEST_BYTES {
            return Err(QueryEmbeddingError::InvalidRequest("worker frame"));
        }
        frame.push(b'\n');
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(QueryEmbeddingError::Restarting);
        };
        if stdin
            .write_all(&frame)
            .and_then(|()| stdin.flush())
            .is_err()
        {
            self.terminate();
            return Err(QueryEmbeddingError::Restarting);
        }

        let hard_deadline = deadline
            .checked_add(self.hard_kill_grace)
            .unwrap_or(deadline);
        loop {
            if shutdown.load(Ordering::Acquire) {
                self.terminate();
                return Err(QueryEmbeddingError::Cancelled);
            }
            let now = Instant::now();
            if now >= hard_deadline {
                self.terminate();
                return Err(QueryEmbeddingError::Deadline);
            }
            match self
                .responses
                .recv_timeout((hard_deadline - now).min(WAIT_POLL))
            {
                Ok(ReaderEvent::Line(line)) => {
                    let response = serde_json::from_slice::<WorkerResponse>(&line)
                        .map_err(|_| QueryEmbeddingError::Protocol)?;
                    if response.schema_version != WORKER_SCHEMA || response.request_id != request_id
                    {
                        self.terminate();
                        return Err(QueryEmbeddingError::Protocol);
                    }
                    return match (response.status, response.embedding, response.error) {
                        (WorkerStatus::Ok, Some(embedding), None) => {
                            validate_worker_embedding(embedding)
                        }
                        (WorkerStatus::Error, None, Some(error)) => Err(error.into()),
                        _ => {
                            self.terminate();
                            Err(QueryEmbeddingError::Protocol)
                        }
                    };
                }
                Ok(ReaderEvent::Eof | ReaderEvent::Invalid)
                | Err(RecvTimeoutError::Disconnected) => {
                    self.terminate();
                    return Err(QueryEmbeddingError::Restarting);
                }
                Err(RecvTimeoutError::Timeout) => {
                    if self.child.try_wait().ok().flatten().is_some() {
                        self.terminate();
                        return Err(QueryEmbeddingError::Restarting);
                    }
                }
            }
        }
    }
}

impl Drop for ProcessEmbeddingWorker {
    fn drop(&mut self) {
        self.terminate();
    }
}

enum ReaderEvent {
    Line(Vec<u8>),
    Eof,
    Invalid,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkerRequest {
    schema_version: String,
    request_id: Uuid,
    model_id: String,
    model_fingerprint: Sha256Digest,
    query: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkerResponse {
    schema_version: String,
    request_id: Uuid,
    status: WorkerStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    embedding: Option<Vec<f32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<WorkerFailure>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum WorkerStatus {
    Ok,
    Error,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum WorkerFailure {
    ModelMissing,
    ModelUnverified,
    ModelIncompatible,
    Restarting,
}

impl From<WorkerFailure> for QueryEmbeddingError {
    fn from(error: WorkerFailure) -> Self {
        match error {
            WorkerFailure::ModelMissing => Self::ModelMissing,
            WorkerFailure::ModelUnverified => Self::ModelUnverified,
            WorkerFailure::ModelIncompatible => Self::ModelIncompatible,
            WorkerFailure::Restarting => Self::Restarting,
        }
    }
}

struct WorkerSession {
    model_id: String,
    model_fingerprint: Sha256Digest,
    model: LocalTextEmbedding,
}

pub(crate) fn run_private_model_worker_from_env() -> Option<ExitCode> {
    let arguments = env::args_os().collect::<Vec<_>>();
    if arguments.get(1).and_then(|value| value.to_str()) != Some(WORKER_ARGUMENT) {
        return None;
    }
    if arguments.len() != 4
        || arguments.get(2).and_then(|value| value.to_str()) != Some(WORKER_CACHE_ARGUMENT)
    {
        return Some(ExitCode::from(2));
    }
    let cache_root = PathBuf::from(&arguments[3]);
    if !cache_root.is_absolute() {
        return Some(ExitCode::from(2));
    }
    Some(match run_private_model_worker(cache_root) {
        Ok(()) => ExitCode::SUCCESS,
        Err(()) => ExitCode::FAILURE,
    })
}

fn run_private_model_worker(cache_root: PathBuf) -> Result<(), ()> {
    let store = ModelStore::new(cache_root);
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = BufReader::new(stdin.lock());
    let mut output = BufWriter::new(stdout.lock());
    let mut session = None;
    while let Some(frame) =
        read_bounded_line(&mut input, MAX_WORKER_REQUEST_BYTES).map_err(|_| ())?
    {
        let request = serde_json::from_slice::<WorkerRequest>(&frame).map_err(|_| ())?;
        if validate_worker_request(&request).is_err() {
            return Err(());
        }
        let (status, embedding, error) = match embed_worker_request(&store, &request, &mut session)
        {
            Ok(embedding) => (WorkerStatus::Ok, Some(embedding), None),
            Err(error) => (WorkerStatus::Error, None, Some(error)),
        };
        let response = WorkerResponse {
            schema_version: WORKER_SCHEMA.to_owned(),
            request_id: request.request_id,
            status,
            embedding,
            error,
        };
        let frame = serde_json::to_vec(&response).map_err(|_| ())?;
        if frame.len() > MAX_WORKER_RESPONSE_BYTES {
            return Err(());
        }
        output.write_all(&frame).map_err(|_| ())?;
        output.write_all(b"\n").map_err(|_| ())?;
        output.flush().map_err(|_| ())?;
    }
    Ok(())
}

fn validate_worker_request(request: &WorkerRequest) -> Result<(), QueryEmbeddingError> {
    if request.schema_version != WORKER_SCHEMA {
        return Err(QueryEmbeddingError::Protocol);
    }
    QueryEmbeddingRequest::new(
        request.model_id.clone(),
        request.model_fingerprint,
        request.query.clone(),
    )?;
    Ok(())
}

fn embed_worker_request(
    store: &ModelStore<'_>,
    request: &WorkerRequest,
    session: &mut Option<WorkerSession>,
) -> Result<Vec<f32>, WorkerFailure> {
    if session.as_ref().is_none_or(|session| {
        session.model_id != request.model_id
            || session.model_fingerprint != request.model_fingerprint
    }) {
        let manifest = store
            .manifest(&request.model_id)
            .map_err(|_| WorkerFailure::ModelIncompatible)?;
        let fingerprint = manifest
            .fingerprint()
            .map_err(|_| WorkerFailure::ModelIncompatible)?;
        if fingerprint != request.model_fingerprint || manifest.dimension != EMBEDDING_DIMENSION {
            return Err(WorkerFailure::ModelIncompatible);
        }
        let artifact = store
            .verify_embedding_artifact(&request.model_id)
            .map_err(classify_inference_error)?;
        let options = EmbeddingOptions::new(
            NonZeroUsize::new(512).expect("worker maximum length is nonzero"),
            NonZeroUsize::new(2).expect("worker thread count is nonzero"),
        );
        let model = artifact
            .read()
            .and_then(|bytes| bytes.initialize(options))
            .map_err(classify_inference_error)?;
        if model.fingerprint() != request.model_fingerprint
            || model.dimension() != EMBEDDING_DIMENSION
        {
            return Err(WorkerFailure::ModelIncompatible);
        }
        *session = Some(WorkerSession {
            model_id: request.model_id.clone(),
            model_fingerprint: request.model_fingerprint,
            model,
        });
    }
    let session = session.as_mut().ok_or(WorkerFailure::Restarting)?;
    let embeddings = session
        .model
        .embed(std::slice::from_ref(&request.query), NonZeroUsize::MIN)
        .map_err(classify_inference_error)?;
    embeddings
        .into_iter()
        .next()
        .ok_or(WorkerFailure::Restarting)
}

fn classify_inference_error(error: EmbeddingInferenceError) -> WorkerFailure {
    match error {
        EmbeddingInferenceError::Artifact(ModelError::NotInstalled { .. }) => {
            WorkerFailure::ModelMissing
        }
        EmbeddingInferenceError::Artifact(_) => WorkerFailure::ModelUnverified,
        EmbeddingInferenceError::WrongKind(_)
        | EmbeddingInferenceError::ManifestFileMissing(_)
        | EmbeddingInferenceError::OutputCount { .. }
        | EmbeddingInferenceError::DimensionOverflow
        | EmbeddingInferenceError::OutputDimension { .. }
        | EmbeddingInferenceError::NonFinite { .. }
        | EmbeddingInferenceError::NotNormalized { .. } => WorkerFailure::ModelIncompatible,
        EmbeddingInferenceError::EmptyInput | EmbeddingInferenceError::FastEmbed(_) => {
            WorkerFailure::Restarting
        }
    }
}

fn read_bounded_line(reader: &mut impl BufRead, maximum: usize) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "unterminated worker frame",
                ))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |position| position + 1);
        let payload = newline.map_or(available, |position| &available[..position]);
        if line.len().saturating_add(payload.len()) > maximum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "worker frame exceeds limit",
            ));
        }
        line.extend_from_slice(payload);
        reader.consume(consumed);
        if newline.is_some() {
            return Ok(Some(line));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tempfile::tempdir;

    use super::*;

    static CANCELLED: AtomicBool = AtomicBool::new(false);
    static CANCELLATION_TEST_LOCK: Mutex<()> = Mutex::new(());
    type ReleaseGate = Arc<(Mutex<bool>, Condvar)>;
    type BlockingServiceFixture = (QueryEmbeddingService, Arc<AtomicUsize>, ReleaseGate);

    fn cancelled() -> bool {
        CANCELLED.load(Ordering::Acquire)
    }

    fn request(query: &str) -> QueryEmbeddingRequest {
        QueryEmbeddingRequest::new(
            "bge-small-en-v1-5-fp32",
            Sha256Digest::of_bytes(b"model"),
            query,
        )
        .unwrap()
    }

    fn control(duration: Duration) -> QueryEmbeddingControl {
        QueryEmbeddingControl {
            deadline: Instant::now() + duration,
            cancelled: None,
        }
    }

    fn unit_vector() -> Vec<f32> {
        let mut vector = vec![0.0; EMBEDDING_DIMENSION as usize];
        vector[0] = 1.0;
        vector
    }

    struct BlockingWorker {
        calls: Arc<AtomicUsize>,
        release: ReleaseGate,
        result: Result<Vec<f32>, QueryEmbeddingError>,
    }

    impl EmbeddingWorker for BlockingWorker {
        fn execute(
            &mut self,
            _request: &QueryEmbeddingRequest,
            _deadline: Instant,
            shutdown: &AtomicBool,
        ) -> Result<Vec<f32>, QueryEmbeddingError> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            let (lock, wake) = self.release.as_ref();
            let mut released = lock.lock().unwrap();
            while !*released && !shutdown.load(Ordering::Acquire) {
                released = wake.wait_timeout(released, WAIT_POLL).unwrap().0;
            }
            self.result.clone()
        }
    }

    struct ImmediateWorker(Result<Vec<f32>, QueryEmbeddingError>);

    impl EmbeddingWorker for ImmediateWorker {
        fn execute(
            &mut self,
            _request: &QueryEmbeddingRequest,
            _deadline: Instant,
            _shutdown: &AtomicBool,
        ) -> Result<Vec<f32>, QueryEmbeddingError> {
            self.0.clone()
        }
    }

    fn blocking_service(worker_count: usize) -> BlockingServiceFixture {
        let calls = Arc::new(AtomicUsize::new(0));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let factory_calls = Arc::clone(&calls);
        let factory_release = Arc::clone(&release);
        let factory: WorkerFactory = Arc::new(move || {
            Ok(Box::new(BlockingWorker {
                calls: Arc::clone(&factory_calls),
                release: Arc::clone(&factory_release),
                result: Ok(unit_vector()),
            }))
        });
        (
            QueryEmbeddingService::with_factory(factory, worker_count).unwrap(),
            calls,
            release,
        )
    }

    fn release(release: &(Mutex<bool>, Condvar)) {
        *release.0.lock().unwrap() = true;
        release.1.notify_all();
    }

    fn wait_until(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !condition() {
            assert!(Instant::now() < deadline, "condition timed out");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn identical_inflight_queries_coalesce_without_consuming_queue_slots() {
        let (service, calls, release_gate) = blocking_service(QUERY_MODEL_WORKERS);
        let service = Arc::new(service);
        let mut callers = Vec::new();
        for _ in 0..16 {
            let service = Arc::clone(&service);
            callers.push(thread::spawn(move || {
                service.embed(request("same query"), control(Duration::from_secs(2)))
            }));
        }
        wait_until(|| calls.load(Ordering::Acquire) == 1);
        wait_until(|| service.in_flight_references(&request("same query")) >= 17);
        assert_eq!(service.pending(), 0);
        release(&release_gate);
        for caller in callers {
            assert_eq!(caller.join().unwrap().unwrap(), unit_vector());
        }
        assert_eq!(calls.load(Ordering::Acquire), 1);
    }

    #[test]
    fn two_workers_and_eight_pending_jobs_are_hard_admission_bounds() {
        let (service, calls, release_gate) = blocking_service(QUERY_MODEL_WORKERS);
        let service = Arc::new(service);
        let mut callers = Vec::new();
        for ordinal in 0..QUERY_MODEL_WORKERS {
            let service = Arc::clone(&service);
            callers.push(thread::spawn(move || {
                service.embed(
                    request(&format!("query {ordinal}")),
                    control(Duration::from_secs(30)),
                )
            }));
        }
        wait_until(|| calls.load(Ordering::Acquire) == QUERY_MODEL_WORKERS);
        for ordinal in QUERY_MODEL_WORKERS..(QUERY_MODEL_WORKERS + QUERY_MODEL_QUEUE) {
            let service = Arc::clone(&service);
            callers.push(thread::spawn(move || {
                service.embed(
                    request(&format!("query {ordinal}")),
                    control(Duration::from_secs(30)),
                )
            }));
        }
        wait_until(|| service.pending() == QUERY_MODEL_QUEUE);
        assert!(matches!(
            service.embed(request("overflow"), control(Duration::from_secs(1))),
            Err(QueryEmbeddingError::Busy)
        ));
        release(&release_gate);
        for caller in callers {
            assert!(caller.join().unwrap().is_ok());
        }
    }

    #[test]
    fn cancel_storm_leaves_lexical_sqlite_service_available() {
        let _serial = CANCELLATION_TEST_LOCK.lock().unwrap();
        CANCELLED.store(false, Ordering::Release);
        let (service, calls, release_gate) = blocking_service(QUERY_MODEL_WORKERS);
        let service = Arc::new(service);
        let mut callers = Vec::new();
        for ordinal in 0..QUERY_MODEL_WORKERS {
            let service = Arc::clone(&service);
            callers.push(thread::spawn(move || {
                service.embed(
                    request(&format!("cancel storm {ordinal}")),
                    QueryEmbeddingControl {
                        deadline: Instant::now() + Duration::from_secs(30),
                        cancelled: Some(cancelled),
                    },
                )
            }));
        }
        wait_until(|| calls.load(Ordering::Acquire) == QUERY_MODEL_WORKERS);
        for ordinal in QUERY_MODEL_WORKERS..(QUERY_MODEL_WORKERS + QUERY_MODEL_QUEUE) {
            let service = Arc::clone(&service);
            callers.push(thread::spawn(move || {
                service.embed(
                    request(&format!("cancel storm {ordinal}")),
                    QueryEmbeddingControl {
                        deadline: Instant::now() + Duration::from_secs(30),
                        cancelled: Some(cancelled),
                    },
                )
            }));
        }
        wait_until(|| service.pending() == QUERY_MODEL_QUEUE);
        CANCELLED.store(true, Ordering::Release);
        for caller in callers {
            assert!(matches!(
                caller.join().unwrap(),
                Err(QueryEmbeddingError::Cancelled)
            ));
        }

        let directory = tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let database = crate::store::IndexDb::create(
            &directory.path().join("index.sqlite"),
            crate::domain::IndexId::new_v4(),
        )
        .unwrap();
        let project_id = crate::domain::ProjectId::new_v4();
        database
            .connection()
            .execute(
                "INSERT INTO projects(id, name, scope_revision, created_at)
                 VALUES (?1, 'lexical-during-cancel', 0, '2026-08-02T00:00:00Z')",
                [project_id.as_uuid().as_bytes().as_slice()],
            )
            .unwrap();
        let lexical = crate::search::SearchRequest::new(
            "*(){}[]",
            crate::search::SearchMode::Lexical,
            10,
            3_000,
            false,
        )
        .unwrap();
        assert!(
            database
                .search(project_id, &lexical)
                .unwrap()
                .results
                .is_empty()
        );

        release(&release_gate);
        CANCELLED.store(false, Ordering::Release);
    }

    #[test]
    fn caller_cancellation_and_deadline_discard_late_results() {
        let _serial = CANCELLATION_TEST_LOCK.lock().unwrap();
        CANCELLED.store(false, Ordering::Release);
        let (service, calls, release_gate) = blocking_service(1);
        let service = Arc::new(service);
        let cancelled_service = Arc::clone(&service);
        let caller = thread::spawn(move || {
            cancelled_service.embed(
                request("cancel me"),
                QueryEmbeddingControl {
                    deadline: Instant::now() + Duration::from_secs(1),
                    cancelled: Some(cancelled),
                },
            )
        });
        wait_until(|| calls.load(Ordering::Acquire) == 1);
        CANCELLED.store(true, Ordering::Release);
        assert!(matches!(
            caller.join().unwrap(),
            Err(QueryEmbeddingError::Cancelled)
        ));
        assert!(matches!(
            service.embed(request("deadline"), control(Duration::from_millis(20))),
            Err(QueryEmbeddingError::Deadline)
        ));
        release(&release_gate);
        CANCELLED.store(false, Ordering::Release);
    }

    #[test]
    fn query_and_worker_outputs_are_bounded_and_validated() {
        assert!(matches!(
            QueryEmbeddingRequest::new(
                "bge-small-en-v1-5-fp32",
                Sha256Digest::of_bytes(b"model"),
                "x".repeat(MAX_QUERY_EMBEDDING_BYTES + 1),
            ),
            Err(QueryEmbeddingError::InvalidRequest("query bytes"))
        ));
        assert!(matches!(
            validate_worker_embedding(vec![0.0; EMBEDDING_DIMENSION as usize]),
            Err(QueryEmbeddingError::ModelIncompatible)
        ));
        let mut cursor = io::Cursor::new(vec![b'x'; MAX_WORKER_RESPONSE_BYTES + 1]);
        assert!(read_bounded_line(&mut cursor, MAX_WORKER_RESPONSE_BYTES).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn overdue_process_is_killed_after_the_grace_period() {
        let directory = tempdir().unwrap();
        let script = directory.path().join("worker.sh");
        fs::write(&script, "#!/bin/sh\nwhile :; do :; done\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let launch = WorkerLaunch {
            program: script,
            arguments: Vec::new(),
            hard_kill_grace: Duration::from_millis(20),
        };
        let mut worker = ProcessEmbeddingWorker::spawn(&launch).unwrap();
        let started = Instant::now();

        assert!(matches!(
            worker.execute(
                &request("overdue"),
                Instant::now() + Duration::from_millis(50),
                &AtomicBool::new(false),
            ),
            Err(QueryEmbeddingError::Deadline)
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(worker.child.try_wait().unwrap().is_some());
    }

    #[test]
    fn failed_worker_is_replaced_before_the_next_job() {
        let factory_calls = Arc::new(AtomicUsize::new(0));
        let calls = Arc::clone(&factory_calls);
        let factory: WorkerFactory = Arc::new(move || {
            let ordinal = calls.fetch_add(1, Ordering::AcqRel);
            let result = if ordinal == 0 {
                Err(QueryEmbeddingError::Restarting)
            } else {
                Err(QueryEmbeddingError::ModelMissing)
            };
            Ok(Box::new(ImmediateWorker(result)))
        });
        let service = QueryEmbeddingService::with_factory(factory, 1).unwrap();

        assert!(matches!(
            service.embed(request("first"), control(Duration::from_secs(1))),
            Err(QueryEmbeddingError::Restarting)
        ));
        assert!(matches!(
            service.embed(request("second"), control(Duration::from_secs(1))),
            Err(QueryEmbeddingError::ModelMissing)
        ));
        assert_eq!(factory_calls.load(Ordering::Acquire), 2);
    }
}
