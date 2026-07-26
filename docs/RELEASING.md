# Releasing hSUM alpha builds

This runbook describes the public alpha release train: a GitHub prerelease with
source, verified archives, checksums, and GitHub artifact attestations. It
deliberately does not publish to crates.io.

**Alpha is not Developer ID-signed or notarized.** The linker gives the macOS
binary an ad-hoc signature, but the project has no Apple Developer account or
Developer ID certificate and cannot notarize it. The release workflow detects
the missing credentials, emits a warning, and publishes anyway. Provenance in
alpha rests on the GPG-signed tag, `SHA256SUMS`, and GitHub attestations — not
on an Apple identity. Do not describe alpha macOS archives as Developer
ID-signed or notarized anywhere in public material.

## One-time GitHub setup

Before cutting a release, the release operator must:

1. Confirm the public `burkan2/hSUM` repository remains the intended release
   owner and this checkout's `origin`.
2. Protect `main`: require pull requests, require both CI matrix jobs, require
   an up-to-date branch, block force pushes, and limit who can bypass rules.
3. Enable release immutability. GitHub applies it only to future releases, so
   the workflow assembles every release as a complete draft before publishing.
4. Enable GitHub Actions and GitHub private vulnerability reporting.
5. Enable GitHub Issues and, if desired, Discussions. These are the channels
   promised by `SUPPORT.md`.

The project needs one accountable release operator. Keep the GitHub owner,
release credentials, and (once one exists) the Apple Developer account under
that operator's control. Do not grant the release workflow broader permissions
than it needs.

## Required GitHub Actions secrets

These secrets are **optional in alpha and currently unset**, so the macOS
archive ships without a Developer ID signature or notarization. Signing is
enabled by setting `APPLE_SIGNING_IDENTITY`; once it is present the workflow
requires the full set below and fails if any one is missing. Add them only after
obtaining an Apple Developer account and importing a current Developer ID
Application certificate:

| Secret | Purpose |
|---|---|
| `APPLE_CERTIFICATE_BASE64` | Base64-encoded `.p12` Developer ID Application certificate |
| `APPLE_CERTIFICATE_PASSWORD` | Password for that `.p12` file |
| `APPLE_SIGNING_IDENTITY` | Exact `Developer ID Application: …` identity |
| `APPLE_ID` | Apple account used for notarization |
| `APPLE_APP_PASSWORD` | App-specific password for notarization |
| `APPLE_TEAM_ID` | Apple Developer team identifier |
| `KEYCHAIN_PASSWORD` | One-use keychain password for the GitHub runner |

Never place those values in the repository, shell history, deployment settings,
or issue text. Rotate them immediately after any suspected exposure.

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
binary, and requires four artifact-level checks:

1. `scripts/reproducible-release-build.sh` rebuilds in an isolated target
   directory and requires byte-for-byte equality with the candidate.
2. `scripts/release-smoke.sh` creates a fresh Git repository and isolated
   `HSUM_HOME`, then validates init, search, immutable get, context, doctor,
   generated MCP client configuration, a real MCP initialize/tools-list
   exchange, and the documented all-source-failure exit.
3. `scripts/no-network-smoke.sh` repeats that first-user path with network
   syscalls denied.
4. `scripts/released-alpha1-upgrade-smoke.sh` downloads the checksum-pinned
   published alpha.1 executable, creates an index with it, then proves the
   candidate rejects stale evidence, rebuilds safely, and invalidates the old
   citation.

Before tagging, inspect the completed CI runs and run the same commands on the
candidate checkout locally:

```bash
cargo +1.91.0 xtask check
cargo +1.91.0 build --locked --release
RUSTUP_TOOLCHAIN=1.91.0 \
  bash scripts/reproducible-release-build.sh "$PWD/target/release/hsum"
bash scripts/release-smoke.sh "$PWD/target/release/hsum"
bash scripts/no-network-smoke.sh "$PWD/target/release/hsum"
bash scripts/released-alpha1-upgrade-smoke.sh "$PWD/target/release/hsum"
cargo +1.91.0 fetch --locked
bash scripts/generate-license-inventory.sh \
  "$PWD/target/hsum-cargo-licenses.json"
```

