# Stable release signing and verification

Status on November 3, 2026: asset verification, authenticated installation, and
the separate `release-stable.yml` draft/publish workflow are implemented.
Production Apple credentials, offline Minisign authority, independent key
distribution and protected release environments remain outstanding. The stable
workflow has not been executed with production authority. The existing
`release.yml` publishes alpha tags only. These instructions do not waive
performance or final release gates: candidate `5ca552c` still fails Linux 100k
ordinary lexical latency.

## Authority setup

The release owner supplies the production Minisign public key; the private key
stays under separately protected offline custody and never enters Actions,
repository files, logs, or chat. Record the full base64 public key and SHA-256
of its decoded 42 bytes in the signed source, `SECURITY.md`, and crates.io
package metadata. `scripts/verify_stable_authority.py` requires these exact pins:

- `release/minisign.pub`: the two-line public-key file from the offline owner.
- `[package.metadata.hsum-release]` in `Cargo.toml`: `minisign-public-key` and
  `minisign-public-key-sha256` strings.
- One line in `SECURITY.md`: ``Minisign public key SHA-256: `<lowercase hash>` ``.

Review key custody, backup access, rotation, revocation and
compromise recovery before signing a stable tag. Do not use the test keys from
the signature tests as release authority.

The signed Git tag uses the configured GPG authority. This is distinct from
the offline artifact signer. `gh release create --verify-tag` checks that the
tag exists remotely; the workflow must separately run `git verify-tag` and
bind the verified tag to the build commit and Cargo version. Configure public
repository variables `RELEASE_GPG_PUBLIC_KEY` and `RELEASE_GPG_FINGERPRINT`.
The guard verifies the signer, the tag object's internal name/type/commit,
the dispatch ref and SHA, and the Cargo version. Publication also compares
the remote annotated tag object and peeled commit with the guarded values.

Apple signing requires `APPLE_CERTIFICATE_BASE64`, `APPLE_CERTIFICATE_PASSWORD`,
`APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_APP_PASSWORD`, `APPLE_TEAM_ID`, and
`KEYCHAIN_PASSWORD` under the protected release environment. Stable builds must
fail if any is absent, require notarization acceptance, and retain verification
evidence. The alpha opt-out is forbidden for stable installation.

## Protected environment setup

The November 3 read-only audit found no release environments configured.
Create `stable-release-build` and `stable-release-publish` with required
reviewers, self-review prevention, administrator bypass disabled, and a custom
deployment policy allowing only the exact stable tag being released. The
guard checks both environments through the authenticated REST API before a
job can enter them; it rejects absent or unrestricted environments, wildcard
tags, branch policies and missing reviewers. Administrators retain authority
to change repository configuration; review access and bypass settings as part
of release authority setup.

Set environment variables `STABLE_RELEASE_APPROVED_TAG` and
`STABLE_RELEASE_APPROVED_COMMIT` to the reviewed tag and full commit, after
both native qualification runs and final review pass. Put Apple secrets in
`stable-release-build`. In `stable-release-publish`, supply
`RELEASE_POLICY_READ_TOKEN`, limited to this repository with **Administration:
read-only** access. It only reads the immutable-release setting; publishing
uses the separate scoped `GITHUB_TOKEN`. Never put the offline private signing
key in either environment.

