# Releasing hSUM alpha builds

This runbook describes the first public release train: a GitHub prerelease with
source, verified archives, checksums, GitHub artifact attestations, and a
notarized macOS arm64 executable. It deliberately does not publish to crates.io.

## One-time GitHub setup

Before opening the repository publicly, the release operator must:

1. Create the public `burkankale/hsum` repository and add this checkout as `origin`.
2. Protect `main`: require pull requests, require both CI matrix jobs, require
   an up-to-date branch, block force pushes, and limit who can bypass rules.
3. Enable GitHub Actions and GitHub private vulnerability reporting.
4. Enable GitHub Issues and, if desired, Discussions. These are the channels
   promised by `SUPPORT.md`.
5. Configure a Vercel project whose output directory is `site`, attach
   `hsum.burkankale.com`, and verify TLS before the release tag is created.

The project needs one accountable release operator. Keep the GitHub owner, DNS
owner, Apple Developer account, and release credentials under that operator's
control. Do not grant the release workflow broader permissions than it needs.

## Required GitHub Actions secrets

The release workflow intentionally fails rather than publish an unsigned macOS
artifact. Add these repository secrets after importing a current Developer ID
Application certificate from the Apple Developer account:

| Secret | Purpose |
|---|---|
| `APPLE_CERTIFICATE_BASE64` | Base64-encoded `.p12` Developer ID Application certificate |
| `APPLE_CERTIFICATE_PASSWORD` | Password for that `.p12` file |
| `APPLE_SIGNING_IDENTITY` | Exact `Developer ID Application: …` identity |
| `APPLE_ID` | Apple account used for notarization |
| `APPLE_APP_PASSWORD` | App-specific password for notarization |
| `APPLE_TEAM_ID` | Apple Developer team identifier |
| `KEYCHAIN_PASSWORD` | One-use keychain password for the GitHub runner |

Never place those values in the repository, shell history, Vercel settings, or
issue text. Rotate them immediately after any suspected exposure.

## Required repository variables

The tag is a release claim, so the workflow requires an annotated GPG-signed
tag from the release operator. Add these **repository variables** before the
first release. They are public key material, not secrets:

| Variable | Purpose |
|---|---|
| `RELEASE_GPG_FINGERPRINT` | Exact uppercase fingerprint of the release signing key |
| `RELEASE_GPG_PUBLIC_KEY` | ASCII-armored public key matching that fingerprint |

The workflow imports only that key and runs `git verify-tag` before it builds
or publishes an asset. The operator should keep the private signing key outside
GitHub and rotate the public key deliberately through a reviewed pull request.

## Candidate evidence

The `CI` workflow runs on clean GitHub-hosted Linux x86_64 and macOS arm64
runners. On each platform it runs `cargo xtask check`, builds the release
binary, and runs `scripts/release-smoke.sh`. The smoke script creates a fresh
Git repository and an isolated `HSUM_HOME`, then validates init, search,
immutable get, context, doctor, and generated MCP client configuration without
creating a repository pointer. It also records first-init time in the CI log,
performs a real MCP initialize/tools-list exchange, and confirms an all-source
ingest failure uses the documented non-zero process exit.

Before tagging, inspect the completed CI runs and run the same commands on the
candidate checkout locally:

```bash
cargo xtask check
cargo build --locked --release
bash scripts/release-smoke.sh "$PWD/target/release/hsum"
```

Each target also emits an SPDX SBOM using Syft through the Anchore action. The
SBOM is attached to the prerelease and cryptographically linked to its archive
by the GitHub SBOM attestation. Review the declared dependency licenses in the
SBOM before the first tag; an automated inventory is evidence, not a legal
opinion.

## Release procedure

1. Confirm `Cargo.toml`, `CHANGELOG.md`, README claims, and the static error
   documentation describe the candidate exactly. Do not claim a platform,
   signature, installer, or benchmark that has not been verified.
2. Confirm the public documentation URL resolves over HTTPS and that one error
   URL embedded by the candidate binary returns the matching page.
3. Create and locally verify an annotated, signed tag matching `Cargo.toml`:

   ```bash
   git tag -s v0.1.0-alpha.1 -m "hSUM 0.1.0-alpha.1"
   git verify-tag v0.1.0-alpha.1
   git push origin v0.1.0-alpha.1
   ```

4. The tag triggers `.github/workflows/release.yml`. It repeats the release
   checks, signs and notarizes the macOS archive, creates checksums and GitHub
   provenance attestations, then creates a GitHub prerelease.
5. Download each archive from the GitHub Release onto a machine that did not
   build it. Verify `SHA256SUMS`, verify its GitHub attestation, run
   `hsum --version --verbose`, and repeat the smoke script using the downloaded
   executable.
6. Record the workflow URLs, artifact hashes, notarization result, and the
   clean-machine results in that release's notes. Promote no claim without this
   evidence.

## User verification

Users can verify an archive from `burkankale/hsum`:

```bash
shasum -a 256 -c SHA256SUMS
gh attestation verify hsum-v0.1.0-alpha.1-ARCHIVE --repo burkankale/hsum
```

The macOS archive must also pass `codesign --verify --deep --strict hsum` after
extraction. The published installer, when introduced, must perform the same
checksum verification without `sudo`.

## Rollback and compromise

Git tags and release assets are immutable evidence, so do not overwrite or
retag a published version. If an artifact is wrong or compromised, mark the
GitHub Release as compromised, remove it from the latest recommendation,
revoke the affected signing credentials, publish a corrected higher version,
and explain the exact affected versions and hashes in public release notes.