Each target also emits an SPDX SBOM using Syft through the Anchore action. The
SBOM is attached to the prerelease and cryptographically linked to its archive
by the GitHub SBOM attestation. Syft's Cargo catalog can leave SPDX license
fields as `NOASSERTION`, so the workflow separately generates a deterministic
inventory from every package declaration returned by locked, offline Cargo
metadata. Review that inventory before the tag; automated declarations are
evidence, not a legal opinion. `cargo fetch --locked` acquires the complete
all-target graph first; the generator then runs Cargo metadata in offline mode.

## Release procedure

1. Confirm `Cargo.toml`, `CHANGELOG.md`, README claims, and the static error
   documentation describe the candidate exactly. Do not claim a platform,
   signature, installer, or benchmark that has not been verified.
2. Confirm the versioned public documentation URL resolves over HTTPS, that
   `llms.txt` describes the candidate, and that one subcode URL from the static
   error catalog returns the matching page. The offline binary intentionally
   directs users to `hsum help error <SUBCODE>` rather than embedding web URLs.
3. Create and locally verify an annotated, signed tag matching `Cargo.toml`:

   ```bash
   version=$(awk -F '"' '$1 ~ /^version = / { print $2; exit }' Cargo.toml)
   tag="v$version"
   git tag -s "$tag" -m "hSUM $version"
   git verify-tag "$tag"
   git push origin "$tag"
   ```

4. The tag triggers `.github/workflows/release.yml`. It repeats the release
   checks, creates checksums and GitHub provenance attestations, assembles all
   assets in a draft, then publishes the GitHub prerelease. While no Apple
   credentials are configured it logs a warning and publishes a macOS archive
   with only the linker's ad-hoc signature; when signing is enabled it also
   Developer ID-signs and notarizes that archive.
5. Download each archive from the GitHub Release onto a machine that did not
   build it. Verify `SHA256SUMS`, verify its GitHub attestation, review the Cargo
   license inventory, run `hsum --version --verbose`, and repeat the smoke
   script using the downloaded executable. On macOS the archive is quarantined
   until signing exists; clear it with `xattr -d com.apple.quarantine hsum`
   only after the checksum and attestation pass.
6. Record the workflow URLs, artifact hashes, whether the macOS archive was
   signed, and the clean-machine results in that release's notes. Promote no
   claim without this evidence.

## User verification

Users can verify an archive and its matching per-asset checksum file from
`burkan2/hSUM`:

```bash
# macOS example; use sha256sum -c on Linux.
asset=hsum-v0.1.0-alpha.2-aarch64-apple-darwin.zip
shasum -a 256 -c "$asset.sha256"
gh attestation verify "$asset" --repo burkan2/hSUM \
  --predicate-type https://spdx.dev/Document/v2.3
```

Those two checks are the whole provenance story in alpha. The macOS executable
has only a linker-generated ad-hoc signature, so `codesign --verify hsum`
succeeds, but it has no Developer ID identity and is not notarized; Gatekeeper
will still block it. Users clear quarantine with
`xattr -d com.apple.quarantine hsum` after the checks above pass. Once signing
is enabled, the macOS executable must additionally pass
`codesign --verify --deep --strict hsum` after extraction. The published
installer, when introduced, must perform the same checksum verification without
`sudo`.

## Rollback and compromise

Git tags and release assets are permanent evidence, so do not overwrite or
retag a published version. GitHub enforces that rule for releases published
after repository release immutability is enabled; `v0.1.0-alpha.2` predates
that setting and relies on its signed tag, checksums, and attestations. If an
artifact is wrong or compromised, mark the GitHub Release as compromised,
remove it from the latest recommendation, revoke the affected signing
credentials, publish a corrected higher version, and explain the exact
affected versions and hashes in public release notes.