Repository immutable releases were enabled during the audit. The publish job
checks that setting again before publication, then requires the resulting
release to report `immutable: true`. The setting endpoint requires
[administration read access](https://docs.github.com/en/rest/repos/repos#check-if-immutable-releases-are-enabled-for-a-repository);
the ordinary workflow token cannot substitute for the policy read credential.

## Draft assembly and offline signing

After the exact candidate passes both native qualification protocols and final
review, dispatch `release-stable.yml` **at the signed tag**, with `tag` equal
to that ref and `action=prepare-draft`. The workflow builds and tests both
targets, checks unsigned binary reproducibility, signs/notarizes macOS, then
assembles the final archives.
Render both `@HSUM_VERSION@` and `@HSUM_MINISIGN_PUBLIC_KEY@` in
`scripts/install.sh` before checksumming. Generate the SBOMs, locked dependency
licenses, attestations, archive checksum sidecars and `SHA256SUMS` after the
payload bytes are final. Retain all provenance outside the exact asset bundle.

For a stable tag such as `v0.1.0`, the unsigned draft contains exactly nine
regular files: two archives, their two checksum sidecars, two SPDX SBOMs, the
Cargo license inventory, `install-hsum.sh`, and `SHA256SUMS`. Validate assembly:

```sh
python3 scripts/verify_stable_release.py "$tag" "$dist" \
  --public-key "$independently_verified_public_key" --unsigned-draft
```

This reports `signatures_verified: false`; it is never a publication gate.
The owner reviews and transfers the final payloads to the offline signing
environment. Sign exactly these four files with tag-and-filename context:

```sh
for asset in SHA256SUMS install-hsum.sh \
  "hsum-$tag-aarch64-apple-darwin.zip" \
  "hsum-$tag-x86_64-unknown-linux-gnu.tar.gz"
do
  minisign -S -s "$offline_secret_key_path" -m "$dist/$asset" \
    -t "hSUM $tag: $asset"
done
```

Use prehashed signatures (the default); do not use legacy `-l`. Only the four
`.minisig` files return to the online workflow. Upload them to the existing
draft without replacing payloads. Download all draft assets into an empty
directory and verify the resulting exact 13-file bundle:

```sh
python3 scripts/verify_stable_release.py "$tag" "$downloaded_draft" \
  --public-key "$independently_verified_public_key"
```

Verification authenticates all four signatures, their trusted comments, the
manifest's exact payload coverage, every payload hash, archive sidecars, and
the asset allowlist. It rejects missing/extra files, symlinks and tampering.
The publisher must also verify the draft still targets the verified source,
review clean-machine trials and attestations, and require the protected final
release approval. Only then may it publish. Verify downloaded immutable assets
again after publication and retain the receipts. Dispatch the same signed
tag with `action=publish-signed-draft` after the four signatures are uploaded.
Both native jobs authenticate the complete bundle, verify separate SLSA and
SPDX attestations against the exact source tag/commit/workflow, and run release,
signed-installer and no-network smoke checks. The protected publisher requires
both native verification receipts to match its freshly downloaded bundle;
the post-publication receipt must match as well. No payload regeneration or
signature substitution is permitted between these steps.

## High-assurance installation

Obtain the public-key fingerprint from an independent trusted distribution
channel and verify the full public key before trusting any downloaded file.
Download the installer, `SHA256SUMS`, and both detached signatures without
executing the installer. Authenticate each with `minisign -V -H`, verify its
trusted comment names the intended tag and file, and check the installer's
hash against the authenticated manifest before executing the local script.
The installer then authenticates the manifest and selected native archive
before extraction or execution, and requires Gatekeeper approval on macOS.
The HTTPS convenience path still trusts delivery of the installer itself.

## Development checks

`scripts/test_verify_stable_release.py` and `scripts/test_stable_installer.py`
use a real Minisign executable and disposable keys deleted after each test.
Set `HSUM_TEST_MINISIGN` to an absolute executable path when it is not on PATH.
They exercise valid signatures, context substitution, tampering, malformed
signature framing, wrong authority, extra assets, alpha-bypass refusal and
Gatekeeper failure. `scripts/test_release_smoke_routing.py` exercises both
stable archive routes with real signatures and disposable keys, plus offline
smoke routing and error propagation. Its binary and OS tools are stubs, so it
does not qualify Gatekeeper, network isolation, or a native product build.
`scripts/test_verify_stable_controls.py` tests tag relabelling and missing or
incorrect environment protection. `scripts/installer-smoke.sh` still covers
alpha installation with the actual candidate binary; for stable it requires
the final signed bundle as a second argument. Only the unsigned build phase
uses `no-network-smoke.sh --binary-only`; both native signed verification jobs
run the full installer smoke inside the OS network restriction. CI runs the
signature and routing regressions in a separate Linux job.
