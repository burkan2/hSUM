use std::env;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use reqwest::{Certificate, Client, StatusCode, redirect, tls};
use sha2::{Digest, Sha256};

use super::cache::{
    ModelError, ModelMutation, ModelStore, create_download_file, finish_download_file,
};
use super::manifest::{ModelFile, ModelManifest};

const MAX_TRANSIENT_RETRIES: u8 = 3;
const MAX_CA_BUNDLE_BYTES: u64 = 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);

impl ModelStore<'_> {
    pub async fn install(
        &self,
        id: &str,
        ca_bundle: Option<&Path>,
    ) -> Result<ModelMutation, ModelError> {
        let manifest = self.manifest(id)?;
        match self.inspect(manifest) {
            Ok(()) => return Ok(ModelMutation::AlreadyPresent(self.verify(id)?)),
            Err(ModelError::NotInstalled { .. }) => {}
            Err(error) => return Err(error),
        }
        if env::var_os("HSUM_OFFLINE").is_some_and(|value| value == "1") {
            return Err(ModelError::Offline {
                path: self.installation_path(manifest)?,
            });
        }

        let client = https_client(ca_bundle)?;
        let staging = self.create_staging(manifest)?;
        for expected in &manifest.files {
            download_with_retries(&client, manifest, expected, staging.path()).await?;
        }
        self.finish_staging(manifest, staging)
    }
}

fn https_client(ca_bundle: Option<&Path>) -> Result<Client, ModelError> {
    let mut builder = Client::builder()
        .https_only(true)
        .min_tls_version(tls::Version::TLS_1_2)
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(DOWNLOAD_TIMEOUT)
        .redirect(redirect::Policy::limited(5))
        .user_agent(concat!("hsum/", env!("CARGO_PKG_VERSION")));
    if let Some(path) = ca_bundle {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.file_type().is_file() || metadata.len() > MAX_CA_BUNDLE_BYTES {
            return Err(ModelError::NetworkConfiguration(format!(
                "CA bundle must be a regular PEM file no larger than {MAX_CA_BUNDLE_BYTES} bytes"
            )));
        }
        let pem = fs::read(path)?;
        let certificates = Certificate::from_pem_bundle(&pem)
            .map_err(|error| ModelError::NetworkConfiguration(error.without_url().to_string()))?;
        for certificate in certificates {
            builder = builder.add_root_certificate(certificate);
        }
    }
    builder
        .build()
        .map_err(|error| ModelError::NetworkConfiguration(error.without_url().to_string()))
}

async fn download_with_retries(
    client: &Client,
    manifest: &ModelManifest,
    expected: &ModelFile,
    staging_root: &Path,
) -> Result<(), ModelError> {
    let destination = staging_root.join(&expected.path);
    if let Some(parent) = destination.parent() {
        super::cache::ensure_download_parent(parent)?;
    }
    let maximum_attempts = MAX_TRANSIENT_RETRIES + 1;
    for attempt in 1..=maximum_attempts {
        match download_once(client, manifest, expected, &destination).await {
            Ok(()) => return Ok(()),
            Err(DownloadAttemptError::Permanent(reason)) => {
                return Err(ModelError::NetworkPermanent { reason });
            }
            Err(DownloadAttemptError::Integrity(error)) => return Err(error),
            Err(DownloadAttemptError::Transient(reason)) if attempt == maximum_attempts => {
                return Err(ModelError::NetworkTransient {
                    attempts: attempt,
                    reason,
                });
            }
            Err(DownloadAttemptError::Transient(_)) => {
                remove_partial(&destination)?;
                tokio::time::sleep(Duration::from_millis(250 * u64::from(attempt))).await;
            }
        }
    }
    unreachable!("the bounded download attempt loop always returns")
}

async fn download_once(
    client: &Client,
    manifest: &ModelManifest,
    expected: &ModelFile,
    destination: &Path,
) -> Result<(), DownloadAttemptError> {
    let url = manifest.download_url(expected);
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|error| DownloadAttemptError::Transient(error.without_url().to_string()))?;
    let status = response.status();
    if !status.is_success() {
        let reason = format!("upstream returned HTTP {status} for {}", expected.path);
        return if transient_status(status) {
            Err(DownloadAttemptError::Transient(reason))
        } else {
            Err(DownloadAttemptError::Permanent(reason))
        };
    }
    if response
        .content_length()
        .is_some_and(|length| length != expected.bytes)
    {
        return Err(DownloadAttemptError::Integrity(ModelError::FileSize {
            path: expected.path.clone(),
            expected: expected.bytes,
            actual: response.content_length().unwrap_or_default(),
        }));
    }

    let mut file = create_download_file(destination).map_err(DownloadAttemptError::Integrity)?;
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| DownloadAttemptError::Transient(error.without_url().to_string()))?
    {
        file.write_all(&chunk)
            .map_err(ModelError::from)
            .map_err(DownloadAttemptError::Integrity)?;
        hasher.update(&chunk);
        bytes = bytes
            .checked_add(
                u64::try_from(chunk.len())
                    .map_err(|_| DownloadAttemptError::Integrity(ModelError::IntegerOverflow))?,
            )
            .ok_or(DownloadAttemptError::Integrity(ModelError::IntegerOverflow))?;
        if bytes > expected.bytes {
            break;
        }
    }
    finish_download_file(file, destination, expected, bytes, hasher)
        .map_err(DownloadAttemptError::Integrity)
}

fn transient_status(status: StatusCode) -> bool {
    status.is_server_error()
        || status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
}

fn remove_partial(path: &Path) -> Result<(), ModelError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ModelError::Io(error)),
    }
}

enum DownloadAttemptError {
    Transient(String),
    Permanent(String),
    Integrity(ModelError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_statuses_are_narrow_and_explicit() {
        assert!(transient_status(StatusCode::REQUEST_TIMEOUT));
        assert!(transient_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(transient_status(StatusCode::BAD_GATEWAY));
        assert!(!transient_status(StatusCode::NOT_FOUND));
        assert!(!transient_status(StatusCode::UNAUTHORIZED));
    }
}
